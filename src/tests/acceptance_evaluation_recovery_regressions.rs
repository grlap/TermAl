// Recovery boundaries found by the whole-change review. These drive real
// request/read/submission owners with scripted tracker replies, not a store.
use super::*;

fn stored_unfinished(path: &FsPath, id: &str) -> Option<AcceptanceUnfinishedEvidenceRead> {
    let connection = rusqlite::Connection::open(path).unwrap();
    let row: String = connection
        .query_row(
            "SELECT value_json FROM delegations WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    let record: DelegationRecord = serde_json::from_str(&row).unwrap();
    record
        .acceptance_evaluation
        .unwrap()
        .attempt_history
        .unwrap()
        .unfinished_evidence
}

#[test]
fn duplicate_catalog_and_record_provenance_fail_closed_with_replay() {
    for fault in ["none", "duplicate", "provenance"] {
        let (state, _, id, child, key) = recovery_fixture();
        let catalog = state
            .read_acceptance_evidence_with_io(
                &child,
                read_request(&key, "page", false),
                |c, a, t| {
                    let mut page = recovery_reader(c, a, t)?;
                    if fault == "duplicate" && a.iter().any(|v| v == "--after") {
                        page["notes"][0] = catalog_note(0);
                    }
                    Ok(page)
                },
                std::time::Instant::now,
            )
            .unwrap();
        let (request, result) = if fault == "duplicate" {
            (read_request(&key, "page", false), catalog)
        } else {
            assert_eq!(catalog["state"], "complete");
            let request = read_request(&key, "body", true);
            let result = state
                .read_acceptance_evidence_with_io(
                    &child,
                    request.clone(),
                    |c, a, t| {
                        let mut body = recovery_reader(c, a, t)?;
                        if fault == "provenance" && a.iter().any(|v| v == "--note") {
                            body["note"]["by"] = json!("another producer");
                        }
                        Ok(body)
                    },
                    std::time::Instant::now,
                )
                .unwrap();
            (request, result)
        };
        if fault == "none" {
            assert_eq!(result["state"], "complete");
            continue;
        }
        assert_eq!(result["state"], "unfinished", "{fault}");
        assert_eq!(result["recorded"], false);
        assert!(result.get("records").is_none() && result.get("catalog").is_none());
        let replay = state
            .read_acceptance_evidence_with_io(
                &child,
                request,
                |_, _, _| panic!("failure replay cannot issue IO"),
                std::time::Instant::now,
            )
            .unwrap();
        assert_eq!(result, replay);
        assert!(stored_unfinished(state.persistence_path.as_ref(), &id).is_some());
    }
}

#[test]
fn selected_reads_leave_every_optional_discovery_slot_available() {
    let (state, _, id, child, key) = recovery_fixture_with_indexed("independent_session", true);
    let start = std::time::Instant::now();
    let selected = read_request(&key, "selected", true);
    let selected_result = state
        .read_acceptance_evidence_with_io(
            &child,
            selected.clone(),
            |_, _, _| panic!("selected proof must reuse the captured record"),
            || start,
        )
        .unwrap();
    for index in 0..MAX_ACCEPTANCE_BRIEF_EVIDENCE_ENTRIES {
        let page = state
            .read_acceptance_evidence_with_io(
                &child,
                read_request(&key, &format!("catalog-{index}"), false),
                recovery_reader,
                || start,
            )
            .unwrap();
        assert_eq!(page["state"], "complete");
    }
    let exhausted = read_request(&key, "over-cap", false);
    let read_without_io = |request| {
        state
            .read_acceptance_evidence_with_io(
                &child,
                request,
                |_, _, _| panic!("a capped or replayed read cannot issue I/O"),
                || start,
            )
            .unwrap()
    };
    let result = read_without_io(exhausted.clone());
    assert_eq!(result["state"], "exhausted");
    assert_eq!(result, read_without_io(exhausted));
    assert_eq!(read_without_io(selected), selected_result);
    assert!(stored_unfinished(state.persistence_path.as_ref(), &id).is_none());
}

#[test]
fn successful_evidence_replies_are_withheld_when_local_scope_changes() {
    for change in ["naming", "reader"] {
        let (state, parent, id, child, key) =
            recovery_fixture_with_indexed("independent_session", true);
        let selected = read_request(&key, "selected-before-move", true);
        let catalog = read_request(&key, "catalog-before-move", false);
        for request in [&selected, &catalog] {
            let result = state
                .read_acceptance_evidence_with_io(
                    &child,
                    request.clone(),
                    recovery_reader,
                    std::time::Instant::now,
                )
                .unwrap();
            assert_eq!(result["state"], "complete");
        }
        {
            let mut inner = state.inner.lock().unwrap();
            if change == "naming" {
                let target = inner.delegations[inner.find_delegation_index(&id).unwrap()]
                    .acceptance_evaluation
                    .as_ref()
                    .unwrap();
                let store = target.store.clone().unwrap();
                let work = target.naming_history.as_ref().unwrap().work_id.clone();
                inner
                    .engram_work_naming_history
                    .iter_mut()
                    .find(|history| history.store == store && history.work_id == work)
                    .unwrap()
                    .revision += 1;
            } else {
                let project = inner.sessions[inner.find_session_index(&parent).unwrap()]
                    .session
                    .project_id
                    .clone()
                    .unwrap();
                inner
                    .projects
                    .iter_mut()
                    .find(|p| p.id == project)
                    .unwrap()
                    .engram = None;
            }
        }
        for request in [selected, catalog] {
            let refusal = state
                .read_acceptance_evidence_with_io(
                    &child,
                    request,
                    |_, _, _| panic!("replay must not issue tracker I/O"),
                    std::time::Instant::now,
                )
                .expect_err("a cached result cannot be served after its authority moves");
            assert_eq!(
                refusal.status,
                StatusCode::CONFLICT,
                "{change}: {refusal:?}"
            );
        }
    }
}

#[test]
fn replay_probes_without_holding_state_and_rechecks_naming_before_exposing_evidence() {
    for selected in [true, false] {
        let (state, _, id, child, key) = recovery_fixture_with_indexed("independent_session", true);
        let request = read_request(&key, "replay-after-probe", selected);
        let complete = state
            .read_acceptance_evidence_with_io(
                &child,
                request.clone(),
                recovery_reader,
                std::time::Instant::now,
            )
            .unwrap();
        assert_eq!(complete["state"], "complete");

        let probes = std::cell::Cell::new(0);
        let refusal = state
            .replay_acceptance_evidence_with_probe(&child, request, |reader, lookup_path| {
                probes.set(probes.get() + 1);
                assert!(
                    state.inner.is_not_held_by_current_thread_for_test(),
                    "probe must leave state unlocked"
                );
                validate_work_read_target_at(reader, lookup_path)?;
                let mut inner = state.inner.lock().unwrap();
                let target = inner.delegations[inner.find_delegation_index(&id).unwrap()]
                    .acceptance_evaluation
                    .as_ref()
                    .unwrap();
                let work = target.naming_history.as_ref().unwrap().work_id.clone();
                inner
                    .engram_work_naming_history
                    .iter_mut()
                    .find(|history| history.store == reader.store && history.work_id == work)
                    .unwrap()
                    .revision += 1;
                Ok(())
            })
            .expect_err("a scope change during the probe must withhold the cached response");
        assert_eq!(probes.get(), 1);
        assert_eq!(refusal.status, StatusCode::CONFLICT);
        assert!(refusal.message.contains("source naming scope changed"));
        assert!(stored_unfinished(state.persistence_path.as_ref(), &id).is_none());
    }
}

#[test]
fn mixed_record_read_charges_discovery_and_reuses_the_selected_body() {
    let (state, _, _, child, key) = recovery_fixture_with_indexed("independent_session", true);
    let selected = read_request(&key, "selected-before-mixed", true);
    let read_selected = || {
        state
            .read_acceptance_evidence_with_io(
                &child,
                selected.clone(),
                |_, _, _| panic!("selected evidence does not issue tracker I/O"),
                std::time::Instant::now,
            )
            .unwrap()
    };
    let before = read_selected();
    let mut request = read_request(&key, "mixed-records", true);
    let discovered = catalog_note(1);
    let locator = discovered["locator"].as_str().unwrap();
    request.locators.push(locator.to_owned());
    let elapsed = std::cell::Cell::new(Duration::ZERO);
    let calls = std::cell::RefCell::new(Vec::new());
    let start = std::time::Instant::now();
    let response = state
        .read_acceptance_evidence_with_io(
            &child,
            request,
            |connection, args, timeout| {
                elapsed.set(elapsed.get() + Duration::from_secs(1));
                if let Some(index) = args.iter().position(|arg| arg == "--note") {
                    assert_eq!(args[index + 1], locator);
                    calls.borrow_mut().push("note");
                    Ok(json!({"work_ref":"w-task", "note":discovered}))
                } else {
                    assert!(args.iter().any(|arg| arg == "inspect"));
                    calls.borrow_mut().push("inspect");
                    recovery_reader(connection, args, timeout)
                }
            },
            || start + elapsed.get(),
        )
        .unwrap();
    assert_eq!(*calls.borrow(), ["inspect", "note", "inspect"]);
    assert_eq!(response["state"], "complete");
    assert_eq!(response["records"][0]["note"], catalog_note(3));
    assert_eq!(response["records"][1]["note"], discovered);
    assert_eq!(
        response["remainingReadMillis"],
        (acceptance_evaluation_evidence_budget() - elapsed.get()).as_millis() as u64
    );
    assert_eq!(
        read_selected(),
        before,
        "replay preserves its original budget"
    );
}

#[test]
fn a_loaded_retained_brief_can_be_redelivered_by_the_host_then_reports_lost_state() {
    let (state, _, id, child, key) = recovery_fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&id).unwrap();
        inner.delegations[index].status = DelegationStatus::Completed;
        let prompt = "Read the omitted evidence using this retained attempt.".to_owned();
        inner.delegations[index]
            .acceptance_evaluation
            .as_mut()
            .unwrap()
            .attempt_history
            .as_mut()
            .unwrap()
            .prepared_brief = Some(AcceptanceEvaluationPreparedBrief {
            attempt_key: key.clone(),
            digest: acceptance_evaluation_payload_digest(&[prompt.clone()]),
            prompt,
            offered: false,
        });
        let serialized = serde_json::to_vec(&inner.delegations[index]).unwrap();
        inner.delegations[index] = serde_json::from_slice(&serialized).unwrap();
        inner.acceptance_evidence_recovery.clear();
        let record = &inner.delegations[index];
        assert!(
            acceptance_evaluation_followup_admission_with_origin_locked(&inner, record, false)
                .is_err()
        );
        acceptance_evaluation_followup_admission_with_origin_locked(&inner, record, true).unwrap();
        // Delivery may now rearm the saved attempt without inventing a cache.
        inner.delegations[index].status = DelegationStatus::Running;
    }
    let result = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "after-restart", false),
            |_, _, _| panic!("a lost cache cannot restart discovery"),
            std::time::Instant::now,
        )
        .unwrap();
    assert_eq!(result["state"], "unfinished");
    assert_eq!(result["recorded"], false);
    assert!(stored_unfinished(state.persistence_path.as_ref(), &id).is_some());
}

#[test]
fn reusing_an_older_delegation_supersedes_a_newer_unfinished_notice() {
    let (state, parent, first, _, _) = recovery_fixture();
    set_delegation_status(&state, &first, DelegationStatus::Completed);
    let AcceptanceEvaluationRequestResponse::Spawned {
        delegation: second, ..
    } = spawn_request(&state, &parent).unwrap()
    else {
        panic!("second evaluator")
    };
    let second_key = second
        .delegation
        .acceptance_evaluation
        .as_ref()
        .unwrap()
        .attempt_key
        .clone();
    state
        .inner
        .lock()
        .unwrap()
        .acceptance_evidence_recovery
        .clear();
    // Model a saved recovery-enabled attempt whose runtime cache was lost.
    // Admission and submission still use the real owner paths below.
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&second.delegation.id).unwrap();
        inner.delegations[index]
            .acceptance_evaluation
            .as_mut()
            .unwrap()
            .attempt_history
            .as_mut()
            .unwrap()
            .evidence_recovery_enabled = true;
    }
    state
        .read_acceptance_evidence_with_io(
            &second.delegation.child_session_id,
            read_request(&second_key, "lost", false),
            |_, _, _| panic!("runtime state lost"),
            std::time::Instant::now,
        )
        .unwrap();
    let mut request = evaluation_request(Some(Agent::Codex));
    request.reuse_delegation_id = Some(first.clone());
    let response = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            request,
            fixture_reader(
                Arc::default(),
                show_receipt(None),
                Ok(policy_receipt(Some(&["independent_session"]))),
            ),
        )
        .unwrap();
    let AcceptanceEvaluationRequestResponse::Spawned {
        delegation: reused,
        reused: true,
        ..
    } = response
    else {
        panic!("host must reuse the older evaluator for this control")
    };
    assert_eq!(reused.delegation.id, first);
    let target = reused.delegation.acceptance_evaluation.as_ref().unwrap();
    let mut submit = two_verdicts();
    submit.attempt_key = Some(target.attempt_key.clone());
    let mut receipt: Value = serde_json::from_str(&evaluate_receipt(false)).unwrap();
    receipt["evaluation"]["attempt_key"] = json!(target.attempt_key);
    receipt["evaluation"]["work_revision"] = json!(7);
    receipt["evaluation"]["evaluated_cut"] = json!(42);
    receipt["work"]["revision"] = json!(7);
    state
        .submit_acceptance_evaluation_with_runner(
            &reused.delegation.child_session_id,
            submit,
            |_, _, _| Ok(cli_output(true, &receipt.to_string(), "")),
        )
        .unwrap();
    let inner = state.inner.lock().unwrap();
    assert!(
        acceptance_unfinished_evidence_notice_locked(
            &inner,
            target.store.as_ref().unwrap(),
            &target.work_ref
        )
        .is_empty(),
        "the recorded reused attempt is later than the second delegation"
    );
}

#[test]
fn unfinished_recovery_waits_for_the_writer_and_retries_the_same_reason_after_failure() {
    for fail_first in [false, true] {
        let (mut state, _, id, child, key) = recovery_fixture();
        {
            let inner = state.inner.lock().unwrap();
            persist_state(state.persistence_path.as_ref(), &inner).unwrap();
        }
        let mut writer = SteppedPersistWriter::attach(&mut state);
        writer.write();
        state
            .inner
            .lock()
            .unwrap()
            .acceptance_evidence_recovery
            .clear();
        let request = read_request(&key, "durable-loss", false);
        std::thread::scope(|scope| {
            let read = scope.spawn(|| {
                state.read_acceptance_evidence_with_io(
                    &child,
                    request.clone(),
                    |_, _, _| panic!("lost state cannot issue IO"),
                    std::time::Instant::now,
                )
            });
            writer.receive_fence();
            assert!(
                !read.is_finished(),
                "terminal reply must wait for the actual writer"
            );
            assert!(
                stored_unfinished(&writer.path, &id).is_none(),
                "a reload before ACK still sees the original attempt"
            );
            let expected = {
                let inner = state.inner.lock().unwrap();
                let target = inner.delegations[inner.find_delegation_index(&id).unwrap()]
                    .acceptance_evaluation
                    .as_ref()
                    .unwrap();
                let pending = acceptance_evidence_unfinished(target).unwrap().clone();
                assert!(!pending.persistence_confirmed);
                assert!(
                    refuse_second_active_acceptance_evaluator_locked(
                        &inner,
                        target.store.as_ref().unwrap(),
                        &target.work_ref,
                        None
                    )
                    .is_err(),
                    "replacement must wait for durable unfinished state"
                );
                pending
            };
            let result = if fail_first {
                writer.fail_write();
                let error = read.join().unwrap().unwrap_err();
                assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
                assert!(stored_unfinished(&writer.path, &id).is_none());
                let retry = scope.spawn(|| {
                    state.read_acceptance_evidence_with_io(
                        &child,
                        request.clone(),
                        |_, _, _| panic!("retry cannot reread tracker"),
                        std::time::Instant::now,
                    )
                });
                writer.receive_fence();
                assert!(!retry.is_finished());
                writer.write();
                retry.join().unwrap().unwrap()
            } else {
                writer.write();
                read.join().unwrap().unwrap()
            };
            assert_eq!(result["state"], "unfinished");
            assert_eq!(result["unfinishedEvidence"]["reason"], expected.reason);
            let disk = stored_unfinished(&writer.path, &id).unwrap();
            assert_eq!(disk.reason, expected.reason);
            assert_eq!(disk.read_key, request.read_key);
            assert_eq!(disk.intent_digest, expected.intent_digest);
            assert!(
                disk.persistence_confirmed,
                "loaded history is already durable"
            );
            let replay = state
                .read_acceptance_evidence_with_io(
                    &child,
                    request.clone(),
                    |_, _, _| panic!("ACKed replay issues no IO"),
                    std::time::Instant::now,
                )
                .unwrap();
            assert_eq!(result, replay);
        });
    }
}

#[test]
fn selected_older_proof_can_reappear_on_a_catalog_page() {
    let (state, _, id, child, key) = recovery_fixture_with_indexed("independent_session", true);
    // Parent-selected proof is already readable without paging the catalog.
    let before = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "selected-body", true),
            recovery_reader,
            std::time::Instant::now,
        )
        .unwrap();
    let page = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "catalog", false),
            recovery_reader,
            std::time::Instant::now,
        )
        .unwrap();
    assert_eq!(page["state"], "complete");
    assert_eq!(page["catalog"][0]["locator"], OLDER_PROOF);
    assert_eq!(page["remainingUnread"], 0);
    let after = state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "catalog-body", true),
            recovery_reader,
            std::time::Instant::now,
        )
        .unwrap();
    assert_eq!(before["records"], after["records"]);
    let inner = state.inner.lock().unwrap();
    let target = inner.delegations[inner.find_delegation_index(&id).unwrap()]
        .acceptance_evaluation
        .as_ref()
        .unwrap();
    assert!(acceptance_evidence_unfinished(target).is_none());
}

#[test]
fn local_scope_movement_is_unfinished_replayable_and_allows_a_fresh_parent_request() {
    for fault in ["none", "naming", "reader", "binding"] {
        let (state, parent, id, child, key) = recovery_fixture();
        let (project, store, settings, work) = {
            let inner = state.inner.lock().unwrap();
            let project = inner.sessions[inner.find_session_index(&parent).unwrap()]
                .session
                .project_id
                .clone()
                .unwrap();
            let target = inner.delegations[inner.find_delegation_index(&id).unwrap()]
                .acceptance_evaluation
                .as_ref()
                .unwrap();
            let settings = inner
                .projects
                .iter()
                .find(|p| p.id == project)
                .unwrap()
                .engram
                .clone();
            (
                project,
                target.store.clone().unwrap(),
                settings,
                target.naming_history.as_ref().unwrap().work_id.clone(),
            )
        };
        let changed = std::cell::Cell::new(false);
        let request = read_request(&key, "scope-race", false);
        let result = state
            .read_acceptance_evidence_with_io(
                &child,
                request.clone(),
                |connection, args, timeout| {
                    let reply = recovery_reader(connection, args, timeout);
                    if args.iter().any(|a| a == "--after") {
                        let mut inner = state.inner.lock().unwrap();
                        match fault {
                            "naming" => {
                                inner
                                    .engram_work_naming_history
                                    .iter_mut()
                                    .find(|h| h.store == store && h.work_id == work)
                                    .unwrap()
                                    .revision += 1
                            }
                            "binding" => {
                                inner
                                    .projects
                                    .iter_mut()
                                    .find(|p| p.id == project)
                                    .unwrap()
                                    .engram
                                    .as_mut()
                                    .unwrap()
                                    .home = Some("C:/another-engram-store".to_owned());
                            }
                            "reader" => {
                                inner
                                    .projects
                                    .iter_mut()
                                    .find(|p| p.id == project)
                                    .unwrap()
                                    .engram = None
                            }
                            _ => (),
                        }
                        changed.set(fault != "none");
                    }
                    reply
                },
                std::time::Instant::now,
            )
            .unwrap();
        assert_eq!(changed.get(), fault != "none");
        if fault == "none" {
            assert_eq!(result["state"], "complete");
            continue;
        }
        assert_eq!(result["state"], "unfinished", "{fault}: {result}");
        assert_eq!(result["recorded"], false);
        assert!(
            result.get("catalog").is_none(),
            "stale-scope evidence stays private"
        );
        let replay = state
            .read_acceptance_evidence_with_io(
                &child,
                request,
                |_, _, _| panic!("replay must work even while the local scope is stale"),
                std::time::Instant::now,
            )
            .unwrap();
        assert_eq!(result, replay);
        {
            let inner = state.inner.lock().unwrap();
            let target = inner.delegations[inner.find_delegation_index(&id).unwrap()]
                .acceptance_evaluation
                .as_ref()
                .unwrap();
            let retained = acceptance_evidence_unfinished(target).unwrap();
            assert_eq!(
                retained.reason,
                result["unfinishedEvidence"]["reason"].as_str().unwrap()
            );
            assert!(matches!(
                target.submission,
                AcceptanceEvaluationSubmission::None
            ));
        }
        // Restore the reader, not the attempt's stale naming token. A fresh
        // request takes its own current scope and the old child cannot block it.
        state
            .inner
            .lock()
            .unwrap()
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram = settings;
        let fresh =
            spawn_request(&state, &parent).expect("unfinished original no longer blocks admission");
        let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = fresh else {
            panic!("fresh evaluator")
        };
        assert_ne!(delegation.delegation.id, id);
        assert!(delegation.delegation.prompt.contains(&key));
    }
}

#[test]
fn an_attempt_without_a_manifest_keeps_the_missing_evidence_verdict_route() {
    for identity in [false, true] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        crate::tests::delegation_support::install_delegation_codex_runtime(
            &state,
            "legacy-evidence-evaluator",
        );
        let mut show = show_receipt(None);
        show["notes_omitted"] = json!(3);
        let base = fixture_reader(
            Arc::default(),
            show,
            Ok(policy_receipt(Some(&["independent_session"]))),
        );
        let response = state.request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(Some(Agent::Codex)),
            |connection, args, timeout| {
                if !identity && args.iter().any(|a| a == "inspect") {
                    Ok(json!({"status":{"work":{"short_ref":"w-task"}}}))
                } else {
                    base(connection, args, timeout)
                }
            },
        );
        if !identity {
            let error = response
                .err()
                .expect("canonical identity remains required for admission");
            assert_eq!(error.status, StatusCode::CONFLICT);
            assert!(state.inner.lock().unwrap().delegations.is_empty());
            continue;
        }
        let response = response.unwrap();
        let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response else {
            panic!("legacy evaluator")
        };
        let record = delegation.delegation;
        let target = record.acceptance_evaluation.as_ref().unwrap();
        assert!(
            !record
                .prompt
                .contains(TERMAL_READ_ACCEPTANCE_EVIDENCE_TOOL_NAME),
            "a missing manifest must not send the evaluator to an unavailable recovery route"
        );
        {
            let inner = state.inner.lock().unwrap();
            assert!(
                !inner
                    .acceptance_evidence_recovery
                    .contains_key(&target.attempt_key)
            );
            assert!(!delegation_control_plane_capability_allowed_locked(
                &inner,
                &record.child_session_id,
                DelegationControlPlaneCapability::ReadAcceptanceEvidence
            ));
        }
        state
            .read_acceptance_evidence_with_io(
                &record.child_session_id,
                read_request(&target.attempt_key, "unsupported", false),
                |_, _, _| panic!("no recovery was enabled"),
                std::time::Instant::now,
            )
            .unwrap_err();
        let mut submit = submission(
            (1..=2)
                .map(|i| {
                    verdict(
                        i,
                        "insufficient_evidence",
                        "Older evidence was not shown.",
                        &[],
                    )
                })
                .collect(),
        );
        submit.attempt_key = Some(target.attempt_key.clone());
        let mut receipt: Value = serde_json::from_str(&evaluate_receipt(false)).unwrap();
        receipt["evaluation"]["attempt_key"] = json!(target.attempt_key);
        receipt["evaluation"]["work_revision"] = json!(7);
        receipt["evaluation"]["evaluated_cut"] = json!(42);
        receipt["work"]["revision"] = json!(7);
        receipt["evaluation"]["passed"] = json!(0);
        receipt["evaluation"]["blocking"] = json!({"criterion":"The route exists", "position":1, "verdict":"insufficient_evidence"});
        for row in receipt["evaluation"]["verdicts"].as_array_mut().unwrap() {
            row["verdict"] = json!("insufficient_evidence");
            row["citations"] = json!(0);
        }
        let writes = AtomicUsize::new(0);
        state
            .submit_acceptance_evaluation_with_runner(
                &record.child_session_id,
                submit,
                |_, _, _| {
                    writes.fetch_add(1, Ordering::SeqCst);
                    Ok(cli_output(true, &receipt.to_string(), ""))
                },
            )
            .unwrap();
        assert_eq!(writes.load(Ordering::SeqCst), 1);
        assert!(matches!(
            submission_of(&state, &record.id),
            AcceptanceEvaluationSubmission::Recorded { .. }
        ));
        let inner = state.inner.lock().unwrap();
        assert!(
            acceptance_evidence_unfinished(
                inner.delegations[inner.find_delegation_index(&record.id).unwrap()]
                    .acceptance_evaluation
                    .as_ref()
                    .unwrap()
            )
            .is_none()
        );
    }
}

#[test]
fn settled_attempts_without_a_cache_refuse_requester_rearming_before_delivery() {
    let (state, parent, id, _, key) = recovery_fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&id).unwrap();
        inner.delegations[index].status = DelegationStatus::Completed;
        inner.sync_running_read_only_delegation_index(index);
        assert!(!inner.acceptance_evidence_recovery.contains_key(&key));
    }
    let error = state
        .followup_delegation(&parent, &id, "Read the omitted proof".to_owned())
        .err()
        .unwrap();
    assert!(
        error
            .message
            .contains("no longer has retained evidence recovery state")
    );
    let inner = state.inner.lock().unwrap();
    let record = &inner.delegations[inner.find_delegation_index(&id).unwrap()];
    assert_eq!(record.status, DelegationStatus::Completed);
    assert!(
        acceptance_evidence_unfinished(record.acceptance_evaluation.as_ref().unwrap()).is_none()
    );
}

#[test]
fn a_later_recorded_evaluation_supersedes_the_unfinished_predecessor_notice() {
    let (state, parent, _, child, key) = recovery_fixture();
    state
        .inner
        .lock()
        .unwrap()
        .acceptance_evidence_recovery
        .clear();
    state
        .read_acceptance_evidence_with_io(
            &child,
            read_request(&key, "lost", false),
            |_, _, _| panic!("lost state"),
            std::time::Instant::now,
        )
        .unwrap();
    let AcceptanceEvaluationRequestResponse::Spawned {
        delegation: second, ..
    } = spawn_request(&state, &parent).unwrap()
    else {
        panic!("replacement evaluator")
    };
    assert!(second.delegation.prompt.contains(&key));
    let target = second.delegation.acceptance_evaluation.as_ref().unwrap();
    let mut request = two_verdicts();
    request.attempt_key = Some(target.attempt_key.clone());
    let mut receipt: Value = serde_json::from_str(&evaluate_receipt(false)).unwrap();
    receipt["evaluation"]["attempt_key"] = json!(target.attempt_key);
    receipt["evaluation"]["work_revision"] = json!(7);
    receipt["evaluation"]["evaluated_cut"] = json!(42);
    receipt["work"]["revision"] = json!(7);
    state
        .submit_acceptance_evaluation_with_runner(
            &second.delegation.child_session_id,
            request,
            |_, _, _| Ok(cli_output(true, &receipt.to_string(), "")),
        )
        .unwrap();
    set_delegation_status(&state, &second.delegation.id, DelegationStatus::Completed);
    let AcceptanceEvaluationRequestResponse::Spawned {
        delegation: third, ..
    } = spawn_request(&state, &parent).unwrap()
    else {
        panic!("later evaluator")
    };
    assert!(
        !third.delegation.prompt.contains(&key),
        "a recorded successor ends the old notice"
    );
}
