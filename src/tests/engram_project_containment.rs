//! Ordinary project settings and delivery beside passive original metadata.
//! Historical control records are data, never a containment or admission owner.
use super::*;

#[path = "engram_project_containment_admission.rs"]
mod admission;
#[path = "engram_project_containment_boundaries.rs"]
mod boundaries;
#[path = "engram_project_containment_checkpoint.rs"]
mod checkpoint;
#[path = "engram_human_queue_choices.rs"]
mod human_choices;
#[path = "engram_human_review_regressions.rs"]
mod human_review;
#[path = "engram_session_obligations.rs"]
mod obligations;
#[path = "engram_project_containment_queue.rs"]
mod queue;
#[path = "engram_project_containment_recovery.rs"]
mod recovery;

fn prompt() -> SendMessageRequest {
    SendMessageRequest {
        text: "Distinct ordinary input.".to_owned(),
        expanded_text: None,
        attachments: Vec::new(),
        source_session_id: None,
        source_mailbox: None,
    }
}

// Keep the public human-source boundary and its persisted source marker.
// No former grant, admission owner or replay executor is installed.
fn enqueue_human_for_resume(state: &AppState, session: &str, reload: bool) {
    let status = {
        let mut inner = state.inner.lock().unwrap();
        let record = inner.session_mut(session).unwrap();
        let status = record.session.status;
        record.session.status = SessionStatus::Active;
        status
    };
    assert!(matches!(
        state.dispatch_human_turn(session, prompt()).unwrap(),
        DispatchTurnResult::Queued
    ));
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(session).unwrap();
    inner.sessions[index].session.status = status;
    let human = inner.sessions[index]
        .queued_prompts
        .iter()
        .find(|head| head.source == QueuedPromptSource::Human)
        .unwrap();
    assert!(human.pending_prompt.source.is_none());
    assert!(human.engram_evaluate.is_none());
    assert!(!human.engram_ordinary_handoff);
    if reload {
        let runtime = inner.sessions[index].runtime.clone();
        let saved =
            serde_json::to_vec(&PersistedSessionRecord::from_record(&inner.sessions[index]))
                .unwrap();
        let mut restored = serde_json::from_slice::<PersistedSessionRecord>(&saved)
            .unwrap()
            .into_record()
            .unwrap();
        assert!(
            restored.human_prompt_turn.is_none(),
            "live handoff authority is not persisted"
        );
        assert!(
            restored
                .queued_prompts
                .iter()
                .any(|head| head.source == QueuedPromptSource::Human)
        );
        restored.runtime = runtime;
        inner.sessions[index] = restored;
    }
    state.commit_locked(&mut inner).unwrap();
}

#[test]
fn ignored_legacy_settings_preserve_an_unknown_head_and_allow_an_unrelated_ordinary_session() {
    let turn = super::uncertain_begin::OrdinaryRoot::new("ordinary-project-peer-progress");
    let original = recovery::install_passive_original(&turn);
    let head = turn.enqueue(
        "Unknown original provider handoff",
        QueuedPromptSource::User,
    );
    turn.edit(|record| {
        record.queued_prompts[0].engram_ordinary_handoff = true;
        record.queued_prompts[0].promotion_disposition_known = false;
        record.set_auto_dispatch_blocked(true);
    });
    let unknown = turn.record(|record| serde_json::to_value(&record.queued_prompts[0]).unwrap());
    for legacy in [true, false] {
        let mut request = serde_json::to_value(
            turn.state
                .inner
                .lock()
                .unwrap()
                .find_project(&turn.project)
                .unwrap()
                .engram
                .as_ref()
                .unwrap(),
        )
        .unwrap();
        request["turnGatedControl"] = json!(legacy);
        let settings: EngramProjectSettings = serde_json::from_value(request).unwrap();
        let response = turn
            .state
            .update_project_engram_settings(&turn.project, settings)
            .unwrap();
        let public = serde_json::to_string(&response).unwrap();
        assert!(!public.contains("turnGatedControl"));
        assert!(!public.contains("engramControlRecovery"));
        turn.record(|record| {
            assert_eq!(record.queued_prompts[0].pending_prompt.id, head);
            assert_eq!(
                serde_json::to_value(&record.queued_prompts[0]).unwrap(),
                unknown
            );
            assert!(record.orchestrator_auto_dispatch_blocked);
        });
        recovery::assert_passive_original(&turn, &original);
    }

    let peer = create_test_project_session(&turn.state, Agent::Codex, &turn.project, &turn.root);
    let dispatch = match turn.state.dispatch_turn(&peer, prompt()).unwrap() {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => {
            panic!("an unrelated idle session must make ordinary progress")
        }
    };
    let token = dispatch.runtime_token().clone();
    let generation = dispatch.active_turn_generation();
    assert!(matches!(
        deliver_turn_dispatch(&turn.state, dispatch),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    match phase_sync::receive(&turn.receiver, "unrelated ordinary provider handoff") {
        CodexRuntimeCommand::Prompt {
            session_id,
            command,
        } => {
            assert_eq!(session_id, peer);
            assert!(command.prompt.contains("Distinct ordinary input."));
        }
        _ => panic!("the unrelated session must receive its ordinary prompt"),
    }
    turn.state
        .finish_turn_ok_if_runtime_and_generation_match(&peer, &token, generation)
        .unwrap();
    turn.assert_no_prompt();
    turn.record(|record| {
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            unknown
        )
    });
    recovery::assert_passive_original(&turn, &original);
}
