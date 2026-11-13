// Acceptance requests must not replace a registered check's root identity
// with a later name. Historical commit-only evidence cannot prove the narrow
// complete-source equivalence required to retain a predecessor generation.
// Owns this admission boundary; naming and registration use the parent
// fixtures, and no live host or Engram store is accessed.
use super::*;

#[path = "acceptance_same_path_recovery.rs"]
mod same_path_recovery;

/// Arrange the completed second naming and publish its canonical proof.
/// The registration is deliberately untouched: its root is historical fact.
fn rename_same_workspace(state: &AppState, parent: &str, store: &EngramAuthorityStoreKey) {
    rename_same_workspace_with_binding(state, parent, store, named_parent_binding());
}

fn rename_same_workspace_with_binding(
    state: &AppState,
    parent: &str,
    store: &EngramAuthorityStoreKey,
    binding: EngramControlWorkBinding,
) {
    let entry = {
        let mut inner = state.inner.lock().unwrap();
        let entry = &mut inner.engram_work_source_roots[0];
        assert_eq!(entry.generation, 1);
        entry.generation = 2;
        entry.claim_fence = binding.claim_fence;
        entry.named_at = "2026-09-27T00:01:00.000Z".into();
        let entry = entry.clone();
        let index = inner.find_session_index(parent).unwrap();
        inner.sessions[index].engram.named_root = Some(EngramNamedRootState::Bound {
            workspace_id: entry.root,
            generation: 2,
            named_at: entry.named_at,
        });
        inner.sessions[index].engram.work_binding = Some(binding.clone());
        inner.engram_work_source_roots[0].clone()
    };
    // Supply the new naming's canonical read explicitly. Registration can
    // retain other historical reads, including an unbound proof at index 0.
    let proof: EngramNamedRootReadResponse = serde_json::from_value(json!({
        "project_id":store.project_id,"work_id":binding.work_id,"run_id":binding.run_id,
        "claim_id":binding.claim_id,"root_execution_id":binding.root_execution_id,
        "run":{"state":"open","generation":2},
        "named_root":{"state":"bound","workspace_id":entry.root,
            "generation":2,"named_at":entry.named_at},
        "latest_event":{"event":"fixture-renamed-event",
            "position":{"feed":{"kind":"run_execution","id":binding.run_id},"position":2},
            "kind":"bound","workspace_id":entry.root,"generation":2,"named_at":entry.named_at},
        "read_cut":{"feed":{"kind":"run_execution","id":binding.run_id},"position":2}
    }))
    .unwrap();
    let owner = state
        .prepare_engram_authority(store, &binding, Duration::from_secs(2))
        .unwrap();
    AppState::learn_engram_authority_locked(
        &mut state.inner.lock().unwrap(),
        store,
        &owner,
        &proof,
    )
    .unwrap();
    state
        .publish_engram_authority(store, &owner, Duration::from_secs(2))
        .unwrap();
}

fn assert_rebound_check_refuses_request(mode: &'static str) {
    let (state, parent, store, named_root) = named_root_parent();
    let head = commit_named_root(&named_root);
    let registration = register_committed_run(
        &state,
        &parent,
        &named_root,
        committed_results("passed", input_record(&head, "clean")),
    );
    host_observed(&state, &registration, &head, acknowledged("settled"));
    assert!(matches!(
        decide(&state, &parent),
        Ok(AcceptanceCommittedInputDecision::Admitted(_))
    ));

    rename_same_workspace(&state, &parent, &store);
    assert_eq!(
        run_git_test_command_output(&worktree(&named_root), &["rev-parse", "HEAD"]),
        head,
        "renaming did not change the judged commit"
    );
    assert_eq!(
        current_registration(&state).named_root,
        registration.named_root,
        "renaming did not relabel the registered check"
    );
    let mut answer = sighting_reply(&named_root, 2, present_sighting());
    answer["current_binding"] = json!("fixture-renamed-event");
    answer["root"]["binding_event"] = json!("fixture-renamed-event");
    answer["root"]["binding_position"] = json!(2);
    let transport = install_sighting_answer(&state, Ok(answer));

    // Retain the actual preflight decision in RED diagnostics. A replay
    // demonstrates this mechanism, not an unrecorded historical RPC result.
    eprintln!(
        "root rebind replay ({mode}): preflight={:?}",
        decide(&state, &parent)
    );
    let response = state.request_acceptance_evaluation_with_runner(
        &parent,
        evaluation_request((mode == "independent_session").then_some(Agent::Codex)),
        reader_admitting(mode),
    );
    let error = match response {
        Err(error) => error,
        Ok(response) => {
            let outcome = match response {
                AcceptanceEvaluationRequestResponse::Spawned { .. } => "spawned evaluator",
                AcceptanceEvaluationRequestResponse::SameSession { .. } => "returned brief",
            };
            eprintln!(
                "root rebind replay ({mode}): {outcome}; sighting reads={}",
                sighting_reads(&transport)
            );
            panic!("a registered check from generation 1 must not admit generation 2");
        }
    };
    assert_eq!(error.status, StatusCode::CONFLICT, "{}", error.message);
    assert!(state.inner.lock().unwrap().delegations.is_empty());
    assert!(
        error.message.contains("generation 1") && error.message.contains("generation 2"),
        "the conflict names both roots: {}",
        error.message
    );
    assert!(
        error.message.contains("registered check") && error.message.contains("current root"),
        "the conflict gives the remedy: {}",
        error.message
    );
    assert_eq!(
        sighting_reads(&transport),
        0,
        "a new sighting cannot repair an old check"
    );
}

#[test]
fn a_renamed_root_refuses_before_spawning_an_evaluator() {
    assert_rebound_check_refuses_request("independent_session");
}

#[test]
fn a_renamed_root_refuses_before_returning_a_same_session_brief() {
    assert_rebound_check_refuses_request("same_session");
}

#[test]
fn a_new_acknowledged_check_in_the_current_name_repairs_the_conflict() {
    let (state, parent, store, named_root) = named_root_parent();
    let head = commit_named_root(&named_root);
    let old = register_committed_run(
        &state,
        &parent,
        &named_root,
        committed_results("passed", input_record(&head, "clean")),
    );
    host_observed(&state, &old, &head, acknowledged("settled"));
    rename_same_workspace(&state, &parent, &store);
    let transport = install_sighting_transport(&state, &named_root, 2, json!({"state":"absent"}));
    let run_id = "test-after-root-renaming";
    let mut results = committed_results("passed", input_record(&head, "clean"));
    results["runId"] = json!(run_id);
    let new = register_committed_run_as(&state, &parent, &named_root, run_id, results);
    host_observed(&state, &new, &head, acknowledged("settled"));
    let response = request(&state, &parent).unwrap_or_else(|error| panic!("{}", error.message));
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response else {
        panic!("the new acknowledged check admits an evaluator");
    };
    let target = delegation.delegation.acceptance_evaluation.unwrap();
    assert_eq!(target.source_root.unwrap().generation, 2);
    assert_eq!(sighting_reads(&transport), 0);
    assert_eq!(current_registration(&state).named_root, old.named_root);
}

#[test]
fn unacknowledged_or_unrelated_checks_do_not_create_a_rebound_conflict() {
    for case in ["unacknowledged", "another store", "another claim"] {
        let (state, parent, store, named_root) = named_root_parent();
        let head = commit_named_root(&named_root);
        let registration = register_committed_run(
            &state,
            &parent,
            &named_root,
            committed_results("passed", input_record(&head, "clean")),
        );
        if case != "unacknowledged" {
            host_observed(&state, &registration, &head, acknowledged("settled"));
            let mut inner = state.inner.lock().unwrap();
            let mut registered = inner
                .test_run_registrations
                .get_mut(COMMITTED_RUN_ID)
                .unwrap();
            if case == "another store" {
                registered.authority_store.as_mut().unwrap().project_id = "another-project".into();
            } else {
                registered.work_binding.as_mut().unwrap().claim_id = "another-claim".into();
            }
        }
        rename_same_workspace(&state, &parent, &store);
        assert_eq!(
            decide(&state, &parent),
            Ok(AcceptanceCommittedInputDecision::Legacy),
            "{case}"
        );
    }
}

#[test]
fn a_new_name_without_an_earlier_check_still_uses_the_sighting_path() {
    let (state, parent, store, named_root) = named_root_parent();
    commit_named_root(&named_root);
    rename_same_workspace(&state, &parent, &store);
    let transport = install_sighting_transport(&state, &named_root, 2, json!({"state":"absent"}));
    assert_eq!(
        decide(&state, &parent),
        Ok(AcceptanceCommittedInputDecision::Legacy)
    );
    let error = refusal(&state, request(&state, &parent), "no earlier check");
    assert!(
        error.message.contains("has no sighting"),
        "{}",
        error.message
    );
    assert_eq!(sighting_reads(&transport), 1);
}
