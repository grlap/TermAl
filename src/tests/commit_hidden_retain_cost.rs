// Work done under StateInner by the per-commit hidden-session retention
// pass (`retain_hidden_source_observation_sessions`) on a large retained
// population of hidden delegation children. The scaling test prints its
// measurements for diagnosis; correctness tests assert outcomes only and use
// no elapsed-time thresholds.

use super::*;

fn delegation_for(parent: &str, child: &str, id: String, workdir: &str) -> DelegationRecord {
    DelegationRecord {
        original_project_id: None,
        project_basis_recorded: false,
        id,
        parent_session_id: parent.to_owned(),
        child_session_id: child.to_owned(),
        mode: DelegationMode::Reviewer,
        status: DelegationStatus::Completed,
        title: "Retained child".to_owned(),
        prompt: "Review.".to_owned(),
        cwd: workdir.to_owned(),
        agent: Agent::Codex,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: Some(stamp_now()),
        completed_at: Some(stamp_now()),
        result: None,
        submitted_review_result: None,
        post_submission_transport_error: None,
        base_revision: None,
        review_result_recovery_probe_attempt: None,
        review_result_recovery_error: None,
        review_result_schema_version: None,
        queued_followup_prompt_id: None,
        review_result_submission_attempt: 0,
        acceptance_evaluation: None,
        attempt: DelegationAttemptState::default(),
    }
}

/// `parents` visible sessions, each with `children_per_parent` hidden
/// delegation children. Returns the state and the visible parent ids.
fn hidden_population(parents: usize, children_per_parent: usize) -> (AppState, Vec<String>) {
    let state = test_app_state();
    let workdir = state.default_workdir.clone();
    let mut parent_ids = Vec::new();
    {
        let mut inner = state.inner.lock().unwrap();
        for _ in 0..parents {
            let parent = inner
                .create_session(
                    Agent::Codex,
                    Some("Parent".into()),
                    workdir.clone(),
                    None,
                    None,
                )
                .session
                .id
                .clone();
            for _ in 0..children_per_parent {
                let child = inner
                    .create_session(
                        Agent::Codex,
                        Some("Child".into()),
                        workdir.clone(),
                        None,
                        None,
                    )
                    .session
                    .id
                    .clone();
                let index = inner.find_session_index(&child).unwrap();
                inner.sessions[index].hidden = true;
                let id = inner.next_delegation_id();
                inner
                    .delegations
                    .push(delegation_for(&parent, &child, id, &workdir));
            }
            parent_ids.push(parent);
        }
        assert!(inner.delegation_inventory_complete());
    }
    (state, parent_ids)
}

/// The pass as it was before the duty/ancestor index: every hidden session
/// takes the full per-session retention. Kept here as the reference oracle.
fn reference_hidden_retention(inner: &mut StateInner) {
    if !inner.delegation_inventory_complete() {
        return;
    }
    let hidden: Vec<String> = inner
        .sessions
        .iter()
        .filter(|record| record.hidden)
        .map(|record| record.session.id.clone())
        .collect();
    for session_id in hidden {
        inner.defer_source_observation_removal(&session_id);
    }
}

/// Deterministic tree population: per visible parent, a hidden child and a
/// hidden grandchild under it. Duties sit on a grandchild, a hidden child, a
/// visible parent and an unrelated visible session, in a fixed pattern, so the
/// same ids carry the same duties in every state built by this function.
fn duty_population() -> AppState {
    let state = test_app_state();
    let workdir = state.default_workdir.clone();
    let mut inner = state.inner.lock().unwrap();
    for parent_index in 0..12 {
        let parent = inner
            .create_session(
                Agent::Codex,
                Some("Parent".into()),
                workdir.clone(),
                None,
                None,
            )
            .session
            .id
            .clone();
        let mut above = parent.clone();
        for depth in 0..2 {
            let child = inner
                .create_session(
                    Agent::Codex,
                    Some("Child".into()),
                    workdir.clone(),
                    None,
                    None,
                )
                .session
                .id
                .clone();
            let index = inner.find_session_index(&child).unwrap();
            inner.sessions[index].hidden = true;
            // Grandchild duty on every third tree, child duty on every fifth.
            if (depth == 1 && parent_index % 3 == 0) || (depth == 0 && parent_index % 5 == 0) {
                inner.sessions[index].engram.source_root_bind_owner = Some(Uuid::new_v4());
            }
            let id = inner.next_delegation_id();
            inner
                .delegations
                .push(delegation_for(&above, &child, id, &workdir));
            above = child;
        }
        if parent_index % 7 == 0 {
            let index = inner.find_session_index(&parent).unwrap();
            inner.sessions[index].engram.source_root_bind_owner = Some(Uuid::new_v4());
        }
    }
    let lone = inner
        .create_session(Agent::Codex, Some("Unrelated".into()), workdir, None, None)
        .session
        .id
        .clone();
    let index = inner.find_session_index(&lone).unwrap();
    inner.sessions[index].engram.source_root_bind_owner = Some(Uuid::new_v4());
    drop(inner);
    state
}

fn retention_image(inner: &StateInner) -> Vec<(String, bool, bool, bool, String)> {
    inner
        .sessions
        .iter()
        .map(|record| {
            (
                record.session.id.clone(),
                record.hidden,
                record.engram.observation_delete_requested,
                record.orchestrator_auto_dispatch_blocked,
                record.session.preview.clone(),
            )
        })
        .collect()
}

#[test]
fn hidden_retention_pass_matches_the_per_session_reference_exactly() {
    let indexed = duty_population();
    let reference = duty_population();
    let mut indexed_inner = indexed.inner.lock().unwrap();
    let mut reference_inner = reference.inner.lock().unwrap();
    assert_eq!(
        retention_image(&indexed_inner),
        retention_image(&reference_inner)
    );
    indexed_inner.retain_hidden_source_observation_sessions();
    reference_hidden_retention(&mut reference_inner);
    let after = retention_image(&indexed_inner);
    assert_eq!(after, retention_image(&reference_inner));
    // The fixture must exercise both outcomes: some hidden sessions retained
    // (made visible and marked), and some left hidden and untouched.
    assert!(
        after
            .iter()
            .any(|(_, hidden, requested, _, _)| !hidden && *requested)
    );
    assert!(
        after
            .iter()
            .any(|(_, hidden, requested, _, _)| *hidden && !requested)
    );
}

#[test]
fn hidden_retention_keeps_a_hidden_ancestor_of_a_deep_duty_and_leaves_clean_trees_hidden() {
    let state = duty_population();
    let mut inner = state.inner.lock().unwrap();
    inner.retain_hidden_source_observation_sessions();
    let inner = &*inner;
    let record = |id: &str| &inner.sessions[inner.find_session_index(id).unwrap()];
    let subtree_has_duty = |root: &str| {
        source_observation_removal_set(inner, root)
            .iter()
            .any(|id| {
                let member = record(id);
                runtime_observation_has_removal_duty(member)
                    || member.engram.source_root_bind_owner.is_some()
            })
    };
    // Every hidden session in this fixture is a delegation child; the
    // visible parents never are.
    let was_hidden = |id: &str| inner.delegations.iter().any(|d| d.child_session_id == id);
    for delegation in &inner.delegations {
        let child = record(&delegation.child_session_id);
        // A hidden session whose subtree holds a duty is retained together
        // with its whole subtree, so a clean child of such a hidden parent is
        // retained too; a child with neither stays hidden and untouched.
        let expected = subtree_has_duty(&delegation.child_session_id)
            || (was_hidden(&delegation.parent_session_id)
                && subtree_has_duty(&delegation.parent_session_id));
        assert_eq!(!child.hidden, expected, "child {}", child.session.id);
        assert_eq!(child.engram.observation_delete_requested, expected);
    }
}

#[test]
fn hidden_retention_pass_scaling_is_printed_for_diagnosis() {
    // The last size matches the live shape: about 8300 retained sessions,
    // nearly all hidden delegation children, and no removal duty (idle).
    for (parents, children) in [(50, 20), (100, 20), (200, 20), (400, 21)] {
        let (state, _) = hidden_population(parents, children);
        let mut inner = state.inner.lock().unwrap();
        let sessions = inner.sessions.len();
        let hidden = inner.sessions.iter().filter(|record| record.hidden).count();
        let started = std::time::Instant::now();
        inner.retain_hidden_source_observation_sessions();
        let indexed = started.elapsed();
        let started = std::time::Instant::now();
        reference_hidden_retention(&mut inner);
        let reference = started.elapsed();
        eprintln!(
            "hidden_retention_profile sessions={sessions} hidden={hidden} delegations={} indexed_us={} reference_us={}",
            inner.delegations.len(),
            indexed.as_micros(),
            reference.as_micros()
        );
        // No session had a removal duty, so nothing may have been retained.
        assert_eq!(
            inner.sessions.iter().filter(|record| record.hidden).count(),
            hidden
        );
    }
}
