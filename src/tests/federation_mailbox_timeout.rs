// Owns domain timeout/link-isolation and canceled worker capacity regressions.
// Does not enlarge production budgets or simulate SSH. Split from federation
// mailbox boundary coverage using real middleware and writer admission.
use super::*;

struct MailboxWorkerRelease(Arc<(Mutex<bool>, std::sync::Condvar)>);

impl Drop for MailboxWorkerRelease {
    fn drop(&mut self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}

fn authenticated(
    sender: &FederationChannel,
    counter: u64,
    method: &str,
    path: &str,
    body: &[u8],
) -> Request<Body> {
    let mut headers = axum::http::HeaderMap::new();
    if method == "POST" {
        headers.insert("content-type", HeaderValue::from_static("application/json"));
    }
    federation_control_fixture::signed(sender, counter, method, path, headers, body)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_deadline_keeps_link_and_worker_capacity_until_worker_settlement() {
    let state = federation_mailboxes::central_store();
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    // Establish the actual proof before verifying and reserving placement;
    // reconnecting an already-placed legacy fixture would fence its actor.
    let record = federation_registry::peer(48001);
    let id = record.identity.host_id.clone();
    state.enroll_federation_peer(record).unwrap();
    let mut channel = FederationChannel::for_app(
        state.federation_peer_snapshot(&id).unwrap(),
        FederationPairCapability::new("a".repeat(64)).unwrap(),
        state.clone(),
    );
    let (sender, _proof) = federation_control_fixture::establish(&channel).await;
    let connected = state.federation_peer_snapshot(&id).unwrap();
    state
        .verify_federation_peer(&connected, federation_registry::checks())
        .unwrap();
    let verified = state.federation_peer_snapshot(&id).unwrap();
    state
        .transition_federation_peer(&verified, FederationMembershipState::Active)
        .unwrap();
    let peer = state.federation_peer_snapshot(&id).unwrap();
    let lease = channel.current_lease().unwrap();
    let placement = state
        .reserve_federation_placement(
            &peer,
            "remote-first".to_owned(),
            "session-1".to_owned(),
            0,
            0,
        )
        .unwrap();
    let actor = FederationMailboxActor {
        binding: FederationMailboxBinding {
            central: peer.local.clone(),
            identity: placement.identity,
            placement_generation: placement.placement_generation,
        },
        name: "Remote".to_owned(),
    };
    state
        .execute_federation_mailbox(
            FederationMailboxRequest {
                protocol_version: FEDERATION_MAILBOX_WIRE_VERSION,
                schema_version: FEDERATION_MAILBOX_SCHEMA_VERSION.to_owned(),
                central: peer.local.clone(),
                actor: Some(actor.clone()),
                operation: FederationMailboxOperation::Register,
            },
            Some(lease.clone()),
        )
        .unwrap();
    let phase = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let release = MailboxWorkerRelease(phase.clone());
    let armed = Arc::new(AtomicBool::new(false));
    let armed_hook = armed.clone();
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let entered = Mutex::new(Some(entered));
    channel.control.observed = Arc::new(move |event, _| {
        if event == "mailboxExecuted" && armed_hook.swap(false, Ordering::SeqCst) {
            entered.lock().unwrap().take().unwrap().send(()).unwrap();
            let (released, _) = phase
                .1
                .wait_timeout_while(phase.0.lock().unwrap(), phase_sync::DEADLOCK_GUARD, |done| {
                    !*done
                })
                .unwrap();
            assert!(*released, "test did not release the executed mailbox worker");
        }
    });
    let router = federation_router(channel.clone(), federation_link::allowed());
    let request = FederationMailboxRequest {
        protocol_version: FEDERATION_MAILBOX_WIRE_VERSION,
        schema_version: FEDERATION_MAILBOX_SCHEMA_VERSION.to_owned(),
        central: actor.binding.central.clone(),
        actor: Some(actor),
        operation: FederationMailboxOperation::GetOrCreate {
            target: target.session_id.clone(),
        },
    };
    let writer = state.mailbox_store.write_lock.clone();
    let held = lock_sqlite_state_writer(&writer);
    let next_ticket = sqlite_state_writer_issued_tickets(&writer) + 1;
    let http = authenticated(
        &sender,
        1,
        "POST",
        "/federation/v1/mailbox",
        &serde_json::to_vec(&request).unwrap(),
    );
    let running = tokio::spawn(router.clone().oneshot(http));
    wait_for_sqlite_state_writer_issued_tickets(&writer, next_ticket);
    let refused = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let refused_status = refused.status();
    let refused_body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(refused.into_body(), 4096).await.unwrap(),
    )
    .unwrap();
    let link_current_after_refusal = lease.with_current(&state, |_, _| Ok(())).is_ok();
    let capacity_after_refusal = channel.bound.permits.available_permits();
    let count_after_refusal: u64 = state
        .mailbox_store
        .connection()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM mailboxes", [], |row| row.get(0))
        .unwrap();
    drop(held);

    // Bounded writer refusal is not a canceled worker. Create the mailbox,
    // then pause the next existing-mailbox request after its real COMMIT.
    let created = router
        .clone()
        .oneshot(authenticated(
            &sender,
            2,
            "POST",
            "/federation/v1/mailbox",
            &serde_json::to_vec(&request).unwrap(),
        ))
        .await
        .unwrap();
    let count_before_deadline: u64 = state
        .mailbox_store
        .connection()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM mailboxes", [], |row| row.get(0))
        .unwrap();
    armed.store(true, Ordering::SeqCst);
    let running = tokio::spawn(router.clone().oneshot(authenticated(
        &sender,
        3,
        "POST",
        "/federation/v1/mailbox",
        &serde_json::to_vec(&request).unwrap(),
    )));
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, entered_rx)
        .await
        .unwrap()
        .unwrap();
    let response = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let response_status = response.status();
    let response_body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 4096).await.unwrap(),
    )
    .unwrap();
    let link_current_at_deadline = lease.with_current(&state, |_, _| Ok(())).is_ok();
    let held_capacity_at_deadline = channel.bound.permits.available_permits();
    drop(release);
    // Drain the canceled blocking worker before judging either control.
    let settled = tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        channel.bound.permits.clone().acquire_many_owned(4),
    )
    .await
    .unwrap()
    .unwrap();
    drop(settled);
    let capacity_after_settlement = channel.bound.permits.available_permits();
    let retry = authenticated(
        &sender,
        4,
        "POST",
        "/federation/v1/mailbox",
        &serde_json::to_vec(&request).unwrap(),
    );
    let retried = router.oneshot(retry).await.unwrap();
    assert_eq!(refused_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        refused_body["mailboxError"]["class"],
        "writer_admission_timeout"
    );
    assert_eq!(refused_body["mailboxError"]["effect"], "notCommitted");
    assert_eq!(refused_body["mailboxError"]["operation"], "getOrCreate");
    assert!(link_current_after_refusal);
    assert_eq!(capacity_after_refusal, 4);
    assert_eq!(count_after_refusal, 0);
    assert_eq!(created.status(), StatusCode::OK);
    assert_eq!(count_before_deadline, 1);
    assert_eq!(response_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response_body["mailboxError"]["class"], "handler_deadline");
    assert_eq!(response_body["mailboxError"]["effect"], "unknown");
    assert_eq!(response_body["mailboxError"]["operation"], "getOrCreate");
    assert!(
        link_current_at_deadline,
        "mailbox contention invalidated the link"
    );
    assert_eq!(capacity_after_settlement, 4);
    assert_eq!(
        held_capacity_at_deadline, 3,
        "canceled worker released its RPC capacity before settlement"
    );
    assert_eq!(retried.status(), StatusCode::OK);
    let connection = state.mailbox_store.connection().unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM mailboxes", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn health_deadline_still_invalidates_its_exact_link_incarnation() {
    let channel = federation_link::channel(FederationRole::Central, 'a');
    let (sender, _proof) = federation_control_fixture::establish(&channel).await;
    let lease = channel.current_lease().unwrap();
    let listener = FederationListenerState {
        channel: channel.clone(),
        ensure_current: federation_link::allowed(),
    };
    let router = Router::new()
        .route(
            "/federation/v1/health",
            get(|| async { std::future::pending::<StatusCode>().await }),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            listener.clone(),
            federation_authenticate,
        ))
        .with_state(listener);
    let response = router
        .oneshot(authenticated(
            &sender,
            1,
            "GET",
            "/federation/v1/health",
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    assert!(lease.with_incarnation(|| Ok(())).is_err());
}
