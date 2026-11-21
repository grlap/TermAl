use super::*;
use crate::tests::test_runs::{full_request, passed_results, running_results, write_run};

async fn active_snapshot(fixture: &Fixture) -> Result<Value, ApiError> {
    let lease = fixture.channel.current_lease()?;
    fixture.rpc.active_test_runs(&lease).await.map(|reply| {
        serde_json::to_value(reply).expect("the active snapshot has a serializable wire form")
    })
}

fn refresh_runs(app: &AppState, alive: &[u32]) {
    let alive = alive.to_vec();
    app.refresh_test_runs_with(&move |pid, _| alive.contains(&pid), &|inner, event| {
        app.publish_delta_locked(inner, event.clone())
    });
}

fn active_run_project(app: &AppState, title: &str) -> (PathBuf, String) {
    let root = app
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("active-runs");
    fs::create_dir_all(&root).unwrap();
    run_git_test_command(&root, &["init", "--quiet"]);
    let project = create_test_project(app, &root, title);
    (root, project)
}

#[tokio::test]
async fn enrolled_central_reads_active_runs_from_their_origin() {
    let fixture = Fixture::production_shape().await;
    let app = &fixture.satellite;
    let (root, _) = active_run_project(app, "Remote tests");
    let runs_dir = root.join(".git").join("review-runs");
    let run_id = "test-remote-active";
    let started = "2026-09-25T10:00:00.000Z";
    write_run(
        &runs_dir,
        run_id,
        full_request(&root, started),
        Some(running_results(Some(21))),
    );
    TEST_TEST_RUN_ARTIFACT_READS.with(|reads| reads.set(0));
    TEST_TEST_RUN_FULL_PROJECTIONS.with(|count| count.set(0));
    refresh_runs(app, &[21]);
    assert!(TEST_TEST_RUN_ARTIFACT_READS.with(|reads| reads.replace(0)) > 0);
    assert!(TEST_TEST_RUN_FULL_PROJECTIONS.with(|count| count.replace(0)) > 0);
    TEST_STATE_SQLITE_READ_OPENS.with(|reads| reads.set(0));
    drop(open_sqlite_state_read_connection(app.persistence_path.as_path()).unwrap());
    assert_eq!(
        TEST_STATE_SQLITE_READ_OPENS.with(|reads| reads.replace(0)),
        1
    );
    assert_eq!(
        app.state_broadcast_senders.stream_events.receiver_count(),
        0
    );

    let snapshot = active_snapshot(&fixture).await.unwrap_or_else(|error| {
        panic!(
            "origin should serve its active Test Runs snapshot: {} {}",
            error.status, error.message
        )
    });
    assert_eq!(TEST_TEST_RUN_FULL_PROJECTIONS.with(|count| count.get()), 0);
    assert_eq!(TEST_TEST_RUN_ARTIFACT_READS.with(|reads| reads.get()), 0);
    assert_eq!(TEST_STATE_SQLITE_READ_OPENS.with(|reads| reads.get()), 0);
    assert_eq!(snapshot["origin"], json!(fixture.channel.snapshot.peer));
    assert_eq!(snapshot["ready"], true);
    assert_eq!(snapshot["truncated"], false);
    assert_eq!(
        snapshot["runs"],
        json!([{
            "runId": run_id,
            "state": "running",
            "project": "Remote tests",
            "owner": null,
            "startedAt": started,
        }])
    );

    write_run(
        &runs_dir,
        run_id,
        full_request(&root, started),
        Some(passed_results()),
    );
    refresh_runs(app, &[]);
    let snapshot = active_snapshot(&fixture).await.unwrap();
    assert_eq!(snapshot["ready"], true);
    assert_eq!(snapshot["runs"], json!([]));
    fixture.finish().await;
}

#[tokio::test]
async fn active_reply_distinguishes_warming_empty_and_truncation() {
    let fixture = Fixture::production_shape().await;
    let app = &fixture.satellite;
    let warming = active_snapshot(&fixture).await.unwrap();
    assert_eq!(warming["ready"], false);
    assert_eq!(warming["asOf"], Value::Null);
    assert_eq!(warming["runs"], json!([]));

    let long_name = "界".repeat(50);
    let (root, _) = active_run_project(app, &long_name);
    refresh_runs(app, &[]);
    let empty = active_snapshot(&fixture).await.unwrap();
    assert_eq!(empty["ready"], true);
    assert_eq!(empty["truncated"], false);
    assert_eq!(empty["runs"], json!([]));

    for number in 0..23 {
        write_run(
            &root.join(".git").join("review-runs"),
            &format!("test-bounded-{number:02}"),
            full_request(&root, "2026-09-25T10:00:00.000Z"),
            Some(running_results(Some(21))),
        );
    }
    refresh_runs(app, &[21]);
    let snapshot = active_snapshot(&fixture).await.unwrap();
    assert_eq!(snapshot["ready"], true);
    assert_eq!(snapshot["truncated"], true);
    let rows = snapshot["runs"].as_array().unwrap();
    let ids: Vec<_> = rows
        .iter()
        .map(|row| row["runId"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        (0..20)
            .map(|number| format!("test-bounded-{number:02}"))
            .collect::<Vec<_>>()
    );
    assert!(rows.iter().all(|row| row["project"] == "界".repeat(32)));
    assert!(serde_json::to_vec(&snapshot).unwrap().len() <= 32 * 1024);
    fixture.finish().await;
}

#[tokio::test]
async fn peer_revocation_makes_previous_active_projection_unavailable() {
    let fixture = Fixture::production_shape().await;
    refresh_runs(&fixture.satellite, &[]);
    assert_eq!(active_snapshot(&fixture).await.unwrap()["ready"], true);
    let app = &fixture.central;
    let expected = app
        .federation_peer_snapshot(&fixture.channel.snapshot.peer.host_id)
        .unwrap();
    app.transition_federation_peer(&expected, FederationMembershipState::Revoked)
        .unwrap();
    assert!(active_snapshot(&fixture).await.is_err());
    fixture.finish().await;
}

#[tokio::test]
async fn active_snapshot_merges_the_launcher_and_its_observed_execution() {
    let fixture = Fixture::production_shape().await;
    let app = &fixture.satellite;
    let (root, project) = active_run_project(app, "Remote launcher");
    let owner = create_test_project_session(app, Agent::Codex, &project, &root);
    {
        let (runtime, _commands) = test_codex_runtime_handle("active-snapshot-runtime");
        let mut inner = app.inner.lock().unwrap();
        let index = inner.find_session_index(&owner).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }
    refresh_runs(app, &[]);
    let command = "node scripts/test-launcher.mjs full";
    app.note_engram_command_started(
        &owner,
        &EngramObservationProvenance::Ambient,
        "remote-active-tool",
        Some(command),
        Some(&root.to_string_lossy()),
    );
    let observed = active_snapshot(&fixture).await.unwrap();
    let rows = observed["runs"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["project"], "Remote launcher");
    assert_eq!(rows[0]["owner"], "Test");
    let observed_id = rows[0]["runId"].as_str().unwrap();
    let started = rows[0]["startedAt"].as_str().unwrap();

    let run_id = format!("test-{}", Uuid::new_v4());
    let pid = std::process::id();
    let mut request = full_request(&fs::canonicalize(&root).unwrap(), started);
    request["owner"] = json!(owner);
    request["creatorPid"] = json!(pid);
    let directory = write_run(
        &root.join(".git").join("review-runs"),
        &run_id,
        request,
        Some(running_results(Some(pid))),
    );
    app.observe_test_command_artifact(
        &owner,
        &EngramObservationProvenance::Ambient,
        "remote-active-tool",
        command,
        &format!("STARTED {}", test_run_display_path(&directory)),
        false,
    );
    {
        let inner = app.inner.lock().unwrap();
        let row = inner.test_run_observations.get(observed_id).unwrap();
        assert_eq!(
            row.artifact
                .as_ref()
                .map(|artifact| artifact.run_id.as_str()),
            Some(run_id.as_str()),
            "the launcher locator must bind the original observation"
        );
    }
    refresh_runs(app, &[pid]);
    let merged = active_snapshot(&fixture).await.unwrap();
    assert_ne!(observed_id, run_id.as_str());
    assert_eq!(
        merged["runs"],
        json!([{
            "runId": run_id,
            "state": "running",
            "project": "Remote launcher",
            "owner": "Test",
            "startedAt": started,
        }])
    );
    fixture.finish().await;
}

#[tokio::test]
async fn an_authenticated_older_peer_is_unsupported_without_retiring_the_pair() {
    use crate::tests::federation_control_fixture::{authenticated_router, establish};
    use crate::tests::federation_link::{allowed, channel};

    let central = channel(FederationRole::Central, 'a');
    let (satellite, _proof) = establish(&central).await;
    let mut server = DomainServer::start(authenticated_router(
        satellite,
        Router::new().route("/federation/v1/health", get(federation_health)),
    ))
    .await;
    let rpc = FederationRpcClient::new(server.port, central.clone(), allowed()).unwrap();
    let lease = central.current_lease().unwrap();
    rpc.health(&lease).await.unwrap();
    assert_eq!(
        rpc.active_test_runs(&lease).await.unwrap_err().status,
        StatusCode::NOT_FOUND,
    );
    rpc.health(&lease).await.unwrap();
    server.finish().await;
}

#[tokio::test]
async fn a_busy_origin_refuses_the_snapshot_without_retiring_the_pair() {
    let fixture = Fixture::production_shape().await;
    let held = fixture
        .satellite_channel
        .active_test_runs_read
        .try_acquire()
        .unwrap();
    let error = active_snapshot(&fixture).await.unwrap_err();
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    drop(held);
    let lease = fixture.channel.current_lease().unwrap();
    fixture.rpc.health(&lease).await.unwrap();
    assert_eq!(active_snapshot(&fixture).await.unwrap()["ready"], false);
    fixture.finish().await;
}

#[tokio::test]
async fn an_owner_must_still_be_local_to_appear_in_the_remote_snapshot() {
    let fixture = Fixture::production_shape().await;
    let app = &fixture.satellite;
    let (root, project) = active_run_project(app, "Remote tests");
    let owner = create_test_project_session(app, Agent::Codex, &project, &root);
    let mut request = full_request(&root, "2026-09-25T10:00:00.000Z");
    request["owner"] = json!(owner);
    write_run(
        &root.join(".git").join("review-runs"),
        "test-owned",
        request,
        Some(running_results(Some(21))),
    );
    refresh_runs(app, &[21]);
    let local = active_snapshot(&fixture).await.unwrap();
    assert_eq!(local["runs"][0]["owner"], "Test");

    {
        let mut inner = app.inner.lock().unwrap();
        let index = inner.find_session_index(&owner).unwrap();
        let record = &mut inner.sessions[index];
        record.remote_id = Some("another-host".to_owned());
        record.remote_session_id = Some("their-session".to_owned());
    }
    // Capture from the same cached run after only the session's locality changes.
    drop(app.summary_snapshot());
    let proxy = active_snapshot(&fixture).await.unwrap();
    assert_eq!(proxy["runs"][0]["runId"], "test-owned");
    assert_eq!(proxy["runs"][0]["owner"], Value::Null);
    assert_eq!(
        app.test_run_summaries(None)[0].owner_session_id.as_deref(),
        Some(owner.as_str())
    );
    fixture.finish().await;
}

async fn while_state_is_held<T>(app: &AppState, read: impl std::future::Future<Output = T>) -> T {
    let app = app.clone();
    let (entered, locked) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::sync_channel(1);
    let holder = tokio::task::spawn_blocking(move || {
        let _state = app.inner.lock().unwrap();
        entered.send(()).unwrap();
        released.recv_timeout(phase_sync::DEADLOCK_GUARD)
    });
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, locked)
        .await
        .expect("the fixture acquired StateInner")
        .unwrap();
    let result = read.await;
    release
        .send(())
        .expect("read returned with the state lock held");
    holder.await.unwrap().unwrap();
    result
}

#[tokio::test]
async fn a_held_state_lock_on_either_host_refuses_the_read_without_retiring_the_pair() {
    let fixture = Fixture::production_shape().await;
    for app in [&fixture.central, &fixture.satellite] {
        let result = while_state_is_held(app, active_snapshot(&fixture)).await;
        assert_eq!(result.unwrap_err().status, StatusCode::SERVICE_UNAVAILABLE);
        let lease = fixture.channel.current_lease().unwrap();
        fixture.rpc.health(&lease).await.unwrap();
    }
    fixture.finish().await;
}

#[tokio::test]
async fn an_origin_busy_after_dispatch_signs_a_refusal_instead_of_its_snapshot() {
    use crate::tests::federation_control_fixture::authenticated_router;
    use crate::tests::federation_link::allowed;

    let fixture = Fixture::production_shape().await;
    let collecting = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let origin = fixture.channel.snapshot.peer.clone();
    let router = Router::new()
        .route("/federation/v1/health", get(federation_health))
        .route(
            FEDERATION_ACTIVE_TEST_RUNS_ROUTE,
            get({
                let collecting = collecting.clone();
                let release = release.clone();
                move || async move {
                    Response::new(Body::from_stream(async_stream::stream! {
                        collecting.notify_one();
                        release.notified().await;
                        yield Ok::<_, std::io::Error>(axum::body::Bytes::from(
                            serde_json::to_vec(&FederatedActiveTestRunsReply {
                                version: 1,
                                origin,
                                snapshot_version: 0,
                                as_of: None,
                                ready: false,
                                truncated: false,
                                runs: Vec::new(),
                            }).unwrap(),
                        ));
                    }))
                }
            }),
        );
    let mut server = DomainServer::start(authenticated_router(
        fixture.satellite_channel.clone(),
        router,
    ))
    .await;
    let rpc = FederationRpcClient::new(server.port, fixture.channel.clone(), allowed()).unwrap();
    let lease = fixture.channel.current_lease().unwrap();
    let request = tokio::spawn({
        let rpc = rpc.clone();
        let lease = lease.clone();
        async move { rpc.active_test_runs(&lease).await }
    });
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, collecting.notified())
        .await
        .expect("the listener is collecting the handler's successful response");
    let result = while_state_is_held(&fixture.satellite, async move {
        release.notify_one();
        request.await.unwrap()
    })
    .await;
    assert_eq!(result.unwrap_err().status, StatusCode::SERVICE_UNAVAILABLE);
    rpc.health(&lease).await.unwrap();
    server.finish().await;
    fixture.finish().await;
}

#[tokio::test]
async fn cancelling_the_active_read_keeps_its_permit_until_the_owned_deadline_settles() {
    use crate::tests::federation_control_fixture::{authenticated_router, establish};
    use crate::tests::federation_link::{allowed, channel};

    let mut central = channel(FederationRole::Central, 'a');
    central.control.clock = FederationControlClock::scripted_io();
    let (satellite, _proof) = establish(&central).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let router = Router::new()
        .route("/federation/v1/health", get(federation_health))
        .route(
            FEDERATION_ACTIVE_TEST_RUNS_ROUTE,
            get({
                let entered = entered.clone();
                let release = release.clone();
                move || async move {
                    entered.notify_one();
                    release.notified().await;
                    StatusCode::SERVICE_UNAVAILABLE
                }
            }),
        );
    let mut server = DomainServer::start(authenticated_router(satellite, router)).await;
    let rpc = FederationRpcClient::new(server.port, central.clone(), allowed()).unwrap();
    let lease = central.current_lease().unwrap();
    let request = tokio::spawn({
        let rpc = rpc.clone();
        let lease = lease.clone();
        async move { rpc.active_test_runs(&lease).await }
    });
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, entered.notified())
        .await
        .expect("the authenticated handler saw the allocated counter");
    let context = central.security_context().unwrap();
    assert_eq!(context.sender.lock().unwrap().outstanding.len(), 1);
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    assert_eq!(
        rpc.active_test_runs(&lease).await.unwrap_err().status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(context.sender.lock().unwrap().outstanding.len(), 1);

    central
        .control
        .clock
        .advance(host_utilization::PEER_DEADLINE);
    let permit = tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        central.active_test_runs_read.clone().acquire_owned(),
    )
    .await
    .expect("the owned request releases its permit at its deadline")
    .unwrap();
    assert!(context.sender.lock().unwrap().outstanding.is_empty());
    drop(permit);
    release.notify_one();
    central.control.clock.advance(Duration::from_secs(10));
    rpc.health(&lease).await.unwrap();
    server.finish().await;
}

#[tokio::test]
async fn an_authenticated_reply_for_another_origin_does_not_retire_the_pair() {
    use crate::tests::federation_control_fixture::{authenticated_router, establish};
    use crate::tests::federation_link::{allowed, channel};

    let central = channel(FederationRole::Central, 'a');
    let (satellite, _proof) = establish(&central).await;
    let wrong_origin = central.snapshot.local.clone();
    let router = Router::new()
        .route("/federation/v1/health", get(federation_health))
        .route(
            FEDERATION_ACTIVE_TEST_RUNS_ROUTE,
            get(move || async move {
                Json(FederatedActiveTestRunsReply {
                    version: 1,
                    origin: wrong_origin,
                    snapshot_version: 0,
                    as_of: None,
                    ready: false,
                    truncated: false,
                    runs: Vec::new(),
                })
            }),
        );
    let mut server = DomainServer::start(authenticated_router(satellite, router)).await;
    let rpc = FederationRpcClient::new(server.port, central.clone(), allowed()).unwrap();
    let lease = central.current_lease().unwrap();
    assert_eq!(
        rpc.active_test_runs(&lease).await.unwrap_err().status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    rpc.health(&lease).await.unwrap();
    server.finish().await;
}

async fn active_reply_after_local_context_changes(corrupt_mac: bool) {
    use crate::tests::federation_control_fixture::authenticated_router;
    use crate::tests::federation_link::allowed;

    let fixture = Fixture::production_shape().await;
    let signed = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let router = authenticated_router(
        fixture.satellite_channel.clone(),
        Router::new().route(
            FEDERATION_ACTIVE_TEST_RUNS_ROUTE,
            get(|| async { Json(json!({})) }),
        ),
    )
    .layer(axum::middleware::from_fn({
        let signed = signed.clone();
        let release = release.clone();
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let signed = signed.clone();
            let release = release.clone();
            async move {
                let mut response = next.run(request).await;
                assert!(response.headers().contains_key(FEDERATION_MAC_HEADER));
                if corrupt_mac {
                    response.headers_mut().remove(FEDERATION_MAC_HEADER);
                }
                signed.notify_one();
                release.notified().await;
                response
            }
        }
    }));
    let mut server = DomainServer::start(router).await;
    let rpc = FederationRpcClient::new(server.port, fixture.channel.clone(), allowed()).unwrap();
    let lease = fixture.channel.current_lease().unwrap();
    let original = fixture.channel.security_context().unwrap();
    let request = tokio::spawn(async move { rpc.active_test_runs(&lease).await });
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, signed.notified())
        .await
        .expect("the signed reply is waiting to be delivered");
    if !corrupt_mac {
        fixture
            .channel
            .state
            .lock()
            .unwrap()
            .security
            .take()
            .unwrap();
    }
    let result = while_state_is_held(&fixture.central, async move {
        release.notify_one();
        request.await.unwrap()
    })
    .await;
    assert!(original.sender.lock().unwrap().outstanding.is_empty());
    if corrupt_mac {
        assert_eq!(result.unwrap_err().status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(*original.retired.borrow());
    } else {
        assert_eq!(result.unwrap_err().status, StatusCode::CONFLICT);
        assert!(!*original.retired.borrow());
        fixture.channel.state.lock().unwrap().security = Some(original);
    }
    assert_eq!(fixture.channel.active_test_runs_read.available_permits(), 1);
    server.finish().await;
    fixture.finish().await;
}

#[tokio::test]
async fn a_verified_reply_on_a_stale_context_settles_while_state_is_held() {
    active_reply_after_local_context_changes(false).await;
}

#[tokio::test]
async fn an_invalid_reply_fails_closed_while_state_is_held() {
    active_reply_after_local_context_changes(true).await;
}

#[tokio::test]
async fn head_cannot_invoke_the_active_snapshot() {
    use crate::tests::federation_control_fixture::signed;
    use tower::ServiceExt;

    let fixture = Fixture::production_shape().await;
    let satellite = fixture.satellite_channel.clone();
    let check = satellite.clone();
    let router = federation_router(
        satellite,
        Arc::new(move || check.current_authority().map(|_| ())),
    );
    let context = fixture.channel.security_context().unwrap();
    let counter = fixture
        .rpc
        .allocate_authenticated_counter(&context)
        .await
        .unwrap();
    let response = router
        .oneshot(signed(
            &fixture.channel,
            counter,
            "HEAD",
            FEDERATION_ACTIVE_TEST_RUNS_ROUTE,
            Default::default(),
            b"",
        ))
        .await
        .unwrap();
    context.settle(counter);
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert!(response.headers().contains_key(FEDERATION_MAC_HEADER));
    fixture.finish().await;
}

#[tokio::test]
async fn an_authenticated_busy_refusal_consumes_the_replay_counter() {
    use crate::tests::federation_control_fixture::{authenticated_router, establish, signed};
    use crate::tests::federation_link::channel;
    use tower::ServiceExt;

    let central = channel(FederationRole::Central, 'a');
    let (satellite, _proof) = establish(&central).await;
    let router = authenticated_router(
        satellite.clone(),
        Router::new().route(
            FEDERATION_ACTIVE_TEST_RUNS_ROUTE,
            get(|| async { StatusCode::OK }),
        ),
    );
    let counter = 1;
    let request = || {
        signed(
            &central,
            counter,
            "GET",
            FEDERATION_ACTIVE_TEST_RUNS_ROUTE,
            Default::default(),
            b"",
        )
    };
    let (reading, started) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let mut delayed = request();
    *delayed.body_mut() = Body::from_stream(async_stream::stream! {
        reading.send(()).unwrap();
        released.await.unwrap();
        yield Ok::<_, std::io::Error>(axum::body::Bytes::new());
    });
    let pending = tokio::spawn(router.clone().oneshot(delayed));
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, started)
        .await
        .unwrap()
        .unwrap();
    let (locked, held) = tokio::sync::oneshot::channel();
    let (unlock, released_link) = std::sync::mpsc::sync_channel(1);
    let holder = tokio::task::spawn_blocking(move || {
        let _link = satellite.state.lock().unwrap();
        locked.send(()).unwrap();
        released_link
            .recv_timeout(phase_sync::DEADLOCK_GUARD)
            .unwrap();
    });
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, held)
        .await
        .unwrap()
        .unwrap();
    release.send(()).unwrap();
    let busy = pending.await.unwrap().unwrap();
    unlock.send(()).unwrap();
    holder.await.unwrap();
    assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(busy.headers().contains_key(FEDERATION_MAC_HEADER));
    let replay = router.oneshot(request()).await.unwrap();
    assert_eq!(replay.status(), StatusCode::CONFLICT);
}
