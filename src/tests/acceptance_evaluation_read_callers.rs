// Parent and settings read contracts through the production raw-CLI decoder.
// Stores and process output are fixtures; no command or evaluator is spawned.
use super::*;

fn refusal(code: &str, message: &str) -> EngramCliOutput {
    cli_output(
        false,
        "",
        &json!({"error":{"code":code,"message":message}}).to_string(),
    )
}

fn parent_failure(output: EngramCliOutput, fail_inspect: bool) -> (ApiError, EngramTransportError) {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let fallback = fixture_reader(
        Arc::default(),
        show_receipt(Some("same_session")),
        Ok(policy_receipt(None)),
    );
    let observed = std::cell::RefCell::new(None);
    let result = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(None),
            |connection, args, timeout| {
                let failing = args
                    .iter()
                    .any(|arg| arg == if fail_inspect { "inspect" } else { "show" })
                    && args.first().map(String::as_str) == Some("work");
                if !failing {
                    return fallback(connection, args, timeout);
                }
                let error = run_acceptance_evaluation_read_with_cli(
                    connection,
                    args,
                    timeout,
                    root.as_path(),
                    |_, _, _, _, _| Ok(output.clone()),
                )
                .unwrap_err();
                *observed.borrow_mut() = Some(error.clone());
                Err(error)
            },
        )
        .err()
        .expect("the failed reader must refuse the parent request");
    assert!(state.inner.lock().unwrap().delegations.is_empty());
    let observed = observed
        .into_inner()
        .expect("the parent must reach the raw CLI failure");
    (result, observed)
}

#[test]
fn parent_raw_cli_unsupported_inspect_usage_keeps_conflict() {
    let (error, observed) = parent_failure(
        cli_exit(
            EngramCliExit::Code(2),
            "",
            "error: unrecognized subcommand 'inspect'\nUsage: engram work core <COMMAND>\n",
        ),
        true,
    );
    assert_eq!(error.status, StatusCode::CONFLICT, "{}", error.message);
    assert!(error.message.contains("does not support core inspect"));
    assert_eq!(observed.cli_failure.unwrap().exit, EngramCliExit::Code(2));
}

#[test]
fn parent_raw_cli_unsupported_inspect_envelope_keeps_kind_code_and_conflict() {
    let (error, observed) = parent_failure(
        refusal("unsupported_operation", "core inspect is not supported"),
        true,
    );
    assert_eq!(error.status, StatusCode::CONFLICT, "{}", error.message);
    assert_eq!(observed.kind, EngramTransportErrorKind::Remote);
    assert_eq!(observed.code.as_deref(), Some("unsupported_operation"));
    assert!(error.message.contains("does not support core inspect"));
}

#[test]
fn parent_raw_cli_work_not_found_keeps_the_user_refusal_code() {
    let (error, observed) = parent_failure(
        refusal(
            "work_not_found",
            "the requested fixture work does not exist",
        ),
        false,
    );
    assert_eq!(error.status, StatusCode::BAD_GATEWAY);
    assert_eq!(observed.kind, EngramTransportErrorKind::Remote);
    assert_eq!(observed.code.as_deref(), Some("work_not_found"));
    assert!(
        error.message.contains("work_not_found"),
        "{}",
        error.message
    );
}

#[test]
fn parent_raw_cli_ambiguous_work_keeps_the_user_refusal_code() {
    let (error, observed) = parent_failure(
        refusal(
            "work_reference_ambiguous",
            "the fixture work reference is ambiguous",
        ),
        false,
    );
    assert_eq!(error.status, StatusCode::BAD_GATEWAY);
    assert_eq!(observed.kind, EngramTransportErrorKind::Remote);
    assert_eq!(observed.code.as_deref(), Some("work_reference_ambiguous"));
    assert!(
        error.message.contains("work_reference_ambiguous"),
        "{}",
        error.message
    );
}

#[test]
fn settings_raw_cli_unsupported_policy_keeps_the_operator_refusal_code() {
    let (state, project, _, root) = fixture();
    install_store(&state, &project, &root);
    let observed = std::cell::RefCell::new(None);
    let snapshot = state
        .project_acceptance_policy_with_reader(&project, |connection, args, timeout| {
            assert_eq!(args, &["control-policy", "show"]);
            let error = run_acceptance_evaluation_read_with_cli(
                connection,
                args,
                timeout,
                root.as_path(),
                |_, _, _, _, _| {
                    Ok(refusal(
                        "unsupported_operation",
                        "control policy is not supported",
                    ))
                },
            )
            .unwrap_err();
            *observed.borrow_mut() = Some(error.clone());
            Err(error)
        })
        .unwrap();
    assert!(!snapshot.available);
    assert!(snapshot.policy.is_none());
    let error = snapshot.error.unwrap();
    assert!(error.contains("unsupported_operation"), "{error}");
    let observed = observed.into_inner().expect("policy reader must run");
    assert_eq!(observed.kind, EngramTransportErrorKind::Remote);
    assert_eq!(observed.code.as_deref(), Some("unsupported_operation"));
}

#[test]
fn parent_raw_cli_success_still_admits_same_session_without_spawning() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let fallback = fixture_reader(
        Arc::default(),
        show_receipt(Some("same_session")),
        Ok(policy_receipt(None)),
    );
    let calls = std::cell::Cell::new(0);
    let result = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(None),
            |connection, args, timeout| {
                calls.set(calls.get() + 1);
                let receipt = fallback(connection, args, timeout)?;
                run_acceptance_evaluation_read_with_cli(
                    connection,
                    args,
                    timeout,
                    root.as_path(),
                    |_, _, _, _, _| Ok(cli_output(true, &receipt.to_string(), "")),
                )
            },
        )
        .unwrap();
    assert_eq!(
        serde_json::to_value(result).unwrap()["mode"],
        "same_session"
    );
    assert!(calls.get() > 0);
    assert!(state.inner.lock().unwrap().delegations.is_empty());
}

#[test]
fn settings_raw_cli_success_still_returns_the_policy() {
    let (state, project, _, root) = fixture();
    install_store(&state, &project, &root);
    let policy = json!({"schema_version":1,"policy":"915c51b89cd640f481a6e2c653bcfffd",
        "epoch":1,"required_assurance":"advisory","acceptance_evaluation":{
            "allowed_modes":[],"mechanical_basis":"asserted","require_source_freshness":false}});
    let snapshot = state
        .project_acceptance_policy_with_reader(&project, |connection, args, timeout| {
            assert_eq!(args, &["control-policy", "show"]);
            run_acceptance_evaluation_read_with_cli(
                connection,
                args,
                timeout,
                root.as_path(),
                |_, _, _, _, _| Ok(cli_output(true, &policy.to_string(), "")),
            )
        })
        .unwrap();
    assert!(snapshot.available);
    assert!(snapshot.error.is_none());
    assert_eq!(
        snapshot.policy.as_deref(),
        Some("915c51b89cd640f481a6e2c653bcfffd")
    );
}
