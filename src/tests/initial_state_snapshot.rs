//! Initial route snapshots must hand an exact cut out of StateInner before
//! materializing the response. The pause observes real response construction.

use super::*;

struct ReleaseMaterialization(Option<mpsc::Sender<()>>);

impl Drop for ReleaseMaterialization {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            let _ = release.send(());
        }
    }
}

async fn initial_snapshot_releases_state_before_materialization(sse: bool, lagged: bool) {
    let (mut state, session, _) = cached_state();
    state.state_broadcast_senders.stream_events = broadcast::channel(16).0;
    state.mailbox_store = Arc::new(
        MailboxStore::open(&resolve_coordination_persistence_path(
            &state.persistence_path,
        ))
        .unwrap(),
    );
    let expected = serde_json::to_value(state.summary_snapshot()).unwrap();
    let app = app_router(state.clone());
    let lagged_body = if lagged {
        assert!(sse);
        let response = request_response(
            &app,
            Request::get("/api/events").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = Box::pin(response.into_body().into_data_stream());
        let (kind, _) = parse_sse_event(&next_sse_event(&mut body).await);
        assert_eq!(kind, "state");
        // Exercise the real route's lag branch, using the same ordered
        // broadcast transport as publication. No receiver polls during fill.
        let payload = Arc::<str>::from(serde_json::to_string(&expected).unwrap());
        for _ in 0..64 {
            state
                .state_broadcast_senders
                .stream_events
                .send(SseStreamEvent::State(payload.clone()))
                .unwrap();
        }
        Some(body)
    } else {
        None
    };
    let (entered, at_materialization) = tokio::sync::oneshot::channel();
    let (release, released) = mpsc::channel();
    let release = ReleaseMaterialization(Some(release));
    let inner = Arc::downgrade(&state.inner);
    state
        .inner
        .lock()
        .unwrap()
        .state_projection_cache
        .lock()
        .unwrap()
        .before_materialize = Some(Box::new(move || {
        let lock_released = inner
            .upgrade()
            .unwrap()
            .is_not_held_by_current_thread_for_test();
        entered.send(lock_released).unwrap();
        released.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
    }));
    let request_app = app.clone();
    let snapshot = tokio::spawn(async move {
        if let Some(mut body) = lagged_body {
            // Recovery cannot yield its first frame until the deliberate
            // materialization pause is released. Bound that whole phase by
            // the existing deadlock guard, not the ordinary event timer.
            let frame = next_sse_event_with_timeout(&mut body, phase_sync::DEADLOCK_GUARD).await;
            let (kind, payload) = parse_sse_event(&frame);
            assert_eq!((kind.as_str(), payload.as_str()), ("lagged", "1"));
            let (kind, payload) = parse_sse_event(&next_sse_event(&mut body).await);
            assert_eq!(kind, "state");
            return serde_json::from_str::<Value>(&payload).unwrap();
        }
        let response = request_response(
            &request_app,
            Request::get(if sse { "/api/events" } else { "/api/state" })
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        if sse {
            let mut body = Box::pin(response.into_body().into_data_stream());
            let frame = next_sse_event(&mut body).await;
            let (kind, payload) = parse_sse_event(&frame);
            assert_eq!(kind, "state");
            serde_json::from_str::<Value>(&payload).unwrap()
        } else {
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            serde_json::from_slice::<Value>(&body).unwrap()
        }
    });
    let lock_released = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, at_materialization)
        .await
        .unwrap()
        .unwrap();
    if !lock_released {
        // Release and join the original witness before asserting, so a RED
        // never leaves a route blocked on a test-owned barrier.
        drop(release);
        snapshot.await.unwrap();
        assert!(
            lock_released,
            "initial snapshot materialization still holds StateInner"
        );
        return;
    }

    let mailbox_state = state.clone();
    let mailbox_session = session.clone();
    tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        tokio::task::spawn_blocking(move || {
            mailbox_state.ensure_mailbox_session_active(&mailbox_session)
        }),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    let body = serde_json::to_vec(&json!({"controlPanelSide": "left", "workspace": {"panes": []}}))
        .unwrap();
    let (status, saved): (StatusCode, WorkspaceLayoutResponse) = tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        request_json(
            &app,
            Request::put("/api/workspaces/while-snapshot-paused")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved.layout.revision, 1);
    // Synchronous fallback uses real SQLite. A successful HTTP response is
    // checked against the durable row, rather than treated as a queued ACK.
    let durable = load_state(state.persistence_path.as_ref())
        .unwrap()
        .unwrap();
    assert!(
        durable
            .workspace_layouts
            .contains_key("while-snapshot-paused")
    );
    {
        let mut inner = state.inner.lock().unwrap();
        let record = inner
            .sessions
            .iter_mut()
            .find(|record| record.session.id == session)
            .unwrap();
        record.session.name = "changed after capture".to_owned();
        record.orchestrator_auto_dispatch_blocked = true;
        state.commit_locked(&mut inner).unwrap();
        assert!(inner.revision > expected["revision"].as_u64().unwrap());
    }
    assert!(
        !snapshot.is_finished(),
        "the route must remain paused during unrelated work"
    );
    drop(release);
    let captured = snapshot.await.unwrap();
    assert_eq!(
        captured, expected,
        "concurrent metadata and workspace writes cannot alter the captured cut"
    );
    let current = serde_json::to_value(state.summary_snapshot()).unwrap();
    assert_ne!(current, captured);
    assert!(
        current["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|workspace| workspace["id"] == "while-snapshot-paused")
    );
    let changed = current["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == session)
        .unwrap();
    assert_eq!(changed["name"], "changed after capture");
    assert_eq!(changed["queuePaused"], true);
}

#[tokio::test]
async fn initial_get_snapshot_materialization_does_not_hold_state() {
    initial_snapshot_releases_state_before_materialization(false, false).await;
}

#[tokio::test]
async fn initial_sse_snapshot_materialization_does_not_hold_state() {
    initial_snapshot_releases_state_before_materialization(true, false).await;
}

#[tokio::test]
async fn lagged_sse_snapshot_materialization_does_not_hold_state() {
    initial_snapshot_releases_state_before_materialization(true, true).await;
}

#[test]
fn explicit_summary_snapshot_reuses_and_revalidates_the_existing_cache() {
    let (state, session, _) = cached_state();
    let original_key = {
        let inner = state.inner.lock().unwrap();
        StateProjectionKey::for_record(
            inner
                .sessions
                .iter()
                .find(|row| row.session.id == session)
                .unwrap(),
        )
    };
    let count = Arc::new(AtomicUsize::new(0));
    let observer = SummaryObserver::install(count.clone());
    let before = state.summary_snapshot();
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "warm explicit reads reuse summary Arcs"
    );
    {
        let mut inner = state.inner.lock().unwrap();
        // Deliberately bypass the stamp: the existing exhaustive validator,
        // rather than revision/stamp equality, owns this safety check.
        let record = inner
            .sessions
            .iter_mut()
            .find(|record| record.session.id == session)
            .unwrap();
        record.session.name = "unstamped explicit read".to_owned();
        assert!(StateProjectionKey::for_record(record) == original_key);
    }
    let after = state.summary_snapshot();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    {
        let mut inner = state.inner.lock().unwrap();
        // Pause is independently keyed. Keep this separate so it cannot
        // mask the name-only comparator witness above.
        let record = inner
            .sessions
            .iter_mut()
            .find(|record| record.session.id == session)
            .unwrap();
        record.orchestrator_auto_dispatch_blocked = true;
    }
    let paused = state.summary_snapshot();
    assert_eq!(count.load(Ordering::SeqCst), 2);
    drop(observer);
    assert_eq!(after.revision, before.revision);
    assert_eq!(paused.revision, before.revision);
    let after_name = after.sessions.iter().find(|row| row.id == session).unwrap();
    assert_eq!(after_name.name, "unstamped explicit read");
    assert!(!after_name.queue_paused);
    let fresh = state.snapshot_from_inner(&state.inner.lock().unwrap());
    assert_eq!(
        serde_json::to_value(paused).unwrap(),
        serde_json::to_value(fresh).unwrap()
    );
}
