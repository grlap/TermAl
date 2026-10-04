// Named-root capacity witnesses through existing canonical recovery and naming.
// Fixtures retain the original run association after the naming session moves on.
use super::*;

mod recovery {
    include!("engram_named_root_reclamation_recovery.rs");
}

mod safety {
    include!("engram_named_root_reclamation_safety.rs");
}

mod reader_authority {
    include!("engram_named_root_reclamation_readers.rs");
}

fn settled_root(label: &str, proof_kind: &str) -> (ClaimedRoot, EngramWorkSourceRoot) {
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let tree = add_claimed_root_worktree(&claimed.root);
    name_root(&claimed, label, Some(&tree), vec![claimed_root_held(label)]).unwrap();
    let (entry, journal) = {
        let inner = claimed.state.inner.lock().unwrap();
        let entry = inner.engram_work_source_roots[0].clone();
        assert!(!engram_authority_work_unresolved(&inner, &entry.store, &entry.work_id));
        let journal = engram_root_journal(
            &inner.engram_named_root_journal, &entry.store, &entry.claim_id,
        ).unwrap().clone();
        assert!(journal.pending.is_none());
        assert!(journal.read_binding.is_some());
        (entry, journal)
    };
    let mut proof = covering_removed_root_read(&journal, proof_kind == "released");
    match proof_kind {
        "completed" | "released" => {}
        "cancelled" => proof["run"]["state"] = json!("cancelled"),
        "ended" => {
            proof["run"]["state"] = json!("active");
            proof["latest_event"]["kind"] = json!("ended");
            proof["latest_event"]["event"] = json!("confirmed-ended-event");
            proof["latest_event"]["position"]["position"] = proof["read_cut"]["position"].clone();
        }
        "live" => {
            proof["run"]["state"] = json!("active");
            proof["named_root"] = json!({"state":"bound", "workspace_id":entry.root,
                "generation":entry.generation, "named_at":entry.named_at});
        }
        "unknown" => proof["named_root"] = json!({"state":"unknown"}),
        _ => unreachable!(),
    }
    if proof_kind != "unknown" {
        serde_json::from_value::<EngramNamedRootReadResponse>(proof.clone()).unwrap()
            .validate_authority(&entry.store, journal.read_binding.as_ref().unwrap()).unwrap();
    }
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .root_reads.insert(entry.claim_id.clone(), proof);
    let mut successor = journal.read_binding.unwrap();
    successor.work_id.push_str("-next");
    successor.claim_id.push_str("-next");
    successor.run_id.push_str("-next");
    successor.root_execution_id.push_str("-next");
    claimed.record(|record| {
        record.engram.work_binding = Some(successor.clone());
        record.engram.named_root = Some(EngramNamedRootState::None);
    });
    claimed.transport.enable_named_roots(&claimed.session_id, &successor);
    claimed.transport.replace_held_claims([Ok(no_held_claims())]);
    (claimed, entry)
}

fn recovery_frees_capacity(proof_kind: &str) {
    let label = format!("root-reclamation-{proof_kind}");
    let (claimed, entry) = settled_root(&label, proof_kind);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        for number in 1..ENGRAM_WORK_SOURCE_ROOT_LIMIT {
            let mut retained = entry.clone();
            retained.work_id = format!("retained-work-{number}");
            retained.claim_id = format!("retained-claim-{number}");
            retained.short_ref = format!("w-retained-{number}");
            inner.engram_work_source_roots.push(retained);
        }
        assert_eq!(inner.engram_work_source_roots.len(), 64);
    }
    claimed.state.recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    let mut inner = claimed.state.inner.lock().unwrap();
    assert!(engram_work_source_root_for_claim(&inner.engram_work_source_roots,
        &entry.store, &entry.work_id, &entry.claim_id).is_none(),
        "a canonically ended old claim must not retain a capacity slot after focus moved");
    assert_eq!(inner.engram_work_source_roots.len(), 63,
        "unknown associations must survive; only the proven old entry retires");
    let mut replacement = entry.clone();
    replacement.work_id = "new-work-after-reclamation".to_owned();
    replacement.claim_id = "new-claim-after-reclamation".to_owned();
    replacement.generation += 1;
    engram_set_work_source_root(&mut inner.engram_work_source_roots,
        &entry.store, &replacement.work_id.clone(), Some(replacement)).unwrap();
    assert_eq!(inner.engram_work_source_roots.len(), 64);
}

#[test]
fn named_root_reclamation_completed_old_claim_frees_capacity() { recovery_frees_capacity("completed"); }
#[test]
fn named_root_reclamation_cancelled_old_claim_frees_capacity() { recovery_frees_capacity("cancelled"); }
#[test]
fn named_root_reclamation_released_old_claim_frees_capacity() { recovery_frees_capacity("released"); }
#[test]
fn named_root_reclamation_ended_old_binding_frees_capacity() { recovery_frees_capacity("ended"); }

fn own_clear_without_live_claim(proof_kind: &str) {
    let label = format!("root-reclamation-clear-{proof_kind}");
    let (claimed, entry) = settled_root(&label, proof_kind);
    let binds_before = claimed.transport.requests().iter()
        .filter(|request| request.request["operation"] == "named_root_bind").count();
    let result = claimed.state.name_engram_source_root(&claimed.session_id,
        EngramSourceRootRequest { work: entry.short_ref.clone(), path: None });
    assert!(result.is_ok(), "the original namer's ended entry clears without a held claim: {result:?}");
    assert!(engram_work_source_root_for_claim(
        &claimed.state.inner.lock().unwrap().engram_work_source_roots,
        &entry.store, &entry.work_id, &entry.claim_id).is_none());
    assert_eq!(claimed.transport.requests().iter()
        .filter(|request| request.request["operation"] == "named_root_bind").count(), binds_before,
        "host retirement cannot fabricate an Ended producer event for the old claim");
}

#[test]
fn named_root_reclamation_own_completed_clear_without_live_claim() { own_clear_without_live_claim("completed"); }
#[test]
fn named_root_reclamation_own_released_clear_without_live_claim() { own_clear_without_live_claim("released"); }

fn recovery_retains_uncertainty(proof_kind: &str) {
    let label = format!("root-reclamation-retain-{proof_kind}");
    let (claimed, entry) = settled_root(&label, proof_kind);
    claimed.state.recover_engram_authority_runs(&claimed.session_id, Duration::from_secs(2));
    assert_eq!(engram_work_source_root_for_claim(
        &claimed.state.inner.lock().unwrap().engram_work_source_roots,
        &entry.store, &entry.work_id, &entry.claim_id), Some(&entry));
    let result = claimed.state.name_engram_source_root(&claimed.session_id,
        EngramSourceRootRequest { work: entry.short_ref.clone(), path: None });
    assert!(result.is_err(), "an open or unknown old binding cannot clear");
    assert_eq!(engram_work_source_root_for_claim(
        &claimed.state.inner.lock().unwrap().engram_work_source_roots,
        &entry.store, &entry.work_id, &entry.claim_id), Some(&entry));
}

#[test]
fn named_root_reclamation_live_old_binding_is_retained() { recovery_retains_uncertainty("live"); }
#[test]
fn named_root_reclamation_unknown_old_binding_is_retained() { recovery_retains_uncertainty("unknown"); }

fn reclamation_plan(claimed: &ClaimedRoot) -> Vec<EngramRootReclamationTask> {
    let now = claimed.state.engram_budget_clock().now();
    engram_root_reclamation_plan_locked(&claimed.state.inner.lock().unwrap(), now)
}

fn assert_root_retained(claimed: &ClaimedRoot, entry: &EngramWorkSourceRoot) {
    assert!(claimed.state.inner.lock().unwrap().engram_work_source_roots.contains(entry));
}

#[test]
fn named_root_reclamation_bounded_pass_retires_unfocused_without_provider_dispatch() {
    let (claimed, entry) = settled_root("reclamation-pass-unfocused", "completed");
    let plan = reclamation_plan(&claimed);
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].route, EngramRootReclamationRoute::Unfocused);
    claimed.state.run_engram_root_reclamation_pass(plan);
    let inner = claimed.state.inner.lock().unwrap();
    assert!(!inner.engram_work_source_roots.contains(&entry));
    assert_eq!(engram_named_root_capacity(&inner), 0);
    let notices = &inner.engram_work_naming_history[0].retirements;
    assert_eq!(notices.len(), 1);
    assert!(notices[0].published);
    assert!(!notices[0].delivered);
    assert!(matches!(claimed.runtime_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
}

#[test]
fn named_root_reclamation_idle_current_binding_uses_existing_recovery_owner() {
    let (claimed, entry) = settled_root("reclamation-idle-current", "completed");
    let binding = engram_root_journal(&claimed.state.inner.lock().unwrap().engram_named_root_journal,
        &entry.store, &entry.claim_id).unwrap().read_binding.clone().unwrap();
    claimed.record(|record| {
        record.engram.work_binding = Some(binding.clone());
        record.session.status = SessionStatus::Idle;
    });
    claimed.transport.enable_named_roots(&claimed.session_id, &binding);
    let plan = reclamation_plan(&claimed);
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].route, EngramRootReclamationRoute::Current(claimed.session_id.clone()));
    claimed.state.run_engram_root_reclamation_pass(plan);
    assert!(!claimed.state.inner.lock().unwrap().engram_work_source_roots.contains(&entry));
    assert!(matches!(claimed.runtime_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
}

#[test]
fn named_root_reclamation_active_current_and_pending_intent_are_not_candidates() {
    let (claimed, entry) = settled_root("reclamation-excluded-owners", "completed");
    let binding = engram_root_journal(&claimed.state.inner.lock().unwrap().engram_named_root_journal,
        &entry.store, &entry.claim_id).unwrap().read_binding.clone().unwrap();
    claimed.record(|record| {
        record.engram.work_binding = Some(binding);
        record.session.status = SessionStatus::Active;
    });
    assert!(reclamation_plan(&claimed).is_empty());
    claimed.record(|record| record.session.status = SessionStatus::Idle);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let journal = &mut inner.engram_named_root_journal[0];
        journal.pending = Some(journal.confirmed.as_ref().unwrap().0.clone());
    }
    assert!(reclamation_plan(&claimed).is_empty());
    assert_root_retained(&claimed, &entry);
}

#[test]
fn named_root_reclamation_owner_becoming_current_during_read_retains_entry() {
    let (claimed, entry) = settled_root("reclamation-owner-race", "completed");
    let binding = engram_root_journal(&claimed.state.inner.lock().unwrap().engram_named_root_journal,
        &entry.store, &entry.claim_id).unwrap().read_binding.clone().unwrap();
    let plan = reclamation_plan(&claimed);
    let state = claimed.state.clone();
    let session_id = claimed.session_id.clone();
    TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session_id).unwrap();
            inner.sessions[index].engram.work_binding = Some(binding);
        }));
    });
    claimed.state.run_engram_root_reclamation_pass(plan);
    assert_root_retained(&claimed, &entry);
    assert!(claimed.state.inner.lock().unwrap().engram_work_naming_history[0].retirements.is_empty());
}

#[test]
fn named_root_reclamation_exact_selection_and_pending_journal_races_are_fenced() {
    for pending in [false, true] {
        let (claimed, entry) = settled_root("reclamation-selection-journal-race", "completed");
        let plan = reclamation_plan(&claimed);
        let state = claimed.state.clone();
        TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                let mut inner = state.inner.lock().unwrap();
                if pending {
                    let journal = &mut inner.engram_named_root_journal[0];
                    journal.pending = Some(journal.confirmed.as_ref().unwrap().0.clone());
                } else {
                    inner.engram_work_source_roots[0].generation += 1;
                }
            }));
        });
        claimed.state.run_engram_root_reclamation_pass(plan);
        let inner = claimed.state.inner.lock().unwrap();
        assert_eq!(inner.engram_work_source_roots.len(), 1);
        assert!(inner.engram_work_naming_history[0].retirements.is_empty());
        assert_eq!(inner.engram_work_source_roots[0].generation, entry.generation + u64::from(!pending));
        assert_eq!(inner.engram_named_root_journal[0].pending.is_some(), pending);
    }
}

#[test]
fn named_root_reclamation_planner_caps_reads_and_cooldown_advances_oldest_entries() {
    let (claimed, entry) = settled_root("reclamation-plan-cap", "unknown");
    let now = claimed.state.engram_budget_clock().now();
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let journal = inner.engram_named_root_journal[0].clone();
        for number in 1..17 {
            let mut root = entry.clone();
            root.work_id = format!("plan-work-{number}");
            root.claim_id = format!("plan-claim-{number}");
            root.named_at = format!("2026-10-02T00:00:{number:02}Z");
            let mut association = journal.clone();
            association.claim_id = root.claim_id.clone();
            let binding = association.read_binding.as_mut().unwrap();
            binding.work_id = root.work_id.clone();
            binding.claim_id = root.claim_id.clone();
            inner.engram_named_root_journal.push(association);
            inner.engram_work_source_roots.push(root);
        }
    }
    let first = reclamation_plan(&claimed);
    assert_eq!(first.len(), 8);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        for task in &first {
            inner.engram_root_reclamation.cooldowns.push((task.root.clone(), now + ENGRAM_ROOT_RECLAMATION_COOLDOWN));
        }
    }
    let next = reclamation_plan(&claimed);
    assert_eq!(next.len(), 8);
    assert!(next.iter().all(|next| first.iter().all(|old| old.root != next.root)));
    assert!(next.windows(2).all(|pair| pair[0].root.named_at <= pair[1].root.named_at));
}

#[test]
fn named_root_reclamation_deadline_stops_pass_without_retiring_unacknowledged_image() {
    let (claimed, entry) = settled_root("reclamation-pass-deadline", "completed");
    let plan = reclamation_plan(&claimed);
    let clock = claimed.state.engram_budget_clock();
    TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || clock.advance(ENGRAM_ROOT_RECLAMATION_BUDGET)));
    });
    claimed.state.run_engram_root_reclamation_pass(plan);
    let inner = claimed.state.inner.lock().unwrap();
    assert_eq!(engram_named_root_capacity(&inner), 1);
    assert!(inner.engram_work_naming_history[0].retirements.iter().all(|notice| !notice.published));
    assert!(inner.engram_work_source_roots.contains(&entry)
        || inner.engram_work_naming_history[0].retirements.iter().any(|notice| notice.selection == entry));
}
