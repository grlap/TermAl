// Owns authenticated read-only native-binding proof contract regressions.
// Does not provision SSH, install bindings over RPC, create references or prove
// live onboarding. Pre-existing bindings/checklists below are fixture setup;
// mutual proof uses production attestation over duplex IO; health and binding
// proof requests use the actual MAC-authenticated domain HTTP listener.
use super::*;

mod fixture_clock;
mod references;
mod relay;

const PROOF_PATH: &str = "/federation/v1/mailbox/native-binding";

struct DomainServer {
    stop: tokio::sync::watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<()>>,
    port: u16,
}

impl DomainServer {
    async fn start(router: Router) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move { federation_wait_stop(&mut stopped).await })
                .await
                .unwrap();
        });
        Self { stop, task: Some(task), port }
    }

    async fn finish(&mut self) {
        self.stop.send_replace(true);
        if let Some(task) = self.task.take() {
            tokio::time::timeout(phase_sync::DEADLOCK_GUARD, task)
                .await
                .expect("domain listener shutdown deadline")
                .unwrap();
        }
    }
}

impl Drop for DomainServer {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

struct Fixture {
    server: DomainServer,
    central: AppState,
    satellite: AppState,
    native: String,
    native_creation: String,
    binding: FederationMailboxBinding,
    channel: FederationChannel,
    satellite_channel: FederationChannel,
    attested: Mutex<Option<federation_control_fixture::Attested>>,
    rpc: FederationRpcClient,
    client: reqwest::Client,
}

impl Fixture {
    async fn new(install_binding: bool) -> Self {
        Self::with_router(install_binding, |router| router).await
    }

    async fn with_router(install_binding: bool, wrap: impl FnOnce(Router) -> Router) -> Self {
        let central = federation_mailboxes::central_store();
        let (caller, _) = federation_mailboxes::local_participant(&central, "Central");
        let base = federation_registry::domain_state();
        let path = resolve_coordination_persistence_path(base.persistence_path.as_path());
        let satellite = AppState {
            mailbox_store: Arc::new(MailboxStore::open(&path).unwrap()),
            ..base
        };
        let native = test_session_id(&satellite, Agent::Codex);
        assert_eq!(caller, native, "equal raw IDs deliberately name different hosts");
        let (satellite_host, native_creation) = {
            let inner = satellite.inner.lock().unwrap();
            (
                inner.federation.local.clone().unwrap(),
                inner.sessions[inner.find_session_index(&native).unwrap()]
                    .federation_native_creation_id.clone(),
            )
        };
        let central_host = central.inner.lock().unwrap().federation.local.clone().unwrap();
        assert_ne!(central_host, satellite_host);
        satellite.configure_federation(FederationSettings {
            enabled: true,
            role: FederationRole::Satellite,
            central: Some(central_host.clone()),
        }).unwrap();
        let mut remote = federation_registry::peer(48001);
        remote.identity = satellite_host.clone();
        central.enroll_federation_peer(remote.clone()).unwrap();
        remote.identity = central_host.clone();
        remote.display_name = "Central".to_owned();
        satellite.enroll_federation_peer(remote).unwrap();
        let mut satellite_channel = FederationChannel::for_app(
            satellite.federation_peer_snapshot(&central_host.host_id).unwrap(),
            FederationPairCapability::new("a".repeat(64)).unwrap(),
            satellite.clone(),
        );
        // Protocol deadlines, the attestation window included, measure a
        // scripted clock, as in the onboarding fixture: elapsed wall time on a
        // loaded host never expires them; only a test that advances it does.
        satellite_channel.control.clock = FederationControlClock::scripted_io();
        let check = satellite_channel.clone();
        let server = DomainServer::start(wrap(federation_router(
            satellite_channel.clone(),
            Arc::new(move || check.current_authority().map(|_| ())),
        ))).await;
        let mut channel = FederationChannel::for_app(
            central.federation_peer_snapshot(&satellite_host.host_id).unwrap(),
            FederationPairCapability::new("a".repeat(64)).unwrap(),
            central.clone(),
        );
        channel.control.clock = FederationControlClock::scripted_io();
        let check = channel.clone();
        let rpc = FederationRpcClient::new(
            server.port,
            channel.clone(),
            Arc::new(move || check.current_authority().map(|_| ())),
        ).unwrap();
        // Real mutual proof owns and durably records both incarnations. Keep
        // its exact cleanup guards; no activate() or replacement channel.
        let attested = federation_control_fixture::attest(&channel, &satellite_channel).await;
        let lease = channel.current_lease().unwrap();
        rpc.health(&lease).await.unwrap();
        for (app, peer_id) in [
            (&central, &satellite_host.host_id),
            (&satellite, &central_host.host_id),
        ] {
            // Fixture-scoped readiness only, not an O1 platform/wake result.
            let peer = app.federation_peer_snapshot(peer_id).unwrap();
            app.verify_federation_peer(&peer, federation_registry::checks()).unwrap();
            let peer = app.federation_peer_snapshot(peer_id).unwrap();
            app.transition_federation_peer(&peer, FederationMembershipState::Active).unwrap();
        }
        let peer = central.federation_peer_snapshot(&satellite_host.host_id).unwrap();
        let placement = central.reserve_federation_placement(
            &peer, "native-proof".to_owned(), native.clone(), 0, 0,
        ).unwrap();
        let binding = FederationMailboxBinding {
            central: central_host.clone(),
            identity: placement.identity,
            placement_generation: placement.placement_generation,
        };
        assert_ne!(binding.identity.creation_id, native_creation,
            "C placement identity is not the S-local native creation token");
        if install_binding {
            satellite.install_federation_mailbox_binding(
                &satellite.federation_peer_snapshot(&central_host.host_id).unwrap(),
                binding.clone(), &native_creation,
            ).unwrap();
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(phase_sync::DEADLOCK_GUARD)
            .build().unwrap();
        Self { server, central, satellite, native, native_creation, binding, channel,
            satellite_channel, attested: Mutex::new(Some(attested)), rpc, client }
    }

    async fn reconnect(&self) -> FederationLinkLease {
        let proof = federation_control_fixture::attest(&self.channel, &self.satellite_channel).await;
        let old = self.attested.lock().unwrap().replace(proof);
        drop(old); // exact old guards cannot retire the newly installed proof
        self.channel.current_lease().unwrap()
    }

    async fn proof(&self, request_id: &str) -> Result<Value, ApiError> {
        self.rpc.request(FederationRpcRoute::NativeBinding(
            self.channel.current_lease().unwrap(),
            FederationNativeBindingRequest {
                protocol_version: 1, request_id: request_id.to_owned(),
                binding: self.binding.clone(),
            },
        )).await
    }

    fn native_snapshot(&self) -> Value {
        let inner = self.satellite.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&self.native).unwrap()];
        json!({
            "creation":record.federation_native_creation_id,
            "binding":record.federation_mailbox_binding,
            "status":record.session.status,
            "messages":record.session.messages.len(),
            "generation":record.active_turn_generation,
            "federationVersion":inner.federation.version,
        })
    }

    async fn finish(mut self) {
        self.attested.lock().unwrap().take();
        self.server.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_native_binding_proof_real_owned_channel_control() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(true).await;
    let lease = fixture.channel.current_lease().unwrap();
    fixture.rpc.health(&lease).await.unwrap();
    assert_eq!(fixture.satellite.federation_mailbox_actor(&fixture.native)
        .unwrap().unwrap().binding, fixture.binding);
    assert!(fixture.central.ensure_federation_peer_current(
        &fixture.channel.current_authority().unwrap()).is_ok());
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_native_binding_proof_reports_native_and_placement_identities_distinctly() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(true).await;
    fixture.rpc.health(&fixture.channel.current_lease().unwrap()).await.unwrap();
    let before = fixture.native_snapshot();
    let request_id = Uuid::new_v4().to_string();
    let response = fixture.proof(&request_id).await;
    assert_eq!(fixture.native_snapshot(), before, "proof read must not mutate native state");
    let proof = response.expect("authenticated native-binding proof read is missing or refused");
    assert_eq!(proof["protocolVersion"], 1);
    assert_eq!(proof["requestId"], request_id);
    assert_eq!(proof["central"], serde_json::to_value(&fixture.binding.central).unwrap());
    assert_eq!(proof["host"], serde_json::to_value(
        &fixture.channel.current_authority().unwrap().peer).unwrap());
    assert_eq!(proof["nativeSessionId"], fixture.native);
    assert_eq!(proof["nativeCreationId"], fixture.native_creation);
    assert_eq!(proof["binding"], serde_json::to_value(&fixture.binding).unwrap());
    assert_eq!(proof["linkIncarnation"], fixture.channel.current_lease().unwrap().incarnation);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_native_binding_proof_missing_binding_is_not_send_side_opt_in() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    fixture.rpc.health(&fixture.channel.current_lease().unwrap()).await.unwrap();
    assert!(fixture.satellite.federation_mailbox_actor(&fixture.native).unwrap().is_none());
    let before = fixture.native_snapshot();
    let response = fixture.proof(&Uuid::new_v4().to_string()).await;
    assert_eq!(fixture.native_snapshot(), before, "proof lookup must not install a binding");
    assert_eq!(response.err().expect("unbound native proof must refuse").status, StatusCode::CONFLICT,
        "an unbound native root must visibly refuse the proof contract");
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_native_binding_proof_recreated_raw_id_cannot_use_old_binding() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(true).await;
    fixture.rpc.health(&fixture.channel.current_lease().unwrap()).await.unwrap();
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        inner.remove_session_at(index).unwrap();
        inner.next_session_number = fixture.native.trim_start_matches("session-").parse().unwrap();
    }
    assert_eq!(test_session_id(&fixture.satellite, Agent::Codex), fixture.native);
    let before = fixture.native_snapshot();
    assert_ne!(before["creation"], fixture.native_creation);
    assert_eq!(before["binding"], Value::Null);
    let response = fixture.proof(&Uuid::new_v4().to_string()).await;
    assert_eq!(fixture.native_snapshot(), before, "old proof request cannot adopt successor");
    assert_eq!(response.err().expect("recreated native proof must refuse").status, StatusCode::CONFLICT,
        "old placement binding cannot prove a recreated native root");
    fixture.finish().await;
}
