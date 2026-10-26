// Owns literal-versus-qualified domain alias collisions and compatibility.
// Remote actors below are explicit, current fixture bindings. These controls
// do not enroll live peers or claim a native transport/onboarding observation.
use super::*;

fn collision() -> (
    AppState,
    FederationMailboxParticipant,
    FederationMailboxParticipant,
    String,
) {
    let state = federation_mailboxes::central_store();
    let (_, local) = federation_mailboxes::local_participant(&state, "Inbox");
    let spelling = format!("{}::{}", local.host_name, local.name);
    let (remote, _lease) = federation_mailbox_intent_remote::remote_actor(&state, &spelling, 48001);
    let remote = state
        .inner
        .lock()
        .unwrap()
        .federation
        .mailbox_directory
        .get(&remote.binding.participant_key())
        .unwrap()
        .clone();
    (state, local, remote, spelling)
}

fn bridge_rows(rows: &[FederationMailboxParticipant]) -> Vec<Value> {
    rows.iter()
        .map(|row| {
            json!({
                "sessionId":row.session_id,
                "name":format!("{}::{}",row.host_name,row.name),
                "localName":row.name,
                "hostName":row.host_name,
                "hostId":row.binding.identity.host_id,
                "localSessionId":row.binding.identity.session_id,
            })
        })
        .collect()
}

#[test]
fn federation_mailbox_alias_grammar_central_collision_stays_refused_after_literal_peer_disconnect()
{
    let (state, local, remote, spelling) = collision();
    let mut inner = state.inner.lock().unwrap();
    assert_eq!(
        resolve_federation_mailbox_participant_locked(&inner, &spelling)
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
    inner
        .federation
        .links
        .remove(&remote.binding.identity.host_id)
        .unwrap();
    assert!(!federation_mailbox_participant_current_locked(
        &inner, &remote
    ));
    assert!(federation_mailbox_participant_current_locked(
        &inner, &local
    ));
    assert!(
        resolve_federation_mailbox_participant_locked(&inner, &spelling).is_err(),
        "a disappearing literal namesake cannot turn an ambiguous spelling into another recipient"
    );
    assert_eq!(
        resolve_federation_mailbox_participant_locked(&inner, &local.session_id)
            .unwrap()
            .binding,
        local.binding
    );
}

#[test]
fn federation_mailbox_alias_grammar_satellite_and_mcp_do_not_reinterpret_a_partial_directory() {
    let (_state, local, remote, spelling) = collision();
    let full = [local.clone(), remote];
    assert!(resolve_federation_delivery_target(&full, &spelling).is_err());
    assert!(resolve_domain_mailbox_reference(&bridge_rows(&full), &spelling, "caller").is_err());
    let remaining = [local.clone()];
    assert!(
        resolve_federation_delivery_target(&remaining, &spelling).is_err(),
        "a current-only directory cannot reinterpret the former literal alias"
    );
    assert!(
        resolve_domain_mailbox_reference(&bridge_rows(&remaining), &spelling, "caller").is_err()
    );
    assert_eq!(
        resolve_federation_delivery_target(&remaining, &local.session_id)
            .unwrap()
            .binding,
        local.binding
    );
}

#[test]
fn federation_mailbox_alias_grammar_inventory_names_keep_recipient_or_name_actionable_literal_form()
{
    // Current domain names observed through the host participants API. Their
    // fixture identities are fresh; no live principal is used by these tests.
    for name in ["Termal::O1-A", "Termal::WindowsB-Gate-Courier"] {
        let state = federation_mailboxes::central_store();
        let (_, row) = federation_mailboxes::local_participant(&state, name);
        let inner = state.inner.lock().unwrap();
        let basis = federation_mailbox_alias_basis_locked(&inner);
        assert!(basis.complete);
        let canonical = format!("name:{name}");
        match resolve_federation_mailbox_participant_locked(&inner, name) {
            Ok(found) => assert_eq!(found.binding, row.binding),
            Err(error) => assert!(error.message.contains(&canonical), "{error:?}"),
        }
        match resolve_federation_delivery_target(&[row.clone()], name) {
            Ok(found) => assert_eq!(found.binding, row.binding),
            Err(error) => assert!(error.message.contains(&canonical), "{error:?}"),
        }
        match resolve_domain_mailbox_reference(&bridge_rows(&[row.clone()]), name, "caller") {
            Ok(id) => assert_eq!(id, row.session_id),
            Err(error) => assert!(error.to_string().contains(&canonical), "{error:?}"),
        }
        assert_eq!(
            resolve_federation_delivery_target_with_basis(&[row.clone()], name, Some(&basis))
                .unwrap()
                .binding,
            row.binding
        );
        assert_eq!(
            resolve_domain_mailbox_reference_with_basis(
                &bridge_rows(&[row.clone()]),
                name,
                "caller",
                Some(&basis)
            )
            .unwrap(),
            row.session_id
        );
    }
}

fn all_resolve(
    state: &AppState,
    rows: &[FederationMailboxParticipant],
    reference: &str,
    expected: &str,
) {
    let inner = state.inner.lock().unwrap();
    let basis = federation_mailbox_alias_basis_locked(&inner);
    assert_eq!(
        resolve_federation_mailbox_participant_locked(&inner, reference)
            .unwrap()
            .session_id,
        expected
    );
    assert_eq!(
        resolve_federation_delivery_target_with_basis(rows, reference, Some(&basis))
            .unwrap()
            .session_id,
        expected
    );
    assert_eq!(
        resolve_domain_mailbox_reference_with_basis(
            &bridge_rows(rows),
            reference,
            "caller",
            Some(&basis)
        )
        .unwrap(),
        expected
    );
}

fn all_refuse(state: &AppState, rows: &[FederationMailboxParticipant], reference: &str) {
    let inner = state.inner.lock().unwrap();
    let basis = federation_mailbox_alias_basis_locked(&inner);
    let errors = [
        resolve_federation_mailbox_participant_locked(&inner, reference)
            .unwrap_err()
            .message,
        resolve_federation_delivery_target_with_basis(rows, reference, Some(&basis))
            .unwrap_err()
            .message,
        resolve_domain_mailbox_reference_with_basis(
            &bridge_rows(rows),
            reference,
            "caller",
            Some(&basis),
        )
        .unwrap_err()
        .to_string(),
    ];
    for error in errors {
        assert!(error.contains(&format!("name:{reference}")), "{error}");
        assert!(error.contains(&format!("host:{reference}")), "{error}");
    }
}

#[test]
fn federation_mailbox_alias_grammar_explicit_forms_fix_collision_without_changing_literal_recipient()
 {
    let (state, local, remote, spelling) = collision();
    let rows = [local.clone(), remote.clone()];
    all_refuse(&state, &rows, &spelling);
    all_resolve(
        &state,
        &rows,
        &format!("name:{spelling}"),
        &remote.session_id,
    );
    all_resolve(
        &state,
        &rows,
        &format!("host:{spelling}"),
        &local.session_id,
    );
    all_resolve(
        &state,
        &rows,
        &format!("host:{}::{spelling}", remote.binding.identity.host_id),
        &remote.session_id,
    );
    all_resolve(&state, &rows, &remote.session_id, &remote.session_id);
    all_resolve(&state, &rows, "iNbOx", &local.session_id);
    state
        .inner
        .lock()
        .unwrap()
        .federation
        .links
        .remove(&remote.binding.identity.host_id)
        .unwrap();
    all_refuse(&state, &[local.clone()], &spelling);
    all_resolve(
        &state,
        &[local.clone()],
        &format!("host:{spelling}"),
        &local.session_id,
    );
    assert!(
        resolve_federation_delivery_target(&[local.clone()], &format!("name:{spelling}")).is_err()
    );
    assert!(
        resolve_domain_mailbox_reference(
            &bridge_rows(&[local]),
            &format!("name:{spelling}"),
            "caller"
        )
        .is_err()
    );
}

#[test]
fn federation_mailbox_alias_grammar_legacy_requires_all_hosts_even_without_participant_rows() {
    let state = federation_mailboxes::central_store();
    let (_, literal) = federation_mailboxes::local_participant(&state, "Termal::O1-A");
    let rows = [literal.clone()];
    all_resolve(&state, &rows, &literal.name, &literal.session_id);
    // An unavailable enrolled peer has NO directory row. Counting visible
    // rows cannot establish that the domain lacks another literal namesake.
    state
        .enroll_federation_peer(federation_registry::peer(48002))
        .unwrap();
    assert!(!federation_mailbox_alias_basis_locked(&state.inner.lock().unwrap()).complete);
    all_refuse(&state, &rows, &literal.name);
    all_resolve(
        &state,
        &rows,
        &format!("name:{}", literal.name),
        &literal.session_id,
    );
    all_resolve(&state, &rows, &literal.session_id, &literal.session_id);
}

#[test]
fn federation_mailbox_alias_grammar_host_prefix_match_refuses_even_without_a_qualified_target() {
    let state = federation_mailboxes::central_store();
    let (_, literal) = federation_mailboxes::local_participant(&state, "Central::Nobody");
    let rows = [literal.clone()];
    all_refuse(&state, &rows, &literal.name);
    all_resolve(
        &state,
        &rows,
        &format!("name:{}", literal.name),
        &literal.session_id,
    );
    all_resolve(
        &state,
        &rows,
        &format!("host:Central::{}", literal.name),
        &literal.session_id,
    );
}

#[path = "federation_mailbox_alias_wire.rs"]
mod wire;

#[path = "federation_mailbox_alias_review.rs"]
mod review;
