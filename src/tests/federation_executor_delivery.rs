// Exercises authenticated wake admission and its exact persisted queue identity.
// Loopback router calls and fresh AppState loads are not SSH or OS-crash proof.
use super::*;

const WAKE_ROUTE: &str = "/federation/v1/executor/wake";

struct Fixture {
    central: AppState,
    satellite: AppState,
    native: String,
    target: FederationMailboxBinding,
    channel: FederationChannel,
    lease: FederationLinkLease,
    outbound: FederationChannel,
    _proof: federation_control_fixture::Attested,
    request: Value,
    queue_driver: tokio::sync::Mutex<()>,
}

impl Fixture {
    fn new() -> Self {
        std::thread::spawn(|| {
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
                .block_on(Self::new_attested())
        }).join().unwrap()
    }

    async fn new_attested() -> Self {
        let central = federation_mailboxes::central_store();
        let base = federation_registry::domain_state();
        let path = resolve_coordination_persistence_path(base.persistence_path.as_path());
        let satellite = AppState {
            mailbox_store: Arc::new(MailboxStore::open(&path).unwrap()),
            ..base
        };
        let native = test_session_id(&satellite, Agent::Codex);
        let (host, native_creation) = {
            let mut inner = satellite.inner.lock().unwrap();
            let host = inner.federation.local.clone().unwrap();
            let index = inner.find_session_index(&native).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            // Admission must not turn a paused session into a running one.
            record.orchestrator_auto_dispatch_blocked = true;
            (host, record.federation_native_creation_id.clone())
        };
        let mut enrolled = federation_registry::peer(48001);
        enrolled.identity = host;
        central.enroll_federation_peer(enrolled.clone()).unwrap();
        let peer = central.federation_peer_snapshot(&enrolled.identity.host_id).unwrap();
        satellite.configure_federation(FederationSettings {
            enabled: true,
            role: FederationRole::Satellite,
            central: Some(peer.local.clone()),
        }).unwrap();
        enrolled.identity = peer.local.clone();
        enrolled.display_name = "Central".to_owned();
        enrolled.record_generation = 1;
        enrolled.transport_generation = 1;
        enrolled.membership = FederationMembershipState::Enrolled;
        enrolled.verification = None;
        enrolled.last_observed = None;
        satellite.enroll_federation_peer(enrolled).unwrap();
        let reverse = satellite.federation_peer_snapshot(&peer.local.host_id).unwrap();
        let channel = FederationChannel::for_app(
            reverse,
            FederationPairCapability::new("a".repeat(64)).unwrap(),
            satellite.clone(),
        );
        let outbound = FederationChannel::for_app(
            peer.clone(), FederationPairCapability::new("a".repeat(64)).unwrap(), central.clone(),
        );
        // Real mutual control proof installs both request/response MAC contexts
        // before placement. The guards own this fixture's exact incarnations.
        let proof = federation_control_fixture::attest(&outbound, &channel).await;
        let connected = central.federation_peer_snapshot(&peer.peer.host_id).unwrap();
        central.verify_federation_peer(&connected, federation_registry::checks()).unwrap();
        let verified = central.federation_peer_snapshot(&peer.peer.host_id).unwrap();
        central.transition_federation_peer(&verified, FederationMembershipState::Active).unwrap();
        let current = central.federation_peer_snapshot(&peer.peer.host_id).unwrap();
        let placement = central.reserve_federation_placement(
            &current, "wake-placement".to_owned(), native.clone(), 0, 0,
        ).unwrap();
        let target = FederationMailboxBinding {
            central: current.local.clone(), identity: placement.identity.clone(),
            placement_generation: placement.placement_generation,
        };
        let reverse = satellite.federation_peer_snapshot(&peer.local.host_id).unwrap();
        satellite.install_federation_mailbox_binding(&reverse, target.clone(), &native_creation).unwrap();
        let lease = channel.current_lease().unwrap();
        let outbound_lease = outbound.current_lease().unwrap();
        let actor = FederationMailboxActor { binding: target.clone(), name: "Satellite".into() };
        central.execute_federation_mailbox(FederationMailboxRequest {
            protocol_version: FEDERATION_MAILBOX_WIRE_VERSION,
            schema_version: FEDERATION_MAILBOX_SCHEMA_VERSION.to_owned(),
            central: peer.local.clone(),
            actor: Some(actor),
            operation: FederationMailboxOperation::Register,
        }, Some(outbound_lease)).unwrap();
        let (sender, participant) = federation_mailboxes::local_participant(&central, "Sender");
        let sent = central.execute_federation_mailbox(federation_mailboxes::request(
            &central, &sender, FederationMailboxOperation::Send {
                target: target.participant_key(), message: "ordinary agent work".into(),
                key: "one-message".into(), topic: Some("handoff".into()), state_stamp: None,
            },
        ), None).unwrap().value;
        let request = json!({
            "protocolVersion": 1,
            "operationKey": format!("mailbox-wake:{}", sent["messageId"].as_str().unwrap()),
            "target": target,
            "wake": {
                "mailboxId": sent["mailboxId"], "messageId": sent["messageId"],
                "sequence": sent["sequence"], "unreadCount": 1,
                "senderSessionId": participant.session_id, "senderName": "Sender",
                "topic": "handoff"
            }
        });
        Self { central, satellite, native, target, channel, lease, outbound,
            _proof: proof, request, queue_driver: tokio::sync::Mutex::new(()) }
    }

    async fn receive(&self, request: &Value) -> (StatusCode, Value) {
        let result = self.receive_raw(request).await;
        if result.0 == StatusCode::ACCEPTED {
            let _owner = self.queue_driver.lock().await;
            // Tests needing associated content explicitly run/join the same
            // owned post-receive worker as the production supervisor. Only
            // this driver is serialized; authenticated receives still race.
            let mut worker = FederationExecutorActivationWorker::default();
            worker.tick(&self.satellite, true).await;
            worker.shutdown(&self.satellite).await;
        }
        result
    }

    async fn receive_raw(&self, request: &Value) -> (StatusCode, Value) {
        let check = self.channel.clone();
        let router = federation_router(self.channel.clone(),
            Arc::new(move || check.current_authority().map(|_| ())));
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        let context = self.outbound.security_context().unwrap();
        let checked = self.outbound.clone();
        let client = FederationRpcClient::new(1, self.outbound.clone(),
            Arc::new(move || checked.current_authority().map(|_| ()))).unwrap();
        let counter = client.allocate_authenticated_counter(&context).await.unwrap();
        let signed = federation_control_fixture::signed(&self.outbound,
            counter, "POST", WAKE_ROUTE,
            headers, &serde_json::to_vec(request).unwrap());
        let response = router.oneshot(signed)
            .await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        context.settle(counter);
        let value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({
            "nonJsonBody": String::from_utf8_lossy(&body)
        }));
        (status, value)
    }

    fn operation(&self) -> FederationExecutorOperation {
        FederationExecutorOperation {
            central_host_id: self.target.central.host_id.clone(),
            central_epoch: self.target.central.host_generation,
            operation_key: self.request["operationKey"].as_str().unwrap().to_owned(),
            target_host_id: self.target.identity.host_id.clone(),
            enrollment_generation: self.channel.snapshot.enrollment_generation,
            session_id: self.target.identity.session_id.clone(),
            creation_id: self.target.identity.creation_id.clone(),
            placement_generation: self.target.placement_generation,
            command_kind: "wake".into(),
            payload: serde_json::to_string(&self.request["wake"]).unwrap(),
        }
    }

    fn inbox(&self) -> Option<FederationExecutorRecord> {
        federation_executor_read(&self.satellite.mailbox_store.connection().unwrap(),
            &self.target.central.host_id, self.request["operationKey"].as_str().unwrap()).unwrap()
    }

    fn queue(&self, app: &AppState) -> Vec<QueuedPromptRecord> {
        let inner = app.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&self.native).unwrap()].queued_prompts
            .iter().cloned().collect()
    }

    async fn queued(&self) -> QueuedPromptRecord {
        tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
            loop {
                let queue = self.queue(&self.satellite);
                if let Some(entry) = queue.into_iter().find(|entry| entry.pending_prompt.source
                    .as_ref().and_then(|source| source.mailbox.as_ref())
                    .is_some_and(|mailbox| mailbox.message_id == self.request["wake"]["messageId"].as_str().unwrap()))
                { return entry; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("the durably received wake must acquire its exact queue association")
    }

    fn reload(&self) -> Reloaded {
        let inner = self.satellite.inner.lock().unwrap();
        self.satellite.persist_internal_locked(&inner).unwrap();
        drop(inner);
        // The original test state has no running persist worker or runtime.
        // Keep it alive only to retain its disposable home while the real boot reads it.
        Reloaded(AppState::new_with_paths(
            self.satellite.persistence_path.parent().unwrap().to_string_lossy().into_owned(),
            self.satellite.persistence_path.as_ref().clone(),
            self.satellite.orchestrator_templates_path.as_ref().clone(),
        ).unwrap())
    }
}

struct Reloaded(AppState);
impl Drop for Reloaded {
    fn drop(&mut self) { self.0.shutdown_persist_blocking(); }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Loopback tests install handles without a supervising runtime. Release
        // its registry ownership just as the production supervisor does: each
        // handle's channel retains the AppState containing this same registry.
        for app in [&self.central, &self.satellite] {
            app.federation_peer_handles.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_runtime_acceptance_is_durable_and_keeps_mailbox_queue_source() {
    let fixture = Fixture::new();
    let (status, body) = fixture.receive(&fixture.request).await;
    assert_eq!(status, StatusCode::ACCEPTED, "authenticated durable wake admission: {body}");
    let inbox = fixture.inbox().expect("acceptance must follow inbox COMMIT");
    assert_eq!(inbox.operation, fixture.operation());
    assert!(inbox.runtime_ack.is_none(), "queue admission is not runtime acceptance");
    assert!(inbox.processed_evidence.is_none(), "delivery is not processing");
    let queued = fixture.queued().await;
    assert_eq!(queued.source, QueuedPromptSource::Mailbox);
    let source = queued.pending_prompt.source.as_ref().unwrap().mailbox.as_ref().unwrap();
    assert_eq!(source.message_id, fixture.request["wake"]["messageId"].as_str().unwrap());
    let inner = fixture.satellite.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
    assert!(record.orchestrator_auto_dispatch_blocked, "remote acceptance cannot resume a paused queue");
    assert_eq!(record.queued_prompts.len(), 1);
    assert!(record.prompt_deliveries.contains_key(&queued.pending_prompt.id),
        "remote wake must use the production write-ahead delivery gate");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_runtime_concurrent_duplicate_has_one_logical_queue_entry() {
    let fixture = Fixture::new();
    let (left, right) = tokio::join!(fixture.receive(&fixture.request), fixture.receive(&fixture.request));
    assert_eq!(left.0, StatusCode::ACCEPTED, "first exact request: {}", left.1);
    assert_eq!(right.0, StatusCode::ACCEPTED, "concurrent exact request: {}", right.1);
    let id = fixture.queued().await.pending_prompt.id;
    let (status, _) = fixture.receive(&fixture.request).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let queue = fixture.queue(&fixture.satellite);
    assert_eq!(queue.len(), 1, "duplicates cannot create another queue identity");
    assert_eq!(queue[0].pending_prompt.id, id);
    let connection = fixture.satellite.mailbox_store.connection().unwrap();
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM federation_executor_operations", [],
        |row| row.get::<_, u64>(0)).unwrap(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_runtime_same_key_changed_wake_conflicts_without_second_queue_entry() {
    let fixture = Fixture::new();
    let (status, body) = fixture.receive(&fixture.request).await;
    assert_eq!(status, StatusCode::ACCEPTED, "initial admission: {body}");
    fixture.queued().await;
    let mut changed = fixture.request.clone();
    changed["wake"]["topic"] = json!("changed under the same operation key");
    let (status, _) = fixture.receive(&changed).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(fixture.queue(&fixture.satellite).len(), 1);
    assert_eq!(fixture.inbox().unwrap().operation, fixture.operation());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_runtime_recreated_target_is_refused_before_receive_commit() {
    let fixture = Fixture::new();
    let mut stale = fixture.request.clone();
    stale["target"]["identity"]["creationId"] = json!(Uuid::new_v4().to_string());
    let (status, body) = fixture.receive(&stale).await;
    assert_eq!(status, StatusCode::CONFLICT, "wrong captured creation: {body}");
    assert!(fixture.inbox().is_none());
    assert!(fixture.queue(&fixture.satellite).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_runtime_boot_recovers_received_operation_without_resuming_queue() {
    let fixture = Fixture::new();
    fixture.satellite.mailbox_store.admit_federation_executor_operation(
        &fixture.operation(), || Ok(()),
    ).unwrap();
    assert!(fixture.queue(&fixture.satellite).is_empty());
    let reloaded = fixture.reload();
    let queue = fixture.queue(&reloaded.0);
    assert_eq!(queue.len(), 1, "post-listen boot must recover received-to-queue gap");
    let source = queue[0].pending_prompt.source.as_ref().unwrap().mailbox.as_ref().unwrap();
    assert_eq!(source.message_id, fixture.request["wake"]["messageId"].as_str().unwrap());
    let inner = reloaded.0.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(record.prompt_deliveries.values().all(|delivery|
        delivery.disposition == PromptDeliveryDisposition::Pending));
}

#[test]
fn federation_m4_runtime_fixture_central_append_owns_one_pending_wake_and_no_satellite_replica() {
    let fixture = Fixture::new();
    let connection = fixture.central.mailbox_store.connection().unwrap();
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM federation_mailbox_wake_obligations WHERE state='pending'",
        [], |row| row.get::<_, u64>(0)).unwrap(), 1);
    let connection = fixture.satellite.mailbox_store.connection().unwrap();
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM mailbox_messages", [],
        |row| row.get::<_, u64>(0)).unwrap(), 0);
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM federation_executor_operations", [],
        |row| row.get::<_, u64>(0)).unwrap(), 0);
}

mod settlement {
    use super::*;
    include!("federation_executor_delivery_settlement.rs");
}

mod recovery_regressions {
    use super::*;
    include!("federation_executor_recovery_regressions.rs");
}

mod observation {
    use super::*;
    include!("federation_executor_observation.rs");
}
