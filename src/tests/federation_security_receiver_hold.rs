// Owns the real mailbox-handler post-commit hold/settlement regression.
// Does not forge responses or change durability recovery. Uses scoped phase
// observations to control the interleaving identified in the security review.
use super::*;
use mailbox_fixture::MailboxCase;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_security_receiver_hold_settled_before_signing_keeps_mailbox_unknown() {
    let case = MailboxCase::new().await;
    let mut channel = case.pair.lease.channel.clone();
    let central = case.pair.central.clone();
    let phase = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = Arc::new(Mutex::new(None));
    let committed = Arc::new(Mutex::new(None));
    let phase_hook = phase.clone();
    let observed_hook = observed.clone();
    let committed_hook = committed.clone();
    channel.control.observed = Arc::new(move |event, failed| {
        if event == "mailboxExecuted"
            && phase_hook
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            // Execution has returned a real committed receipt, with no state
            // lock held. Only the post-execution currency check sees this hold.
            let id: String = central
                .mailbox_store
                .connection()
                .unwrap()
                .query_row("SELECT id FROM mailbox_messages", [], |row| row.get(0))
                .unwrap();
            *committed_hook.lock().unwrap() = Some(id);
            central.inner.lock().unwrap().federation_durable = false;
        } else if event == "mailboxPostcheck" && phase_hook.load(Ordering::SeqCst) == 1 {
            *observed_hook.lock().unwrap() = Some(failed);
            central.inner.lock().unwrap().federation_durable = true;
            phase_hook.store(2, Ordering::SeqCst);
        }
    });
    // Serve the production router and handler with the already-attested shared
    // channel, adding only a per-fixture observer. The real RPC verifies its MAC.
    let checked = channel.clone();
    let router = federation_router(
        channel,
        Arc::new(move || checked.current_authority().map(|_| ())),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = case.client(listener.local_addr().unwrap().port());
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                federation_wait_stop(&mut stopped).await;
            })
            .await
            .unwrap();
    });
    let lease = client.channel.current_lease().unwrap();
    let context = client.channel.security_context().unwrap();
    let outcome = client
        .request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
            lease.clone(),
            case.request.clone(),
        ))
        .await;
    let count_after = case.message_count();
    let unknown = outcome.err().map(|error| {
        let guidance = error.message.contains("same idempotencyKey");
        let raw: anyhow::Error = TermalDelegationApiError {
            status: error.status,
            message: error.message,
        }
        .into();
        let classified = termal_delegation_outcome_is_unknown(&raw);
        let bridge = mailbox_send_bridge_error(raw);
        (
            classified,
            bridge
                .downcast_ref::<TermalDelegationUnknownOutcome>()
                .is_some(),
            guidance,
        )
    });
    let retained = client.channel.security_current(&context).is_ok()
        && lease.with_incarnation(|| Ok(())).is_ok();
    let settled = context.sender.lock().unwrap().outstanding.is_empty();
    let recovered = client
        .request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
            lease.clone(),
            case.request.clone(),
        ))
        .await;
    let final_count = case.message_count();
    let mut invalid = case.request.clone();
    invalid.actor.as_mut().unwrap().binding.placement_generation += 1;
    let known = client
        .request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(lease, invalid))
        .await
        .err()
        .map(|error| {
            let status = error.status;
            let raw: anyhow::Error = TermalDelegationApiError {
                status,
                message: error.message,
            }
            .into();
            (status, termal_delegation_outcome_is_unknown(&raw))
        });
    stop.send_replace(true);
    server.await.unwrap();
    case.pair.finish().await;
    // Drain the real server/control IO before judging the RED result.
    assert_eq!(phase.load(Ordering::SeqCst), 2);
    assert_eq!(
        *observed.lock().unwrap(),
        Some(1),
        "handler must observe the hold before it settles"
    );
    assert_eq!(count_after, 1);
    assert!(
        retained && settled,
        "application uncertainty preserves and settles the authenticated transport"
    );
    assert_eq!(
        unknown,
        Some((true, true, true)),
        "a committed send remains bridge-unknown after receiver settlement"
    );
    let recovered = recovered.unwrap();
    assert_eq!(
        recovered.value["messageId"],
        committed.lock().unwrap().as_ref().unwrap().as_str()
    );
    assert_eq!(recovered.value["duplicate"], true);
    assert_eq!(final_count, 1);
    assert_eq!(
        known,
        Some((StatusCode::CONFLICT, false)),
        "pre-admission application conflicts remain known"
    );
}
