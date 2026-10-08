// Typed Build recognition and foreground focused-stage evidence. Test grammar,
// test counts and carried full gates stay in their existing modules. Artifact
// reads happen off the state lock and never substitute for source/grant fences.

/// Only Cargo's build subcommand, with at most one plain toolchain selector.
/// Help, version, planning and dry-run requests do not execute a build.
fn engram_cargo_build_args(args: &[String]) -> bool {
    let rest = if args.first().is_some_and(|arg| arg.starts_with('+')) {
        let selector = &args[0][1..];
        if !engram_plain_toolchain_name(selector) {
            return false;
        }
        &args[1..]
    } else {
        args
    };
    rest.first().is_some_and(|arg| arg == "build")
        && !rest.iter().skip(1).any(|arg| {
            let option = arg.split('=').next().unwrap_or(arg);
            matches!(
                option,
                "--help"
                    | "--version"
                    | "--build-plan"
                    | "--unit-graph"
                    | "--dry-run"
                    | "--list"
                    | "--no-run"
            ) || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains('h'))
        })
}

/// The bounded foreground wrapper; no detach, notification, arbitrary script
/// suffix or custom stage file can stand for a Build command.
fn engram_focused_build_words(program: &str, args: &[String]) -> Option<Vec<String>> {
    let [script, mode, separator, cargo, rest @ ..] = args else {
        return None;
    };
    (program == "node"
        && matches!(
            script.replace('\\', "/").as_str(),
            "scripts/test-launcher.mjs" | "./scripts/test-launcher.mjs"
        )
        && mode == "focused"
        && separator == "--"
        && engram_program_name(cargo) == "cargo"
        && engram_cargo_build_args(rest))
    .then(|| {
        std::iter::once(cargo.clone())
            .chain(rest.iter().cloned())
            .collect()
    })
}

fn engram_is_build_command(program: &str, args: &[String]) -> bool {
    (program == "cargo" && engram_cargo_build_args(args))
        || engram_focused_build_words(program, args).is_some()
}

/// Capture the nested Cargo selector at command start, without probing a
/// workspace-chosen path. The report still fingerprints the original wrapper.
fn engram_build_toolchain_command(check: &EngramCheckCommand) -> EngramCheckCommand {
    let mut toolchain = check.clone();
    if check.kind == EngramVerificationKind::Build
        && check.program == "node"
        && let Some(words) = engram_shell_words(&check.normalized)
        && let Some(build) = engram_focused_build_words(&check.program, &words[1..])
        && build[0] == "cargo"
    {
        toolchain.program = "cargo".to_owned();
        toolchain.normalized = build.join(" ");
    }
    toolchain
}

/// Bounded first-rejection categories; no artifact contents enter diagnostics.
#[derive(Clone, Copy, Debug)]
enum EngramBuildRejection {
    Owner,
    Root,
    Path,
    Argv,
    Terminal,
    Interval,
    Input,
    Association,
}

impl EngramBuildRejection {
    fn label(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Root => "root",
            Self::Path => "path",
            Self::Argv => "argv",
            Self::Terminal => "terminal",
            Self::Interval => "interval",
            Self::Input => "input",
            Self::Association => "association",
        }
    }
}

/// Runtime ingress is diagnostic evidence, not an eligible stage exit. This
/// stays separate from `EngramTurnCheckEnd.exit`, which controls credit/refs.
#[derive(Clone, Debug)]
struct EngramBuildDiagnostic {
    observed_exit: Option<i64>,
    rejection: EngramBuildRejection,
}

impl EngramBuildDiagnostic {
    fn rejected(exit: EngramCommandExit, rejection: EngramBuildRejection) -> Self {
        Self {
            observed_exit: match exit {
                EngramCommandExit::Code(code) => Some(code),
                _ => None,
            },
            rejection,
        }
    }

    fn summary(&self, command: &EngramCheckCommand) -> String {
        let terminal = self.observed_exit.map_or_else(
            || "no native terminal exit observed".to_owned(),
            |code| format!("observed native exit {code}"),
        );
        // Put the discriminating facts before the untrusted command text so
        // neither can be lost to the existing UTF-8 summary budget.
        let prefix = format!(
            "Build Unknown: {terminal}; {} validation rejected; command: ",
            self.rejection.label()
        );
        format!(
            "{prefix}{}",
            engram_truncate_utf8(
                &command.normalized,
                ENGRAM_CHECK_SUMMARY_MAX_BYTES - prefix.len()
            )
        )
    }
}

/// A plain Cargo exit belongs to Cargo. A launcher exit instead needs its
/// invocation's native stage; absent or inconsistent artifacts yield Unknown.
fn engram_build_completion(
    check: &EngramTurnCheck,
    session_id: &str,
    output: &str,
    exit: EngramCommandExit,
) -> (
    EngramCommandExit,
    Vec<String>,
    Option<EngramBuildDiagnostic>,
) {
    let rejected = |reason| {
        (
            EngramCommandExit::Unknown,
            Vec::new(),
            Some(EngramBuildDiagnostic::rejected(exit, reason)),
        )
    };
    if check.command.program != "node" {
        return match exit {
            EngramCommandExit::Code(_) => (exit, Vec::new(), None),
            EngramCommandExit::NotFinished => (exit, Vec::new(), None),
            _ => rejected(EngramBuildRejection::Terminal),
        };
    }
    if matches!(
        exit,
        EngramCommandExit::Unknown | EngramCommandExit::NotFinished
    ) {
        return rejected(EngramBuildRejection::Terminal);
    }
    let words = engram_shell_words(&check.command.normalized);
    let build = words
        .as_ref()
        .and_then(|words| engram_focused_build_words(&check.command.program, &words[1..]));
    let Some(build) = build else {
        return rejected(EngramBuildRejection::Argv);
    };
    let code = match engram_validated_focused_stage_result(check, session_id, output, exit, &build)
    {
        Ok((code, _)) => code,
        Err(reason) => return rejected(reason),
    };
    (
        EngramCommandExit::Code(code),
        vec![format!("focused build: native exit {code}")],
        None,
    )
}

/// Match owner, root, argv, execution interval, input and terminal stage. A
/// copied/replayed result from before this command cannot lend it an exit.
/// This is a local launcher evidence seam, not cryptographic attestation of
/// arbitrary files. Existing overlap and source checks still apply afterward.
#[cfg(test)]
fn engram_focused_build_exit(
    check: &EngramTurnCheck,
    session_id: &str,
    output: &str,
    exit: EngramCommandExit,
) -> Option<i64> {
    let words = engram_shell_words(&check.command.normalized)?;
    let build = engram_focused_build_words(&check.command.program, &words[1..])?;
    engram_validated_focused_stage(check, session_id, output, exit, &build)
        .map(|(native, _)| native)
}

/// Shared local launcher facts. Callers own the bounded inner-command grammar
/// and interpretation of counts; owner, argv, interval and input policy stay
/// here, unchanged for Build. Reads happen before the lifecycle lock recheck.
fn engram_validated_focused_stage(
    check: &EngramTurnCheck,
    session_id: &str,
    output: &str,
    exit: EngramCommandExit,
    build: &[String],
) -> Option<(i64, Value)> {
    engram_validated_focused_stage_result(check, session_id, output, exit, build).ok()
}

/// Same validity policy for both callers. Node keeps its existing Option
/// boundary; Build retains the first rejected guard for truthful diagnostics.
fn engram_validated_focused_stage_result(
    check: &EngramTurnCheck,
    session_id: &str,
    output: &str,
    exit: EngramCommandExit,
    build: &[String],
) -> Result<(i64, Value), EngramBuildRejection> {
    use EngramBuildRejection as Rejected;
    // The launcher resolves its root from its script location. This bounded
    // form is supported only from the credited repository root.
    if engram_exact_path_key(&check.target.directory) != engram_exact_path_key(&check.target.root) {
        return Err(Rejected::Root);
    }
    let path = output
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix("results: "))
        .ok_or(Rejected::Path)?;
    let results_path = FsPath::new(path.trim());
    if !results_path.is_absolute()
        || engram_network_path(path)
        || results_path.file_name().and_then(|name| name.to_str()) != Some("results.json")
    {
        return Err(Rejected::Path);
    }
    let directory = results_path.parent().ok_or(Rejected::Path)?;
    let run = directory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(Rejected::Path)?;
    let uuid = run.strip_prefix("test-").ok_or(Rejected::Path)?;
    let parsed = uuid::Uuid::parse_str(uuid).map_err(|_| Rejected::Path)?;
    if parsed.to_string() != uuid {
        return Err(Rejected::Path);
    }
    let expected = engram_git_run_directory(&check.target.root)
        .ok_or(Rejected::Path)?
        .join(run);
    if engram_exact_path_key(directory) != engram_exact_path_key(&expected) {
        return Err(Rejected::Path);
    }
    // Reject links out of the local artifact directory as well as textual
    // foreign paths. No record contents or path are forwarded to Engram.
    let canonical = fs::canonicalize(&expected).map_err(|_| Rejected::Path)?;
    let read = |name: &str| -> Result<Value, EngramBuildRejection> {
        let file = directory.join(name);
        if fs::canonicalize(&file).map_err(|_| Rejected::Path)? != canonical.join(name) {
            return Err(Rejected::Path);
        }
        serde_json::from_slice(&engram_read_launcher_record(&file).ok_or(Rejected::Input)?)
            .map_err(|_| Rejected::Input)
    };
    if canonical
        != fs::canonicalize(expected.parent().ok_or(Rejected::Path)?)
            .map_err(|_| Rejected::Path)?
            .join(run)
    {
        return Err(Rejected::Path);
    }
    let request = read("request.json")?;
    let results = read("results.json")?;
    let input = read("input.json")?;
    let text = |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    for record in [&request, &results] {
        if text(record, "runId").as_deref() != Some(run) {
            return Err(Rejected::Association);
        }
        if text(record, "owner").as_deref() != Some(session_id) {
            return Err(Rejected::Owner);
        }
    }
    let root = text(&request, "root").ok_or(Rejected::Root)?;
    if !FsPath::new(&root).is_absolute()
        || engram_network_path(&root)
        || engram_exact_path_key(FsPath::new(&root)) != engram_exact_path_key(&check.target.root)
        || request.get("detached").and_then(Value::as_bool) != Some(false)
        || !engram_launcher_is_focused_request(&request)
    {
        return Err(Rejected::Root);
    }
    let [requested] = request
        .get("stages")
        .and_then(Value::as_array)
        .ok_or(Rejected::Argv)?
        .as_slice()
    else {
        return Err(Rejected::Argv);
    };
    if text(requested, "command").as_deref() != Some(build[0].as_str())
        || requested.get("args").ok_or(Rejected::Argv)? != &serde_json::json!(build[1..])
        || requested
            .get("cwd")
            .is_some_and(|cwd| cwd.as_str() != Some("."))
    {
        return Err(Rejected::Argv);
    }
    let [stage] = results
        .get("stages")
        .and_then(Value::as_array)
        .ok_or(Rejected::Terminal)?
        .as_slice()
    else {
        return Err(Rejected::Terminal);
    };
    let native = stage
        .get("code")
        .and_then(Value::as_i64)
        .ok_or(Rejected::Terminal)?;
    let state = if native == 0 { "passed" } else { "failed" };
    if native < 0
        || stage.get("name").and_then(Value::as_str) != Some("focused")
        || stage.get("state").and_then(Value::as_str) != Some(state)
        || results.get("state").and_then(Value::as_str) != Some(state)
        || results.get("exitCode").and_then(Value::as_i64) != Some(native)
        || [&results, stage].iter().any(|record| {
            record.get("error").is_some_and(|v| !v.is_null())
                || record.get("signal").is_some_and(|v| !v.is_null())
        })
    {
        return Err(Rejected::Terminal);
    }
    match exit {
        EngramCommandExit::Code(code) if code == native => {}
        EngramCommandExit::ReportedSuccess if native == 0 => {}
        _ => return Err(Rejected::Terminal),
    }
    let argv = stage
        .get("command")
        .and_then(Value::as_array)
        .ok_or(Rejected::Argv)?;
    let executable = argv.first().and_then(Value::as_str).ok_or(Rejected::Argv)?;
    if engram_program_name(executable) != engram_program_name(&build[0])
        || !FsPath::new(executable).is_absolute()
        || engram_network_path(executable)
        || argv[1..]
            != serde_json::json!(build[1..])
                .as_array()
                .ok_or(Rejected::Argv)?
                .as_slice()[..]
    {
        return Err(Rejected::Argv);
    }
    // An explicitly named executable must be the one the stage resolved.
    if FsPath::new(&build[0]).is_absolute()
        && engram_exact_path_key(FsPath::new(&build[0]))
            != engram_exact_path_key(FsPath::new(executable))
    {
        return Err(Rejected::Argv);
    }
    let cwd = text(stage, "cwd").ok_or(Rejected::Root)?;
    if !FsPath::new(&cwd).is_absolute()
        || engram_network_path(&cwd)
        || engram_exact_path_key(FsPath::new(&cwd))
            != engram_exact_path_key(&check.target.directory)
    {
        return Err(Rejected::Root);
    }
    let time = |value: &Value, key: &str| {
        text(value, key)
            .and_then(|text| engram_parse_time(&text))
            .ok_or(Rejected::Interval)
    };
    let observed_start = engram_parse_time(&check.started_at).ok_or(Rejected::Interval)?;
    let requested_start = time(&request, "started")?;
    let started = time(stage, "started")?;
    let ended = time(stage, "ended")?;
    let run_ended = time(&results, "ended")?;
    if observed_start > requested_start
        || requested_start > started
        || started > ended
        || ended > run_ended
        || run_ended > chrono::Utc::now()
        || text(&request, "started") != text(&results, "started")
    {
        return Err(Rejected::Interval);
    }
    let fingerprint = text(&request, "expectedFingerprint").ok_or(Rejected::Input)?;
    if fingerprint.len() != 64
        || !is_lowercase_hex(&fingerprint)
        || [
            &results["expectedFingerprint"],
            &results["before"],
            &results["after"],
            &input["fingerprint"],
        ]
        .iter()
        .any(|value| value.as_str() != Some(fingerprint.as_str()))
    {
        return Err(Rejected::Input);
    }
    let (verdict, verdict_run, verdict_exit) =
        engram_launcher_first_verdict(output).ok_or(Rejected::Terminal)?;
    if verdict != (if native == 0 { "PASS" } else { "FAIL" })
        || verdict_run != run
        || verdict_exit != format!("exit={native}")
    {
        return Err(Rejected::Terminal);
    }
    Ok((native, stage.clone()))
}
