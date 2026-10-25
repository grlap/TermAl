// Native relay identity, home and membership controls; no transport or delivery claims.
use super::*;

fn principal(host: u128, creation: u128) -> MailboxRelayPrincipal {
    MailboxRelayPrincipal {
        store_id: Uuid::from_u128(100 + host).to_string(),
        host: FederationHostIdentity {
            host_id: Uuid::from_u128(host).to_string(),
            host_generation: 1,
        },
        native_session_id: "session-1".to_owned(),
        native_creation_id: Uuid::from_u128(creation).to_string(),
    }
}

fn home(actor: &MailboxRelayPrincipal) -> MailboxRelayHome {
    MailboxRelayHome {
        host: actor.host.clone(),
        store_id: actor.store_id.clone(),
    }
}

#[test]
fn mailbox_relay_opposite_first_sends_select_the_same_central_home_and_pair() {
    let (a, b) = (principal(1, 11), principal(2, 12));
    let first = MailboxRelayConversation::new(home(&a), a.clone(), b.clone()).unwrap();
    let opposite = MailboxRelayConversation::new(home(&a), b.clone(), a.clone()).unwrap();
    assert_eq!(first, opposite);
    assert_eq!(first.pair_key(), opposite.pair_key());
    first.require_home(&home(&a)).unwrap();
    assert!(opposite.require_home(&home(&b)).is_err());
    first.require_member(&a).unwrap();
    first.require_member(&b).unwrap();
}

#[test]
fn mailbox_relay_creation_host_and_store_replacement_never_reuse_the_old_conversation() {
    let (a, b) = (principal(1, 11), principal(2, 12));
    let first = MailboxRelayConversation::new(home(&a), a.clone(), b.clone()).unwrap();
    let replacement = principal(2, 13);
    assert!(first.require_member(&replacement).is_err());
    assert_ne!(replacement.participant_key(), b.participant_key());
    let mut host_replacement = b.clone();
    host_replacement.host.host_generation += 1;
    assert!(first.require_member(&host_replacement).is_err());
    let mut store_replacement = home(&a);
    store_replacement.store_id = Uuid::from_u128(102).to_string();
    assert!(first.require_home(&store_replacement).is_err());
}

#[test]
fn mailbox_relay_request_claims_do_not_override_the_authenticated_origin() {
    let a = principal(1, 11);
    a.require_origin(&a.host).unwrap();
    assert!(a.require_origin(&principal(2, 12).host).is_err());
    let mut stale_host = a.host.clone();
    stale_host.host_generation += 1;
    assert!(a.require_origin(&stale_host).is_err());
}

#[test]
fn mailbox_relay_same_number_on_two_hosts_is_not_self_and_a_foreign_actor_has_no_membership() {
    let (a, b) = (principal(1, 11), principal(2, 12));
    assert_eq!(a.native_session_id, b.native_session_id);
    assert_ne!(a.participant_key(), b.participant_key());
    let conversation = MailboxRelayConversation::new(home(&a), a.clone(), b.clone()).unwrap();
    assert!(conversation.require_member(&principal(3, 13)).is_err());
    assert!(MailboxRelayConversation::new(home(&a), a.clone(), a).is_err());
}

#[test]
fn mailbox_relay_wire_rejects_extensions_versions_and_noncanonical_creations() {
    let (a, b) = (principal(1, 11), principal(2, 12));
    let conversation = MailboxRelayConversation::new(home(&a), a.clone(), b).unwrap();
    let mut wire = serde_json::to_value(&conversation).unwrap();
    wire["executor"] = json!(true);
    assert!(serde_json::from_value::<MailboxRelayConversation>(wire).is_err());
    let mut unsupported = conversation.clone();
    unsupported.version += 1;
    assert!(unsupported.validate().is_err());
    let mut nil_creation = a;
    nil_creation.native_creation_id = Uuid::nil().to_string();
    assert!(nil_creation.validate().is_err());
    assert_eq!(
        serde_json::from_str::<MailboxRelayConversation>(
            &serde_json::to_string(&conversation).unwrap()
        )
        .unwrap(),
        conversation
    );
}

#[test]
fn mailbox_relay_locators_are_canonical_typed_locations_not_membership() {
    let a = principal(1, 11);
    let mailbox = format!("mailbox-{}", Uuid::new_v4());
    let message = format!("mailbox-message-{}", Uuid::new_v4());
    let encoded = MailboxRelayLocator::encode(&home(&a), &mailbox, false).unwrap();
    assert_eq!(
        MailboxRelayLocator::decode(&encoded, false)
            .unwrap()
            .unwrap()
            .id,
        mailbox
    );
    assert!(MailboxRelayLocator::decode("relay-mailbox-not-json", false).is_err());
    assert!(MailboxRelayLocator::encode(&home(&a), &message, false).is_err());
    assert!(MailboxRelayLocator::encode(&home(&a), &mailbox, true).is_err());
    assert!(
        MailboxRelayLocator::decode(&mailbox, false)
            .unwrap()
            .is_none()
    );
    assert!(MailboxRelayLocator::decode(&(encoded + "="), false).is_err());
}
