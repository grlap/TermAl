//! Owns selected-record refusal diagnostics and unchanged-acceptance controls.
//! Added beside src/tests/acceptance_evaluation_evidence.rs, using its fixtures.
//! Uses acceptance_evaluation_evidence's fixtures; does not own catalog paging,
//! citability policy, transport retries or the evaluator's verdicts.
use super::*;

const RECORD: &str = "cccccccccccccccccccccccccccccccc";
const PRIVATE_BODY: &str = "private-proof-and-routing-token-must-not-be-echoed";

fn receipt(locator: &str) -> Value {
    json!({"work_ref":"w-task", "note":{"locator":locator,
        "family":"notes", "kind":"generic", "summary":PRIVATE_BODY}})
}

fn selected_read(receipts: Vec<(String, Value)>) -> (Result<(), ApiError>, Vec<String>) {
    let (show, full, _) = canonical_binding_receipts();
    let mut task = parse_acceptance_evaluation_task(show.clone(), full).unwrap();
    let (state, project, _, root) = fixture();
    install_store(&state, &project, &root);
    let connection = state
        .work_read_snapshot(&project, None)
        .unwrap()
        .1
        .unwrap()
        .connection;
    let links = vec![AcceptanceCriterionEvidenceRequest {
        criterion: 1,
        locators: receipts
            .iter()
            .map(|(locator, _)| locator.clone())
            .collect(),
    }];
    let calls = Mutex::new(Vec::new());
    let result = read_requested_acceptance_evidence(
        &mut task,
        &links,
        &connection,
        &["work", "show", "w-task", "--notes", "--gates", "--json"].map(str::to_owned),
        &|_, args, _| {
            if let Some(index) = args.iter().position(|arg| arg == "--note") {
                let locator = &args[index + 1];
                calls.lock().unwrap().push(locator.clone());
                Ok(receipts
                    .iter()
                    .find(|(key, _)| key == locator)
                    .unwrap()
                    .1
                    .clone())
            } else {
                Ok(show.clone())
            }
        },
        std::time::Instant::now() + ACCEPTANCE_CRITERION_EVIDENCE_READ_BUDGET,
        &std::time::Instant::now,
    );
    assert!(state.inner.lock().unwrap().delegations.is_empty());
    (result, calls.into_inner().unwrap())
}

fn assert_refusal(value: Value, status: StatusCode, reason: &str, remedy: &str) {
    let (result, calls) = selected_read(vec![(RECORD.to_owned(), value)]);
    let error = result
        .err()
        .expect("the invalid record must remain refused");
    assert_eq!(error.status, status, "{error:?}");
    assert_eq!(calls, [RECORD]);
    assert!(!error.message.contains(PRIVATE_BODY), "{error:?}");
    assert!(
        error.message.contains(RECORD),
        "missing requested locator: {error:?}"
    );
    assert!(
        error.message.contains(reason),
        "missing specific reason: {error:?}"
    );
    assert!(
        error.message.contains(remedy),
        "missing corrective remedy: {error:?}"
    );
}

#[test]
fn wrong_returned_locator_is_named() {
    assert_refusal(
        receipt("dddddddddddddddddddddddddddddddd"),
        StatusCode::CONFLICT,
        "locator mismatch",
        "correct",
    );
}

#[test]
fn omitted_body_is_named() {
    let mut value = receipt(RECORD);
    value["note"]["body_omitted"] = json!(true);
    assert_refusal(value, StatusCode::CONFLICT, "body omitted", "smaller");
}

#[test]
fn truncated_summary_is_named() {
    let mut value = receipt(RECORD);
    value["note"]["summary_truncated"] = json!(true);
    assert_refusal(value, StatusCode::CONFLICT, "summary truncated", "smaller");
}

#[test]
fn strict_non_holder_refusal_names_holder_note_remedy() {
    let mut value = receipt(RECORD);
    value["note"]["non_holder"] = json!(true);
    assert_refusal(value, StatusCode::CONFLICT, "non-holder", "holder note");
}

#[test]
fn wrong_task_is_named_without_echoing_body() {
    let mut value = receipt(RECORD);
    value["work_ref"] = json!("w-another-task");
    assert_refusal(value, StatusCode::BAD_GATEWAY, "another task", "correct");
}

#[test]
fn malformed_record_is_named_without_echoing_body() {
    let mut value = receipt(RECORD);
    value["note"]["summary"] = json!([PRIVATE_BODY]);
    assert_refusal(value, StatusCode::BAD_GATEWAY, "invalid record", "retry");
}

#[test]
fn missing_body_is_named() {
    let mut value = receipt(RECORD);
    value["note"]["summary"] = Value::Null;
    assert_refusal(value, StatusCode::CONFLICT, "no readable body", "smaller");
}

#[test]
fn oversized_body_is_named_without_echoing_body() {
    let mut value = receipt(RECORD);
    value["note"]["summary"] = json!(PRIVATE_BODY.repeat(1024));
    assert_refusal(value, StatusCode::CONFLICT, "brief read bound", "smaller");
}

#[test]
fn strict_multi_invalid_selection_reports_every_locator() {
    let mut values = Vec::new();
    for (index, flag) in ["body_omitted", "summary_truncated", "non_holder"]
        .iter()
        .enumerate()
    {
        let locator = format!("{:032x}", index + 1);
        let mut value = receipt(&locator);
        value["note"][flag] = json!(true);
        values.push((locator, value));
    }
    let (result, calls) = selected_read(values.clone());
    let error = result.err().unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(!error.message.contains(PRIVATE_BODY));
    for (locator, _) in values {
        assert!(
            error.message.contains(&locator),
            "missing {locator}: {error:?}"
        );
        assert!(
            calls.contains(&locator),
            "each selected record is read within the bound"
        );
    }
    for reason in ["body omitted", "summary truncated", "non-holder"] {
        assert!(
            error.message.contains(reason),
            "missing {reason}: {error:?}"
        );
    }
}

#[test]
fn production_multi_invalid_request_never_spawns_partial_evaluator() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let (show, full, binding) = canonical_binding_receipts();
    install_binding_evidence_transport(&state, &parent, vec![binding]);
    let first = "11111111111111111111111111111111";
    let second = "22222222222222222222222222222222";
    let request = serde_json::from_value(json!({"workRef":"w-task", "criterionEvidence":[
        {"criterion":1,"locators":[first, second]}]}))
    .unwrap();
    let error = state
        .request_acceptance_evaluation_with_runner(&parent, request, |_, args, _| {
            if let Some(index) = args.iter().position(|arg| arg == "--note") {
                let locator = &args[index + 1];
                let mut value = receipt(locator);
                value["note"][if locator == first {
                    "body_omitted"
                } else {
                    "summary_truncated"
                }] = json!(true);
                Ok(value)
            } else if args.iter().any(|arg| arg == "inspect") {
                Ok(canonical_core_receipt())
            } else if args.iter().any(|arg| arg == "--full") {
                Ok(full.clone())
            } else if args.iter().any(|arg| arg == "held") {
                Ok(json!({"items":[],"omitted":0}))
            } else {
                assert_ne!(
                    args.first().map(String::as_str),
                    Some("control-policy"),
                    "invalid selected proof must be refused before evaluator admission"
                );
                Ok(show.clone())
            }
        })
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(state.inner.lock().unwrap().delegations.is_empty());
    assert!(!error.message.contains(PRIVATE_BODY));
    for (locator, reason) in [(first, "body omitted"), (second, "summary truncated")] {
        assert!(
            error.message.contains(locator),
            "missing {locator}: {error:?}"
        );
        assert!(
            error.message.contains(reason),
            "missing {reason}: {error:?}"
        );
    }
}

#[test]
fn whole_holder_receipt_stays_accepted() {
    let (result, calls) = selected_read(vec![(RECORD.to_owned(), receipt(RECORD))]);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(calls, [RECORD]);
}

#[test]
fn mixed_refusals_preserve_the_first_http_class_and_name_both_records() {
    let first = "11111111111111111111111111111111";
    let second = "22222222222222222222222222222222";
    for gateway_first in [false, true] {
        let mut values = Vec::new();
        for (index, locator) in [first, second].iter().enumerate() {
            let mut value = receipt(locator);
            if (index == 0) == gateway_first {
                value["note"]["summary"] = json!([PRIVATE_BODY]);
            } else {
                value["note"]["body_omitted"] = json!(true);
            }
            values.push(((*locator).to_owned(), value));
        }
        let (result, calls) = selected_read(values);
        let error = result.err().unwrap();
        assert_eq!(
            error.status,
            if gateway_first {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::CONFLICT
            }
        );
        assert_eq!(calls, [first, second]);
        for text in [
            first,
            second,
            "invalid record",
            "body omitted",
            "retry",
            "smaller",
        ] {
            assert!(error.message.contains(text), "{error:?}");
        }
        assert!(!error.message.contains(PRIVATE_BODY));
    }
}

#[test]
fn maximum_invalid_selection_reports_all_sixteen_without_extra_reads() {
    let values = (1..=16)
        .map(|index| {
            let locator = format!("{index:032x}");
            let mut value = receipt(&locator);
            value["note"]["summary_truncated"] = json!(true);
            (locator, value)
        })
        .collect::<Vec<_>>();
    let (result, calls) = selected_read(values.clone());
    let error = result.err().unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(
        calls,
        values
            .iter()
            .map(|(locator, _)| locator.clone())
            .collect::<Vec<_>>()
    );
    for (locator, _) in values {
        assert!(error.message.contains(&locator), "{error:?}");
    }
    assert!(error.message.len() < 8 * 1024, "diagnostics remain bounded");
    assert!(!error.message.contains(PRIVATE_BODY));
}

/// An exact-note CLI refusal has no receipt to decode. It is a caller's
/// invalid selection, not a transport outage; still collect other invalid
/// selected records without exposing the producer's error body.
fn cross_item_request(include_partial: bool) -> (ApiError, Vec<String>) {
    cross_item_request_with_read(include_partial, |_, _, _| {
        Err(EngramTransportError::remote(EngramControlErrorBody {
            code: "work_note_reference_invalid".to_owned(),
            message: format!("locator belongs to another item: {PRIVATE_BODY}"),
        }))
    })
}

fn cross_item_request_with_read(
    include_partial: bool,
    read_refused_note: impl Fn(
        &EngramConnectionConfig,
        &[String],
        Duration,
    ) -> Result<Value, EngramTransportError>,
) -> (ApiError, Vec<String>) {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let (show, full, binding) = canonical_binding_receipts();
    install_binding_evidence_transport(&state, &parent, vec![binding]);
    let partial = "22222222222222222222222222222222";
    let locators = if include_partial {
        vec![RECORD, partial]
    } else {
        vec![RECORD]
    };
    let request = serde_json::from_value(json!({"workRef":"w-task",
        "criterionEvidence":[{"criterion":1,"locators":locators}]}))
    .unwrap();
    let calls = Mutex::new(Vec::new());
    let error = state
        .request_acceptance_evaluation_with_runner(&parent, request, |connection, args, timeout| {
            if let Some(index) = args.iter().position(|arg| arg == "--note") {
                let locator = &args[index + 1];
                calls.lock().unwrap().push(locator.clone());
                if locator == RECORD {
                    read_refused_note(connection, args, timeout)
                } else {
                    let mut value = receipt(locator);
                    value["note"]["body_omitted"] = json!(true);
                    Ok(value)
                }
            } else if args.iter().any(|arg| arg == "inspect") {
                Ok(canonical_core_receipt())
            } else if args.iter().any(|arg| arg == "--full") {
                Ok(full.clone())
            } else if args.iter().any(|arg| arg == "held") {
                Ok(json!({"items":[],"omitted":0}))
            } else {
                assert_ne!(args.first().map(String::as_str), Some("control-policy"));
                Ok(show.clone())
            }
        })
        .err()
        .expect("invalid citations do not create an evaluator");
    assert!(state.inner.lock().unwrap().delegations.is_empty());
    (error, calls.into_inner().unwrap())
}

#[test]
fn cross_item_locator_is_a_caller_error_without_echoing_remote_body() {
    let (error, calls) = cross_item_request(false);
    assert!(
        error.status.is_client_error(),
        "cross-item citation is a caller error: {error:?}"
    );
    assert_eq!(calls, [RECORD]);
    assert!(!error.message.contains(PRIVATE_BODY), "{error:?}");
    for text in [RECORD, "does not belong", "this task", "select"] {
        assert!(error.message.contains(text), "missing {text}: {error:?}");
    }
}

#[test]
fn cross_item_and_partial_locators_are_reported_together() {
    let (error, calls) = cross_item_request(true);
    let partial = "22222222222222222222222222222222";
    assert!(error.status.is_client_error(), "{error:?}");
    assert_eq!(
        calls,
        [RECORD, partial],
        "all invalid requested locators are read"
    );
    assert!(!error.message.contains(PRIVATE_BODY), "{error:?}");
    for text in [
        RECORD,
        partial,
        "does not belong",
        "body omitted",
        "select",
        "smaller",
    ] {
        assert!(error.message.contains(text), "missing {text}: {error:?}");
    }
}

#[test]
fn selected_evidence_cli_refusal_reaches_caller_diagnostics_and_aggregation() {
    let temp = TestTempRoot::create("acceptance-selected-cli-refusal");
    let project_file = temp.path().join(".engram-project");
    fs::write(&project_file, "fixture\n").unwrap();
    // Engram's JSON work-word refusal contract: exit1, empty stdout,
    // structured error envelope on stderr. Additional private details are
    // intentionally present so the response must classify by code alone.
    let envelope = serde_json::to_string(&json!({"error":{
        "code":"work_note_reference_invalid", "message":PRIVATE_BODY,
        "details":{"reason":PRIVATE_BODY,"remedy":"use the complete locator printed beside the note"},
        "reminders":[],"next":[]
    }})).unwrap();
    #[cfg(windows)]
    let (script, contents) = (
        temp.path().join("note-refusal.ps1"),
        format!(
            "[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'started'), 'yes')\n[Console]::Error.WriteLine('{}')\nexit 1\n",
            envelope.replace('\'', "''")
        ),
    );
    #[cfg(unix)]
    let (script, contents) = (
        temp.path().join("note-refusal.sh"),
        format!(
            "#!/bin/sh\nprintf '%s' yes > \"$(dirname \"$0\")/started\"\nprintf '%s\\n' '{}' >&2\nexit 1\n",
            envelope.replace('\'', "'\\''")
        ),
    );
    fs::write(&script, contents).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let connection = EngramConnectionConfig {
        binary_path: script,
        project_file,
        home: temp.path().to_path_buf(),
        project_root: temp.path().to_path_buf(),
        actor_id: "selected-evidence-fixture".to_owned(),
        actor_context: None,
        session_id: "selected-evidence-fixture".to_owned(),
    };
    let (error, calls) = cross_item_request_with_read(true, |_, args, timeout| {
        run_acceptance_evaluation_read(&connection, args, timeout, temp.path())
    });
    assert!(
        temp.path().join("started").exists(),
        "the real CLI fixture must launch: {error:?}"
    );
    assert_eq!(
        error.status,
        StatusCode::BAD_REQUEST,
        "typed CLI refusal must reach caller diagnostics: {error:?}"
    );
    assert_eq!(calls, [RECORD, "22222222222222222222222222222222"]);
    assert!(!error.message.contains(PRIVATE_BODY), "{error:?}");
    for text in [
        RECORD,
        "does not belong",
        "select",
        "22222222222222222222222222222222",
        "body omitted",
        "smaller",
    ] {
        assert!(error.message.contains(text), "missing {text}: {error:?}");
    }
}

#[test]
fn selected_cli_decoder_keeps_unconfirmed_failures_fatal_and_private() {
    let refusal = serde_json::to_string(&json!({"error":{
        "code":"work_note_reference_invalid", "message":PRIVATE_BODY
    }}))
    .unwrap();
    for (exit, stdout, stderr) in [
        (EngramCliExit::Code(2), "", refusal.clone()),
        (EngramCliExit::Code(101), "", refusal.clone()),
        (EngramCliExit::Abnormal, "", refusal.clone()),
        (EngramCliExit::Code(1), refusal.as_str(), String::new()),
        (
            EngramCliExit::Code(1),
            "",
            format!("warning {PRIVATE_BODY}\n{refusal}"),
        ),
        (
            EngramCliExit::Code(1),
            "",
            format!("{{invalid {PRIVATE_BODY}"),
        ),
        (EngramCliExit::Code(1), "", PRIVATE_BODY.to_owned()),
        (
            EngramCliExit::Code(1),
            "",
            serde_json::to_string(&json!({"error":{
                "code":"another_refusal", "message":PRIVATE_BODY
            }}))
            .unwrap(),
        ),
        (
            EngramCliExit::Code(1),
            "",
            serde_json::to_string(&json!({"error":{
                "code":"work_note_reference_invalid", "message":[PRIVATE_BODY]
            }}))
            .unwrap(),
        ),
    ] {
        let error =
            decode_acceptance_selected_cli_output(cli_exit(exit, stdout, &stderr)).unwrap_err();
        assert_eq!(
            error.kind,
            EngramTransportErrorKind::Transport,
            "{exit:?}: {error:?}"
        );
        assert!(!error.message.contains(PRIVATE_BODY), "{error:?}");
        assert_ne!(error.code.as_deref(), Some("work_note_reference_invalid"));
    }
    let accepted = receipt(RECORD);
    assert_eq!(
        decode_acceptance_selected_cli_output(cli_output(true, &accepted.to_string(), &refusal))
            .unwrap(),
        accepted,
        "a successful receipt remains successful, even with stderr diagnostics"
    );
}

#[test]
fn selected_note_route_accepts_attribution_prefixes_without_matching_other_reads() {
    for prefix in [
        vec!["work"],
        vec!["work", "--actor-id", "actor", "--session-id", "session"],
        vec![
            "work",
            "--actor-id",
            "show",
            "--session-id",
            "--note",
            "--actor-context",
            "--json",
        ],
    ] {
        let mut selected = prefix.clone();
        selected.extend(["show", "w-task", "--note", RECORD, "--json"]);
        assert!(is_acceptance_selected_note_read(&selected), "{selected:?}");
        let mut selected_64 = selected.clone();
        selected_64[selected.len() - 2] =
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        assert!(is_acceptance_selected_note_read(&selected_64));
        for tail in [
            vec!["show", "w-task", "--notes", "--gates", "--json"],
            vec!["show", "w-task", "--full", "--json"],
            vec!["core", "inspect", "w-task", "--json"],
            vec!["core", "held", "--json"],
            vec!["show", "w-task", "--note", "prefix", "--json"],
            vec![
                "show",
                "w-task",
                "--note",
                "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC",
                "--json",
            ],
            vec!["show", "w-task", "--note", RECORD],
            vec!["show", "w-task", "--note", RECORD, "--json", "--extra"],
        ] {
            let mut other = prefix.clone();
            other.extend(tail);
            assert!(!is_acceptance_selected_note_read(&other), "{other:?}");
        }
    }
    for args in [
        vec![],
        vec!["work"],
        vec!["show", "w-task", "--note", RECORD, "--json"],
        vec!["other", "show", "w-task", "--note", RECORD, "--json"],
    ] {
        assert!(!is_acceptance_selected_note_read(&args), "{args:?}");
    }
}

#[test]
fn malformed_flag_does_not_echo_body_text() {
    let mut value = receipt(RECORD);
    value["note"]["body_omitted"] = json!(PRIVATE_BODY);
    assert_refusal(value, StatusCode::BAD_GATEWAY, "invalid record", "retry");
}

#[test]
fn duplicate_selected_holder_record_is_read_once() {
    let (result, calls) = selected_read(vec![
        (RECORD.to_owned(), receipt(RECORD)),
        (RECORD.to_owned(), receipt(RECORD)),
    ]);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(calls, [RECORD]);
}

#[test]
fn reader_failures_stay_fatal_and_redacted_after_prior_refusals() {
    for kind in [
        EngramTransportErrorKind::LocalState,
        EngramTransportErrorKind::Deadline,
        EngramTransportErrorKind::Transport,
        EngramTransportErrorKind::Protocol,
        EngramTransportErrorKind::Remote,
        EngramTransportErrorKind::Backoff,
    ] {
        for prior_refusal in [false, true] {
            let (show, full, _) = canonical_binding_receipts();
            let mut task = parse_acceptance_evaluation_task(show, full).unwrap();
            let (state, project, _, root) = fixture();
            install_store(&state, &project, &root);
            let connection = state
                .work_read_snapshot(&project, None)
                .unwrap()
                .1
                .unwrap()
                .connection;
            let earlier = "11111111111111111111111111111111";
            let unread = "22222222222222222222222222222222";
            let mut locators = vec![RECORD.to_owned(), unread.to_owned()];
            if prior_refusal {
                locators.insert(0, earlier.to_owned());
            }
            let calls = Mutex::new(Vec::new());
            let error = read_requested_acceptance_evidence(
                &mut task,
                &[AcceptanceCriterionEvidenceRequest {
                    criterion: 1,
                    locators,
                }],
                &connection,
                &["work", "show", "w-task", "--notes", "--json"].map(str::to_owned),
                &|_, args, _| {
                    let index = args.iter().position(|arg| arg == "--note").unwrap();
                    let locator = &args[index + 1];
                    calls.lock().unwrap().push(locator.clone());
                    if locator == earlier {
                        let mut value = receipt(locator);
                        value["note"]["body_omitted"] = json!(true);
                        Ok(value)
                    } else {
                        assert_eq!(locator, RECORD, "no read after an unavailable reader");
                        let mut error = EngramTransportError::transport(PRIVATE_BODY);
                        error.kind = kind.clone();
                        error.code = Some("unrecognised-producer-code".to_owned());
                        Err(error)
                    }
                },
                std::time::Instant::now() + ACCEPTANCE_CRITERION_EVIDENCE_READ_BUDGET,
                &std::time::Instant::now,
            )
            .unwrap_err();
            assert_eq!(
                error.status,
                if prior_refusal {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::BAD_GATEWAY
                }
            );
            assert!(
                error.message.contains(RECORD) && error.message.contains("retry"),
                "{error:?}"
            );
            assert!(!error.message.contains(PRIVATE_BODY), "{error:?}");
            assert!(!error.message.contains("unrecognised-producer-code"));
            assert_eq!(
                calls.into_inner().unwrap().len(),
                if prior_refusal { 2 } else { 1 }
            );
            if prior_refusal {
                assert!(error.message.contains(earlier) && error.message.contains("body omitted"));
            }
        }
    }
}

#[test]
fn aggregation_keeps_prior_diagnosis_without_reading_after_deadline() {
    let (show, full, _) = canonical_binding_receipts();
    let mut task = parse_acceptance_evaluation_task(show, full).unwrap();
    let (state, project, _, root) = fixture();
    install_store(&state, &project, &root);
    let connection = state
        .work_read_snapshot(&project, None)
        .unwrap()
        .1
        .unwrap()
        .connection;
    let start = std::time::Instant::now();
    let deadline = start + ACCEPTANCE_CRITERION_EVIDENCE_READ_BUDGET;
    let reads = AtomicUsize::new(0);
    let error = read_requested_acceptance_evidence(
        &mut task,
        &[AcceptanceCriterionEvidenceRequest {
            criterion: 1,
            locators: vec![
                RECORD.to_owned(),
                "22222222222222222222222222222222".to_owned(),
            ],
        }],
        &connection,
        &["work", "show", "w-task", "--notes", "--json"].map(str::to_owned),
        &|_, _, _| {
            assert_eq!(reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst), 0);
            let mut value = receipt(RECORD);
            value["note"]["summary_truncated"] = json!(true);
            Ok(value)
        },
        deadline,
        &|| {
            if reads.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                start
            } else {
                deadline
            }
        },
    )
    .unwrap_err();
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(error.status, StatusCode::CONFLICT);
    for text in [RECORD, "summary truncated", "smaller", "budget was spent"] {
        assert!(error.message.contains(text), "{error:?}");
    }
    assert!(!error.message.contains(PRIVATE_BODY));
}
