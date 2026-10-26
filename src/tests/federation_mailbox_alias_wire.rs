// Owns old-wire compatibility, namespace fencing and actual append target controls.
// Uses the domain fixtures of the parent alias grammar module, not live stores.
use super::*;

#[test]
fn federation_mailbox_alias_grammar_missing_basis_and_old_wire_shape_fail_closed() {
    let state = federation_mailboxes::central_store();
    let (_, row) = federation_mailboxes::local_participant(&state, "Termal::O1-A");
    let response: FederationMailboxResponse = serde_json::from_value(json!({
        "protocolVersion":FEDERATION_MAILBOX_WIRE_VERSION, "schemaVersion":FEDERATION_MAILBOX_SCHEMA_VERSION,
        "central":row.binding.central, "value":[row],
    })).unwrap();
    assert!(response.alias_basis.is_none());
    let rows: Vec<FederationMailboxParticipant> = serde_json::from_value(response.value).unwrap();
    assert!(
        resolve_federation_delivery_target_with_basis(
            &rows,
            &row.name,
            response.alias_basis.as_ref()
        )
        .is_err()
    );
    assert!(
        resolve_domain_mailbox_reference_with_basis(&bridge_rows(&rows), &row.name, "caller", None)
            .is_err()
    );
    assert_eq!(
        resolve_federation_delivery_target(&rows, &row.session_id)
            .unwrap()
            .binding,
        row.binding
    );
    assert_eq!(FEDERATION_PROFILE_SCHEMA_VERSION, "2");
}

#[test]
fn federation_mailbox_alias_grammar_old_strict_wire_without_opt_in_is_byte_shape_identical() {
    #[derive(Deserialize, Serialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct OldResponse {
        protocol_version: u16,
        schema_version: String,
        central: FederationHostIdentity,
        value: Value,
    }
    #[derive(Deserialize)]
    #[serde(tag = "operation", rename_all = "camelCase", deny_unknown_fields)]
    enum OldDirectory {
        Directory,
    }
    let state = federation_mailboxes::central_store();
    let (caller, _) = federation_mailboxes::local_participant(&state, "Caller");
    let make_request = |alias_basis| {
        federation_mailboxes::request(
            &state,
            &caller,
            FederationMailboxOperation::Directory { alias_basis },
        )
    };
    for flag in [false, true] {
        let bytes = serde_json::to_vec(&make_request(flag).operation).unwrap();
        assert_eq!(bytes, br#"{"operation":"directory"}"#);
        assert!(
            serde_json::from_slice::<OldDirectory>(&bytes).is_ok(),
            "old central accepts identical body"
        );
    }
    let response = state
        .execute_federation_mailbox(make_request(false), None)
        .unwrap();
    assert!(response.alias_basis.is_none());
    let old = OldResponse {
        protocol_version: response.protocol_version,
        schema_version: response.schema_version.clone(),
        central: response.central.clone(),
        value: response.value.clone(),
    };
    let bytes = serde_json::to_vec(&response).unwrap();
    assert_eq!(
        bytes,
        serde_json::to_vec(&old).unwrap(),
        "old satellites receive exactly the old JSON fields/order"
    );
    assert!(serde_json::from_slice::<OldResponse>(&bytes).is_ok());
    let opted_in = state
        .execute_federation_mailbox(make_request(true), None)
        .unwrap();
    assert!(opted_in.alias_basis.unwrap().complete);
}

#[test]
fn federation_mailbox_alias_grammar_prefixes_do_not_escape_reserved_namespaces() {
    let state = federation_mailboxes::central_store();
    for name in [
        "session-999",
        "domain-session-stale",
        "relay-principal-stale",
        "MAILBOXREFERENCE-stale",
    ] {
        let (_, row) = federation_mailboxes::local_participant(&state, name);
        for reference in [format!("name:{name}"), format!("host:Central::{name}")] {
            assert!(
                resolve_federation_mailbox_participant_locked(
                    &state.inner.lock().unwrap(),
                    &reference
                )
                .is_err()
            );
            assert!(resolve_federation_delivery_target(&[row.clone()], &reference).is_err());
            assert!(
                resolve_domain_mailbox_reference(
                    &bridge_rows(&[row.clone()]),
                    &reference,
                    "caller"
                )
                .is_err()
            );
        }
        all_resolve(&state, &[row.clone()], &row.session_id, &row.session_id);
    }
}

#[tokio::test]
async fn federation_mailbox_alias_grammar_actual_append_keeps_the_explicit_target_across_disconnect()
 {
    let (state, local, remote, spelling) = collision();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Sender");
    for phase in [0, 1] {
        if phase == 1 {
            state
                .inner
                .lock()
                .unwrap()
                .federation
                .links
                .remove(&remote.binding.identity.host_id)
                .unwrap();
        }
        assert!(
            state
                .federation_mailbox_operation::<MailboxAppendReceipt>(
                    &sender,
                    FederationMailboxOperation::Send {
                        target: spelling.clone(),
                        message: "do not guess".into(),
                        key: format!("refused-{phase}"),
                        topic: None,
                        state_stamp: None,
                    }
                )
                .await
                .is_err()
        );
        let receipt: MailboxAppendReceipt = state
            .federation_mailbox_operation(
                &sender,
                FederationMailboxOperation::Send {
                    target: format!("host:{spelling}"),
                    message: "exact selected target".into(),
                    key: format!("sent-{phase}"),
                    topic: None,
                    state_stamp: None,
                },
            )
            .await
            .unwrap();
        let target: String = state
            .mailbox_store
            .connection()
            .unwrap()
            .query_row(
                "SELECT target_session_id FROM mailbox_messages WHERE id=?1",
                [&receipt.message_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(target, local.session_id);
        assert_ne!(target, remote.session_id);
    }
    assert_eq!(
        state
            .mailbox_store
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        2
    );
}
