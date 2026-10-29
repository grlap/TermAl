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
