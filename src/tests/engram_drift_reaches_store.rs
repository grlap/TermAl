//! Ordinary receipts do not establish or advance historical source baselines.
//! Missing history stays explicit, and observation delivery never gates a prompt.
use super::*;

fn named_ordinary_fixture(label: &str) -> (EOCheckedTurn, PathBuf) {
    let turn = EOCheckedTurn::idle(label, true);
    let worktree = add_observed_worktree(&turn);
    name_observed_worktree(&turn, &worktree);
    (turn, worktree)
}

#[test]
fn successive_ordinary_turns_without_history_keep_the_missing_baseline_explicit() {
    let (turn, worktree) = named_ordinary_fixture("ordinary-missing-history");
    let first_revision = content_revision_of(&worktree);
    for index in 0..2 {
        if index == 1 {
            fs::write(worktree.join("README.md"), "changed between ordinary turns\n").unwrap();
            assert_ne!(content_revision_of(&worktree), first_revision);
        }
        let revision = content_revision_of(&worktree);
        assert!(turn.record(|record| record.engram.runtime_checkpoint.is_none()));
        let prompt = turn.begin_prompt("Observe the current contents without editing.");
        assert!(prompt.prompt.contains("Observe the current contents without editing."));
        let turn_ref = observed_turn_ref(&turn);
        assert!(ordinary_turn_requests(&turn, &turn_ref).is_empty(),
            "provider delivery precedes this turn's closing observation");
        let request = turn.finish();
        assert_ordinary_observation_has_no_native_credit(&turn, &request);
        assert_eq!(request["causality"]["kind"], "unknown");
        let change = &request["occurrence"]["source_change"];
        assert_eq!(change["detection"], "assumed_missing_baseline");
        assert!(change.get("baseline").is_none());
        assert_eq!(change["sighting"]["source_basis"]["source_revision"], revision);
        assert_eq!(PathBuf::from(change["workspace_id"].as_str().unwrap()),
            fs::canonicalize(&worktree).unwrap());
        wait_for_observation_delivery(&turn);
        assert!(turn.record(|record| record.engram.runtime_checkpoint.is_none()),
            "a successful ordinary receipt creates no historical checkpoint");
    }
    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.session_id).unwrap()];
    assert!(record.engram.runtime_checkpoint.is_none());
}

#[test]
fn ordinary_receipts_compare_with_but_never_advance_an_explicit_passive_baseline() {
    for changed in [false, true] {
        let (turn, worktree) = named_ordinary_fixture("ordinary-passive-history");
        let historical = install_passive_measured_baseline(&turn, &worktree);
        if changed {
            fs::write(worktree.join("README.md"), "changed after the historical measurement\n").unwrap();
        }
        let current = content_revision_of(&worktree);
        assert_eq!(historical.endpoint.source_revision != current, changed);
        for _ in 0..2 {
            turn.begin("Observe the current source without editing.");
            let request = turn.finish();
            assert_ordinary_observation_has_no_native_credit(&turn, &request);
            assert_eq!(request["causality"]["kind"], "unknown");
            let change = &request["occurrence"]["source_change"];
            if changed {
                assert_eq!(change["detection"], "content_comparison");
                assert_eq!(change["baseline"], serde_json::to_value(&historical.endpoint).unwrap());
                assert_eq!(change["sighting"]["source_basis"]["source_revision"], current);
                assert_eq!(PathBuf::from(change["workspace_id"].as_str().unwrap()),
                    fs::canonicalize(&worktree).unwrap());
                let from = chrono::DateTime::parse_from_rfc3339(request["observed_interval"]["from"].as_str().unwrap()).unwrap();
                let baseline_at = chrono::DateTime::parse_from_rfc3339(&historical.endpoint.observed_at).unwrap();
                let through = chrono::DateTime::parse_from_rfc3339(request["observed_interval"]["through"].as_str().unwrap()).unwrap();
                assert!(from <= baseline_at && baseline_at <= through);
            } else {
                assert!(change.is_null(), "equal historical and current content supplies no source change");
            }
            wait_for_observation_delivery(&turn);
            assert_eq!(turn.record(|record| record.engram.runtime_checkpoint.clone()), Some(historical.clone()),
                "ordinary accounting leaves the explicitly seeded historical fixture untouched");
            assert_eq!(content_revision_of(&worktree), current);
        }
        turn.state.shutdown_persist_blocking();
        let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
        let record = &loaded.sessions[loaded.find_session_index(&turn.session_id).unwrap()];
        assert_eq!(record.engram.runtime_checkpoint.as_ref(), Some(&historical));
    }
}

#[test]
fn a_lost_observation_reply_cannot_hold_an_ordinary_provider_handoff() {
    struct BeforeObservationRetry {
        control: Arc<ScriptedEngramControlTransport>,
        attempts: std::sync::atomic::AtomicUsize,
        retry: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }
    impl EngramControlTransport for BeforeObservationRetry {
        fn request(&self, connection: &EngramConnectionConfig, request: &EngramControlRequest,
            timeout: Duration) -> Result<Value, EngramTransportError> {
            if matches!(request, EngramControlRequest::ExecutionObserve { .. })
                && self.attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1
            {
                let _ = self.retry.send(());
                self.release.lock().unwrap().recv_timeout(phase_sync::DEADLOCK_GUARD)
                    .map_err(|error| EngramTransportError::transport(format!("retry fixture released: {error}")))?;
            }
            self.control.request(connection, request, timeout)
        }
        fn read_observation_policy(&self, connection: &EngramConnectionConfig,
            store: &EngramAuthorityStoreKey, timeout: Duration, host_workdir: &FsPath)
            -> Result<EngramObservationPolicyBasis, EngramTransportError> {
            self.control.read_observation_policy(connection, store, timeout, host_workdir)
        }
        fn read_work_binding(&self, connection: &EngramConnectionConfig,
            preference: EngramBindingPreference<'_>, timeout: Duration)
            -> Result<Option<EngramControlWorkBinding>, EngramTransportError> {
            self.control.read_work_binding(connection, preference, timeout)
        }
        fn read_held_claims(&self, connection: &EngramConnectionConfig, timeout: Duration)
            -> Result<EngramHeldClaims, EngramTransportError> {
            self.control.read_held_claims(connection, timeout)
        }
        fn shutdown_session(&self, session: &str) { self.control.shutdown_session(session); }
    }
    let (turn, worktree) = named_ordinary_fixture("ordinary-no-admission-barrier");
    fs::write(worktree.join("README.md"), "source before a lost observation reply\n").unwrap();
    turn.transport.named_roots.lock().unwrap().as_mut().unwrap().lose_next_observation_reply = true;
    let (retry, retry_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    turn.state.install_test_engram_transport(Arc::new(BeforeObservationRetry {
        control: turn.transport.clone(), attempts: std::sync::atomic::AtomicUsize::new(0),
        retry, release: Mutex::new(release_rx),
    }));
    let prompt = turn.begin_prompt("The provider receives this before the observation can fail.");
    assert!(prompt.prompt.contains("The provider receives this"));
    let turn_ref = observed_turn_ref(&turn);
    assert!(ordinary_turn_requests(&turn, &turn_ref).is_empty());
    turn.end();
    phase_sync::receive(&retry_rx, "the actual worker reaches retry after persisting the lost reply");
    // The transport holds the replay before delegating it to the model. The
    // exact pending image has already passed the worker's SQLite read fence.
    let pending = turn.record(|record| record.engram.unadmitted_reports.iter()
            .find(|report| report.host_turn_ref == turn_ref
                && report.envelopes.iter().any(|envelope| matches!(&envelope.delivery,
                    EngramRuntimeObservationDelivery::Pending { last_error: Some(_) })))
            .cloned().expect("the lost reply retains this exact turn's pending envelope"));
    let requests = ordinary_turn_requests(&turn, &turn_ref);
    assert_eq!(requests, [pending.envelopes[0].request.clone()]);
    let recorded = turn.transport.named_roots.lock().unwrap().as_ref().unwrap().observations
        .get(&(turn.session_id.clone(), pending.envelopes[0].request["idempotency_key"].as_str().unwrap().to_owned()))
        .cloned().expect("the model applied this exact request before losing its reply");
    assert_eq!(recorded.0, pending.envelopes[0].request);
    assert_eq!(pending.envelopes[0].request["causality"]["kind"], "unknown");
    assert_eq!(pending.envelopes[0].request["occurrence"]["source_change"]["detection"], "assumed_missing_baseline");
    // Retire settlement before releasing the controlled replay in teardown.
    turn.state.runtime_observation_lifetime.stop_and_join();
    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.session_id).unwrap()];
    let saved = record.engram.unadmitted_reports.iter().find(|report| report.host_turn_ref == turn_ref).unwrap();
    assert_eq!(saved.envelopes, pending.envelopes, "actual SQLite keeps the exact possibly-applied request");
    assert!(record.engram.runtime_checkpoint.is_none());
    assert_ordinary_turn_has_no_native_requests(&turn);
    drop(release);
}
