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
}

impl ObservationPublicationFixture {
    fn new() -> Self {
        let mut writer = FenceFixture::new();
        let mut state = test_app_state();
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
        state.persistence_path = Arc::new(writer.root.path().join("termal.sqlite"));
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
            .spawn(move || loop {
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
        assert_eq!(self.writer.receive(false), PersistWorkerWaitOutcome::Process);
        assert!(!self.writer.batch.drain(&self.writer.rx, false));
        let delta = collect_persist_delta_from_shared_state(&self.state.inner, self.watermark);
        assert_eq!(self.writer.batch.pending.len(), 1);
        assert!(self.writer.batch.pending[0].target.is_in_delta(&delta));
        assert!(self.writer.batch.pending[0]
            .completion
            .poll_at(std::time::Instant::now())
            .is_none());
        delta
    }

    fn write_pending(&mut self) {
        let delta = self.collect_pending();
        self.writer.write(&delta).unwrap();
        self.watermark = delta.watermark;
        self.state.step_test_command_observation_persistence();
    }

    fn stored_observation(&self, id: &str) -> TestRunCommandObservation {
        let encoded: String = self
            .writer
            .cache
            .connection
            .as_ref()
            .unwrap()
            .query_row(
                "SELECT value_json FROM app_state WHERE key = 'metadataState'",
                [],
                |row| row.get(0),
            )
            .unwrap();
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

fn observation_delta(events: &[SseStreamEvent], id: &str, expected: TestRunState, durability: &str) {
    let runs: Vec<_> = events
        .iter()
        .filter_map(|event| {
            let SseStreamEvent::Delta(payload) = event else {
                return None;
            };
            match serde_json::from_str::<DeltaEvent>(payload).unwrap() {
                DeltaEvent::TestRunChanged { run, .. } => Some(run),
                _ => panic!("unexpected observation delta: {payload}"),
            }
        })
        .collect();
    assert_eq!(
        runs.len(),
        1,
        "one exact changed run per observation transition"
    );
    assert_eq!(runs[0].run_id, id);
    assert_eq!(runs[0].state, expected);
    assert_eq!(
        runs[0].registration.tracking_durability.as_deref(),
        Some(durability)
    );
    assert!(
        runs[0].registration.verification_record_ids.is_empty(),
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
    assert!(!events
        .iter()
        .any(|event| matches!(event, SseStreamEvent::Lagged)));
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
        observation_delta(&events, &id, TestRunState::Running, "pending");
        all.extend(events);
        assert!(!fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_runs
            .observations_durable
            .contains(&id));
        fixture.write_pending();
        let stored = fixture.stored_observation(&id);
        assert_eq!(stored.summary.state, TestRunState::Running);
        let events = fixture.flush();
        observation_delta(&events, &id, TestRunState::Running, "durable");
        all.extend(events);

        fixture.end(&key);
        let events = fixture.flush();
        observation_delta(&events, &id, TestRunState::Passed, "pending");
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
        observation_delta(&events, &id, TestRunState::Passed, "durable");
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
    observation_delta(&all, &id, TestRunState::Running, "pending");
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
    observation_delta(&events, &id, TestRunState::Running, "failed");
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
fn observation_publication_late_start_ack_cannot_ack_the_new_terminal_image() {
    let mut fixture = ObservationPublicationFixture::new();
    let id = fixture.start("late-start-ack");
    let mut all = fixture.flush();
    let start_delta = fixture.collect_pending();
    fixture.end("late-start-ack");
    let events = fixture.flush();
    observation_delta(&events, &id, TestRunState::Passed, "pending");
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
    assert!(!fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_runs
        .observations_durable
        .contains(&id));
    fixture.write_pending();
    assert_eq!(
        fixture.stored_observation(&id).summary.state,
        TestRunState::Passed
    );
    let events = fixture.flush();
    observation_delta(&events, &id, TestRunState::Passed, "durable");
    all.extend(events);
    assert_no_observation_snapshots(&all);
}

#[test]
fn observation_publication_disconnected_writer_keeps_synchronous_durability() {
    let mut fixture = ObservationPublicationFixture::new();
    fixture.state.shutdown_persist_blocking();
    // This fixture drives its receiver directly; release it after the shutdown
    // fence rather than replacing a sender that could leave a worker running.
    let (_, stopped_rx) = mpsc::channel();
    drop(std::mem::replace(&mut fixture.writer.rx, stopped_rx));
    assert!(fixture.state.persist_tx.send(PersistRequest::Delta).is_err());
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner.settings_persist_dirty = true;
        inner.remote_settings_persist_dirty = true;
        inner.remote_delta_persist_dirty = true;
    }
    let id = fixture.start("synchronous-observation");
    let mut all = fixture.flush();
    observation_delta(&all, &id, TestRunState::Running, "durable");
    fixture.end("synchronous-observation");
    let events = fixture.flush();
    observation_delta(&events, &id, TestRunState::Passed, "durable");
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
    assert!(fixture.state.persist_tx.send(PersistRequest::Delta).is_err());
    let revision = fixture.state.inner.lock().unwrap().revision;
    let id = fixture.start("synchronous-write-failure");
    let events = fixture.flush();
    observation_delta(&events, &id, TestRunState::Running, "failed");
    let [SseStreamEvent::Delta(payload)] = events.as_slice() else {
        panic!("failed persistence must leave one narrow factual update");
    };
    assert_eq!(
        serde_json::from_str::<DeltaEvent>(payload).unwrap().revision(),
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
