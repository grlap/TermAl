// Owns mixed-build capability admission over the real satellite API and store.
// Does not claim rolling-upgrade interoperability or weaken authenticated RPC.
// The central boundary reproduces the older capability and unknown variants.
use super::federation_mailbox_process::Satellite;
use super::*;

struct CapabilityPair {
    central: AppState,
    target: String,
    target_binding: FederationMailboxBinding,
    satellite: Satellite,
    base: String,
    client: reqwest::Client,
    supported: Arc<std::sync::atomic::AtomicBool>,
    lose_reply: Arc<std::sync::atomic::AtomicBool>,
    unavailable: Arc<std::sync::atomic::AtomicBool>,
    unsupported: Arc<std::sync::atomic::AtomicUsize>,
    stop: tokio::sync::watch::Sender<bool>,
    server: tokio::task::JoinHandle<()>,
}

async fn capability_pair(supported: bool) -> CapabilityPair {
    let central = federation_mailboxes::central_store();
    let (_, target) = federation_mailboxes::local_participant(&central, "Target");
    let (satellite, native) = tokio::task::spawn_blocking(Satellite::spawn).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (record, channel) = federation_mailbox_native_link::channel(&central, &native, port);
    let check = channel.clone();
    let unsupported = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = unsupported.clone();
    let supported = Arc::new(std::sync::atomic::AtomicBool::new(supported));
    let advertised = supported.clone();
    let lose_reply = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let drop_response = lose_reply.clone();
    let unavailable = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let outage = unavailable.clone();
    // Model application capability/refusal BEFORE the production middleware
    // signs it. Rewriting an already-signed response would test MAC tampering.
    let state = FederationListenerState { channel: channel.clone(),
        ensure_current: Arc::new(move || check.current_authority().map(|_| ())) };
    let router = federation_mailbox_routes()
        .layer(axum::middleware::from_fn(move |request: axum::extract::Request,
            next: axum::middleware::Next| {
            let observed = observed.clone();
            let advertised = advertised.clone();
            let drop_response = drop_response.clone();
            let outage = outage.clone();
            async move {
                if outage.load(std::sync::atomic::Ordering::SeqCst) {
                    return (StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"error":"fixture central transport unavailable"}))).into_response();
                }
                let (parts, body) = request.into_parts();
                let bytes = axum::body::to_bytes(body, FEDERATION_MAILBOX_REQUEST_BYTES).await.unwrap();
                let request: FederationMailboxRequest = serde_json::from_slice(&bytes).unwrap();
                let protocol = matches!(&request.operation, FederationMailboxOperation::Protocol);
                let send = matches!(&request.operation, FederationMailboxOperation::SendExact { .. });
                // The landed M2 enum rejects both new variants during JSON extraction.
                if !advertised.load(std::sync::atomic::Ordering::SeqCst)
                    && matches!(&request.operation, FederationMailboxOperation::SendExact { .. }
                        | FederationMailboxOperation::CancelExact { .. }) {
                    observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    return (StatusCode::UNPROCESSABLE_ENTITY,
                        Json(json!({"error":"unknown mailbox operation"}))).into_response();
                }
                let response = next.run(axum::extract::Request::from_parts(
                    parts, axum::body::Body::from(bytes))).await;
                if send && response.status().is_success()
                    && drop_response.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    return (StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"error":"fixture lost the committed reply"}))).into_response();
                }
                if !protocol || !response.status().is_success() {
                    return response;
                }
                // All supported operations still pass through the normal authenticated router.
                let bytes = axum::body::to_bytes(response.into_body(), FEDERATION_MAILBOX_RESPONSE_BYTES)
                    .await.unwrap();
                let mut response: FederationMailboxResponse = serde_json::from_slice(&bytes).unwrap();
                response.value["senderIntentRegistry"] = json!(advertised.load(std::sync::atomic::Ordering::SeqCst));
                Json(response).into_response()
            }
        }))
        .layer(axum::middleware::from_fn_with_state(state.clone(), federation_authenticate))
        .with_state(state);
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).with_graceful_shutdown(async move {
            federation_wait_stop(&mut stopped).await
        }).await.unwrap();
    });
    let (satellite, satellite_port, control) = federation_mailbox_native_link::bind(
        &central, &channel, satellite, &native, record, port,
    ).await;
    let server = tokio::spawn(async move {
        server.await.unwrap();
        control.finish().await;
    });
    let client = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
        .timeout(phase_sync::DEADLOCK_GUARD).build().unwrap();
    let base = format!("http://127.0.0.1:{satellite_port}/api/sessions/{}", native.session);
    CapabilityPair { central, target: target.session_id, target_binding: target.binding,
        satellite, base, client, supported, lose_reply, unavailable, unsupported, stop, server }
}

async fn capability_inspect(satellite: Satellite) -> (Satellite, Value) {
    tokio::task::spawn_blocking(move || {
        let mut satellite = satellite;
        let inspection = satellite.command(json!({"command":"inspectIntents"}));
        (satellite, inspection)
    }).await.unwrap()
}

async fn capability_finish(
    satellite: Satellite,
    stop: tokio::sync::watch::Sender<bool>,
    server: tokio::task::JoinHandle<()>,
) {
    tokio::task::spawn_blocking(move || satellite.finish()).await.unwrap();
    stop.send_replace(true);
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_delivery_capability_m2_central_refuses_before_intent_or_quota() {
    let CapabilityPair { target, satellite, base, client, unsupported, stop, server, .. }
        = capability_pair(false).await;
    // Independently prove that this fixture advertises the older central capability.
    let protocol: Value = client.get(format!("{base}/mailboxes/protocol"))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(protocol["senderIntentRegistry"], false);
    let mut outcomes = Vec::new();
    for (route, key) in [("mailbox-intents", "prepare-old-central"),
        ("mailboxes/send", "send-old-central")] {
        let response = client.post(format!("{base}/{route}")).json(&json!({
            "targetSessionId":target,"idempotencyKey":key,
            "message":"must not consume a sender slot on an unsupported central",
        })).send().await.unwrap();
        let status = response.status();
        let body: Value = response.json().await.unwrap();
        outcomes.push((status, body));
    }
    let (satellite, inspection) = capability_inspect(satellite).await;
    capability_finish(satellite, stop, server).await;
    assert_eq!(inspection, json!({"pending":0,"bytes":0,"total":0}),
        "unsupported central must not retain an intent or consume sender quota; responses: {outcomes:?}");
    assert_eq!(unsupported.load(std::sync::atomic::Ordering::SeqCst), 0,
        "unsupported operations must never be dispatched");
    for (status, body) in outcomes {
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "central does not support durable sender intents; this request made no delivery attempt");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_delivery_capability_positive_control_and_unsupported_retry_preserve_outcomes() {
    let CapabilityPair { central, target, satellite, base, client, supported, lose_reply,
        unsupported, stop, server, .. } = capability_pair(true).await;
    let payload = |key: &str| json!({"targetSessionId":target,"idempotencyKey":key,
        "message":"retained exact bytes"});
    let committed = client.post(format!("{base}/mailboxes/send")).json(&payload("committed"))
        .send().await.unwrap().error_for_status().unwrap().json::<MailboxAppendReceipt>().await.unwrap();
    let prepare = client.post(format!("{base}/mailbox-intents")).json(&payload("never-sent"))
        .send().await.unwrap();
    assert_eq!(prepare.status(), StatusCode::ACCEPTED);
    let cancellable = client.post(format!("{base}/mailbox-intents")).json(&payload("cancel-control"))
        .send().await.unwrap();
    assert_eq!(cancellable.status(), StatusCode::ACCEPTED);
    let cancelled = client.post(format!("{base}/mailbox-intents/cancel-control/cancel"))
        .send().await.unwrap().error_for_status().unwrap().json::<Value>().await.unwrap();
    assert_eq!(cancelled["state"], "abandoned");
    let queued_cancel = client.post(format!("{base}/mailbox-intents")).json(&payload("cancel-unsupported"))
        .send().await.unwrap();
    assert_eq!(queued_cancel.status(), StatusCode::ACCEPTED);
    lose_reply.store(true, std::sync::atomic::Ordering::SeqCst);
    let lost = client.post(format!("{base}/mailboxes/send")).json(&payload("lost-reply"))
        .send().await.unwrap();
    assert_eq!(lost.status(), StatusCode::SERVICE_UNAVAILABLE);
    let (satellite, before) = capability_inspect(satellite).await;
    assert_eq!(before["pending"], 3);

    // Capability is freshly negotiated, never inferred from the earlier true reply.
    supported.store(false, std::sync::atomic::Ordering::SeqCst);
    for (key, operation) in [("cancel-unsupported", "cancel"), ("never-sent", "retry"),
        ("never-sent", "cancel"), ("lost-reply", "retry")] {
        let response = client.post(format!("{base}/mailbox-intents/{key}/{operation}"))
            .send().await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body: Value = response.json().await.unwrap();
        let expected = if key == "lost-reply" {
            "central does not support durable sender intents; this request made no delivery attempt"
        } else { "delivery locally refused: centralCapabilityUnsupported; terminal outcome retained" };
        assert_eq!(body["error"], expected);
    }
    for (key, expected) in [("cancel-unsupported", "refused"), ("never-sent", "refused"),
        ("lost-reply", "appendOutcomeUnknown")] {
        let status: Value = client.get(format!("{base}/mailbox-intents/{key}"))
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        assert_eq!(status["state"], expected);
        if key != "lost-reply" {
            assert_eq!(status["refusalReason"], "centralCapabilityUnsupported");
            assert_eq!(status["refusalSource"], "sender");
        } else { assert!(status.get("refusalReason").is_none()); }
    }
    let duplicate: MailboxAppendReceipt = client.post(format!("{base}/mailbox-intents/committed/retry"))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(duplicate.message_id, committed.message_id);
    assert!(duplicate.duplicate);
    let ordinary_duplicate: MailboxAppendReceipt = client.post(format!("{base}/mailboxes/send"))
        .json(&payload("committed")).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(ordinary_duplicate.message_id, committed.message_id,
        "a changed capability cannot rewrite a retained committed result");
    let (satellite, after) = capability_inspect(satellite).await;
    assert_eq!(after["total"], before["total"]);
    assert_eq!(after["pending"], 1, "only the two never-attempted rows release pending quota");
    assert_eq!(unsupported.load(std::sync::atomic::Ordering::SeqCst), 0);
    let messages: u64 = central.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get(0)).unwrap();
    assert_eq!(messages, 2, "the confirmed send and lost reply each append exactly once");
    capability_finish(satellite, stop, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_delivery_capability_first_offline_queue_then_false_releases_only_local_quota() {
    let CapabilityPair { central, target, target_binding, satellite, base, client, supported,
        unavailable, unsupported, stop, server, .. } = capability_pair(false).await;
    let (satellite, membership) = tokio::task::spawn_blocking(move || {
        let mut satellite = satellite;
        let membership = satellite.command(json!({"command":"inspectCentral"}));
        (satellite, membership)
    }).await.unwrap();
    assert_eq!(membership["membership"], "enrolled", "use the production post-link state, without test activation");
    unavailable.store(true, std::sync::atomic::Ordering::SeqCst);
    // No earlier protocol negotiation or directory lookup; the caller already
    // captured this recipient binding and the satellite has its durable sender binding.
    let payload = json!({"targetSessionId":target,"targetBinding":target_binding,
        "idempotencyKey":"first-offline","message":"queued without an append attempt"});
    let ordinary = client.post(format!("{base}/mailboxes/send")).json(&payload).send().await.unwrap();
    assert_eq!(ordinary.status(), StatusCode::UNPROCESSABLE_ENTITY, "descriptor override is prepare-only");
    let mut omitted = payload.clone();
    omitted.as_object_mut().unwrap().remove("targetBinding");
    let response = client.post(format!("{base}/mailbox-intents")).json(&omitted).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "a fresh id alone cannot discover its binding offline");
    let mut alias = payload.clone();
    alias["targetSessionId"] = json!("Target");
    let response = client.post(format!("{base}/mailbox-intents")).json(&alias).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT, "offline aliases are not guessed");
    let mut epoch = payload.clone();
    let mut other_epoch = target_binding.clone();
    other_epoch.central.host_generation += 1;
    epoch["targetBinding"] = json!(other_epoch);
    epoch["targetSessionId"] = json!(other_epoch.participant_key());
    let response = client.post(format!("{base}/mailbox-intents")).json(&epoch).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT, "descriptor epoch cannot replace server-derived sender authority");
    let response = client.post(format!("{base}/mailbox-intents")).json(&payload).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let queued: Value = response.json().await.unwrap();
    assert_eq!(queued["state"], "localIntentDurable");
    assert!(queued.get("messageId").is_none());
    assert!(queued.get("sequence").is_none());
    let mut changed = payload.clone();
    let mut replacement = target_binding.clone();
    replacement.identity.creation_id = Uuid::new_v4().to_string();
    changed["targetSessionId"] = json!(replacement.participant_key());
    changed["targetBinding"] = json!(replacement);
    let response = client.post(format!("{base}/mailbox-intents")).json(&changed).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT, "the same key cannot capture a new creation");
    let response = client.post(format!("{base}/mailbox-intents/first-offline/retry"))
        .send().await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let status: Value = client.get(format!("{base}/mailbox-intents/first-offline"))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(status["state"], "localIntentDurable", "negotiation failure is not a delivery attempt");
    let (satellite, before) = capability_inspect(satellite).await;
    assert_eq!(before["pending"], 1);
    assert!(before["bytes"].as_u64().unwrap() > 0);
    let (satellite, local) = tokio::task::spawn_blocking(move || {
        let mut satellite = satellite;
        let local = satellite.command(json!({"command":"inspect"}));
        (satellite, local)
    }).await.unwrap();
    assert_eq!(local, json!({"directory":0,"mailboxes":0,"messages":0}),
        "a supplied descriptor must not create local recipient authority or mailbox content");
    unavailable.store(false, std::sync::atomic::Ordering::SeqCst);
    let response = client.post(format!("{base}/mailbox-intents/first-offline/retry"))
        .send().await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let first: Value = response.json().await.unwrap();
    let status: Value = client.get(format!("{base}/mailbox-intents/first-offline"))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(status["state"], "refused");
    assert_eq!(status["refusalSource"], "sender");
    assert_eq!(status["refusalReason"], "centralCapabilityUnsupported");
    let (satellite, after) = capability_inspect(satellite).await;
    assert_eq!(after, json!({"pending":0,"bytes":0,"total":1}));
    let gc = client.post(format!("{base}/mailbox-intents/first-offline/gc"))
        .send().await.unwrap();
    assert_eq!(gc.status(), StatusCode::OK);
    supported.store(true, std::sync::atomic::Ordering::SeqCst);
    let response = client.post(format!("{base}/mailbox-intents/first-offline/retry"))
        .send().await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(response.json::<Value>().await.unwrap(), first, "terminal replay survives payload GC and capability change");
    assert_eq!(unsupported.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(central.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get::<_,u64>(0)).unwrap(), 0);
    capability_finish(satellite, stop, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_delivery_capability_revoked_central_cannot_queue_offline() {
    let CapabilityPair { target, target_binding, satellite, base, client,
        unavailable, stop, server, .. } = capability_pair(false).await;
    let satellite = tokio::task::spawn_blocking(move || {
        let mut satellite = satellite;
        assert_eq!(satellite.command(json!({"command":"inspectCentral"}))["membership"], "enrolled");
        assert_eq!(satellite.command(json!({"command":"revokeCentral"})), json!({"revoked":true}));
        satellite
    }).await.unwrap();
    unavailable.store(true, std::sync::atomic::Ordering::SeqCst);
    let response = client.post(format!("{base}/mailbox-intents")).json(&json!({
        "targetSessionId":target,"targetBinding":target_binding,
        "idempotencyKey":"revoked-offline","message":"revocation is not an outage",
    })).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let (satellite, intents) = capability_inspect(satellite).await;
    assert_eq!(intents, json!({"pending":0,"bytes":0,"total":0}));
    capability_finish(satellite, stop, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_delivery_capability_stale_descriptor_is_not_retargeted_to_recreated_recipient() {
    let CapabilityPair { central, target, target_binding, satellite, base, client, stop, server, .. }
        = capability_pair(true).await;
    let native = target_binding.identity.session_id.clone();
    {
        let mut inner = central.inner.lock().unwrap();
        let index = inner.find_session_index(&native).unwrap();
        inner.remove_session_at(index).unwrap();
        inner.next_session_number = native.trim_start_matches("session-").parse().unwrap();
    }
    let (recreated, successor) = federation_mailboxes::local_participant(&central, "Target");
    assert_eq!(recreated, native);
    assert_ne!(successor.binding, target_binding);
    let response = client.post(format!("{base}/mailbox-intents")).json(&json!({
        "targetSessionId":target,"targetBinding":target_binding,
        "idempotencyKey":"stale-target","message":"must never reach the new creation",
    })).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED, "preparation records routing, not central recipient validation");
    let response = client.post(format!("{base}/mailbox-intents/stale-target/retry"))
        .send().await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let status: Value = client.get(format!("{base}/mailbox-intents/stale-target"))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(status["state"], "refused");
    assert!(status.get("refusalSource").is_none(), "this is a central outcome, not sender capability refusal");
    assert_eq!(status["targetSessionId"], target, "the original creation remains captured");
    assert_eq!(central.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get::<_,u64>(0)).unwrap(), 0);
    capability_finish(satellite, stop, server).await;
}

#[test]
fn federation_delivery_capability_retired_proof_rolls_back_at_commit_and_cannot_mark_unknown() {
    let base = federation_registry::domain_state();
    let path = resolve_coordination_persistence_path(base.persistence_path.as_path());
    let app = AppState { mailbox_store: Arc::new(MailboxStore::open(&path).unwrap()), ..base };
    let session = test_session_id(&app, Agent::Codex);
    let central = FederationHostIdentity { host_id: Uuid::new_v4().to_string(), host_generation: 1 };
    app.configure_federation(FederationSettings {
        enabled: true, role: FederationRole::Satellite, central: Some(central.clone()),
    }).unwrap();
    let mut record = federation_registry::peer(48001);
    record.identity = central.clone();
    let peer = federation_registry::active(&app, record);
    let incarnation = app.inner.lock().unwrap().federation.links[&central.host_id].clone();
    let channel = FederationChannel::for_app(peer,
        FederationPairCapability::new("a".repeat(64)).unwrap(), app.clone());
    let lease = channel.activate(incarnation.clone());
    let (sender, creation) = {
        let mut inner = app.inner.lock().unwrap();
        let local = inner.federation.local.clone().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let native = inner.session_mut_by_index(index).unwrap();
        let creation = native.federation_native_creation_id.clone();
        let sender = FederationMailboxBinding { central: central.clone(),
            identity: FederationSessionIdentity { host_id: local.host_id,
                session_id: session.clone(), creation_id: creation.clone() }, placement_generation: 1 };
        native.federation_mailbox_binding = Some(sender.clone());
        (sender, creation)
    };
    let delivery = FederationDelivery { version: 1, sender,
        target: FederationMailboxBinding { central: central.clone(),
            identity: FederationSessionIdentity { host_id: central.host_id.clone(),
                session_id: "session-8".into(), creation_id: Uuid::new_v4().to_string() },
            placement_generation: 1 },
        key: "positive-proof".into(), message: "retained exact payload".into(), topic: None, state_stamp: None };
    // This fixture supplies an already-negotiated true proof to isolate the
    // commit fence. The HTTP tests above exercise negotiation itself.
    let proof = FederationDeliveryCapability { central: central.clone(), lease: Some(lease), supported: true };
    app.mailbox_store.prepare_federation_intent(&delivery, &session, &creation,
        || proof.lock(&app, &session, &creation, &delivery.sender)).unwrap();
    let mut refused = delivery.clone();
    refused.key = "retired-before-commit".into();
    let retire = channel.clone();
    FEDERATION_DELIVERY_TEST_CUT.with(|slot| {
        *slot.borrow_mut() = Some(("beforeLocalDurability", Box::new(move || retire.invalidate())));
    });
    let error = app.mailbox_store.prepare_federation_intent(&refused, &session, &creation,
        || proof.lock(&app, &session, &creation, &refused.sender)).err().unwrap();
    assert_eq!(mailbox_api_error(error).status, StatusCode::CONFLICT);
    let error = app.mailbox_store.mark_federation_intent_unknown(&delivery,
        || proof.lock(&app, &session, &creation, &delivery.sender)).unwrap_err();
    assert_eq!(mailbox_api_error(error).status, StatusCode::CONFLICT);
    {
        let connection = app.mailbox_store.connection().unwrap();
        assert!(federation_intent_read(&connection, &refused.sender.participant_key(), &refused.key)
            .unwrap().is_none(), "stale admission must roll the new row back");
        assert_eq!(federation_intent_read(&connection, &delivery.sender.participant_key(), &delivery.key)
            .unwrap().unwrap().state, "localIntentDurable", "failed fencing cannot mark an attempt");
    }
    // UUID reuse still cannot revive a proof from the previous transport generation.
    let expected = app.federation_peer_snapshot(&central.host_id).unwrap();
    app.federation_peer_connected(&expected, incarnation.clone()).unwrap();
    channel.activate(incarnation.clone());
    assert!(proof.lock(&app, &session, &creation, &delivery.sender).is_err());
    let current_peer = app.federation_peer_snapshot(&central.host_id).unwrap();
    let current_channel = FederationChannel::for_app(current_peer,
        FederationPairCapability::new("a".repeat(64)).unwrap(), app.clone());
    let lease = current_channel.activate(incarnation);
    let current = FederationDeliveryCapability { central, lease: Some(lease), supported: false };
    // The new incarnation is valid before the cut; refusal must recheck the
    // negotiated proof next to COMMIT, after the tentative quota transition.
    assert!(current.lock(&app, &session, &creation, &delivery.sender).is_ok());
    let before: (u64, u64) = app.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*),SUM(payload_bytes) FROM federation_mailbox_intents
         WHERE state IN ('localIntentDurable','appendOutcomeUnknown')",
        [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    let retire = current_channel.clone();
    FEDERATION_DELIVERY_TEST_CUT.with(|slot| {
        *slot.borrow_mut() = Some(("beforeLocalRefusal", Box::new(move || retire.invalidate())));
    });
    let error = app.mailbox_store.refuse_unattempted_federation_intent(&delivery, &session, &creation,
        || current.lock(&app, &session, &creation, &delivery.sender)).unwrap_err();
    assert_eq!(mailbox_api_error(error).status, StatusCode::CONFLICT);
    let connection = app.mailbox_store.connection().unwrap();
    let retained = federation_intent_read(&connection, &delivery.sender.participant_key(), &delivery.key)
        .unwrap().unwrap();
    assert_eq!(retained.state, "localIntentDurable");
    assert!(retained.local_refusal().is_none());
    let after: (u64, u64) = connection.query_row(
        "SELECT COUNT(*),SUM(payload_bytes) FROM federation_mailbox_intents
         WHERE state IN ('localIntentDurable','appendOutcomeUnknown')",
        [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(after, before, "retired refusal proof cannot release pending quota");
}

#[test]
fn federation_delivery_capability_local_refusal_survives_real_process_death_and_replays() {
    use super::federation_mailbox_delivery_process::CrashChild;
    let (mut child, receipt) = CrashChild::spawn("afterLocalCapabilityRefusal");
    child.kill_at_cut();
    let store = MailboxStore::open(&receipt.path).unwrap();
    let retained = {
        let connection = store.connection().unwrap();
        federation_intent_read(&connection, &receipt.sender, "crash-key").unwrap().unwrap()
    };
    assert_eq!(retained.state, "refused");
    assert_eq!(retained.status()["refusalReason"], "centralCapabilityUnsupported");
    assert_eq!(retained.status()["refusalSource"], "sender");
    assert_eq!(retained.terminal_error().unwrap().message, federation_delivery_local_refusal().message);
    let delivery: FederationDelivery = serde_json::from_str(retained.payload.as_deref().unwrap()).unwrap();
    assert!(store.prepare_federation_intent(&delivery, &retained.owner, &retained.creation,
        || Ok(())).is_err(), "a delayed first sender cannot revive a terminal key");
    let connection = store.connection().unwrap();
    let pending: (u64,u64) = connection.query_row(
        "SELECT COUNT(*),COALESCE(SUM(payload_bytes),0) FROM federation_mailbox_intents
         WHERE state IN ('localIntentDurable','appendOutcomeUnknown')",
        [], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
    assert_eq!(pending, (0,0));
    let after = federation_intent_read(&connection, &receipt.sender, "crash-key").unwrap().unwrap();
    assert_eq!(after.status(), retained.status());
    assert_eq!(after.digest, retained.digest);
    assert_eq!(after.target, receipt.target);
}

#[test]
fn federation_delivery_capability_local_refusal_excludes_an_attempt_and_rechecks_commit_authority() {
    let state = federation_mailboxes::central_store();
    let (sender, actor) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    let creation = state.federation_intent_owner(&sender).unwrap();
    let delivery = FederationDelivery { version: 1, sender: actor.binding,
        target: target.binding, key: "attempt-wins".into(), message: "exact bytes".into(),
        topic: None, state_stamp: None };
    state.mailbox_store.prepare_federation_intent(&delivery, &sender, &creation, || Ok(())).unwrap();
    state.mailbox_store.mark_federation_intent_unknown(&delivery, || Ok(())).unwrap();
    assert!(state.mailbox_store.refuse_unattempted_federation_intent(&delivery, &sender, &creation,
        || state.lock_federation_intent_owner(&sender, &creation, &delivery.sender)).unwrap().is_none());
    let mut queued = delivery.clone();
    queued.key = "refusal-wins".into();
    state.mailbox_store.prepare_federation_intent(&queued, &sender, &creation, || Ok(())).unwrap();
    state.mailbox_store.refuse_unattempted_federation_intent(&queued, &sender, &creation,
        || state.lock_federation_intent_owner(&sender, &creation, &queued.sender)).unwrap();
    assert!(state.mailbox_store.mark_federation_intent_unknown(&queued, || Ok(())).is_err(),
        "a dispatch that loses the writer-serialized race cannot mark an attempt");
    let mut held = delivery.clone();
    held.key = "authority-changes-before-commit".into();
    state.mailbox_store.prepare_federation_intent(&held, &sender, &creation, || Ok(())).unwrap();
    let change = state.clone();
    let native = sender.clone();
    FEDERATION_DELIVERY_TEST_CUT.with(|slot| {
        *slot.borrow_mut() = Some(("beforeLocalRefusal", Box::new(move || {
            let mut inner = change.inner.lock().unwrap();
            let index = inner.find_session_index(&native).unwrap();
            inner.session_mut_by_index(index).unwrap().federation_native_creation_id = Uuid::new_v4().to_string();
        })));
    });
    assert!(state.mailbox_store.refuse_unattempted_federation_intent(&held, &sender, &creation,
        || state.lock_federation_intent_owner(&sender, &creation, &held.sender)).is_err());
    let connection = state.mailbox_store.connection().unwrap();
    assert_eq!(federation_intent_read(&connection, &delivery.sender.participant_key(), &delivery.key)
        .unwrap().unwrap().state, "appendOutcomeUnknown");
    assert_eq!(federation_intent_read(&connection, &held.sender.participant_key(), &held.key)
        .unwrap().unwrap().state, "localIntentDurable", "failed settlement authority rolls back quota release");
}
