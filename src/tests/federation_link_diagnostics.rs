// Observable retention over real disposable attestation and persistence paths.
// No SSH, live state, transport retry changes or stderr-content assertions.
use super::*;

fn host_channel() -> (AppState, FederationChannel) {
    let app = federation_registry::central();
    let peer = federation_registry::peer(47104);
    let id = peer.identity.host_id.clone();
    app.enroll_federation_peer(peer).unwrap();
    let mut channel = FederationChannel::for_app(
        app.federation_peer_snapshot(&id).unwrap(),
        FederationPairCapability::new("a".repeat(64)).unwrap(),
        app.clone(),
    );
    channel.control.clock = FederationControlClock::scripted_io();
    (app, channel)
}

fn snapshot(app: &AppState) -> Value {
    app.inner
        .lock()
        .unwrap()
        .federation_durability_diagnostics
        .snapshot()
}

#[tokio::test]
async fn link_terminal_history_survives_the_next_authenticated_context() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (app, channel) = host_channel();
    let (_peer, first) = federation_control_fixture::establish(&channel).await;
    assert_eq!(
        snapshot(&app)["linkEvents"],
        json!([]),
        "healthy attestation is quiet"
    );
    let context = channel.security_context().unwrap();
    let installed = channel.transport_basis();
    channel.observe_terminal_detail(&context, "control", "readFailed", Some("UnexpectedEof"));
    let recorded = snapshot(&app)["linkEvents"].clone();
    channel.observe_terminal(&context, "monitor", "sshChildExited");
    assert_eq!(
        snapshot(&app)["linkEvents"],
        recorded,
        "one first cause per context"
    );
    let (_peer, next) = federation_control_fixture::establish(&channel).await;
    assert_ne!(
        channel.security_context().unwrap().incarnation,
        context.incarnation
    );
    assert!(channel.transport_basis().transport_generation > installed.transport_generation);
    let after = snapshot(&app);
    assert_eq!(after["linkEvents"], recorded);
    let row = &after["linkEvents"][0];
    assert_eq!(row["peerHostId"], installed.peer.host_id);
    assert_eq!(row["enrollmentGeneration"], installed.enrollment_generation);
    assert_eq!(row["transportGeneration"], installed.transport_generation);
    assert_eq!(row["generationBasis"], "installed_receipt");
    assert_eq!(row["incarnation"], context.incarnation);
    assert_eq!(row["reason"], "readFailed");
    assert_eq!(row["ioKind"], "UnexpectedEof");
    assert!(chrono::DateTime::parse_from_rfc3339(row["at"].as_str().unwrap()).is_ok());
    assert_eq!(
        app.federation_registry_view().unwrap()["peers"][0]["attestation"]["lastReason"],
        "authenticated"
    );
    drop(next);
    drop(first);
}

#[tokio::test]
async fn link_install_failure_history_retains_the_real_failed_install() {
    use tokio::io::AsyncReadExt;
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (app, channel) = host_channel();
    let connection = rusqlite::Connection::open(app.persistence_path.as_path()).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_link_metadata BEFORE INSERT ON state_metadata_units BEGIN SELECT RAISE(ABORT,'PRIVATE diagnostic failure text'); END;").unwrap();
    let mut opposite = channel.snapshot.clone();
    std::mem::swap(&mut opposite.local, &mut opposite.peer);
    opposite.local_role = FederationRole::Satellite;
    let mut satellite = FederationChannel::new(
        opposite,
        FederationPairCapability::new("a".repeat(64)).unwrap(),
    );
    satellite.control.clock = FederationControlClock::scripted_io();
    let (c, s) = tokio::io::duplex(FEDERATION_FRAME_LIMIT);
    let mut c: FederationControlIo = Box::new(c);
    let mut s: FederationControlIo = Box::new(s);
    let peer = async {
        let mut preface = vec![0; FEDERATION_CONTROL_PREFACE.len()];
        s.read_exact(&mut preface).await.unwrap();
        federation_attest_satellite(&satellite, &mut s).await
    };
    let (central_result, satellite_result) =
        tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
            tokio::join!(federation_attest_central(&channel, &mut c), peer)
        })
        .await
        .unwrap();
    let error = central_result
        .err()
        .expect("the actual local persistence install must fail");
    let retained = snapshot(&app)["linkEvents"].clone();
    let row = &retained[0];
    assert_eq!(retained.as_array().unwrap().len(), 1);
    assert_eq!(row["kind"], "install_failure");
    assert_eq!(row["stage"], "install");
    assert_eq!(row["reason"], "localInstallFailed");
    assert_eq!(row["status"], error.status.as_u16());
    assert_eq!(
        row["transportGeneration"],
        channel.transport_basis().transport_generation
    );
    assert!(!retained.to_string().contains("PRIVATE"));
    assert!(
        !retained
            .to_string()
            .contains(app.persistence_path.to_str().unwrap())
    );
    connection
        .execute_batch("DROP TRIGGER reject_link_metadata;")
        .unwrap();
    app.reconcile_federation_durability().unwrap();
    let fresh = FederationChannel::for_app(
        app.federation_peer_snapshot(&channel.snapshot.peer.host_id)
            .unwrap(),
        FederationPairCapability::new("a".repeat(64)).unwrap(),
        app.clone(),
    );
    let (_peer, proof) = federation_control_fixture::establish(&fresh).await;
    assert_eq!(snapshot(&app)["linkEvents"], retained);
    assert_eq!(
        app.federation_registry_view().unwrap()["peers"][0]["attestation"]["lastReason"],
        "authenticated"
    );
    drop(proof);
    drop(satellite_result.unwrap());
}

#[tokio::test]
async fn link_history_bound_is_independent_and_reconstruction_clears_it() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (app, channel) = host_channel();
    let (_peer, proof) = federation_control_fixture::establish(&channel).await;
    let context = channel.security_context().unwrap();
    let before = snapshot(&app);
    for _ in 0..70 {
        channel.retain_install_diagnostic(&context, StatusCode::LOCKED);
    }
    let after = snapshot(&app);
    assert_eq!(after["linkEvents"].as_array().unwrap().len(), 64);
    assert_eq!(after["linkEventsEvicted"], 6);
    assert_eq!(after["records"], before["records"]);
    assert_eq!(after["evicted"], before["evicted"]);
    let persisted =
        serde_json::to_vec(&PersistedState::from_inner(&app.inner.lock().unwrap())).unwrap();
    let restored = serde_json::from_slice::<PersistedState>(&persisted)
        .unwrap()
        .into_inner()
        .unwrap()
        .federation_durability_diagnostics
        .snapshot();
    assert_ne!(restored["observerId"], after["observerId"]);
    assert_eq!(restored["linkEvents"], json!([]));
    assert_eq!(restored["linkEventsEvicted"], 0);
    drop(proof);
}

#[tokio::test]
async fn link_pre_receipt_failure_uses_the_reused_channels_attempt_basis() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (app, channel) = host_channel();
    let (_peer, first) = federation_control_fixture::establish(&channel).await;
    let (_peer, second) = federation_control_fixture::establish(&channel).await;
    let basis = channel.transport_basis();
    assert!(basis.transport_generation > channel.snapshot.transport_generation);
    let owner = channel.next_security_owner().unwrap();
    let context = Arc::new(FederationSecurityContext::derive(owner, Uuid::new_v4().to_string(),
        vec![], &[1; 32], &[2; 32], &[3; 32]).unwrap());
    app.inner.lock().unwrap().set_federation_durable(false, "test_hold");
    let result = channel.install_security(context).await;
    assert!(matches!(result, Err(error) if error.status == StatusCode::LOCKED));
    let after = snapshot(&app);
    let rows = after["linkEvents"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["generationBasis"], "attempt_basis");
    assert_eq!(rows[0]["enrollmentGeneration"], basis.enrollment_generation);
    assert_eq!(rows[0]["transportGeneration"], basis.transport_generation);
    assert_eq!(rows[0]["ownerGeneration"], owner);
    drop(second);
    drop(first);
}

#[tokio::test]
async fn channel_hold_probes_and_repeated_wire_refusals_preserve_the_opening_cause() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (app, channel) = host_channel();
    let (_peer, proof) = federation_control_fixture::establish(&channel).await;
    let lease = channel.current_lease().unwrap();
    let client = FederationRpcClient::new(1, channel.clone(), Arc::new(|| Ok(()))).unwrap();
    app.inner.lock().unwrap().set_federation_durable(false, "test_install_hold");
    let before = snapshot(&app);
    // The same predicate used by both monitor loops and diagnostic health handling.
    for _ in 0..70 {
        assert!(matches!(channel.current_authority_checked(false), Err(error) if error.status == StatusCode::LOCKED));
        assert!(client.retain_health_refusal(&lease, &ApiError::from_status(StatusCode::LOCKED, "held")));
    }
    assert_eq!(snapshot(&app), before, "idle probes must not append refusal observations");
    // Genuine inbound operation checks still record, even during a long hold.
    for _ in 0..70 {
        assert!(matches!(channel.current_authority(), Err(error) if error.status == StatusCode::LOCKED));
        app.inner.lock().unwrap().federation_durability_diagnostics.refusal(false, "central_authority_durability", 409);
    }
    let after = snapshot(&app);
    let before_rows = before["records"].as_array().unwrap();
    let rows = after["records"].as_array().unwrap();
    assert_eq!(&rows[..before_rows.len()], before_rows.as_slice());
    assert_eq!(rows.len(), before_rows.len() + 2);
    assert_eq!(after["evicted"], before["evicted"]);
    for row in &rows[before_rows.len()..] {
        assert_eq!(row["windowId"], before_rows.last().unwrap()["windowId"]);
        assert_eq!(row["count"], 70);
        assert!(row["lastFalseForMs"].as_u64().unwrap() >= row["falseForMs"].as_u64().unwrap());
        assert!(chrono::DateTime::parse_from_rfc3339(row["lastAt"].as_str().unwrap()).is_ok());
    }
    app.inner.lock().unwrap().set_federation_durable(true, "test_settled");
    assert!(channel.current_authority().is_ok());
    drop(proof);
}
