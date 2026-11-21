// Observation publication through the production ordered broadcaster and
// connected persistence channel. Tests explicitly drive production SQLite
// collection/write/fence steps; they do not run a provider command or launcher.
use super::*;

#[path = "test_run_observation_replica.rs"]
mod replica;

const BROADCAST_BARRIER_REVISION: u64 = u64::MAX;

struct ObservationPublicationFixture {
    state: AppState,
    writer: FenceFixture,
    session: String,
    mailbox: Arc<StateBroadcastMailbox>,
    events: broadcast::Receiver<SseStreamEvent>,
    barrier_rx: mpsc::Receiver<()>,
    stop: Arc<AtomicBool>,
    broadcaster: Option<std::thread::JoinHandle<()>>,
    watermark: u64,
    /// The clock this state's observation fences measure their deadlines on.
    /// These tests drive every step by hand, so only an explicit advance, not
    /// elapsed host time, can expire a fence.
    clock: EngramBudgetClock,
}

impl ObservationPublicationFixture {
    fn new() -> Self {
        let mut writer = FenceFixture::new();
        let mut state = writer.app_state();
        let clock = state.select_test_scripted_engram_budget_clock();
        let root = writer.root.path().join("project");
        fs::create_dir(&root).unwrap();
        run_git_test_command(&root, &["init", "--quiet"]);
        let project = create_test_project(&state, &root, "Observation publication");
        let session = create_test_project_session(&state, Agent::Codex, &project, &root);
        let (runtime, _commands) = test_codex_runtime_handle("observation-publication-runtime");
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.session_mut_by_index(index).unwrap().runtime = SessionRuntime::Codex(runtime);
        }
        state.persist_tx = writer.tx.clone();
        writer.inner = state.inner.clone();
        let baseline = writer.collect();
        writer.write(&baseline).unwrap();

        let mailbox = Arc::new(StateBroadcastMailbox::default());
        state.state_broadcast_mailbox = Some(mailbox.clone());
        let events = state.subscribe_stream_events();
        let senders = state.state_broadcast_senders.clone();
        let worker_mailbox = mailbox.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (barrier_tx, barrier_rx) = mpsc::channel();
        let broadcaster = std::thread::Builder::new()
            .name("test-observation-state-broadcast".to_owned())
            .spawn(move || {
                loop {
                    let work = worker_mailbox.recv_next();
                    if worker_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let barrier = matches!(&work, StateBroadcastWork::Delta(event)
                    if event.revision() == BROADCAST_BARRIER_REVISION);
                    forward_state_broadcast_work(work, &senders);
                    if barrier {
                        barrier_tx.send(()).unwrap();
                    }
                }
            })
            .unwrap();
        Self {
            state,
            writer,
            session,
            mailbox,
            events,
            barrier_rx,
            stop,
            broadcaster: Some(broadcaster),
            watermark: baseline.watermark,
            clock,
        }
    }

    // This synthetic delta is only a drain barrier. The worker forwards it
    // through the same FIFO as real work; it neither commits nor hides an
    // event. Receiving it proves that every earlier snapshot was observed.
    fn flush(&mut self) -> Vec<SseStreamEvent> {
        self.mailbox.publish_delta(DeltaEvent::CodexUpdated {
            revision: BROADCAST_BARRIER_REVISION,
            codex: CodexState::default(),
        });
        self.barrier_rx
            .recv_timeout(phase_sync::DEADLOCK_GUARD)
            .unwrap();
        let mut events = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(SseStreamEvent::Delta(payload)) => {
                    let delta: DeltaEvent = serde_json::from_str(&payload).unwrap();
                    if delta.revision() == BROADCAST_BARRIER_REVISION {
                        return events;
                    }
                    events.push(SseStreamEvent::Delta(payload));
                }
                Ok(event) => events.push(event),
                Err(error) => panic!("ordered broadcaster lost events: {error:?}"),
            }
        }
    }

    fn start(&self, key: &str) -> String {
        self.state.note_engram_command_started(
            &self.session,
            &EngramObservationProvenance::Ambient,
            key,
            Some("cargo test"),
            Some(&self.writer.root.path().join("project").to_string_lossy()),
        );
        self.state
            .inner
            .lock()
            .unwrap()
            .test_run_observations
            .values()
            .find(|row| row.tool_identity == sha256_hex(key.as_bytes()))
            .unwrap()
            .execution_id
            .clone()
    }

    fn end(&self, key: &str) {
        self.state.observe_test_command_end(
            &self.session,
            &EngramObservationProvenance::Ambient,
            key,
            Some(EngramCommandExit::Code(0)),
        );
    }

    fn collect_pending(&mut self) -> PersistDelta {
        assert_eq!(
            self.writer.receive(false),
            PersistWorkerWaitOutcome::Process
        );
        assert!(!self.writer.batch.drain(&self.writer.rx, false));
        let delta = collect_persist_delta_from_shared_state(&self.state.inner, self.watermark);
        assert_eq!(self.writer.batch.pending.len(), 1);
        assert!(self.writer.batch.pending[0].target.is_in_delta(&delta));
        // Still live at its own clock's now, the clock its deadline runs on.
        let completion = &self.writer.batch.pending[0].completion;
        assert!(completion.poll_at(completion.clock.now()).is_none());
        delta
    }

    fn write_pending(&mut self) {
        let delta = self.collect_pending();
        self.writer.write(&delta).unwrap();
        self.watermark = delta.watermark;
        self.state.step_test_command_observation_persistence();
    }

    fn stored_observation(&self, id: &str) -> TestRunCommandObservation {
        let connection = self.writer.cache.connection.as_ref().unwrap();
        let encoded = read_persisted_metadata_json(connection).unwrap().unwrap();
        let persisted: PersistedState = serde_json::from_str(&encoded).unwrap();
        persisted.test_run_observations[id].clone()
    }
}

impl Drop for ObservationPublicationFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.mailbox.publish_delta(DeltaEvent::CodexUpdated {
            revision: BROADCAST_BARRIER_REVISION,
            codex: CodexState::default(),
        });
        if let Some(worker) = self.broadcaster.take() {
            worker.join().unwrap();
        }
    }
}

fn observation_delta(
    fixture: &ObservationPublicationFixture,
    events: &[SseStreamEvent],
    id: &str,
    expected: TestRunState,
    durability: &str,
) {
    let [SseStreamEvent::Delta(payload)] = events else {
        panic!("one bounded invalidation per observation transition: {events:?}");
    };
    let DeltaEvent::TestRunsInvalidated {
        overview,
        activity,
        server_instance_id,
        ..
    } = serde_json::from_str::<DeltaEvent>(payload).unwrap()
    else {
        panic!("unexpected observation delta: {payload}");
    };
    assert_eq!(server_instance_id, fixture.state.server_instance_id);
    let running = usize::from(expected == TestRunState::Running);
    assert_eq!(overview.get().counts.running, running);
    assert_eq!(
        activity
            .wire
            .get(&fixture.session)
            .map_or(0, |row| row.owned_running),
        running
    );
    if expected == TestRunState::Passed {
        assert!(overview.get().counts.passed > 0);
    }
    // The public event contains counters, not the exact row. Read it through
    // the production exact-ID owner and retain the durability/credit oracles.
    let mut diagnostics = TestRunRegistrationDiagnostics::new(&fixture.state.server_instance_id);
    let Json(detail) =
        get_test_run_observed(fixture.state.clone(), id, &mut diagnostics, true).unwrap();
    assert_eq!(detail.run.run_id, id);
    assert_eq!(detail.run.state, expected);
    assert_eq!(
        detail.run.registration.tracking_durability.as_deref(),
        Some(durability)
    );
    assert!(
        detail.run.registration.verification_record_ids.is_empty(),
        "visibility is not verification credit"
    );
}

fn assert_no_observation_snapshots(events: &[SseStreamEvent]) {
    let snapshots = events
        .iter()
        .filter(|event| matches!(event, SseStreamEvent::State(_)))
        .count();
    assert_eq!(
        snapshots, 0,
        "observation updates must not rebuild/broadcast global state"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, SseStreamEvent::Lagged))
    );
    let revisions: Vec<_> = events
        .iter()
        .map(|event| match event {
            SseStreamEvent::Delta(payload) => serde_json::from_str::<DeltaEvent>(payload)
                .unwrap()
                .revision(),
            _ => unreachable!(),
        })
        .collect();
    assert!(
        revisions.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "narrow-only publication must leave no revision gaps"
    );
}

#[test]
fn observation_persistence_tick_work_is_independent_of_terminal_history() {
    fn run_with_history(history_size: usize) -> (usize, usize) {
        let mut fixture = ObservationPublicationFixture::new();
        let keys = ["first-finisher", "second-finisher"];
        let finished = keys.map(|key| {
            let id = fixture.start(key);
            fixture.write_pending();
            id
        });
        let stale = fixture.start("stale-runtime");
        fixture.write_pending();
        for key in keys {
            fixture.end(key);
        }

        let history: Vec<_> = {
            let mut inner = fixture.state.inner.lock().unwrap();
            let template = inner.test_run_observations[&finished[0]].clone();
            (0..history_size)
                .map(|ordinal| {
                    let id = format!("observed-unrelated-history-{ordinal:04}");
                    let mut row = template.clone();
                    row.execution_id = id.clone();
                    row.summary.run_id = id.clone();
                    row.summary.owner_session_id = None;
                    row.runtime_identity = None;
                    row.validated_unbound = false;
                    row.final_unbound_proof = None;
                    inner.test_run_observations.insert(id.clone(), row.clone());
                    (id, row)
                })
                .collect()
        };
        fixture.flush();
        assert_eq!(
            fixture.writer.receive(false),
            PersistWorkerWaitOutcome::Process
        );
        assert!(!fixture.writer.batch.drain(&fixture.writer.rx, false));
        assert_eq!(fixture.writer.batch.pending.len(), finished.len());
        let delta =
            collect_persist_delta_from_shared_state(&fixture.state.inner, fixture.watermark);
        fixture.writer.write(&delta).unwrap();
        fixture.watermark = delta.watermark;
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.session).unwrap();
            inner.session_mut_by_index(index).unwrap().runtime = SessionRuntime::None;
        }

        TEST_TEST_RUN_FULL_PROJECTIONS.with(|count| count.set(0));
        TEST_TEST_RUN_IDENTITY_CANDIDATES.with(|count| count.set(0));
        fixture.state.step_test_command_observation_persistence();
        let work = (
            TEST_TEST_RUN_FULL_PROJECTIONS.with(|count| count.get()),
            TEST_TEST_RUN_IDENTITY_CANDIDATES.with(|count| count.get()),
        );

        let mut events = fixture.flush();
        assert!(!events.is_empty());
        fixture.write_pending();
        events.extend(fixture.flush());
        assert_no_observation_snapshots(&events);
        let persisted = load_state(fixture.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let inner = fixture.state.inner.lock().unwrap();
        for id in &finished {
            let row = &inner.test_run_observations[id];
            assert_eq!(row.summary.state, TestRunState::Passed);
            assert_eq!(row.summary.exit_code, Some(0));
            assert!(inner.test_runs.observations_durable.contains(id));
            assert_eq!(&persisted.test_run_observations[id], row);
        }
        let stale_row = &inner.test_run_observations[&stale];
        assert_eq!(stale_row.summary.state, TestRunState::Unknown);
        assert!(stale_row.summary.interrupted);
        assert!(stale_row.summary.ended_at.is_none());
        assert!(inner.test_runs.observations_durable.contains(&stale));
        assert_eq!(&persisted.test_run_observations[&stale], stale_row);
        for (id, row) in &history {
            assert_eq!(&inner.test_run_observations[id], row);
            assert_eq!(&persisted.test_run_observations[id], row);
        }
        work
    }

    let small = run_with_history(0);
    let large = run_with_history(512);
    assert_eq!(
        (small.0, large.0),
        (0, 0),
        "observation persistence must not project all terminal history"
    );
    assert_eq!(
        small.1, large.1,
        "unrelated terminal history must not increase changed-identity work"
    );
}

#[test]
fn observation_publication_repeated_start_end_preserves_pending_durable_and_stored_rows() {
    let mut fixture = ObservationPublicationFixture::new();
    // One initial full snapshot is legitimate. Subscribe before its enqueue
    // and consume it through the real serializer, not a per-kind test channel.
    fixture
        .mailbox
        .publish_snapshot(fixture.state.summary_snapshot());
    assert!(matches!(
        fixture.flush().as_slice(),
        [SseStreamEvent::State(_)]
    ));
    let mut all = Vec::new();
    for number in 0..3 {
        let key = format!("observed-command-{number}");
        let id = fixture.start(&key);
        let events = fixture.flush();
        observation_delta(&fixture, &events, &id, TestRunState::Running, "pending");
        all.extend(events);
        assert!(
            !fixture
                .state
                .inner
                .lock()
                .unwrap()
                .test_runs
                .observations_durable
                .contains(&id)
        );
        fixture.write_pending();
        let stored = fixture.stored_observation(&id);
        assert_eq!(stored.summary.state, TestRunState::Running);
        let events = fixture.flush();
        observation_delta(&fixture, &events, &id, TestRunState::Running, "durable");
        all.extend(events);

        fixture.end(&key);
        let events = fixture.flush();
        observation_delta(&fixture, &events, &id, TestRunState::Passed, "pending");
        all.extend(events);
        fixture.write_pending();
        let stored = fixture.stored_observation(&id);
        let inner = fixture.state.inner.lock().unwrap();
        assert_eq!(
            stored, inner.test_run_observations[&id],
            "exact terminal content reached SQLite before durable projection"
        );
        assert_eq!(stored.summary.exit_code, Some(0));
        drop(inner);
        let events = fixture.flush();
        observation_delta(&fixture, &events, &id, TestRunState::Passed, "durable");
        all.extend(events);
    }
    assert_eq!(
        all.iter()
            .filter(|event| matches!(event, SseStreamEvent::Delta(_)))
            .count(),
        12
    );
    assert_no_observation_snapshots(&all);
}

#[test]
fn observation_publication_failed_sqlite_write_remains_visible_without_durable_ack() {
    let mut fixture = ObservationPublicationFixture::new();
    let id = fixture.start("write-failure");
    let mut all = fixture.flush();
    observation_delta(&fixture, &all, &id, TestRunState::Running, "pending");
    let delta = fixture.collect_pending();
    // This fixture's actual cached SQLite connection refuses writes. The
    // production writer/fence finisher supplies the failure; no ACK is faked.
    fixture
        .writer
        .cache
        .connection
        .as_ref()
        .unwrap()
        .execute_batch("PRAGMA query_only = ON")
        .unwrap();
    assert!(fixture.writer.write(&delta).is_err());
    fixture.state.step_test_command_observation_persistence();
    let events = fixture.flush();
    observation_delta(&fixture, &events, &id, TestRunState::Running, "failed");
    all.extend(events);
    let inner = fixture.state.inner.lock().unwrap();
    assert!(inner.test_runs.observations_failed.contains(&id));
    assert!(!inner.test_runs.observations_durable.contains(&id));
    drop(inner);
    let loaded = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(
        !loaded.test_run_observations.contains_key(&id),
        "failed transaction cannot fabricate the stored observation"
    );
    assert_no_observation_snapshots(&all);
}

#[test]
fn observation_publication_fence_expires_on_its_clock_and_records_failed_tracking() {
    let mut fixture = ObservationPublicationFixture::new();
    let id = fixture.start("expired-fence");
    let mut all = fixture.flush();
    observation_delta(&fixture, &all, &id, TestRunState::Running, "pending");
    let delta = fixture.collect_pending();
    // A genuinely expired fence still fails. Its 5 s deadline runs on the
    // fixture's clock, so this advance, not host time, is what expires it.
    fixture.clock.advance(Duration::from_secs(5));
    fixture.writer.write(&delta).unwrap();
    fixture.watermark = delta.watermark;
    fixture.state.step_test_command_observation_persistence();
    let events = fixture.flush();
    observation_delta(&fixture, &events, &id, TestRunState::Running, "failed");
    all.extend(events);
    let inner = fixture.state.inner.lock().unwrap();
    assert!(inner.test_runs.observations_failed.contains(&id));
    assert!(!inner.test_runs.observations_durable.contains(&id));
    drop(inner);
    assert_no_observation_snapshots(&all);
}

#[test]
fn observation_publication_late_start_ack_cannot_ack_the_new_terminal_image() {
    let mut fixture = ObservationPublicationFixture::new();
    let id = fixture.start("late-start-ack");
    let mut all = fixture.flush();
    let start_delta = fixture.collect_pending();
    fixture.end("late-start-ack");
    let events = fixture.flush();
    observation_delta(&fixture, &events, &id, TestRunState::Passed, "pending");
    all.extend(events);
    fixture.writer.write(&start_delta).unwrap();
    fixture.watermark = start_delta.watermark;
    assert_eq!(
        fixture.stored_observation(&id).summary.state,
        TestRunState::Running
    );
    fixture.state.step_test_command_observation_persistence();
    assert!(
        fixture.flush().is_empty(),
        "obsolete ACK must not publish terminal durability"
    );
    assert!(
        !fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_runs
            .observations_durable
            .contains(&id)
    );
    fixture.write_pending();
    assert_eq!(
        fixture.stored_observation(&id).summary.state,
        TestRunState::Passed
    );
    let events = fixture.flush();
    observation_delta(&fixture, &events, &id, TestRunState::Passed, "durable");
    all.extend(events);
    assert_no_observation_snapshots(&all);
}

#[test]
fn observation_persistence_keeps_an_identical_replacement_fence_after_poll() {
    let mut fixture = ObservationPublicationFixture::new();
    let id = fixture.start("replace-polled-fence");
    fixture.flush();
    let delta = fixture.collect_pending();
    fixture.writer.write(&delta).unwrap();
    fixture.watermark = delta.watermark;

    fixture
        .state
        .step_test_command_observation_persistence_with_probe(|| {
            assert!(fixture.state.inner.is_not_held_by_current_thread_for_test());
            let mut inner = fixture.state.inner.lock().unwrap();
            let row = inner.test_run_observations[&id].clone();
            inner.test_run_observations.insert(id.clone(), row.clone());
            fixture
                .state
                .persist_test_command_observation_locked(&mut inner, row);
        });
    let events = fixture.flush();
    observation_delta(&fixture, &events, &id, TestRunState::Running, "pending");
    {
        let inner = fixture.state.inner.lock().unwrap();
        assert!(!inner.test_runs.observations_durable.contains(&id));
        assert!(
            inner
                .test_runs
                .observation_persistence
                .0
                .lock()
                .unwrap()
                .contains_key(&id)
        );
    }

    fixture.write_pending();
    let events = fixture.flush();
    observation_delta(&fixture, &events, &id, TestRunState::Running, "durable");
    let stored = fixture.stored_observation(&id);
    let inner = fixture.state.inner.lock().unwrap();
    assert!(inner.test_runs.observations_durable.contains(&id));
    assert_eq!(stored, inner.test_run_observations[&id]);
}

#[test]
fn observation_persistence_keeps_newer_synchronous_save_after_old_fence_fails() {
    for batch in [false, true] {
        let mut fixture = ObservationPublicationFixture::new();
        let id = fixture.start("replace-failed-fence-synchronously");
        fixture.flush();
        fixture.collect_pending();
        fixture.state.shutdown_persist_blocking();
        let (_, stopped_rx) = mpsc::channel();
        drop(std::mem::replace(&mut fixture.writer.rx, stopped_rx));
        // Abandon the writer's outstanding fence, then retry the exact row
        // after the tick observes WorkerStopped but before it applies it.
        drop(fixture.writer.batch.pending.pop().unwrap());
        fixture
            .state
            .step_test_command_observation_persistence_with_probe(|| {
                let mut inner = fixture.state.inner.lock().unwrap();
                let row = inner.test_run_observations[&id].clone();
                if batch {
                    fixture
                        .state
                        .persist_test_run_evidence_batch_deferring_compaction_locked(
                            &mut inner,
                            vec![row],
                            Vec::new(),
                        );
                } else {
                    fixture
                        .state
                        .persist_test_command_observation_locked(&mut inner, row);
                }
            });
        let events = fixture.flush();
        observation_delta(&fixture, &events, &id, TestRunState::Running, "durable");
        let stored = fixture.stored_observation(&id);
        let inner = fixture.state.inner.lock().unwrap();
        assert_eq!(stored, inner.test_run_observations[&id]);
    }
}

#[test]
fn observation_persistence_repairs_past_a_full_page_and_wraps_after_the_tail() {
    let mut fixture = ObservationPublicationFixture::new();
    let id = fixture.start("paging-template");
    fixture.write_pending();
    let template = fixture.state.inner.lock().unwrap().test_run_observations[&id].clone();
    assert!(template.runtime_identity.is_some());
    fixture.end("paging-template");
    fixture.write_pending();
    fixture.flush();

    let key = |ordinal| format!("paging-observation-{ordinal:03}");
    let seed = |inner: &mut StateInner, ordinal, stale| {
        let id = key(ordinal);
        let mut row = template.clone();
        row.execution_id = id.clone();
        row.summary.run_id = id.clone();
        if stale {
            row.runtime_identity = None;
        }
        inner.test_run_observations.insert(id, row);
    };
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        for ordinal in 1..=64 {
            seed(&mut inner, ordinal, ordinal > 32);
        }
    }
    let runs_in_state = |ordinals: std::ops::RangeInclusive<usize>, expected| {
        let inner = fixture.state.inner.lock().unwrap();
        for ordinal in ordinals {
            assert_eq!(
                inner.test_run_observations[&key(ordinal)].summary.state,
                expected
            );
        }
    };
    fixture.state.step_test_command_observation_persistence();
    runs_in_state(1..=64, TestRunState::Running);
    // The first page remains eligible. The next tick must still reach the
    // second, exactly full page instead of repeating the first.
    fixture.state.step_test_command_observation_persistence();
    runs_in_state(1..=32, TestRunState::Running);
    runs_in_state(33..=64, TestRunState::Unknown);
    seed(&mut fixture.state.inner.lock().unwrap(), 0, true);
    // The cursor has passed the new row. Give it the empty tail page and
    // the wrapped page, then verify the repair through the live row.
    fixture.state.step_test_command_observation_persistence();
    fixture.state.step_test_command_observation_persistence();
    runs_in_state(0..=0, TestRunState::Unknown);
}

#[test]
fn observation_publication_disconnected_writer_keeps_synchronous_durability() {
    let mut fixture = ObservationPublicationFixture::new();
    fixture.state.shutdown_persist_blocking();
    // This fixture drives its receiver directly; release it after the shutdown
    // fence rather than replacing a sender that could leave a worker running.
    let (_, stopped_rx) = mpsc::channel();
    drop(std::mem::replace(&mut fixture.writer.rx, stopped_rx));
    assert!(
        fixture
            .state
            .persist_tx
            .send(PersistRequest::Delta)
            .is_err()
    );
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner.settings_persist_dirty = true;
        inner.remote_settings_persist_dirty = true;
        inner.remote_delta_persist_dirty = true;
    }
    let id = fixture.start("synchronous-observation");
    let mut all = fixture.flush();
    observation_delta(&fixture, &all, &id, TestRunState::Running, "durable");
    fixture.end("synchronous-observation");
    let events = fixture.flush();
    observation_delta(&fixture, &events, &id, TestRunState::Passed, "durable");
    all.extend(events);
    let loaded = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        loaded.test_run_observations[&id].summary.state,
        TestRunState::Passed
    );
    assert_eq!(loaded.test_run_observations[&id].summary.exit_code, Some(0));
    let inner = fixture.state.inner.lock().unwrap();
    assert!(!inner.settings_persist_dirty);
    assert!(!inner.remote_settings_persist_dirty);
    assert!(!inner.remote_delta_persist_dirty);
    drop(inner);
    assert_no_observation_snapshots(&all);
}

#[test]
fn observation_publication_synchronous_write_failure_uses_the_commits_revision() {
    let mut fixture = ObservationPublicationFixture::new();
    let unavailable = fixture.writer.root.path().join("not-a-database");
    fs::create_dir(&unavailable).unwrap();
    fixture.state.persistence_path = Arc::new(unavailable);
    fixture.state.shutdown_persist_blocking();
    let (_, stopped_rx) = mpsc::channel();
    drop(std::mem::replace(&mut fixture.writer.rx, stopped_rx));
    assert!(
        fixture
            .state
            .persist_tx
            .send(PersistRequest::Delta)
            .is_err()
    );
    let revision = fixture.state.inner.lock().unwrap().revision;
    let id = fixture.start("synchronous-write-failure");
    let events = fixture.flush();
    observation_delta(&fixture, &events, &id, TestRunState::Running, "failed");
    let [SseStreamEvent::Delta(payload)] = events.as_slice() else {
        panic!("failed persistence must leave one narrow factual update");
    };
    assert_eq!(
        serde_json::from_str::<DeltaEvent>(payload)
            .unwrap()
            .revision(),
        revision + 1,
        "failed persistence must not leave an unpublished revision"
    );
    let inner = fixture.state.inner.lock().unwrap();
    assert!(inner.test_runs.observations_failed.contains(&id));
    assert!(!inner.test_runs.observations_durable.contains(&id));
    drop(inner);
    assert_no_observation_snapshots(&events);
}

#[test]
fn observation_publication_idle_poll_is_silent_and_structural_commit_still_publishes() {
    let mut fixture = ObservationPublicationFixture::new();
    let revision = fixture.state.inner.lock().unwrap().revision;
    for _ in 0..3 {
        fixture.state.step_test_command_observation_persistence();
    }
    assert!(fixture.flush().is_empty());
    assert_eq!(fixture.state.inner.lock().unwrap().revision, revision);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        inner.session_mut_by_index(index).unwrap().session.name =
            "Changed structural metadata".to_owned();
        fixture.state.commit_locked(&mut inner).unwrap();
    }
    let events = fixture.flush();
    let [SseStreamEvent::State(payload)] = events.as_slice() else {
        panic!("structural commit must still publish exactly one full state: {events:?}");
    };
    let snapshot: StateResponse = serde_json::from_str(payload).unwrap();
    assert_eq!(snapshot.revision, revision + 1);
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == fixture.session)
            .unwrap()
            .name,
        "Changed structural metadata"
    );
}
