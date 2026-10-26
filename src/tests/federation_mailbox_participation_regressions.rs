// Review counterexamples for transition ownership and authority at consumers.
// Uses authenticated disposable hosts; never live providers or user stores.
use super::*;

#[path = "federation_mailbox_participation_lifecycle.rs"]
mod lifecycle;

pub(super) async fn public_send(state: &AppState, from: &str, target: &str, key: &str, body: &str)
    -> Result<MailboxAppendReceipt, ApiError>
{
    send_mailbox_message(AxumPath(from.to_owned()), State(state.clone()),
        Json(serde_json::from_value(json!({"targetSessionId":target,
            "idempotencyKey":key,"message":body})).unwrap())).await.map(|(_, Json(receipt))| receipt)
}

#[tokio::test]
async fn automatic_binding_cannot_admit_a_signed_executor_wake() {
    let pair = qualified_pair().await;
    let c = native(&pair.central, "C");
    let s = native(&pair.satellite, "S");
    let sent = public_send(&pair.central, &c, "S", "no-executor-grant", "ordinary mailbox message").await.unwrap();
    let target = pair.satellite.federation_mailbox_actor(&s).unwrap().unwrap().binding;
    {
        let inner = pair.satellite.inner.lock().unwrap();
        let root = &inner.sessions[inner.find_session_index(&s).unwrap()];
        assert!(root.federation_mailbox_binding.is_none());
        assert_eq!(root.federation_automatic_mailbox_binding.as_ref(), Some(&target));
        let saved = serde_json::to_value(PersistedSessionRecord::from_record(root)).unwrap();
        assert!(saved.get("federationMailboxBinding").is_none(), "older receiver must see no grant-shaped binding");
        assert_eq!(saved["federationAutomaticMailboxBinding"], serde_json::to_value(&target).unwrap());
    }
    let sender = pair.central.federation_mailbox_actor(&c).unwrap().unwrap().binding.participant_key();
    let result = pair.rpc.request::<FederationExecutorReceipt>(FederationRpcRoute::ExecutorWake(
        pair.lease.clone(), FederationExecutorWakeRequest {
            protocol_version: FEDERATION_PROTOCOL_VERSION,
            operation_key: format!("mailbox-wake:{}", sent.message_id), target,
            wake: FederationExecutorWake { mailbox_id: sent.mailbox_id, message_id: sent.message_id,
                sequence: sent.sequence, unread_count: 1, sender_session_id: sender,
                sender_name: "C".into(), topic: None },
        },
    )).await;
    assert!(result.is_err(), "mailbox qualification must not authorize the executor receiver");
    let connection = pair.satellite.mailbox_store.connection().unwrap();
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM federation_executor_operations", [],
        |row| row.get::<_, u64>(0)).unwrap(), 0);
    drop(connection);
    pair.finish().await;
}

#[tokio::test]
async fn committed_standalone_retry_keeps_its_receipt_after_automatic_join() {
    let state = federation_mailboxes::central_store();
    let identity = state.inner.lock().unwrap().federation.local.clone().unwrap();
    state.configure_federation(FederationSettings { enabled: false,
        role: FederationRole::Standalone, central: None }).unwrap();
    let a = native(&state, "A");
    let b = native(&state, "B");
    let original = public_send(&state, &a, &b, "lost-reply", "committed original bytes").await.unwrap();
    state.configure_federation(FederationSettings { enabled: true,
        role: FederationRole::Central, central: Some(identity) }).unwrap();
    directory(&state, &a).await;
    let retry = public_send(&state, &a, &b, "lost-reply", "committed original bytes").await.unwrap();
    assert_eq!(retry.mailbox_id, original.mailbox_id);
    assert_eq!(retry.message_id, original.message_id);
    assert_eq!(retry.sequence, original.sequence);
    let conflict = public_send(&state, &a, &b, "lost-reply", "changed bytes").await.unwrap_err();
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    assert_eq!(state.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get::<_, u64>(0)).unwrap(), 1);
}

#[tokio::test]
async fn automatic_local_history_listing_reactivates_before_its_snapshot() {
    let state = federation_mailboxes::central_store();
    let a = native(&state, "A");
    let b = native(&state, "B");
    let original = state.append_mailbox_message_and_notify(&a, SendMailboxMessageRequest {
        target_session_id: b.clone(), message: "local history".into(), idempotency_key: "left-row".into(),
        topic: None, state_stamp: None, class: None,
    }).unwrap();
    state.mailbox_store.mark_session_left(&b).unwrap();
    directory(&state, &b).await;
    let listed = list_mailboxes_with_availability(&b, &state).await.unwrap();
    assert!(listed.mailboxes.iter().any(|mailbox| mailbox.id == original.mailbox_id));
}

#[tokio::test]
async fn known_bare_local_target_does_not_need_an_unavailable_peer() {
    let pair = qualified_pair().await;
    let a = native(&pair.central, "A");
    let b = native(&pair.central, "B");
    directory(&pair.central, &a).await;
    // Verification/link stay current; only the unrelated reverse RPC handle
    // disappears. No call to that peer is needed to address these local roots.
    pair.central.federation_peer_handles.lock().unwrap().clear();
    let sent = public_send(&pair.central, &a, &b, "local-independent", "local bytes").await;
    assert!(sent.is_ok(), "known local target depended on unrelated discovery: {sent:?}");
    let created: Result<MailboxSummary, ApiError> = pair.central.federation_mailbox_operation(
        &a, FederationMailboxOperation::GetOrCreate { target: b.clone() }).await;
    assert_eq!(created.unwrap_err().status, StatusCode::NOT_FOUND,
        "domain-only create cannot reinterpret a bare native target");
    let target = pair.central.federation_mailbox_actor(&b).unwrap().unwrap().binding.participant_key();
    let created: Result<MailboxSummary, ApiError> = pair.central.federation_mailbox_operation(
        &a, FederationMailboxOperation::GetOrCreate { target }).await;
    assert!(created.is_ok(), "known local GetOrCreate depended on unrelated discovery");
    pair.finish().await;
}

#[tokio::test]
async fn one_fenced_root_does_not_hide_other_qualified_roots() {
    let pair = qualified_pair().await;
    let c = native(&pair.central, "C");
    let good = native(&pair.satellite, "Good");
    let bad = native(&pair.satellite, "Fenced");
    directory(&pair.central, &c).await;
    {
        let mut inner = pair.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&bad).unwrap();
        inner.session_mut_by_index(index).unwrap().federation_automatic_mailbox_binding.as_mut().unwrap()
            .central.host_generation += 1;
    }
    let rows = directory(&pair.central, &c).await;
    assert!(rows.iter().any(|row| row.binding.identity.session_id == good));
    assert!(!rows.iter().any(|row| row.binding.identity.session_id == bad));
    pair.finish().await;
}

#[test]
fn catalog_steps_bound_inventory_work_and_preserve_every_eligible_root() {
    let state = federation_mailboxes::central_store();
    let ids: HashSet<_> = (0..130).map(|index| native(&state, &format!("Native {index}"))).collect();
    let inner = state.inner.lock().unwrap();
    let central = inner.federation.local.clone().unwrap();
    let mut after: Option<String> = None;
    let mut seen = HashSet::new();
    let mut prior = 0;
    loop {
        let page = federation_native_catalog_locked(&inner, after.as_deref(), central.clone(), central.clone(), String::new()).unwrap();
        assert!(page.roots.len() <= FEDERATION_CATALOG_PAGE);
        for root in page.roots { assert!(seen.insert(root.session_id)); }
        let Some(next) = page.next else { break; };
        let position: FederationCatalogPosition = serde_json::from_str(&next).unwrap();
        assert!(position.session > prior && position.session - prior <= FEDERATION_CATALOG_SCAN);
        prior = position.session;
        after = Some(next);
    }
    assert_eq!(seen, ids);
}
