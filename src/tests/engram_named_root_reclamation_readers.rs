// Distinguishes a retained local run view from current exact-store authority.
// Uses the real planner and canonical recovery owner, never a manufactured route.
use super::*;

fn stale_owner_with_reader(
    label: &str,
    proof: &str,
) -> (ClaimedRoot, EngramWorkSourceRoot, String, String) {
    let (claimed, entry) = settled_root(label, proof);
    let (project, reader_state, binding) = {
        let inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        let binding = engram_root_journal(&inner.engram_named_root_journal,
            &entry.store, &entry.claim_id).unwrap().read_binding.clone().unwrap();
        (inner.sessions[index].session.project_id.clone().unwrap(),
            inner.sessions[index].engram.clone(), binding)
    };
    let reader = create_test_project_session(&claimed.state, Agent::Codex, &project, &claimed.root);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&reader).unwrap();
        inner.sessions[index].engram = reader_state;
        inner.sessions[index].session.status = SessionStatus::Idle;
    }
    let reader_binding = {
        let inner = claimed.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&reader).unwrap()]
            .engram.work_binding.clone().unwrap()
    };
    claimed.transport.enable_named_roots(&reader, &reader_binding);
    let token = claimed.record(|record| {
        record.engram.work_binding = Some(binding);
        record.session.status = SessionStatus::Idle;
        record.engram.routing_token.take().unwrap()
    });
    {
        let inner = claimed.state.inner.lock().unwrap();
        let stale = AppState::engram_binding_target_for_session_shape_locked(
            &inner, &claimed.session_id, true).unwrap().unwrap();
        let authorized = AppState::engram_binding_target_for_session_shape_locked(
            &inner, &reader, true).unwrap().unwrap();
        assert!(stale.routing_token.is_none());
        assert!(authorized.routing_token.is_some());
        assert_eq!(authorized.settings.authority_store_key.as_ref(), Some(&entry.store));
    }
    (claimed, entry, reader, token)
}

fn assert_peer_retirement(claimed: &ClaimedRoot, entry: &EngramWorkSourceRoot, reader: &str) {
    let plan = reclamation_plan(claimed);
    assert_eq!(plan.len(), 1, "a stale local binding cannot block an authorized peer");
    assert_eq!(plan[0].root, *entry);
    assert_eq!(plan[0].route, EngramRootReclamationRoute::Unfocused);
    let before = claimed.transport.requests().len();
    let root = PathBuf::from(&entry.root);
    let git_before = fs::read(root.join(".git")).unwrap();
    let revision_before = content_revision(&root).unwrap().1;
    claimed.state.run_engram_root_reclamation_pass(plan);
    let requests = claimed.transport.requests();
    let reads = requests[before..].iter()
        .filter(|request| request.request["operation"] == "named_root_read")
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].connection.session_id, reader);
    assert_eq!(reads[0].request["claim_id"], entry.claim_id);
    let notice_id = {
        let inner = claimed.state.inner.lock().unwrap();
        assert!(!inner.engram_work_source_roots.contains(entry));
        assert_eq!(engram_named_root_capacity(&inner), 0);
        let notices = &inner.engram_work_naming_history[0].retirements;
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].selection, *entry);
        assert!(notices[0].published);
        assert_eq!(notices[0].read.run.state, "completed");
        assert!(notices[0].line.contains("its run completed"));
        notices[0].id.clone()
    };
    claimed.state.run_engram_root_reclamation_pass(reclamation_plan(claimed));
    let inner = claimed.state.inner.lock().unwrap();
    assert_eq!(inner.engram_work_naming_history[0].retirements.len(), 1);
    assert_eq!(inner.engram_work_naming_history[0].retirements[0].id, notice_id);
    drop(inner);
    assert_eq!(fs::read(root.join(".git")).unwrap(), git_before);
    assert_eq!(content_revision(&root).unwrap().1, revision_before);
    assert!(matches!(claimed.runtime_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
}

#[test]
fn named_root_reclamation_tokenless_stale_owner_retires_through_peer() {
    let (claimed, entry, reader, _) = stale_owner_with_reader("reclaim-tokenless-owner", "completed");
    assert_peer_retirement(&claimed, &entry, &reader);
}

#[test]
fn named_root_reclamation_wrong_store_stale_owner_retires_through_peer() {
    let (claimed, entry, reader, token) = stale_owner_with_reader("reclaim-wrong-store-owner", "completed");
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        let project = inner.sessions[index].session.project_id.clone().unwrap();
        let mut wrong_project = inner.find_project(&project).unwrap().clone();
        wrong_project.id = format!("other-store-{}", Uuid::new_v4());
        wrong_project.engram.as_mut().unwrap().authority_store_key.as_mut().unwrap()
            .project_id.push_str("-other-store");
        inner.sessions[index].session.project_id = Some(wrong_project.id.clone());
        inner.sessions[index].engram.routing_token = Some(token);
        inner.projects.push(wrong_project);
        let wrong = AppState::engram_binding_target_for_session_shape_locked(
            &inner, &claimed.session_id, true).unwrap().unwrap();
        assert!(wrong.routing_token.is_some());
        assert_ne!(wrong.settings.authority_store_key.as_ref(), Some(&entry.store));
    }
    assert_peer_retirement(&claimed, &entry, &reader);
}

#[test]
fn named_root_reclamation_tokenless_stale_owner_keeps_open_matching_bound() {
    // This scripted canonical carrier is active Bound. Evaluation is not a
    // field in NamedRootRead; the pinned live witness separately issues a real
    // accepted evaluation and checks the same still-active Bound carrier.
    let (claimed, entry, reader, _) = stale_owner_with_reader("reclaim-tokenless-open", "live");
    let plan = reclamation_plan(&claimed);
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].route, EngramRootReclamationRoute::Unfocused);
    let before = claimed.transport.requests().len();
    claimed.state.run_engram_root_reclamation_pass(plan);
    assert_root_retained(&claimed, &entry);
    assert!(claimed.state.inner.lock().unwrap().engram_work_naming_history[0].retirements.is_empty());
    assert!(claimed.transport.requests()[before..].iter().any(|request|
        request.request["operation"] == "named_root_read" && request.connection.session_id == reader));
}

#[test]
fn named_root_reclamation_stale_owner_authority_restored_during_read_is_fenced() {
    let (claimed, entry, reader, token) = stale_owner_with_reader("reclaim-owner-restored-race", "completed");
    let plan = reclamation_plan(&claimed);
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].route, EngramRootReclamationRoute::Unfocused);
    let state = claimed.state.clone();
    let owner = claimed.session_id.clone();
    TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&owner).unwrap();
            inner.sessions[index].engram.routing_token = Some(token);
        }));
    });
    let before = claimed.transport.requests().len();
    claimed.state.run_engram_root_reclamation_pass(plan);
    assert!(claimed.transport.requests()[before..].iter().any(|request|
        request.request["operation"] == "named_root_read" && request.connection.session_id == reader));
    assert_root_retained(&claimed, &entry);
    {
        let inner = claimed.state.inner.lock().unwrap();
        assert!(inner.engram_work_naming_history[0].retirements.is_empty());
        assert!(inner.engram_work_naming_history[0].recovery_reason.as_ref().unwrap()
            .contains("a current owner appeared"));
    }
    let after_cooldown = claimed.state.engram_budget_clock().now() + ENGRAM_ROOT_RECLAMATION_COOLDOWN;
    let next = engram_root_reclamation_plan_locked(&claimed.state.inner.lock().unwrap(), after_cooldown);
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].route, EngramRootReclamationRoute::Current(claimed.session_id.clone()));
}

#[test]
fn named_root_reclamation_rebind_first_keeps_selection_loss_without_second_retirement() {
    // Mirror the live pass-first witness with the existing rebind owner first.
    // Script only canonical producer replies; both owners and the planner are real.
    let (claimed, entry) = settled_root("reclamation-rebind-first", "completed");
    let binding = engram_root_journal(&claimed.state.inner.lock().unwrap().engram_named_root_journal,
        &entry.store, &entry.claim_id).unwrap().read_binding.clone().unwrap();
    let successor = claimed.record(|record| {
        let successor = record.engram.work_binding.replace(binding.clone()).unwrap();
        record.session.status = SessionStatus::Idle;
        record.engram.rebind_required = true;
        successor
    });
    claimed.transport.enable_named_roots(&claimed.session_id, &binding);
    {
        let mut queued = claimed.transport.work_bindings.lock().unwrap();
        queued.clear();
        queued.push_back(Ok(Some(successor.clone())));
    }
    claimed.transport.responses.lock().unwrap()
        .push_back(bind_reply("reclamation-rebind-first-successor"));
    let planned_before_rebind = reclamation_plan(&claimed);
    assert_eq!(planned_before_rebind.len(), 1);
    assert_eq!(planned_before_rebind[0].root, entry);
    assert_eq!(planned_before_rebind[0].route,
        EngramRootReclamationRoute::Current(claimed.session_id.clone()));
    let root = PathBuf::from(&entry.root);
    let git_before = fs::read(root.join(".git")).unwrap();
    let revision_before = content_revision(&root).unwrap().1;
    let before = claimed.transport.requests().len();
    claimed.state.ensure_engram_session_bound_off_lock(&claimed.session_id).unwrap().unwrap();
    let requests = claimed.transport.requests();
    assert_eq!(requests[before].request["operation"], "session_status");
    assert!(requests[before..].iter().any(|request|
        request.request["operation"] == "named_root_read"
            && request.request["claim_id"] == entry.claim_id));
    assert!(requests[before..].iter().any(|request|
        request.request["operation"] == "session_bind"
            && request.request["work_binding"]["claim_id"] == successor.claim_id));
    let notices = claimed.record(|record| {
        assert_eq!(record.engram.work_binding.as_ref(), Some(&successor));
        assert_eq!(record.engram.source_root_notices.len(), 1);
        assert!(matches!(&record.engram.source_root_notices[0].kind,
            EngramSourceRootNoticeKind::SelectionLoss { selection } if selection == &entry));
        record.engram.source_root_notices.clone()
    });
    {
        let inner = claimed.state.inner.lock().unwrap();
        assert!(!inner.engram_work_source_roots.contains(&entry));
        assert!(!engram_authority_work_unresolved(&inner, &entry.store, &entry.work_id));
        assert!(inner.engram_work_naming_history.iter().all(|history| history.retirements.is_empty()));
    }
    let after_rebind = claimed.transport.requests().len();
    let later_plan = reclamation_plan(&claimed);
    assert!(later_plan.is_empty());
    claimed.state.run_engram_root_reclamation_pass(later_plan);
    // An already selected task cannot borrow authority after the entry is gone.
    claimed.state.run_engram_root_reclamation_pass(planned_before_rebind);
    assert_eq!(claimed.transport.requests().len(), after_rebind,
        "neither the later planner nor a stale task may perform a second retirement read");
    claimed.record(|record| assert_eq!(record.engram.source_root_notices, notices));
    let inner = claimed.state.inner.lock().unwrap();
    assert!(!inner.engram_work_source_roots.contains(&entry));
    assert!(inner.engram_work_naming_history.iter().all(|history| history.retirements.is_empty()));
    drop(inner);
    assert_eq!(fs::read(root.join(".git")).unwrap(), git_before);
    assert_eq!(content_revision(&root).unwrap().1, revision_before);
    assert!(matches!(claimed.runtime_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
}
