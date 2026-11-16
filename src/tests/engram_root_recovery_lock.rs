// Count actual recovery row visits and lock ownership using the existing root
// fixtures and planning probe. CPU scheduling cannot change the work bound.
use super::*;

#[derive(Debug, Default)]
struct RecoveryWorkMeasure {
    rows: BTreeMap<String, usize>,
    planning_rows_under_state: usize,
}

#[derive(Debug, PartialEq)]
enum RecoveryWorkViolation {
    PlanningUnderState,
    RowVisits(String, usize, usize),
    MissingObservation,
}

impl RecoveryWorkMeasure {
    fn count(&self, phase: &str) -> usize {
        self.rows.get(phase).copied().unwrap_or_default()
    }

    fn check(
        &self,
        sessions: usize,
        projects: usize,
        parents: usize,
        journals: usize,
        active: bool,
    ) -> Result<(), RecoveryWorkViolation> {
        if self.planning_rows_under_state != 0 {
            return Err(RecoveryWorkViolation::PlanningUnderState);
        }
        // A batch with one retained run captures each inventory once. Its
        // journal passes are linear, never one session scan per journal.
        // Current-reader validation still runs under the real authority cut.
        for (phase, inventory) in [
            ("reader_capture_session", sessions),
            ("reader_capture_project", projects),
            ("reader_capture_parent", parents),
            ("reader_index_session", sessions),
            ("reader_index_project", projects),
            ("reader_index_parent", parents),
            ("reader_select_session", sessions),
            ("recovery_pending_journal", journals),
            ("recovery_snapshot_journal", journals),
            ("recovery_current_journal", journals),
            ("recovery_settle_journal", journals),
        ] {
            let expected = if active { inventory } else { 0 };
            let actual = self.count(phase);
            if actual > expected {
                return Err(RecoveryWorkViolation::RowVisits(
                    phase.to_owned(), expected, actual,
                ));
            }
        }
        for phase in ["recovery_candidate", "recovery_preference"] {
            let actual = self.count(phase);
            let expected = usize::from(active);
            if actual > expected {
                return Err(RecoveryWorkViolation::RowVisits(
                    phase.to_owned(), expected, actual,
                ));
            }
        }
        if active
            && [
                "reader_capture_session", "reader_index_session",
                "reader_select_session", "recovery_candidate",
            ].iter().any(|phase| self.count(phase) == 0)
        {
            return Err(RecoveryWorkViolation::MissingObservation);
        }
        Ok(())
    }
}

struct RecoveryWorkFixture {
    claimed: EOCheckedTurn,
    store: EngramAuthorityStoreKey,
    owner: EngramAuthorityTransition,
}

impl RecoveryWorkFixture {
    fn new(sessions: usize, journals: usize) -> Self {
        assert!(sessions >= 2);
        assert!((1..=ENGRAM_NAMED_ROOT_JOURNAL_LIMIT).contains(&journals));
        let (claimed, root) = settled_root("recovery-lock-scaling", "completed");
        claimed.wait_for_snapshots();
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
            // Earlier rows have no project association, runtime or claimed work.
            // They participate in the real scan without being eligible readers.
            for ordinal in 0..sessions - 1 {
                let mut retained = reporter.clone();
                retained.session.id = format!("session-{}", 100_000 + ordinal);
                retained.session.status = SessionStatus::Idle;
                retained.runtime = SessionRuntime::None;
                retained.session.project_id = None;
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
        Self { claimed, store, owner }
    }

    fn observe(&self, action: impl FnOnce()) -> RecoveryWorkMeasure {
        let measured = std::rc::Rc::new(std::cell::RefCell::new(RecoveryWorkMeasure::default()));
        let observed = measured.clone();
        let state = self.claimed.state.clone();
        let probe = super::scheduler::PlanningProbe::install(move |phase, _| {
            if !(phase.starts_with("reader_") || phase.starts_with("recovery_"))
                || phase == "recovery_selected"
            {
                return;
            }
            let mut work = observed.borrow_mut();
            *work.rows.entry(phase.to_owned()).or_default() += 1;
            if (phase.starts_with("reader_index_") || phase == "reader_select_session"
                || phase == "recovery_candidate" || phase == "recovery_preference")
                && !state.inner.is_not_held_by_current_thread_for_test()
            {
                work.planning_rows_under_state += 1;
            }
        });
        action();
        drop(probe);
        std::rc::Rc::try_unwrap(measured).unwrap().into_inner()
    }

    fn measure(&self, unresolved: bool) -> RecoveryWorkMeasure {
        let requests_before = self.claimed.transport.requests().len();
        let journals_before = self.claimed.state.inner.lock().unwrap()
            .engram_named_root_journal.clone();
        {
            let mut inner = self.claimed.state.inner.lock().unwrap();
            let history = inner.engram_work_naming_history.iter_mut()
                .find(|history| history.work_id == self.owner.binding.work_id).unwrap();
            history.unresolved_runs.clear();
            if unresolved {
                history.unresolved_runs.push(self.owner.binding.clone());
            }
        }
        let clock = self.claimed.state.engram_budget_clock();
        // Exclude fixture preparation; count actual row visits in the
        // foreground batch, including its journal settlement passes.
        let work = self.observe(|| {
            self.claimed.state.recover_engram_authority_batch_until(
                &self.store, &self.owner, clock.now() + DEADLOCK_GUARD, true, None,
            ).unwrap();
        });
        let inner = self.claimed.state.inner.lock().unwrap();
        assert_eq!(inner.engram_named_root_journal, journals_before,
            "candidate scans must not settle unrelated or executable intents");
        let history = inner.engram_work_naming_history.iter()
            .find(|history| history.work_id == self.owner.binding.work_id).unwrap();
        assert!(history.unresolved_runs.is_empty(), "the exact completed run was read");
        assert_eq!(history.transition.as_ref().unwrap(), &self.owner);
        drop(inner);
        assert_eq!(self.claimed.transport.requests().len() - requests_before,
            usize::from(unresolved),
            "one canonical read per retained run, none for an empty batch");
        work
    }
}

#[test]
fn root_recovery_empty_batch_does_not_hold_state_for_retained_inventory() {
    let fixture = RecoveryWorkFixture::new(4096, ENGRAM_NAMED_ROOT_JOURNAL_LIMIT);
    let measure = fixture.measure(false);
    assert_eq!(measure.check(4096, 1, 0, ENGRAM_NAMED_ROOT_JOURNAL_LIMIT, false), Ok(()),
        "empty recovery must not visit retained inventories: {measure:?}");
}

#[test]
fn root_recovery_active_batch_holds_stay_bounded_as_sessions_and_journals_grow() {
    for (sessions, journals) in [(64, 1), (1024, 1), (4096, 1), (4096, ENGRAM_NAMED_ROOT_JOURNAL_LIMIT)] {
        let fixture = RecoveryWorkFixture::new(sessions, journals);
        let measure = fixture.measure(true);
        let inner = fixture.claimed.state.inner.lock().unwrap();
        assert_eq!(measure.check(sessions, inner.projects.len(), inner.delegations.len(), journals, true), Ok(()),
            "recovery work must stay linear at {sessions} sessions and {journals} journals: {measure:?}");
    }
}

#[test]
fn root_recovery_work_oracle_rejects_reader_planning_under_state_lock() {
    let fixture = RecoveryWorkFixture::new(64, 1);
    // This retained reference path really indexes and selects readers under
    // StateInner, reproducing the lock-scope regression without a pause.
    let measure = fixture.observe(|| {
        let inner = fixture.claimed.state.inner.lock().unwrap();
        assert!(engram_root_recovery_reader_target_locked(
            &inner, &fixture.store, Some(&fixture.claimed.session_id)).is_some());
    });
    assert!(measure.count("reader_index_session") >= 64);
    assert_eq!(measure.check(64, 1, 0, 1, true), Err(RecoveryWorkViolation::PlanningUnderState));
}

#[test]
fn root_recovery_work_oracle_rejects_repeated_inventory_capture() {
    let fixture = RecoveryWorkFixture::new(64, 1);
    let measure = fixture.observe(|| {
        let inner = fixture.claimed.state.inner.lock().unwrap();
        for _ in 0..2 {
            let snapshot = EngramRootReaderSnapshot::capture(&inner);
            assert_eq!(snapshot.sessions.len(), 64);
        }
    });
    assert_eq!(measure.check(64, 1, 0, 1, true),
        Err(RecoveryWorkViolation::RowVisits("reader_capture_session".to_owned(), 64, 128)));
}

#[test]
fn root_recovery_revalidates_owner_and_reader_after_unlocked_selection() {
    for replace_owner in [true, false] {
        let fixture = RecoveryWorkFixture::new(64, 1);
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
                inner.sessions[index].session.project_id = None;
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
            "neither a superseded owner nor a projectless candidate may read");
        let inner = fixture.claimed.state.inner.lock().unwrap();
        assert_eq!(inner.engram_named_root_journal, journals);
        assert_eq!(inner.engram_work_naming_history.iter()
            .find(|history| history.work_id == fixture.owner.binding.work_id).unwrap()
            .unresolved_runs, vec![fixture.owner.binding.clone()]);
    }
}

#[test]
fn root_recovery_revalidates_abandoned_reporter_after_unlocked_selection() {
    let fixture = RecoveryWorkFixture::new(64, 1);
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
fn root_recovery_skips_projectless_readers_without_reactivating_historical_exclusions() {
    for (preferred, all_excluded) in [(true, false), (false, false), (true, true), (false, true)] {
        let fixture = RecoveryWorkFixture::new(2, 1);
        let excluded = fixture.claimed.session_id.clone();
        let healthy = "session-healthy-root-reader".to_owned();
        let (ledger, binding) = {
            let mut inner = fixture.claimed.state.inner.lock().unwrap();
            let index = inner.find_session_index(&excluded).unwrap();
            let original = inner.sessions[index].clone();
            let target = AppState::engram_source_root_target_locked(&inner, &excluded)
                .unwrap().unwrap();
            let token = "historical-reader-token".to_owned();
            let grant = "retained-root-reader-grant".to_owned();
            let request = json!({
                "operation":"turn_checkpoint", "routing_token":token,
                "grant_id":grant, "next_intent":"exit",
                "idempotency_key":"retained-root-reader-checkpoint"
            });
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
                frozen_request: request,
                settlement_evidence: Value::Null,
            };
            inner.engram_session_obligations = serde_json::from_value(json!({
                "schemaVersion":1, "entries":[debt.clone()], "checkpointOperations":[]
            })).unwrap();
            let mut successor = original.clone();
            successor.session.id = healthy.clone();
            successor.engram.historical_control.insert("engramRoutingToken".into(), json!("healthy-historical-token"));
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
            assert!(AppState::engram_source_root_target_locked(&inner, &excluded).unwrap().is_some(),
                "historical control debt has no ordinary exclusion authority");
            let excluded_index = inner.find_session_index(&excluded).unwrap();
            inner.sessions[excluded_index].session.project_id = None;
            if all_excluded {
                let mut successor_debt = debt;
                successor_debt.id.push_str("-successor");
                successor_debt.local_session_id = healthy.clone();
                successor_debt.connection.session_id = healthy.clone();
                successor_debt.external_ref = format!("termal:session:{healthy}");
                successor_debt.routing_token = "healthy-root-reader-token".to_owned();
                successor_debt.grant_id.push_str("-successor");
                successor_debt.frozen_request = json!({
                    "operation":"turn_checkpoint", "routing_token":successor_debt.routing_token,
                    "grant_id":successor_debt.grant_id, "next_intent":"exit",
                    "idempotency_key":"successor-root-reader-checkpoint"
                });
                let mut saved = serde_json::to_value(&inner.engram_session_obligations).unwrap();
                saved["entries"].as_array_mut().unwrap().push(serde_json::to_value(successor_debt).unwrap());
                inner.engram_session_obligations = serde_json::from_value(saved).unwrap();
                let healthy_index = inner.find_session_index(&healthy).unwrap();
                inner.sessions[healthy_index].session.project_id = None;
            }
            assert_eq!(inner.engram_session_obligations.entries.len(), 1 + usize::from(all_excluded));
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
        assert_eq!(inner.engram_session_obligations.entries.len(), 1 + usize::from(all_excluded));
        assert_eq!(inner.engram_work_naming_history.iter()
            .find(|history| history.work_id == fixture.owner.binding.work_id).unwrap()
            .unresolved_runs.is_empty(), !all_excluded,
            "only the healthy successor resolves the retained run");
        assert!(matches!(fixture.claimed._runtime_rx.lock().unwrap().try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)));
    }
}
