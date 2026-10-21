// Owns frozen-page identity, issuance authority, restart and memory bounds.
// Uses disposable authenticated host fixtures; never opens a live store.
use super::*;
use mailbox_availability_reads::{incoming, read_request};
use mailbox_fixture::MailboxCase;

fn keyed(case: &MailboxCase, original: &FederationMailboxRequest, key: &str, resolve: bool) -> FederationMailboxRequest {
    let FederationMailboxOperation::Read { mailbox_id, after_sequence, limit, issue_receipt } = &original.operation
        else { panic!("read fixture required") };
    FederationMailboxRequest {
        operation: FederationMailboxOperation::ReadKeyed {
            mailbox_id: mailbox_id.clone(), after_sequence: *after_sequence, limit: *limit,
            issue_receipt: *issue_receipt, read_key: key.to_owned(), resolve,
        }, ..case.request.clone()
    }
}

fn new_key(case: &MailboxCase) -> String {
    case.pair.central.mailbox_store.federation_reads.lock().unwrap().offer_key()
        .expect("fixture has room for a read admission token")
}

fn execute(case: &MailboxCase, request: FederationMailboxRequest) -> Result<Value, ApiError> {
    case.pair.central.execute_federation_mailbox(request, Some(case.pair.lease.clone())).map(|response| response.value)
}

fn without_issuance(mut page: Value) -> Value {
    page["receipt"] = Value::Null;
    page["receiptState"] = json!("receiptPending");
    page
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_key_freezes_messages_cursor_and_wake_metadata_through_resolution() {
    let case = MailboxCase::with_writer_timeout(Duration::from_millis(20)).await;
    let original = read_request(&case).await;
    let key = new_key(&case);
    let writer = case.pair.central.mailbox_store.write_lock.clone();
    let held = lock_sqlite_state_writer(&writer);
    let first = execute(&case, keyed(&case, &original, &key, false)).unwrap();
    let repeated = execute(&case, keyed(&case, &original, &key, true)).unwrap();
    drop(held);
    assert_eq!(first, repeated);
    assert_eq!(first["receiptState"], "receiptPending");
    let later = incoming(&case, "later-incoming", "later message must not enter old page");
    let mailbox = later["mailboxId"].as_str().unwrap();
    let reader = case.request.actor.as_ref().unwrap().binding.participant_key();
    let issued = case.pair.central.mailbox_store.read_range_with_issuance(
        &reader, mailbox, Some(0), 1, true).unwrap();
    case.pair.central.mailbox_store.acknowledge_page(&reader, mailbox,
        issued.receipt.as_deref().unwrap()).unwrap();
    case.pair.central.mailbox_store.connection().unwrap().execute(
        "UPDATE mailbox_messages SET notification_disposition='deliveredToIdleSession' WHERE id=?1",
        [first["messages"][0]["id"].as_str().unwrap()]).unwrap();
    let resolved = execute(&case, keyed(&case, &original, &key, true)).unwrap();
    case.pair.finish().await;
    assert_eq!(resolved["receiptState"], "issued");
    assert!(resolved["receipt"].is_string());
    assert_eq!(without_issuance(resolved), first,
        "receipt resolution may change only issuance, not frozen page fields");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn omitted_cursor_and_explicit_zero_are_different_keyed_inputs() {
    let case = MailboxCase::new().await;
    let original = read_request(&case).await;
    let key = new_key(&case);
    execute(&case, keyed(&case, &original, &key, false)).unwrap();
    let mut changed = original.clone();
    if let FederationMailboxOperation::Read { after_sequence, .. } = &mut changed.operation {
        *after_sequence = Some(0);
    }
    let conflict = execute(&case, keyed(&case, &changed, &key, true)).unwrap_err();
    case.pair.finish().await;
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    assert_eq!(conflict.kind, Some(ApiErrorKind::FederationMailboxReadConflict));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_and_lost_ring_refuse_old_key_without_selecting_another_page() {
    let case = MailboxCase::new().await;
    let original = read_request(&case).await;
    let key = new_key(&case);
    execute(&case, keyed(&case, &original, &key, false)).unwrap();
    case.pair.central.mailbox_store.federation_reads.lock().unwrap().entries.clear();
    let lost = execute(&case, keyed(&case, &original, &key, true)).unwrap_err();
    let path = resolve_coordination_persistence_path(case.pair.central.persistence_path.as_path());
    let reopened = AppState { mailbox_store: Arc::new(MailboxStore::open(&path).unwrap()),
        ..case.pair.central.clone() };
    let restarted = reopened.execute_federation_mailbox(keyed(&case, &original, &key, true),
        Some(case.pair.lease.clone())).unwrap_err();
    case.pair.finish().await;
    for error in [lost, restarted] {
        assert_eq!(error.status, StatusCode::GONE);
        assert_eq!(error.kind, Some(ApiErrorKind::FederationMailboxReadExpired));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frozen_empty_page_cannot_absorb_a_later_append() {
    let case = MailboxCase::new().await;
    let mut original = read_request(&case).await;
    if let FederationMailboxOperation::Read { after_sequence, .. } = &mut original.operation {
        *after_sequence = Some(1);
    }
    let key = new_key(&case);
    let first = execute(&case, keyed(&case, &original, &key, false)).unwrap();
    incoming(&case, "after-empty", "later incoming");
    let resolved = execute(&case, keyed(&case, &original, &key, true)).unwrap();
    case.pair.finish().await;
    assert_eq!(first["messages"], json!([]));
    assert!(first["receipt"].is_null());
    assert_eq!(resolved, first);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_page_neither_issues_numeric_ack_authority_nor_accepts_fake_receipt() {
    let case = MailboxCase::with_writer_timeout(Duration::from_millis(20)).await;
    let original = read_request(&case).await;
    let key = new_key(&case);
    let writer = case.pair.central.mailbox_store.write_lock.clone();
    let held = lock_sqlite_state_writer(&writer);
    let pending = execute(&case, keyed(&case, &original, &key, false)).unwrap();
    drop(held);
    let mailbox = pending["messages"][0]["mailboxId"].as_str().unwrap();
    let reader = case.request.actor.as_ref().unwrap().binding.participant_key();
    let numeric = case.pair.central.mailbox_store.acknowledge(&reader, mailbox, 0, 1);
    let fake = case.pair.central.mailbox_store.acknowledge_page(&reader, mailbox, &key);
    case.pair.finish().await;
    assert!(pending["receipt"].is_null());
    assert!(numeric.is_err());
    assert!(fake.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn page_byte_bound_returns_a_nonempty_prefix_and_never_a_partial_message() {
    let case = MailboxCase::new().await;
    let original = read_request(&case).await;
    let escaped = "\"".repeat(MAX_MAILBOX_BODY_BYTES);
    for number in 0..5 { incoming(&case, &format!("large-{number}"), &escaped); }
    let key = new_key(&case);
    let page = execute(&case, keyed(&case, &original, &key, false)).unwrap();
    case.pair.finish().await;
    assert!(serde_json::to_vec(&page).unwrap().len() <= FEDERATION_MAILBOX_PAGE_BYTES);
    assert_eq!(page["hasMore"], true);
    let messages = page["messages"].as_array().unwrap();
    assert!(messages.len() > 1 && messages.len() < 6);
    for message in messages.iter().skip(1) { assert_eq!(message["body"], escaped); }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frozen_key_capacity_and_expiry_refuse_without_evicting_live_keys() {
    let case = MailboxCase::with_writer_timeout(Duration::from_millis(10)).await;
    let reader = case.request.actor.as_ref().unwrap().binding.participant_key();
    let mut reads = Vec::new();
    for number in 0..=FEDERATION_MAILBOX_FROZEN_KEYS {
        let (sender, _) = super::super::federation_mailboxes::local_participant(
            &case.pair.central, &format!("Capacity {number}"));
        let send = super::super::federation_mailboxes::request(&case.pair.central, &sender,
            FederationMailboxOperation::Send { target: reader.clone(), message: "pending page".into(),
                key: format!("capacity-{number}"), topic: None, state_stamp: None });
        let sent = case.pair.central.execute_federation_mailbox(send, None).unwrap().value;
        reads.push(FederationMailboxRequest {
            operation: FederationMailboxOperation::Read { mailbox_id: sent["mailboxId"].as_str().unwrap().into(),
                after_sequence: None, limit: 200, issue_receipt: true }, ..case.request.clone()
        });
    }
    let original = &reads[0];
    let writer = case.pair.central.mailbox_store.write_lock.clone();
    let held = lock_sqlite_state_writer(&writer);
    let first_key = new_key(&case);
    let first = execute(&case, keyed(&case, original, &first_key, false)).unwrap();
    assert_eq!(first["receiptState"], "receiptPending");
    for read in reads.iter().take(FEDERATION_MAILBOX_FROZEN_KEYS).skip(1) {
        let page = execute(&case, keyed(&case, read, &new_key(&case), false)).unwrap();
        assert_eq!(page["receiptState"], "receiptPending");
    }
    let protocol = execute(&case, FederationMailboxRequest {
        operation: FederationMailboxOperation::Protocol, ..case.request.clone()
    }).unwrap();
    let full = execute(&case, reads.last().unwrap().clone()).unwrap_err();
    let retained = execute(&case, keyed(&case, original, &first_key, true)).unwrap();
    let incarnation = case.pair.central.mailbox_store.federation_reads.lock().unwrap().incarnation.clone();
    let expired_key = federation_mailbox_new_read_key(&incarnation,
        federation_mailbox_read_now_ms().saturating_sub(FEDERATION_MAILBOX_READ_TTL_MS + 1));
    let expired = execute(&case, keyed(&case, original, &expired_key, false)).unwrap_err();
    drop(held);
    case.pair.finish().await;
    assert!(protocol["readKey"].is_null(), "a seventeenth admission cannot evict a pending page");
    assert_eq!(full.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retained["readKey"], first_key);
    assert_eq!(expired.status, StatusCode::GONE);
}

#[path = "federation_mailbox_read_regressions.rs"]
mod review_regressions;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_same_key_callers_share_one_frozen_page() {
    let case = MailboxCase::with_writer_timeout(Duration::from_millis(100)).await;
    let original = read_request(&case).await;
    let key = new_key(&case);
    let request = keyed(&case, &original, &key, false);
    let writer = case.pair.central.mailbox_store.write_lock.clone();
    let held = lock_sqlite_state_writer(&writer);
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut jobs = Vec::new();
    for _ in 0..2 {
        let app = case.pair.central.clone();
        let lease = case.pair.lease.clone();
        let request = request.clone();
        let barrier = barrier.clone();
        jobs.push(tokio::task::spawn_blocking(move || {
            barrier.wait();
            app.execute_federation_mailbox(request, Some(lease)).unwrap().value
        }));
    }
    barrier.wait();
    let first = jobs.remove(0).await.unwrap();
    let second = jobs.remove(0).await.unwrap();
    drop(held);
    let count = case.pair.central.mailbox_store.federation_reads.lock().unwrap().entries.len();
    case.pair.finish().await;
    assert_eq!(count, 1);
    assert_eq!(first, second);
    assert!(first["receipt"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cached_issued_page_still_checks_mailbox_participant_retirement() {
    let case = MailboxCase::new().await;
    let original = read_request(&case).await;
    let key = new_key(&case);
    let page = execute(&case, keyed(&case, &original, &key, false)).unwrap();
    let mailbox = page["messages"][0]["mailboxId"].as_str().unwrap();
    let reader = case.request.actor.as_ref().unwrap().binding.participant_key();
    case.pair.central.mailbox_store.connection().unwrap().execute(
        "UPDATE mailbox_participants SET left_at='retired-by-fixture' WHERE mailbox_id=?1 AND session_id=?2",
        rusqlite::params![mailbox, reader]).unwrap();
    let retired = execute(&case, keyed(&case, &original, &key, true));
    case.pair.finish().await;
    assert!(retired.is_err(), "retained historical receipt does not grant current read authority");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_expiry_survives_the_client_error_boundary() {
    let case = MailboxCase::new().await;
    let original = read_request(&case).await;
    let client = case.client(case.central_port());
    let lease = client.channel.current_lease().unwrap();
    let expired = client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
        lease, keyed(&case, &original, &new_key(&case), true))).await.unwrap_err();
    case.pair.finish().await;
    assert_eq!(expired.status, StatusCode::GONE);
    assert_eq!(expired.kind, Some(ApiErrorKind::FederationMailboxReadExpired));
    assert!(expired.message.contains("readKeyExpired"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_actor_fence_failure_does_not_enter_the_mailbox_append() {
    let case = MailboxCase::new().await;
    let mut app = case.pair.central.clone();
    let (send, receive) = mpsc::channel();
    app.persist_tx = send;
    app.persist_worker_alive = Arc::new(AtomicBool::new(true));
    let writer = std::thread::spawn(move || {
        while let Ok(request) = receive.recv() {
            if let PersistRequest::Fence(fence) = request {
                fence.finish(Err(PersistFenceError::Deadline));
            }
        }
    });
    let mut changed = case.request.clone();
    changed.actor.as_mut().unwrap().name = "Changed directory entry".to_owned();
    let refused = app.execute_federation_mailbox(changed, Some(case.pair.lease.clone())).unwrap_err();
    let count = case.message_count();
    drop(app);
    writer.join().unwrap();
    case.pair.finish().await;
    assert_eq!(refused.kind, Some(ApiErrorKind::FederationMailboxDirectoryFence));
    assert_eq!(count, 0, "a directory fence receipt is required before requested append entry");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_mailbox_version_is_refused_before_any_operation() {
    let case = MailboxCase::new().await;
    let mut legacy = case.request.clone();
    legacy.protocol_version = 1;
    legacy.schema_version = "1".to_owned();
    let client = case.client(case.central_port());
    let lease = client.channel.current_lease().unwrap();
    let refused = client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
        lease.clone(), legacy.clone())).await.unwrap_err();
    let repeated = client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
        lease, legacy)).await.unwrap_err();
    let count = case.message_count();
    case.pair.finish().await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert_eq!(refused.kind, Some(ApiErrorKind::FederationMailboxUpgradeRequired));
    assert_eq!(repeated.kind, refused.kind);
    assert!(refused.message.contains("upgradeRequired"));
    assert_eq!(count, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_native_read_reuses_the_bridge_owned_pending_key_without_new_arguments() {
    let case = MailboxCase::with_writer_timeout(Duration::from_millis(20)).await;
    let client = case.client(case.central_port());
    let lease = client.channel.current_lease().unwrap();
    let sent = client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
        lease.clone(), case.request.clone())).await.unwrap();
    let target = match &case.request.operation {
        FederationMailboxOperation::Send { target, .. } => target,
        _ => unreachable!(),
    };
    let native = case.pair.central.inner.lock().unwrap().federation.mailbox_directory
        .get(target).unwrap().binding.identity.session_id.clone();
    let mailbox = sent.value["mailboxId"].as_str().unwrap().to_owned();
    let writer = case.pair.central.mailbox_store.write_lock.clone();
    let held = lock_sqlite_state_writer(&writer);
    let first = case.pair.central.read_federation_mailbox(&native, mailbox.clone(), None, 200, true, None)
        .await.unwrap();
    drop(held);
    let mut later = case.request.clone();
    if let FederationMailboxOperation::Send { key, message, .. } = &mut later.operation {
        *key = "later-native-incoming".to_owned();
        *message = "later native incoming".to_owned();
    }
    client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(lease, later)).await.unwrap();
    let resolved = case.pair.central.read_federation_mailbox(&native, mailbox, None, 200, true, None)
        .await.unwrap();
    let remaining = case.pair.central.mailbox_store.federation_read_intents.lock().unwrap().len();
    case.pair.finish().await;
    assert_eq!(first.receipt_state.as_deref(), Some("receiptPending"));
    assert!(resolved.receipt.is_some());
    assert_eq!(without_issuance(serde_json::to_value(resolved).unwrap()),
        serde_json::to_value(first).unwrap());
    assert_eq!(remaining, 0, "issued reads release their bridge intent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_transient_503_then_same_key_resolve_returns_the_committed_page() {
    let case = MailboxCase::new().await;
    let original = read_request(&case).await;
    let key = new_key(&case);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = FederationListenerState { channel: case.pair.lease.channel.clone(),
        ensure_current: super::super::federation_link::allowed() };
    let app = case.pair.central.clone();
    let lease = case.pair.lease.clone();
    let first = Arc::new(AtomicBool::new(true));
    let recorded = Arc::new(Mutex::new(None));
    let saved = recorded.clone();
    let router = Router::new().route("/federation/v1/mailbox", post(move |Json(request): Json<FederationMailboxRequest>| {
        let app = app.clone();
        let lease = lease.clone();
        let first = first.clone();
        let saved = saved.clone();
        async move {
            let original = request.clone();
            let response = run_blocking_api(move || app.execute_federation_mailbox(request, Some(lease))).await.unwrap();
            if first.swap(false, Ordering::SeqCst) {
                *saved.lock().unwrap() = Some(response.value.clone());
                federation_mailbox_unknown_response(Some(&original), "handler_deadline")
            } else { Json(response).into_response() }
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
    let uncertain = client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
        lease.clone(), keyed(&case, &original, &key, false))).await.unwrap_err();
    incoming(&case, "after-transient", "later message outside frozen page");
    let resolved = client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
        lease, keyed(&case, &original, &key, true))).await.unwrap().value;
    stop.send_replace(true);
    task.await.unwrap();
    let committed = recorded.lock().unwrap().take().unwrap();
    case.pair.finish().await;
    assert_eq!(uncertain.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(uncertain.message.contains(FEDERATION_MAILBOX_UNKNOWN_OUTCOME));
    assert!(uncertain.message.contains(&key));
    assert!(!uncertain.message.contains("idempotencyKey"));
    assert_eq!(resolved, committed);
}
