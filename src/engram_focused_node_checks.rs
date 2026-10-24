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
            // Standalone runner control; preserve it in argv without consuming
            // a value or changing how the validated terminal counts are read.
            if arg == "--test-force-exit" {
                index += 1;
                continue;
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
        && engram_focused_node_words(&check.program, &check.argv[1..]).is_some()
}

/// A matched terminal stage and coherent native-runner counts are both needed.
/// No counts or positive-looking summary text can rescue unvalidated artifacts.
/// A rejection whose run reported a terminal keeps that exit and names the
/// first guard that failed, as a Build's does (`EngramBuildDiagnostic`); the
/// exit stays out of the check's own, so it earns nothing. Only a run that
/// reported no terminal at all ends without an exit status.
fn engram_focused_node_completion(
    check: &EngramTurnCheck,
    session_id: &str,
    output: &str,
    exit: EngramCommandExit,
) -> (
    EngramCommandExit,
    Vec<String>,
    bool,
    Option<EngramBuildDiagnostic>,
) {
    use EngramBuildRejection as Rejected;
    let validated = || {
        // A pipe, list, redirection or substitution, as TermAl reads the line,
        // could mask the launcher's exit, so the exit cannot be the test's.
        if !check.command.simple {
            // Diagnostics only: a wholly quoted-looking pipe may be a
            // single command, but wrapper/unknown runtime text proves no
            // containment. Do not falsely call that command a real pipeline.
            return Err(engram_pipe_rejection(&check.command));
        }
        let words = &check.command.argv;
        let node = engram_focused_node_words(&check.command.program, &words[1..])
            .ok_or(Rejected::Argv)?;
        let (native, stage) =
            engram_validated_focused_stage_result(check, session_id, output, exit, &node)?;
        let counts = stage
            .get("tests")
            .filter(|counts| counts.is_object())
            .ok_or(Rejected::Counts)?;
        if counts.get("runner").and_then(Value::as_str) != Some("node-test") {
            return Err(Rejected::Runner);
        }
        let count = |name: &str| counts.get(name).and_then(Value::as_u64).ok_or(Rejected::Counts);
        let (passed, failed, ignored) = (count("passed")?, count("failed")?, count("ignored")?);
        let executed = passed.checked_add(failed).ok_or(Rejected::Counts)?;
        executed.checked_add(ignored).ok_or(Rejected::Counts)?;
        if executed == 0 || (native == 0) != (failed == 0) {
            return Err(Rejected::Counts);
        }
        Ok((native, passed, failed, ignored))
    };
    match validated() {
        Ok((native, passed, failed, ignored)) => (
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
            None,
        ),
        Err(rejection)
            if matches!(
                exit,
                EngramCommandExit::Code(_) | EngramCommandExit::ReportedSuccess
            ) =>
        {
            (
                EngramCommandExit::Unknown,
                Vec::new(),
                false,
                Some(EngramBuildDiagnostic::test_rejected(exit, rejection)),
            )
        }
        Err(_) => (
            EngramCommandExit::Unknown,
            vec![
                "focused Node Test evidence unavailable or inconsistent; no passing credit"
                    .to_owned(),
            ],
            false,
            None,
        ),
    }
}

/// Only this bounded foreground launcher form gets a non-recognition notice.
/// Describe non-test commands separately from unsupported test forms. This
/// changes feedback only; recognized checks (including Builds) stay excluded.
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
    let [node, script, mode, separator, inner, inner_args @ ..] = words.as_slice() else {
        return None;
    };
    if !(engram_program_name(node) == "node"
        && matches!(
            script.replace('\\', "/").as_str(),
            "scripts/test-launcher.mjs" | "./scripts/test-launcher.mjs"
        )
        && mode == "focused"
        && separator == "--"
        && !inner.is_empty())
    {
        return None;
    }
    let inner_program = engram_program_name(inner);
    // Native Node --test may express test intent while falling outside the
    // bounded foreground grammar. --no-run and lint/script commands do not
    // claim a Test; they may still have evidence of another verification kind.
    let test_intent = engram_is_test_command(&inner_program, inner_args)
        || (inner_program == "node" && inner_args.first().is_some_and(|arg| arg == "--test"));
    Some(if test_intent {
        format!(
            "[TermAl] Unsupported focused test-runner form {line:?}: no Test record will follow; \
             use a supported test runner form."
        )
    } else {
        format!(
            "[TermAl] Focused launcher command {line:?}: its inner command is not a recognized \
             test runner; no Test record will follow; no rerun is needed."
        )
    })
}
