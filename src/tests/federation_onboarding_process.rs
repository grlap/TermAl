// Owns the explicit central verify action against a second TermAl process.
// Does not claim real SSH/platform pilot evidence. New O1 proof uses actual
// domain HTTP and registry state, with loopback ports standing in for forwards.
use super::federation_onboarding_child::{Satellite, Setup};
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_two_process_fake_link_operator_verify() {
    let app = federation_registry::central();
    let root = app.test_temp_root_path().unwrap().to_owned();
    let cleaned = app.test_temp_root.as_ref().unwrap().observe_cleanup();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let central_port = listener.local_addr().unwrap().port();
    let (mut child, ready) = tokio::task::spawn_blocking(Satellite::start).await.unwrap();
    let mut record = federation_registry::peer(central_port);
    record.identity = ready.identity;
    record.profile = federation_onboarding_fixture::profile();
    record.platform = std::env::consts::OS.to_owned();
    record.arch = std::env::consts::ARCH.to_owned();
    record.endpoint.satellite_federation_port = ready.port;
    record.endpoint.satellite_reverse_forward_port = central_port;
    let id = record.identity.host_id.clone();
    app.enroll_federation_peer(record).unwrap();
    let snapshot = app.federation_peer_snapshot(&id).unwrap();
    let setup = Setup {
        central: snapshot.local.clone(),
        central_port,
    };
    let child = tokio::task::spawn_blocking(move || {
        child.configure(setup);
        child
    })
    .await
    .unwrap();
    let channel = FederationChannel::for_app(
        snapshot,
        FederationPairCapability::new("a".repeat(64)).unwrap(),
        app.clone(),
    );
    let checked = channel.clone();
    let check: FederationAuthorityCheck = Arc::new(move || checked.current_authority().map(|_| ()));
    let rpc = FederationRpcClient::new(ready.port, channel.clone(), check.clone()).unwrap();
    let router = federation_router(channel.clone(), check).layer(axum::Extension(
        FederationOnboardingHost {
            app: app.clone(),
            rpc: rpc.clone(),
            io: federation_onboarding_fixture::io(),
        },
    ));
    let (stop, mut rx) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { federation_wait_stop(&mut rx).await })
            .await
            .unwrap();
    });
    app.federation_peer_handles.lock().unwrap().insert(
        id.clone(),
        Arc::new(FederationPeerHandle {
            channel: channel.clone(),
            rpc: rpc.clone(),
        }),
    );
    let mut control: FederationControlIo = Box::new(
        tokio::net::TcpStream::connect(("127.0.0.1", ready.port))
            .await
            .unwrap(),
    );
    let (context, guard) = tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        federation_attest_central(&channel, &mut control),
    )
    .await
    .unwrap()
    .unwrap();
    let (control_stop, control_rx) = tokio::sync::watch::channel(false);
    let (control_ready, ready_rx) = tokio::sync::oneshot::channel();
    let control_task = tokio::spawn(federation_control_central(
        channel.clone(),
        control,
        context,
        guard,
        control_rx,
        control_ready,
    ));
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, ready_rx)
        .await
        .unwrap()
        .unwrap();
    let response = app_router(app.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/federation/peers/{id}/verify"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let report: FederationOnboardingReport =
        serde_json::from_slice(&to_bytes(response.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert!(
        report.outcomes.iter().all(|check| check.passed),
        "{:?}",
        report.outcomes
    );
    assert!(report.termal_complete());
    assert_eq!(
        app.federation_peer_snapshot(&id).unwrap().membership,
        FederationMembershipState::Verified
    );
    {
        let persisted = rusqlite::Connection::open(app.persistence_path.as_path()).unwrap();
        let image = read_federation_persist_image(&persisted).unwrap().unwrap();
        assert_eq!(
            image["domain"]["onboarding"]["reports"][&id]["attemptId"],
            report.attempt_id
        );
    }
    control_stop.send_replace(true);
    control_task.await.unwrap().unwrap();
    tokio::task::spawn_blocking(move || child.finish())
        .await
        .unwrap();
    channel.invalidate();
    stop.send_replace(true);
    server.await.unwrap();
    // The registered channel owns an AppState clone. Release this fixture's
    // handle before dropping state so the lookup map cannot retain itself.
    assert!(
        app.federation_peer_handles
            .lock()
            .unwrap()
            .remove(&id)
            .is_some()
    );
    drop(rpc);
    drop(channel);
    drop(app);
    phase_sync::receive(&cleaned, "onboarding process central cleanup").unwrap();
    assert!(!root.exists());
    assert!(!TestTempRoot::owner_marker(&root).exists());
}
