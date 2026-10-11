// Owns the witnesses that named-root reclamation answers its eligibility and
// current-owner questions from a narrow authority view, without building a
// full binding target or the root read fence each target captures (which
// clones every root, journal and project store) under the global lock. Does
// not own reclamation's selection, retirement or recovery behaviour (the
// sibling reclamation modules) or the target builders themselves
// (engram_host_adapter.rs). New module beside the reclamation tests.
use super::*;

#[test]
fn named_root_reclamation_eligibility_scans_build_no_binding_target_or_root_read_fence() {
    let (claimed, _entry) = settled_root("reclamation-eligibility-builds", "completed");
    reset_engram_authority_build_counts();
    let plan = reclamation_plan(&claimed);
    assert_eq!(plan.len(), 1, "the settled entry is a reclamation candidate");
    {
        let inner = claimed.state.inner.lock().unwrap();
        let root = &plan[0].root;
        assert!(engram_root_reclamation_reader_exists(&inner, &root.store));
        assert_eq!(engram_named_root_current_owner(&inner, root), None);
        assert!(engram_root_reclamation_route_current(
            &inner,
            root,
            &EngramRootReclamationRoute::OwnClear(claimed.session_id.clone()),
        ));
        assert!(engram_root_reclamation_route_current(
            &inner,
            root,
            &EngramRootReclamationRoute::Unfocused,
        ));
    }
    assert_eq!(
        engram_authority_build_counts(),
        (0, 0),
        "(root read fences, binding targets) built by the reclamation eligibility scans"
    );
}

type ReaderFacts = Option<(Option<String>, Option<EngramAuthorityStoreKey>)>;

/// (what the full binding target says, what the narrow reader view says).
fn reader_facts(claimed: &ClaimedRoot, session_id: &str) -> (ReaderFacts, ReaderFacts) {
    let inner = claimed.state.inner.lock().unwrap();
    let target = AppState::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
        .ok()
        .flatten()
        .map(|target| (target.routing_token.clone(), target.settings.authority_store_key.clone()));
    let view = engram_session_reader_view_locked(&inner, session_id)
        .map(|view| (view.routing_token.map(str::to_owned), view.store.cloned()));
    (target, view)
}

/// Asserts the view agrees with the full target for `session_id` and returns
/// what they agree on, so each case can also pin which outcome it produces.
#[track_caller]
fn parity(claimed: &ClaimedRoot, session_id: &str, case: &str) -> ReaderFacts {
    let (target, view) = reader_facts(claimed, session_id);
    assert_eq!(view, target, "the reader view disagrees with the full target: {case}");
    view
}

fn with_project(claimed: &ClaimedRoot, project_id: &str, change: impl FnOnce(&mut Project)) {
    let mut inner = claimed.state.inner.lock().unwrap();
    let project = inner.projects.iter_mut().find(|project| project.id == project_id).unwrap();
    change(project);
}

#[test]
fn named_root_reclamation_reader_view_agrees_with_the_full_binding_target() {
    let (claimed, entry) = settled_root("reclamation-view-parity", "completed");
    let root = claimed.session_id.clone();
    let (delegation, child) =
        crate::tests::delegation_support::install_required_review_delegation(&claimed.state, &root);
    let project_id = claimed.record(|record| record.session.project_id.clone().unwrap());

    let reader = parity(&claimed, &root, "root session").expect("the root session is a reader");
    assert!(reader.0.is_some(), "the root session holds a routing token");
    assert_eq!(reader.1.as_ref(), Some(&entry.store));
    let child_facts = parity(&claimed, &child, "delegation child").expect("the child resolves its parent's project");
    assert_eq!(child_facts.0, None, "a child reads its own routing token, not its parent's");
    assert_eq!(child_facts.1.as_ref(), Some(&entry.store));

    let token = claimed.record(|record| record.engram.routing_token.take());
    assert_eq!(parity(&claimed, &root, "tokenless root").map(|facts| facts.0), Some(None));
    claimed.record(|record| record.engram.routing_token = token);

    claimed.record(|record| record.engram.disabled_reason = Some("disabled for parity".to_owned()));
    assert_eq!(parity(&claimed, &root, "disabled root"), None);
    claimed.record(|record| record.engram.disabled_reason = None);

    with_project(&claimed, &project_id, |project| project.engram.as_mut().unwrap().turn_gated_control = false);
    assert_eq!(parity(&claimed, &root, "control off (root)"), None);
    assert_eq!(parity(&claimed, &child, "control off (child)"), None);
    with_project(&claimed, &project_id, |project| project.engram.as_mut().unwrap().turn_gated_control = true);

    let remote = claimed.state.inner.lock().unwrap().find_project(&project_id).unwrap().remote_id.clone();
    with_project(&claimed, &project_id, |project| project.remote_id = "remote-for-parity".to_owned());
    assert_eq!(parity(&claimed, &root, "remote project (root)"), None);
    assert_eq!(parity(&claimed, &child, "remote project (child)"), None);
    with_project(&claimed, &project_id, |project| project.remote_id = remote);

    let binary = claimed.state.inner.lock().unwrap().find_project(&project_id).unwrap()
        .engram.as_ref().unwrap().binary_path.clone();
    with_project(&claimed, &project_id, |project| project.engram.as_mut().unwrap().binary_path = None);
    assert!(AppState::engram_binding_target_for_session_shape_locked(
        &claimed.state.inner.lock().unwrap(), &root, true).is_err(),
        "a missing binary path is a target error, which the reader view reads as no reader");
    assert_eq!(parity(&claimed, &root, "missing binary path"), None);
    with_project(&claimed, &project_id, |project| project.engram.as_mut().unwrap().binary_path = binary);

    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&delegation).unwrap();
        inner.delegations[index].parent_session_id = "missing-parent-session".to_owned();
    }
    assert_eq!(parity(&claimed, &child, "child of a missing parent"), None);
    assert!(parity(&claimed, &root, "root beside an orphaned child").is_some());

    assert!(claimed.state.inner.lock().unwrap().engram_project_resets.claim(&project_id).is_some());
    assert_eq!(parity(&claimed, &root, "project reset in progress"), None);
}

#[test]
fn named_root_reclamation_writes_its_lines_after_releasing_the_global_lock() {
    let (claimed, entry) = settled_root("reclamation-emit-unlocked", "completed");
    let _ = take_engram_root_reclamation_emitted();
    let plan = reclamation_plan(&claimed);
    assert_eq!(plan.len(), 1);
    // Every emission asserts the global lock is not held by its thread.
    claimed.state.run_engram_root_reclamation_pass(plan);
    let notice = {
        let inner = claimed.state.inner.lock().unwrap();
        assert!(!inner.engram_work_source_roots.contains(&entry), "the settled entry retired");
        inner.engram_work_naming_history[0].retirements[0].line.clone()
    };
    let emitted = take_engram_root_reclamation_emitted();
    assert!(emitted.contains(&format!("engram> {notice}")),
        "the retirement line was written: {emitted:?}");
    assert!(emitted.iter().any(|line| line.contains("named-root reclamation store=")
        && line.contains("outcome=retired")), "the outcome line was written: {emitted:?}");
    assert!(emitted.iter().any(|line| line.contains("named-root reclamation pass attempted=1")),
        "the pass line was written: {emitted:?}");
}

/// The recovery reader as the full-target scan chose it before the view:
/// a target per session, filtered by store and token, namer first.
fn reader_by_full_targets(
    inner: &StateInner,
    store: &EngramAuthorityStoreKey,
    namer: Option<&str>,
) -> Option<String> {
    let mut readers = inner.sessions.iter()
        .filter_map(|record| AppState::engram_binding_target_for_session_shape_locked(
            inner, &record.session.id, true).ok().flatten())
        .filter(|target| target.settings.authority_store_key.as_ref() == Some(store)
            && target.routing_token.is_some())
        .collect::<Vec<_>>();
    readers.sort_by_key(|target| Some(target.connection.session_id.as_str()) != namer);
    readers.into_iter().next().map(|target| target.connection.session_id)
}

#[test]
fn named_root_reclamation_reporter_and_reader_selection_build_only_the_winners_target() {
    let (claimed, entry) = settled_root("reclamation-reader-selection", "completed");
    let root = claimed.session_id.clone();
    let (_delegation, child) =
        crate::tests::delegation_support::install_required_review_delegation(&claimed.state, &root);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        inner.sessions[index].engram.routing_token = Some("child-routing-token".to_owned());
    }
    let event = {
        let inner = claimed.state.inner.lock().unwrap();
        engram_root_journal(&inner.engram_named_root_journal, &entry.store, &entry.claim_id)
            .unwrap().confirmed.as_ref().unwrap().0.clone()
    };
    let reporter_by_target = AppState::engram_binding_target_for_session_shape_locked(
        &claimed.state.inner.lock().unwrap(), &event.reporter, true)
        .ok().flatten()
        .is_some_and(|target| target.settings.authority_store_key.as_ref() == Some(&event.root.store)
            && target.routing_token.is_some());

    reset_engram_authority_build_counts();
    let reporter = AppState::engram_root_reporter_available(&claimed.state.inner.lock().unwrap(), &event);
    assert_eq!(reporter, reporter_by_target, "reporter availability agrees with the full target");
    assert_eq!(engram_authority_build_counts(), (0, 0), "reporter availability builds nothing");

    for namer in [None, Some(child.as_str()), Some(root.as_str()), Some("not-a-reader")] {
        let expected = reader_by_full_targets(&claimed.state.inner.lock().unwrap(), &entry.store, namer);
        assert!(expected.is_some(), "a reader exists for namer {namer:?}");
        reset_engram_authority_build_counts();
        let target = engram_root_recovery_reader_target_locked(
            &claimed.state.inner.lock().unwrap(), &entry.store, namer);
        assert_eq!(engram_authority_build_counts(), (1, 1),
            "eligibility and selection build nothing; only the winner's one target (namer {namer:?})");
        assert_eq!(target.map(|target| target.connection.session_id), expected,
            "the same reader as the full-target scan (namer {namer:?})");
    }
    assert_eq!(reader_by_full_targets(&claimed.state.inner.lock().unwrap(), &entry.store, Some(&child)),
        Some(child.clone()), "the namer preference is exercised, not just session order");
}

/// The current-owner questions with a real current owner: the session's work
/// binding is the journal's read binding and the session is idle, so the plan
/// takes the Current route and current_owner, idle_owner and route_current
/// (Current) all reach the reader predicate. None of them builds anything.
#[test]
fn named_root_reclamation_current_owner_questions_build_no_binding_target_or_root_read_fence() {
    let (claimed, entry) = settled_root("reclamation-current-owner-builds", "completed");
    let binding = engram_root_journal(&claimed.state.inner.lock().unwrap().engram_named_root_journal,
        &entry.store, &entry.claim_id).unwrap().read_binding.clone().unwrap();
    claimed.record(|record| {
        record.engram.work_binding = Some(binding.clone());
        record.session.status = SessionStatus::Idle;
    });
    reset_engram_authority_build_counts();
    let plan = reclamation_plan(&claimed);
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].route, EngramRootReclamationRoute::Current(claimed.session_id.clone()),
        "the plan took the Current route through the current-owner and idle-owner checks");
    {
        let inner = claimed.state.inner.lock().unwrap();
        assert_eq!(engram_named_root_current_owner(&inner, &entry), Some(claimed.session_id.clone()));
        assert!(engram_root_reclamation_idle_owner(&inner, &claimed.session_id));
        assert!(engram_root_reclamation_route_current(&inner, &entry,
            &EngramRootReclamationRoute::Current(claimed.session_id.clone())));
    }
    assert_eq!(engram_authority_build_counts(), (0, 0),
        "(root read fences, binding targets) built by the current-owner questions");
}
