// Scheduler lock scope and plan parity. The planning probe observes the actual
// scheduler, without sleeps, external processes, or a performance timing guess.
use super::*;

pub(super) struct PlanningProbe;

impl PlanningProbe {
    pub(super) fn install(hook: impl Fn(&str, &[EngramRootReclamationTask]) + 'static) -> Self {
        TEST_ENGRAM_ROOT_RECLAMATION_PLANNING.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(Box::new(hook));
        });
        Self
    }
}

impl Drop for PlanningProbe {
    fn drop(&mut self) {
        TEST_ENGRAM_ROOT_RECLAMATION_PLANNING.with(|slot| *slot.borrow_mut() = None);
    }
}

fn scaled_scheduler_state(label: &str) -> EOCheckedTurn {
    let (claimed, entry) = settled_root(label, "live");
    let mut reads = Vec::new();
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let journal = inner.engram_named_root_journal[0].clone();
        let history = inner.engram_work_naming_history.iter()
            .find(|history| history.store == entry.store && history.work_id == entry.work_id)
            .unwrap().clone();
        inner.engram_work_source_roots.clear();
        for number in 0..ENGRAM_WORK_SOURCE_ROOT_LIMIT {
            let mut root = entry.clone();
            root.work_id = format!("scheduler-work-{number}");
            root.claim_id = format!("scheduler-claim-{number}");
            root.named_at = format!("2026-10-01T00:00:00.{number:03}Z");
            let mut association = journal.clone();
            association.claim_id = root.claim_id.clone();
            let binding = association.read_binding.as_mut().unwrap();
            binding.work_id = root.work_id.clone();
            binding.claim_id = root.claim_id.clone();
            binding.run_id = format!("scheduler-run-{number}");
            binding.root_execution_id = format!("scheduler-execution-{number}");
            let binding = binding.clone();
            let (event, receipt) = association.confirmed.as_mut().unwrap();
            event.root = root.clone();
            receipt.event = format!("scheduler-bound-{number}");
            receipt.position.feed.id = binding.run_id.clone();
            // Scale the settled authority too. Missing history would force
            // real full-image recovery writes against an unadvanced scripted
            // clock, unrelated to this scheduler planning witness.
            let mut read = retained_root_proof(&association, false);
            read["run"]["state"] = json!("active");
            read["named_root"] = json!({"state":"bound", "workspace_id":root.root,
                "generation":root.generation, "named_at":root.named_at});
            let proof: EngramNamedRootReadResponse = serde_json::from_value(read.clone()).unwrap();
            proof.validate_authority(&root.store, &binding).unwrap();
            assert!(proof.covers(&association));
            let mut settled = history.clone();
            settled.work_id = root.work_id.clone();
            settled.frontier = proof.frontier();
            settled.proofs = vec![EngramAuthorityProof { binding: binding.clone(), read: proof }];
            let owner = settled.transition.as_mut().unwrap();
            owner.id = format!("scheduler-owner-{number}");
            owner.binding = binding.clone();
            assert_eq!(owner.phase, EngramAuthorityPhase::Published);
            inner.engram_work_naming_history.push(settled);
            inner.engram_named_root_journal.push(association);
            inner.engram_work_source_roots.push(root);
            reads.push((binding, read));
        }
        for number in 0..256 {
            let mut unrelated = journal.clone();
            unrelated.claim_id = format!("unrelated-scheduler-claim-{number}");
            inner.engram_named_root_journal.insert(0, unrelated);
        }
        for number in 0..512 {
            inner.create_session(
                Agent::Codex,
                Some(format!("Unrelated scheduler session {number}")),
                claimed.root.to_string_lossy().into_owned(),
                None,
                None,
            );
        }
    }
    for (binding, read) in reads {
        claimed.transport.register_named_root_run(&binding);
        claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
            .root_reads.insert(binding.claim_id.clone(), read);
    }
    claimed
}

#[test]
fn named_root_reclamation_scheduler_plans_with_state_lock_available() {
    let claimed = scaled_scheduler_state("scheduler-lock-scope");
    let now = claimed.state.engram_budget_clock().now();
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        inner.engram_root_reclamation.cooldowns = inner.engram_work_source_roots.iter()
            .map(|root| (root.clone(), now + ENGRAM_ROOT_RECLAMATION_COOLDOWN))
            .collect();
    }
    let observations = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let observed = observations.clone();
    let state = claimed.state.clone();
    let _probe = PlanningProbe::install(move |phase, _| {
        // Checking this exact mutex's owner is deterministic even if another
        // fixture thread briefly holds it. Do not panic under the old guard.
        if phase != "selected" {
            observed.borrow_mut().push((
                phase.to_owned(), state.inner.is_not_held_by_current_thread_for_test(),
            ));
        }
    });
    assert_eq!(claimed.state.schedule_engram_root_reclamation(true),
        "no eligible reclamation pass was scheduled");
    assert_eq!(claimed.state.inner.lock().unwrap().engram_root_reclamation.next_due,
        Some(now + ENGRAM_ROOT_RECLAMATION_BUDGET));
    assert_eq!(claimed.state.schedule_engram_root_reclamation(false),
        "the recurring pass is not due yet");
    assert_eq!(*observations.borrow(), vec![
        ("canonical".to_owned(), true), ("disposition".to_owned(), true),
    ], "both state-sized planning scans must run outside the scheduler's state guard");
}

#[test]
fn named_root_reclamation_scheduler_preserves_reference_plan() {
    let claimed = scaled_scheduler_state("scheduler-plan-parity");
    let expected = reclamation_plan(&claimed);
    assert_eq!(expected.len(), ENGRAM_ROOT_RECLAMATION_READ_LIMIT);
    let authority_before = {
        let inner = claimed.state.inner.lock().unwrap();
        for root in &inner.engram_work_source_roots {
            assert!(!engram_authority_work_unresolved(&inner, &root.store, &root.work_id));
        }
        (inner.engram_work_source_roots.clone(), inner.engram_named_root_journal.clone(),
            inner.engram_work_naming_history.clone())
    };
    let requests_before = claimed.transport.requests().len();
    let selected = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let observed = selected.clone();
    let _probe = PlanningProbe::install(move |phase, plan| {
        if phase == "selected" {
            *observed.borrow_mut() = plan.iter()
                .map(|task| (task.root.clone(), task.route.clone())).collect();
        }
    });
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_writes = writes.clone();
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary =
        Some(Arc::new(move |phase| {
            if phase == "after_delta_capture" {
                observed_writes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            if phase == "after_reclamation_pass" { let _ = done_tx.send(()); }
        }));
    let result = claimed.state.schedule_engram_root_reclamation(true);
    assert_eq!(result, "a bounded reclamation pass was scheduled");
    done_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).expect("the scheduled pass finishes");
    let mut inner = claimed.state.inner.lock().unwrap();
    inner.test_engram_authority_ack_boundary = None;
    assert!(inner.engram_root_reclamation.flight.is_none(), "the real worker released its flight");
    assert_eq!((inner.engram_work_source_roots.clone(), inner.engram_named_root_journal.clone(),
        inner.engram_work_naming_history.clone()), authority_before,
        "settled live roots retain their acknowledged authority through the pass");
    drop(inner);
    assert_eq!(writes.load(std::sync::atomic::Ordering::SeqCst), 0,
        "plan parity must not accidentally exercise full-image authority recovery");
    let reads = claimed.transport.requests().into_iter().skip(requests_before)
        .map(|request| request.request)
        .filter(|request| request["operation"] == "named_root_read")
        .map(|request| request["claim_id"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
    assert_eq!(reads, expected.iter().map(|task| task.root.claim_id.clone()).collect::<Vec<_>>(),
        "the actual pass reads the selected roots through canonical authority");
    assert_eq!(*selected.borrow(), expected.iter()
        .map(|task| (task.root.clone(), task.route.clone())).collect::<Vec<_>>());
}

#[test]
fn named_root_reclamation_capture_expiry_keeps_unacknowledged_root() {
    let (claimed, entry) = settled_root("reclaim-capture-expiry", "completed");
    let clock = claimed.state.engram_budget_clock();
    let advanced = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = advanced.clone();
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary =
        Some(Arc::new(move |phase| {
            if phase == "after_delta_capture"
                && !observed.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                clock.advance(ENGRAM_ROOT_RECLAMATION_BUDGET + Duration::from_millis(1));
            }
        }));
    claimed.state.run_engram_root_reclamation_pass(reclamation_plan(&claimed));
    let mut inner = claimed.state.inner.lock().unwrap();
    inner.test_engram_authority_ack_boundary = None;
    assert!(advanced.load(std::sync::atomic::Ordering::SeqCst),
        "the actual authority capture crossed the pass deadline");
    assert!(inner.engram_work_source_roots.contains(&entry));
    let history = inner.engram_work_naming_history.iter()
        .find(|history| history.store == entry.store && history.work_id == entry.work_id).unwrap();
    assert_eq!(history.transition.as_ref().unwrap().phase, EngramAuthorityPhase::Prepared);
    assert!(history.retirements.is_empty(), "no retirement was published from a late capture");
    assert!(engram_authority_work_unresolved(&inner, &entry.store, &entry.work_id));
    let attempt = inner.engram_root_reclamation.attempts.iter()
        .find(|attempt| attempt.root == entry).unwrap();
    assert!(attempt.refusal.as_ref().unwrap().1.contains("acknowledgement exceeded"),
        "expiry must retain the root with its actual refusal: {attempt:?}");
}

#[test]
fn named_root_reclamation_scheduler_reserves_one_flight_during_unlocked_planning() {
    let claimed = scaled_scheduler_state("scheduler-planning-flight");
    let now = claimed.state.engram_budget_clock().now();
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        inner.engram_root_reclamation.cooldowns = inner.engram_work_source_roots.iter()
            .map(|root| (root.clone(), now + ENGRAM_ROOT_RECLAMATION_COOLDOWN)).collect();
    }
    let state = claimed.state.clone();
    let phases = std::rc::Rc::new(std::cell::Cell::new(0));
    let observed = phases.clone();
    let _probe = PlanningProbe::install(move |phase, _| {
        if phase == "canonical" || phase == "disposition" {
            assert!(state.inner.is_not_held_by_current_thread_for_test());
            assert_eq!(state.schedule_engram_root_reclamation(true),
                "a bounded reclamation pass is already running");
            observed.set(observed.get() + 1);
        }
    });
    assert_eq!(claimed.state.schedule_engram_root_reclamation(true),
        "no eligible reclamation pass was scheduled");
    assert_eq!(phases.get(), 2);
    let inner = claimed.state.inner.lock().unwrap();
    assert!(inner.engram_root_reclamation.flight.is_none(), "empty planning releases its flight");
    assert_eq!(inner.engram_root_reclamation.next_due, Some(now + ENGRAM_ROOT_RECLAMATION_BUDGET));
}
