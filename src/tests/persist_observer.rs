// Passive writer telemetry regressions. Owns bounded observations and actual
// SQLite/fence/HTTP integration checks, not performance thresholds or retries.
use super::*;

fn finish_at(observer: &Arc<PersistObserver>, base: std::time::Instant, success: bool) {
    let mut scope = observer.begin_at(base);
    observer.finish_at(
        success,
        &PersistTickTimings::default(),
        base + Duration::from_micros(50),
    );
    scope.finished = true;
}

#[test]
fn recent_history_is_bounded_and_failed_ticks_are_counted() {
    let observer = Arc::new(PersistObserver::default());
    let base = std::time::Instant::now();
    for tick in 0..40 {
        finish_at(
            &observer,
            base + Duration::from_micros(tick * 100),
            tick % 2 == 0,
        );
    }
    let snapshot = observer.snapshot_at(base);
    assert_eq!(snapshot.completed_ticks, 40);
    assert_eq!(snapshot.failed_ticks, 20);
    assert_eq!(snapshot.recent.len(), 16);
    assert_eq!(snapshot.recent[0].tick, 25);
    assert_eq!(snapshot.recent[15].tick, 40);
    assert!(snapshot.recent.iter().all(|tick| tick.loop_wall_us == 50));
    assert!(snapshot.current.is_none());
    assert!(snapshot.channel_queue_depth.is_none());
}

#[test]
fn live_phase_and_age_remain_available_before_a_tick_completes() {
    let observer = Arc::new(PersistObserver::default());
    let base = std::time::Instant::now();
    let scope = observer.begin_at(base);
    persist_observe_phase("commit");
    observer.update(|tick| tick.counts.metadata_bytes = 123);
    let snapshot = observer.snapshot_at(base + Duration::from_micros(10_000));
    let current = snapshot.current.unwrap();
    assert_eq!(current.phase, "commit");
    assert_eq!(current.age_us, 10_000);
    assert!(current.phase_age_us <= current.age_us);
    assert_eq!(current.sample.counts.metadata_bytes, 123);
    assert_eq!(snapshot.completed_ticks, 0);
    scope.finish(true, &PersistTickTimings::default());
}

#[test]
fn collection_wait_and_hold_are_separate_and_accumulate() {
    let observer = Arc::new(PersistObserver::default());
    let scope = observer.begin_at(std::time::Instant::now());
    observer.collection_lock(Duration::from_micros(13), Duration::from_micros(71));
    observer.collection_lock(Duration::from_micros(5), Duration::from_micros(17));
    let phases = &observer
        .snapshot_at(std::time::Instant::now())
        .current
        .unwrap()
        .sample
        .phases;
    assert_eq!(phases.collect_wait_us, 18);
    assert_eq!(phases.collect_hold_us, 88);
    assert_eq!(phases.collect_lock_count, 2);
    scope.finish(true, &PersistTickTimings::default());
}

#[test]
fn all_existing_phase_durations_are_preserved_in_microseconds() {
    let observer = Arc::new(PersistObserver::default());
    let scope = observer.begin_at(std::time::Instant::now());
    let timings = PersistTickTimings {
        collect: Duration::from_micros(1),
        serialize: Duration::from_micros(2),
        open: Duration::from_micros(3),
        ticket_wait: Duration::from_micros(4),
        statements: Duration::from_micros(5),
        commit: Duration::from_micros(6),
        post_commit: Duration::from_micros(7),
        acknowledge: Duration::from_micros(8),
    };
    scope.finish(true, &timings);
    let sample = observer
        .snapshot_at(std::time::Instant::now())
        .recent
        .pop()
        .unwrap();
    assert_eq!(
        [
            sample.phases.collect_us,
            sample.phases.serialize_us,
            sample.phases.open_us,
            sample.phases.ticket_wait_us,
            sample.phases.statements_us,
            sample.phases.commit_us,
            sample.phases.post_commit_us,
            sample.phases.acknowledge_us
        ],
        [1, 2, 3, 4, 5, 6, 7, 8]
    );
}

#[test]
fn scope_is_thread_local_and_restores_the_previous_observer() {
    let outer = Arc::new(PersistObserver::default());
    let inner = Arc::new(PersistObserver::default());
    let outer_scope = outer.begin_at(std::time::Instant::now());
    {
        let inner_scope = inner.begin_at(std::time::Instant::now());
        persist_observe_phase("serialize");
        inner_scope.finish(true, &PersistTickTimings::default());
    }
    std::thread::spawn(|| persist_observe_phase("foreign_thread"))
        .join()
        .unwrap();
    persist_observe_phase("open");
    assert_eq!(
        outer
            .snapshot_at(std::time::Instant::now())
            .current
            .unwrap()
            .phase,
        "open"
    );
    assert_eq!(
        inner.snapshot_at(std::time::Instant::now()).completed_ticks,
        1
    );
    outer_scope.finish(true, &PersistTickTimings::default());
    PERSIST_OBSERVATION.with(|slot| assert!(slot.borrow().is_none()));
}

#[test]
fn abandoned_scope_records_failure_and_clears_live_state() {
    let observer = Arc::new(PersistObserver::default());
    {
        let _scope = observer.begin_at(std::time::Instant::now());
        persist_observe_phase("serialize");
    }
    let snapshot = observer.snapshot_at(std::time::Instant::now());
    assert_eq!(snapshot.failed_ticks, 1);
    assert!(snapshot.current.is_none());
    PERSIST_OBSERVATION.with(|slot| assert!(slot.borrow().is_none()));
}

#[test]
fn drain_counts_and_fence_age_do_not_change_shutdown_or_expiry() {
    let now = std::time::Instant::now();
    let (mut fence, waiter) =
        PersistFence::new(PersistFenceTarget::TestRunCardsEpoch("e".into()), now);
    fence.observed_created_at = now - Duration::from_micros(500);
    let (tx, rx) = mpsc::channel();
    tx.send(PersistRequest::Delta).unwrap();
    tx.send(PersistRequest::Fence(Box::new(fence))).unwrap();
    tx.send(PersistRequest::Shutdown).unwrap();
    let mut batch = PersistFenceBatch::default();
    assert_eq!(batch.drain_observed(&rx, false), (true, 3));
    let (accepted, age) = batch.take_observer_acceptance();
    assert_eq!(accepted, 1);
    assert!(age >= 500);
    assert_eq!(batch.take_observer_acceptance(), (0, 0));
    assert_eq!(
        waiter.wait_until(now),
        Some(Err(PersistFenceError::Deadline))
    );
    batch.expire_resolved();
    assert!(!batch.has_pending());
}

#[test]
fn real_sql_write_reports_bytes_without_leaking_content_or_changing_ack() {
    let state = test_app_state();
    let id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        inner.sessions[index].session.preview = "PRIVATE-PREVIEW-🦀".into();
        let record = &mut inner.sessions[index];
        push_message_on_record(
            record,
            Message::Text {
                id: "private-message-id".into(),
                timestamp: "now".into(),
                author: Author::Assistant,
                text: "PRIVATE-TRANSCRIPT-界".into(),
                expanded_text: None,
                source: None,
                attachments: vec![],
            },
        );
        inner.test_run_cards_epoch = Some("private-epoch".into());
    }
    let observer = Arc::new(PersistObserver::default());
    let scope = observer.begin_at(std::time::Instant::now());
    persist_observe_phase("collect");
    let started = std::time::Instant::now();
    let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
    let mut timings = PersistTickTimings {
        collect: started.elapsed(),
        ..Default::default()
    };
    persist_observe_delta(&delta);
    let (fence, waiter) = PersistFence::new(
        PersistFenceTarget::TestRunCardsEpoch("private-epoch".into()),
        std::time::Instant::now() + TEST_PHASE_DEADLOCK_GUARD,
    );
    let lock = Arc::clone(&state.inner);
    let fence = fence.on_acknowledged(move || assert!(lock.try_lock_now().is_some()));
    let mut batch = PersistFenceBatch::default();
    batch.accept(PersistRequest::Fence(Box::new(fence)));
    let mut cache = SqlitePersistConnectionCache::new();
    persist_delta_with_fences_timed(
        &mut cache,
        state.persistence_path.as_path(),
        &delta,
        &mut batch,
        &mut timings,
    )
    .unwrap();
    assert_eq!(waiter.wait(), Ok(()));
    scope.finish(true, &timings);
    let snapshot = observer.snapshot_at(std::time::Instant::now());
    let sample = &snapshot.recent[0];
    let connection = cache.connection.as_ref().unwrap();
    let metadata_bytes: usize = connection
        .query_row(
            "SELECT length(CAST(value_json AS BLOB)) FROM app_state WHERE key=?1",
            [SQLITE_METADATA_KEY],
            |row| row.get(0),
        )
        .unwrap();
    let message_bytes: usize = connection.query_row("SELECT length(CAST(value_json AS BLOB)) FROM messages WHERE session_id=?1 AND message_id=?2", [id.as_str(), "private-message-id"], |row| row.get(0)).unwrap();
    assert!(sample.success);
    assert!(sample.counts.payload_measured);
    assert_eq!(sample.counts.metadata_bytes, metadata_bytes);
    assert_eq!(sample.counts.message_bytes, message_bytes);
    assert_eq!(sample.counts.retained_messages, 1);
    assert_eq!(sample.counts.changed_sessions, 1);
    assert!(sample.phases.collect_lock_count >= 2);
    let encoded = serde_json::to_string(&snapshot).unwrap();
    for secret in [
        "PRIVATE-PREVIEW",
        "PRIVATE-TRANSCRIPT",
        "private-message-id",
        "private-epoch",
        &id,
    ] {
        assert!(!encoded.contains(secret), "observer leaked content");
    }
}

#[test]
fn failed_sql_write_still_records_observations_and_preserves_the_error() {
    let root = TestTempRoot::create("persist-observer-failure");
    let blocker = root.path().join("file-not-directory");
    fs::write(&blocker, "block").unwrap();
    let state = test_app_state();
    let observer = Arc::new(PersistObserver::default());
    let scope = observer.begin_at(std::time::Instant::now());
    let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
    let mut cache = SqlitePersistConnectionCache::new();
    let mut timings = PersistTickTimings::default();
    let result = persist_delta_via_cache_timed(
        &mut cache,
        &blocker.join("state.sqlite"),
        &delta,
        &mut timings,
    );
    assert!(result.is_err());
    assert!(cache.connection.is_none());
    scope.finish(result.is_ok(), &timings);
    let snapshot = observer.snapshot_at(std::time::Instant::now());
    assert_eq!(snapshot.failed_ticks, 1);
    assert!(!snapshot.recent[0].success);
    assert!(snapshot.recent[0].counts.payload_measured);
}

#[tokio::test]
async fn diagnostics_http_is_bounded_read_only_and_does_not_enqueue_work() {
    let mut state = test_app_state();
    let (tx, rx) = mpsc::channel();
    state.persist_tx = tx;
    let revision = state.inner.lock().unwrap().revision;
    let app = app_router(state.clone());
    for _ in 0..2 {
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .uri("/api/diagnostics/persistence")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["serverInstanceId"], state.server_instance_id);
        assert_eq!(body["writer"]["version"], 1);
        assert_eq!(body["writer"]["recentLimit"], 16);
        assert_eq!(body["writer"]["completedTicks"], 0);
        assert!(body["writer"]["channelQueueDepth"].is_null());
        assert!(body["writer"]["current"].is_null());
    }
    assert_eq!(state.inner.lock().unwrap().revision, revision);
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/diagnostics/persistence")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[test]
fn diagnostics_http_completes_while_state_inner_is_held_elsewhere() {
    let state = test_app_state();
    let app = app_router(state.clone());
    let held = state.inner.lock().unwrap();
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result: (StatusCode, Value) = runtime.block_on(request_json(
            &app,
            Request::builder()
                .uri("/api/diagnostics/persistence")
                .body(Body::empty())
                .unwrap(),
        ));
        tx.send(result).unwrap();
    });
    // This is the suite's existing deadlock guard, not a performance bound.
    // Release the mutex before joining even if a future regression blocks GET.
    let result = rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD);
    drop(held);
    worker.join().unwrap();
    assert_eq!(
        result.expect("diagnostics must not acquire StateInner").0,
        StatusCode::OK
    );
}
