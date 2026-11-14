// Real relay store transactions with the native authority adapter. The existing
// authorization callback parks before COMMIT; no network or elapsed-time claim.
use super::*;

fn pending_append(fixture: &Fixture, key: &str) -> MailboxRelayAppend {
    let sender = fixture
        .satellite
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap();
    let target = fixture
        .central
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap();
    MailboxRelayAppend {
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
        message: "body retained across a paused native relay commit".into(),
        idempotency_key: key.into(),
        topic: None,
        state_stamp: None,
    }
}

fn authority_for(
    fixture: &Fixture,
    append: &MailboxRelayAppend,
) -> (MailboxRelayAuthority, FederationPeerSnapshot) {
    let lease = fixture.channel.current_lease().unwrap();
    let peer = lease
        .with_current(&fixture.central, |_, peer| Ok(peer.clone()))
        .unwrap();
    let authority = fixture
        .central
        .mailbox_relay_home_authority(
            &lease,
            &append.conversation.home,
            &[append.sender.clone(), append.target.clone()],
        )
        .unwrap();
    (authority, peer)
}

fn durable_counts(app: &AppState) -> (u64, u64, u64) {
    app.mailbox_store
        .connection()
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM mailbox_messages),
                    (SELECT count(*) FROM mailbox_relay_wakes),
                    (SELECT count(*) FROM mailbox_relay_associations)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_admitted_append_releases_state_before_its_store_commit() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let append = pending_append(&fixture, "paused-native-append");
    let (authority, peer) = authority_for(&fixture, &append);
    let (state_available, link_fenced, receipt) = std::thread::scope(|scope| {
        // Drop opens the gate even if the driver fails before joining.
        let release = phase_sync::ReleaseGateOnDrop::new();
        let gate = release.gate();
        let (authorized_tx, authorized_rx) = std::sync::mpsc::channel();
        let store = &fixture.central.mailbox_store;
        let append = &append;
        let peer = &peer;
        let authority = &authority;
        let worker = scope.spawn(move || {
            store.append_mailbox_relay(
                append,
                peer,
                || authority.lock(),
                |_| {
                    authorized_tx.send(()).unwrap();
                    phase_sync::wait_for_gate(&gate, true);
                    Ok(())
                },
            )
        });
        phase_sync::receive(&authorized_rx, "native append authorized before COMMIT");
        // The transaction cannot reach COMMIT until release. This single probe
        // measures lock availability at that phase, not scheduling or latency.
        let state_available = fixture.central.inner.try_lock_now().is_some();
        let link_fenced = matches!(
            fixture.channel.state.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        );
        release.release();
        let receipt = worker.join().unwrap().unwrap();
        (state_available, link_fenced, receipt)
    });

    // Inspect the real committed result and exact-key replay after releasing the
    // worker, so a RED assertion cannot poison or strand its held authority.
    let retry: MailboxAppendReceipt = serde_json::from_value(
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
    let message = fixture
        .central
        .mailbox_store
        .read_message(&append.target.participant_key(), &receipt.message_id)
        .unwrap();
    let wakes: u64 = fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM mailbox_relay_wakes WHERE message_id=?1",
            [&receipt.message_id],
            |row| row.get(0),
        )
        .unwrap();
    let counts = durable_counts(&fixture.central);
    fixture.finish().await;
    assert!(
        state_available,
        "an admitted relay transaction must release StateInner before its own store commit"
    );
    assert!(link_fenced, "link retirement stays fenced through COMMIT");
    assert_eq!(message.body, append.message);
    assert!(retry.duplicate);
    assert_eq!(retry.message_id, receipt.message_id);
    assert_eq!(retry.sequence, receipt.sequence);
    assert_eq!(wakes, 1, "exact-key replay retains one wake obligation");
    assert_eq!(counts, (1, 1, 1), "exact-key replay appends nothing twice");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_stale_authority_before_admission_rolls_back_tentative_append() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for stale in ["root", "transport", "configuration"] {
        let fixture = Fixture::production_shape().await;
        let append = pending_append(&fixture, stale);
        let (authority, peer) = authority_for(&fixture, &append);
        match stale {
            "root" => {
                let mut inner = fixture.central.inner.lock().unwrap();
                let index = inner.find_session_index(&fixture.native).unwrap();
                inner.sessions[index].hidden = true;
            }
            "transport" => fixture.channel.invalidate(),
            "configuration" => {
                fixture
                    .central
                    .inner
                    .lock()
                    .unwrap()
                    .preferences
                    .federation
                    .enabled = false;
            }
            _ => unreachable!(),
        }
        let error = fixture
            .central
            .mailbox_store
            .append_mailbox_relay(&append, &peer, || authority.lock(), |_| Ok(()))
            .map_err(mailbox_api_error)
            .err()
            .expect("stale authority refuses the tentative append");
        let counts = durable_counts(&fixture.central);
        fixture.finish().await;
        assert_eq!(error.status, StatusCode::CONFLICT, "{stale}");
        let reason = match stale {
            "root" => "mailbox participant must be a local root session",
            "transport" => NATIVE_MAILBOX_RELAY_LINK_CHANGED,
            "configuration" => "federation is disabled",
            _ => unreachable!(),
        };
        assert_eq!(error.message, reason, "{stale} changed before admission");
        assert_eq!(counts, (0, 0, 0), "{stale} refuses without durable effects");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_preflight_quarantine_refuses_both_new_send_and_exact_retry() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let append = pending_append(&fixture, "qualified-original");
    let send = |append: MailboxRelayAppend| {
        fixture.central.execute_mailbox_relay(
            MailboxRelayOperation::Send {
                append: append.clone(),
            },
            &fixture.channel.current_lease().unwrap(),
            &append.sender.host,
        )
    };
    let _original: MailboxAppendReceipt =
        serde_json::from_value(send(append.clone()).unwrap()).unwrap();
    let before = durable_counts(&fixture.central);
    fixture
        .central
        .inner
        .lock()
        .unwrap()
        .federation
        .admission_quarantine = Some(FederationQuarantineReason::DeclaredRestore);
    let retry_error = send(append.clone())
        .err()
        .expect("quarantined home refuses the retry");
    let mut next = append;
    next.idempotency_key = "unqualified-new-send".into();
    let next_error = send(next)
        .err()
        .expect("quarantined home refuses the new send");
    let after = durable_counts(&fixture.central);
    fixture.finish().await;
    for error in [retry_error, next_error] {
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(
            error
                .message
                .contains("central mailbox authority is quarantined"),
            "{}",
            error.message
        );
    }
    assert_eq!(before, (1, 1, 1));
    assert_eq!(
        after, before,
        "both preflight refusals leave the store unchanged"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_late_quarantine_refuses_new_commit_but_preserves_exact_retry() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let append = pending_append(&fixture, "late-qualified-original");
    let original: MailboxAppendReceipt = serde_json::from_value(
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
    // The early home check has passed. Quarantine arrives before the actual
    // commit-adjacent authority cut, where only NEW send qualification changes.
    let (authority, peer) = authority_for(&fixture, &append);
    let before = durable_counts(&fixture.central);
    fixture
        .central
        .inner
        .lock()
        .unwrap()
        .federation
        .admission_quarantine = Some(FederationQuarantineReason::DeclaredRestore);
    let retry = fixture
        .central
        .mailbox_store
        .append_mailbox_relay(
            &append,
            &peer,
            || authority.lock(),
            |guard| guard.authorize_new_send(),
        )
        .unwrap();
    let mut next = append;
    next.idempotency_key = "late-unqualified-new-send".into();
    let next_error = fixture
        .central
        .mailbox_store
        .append_mailbox_relay(
            &next,
            &peer,
            || authority.lock(),
            |guard| guard.authorize_new_send(),
        )
        .map_err(mailbox_api_error)
        .err()
        .expect("late quarantine refuses new commit qualification");
    let after = durable_counts(&fixture.central);
    fixture.finish().await;
    assert!(retry.duplicate);
    assert_eq!(retry.message_id, original.message_id);
    assert_eq!(retry.mailbox_id, original.mailbox_id);
    assert_eq!(retry.sequence, original.sequence);
    assert_eq!(next_error.status, StatusCode::CONFLICT);
    assert_eq!(next_error.message, FEDERATION_STALE_AUTHORITY);
    assert_eq!(before, (1, 1, 1));
    assert_eq!(
        after, before,
        "duplicate and refused new commit have no new durable effects"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_in_flight_commit_keeps_admitted_root_facts_after_replacement() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let append = pending_append(&fixture, "admitted-before-replacement");
    let (authority, peer) = authority_for(&fixture, &append);
    let replacement = Uuid::new_v4().to_string();
    let (changed, receipt) = std::thread::scope(|scope| {
        let release = phase_sync::ReleaseGateOnDrop::new();
        let gate = release.gate();
        let (authorized_tx, authorized_rx) = std::sync::mpsc::channel();
        let app = &fixture.central;
        let append = &append;
        let peer = &peer;
        let authority = &authority;
        let worker = scope.spawn(move || {
            app.mailbox_store.append_mailbox_relay(
                append,
                peer,
                || authority.lock(),
                |guard| {
                    guard.authorize_new_send()?;
                    authorized_tx.send(()).unwrap();
                    phase_sync::wait_for_gate(&gate, true);
                    Ok(())
                },
            )
        });
        phase_sync::receive(&authorized_rx, "native root admitted before replacement");
        let changed = if let Some(mut inner) = fixture.central.inner.try_lock_now() {
            let index = inner.find_session_index(&fixture.native).unwrap();
            inner.sessions[index].federation_native_creation_id = replacement.clone();
            inner.sessions[index].session.name = "replacement root name".into();
            true
        } else {
            false
        };
        release.release();
        (changed, worker.join().unwrap().unwrap())
    });
    let stored = fixture
        .central
        .mailbox_store
        .read_message(&append.target.participant_key(), &receipt.message_id)
        .unwrap();
    let wake_json: String = fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .query_row(
            "SELECT wake_json FROM mailbox_relay_wakes WHERE message_id=?1",
            [&receipt.message_id],
            |row| row.get(0),
        )
        .unwrap();
    let wake: MailboxRelayWake = serde_json::from_str(&wake_json).unwrap();
    let mut next = append.clone();
    next.idempotency_key = "after-root-replacement".into();
    let next_result = fixture.central.execute_mailbox_relay(
        MailboxRelayOperation::Send { append: next },
        &fixture.channel.current_lease().unwrap(),
        &append.sender.host,
    );
    let counts = durable_counts(&fixture.central);
    fixture.finish().await;
    assert!(
        changed,
        "root replacement must complete while the admitted store commit is parked"
    );
    assert_eq!(stored.body, append.message);
    assert_eq!(stored.target_name, append.target_name);
    assert_eq!(
        wake.target, append.target,
        "the wake names the admitted root, never its replacement"
    );
    assert_ne!(wake.target.native_creation_id, replacement);
    assert_eq!(wake.message_id, receipt.message_id);
    assert_eq!(wake.sequence, receipt.sequence);
    let error = next_result
        .err()
        .expect("the next operation must refuse the stale root");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, "native mailbox session was replaced");
    assert_eq!(
        counts,
        (1, 1, 1),
        "the next stale request commits no effects"
    );
}
