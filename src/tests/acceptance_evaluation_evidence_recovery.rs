// Evaluator-scoped recovery after optional catalog discovery stops. All
// tracker reads use the parent's deterministic fixture, never a live store.
use super::*;

const OLDER_PROOF: &str = "00000000000000000000000000000003";

#[path = "acceptance_evaluation_recovery_regressions.rs"]
mod regressions;

fn catalog_note(index: u64) -> Value {
    let body = if index == 3 {
        "Older proof: both route criteria were checked.".to_owned()
    } else {
        format!("Recent context {index}")
    };
    json!({"locator":format!("{index:032x}"), "family":"notes", "kind":"generic",
        "by":"fixture holder", "created_at":"2026-11-01T00:00:00Z", "non_holder":false,
        "body_bytes":body.len(), "summary":body})
}

fn catalog_page(index: u64) -> Value {
    json!({"work":{"short_ref":"w-task"}, "notes":[catalog_note(index)],
        "notes_window":canonical_notes_window(4,index,1,42,
            (index < 3).then(|| format!("page-{}",index+1)).as_deref()),
        "notes_omitted":3-index})
}

fn recovery_fixture() -> (AppState, String, String, String, String) {
    recovery_fixture_for("independent_session")
}

fn recovery_fixture_for(mode: &str) -> (AppState, String, String, String, String) {
    recovery_fixture_with_indexed(mode, false)
}

fn recovery_fixture_with_indexed(
    mode: &str,
    selected: bool,
) -> (AppState, String, String, String, String) {
    recovery_fixture_with_catalog(mode, selected, catalog_page(0), "time_budget", 1)
}

fn recovery_fixture_with_catalog(
    mode: &str,
    selected: bool,
    first_page: Value,
    paging_stop: &str,
    unread: usize,
) -> (AppState, String, String, String, String) {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    crate::tests::delegation_support::install_delegation_codex_runtime(
        &state,
        "older-evidence-recovery",
    );
    let elapsed = std::cell::Cell::new(Duration::ZERO);
    let start = std::time::Instant::now();
    let mut request = evaluation_request(Some(Agent::Codex));
    if selected {
        request.criterion_evidence = vec![AcceptanceCriterionEvidenceRequest {
            criterion: 1,
            locators: vec![OLDER_PROOF.to_owned()],
        }];
    }
    let response = state
        .request_acceptance_evaluation_until(
            &parent,
            request,
            |connection, args, timeout| {
                assert!(timeout >= ENGRAM_MIN_CALL_TIMEOUT);
                if args.iter().any(|a| a == "inspect") {
                    Ok(evidence_selection::canonical_core_receipt())
                } else if args.iter().any(|a| a == "--note") {
                    recovery_reader(connection, args, timeout)
                } else if let Some(i) = args.iter().position(|a| a == "--after") {
                    assert_ne!(paging_stop, "entry_limit", "optional paging should stop");
                    let index = args[i + 1].strip_prefix("page-").unwrap().parse().unwrap();
                    elapsed.set(elapsed.get() + ENGRAM_MIN_CALL_TIMEOUT);
                    Ok(catalog_page(index))
                } else if args.iter().any(|a| a == "held") {
                    Ok(json!({"items":[],"omitted":0}))
                } else if args.iter().any(|a| a == "--full") {
                    Ok(full_receipt())
                } else if args.first().map(String::as_str) == Some("control-policy") {
                    Ok(policy_receipt(Some(&[mode])))
                } else {
                    let mut show = show_receipt(None);
                    show["notes"] = first_page["notes"].clone();
                    show["notes_window"] = first_page["notes_window"].clone();
                    show["notes_omitted"] = first_page["notes_omitted"].clone();
                    Ok(show)
                }
            },
            start + acceptance_evaluation_request_tracker_budget(),
            || start + elapsed.get(),
        )
        .unwrap();
    let wire = serde_json::to_value(&response).unwrap();
    assert_eq!(wire["evidenceOmissions"]["unread"]["reason"], paging_stop);
    assert_eq!(wire["evidenceOmissions"]["unread"]["count"], unread);
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response else {
        panic!("evaluator")
    };
    if !selected {
        assert!(
            !delegation.delegation.prompt.contains(OLDER_PROOF),
            "older locator was not discovered by the brief"
        );
    }
    let target = delegation
        .delegation
        .acceptance_evaluation
        .as_ref()
        .unwrap();
    let key = target.attempt_key.clone();
    (
        state,
        parent,
        delegation.delegation.id,
        delegation.delegation.child_session_id,
        key,
    )
}

fn read_request(key: &str, read_key: &str, records: bool) -> ReadAcceptanceEvidenceRequest {
    ReadAcceptanceEvidenceRequest {
        attempt_key: key.to_owned(),
        read_key: read_key.to_owned(),
        action: if records { "records" } else { "catalog" }.to_owned(),
        cursor: None,
        locators: if records {
            vec![OLDER_PROOF.to_owned()]
        } else {
            Vec::new()
        },
    }
}

fn recovery_reader(
    _: &EngramConnectionConfig,
    args: &[String],
    timeout: Duration,
) -> Result<Value, EngramTransportError> {
    assert!(timeout >= ENGRAM_MIN_CALL_TIMEOUT);
    if args.iter().any(|a| a == "inspect") {
        Ok(evidence_selection::canonical_core_receipt())
    } else if let Some(i) = args.iter().position(|a| a == "--after") {
        assert_eq!(
            args[i + 1],
            "page-3",
            "continue the captured catalog, never restart at its head"
        );
        Ok(catalog_page(3))
    } else if let Some(i) = args.iter().position(|a| a == "--note") {
        assert_eq!(args[i + 1], OLDER_PROOF);
        Ok(json!({"work_ref":"w-task", "note":catalog_note(3)}))
    } else {
        panic!("unexpected recovery read {args:?}")
    }
}

#[test]
fn older_only_proof_is_recovered_whole_then_a_verdict_is_submitted() {
    let (state, _, id, child, key) = recovery_fixture();
    let catalog = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "catalog", false),
            recovery_reader,
            std::time::Instant::now,
        )
        .unwrap();
    assert_eq!(catalog["state"], "complete");
    assert_eq!(catalog["catalog"][0]["locator"], OLDER_PROOF);
    assert_eq!(catalog["remainingUnread"], 0);
    let records = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "proof", true),
            recovery_reader,
            std::time::Instant::now,
        )
        .unwrap();
    assert_eq!(
        records["records"][0]["note"]["summary"],
        catalog_note(3)["summary"]
    );
    let mut request = submission(
        (1..=2)
            .map(|i| {
                verdict(
                    i,
                    "pass",
                    records["records"][0]["note"]["summary"].as_str().unwrap(),
                    &[OLDER_PROOF],
                )
            })
            .collect(),
    );
    request.attempt_key = Some(key.clone());
    let writes = AtomicUsize::new(0);
    state
        .submit_acceptance_evaluation_with_runner(&child, request, |connection, args, _| {
            writes.fetch_add(1, Ordering::SeqCst);
            assert_eq!(connection.session_id, child);
            assert!(args.iter().any(|s| s.contains(OLDER_PROOF)));
            assert_eq!(
                args.windows(2)
                    .filter(|pair| pair[0] == "--verdict")
                    .map(|pair| pair[1].as_str())
                    .collect::<Vec<_>>(),
                ["1=pass:judgment", "2=pass:judgment"]
            );
            let mut receipt: Value = serde_json::from_str(&evaluate_receipt(false)).unwrap();
            receipt["evaluation"]["attempt_key"] = json!(key);
            receipt["evaluation"]["work_revision"] = json!(7);
            receipt["evaluation"]["evaluated_cut"] = json!(42);
            receipt["work"]["revision"] = json!(7);
            receipt["evaluation"]["passed"] = json!(2);
            receipt["evaluation"]["blocking"] = Value::Null;
            receipt["evaluation"]["verdicts"][1]["verdict"] = json!("pass");
            receipt["evaluation"]["verdicts"][1]["citations"] = json!(1);
            Ok(cli_output(true, &receipt.to_string(), ""))
        })
        .unwrap();
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    let inner = state.inner.lock().unwrap();
    let target = inner.delegations[inner.find_delegation_index(&id).unwrap()]
        .acceptance_evaluation
        .as_ref()
        .unwrap();
    assert!(matches!(
        target.submission,
        AcceptanceEvaluationSubmission::Recorded { .. }
    ));
    assert!(acceptance_evidence_unfinished(target).is_none());
    if let AcceptanceEvaluationSubmission::Recorded { receipt, .. } = &target.submission {
        assert_eq!(receipt.passed, Some(2));
    }
}

#[test]
fn repeated_read_keys_replay_success_without_io_or_budget_charge_and_reject_changed_intent() {
    let (state, _, _, child, key) = recovery_fixture();
    let calls = AtomicUsize::new(0);
    let read = |connection: &EngramConnectionConfig, args: &[String], timeout| {
        calls.fetch_add(1, Ordering::SeqCst);
        recovery_reader(connection, args, timeout)
    };
    let first = read_request(&key, "catalog", false);
    let result = state
        .read_acceptance_evidence_with_io(&child, first.clone(), &read, std::time::Instant::now)
        .unwrap();
    let before = calls.load(Ordering::SeqCst);
    let replay = state
        .read_acceptance_evidence_with_io(&child, first, &read, std::time::Instant::now)
        .unwrap();
    assert_eq!(result, replay);
    assert_eq!(calls.load(Ordering::SeqCst), before);
    let error = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "catalog", true),
            &read,
            std::time::Instant::now,
        )
        .unwrap_err();
    assert!(error.message.contains("another evidence read intent"));
    assert_eq!(calls.load(Ordering::SeqCst), before);
    assert_eq!(
        state
            .read_acceptance_evidence_with_io(
                &child,
                read_request(&key, "body", true),
                &read,
                std::time::Instant::now
            )
            .unwrap()["state"],
        "complete"
    );
}

#[test]
fn invalid_caller_keys_and_unissued_records_do_not_damage_the_real_attempt() {
    let (state, parent, id, child, key) = recovery_fixture();
    let before = state
        .inner
        .lock()
        .unwrap()
        .delegations
        .iter()
        .find(|d| d.id == id)
        .unwrap()
        .clone();
    for (caller, mut request) in [
        (parent.as_str(), read_request(&key, "parent", false)),
        (
            child.as_str(),
            read_request("wrong-attempt", "wrong", false),
        ),
        (child.as_str(), read_request(&key, "unissued-record", true)),
        (child.as_str(), read_request(&key, "unissued-cursor", false)),
    ] {
        if request.read_key == "unissued-cursor" {
            request.cursor = Some("invented".to_owned());
        }
        assert!(
            state
                .read_acceptance_evidence_with_io(
                    caller,
                    request,
                    |_, _, _| panic!("no tracker read"),
                    std::time::Instant::now
                )
                .is_err()
        );
    }
    let inner = state.inner.lock().unwrap();
    assert_eq!(
        inner.delegations[inner.find_delegation_index(&id).unwrap()],
        before
    );
}

/// Engram's refusal of a notes continuation whose window gained a record after
/// its cut, as a failed CLI call and as a control reply deliver it.
fn window_changed_refusals() -> [EngramTransportError; 2] {
    let message = "the window changed; start a fresh window";
    let mut cli = EngramTransportError::transport(format!(
        "Engram acceptance reader failed: {{\"error\":{{\"code\":\"work_show_cursor_invalid\",\"message\":\"{message}\"}}}}"
    ));
    cli.refusal_code = Some("work_show_cursor_invalid".into());
    let control = EngramTransportError::remote(EngramControlErrorBody {
        code: "work_show_cursor_invalid".to_owned(),
        message: message.to_owned(),
    });
    [cli, control]
}

#[test]
fn a_record_appended_after_the_captured_cut_is_a_retryable_stale_basis_not_a_bare_no_verdict() {
    for refusal in window_changed_refusals() {
        let (state, _, id, child, key) = recovery_fixture();
        let instant = std::time::Instant::now();
        let request = read_request(&key, "catalog-after-append", false);
        let result = state
            .read_acceptance_evidence_with_io(
                &child,
                request.clone(),
                |connection, args, timeout| {
                    if args.iter().any(|a| a == "--after") {
                        Err(refusal.clone())
                    } else {
                        recovery_reader(connection, args, timeout)
                    }
                },
                move || instant,
            )
            .unwrap();
        assert_eq!(result["state"], "stale", "{result}");
        assert_eq!(result["retryable"], true);
        assert_eq!(result["recorded"], false);
        assert!(
            result.get("catalog").is_none(),
            "nothing past the cut is exposed"
        );
        let reason = result["unfinishedEvidence"]["reason"].as_str().unwrap();
        assert!(
            reason.contains("the item's evidence changed after the captured cut"),
            "{reason}"
        );
        assert!(
            reason.contains("the window changed"),
            "names Engram's refusal: {reason}"
        );
        let replay = state
            .read_acceptance_evidence_with_io(
                &child,
                request,
                |_, _, _| panic!("a recorded stale basis is replayed, not read again"),
                move || instant,
            )
            .unwrap();
        assert_eq!(result, replay);
        let mut submit = two_verdicts();
        submit.attempt_key = Some(key.clone());
        let refused = state
            .submit_acceptance_evaluation_with_runner(&child, submit, runner_must_not_run)
            .unwrap_err()
            .message;
        assert!(refused.contains("went stale"), "{refused}");
        assert!(refused.contains(reason), "{refused}");
        let inner = state.inner.lock().unwrap();
        let target = inner.delegations[inner.find_delegation_index(&id).unwrap()]
            .acceptance_evaluation
            .as_ref()
            .unwrap();
        let outcome = acceptance_evaluation_outcome_line(target);
        assert!(outcome.contains("retryable"), "{outcome}");
        assert!(outcome.contains(reason), "{outcome}");
        assert!(!outcome.contains("no verdict"), "{outcome}");
        let round_trip: DelegationAcceptanceEvaluation =
            serde_json::from_value(serde_json::to_value(target).unwrap()).unwrap();
        assert!(
            acceptance_evidence_unfinished(&round_trip)
                .unwrap()
                .stale_basis
        );
    }
}

#[test]
fn a_reused_evaluators_stale_attempt_is_reported_beside_its_earlier_retired_write() {
    let [_, refusal] = window_changed_refusals();
    let (state, _, id, child, key) = recovery_fixture();
    let instant = std::time::Instant::now();
    let result = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "catalog-after-append", false),
            |connection, args, timeout| {
                if args.iter().any(|a| a == "--after") {
                    Err(refusal.clone())
                } else {
                    recovery_reader(connection, args, timeout)
                }
            },
            move || instant,
        )
        .unwrap();
    assert_eq!(result["state"], "stale", "{result}");
    let reason = result["unfinishedEvidence"]["reason"].as_str().unwrap();
    let inner = state.inner.lock().unwrap();
    let mut reused = inner.delegations[inner.find_delegation_index(&id).unwrap()]
        .acceptance_evaluation
        .clone()
        .unwrap();
    // The persisted history reuse keeps for an earlier attempt whose write
    // was retired locally.
    let earlier = AcceptanceEvaluationPreviousAttempt {
        ordinal: 1,
        key: format!("{key}-earlier"),
        acceptance_basis: reused.acceptance_basis,
        evidence_basis: reused.evidence_basis,
        source_fingerprint: None,
        source_id: None,
        submission: AcceptanceEvaluationSubmission::None,
        refusal: Some(AcceptanceEvaluationDefinitiveRefusal {
            code: ACCEPTANCE_LOCAL_RETIREMENT_CODE.to_owned(),
            message: "the earlier write was retired locally".to_owned(),
            original: AcceptanceEvaluationOpenWrite::default(),
        }),
    };
    reused
        .attempt_history
        .as_mut()
        .unwrap()
        .previous
        .push(earlier);
    let outcome = acceptance_evaluation_outcome_line(&reused);
    assert!(outcome.contains("retryable"), "{outcome}");
    assert!(outcome.contains(reason), "{outcome}");
    assert!(
        outcome.contains(ACCEPTANCE_EARLIER_RETIRED_WRITE_NOTICE),
        "{outcome}"
    );
    assert!(!outcome.contains("no verdict"), "{outcome}");
}

#[test]
fn a_large_captured_catalog_recovers_every_older_record_while_notes_are_appended() {
    const IN_BRIEF: u64 = 40;
    const PAGE_SIZE: u64 = 10;
    const PAGES: u64 = 13;
    const OLDER: u64 = PAGES * PAGE_SIZE;
    const CAPTURED: u64 = IN_BRIEF + OLDER;
    let first_page = json!({
        "notes": ((OLDER + 1)..=CAPTURED).map(catalog_note).collect::<Vec<_>>(),
        "notes_window": canonical_notes_window(CAPTURED, 0, IN_BRIEF, 42, Some("page-1")),
        "notes_omitted": OLDER,
    });
    let (state, _, id, child, key) = recovery_fixture_with_catalog(
        "independent_session",
        false,
        first_page,
        "entry_limit",
        OLDER as usize,
    );
    let instant = std::time::Instant::now();
    let mut cursor = None;
    for number in 1..=PAGES {
        let mut request = read_request(&key, &format!("catalog-{number}"), false);
        request.cursor = cursor.take();
        let remaining = OLDER - number * PAGE_SIZE;
        let result = state
            .read_acceptance_evidence_with_io(
                &child,
                request,
                |_, args, _| {
                    if let Some(i) = args.iter().position(|arg| arg == "--after") {
                        assert_eq!(args[i + 1], format!("page-{number}"));
                        let appended = number;
                        let next = (remaining > 0).then(|| format!("page-{}", number + 1));
                        Ok(json!({
                            "work":{"short_ref":"w-task"},
                            "acceptance_basis": 7,
                            "evidence_basis": 42,
                            "notes": ((remaining + 1)..=(remaining + PAGE_SIZE))
                                .map(catalog_note).collect::<Vec<_>>(),
                            "notes_window":canonical_notes_window(
                                CAPTURED + appended,
                                IN_BRIEF + (number - 1) * PAGE_SIZE + appended,
                                PAGE_SIZE,
                                42 + number as i64,
                                next.as_deref(),
                            ),
                            "notes_omitted":remaining,
                        }))
                    } else {
                        assert!(args.iter().any(|arg| arg == "inspect"), "{args:?}");
                        Ok(evidence_selection::canonical_core_receipt())
                    }
                },
                || instant,
            )
            .unwrap();
        assert_eq!(result["state"], "complete", "page {number}: {result}");
        assert_eq!(result["remainingUnread"], remaining, "page {number}");
        let expected: Vec<_> = ((remaining + 1)..=(remaining + PAGE_SIZE))
            .map(catalog_note)
            .collect();
        assert_eq!(result["catalog"], json!(expected), "page {number}");
        cursor = result["cursor"].as_str().map(str::to_owned);
        assert_eq!(cursor.is_some(), remaining > 0);
    }
    let inner = state.inner.lock().unwrap();
    let target = inner.delegations[inner.find_delegation_index(&id).unwrap()]
        .acceptance_evaluation
        .as_ref()
        .unwrap();
    assert!(acceptance_evidence_unfinished(target).is_none());
}

#[test]
fn a_catalog_that_shrinks_after_an_append_is_refused_at_the_same_boundary() {
    let first_page = json!({
        "notes": (5..=44).map(catalog_note).collect::<Vec<_>>(),
        "notes_window": canonical_notes_window(44, 0, 40, 42, Some("after-head")),
        "notes_omitted": 4,
    });
    let (state, _, _, child, key) =
        recovery_fixture_with_catalog("independent_session", false, first_page, "entry_limit", 4);
    let instant = std::time::Instant::now();
    let read = |connection: &EngramConnectionConfig, args: &[String], timeout: Duration| {
        let Some(i) = args.iter().position(|arg| arg == "--after") else {
            return recovery_reader(connection, args, timeout);
        };
        let (notes, window, older) = match args[i + 1].as_str() {
            // One append increases total and newer equally. The requested
            // prefix still ends at four, just below the captured first page.
            "after-head" => (
                [catalog_note(3), catalog_note(4)],
                canonical_notes_window(45, 41, 2, 43, Some("after-append")),
                2,
            ),
            // The prefix still ends at two. Both rows have valid and
            // distinct locators, but the reported catalog shrank.
            "after-append" => (
                [catalog_note(1), catalog_note(2)],
                canonical_notes_window(43, 41, 2, 44, None),
                0,
            ),
            token => panic!("unexpected tracker cursor {token}"),
        };
        Ok(json!({"work":{"short_ref":"w-task"}, "notes":notes,
            "notes_window":window, "notes_omitted":older}))
    };
    let first = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "after-append", false),
            &read,
            || instant,
        )
        .unwrap();
    assert_eq!(first["state"], "complete", "{first}");
    assert_eq!(first["catalog"], json!([catalog_note(3), catalog_note(4)]));
    assert_eq!(first["remainingUnread"], 2);

    let mut request = read_request(&key, "after-shrink", false);
    request.cursor = Some(first["cursor"].as_str().unwrap().to_owned());
    let result = state
        .read_acceptance_evidence_with_io(&child, request, read, || instant)
        .unwrap();
    assert_eq!(result["state"], "unfinished", "{result}");
    assert_eq!(result["failureClass"], "evaluator_infrastructure");
    assert!(result.get("catalog").is_none(), "{result}");
    assert!(
        result["unfinishedEvidence"]["reason"]
            .as_str()
            .unwrap()
            .contains("catalog cut, counts or cursor changed"),
        "{result}",
    );
}

#[test]
fn moved_catalog_boundary_is_refused_without_revealing_a_page() {
    let (state, _, _, child, key) = recovery_fixture();
    let instant = std::time::Instant::now();
    let result = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "moved-boundary", false),
            |connection, args, timeout| {
                let mut page = recovery_reader(connection, args, timeout)?;
                if args.iter().any(|arg| arg == "--after") {
                    // Valid page arithmetic, including a continuation, but a
                    // different prefix end than the preceding page captured.
                    page["notes_window"] = canonical_notes_window(6, 4, 1, 43, Some("other-page"));
                    page["notes_omitted"] = json!(1);
                }
                Ok(page)
            },
            || instant,
        )
        .unwrap();
    assert_eq!(result["state"], "unfinished");
    assert_eq!(result["failureClass"], "evaluator_infrastructure");
    assert!(result.get("catalog").is_none(), "{result}");
    assert!(
        result["unfinishedEvidence"]["reason"]
            .as_str()
            .unwrap()
            .contains("catalog cut, counts or cursor changed"),
        "{result}",
    );
}

#[test]
fn catalog_timeout_is_an_evaluator_infrastructure_failure_with_the_engram_reason() {
    let (state, _, id, child, key) = recovery_fixture();
    let instant = std::time::Instant::now();
    let result = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "catalog-timeout", false),
            |connection, args, timeout| {
                if args.iter().any(|arg| arg == "--after") {
                    Err(EngramTransportError::deadline(
                        "fixture tracker process timed out",
                    ))
                } else {
                    recovery_reader(connection, args, timeout)
                }
            },
            || instant,
        )
        .unwrap();
    assert_eq!(result["state"], "unfinished");
    assert_eq!(result["failureClass"], "evaluator_infrastructure");
    assert_eq!(result["recorded"], false);
    let reason = result["unfinishedEvidence"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("(deadline)") && reason.contains("fixture tracker process timed out"),
        "{reason}"
    );
    let inner = state.inner.lock().unwrap();
    let target = inner.delegations[inner.find_delegation_index(&id).unwrap()]
        .acceptance_evaluation
        .as_ref()
        .unwrap();
    assert!(matches!(
        target.submission,
        AcceptanceEvaluationSubmission::None
    ));
    let outcome = acceptance_evaluation_outcome_line(target);
    for wording in [
        "evaluator infrastructure failure",
        "No criterion verdict was sent",
        reason,
    ] {
        assert!(outcome.contains(wording), "{outcome}");
    }
}

#[test]
fn requester_proof_beyond_the_brief_and_discovery_exhaustion_allow_mixed_verdicts() {
    let first_page = json!({
        "notes": (402..=441).map(catalog_note).collect::<Vec<_>>(),
        "notes_window": canonical_notes_window(441, 0, 40, 42, Some("older-cut")),
        "notes_omitted": 401
    });
    let (state, _, id, child, key) =
        recovery_fixture_with_catalog("independent_session", true, first_page, "entry_limit", 401);
    let selected = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "selected-first", true),
            |_, _, _| panic!("the requester proof was already read before spawn"),
            std::time::Instant::now,
        )
        .unwrap();
    assert_eq!(selected["records"][0]["note"], catalog_note(3));
    assert_eq!(
        selected["remainingReadMillis"],
        acceptance_evaluation_evidence_budget().as_millis() as u64
    );

    let elapsed = std::cell::Cell::new(Duration::ZERO);
    let start = std::time::Instant::now();
    let calls = AtomicUsize::new(0);
    let request = read_request(&key, "slow-catalog", false);
    let result = state
        .read_acceptance_evidence_with_io(
            &child,
            request.clone(),
            |_, args, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                elapsed.set(elapsed.get() + ENGRAM_MIN_CALL_TIMEOUT);
                if let Some(i) = args.iter().position(|a| a == "--after") {
                    assert_eq!(args[i + 1], "older-cut");
                    Ok(json!({
                        "work": {"short_ref":"w-task"},
                        "notes": [catalog_note(401)],
                        "notes_window": canonical_notes_window(441, 40, 1, 42, Some("next-older-cut")),
                        "notes_omitted": 400
                    }))
                } else {
                    assert!(args.iter().any(|a| a == "inspect"));
                    Ok(evidence_selection::canonical_core_receipt())
                }
            },
            || start + elapsed.get(),
        )
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2, "no unfunded closing read");
    assert_eq!(result["state"], "exhausted");
    assert_eq!(result["recorded"], false);
    assert!(
        result.get("catalog").is_none(),
        "unbracketed evidence is not exposed"
    );
    let replay = state
        .read_acceptance_evidence_with_io(
            &child,
            request,
            |_, _, _| panic!("cached failure"),
            std::time::Instant::now,
        )
        .unwrap();
    assert_eq!(result, replay);
    let refusal = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "fresh-catalog-after-exhaustion", false),
            |_, _, _| panic!("exhausted discovery must not issue tracker I/O"),
            std::time::Instant::now,
        )
        .expect_err("a fresh discovery key cannot reopen exhaustion");
    assert_eq!(refusal.status, StatusCode::CONFLICT);
    assert_eq!(refusal.message, result["remedy"].as_str().unwrap());
    let selected_after_exhaustion = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "selected-again", true),
            |_, _, _| panic!("discovery exhaustion cannot force an I/O re-read"),
            std::time::Instant::now,
        )
        .unwrap();
    assert_eq!(selected_after_exhaustion["records"], selected["records"]);

    let mut submit = submission(vec![
        verdict(
            1,
            "pass",
            selected["records"][0]["note"]["summary"].as_str().unwrap(),
            &[OLDER_PROOF],
        ),
        verdict(
            2,
            "insufficient-evidence",
            "The acceptance test result was not shown when discovery ran out.",
            &[],
        ),
    ]);
    submit.attempt_key = Some(key.clone());
    let mut receipt: Value = serde_json::from_str(&evaluate_receipt(false)).unwrap();
    receipt["evaluation"]["attempt_key"] = json!(key);
    receipt["evaluation"]["work_revision"] = json!(7);
    receipt["evaluation"]["evaluated_cut"] = json!(42);
    receipt["work"]["revision"] = json!(7);
    receipt["evaluation"]["blocking"]["verdict"] = json!("insufficient_evidence");
    receipt["evaluation"]["verdicts"][1]["verdict"] = json!("insufficient_evidence");
    state
        .submit_acceptance_evaluation_with_runner(&child, submit, |_, args, _| {
            assert_eq!(
                args.windows(2)
                    .filter(|pair| pair[0] == "--verdict")
                    .map(|pair| pair[1].as_str())
                    .collect::<Vec<_>>(),
                ["1=pass:judgment", "2=insufficient_evidence:judgment"]
            );
            assert!(args.iter().any(|arg| arg == &format!("1={OLDER_PROOF}")));
            assert!(args.windows(2).any(|pair| {
                pair[0] == "--rationale"
                    && pair[1].starts_with("2=")
                    && pair[1].ends_with(
                        "The acceptance test result was not shown when discovery ran out.",
                    )
            }));
            Ok(cli_output(true, &receipt.to_string(), ""))
        })
        .unwrap();
    let inner = state.inner.lock().unwrap();
    let target = inner.delegations[inner.find_delegation_index(&id).unwrap()]
        .acceptance_evaluation
        .as_ref()
        .unwrap();
    assert!(acceptance_evidence_unfinished(target).is_none());
    let AcceptanceEvaluationSubmission::Recorded { receipt, .. } = &target.submission else {
        panic!("expected a recorded verdict");
    };
    assert_eq!(receipt.passed, Some(1));
    let blocking = receipt.blocking.as_ref().unwrap();
    assert_eq!(blocking.position, 2);
    assert_eq!(blocking.verdict, "insufficient_evidence");
    assert!(
        acceptance_evaluation_outcome_line(target).contains("criterion 2 is insufficient_evidence")
    );
}

#[test]
fn expired_cursor_moving_identity_and_wrong_catalog_cut_end_without_a_judgment() {
    for fault in [
        "none",
        "expired",
        "work",
        "run",
        "closing_run",
        "cut",
        "count",
        "partial",
    ] {
        let (state, _, id, child, key) = recovery_fixture();
        let core_reads = std::cell::Cell::new(0);
        let result = state
            .read_acceptance_evidence_with_io(
                &child,
                read_request(&key, "catalog", false),
                |connection, args, timeout| {
                    let mut result = recovery_reader(connection, args, timeout)?;
                    if args.iter().any(|a| a == "--after") {
                        match fault {
                            "expired" => {
                                return Err(EngramTransportError::transport(
                                    "expired opaque cursor",
                                ));
                            }
                            "work" => result["work"]["short_ref"] = json!("w-other"),
                            "cut" => {
                                result["notes_window"]["read_cut"]["project_position"] = json!(1)
                            }
                            "count" => result["notes_window"]["total"] = json!(5),
                            _ => (),
                        }
                    }
                    if args.iter().any(|a| a == "inspect") {
                        core_reads.set(core_reads.get() + 1);
                        if fault == "run" || fault == "closing_run" && core_reads.get() == 2 {
                            result["run"]["generation"] = json!(2);
                        }
                    }
                    Ok(result)
                },
                std::time::Instant::now,
            )
            .unwrap();
        if matches!(fault, "none" | "partial") {
            assert_eq!(result["state"], "complete");
            let exact = state
                .read_acceptance_evidence_with_io(
                    &child,
                    read_request(&key, "body", true),
                    |connection, args, timeout| {
                        let mut value = recovery_reader(connection, args, timeout)?;
                        if fault == "partial" && args.iter().any(|a| a == "--note") {
                            value["note"]["body_omitted"] = json!(true);
                        }
                        Ok(value)
                    },
                    std::time::Instant::now,
                )
                .unwrap();
            assert_eq!(
                exact["state"],
                if fault == "none" {
                    "complete"
                } else {
                    "unfinished"
                }
            );
        } else {
            assert_eq!(result["state"], "unfinished", "{fault}: {result}");
            assert_eq!(result["recorded"], false);
        }
        let inner = state.inner.lock().unwrap();
        assert!(matches!(
            inner.delegations[inner.find_delegation_index(&id).unwrap()]
                .acceptance_evaluation
                .as_ref()
                .unwrap()
                .submission,
            AcceptanceEvaluationSubmission::None
        ));
    }
}

#[test]
fn lost_runtime_state_refuses_without_resetting_the_original_allowance() {
    let (state, _, _, child, key) = recovery_fixture();
    state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "first", false),
            recovery_reader,
            std::time::Instant::now,
        )
        .unwrap();
    state
        .inner
        .lock()
        .unwrap()
        .acceptance_evidence_recovery
        .clear();
    let result = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "first", false),
            |_, _, _| panic!("restart cannot restart reads"),
            std::time::Instant::now,
        )
        .unwrap();
    assert_eq!(result["state"], "unfinished");
    assert!(
        result["unfinishedEvidence"]["reason"]
            .as_str()
            .unwrap()
            .contains("not reset")
    );
}

#[test]
fn distinct_logical_reads_spend_the_same_allowance_and_thinking_time_spends_none() {
    let (state, _, _, child, key) = recovery_fixture();
    let elapsed = std::cell::Cell::new(Duration::ZERO);
    let start = std::time::Instant::now();
    let calls = AtomicUsize::new(0);
    let read = |connection: &EngramConnectionConfig, args: &[String], timeout| {
        calls.fetch_add(1, Ordering::SeqCst);
        let value = recovery_reader(connection, args, timeout);
        elapsed.set(elapsed.get() + Duration::from_secs(20));
        value
    };
    let catalog = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "catalog", false),
            &read,
            || start + elapsed.get(),
        )
        .unwrap();
    assert_eq!(catalog["remainingReadMillis"], 60_000);
    elapsed.set(elapsed.get() + Duration::from_secs(3600));
    let proof = state
        .read_acceptance_evidence_with_io(&child, read_request(&key, "proof", true), &read, || {
            start + elapsed.get()
        })
        .unwrap();
    assert_eq!(proof["state"], "exhausted");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        4,
        "a fresh readKey cannot reset the allowance"
    );
}

#[test]
fn sub_agent_recovery_exposes_its_actual_attempt_key_and_preserves_parent_identity() {
    let (state, parent, id, child, key) = recovery_fixture_for("sub_agent");
    assert_eq!(key, id);
    {
        let inner = state.inner.lock().unwrap();
        let delegation = &inner.delegations[inner.find_delegation_index(&id).unwrap()];
        assert!(delegation.prompt.contains(&format!("Attempt key: {key}")));
        let target = delegation.acceptance_evaluation.as_ref().unwrap();
        assert_eq!(target.parent_session.as_deref(), Some(parent.as_str()));
        assert_eq!(target.execution_identity.as_deref(), Some(id.as_str()));
    }
    assert_eq!(
        state
            .read_acceptance_evidence_with_io(
                &child,
                read_request(&key, "catalog", false),
                recovery_reader,
                std::time::Instant::now
            )
            .unwrap()["catalog"][0]["locator"],
        OLDER_PROOF
    );
}

#[test]
fn concurrent_identical_reads_share_one_result_and_exclude_submission() {
    let (state, _, _, child, key) = recovery_fixture();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let request = read_request(&key, "same-logical-read", false);
    let worker = state.clone();
    let evaluator = child.clone();
    let first = request.clone();
    let seen = calls.clone();
    let thread = std::thread::spawn(move || {
        worker
            .read_acceptance_evidence_with_io(
                &evaluator,
                first,
                |connection, args, timeout| {
                    if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    }
                    recovery_reader(connection, args, timeout)
                },
                std::time::Instant::now,
            )
            .unwrap()
    });
    entered_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
    let mut submit = two_verdicts();
    submit.attempt_key = Some(key);
    assert!(
        state
            .submit_acceptance_evaluation_with_runner(&child, submit, runner_must_not_run)
            .unwrap_err()
            .message
            .contains("already in progress")
    );
    let second = state.clone();
    let other = child.clone();
    let duplicate = std::thread::spawn(move || {
        second
            .read_acceptance_evidence_with_io(
                &other,
                request,
                |_, _, _| panic!("duplicate IO"),
                std::time::Instant::now,
            )
            .unwrap()
    });
    release_tx.send(()).unwrap();
    assert_eq!(thread.join().unwrap(), duplicate.join().unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn evidence_read_permissions_require_the_termal_server_and_an_evaluator_child() {
    let (state, parent, _, child, key) = recovery_fixture();
    assert_eq!(
        delegation_control_plane_capability_for_claude_tool_name(
            TERMAL_READ_ACCEPTANCE_EVIDENCE_QUALIFIED_TOOL_NAME
        ),
        Some(DelegationControlPlaneCapability::ReadAcceptanceEvidence)
    );
    for foreign in [
        TERMAL_READ_ACCEPTANCE_EVIDENCE_TOOL_NAME,
        "mcp__foreign__termal_read_acceptance_evidence",
    ] {
        assert_eq!(
            delegation_control_plane_capability_for_claude_tool_name(foreign),
            None
        );
    }
    let request = json!({"threadId":"fixture","turnId":"fixture",
        "serverName":TERMAL_DELEGATION_MCP_SERVER_NAME,"mode":"form","message":"read proof",
        "requestedSchema":{"type":"object","properties":{}},
        "_meta":{"codex_approval_kind":"mcp_tool_call",
            "tool_description":TERMAL_READ_ACCEPTANCE_EVIDENCE_TOOL_DESCRIPTION,
            "tool_params":serde_json::to_value(read_request(&key,"catalog",false)).unwrap()}});
    let payload: McpElicitationRequestPayload = serde_json::from_value(request.clone()).unwrap();
    assert_eq!(
        delegation_control_plane_capability_for_codex_elicitation(&payload),
        Some(DelegationControlPlaneCapability::ReadAcceptanceEvidence)
    );
    let mut foreign = request;
    foreign["serverName"] = json!("foreign");
    assert_eq!(
        delegation_control_plane_capability_for_codex_elicitation(
            &serde_json::from_value(foreign).unwrap()
        ),
        None
    );
    let inner = state.inner.lock().unwrap();
    assert!(delegation_control_plane_capability_allowed_locked(
        &inner,
        &child,
        DelegationControlPlaneCapability::ReadAcceptanceEvidence
    ));
    assert!(!delegation_control_plane_capability_allowed_locked(
        &inner,
        &parent,
        DelegationControlPlaneCapability::ReadAcceptanceEvidence
    ));
}

#[test]
fn a_target_replaced_during_read_never_receives_the_old_result_or_failure() {
    let (state, _, id, child, key) = recovery_fixture();
    let changed = std::cell::Cell::new(false);
    let error = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "catalog", false),
            |connection, args, timeout| {
                let value = recovery_reader(connection, args, timeout);
                if args.iter().any(|a| a == "--after") {
                    update_evaluation_target(&state, &id, |t| t.evidence_basis += 1);
                    changed.set(true);
                }
                value
            },
            std::time::Instant::now,
        )
        .unwrap_err();
    assert!(changed.get());
    assert!(
        error
            .message
            .contains("changed while evidence was being read")
    );
    let inner = state.inner.lock().unwrap();
    let target = inner.delegations[inner.find_delegation_index(&id).unwrap()]
        .acceptance_evaluation
        .as_ref()
        .unwrap();
    assert_eq!(target.evidence_basis, 43);
    assert!(acceptance_evidence_unfinished(target).is_none());
    assert!(matches!(
        target.submission,
        AcceptanceEvaluationSubmission::None
    ));
}

#[tokio::test]
async fn budget_limited_evaluator_has_a_recovery_route_bound_to_its_attempt() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    super::super::delegation_support::install_delegation_codex_runtime(
        &state,
        "acceptance-evidence-recovery-runtime",
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let reader = endless_evidence_reader(calls);
    let elapsed = std::cell::Cell::new(Duration::ZERO);
    let start = std::time::Instant::now();
    let response = state
        .request_acceptance_evaluation_until(
            &parent,
            evaluation_request(Some(Agent::Codex)),
            |connection, args, timeout| {
                assert!(timeout >= ENGRAM_MIN_CALL_TIMEOUT);
                if args.first().map(String::as_str) == Some("control-policy") {
                    return Ok(policy_receipt(Some(&["independent_session"])));
                }
                let result = reader(connection, args, timeout);
                if args.iter().any(|arg| arg == "--after") {
                    elapsed.set(elapsed.get() + ENGRAM_MIN_CALL_TIMEOUT);
                }
                result
            },
            start + acceptance_evaluation_request_tracker_budget(),
            || start + elapsed.get(),
        )
        .unwrap();
    let wire = serde_json::to_value(response).unwrap();
    assert_eq!(wire["evidenceOmissions"]["unread"]["reason"], "time_budget");
    assert_eq!(wire["evidenceOmissions"]["unread"]["count"], 97);
    let child = wire["childSession"]["id"].as_str().unwrap();
    let id = wire["delegation"]["id"].as_str().unwrap();
    let before = {
        let inner = state.inner.lock().unwrap();
        inner.delegations[inner.find_delegation_index(id).unwrap()]
            .acceptance_evaluation
            .clone()
            .unwrap()
    };
    let app = app_router(state.clone());
    let response = tower::ServiceExt::oneshot(
        app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{child}/acceptance-evidence"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"attemptKey":"another-attempt", "readKey":"catalog-1", "action":"catalog"})
                    .to_string(),
            ))
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let inner = state.inner.lock().unwrap();
    assert_eq!(
        inner.delegations[inner.find_delegation_index(id).unwrap()]
            .acceptance_evaluation
            .as_ref(),
        Some(&before),
        "a wrong key cannot retire the legitimate evaluator or record a verdict"
    );
}
