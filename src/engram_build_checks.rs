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

/// A plain Cargo exit belongs to Cargo. A launcher exit instead needs its
/// invocation's native stage; absent or inconsistent artifacts yield Unknown.
fn engram_build_completion(
    check: &EngramTurnCheck,
    session_id: &str,
    output: &str,
    exit: EngramCommandExit,
) -> (EngramCommandExit, Vec<String>) {
    if check.command.program != "node" {
        return (
            if exit == EngramCommandExit::ReportedSuccess {
                EngramCommandExit::Unknown
            } else {
                exit
            },
            Vec::new(),
        );
    }
    let Some(code) = engram_focused_build_exit(check, session_id, output, exit) else {
        return (EngramCommandExit::Unknown, Vec::new());
    };
    (
        EngramCommandExit::Code(code),
        vec![format!("focused build: native exit {code}")],
    )
}

/// Match owner, root, argv, execution interval, input and terminal stage. A
/// copied/replayed result from before this command cannot lend it an exit.
/// This is a local launcher evidence seam, not cryptographic attestation of
/// arbitrary files. Existing overlap and source checks still apply afterward.
fn engram_focused_build_exit(
    check: &EngramTurnCheck,
    session_id: &str,
    output: &str,
    exit: EngramCommandExit,
) -> Option<i64> {
    let words = engram_shell_words(&check.command.normalized)?;
    let build = engram_focused_build_words(&check.command.program, &words[1..])?;
    // The launcher resolves its root from its script location. This bounded
    // form is supported only from the credited repository root.
    if engram_exact_path_key(&check.target.directory) != engram_exact_path_key(&check.target.root) {
        return None;
    }
    let path = output
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix("results: "))?;
    let results_path = FsPath::new(path.trim());
    if !results_path.is_absolute()
        || engram_network_path(path)
        || results_path.file_name()?.to_str()? != "results.json"
    {
        return None;
    }
    let directory = results_path.parent()?;
    let run = directory.file_name()?.to_str()?;
    let uuid = run.strip_prefix("test-")?;
    let parsed = uuid::Uuid::parse_str(uuid).ok()?;
    if parsed.to_string() != uuid {
        return None;
    }
    let expected = engram_git_run_directory(&check.target.root)?.join(run);
    if engram_exact_path_key(directory) != engram_exact_path_key(&expected) {
        return None;
    }
    // Reject links out of the local artifact directory as well as textual
    // foreign paths. No record contents or path are forwarded to Engram.
    let canonical = fs::canonicalize(&expected).ok()?;
    let read = |name: &str| -> Option<Value> {
        let file = directory.join(name);
        if fs::canonicalize(&file).ok()? != canonical.join(name) {
            return None;
        }
        serde_json::from_slice(&engram_read_launcher_record(&file)?).ok()
    };
    if canonical != fs::canonicalize(expected.parent()?).ok()?.join(run) {
        return None;
    }
    let request = read("request.json")?;
    let results = read("results.json")?;
    let input = read("input.json")?;
    let text = |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    for record in [&request, &results] {
        if text(record, "runId").as_deref() != Some(run)
            || text(record, "owner").as_deref() != Some(session_id)
        {
            return None;
        }
    }
    let root = text(&request, "root")?;
    if !FsPath::new(&root).is_absolute()
        || engram_network_path(&root)
        || engram_exact_path_key(FsPath::new(&root)) != engram_exact_path_key(&check.target.root)
        || request.get("detached").and_then(Value::as_bool) != Some(false)
        || !engram_launcher_is_focused_request(&request)
    {
        return None;
    }
    let [requested] = request.get("stages")?.as_array()?.as_slice() else {
        return None;
    };
    if text(requested, "command").as_deref() != Some(build[0].as_str())
        || requested.get("args")? != &serde_json::json!(build[1..])
        || requested
            .get("cwd")
            .is_some_and(|cwd| cwd.as_str() != Some("."))
    {
        return None;
    }
    let [stage] = results.get("stages")?.as_array()?.as_slice() else {
        return None;
    };
    let native = stage.get("code")?.as_i64()?;
    let state = if native == 0 { "passed" } else { "failed" };
    if native < 0
        || stage.get("name")?.as_str()? != "focused"
        || stage.get("state")?.as_str()? != state
        || results.get("state")?.as_str()? != state
        || results.get("exitCode")?.as_i64()? != native
        || [&results, stage].iter().any(|record| {
            record.get("error").is_some_and(|v| !v.is_null())
                || record.get("signal").is_some_and(|v| !v.is_null())
        })
    {
        return None;
    }
    match exit {
        EngramCommandExit::Code(code) if code == native => {}
        EngramCommandExit::ReportedSuccess if native == 0 => {}
        _ => return None,
    }
    let argv = stage.get("command")?.as_array()?;
    let executable = argv.first()?.as_str()?;
    if engram_program_name(executable) != "cargo"
        || !FsPath::new(executable).is_absolute()
        || engram_network_path(executable)
        || argv[1..] != serde_json::json!(build[1..]).as_array()?.as_slice()[..]
    {
        return None;
    }
    // An explicitly named executable must be the one the stage resolved.
    if FsPath::new(&build[0]).is_absolute()
        && engram_exact_path_key(FsPath::new(&build[0]))
            != engram_exact_path_key(FsPath::new(executable))
    {
        return None;
    }
    let cwd = text(stage, "cwd")?;
    if !FsPath::new(&cwd).is_absolute()
        || engram_network_path(&cwd)
        || engram_exact_path_key(FsPath::new(&cwd))
            != engram_exact_path_key(&check.target.directory)
    {
        return None;
    }
    let observed_start = engram_parse_time(&check.started_at)?;
    let requested_start = engram_parse_time(&text(&request, "started")?)?;
    let started = engram_parse_time(&text(stage, "started")?)?;
    let ended = engram_parse_time(&text(stage, "ended")?)?;
    let run_ended = engram_parse_time(&text(&results, "ended")?)?;
    if observed_start > requested_start
        || requested_start > started
        || started > ended
        || ended > run_ended
        || run_ended > chrono::Utc::now()
        || text(&request, "started") != text(&results, "started")
    {
        return None;
    }
    let fingerprint = text(&request, "expectedFingerprint")?;
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
        return None;
    }
    let (verdict, verdict_run, verdict_exit) = engram_launcher_first_verdict(output)?;
    if verdict != (if native == 0 { "PASS" } else { "FAIL" })
        || verdict_run != run
        || verdict_exit != format!("exit={native}")
    {
        return None;
    }
    Some(native)
}
