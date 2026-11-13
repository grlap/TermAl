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
                    show["notes"] = catalog_page(0)["notes"].clone();
                    show["notes_window"] = catalog_page(0)["notes_window"].clone();
                    show["notes_omitted"] = json!(3);
                    Ok(show)
                }
            },
            start + acceptance_evaluation_request_tracker_budget(),
            || start + elapsed.get(),
        )
        .unwrap();
    let wire = serde_json::to_value(&response).unwrap();
    assert_eq!(wire["evidenceOmissions"]["unread"]["reason"], "time_budget");
    assert_eq!(wire["evidenceOmissions"]["unread"]["count"], 1);
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

#[test]
fn exhausted_allowance_is_an_unfinished_attempt_not_a_verdict_and_parent_can_request_again() {
    let (state, parent, id, child, key) = recovery_fixture();
    let elapsed = std::cell::Cell::new(Duration::ZERO);
    let start = std::time::Instant::now();
    let calls = AtomicUsize::new(0);
    let request = read_request(&key, "slow-catalog", false);
    let result = state
        .read_acceptance_evidence_with_io(
            &child,
            request.clone(),
            |connection, args, timeout| {
                calls.fetch_add(1, Ordering::SeqCst);
                let result = recovery_reader(connection, args, timeout);
                elapsed.set(elapsed.get() + ENGRAM_MIN_CALL_TIMEOUT);
                result
            },
            || start + elapsed.get(),
        )
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2, "no unfunded closing read");
    assert_eq!(result["state"], "unfinished");
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
    let mut submit = two_verdicts();
    submit.attempt_key = Some(key.clone());
    assert!(
        state
            .submit_acceptance_evaluation_with_runner(&child, submit, runner_must_not_run)
            .unwrap_err()
            .message
            .contains("ended without a verdict")
    );
    {
        let inner = state.inner.lock().unwrap();
        let target = inner.delegations[inner.find_delegation_index(&id).unwrap()]
            .acceptance_evaluation
            .as_ref()
            .unwrap();
        assert!(matches!(
            target.submission,
            AcceptanceEvaluationSubmission::None
        ));
        assert!(
            acceptance_evaluation_outcome_line(target)
                .contains(result["unfinishedEvidence"]["reason"].as_str().unwrap())
        );
        assert_eq!(
            target
                .client_view()
                .attempt_history
                .as_ref()
                .unwrap()
                .unfinished_evidence,
            target.attempt_history.as_ref().unwrap().unfinished_evidence
        );
        let round_trip: DelegationAcceptanceEvaluation =
            serde_json::from_value(serde_json::to_value(target).unwrap()).unwrap();
        assert_eq!(
            acceptance_evidence_unfinished(&round_trip)
                .unwrap()
                .attempt_key,
            key
        );
        assert!(
            !acceptance_evidence_unfinished(&round_trip)
                .unwrap()
                .recorded
        );
        assert!(
            round_trip
                .attempt_history
                .unwrap()
                .refused_submissions
                .is_empty(),
            "no fabricated refused judgment"
        );
    }
    let response = spawn_request(&state, &parent)
        .expect("unfinished read does not block a fresh parent request");
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response else {
        panic!("fresh child")
    };
    assert_ne!(delegation.delegation.id, id);
    assert!(
        delegation
            .delegation
            .prompt
            .contains(result["unfinishedEvidence"]["reason"].as_str().unwrap())
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
    assert_eq!(proof["state"], "unfinished");
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
