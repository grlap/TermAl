// Owns revoked/recreated remote principal and commit-cut authorization proofs.
// Does not simulate SSH or grant runtime authority from payload ids. New M2
// controls over M1 placement, a fixed L1 channel, and real SQLite transactions.
use super::*;

pub(super) fn remote(
    state: &AppState,
) -> (
    FederationPeerSnapshot,
    FederationPlacement,
    FederationLinkLease,
    FederationMailboxActor,
) {
    let peer = federation_registry::active(state, federation_registry::peer(48001));
    let placement = state
        .reserve_federation_placement(
            &peer,
            "remote-first".to_owned(),
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
            central: peer.local.clone(),
            identity: placement.identity.clone(),
            placement_generation: placement.placement_generation,
        },
        name: "Remote".to_owned(),
    };
    state
        .execute_federation_mailbox(
            request(&actor, FederationMailboxOperation::Register),
            Some(lease.clone()),
        )
        .unwrap();
    (peer, placement, lease, actor)
}

fn request(
    actor: &FederationMailboxActor,
    operation: FederationMailboxOperation,
) -> FederationMailboxRequest {
    FederationMailboxRequest {
        protocol_version: 1,
        schema_version: "1".to_owned(),
        central: actor.binding.central.clone(),
        actor: Some(actor.clone()),
        operation,
    }
}

#[test]
fn revoked_peer_and_asserted_central_identity_have_no_access_to_existing_mail() {
    let state = federation_mailboxes::central_store();
    let (local, _) = federation_mailboxes::local_participant(&state, "Local");
    let (peer, _, lease, actor) = remote(&state);
    let req = federation_mailboxes::request(
        &state,
        &local,
        FederationMailboxOperation::Send {
            target: actor.binding.participant_key(),
            message: "stored only at C".to_owned(),
            key: "local".to_owned(),
            topic: None,
            state_stamp: None,
        },
    );
    let sent = state.execute_federation_mailbox(req, None).unwrap().value;
    let mut impersonation = actor.clone();
    impersonation.binding.identity.host_id = peer.local.host_id.clone();
    assert_eq!(
        state
            .execute_federation_mailbox(
                request(&impersonation, FederationMailboxOperation::List),
                Some(lease.clone())
            )
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    state
        .transition_federation_peer(&peer, FederationMembershipState::Revoked)
        .unwrap();
    for operation in [
        FederationMailboxOperation::List,
        FederationMailboxOperation::ReadMessage {
            message_id: sent["messageId"].as_str().unwrap().to_owned(),
        },
        FederationMailboxOperation::Read {
            mailbox_id: sent["mailboxId"].as_str().unwrap().to_owned(),
            after_sequence: None,
            limit: 50,
            issue_receipt: true,
        },
    ] {
        assert_eq!(
            state
                .execute_federation_mailbox(request(&actor, operation), Some(lease.clone()))
                .unwrap_err()
                .status,
            StatusCode::CONFLICT
        );
    }
    assert_eq!(
        state
            .mailbox_store
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM mailbox_messages", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn recreated_remote_placement_has_a_new_namespace_and_cannot_use_old_receipts() {
    let state = federation_mailboxes::central_store();
    let (local, _) = federation_mailboxes::local_participant(&state, "Local");
    let (peer, placement, lease, actor) = remote(&state);
    let sent = state
        .execute_federation_mailbox(
            federation_mailboxes::request(
                &state,
                &local,
                FederationMailboxOperation::Send {
                    target: actor.binding.participant_key(),
                    message: "old namespace".to_owned(),
                    key: "old".to_owned(),
                    topic: None,
                    state_stamp: None,
                },
            ),
            None,
        )
        .unwrap()
        .value;
    let mailbox = sent["mailboxId"].as_str().unwrap().to_owned();
    let page = state
        .execute_federation_mailbox(
            request(
                &actor,
                FederationMailboxOperation::Read {
                    mailbox_id: mailbox.clone(),
                    after_sequence: None,
                    limit: 50,
                    issue_receipt: true,
                },
            ),
            Some(lease.clone()),
        )
        .unwrap()
        .value;
    state
        .reconcile_federation_placement(&placement, true)
        .unwrap();
    let next = state
        .reserve_federation_placement(
            &peer,
            "remote-recreated".to_owned(),
            placement.identity.session_id.clone(),
            0,
            0,
        )
        .unwrap();
    let mut replacement = actor.clone();
    replacement.binding.identity = next.identity;
    replacement.binding.placement_generation = next.placement_generation;
    assert_ne!(
        actor.binding.participant_key(),
        replacement.binding.participant_key()
    );
    assert_eq!(
        state
            .execute_federation_mailbox(
                request(&actor, FederationMailboxOperation::List),
                Some(lease.clone())
            )
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    let list = state
        .execute_federation_mailbox(
            request(&replacement, FederationMailboxOperation::List),
            Some(lease.clone()),
        )
        .unwrap()
        .value;
    assert_eq!(list, json!([]));
    assert!(
        state
            .execute_federation_mailbox(
                request(
                    &replacement,
                    FederationMailboxOperation::Acknowledge {
                        mailbox_id: mailbox,
                        receipt: page["receipt"].as_str().unwrap().to_owned()
                    }
                ),
                Some(lease)
            )
            .is_err()
    );
}

#[test]
fn revocation_between_preflight_and_store_apply_rolls_back_the_whole_append() {
    let state = federation_mailboxes::central_store();
    let (_, target) = federation_mailboxes::local_participant(&state, "Local");
    let (peer, _, lease, actor) = remote(&state);
    let authority = FederationMailboxAuthority {
        app: state.clone(),
        actor: actor.binding.clone(),
        lease: Some(lease),
        target: Some(target.clone()),
    };
    // Capture a valid request, then revoke at the storage apply cut. The
    // transaction is already open; authorization must still refuse it there.
    state
        .validate_federation_session_assertion(&peer, &actor.binding.identity)
        .unwrap();
    let input = MailboxAppendInput {
        sender_session_id: actor.binding.participant_key(),
        sender_name: actor.name,
        target_session_id: target.session_id,
        target_name: target.name,
        body: "must not append".to_owned(),
        idempotency_key: "cut".to_owned(),
        topic: None,
        state_stamp: None,
    };
    let result = state.mailbox_store.append_authorized(&input, || {
        state
            .transition_federation_peer(&peer, FederationMembershipState::Revoked)
            .unwrap();
        authority.lock()
    });
    assert!(result.is_err());
    let connection = state.mailbox_store.connection().unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM mailbox_messages", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM mailboxes", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn authority_epoch_change_rejects_the_original_bound_request() {
    let state = federation_mailboxes::central_store();
    let (local, _) = federation_mailboxes::local_participant(&state, "Local");
    let request = federation_mailboxes::request(&state, &local, FederationMailboxOperation::List);
    let host = state
        .inner
        .lock()
        .unwrap()
        .federation
        .local
        .clone()
        .unwrap();
    state.reset_federation_continuity(&host).unwrap();
    assert_eq!(
        state
            .execute_federation_mailbox(request, None)
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        state.federation_mailbox_actor(&local).unwrap_err().status,
        StatusCode::CONFLICT
    );
}
