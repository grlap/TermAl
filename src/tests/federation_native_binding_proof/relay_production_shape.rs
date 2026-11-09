// Owns production-shape native relay wake, page-read and recovery witnesses: only central verifies
// and activates the pair, so the satellite's own record of its central stays
// enrolled, as on deployed hosts. Does not own the both-sides-active relay
// controls in relay.rs or the revocation holds in relay_review*.rs.
use super::*;

#[path = "relay_ack_association.rs"]
mod ack_association;

#[path = "relay_home_reasons.rs"]
mod home_reasons;

#[path = "relay_home_rpc_failures.rs"]
mod home_rpc_failures;

pub(super) struct ProxySend {
    caller: String,
    _proxy: String,
    central_api: DomainServer,
    central_domain: DomainServer,
    _handles: RelayHandleGuard,
}

impl ProxySend {
    /// A central caller and a central proxy for the satellite's native root,
    /// with each host's peer handle to the other, as on a connected pair.
    pub(super) async fn new(fixture: &Fixture) -> Self {
        Self::with_home_router(fixture, |router| router).await
    }

    async fn with_home_router(fixture: &Fixture, wrap: impl FnOnce(Router) -> Router) -> Self {
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
        let central_domain = DomainServer::start(wrap(federation_router(
            fixture.channel.clone(),
            Arc::new(move || checked.current_authority().map(|_| ())),
        )))
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

    pub(super) async fn finish(mut self) {
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
    let first = proxy.send(&fixture, "boot-acknowledged-first").await;
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let first_page = read_relay_page(&fixture.satellite, &fixture.native, &first.mailbox_id).await.unwrap();
    assert_eq!(first_page.messages.len(), 1);
    assert_eq!(first_page.messages[0].id, first.message_id);
    let first_receipt = first_page.receipt.expect("the first page issues an ACK receipt");
    let actor = fixture.satellite.native_mailbox_relay_descriptor(&fixture.native).unwrap().principal;
    let acknowledged = acknowledge_mailbox(
        AxumPath((fixture.native.clone(), first.mailbox_id.clone())),
        State(fixture.satellite.clone()), Json(AcknowledgeMailboxRequest {
            receipt: Some(first_receipt.clone()), expected_processed_through: None,
            processed_through: None, no_continuation: false,
        }),
    ).await.unwrap().0;
    assert!(first.sequence > 0);
    assert_eq!(acknowledged.participants.iter().find(|participant|
        participant.session_id == actor.participant_key()).unwrap().processed_through, first.sequence);
    assert_eq!(acknowledged.unread_count, 0);

    let sent = proxy.send(&fixture, "boot-waits-for-link").await;
    assert_eq!(sent.mailbox_id, first.mailbox_id);
    assert_eq!(sent.sequence, first.sequence + 1);
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    assert!(queued_relay_wake(&fixture, &sent.mailbox_id).is_some());
    let before = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await.unwrap();
    assert_eq!(before.processed_through, first.sequence);
    assert_eq!(before.messages.len(), 1);
    assert_eq!(before.messages[0].id, sent.message_id);
    assert_eq!(before.messages[0].sequence, sent.sequence);
    assert_eq!(before.messages[0].body, "routine body stays at the central home");
    let retained_receipt = before.receipt.expect("retain the second page receipt across reconnect");
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
    {
        let inner = fixture.satellite.inner.lock().unwrap();
        let native = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
        let sources = native.queued_prompts.iter().filter_map(|queued|
            queued.pending_prompt.source.as_ref()?.mailbox.as_ref())
            .filter(|source| source.mailbox_id == sent.mailbox_id).collect::<Vec<_>>();
        assert_eq!(sources.len(), 1, "only the retained unread boundary is rearmed");
        assert_eq!(sources[0].message_id, sent.message_id);
        assert_eq!(sources[0].sequence, sent.sequence);
    }
    let recovered = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await.unwrap();
    assert_eq!(recovered.processed_through, first.sequence);
    assert_eq!(recovered.messages.len(), 1, "the acknowledged first message is not replayed");
    assert_eq!(recovered.messages[0].id, sent.message_id);
    assert_eq!(recovered.messages[0].sequence, sent.sequence);
    assert_eq!(recovered.messages[0].body, before.messages[0].body);

    let old_ack = acknowledge_mailbox(
        AxumPath((fixture.native.clone(), sent.mailbox_id.clone())),
        State(fixture.satellite.clone()), Json(AcknowledgeMailboxRequest {
            receipt: Some(first_receipt), expected_processed_through: None,
            processed_through: None, no_continuation: false,
        }),
    ).await.unwrap().0;
    assert_eq!(old_ack.participants.iter().find(|participant|
        participant.session_id == actor.participant_key()).unwrap().processed_through, first.sequence);
    assert_eq!(old_ack.unread_count, 1, "an old ACK cannot consume the retained newer message");
    let final_ack = acknowledge_mailbox(
        AxumPath((fixture.native.clone(), sent.mailbox_id.clone())),
        State(fixture.satellite.clone()), Json(AcknowledgeMailboxRequest {
            receipt: Some(retained_receipt), expected_processed_through: None,
            processed_through: None, no_continuation: false,
        }),
    ).await.unwrap().0;
    assert_eq!(final_ack.participants.iter().find(|participant|
        participant.session_id == actor.participant_key()).unwrap().processed_through, sent.sequence);
    assert_eq!(final_ack.unread_count, 0, "the pre-outage second receipt still owns its exact boundary");
    let empty = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await.unwrap();
    assert_eq!(empty.processed_through, sent.sequence);
    assert!(empty.messages.is_empty());
    let locator = MailboxRelayLocator::decode(&sent.mailbox_id, false).unwrap().unwrap();
    let count: u64 = fixture.central.mailbox_store.connection().unwrap().query_row(
        "SELECT count(*) FROM mailbox_messages WHERE mailbox_id=?1", [&locator.id], |row| row.get(0),
    ).unwrap();
    assert_eq!(count, 2, "boot recovery never replays either committed message");
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

async fn read_relay_page(
    state: &AppState,
    session: &str,
    mailbox: &str,
) -> Result<MailboxReadResponse, ApiError> {
    read_mailbox(
        AxumPath((session.to_owned(), mailbox.to_owned())),
        State(state.clone()),
        Json(ReadMailboxRequest {
            after_sequence: None,
            limit: 20,
            issue_receipt: true,
            read_key: None,
        }),
    )
    .await
    .map(|page| page.0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_page_and_exact_message_agree_on_temporary_home_loss() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for loss in ["missing-handle", "stale-link"] {
        let fixture = Fixture::production_shape().await;
        let proxy = ProxySend::new(&fixture).await;
        let sent = proxy.send(&fixture, loss).await;
        read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await.unwrap();
        let home = fixture.satellite_channel.snapshot.peer.host_id.clone();
        let handle = if loss == "missing-handle" {
            fixture.satellite.federation_peer_handles.lock().unwrap().remove(&home)
        } else {
            fixture.satellite_channel.invalidate();
            None
        };
        let page = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await;
        let message = read_mailbox_message(
            AxumPath((fixture.native.clone(), sent.message_id.clone())), State(fixture.satellite.clone()),
        ).await;
        if let Some(handle) = handle {
            fixture.satellite.federation_peer_handles.lock().unwrap().insert(home, handle);
        } else {
            fixture.reconnect().await;
        }
        let restored_page = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await;
        let restored_message = read_mailbox_message(
            AxumPath((fixture.native.clone(), sent.message_id.clone())), State(fixture.satellite.clone()),
        ).await;
        proxy.finish().await;
        fixture.finish().await;

        let page = page.err().expect("an unavailable home cannot supply a page");
        let message = message.err().expect("an unavailable home cannot supply a message");
        assert_eq!(page.status, StatusCode::SERVICE_UNAVAILABLE, "{loss}: {}", page.message);
        assert_eq!(page.status, message.status, "{loss}");
        assert!(page.message.contains("try the read again later"), "{loss}: {}", page.message);
        assert!(!page.message.contains("refresh enrollment"), "{loss}: {}", page.message);
        assert!(restored_page.unwrap().messages.iter().any(|row| row.id == sent.message_id));
        assert_eq!(restored_message.unwrap().0.id, sent.message_id);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_missing_association_stale_home_recovers_after_reconnection() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let proxy = ProxySend::new(&fixture).await;
    let sent = proxy.send(&fixture, "missing-association-stale-home").await;
    let locator = MailboxRelayLocator::decode(&sent.mailbox_id, false).unwrap().unwrap();
    let actor = fixture.satellite.native_mailbox_relay_descriptor(&fixture.native).unwrap().principal;
    assert!(fixture.satellite.mailbox_store.mailbox_relay_association(&actor, &locator.id).unwrap().is_none());
    fixture.satellite_channel.invalidate();
    let refused = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await;
    let still_missing = fixture.satellite.mailbox_store.mailbox_relay_association(&actor, &locator.id).unwrap().is_none();
    fixture.reconnect().await;
    let recovered = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await;
    proxy.finish().await;
    fixture.finish().await;

    assert!(still_missing, "a stale lease cannot create membership");
    let error = refused.err().expect("a stale home link cannot recover an association");
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE, "{}", error.message);
    assert!(error.message.contains("federation link changed"), "{}", error.message);
    assert!(error.message.contains("try the read again later"), "{}", error.message);
    assert!(recovered.unwrap().messages.iter().any(|row| row.id == sent.message_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_recovery_preserves_local_writer_busy_safe_replay() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let proxy = ProxySend::new(&fixture).await;
    let sent = proxy.send(&fixture, "local-recovery-writer-busy").await;
    let locator = MailboxRelayLocator::decode(&sent.mailbox_id, false).unwrap().unwrap();
    let actor = fixture.satellite.native_mailbox_relay_descriptor(&fixture.native).unwrap().principal;
    let writer = fixture.satellite.mailbox_store.write_lock.clone();
    let held = lock_sqlite_state_writer(&writer);
    let app = fixture.satellite.clone();
    let session = fixture.native.clone();
    let mailbox = sent.mailbox_id.clone();
    let running = tokio::spawn(async move { read_relay_page(&app, &session, &mailbox).await });
    let refused = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, running).await.unwrap().unwrap();
    drop(held);
    let still_missing = fixture.satellite.mailbox_store.mailbox_relay_association(&actor, &locator.id).unwrap().is_none();
    let link_current = fixture.satellite_channel.current_lease().is_ok();
    let recovered = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await;
    proxy.finish().await;
    fixture.finish().await;

    assert!(still_missing && link_current, "the home answered but local admission committed nothing");
    let error = refused.err().expect("the held local writer refuses first-read recovery");
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(message_carries_safe_replay_clause(&error.message), "{}", error.message);
    assert!(error.message.contains("mailbox storage is temporarily busy"), "{}", error.message);
    assert!(!error.message.contains("home could not be reached"), "{}", error.message);
    assert!(!error.message.contains("connection recovers"), "{}", error.message);
    assert!(recovered.unwrap().messages.iter().any(|row| row.id == sent.message_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_recovery_commit_fence_link_loss_is_retryable() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let proxy = ProxySend::new(&fixture).await;
    let sent = proxy.send(&fixture, "recovery-commit-link-loss").await;
    let locator = MailboxRelayLocator::decode(&sent.mailbox_id, false).unwrap().unwrap();
    let actor = fixture.satellite.native_mailbox_relay_descriptor(&fixture.native).unwrap().principal;
    let channel = fixture.satellite_channel.clone();
    // Retire the actual lease synchronously after List and the tentative SQL
    // association write, immediately before its real authorization fence.
    *fixture.satellite.mailbox_store.before_relay_association_authorize.lock().unwrap() =
        Some(Box::new(move || { channel.invalidate(); }));
    let refused = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await;
    let boundary_consumed = fixture.satellite.mailbox_store.before_relay_association_authorize.lock().unwrap().is_none();
    let still_missing = fixture.satellite.mailbox_store.mailbox_relay_association(&actor, &locator.id).unwrap().is_none();
    fixture.reconnect().await;
    let recovered = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await;
    proxy.finish().await;
    fixture.finish().await;

    assert!(boundary_consumed, "the recovery transaction consumed the one-shot boundary");
    assert!(still_missing, "the invalidated commit fence rolls back the new association");
    let error = refused.err().expect("the old lease cannot commit recovery");
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE, "{}", error.message);
    assert!(error.message.contains("native mailbox relay link changed"), "{}", error.message);
    assert!(error.message.contains("try the read again later"), "{}", error.message);
    assert!(!error.message.contains("refresh enrollment"), "{}", error.message);
    assert!(recovered.unwrap().messages.iter().any(|row| row.id == sent.message_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_page_authenticated_statuses_cover_recovery_and_saved_reads() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for (saved, status) in [false, true].into_iter().flat_map(|saved| {
        [StatusCode::BAD_GATEWAY, StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::GATEWAY_TIMEOUT, StatusCode::FORBIDDEN].map(|status| (saved, status))
    }) {
        let fixture = Fixture::production_shape().await;
        let checked = fixture.channel.clone();
        let listener = FederationListenerState {
            channel: fixture.channel.clone(),
            ensure_current: Arc::new(move || checked.current_authority().map(|_| ())),
        };
        let refuse = Arc::new(AtomicBool::new(!saved));
        let refuse_request = refuse.clone();
        let proxy = ProxySend::with_home_router(&fixture, move |_| {
            Router::new().route(MAILBOX_RELAY_RPC_ROUTE, post(move |
                peer: axum::Extension<FederationAuthenticatedPeer>, request: Json<MailboxRelayRequest>,
            | {
                let refuse_request = refuse_request.clone();
                async move {
                    if refuse_request.load(Ordering::SeqCst) {
                        Err(ApiError::from_status(status, "test home status refusal"))
                    } else {
                        serve_mailbox_relay(peer, request).await
                    }
                }
            }))
                .layer(axum::middleware::from_fn_with_state(listener, federation_authenticate))
        }).await;
        let sent = proxy.send(&fixture, "unusable-home-response").await;
        let locator = MailboxRelayLocator::decode(&sent.mailbox_id, false).unwrap().unwrap();
        let actor = fixture.satellite.native_mailbox_relay_descriptor(&fixture.native).unwrap().principal;
        if saved {
            read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await.unwrap();
            refuse.store(true, Ordering::SeqCst);
        }
        let result = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await;
        let missing = fixture.satellite.mailbox_store.mailbox_relay_association(&actor, &locator.id).unwrap().is_none();
        let lease_current = fixture.satellite_channel.current_lease().is_ok();
        proxy.finish().await;
        fixture.finish().await;

        assert_eq!(missing, !saved, "a refusal neither creates nor removes the association");
        assert!(lease_current, "the authenticated {status} settles without fencing the link");
        let error = result.err().expect("the unavailable home cannot answer the page read");
        if status.is_server_error() {
            assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE, "{status}: {}", error.message);
            assert!(error.message.contains("native mailbox relay response unavailable"), "{}", error.message);
            assert!(error.message.contains("try the read again later"), "{}", error.message);
        } else {
            assert_eq!(error.status, status, "{}", error.message);
            assert!(error.message.contains("authenticated native mailbox relay refused"), "{}", error.message);
            assert!(!error.message.contains("try the read again later"), "{}", error.message);
        }
        assert!(!error.message.contains("idempotencyKey"), "{}", error.message);
        assert!(!error.message.contains("outcome may be unknown"), "{}", error.message);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_page_permanent_home_refusals_do_not_suggest_connection_recovery() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for refusal in ["revoked-without-handle", "unknown-host", "different-store"] {
        let fixture = Fixture::production_shape().await;
        let proxy = ProxySend::new(&fixture).await;
        let sent = proxy.send(&fixture, refusal).await;
        let mut locator = MailboxRelayLocator::decode(&sent.mailbox_id, false).unwrap().unwrap();
        let actor = fixture.satellite.native_mailbox_relay_descriptor(&fixture.native).unwrap().principal;
        if refusal == "unknown-host" {
            locator.home.host.host_id = Uuid::new_v4().to_string();
        } else if refusal == "different-store" {
            locator.home.store_id = Uuid::new_v4().to_string();
        } else {
            fixture.satellite.inner.lock().unwrap().federation.peers
                .get_mut(&locator.home.host.host_id).unwrap().membership = FederationMembershipState::Revoked;
            fixture.satellite.federation_peer_handles.lock().unwrap().remove(&locator.home.host.host_id);
        }
        let encoded = MailboxRelayLocator::encode(&locator.home, &locator.id, false).unwrap();
        let result = read_relay_page(&fixture.satellite, &fixture.native, &encoded).await;
        let missing = fixture.satellite.mailbox_store.mailbox_relay_association(&actor, &locator.id).unwrap().is_none();
        proxy.finish().await;
        fixture.finish().await;

        assert!(missing, "{refusal} cannot create an association");
        let error = result.err().expect("the original home identity and membership remain required");
        assert_eq!(error.status, StatusCode::CONFLICT, "{refusal}: {}", error.message);
        assert!(!error.message.contains("try the read again later"), "{refusal}: {}", error.message);
        assert!(!error.message.contains("connection recovers"), "{refusal}: {}", error.message);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_recovery_rejects_a_different_home_identity_without_metadata() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let proxy = ProxySend::new(&fixture).await;
    let sent = proxy.send(&fixture, "different-original-home").await;
    let mut locator = MailboxRelayLocator::decode(&sent.mailbox_id, false).unwrap().unwrap();
    locator.home.host.host_generation += 1;
    let encoded = MailboxRelayLocator::encode(&locator.home, &locator.id, false).unwrap();
    let actor = fixture.satellite.native_mailbox_relay_descriptor(&fixture.native).unwrap().principal;
    let refused = read_relay_page(&fixture.satellite, &fixture.native, &encoded).await;
    let handle = fixture.satellite.federation_peer_handles.lock().unwrap()
        .remove(&locator.home.host.host_id).unwrap();
    let refused_without_handle = read_relay_page(&fixture.satellite, &fixture.native, &encoded).await;
    fixture.satellite.federation_peer_handles.lock().unwrap()
        .insert(locator.home.host.host_id.clone(), handle);
    let missing = fixture.satellite.mailbox_store.mailbox_relay_association(&actor, &locator.id).unwrap().is_none();
    proxy.finish().await;
    fixture.finish().await;

    assert!(missing, "a different home identity must not create an association");
    for result in [refused, refused_without_handle] {
        let error = result.err().expect("a different home is unauthorized even when its handle is absent");
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(error.message.contains("configured central home"), "{}", error.message);
        assert!(error.message.contains("retry alone will not"), "{}", error.message);
        assert!(!error.message.contains("try the read again later"), "a permanent refusal must not claim transient recovery");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_unknown_local_locator_names_the_exact_handle_source() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let caller = test_session_id(&fixture.central, Agent::Codex);
    let actor = fixture.central.native_mailbox_relay_descriptor(&caller).unwrap().principal;
    let home = MailboxRelayHome { host: actor.host.clone(), store_id: actor.store_id.clone() };
    let mailbox_id = format!("mailbox-{}", Uuid::new_v4());
    let locator = MailboxRelayLocator::encode(&home, &mailbox_id, false).unwrap();
    let missing = fixture.central.mailbox_store.mailbox_relay_association(&actor, &mailbox_id)
        .unwrap().is_none();
    let no_handle = fixture.central.federation_peer_handle(&home.host.host_id).is_none();
    let result = read_relay_page(&fixture.central, &caller, &locator).await;
    fixture.finish().await;

    assert!(missing && no_handle, "the local home has no saved association or peer handle");
    let error = result.err().expect("an unowned mailbox locator cannot create membership");
    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert!(error.message.starts_with("unknown_native_mailbox_locator:"), "{}", error.message);
    assert!(error.message.contains("this local home for this caller"), "{}", error.message);
    assert!(error.message.contains("copy mailboxId exactly"), "{}", error.message);
    assert!(error.message.contains("termal_list_mailboxes or the original host wake"), "{}", error.message);
    assert!(error.message.contains("do not reconstruct or retype"), "{}", error.message);
    assert!(!error.message.contains("association metadata"), "an unknown id does not establish metadata loss");
    assert!(!error.message.contains(&actor.participant_key()), "no extra actor identity");
    assert!(!error.message.contains(&home.store_id), "no extra store identity");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_corrupted_local_locator_does_not_change_the_original_mailbox() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let proxy = ProxySend::new(&fixture).await;
    let sent = proxy.send(&fixture, "corrupted-local-locator").await;
    let locator = MailboxRelayLocator::decode(&sent.mailbox_id, false).unwrap().unwrap();
    let actor = fixture.central.native_mailbox_relay_descriptor(&proxy.caller).unwrap().principal;
    let unread = read_relay_page(&fixture.central, &proxy.caller, &sent.mailbox_id).await;
    // The caught-up sender has already processed its own append. Inspect history
    // explicitly so this control checks message retention rather than unread mail.
    let read_original = || read_mailbox(
        AxumPath((proxy.caller.clone(), sent.mailbox_id.clone())),
        State(fixture.central.clone()),
        Json(ReadMailboxRequest {
            after_sequence: Some(0), limit: 20, issue_receipt: false, read_key: None,
        }),
    );
    let before = read_original().await.map(|page| page.0);
    let original_tail = locator.id.chars().last().unwrap();
    let mut refused = Vec::new();
    for tail in ['0', '1', '2'].into_iter().filter(|tail| *tail != original_tail).take(2) {
        let mut corrupted = locator.clone();
        corrupted.id.pop();
        corrupted.id.push(tail);
        let encoded = MailboxRelayLocator::encode(&corrupted.home, &corrupted.id, false).unwrap();
        let result = read_relay_page(&fixture.central, &proxy.caller, &encoded).await;
        let association = fixture.central.mailbox_store.mailbox_relay_association(&actor, &corrupted.id).unwrap();
        refused.push((result, association));
    }
    let after = read_original().await.map(|page| page.0);
    proxy.finish().await;
    fixture.finish().await;

    assert!(sent.sender_cursor_advanced);
    assert_eq!(sent.sender_processed_through, sent.sequence);
    let unread = unread.unwrap();
    assert_eq!(unread.processed_through, sent.sequence);
    assert!(!unread.messages.iter().any(|message| message.id == sent.message_id));
    assert!(before.unwrap().messages.iter().any(|message| message.id == sent.message_id));
    assert_eq!(refused.len(), 2);
    for (result, association) in refused {
        let error = result.err().expect("a corrupted locator must not select the original mailbox");
        assert_eq!(error.status, StatusCode::NOT_FOUND);
        assert!(error.message.starts_with("unknown_native_mailbox_locator:"), "{}", error.message);
        assert!(association.is_none(), "a rejected locator must not create an association");
        let error: anyhow::Error = TermalDelegationApiError { status: error.status, message: error.message }.into();
        assert!(!termal_delegation_outcome_is_unknown(&error));
        assert!(!error_is_safe_replay_service_unavailable(&error));
    }
    assert!(after.unwrap().messages.iter().any(|message| message.id == sent.message_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_missing_association_remote_home_names_the_missing_handle() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let proxy = ProxySend::new(&fixture).await;
    let sent = proxy.send(&fixture, "missing-association-remote").await;
    let locator = MailboxRelayLocator::decode(&sent.mailbox_id, false).unwrap().unwrap();
    let actor = fixture.satellite.native_mailbox_relay_descriptor(&fixture.native).unwrap().principal;
    let missing = fixture.satellite.mailbox_store.mailbox_relay_association(&actor, &locator.id)
        .unwrap().is_none();
    let handle = fixture.satellite.federation_peer_handles.lock().unwrap()
        .remove(&locator.home.host.host_id).expect("fixture starts with a connected home");
    let refused = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await;
    let still_missing = fixture.satellite.mailbox_store.mailbox_relay_association(&actor, &locator.id)
        .unwrap().is_none();
    fixture.satellite.federation_peer_handles.lock().unwrap()
        .insert(locator.home.host.host_id.clone(), handle);
    let recovered = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await;
    proxy.finish().await;
    fixture.finish().await;

    assert!(missing && still_missing, "a refused read must not invent an association");
    let page = recovered.expect("restoring the authenticated home handle lets the read recover");
    assert!(page.messages.iter().any(|message| message.id == sent.message_id));
    let error = refused.err().expect("a missing handle cannot access the remote home");
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(error.message.contains("no saved association"), "{}", error.message);
    assert!(error.message.contains("home=remote"), "{}", error.message);
    assert!(error.message.contains("peer_handle=absent"), "{}", error.message);
    assert!(error.message.contains("try the read again later after the configured connection recovers"), "{}", error.message);
    assert!(error.message.contains("this read does not reconnect"), "{}", error.message);
    assert!(!error.message.contains(&actor.participant_key()), "no extra actor identity");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_missing_association_available_home_recovers_through_the_public_read() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let proxy = ProxySend::new(&fixture).await;
    let sent = proxy.send(&fixture, "missing-association-available").await;
    let locator = MailboxRelayLocator::decode(&sent.mailbox_id, false).unwrap().unwrap();
    let actor = fixture.satellite.native_mailbox_relay_descriptor(&fixture.native).unwrap().principal;
    let missing_before = fixture.satellite.mailbox_store.mailbox_relay_association(&actor, &locator.id)
        .unwrap().is_none();
    let result = read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id).await;
    let recovered = fixture.satellite.mailbox_store.mailbox_relay_association(&actor, &locator.id)
        .unwrap().is_some();
    proxy.finish().await;
    fixture.finish().await;

    assert!(missing_before && recovered, "the public page read retains the authoritative association");
    let page = result.expect("a reachable authenticated home can recover membership");
    assert!(page.receipt.is_some(), "the recovered read issues a real receipt");
    assert!(page.messages.iter().any(|message| message.id == sent.message_id));
}
