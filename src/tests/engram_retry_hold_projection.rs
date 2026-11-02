//! Ordinary pause projections beside passive historical retry cards.
//! Provider-capacity retry has its separate live owner and disposition.
use super::*;

/// A wake behind an ordinary idle pause stays queued until explicit Resume.
pub(crate) const PAUSED_QUEUE_DISPOSITION: &str = "heldBehindPausedQueue";

pub(crate) fn wire_queue_holds(state: &AppState, session: &str) -> (Value, Value) {
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    let full = serde_json::to_value(AppState::wire_session_from_record(
        &state.server_instance_id, record,
    )).unwrap();
    let summary = serde_json::to_value(AppState::wire_session_summary_from_record(
        &state.server_instance_id, record,
    )).unwrap();
    assert!(full.get("queueHold").is_none(), "the removed control projection must be absent");
    assert!(summary.get("queueHold").is_none(), "the summary must not retain the removed control projection");
    (
        full.get("queueHold").cloned().unwrap_or(Value::Null),
        summary.get("queueHold").cloned().unwrap_or(Value::Null),
    )
}

fn latched(state: &AppState, session: &str) -> bool {
    let inner = state.inner.lock().unwrap();
    inner.sessions[inner.find_session_index(session).unwrap()].orchestrator_auto_dispatch_blocked
}

pub(crate) fn assert_plain_pause(state: &AppState, session: &str, label: &str) {
    assert!(latched(state, session), "{label}: the queue stays paused");
    assert_eq!(state.mailbox_queued_wake_disposition(session), PAUSED_QUEUE_DISPOSITION);
    let (full, summary) = wire_queue_holds(state, session);
    assert_eq!(full, Value::Null, "{label}: no automatic retry is claimed");
    assert_eq!(summary, Value::Null, "{label}: nor in the summary");
}

pub(crate) fn assert_no_hold(state: &AppState, session: &str, label: &str) {
    assert!(!latched(state, session), "{label}: the queue is no longer held");
    let (full, summary) = wire_queue_holds(state, session);
    assert_eq!(full, Value::Null, "{label}: no retry hold remains");
    assert_eq!(summary, Value::Null, "{label}: nor in the summary");
}

#[test]
fn historical_control_retry_cards_never_project_or_execute_an_automatic_hold() {
    for key in ["engramAdmissionRetry", "engramBeginPreparationLineage", "engramAbortRetry", "engramBindRetry"] {
        let label = "passive-retry-projection";
        let (state, receiver) =
            crate::tests::delegation_support::test_app_state_with_delegation_codex_runtime(label);
        state.select_test_scripted_engram_budget_clock();
        let root = state.test_temp_root.as_ref().unwrap().path().join(label);
        fs::create_dir_all(&root).unwrap();
        let project = create_test_project(&state, &root, "Ordinary retry history projection");
        let session = create_test_project_session(&state, Agent::Codex, &project, &root);
        fs::write(root.join(".engram-project"), format!("{project}\n")).unwrap();
        let transport = ScriptedEngramControlTransport::new([]);
        state.install_control_test_transport(transport.clone());
        let card = json!({
            "due_at":"2000-01-01T00:00:00Z", "attempts":3, "cause":"control_store_busy",
            "originalKey":"original-unknown-operation", "outcome":"unknown",
            "opaque":[null, {"kept":"źródło 🦀"}],
        });
        {
            let mut inner = state.inner.lock().unwrap();
            inner.projects.iter_mut().find(|entry| entry.id == project).unwrap().engram =
                Some(EngramProjectSettings {
                    enabled: true, execution_observability: false,
                    home: Some(root.to_string_lossy().into_owned()),
                    binary_path: Some(root.join("missing-engram.exe").to_string_lossy().into_owned()),
                    ..EngramProjectSettings::default()
                });
            inner.engram_declared_project_ids.insert(project.clone());
            inner.engram_declaration_checked_project_ids.insert(project);
            let record = inner.session_mut(&session).unwrap();
            record.engram.context_nudge_pending = false;
            record.set_auto_dispatch_blocked(true);
            record.engram.historical_control.insert(key.into(), card.clone());
            state.commit_locked(&mut inner).unwrap();
        }
        let prompt = {
            let mut inner = state.inner.lock().unwrap();
            let id = inner.next_message_id();
            queue_prompt_on_record_with_source(inner.session_mut(&session).unwrap(), PendingPrompt {
                engram_interrupted: false, is_engram_retained: false, attachments: Vec::new(),
                id: id.clone(), timestamp: stamp_now(), text: "current ordinary queue head".into(),
                expanded_text: None, source: None,
            }, Vec::new(), QueuedPromptSource::User);
            state.commit_locked(&mut inner).unwrap();
            id
        };
        let before = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap()
        };
        assert_plain_pause(&state, &session, key);
        state.provider_capacity_retry_tick(chrono::Utc::now() + chrono::Duration::hours(1));
        state.dispatch_orphaned_workflow_prompts();
        assert_plain_pause(&state, &session, key);
        let after = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert_eq!(record.queued_prompts[0].pending_prompt.id, prompt);
            assert!(record.provider_capacity_retry.is_none());
            assert_eq!(record.engram.historical_control[key], card);
            serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap()
        };
        assert_eq!(after, before, "neither projection nor ordinary ticks may mutate historical retry data");
        while let Ok(command) = receiver.try_recv() {
            assert!(!matches!(command, CodexRuntimeCommand::Prompt { .. }), "unexpected provider handoff");
        }
        assert!(transport.requests().is_empty(), "ordinary queue service must not call control");

        state.resume_session_queue(&session).unwrap();
        let command = loop {
            if let CodexRuntimeCommand::Prompt { session_id, command } =
                crate::tests::phase_sync::receive(&receiver, "ordinary provider prompt beside retry history")
            {
                assert_eq!(session_id, session);
                break command;
            }
        };
        assert!(command.prompt.contains("current ordinary queue head"));
        assert_no_hold(&state, &session, key);
        {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(record.queued_prompts.is_empty());
            assert_eq!(record.engram.historical_control[key], card, "Resume never renews a historical control operation");
        }
        while let Ok(command) = receiver.try_recv() {
            assert!(!matches!(command, CodexRuntimeCommand::Prompt { .. }), "the ordinary prompt is handed off exactly once");
        }
        assert!(transport.requests().is_empty(), "ordinary queue service must not call control");
        state.shutdown_persist_blocking();
    }
}
