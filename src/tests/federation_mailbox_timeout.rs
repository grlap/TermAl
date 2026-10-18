// Owns domain timeout/link-isolation and canceled worker capacity regressions.
// Does not enlarge production budgets or simulate SSH. Split from federation
// mailbox boundary coverage using real middleware and writer admission.
use super::*;

fn incoming_authority(channel: &FederationChannel) -> String {
    let mut caller = channel.snapshot.clone();
    std::mem::swap(&mut caller.local, &mut caller.peer);
    serde_json::to_string(&FederationRequestAuthority::outgoing(&caller)).unwrap()
}

fn authenticated(channel: &FederationChannel, lease: &FederationLinkLease, path: &str) -> axum::http::request::Builder {
    Request::builder().uri(path)
        .header(FEDERATION_CAPABILITY_HEADER, "a".repeat(64))
        .header(FEDERATION_AUTHORITY_HEADER, incoming_authority(channel))
        .header(FEDERATION_INCARNATION_HEADER, &lease.incarnation)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_deadline_keeps_link_and_worker_capacity_until_writer_settlement() {
    let mut state = federation_mailboxes::central_store();
    // Force the listener deadline, rather than racing the equal production
    // writer deadline. The writer is released by this test after the response.
    Arc::get_mut(&mut state.mailbox_store).unwrap().write_admission_timeout = phase_sync::DEADLOCK_GUARD;
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    let (_, _, lease, actor) = federation_mailbox_authority::remote(&state);
    let channel = lease.channel.clone();
    let router = federation_router(channel.clone(), federation_link::allowed());
    let request = FederationMailboxRequest {
        protocol_version: 1, schema_version: "1".to_owned(),
        central: actor.binding.central.clone(), actor: Some(actor),
        operation: FederationMailboxOperation::GetOrCreate { target: target.session_id.clone() },
    };
    let writer = state.mailbox_store.write_lock.clone();
    let held = lock_sqlite_state_writer(&writer);
    let next_ticket = sqlite_state_writer_issued_tickets(&writer) + 1;
    let http = authenticated(&channel, &lease, "/federation/v1/mailbox").method("POST")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&request).unwrap())).unwrap();
    let running = tokio::spawn(router.clone().oneshot(http));
    wait_for_sqlite_state_writer_issued_tickets(&writer, next_ticket);
    let response = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, running).await.unwrap().unwrap().unwrap();
    let link_current_at_deadline = lease.with_current(&state, |_, _| Ok(())).is_ok();
    let held_capacity_at_deadline = channel.bound.permits.available_permits();
    drop(held);
    // Acquiring all four permits also waits for the canceled blocking worker;
    // cleanup happens before any RED assertion, with no detached fixture work.
    let settled = tokio::time::timeout(phase_sync::DEADLOCK_GUARD,
        channel.bound.permits.clone().acquire_many_owned(4)).await.unwrap().unwrap();
    drop(settled);
    let retry = authenticated(&channel, &lease, "/federation/v1/mailbox").method("POST")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&request).unwrap())).unwrap();
    let retried = router.oneshot(retry).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(link_current_at_deadline, "mailbox contention invalidated the link");
    assert_eq!(held_capacity_at_deadline, 3, "canceled worker released its RPC capacity before settlement");
    assert_eq!(retried.status(), StatusCode::OK);
    let connection = state.mailbox_store.connection().unwrap();
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM mailboxes", [],
        |row| row.get::<_, u64>(0)).unwrap(), 1);
}

#[tokio::test]
async fn health_deadline_still_invalidates_its_exact_link_incarnation() {
    let channel = federation_link::channel(FederationRole::Central, 'a');
    let lease = channel.activate(Uuid::new_v4().to_string());
    let listener = FederationListenerState { channel: channel.clone(), ensure_current: federation_link::allowed() };
    let router = Router::new().route("/federation/v1/health", get(|| async {
        std::future::pending::<StatusCode>().await
    })).route_layer(axum::middleware::from_fn_with_state(listener.clone(), federation_authenticate))
        .with_state(listener);
    let response = router.oneshot(authenticated(&channel, &lease, "/federation/v1/health")
        .body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    assert!(lease.with_incarnation(|| Ok(())).is_err());
}
