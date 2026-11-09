//! Ordinary queue, explicit Human provenance, and durable failure boundaries.
use super::super::uncertain_begin::OrdinaryRoot;
use super::*;

pub(super) fn retained_queue_fixture() -> OrdinaryRoot {
    let turn = OrdinaryRoot::new("retained-ordinary-queue");
    recovery::install_passive_original(&turn);
    turn.edit(|record| {
        let mut pending = human_choices::selection_prompt("original-held-head");
        pending.text = "retained input".into();
        pending.engram_interrupted = true;
        pending.is_engram_retained = true;
        queue_prompt_on_record(record, pending, Vec::new());
        record.queued_prompts[0].engram_interrupted = true;
        record.queued_prompts[0].engram_ordinary_handoff = true;
        record.queued_prompts[0].promotion_disposition_known = false;
        record.set_auto_dispatch_blocked(true);
    });
    turn
}

#[test]
fn human_prompt_resume_preserves_passive_checkpoint_without_control_requests() {
    for cancelled in [false, true] {
        for reload in [false, true] {
            let turn = retained_queue_fixture();
            let duties =
                serde_json::to_value(&turn.state.inner.lock().unwrap().engram_session_obligations)
                    .unwrap();
            enqueue_human_for_resume(&turn.state, &turn.session, reload);
            if cancelled {
                turn.state
                    .cancel_queued_prompt(&turn.session, "original-held-head")
                    .unwrap();
            }
            turn.state.resume_session_queue(&turn.session).unwrap();
            let command = turn.prompt();
            assert!(command.prompt.contains("Distinct ordinary input."));
            turn.assert_no_prompt();
            recovery::assert_passive_original(&turn, &duties);
        }
    }
}

#[test]
fn human_prompt_send_is_ordinary_and_cannot_report_an_earlier_grant() {
    let turn = OrdinaryRoot::new("human-ordinary-provenance");
    let duties = recovery::install_passive_original(&turn);
    let (policy, sandbox) =
        turn.record(|record| (record.codex_approval_policy, record.codex_sandbox_mode));
    let dispatch = match turn
        .state
        .dispatch_human_turn(&turn.session, prompt())
        .unwrap()
    {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("idle explicit Human must prepare"),
    };
    let original_id = dispatch.human_prompt_id().unwrap().to_owned();
    assert!(matches!(
        deliver_turn_dispatch(&turn.state, dispatch),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    assert!(turn.prompt().prompt.contains("Distinct ordinary input."));
    turn.assert_no_prompt();
    turn.record(|record| {
        assert!(human_prompt_turn_is_current(record));
        assert_eq!(
            record.human_prompt_turn.as_ref().unwrap().prompt_id,
            original_id
        );
        assert_eq!(
            (record.codex_approval_policy, record.codex_sandbox_mode),
            (policy, sandbox)
        );
        assert!(
            record.engram.unadmitted_active_turn.is_none(),
            "observation is off"
        );
    });
    recovery::assert_passive_original(&turn, &duties);
}

#[test]
fn human_prompt_source_is_explicit_and_survives_reload_without_upgrading_unknown_peers() {
    let turn = OrdinaryRoot::new("explicit-human-source");
    let state = turn.state.clone();
    let session = turn.session.clone();
    turn.edit(|record| record.session.status = SessionStatus::Active);
    for sender in [None, Some("a-deleted-peer")] {
        let route: SendMessageRouteRequest = serde_json::from_value(json!({
            "text": "historical or automated input", "sourceSessionId": sender,
        }))
        .unwrap();
        assert!(route.origin.is_none());
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
    turn.record(|record| {
        assert_eq!(
            record
                .queued_prompts
                .iter()
                .map(|q| q.source)
                .collect::<Vec<_>>(),
            [
                QueuedPromptSource::User,
                QueuedPromptSource::User,
                QueuedPromptSource::User,
                QueuedPromptSource::Human
            ]
        );
        let saved = serde_json::to_vec(&PersistedSessionRecord::from_record(record)).unwrap();
        let loaded = serde_json::from_slice::<PersistedSessionRecord>(&saved)
            .unwrap()
            .into_record()
            .unwrap();
        assert_eq!(
            loaded
                .queued_prompts
                .iter()
                .map(|q| q.source)
                .collect::<Vec<_>>(),
            record
                .queued_prompts
                .iter()
                .map(|q| q.source)
                .collect::<Vec<_>>()
        );
        for (before, after) in record.queued_prompts.iter().zip(&loaded.queued_prompts) {
            assert_eq!(before.pending_prompt.text, after.pending_prompt.text);
            assert_eq!(before.pending_prompt.id, after.pending_prompt.id);
        }
    });
    assert_eq!(QueuedPromptSource::default(), QueuedPromptSource::User);
    assert_ne!(
        queued_prompt_intent_fingerprint("same", None, &[], None, QueuedPromptSource::User),
        queued_prompt_intent_fingerprint("same", None, &[], None, QueuedPromptSource::Human)
    );
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn human_prompt_unknown_handoff_is_retained_after_reload_and_never_replayed() {
    let turn = OrdinaryRoot::new("human-unknown-reload");
    let state = turn.state.clone();
    let session = turn.session.clone();
    let dispatch = match state.dispatch_human_turn(&session, prompt()).unwrap() {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("Human opening must prepare"),
    };
    let prompt_id = dispatch.human_prompt_id().unwrap().to_owned();
    let (expected_prompt, expected_deliveries) = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &inner.sessions[index];
        let runtime = record.runtime.clone();
        let expected_prompt =
            serde_json::to_value(&record.queued_prompts[0].pending_prompt).unwrap();
        let saved = serde_json::to_vec(&PersistedSessionRecord::from_record(record)).unwrap();
        let mut loaded = serde_json::from_slice::<PersistedSessionRecord>(&saved)
            .unwrap()
            .into_record()
            .unwrap();
        assert!(loaded.human_prompt_turn.is_none());
        assert_eq!(loaded.queued_prompts[0].pending_prompt.id, prompt_id);
        // Ordinary Human promotion records uncertainty with this marker;
        // the old control-specific interrupted flag is not required.
        assert!(loaded.queued_prompts[0].engram_ordinary_handoff);
        assert_eq!(
            serde_json::to_value(&loaded.queued_prompts[0].pending_prompt).unwrap(),
            expected_prompt
        );
        assert!(!ordinary_queue_entry_is_runnable(
            &loaded,
            &loaded.queued_prompts[0]
        ));
        assert_eq!(
            loaded.prompt_deliveries.len(),
            record.prompt_deliveries.len()
        );
        assert!(
            record
                .prompt_deliveries
                .keys()
                .all(|id| loaded.prompt_deliveries.contains_key(id))
        );
        let expected_deliveries = serde_json::to_value(&loaded.prompt_deliveries).unwrap();
        loaded.runtime = runtime;
        inner.sessions[index] = loaded;
        (expected_prompt, expected_deliveries)
    };
    assert!(matches!(
        state.start_human_queued_turn(&session, true).unwrap(),
        HumanPromptDrain::Absent
    ));
    assert!(!matches!(
        deliver_turn_dispatch(&state, dispatch),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    turn.assert_no_prompt();
    turn.record(|record| {
        let retained = record
            .queued_prompts
            .iter()
            .find(|q| q.pending_prompt.id == prompt_id)
            .unwrap();
        assert_eq!(retained.source, QueuedPromptSource::Human);
        assert!(retained.engram_ordinary_handoff);
        assert_eq!(
            serde_json::to_value(&retained.pending_prompt).unwrap(),
            expected_prompt
        );
        assert!(!ordinary_queue_entry_is_runnable(record, retained));
        assert_eq!(
            serde_json::to_value(&record.prompt_deliveries).unwrap(),
            expected_deliveries
        );
    });
    turn.assert_no_control();
}

#[test]
fn ordinary_unproven_pause_stays_visible_until_one_resume() {
    let turn = retained_queue_fixture();
    let head = turn.record(|record| {
        assert!(record.orchestrator_auto_dispatch_blocked && record.session.queue_paused);
        serde_json::to_value(&record.queued_prompts[0]).unwrap()
    });
    let duties =
        serde_json::to_value(&turn.state.inner.lock().unwrap().engram_session_obligations).unwrap();
    // Preserve the frozen unproven-pause contract without adding a runnable
    // Human head to change the premise of this negative control.
    assert!(matches!(
        turn.state.dispatch_turn(&turn.session, prompt()).unwrap(),
        DispatchTurnResult::Queued
    ));
    turn.assert_no_prompt();
    turn.record(|record| {
        assert!(record.orchestrator_auto_dispatch_blocked && record.session.queue_paused)
    });
    turn.state.resume_session_queue(&turn.session).unwrap();
    assert!(turn.prompt().prompt.contains("Distinct ordinary input."));
    turn.assert_no_prompt();
    turn.record(|record| {
        assert!(!record.orchestrator_auto_dispatch_blocked && !record.session.queue_paused);
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            head
        );
    });
    recovery::assert_passive_original(&turn, &duties);
}

#[test]
fn ordinary_queue_dispatches_durably_without_settling_history_after_restart_or_cancel() {
    let turn = retained_queue_fixture();
    let head = turn.record(|record| serde_json::to_value(&record.queued_prompts[0]).unwrap());
    let duties =
        serde_json::to_value(&turn.state.inner.lock().unwrap().engram_session_obligations).unwrap();
    // Explicit Resume owns the pause; a historical control marker does not.
    turn.state.resume_session_queue(&turn.session).unwrap();
    let dispatch = turn.prepare("ordinary work beside passive history");
    let token = dispatch.runtime_token().clone();
    let generation = dispatch.active_turn_generation();
    assert!(matches!(
        deliver_turn_dispatch(&turn.state, dispatch),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    assert!(
        turn.prompt()
            .prompt
            .contains("ordinary work beside passive history")
    );
    turn.assert_no_prompt();
    turn.record(|record| {
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            head
        );
    });
    let loaded = load_state(turn.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.session).unwrap()];
    assert_eq!(record.queued_prompts.len(), 1);
    assert!(!record.orchestrator_auto_dispatch_blocked);
    turn.state
        .cancel_queued_prompt(&turn.session, "original-held-head")
        .unwrap();
    turn.state
        .finish_turn_ok_if_runtime_and_generation_match(&turn.session, &token, generation)
        .unwrap();
    turn.state.resume_session_queue(&turn.session).unwrap();
    assert!(
        turn.state
            .dispatch_next_queued_turn(&turn.session, false)
            .unwrap()
            .is_none()
    );
    turn.assert_no_prompt();
    recovery::assert_passive_original(&turn, &duties);
}

#[test]
fn ordinary_selected_queue_rechecks_owner_pause_stop_reset_and_terminal_facts() {
    for changed in ["owner", "head", "pause", "stop", "reset", "terminal"] {
        let turn = OrdinaryRoot::new("selected-queue-revalidation");
        turn.enqueue("exact original Human", QueuedPromptSource::Human);
        turn.edit(|record| record.set_auto_dispatch_blocked(true));
        let selected = human_choices::selection(&turn.state, &turn.session);
        let state = turn.state.clone();
        let session = turn.session.clone();
        let project = turn.project.clone();
        let reached = Arc::new(AtomicBool::new(false));
        let observed = reached.clone();
        TEST_HUMAN_BEFORE_PROMOTION.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                observed.store(true, Ordering::SeqCst);
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&session).unwrap();
                match changed {
                    "owner" => inner.sessions[index].active_turn_generation += 1,
                    "head" => inner.sessions[index].queued_prompts[0]
                        .pending_prompt
                        .text
                        .push_str(" changed"),
                    "pause" => {
                        inner.sessions[index].set_auto_dispatch_blocked(false);
                        inner.sessions[index].set_auto_dispatch_blocked(true);
                    }
                    "stop" => inner.sessions[index].runtime_stop_in_progress = true,
                    "reset" => {
                        assert!(inner.claim_engram_project_reset(&project).is_some());
                    }
                    "terminal" => inner.sessions[index].codex_terminal_identity_unresolved = true,
                    _ => unreachable!(),
                }
                inner.stamp_session_at_index(index);
            }));
        });
        assert!(
            turn.state
                .resume_selected_human_prompt(&turn.session, selected)
                .is_err(),
            "{changed}"
        );
        assert!(
            reached.load(Ordering::SeqCst),
            "{changed}: boundary must execute"
        );
        turn.record(|record| {
            assert_eq!(record.queued_prompts.len(), 1);
            assert!(record.session.messages.iter().all(|message| !matches!(message, Message::Text { text, .. } if text=="exact original Human")));
        });
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

#[test]
fn ordinary_queue_failed_own_row_persistence_never_reports_acceptance() {
    for directory_path in [false, true] {
        let turn = retained_queue_fixture();
        let before = turn.record(|record| serde_json::to_value(&record.queued_prompts).unwrap());
        let state = if directory_path {
            AppState {
                persistence_path: Arc::new(turn.root.clone()),
                ..turn.state.clone()
            }
        } else {
            let connection =
                rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
            connection.execute_batch(&format!(
                "CREATE TRIGGER refuse_queue_row BEFORE UPDATE ON sessions WHEN NEW.id='{}' BEGIN SELECT RAISE(ABORT,'own-row queue refusal'); END;", turn.session
            )).unwrap();
            turn.state.clone()
        };
        assert!(state.dispatch_human_turn(&turn.session, prompt()).is_err());
        turn.record(|record| {
            assert_eq!(
                serde_json::to_value(&record.queued_prompts).unwrap(),
                before
            )
        });
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}
