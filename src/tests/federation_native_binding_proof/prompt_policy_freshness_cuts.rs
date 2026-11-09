// Owns requester deadline origin and expiry between real authenticated RPCs
// and existing mutation/capture cuts. Only the per-channel monotonic clock is
// scripted; permissions, durability and MAC replies use the real fixture.
use super::*;

#[derive(Default)]
struct ObservationPhases {
    started: Option<std::time::Instant>,
    ready: usize,
    cut: usize,
}

fn instrument(
    channel: &mut FederationChannel,
    expire_at: Option<&'static str>,
    delay_after_reply: bool,
) -> Arc<Mutex<ObservationPhases>> {
    channel.control.clock = FederationControlClock::scripted();
    let clock = channel.control.clock.clone();
    let phases = Arc::new(Mutex::new(ObservationPhases::default()));
    let recorded = phases.clone();
    channel.control.observed = Arc::new(move |event, _| {
        let mut phases = recorded.lock().unwrap();
        if event == "promptPolicyObservationStarted" {
            phases.started = Some(clock.now());
        }
        if event == "promptPolicyObservationReady" {
            phases.ready += 1;
            if delay_after_reply {
                clock.advance(Duration::from_secs(5));
            }
        }
        if Some(event) == expire_at {
            phases.cut += 1;
            // The reply has already passed observe(). Expire exactly before
            // the actual owner recheck, without waiting or changing its basis.
            assert_eq!(phases.ready, 1);
            clock.advance(FEDERATION_PROMPT_OBSERVATION_LIFETIME);
        }
    });
    phases
}

fn register_handle(fixture: &Fixture, channel: FederationChannel) {
    fixture
        .central
        .federation_peer_handles
        .lock()
        .unwrap()
        .insert(
            channel.snapshot.peer.host_id.clone(),
            Arc::new(FederationPeerHandle {
                channel,
                rpc: fixture.rpc.clone(),
            }),
        );
}

async fn grant_fixture() -> (PolicyFixture, FederationPromptPermission) {
    let fixture = policy_fixture(true).await;
    let scope = scope(&fixture, true);
    let consent = consent(&fixture, &scope);
    let grant = fixture
        .central
        .issue_federation_prompt_grant_on_link(
            version(&fixture.central),
            "freshness-grant".into(),
            scope,
            consent.identity,
            &fixture.channel.current_lease().unwrap(),
            &fixture.rpc,
        )
        .await
        .unwrap();
    (fixture, grant)
}

fn request(grant: &FederationPromptPermission) -> remote_prompt_protocol::OwnPrompt {
    remote_prompt_protocol::OwnPrompt {
        reference: Uuid::parse_str(&grant.identity.id).unwrap(),
        idempotency_key: "freshness-exact-key".into(),
        text: "exact body".into(),
    }
}

fn retained(app: &AppState) -> serde_json::Value {
    serde_json::to_value(&app.inner.lock().unwrap().federation).unwrap()
}

fn durable(app: &AppState) -> serde_json::Value {
    let connection = rusqlite::Connection::open(app.persistence_path.as_path()).unwrap();
    let image = read_federation_persist_image(&connection).unwrap().unwrap();
    verify_federation_image_marker(&connection, &image).unwrap();
    image["domain"].clone()
}

#[tokio::test]
async fn federation_prompt_policy_capture_returns_exact_before_rpc_deadline_after_off_lock_work() {
    let (mut fixture, grant) = grant_fixture().await;
    let mut channel = fixture.channel.clone();
    let phases = instrument(&mut channel, None, true);
    register_handle(&fixture, channel.clone());
    let (_, _, _, _, freshness) = fixture
        .central
        .capture_remote_prompt_for_caller(&grant.scope.source.session_id, &request(&grant))
        .await
        .unwrap();
    let phases = phases.lock().unwrap();
    assert_eq!(phases.ready, 1);
    let original = phases.started.unwrap() + FEDERATION_PROMPT_OBSERVATION_LIFETIME;
    assert_eq!(
        freshness.deadline, original,
        "capture must return the exact before-RPC deadline, not mint one at return"
    );
    assert_eq!(
        channel.control.clock.now(),
        phases.started.unwrap() + Duration::from_secs(5)
    );
    freshness
        .ensure_fresh_at(original - Duration::from_nanos(1))
        .unwrap();
    assert!(freshness.ensure_fresh_at(original).is_err());
    drop(phases);
    fixture.server.finish().await;
}

#[tokio::test]
async fn federation_prompt_policy_grant_rechecks_expiry_inside_real_mutation() {
    for expire in [false, true] {
        let mut fixture = policy_fixture(true).await;
        let scope = scope(&fixture, true);
        let consent = consent(&fixture, &scope);
        let mut channel = fixture.channel.clone();
        let phases = instrument(
            &mut channel,
            expire.then_some("promptPolicyGrantMutation"),
            false,
        );
        let before = retained(&fixture.central);
        let stored = durable(&fixture.central);
        let result = fixture
            .central
            .issue_federation_prompt_grant_on_link(
                version(&fixture.central),
                "freshness-cut".into(),
                scope,
                consent.identity,
                &channel.current_lease().unwrap(),
                &fixture.rpc,
            )
            .await;
        assert_eq!(
            phases.lock().unwrap().ready,
            1,
            "real reply preceded mutation"
        );
        if expire {
            assert_eq!(result.unwrap_err().status, StatusCode::CONFLICT);
            assert_eq!(phases.lock().unwrap().cut, 1);
            assert_eq!(retained(&fixture.central), before);
            assert_eq!(durable(&fixture.central), stored);
        } else {
            let granted = result.unwrap();
            assert_eq!(granted.kind, FederationPromptPermissionKind::CallerGrant);
            assert_ne!(retained(&fixture.central), before);
            assert_ne!(durable(&fixture.central), stored);
        }
        fixture.server.finish().await;
    }
}

#[tokio::test]
async fn federation_prompt_policy_capture_rechecks_expiry_at_final_return_cut() {
    let (mut fixture, grant) = grant_fixture().await;
    let mut channel = fixture.channel.clone();
    let phases = instrument(&mut channel, Some("promptPolicyCaptureReturn"), false);
    register_handle(&fixture, channel);
    let before = retained(&fixture.central);
    let stored = durable(&fixture.central);
    let result = fixture
        .central
        .capture_remote_prompt_for_caller(&grant.scope.source.session_id, &request(&grant))
        .await;
    assert!(matches!(result, Err(error) if error.status == StatusCode::CONFLICT));
    assert_eq!(phases.lock().unwrap().ready, 1);
    assert_eq!(phases.lock().unwrap().cut, 1);
    assert_eq!(retained(&fixture.central), before);
    assert_eq!(durable(&fixture.central), stored);
    fixture.server.finish().await;
}

#[tokio::test]
async fn federation_prompt_policy_mirror_rechecks_expiry_inside_real_mutation() {
    for expire in [false, true] {
        let (mut fixture, grant) = grant_fixture().await;
        register_handle(&fixture, fixture.channel.clone());
        let (intent, proof, _, _, _) = fixture
            .central
            .capture_remote_prompt_for_caller(&grant.scope.source.session_id, &request(&grant))
            .await
            .unwrap();
        let mut reverse = DomainServer::start(policy_router(fixture.channel.clone())).await;
        let mut channel = fixture.satellite_channel.clone();
        let phases = instrument(
            &mut channel,
            expire.then_some("promptPolicyMirrorMutation"),
            false,
        );
        let check = channel.clone();
        let rpc = FederationRpcClient::new(
            reverse.port,
            channel.clone(),
            Arc::new(move || check.current_authority().map(|_| ())),
        )
        .unwrap();
        let before = retained(&fixture.satellite);
        let stored = durable(&fixture.satellite);
        let result = fixture
            .satellite
            .observe_remote_prompt_grant(&channel.current_lease().unwrap(), &rpc, &intent, &proof)
            .await;
        assert_eq!(phases.lock().unwrap().ready, 1);
        if expire {
            assert_eq!(result.unwrap_err().status, StatusCode::CONFLICT);
            assert_eq!(phases.lock().unwrap().cut, 1);
            assert_eq!(retained(&fixture.satellite), before);
            assert_eq!(durable(&fixture.satellite), stored);
        } else {
            result.unwrap();
            assert_ne!(retained(&fixture.satellite), before);
            assert_ne!(durable(&fixture.satellite), stored);
        }
        reverse.finish().await;
        fixture.server.finish().await;
    }
}

#[tokio::test]
async fn federation_prompt_policy_ack_rechecks_expiry_inside_real_mutation() {
    for expire in [false, true] {
        let (mut fixture, grant) = grant_fixture().await;
        fixture
            .central
            .revoke_federation_prompt_permission(version(&fixture.central), &grant.identity.id)
            .unwrap();
        let mut channel = fixture.channel.clone();
        let phases = instrument(
            &mut channel,
            expire.then_some("promptPolicyAckMutation"),
            false,
        );
        register_handle(&fixture, channel);
        let before = retained(&fixture.central);
        let stored = durable(&fixture.central);
        let result = fixture
            .central
            .confirm_remote_prompt_revocation(&grant.identity.id)
            .await;
        assert_eq!(phases.lock().unwrap().ready, 1);
        // Receiver really committed its ack; only the issuer's later recording
        // expires. Receiver facts never substitute for that local owner commit.
        let remote: FederationState = serde_json::from_value(durable(&fixture.satellite)).unwrap();
        assert_eq!(
            remote
                .prompt_permissions
                .unwrap()
                .find(&grant.identity.id)
                .unwrap()
                .receiver_observed_generation,
            Some(2)
        );
        if expire {
            assert_eq!(result.unwrap_err().status, StatusCode::CONFLICT);
            assert_eq!(phases.lock().unwrap().cut, 1);
            assert_eq!(retained(&fixture.central), before);
            assert_eq!(durable(&fixture.central), stored);
        } else {
            assert_eq!(result.unwrap().receiver_observed_generation, Some(2));
            assert_ne!(retained(&fixture.central), before);
            assert_ne!(durable(&fixture.central), stored);
        }
        fixture.server.finish().await;
    }
}
