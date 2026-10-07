// Bounded foreground native-Node Test grammar and interpretation. Shared
// launcher validation and the lifecycle's source/grant fences own admission.

fn engram_focused_node_words(program: &str, args: &[String]) -> Option<Vec<String>> {
    let [script, mode, separator, node, rest @ ..] = args else {
        return None;
    };
    if program != "node"
        || !matches!(
            script.replace('\\', "/").as_str(),
            "scripts/test-launcher.mjs" | "./scripts/test-launcher.mjs"
        )
        || mode != "focused"
        || separator != "--"
        || engram_program_name(node) != "node"
        || rest.first()?.as_str() != "--test"
    {
        return None;
    }
    let mut index = 1;
    let mut files = false;
    let mut literal_files = false;
    while let Some(arg) = rest.get(index) {
        if arg == "--" && !literal_files {
            files = true;
            literal_files = true;
            index += 1;
            continue;
        }
        if literal_files || !arg.starts_with('-') {
            files = true;
        } else {
            if files {
                return None;
            }
            let (option, inline) = arg
                .split_once('=')
                .map_or((arg.as_str(), None), |(option, value)| {
                    (option, Some(value))
                });
            if !matches!(
                option,
                "--test-name-pattern"
                    | "--test-skip-pattern"
                    | "--test-reporter"
                    | "--test-concurrency"
                    | "--test-timeout"
            ) {
                return None;
            }
            let value = match inline {
                Some(value) => value,
                None => {
                    index += 1;
                    rest.get(index)?.as_str()
                }
            };
            if value.is_empty()
                || (option == "--test-reporter" && !matches!(value, "tap" | "spec"))
                || (matches!(option, "--test-concurrency" | "--test-timeout")
                    && !value.parse::<u64>().is_ok_and(|n| n > 0))
            {
                return None;
            }
        }
        index += 1;
    }
    Some(
        std::iter::once(node.clone())
            .chain(rest.iter().cloned())
            .collect(),
    )
}

fn engram_is_focused_node_check(check: &EngramCheckCommand) -> bool {
    check.kind == EngramVerificationKind::Test
        && engram_shell_words(&check.normalized)
            .is_some_and(|words| engram_focused_node_words(&check.program, &words[1..]).is_some())
}

/// A matched terminal stage and coherent native-runner counts are both needed.
/// No counts or positive-looking summary text can rescue unvalidated artifacts.
fn engram_focused_node_completion(
    check: &EngramTurnCheck,
    session_id: &str,
    output: &str,
    exit: EngramCommandExit,
) -> (EngramCommandExit, Vec<String>, bool) {
    let validated = || {
        if !check.command.simple {
            return None;
        }
        let words = engram_shell_words(&check.command.normalized)?;
        let node = engram_focused_node_words(&check.command.program, &words[1..])?;
        let (native, stage) =
            engram_validated_focused_stage(check, session_id, output, exit, &node)?;
        let counts = stage.get("tests")?;
        if counts.get("runner")?.as_str()? != "node-test" {
            return None;
        }
        let passed = counts.get("passed")?.as_u64()?;
        let failed = counts.get("failed")?.as_u64()?;
        let ignored = counts.get("ignored")?.as_u64()?;
        let executed = passed.checked_add(failed)?;
        executed.checked_add(ignored)?;
        if executed == 0 || (native == 0) != (failed == 0) {
            return None;
        }
        Some((native, passed, failed, ignored))
    };
    match validated() {
        Some((native, passed, failed, ignored)) => (
            EngramCommandExit::Code(native),
            vec![
                // Generated only from the validated stage, so the existing
                // one-call guard can attribute a nonzero exit to the runner.
                format!("focused: {}", if native == 0 { "passed" } else { "failed" }),
                format!(
                    "focused Node tests: passed={passed} failed={failed} ignored={ignored}; native exit {native}"
                ),
            ],
            native == 0 && passed > 0,
        ),
        None => (
            EngramCommandExit::Unknown,
            vec![
                "focused Node Test evidence unavailable or inconsistent; no passing credit"
                    .to_owned(),
            ],
            false,
        ),
    }
}

/// Only this bounded foreground launcher form gets a non-recognition notice.
/// Build and other recognized checks are excluded by the caller.
fn engram_unsupported_focused_line(command: &str) -> Option<String> {
    if engram_check_command(command).is_some() {
        return None;
    }
    let command_line = command.trim();
    let line = if let Some((_, inner)) = engram_one_call_prefix(command_line) {
        let (line, dialect) = engram_unwrap_shell_command(inner);
        // The supported one-call form never starts another shell after pushd.
        if dialect != EngramShellDialect::Unknown {
            return None;
        }
        line
    } else {
        engram_unwrap_shell_command(command_line).0
    };
    // Diagnose only a supported simple envelope, not arbitrary shell scripts.
    // This is feedback classification and never admits a Test or Build.
    if engram_has_unquoted_line_break(&line)
        || line.contains(['|', '&', ';', '<', '>', '`', '\n', '\r'])
        || line.contains("$(")
    {
        return None;
    }
    let words = engram_shell_words(&line)?;
    let [node, script, mode, separator, inner, ..] = words.as_slice() else {
        return None;
    };
    (engram_program_name(node) == "node"
        && matches!(script.replace('\\', "/").as_str(),
            "scripts/test-launcher.mjs" | "./scripts/test-launcher.mjs")
        && mode == "focused" && separator == "--" && !inner.is_empty())
        .then(|| format!("[TermAl] Unsupported focused launcher command {command:?}: no Test record will follow. Use a supported test runner form."))
}
