// Owns the production-shape native relay wake witness: only central verifies
// and activates the pair, so the satellite's own record of its central stays
// enrolled, as on deployed hosts. Does not own the both-sides-active relay
// controls in relay.rs or the revocation holds in relay_review*.rs.
use super::*;

struct ProxySend {
    caller: String,
    _proxy: String,
    central_api: DomainServer,
    central_domain: DomainServer,
    _handles: RelayHandleGuard,
}

impl ProxySend {
    /// A central caller and a central proxy for the satellite's native root,
    /// with each host's peer handle to the other, as on a connected pair.
    async fn new(fixture: &Fixture) -> Self {
        let caller = test_session_id(&fixture.central, Agent::Codex);
        let proxy = test_session_id(&fixture.central, Agent::Codex);
        let peer = fixture.channel.current_authority().unwrap();
        {
            let mut inner = fixture.central.inner.lock().unwrap();
            inner.preferences.remotes.push(RemoteConfig {
                id: "native-relay-mac".into(),
                name: "Known Mac".into(),
                transport: RemoteTransport::Ssh,
                enabled: true,
                host: Some(peer.endpoint.ssh_host.clone()),
                port: Some(peer.endpoint.ssh_port),
                user: Some(peer.endpoint.ssh_user.clone()),
            });
            let index = inner.find_session_index(&proxy).unwrap();
            inner.sessions[index].remote_id = Some("native-relay-mac".into());
            inner.sessions[index].remote_session_id = Some(fixture.native.clone());
            let index = inner.find_session_index(&caller).unwrap();
            inner.sessions[index].orchestrator_auto_dispatch_blocked = true;
        }
        {
            // Keep the admitted wake queued so the test can inspect it.
            let mut inner = fixture.satellite.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.native).unwrap();
            inner.sessions[index].orchestrator_auto_dispatch_blocked = true;
        }
        let handles = RelayHandleGuard(vec![fixture.central.clone(), fixture.satellite.clone()]);
        fixture.central.federation_peer_handles.lock().unwrap().insert(
            peer.peer.host_id.clone(),
            Arc::new(FederationPeerHandle { channel: fixture.channel.clone(), rpc: fixture.rpc.clone() }),
        );
        let checked = fixture.channel.clone();
        let central_domain = DomainServer::start(federation_router(
            fixture.channel.clone(),
            Arc::new(move || checked.current_authority().map(|_| ())),
        ))
        .await;
        let checked = fixture.satellite_channel.clone();
        let reverse = FederationRpcClient::new(
            central_domain.port,
            fixture.satellite_channel.clone(),
            Arc::new(move || checked.current_authority().map(|_| ())),
        )
        .unwrap();
        fixture.satellite.federation_peer_handles.lock().unwrap().insert(
            peer.local.host_id.clone(),
            Arc::new(FederationPeerHandle { channel: fixture.satellite_channel.clone(), rpc: reverse }),
        );
        let central_api = DomainServer::start(app_router(fixture.central.clone())).await;
        Self { caller, _proxy: proxy, central_api, central_domain, _handles: handles }
    }

    async fn send(&self, fixture: &Fixture, key: &str) -> MailboxAppendReceipt {
        let response = fixture
            .client
            .post(format!("http://127.0.0.1:{}/api/sessions/{}/mailboxes/send", self.central_api.port, self.caller))
            .json(&json!({"targetSessionId": format!("host:{}::{}", fixture.channel.snapshot.peer.host_id, fixture.native), "message": "routine body stays at the central home",
                "idempotencyKey": key}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        response.json().await.unwrap()
    }

    async fn finish(mut self) {
        self.central_api.finish().await;
        self.central_domain.finish().await;
    }

    /// Undelivered home wake obligations at central.
    fn pending_home_wakes(fixture: &Fixture) -> u64 {
        fixture.central.mailbox_store.connection().unwrap()
            .query_row("SELECT count(*) FROM mailbox_relay_wakes WHERE delivered=0", [], |row| row.get(0))
            .unwrap()
    }
}

/// The satellite's queued wake for `mailbox`, if one was admitted.
fn queued_relay_wake(fixture: &Fixture, mailbox: &str) -> Option<String> {
    let inner = fixture.satellite.inner.lock().unwrap();
    let native = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
    native.queued_prompts.iter().find_map(|queued| {
        let source = queued.pending_prompt.source.as_ref()?.mailbox.as_ref()?;
        (source.mailbox_id == mailbox).then(|| queued.pending_prompt.text.clone())
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_wake_reaches_a_satellite_whose_record_of_central_is_only_enrolled() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let central_host = fixture.channel.current_authority().unwrap().local;
    let satellite_host = fixture.channel.current_authority().unwrap().peer;
    assert_eq!(fixture.central.federation_peer_snapshot(&satellite_host.host_id).unwrap().membership,
        FederationMembershipState::Active, "central activated its satellite");
    assert_eq!(fixture.satellite.federation_peer_snapshot(&central_host.host_id).unwrap().membership,
        FederationMembershipState::Enrolled, "a deployed satellite never activates its central");
    let proxy = ProxySend::new(&fixture).await;
    let sent = proxy.send(&fixture, "production-shape").await;
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let wake = queued_relay_wake(&fixture, &sent.mailbox_id)
        .expect("the satellite admits a wake from its enrolled central home");
    assert!(wake.contains("[TermAl mailbox notification]") && !wake.contains("routine body"), "{wake}");
    assert_eq!(ProxySend::pending_home_wakes(&fixture), 0, "the home settles the delivered obligation");
    assert!(fixture.central.mailbox_store.relay_home_wake_refusals.snapshot().0.is_empty());
    proxy.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_boot_boundary_retries_after_missing_link_without_replaying_mail() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let proxy = ProxySend::new(&fixture).await;
    let sent = proxy.send(&fixture, "boot-waits-for-link").await;
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    assert!(queued_relay_wake(&fixture, &sent.mailbox_id).is_some());
    // The restored process has lost its former runtime queue. Keep the exact
    // durable boundary and message; only process-owned delivery is gone.
    fixture.satellite.mailbox_store.connection().unwrap().execute(
        "UPDATE mailbox_relay_notifications SET handed_off=1,queued=1", [],
    ).unwrap();
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        inner.sessions[index].queued_prompts.clear();
        sync_pending_prompts(&mut inner.sessions[index]);
    }
    fixture.satellite_channel.invalidate();
    let mut boot = MailboxRelayBootRecovery::default();
    fixture.satellite.reconcile_mailbox_relay_notifications_after_boot(&mut boot).await.unwrap();
    assert_eq!(boot.retry.len(), 1, "the unavailable home retains the original boundary");
    assert!(!boot.complete());
    assert!(queued_relay_wake(&fixture, &sent.mailbox_id).is_none());
    fixture.reconnect().await;
    fixture.satellite.reconcile_mailbox_relay_notifications_after_boot(&mut boot).await.unwrap();
    assert!(boot.complete(), "fresh authentication releases the same retained boot boundary");
    fixture.satellite.run_mailbox_relay_wake_batch(0, 0).await;
    let wake = queued_relay_wake(&fixture, &sent.mailbox_id).expect("unread metadata is rearmed");
    assert!(wake.contains("[TermAl mailbox notification]"));
    assert!(!wake.contains("routine body"));
    let locator = MailboxRelayLocator::decode(&sent.mailbox_id, false).unwrap().unwrap();
    let count: u64 = fixture.central.mailbox_store.connection().unwrap().query_row(
        "SELECT count(*) FROM mailbox_messages WHERE mailbox_id=?1", [&locator.id], |row| row.get(0),
    ).unwrap();
    assert_eq!(count, 1, "boot recovery never replays the committed message");
    proxy.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_hostname_migration_recovers_send_read_ack_without_reference_republication() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let mut fixture = Fixture::production_shape().await;
    let mut proxy = ProxySend::new(&fixture).await;
    let old = fixture.channel.current_lease().unwrap();
    let enrolled = fixture.channel.current_authority().unwrap();
    let version = fixture.central.inner.lock().unwrap().federation.version;
    fixture.central.update_federation_ssh_host(&enrolled.peer.host_id, version, "moved.local".into()).unwrap();
    assert!(old.with_current(&fixture.central, |_, _| Ok(())).is_err());
    let mut successor = FederationChannel::for_app(
        fixture.central.federation_peer_snapshot(&enrolled.peer.host_id).unwrap(),
        FederationPairCapability::new("a".repeat(64)).unwrap(), fixture.central.clone(),
    );
    successor.control.clock = FederationControlClock::scripted_io();
    let checked = successor.clone();
    fixture.rpc = FederationRpcClient::new(fixture.server.port, successor.clone(),
        Arc::new(move || checked.current_authority().map(|_| ()))).unwrap();
    fixture.channel = successor;
    fixture.reconnect().await;
    proxy.central_domain.finish().await;
    let checked = fixture.channel.clone();
    proxy.central_domain = DomainServer::start(federation_router(fixture.channel.clone(),
        Arc::new(move || checked.current_authority().map(|_| ())))).await;
    fixture.central.federation_peer_handles.lock().unwrap().insert(enrolled.peer.host_id.clone(),
        Arc::new(FederationPeerHandle { channel: fixture.channel.clone(), rpc: fixture.rpc.clone() }));
    let checked = fixture.satellite_channel.clone();
    let reverse = FederationRpcClient::new(proxy.central_domain.port, fixture.satellite_channel.clone(),
        Arc::new(move || checked.current_authority().map(|_| ()))).unwrap();
    fixture.satellite.federation_peer_handles.lock().unwrap().insert(enrolled.local.host_id.clone(),
        Arc::new(FederationPeerHandle { channel: fixture.satellite_channel.clone(), rpc: reverse }));
    let sent = proxy.send(&fixture, "mail-after-address-change").await;
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    assert!(queued_relay_wake(&fixture, &sent.mailbox_id).is_some());
    let page = read_mailbox(AxumPath((fixture.native.clone(), sent.mailbox_id.clone())),
        State(fixture.satellite.clone()), Json(ReadMailboxRequest {
            after_sequence: None, limit: 20, issue_receipt: true, read_key: None,
        })).await.unwrap().0;
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].id, sent.message_id);
    let summary = acknowledge_mailbox(AxumPath((fixture.native.clone(), sent.mailbox_id.clone())),
        State(fixture.satellite.clone()), Json(AcknowledgeMailboxRequest {
            receipt: page.receipt, expected_processed_through: None,
            processed_through: None, no_continuation: false,
        })).await.unwrap().0;
    assert_eq!(summary.unread_count, 0);
    let fresh = fixture.channel.current_authority().unwrap();
    assert_eq!(fresh.enrollment_generation, enrolled.enrollment_generation);
    assert_eq!(fresh.peer, enrolled.peer);
    let references = fixture.central.inner.lock().unwrap().federation.mailbox_references.len();
    assert_eq!(references, 0, "ordinary native mail never needs a published reference");
    proxy.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn satellite_accepts_its_central_short_of_revocation_and_only_at_the_current_enrollment() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let central = fixture.channel.current_authority().unwrap().local;
    let enrollment = fixture.satellite.federation_peer_snapshot(&central.host_id).unwrap().enrollment_generation;
    let accepts = |membership: FederationMembershipState, generation: u64| {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let saved = inner.federation.peers[&central.host_id].membership;
        inner.federation.peers.get_mut(&central.host_id).unwrap().membership = membership;
        let accepted = federation_satellite_central_current_locked(&inner, &central, generation);
        inner.federation.peers.get_mut(&central.host_id).unwrap().membership = saved;
        accepted
    };
    use FederationMembershipState::{Active, Draining, Enrolled, Revoked, Verified};
    for membership in [Enrolled, Verified, Active, Draining] {
        assert!(accepts(membership, enrollment), "{membership:?}");
    }
    assert!(!accepts(Revoked, enrollment));
    assert!(!accepts(Enrolled, enrollment + 1), "another enrollment is not current");
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        inner.federation.admission_quarantine = Some(FederationQuarantineReason::StoreBindingChanged);
        assert!(!federation_satellite_central_current_locked(&inner, &central, enrollment));
        inner.federation.admission_quarantine = None;
    }
    // Central's own view of a satellite is never a satellite's central.
    let satellite = fixture.channel.current_authority().unwrap().peer;
    let inner = fixture.central.inner.lock().unwrap();
    assert!(!federation_satellite_central_current_locked(&inner, &satellite, enrollment));
    drop(inner);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_refused_home_wake_is_visible_until_it_is_delivered() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let proxy = ProxySend::new(&fixture).await;
    let sent = proxy.send(&fixture, "refused-then-delivered").await;
    let set_hidden = |hidden: bool| {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        inner.sessions[index].hidden = hidden;
    };
    let diagnostics = || async {
        let response = fixture.client
            .get(format!("http://127.0.0.1:{}/api/diagnostics/mailbox-relay-wakes", proxy.central_api.port))
            .send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.json::<Value>().await.unwrap()
    };
    // The target can no longer be described: its wake is refused and held.
    set_hidden(true);
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    assert!(queued_relay_wake(&fixture, &sent.mailbox_id).is_none());
    let body = diagnostics().await;
    assert_eq!(body["pendingHomeWakes"], 1, "{body}");
    let refused = body["refusedHomeWakes"].as_array().unwrap();
    assert_eq!(refused.len(), 1, "{body}");
    assert_eq!(refused[0]["attempts"], 2, "{body}");
    assert_eq!(refused[0]["targetSessionId"], fixture.native, "{body}");
    assert!(refused[0]["status"].as_u64().is_some_and(|status| status >= 400), "{body}");
    assert!(!refused[0]["reason"].as_str().unwrap().is_empty(), "{body}");
    assert!(!body.to_string().contains("routine body"), "metadata only: {body}");
    // The same obligation is delivered once the target is back; the refusal clears.
    set_hidden(false);
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    assert!(queued_relay_wake(&fixture, &sent.mailbox_id).is_some());
    let body = diagnostics().await;
    assert_eq!((body["pendingHomeWakes"].clone(), body["refusedHomeWakes"].clone()), (json!(0), json!([])), "{body}");
    proxy.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_production_shape_holds_on_link_loss_and_delivers_on_a_fresh_proof() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let proxy = ProxySend::new(&fixture).await;
    let sent = proxy.send(&fixture, "link-loss").await;
    // Drop the actual proof owners: no current link, so the wake is held.
    drop(fixture.attested.lock().unwrap().take());
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    assert!(queued_relay_wake(&fixture, &sent.mailbox_id).is_none(), "link loss holds the wake");
    assert_eq!(ProxySend::pending_home_wakes(&fixture), 1);
    fixture.reconnect().await;
    // A fresh pair proof restores ordinary mail immediately. It retains the
    // operator's membership but does not fabricate current verification.
    let satellite = fixture.channel.snapshot.peer.host_id.clone();
    let peer = fixture.central.federation_peer_snapshot(&satellite).unwrap();
    assert_eq!(peer.membership, FederationMembershipState::Active);
    let lease = fixture.channel.current_lease().unwrap();
    let verification_current = fixture.central.inner.lock().unwrap().federation.peers[&satellite]
        .verified_on(Some(&lease.incarnation));
    assert!(!verification_current);
    let central = fixture.satellite_channel.snapshot.peer.host_id.clone();
    assert_eq!(fixture.satellite.federation_peer_snapshot(&central).unwrap().membership,
        FederationMembershipState::Enrolled);
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    assert!(queued_relay_wake(&fixture, &sent.mailbox_id).is_some(), "the fresh lease delivers the held wake");
    assert_eq!(ProxySend::pending_home_wakes(&fixture), 0);
    proxy.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_production_shape_holds_while_the_satellite_has_revoked_its_central() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let proxy = ProxySend::new(&fixture).await;
    let sent = proxy.send(&fixture, "revoked-central").await;
    let central = fixture.satellite_channel.snapshot.peer.host_id.clone();
    let set_membership = |membership: FederationMembershipState| {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        inner.federation.peers.get_mut(&central).unwrap().membership = membership;
    };
    set_membership(FederationMembershipState::Revoked);
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    assert!(queued_relay_wake(&fixture, &sent.mailbox_id).is_none(), "a revoked central holds the wake");
    assert_eq!(ProxySend::pending_home_wakes(&fixture), 1);
    let (refused, _) = fixture.central.mailbox_store.relay_home_wake_refusals.snapshot();
    assert_eq!(refused.len(), 1);
    assert!(refused[0].status >= 400 && !refused[0].reason.chars().any(char::is_control), "{refused:?}");
    // The same obligation is delivered once the record is enrolled again.
    set_membership(FederationMembershipState::Enrolled);
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    assert!(queued_relay_wake(&fixture, &sent.mailbox_id).is_some());
    assert_eq!(ProxySend::pending_home_wakes(&fixture), 0);
    proxy.finish().await;
    fixture.finish().await;
}

#[test]
fn mailbox_relay_refusal_reason_is_bounded_and_has_no_control_characters() {
    let reason = mailbox_relay_bounded_reason("held\nnext line \u{1b}[31mred\r");
    assert_eq!(reason, "held next line  [31mred ");
    let long = mailbox_relay_bounded_reason(&"é".repeat(400));
    assert!(long.len() <= MAILBOX_RELAY_HOME_WAKE_REASON_BYTES && long.chars().all(|c| c == 'é'));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_home_recovery_and_wake_keep_one_association_and_ack_owner() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for order in ["list-first", "wake-first", "concurrent"] {
        let fixture = Fixture::production_shape().await;
        let proxy = ProxySend::new(&fixture).await;
        let sent = proxy.send(&fixture, order).await;
        let locator = MailboxRelayLocator::decode(&sent.mailbox_id, false).unwrap().unwrap();
        let actor = fixture.satellite.native_mailbox_relay_descriptor(&fixture.native).unwrap().principal;
        assert!(fixture.satellite.mailbox_store.mailbox_relay_association(&actor, &locator.id).unwrap().is_none());
        let wake: MailboxRelayWake = {
            let connection = fixture.central.mailbox_store.connection().unwrap();
            let json: String = connection.query_row(
                "SELECT wake_json FROM mailbox_relay_wakes WHERE mailbox_id=?1", [&locator.id],
                |row| row.get(0)).unwrap();
            serde_json::from_str(&json).unwrap()
        };
        let recover = || fixture.satellite.recover_native_mailbox_relay_association(&actor, &locator);
        if order == "list-first" {
            recover().await.unwrap();
            fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
        } else if order == "wake-first" {
            fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
            recover().await.unwrap();
        } else {
            let (recovered, _) = tokio::join!(recover(), fixture.central.run_mailbox_relay_wake_batch(0, 0));
            recovered.unwrap();
        }
        assert_eq!(ProxySend::pending_home_wakes(&fixture), 0, "{order}");
        let conversation = fixture.satellite.mailbox_store.mailbox_relay_association(&actor, &locator.id)
            .unwrap().unwrap();
        assert_eq!(conversation, wake.conversation);
        let page = read_mailbox(AxumPath((fixture.native.clone(), sent.mailbox_id.clone())),
            State(fixture.satellite.clone()), Json(ReadMailboxRequest {
                after_sequence: None, limit: 20, issue_receipt: true, read_key: None,
            })).await.unwrap().0;
        assert!(page.messages.iter().any(|message| message.id == sent.message_id));
        let _ = acknowledge_mailbox(AxumPath((fixture.native.clone(), sent.mailbox_id.clone())),
            State(fixture.satellite.clone()), Json(AcknowledgeMailboxRequest {
                receipt: page.receipt, expected_processed_through: None,
                processed_through: None, no_continuation: false,
            })).await.unwrap();
        let notification = || {
            let connection = fixture.satellite.mailbox_store.connection().unwrap();
            connection.query_row(
                "SELECT acknowledged,handed_off,queued FROM mailbox_relay_notifications WHERE mailbox_id=?1",
                [&locator.id], |row| Ok((row.get::<_, bool>(0)?, row.get::<_, bool>(1)?, row.get::<_, bool>(2)?))).unwrap()
        };
        let before = notification();
        assert!(before.0, "the original receipt ACK belongs to the same native principal");
        recover().await.unwrap();
        request(&fixture.rpc, &fixture.channel, MailboxRelayOperation::Wake { wake }).await.unwrap();
        assert_eq!(notification(), before, "recovery and a duplicate wake retain notification ownership: {order}");
        let connection = fixture.satellite.mailbox_store.connection().unwrap();
        let row: (u64, u64) = connection.query_row(
            "SELECT COUNT(*),MAX(acknowledged_through) FROM mailbox_relay_associations WHERE mailbox_id=?1",
            [&locator.id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(row, (1, sent.sequence), "{order}");
        drop(connection);
        assert_eq!(fixture.satellite.mailbox_store.mailbox_relay_association(&actor, &locator.id).unwrap(), Some(conversation));
        proxy.finish().await;
        fixture.finish().await;
    }
}
