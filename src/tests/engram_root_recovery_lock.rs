// Owns recovery-batch lock-hold scaling witnesses using the existing root
// fixtures and StateMutex reporter. Does not read a live store or measure
// canonical transport latency. Split beside the reclamation regressions.
use super::*;

const RECOVERY_HOLD_SAMPLES: usize = 100;
const RECOVERY_HOLD_BOUND: Duration = Duration::from_millis(50);

struct RecoveryHoldFixture {
    claimed: ClaimedRoot,
    store: EngramAuthorityStoreKey,
    owner: EngramAuthorityTransition,
    holds: Arc<Mutex<Vec<Duration>>>,
}

impl RecoveryHoldFixture {
    fn new(sessions: usize, journals: usize) -> Self {
        assert!(sessions >= 2);
        assert!((1..=ENGRAM_NAMED_ROOT_JOURNAL_LIMIT).contains(&journals));
        let (mut claimed, root) = settled_root("recovery-lock-scaling", "completed");
        claimed.settle_capture_workers();
        let store = root.store.clone();
        let owner = {
            let mut inner = claimed.state.inner.lock().unwrap();
            let journal = inner.engram_named_root_journal[0].clone();
            let binding = journal.read_binding.as_ref().unwrap().clone();
            let image = claimed.state.prepare_engram_authority_image_locked(
                &mut inner, &store, &binding,
            ).unwrap();
            let owner = image.history.transition.unwrap();
            let index = inner.find_session_index(&claimed.session_id).unwrap();
            let reporter = inner.sessions[index].clone();
            inner.sessions.clear();
            // Retained roots have no runtime, token or claimed work. They
            // still participate in the production reader's session scan.
            for ordinal in 0..sessions - 1 {
                let mut retained = reporter.clone();
                retained.session.id = format!("session-{}", 100_000 + ordinal);
                retained.session.status = SessionStatus::Idle;
                retained.runtime = SessionRuntime::None;
                retained.engram.routing_token = None;
                retained.engram.work_binding = None;
                retained.engram.named_root = Some(EngramNamedRootState::None);
                inner.sessions.push(retained);
            }
            // A late eligible reader exercises the linear lookup cost and
            // the original namer preference, not a first-row shortcut.
            inner.sessions.push(reporter);
            for ordinal in 1..ENGRAM_WORK_SOURCE_ROOT_LIMIT {
                let mut retained = root.clone();
                retained.work_id = format!("retained-root-work-{ordinal}");
                retained.claim_id = format!("retained-root-claim-{ordinal}");
                retained.short_ref = format!("retained-root-{ordinal}");
                inner.engram_work_source_roots.push(retained);
            }
            inner.engram_named_root_journal.clear();
            for ordinal in 0..journals {
                let mut retained = journal.clone();
                if ordinal > 0 {
                    retained.claim_id = format!("retained-journal-claim-{ordinal}");
                    let binding = retained.read_binding.as_mut().unwrap();
                    binding.work_id = format!("retained-journal-work-{ordinal}");
                    binding.claim_id = retained.claim_id.clone();
                    binding.run_id = format!("retained-journal-run-{ordinal}");
                    binding.root_execution_id = format!("retained-journal-execution-{ordinal}");
                    let (event, receipt) = retained.confirmed.as_mut().unwrap();
                    event.root.work_id = binding.work_id.clone();
                    event.root.claim_id = binding.claim_id.clone();
                    receipt.position.feed.id = binding.run_id.clone();
                }
                let mut intent = retained.confirmed.as_ref().unwrap().0.clone();
                // Cover both a live reporter at the end of the session table
                // and a missing reporter; neither authorizes settlement here.
                if ordinal % 2 == 1 {
                    intent.reporter = "session-missing-recovery-reporter".to_owned();
                }
                retained.pending = Some(intent);
                inner.engram_named_root_journal.push(retained);
            }
            owner
        };
        let holds = Arc::new(Mutex::new(Vec::new()));
        let seen = holds.clone();
        let measured_thread = std::thread::current().id();
        // The control-only wrapper owns a Weak pointer to this mutex. Even
        // after capture workers settle, that pointer prevents Arc::get_mut.
        // Drop only the wrapper while installing the reporter, then restore
        // the same control fixture before any recovery call is measured.
        claimed.state.install_test_engram_transport(claimed.transport.clone());
        let mutex = Arc::get_mut(&mut claimed.state.inner)
            .expect("the settled fixture exclusively owns its state mutex");
        mutex.warn_after = Duration::ZERO;
        mutex.diagnostic_reporter = Arc::new(move |diagnostic| {
            if std::thread::current().id() == measured_thread
                && let StateMutexDiagnostic::Held { held, .. } = diagnostic
            {
                seen.lock().unwrap().push(held);
            }
        });
        install_control_only_transport(&claimed.state, claimed.transport.clone());
        Self { claimed, store, owner, holds }
    }

    fn measure(&self, unresolved: bool) -> (Duration, Duration) {
        let requests_before = self.claimed.transport.requests().len();
        let journals_before = self.claimed.state.inner.lock().unwrap()
            .engram_named_root_journal.clone();
        let mut worst_per_call = Vec::new();
        for _ in 0..RECOVERY_HOLD_SAMPLES {
            {
                let mut inner = self.claimed.state.inner.lock().unwrap();
                let history = inner.engram_work_naming_history.iter_mut()
                    .find(|history| history.work_id == self.owner.binding.work_id).unwrap();
                history.unresolved_runs.clear();
                if unresolved {
                    history.unresolved_runs.push(self.owner.binding.clone());
                }
            }
            // Exclude fixture preparation; capture every foreground lock in
            // the actual batch, including reader validation and settlement.
            self.holds.lock().unwrap().clear();
            let clock = self.claimed.state.engram_budget_clock();
            self.claimed.state.recover_engram_authority_batch_until(
                &self.store, &self.owner, clock.now() + DEADLOCK_GUARD, true, None,
            ).unwrap();
            let observed = std::mem::take(&mut *self.holds.lock().unwrap());
            worst_per_call.push(*observed.iter().max().expect("the batch took a state lock"));
            let inner = self.claimed.state.inner.lock().unwrap();
            assert_eq!(inner.engram_named_root_journal, journals_before,
                "candidate scans must not settle unrelated or executable intents");
            let history = inner.engram_work_naming_history.iter()
                .find(|history| history.work_id == self.owner.binding.work_id).unwrap();
            assert!(history.unresolved_runs.is_empty(), "the exact completed run was read");
            assert_eq!(history.transition.as_ref().unwrap(), &self.owner);
        }
        assert_eq!(self.claimed.transport.requests().len() - requests_before,
            if unresolved { RECOVERY_HOLD_SAMPLES } else { 0 },
            "one canonical read per retained run, none for an empty batch");
        worst_per_call.sort_unstable();
        // Nearest-rank empirical p99 of 100 actual batch calls.
        (worst_per_call[98], *worst_per_call.last().unwrap())
    }
}

#[test]
fn root_recovery_empty_batch_does_not_hold_state_for_retained_inventory() {
    let fixture = RecoveryHoldFixture::new(4096, ENGRAM_NAMED_ROOT_JOURNAL_LIMIT);
    let (p99, maximum) = fixture.measure(false);
    eprintln!("root_recovery_hold empty=true sessions=4096 journals=256 samples=100 p99_us={} max_us={}",
        p99.as_micros(), maximum.as_micros());
    assert!(p99 < RECOVERY_HOLD_BOUND,
        "empty recovery batch p99 {p99:?} exceeds {RECOVERY_HOLD_BOUND:?} (max {maximum:?})");
}

#[test]
fn root_recovery_active_batch_holds_stay_bounded_as_sessions_and_journals_grow() {
    let mut measurements = Vec::new();
    for (sessions, journals) in [(64, 1), (1024, 1), (4096, 1), (4096, 256)] {
        let fixture = RecoveryHoldFixture::new(sessions, journals);
        let (p99, maximum) = fixture.measure(true);
        eprintln!("root_recovery_hold empty=false sessions={sessions} journals={journals} samples=100 p99_us={} max_us={}",
            p99.as_micros(), maximum.as_micros());
        measurements.push((sessions, journals, p99, maximum));
    }
    assert!(measurements.iter().all(|(_, _, p99, _)| *p99 < RECOVERY_HOLD_BOUND),
        "recovery batch holds must stay below {RECOVERY_HOLD_BOUND:?}: {measurements:?}");
}

#[test]
fn root_recovery_revalidates_owner_and_reader_after_unlocked_selection() {
    for replace_owner in [true, false] {
        let fixture = RecoveryHoldFixture::new(64, 1);
        {
            let mut inner = fixture.claimed.state.inner.lock().unwrap();
            inner.engram_work_naming_history.iter_mut()
                .find(|history| history.work_id == fixture.owner.binding.work_id).unwrap()
                .unresolved_runs = vec![fixture.owner.binding.clone()];
        }
        let journals = fixture.claimed.state.inner.lock().unwrap().engram_named_root_journal.clone();
        let before = fixture.claimed.transport.requests().len();
        let state = fixture.claimed.state.clone();
        let session = fixture.claimed.session_id.clone();
        let work = fixture.owner.binding.work_id.clone();
        let visited = std::rc::Rc::new(std::cell::Cell::new(false));
        let observed = visited.clone();
        let _probe = super::scheduler::PlanningProbe::install(move |phase, _| {
            if phase != "recovery_selected" { return; }
            assert!(state.inner.is_not_held_by_current_thread_for_test());
            observed.set(true);
            let mut inner = state.inner.lock().unwrap();
            if replace_owner {
                inner.engram_work_naming_history.iter_mut()
                    .find(|history| history.work_id == work).unwrap()
                    .transition.as_mut().unwrap().version += 1;
            } else {
                let index = inner.find_session_index(&session).unwrap();
                inner.sessions[index].engram.routing_token = None;
            }
        });
        let clock = fixture.claimed.state.engram_budget_clock();
        let result = fixture.claimed.state.recover_engram_authority_batch_until(
            &fixture.store, &fixture.owner, clock.now() + DEADLOCK_GUARD, true, None,
        );
        assert!(visited.get(), "the candidate was chosen outside the lock");
        if replace_owner {
            assert!(result.unwrap_err().message.contains("superseded during reader selection"));
        } else {
            result.unwrap();
        }
        assert_eq!(fixture.claimed.transport.requests().len(), before,
            "neither a superseded owner nor a tokenless candidate may read");
        let inner = fixture.claimed.state.inner.lock().unwrap();
        assert_eq!(inner.engram_named_root_journal, journals);
        assert_eq!(inner.engram_work_naming_history.iter()
            .find(|history| history.work_id == fixture.owner.binding.work_id).unwrap()
            .unresolved_runs, vec![fixture.owner.binding.clone()]);
    }
}

#[test]
fn root_recovery_revalidates_abandoned_reporter_after_unlocked_selection() {
    let fixture = RecoveryHoldFixture::new(64, 1);
    let missing = "session-returning-recovery-reporter";
    let journals = {
        let mut inner = fixture.claimed.state.inner.lock().unwrap();
        inner.engram_named_root_journal[0].pending.as_mut().unwrap().reporter = missing.to_owned();
        inner.engram_work_naming_history.iter_mut()
            .find(|history| history.work_id == fixture.owner.binding.work_id).unwrap()
            .unresolved_runs = vec![fixture.owner.binding.clone()];
        inner.engram_named_root_journal.clone()
    };
    let state = fixture.claimed.state.clone();
    let session = fixture.claimed.session_id.clone();
    let _probe = super::scheduler::PlanningProbe::install(move |phase, _| {
        if phase != "recovery_selected" { return; }
        assert!(state.inner.is_not_held_by_current_thread_for_test());
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let mut returning = inner.sessions[index].clone();
        returning.session.id = missing.to_owned();
        inner.sessions.push(returning);
    });
    let before = fixture.claimed.transport.requests().len();
    let clock = fixture.claimed.state.engram_budget_clock();
    fixture.claimed.state.recover_engram_authority_batch_until(
        &fixture.store, &fixture.owner, clock.now() + DEADLOCK_GUARD, true, None,
    ).unwrap();
    assert_eq!(fixture.claimed.transport.requests().len(), before + 1);
    assert_eq!(fixture.claimed.state.inner.lock().unwrap().engram_named_root_journal, journals,
        "a currently available reporter retains its executable intent");
}

#[test]
fn root_recovery_skips_excluded_preferred_and_first_readers() {
    for (preferred, all_excluded) in [(true, false), (false, false), (true, true), (false, true)] {
        let fixture = RecoveryHoldFixture::new(2, 1);
        let excluded = fixture.claimed.session_id.clone();
        let healthy = "session-healthy-root-reader".to_owned();
        let (ledger, binding) = {
            let mut inner = fixture.claimed.state.inner.lock().unwrap();
            let index = inner.find_session_index(&excluded).unwrap();
            let original = inner.sessions[index].clone();
            let target = AppState::engram_source_root_target_locked(&inner, &excluded)
                .unwrap().unwrap();
            let token = original.engram.routing_token.clone().unwrap();
            let grant = "retained-root-reader-grant".to_owned();
            let request = EngramControlRequest::TurnCheckpoint {
                routing_token: token.clone(),
                grant_id: grant.clone(),
                next_intent: EngramNextIntent::Exit,
                report: EngramTurnReport::default(),
                idempotency_key: "retained-root-reader-checkpoint".to_owned(),
            };
            let debt = EngramSessionObligation {
                id: "retained-root-reader-duty".to_owned(),
                project_id: original.session.project_id.clone().unwrap(),
                authority: fixture.store.clone(),
                authority_confirmed: true,
                connection: target.connection,
                external_ref: format!("termal:session:{excluded}"),
                local_session_id: excluded.clone(),
                owner_generation: 1,
                prompt_id: None,
                routing_token: token,
                grant_id: grant,
                kind: EngramSessionObligationKind::CheckpointContainment,
                phase: EngramSessionObligationPhase::Sent,
                basis: EngramSessionObligationBasis::CheckpointUnsettled,
                reason_code: "checkpoint_failed".to_owned(),
                frozen_request: serde_json::to_value(request).unwrap(),
                settlement_evidence: Value::Null,
            };
            inner.engram_session_obligations =
                inner.engram_session_obligations.with_entry(debt.clone()).unwrap();
            let mut successor = original.clone();
            successor.session.id = healthy.clone();
            successor.engram.routing_token = Some("healthy-root-reader-token".to_owned());
            inner.sessions = vec![original, successor];
            assert_eq!(inner.sessions[0].session.id, excluded);
            inner.engram_work_source_roots.iter_mut().find(|root| {
                root.store == fixture.store && root.claim_id == fixture.owner.binding.claim_id
            }).unwrap().named_by_session = if preferred {
                excluded.clone()
            } else {
                "session-absent-root-namer".to_owned()
            };
            inner.engram_work_naming_history.iter_mut()
                .find(|history| history.work_id == fixture.owner.binding.work_id).unwrap()
                .unresolved_runs = vec![fixture.owner.binding.clone()];
            assert!(engram_admission_obligation_locked(&inner, &excluded).is_some());
            if all_excluded {
                let mut successor_debt = debt;
                successor_debt.id.push_str("-successor");
                successor_debt.local_session_id = healthy.clone();
                successor_debt.connection.session_id = healthy.clone();
                successor_debt.external_ref = format!("termal:session:{healthy}");
                successor_debt.routing_token = "healthy-root-reader-token".to_owned();
                successor_debt.grant_id.push_str("-successor");
                successor_debt.frozen_request = serde_json::to_value(
                    EngramControlRequest::TurnCheckpoint {
                        routing_token: successor_debt.routing_token.clone(),
                        grant_id: successor_debt.grant_id.clone(),
                        next_intent: EngramNextIntent::Exit,
                        report: EngramTurnReport::default(),
                        idempotency_key: "successor-root-reader-checkpoint".to_owned(),
                    },
                ).unwrap();
                inner.engram_session_obligations =
                    inner.engram_session_obligations.with_entry(successor_debt).unwrap();
            }
            assert_eq!(engram_admission_obligation_locked(&inner, &healthy).is_some(), all_excluded);
            assert!(AppState::engram_source_root_target_locked(&inner, &excluded)
                .unwrap().is_none());
            assert_eq!(AppState::engram_source_root_target_locked(&inner, &healthy)
                .unwrap().is_none(), all_excluded);
            let reporter = inner.engram_named_root_journal[0].pending.as_ref().unwrap();
            assert!(!AppState::engram_root_reporter_available(&inner, reporter));
            if all_excluded {
                assert!(engram_root_recovery_reader_target_locked(
                    &inner, &fixture.store, Some(&excluded)).is_none());
            }
            (serde_json::to_value(&inner.engram_session_obligations).unwrap(),
                inner.sessions[1].engram.work_binding.clone().unwrap())
        };
        fixture.claimed.transport.enable_named_roots(&healthy, &binding);
        let before = fixture.claimed.transport.requests().len();
        let clock = fixture.claimed.state.engram_budget_clock();
        fixture.claimed.state.recover_engram_authority_batch_until(
            &fixture.store, &fixture.owner, clock.now() + DEADLOCK_GUARD, true, None,
        ).unwrap();
        let requests = fixture.claimed.transport.requests();
        assert_eq!(requests.len() - before, usize::from(!all_excluded),
            "only a healthy reader can perform the canonical recovery read");
        assert!(requests[before..].iter().all(|request| {
            request.connection.session_id == healthy
                && request.request["operation"] == "named_root_read"
        }), "an excluded preferred/first reader must never perform an RPC");
        let inner = fixture.claimed.state.inner.lock().unwrap();
        assert_eq!(serde_json::to_value(&inner.engram_session_obligations).unwrap(), ledger);
        assert!(engram_admission_obligation_locked(&inner, &excluded).is_some());
        assert_eq!(inner.engram_work_naming_history.iter()
            .find(|history| history.work_id == fixture.owner.binding.work_id).unwrap()
            .unresolved_runs.is_empty(), !all_excluded,
            "only the healthy successor resolves the retained run");
        assert!(matches!(fixture.claimed.runtime_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)));
    }
}
