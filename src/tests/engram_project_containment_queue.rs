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

fn held_fixture() -> (
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
        record.set_auto_dispatch_blocked(true);
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
fn project_containment_off_queue_append_is_durable_and_does_not_admit_after_restart_or_cancel() {
    let (state, project, session, receiver, transport) = held_fixture();
    let before_requests = request_history(&transport);
    let (head, entry) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        (
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            serde_json::to_value(&inner.engram_session_obligations.entries[0]).unwrap(),
        )
    };
    assert!(matches!(
        state.dispatch_turn(&session, prompt()).unwrap(),
        DispatchTurnResult::Queued
    ));
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.queued_prompts.len(), 2);
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            head
        );
        assert_eq!(
            serde_json::to_value(&inner.engram_session_obligations.entries[0]).unwrap(),
            entry
        );
        assert!(record.orchestrator_auto_dispatch_blocked && record.session.queue_paused);
    }
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
    assert_eq!(record.queued_prompts.len(), 2);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(engram_session_obligation_locked(&loaded, &session).is_some());
    state
        .cancel_queued_prompt(&session, "original-held-head")
        .unwrap();
    assert!(
        state.resume_session_queue(&session).is_err(),
        "cancellation is not control settlement"
    );
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
}

#[test]
fn project_containment_off_queue_rechecks_every_eligibility_fact_after_parsing() {
    for changed in [
        "owner", "settings", "project", "head", "entry", "reset", "terminal",
    ] {
        let (state, project, session, receiver, transport) = held_fixture();
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
            state.dispatch_turn(&session, prompt()).is_err(),
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
fn project_containment_off_queue_unknown_or_failed_persistence_never_reports_acceptance() {
    for failed_write in [false, true] {
        let (state, _project, session, receiver, transport) = held_fixture();
        let before_requests = request_history(&transport);
        if failed_write {
            let state = AppState {
                persistence_path: Arc::new(
                    state.test_temp_root.as_ref().unwrap().path().to_path_buf(),
                ),
                ..state
            };
            assert!(state.dispatch_turn(&session, prompt()).is_err());
        } else {
            TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
            let result = state.dispatch_turn(&session, prompt());
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
