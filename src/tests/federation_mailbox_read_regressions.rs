// Owns read snapshot/cache ownership and strict mixed-version refusal controls.
// Uses disposable hosts and authenticated fixtures, never live stores or peers.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_json_decoder_rejects_keyed_resolution_as_typed_upgrade_required() {
    use axum::extract::FromRequest;
    // Mirror the old request's actual serde/Axum data-rejection boundary.
    // ReadKeyed is intentionally absent, not decoded by today's enum.
    #[derive(Deserialize, Serialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct OldRequest {
        protocol_version: u16,
        schema_version: String,
        central: Value,
        actor: Option<Value>,
        operation: OldOperation,
    }
    #[derive(Deserialize, Serialize)]
    #[serde(tag = "operation", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
    enum OldOperation {
        Protocol,
        Read { mailbox_id: String, after_sequence: Option<u64>, limit: u64, issue_receipt: bool },
    }
    let case = MailboxCase::new().await;
    let original = read_request(&case).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = FederationListenerState { channel: case.pair.lease.channel.clone(),
        ensure_current: super::super::super::federation_link::allowed() };
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    let router = Router::new().route("/federation/v1/mailbox", post(move |request: axum::extract::Request| {
        let seen = seen.clone();
        async move {
            seen.fetch_add(1, Ordering::SeqCst);
            match Json::<OldRequest>::from_request(request, &()).await {
                Ok(_) => (StatusCode::BAD_REQUEST, Json(json!({"error":"unsupported central mailbox protocol or schema"}))).into_response(),
                Err(rejection) => {
                    assert_eq!(rejection.status(), StatusCode::UNPROCESSABLE_ENTITY);
                    rejection.into_response()
                }
            }
        }
    })).route_layer(axum::middleware::from_fn_with_state(state.clone(), federation_authenticate)).with_state(state);
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move {
        axum::serve(listener, router).with_graceful_shutdown(async move {
            federation_wait_stop(&mut stopped).await;
        }).await.unwrap();
    });
    let client = case.client(port);
    let lease = client.channel.current_lease().unwrap();
    let key = new_key(&case);
    let before = case.message_count();
    for resolve in [false, true] {
        let error = client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
            lease.clone(), keyed(&case, &original, &key, resolve))).await.unwrap_err();
        assert_eq!(error.kind, Some(ApiErrorKind::FederationMailboxUpgradeRequired));
        assert!(!error.message.contains(FEDERATION_MAILBOX_UNKNOWN_OUTCOME));
    }
    stop.send_replace(true);
    task.await.unwrap();
    let after = case.message_count();
    case.pair.finish().await;
    assert_eq!(calls.load(Ordering::SeqCst), 2, "no version refusal retries or downgrade");
    assert_eq!(after, before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn current_peer_bad_request_is_not_mislabeled_as_a_version_mismatch() {
    let case = MailboxCase::new().await;
    let client = case.client(case.central_port());
    let lease = client.channel.current_lease().unwrap();
    let error = client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(lease,
        FederationMailboxRequest { actor: None, operation: FederationMailboxOperation::List,
            ..case.request.clone() })).await.unwrap_err();
    case.pair.finish().await;
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.kind, None);
    assert!(!error.message.contains("upgradeRequired"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_and_empty_reads_reclaim_slots_without_changing_old_pages() {
    let case = MailboxCase::new().await;
    let original = read_request(&case).await;
    let oldest = new_key(&case);
    execute(&case, keyed(&case, &original, &oldest, false)).unwrap();
    for _ in 0..FEDERATION_MAILBOX_FROZEN_KEYS * 2 {
        let issued = execute(&case, keyed(&case, &original, &new_key(&case), false)).unwrap();
        assert_eq!(issued["receiptState"], "issued");
    }
    incoming(&case, "after-issued-eviction", "new rows cannot reuse an issued key");
    for resolve in [false, true] {
        let lost = execute(&case, keyed(&case, &original, &oldest, resolve)).unwrap_err();
        assert_eq!(lost.kind, Some(ApiErrorKind::FederationMailboxReadExpired));
    }
    let mut empty = original.clone();
    if let FederationMailboxOperation::Read { after_sequence, .. } = &mut empty.operation {
        *after_sequence = Some(2);
    }
    let empty_key = new_key(&case);
    execute(&case, keyed(&case, &empty, &empty_key, false)).unwrap();
    for _ in 0..FEDERATION_MAILBOX_FROZEN_KEYS * 2 {
        let page = execute(&case, keyed(&case, &empty, &new_key(&case), false)).unwrap();
        assert_eq!(page["messages"], json!([]));
        assert!(page["receipt"].is_null());
    }
    incoming(&case, "after-empty-eviction", "new rows cannot reuse an empty key");
    for resolve in [false, true] {
        let lost = execute(&case, keyed(&case, &empty, &empty_key, resolve)).unwrap_err();
        assert_eq!(lost.kind, Some(ApiErrorKind::FederationMailboxReadExpired));
    }
    let count = case.pair.central.mailbox_store.federation_reads.lock().unwrap().entries.len();
    case.pair.finish().await;
    assert!(count <= FEDERATION_MAILBOX_FROZEN_KEYS);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reclaimed_admission_offers_cannot_recreate_a_page_and_remain_bounded() {
    let case = MailboxCase::new().await;
    let original = read_request(&case).await;
    let first = new_key(&case);
    for _ in 0..FEDERATION_MAILBOX_FROZEN_KEYS * 2 { new_key(&case); }
    let count = {
        let cache = case.pair.central.mailbox_store.federation_reads.lock().unwrap();
        cache.offered.len() + cache.entries.len()
    };
    for resolve in [false, true] {
        let lost = execute(&case, keyed(&case, &original, &first, resolve)).unwrap_err();
        assert_eq!(lost.kind, Some(ApiErrorKind::FederationMailboxReadExpired));
    }
    case.pair.finish().await;
    assert_eq!(count, FEDERATION_MAILBOX_FROZEN_KEYS);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_page_snapshot_cannot_remove_a_newer_live_wake_reservation() {
    let case = MailboxCase::new().await;
    let original = read_request(&case).await;
    let native = match &case.request.operation {
        FederationMailboxOperation::Send { target, .. } => case.pair.central.inner.lock().unwrap()
            .federation.mailbox_directory[target].binding.identity.session_id.clone(),
        _ => unreachable!(),
    };
    let reader = case.request.actor.as_ref().unwrap().binding.participant_key();
    let later = super::super::super::federation_mailboxes::request(&case.pair.central, &native,
        FederationMailboxOperation::Send { target: reader, message: "newer reservation".into(),
            key: "after-page-snapshot".into(), topic: None, state_stamp: None });
    let app = case.pair.central.clone();
    let newer = Arc::new(Mutex::new(None));
    let saved = newer.clone();
    TEST_MAILBOX_PAGE_SNAPSHOT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
        let sent = app.execute_federation_mailbox(later, None).unwrap().value;
        let id = sent["messageId"].as_str().unwrap().to_owned();
        app.refresh_federation_executor_reservation_domain().unwrap();
        let reserved = app.mailbox_store.reserve_federation_executor_wakes().unwrap();
        assert!(reserved.iter().any(|request| request.wake.message_id == id));
        *saved.lock().unwrap() = Some(id);
    })));
    let page = execute(&case, keyed(&case, &original, &new_key(&case), false)).unwrap();
    let newer = newer.lock().unwrap().clone().expect("snapshot interleave executed");
    let cached = case.pair.central.mailbox_store.executor_reservations.lock().unwrap()
        .window.values().any(|(_, request)| request.wake.message_id == newer);
    let next = case.pair.central.mailbox_store.reserve_federation_executor_wakes().unwrap();
    case.pair.finish().await;
    assert_eq!(page["messages"].as_array().unwrap().len(), 1, "old snapshot stays frozen");
    assert!(cached, "historical diagnostics must leave the live reservation window intact");
    assert!(next.iter().any(|request| request.wake.message_id == newer));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn newer_client_refuses_an_older_central_across_read_and_mutation_entry_points() {
    let case = MailboxCase::new().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = FederationListenerState { channel: case.pair.lease.channel.clone(),
        ensure_current: super::super::super::federation_link::allowed() };
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let router = Router::new().route("/federation/v1/mailbox", post(move |Json(request): Json<FederationMailboxRequest>| {
        seen.lock().unwrap().push(request);
        async { (StatusCode::BAD_REQUEST, Json(json!({"error":"unsupported central mailbox protocol or schema"}))) }
    })).route_layer(axum::middleware::from_fn_with_state(state.clone(), federation_authenticate)).with_state(state);
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move {
        axum::serve(listener, router).with_graceful_shutdown(async move {
            federation_wait_stop(&mut stopped).await;
        }).await.unwrap();
    });
    let client = case.client(port);
    let lease = client.channel.current_lease().unwrap();
    let mut operations = vec![FederationMailboxOperation::Protocol, FederationMailboxOperation::Register,
        FederationMailboxOperation::List, FederationMailboxOperation::Directory { alias_basis: false },
        FederationMailboxOperation::Read { mailbox_id: "mailbox".into(), after_sequence: None,
            limit: 200, issue_receipt: true },
        FederationMailboxOperation::ReadKeyed { mailbox_id: "mailbox".into(), after_sequence: None,
            limit: 200, issue_receipt: true, read_key: new_key(&case), resolve: true },
        FederationMailboxOperation::ReadMessage { message_id: "message".into() },
        FederationMailboxOperation::GetOrCreate { target: "target".into() },
        FederationMailboxOperation::Acknowledge { mailbox_id: "mailbox".into(), receipt: "receipt".into() }];
    operations.push(case.request.operation.clone());
    let expected = operations.len();
    for operation in operations {
        let error = client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(lease.clone(),
            FederationMailboxRequest { operation, ..case.request.clone() })).await.unwrap_err();
        assert_eq!(error.kind, Some(ApiErrorKind::FederationMailboxUpgradeRequired));
        assert!(!error.message.contains(FEDERATION_MAILBOX_UNKNOWN_OUTCOME));
    }
    stop.send_replace(true);
    task.await.unwrap();
    let seen = requests.lock().unwrap().clone();
    let count = case.message_count();
    case.pair.finish().await;
    assert_eq!(seen.len(), expected, "version refusal must not retry or downgrade");
    assert!(seen.iter().all(|request| request.protocol_version == 2 && request.schema_version == "2"));
    assert_eq!(count, 0);
}
