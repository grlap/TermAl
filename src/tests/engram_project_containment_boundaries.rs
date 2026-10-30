// Owns admitted connection, durable-confirmation and lifecycle boundary tests.
// Does not exercise live authority or certify the separate retirement producer.
use super::*;

#[path = "engram_project_containment_settings_races.rs"]
mod settings_races;

fn retain_refused_deletion_duty(state: &AppState, project: &str) {
    // Checkpoint-only debt may outlive deletion. Keep a distinct unresolved
    // grant in the frozen image to exercise the independent-duty blocker.
    {
        let mut inner = state.inner.lock().unwrap();
        let session = inner.sessions.iter().find(|record|
            record.session.project_id.as_deref() == Some(project)).unwrap().session.id.clone();
        inner.session_mut(&session).unwrap().engram.uncertain_grant_id = Some("independent-grant".into());
        state.commit_locked(&mut inner).unwrap();
    }
    let transport =
        ScriptedEngramControlTransport::new([checkpoint_refusal_reply("GrantScopeMismatch")]);
    install_control_only_transport(state, transport);
    state
        .update_project_engram_settings(project, settings(state, project, false))
        .unwrap();
}

#[test]
fn project_containment_delete_normalizes_identity_before_refusing_unresolved_duty() {
    for padded in [false, true] {
        let (state, project, session, _root, receiver) = fixture();
        retain_refused_deletion_duty(&state, &project);
        let before = {
            let inner = state.inner.lock().unwrap();
            serde_json::to_value(&inner.engram_session_obligations).unwrap()
        };
        let supplied = if padded {
            format!("  {project}  ")
        } else {
            project.clone()
        };
        let error = state
            .delete_project(&supplied)
            .err()
            .expect("normalization cannot bypass the unresolved project duty");
        assert_eq!(error.status, StatusCode::CONFLICT);
        let inner = state.inner.lock().unwrap();
        assert!(inner.find_project(&project).is_some());
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.session.project_id.as_deref(), Some(project.as_str()));
        assert_eq!(
            record.engram.active_grant_id.as_deref(),
            Some("retained-grant")
        );
        assert_eq!(
            serde_json::to_value(&inner.engram_session_obligations).unwrap(),
            before
        );
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn project_containment_delete_transactions_recheck_a_published_duty() {
    for with_reset in [false, true] {
        let (state, project, session, _root, receiver) = fixture();
        let snapshot = state
            .inner
            .lock()
            .unwrap()
            .find_project(&project)
            .unwrap()
            .clone();
        // Models a duty published after the public route took its snapshot.
        retain_refused_deletion_duty(&state, &project);
        let current_snapshot = state
            .inner
            .lock()
            .unwrap()
            .find_project(&project)
            .unwrap()
            .clone();
        let result = if with_reset {
            state.delete_project_with_engram_reset(project.clone(), current_snapshot)
        } else {
            state.delete_project_without_engram_reset(&project)
        };
        let error = result
            .err()
            .expect("the locked deletion transaction must recheck duties");
        assert_eq!(error.status, StatusCode::CONFLICT);
        let inner = state.inner.lock().unwrap();
        assert!(inner.find_project(&snapshot.id).is_some());
        assert!(engram_session_obligation_locked(&inner, &session).is_some());
        assert!(!inner.engram_project_resets.contains(&project));
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn project_containment_delete_final_removal_rechecks_a_concurrently_published_duty() {
    let (state, project, session, _root, receiver) = fixture();
    let retained = {
        let mut inner = state.inner.lock().unwrap();
        let authority = inner
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .authority_store_key
            .as_ref()
            .unwrap()
            .clone();
        let index = inner.find_session_index(&session).unwrap();
        inner.session_mut_by_index(index).unwrap().engram.uncertain_grant_id = Some("independent-grant".into());
        let entry =
            prepare_engram_containment_entry_locked(&inner, &session, &project, &authority, true)
                .unwrap()
                .unwrap()
                .obligation;
        let index = inner.find_session_index(&session).unwrap();
        // The late metadata duty is independent of this now-idle local row.
        inner.sessions[index].engram = EngramSessionState::default();
        state.commit_locked(&mut inner).unwrap();
        entry
    };
    let gate = gate_next_engram_project_reset_fence(&project);
    let deleting_state = state.clone();
    let deleting_project = project.clone();
    let deleting = std::thread::spawn(move || {
        let result = deleting_state.delete_project(&deleting_project);
        abort_engram_project_reset_fence_gate(&deleting_project, &fence_gate_detail(&result));
        result
    });
    gate.wait_until_entered();
    {
        let mut inner = state.inner.lock().unwrap();
        inner.engram_session_obligations = inner
            .engram_session_obligations
            .with_entry(retained.clone())
            .unwrap();
        state.commit_locked(&mut inner).unwrap();
    }
    gate.release();
    let error = deleting
        .join()
        .unwrap()
        .err()
        .expect("a duty published after reservation must prevent final removal");
    assert_eq!(error.status, StatusCode::CONFLICT);
    let inner = state.inner.lock().unwrap();
    assert!(inner.find_project(&project).is_some());
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.session.project_id.as_deref(), Some(project.as_str()));
    assert!(
        inner
            .engram_session_obligations
            .entries
            .iter()
            .any(|entry| entry.id == retained.id)
    );
    assert!(!inner.engram_project_resets.contains(&project));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_public_patch_preserves_the_default_path_command() {
    let (state, project, _session, root, _receiver) = fixture();
    let _readiness = stage_test_engram_readiness_without_launch(&[&root]);
    {
        let mut inner = state.inner.lock().unwrap();
        inner.preferences.engram.binary_path = default_engram_binary_path();
        inner.preferences.engram.home = root.to_string_lossy().into_owned();
        let settings = inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram
            .as_mut()
            .unwrap();
        settings.binary_path = Some(default_engram_binary_path());
        settings.home = Some(root.to_string_lossy().into_owned());
        settings.deadline_ms = None;
        state.commit_locked(&mut inner).unwrap();
    }
    let transport =
        ScriptedEngramControlTransport::new([checkpoint_refusal_reply("GrantScopeMismatch")]);
    install_control_only_transport(&state, transport.clone());
    let request = serde_json::from_value(json!({"enabled":true,"turnGatedControl":false})).unwrap();
    let response = state
        .patch_project_engram_settings(&project, request)
        .unwrap();
    assert_eq!(response.engram_control_recovery.len(), 1);
    let inner = state.inner.lock().unwrap();
    assert!(
        !inner
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .turn_gated_control
    );
    assert_eq!(
        inner.engram_session_obligations.entries[0]
            .connection
            .binary_path,
        PathBuf::from("engram")
    );
    assert_eq!(
        transport.requests()[0].connection.binary_path,
        PathBuf::from("engram")
    );
}

#[test]
fn project_containment_local_invalid_authority_refuses_even_without_affected_sessions() {
    for affected in [false, true] {
        for invalid in ["home", "marker", "store_identity"] {
            let (state, project, session, root, receiver) = fixture();
            {
                let mut inner = state.inner.lock().unwrap();
                if !affected {
                    let index = inner.find_session_index(&session).unwrap();
                    inner.sessions[index].engram = EngramSessionState::default();
                }
                let settings = inner
                    .projects
                    .iter_mut()
                    .find(|p| p.id == project)
                    .unwrap()
                    .engram
                    .as_mut()
                    .unwrap();
                if invalid == "home" {
                    settings.home = Some(root.join("missing-home").to_string_lossy().into_owned());
                } else if invalid == "store_identity" {
                    settings.authority_store_key.as_mut().unwrap().project_id =
                        "different-store".into();
                }
                state.commit_locked(&mut inner).unwrap();
            }
            if invalid == "marker" {
                fs::remove_file(root.join(".engram-project")).unwrap();
            }
            let transport = ScriptedEngramControlTransport::new([]);
            install_control_only_transport(&state, transport.clone());
            assert!(
                state
                    .update_project_engram_settings(&project, settings(&state, &project, false))
                    .is_err(),
                "{invalid}/{affected}"
            );
            let inner = state.inner.lock().unwrap();
            assert!(
                inner
                    .find_project(&project)
                    .unwrap()
                    .engram
                    .as_ref()
                    .unwrap()
                    .turn_gated_control
            );
            assert!(inner.engram_session_obligations.entries.is_empty());
            assert!(!inner.engram_project_resets.contains(&project));
            assert!(transport.requests().is_empty());
            assert!(receiver.try_recv().is_err());
        }
    }
}

#[test]
fn project_containment_remote_codes_cannot_poison_the_store_or_public_diagnostics() {
    for code in [
        String::new(),
        " \n".into(),
        "bad\u{1b}code".into(),
        "x".repeat(65_537),
        "GrantScopeMismatch".into(),
    ] {
        let (state, project, session, _root, _receiver) = fixture();
        let transport = ScriptedEngramControlTransport::new([checkpoint_refusal_reply(&code)]);
        install_control_only_transport(&state, transport);
        let response = state
            .update_project_engram_settings(&project, settings(&state, &project, false))
            .unwrap();
        let expected = if code == "GrantScopeMismatch" {
            code.as_str()
        } else {
            "checkpoint_failed"
        };
        assert_eq!(response.engram_control_recovery[0].code, expected);
        let failure = EngramTransportError::remote(EngramControlErrorBody {
            code: code.clone(),
            message: "private producer output".into(),
        });
        let diagnostic = engram_project_checkpoint_failure(&project, &session, &failure);
        assert!(diagnostic.contains(&format!("code={expected}")));
        assert!(!diagnostic.contains("private producer output"));
        assert!(!diagnostic.contains('\u{1b}'));
        state.shutdown_persist_blocking();
        let loaded = load_state(state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        loaded.engram_session_obligations.validate().unwrap();
        assert_eq!(
            loaded.engram_session_obligations.entries[0].reason_code,
            expected
        );
    }
}

#[test]
fn project_containment_confirmed_release_survives_token_rotation_and_restart() {
    let (state, project, session, _root, _receiver) = fixture();
    let transport = ScriptedEngramControlTransport::new([checkpoint_reply("retained-grant")]);
    install_control_only_transport(&state, transport);
    state
        .update_project_engram_settings(&project, settings(&state, &project, false))
        .unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.engram.routing_token = Some("later-accepted-binding-token".into());
        assert!(engram_session_obligation_locked(&inner, &session).is_none());
        state.commit_locked(&mut inner).unwrap();
    }
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(!loaded.engram_confirmed_containment_ids.is_empty());
    assert!(engram_session_obligation_locked(&loaded, &session).is_none());
    assert!(!engram_project_has_unresolved_obligations(
        &loaded, &project
    ));
}

#[test]
fn project_containment_store_ack_alone_never_releases_an_unsettled_grant() {
    let (state, project, session, _root, _receiver) = fixture();
    let transport =
        ScriptedEngramControlTransport::new([checkpoint_refusal_reply("GrantScopeMismatch")]);
    install_control_only_transport(&state, transport);
    state
        .update_project_engram_settings(&project, settings(&state, &project, false))
        .unwrap();
    let mut inner = state.inner.lock().unwrap();
    let image =
        EngramProjectContainmentFence::capture(&inner, &project, &[session.clone()]).unwrap();
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    assert!(image.is_already_durable(&connection).unwrap());
    assert!(engram_session_obligation_locked(&inner, &session).is_some());
    // Even a matching terminal image stays excluded before its final writer ACK.
    inner.engram_session_obligations.entries[0].phase = EngramSessionObligationPhase::Confirmed;
    inner.engram_session_obligations.entries[0].settlement_evidence = json!({
        "operation":"turn_checkpoint", "response":{"decision":"checkpointed",
        "receipt":{"grant_id":"retained-grant", "cursor":1, "confirmed_cursor":1}}
    });
    let index = inner.find_session_index(&session).unwrap();
    inner.sessions[index].engram.active_grant_id = None;
    assert!(engram_containment_confirmation_materialized(
        &inner,
        &inner.engram_session_obligations.entries[0]
    ));
    assert!(engram_session_obligation_locked(&inner, &session).is_some());
    let unacknowledged =
        EngramProjectContainmentFence::capture(&inner, &project, &[session.clone()]).unwrap();
    assert!(!unacknowledged.is_already_durable(&connection).unwrap());
}

#[test]
fn project_containment_excluded_queue_drain_does_not_abort_shared_runtime_exit() {
    let (state, project, session, _root, _receiver) = fixture();
    let transport =
        ScriptedEngramControlTransport::new([checkpoint_refusal_reply("GrantScopeMismatch")]);
    install_control_only_transport(&state, transport);
    state
        .update_project_engram_settings(&project, settings(&state, &project, false))
        .unwrap();
    let healthy = test_session_id(&state, Agent::Codex);
    let process_owner = phase_sync::ParkedProcess::spawn();
    let process = process_owner.process.clone();
    let (runtime, _input_rx, _) =
        test_shared_codex_runtime_with_process("containment-exit", process.clone());
    *state.shared_codex_runtime.lock().unwrap() = Some(runtime.clone());
    {
        let mut inner = state.inner.lock().unwrap();
        for id in [&session, &healthy] {
            let index = inner.find_session_index(id).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                runtime_id: runtime.runtime_id.clone(),
                input_tx: runtime.input_tx.clone(),
                process: process.clone(),
                shared_session: Some(SharedCodexSessionHandle {
                    runtime: runtime.clone(),
                    session_id: id.clone(),
                }),
            });
            record.session.status = SessionStatus::Active;
            record.session.queue_paused = false;
            if id == &session {
                queue_prompt_on_record(
                    record,
                    PendingPrompt {
                        id: "excluded-queued-head".into(),
                        timestamp: stamp_now(),
                        text: "keep held".into(),
                        expanded_text: None,
                        source: None,
                        attachments: Vec::new(),
                        engram_interrupted: false,
                        is_engram_retained: false,
                    },
                    Vec::new(),
                );
            }
        }
        state.commit_locked(&mut inner).unwrap();
    }
    assert!(
        state
            .dispatch_next_queued_turn(&session, false)
            .unwrap()
            .is_none()
    );
    state
        .handle_shared_codex_runtime_exit("containment-exit", Some("fixture runtime exit"))
        .unwrap();
    let inner = state.inner.lock().unwrap();
    for id in [&session, &healthy] {
        let record = &inner.sessions[inner.find_session_index(id).unwrap()];
        assert!(matches!(record.runtime, SessionRuntime::None));
        assert_eq!(record.session.status, SessionStatus::Error);
    }
    assert!(state.shared_codex_runtime.lock().unwrap().is_none());
    assert!(engram_session_obligation_locked(&inner, &session).is_some());
    drop(inner);
    phase_sync::process_exit(&process, "contained shared runtime termination");
    assert!(process.try_wait().unwrap().is_some());
}

#[test]
fn project_containment_stop_interrupts_under_its_own_fence_and_defers_foreign_holds() {
    for hold in ["own", "foreign", "retained_foreign", "session_only"] {
        let (state, project, session, root, receiver) = fixture();
        let clock = state.engram_budget_clock();
        clock.pin_wall();
        let process_owner = phase_sync::ParkedProcess::spawn();
        let process = process_owner.process.clone();
        let (input_tx, _input_rx) = mpsc::channel();
        let settling = create_test_project_session(&state, Agent::Codex, &project, &root);
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                runtime_id: format!("prompt-containment-stop-{hold}"),
                input_tx,
                process: process.clone(),
                shared_session: None,
            });
            record.session.status = SessionStatus::Active;
            queue_prompt_on_record(record, PendingPrompt {
                id: "successor-must-stay-queued".into(), timestamp: stamp_now(),
                text: "Do not run during containment.".into(), expanded_text: None,
                source: None, attachments: Vec::new(), engram_interrupted: false,
                is_engram_retained: false,
            }, Vec::new());
            let index = inner.find_session_index(&settling).unwrap();
            let record = &mut inner.sessions[index];
            record.runtime = SessionRuntime::None;
            record.session.status = SessionStatus::Error;
            record.engram.routing_token = Some("settling-token".into());
            record.engram.active_grant_id = Some("settling-grant".into());
            record.engram.begins_recorded = true;
            state.commit_locked(&mut inner).unwrap();
        }
        let (step, settlement) = gated_engram_step("turn_checkpoint", checkpoint_reply("settling-grant"));
        let transport = GatedEngramControlTransport::new([step]);
        state.install_control_test_transport(transport.clone());
        let (ack_tx, ack_rx) = mpsc::channel();
        let off_state = state.clone();
        let off_project = project.clone();
        let before_ack_state = state.clone();
        let before_ack_session = session.clone();
        let before_ack_process = process.clone();
        let off_settings = settings(&state, &project, false);
        let off = std::thread::spawn(move || {
            TEST_CONTAINMENT_BEFORE_ACK_RETURN.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    before_ack_state.engram_abort_retry_tick(before_ack_state.engram_budget_clock().wall_now());
                    let inner = before_ack_state.inner.lock().unwrap();
                    let record = &inner.sessions[inner.find_session_index(&before_ack_session).unwrap()];
                    ack_tx.send((record.engram_containment_stop.is_none(),
                        !record.runtime_stop_in_progress,
                        before_ack_process.try_wait().unwrap().is_none())).unwrap();
                }));
            });
            off_state.update_project_engram_settings(&off_project, off_settings)
        });
        let settlement = settlement;
        assert_eq!(receive(&ack_rx, "initial containment ACK"), (true, true, true),
            "no Stop is admitted before the initial acknowledgement returns");
        let request = settlement.wait();
        assert_eq!(request.connection.session_id, settling);
        let (owner, due_at) = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            let pending = record.engram_containment_stop.as_ref().unwrap();
            assert!(inner.engram_project_resets.is_owned_by(&project, pending.owner.reset_generation));
            assert!(record.engram.project_reset_in_progress);
            (pending.owner.clone(), pending.due_at.clone())
        };
        assert!(process.try_wait().unwrap().is_none());
        let foreign_generation = if hold == "own" {
            let occupied = (0..ENGRAM_RETRY_MAX_IN_FLIGHT)
                .map(|_| state.engram_retry_slots.try_acquire().unwrap()).collect::<Vec<_>>();
            state.engram_abort_retry_tick(clock.wall_now());
            let age = {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                assert_eq!(record.engram_containment_stop.as_ref().unwrap().attempts, 0);
                record.engram_queue_schedule.eligibility.clone().unwrap()
            };
            state.engram_abort_retry_tick(clock.wall_now());
            {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                assert_eq!(record.engram_queue_schedule.eligibility.as_ref(), Some(&age));
                assert_eq!(record.engram_containment_stop.as_ref().unwrap().due_at, due_at);
            }
            assert!(process.try_wait().unwrap().is_none());
            drop(occupied);
            state.engram_abort_retry_tick(clock.wall_now());
            assert!(process.try_wait().unwrap().is_some(),
                "actual provider interruption precedes settlement and PATCH completion");
            assert!(!off.is_finished(), "settlement is still held");
            None
        } else {
            if hold == "retained_foreign" {
                {
                    let mut inner = state.inner.lock().unwrap();
                    let index = inner.find_session_index(&session).unwrap();
                    inner.sessions[index].engram_containment_stop.as_mut().unwrap().in_flight = true;
                }
                let (_, claim) = state.begin_requested_stop_session_for_owner(
                    &session, &StopSessionOptions::default(), Some(&owner)).unwrap();
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&session).unwrap();
                let pending = inner.sessions[index].engram_containment_stop.as_mut().unwrap();
                pending.claim = Some(claim.expect("an owned fence admits the original Stop"));
                pending.in_flight = false;
            }
            let mut selected = vec![EngramDueRetry {
                session_id: session.clone(), owner: EngramRetryOwner::Stop(owner.clone()),
                kind: EngramRetryKind::ContainmentStop, due_at: due_at.clone(),
            }];
            state.order_engram_due_retries(&mut selected);
            let (generation, age) = {
                let mut inner = state.inner.lock().unwrap();
                inner.engram_project_resets.release(&project, owner.reset_generation);
                let generation = (hold != "session_only").then(||
                    inner.engram_project_resets.claim_with_clock(&project, clock.clone(), true).unwrap());
                let index = inner.find_session_index(&session).unwrap();
                let record = &mut inner.sessions[index];
                // Model a fence change after selection and slot admission.
                record.engram_containment_stop.as_mut().unwrap().in_flight = true;
                (generation, record.engram_queue_schedule.eligibility.clone())
            };
            let slot = state.engram_retry_slots.try_acquire().unwrap();
            state.run_engram_retry_attempt(selected.pop().unwrap(), clock.wall_now(), slot);
            state.engram_abort_retry_tick(clock.wall_now());
            {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                let pending = record.engram_containment_stop.as_ref().unwrap();
                assert_eq!(pending.owner, owner);
                assert_eq!(pending.attempts, 0);
                assert_eq!(pending.due_at, due_at);
                assert!(!pending.in_flight);
                assert_eq!(pending.claim.is_some(), hold == "retained_foreign");
                assert_eq!(record.engram_queue_schedule.eligibility, age,
                    "reset deferral does not charge the original service age");
            }
            assert!(process.try_wait().unwrap().is_none(), "foreign/unexplained holds refuse interruption");
            generation
        };
        {
            let inner = state.inner.lock().unwrap();
            assert!(inner.sessions[inner.find_session_index(&session).unwrap()].engram.project_reset_in_progress);
            assert!(engram_session_obligation_locked(&inner, &session).is_some());
            assert_eq!(inner.sessions[inner.find_session_index(&session).unwrap()]
                .queued_prompts.front().unwrap().pending_prompt.id, "successor-must-stay-queued");
        }
        assert!(receiver.try_recv().is_err(), "no successor or Begin follows containment Stop");
        assert_eq!(state.engram_retry_slots.in_flight(), 0);
        assert_eq!(transport.requests().len(), 1);
        settlement.release();
        let outcome = off.join().unwrap();
        if hold == "own" {
            outcome.expect("the owning reset still completes after its Stop");
        } else {
            if let Some(generation) = foreign_generation {
                state.release_engram_project_reset_fence(&project, generation, &[session.clone(), settling.clone()]);
            } else {
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&session).unwrap();
                inner.sessions[index].engram.project_reset_in_progress = false;
            }
            state.engram_abort_retry_tick(clock.wall_now());
            assert!(process.try_wait().unwrap().is_some(), "normal no-reset service retains the original Stop");
        }
        assert!(receiver.try_recv().is_err());
        assert_eq!(state.engram_retry_slots.in_flight(), 0);
    }
}

#[test]
fn project_containment_deferred_stops_survive_cap_and_budget_and_serve_each_original_once() {
    let (state, project, session, _root, _receiver) = fixture();
    let ids = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].session.status = SessionStatus::Active;
        let template = inner.sessions[index].clone();
        let mut ids = vec![session.clone()];
        for ordinal in 1..5 {
            let mut record = template.clone();
            record.session.id = format!("deferred-stop-{ordinal}");
            record.engram.routing_token = Some(format!("retained-token-{ordinal}"));
            record.engram.active_grant_id = Some(format!("retained-grant-{ordinal}"));
            ids.push(record.session.id.clone());
            inner.sessions.push(record);
        }
        state.commit_locked(&mut inner).unwrap();
        ids
    };
    let transport = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());
    let mut occupied = (0..ENGRAM_RETRY_MAX_IN_FLIGHT)
        .map(|_| state.engram_retry_slots.try_acquire().unwrap()).collect::<Vec<_>>();
    let response = state.update_project_engram_settings(&project, settings(&state, &project, false)).unwrap();
    for id in &ids {
        assert!(response.engram_control_recovery.iter().any(|entry|
            entry.session_id == *id && entry.code == "provider_stop_deferred"
                && entry.excluded && !entry.settlement_confirmed));
    }
    let clock = state.engram_budget_clock();
    state.engram_abort_retry_tick(clock.wall_now());
    assert_eq!(state.engram_retry_slots.in_flight(), ENGRAM_RETRY_MAX_IN_FLIGHT);
    // Service is independent of the already-finished reset's 21s budget.
    clock.advance(ENGRAM_CONTROL_SETTLE_TIMEOUT + Duration::from_secs(2));
    drop(occupied.pop());
    for id in &ids {
        let stop = install_test_stop_fence_gate(&state, id);
        std::thread::scope(|scope| {
            let stop = stop;
            let tick = scope.spawn(|| state.engram_abort_retry_tick(clock.wall_now()));
            stop.wait_until_claimed();
            assert_eq!(state.engram_retry_slots.in_flight(), ENGRAM_RETRY_MAX_IN_FLIGHT,
                "the existing Stop holds its slot through cleanup");
            // A simultaneous tick cannot start this same interruption twice.
            state.engram_abort_retry_tick(clock.wall_now());
            {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(id).unwrap()];
                assert_eq!(record.session.status, SessionStatus::Stopping);
                assert!(record.runtime_stop_in_progress);
            }
            stop.release();
            tick.join().unwrap();
        });
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(id).unwrap()];
        assert_eq!(record.session.status, SessionStatus::Idle);
        assert!(!record.runtime_stop_in_progress);
        assert!(engram_session_obligation_locked(&inner, id).is_some(),
            "Stop completion is not grant settlement");
    }
    assert!(transport.requests().is_empty());
    drop(occupied);
    state.engram_abort_retry_tick(clock.wall_now());
    assert_eq!(state.engram_retry_slots.in_flight(), 0);
}

#[test]
fn project_containment_failed_stop_reclaims_only_the_original_runtime_and_turn() {
    for replacement in ["none", "runtime", "turn"] {
        let (state, project, session, _root, receiver) = fixture();
        let clock = state.engram_budget_clock();
        clock.pin_wall();
        let process_owner = phase_sync::ParkedProcess::spawn();
        let process = process_owner.process.clone();
        let (input_tx, input_rx) = mpsc::channel();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                runtime_id: "retry-original-stop".to_owned(),
                input_tx,
                process: process.clone(),
                shared_session: None,
            });
            inner.sessions[index].session.status = SessionStatus::Active;
            state.commit_locked(&mut inner).unwrap();
        }
        let transport = ScriptedEngramControlTransport::new([]);
        state.install_control_test_transport(transport.clone());
        state.update_project_engram_settings(&project, settings(&state, &project, false)).unwrap();
        let original = {
            let inner = state.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&session).unwrap()]
                .engram_containment_stop.as_ref().unwrap().owner.clone()
        };
        {
            let _failure = force_test_kill_child_process_failure_once(&process, "Codex");
            state.engram_abort_retry_tick(clock.wall_now());
        }
        assert!(process.try_wait().unwrap().is_none(), "the first real shutdown failed");
        let (failed_generation, due_at) = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert_eq!(record.session.status, SessionStatus::Error);
            assert!(!record.runtime_stop_in_progress);
            let pending = record.engram_containment_stop.as_ref().unwrap();
            assert_eq!(pending.owner, original);
            assert!(pending.claim.is_none(), "the lifecycle released the failed Stop claim");
            assert!(!pending.in_flight);
            assert_eq!(pending.attempts, 1);
            let entry = engram_session_obligation_locked(&inner, &session).unwrap();
            assert_eq!(entry.id, original.obligation_id);
            assert!(!inner.engram_confirmed_containment_ids.contains(&entry.id));
            (record.runtime_stop_generation, pending.due_at.parse::<chrono::DateTime<chrono::Utc>>().unwrap())
        };
        assert_eq!(state.engram_retry_slots.in_flight(), 0);
        assert!(state.begin_requested_stop_session(&session, &StopSessionOptions::default()).is_err(),
            "ordinary user Stop admission is unchanged for a live Error runtime");

        let successor = (replacement == "runtime").then(phase_sync::ParkedProcess::spawn);
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            if let Some(successor) = successor.as_ref() {
                let SessionRuntime::Codex(runtime) = &mut inner.sessions[index].runtime else {
                    panic!("the failed original runtime must remain retained");
                };
                runtime.runtime_id = "replacement-after-failed-stop".to_owned();
                runtime.process = successor.process.clone();
            } else if replacement == "turn" {
                inner.sessions[index].active_turn_generation += 1;
            }
        }
        clock.advance((due_at - clock.wall_now()).to_std().unwrap());
        assert!(clock.wall_now() >= due_at, "the retry must be due before waiting for its Stop claim");
        if replacement == "none" {
            let stop = install_test_stop_fence_gate(&state, &session);
            std::thread::scope(|scope| {
                let stop = stop;
                let tick = scope.spawn(|| state.engram_abort_retry_tick(clock.wall_now()));
                stop.wait_until_claimed();
                {
                    let inner = state.inner.lock().unwrap();
                    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                    let pending = record.engram_containment_stop.as_ref().unwrap();
                    let claim = pending.claim.as_ref().unwrap();
                    assert_eq!(pending.owner, original);
                    assert!(claim.owns_stop(record));
                    assert!(claim.owner_generation > failed_generation,
                        "the second real shutdown must use a newly acquired Stop claim");
                    assert_eq!(state.engram_retry_slots.in_flight(), 1);
                }
                stop.release();
                tick.join().unwrap();
            });
            phase_sync::process_exit(&process, "retried original containment Stop");
        } else {
            state.engram_abort_retry_tick(clock.wall_now());
            assert!(process.try_wait().unwrap().is_none());
            if let Some(successor) = successor.as_ref() {
                assert!(successor.process.try_wait().unwrap().is_none(), "the successor is not interrupted");
            }
        }
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.engram_containment_stop.is_none());
        assert!(!record.runtime_stop_in_progress);
        assert_eq!(record.session.status, if replacement == "none" {
            SessionStatus::Idle
        } else {
            SessionStatus::Error
        });
        let entry = engram_session_obligation_locked(&inner, &session).unwrap();
        assert_eq!(entry.id, original.obligation_id);
        assert!(!inner.engram_confirmed_containment_ids.contains(&entry.id),
            "neither retry nor replacement settles the original control duty");
        assert_eq!(state.engram_retry_slots.in_flight(), 0);
        assert!(transport.requests().is_empty());
        assert!(input_rx.try_recv().is_err());
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn project_containment_deferred_stop_never_interrupts_a_replaced_runtime_or_turn() {
    for replace_runtime in [false, true] {
        let (state, project, session, _root, _receiver) = fixture();
        let process_owner = phase_sync::ParkedProcess::spawn();
        let process = process_owner.process.clone();
        let (runtime, input, _) = test_shared_codex_runtime_with_process("replacement-stop", process.clone());
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].session.status = SessionStatus::Active;
            state.commit_locked(&mut inner).unwrap();
        }
        state.install_control_test_transport(ScriptedEngramControlTransport::new([]));
        state.update_project_engram_settings(&project, settings(&state, &project, false)).unwrap();
        let original = {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let original = inner.sessions[index].engram_containment_stop.as_ref().unwrap().owner.clone();
            let record = &mut inner.sessions[index];
            if replace_runtime {
                record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                    runtime_id: runtime.runtime_id.clone(), input_tx: runtime.input_tx.clone(),
                    process: process.clone(), shared_session: None,
                });
            } else {
                record.active_turn_generation += 1;
            }
            original
        };
        state.engram_abort_retry_tick(state.engram_budget_clock().wall_now());
        {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert_eq!(record.session.status, SessionStatus::Active);
            assert!(!record.runtime_stop_in_progress);
            assert!(record.engram_containment_stop.is_none());
            let entry = engram_session_obligation_locked(&inner, &session).unwrap();
            assert_eq!(entry.id, original.obligation_id);
            assert_eq!(entry.reason_code, "provider_stop_owner_changed");
            assert!(!inner.engram_confirmed_containment_ids.contains(&entry.id));
        }
        assert!(input.try_recv().is_err());
        assert!(process.try_wait().unwrap().is_none(), "the successor runtime stays alive");
    }
}

#[test]
fn project_containment_stop_rechecks_original_identity_when_selecting_the_runtime_handle() {
    for replace_runtime in [false, true] {
        let (state, project, session, _root, _receiver) = fixture();
        let process_owner = phase_sync::ParkedProcess::spawn();
        let process = process_owner.process.clone();
        let (runtime, input, _) = test_shared_codex_runtime_with_process("claimed-stop-replacement", process.clone());
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].session.status = SessionStatus::Active;
            state.commit_locked(&mut inner).unwrap();
        }
        state.install_control_test_transport(ScriptedEngramControlTransport::new([]));
        state.update_project_engram_settings(&project, settings(&state, &project, false)).unwrap();
        let owner = {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let pending = inner.sessions[index].engram_containment_stop.as_mut().unwrap();
            pending.in_flight = true;
            pending.owner.clone()
        };
        let (_, claim) = state.begin_requested_stop_session_for_owner(
            &session, &StopSessionOptions::default(), Some(&owner),
        ).unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            if replace_runtime {
                record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                    runtime_id: runtime.runtime_id.clone(), input_tx: runtime.input_tx.clone(),
                    process: process.clone(), shared_session: None,
                });
            } else {
                record.active_turn_generation += 1;
            }
        }
        let error = state.stop_local_session_with_options(&session, StopSessionOptions::default(), claim)
            .err().expect("a stored Stop claim cannot authorize a new runtime or turn");
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(input.try_recv().is_err());
        assert!(process.try_wait().unwrap().is_none());
        let inner = state.inner.lock().unwrap();
        let entry = engram_session_obligation_locked(&inner, &session).unwrap();
        assert_eq!(entry.id, owner.obligation_id);
        assert!(!inner.engram_confirmed_containment_ids.contains(&entry.id));
    }
}

#[test]
fn project_containment_active_and_approval_off_requests_stop_without_claiming_settlement() {
    for status in [SessionStatus::Active, SessionStatus::Approval] {
        let (state, project, session, _root, receiver) = fixture();
        let process_owner = phase_sync::ParkedProcess::spawn();
        let process = process_owner.process.clone();
        let (runtime, input, _) =
            test_shared_codex_runtime_with_process("containment-stop", process.clone());
        let transport = ScriptedEngramControlTransport::new([]);
        state.install_control_test_transport(transport.clone());
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                runtime_id: runtime.runtime_id.clone(),
                input_tx: runtime.input_tx.clone(),
                process: process.clone(),
                shared_session: None,
            });
            record.session.status = status;
            state.commit_locked(&mut inner).unwrap();
        }
        let stop = install_test_stop_fence_gate(&state, &session);
        let response = state
            .update_project_engram_settings(&project, settings(&state, &project, false))
            .unwrap();
        std::thread::scope(|scope| {
            let stop = stop;
            let tick = scope.spawn(|| state.engram_abort_retry_tick(state.engram_budget_clock().wall_now()));
            stop.wait_until_claimed();
            {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                assert_eq!(record.session.status, SessionStatus::Stopping);
                assert!(
                    record.runtime_stop_in_progress,
                    "OFF must invoke the existing Stop lifecycle"
                );
                let entry = engram_session_obligation_locked(&inner, &session).unwrap();
                assert_eq!(entry.reason_code, "provider_interruption_unconfirmed");
                assert_ne!(entry.phase, EngramSessionObligationPhase::Confirmed);
                assert!(!inner.engram_confirmed_containment_ids.contains(&entry.id));
            }
            assert!(
                response
                    .engram_control_recovery
                    .iter()
                    .any(|entry| entry.session_id == session
                        && entry.excluded
                        && !entry.settlement_confirmed)
            );
            assert!(transport.requests().is_empty());
            assert!(input.try_recv().is_err());
            assert!(receiver.try_recv().is_err());
            stop.release();
            tick.join().unwrap();
        });
        phase_sync::process_exit(&process, "containment requested provider Stop");
        let guard = phase_sync::PollGuard::new();
        loop {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            if !record.runtime_stop_in_progress {
                break;
            }
            drop(inner);
            guard.wait("existing Stop lifecycle completes");
        }
        let inner = state.inner.lock().unwrap();
        assert!(
            engram_session_obligation_locked(&inner, &session).is_some(),
            "a completed Stop alone does not settle the retained control duty"
        );
        assert!(inner.engram_confirmed_containment_ids.is_empty());
        assert!(transport.requests().is_empty());
    }
}

#[test]
fn project_containment_legacy_route_never_becomes_confirmed_authority() {
    for database_exists in [false, true] {
        let (state, project, session, _root, receiver) = fixture();
        let original_path = {
            let mut inner = state.inner.lock().unwrap();
            let settings = inner
                .projects
                .iter_mut()
                .find(|p| p.id == project)
                .unwrap()
                .engram
                .as_mut()
                .unwrap();
            let path = settings.authority_store_key.take().unwrap().database_path;
            state.commit_locked(&mut inner).unwrap();
            path
        };
        if !database_exists {
            fs::remove_file(&original_path).unwrap();
        }
        let transport = ScriptedEngramControlTransport::new([]);
        install_control_only_transport(&state, transport.clone());
        let response = state
            .update_project_engram_settings(&project, settings(&state, &project, false))
            .unwrap();
        assert_eq!(
            response.engram_control_recovery[0].code,
            "original_authority_unconfirmed"
        );
        {
            let inner = state.inner.lock().unwrap();
            let entry = &inner.engram_session_obligations.entries[0];
            assert!(!entry.authority_confirmed);
            assert_eq!(entry.phase, EngramSessionObligationPhase::Intended);
            assert!(
                inner
                    .find_project(&project)
                    .unwrap()
                    .engram
                    .as_ref()
                    .unwrap()
                    .authority_store_key
                    .is_none()
            );
            let mut invented = entry.clone();
            invented.phase = EngramSessionObligationPhase::Confirmed;
            invented.settlement_evidence = json!({"operation":"turn_checkpoint", "response":{
                "decision":"checkpointed", "receipt":{"grant_id":entry.grant_id,"cursor":1,"confirmed_cursor":1}}});
            assert!(
                invented.validate().is_err(),
                "a prospective route never authenticates a receipt"
            );
        }
        assert!(
            state
                .update_project_engram_settings(&project, settings(&state, &project, true))
                .is_err()
        );
        assert!(state.resume_session_queue(&session).is_err());
        assert!(state.dispatch_turn(&session, prompt()).is_err());
        assert!(transport.requests().is_empty());
        assert!(receiver.try_recv().is_err());
        assert_eq!(
            original_path.exists(),
            database_exists,
            "containment creates no missing authority store"
        );
    }
}

#[test]
fn project_containment_public_patch_preserves_private_admission_fields() {
    let (state, project, session, root, _receiver) = fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        inner.preferences.engram.binary_path = real_engram_control_fixture_path()
            .to_string_lossy()
            .into_owned();
        inner.preferences.engram.home = root.to_string_lossy().into_owned();
        let settings = inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram
            .as_mut()
            .unwrap();
        settings.work_authority_grant = Some("original-private-work-grant".into());
        settings.deadline_ms = Some(321);
        state.commit_locked(&mut inner).unwrap();
    }
    let transport =
        ScriptedEngramControlTransport::new([checkpoint_refusal_reply("GrantScopeMismatch")]);
    install_control_only_transport(&state, transport.clone());
    let mut contained_history = None;
    for gated in [false, true] {
        let request =
            serde_json::from_value(json!({"enabled":true,"turnGatedControl":gated})).unwrap();
        state
            .patch_project_engram_settings(&project, request)
            .unwrap();
        let inner = state.inner.lock().unwrap();
        let settings = inner
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap();
        assert_eq!(
            settings.work_authority_grant.as_deref(),
            Some("original-private-work-grant")
        );
        assert_eq!(settings.deadline_ms, Some(321));
        assert!(engram_session_obligation_locked(&inner, &session).is_some());
        let history = transport
            .requests()
            .into_iter()
            .map(|r| (r.connection, r.request))
            .collect::<Vec<_>>();
        if gated {
            assert_eq!(
                Some(history),
                contained_history,
                "ON preservation does not settle or rebind"
            );
        } else {
            assert_eq!(
                history
                    .iter()
                    .map(|(_, r)| r["operation"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                vec!["turn_checkpoint", "turn_grant_read"]
            );
            contained_history = Some(history);
        }
    }
}
