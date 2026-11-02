// Owns append-only OFF queue admission, final eligibility fences and durability.
// Does not settle the retained control duty or exercise any live producer.
use super::*;

fn request_history(
    transport: &ScriptedEngramControlTransport,
) -> Vec<(EngramConnectionConfig, Value)> {
    transport
        .requests()
        .into_iter()
        .map(|record| (record.connection, record.request))
        .collect()
}

pub(super) fn held_fixture() -> (
    AppState,
    String,
    String,
    mpsc::Receiver<CodexRuntimeCommand>,
    Arc<ScriptedEngramControlTransport>,
) {
    held_fixture_with_control_pause(false)
}

fn held_fixture_with_control_pause(control_pause: bool) -> (
    AppState,
    String,
    String,
    mpsc::Receiver<CodexRuntimeCommand>,
    Arc<ScriptedEngramControlTransport>,
) {
    let (state, project, session, _root, receiver) = fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        queue_prompt_on_record(
            record,
            PendingPrompt {
                id: "original-held-head".into(),
                timestamp: stamp_now(),
                text: "retained input".into(),
                expanded_text: None,
                source: None,
                attachments: Vec::new(),
                engram_interrupted: true,
                is_engram_retained: true,
            },
            Vec::new(),
        );
        record.queued_prompts[0].engram_interrupted = true;
        // The fixture represents historical delivery with no proof of a
        // never-promoted attempt, not merely an interrupted unsent envelope.
        record.queued_prompts[0].promotion_disposition_known = false;
        record.set_auto_dispatch_blocked(true);
        if control_pause {
            record.session.preview = ENGRAM_RETAINED_AUTHORIZATION_PREVIEW.to_owned();
            assert!(control_pause_is_owned(record));
        }
        state.commit_locked(&mut inner).unwrap();
    }
    let transport = ScriptedEngramControlTransport::new([]);
    install_control_only_transport(&state, transport.clone());
    state
        .update_project_engram_settings(&project, settings(&state, &project, false))
        .unwrap();
    assert_eq!(
        transport
            .requests()
            .iter()
            .map(|record| record.request["operation"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        ["turn_checkpoint", "turn_grant_read"]
    );
    (state, project, session, receiver, transport)
}

#[test]
fn human_prompt_off_resume_preserves_unconfirmed_checkpoint_without_control_requests() {
    for cancelled in [false, true] {
        for reload in [false, true] {
            let (state, _project, session, receiver, transport) = held_fixture();
            enqueue_human_for_resume(&state, &session, reload);
            let before = request_history(&transport);
            let original =
                serde_json::to_value(&state.inner.lock().unwrap().engram_session_obligations)
                    .unwrap();
            if cancelled {
                state
                    .cancel_queued_prompt(&session, "original-held-head")
                    .unwrap();
            }
            state.resume_session_queue(&session).expect(
                "control OFF must resume ordinary turns without settling the old checkpoint",
            );
            assert_eq!(
                request_history(&transport),
                before,
                "no control request during ordinary Resume"
            );
            assert!(matches!(
                receiver.try_recv().unwrap(),
                CodexRuntimeCommand::Prompt { .. }
            ));
            assert!(receiver.try_recv().is_err());
            let inner = state.inner.lock().unwrap();
            assert_eq!(
                serde_json::to_value(&inner.engram_session_obligations).unwrap(),
                original,
                "old checkpoint remains intact and unresolved"
            );
        }
    }
}

#[test]
fn human_prompt_on_send_is_ordinary_and_cannot_report_an_earlier_grant() {
    let (state, project, session, _root, receiver) = fixture();
    assert!(settings(&state, &project, true).is_control_enabled());
    let transport = ScriptedEngramControlTransport::new([]);
    install_control_only_transport(&state, transport.clone());
    let (policy, sandbox) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        (record.codex_approval_policy, record.codex_sandbox_mode)
    };
    let dispatch = match state.dispatch_human_turn(&session, prompt()).unwrap() {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("idle human prompt must start under control ON"),
    };
    assert!(dispatch.human_prompt_id().is_some());
    assert!(dispatch.engram_dispatch_generation().is_none());
    assert!(matches!(
        deliver_turn_dispatch(&state, dispatch),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert!(receiver.try_recv().is_err());
    assert!(
        transport.requests().is_empty(),
        "human admission sends no control operation"
    );
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(human_prompt_turn_is_current(record));
    assert_eq!(
        record.engram.active_grant_id.as_deref(),
        Some("retained-grant")
    );
    assert_eq!(
        (record.codex_approval_policy, record.codex_sandbox_mode),
        (policy, sandbox)
    );
    assert_eq!(
        claude_observation_disposition(record, &EngramObservationProvenance::Ambient),
        ClaudeObservationDisposition::Unowned,
        "the earlier grant cannot take this turn's observations"
    );
    assert!(
        AppState::engram_checkpoint_grant_for_attempt_locked(
            &inner,
            &session,
            EngramCheckpointPurpose::TurnTerminal,
            None,
            None,
            None,
            false,
            None
        )
        .is_none()
    );
}

#[test]
fn human_prompt_source_is_explicit_and_survives_reload_without_upgrading_unknown_peers() {
    let (state, _project, session, _root, _receiver) = fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    for sender in [None, Some("a-deleted-peer")] {
        let route: SendMessageRouteRequest = serde_json::from_value(json!({
            "text": "historical or automated input", "sourceSessionId": sender,
        }))
        .unwrap();
        assert!(
            route.origin.is_none(),
            "missing source is not explicit human provenance"
        );
        dispatch_turn_and_snapshot_with_origin(&state, &session, route.prompt, route.origin)
            .unwrap();
    }
    let non_ui: SendMessageRequest = serde_json::from_value(json!({
        "text": "an automated envelope cannot opt in", "origin": "human",
    }))
    .unwrap();
    assert!(matches!(
        state.dispatch_turn(&session, non_ui).unwrap(),
        DispatchTurnResult::Queued
    ));
    let route: SendMessageRouteRequest = serde_json::from_value(json!({
        "text": "typed human input", "origin": "human",
    }))
    .unwrap();
    assert_eq!(route.origin, Some(ExplicitPromptOrigin::Human));
    dispatch_turn_and_snapshot_with_origin(&state, &session, route.prompt, route.origin).unwrap();
    let mixed: SendMessageRouteRequest = serde_json::from_value(json!({
        "text": "mixed origin", "origin": "human", "sourceSessionId": "a-deleted-peer",
    }))
    .unwrap();
    assert!(
        dispatch_turn_and_snapshot_with_origin(&state, &session, mixed.prompt, mixed.origin)
            .is_err()
    );
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.queued_prompts[0].source, QueuedPromptSource::User);
    assert_eq!(record.queued_prompts[1].source, QueuedPromptSource::User);
    assert_eq!(record.queued_prompts[2].source, QueuedPromptSource::User);
    assert_eq!(record.queued_prompts[3].source, QueuedPromptSource::Human);
    let data = serde_json::to_vec(&PersistedSessionRecord::from_record(record)).unwrap();
    let loaded = serde_json::from_slice::<PersistedSessionRecord>(&data)
        .unwrap()
        .into_record()
        .unwrap();
    assert_eq!(loaded.queued_prompts[0].source, QueuedPromptSource::User);
    assert_eq!(loaded.queued_prompts[1].source, QueuedPromptSource::User);
    assert_eq!(loaded.queued_prompts[2].source, QueuedPromptSource::User);
    assert_eq!(loaded.queued_prompts[3].source, QueuedPromptSource::Human);
    assert_eq!(QueuedPromptSource::default(), QueuedPromptSource::User);
    assert_ne!(
        engram_turn_intent_fingerprint("same", None, &[], None, QueuedPromptSource::User),
        engram_turn_intent_fingerprint("same", None, &[], None, QueuedPromptSource::Human)
    );
}

#[test]
fn human_prompt_unknown_handoff_is_retained_after_reload_and_never_replayed() {
    let (state, _project, session, _root, receiver) = fixture();
    let dispatch = match state.dispatch_human_turn(&session, prompt()).unwrap() {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("human opening must be prepared"),
    };
    let prompt_id = dispatch.human_prompt_id().unwrap().to_owned();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &inner.sessions[index];
        let runtime = record.runtime.clone();
        let saved = serde_json::to_vec(&PersistedSessionRecord::from_record(record)).unwrap();
        let mut loaded = serde_json::from_slice::<PersistedSessionRecord>(&saved)
            .unwrap()
            .into_record()
            .unwrap();
        assert!(loaded.human_prompt_turn.is_none());
        let head = loaded.queued_prompts.front().unwrap();
        assert_eq!(head.pending_prompt.id, prompt_id);
        assert!(head.engram_ordinary_handoff && head.engram_interrupted);
        loaded.runtime = runtime;
        inner.sessions[index] = loaded;
    }
    assert!(matches!(
        state.start_human_queued_turn(&session, true).unwrap(),
        HumanPromptDrain::Absent
    ));
    // The Human drainer owns no executable turn after reload. Ordinary
    // recovery may run, but this original unknown handoff cannot be replayed.
    assert!(!matches!(
        deliver_turn_dispatch(&state, dispatch),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    let retained = record.queued_prompts.iter()
        .find(|queued| queued.pending_prompt.id == prompt_id).unwrap();
    assert_eq!(retained.source, QueuedPromptSource::Human);
    assert!(retained.engram_ordinary_handoff && retained.engram_interrupted);
}

#[test]
fn project_containment_off_unproven_pause_stays_visible_until_one_resume() {
    let (state, _project, session, receiver, transport) = held_fixture();
    let before_requests = request_history(&transport);
    let (head, debt) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.orchestrator_auto_dispatch_blocked && record.session.queue_paused);
        assert!(!control_pause_is_owned(record));
        (serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            serde_json::to_value(&inner.engram_session_obligations).unwrap())
    };
    let request = prompt();
    let text = request.text.clone();
    assert!(matches!(state.dispatch_turn(&session, request).unwrap(), DispatchTurnResult::Queued));
    assert!(receiver.try_recv().is_err());
    state.resume_session_queue(&session).unwrap();
    assert!(matches!(phase_sync::receive(&receiver, "one explicit Resume of unproven pause"),
        CodexRuntimeCommand::Prompt { command, .. } if command.prompt.ends_with(&text)));
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(!record.orchestrator_auto_dispatch_blocked && !record.session.queue_paused);
    assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), head);
    assert_eq!(serde_json::to_value(&inner.engram_session_obligations).unwrap(), debt);
    assert_eq!(request_history(&transport), before_requests);
}

#[test]
fn project_containment_off_queue_dispatches_durably_without_settling_after_restart_or_cancel() {
    // This case starts behind a proven control hold. The default fixture's
    // unproven pause remains an independent barrier until explicit Resume.
    let (state, project, session, receiver, transport) = held_fixture_with_control_pause(true);
    let before_requests = request_history(&transport);
    let (head, entry) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        (
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            serde_json::to_value(&inner.engram_session_obligations.entries[0]).unwrap(),
        )
    };
    let dispatch = match state.dispatch_turn(&session, prompt()).unwrap() {
        DispatchTurnResult::Dispatched(dispatch) | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("OFF ordinary work must pass the unknown historical head"),
    };
    assert!(matches!(deliver_turn_dispatch(&state, dispatch), TurnDispatchDeliveryOutcome::Delivered));
    assert!(matches!(phase_sync::receive(&receiver, "OFF ordinary work behind historical debt"),
        CodexRuntimeCommand::Prompt { .. }));
    assert!(receiver.try_recv().is_err());
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            head
        );
        assert_eq!(
            serde_json::to_value(&inner.engram_session_obligations.entries[0]).unwrap(),
            entry
        );
        assert!(!record.orchestrator_auto_dispatch_blocked && !record.session.queue_paused);
    }
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
    assert_eq!(record.queued_prompts.len(), 1);
    assert!(!record.orchestrator_auto_dispatch_blocked);
    assert!(engram_session_obligation_locked(&loaded, &session).is_some());
    state
        .cancel_queued_prompt(&session, "original-held-head")
        .unwrap();
    let token = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session).unwrap()].runtime.runtime_token().unwrap()
    };
    state.finish_turn_ok_if_runtime_matches(&session, &token).unwrap();
    state.resume_session_queue(&session).unwrap();
    assert!(
        state
            .dispatch_next_queued_turn(&session, false)
            .unwrap()
            .is_none()
    );
    assert!(
        state
            .inner
            .lock()
            .unwrap()
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .is_some()
    );
    assert_eq!(request_history(&transport), before_requests);
    assert!(receiver.try_recv().is_err());
    assert_eq!(serde_json::to_value(&state.inner.lock().unwrap().engram_session_obligations.entries[0]).unwrap(), entry);
}

#[test]
fn project_containment_legacy_control_queue_rechecks_every_eligibility_fact_after_parsing() {
    for changed in [
        "owner", "settings", "project", "head", "entry", "reset", "terminal",
    ] {
        let (state, project, session, receiver, transport) = held_fixture();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].set_auto_dispatch_blocked(true);
        }
        let before_requests = request_history(&transport);
        let target = state.clone();
        let target_session = session.clone();
        let target_project = project.clone();
        TEST_CONTAINED_QUEUE_AFTER_PARSE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                let mut inner = target.inner.lock().unwrap();
                let index = inner.find_session_index(&target_session).unwrap();
                match changed {
                    "owner" => inner.sessions[index].engram.dispatch_generation += 1,
                    "settings" => {
                        inner
                            .projects
                            .iter_mut()
                            .find(|p| p.id == target_project)
                            .unwrap()
                            .engram
                            .as_mut()
                            .unwrap()
                            .turn_gated_control = true
                    }
                    "project" => inner.sessions[index].session.project_id = None,
                    "head" => inner.sessions[index].queued_prompts[0]
                        .pending_prompt
                        .text
                        .push_str(" changed"),
                    "entry" => {
                        inner.engram_session_obligations.entries[0].reason_code =
                            "owner_changed".into()
                    }
                    "reset" => inner.sessions[index].engram.project_reset_in_progress = true,
                    "terminal" => inner.sessions[index].codex_terminal_identity_unresolved = true,
                    _ => unreachable!(),
                }
                target.commit_locked(&mut inner).unwrap();
            }))
        });
        assert!(
            state.queue_user_behind_contained_head(&session, &prompt(), None).is_err(),
            "{changed}"
        );
        assert_eq!(
            state
                .inner
                .lock()
                .unwrap()
                .sessions
                .iter()
                .find(|r| r.session.id == session)
                .unwrap()
                .queued_prompts
                .len(),
            1,
            "a changed predicate must not fall through to dispatch or queue append"
        );
        assert_eq!(request_history(&transport), before_requests);
        assert!(receiver.try_recv().is_err());
        TEST_CONTAINED_QUEUE_AFTER_PARSE.with(|hook| assert!(hook.borrow().is_none()));
    }
}

#[test]
fn project_containment_legacy_control_queue_unknown_or_failed_persistence_never_reports_acceptance() {
    for failed_write in [false, true] {
        let (state, _project, session, receiver, transport) = held_fixture();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].set_auto_dispatch_blocked(true);
        }
        let before_requests = request_history(&transport);
        if failed_write {
            let state = AppState {
                persistence_path: Arc::new(
                    state.test_temp_root.as_ref().unwrap().path().to_path_buf(),
                ),
                ..state
            };
            assert!(state.queue_user_behind_contained_head(&session, &prompt(), None).is_err());
        } else {
            TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
            let result = state.queue_user_behind_contained_head(&session, &prompt(), None);
            TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
            assert!(result.is_err(), "an unconfirmed queue write is not Queued");
            assert!(
                engram_session_obligation_locked(&state.inner.lock().unwrap(), &session).is_some()
            );
        }
        assert_eq!(request_history(&transport), before_requests);
        assert!(receiver.try_recv().is_err());
    }
}
