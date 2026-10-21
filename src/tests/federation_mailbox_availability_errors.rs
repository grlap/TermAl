// Owns authenticated no-write classifications and conservative failure controls.
// Uses real mailbox RPC and isolated writer admission, never a live pair.
use super::*;
use mailbox_fixture::MailboxCase;

async fn classified_reply(
    case: &MailboxCase,
    status: StatusCode,
    body: Value,
) -> Result<FederationMailboxResponse, ApiError> {
    // The response contents are adversarial, but the real middleware supplies
    // the MAC, counter and request binding. This tests validation AFTER auth.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = FederationListenerState {
        channel: case.pair.lease.channel.clone(),
        ensure_current: super::super::federation_link::allowed(),
    };
    let router = Router::new()
        .route("/federation/v1/mailbox", post(move || {
            let body = body.clone();
            async move { (status, Json(body)) }
        }))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(), federation_authenticate,
        ))
        .with_state(state);
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move {
        axum::serve(listener, router).with_graceful_shutdown(async move {
            federation_wait_stop(&mut stopped).await;
        }).await.unwrap();
    });
    let client = case.client(port);
    let lease = client.channel.current_lease().unwrap();
    let result = client.request(FederationRpcRoute::Mailbox(lease, case.request.clone())).await;
    stop.send_replace(true);
    task.await.unwrap();
    result
}

fn writer_envelope(case: &MailboxCase) -> Value {
    json!({"mailboxError": {
        "class": "writer_admission_timeout",
        "effect": "notCommitted",
        "operation": "send",
        "requestDigest": sha256_hex(&serde_json::to_vec(&case.request).unwrap()),
    }})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verified_allowlisted_envelope_retains_only_its_safe_no_write_class() {
    let case = MailboxCase::new().await;
    let result = classified_reply(&case, StatusCode::SERVICE_UNAVAILABLE,
        writer_envelope(&case)).await;
    case.pair.finish().await;
    let error = result.err().expect("a classified refusal remains an error");
    assert!(error.message.contains("writer_admission_timeout"));
    assert!(!error.message.contains(FEDERATION_MAILBOX_UNKNOWN_OUTCOME));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_invalid_envelopes_never_acquire_safe_retry_status() {
    let case = MailboxCase::new().await;
    let original = writer_envelope(&case);
    let mut observations = Vec::new();
    for kind in ["class", "effect", "operation", "digest", "extra", "oversized", "status"] {
        let mut body = original.clone();
        let mut status = StatusCode::SERVICE_UNAVAILABLE;
        match kind {
            "class" => body["mailboxError"]["class"] = json!("unrecognized"),
            "effect" => body["mailboxError"]["effect"] = json!("committed"),
            "operation" => body["mailboxError"]["operation"] = json!("acknowledge"),
            "digest" => body["mailboxError"]["requestDigest"] = json!("0".repeat(64)),
            "extra" => body["mailboxError"]["detail"] = json!("SECRET_ENVELOPE_SENTINEL"),
            "oversized" => body["mailboxError"]["detail"] = json!("SECRET_ENVELOPE_SENTINEL".repeat(256)),
            "status" => status = StatusCode::INTERNAL_SERVER_ERROR,
            _ => unreachable!(),
        }
        observations.push((kind, classified_reply(&case, status, body).await));
    }
    case.pair.finish().await;
    for (kind, result) in observations {
        let error = result.err().expect("invalid envelope must not be success");
        assert!(error.message.contains(FEDERATION_MAILBOX_UNKNOWN_OUTCOME),
            "invalid {kind} must stay UNKNOWN: {}", error.message);
        assert!(!error.message.contains("SECRET_ENVELOPE_SENTINEL"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writer_refusal_preserves_its_class_without_claiming_an_unknown_append() {
    let case = MailboxCase::with_writer_timeout(Duration::from_millis(100)).await;
    let client = case.client(case.central_port());
    let lease = client.channel.current_lease().unwrap();
    let writer = case.pair.central.mailbox_store.write_lock.clone();
    let held = lock_sqlite_state_writer(&writer);
    let refused = client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
        lease.clone(), case.request.clone(),
    )).await;
    drop(held);
    let absent = case.message_count();
    let retried = client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
        lease, case.request.clone(),
    )).await;
    let committed = case.message_count();
    case.pair.finish().await;
    assert_eq!(absent, 0, "writer admission refusal must precede append COMMIT");
    assert_eq!(committed, 1, "the identical application key remains usable");
    assert!(retried.is_ok(), "release permits a real committed reply: {retried:?}");
    let error = refused.err().expect("held writer must refuse this attempt");
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(error.message.contains("writer_admission_timeout"),
        "verified writer refusal lost its bounded class: {}", error.message);
    assert!(!error.message.contains(FEDERATION_MAILBOX_UNKNOWN_OUTCOME),
        "a certified pre-write refusal must not become UNKNOWN");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unchanged_actor_append_does_not_enter_the_primary_writer() {
    let case = MailboxCase::new().await;
    let client = case.client(case.central_port());
    let lease = client.channel.current_lease().unwrap();
    let primary = sqlite_state_write_lock(case.pair.central.persistence_path.as_path());
    let held = lock_sqlite_state_writer(&primary);
    let response = client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
        lease, case.request.clone(),
    )).await;
    drop(held);
    let committed = case.message_count();
    case.pair.finish().await;
    assert!(response.is_ok(), "unchanged directory entry needs no primary fence: {response:?}");
    assert_eq!(committed, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_actor_refence_has_a_typed_unknown_reconcile_outcome() {
    let case = MailboxCase::new().await;
    let client = case.client(case.central_port());
    let lease = client.channel.current_lease().unwrap();
    let mut stale = case.request.clone();
    stale.actor.as_mut().unwrap().binding.placement_generation += 1;
    let refused = client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
        lease, stale,
    )).await;
    let count = case.message_count();
    case.pair.finish().await;
    assert_eq!(count, 0);
    let error = refused.err().expect("a different actor fence must be refused");
    assert!(error.message.contains("authority_changed"),
        "a real authority change needs a typed reconcile result: {}", error.message);
    assert!(error.message.contains(FEDERATION_MAILBOX_UNKNOWN_OUTCOME),
        "authority drift cannot certify no historical commitment");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_failure_text_never_escapes_the_authenticated_mailbox_boundary() {
    let case = MailboxCase::new().await;
    case.pair.central.mailbox_store.connection().unwrap().execute_batch(
        "CREATE TRIGGER secret_append_failure BEFORE INSERT ON federation_mailbox_wake_obligations
         BEGIN SELECT RAISE(ABORT, 'SECRET_SQLITE_PATH_CREDENTIAL_SENTINEL'); END;",
    ).unwrap();
    let client = case.client(case.central_port());
    let lease = client.channel.current_lease().unwrap();
    let refused = client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
        lease, case.request.clone(),
    )).await;
    let count = case.message_count();
    case.pair.finish().await;
    assert_eq!(count, 0);
    let error = refused.err().expect("trigger must refuse the actual transaction");
    assert!(!error.message.contains("SECRET_SQLITE_PATH_CREDENTIAL_SENTINEL"));
    assert!(error.message.contains(FEDERATION_MAILBOX_UNKNOWN_OUTCOME),
        "unclassified execution failures retain conservative uncertainty");
}
