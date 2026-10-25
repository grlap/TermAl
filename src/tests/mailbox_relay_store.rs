// Native relay transaction/reload controls on real disposable coordination stores.
// Authority closures are explicit seams here; no authenticated-network claim.
use super::*;

fn store() -> (TestTempRoot, PathBuf, MailboxStore) {
    let root = TestTempRoot::create("native-mailbox-relay-store");
    let path = root.path().join("coordination.sqlite");
    let store = MailboxStore::open(&path).unwrap();
    (root, path, store)
}

fn envelope(store: &MailboxStore) -> (MailboxRelayAppend, FederationPeerSnapshot) {
    let remote = federation_registry::peer(47003);
    let central = FederationHostIdentity {
        host_id: Uuid::new_v4().to_string(),
        host_generation: 1,
    };
    let sender = MailboxRelayPrincipal {
        host: central.clone(),
        store_id: mailbox_relay_store_id(&store.connection().unwrap()).unwrap(),
        native_session_id: "session-1".into(),
        native_creation_id: Uuid::new_v4().to_string(),
    };
    let target = MailboxRelayPrincipal {
        host: remote.identity.clone(),
        store_id: Uuid::new_v4().to_string(),
        native_session_id: "session-1".into(),
        native_creation_id: Uuid::new_v4().to_string(),
    };
    let conversation = MailboxRelayConversation::new(
        MailboxRelayHome {
            host: central.clone(),
            store_id: sender.store_id.clone(),
        },
        sender.clone(),
        target.clone(),
    )
    .unwrap();
    let peer = FederationPeerSnapshot {
        local: central.clone(),
        local_role: FederationRole::Central,
        central,
        peer: remote.identity,
        enrollment_generation: 1,
        record_generation: 1,
        transport_generation: 1,
        pair_capability_ref: remote.pair_capability_ref,
        endpoint: remote.endpoint,
        membership: FederationMembershipState::Active,
    };
    (
        MailboxRelayAppend {
            requested_target: "session-remote-proxy".into(),
            conversation,
            sender,
            target,
            sender_name: "Sender".into(),
            target_name: "Mac root".into(),
            message: "read-only build question".into(),
            idempotency_key: "original-send".into(),
            topic: Some("Build".into()),
            state_stamp: None,
        },
        peer,
    )
}

fn count(store: &MailboxStore, table: &str) -> u64 {
    store
        .connection()
        .unwrap()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn mailbox_relay_review_home_wake_keeps_original_enrollment_through_reopen_and_retry() {
    let (_root, path, store) = store();
    let (relay, peer) = envelope(&store);
    let receipt = store
        .append_mailbox_relay(&relay, &peer, || Ok(()))
        .unwrap();
    drop(store);
    let store = MailboxStore::open(&path).unwrap();
    let mut reconnect = peer.clone();
    reconnect.record_generation += 1;
    reconnect.transport_generation += 1;
    assert!(
        store
            .append_mailbox_relay(&relay, &reconnect, || Ok(()))
            .unwrap()
            .duplicate
    );
    let original: String = store
        .connection()
        .unwrap()
        .query_row(
            "SELECT peer_json FROM mailbox_relay_wakes WHERE message_id=?1",
            [&receipt.message_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<FederationPeerSnapshot>(&original).unwrap(),
        peer,
        "a duplicate may not rebase immutable admission evidence"
    );
    reconnect.enrollment_generation += 1;
    assert!(
        store
            .append_mailbox_relay(&relay, &reconnect, || Ok(()))
            .is_err()
    );
    assert_eq!(count(&store, "mailbox_messages"), 1);
    store
        .connection()
        .unwrap()
        .execute("UPDATE mailbox_relay_wakes SET peer_json='{}'", [])
        .unwrap();
    drop(store);
    assert!(
        MailboxStore::open(&path).is_err(),
        "corrupt authority is not backfilled from current peer"
    );
}

#[test]
fn mailbox_relay_review_notification_duplicate_does_not_rebase_its_first_acceptance() {
    let (_root, path, store) = store();
    let (relay, peer) = envelope(&store);
    let mailbox = format!("mailbox-{}", Uuid::new_v4());
    store
        .remember_mailbox_relay(
            &relay.conversation,
            &mailbox,
            &["A".into(), "B".into()],
            || Ok(()),
        )
        .unwrap();
    let wake = MailboxRelayWake {
        version: MAILBOX_RELAY_VERSION,
        conversation: relay.conversation,
        sender: relay.target,
        target: relay.sender,
        mailbox_id: mailbox.clone(),
        message_id: format!("mailbox-message-{}", Uuid::new_v4()),
        sequence: 1,
        unread_count: 1,
        sender_name: "Peer".into(),
        topic: None,
    };
    store
        .receive_mailbox_relay_notification(&wake, &peer, || Ok(()))
        .unwrap();
    drop(store);
    let store = MailboxStore::open(&path).unwrap();
    let mut changed = peer.clone();
    changed.transport_generation += 1;
    changed.record_generation += 1;
    store
        .receive_mailbox_relay_notification(&wake, &changed, || Ok(()))
        .unwrap();
    let original = store
        .mailbox_relay_notification(&wake.target, &mailbox, 1)
        .unwrap()
        .unwrap();
    assert_eq!(original.peer, peer);
    changed.enrollment_generation += 1;
    assert!(
        store
            .receive_mailbox_relay_notification(&wake, &changed, || Ok(()))
            .is_err()
    );
    assert_eq!(count(&store, "mailbox_relay_notifications"), 1);
    store
        .connection()
        .unwrap()
        .execute_batch("ALTER TABLE mailbox_relay_notifications DROP COLUMN peer_json")
        .unwrap();
    drop(store);
    assert!(
        MailboxStore::open(&path).is_err(),
        "incomplete same-marker draft is not silently repaired"
    );
}

#[test]
fn mailbox_relay_ack_before_delayed_native_wake_survives_store_reload_without_unread_claim() {
    let (_root, path, store) = store();
    let (relay, peer) = envelope(&store);
    let mailbox = format!("mailbox-{}", Uuid::new_v4());
    let names = ["First".to_owned(), "Second".to_owned()];
    store
        .remember_mailbox_relay(&relay.conversation, &mailbox, &names, || Ok(()))
        .unwrap();
    store
        .mark_mailbox_relay_notification(&relay.sender, &mailbox, 7, false, || Ok(()))
        .unwrap();
    drop(store);
    let store = MailboxStore::open(&path).unwrap();
    let wake = MailboxRelayWake {
        version: MAILBOX_RELAY_VERSION,
        conversation: relay.conversation.clone(),
        sender: relay.target,
        target: relay.sender,
        mailbox_id: mailbox,
        message_id: format!("mailbox-message-{}", Uuid::new_v4()),
        sequence: 7,
        unread_count: 4,
        sender_name: "Delayed peer".into(),
        topic: None,
    };
    let retained = store
        .receive_mailbox_relay_notification(&wake, &peer, || Ok(()))
        .unwrap();
    assert!(retained.acknowledged);
    assert!(
        !retained.handed_off,
        "acknowledgement never certifies an executor handoff"
    );
    assert_eq!(
        mailbox_relay_unread_wake(&retained).unwrap().unread_count,
        0
    );
    assert_eq!(
        count(&store, "mailbox_messages"),
        0,
        "only wake metadata lives at this receiver"
    );
}

#[test]
fn mailbox_relay_key_cannot_cross_the_existing_standalone_namespace_in_either_order() {
    let (_root, _path, store) = store();
    let (relay, peer) = envelope(&store);
    let raw = MailboxAppendInput {
        sender_session_id: relay.sender.native_session_id.clone(),
        sender_name: "Local".into(),
        target_session_id: "session-other".into(),
        target_name: "Other".into(),
        body: "old local send".into(),
        idempotency_key: relay.idempotency_key.clone(),
        topic: None,
        state_stamp: None,
    };
    store.append(&raw).unwrap();
    assert!(store.pin_mailbox_relay(&relay, &peer, || Ok(())).is_err());
    let mut relay = relay;
    relay.idempotency_key = "relay-first".into();
    store.pin_mailbox_relay(&relay, &peer, || Ok(())).unwrap();
    let mut raw = raw;
    raw.idempotency_key = relay.idempotency_key;
    assert!(store.append(&raw).is_err());
    assert_eq!(count(&store, "mailbox_messages"), 1);
}

#[test]
fn mailbox_relay_store_two_way_same_home_preserves_raw_history_and_receipt_isolation() {
    let (_root, _path, store) = store();
    let old = MailboxAppendInput {
        sender_session_id: "old-peer".into(),
        sender_name: "Old peer".into(),
        target_session_id: "session-1".into(),
        target_name: "Sender".into(),
        body: "untouched local history".into(),
        idempotency_key: "old-key".into(),
        topic: None,
        state_stamp: None,
    };
    let raw = store.append(&old).unwrap().receipt.clone();
    let (relay, peer) = envelope(&store);
    let sent = store
        .append_mailbox_relay(&relay, &peer, || Ok(()))
        .unwrap();
    let mut reply = relay.clone();
    std::mem::swap(&mut reply.sender, &mut reply.target);
    std::mem::swap(&mut reply.sender_name, &mut reply.target_name);
    reply.message = "current build reply".into();
    reply.idempotency_key = "reply".into();
    let replied = store
        .append_mailbox_relay(&reply, &peer, || Ok(()))
        .unwrap();
    assert_eq!(sent.mailbox_id, replied.mailbox_id);
    assert_eq!((sent.sequence, replied.sequence), (1, 2));
    assert_eq!(
        store
            .read_message("session-1", &raw.message_id)
            .unwrap()
            .body,
        "untouched local history"
    );
    assert!(
        store.read_message("session-1", &sent.message_id).is_err(),
        "raw id is not the relay principal"
    );
    let actor = relay.sender.participant_key();
    let page = store
        .read_range_with_issuance(&actor, &sent.mailbox_id, None, 50, true)
        .unwrap();
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].body, "current build reply");
    let receipt = page.receipt.unwrap();
    assert!(
        store
            .acknowledge_page("session-1", &raw.mailbox_id, &receipt)
            .is_err()
    );
    store
        .acknowledge_page(&actor, &sent.mailbox_id, &receipt)
        .unwrap();
    assert_eq!(
        mailbox_processed_through(&store.connection().unwrap(), &raw.mailbox_id, "session-1")
            .unwrap(),
        0
    );
    assert_eq!(
        mailbox_processed_through(&store.connection().unwrap(), &sent.mailbox_id, &actor).unwrap(),
        2
    );
    assert_eq!(count(&store, "mailbox_relay_associations"), 1);
    assert_eq!(count(&store, "mailbox_relay_wakes"), 2);
}

#[test]
fn mailbox_relay_store_lost_reply_and_reload_reuse_original_append_and_wake() {
    let (_root, path, store) = store();
    let (relay, peer) = envelope(&store);
    assert!(
        store
            .pin_mailbox_relay(&relay, &peer, || Ok(()))
            .unwrap()
            .is_none()
    );
    let committed = store
        .append_mailbox_relay(&relay, &peer, || Ok(()))
        .unwrap();
    drop(store); // Lost response: the metadata pin has no settled receipt yet.
    let reopened = MailboxStore::open(&path).unwrap();
    assert!(
        reopened
            .pin_mailbox_relay(&relay, &peer, || Ok(()))
            .unwrap()
            .is_none()
    );
    let duplicate = reopened
        .append_mailbox_relay(&relay, &peer, || Ok(()))
        .unwrap();
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.message_id, committed.message_id);
    assert_eq!(duplicate.mailbox_id, committed.mailbox_id);
    assert_eq!(count(&reopened, "mailbox_messages"), 1);
    assert_eq!(count(&reopened, "mailbox_relay_wakes"), 1);
    reopened
        .settle_mailbox_relay(&relay, &duplicate, || Ok(()))
        .unwrap();
    assert_eq!(
        reopened
            .pin_mailbox_relay(&relay, &peer, || Ok(()))
            .unwrap()
            .unwrap()
            .message_id,
        committed.message_id
    );
    let mut changed = relay.clone();
    changed.message.push_str(" changed");
    assert!(
        reopened
            .pin_mailbox_relay(&changed, &peer, || Ok(()))
            .is_err()
    );
    assert!(
        reopened
            .append_mailbox_relay(&changed, &peer, || Ok(()))
            .is_err()
    );
    assert_eq!(count(&reopened, "mailbox_messages"), 1);
    validate_current_coordination_schema(&reopened.connection().unwrap()).unwrap();
}

#[test]
fn mailbox_relay_store_authority_failure_rolls_back_message_membership_wake_and_pin() {
    let (_root, _path, store) = store();
    let (relay, peer) = envelope(&store);
    assert!(
        store
            .append_mailbox_relay(&relay, &peer, || Err::<(), _>(relay_store_error(
                "owner changed"
            )))
            .is_err()
    );
    for table in [
        "mailboxes",
        "mailbox_messages",
        "mailbox_relay_associations",
        "mailbox_relay_wakes",
    ] {
        assert_eq!(count(&store, table), 0, "{table}");
    }
    assert!(
        store
            .pin_mailbox_relay(&relay, &peer, || Err::<(), _>(relay_store_error(
                "owner changed"
            )))
            .is_err()
    );
    assert_eq!(count(&store, "mailbox_relay_intents"), 0);
    assert!(
        store
            .pin_mailbox_relay(&relay, &peer, || Ok(()))
            .unwrap()
            .is_none()
    );
}

#[test]
fn mailbox_relay_store_recreated_sender_target_peer_or_home_cannot_retarget_a_pin() {
    let (_root, _path, store) = store();
    let (relay, peer) = envelope(&store);
    store.pin_mailbox_relay(&relay, &peer, || Ok(())).unwrap();
    for mutation in ["sender", "target", "home"] {
        let mut changed = relay.clone();
        match mutation {
            "sender" => changed.sender.native_creation_id = Uuid::new_v4().to_string(),
            "target" => changed.target.native_creation_id = Uuid::new_v4().to_string(),
            "home" => changed.conversation.home.store_id = Uuid::new_v4().to_string(),
            _ => unreachable!(),
        }
        if mutation != "home" {
            changed.conversation = MailboxRelayConversation::new(
                relay.conversation.home.clone(),
                changed.sender.clone(),
                changed.target.clone(),
            )
            .unwrap();
        }
        assert!(
            store.pin_mailbox_relay(&changed, &peer, || Ok(())).is_err(),
            "{mutation}"
        );
    }
    let mut other_peer = peer;
    other_peer.enrollment_generation += 1;
    assert!(
        store
            .pin_mailbox_relay(&relay, &other_peer, || Ok(()))
            .is_err()
    );
    assert_eq!(count(&store, "mailbox_relay_intents"), 1);
    assert_eq!(count(&store, "mailbox_messages"), 0);
}

#[test]
fn mailbox_relay_store_replacement_home_refuses_before_append() {
    let (_root, _path, store) = store();
    let (relay, peer) = envelope(&store);
    let (_other, _other_path, replacement) = self::store();
    assert!(
        replacement
            .append_mailbox_relay(&relay, &peer, || Ok(()))
            .is_err()
    );
    assert_eq!(count(&replacement, "mailbox_messages"), 0);
    assert_eq!(count(&replacement, "mailbox_relay_associations"), 0);
}

#[test]
fn mailbox_relay_pin_allows_only_connection_accounting_to_advance_not_enrollment_material() {
    let (_root, _path, store) = store();
    let (relay, peer) = envelope(&store);
    store.pin_mailbox_relay(&relay, &peer, || Ok(())).unwrap();
    let mut connected = peer.clone();
    connected.transport_generation += 1;
    connected.record_generation += 1;
    assert!(
        !peer.same_transport_as(&connected),
        "shared transport admission stays exact"
    );
    assert!(same_mailbox_relay_enrollment(&peer, &connected));
    store
        .pin_mailbox_relay(&relay, &connected, || Ok(()))
        .unwrap();
    for mutation in [
        "enrollment",
        "capability",
        "endpoint",
        "host",
        "central",
        "revoked",
    ] {
        let mut changed = connected.clone();
        match mutation {
            "enrollment" => changed.enrollment_generation += 1,
            "capability" => changed.pair_capability_ref = "replacement-pair".into(),
            "endpoint" => changed.endpoint.ssh_port += 1,
            "host" => changed.peer.host_generation += 1,
            "central" => changed.central.host_generation += 1,
            "revoked" => changed.membership = FederationMembershipState::Revoked,
            _ => unreachable!(),
        }
        assert!(
            !same_mailbox_relay_enrollment(&peer, &changed),
            "{mutation}"
        );
        assert!(
            store
                .pin_mailbox_relay(&relay, &changed, || Ok(()))
                .is_err(),
            "{mutation}"
        );
    }
    assert_eq!(
        store
            .mailbox_relay_intent(&relay.sender.native_session_id, &relay.idempotency_key)
            .unwrap()
            .unwrap()
            .1,
        peer,
        "logical recovery never rewrites the original pin provenance"
    );
}
