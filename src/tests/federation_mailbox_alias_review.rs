// Owns advertised-name and full host-selector regression controls.
// Stored-label mismatches are injected discovery images, not an executed
// re-enrollment history or a live-host routing observation.
use super::*;

fn labelled_remote(
    state: &AppState,
    registry_label: &str,
    stored_label: &str,
    port: u16,
) -> FederationMailboxParticipant {
    let (actor, _lease) = federation_mailbox_intent_remote::remote_actor(state, "Worker", port);
    let mut inner = state.inner.lock().unwrap();
    inner.federation.peers.get_mut(&actor.binding.identity.host_id)
        .unwrap().display_name = registry_label.into();
    let row = inner.federation.mailbox_directory.get_mut(&actor.binding.participant_key()).unwrap();
    row.host_name = stored_label.into();
    let row = row.clone();
    assert!(federation_mailbox_participant_current_locked(&inner, &row));
    assert!(federation_mailbox_alias_basis_locked(&inner).complete);
    row
}

#[test]
fn federation_mailbox_alias_grammar_stored_host_label_cannot_become_an_unrelated_literal() {
    let state = federation_mailboxes::central_store();
    let (_, literal) = federation_mailboxes::local_participant(&state, "Alpha::Worker");
    let remote = labelled_remote(&state, "Beta", "Alpha", 48006);
    let rows = [literal.clone(), remote.clone()];
    all_refuse(&state, &rows, &literal.name);
    all_resolve(&state, &rows, "name:Alpha::Worker", &literal.session_id);
    all_resolve(&state, &rows, "host:Alpha::Worker", &remote.session_id);
    // An older server may certify completeness without including row labels.
    // The common selector must also fence every label its input rows accept.
    let old_basis = FederationMailboxAliasBasis {
        complete: true,
        hosts: vec![FederationMailboxAliasHost {
            id: remote.binding.identity.host_id.clone(), name: "Beta".into(),
        }],
    };
    assert!(resolve_federation_delivery_target_with_basis(&rows, &literal.name, Some(&old_basis)).is_err());
    assert!(resolve_domain_mailbox_reference_with_basis(
        &bridge_rows(&rows), &literal.name, "caller", Some(&old_basis),
    ).is_err());
    {
        let mut inner = state.inner.lock().unwrap();
        inner.federation.links.remove(&remote.binding.identity.host_id);
    }
    all_refuse(&state, &[literal.clone()], &literal.name);
    {
        let mut inner = state.inner.lock().unwrap();
        inner.federation.peers.get_mut(&remote.binding.identity.host_id)
            .unwrap().membership = FederationMembershipState::Revoked;
        assert!(!federation_mailbox_participant_current_locked(&inner, &remote));
        assert!(federation_mailbox_alias_basis_locked(&inner).complete);
    }
    all_refuse(&state, &[literal.clone()], &literal.name);
    all_resolve(&state, &[literal.clone()], "name:Alpha::Worker", &literal.session_id);
}

#[test]
fn federation_mailbox_alias_grammar_whole_host_label_with_separator_fences_literal() {
    let state = federation_mailboxes::central_store();
    let (_, literal) = federation_mailboxes::local_participant(&state, "Lab::Mac::Worker");
    let (_, near) = federation_mailboxes::local_participant(&state, "Lab::Macintosh::Worker");
    let remote = labelled_remote(&state, "Lab::Mac", "Lab::Mac", 48007);
    let rows = [literal.clone(), remote.clone()];
    all_refuse(&state, &rows, &literal.name);
    all_resolve(&state, &rows, "name:Lab::Mac::Worker", &literal.session_id);
    all_resolve(&state, &rows,
        &format!("host:{}::Worker", remote.binding.identity.host_id), &remote.session_id);
    // Similar text without the complete separator is not a host prefix.
    all_resolve(&state, &[literal, remote, near.clone()], &near.name, &near.session_id);
}

#[test]
fn federation_mailbox_alias_grammar_refusal_keeps_unicode_and_uses_uuid_for_explicit_ambiguity() {
    let state = federation_mailboxes::central_store();
    let (_, local) = federation_mailboxes::local_participant(&state, "Worker");
    let (_, unicode) = federation_mailboxes::local_participant(&state, "Zespół::A");
    let remote = labelled_remote(&state, "Central", "Central", 48008);
    let rows = [local.clone(), remote.clone()];
    for reference in ["host:Central::Worker", "name:Worker"] {
        let errors = [
            resolve_federation_mailbox_participant_locked(&state.inner.lock().unwrap(), reference)
                .unwrap_err().message,
            resolve_federation_delivery_target_with_basis(&rows, reference, None).unwrap_err().message,
            resolve_domain_mailbox_reference(&bridge_rows(&rows), reference, "caller").unwrap_err().to_string(),
        ];
        for error in errors {
            assert!(error.contains("host:<host UUID>::Worker"), "{error}");
            assert!(!error.contains("host:host:"), "{error}");
            assert!(!error.contains("name:host:"), "{error}");
        }
    }
    all_resolve(&state, &rows, &format!("host:{}::Worker", remote.binding.identity.host_id), &remote.session_id);
    let error = resolve_federation_delivery_target(&[unicode.clone()], &unicode.name).unwrap_err();
    assert!(error.message.contains("name:Zespół::A"), "{}", error.message);
    all_resolve(&state, &[local, remote, unicode.clone()], "name:Zespół::A", &unicode.session_id);
    let error = resolve_federation_delivery_target(&[unicode], "name:").unwrap_err();
    assert!(error.message.contains("name is empty"));
    assert!(!error.message.contains("reserved"));
    assert!(!federation_mailbox_alias_error("X\nY::Z").contains('\n'));
}

#[test]
fn federation_mailbox_alias_grammar_exact_native_pair_is_typed_and_keeps_hosts_distinct() {
    let state = federation_mailboxes::central_store();
    let (_, local) = federation_mailboxes::local_participant(&state, "Local worker");
    let (_, decoy) = federation_mailboxes::local_participant(&state, "session-999999");
    let remote = labelled_remote(&state, "Mac", "Mac", 48009);
    assert_eq!(local.binding.identity.session_id, remote.binding.identity.session_id,
        "the native ids intentionally collide across the two hosts");
    let rows = [local.clone(), remote.clone()];
    for target in &rows {
        let reference = format!("host:{}::{}",
            target.binding.identity.host_id, target.binding.identity.session_id);
        match FederationMailboxAlias::parse(&reference, None).unwrap() {
            FederationMailboxAlias::HostNative { host, session_id } => {
                assert_eq!(host.to_string(), target.binding.identity.host_id);
                assert_eq!(session_id, target.binding.identity.session_id);
            }
            _ => panic!("native locator must be a typed pair, not a display alias"),
        }
        all_resolve(&state, &rows, &reference, &target.session_id);
    }
    let bare = &remote.binding.identity.session_id;
    assert!(resolve_federation_mailbox_participant_locked(&state.inner.lock().unwrap(), bare).is_err());
    assert!(resolve_federation_delivery_target(&[remote.clone()], bare).is_err());
    assert!(resolve_domain_mailbox_reference(&bridge_rows(&[remote.clone()]), bare, "caller").is_err());
    // This is only existing-directory selection, not remote native discovery.
    let before = state.inner.lock().unwrap().federation.mailbox_directory.len();
    let unknown = format!("host:{}::session-999999", decoy.binding.identity.host_id);
    let rows = [local, remote, decoy.clone()];
    assert!(resolve_federation_mailbox_participant_locked(&state.inner.lock().unwrap(), &unknown).is_err());
    assert!(resolve_federation_delivery_target(&rows, &unknown).is_err());
    assert!(resolve_domain_mailbox_reference(&bridge_rows(&rows), &unknown, "caller").is_err());
    assert_eq!(state.inner.lock().unwrap().federation.mailbox_directory.len(), before);
    all_resolve(&state, &rows, &decoy.session_id, &decoy.session_id);
}

#[test]
fn federation_mailbox_alias_grammar_exact_native_invalid_or_retired_target_never_falls_back() {
    let state = federation_mailboxes::central_store();
    let (_, local) = federation_mailboxes::local_participant(&state, "Worker");
    let remote = labelled_remote(&state, "Mac", "Mac", 48010);
    let host = &remote.binding.identity.host_id;
    for reference in [
        "host:Mac::session-1".to_owned(),
        format!("host:{}::session-1", Uuid::nil()),
        format!("host:{host}::session-"),
        format!("host:{host}::session-1/other"),
        format!("host:{host}::session-1 other"),
        format!("host:{host}::session-1?other"),
        format!("host:{host}::domain-session-other"),
    ] {
        assert!(FederationMailboxAlias::parse(&reference, None).is_err(), "{reference}");
    }
    let reference = format!("host:{host}::{}", remote.binding.identity.session_id);
    all_resolve(&state, &[local.clone(), remote.clone()], &reference, &remote.session_id);
    {
        let mut inner = state.inner.lock().unwrap();
        inner.federation.placements.values_mut()
            .find(|placement| placement.identity == remote.binding.identity).unwrap()
            .state = FederationPlacementState::Completed;
        assert!(!federation_mailbox_participant_current_locked(&inner, &remote));
        assert!(resolve_federation_mailbox_participant_locked(&inner, &reference).is_err());
    }
    assert!(resolve_federation_delivery_target(&[local.clone()], &reference).is_err());
    assert!(resolve_domain_mailbox_reference(&bridge_rows(&[local]), &reference, "caller").is_err());
}
