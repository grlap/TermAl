// Exercises issued-receipt ACKs at the local native home. Association deletion
// is an injected metadata-loss boundary, not a claim that normal boot deletes it.
use super::*;

async fn issued_page(fixture: &Fixture, proxy: &ProxySend, mailbox: &str) -> MailboxReadResponse {
    read_mailbox(
        AxumPath((proxy.caller.clone(), mailbox.to_owned())),
        State(fixture.central.clone()),
        Json(ReadMailboxRequest {
            after_sequence: Some(0),
            limit: 20,
            issue_receipt: true,
            read_key: None,
        }),
    )
    .await
    .unwrap()
    .0
}

async fn ack(
    state: &AppState,
    caller: &str,
    mailbox: &str,
    receipt: String,
) -> Result<MailboxSummary, ApiError> {
    acknowledge_mailbox(
        AxumPath((caller.to_owned(), mailbox.to_owned())),
        State(state.clone()),
        Json(AcknowledgeMailboxRequest {
            receipt: Some(receipt),
            expected_processed_through: None,
            processed_through: None,
            no_continuation: false,
        }),
    )
    .await
    .map(|summary| summary.0)
}

fn cursors(state: &AppState, mailbox: &str) -> Vec<(String, u64)> {
    let connection = state.mailbox_store.connection().unwrap();
    let mut query = connection
        .prepare("SELECT session_id,processed_through FROM mailbox_participants WHERE mailbox_id=?1 ORDER BY session_id")
        .unwrap();
    let rows = query
        .query_map([mailbox], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    rows
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_receipt_ack_after_association_loss_names_reresolve_without_advancing() {
    association_loss_ack(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_receipt_ack_after_association_loss_and_list_keeps_authority_fenced() {
    association_loss_ack(true).await;
}

async fn association_loss_ack(list_first: bool) {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let proxy = ProxySend::new(&fixture).await;
    let sent = proxy.send(&fixture, "local-receipt-association-loss").await;
    let locator = MailboxRelayLocator::decode(&sent.mailbox_id, false)
        .unwrap()
        .unwrap();
    let actor = fixture
        .central
        .native_mailbox_relay_descriptor(&proxy.caller)
        .unwrap()
        .principal;
    assert_eq!(locator.home.host, actor.host);
    let page = issued_page(&fixture, &proxy, &sent.mailbox_id).await;
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].id, sent.message_id);
    let receipt = page.receipt.expect("the real read issued a receipt");
    let before = cursors(&fixture.central, &locator.id);
    assert_eq!(
        fixture
            .central
            .mailbox_store
            .connection()
            .unwrap()
            .execute(
                "DELETE FROM mailbox_relay_associations WHERE mailbox_id=?1",
                [&locator.id],
            )
            .unwrap(),
        1
    );
    if list_first {
        let listed = list_mailboxes(
            AxumPath(proxy.caller.clone()),
            State(fixture.central.clone()),
        )
        .await;
        eprintln!(
            "local association-loss list: {}",
            match listed {
                Ok(rows) => format!("{} mailboxes", rows.0.len()),
                Err(error) => format!("{} {}", error.status, error.message),
            }
        );
    }
    let result = ack(&fixture.central, &proxy.caller, &sent.mailbox_id, receipt).await;
    let after = cursors(&fixture.central, &locator.id);
    let absent = fixture
        .central
        .mailbox_store
        .mailbox_relay_association(&actor, &locator.id)
        .unwrap()
        .is_none();
    proxy.finish().await;
    fixture.finish().await;

    let error = result.expect_err("missing local metadata must not fabricate authority");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("association"), "{}", error.message);
    assert!(error.message.contains("re-resolve"), "{}", error.message);
    assert!(error.message.contains("mailbox"), "{}", error.message);
    assert!(absent, "a refused ACK cannot invent an association");
    assert_eq!(
        after, before,
        "refusal changes neither participant's cursor"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_receipt_ack_rejects_wrong_member_receipt_and_home_without_repair() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let proxy = ProxySend::new(&fixture).await;
    let sent = proxy
        .send(&fixture, "local-receipt-authority-controls")
        .await;
    let locator = MailboxRelayLocator::decode(&sent.mailbox_id, false)
        .unwrap()
        .unwrap();
    let actor = fixture
        .central
        .native_mailbox_relay_descriptor(&proxy.caller)
        .unwrap()
        .principal;
    let conversation = fixture
        .central
        .mailbox_store
        .mailbox_relay_association(&actor, &locator.id)
        .unwrap();
    let page = issued_page(&fixture, &proxy, &sent.mailbox_id).await;
    let receipt = page.receipt.unwrap();
    let before = cursors(&fixture.central, &locator.id);
    let stranger = test_session_id(&fixture.central, Agent::Codex);
    let wrong_member = ack(
        &fixture.central,
        &stranger,
        &sent.mailbox_id,
        receipt.clone(),
    )
    .await;
    let invalid_receipt = ack(
        &fixture.central,
        &proxy.caller,
        &sent.mailbox_id,
        Uuid::new_v4().to_string(),
    )
    .await;
    let mut wrong_home = locator.home.clone();
    wrong_home.store_id = Uuid::new_v4().to_string();
    let wrong_locator = MailboxRelayLocator::encode(&wrong_home, &locator.id, false).unwrap();
    let mismatched_home = ack(
        &fixture.central,
        &proxy.caller,
        &wrong_locator,
        receipt.clone(),
    )
    .await;
    let after = cursors(&fixture.central, &locator.id);
    let retained = fixture
        .central
        .mailbox_store
        .mailbox_relay_association(&actor, &locator.id)
        .unwrap();
    let valid = ack(&fixture.central, &proxy.caller, &sent.mailbox_id, receipt).await;
    proxy.finish().await;
    fixture.finish().await;

    let error = wrong_member.expect_err("another local root does not own this receipt");
    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert!(
        !error.message.contains("re-resolve"),
        "wrong membership is not lost metadata"
    );
    assert!(invalid_receipt.is_err(), "unknown receipt must be rejected");
    assert!(
        mismatched_home.is_err(),
        "a locator cannot move the receipt to another home"
    );
    assert_eq!(after, before);
    assert_eq!(retained, conversation);
    assert!(
        valid.is_ok(),
        "refused attempts preserve the original valid receipt"
    );
}
