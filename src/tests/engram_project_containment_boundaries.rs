//! Public project and runtime boundaries beside passive original metadata.
use super::*;
use super::super::uncertain_begin::OrdinaryRoot;
use super::recovery::{assert_passive_original, install_passive_original};

#[path = "engram_project_containment_settings_races.rs"]
mod settings_races;

fn ordinary_settings(turn: &OrdinaryRoot) -> EngramProjectSettings {
    turn.state.inner.lock().unwrap().find_project(&turn.project).unwrap().engram.clone().unwrap()
}

#[test]
fn ordinary_project_delete_normalizes_identity_and_preserves_private_history() {
    for padded in [false, true] {
        let turn = OrdinaryRoot::new("ordinary-delete-identity");
        let expected = install_passive_original(&turn);
        let supplied = if padded { format!("  {}  ", turn.project) } else { turn.project.clone() };
        turn.state.delete_project(&supplied).unwrap();
        {
            let inner = turn.state.inner.lock().unwrap();
            assert!(inner.find_project(&turn.project).is_none());
            assert!(inner.sessions[inner.find_session_index(&turn.session).unwrap()].session.project_id.is_none());
            assert!(!inner.engram_project_resets.contains(&turn.project));
        }
        turn.state.shutdown_persist_blocking();
        let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
        assert!(loaded.find_project(&turn.project).is_none());
        assert!(loaded.sessions[loaded.find_session_index(&turn.session).unwrap()].session.project_id.is_none());
        assert_passive_original(&turn, &expected);
        turn.assert_no_prompt();
    }
}

#[test]
fn ordinary_project_delete_rechecks_the_connection_snapshot() {
    let turn = OrdinaryRoot::new("ordinary-delete-stale-snapshot");
    let expected = install_passive_original(&turn);
    let snapshot = turn.state.inner.lock().unwrap().find_project(&turn.project).unwrap().clone();
    {
        let mut inner = turn.state.inner.lock().unwrap();
        inner.projects.iter_mut().find(|project| project.id == turn.project)
            .unwrap().engram.as_mut().unwrap().home = Some(turn.root.join("successor").to_string_lossy().into_owned());
        turn.state.commit_locked(&mut inner).unwrap();
    }
    let error = turn.state.delete_project_reference(&turn.project, &snapshot).err().expect("the current owner must refuse this operation");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(turn.state.inner.lock().unwrap().find_project(&turn.project).is_some());
    assert_eq!(turn.record(|record| record.session.project_id.clone()), Some(turn.project.clone()));
    assert_passive_original(&turn, &expected);
}

#[test]
fn ordinary_project_delete_rechecks_a_new_connection_owner() {
    let turn = OrdinaryRoot::new("ordinary-delete-reset-owner");
    let expected = install_passive_original(&turn);
    let (snapshot, generation) = {
        let mut inner = turn.state.inner.lock().unwrap();
        let snapshot = inner.find_project(&turn.project).unwrap().clone();
        let generation = inner.claim_engram_project_reset(&turn.project).unwrap();
        (snapshot, generation)
    };
    for result in [turn.state.delete_project(&turn.project),
        turn.state.delete_project_reference(&turn.project, &snapshot)] {
        assert_eq!(result.err().expect("the current owner must refuse this operation").status, StatusCode::CONFLICT);
    }
    {
        let mut inner = turn.state.inner.lock().unwrap();
        assert!(inner.find_project(&turn.project).is_some());
        assert!(inner.engram_project_resets.is_owned_by(&turn.project, generation));
        assert!(inner.engram_project_resets.release(&turn.project, generation));
    }
    assert_passive_original(&turn, &expected);
}

#[test]
fn ordinary_public_patch_preserves_default_command_deadline_and_private_identity() {
    let turn = OrdinaryRoot::new("ordinary-public-patch");
    let expected = install_passive_original(&turn);
    let private = EngramAuthorityStoreKey {
        database_path: turn.root.join("private-authority.sqlite"),
        project_id: "private-store-identity".into(),
    };
    {
        let mut inner = turn.state.inner.lock().unwrap();
        inner.preferences.engram.binary_path = default_engram_binary_path();
        inner.preferences.engram.home = turn.root.to_string_lossy().into_owned();
        let project = inner.projects.iter_mut().find(|entry| entry.id == turn.project).unwrap();
        let settings = project.engram.as_mut().unwrap();
        settings.binary_path = Some(default_engram_binary_path());
        settings.deadline_ms = Some(321);
        settings.authority_store_key = Some(private.clone());
        turn.state.commit_locked(&mut inner).unwrap();
    }
    let private_request = serde_json::from_value::<UpdateProjectEngramSettingsRequest>(json!({
        "enabled": true, "executionObservability": true,
        "authorityStoreKey": {"databasePath": "invented", "projectId": "invented"}
    }));
    assert!(private_request.is_err(), "private store identity is not a public request field");
    assert_eq!(ordinary_settings(&turn).authority_store_key, Some(private.clone()));
    assert_passive_original(&turn, &expected);
    for observing in [true, false] {
        let request = serde_json::from_value(json!({
            "enabled": true, "executionObservability": observing,
            "binaryPath": "untrusted-client-command", "home": "untrusted-client-home",
            "deadlineMs": 999
        })).unwrap();
        let response = turn.state.patch_project_engram_settings(&turn.project, request).unwrap();
        let public = serde_json::to_string(&response).unwrap();
        for secret in ["private-authority.sqlite", "private-store-identity", "old-token",
            "old-grant", "frozen-checkpoint-key", "distinct-retirement-key", "settlementEvidence"] {
            assert!(!public.contains(secret), "private original escaped: {secret}");
        }
        let settings = ordinary_settings(&turn);
        assert_eq!(settings.binary_path, Some(default_engram_binary_path()));
        assert_eq!(settings.home, Some(turn.root.to_string_lossy().into_owned()));
        assert_eq!(settings.deadline_ms, Some(321));
        assert_eq!(settings.authority_store_key, Some(private.clone()));
        assert_eq!(settings.execution_observability, observing);
        assert_passive_original(&turn, &expected);
    }
    turn.assert_no_prompt();
}

#[test]
fn ordinary_invalid_connection_home_cannot_mutate_settings_or_history() {
    let turn = OrdinaryRoot::new("ordinary-invalid-home");
    let expected = install_passive_original(&turn);
    let before = ordinary_settings(&turn);
    let mut requested = before.clone();
    requested.enabled = true;
    requested.home = Some(turn.root.join("does-not-exist").to_string_lossy().into_owned());
    assert!(turn.state.update_project_engram_settings(&turn.project, requested).is_err());
    assert_eq!(ordinary_settings(&turn), before);
    assert!(!turn.state.inner.lock().unwrap().engram_project_resets.contains(&turn.project));
    assert_passive_original(&turn, &expected);
    turn.assert_no_prompt();
}

#[test]
fn ordinary_readiness_rechecks_marker_and_receipt_store_without_mutating_history() {
    let turn = OrdinaryRoot::new("ordinary-readiness-boundary");
    let expected = install_passive_original(&turn);
    let marker = turn.root.join(".engram-project");
    fs::write(&marker, "fixture-ready\n").unwrap();
    let _seam = stage_test_engram_readiness_without_launch(&[&turn.root]);
    {
        let mut inner = turn.state.inner.lock().unwrap();
        inner.preferences.engram.binary_path = default_engram_binary_path();
        inner.preferences.engram.home = turn.root.to_string_lossy().into_owned();
        turn.state.commit_locked(&mut inner).unwrap();
    }
    let before = ordinary_settings(&turn);
    let request = serde_json::from_value(json!({"enabled": false, "executionObservability": false})).unwrap();
    let verified = turn.state.verify_project_engram_settings(&turn.project, request).unwrap();
    assert!(verified.verified);
    assert_eq!(verified.full_audit, "not_run");
    assert_eq!(ordinary_settings(&turn), before, "Verify is not a Save or an authority token");
    let output = staged_test_engram_readiness_receipt(&marker, &turn.root).unwrap();
    for changed in ["project", "database"] {
        let mut receipt: EngramReadinessReceipt = serde_json::from_slice(&output.stdout).unwrap();
        if changed == "project" { receipt.project_id = "different-store".into(); }
        else { receipt.database = turn.root.join("wrong-store.sqlite"); }
        assert!(validate_engram_readiness(&receipt, &marker, &turn.root).is_err());
    }
    fs::remove_file(&marker).unwrap();
    let request = serde_json::from_value(json!({"enabled": true})).unwrap();
    assert!(turn.state.verify_project_engram_settings(&turn.project, request).is_err());
    assert_eq!(ordinary_settings(&turn), before);
    assert!(!turn.root.join("historical.sqlite").exists(), "readiness never repairs the old authority store");
    assert_passive_original(&turn, &expected);
    turn.assert_no_prompt();
}

#[test]
fn ordinary_stored_reason_codes_remain_strict_and_host_private() {
    let turn = OrdinaryRoot::new("ordinary-private-reason-codes");
    let expected = install_passive_original(&turn);
    for code in ["".to_owned(), " \n".into(), "bad\u{1b}code".into(), "x".repeat(65_537)] {
        let mut malformed: EngramSessionObligationLedger = serde_json::from_value(expected.clone()).unwrap();
        malformed.entries[0].reason_code = code;
        assert!(malformed.validate().is_err(), "invalid historical data is not an executable reply");
    }
    assert_passive_original(&turn, &expected);
}

#[test]
fn ordinary_settings_and_restart_never_confirm_passive_original_receipts() {
    let turn = OrdinaryRoot::new("ordinary-restart-original-receipts");
    let expected = install_passive_original(&turn);
    let mut changed = ordinary_settings(&turn);
    changed.execution_observability = true;
    turn.state.update_project_engram_settings(&turn.project, changed).unwrap();
    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    loaded.engram_session_obligations.validate().unwrap();
    assert_eq!(serde_json::to_value(&loaded.engram_session_obligations).unwrap(), expected);
    *turn.state.inner.lock().unwrap() = loaded;
    turn.state.resume_session_queue(&turn.session).unwrap();
    assert_passive_original(&turn, &expected);
    turn.assert_no_prompt();
}

#[test]
fn ordinary_operator_pause_does_not_abort_shared_runtime_exit() {
    let turn = OrdinaryRoot::new("ordinary-paused-shared-exit");
    let expected = install_passive_original(&turn);
    let healthy = create_test_project_session(&turn.state, Agent::Codex, &turn.project, &turn.root);
    let process_owner = phase_sync::ParkedProcess::spawn();
    let process = process_owner.process.clone();
    let (runtime, _input, _) = test_shared_codex_runtime_with_process("ordinary-shared-exit", process.clone());
    *turn.state.shared_codex_runtime.lock().unwrap() = Some(runtime.clone());
    let queued = turn.enqueue("keep operator-paused original", QueuedPromptSource::User);
    {
        let mut inner = turn.state.inner.lock().unwrap();
        for id in [&turn.session, &healthy] {
            let record = inner.session_mut(id).unwrap();
            record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                runtime_id: runtime.runtime_id.clone(), input_tx: runtime.input_tx.clone(),
                process: process.clone(), shared_session: Some(SharedCodexSessionHandle {
                    runtime: runtime.clone(), session_id: id.clone(),
                }),
            });
            record.session.status = SessionStatus::Active;
        }
        let record = inner.session_mut(&turn.session).unwrap();
        record.engram.operator_paused = true;
        record.set_auto_dispatch_blocked(true);
        turn.state.commit_locked(&mut inner).unwrap();
    }
    assert!(turn.state.dispatch_next_queued_turn(&turn.session, false).unwrap().is_none());
    turn.state.handle_shared_codex_runtime_exit("ordinary-shared-exit", Some("fixture exit")).unwrap();
    {
        let inner = turn.state.inner.lock().unwrap();
        for id in [&turn.session, &healthy] {
            let record = &inner.sessions[inner.find_session_index(id).unwrap()];
            assert!(matches!(record.runtime, SessionRuntime::None));
            assert_eq!(record.session.status, SessionStatus::Error);
        }
        let record = &inner.sessions[inner.find_session_index(&turn.session).unwrap()];
        assert!(record.engram.operator_paused && record.orchestrator_auto_dispatch_blocked);
        assert!(record.session.queue_paused);
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.queued_prompts[0].pending_prompt.id, queued);
        assert_eq!(record.queued_prompts[0].pending_prompt.text, "keep operator-paused original");
    }
    assert!(turn.state.shared_codex_runtime.lock().unwrap().is_none());
    phase_sync::process_exit(&process, "ordinary shared runtime exit");
    assert_passive_original(&turn, &expected);
    turn.assert_no_prompt();
}

#[test]
fn ordinary_stop_rejects_replaced_runtime_turn_or_stop_owner() {
    for replaced in ["runtime", "turn", "stop"] {
        let turn = OrdinaryRoot::new("ordinary-stale-stop-selection");
        let expected = install_passive_original(&turn);
        turn.edit(|record| { record.runtime = SessionRuntime::None; record.session.status = SessionStatus::Active; });
        let options = StopSessionOptions::default();
        let (_, claim) = turn.state.begin_requested_stop_session(&turn.session, &options).unwrap();
        let claim = claim.unwrap();
        let mut successor_input = None;
        let successor_process = (replaced == "runtime").then(phase_sync::ParkedProcess::spawn);
        if replaced == "runtime" {
            let process = successor_process.as_ref().unwrap().process.clone();
            let (runtime, input, _) = test_shared_codex_runtime_with_process("ordinary-stop-successor", process.clone());
            successor_input = Some(input);
            turn.edit(|record| record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                runtime_id: runtime.runtime_id.clone(), input_tx: runtime.input_tx.clone(),
                process, shared_session: None,
            }));
        } else {
            turn.edit(|record| {
                if replaced == "turn" { record.active_turn_generation += 1; }
                else { record.claim_missing_runtime_stop(RuntimeStopOwnerKind::UserStop); }
            });
        }
        let before = turn.saved();
        let identity = turn.record(|record| (record.runtime.runtime_token(), record.active_turn_generation,
            record.runtime_stop_generation, record.runtime_stop_in_progress));
        let error = turn.state.stop_local_session_with_options(&turn.session, options, Some(claim)).err().expect("the current owner must refuse this operation");
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert_eq!(turn.saved(), before);
        assert_eq!(turn.record(|record| (record.runtime.runtime_token(), record.active_turn_generation,
            record.runtime_stop_generation, record.runtime_stop_in_progress)), identity);
        if let Some(input) = successor_input { assert!(input.try_recv().is_err()); }
        if let Some(owner) = successor_process.as_ref() { assert!(owner.process.try_wait().unwrap().is_none()); }
        assert_passive_original(&turn, &expected);
        turn.assert_no_prompt();
    }
}

#[test]
fn ordinary_requested_stop_progresses_once_without_control_retry_or_claim_wait() {
    for status in [SessionStatus::Active, SessionStatus::Approval] {
        let turn = OrdinaryRoot::new("ordinary-requested-stop-progress");
        let expected = install_passive_original(&turn);
        let queued = turn.enqueue("preserve original user queue", QueuedPromptSource::User);
        let process_owner = phase_sync::ParkedProcess::spawn();
        let process = process_owner.process.clone();
        let (runtime, input, _) = test_shared_codex_runtime_with_process("ordinary-stop-progress", process.clone());
        turn.edit(|record| {
            record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                runtime_id: runtime.runtime_id.clone(), input_tx: runtime.input_tx.clone(),
                process: process.clone(), shared_session: None,
            });
            record.session.status = status;
        });
        let options = StopSessionOptions::default();
        let (_, claim) = turn.state.begin_requested_stop_session(&turn.session, &options).unwrap();
        let generation = turn.record(|record| record.runtime_stop_generation);
        let (_, duplicate) = turn.state.begin_requested_stop_session(&turn.session, &options).unwrap();
        assert!(duplicate.is_none());
        assert_eq!(turn.record(|record| record.runtime_stop_generation), generation);
        turn.state.stop_local_session_with_options(&turn.session, options, claim).unwrap();
        phase_sync::process_exit(&process, "ordinary requested Stop");
        assert!(input.try_recv().is_err());
        turn.record(|record| {
            assert_eq!(record.session.status, SessionStatus::Idle);
            assert!(!record.runtime_stop_in_progress);
            assert!(record.engram.operator_paused && record.orchestrator_auto_dispatch_blocked);
            assert_eq!(record.queued_prompts.len(), 1);
            assert_eq!(record.queued_prompts[0].pending_prompt.id, queued);
            assert_eq!(record.queued_prompts[0].pending_prompt.text, "preserve original user queue");
        });
        assert_passive_original(&turn, &expected);
        turn.assert_no_prompt();
    }
}

#[test]
fn ordinary_failed_stop_retains_original_runtime_without_replaying_queued_content() {
    let turn = OrdinaryRoot::new("ordinary-failed-stop");
    let expected = install_passive_original(&turn);
    let process_owner = phase_sync::ParkedProcess::spawn();
    let process = process_owner.process.clone();
    let (runtime, input, _) = test_shared_codex_runtime_with_process("ordinary-failed-stop", process.clone());
    let queued = turn.enqueue("not replayed after failure", QueuedPromptSource::User);
    turn.edit(|record| {
        record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(), input_tx: runtime.input_tx.clone(),
            process: process.clone(), shared_session: None,
        });
        record.session.status = SessionStatus::Active;
    });
    let options = StopSessionOptions::default();
    let (_, claim) = turn.state.begin_requested_stop_session(&turn.session, &options).unwrap();
    let token = turn.record(|record| record.runtime.runtime_token());
    let failure = force_test_kill_child_process_failure_once(&process, "Codex");
    assert!(turn.state.stop_local_session_with_options(&turn.session, options, claim).is_err());
    drop(failure);
    assert!(process.try_wait().unwrap().is_none());
    turn.record(|record| {
        assert_eq!(record.runtime.runtime_token(), token);
        assert!(!record.runtime_stop_in_progress);
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.queued_prompts[0].pending_prompt.id, queued);
        assert_eq!(record.queued_prompts[0].pending_prompt.text, "not replayed after failure");
    });
    assert!(input.try_recv().is_err());
    assert_passive_original(&turn, &expected);
    turn.assert_no_prompt();
}

#[test]
fn ordinary_disable_cleans_installed_descriptor_without_inventing_operator_pause() {
    for paused in [false, true] {
        let mut turn = OrdinaryRoot::new("ordinary-descriptor-disable");
        let expected = install_passive_original(&turn);
        let process_owner = phase_sync::ParkedProcess::spawn();
        let process = process_owner.process.clone();
        let (runtime, input, _) = test_shared_codex_runtime_with_process("ordinary-descriptor-disable", process.clone());
        turn.edit(|record| {
            record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                runtime_id: runtime.runtime_id.clone(), input_tx: runtime.input_tx.clone(),
                process: process.clone(), shared_session: None,
            });
            record.session.status = SessionStatus::Active;
            record.engram_mcp_installed = Some(EngramMcpInstalledDescriptor {
                binary_path: turn.root.join("engram-fixture").to_string_lossy().into_owned(),
                home: turn.root.to_string_lossy().into_owned(), actor_id: "original-actor".into(),
                actor_context: None, store_key: None,
            });
            record.engram.operator_paused = paused;
            record.set_auto_dispatch_blocked(paused);
        });
        let mut settings = ordinary_settings(&turn);
        settings.enabled = false;
        turn.state.update_project_engram_settings(&turn.project, settings).unwrap();
        phase_sync::process_exit(&process, "installed descriptor shutdown");
        turn.record(|record| {
            assert!(matches!(record.runtime, SessionRuntime::None));
            assert!(record.engram_mcp_installed.is_none());
            assert!(!record.runtime_stop_in_progress);
            assert_eq!(record.engram.operator_paused, paused);
            assert_eq!(record.orchestrator_auto_dispatch_blocked, paused);
        });
        assert!(input.try_recv().is_err());
        assert_passive_original(&turn, &expected);
        turn.assert_no_prompt();
        if !paused {
            for raw in ["first ordinary turn after descriptor cleanup", "second ordinary turn after descriptor cleanup"] {
                let dispatch = turn.prepare(raw);
                let token = dispatch.runtime_token().clone();
                let generation = dispatch.active_turn_generation();
                assert!(matches!(deliver_turn_dispatch(&turn.state, dispatch), TurnDispatchDeliveryOutcome::Delivered));
                let command = turn.prompt();
                assert!(command.prompt == raw || command.prompt.ends_with(&format!("\n\n{raw}")));
                assert_eq!(command.prompt.matches(raw).count(), 1);
                turn.state.finish_turn_ok_if_runtime_and_generation_match(&turn.session, &token, generation).unwrap();
                turn.assert_no_prompt();
                assert_passive_original(&turn, &expected);
            }
            let path = resolve_coordination_persistence_path(turn.state.persistence_path.as_ref());
            turn.state.mailbox_store = Arc::new(MailboxStore::open(&path).unwrap());
            let sender = test_session_id(&turn.state, Agent::Claude);
            let receipt = turn.state.append_mailbox_message_and_notify(&sender, SendMailboxMessageRequest {
                target_session_id: turn.session.clone(), message: "ordinary wake after descriptor cleanup".into(),
                idempotency_key: "ordinary-descriptor-cleanup-wake".into(), topic: None, state_stamp: None,
                class: Some("routine".into()),
            }).unwrap();
            assert!(turn.prompt().prompt.contains(&receipt.mailbox_id));
            turn.assert_no_prompt();
            assert_passive_original(&turn, &expected);
        }
    }
}

#[test]
fn ordinary_project_delete_refuses_failed_installed_descriptor_cleanup() {
    let turn = OrdinaryRoot::new("ordinary-delete-cleanup-failure");
    let expected = install_passive_original(&turn);
    let process_owner = phase_sync::ParkedProcess::spawn();
    let process = process_owner.process.clone();
    let (runtime, input, _) = test_shared_codex_runtime_with_process("ordinary-delete-cleanup-failure", process.clone());
    turn.edit(|record| {
        record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(), input_tx: runtime.input_tx.clone(),
            process: process.clone(), shared_session: None,
        });
        record.engram_mcp_installed = Some(EngramMcpInstalledDescriptor {
            binary_path: turn.root.join("engram-fixture").to_string_lossy().into_owned(),
            home: turn.root.to_string_lossy().into_owned(), actor_id: "original-actor".into(),
            actor_context: None, store_key: None,
        });
    });
    let token = turn.record(|record| record.runtime.runtime_token());
    let failure = force_test_kill_child_process_failure(&process, "Codex");
    assert!(turn.state.delete_project(&turn.project).is_err());
    drop(failure);
    assert!(process.try_wait().unwrap().is_none());
    assert!(turn.state.inner.lock().unwrap().find_project(&turn.project).is_some());
    turn.record(|record| {
        assert_eq!(record.session.project_id, Some(turn.project.clone()));
        assert_eq!(record.runtime.runtime_token(), token);
        assert!(record.engram_mcp_runtime_quarantined);
        assert!(!record.runtime_stop_in_progress);
    });
    assert!(input.try_recv().is_err());
    assert_passive_original(&turn, &expected);
    turn.assert_no_prompt();
}

#[test]
fn ordinary_descriptor_disable_defers_to_public_stop_and_preserves_its_pause() {
    let turn = OrdinaryRoot::new("ordinary-descriptor-stop-owner");
    let expected = install_passive_original(&turn);
    let process_owner = phase_sync::ParkedProcess::spawn();
    let process = process_owner.process.clone();
    let (runtime, input, _) = test_shared_codex_runtime_with_process("ordinary-descriptor-stop-owner", process.clone());
    let head = turn.enqueue("exact user content during descriptor cleanup", QueuedPromptSource::User);
    turn.edit(|record| {
        record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(), input_tx: runtime.input_tx.clone(),
            process: process.clone(), shared_session: None,
        });
        record.session.status = SessionStatus::Active;
        record.engram_mcp_installed = Some(EngramMcpInstalledDescriptor {
            binary_path: turn.root.join("engram-fixture").to_string_lossy().into_owned(),
            home: turn.root.to_string_lossy().into_owned(), actor_id: "original-actor".into(),
            actor_context: None, store_key: None,
        });
    });
    let options = StopSessionOptions::default();
    let (_, claim) = turn.state.begin_requested_stop_session(&turn.session, &options).unwrap();
    let claim = claim.unwrap();
    let generation = turn.record(|record| record.runtime_stop_generation);
    let mut disabled = ordinary_settings(&turn);
    disabled.enabled = false;
    let error = turn.state.update_project_engram_settings(&turn.project, disabled)
        .err().expect("descriptor cleanup waits for the existing Stop");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(!ordinary_settings(&turn).enabled, "settings were committed before pending cleanup was reported");
    turn.record(|record| {
        assert!(claim.owns_stop(record));
        assert_eq!(record.runtime_stop_generation, generation);
        assert!(record.engram_mcp_revocation_pending);
        assert!(record.engram.operator_paused && record.session.queue_paused);
    });
    turn.state.stop_local_session_with_options(&turn.session, options, Some(claim)).unwrap();
    phase_sync::process_exit(&process, "public Stop followed by descriptor cleanup");
    turn.record(|record| {
        assert!(!record.runtime_stop_in_progress);
        assert!(record.engram.operator_paused && record.session.queue_paused);
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.queued_prompts[0].pending_prompt.id, head);
        assert_eq!(record.queued_prompts[0].pending_prompt.text, "exact user content during descriptor cleanup");
    });
    assert!(input.try_recv().is_err());
    assert_passive_original(&turn, &expected);
    turn.assert_no_prompt();
}

#[test]
fn ordinary_unknown_handoff_stays_exact_through_observation_edit_and_resume() {
    let turn = OrdinaryRoot::new("ordinary-private-unknown-handoff");
    let expected = install_passive_original(&turn);
    let id = turn.enqueue("original ambiguous handoff", QueuedPromptSource::User);
    turn.edit(|record| {
        record.queued_prompts[0].engram_ordinary_handoff = true;
        record.queued_prompts[0].promotion_disposition_known = false;
        record.set_auto_dispatch_blocked(true);
    });
    let before = turn.record(|record| serde_json::to_value(&record.queued_prompts[0]).unwrap());
    let mut changed = ordinary_settings(&turn);
    changed.execution_observability = true;
    turn.state.update_project_engram_settings(&turn.project, changed).unwrap();
    turn.state.resume_session_queue(&turn.session).unwrap();
    assert_eq!(turn.record(|record| record.queued_prompts[0].pending_prompt.id.clone()), id);
    assert_eq!(turn.record(|record| serde_json::to_value(&record.queued_prompts[0]).unwrap()), before);
    assert_passive_original(&turn, &expected);
    turn.assert_no_prompt();
}
