// Owns local/remote target key scope and authenticated sender namespaces.
// Does not fake remote wake acceptance. Namespace controls use current
// placements; alias-renaming coverage uses authenticated loopback discovery.
use super::*;

pub(super) fn remote_actor(
    state: &AppState,
    name: &str,
    port: u16,
) -> (FederationMailboxActor, FederationLinkLease) {
    let peer = federation_registry::active(state, federation_registry::peer(port));
    let placement = state
        .reserve_federation_placement(
            &peer,
            Uuid::new_v4().to_string(),
            "session-1".to_owned(),
            0,
            0,
        )
        .unwrap();
    let channel = FederationChannel::for_app(
        peer.clone(),
        FederationPairCapability::new("a".repeat(64)).unwrap(),
        state.clone(),
    );
    let lease = channel.activate(placement.link_incarnation.clone());
    let actor = FederationMailboxActor {
        binding: FederationMailboxBinding {
            central: peer.local,
            identity: placement.identity,
            placement_generation: placement.placement_generation,
        },
        name: name.to_owned(),
    };
    state
        .execute_federation_mailbox(
            request(&actor, FederationMailboxOperation::Register),
            Some(lease.clone()),
        )
        .unwrap();
    (actor, lease)
}

fn request(
    actor: &FederationMailboxActor,
    operation: FederationMailboxOperation,
) -> FederationMailboxRequest {
    FederationMailboxRequest {
        protocol_version: FEDERATION_MAILBOX_WIRE_VERSION,
        schema_version: FEDERATION_MAILBOX_SCHEMA_VERSION.to_owned(),
        central: actor.binding.central.clone(),
        actor: Some(actor.clone()),
        operation,
    }
}

fn send(target: &str, key: &str) -> FederationMailboxOperation {
    FederationMailboxOperation::Send {
        target: target.to_owned(),
        message: "identical semantic body".to_owned(),
        key: key.to_owned(),
        topic: None,
        state_stamp: None,
    }
}

#[tokio::test]
async fn federation_delivery_same_key_cannot_switch_between_local_and_remote_targets() {
    let state = federation_mailboxes::central_store();
    let (sender, actor) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, local) = federation_mailboxes::local_participant(&state, "Local target");
    let (remote, _) = remote_actor(&state, "Remote target", 48001);
    let remote_key = remote.binding.participant_key();
    let original: MailboxAppendReceipt = state
        .federation_mailbox_operation(&sender, send(&local.session_id, "cross-target"))
        .await
        .unwrap();
    assert_eq!(
        state
            .federation_mailbox_operation::<Value>(&sender, send(&remote_key, "cross-target"))
            .await
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    let other: MailboxAppendReceipt = state
        .federation_mailbox_operation(&sender, send(&remote_key, "remote-first"))
        .await
        .unwrap();
    assert_eq!(other.notification_disposition, "durableButNotWoken");
    assert_eq!(
        state
            .federation_mailbox_operation::<Value>(&sender, send(&local.session_id, "remote-first"))
            .await
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    assert_ne!(other.message_id, original.message_id);
    let connection = state.mailbox_store.connection().unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM federation_mailbox_intents WHERE sender_session_id=?1",
                [&actor.session_id],
                |row| row.get::<_, u64>(0),
            )
            .expect("remote and local targets must share one retained sender registry"),
        2
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM federation_mailbox_wake_obligations WHERE message_id=?1",
                [&other.message_id],
                |row| row.get::<_, u64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn federation_delivery_equal_remote_local_ids_have_distinct_durable_sender_dedup_namespaces() {
    let state = federation_mailboxes::central_store();
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    let (left, left_lease) = remote_actor(&state, "Left host", 48001);
    let (right, right_lease) = remote_actor(&state, "Right host", 48002);
    assert_eq!(
        left.binding.identity.session_id,
        right.binding.identity.session_id
    );
    assert_ne!(
        left.binding.identity.host_id,
        right.binding.identity.host_id
    );
    let first = state
        .execute_federation_mailbox(
            request(&left, send(&target.session_id, "same-key")),
            Some(left_lease),
        )
        .unwrap()
        .value;
    let second = state
        .execute_federation_mailbox(
            request(&right, send(&target.session_id, "same-key")),
            Some(right_lease),
        )
        .unwrap()
        .value;
    assert_ne!(first["messageId"], second["messageId"]);
    let connection = state.mailbox_store.connection().unwrap();
    let rows: (u64, u64) = connection.query_row(
        "SELECT COUNT(*), COUNT(DISTINCT sender_host_id) FROM federation_mailbox_dedup WHERE idempotency_key='same-key'",
        [], |row| Ok((row.get(0)?, row.get(1)?)),
    ).expect("authenticated host identity is part of central dedup identity");
    assert_eq!(rows, (2, 2));
}

#[tokio::test]
async fn federation_delivery_remote_rename_changes_discovery_without_changing_digest() {
    let pair = federation_onboarding_fixture::Pair::with_enrolled_profile(
        federation_mailboxes::central_store(), federation_onboarding_fixture::io(),
        federation_onboarding_fixture::io(), federation_onboarding_fixture::profile(),
    ).await;
    assert!(pair.verify().await.unwrap().termal_complete());
    let state = &pair.central;
    let (sender, actor) = federation_mailboxes::local_participant(state, "Sender");
    let remote = test_session_id(&pair.satellite, Agent::Codex);
    {
        let mut inner = pair.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&remote).unwrap();
        let root = inner.session_mut_by_index(index).unwrap();
        root.session.name = "Before".into();
        root.session.status = SessionStatus::Active;
    }
    let original: MailboxAppendReceipt = state
        .federation_mailbox_operation(&sender, send("Before", "rename"))
        .await
        .unwrap();
    let digest: String = state.mailbox_store.connection().unwrap().query_row(
        "SELECT canonical_digest FROM federation_mailbox_intents WHERE sender_session_id=?1 AND idempotency_key='rename'",
        [&actor.session_id], |row| row.get(0),
    ).expect("digest must describe exact qualified identity, not display aliases");
    {
        let mut inner = pair.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&remote).unwrap();
        inner.session_mut_by_index(index).unwrap().session.name = "After".into();
    }
    let _: Value = pair.satellite.federation_mailbox_operation(
        &remote, FederationMailboxOperation::Register).await.unwrap();
    let retry: MailboxAppendReceipt = state
        .federation_mailbox_operation(&sender, send("After", "rename"))
        .await
        .unwrap();
    assert_eq!(retry.message_id, original.message_id);
    assert!(retry.duplicate);
    assert_eq!(state.mailbox_store.connection().unwrap().query_row(
        "SELECT canonical_digest FROM federation_mailbox_intents WHERE sender_session_id=?1 AND idempotency_key='rename'",
        [&actor.session_id], |row| row.get::<_, String>(0),
    ).unwrap(), digest);
    pair.finish().await;
}
