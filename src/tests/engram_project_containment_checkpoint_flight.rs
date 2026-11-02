//! Public administration and ordinary owner fences beside passive history.
use super::*;

#[test]
fn ordinary_administration_preserves_original_checkpoint_without_settlement() {
    for progress in ["unknown", "refused", "accepted"] {
        for action in ["disable", "session_delete", "project_delete"] {
            for reload in [false, true] {
                let (turn, mut operation) = passive_checkpoint_turn("checkpoint-administration");
                if progress == "accepted" { accepted_history(&mut operation); }
                if progress == "refused" {
                    append_historical_result(&mut operation, EngramCheckpointAttemptResult::Refused {
                        response: json!({"decision":"refuse","code":"stale_fence"}),
                    });
                }
                operation.validate().unwrap();
                store_passive_checkpoint(&turn, &operation);
                if reload {
                    turn.state.shutdown_persist_blocking();
                    *turn.state.inner.lock().unwrap() =
                        load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
                    turn.state.select_test_scripted_engram_budget_clock();
                    turn.state.install_control_test_transport(turn.transport.clone());
                }
                match action {
                    "disable" => {
                        let mut settings = turn.state.inner.lock().unwrap()
                            .find_project(&turn.project).unwrap().engram.clone().unwrap();
                        settings.enabled = false;
                        turn.state.update_project_engram_settings(&turn.project, settings).unwrap();
                    }
                    "session_delete" => { turn.state.kill_session(&turn.session).unwrap(); }
                    "project_delete" => { turn.state.delete_project(&turn.project).unwrap(); }
                    _ => unreachable!(),
                }
                let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
                match action {
                    "disable" => assert!(!loaded.find_project(&turn.project).unwrap().engram.as_ref().unwrap().enabled),
                    "session_delete" => assert!(loaded.find_session_index(&turn.session).is_none()),
                    "project_delete" => assert!(loaded.find_project(&turn.project).is_none()),
                    _ => unreachable!(),
                }
                assert_passive_checkpoint(&turn, &operation);
                turn.assert_no_prompt();
            }
        }
    }
}

#[test]
fn ordinary_fresh_delivery_and_completion_preserve_unknown_checkpoint_history() {
    let (turn, original) = passive_checkpoint_turn("checkpoint-fresh-delivery");
    let text = "Distinct fresh ordinary input, never the historical operation";
    let dispatch = turn.prepare(text);
    let token = dispatch.runtime_token().clone();
    let generation = dispatch.active_turn_generation();
    assert!(matches!(deliver_turn_dispatch(&turn.state, dispatch), TurnDispatchDeliveryOutcome::Delivered));
    assert!(turn.prompt().prompt.contains(text));
    turn.state.finish_turn_ok_if_runtime_and_generation_match(&turn.session, &token, generation).unwrap();
    turn.assert_no_prompt();
    assert_passive_checkpoint(&turn, &original);
}

#[test]
fn passive_checkpoint_cannot_authorize_a_stale_ordinary_runtime_or_turn_handoff() {
    for replacement in ["runtime", "turn"] {
        let (turn, original) = passive_checkpoint_turn("checkpoint-stale-handoff");
        let dispatch = turn.prepare("Unsent old ordinary input");
        turn.edit(|record| {
            if replacement == "runtime" { record.runtime = SessionRuntime::None; }
            else { record.active_turn_generation += 1; }
        });
        let before = turn.saved();
        assert!(matches!(deliver_turn_dispatch(&turn.state, dispatch), TurnDispatchDeliveryOutcome::Superseded));
        assert_eq!(turn.saved(), before, "a stale handoff cannot mutate its successor");
        turn.assert_no_prompt();
        assert_passive_checkpoint(&turn, &original);
    }
}

fn attach_existing_checkpoint_child(state: &AppState, parent: &str, child: &str) {
    let (delegation, placeholder) =
        crate::tests::delegation_support::install_required_review_delegation(state, parent);
    let mut inner = state.inner.lock().unwrap();
    inner.delegations.iter_mut().find(|row| row.id == delegation).unwrap()
        .child_session_id = child.to_owned();
    inner.session_mut(child).unwrap().session.parent_delegation_id = Some(delegation);
    let index = inner.find_session_index(&placeholder).unwrap();
    inner.remove_session_at(index).unwrap();
    state.commit_locked(&mut inner).unwrap();
}

#[test]
fn passive_checkpoint_history_does_not_veto_public_delegated_subtree_removal() {
    let (turn, original) = passive_checkpoint_turn("checkpoint-subtree-history");
    let parent = create_test_project_session(&turn.state, Agent::Codex, &turn.project, &turn.root);
    attach_existing_checkpoint_child(&turn.state, &parent, &turn.session);
    turn.state.kill_session(&parent).unwrap();
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    assert!(loaded.find_session_index(&parent).is_none());
    assert!(loaded.find_session_index(&turn.session).is_none());
    assert_passive_checkpoint(&turn, &original);
    turn.assert_no_prompt();
}
