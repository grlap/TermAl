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

fn scaled_scheduler_state(label: &str) -> ClaimedRoot {
    let (claimed, entry) = settled_root(label, "unknown");
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let journal = inner.engram_named_root_journal[0].clone();
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
            inner.engram_named_root_journal.push(association);
            inner.engram_work_source_roots.push(root);
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
    let selected = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let observed = selected.clone();
    let _probe = PlanningProbe::install(move |phase, plan| {
        if phase == "selected" {
            *observed.borrow_mut() = plan.iter()
                .map(|task| (task.root.clone(), task.route.clone())).collect();
        }
    });
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary =
        Some(Arc::new(move |phase| {
            if phase == "after_reclamation_pass" { let _ = done_tx.send(()); }
        }));
    let result = claimed.state.schedule_engram_root_reclamation(true);
    done_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).expect("the scheduled pass finishes");
    claimed.state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;
    assert_eq!(result, "a bounded reclamation pass was scheduled");
    assert_eq!(*selected.borrow(), expected.iter()
        .map(|task| (task.root.clone(), task.route.clone())).collect::<Vec<_>>());
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
