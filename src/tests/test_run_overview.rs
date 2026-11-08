//! Public catalogue and immutable inputs for counters-only global transport.
use super::*;

async fn page(app: &Router, uri: &str) -> (StatusCode, Value) {
    request_json(
        app,
        Request::builder().uri(uri).body(Body::empty()).unwrap(),
    )
    .await
}

#[tokio::test]
async fn filtered_pages_reach_every_active_run_and_bind_the_cursor_to_all_filters() {
    let fixture = RunFixture::new("overview-active-pages");
    let owner = create_test_project_session(
        &fixture.state,
        Agent::Codex,
        &fixture.project_id,
        &fixture.root,
    );
    let recipient = create_test_project_session(
        &fixture.state,
        Agent::Codex,
        &fixture.project_id,
        &fixture.root,
    );
    let runs = fixture.runs_dir();
    for number in 0..256 {
        let mut request = full_request(&fixture.root, "2026-09-25T10:00:00.000Z");
        request["owner"] = json!(if number < 230 { &owner } else { &recipient });
        if number < 10 {
            request["notifyTo"] = json!(recipient);
        }
        write_run(
            &runs,
            &format!("test-active-{number:03}"),
            request,
            Some(running_results(Some(42))),
        );
    }
    write_run(
        &runs,
        "test-terminal",
        full_request(&fixture.root, "2026-09-25T10:00:00.000Z"),
        Some(passed_results()),
    );
    fixture.refresh(&[42]);
    let before = fixture.summaries();
    let app = app_router(fixture.state.clone());
    let (status, first) = page(&app, "/api/test-runs?view=active&limit=200").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["total"], 256);
    assert_eq!(first["runs"].as_array().unwrap().len(), 200);
    assert!(
        first["runs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["state"] == "running")
    );
    let cursor = first["nextCursor"].as_str().unwrap();
    let (status, second) = page(
        &app,
        &format!("/api/test-runs?view=active&limit=200&after={cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second["total"], 256);
    assert_eq!(second["runs"].as_array().unwrap().len(), 56);
    assert!(second["nextCursor"].is_null());
    let ids: HashSet<_> = first["runs"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["runs"].as_array().unwrap())
        .map(|row| row["runId"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids.len(),
        256,
        "equal start times cannot lose or repeat a boundary identity"
    );
    for changed_filter in [
        "view=history".to_owned(),
        "view=attention".to_owned(),
        format!("view=active&sessionId={recipient}"),
        "view=active&state=running".to_owned(),
        format!("view=active&projectId={}", fixture.project_id),
    ] {
        assert_eq!(
            page(
                &app,
                &format!("/api/test-runs?{changed_filter}&after={cursor}")
            )
            .await
            .0,
            StatusCode::BAD_REQUEST,
            "a cursor cannot change its filter: {changed_filter}"
        );
    }
    let (_, notified) = page(
        &app,
        &format!("/api/test-runs?view=active&sessionId={recipient}"),
    )
    .await;
    assert_eq!(
        notified["total"], 36,
        "the session filter includes 26 owned and 10 notify-only runs"
    );
    let (_, terminal) = page(&app, "/api/test-runs?state=passed").await;
    assert_eq!(terminal["total"], 1);
    assert_eq!(terminal["runs"][0]["runId"], "test-terminal");
    let (_, attention) = page(&app, "/api/test-runs?view=attention&state=passed").await;
    assert_eq!(
        attention["total"], 1,
        "a passed but unresolved discovered run still needs attention"
    );
    assert_eq!(
        fixture.summaries(),
        before,
        "history reads cannot settle, remove, or rewrite runs"
    );

    // Exercise the real global producer, not a smaller manually chosen run
    // set. First materialization is outside the state lock; subsequent
    // unrelated publication reuses that exact immutable projection.
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .state_projection_cache
        .lock()
        .unwrap()
        .test_runs = None;
    let state = fixture.state.clone();
    TEST_RUN_PROJECTION_STARTED.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            assert!(state.inner.is_not_held_by_current_thread_for_test());
        }))
    });
    let snapshot = fixture.state.summary_snapshot();
    let wire = serde_json::to_value(&snapshot).unwrap();
    assert!(wire.get("testRuns").is_none());
    assert_eq!(wire["testRunOverview"]["total"], 257);
    assert_eq!(wire["testRunOverview"]["running"], 256);
    assert_eq!(wire["testRunOverview"]["passed"], 1);
    let session_activity = |id: &str| {
        wire["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id)
            .unwrap()["testRunActivity"]
            .clone()
    };
    assert_eq!(session_activity(&owner)["ownedRunning"], 230);
    assert_eq!(session_activity(&recipient)["ownedRunning"], 26);
    assert_eq!(session_activity(&recipient)["notifyRunning"], 10);
    let after_bytes = serde_json::to_vec(&wire).unwrap().len();
    let mut legacy = wire.clone();
    legacy.as_object_mut().unwrap().remove("testRunOverview");
    legacy["testRuns"] = serde_json::to_value(&before).unwrap();
    for row in legacy["sessions"].as_array_mut().unwrap() {
        row.as_object_mut().unwrap().remove("testRunActivity");
    }
    let legacy_bytes = serde_json::to_vec(&legacy).unwrap().len();
    eprintln!(
        "fixture transport comparison: runs=257 legacy_schema_bytes={legacy_bytes} overview_schema_bytes={after_bytes}; reconstructed legacy schema on identical fixture, not a live-host timing"
    );
    assert!(after_bytes < legacy_bytes);
    TEST_TEST_RUN_FULL_PROJECTIONS.with(|count| count.set(0));
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner.revision += 1;
        fixture.state.publish_state_locked(&inner);
    }
    assert_eq!(
        TEST_TEST_RUN_FULL_PROJECTIONS.with(|count| count.get()),
        0,
        "unrelated publication must not rebuild or sort complete run history"
    );

    let mut stream = fixture.state.subscribe_stream_events();
    for id in ["test-active-000", "test-active-001"] {
        let mut results = passed_results();
        results["runId"] = json!(id);
        fs::write(runs.join(id).join("results.json"), results.to_string()).unwrap();
    }
    fixture.refresh(&[42]);
    let mut changes = Vec::new();
    while let Ok(event) = stream.try_recv() {
        if let SseStreamEvent::State(payload) = event {
            changes.push(serde_json::from_str::<Value>(&payload).unwrap());
        }
    }
    assert_eq!(
        changes.len(),
        1,
        "one public update after the scanner installs its batch"
    );
    assert!(changes[0].get("run").is_none());
    assert!(changes[0].get("testRuns").is_none());
    assert_eq!(changes[0]["testRunOverview"]["running"], 254);
    assert_eq!(changes[0]["testRunOverview"]["passed"], 3);
    let (_, reconnect) = page(&app, "/api/state").await;
    assert!(reconnect.get("testRuns").is_none());
    assert_eq!(reconnect["testRunOverview"], changes[0]["testRunOverview"]);
}

#[test]
fn multi_run_scans_cover_hidden_revisions_before_public_cards() {
    for cards in [false, true] {
        let mut fixture = RunFixture::new(if cards { "overview-cards-order" } else { "overview-batch-order" });
        let owner = create_test_project_session(&fixture.state, Agent::Codex, &fixture.project_id, &fixture.root);
        if cards { assert!(fixture.state.ensure_test_run_cards_epoch()); }
        let mailbox = Arc::new(StateBroadcastMailbox::default());
        fixture.state.state_broadcast_mailbox = Some(mailbox.clone());
        let senders = fixture.state.state_broadcast_senders.clone();
        let mut producer = StateDiffProducer::default();
        let mut stream = fixture.state.subscribe_stream_events();
        let mut latest = fixture.state.inner.lock().unwrap().revision;
        let mut baseline = latest;
        {
            let inner = fixture.state.inner.lock().unwrap();
            fixture.state.publish_state_locked(&inner);
        }
        for work in mailbox.take_pending_for_test() { producer.forward(work, &senders); }
        assert!(matches!(stream.try_recv().unwrap(), SseStreamEvent::State(_)));
        let started = (chrono::Utc::now() + chrono::TimeDelta::seconds(1)).to_rfc3339();
        for terminal in [false, true] {
            for id in ["test-batch-a", "test-batch-b"] {
                let mut request = full_request(&fixture.root, &started);
                request["owner"] = json!(owner);
                write_run(&fixture.runs_dir(), id, request,
                    Some(if terminal { passed_results() } else { running_results(Some(42)) }));
            }
            fixture.refresh(&[42]);
            for work in mailbox.take_pending_for_test() { producer.forward(work, &senders); }
            let mut overview = None;
            let mut card_events = 0;
            while let Ok(event) = stream.try_recv() {
                match event {
                    SseStreamEvent::StateDiff(payload) => {
                        let value: Value = serde_json::from_str(&payload).unwrap();
                        assert_eq!(value["baseRevision"], baseline);
                        let revision = value["revision"].as_u64().unwrap();
                        assert!(revision >= latest, "a cumulative cut can cover hidden revisions or follow equal-revision card deltas");
                        baseline = revision;
                        latest = revision;
                        assert!(value.get("testRuns").is_none());
                        overview = Some(value["header"]["testRunOverview"].clone());
                    }
                    SseStreamEvent::Delta(payload) => {
                        let value: Value = serde_json::from_str(&payload).unwrap();
                        let revision = value["revision"].as_u64().unwrap();
                        assert_eq!(revision, latest + 1, "public deltas must not force a gap repair: {value}");
                        latest = revision;
                        assert!(matches!(value["type"].as_str(), Some("messageCreated" | "testRunCardUpdated")));
                        card_events += 1;
                    }
                    _ => panic!("expected the existing cumulative diff path, not a full state or loss signal"),
                }
            }
            let overview = overview.expect("the final aggregate cut is public");
            assert_eq!(overview["total"], 2);
            assert_eq!(overview["running"], if terminal { 0 } else { 2 });
            assert_eq!(overview["passed"], if terminal { 2 } else { 0 });
            assert_eq!(card_events, if cards { 2 } else { 0 });
            assert_eq!(latest, fixture.state.inner.lock().unwrap().revision);
        }
    }
}

#[test]
fn history_projection_keeps_its_exact_cut_and_runs_without_the_state_lock() {
    let fixture = RunFixture::new("overview-history-cut");
    write_run(
        &fixture.runs_dir(),
        "test-cut",
        full_request(&fixture.root, "2026-09-25T10:00:00.000Z"),
        Some(running_results(Some(42))),
    );
    fixture.refresh(&[42]);
    let cut = {
        let inner = fixture.state.inner.lock().unwrap();
        TestRunProjectionCut::capture_history(&inner)
    };
    let state = fixture.state.clone();
    TEST_RUN_PROJECTION_STARTED.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            assert!(state.inner.is_not_held_by_current_thread_for_test());
            let mut inner = state.inner.lock().unwrap();
            let revision = inner.revision;
            inner
                .test_runs
                .entries
                .update(0, |row| row.summary.state = TestRunState::Failed);
            assert_eq!(
                inner.revision, revision,
                "exercise an unstamped mutation, not a revision shortcut"
            );
        }))
    });
    let old = test_run_project_history(&cut, true);
    assert_eq!(old[0].state, TestRunState::Running);
    let successor = {
        let inner = fixture.state.inner.lock().unwrap();
        assert!(!cut.matches(&inner));
        TestRunProjectionCut::capture_history(&inner)
    };
    assert_eq!(
        test_run_project_history(&successor, true)[0].state,
        TestRunState::Failed
    );
    assert_eq!(
        test_run_project_history(&cut, true)[0].state,
        TestRunState::Running
    );
}

#[test]
fn active_session_diffs_reuse_unchanged_rows_but_emit_a_recaptured_equal_row() {
    let fixture = RunFixture::new("overview-activity-recapture");
    let owner = create_test_project_session(
        &fixture.state,
        Agent::Codex,
        &fixture.project_id,
        &fixture.root,
    );
    let mut request = full_request(&fixture.root, "2026-09-25T10:00:00.000Z");
    request["owner"] = json!(owner);
    write_run(
        &fixture.runs_dir(),
        "test-activity",
        request,
        Some(running_results(Some(42))),
    );
    fixture.refresh(&[42]);
    let capture = || {
        let inner = fixture.state.inner.lock().unwrap();
        fixture.state.immutable_state_snapshot_locked(&inner)
    };
    let senders = fixture.state.state_broadcast_senders.clone();
    let mut events = fixture.state.subscribe_stream_events();
    let mut producer = StateDiffProducer::default();
    producer.forward(StateBroadcastWork::ImmutableSnapshot(capture()), &senders);
    assert!(matches!(
        events.try_recv().unwrap(),
        SseStreamEvent::State(_)
    ));
    fixture.state.inner.lock().unwrap().revision += 1;
    producer.forward(StateBroadcastWork::ImmutableSnapshot(capture()), &senders);
    let SseStreamEvent::StateDiff(payload) = events.try_recv().unwrap() else {
        panic!("expected state diff")
    };
    let unchanged: Value = serde_json::from_str(&payload).unwrap();
    assert!(
        unchanged["sessions"]["changed"]
            .as_array()
            .unwrap()
            .is_empty(),
        "unchanged activity cannot turn every metadata publication into a changed row"
    );

    // Capture an intermediate name without emitting it, then restore the
    // exact old value without stamping. The existing metadata owner mints a
    // new Arc for this recapture: value equality must not erase that signal.
    let original = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&owner).unwrap();
        let name = inner.sessions[index].session.name.clone();
        inner.sessions[index].session.name = "Intermediate name".to_owned();
        name
    };
    drop(capture());
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&owner).unwrap();
        inner.sessions[index].session.name = original;
        inner.revision += 1;
    }
    producer.forward(StateBroadcastWork::ImmutableSnapshot(capture()), &senders);
    let SseStreamEvent::StateDiff(payload) = events.try_recv().unwrap() else {
        panic!("expected state diff")
    };
    let recaptured: Value = serde_json::from_str(&payload).unwrap();
    let changed = recaptured["sessions"]["changed"].as_array().unwrap();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0]["id"], owner);
    assert_eq!(changed[0]["testRunActivity"]["ownedRunning"], 1);
}
