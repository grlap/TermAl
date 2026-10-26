// Public local sends keep their native owner independently of domain routing.
// Disposable hosts only; provider execution and live delivery are not exercised.
use super::*;
use super::regressions::public_send;

pub(super) async fn acknowledge_native(state: &AppState, recipient: &str, sent: &MailboxAppendReceipt) {
    let page = read_mailbox(AxumPath((recipient.into(), sent.mailbox_id.clone())), State(state.clone()),
        Json(ReadMailboxRequest { after_sequence: None, limit: 20, issue_receipt: true, read_key: None }))
        .await.unwrap().0;
    assert!(page.messages.iter().any(|message| message.id == sent.message_id));
    let exact = read_mailbox_message(AxumPath((recipient.into(), sent.message_id.clone())),
        State(state.clone())).await.unwrap().0;
    assert_eq!(exact.id, sent.message_id);
    let ack = acknowledge_mailbox(AxumPath((recipient.into(), sent.mailbox_id.clone())), State(state.clone()),
        Json(AcknowledgeMailboxRequest { receipt: page.receipt, expected_processed_through: None,
            processed_through: None, no_continuation: false })).await.unwrap().0;
    assert!(ack.participants.iter().any(|row| row.session_id == recipient && row.processed_through == sent.sequence));
}

async fn unavailable_satellite_local_send(bound: bool) {
    let pair = qualified_pair().await;
    let sender = native(&pair.satellite, "Local sender");
    let target = native(&pair.satellite, "Local recipient");
    let remote = native(&pair.central, "Domain recipient");
    if bound { directory(&pair.satellite, &sender).await; }
    let remote_binding = if bound {
        Some(pair.central.federation_mailbox_actor(&remote).unwrap().unwrap().binding.participant_key())
    } else { None };
    pair.satellite.federation_peer_handles.lock().unwrap().clear();
    let before = pair.satellite.inner.lock().unwrap().sessions.iter()
        .map(|row| row.mailbox_binding().cloned()).collect::<Vec<_>>();
    let sent = public_send(&pair.satellite, &sender, &target, "local-offline", "local mail").await.unwrap();
    acknowledge_native(&pair.satellite, &target, &sent).await;
    let retry = public_send(&pair.satellite, &sender, &target, "local-offline", "local mail").await.unwrap();
    assert!(retry.duplicate);
    assert_eq!(retry.message_id, sent.message_id);
    let domain = public_send(&pair.satellite, &sender,
        remote_binding.as_deref().unwrap_or("Domain recipient"), "domain-offline", "must refuse").await;
    assert!(domain.is_err(), "domain targets still require automatic participation");
    let connection = pair.satellite.mailbox_store.connection().unwrap();
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get::<_, u64>(0)).unwrap(), 1);
    drop(connection);
    assert_eq!(before, pair.satellite.inner.lock().unwrap().sessions.iter()
        .map(|row| row.mailbox_binding().cloned()).collect::<Vec<_>>(), "local sends cannot admit or change domain bindings");
    pair.finish().await;
}

#[tokio::test]
async fn unbound_satellite_local_send_does_not_require_a_central_handle() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    unavailable_satellite_local_send(false).await;
}

#[tokio::test]
async fn automatically_bound_satellite_local_send_does_not_require_a_central_handle() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    unavailable_satellite_local_send(true).await;
}

#[tokio::test]
async fn local_native_send_does_not_need_host_verification() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let pair = federation_onboarding_fixture::Pair::with_enrolled_profile(
        federation_mailboxes::central_store(), federation_onboarding_fixture::io(),
        federation_onboarding_fixture::io(), federation_onboarding_fixture::profile(),
    ).await;
    let sender = native(&pair.satellite, "Local sender");
    let target = native(&pair.satellite, "Local recipient");
    let sent = public_send(&pair.satellite, &sender, &target, "unverified-local", "local").await.unwrap();
    acknowledge_native(&pair.satellite, &target, &sent).await;
    assert!(pair.satellite.federation_mailbox_actor(&sender).unwrap().is_none());
    assert!(public_send(&pair.satellite, &sender, "Remote", "unverified-domain", "refuse").await.is_err());
    assert!(federation_mailbox_root_locked(&pair.satellite.inner.lock().unwrap(), &sender)
        .unwrap().federation_mailbox_binding.is_none());
    pair.finish().await;
}

#[tokio::test]
async fn explicitly_bound_native_roots_keep_public_bare_sends_local() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = federation_mailboxes::central_store();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Explicit sender");
    let (target, _) = federation_mailboxes::local_participant(&state, "Explicit recipient");
    let sent = public_send(&state, &sender, &target, "explicit-local", "local").await.unwrap();
    acknowledge_native(&state, &target, &sent).await;
    assert_eq!(state.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM federation_mailbox_intents", [], |row| row.get::<_, u64>(0)).unwrap(), 0);
}

#[tokio::test]
async fn local_and_domain_retries_keep_their_original_namespace_and_receipt() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = federation_mailboxes::central_store();
    let sender = native(&state, "Sender");
    let target = native(&state, "Target");
    let domain = public_send(&state, &sender, "Target", "domain-key", "domain").await.unwrap();
    let retry = public_send(&state, &sender, &target, "domain-key", "domain").await.unwrap();
    assert!(retry.duplicate);
    assert_eq!(retry.message_id, domain.message_id);
    let local = public_send(&state, &sender, &target, "local-key", "local").await.unwrap();
    assert_ne!(local.mailbox_id, domain.mailbox_id);
    acknowledge_native(&state, &target, &local).await;
    assert!(public_send(&state, &sender, "Target", "local-key", "local").await.is_err());
    let retry = public_send(&state, &sender, &target, "local-key", "local").await.unwrap();
    assert_eq!(retry.message_id, local.message_id);
    let connection = state.mailbox_store.connection().unwrap();
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get::<_, u64>(0)).unwrap(), 2);
}

#[tokio::test]
async fn concurrent_local_and_domain_sends_cannot_claim_the_same_sender_key() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = federation_mailboxes::central_store();
    let sender = native(&state, "Sender");
    let target = native(&state, "Target");
    directory(&state, &sender).await;
    let (local, domain) = tokio::join!(
        public_send(&state, &sender, &target, "same-key", "local"),
        public_send(&state, &sender, "Target", "same-key", "domain"));
    assert_ne!(local.is_ok(), domain.is_ok(), "local={local:?}, domain={domain:?}");
    let connection = state.mailbox_store.connection().unwrap();
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get::<_, u64>(0)).unwrap(), 1);
}
