//! An ordinary turn's recorded closing sighting is the next turn's source
//! baseline: each closing compares with the previous turn's recorded sighting
//! (or a seeded one, used once), and observation delivery never gates a prompt.
use super::*;

fn named_ordinary_fixture(label: &str) -> (EOCheckedTurn, PathBuf) {
    let turn = EOCheckedTurn::idle(label, true);
    let worktree = add_observed_worktree(&turn);
    name_observed_worktree(&turn, &worktree);
    (turn, worktree)
}

/// The measured source sighting this turn's closing report sent.
fn sighting_request(turn: &EOCheckedTurn, turn_ref: &str) -> Value {
    let key = format!("termal-source-sighting:{turn_ref}");
    let sightings: Vec<_> = turn
        .transport
        .requests()
        .into_iter()
        .map(|sent| sent.request)
        .filter(|request| {
            request["operation"] == "execution_observe"
                && request["occurrence"]["kind"] == "source_sighting"
                && request["idempotency_key"] == key.as_str()
        })
        .collect();
    assert_eq!(sightings.len(), 1, "one sighting for {turn_ref}");
    sightings[0].clone()
}

/// The session's baseline after this turn settled: exactly its own recorded
/// sighting, under the session's claim and named root image.
fn assert_baseline_is_the_recorded_sighting(
    turn: &EOCheckedTurn,
    turn_ref: &str,
) -> EngramRuntimeCheckpointBasis {
    let sighting = sighting_request(turn, turn_ref);
    let basis = &sighting["occurrence"]["source_basis"];
    let baseline = turn
        .record(|record| record.engram.runtime_checkpoint.clone())
        .expect("the recorded closing sighting becomes the next baseline");
    assert_eq!(
        baseline.binding,
        turn.record(|record| record.engram.work_binding.clone().unwrap())
    );
    assert_eq!(
        serde_json::to_value(&baseline.endpoint).unwrap(),
        json!({
            "workspace_id": basis["workspace_id"],
            "source_id": basis["source_id"],
            "observed_at": sighting["occurrence"]["observed_at"],
        })
    );
    assert!(
        basis["source_root_generation"].as_i64().is_some(),
        "a named root's sighting carries its generation"
    );
    assert_eq!(
        baseline.source_root_generation,
        basis["source_root_generation"].as_i64()
    );
    assert_eq!(
        baseline.source_root_state,
        Some(EngramSourceRootState::Named)
    );
    baseline
}

fn assert_restored_baseline(turn: &EOCheckedTurn, expected: &EngramRuntimeCheckpointBasis) {
    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.session_id).unwrap()];
    assert_eq!(
        record.engram.runtime_checkpoint.as_ref(),
        Some(expected),
        "the baseline survives a restart"
    );
}

/// Real turns, through closing and delivery, past what a session keeps: every
/// turn's report was sent exactly once, nothing is sent again when the worker
/// runs after them or after a reload, and the row holds only the newest reports.
#[test]
fn ordinary_turns_past_the_retention_send_each_report_once_and_keep_only_the_newest() {
    let (turn, _worktree) = named_ordinary_fixture("ordinary-report-retention");
    let turns = ENGRAM_DISCHARGED_REPORT_RETENTION + 4;
    let mut refs = Vec::new();
    for _ in 0..turns {
        turn.begin_prompt("Observe the current contents.");
        refs.push(observed_turn_ref(&turn));
        turn.finish();
        wait_for_observation_delivery(&turn);
    }
    let sends = |turn: &EOCheckedTurn| {
        turn.transport
            .requests()
            .into_iter()
            .filter(|sent| sent.request["operation"] == "execution_observe")
            .map(|sent| sent.request["idempotency_key"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let sent = sends(&turn);
    assert_eq!(
        sent.len(),
        sent.iter().collect::<BTreeSet<_>>().len(),
        "no observation was sent twice: {sent:?}"
    );
    for turn_ref in &refs {
        assert_eq!(
            ordinary_turn_requests(&turn, turn_ref).len(),
            1,
            "{turn_ref}'s report went once"
        );
    }
    let newest = refs[turns - ENGRAM_DISCHARGED_REPORT_RETENTION..].to_vec();
    let retained = |record: &SessionRecord| {
        record
            .engram
            .unadmitted_reports
            .iter()
            .map(|report| report.host_turn_ref.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(turn.record(|record| retained(record)), newest);

    // A worker run finds nothing to report, evicted turns included.
    turn.state
        .start_runtime_observation_worker(&turn.session_id);
    wait_for_observation_delivery(&turn);
    assert_eq!(sends(&turn), sent, "nothing was reported again");

    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.session_id).unwrap()];
    assert_eq!(retained(record), newest, "the bound survives a restart");
    assert!(
        record.engram.unadmitted_closing_jobs.is_empty()
            && record.engram.unadmitted_active_turn.is_none(),
        "nothing restored could report an evicted turn again"
    );
    assert!(
        record
            .engram
            .unadmitted_reports
            .iter()
            .all(|report| report.is_settled()),
        "a restart finds nothing owed"
    );
}

#[test]
fn successive_ordinary_turns_compare_against_the_previous_recorded_sighting() {
    let (turn, worktree) = named_ordinary_fixture("ordinary-recorded-baseline");
    let workspace = fs::canonicalize(&worktree).unwrap();
    let mut previous: Option<EngramRuntimeCheckpointBasis> = None;
    for edit in [false, true, false] {
        if edit {
            fs::write(
                worktree.join("README.md"),
                "changed between ordinary turns\n",
            )
            .unwrap();
            turn.sources.advance(&worktree, "README.md");
        }
        let revision = owner_source_of(&turn, &worktree);
        assert_eq!(
            turn.record(|record| record.engram.runtime_checkpoint.clone()),
            previous
        );
        let prompt = turn.begin_prompt("Observe the current contents.");
        assert!(prompt.prompt.contains("Observe the current contents."));
        let turn_ref = observed_turn_ref(&turn);
        assert!(
            ordinary_turn_requests(&turn, &turn_ref).is_empty(),
            "provider delivery precedes this turn's closing observation"
        );
        let request = turn.finish();
        assert_ordinary_observation_has_no_native_credit(&turn, &request);
        assert_eq!(request["causality"]["kind"], "unknown");
        let change = &request["occurrence"]["source_change"];
        match &previous {
            None => {
                assert_eq!(
                    change["detection"], "assumed_missing_baseline",
                    "the first turn has no baseline"
                );
                assert!(change.get("baseline").is_none());
                assert_eq!(change["sighting"]["source_basis"]["source_id"], revision);
                assert_eq!(
                    PathBuf::from(change["workspace_id"].as_str().unwrap()),
                    workspace
                );
            }
            Some(baseline)
                if serde_json::to_value(&baseline.endpoint.source_id).unwrap() != revision =>
            {
                assert_eq!(change["detection"], "content_comparison");
                assert_eq!(
                    change["baseline"],
                    serde_json::to_value(&baseline.endpoint).unwrap(),
                    "compared with exactly the previous turn's recorded sighting"
                );
                assert_eq!(change["sighting"]["source_basis"]["source_id"], revision);
            }
            Some(_) => assert!(
                change.is_null(),
                "unchanged contents since the previous recorded sighting report no change: {request:#}"
            ),
        }
        wait_for_observation_delivery(&turn);
        let recorded = assert_baseline_is_the_recorded_sighting(&turn, &turn_ref);
        assert_eq!(
            serde_json::to_value(&recorded.endpoint.source_id).unwrap(),
            revision
        );
        assert_ne!(
            Some(&recorded),
            previous.as_ref(),
            "each recorded sighting replaces the last"
        );
        previous = Some(recorded);
    }
    assert_restored_baseline(&turn, previous.as_ref().unwrap());
}

#[test]
fn a_seeded_baseline_is_compared_once_then_replaced_by_the_recorded_sighting() {
    for changed in [false, true] {
        let (turn, worktree) = named_ordinary_fixture("ordinary-passive-history");
        let historical = install_passive_measured_baseline(&turn, &worktree);
        if changed {
            fs::write(
                worktree.join("README.md"),
                "changed after the historical measurement\n",
            )
            .unwrap();
            turn.sources.advance(&worktree, "README.md");
        }
        let current = owner_source_of(&turn, &worktree);
        assert_eq!(
            serde_json::to_value(&historical.endpoint.source_id).unwrap() != current,
            changed
        );

        turn.begin("Observe the current source without editing.");
        let first_ref = observed_turn_ref(&turn);
        let request = turn.finish();
        assert_ordinary_observation_has_no_native_credit(&turn, &request);
        assert_eq!(request["causality"]["kind"], "unknown");
        let change = &request["occurrence"]["source_change"];
        if changed {
            assert_eq!(change["detection"], "content_comparison");
            assert_eq!(
                change["baseline"],
                serde_json::to_value(&historical.endpoint).unwrap(),
                "the first turn compares with the seeded baseline"
            );
            assert_eq!(change["sighting"]["source_basis"]["source_id"], current);
            assert_eq!(
                PathBuf::from(change["workspace_id"].as_str().unwrap()),
                fs::canonicalize(&worktree).unwrap()
            );
            let from = chrono::DateTime::parse_from_rfc3339(
                request["observed_interval"]["from"].as_str().unwrap(),
            )
            .unwrap();
            let baseline_at =
                chrono::DateTime::parse_from_rfc3339(&historical.endpoint.observed_at).unwrap();
            let through = chrono::DateTime::parse_from_rfc3339(
                request["observed_interval"]["through"].as_str().unwrap(),
            )
            .unwrap();
            assert!(from <= baseline_at && baseline_at <= through);
        } else {
            assert!(
                change.is_null(),
                "equal seeded and current content supplies no source change"
            );
        }
        wait_for_observation_delivery(&turn);
        let first = assert_baseline_is_the_recorded_sighting(&turn, &first_ref);
        assert_ne!(
            first, historical,
            "the seeded baseline is used once, then replaced"
        );
        assert_eq!(
            serde_json::to_value(&first.endpoint.source_id).unwrap(),
            current
        );

        turn.begin("Observe the same source again.");
        let second_ref = observed_turn_ref(&turn);
        let request = turn.finish();
        assert!(
            request["occurrence"]["source_change"].is_null(),
            "the second turn compares with the first turn's recorded sighting, not the seed: {request:#}"
        );
        wait_for_observation_delivery(&turn);
        let second = assert_baseline_is_the_recorded_sighting(&turn, &second_ref);
        assert_ne!(second, first);
        assert_eq!(owner_source_of(&turn, &worktree), current);
        assert_restored_baseline(&turn, &second);
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
        fn request(
            &self,
            connection: &EngramConnectionConfig,
            request: &EngramControlRequest,
            timeout: Duration,
        ) -> Result<Value, EngramTransportError> {
            if matches!(request, EngramControlRequest::ExecutionObserve { .. })
                && self
                    .attempts
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    == 1
            {
                let _ = self.retry.send(());
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(phase_sync::DEADLOCK_GUARD)
                    .map_err(|error| {
                        EngramTransportError::transport(format!("retry fixture released: {error}"))
                    })?;
            }
            self.control.request(connection, request, timeout)
        }
        fn read_observation_policy(
            &self,
            connection: &EngramConnectionConfig,
            store: &EngramAuthorityStoreKey,
            timeout: Duration,
            host_workdir: &FsPath,
        ) -> Result<EngramObservationPolicyBasis, EngramTransportError> {
            self.control
                .read_observation_policy(connection, store, timeout, host_workdir)
        }
        fn read_work_binding(
            &self,
            connection: &EngramConnectionConfig,
            preference: EngramBindingPreference<'_>,
            timeout: Duration,
        ) -> Result<Option<EngramControlWorkBinding>, EngramTransportError> {
            self.control
                .read_work_binding(connection, preference, timeout)
        }
        fn read_held_claims(
            &self,
            connection: &EngramConnectionConfig,
            timeout: Duration,
        ) -> Result<EngramHeldClaims, EngramTransportError> {
            self.control.read_held_claims(connection, timeout)
        }
        fn shutdown_session(&self, session: &str) {
            self.control.shutdown_session(session);
        }
    }
    let (turn, worktree) = named_ordinary_fixture("ordinary-no-admission-barrier");
    fs::write(
        worktree.join("README.md"),
        "source before a lost observation reply\n",
    )
    .unwrap();
    turn.sources.advance(&worktree, "README.md");
    turn.transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .lose_next_observation_reply = true;
    let (retry, retry_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    turn.state
        .install_test_engram_transport(Arc::new(BeforeObservationRetry {
            control: turn.transport.clone(),
            attempts: std::sync::atomic::AtomicUsize::new(0),
            retry,
            release: Mutex::new(release_rx),
        }));
    let prompt = turn.begin_prompt("The provider receives this before the observation can fail.");
    assert!(prompt.prompt.contains("The provider receives this"));
    let turn_ref = observed_turn_ref(&turn);
    assert!(ordinary_turn_requests(&turn, &turn_ref).is_empty());
    turn.end();
    phase_sync::receive(
        &retry_rx,
        "the actual worker reaches retry after persisting the lost reply",
    );
    // The transport holds the replay before delegating it to the model. The
    // exact pending image has already passed the worker's SQLite read fence.
    let pending = turn.record(|record| {
        record
            .engram
            .unadmitted_reports
            .iter()
            .find(|report| {
                report.host_turn_ref == turn_ref
                    && report.envelopes.iter().any(|envelope| {
                        matches!(
                            &envelope.delivery,
                            EngramRuntimeObservationDelivery::Pending {
                                last_error: Some(_)
                            }
                        )
                    })
            })
            .cloned()
            .expect("the lost reply retains this exact turn's pending envelope")
    });
    let requests = ordinary_turn_requests(&turn, &turn_ref);
    assert_eq!(requests, [pending.envelopes[0].request.clone()]);
    let recorded = turn
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .observations
        .get(&(
            turn.session_id.clone(),
            pending.envelopes[0].request["idempotency_key"]
                .as_str()
                .unwrap()
                .to_owned(),
        ))
        .cloned()
        .expect("the model applied this exact request before losing its reply");
    assert_eq!(recorded.0, pending.envelopes[0].request);
    assert_eq!(pending.envelopes[0].request["causality"]["kind"], "unknown");
    assert_eq!(
        pending.envelopes[0].request["occurrence"]["source_change"]["detection"],
        "assumed_missing_baseline"
    );
    // Retire settlement before releasing the controlled replay in teardown.
    turn.state.runtime_observation_lifetime.stop_and_join();
    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.session_id).unwrap()];
    let saved = record
        .engram
        .unadmitted_reports
        .iter()
        .find(|report| report.host_turn_ref == turn_ref)
        .unwrap();
    assert_eq!(
        saved.envelopes, pending.envelopes,
        "actual SQLite keeps the exact possibly-applied request"
    );
    assert!(record.engram.runtime_checkpoint.is_none());
    assert_ordinary_turn_has_no_native_requests(&turn);
    drop(release);
}
