// Domain exact reads must present the retained receiver cause consistently.
// Reuses the authenticated outcome fixture; receipt and ACK stay independent.
use super::*;

fn sender(fixture: &Fixture) -> String {
    fixture.central.inner.lock().unwrap().sessions.iter()
        .find(|record| record.session.name == "Sender")
        .expect("the fixture's local sender").session.id.clone()
}

fn exact(fixture: &Fixture) -> MailboxMessage {
    serde_json::from_value(fixture.central.execute_federation_mailbox(
        federation_mailboxes::request(&fixture.central, &sender(fixture),
            FederationMailboxOperation::ReadMessage { message_id: message(fixture) }),
        None,
    ).unwrap().value).unwrap()
}

fn append_receipt(fixture: &Fixture) -> Value {
    fixture.central.execute_federation_mailbox(federation_mailboxes::request(
        &fixture.central, &sender(fixture), FederationMailboxOperation::Send {
            target: fixture.target.participant_key(), message: "ordinary agent work".into(),
            key: "one-message".into(), topic: Some("handoff".into()), state_stamp: None,
        },
    ), None).unwrap().value
}

fn acknowledge_recipient(fixture: &Fixture) {
    let request = |operation| FederationMailboxRequest {
        protocol_version: FEDERATION_MAILBOX_WIRE_VERSION,
        schema_version: FEDERATION_MAILBOX_SCHEMA_VERSION.to_owned(),
        central: fixture.target.central.clone(),
        actor: Some(FederationMailboxActor {
            binding: fixture.target.clone(), name: "Satellite".into(),
        }),
        operation,
    };
    let mailbox_id = fixture.request["wake"]["mailboxId"].as_str().unwrap().to_owned();
    let lease = fixture.outbound.current_lease().unwrap();
    let page: MailboxReadResponse = serde_json::from_value(
        fixture.central.execute_federation_mailbox(request(FederationMailboxOperation::Read {
            mailbox_id: mailbox_id.clone(), after_sequence: None, limit: 10, issue_receipt: true,
        }), Some(lease.clone())).unwrap().value,
    ).unwrap();
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].id, message(fixture));
    fixture.central.execute_federation_mailbox(request(FederationMailboxOperation::Acknowledge {
        mailbox_id, receipt: page.receipt.expect("the real issued recipient receipt"),
    }), Some(lease)).unwrap();
}

fn assert_consistent(message: &MailboxMessage, acknowledged: bool) {
    let cause = message.notification_cause.as_deref().expect("the existing remote wake cause");
    let status = message.current_status.as_ref().expect("the exact-read status");
    assert_eq!(message.notification_state, "durableButNotWoken");
    assert_eq!(status.notification_state, message.notification_state);
    assert_eq!(status.recipient_acknowledged, acknowledged);
    assert!(message.notification_summary.contains(cause),
        "notification summary must describe its receiver cause: {message:?}");
    assert!(status.summary.contains(cause),
        "current status must describe its receiver cause: {message:?}");
    assert!(!message.notification_summary.contains("No wake confirmed"));
    assert!(!status.summary.contains("No wake confirmed"));
    assert!(status.summary.contains(if acknowledged {
        "The recipient has acknowledged it."
    } else {
        "Not yet acknowledged by the recipient."
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn domain_exact_read_summaries_follow_verified_wake_and_recipient_ack() {
    let mut fixture = Fixture::new();
    acknowledge_all_fences(&mut fixture.satellite);
    let input = root(&fixture, "domain-summary-woken", false);
    let original = append_receipt(&fixture);
    let (stop, server) = attach_loopback(&fixture).await;
    dispatch_and_activate(&fixture).await;
    let CodexRuntimeCommand::Prompt { .. } = phase_sync::receive(&input, "the remote provider prompt")
    else { panic!("the remote root must receive this wake"); };
    let turn = {
        let inner = fixture.satellite.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&fixture.native).unwrap()]
            .prompt_deliveries[&fixture.operation().queue_identity()].turn_generation
    };
    fixture.satellite.note_prompt_delivery_outcome(&fixture.native, turn, PromptDeliveryOutcome::Accepted);
    fixture.central.poll_federation_executor_outcomes().await;
    let row = outcome_row(&fixture).expect("the authenticated receiver's answer");
    let before_ack = exact(&fixture);
    acknowledge_recipient(&fixture);
    let after_ack = exact(&fixture);
    let repeated = append_receipt(&fixture);
    let _ = stop.send(());
    server.await.unwrap();

    assert_eq!(row.last_outcome, Some(FederationExecutorOutcome::Woken));
    assert!(row.terminal);
    let recorded_at = row.last_observed_at.as_deref().unwrap();
    assert!(before_ack.notification_cause.as_deref().unwrap().contains(recorded_at));
    assert_eq!(before_ack.notification_cause, after_ack.notification_cause);
    assert_eq!(original, repeated, "current status must not rewrite the append receipt");
    assert_consistent(&before_ack, false);
    assert_consistent(&after_ack, true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn domain_exact_read_summaries_keep_pending_and_unavailable_honest() {
    let mut fixture = Fixture::new();
    acknowledge_all_fences(&mut fixture.satellite);
    let input = root(&fixture, "domain-summary-pending", true);
    let original = append_receipt(&fixture);
    let (stop, server) = attach_loopback(&fixture).await;
    dispatch_and_activate(&fixture).await;
    // No outcome poll: queue ownership exists, but no receiver handoff is known.
    let pending = exact(&fixture);
    acknowledge_recipient(&fixture);
    let acknowledged = exact(&fixture);
    {
        // Make only this fixture's outcome query unavailable, without putting
        // an invalid enum through the production schema's CHECK constraint.
        let connection = fixture.central.mailbox_store.connection().unwrap();
        connection.execute_batch(
            "ALTER TABLE federation_executor_outcomes RENAME TO unavailable_fixture_outcomes"
        ).unwrap();
    }
    let unavailable = exact(&fixture);
    fixture.central.mailbox_store.connection().unwrap().execute_batch(
        "ALTER TABLE unavailable_fixture_outcomes RENAME TO federation_executor_outcomes"
    ).unwrap();
    let repeated = append_receipt(&fixture);
    let _ = stop.send(());
    server.await.unwrap();

    assert!(input.try_recv().is_err(), "a paused root must not receive a provider prompt");
    assert!(pending.notification_cause.as_deref().unwrap().contains("wake outcome pending"));
    assert_eq!(pending.notification_cause, acknowledged.notification_cause);
    assert!(unavailable.notification_cause.as_deref().unwrap().contains("unavailable"));
    assert_eq!(original, repeated, "ACK and unavailable evidence do not rewrite the receipt");
    for (message, acknowledged) in [(&pending, false), (&acknowledged, true), (&unavailable, true)] {
        assert_consistent(message, acknowledged);
        assert!(!message.notification_summary.contains("remote session woken"));
        assert!(!message.current_status.as_ref().unwrap().summary.contains("remote session woken"));
    }
}
