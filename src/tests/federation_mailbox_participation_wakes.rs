// Preserved native history keeps its own wake and acknowledgement ownership.
// Provider handoff uses a disposable test channel, never a live runtime.
use super::*;

async fn preserved_history_wake(satellite: bool, boot: bool) {
    let pair = qualified_pair().await;
    let state = if satellite { &pair.satellite } else { &pair.central };
    let sender = native(state, "History sender");
    let target = test_session_id(state, Agent::Claude);
    let runtime_id = "preserved-history-wake";
    let (runtime, input) = test_claude_runtime_handle(runtime_id);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&target).unwrap();
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }
    let sent = state.append_mailbox_message_and_notify(&sender, SendMailboxMessageRequest {
        target_session_id: target.clone(), message: "private history body".into(),
        idempotency_key: "history-before-binding".into(), topic: None, state_stamp: None,
        class: Some("routine".into()),
    }).unwrap();
    assert_eq!(sent.notification_disposition, "queuedBehindActiveTurn");
    let central_root = native(&pair.central, "Central directory reader");
    directory(&pair.central, &central_root).await;
    assert!(state.federation_mailbox_actor(&target).unwrap().is_some());
    if boot {
        // Model the volatile queue loss at boot, then run the production
        // all-unread recovery entrypoint against the retained native rows.
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&target).unwrap();
        inner.sessions[index].queued_prompts.clear();
        sync_pending_prompts(&mut inner.sessions[index]);
        drop(inner);
        state.reconcile_unread_mailbox_wakeups_after_boot();
    } else {
        let page = read_mailbox(AxumPath((target.clone(), sent.mailbox_id.clone())),
            State(state.clone()), Json(ReadMailboxRequest {
                read_key: None, issue_receipt: true, after_sequence: None, limit: 20,
            })).await.unwrap().0;
        let _ = acknowledge_mailbox(AxumPath((target.clone(), sent.mailbox_id.clone())),
            State(state.clone()), Json(AcknowledgeMailboxRequest {
                receipt: page.receipt, expected_processed_through: None,
                processed_through: None, no_continuation: false,
            })).await.unwrap();
    }
    state.finish_turn_ok_if_runtime_matches(&target, &RuntimeToken::Claude(runtime_id.into())).unwrap();
    let command = input.try_recv();
    let stored = state.mailbox_store.read_message(&target, &sent.message_id).unwrap();
    pair.finish().await;
    let Ok(ClaudeRuntimeCommand::Prompt(command)) = command else {
        panic!("preserved standalone wake must reach the runtime after binding (satellite={satellite}, boot={boot})");
    };
    assert!(command.text.contains(&sent.mailbox_id));
    assert!(!command.text.contains("private history body"));
    assert!(command.text.contains(if boot { "unread message(s)" } else { "already acknowledged" }));
    assert_eq!(stored.notification_state, "deliveredToIdleSession",
        "handoff marking must update the original native mailbox row");
}

#[tokio::test]
async fn automatic_central_history_acknowledged_wake_reaches_runtime() { preserved_history_wake(false, false).await; }

#[tokio::test]
async fn automatic_satellite_history_acknowledged_wake_reaches_runtime() { preserved_history_wake(true, false).await; }

#[tokio::test]
async fn automatic_central_history_boot_recovery_reaches_runtime() { preserved_history_wake(false, true).await; }

#[tokio::test]
async fn automatic_satellite_history_boot_recovery_reaches_runtime() { preserved_history_wake(true, true).await; }

#[tokio::test]
async fn reenrolled_namesake_stays_unavailable_until_reverified() {
    let pair = qualified_pair().await;
    let sender = native(&pair.central, "Sender");
    native(&pair.central, "Reviewer");
    native(&pair.satellite, "Reviewer");
    directory(&pair.central, &sender).await;
    let peer = pair.central.inner.lock().unwrap().federation.peers[&pair.id].clone();
    pair.central.enroll_federation_peer(peer).unwrap();
    let snapshot = pair.central.mailbox_directory_snapshot(&sender).await.unwrap();
    let sent = send_mailbox_message(AxumPath(sender), State(pair.central.clone()),
        Json(serde_json::from_value(json!({"targetSessionId":"Reviewer",
            "idempotencyKey":"reenrolled-alias","message":"must not choose local"})).unwrap())).await;
    let unavailable = snapshot.unavailable_hosts.iter().any(|host| host.host_id == pair.id);
    pair.finish().await;
    assert!(unavailable, "re-enrollment cannot erase a previously qualified namesake from uniqueness");
    assert!(sent.is_err());
}

#[tokio::test]
async fn routing_only_exact_duplicate_retains_notification_decision() {
    let pair = qualified_pair().await;
    let c = native(&pair.central, "C");
    native(&pair.satellite, "S");
    directory(&pair.central, &c).await;
    let delivery = pair.central.prepare_federation_delivery(&c, FederationMailboxOperation::Send {
        target: "S".into(), message: "routing-only exact".into(), key: "routing-only-exact".into(),
        topic: None, state_stamp: None,
    }).await.unwrap();
    let request = || federation_mailboxes::request(&pair.central, &c,
        FederationMailboxOperation::SendExact { delivery: delivery.clone() });
    pair.central.execute_federation_mailbox(request(), None).unwrap();
    let replay = pair.central.execute_federation_mailbox(request(), None).unwrap();
    let outcome: FederationDeliveryOutcome = serde_json::from_value(replay.value).unwrap();
    pair.finish().await;
    let FederationDeliveryOutcome::Committed(receipt) = outcome else { panic!("committed outcome required"); };
    assert!(receipt.duplicate);
    assert_ne!(receipt.notification_cause.as_deref(), Some(FEDERATION_REMOTE_WAKE_CAUSE));
}
