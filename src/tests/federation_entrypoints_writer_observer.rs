// Controlled fixture phases validate the observer, not the cause of any
// historical deadline. The held lock belongs only to this disposable app.
use super::*;

fn with_writer(check: impl FnOnce(&AppState, &federation_entrypoints_writer::WriterControl)) {
    let mut app = test_app_state();
    let control = federation_entrypoints_writer::start_writer(&mut app);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&app, &control)));
    // In particular, an assertion must release its StateInner guard before
    // joining the writer. Do not leave a failed discriminator's thread behind.
    app.shutdown_persist_blocking();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    let snapshot = app.persist_observer.snapshot_at(std::time::Instant::now());
    assert!(snapshot.completed_ticks > 0);
    assert_eq!(snapshot.failed_ticks, 0);
    assert!(
        snapshot.current.is_none(),
        "shutdown must finish its observation"
    );
}

fn confirm_exact_sqlite_content(app: &AppState, epoch: &str) {
    let target = PersistFenceTarget::TestRunCardsEpoch(epoch.into());
    let (fence, waiter) = PersistFence::new(
        target.clone(),
        std::time::Instant::now() + Duration::from_secs(5),
    );
    app.persist_tx
        .send(PersistRequest::Fence(Box::new(fence)))
        .unwrap();
    assert_eq!(
        waiter.wait(),
        Ok(()),
        "real-clock five-second fence must acknowledge"
    );
    let connection = rusqlite::Connection::open(app.persistence_path.as_path()).unwrap();
    // Epoch fences acknowledge the complete committed delta; their late-row
    // probe deliberately returns false. Read the actual SQLite units instead.
    let stored = read_persisted_metadata_field(&connection, "testRunCardsEpoch").unwrap();
    assert_eq!(stored, Some(json!(epoch)));
    assert_ne!(stored, Some(json!("different-content")));
}

#[test]
fn fixture_writer_observer_distinguishes_queued_fence_behind_collection() {
    with_writer(|app, control| {
        let mut held = app.inner.lock().unwrap();
        held.test_run_cards_epoch = Some("queued-control".into());
        let collecting = control.observe_next_collection();
        app.persist_tx.send(PersistRequest::Delta).unwrap();
        phase_sync::receive(&collecting, "preceding delta reached collection");

        let clock = EngramBudgetClock::scripted();
        let deadline = clock.now() + Duration::from_secs(5);
        let (fence, waiter) = PersistFence::new_with_clock(
            PersistFenceTarget::TestRunCardsEpoch("queued-control".into()),
            deadline,
            clock.clone(),
        );
        app.persist_tx
            .send(PersistRequest::Fence(Box::new(fence)))
            .unwrap();
        let phase_before = waiter.progress().phase;
        let snapshot = app.persist_observer.snapshot_at(std::time::Instant::now());
        clock.advance(Duration::from_secs(5));
        let expired = waiter.wait_until(deadline);
        let phase_after = waiter.progress().phase;
        drop(held);

        eprintln!(
            "FIXTURE_WRITER_QUEUED_CONTROL {}",
            serde_json::to_string(&snapshot).unwrap()
        );
        confirm_exact_sqlite_content(app, "queued-control");
        assert_eq!(phase_before, PersistFencePhase::Queued);
        assert_eq!(expired, Some(Err(PersistFenceError::Deadline)));
        assert_eq!(phase_after, PersistFencePhase::Queued);
        let current = snapshot
            .current
            .expect("fixture must expose its preceding tick");
        assert_eq!(current.phase, "collect");
        assert_eq!(current.sample.counts.accepted_fences, 0);
        assert_eq!(current.sample.counts.pending_fences_before, 0);
        let snapshot = app.persist_observer.snapshot_at(std::time::Instant::now());
        assert!(
            snapshot
                .recent
                .iter()
                .any(|tick| tick.phases.collect_lock_count > 0)
        );
    });
}

#[test]
fn fixture_writer_observer_distinguishes_fence_in_its_own_collection() {
    with_writer(|app, control| {
        let mut held = app.inner.lock().unwrap();
        held.test_run_cards_epoch = Some("in-tick-control".into());
        let collecting = control.observe_next_collection();
        let clock = EngramBudgetClock::scripted();
        let deadline = clock.now() + Duration::from_secs(5);
        let (fence, waiter) = PersistFence::new_with_clock(
            PersistFenceTarget::TestRunCardsEpoch("in-tick-control".into()),
            deadline,
            clock.clone(),
        );
        app.persist_tx
            .send(PersistRequest::Fence(Box::new(fence)))
            .unwrap();
        phase_sync::receive(&collecting, "accepted fence reached collection");
        let phase_before = waiter.progress().phase;
        let snapshot = app.persist_observer.snapshot_at(std::time::Instant::now());
        clock.advance(Duration::from_secs(5));
        let expired = waiter.wait_until(deadline);
        let phase_after = waiter.progress().phase;
        drop(held);

        eprintln!(
            "FIXTURE_WRITER_IN_TICK_CONTROL {}",
            serde_json::to_string(&snapshot).unwrap()
        );
        confirm_exact_sqlite_content(app, "in-tick-control");
        assert_eq!(
            phase_before,
            PersistFencePhase::InTick,
            "collection belongs to the accepted fence's tick"
        );
        assert_eq!(expired, Some(Err(PersistFenceError::Deadline)));
        assert_eq!(phase_after, PersistFencePhase::InTick);
        let current = snapshot.current.expect("fixture must expose its own tick");
        assert_eq!(current.phase, "collect");
        assert_eq!(current.sample.counts.accepted_fences, 1);
        assert_eq!(current.sample.counts.pending_fences_before, 1);
    });
}
