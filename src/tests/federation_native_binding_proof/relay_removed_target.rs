// Removed recipients retire wake metadata without acknowledging their messages.
use super::*;

fn home_facts(app: &AppState) -> (u64, u64, u64) {
    let connection = app.mailbox_store.connection().unwrap();
    (
        connection
            .query_row("SELECT count(*) FROM mailbox_messages", [], |row| {
                row.get(0)
            })
            .unwrap(),
        connection
            .query_row(
                "SELECT coalesce(sum(acknowledged_through),0) FROM mailbox_relay_associations",
                [],
                |row| row.get(0),
            )
            .unwrap(),
        connection
            .query_row(
                "SELECT coalesce(sum(delivered),0) FROM mailbox_relay_wakes",
                [],
                |row| row.get(0),
            )
            .unwrap(),
    )
}

async fn removed_recipient_boundary(boot: bool) {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let (append, receipt) = append_to_central(&fixture, "removed-recipient-boundary");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    assert!(
        fixture
            .central
            .mailbox_store
            .mailbox_relay_notification(&append.target, &receipt.mailbox_id, receipt.sequence,)
            .unwrap()
            .is_some(),
        "a genuine accepted wake must precede removal"
    );
    fixture.central.kill_session(&fixture.native).unwrap();
    let before = home_facts(&fixture.central);
    let persisted = {
        let inner = fixture.central.inner.lock().unwrap();
        PersistedState::from_inner(&inner)
    };
    let restored = AppState {
        inner: Arc::new(StateMutex::new(persisted.into_inner().unwrap())),
        mailbox_store: Arc::new(MailboxStore::open(&fixture.central.mailbox_store.path).unwrap()),
        ..fixture.central.clone()
    };
    if boot {
        let mut recovery = MailboxRelayBootRecovery::default();
        restored
            .reconcile_mailbox_relay_notifications_after_boot(&mut recovery)
            .await
            .unwrap();
        assert!(
            recovery.complete(),
            "a removed recipient must not keep boot recovery retrying"
        );
    } else {
        restored.run_mailbox_relay_wake_batch(0, 0).await;
    }
    assert!(
        restored
            .mailbox_store
            .mailbox_relay_notification(&append.target, &receipt.mailbox_id, receipt.sequence,)
            .unwrap()
            .is_none(),
        "a removed recipient's wake metadata must be retired"
    );
    assert_eq!(
        home_facts(&restored),
        before,
        "retiring a wake must not delete mail, acknowledge it, or fabricate delivery"
    );
    let message: String = restored
        .mailbox_store
        .connection()
        .unwrap()
        .query_row(
            "SELECT body FROM mailbox_messages WHERE id=?1",
            [&receipt.message_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(message, append.message);
    // Repeated ordinary and boot recovery have no orphan left to retry.
    restored.run_mailbox_relay_wake_batch(0, 0).await;
    let mut recovery = MailboxRelayBootRecovery::default();
    restored
        .reconcile_mailbox_relay_notifications_after_boot(&mut recovery)
        .await
        .unwrap();
    assert!(recovery.complete());
    assert_eq!(home_facts(&restored), before);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removed_recipient_boot_retires_only_notification_metadata() {
    removed_recipient_boundary(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removed_recipient_worker_retires_only_notification_metadata() {
    removed_recipient_boundary(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_roots_and_incomplete_inventory_keep_notification_obligations() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for control in ["live", "hidden", "quarantined", "starting", "failed"] {
        let fixture = Fixture::new(false).await;
        let _handles = central_handle(&fixture);
        let (append, receipt) = append_to_central(&fixture, control);
        fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
        if matches!(control, "quarantined" | "starting" | "failed") {
            fixture.central.kill_session(&fixture.native).unwrap();
        }
        {
            let mut inner = fixture.central.inner.lock().unwrap();
            match control {
                "hidden" => {
                    let index = inner.find_session_index(&fixture.native).unwrap();
                    inner.sessions[index].hidden = true;
                }
                "quarantined" => {
                    inner
                        .quarantined_persisted_session_ids
                        .insert(fixture.native.clone());
                }
                "starting" => {
                    inner.delegation_inventory = DelegationInventoryState::Loading {
                        token: "fixture-loading".into(),
                        source: delegation_inventory_file_identity(
                            &fixture.central.mailbox_store.path,
                        )
                        .unwrap(),
                        worker_started: false,
                    };
                }
                "failed" => {
                    inner.delegation_inventory =
                        DelegationInventoryState::Failed { reason: "fixture" }
                }
                _ => {}
            }
        }
        let before = home_facts(&fixture.central);
        let mut boot = MailboxRelayBootRecovery::default();
        fixture
            .central
            .reconcile_mailbox_relay_notifications_after_boot(&mut boot)
            .await
            .unwrap();
        fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
        assert!(
            fixture
                .central
                .mailbox_store
                .mailbox_relay_notification(&append.target, &receipt.mailbox_id, receipt.sequence,)
                .unwrap()
                .is_some(),
            "{control} is not a removed recipient"
        );
        if control != "live" {
            assert!(!boot.complete(), "{control} must remain recoverable");
            assert_eq!(boot.retry.len(), 1);
        }
        assert_eq!(home_facts(&fixture.central), before, "{control}");
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_is_exact_and_requires_the_original_local_host_and_store() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let (append, receipt) = append_to_central(&fixture, "removed-exact");
    let healthy = test_session_id(&fixture.central, Agent::Codex);
    {
        let mut inner = fixture.central.inner.lock().unwrap();
        let index = inner.find_session_index(&healthy).unwrap();
        inner.sessions[index].orchestrator_auto_dispatch_blocked = true;
    }
    let (other, other_receipt) = append_to_central_target(&fixture, "healthy-exact", &healthy);
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let wake = fixture
        .central
        .mailbox_store
        .mailbox_relay_notification(&append.target, &receipt.mailbox_id, receipt.sequence)
        .unwrap()
        .unwrap()
        .wake;
    fixture.central.kill_session(&fixture.native).unwrap();
    let before = home_facts(&fixture.central);
    let mut wrong_store = wake.clone();
    wrong_store.target.store_id = Uuid::new_v4().to_string();
    let mut wrong_host = wake.clone();
    wrong_host.target.host.host_id = Uuid::new_v4().to_string();
    for mut changed in [wrong_store, wrong_host] {
        let mut home = changed.conversation.home.clone();
        home.host = changed.target.host.clone();
        home.store_id = changed.target.store_id.clone();
        changed.conversation =
            MailboxRelayConversation::new(home, changed.sender.clone(), changed.target.clone())
                .unwrap();
        changed.validate().unwrap();
        assert!(
            fixture
                .central
                .retire_removed_mailbox_relay_notification(&changed)
                .is_err()
        );
        assert!(
            fixture
                .central
                .mailbox_store
                .mailbox_relay_notification(&append.target, &receipt.mailbox_id, receipt.sequence,)
                .unwrap()
                .is_some()
        );
    }
    assert!(
        fixture
            .central
            .retire_removed_mailbox_relay_notification(&wake)
            .unwrap()
    );
    assert!(
        fixture
            .central
            .mailbox_store
            .mailbox_relay_notification(&append.target, &receipt.mailbox_id, receipt.sequence,)
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .central
            .mailbox_store
            .mailbox_relay_notification(
                &other.target,
                &other_receipt.mailbox_id,
                other_receipt.sequence,
            )
            .unwrap()
            .is_some(),
        "another recipient's notification must survive"
    );
    assert_eq!(home_facts(&fixture.central), before);
    // Repeated retirement is harmless and has no changed row to log again.
    assert!(
        fixture
            .central
            .retire_removed_mailbox_relay_notification(&wake)
            .unwrap()
    );
    assert_eq!(home_facts(&fixture.central), before);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_remote_home_keeps_a_live_recipient_pending() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        inner.sessions[index].orchestrator_auto_dispatch_blocked = true;
    }
    let sender = fixture
        .central
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap();
    let target = fixture
        .satellite
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap();
    let append = MailboxRelayAppend {
        test_request: None,
        requested_target: target.principal.participant_key(),
        conversation: MailboxRelayConversation::new(
            fixture.central.native_mailbox_relay_home().unwrap(),
            sender.principal.clone(),
            target.principal.clone(),
        )
        .unwrap(),
        sender: sender.principal,
        target: target.principal,
        sender_name: sender.name,
        target_name: target.name,
        message: "remote home body remains canonical".into(),
        idempotency_key: "unavailable-home-control".into(),
        topic: None,
        state_stamp: None,
    };
    let receipt: MailboxAppendReceipt = serde_json::from_value(
        fixture
            .central
            .execute_mailbox_relay(
                MailboxRelayOperation::Send {
                    append: append.clone(),
                },
                &fixture.channel.current_lease().unwrap(),
                &append.sender.host,
            )
            .unwrap(),
    )
    .unwrap();
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    assert!(
        fixture
            .satellite
            .mailbox_store
            .mailbox_relay_notification(&append.target, &receipt.mailbox_id, receipt.sequence,)
            .unwrap()
            .is_some()
    );
    fixture
        .satellite
        .federation_peer_handles
        .lock()
        .unwrap()
        .clear();
    let before = home_facts(&fixture.central);
    let mut boot = MailboxRelayBootRecovery::default();
    fixture
        .satellite
        .reconcile_mailbox_relay_notifications_after_boot(&mut boot)
        .await
        .unwrap();
    fixture.satellite.run_mailbox_relay_wake_batch(0, 0).await;
    assert!(!boot.complete());
    assert_eq!(boot.retry.len(), 1);
    assert!(
        fixture
            .satellite
            .mailbox_store
            .mailbox_relay_notification(&append.target, &receipt.mailbox_id, receipt.sequence,)
            .unwrap()
            .is_some()
    );
    assert_eq!(home_facts(&fixture.central), before);
    fixture.finish().await;
}
