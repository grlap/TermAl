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
async fn altered_relay_locator_returns_the_same_refusal_without_spending_a_receipt() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let proxy = ProxySend::new(&fixture).await;
    let sent = proxy.send(&fixture, "altered-locator").await;
    let central = &fixture.central;
    let mailbox = MailboxRelayLocator::decode(&sent.mailbox_id, false)
        .unwrap()
        .unwrap();
    let satellite = &fixture.satellite;
    let satellite_actor = satellite
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap()
        .principal;
    assert!(
        satellite
            .mailbox_store
            .mailbox_relay_association(&satellite_actor, &mailbox.id)
            .unwrap()
            .is_none()
    );
    let cold_error = read_mailbox_message(
        AxumPath((fixture.native.clone(), sent.message_id.clone())),
        State(satellite.clone()),
    )
    .await
    .unwrap_err();
    assert_eq!(cold_error.status, StatusCode::NOT_FOUND);
    assert_eq!(cold_error.kind, None);

    let reply = fixture
        .satellite
        .send_native_mailbox_relay(
            &fixture.native,
            &SendMailboxMessageRequest {
                target_session_id: format!("host:{}::{}", mailbox.home.host.host_id, proxy.caller),
                message: "unread reply for the original receipt".to_owned(),
                idempotency_key: "altered-locator-reply".to_owned(),
                topic: None,
                state_stamp: None,
                class: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reply.mailbox_id, sent.mailbox_id);
    let page = issued_page(&fixture, &proxy, &sent.mailbox_id).await;
    assert!(page.processed_through < reply.sequence);
    let receipt = page.receipt.unwrap();
    let remote_receipt = read_relay_page(satellite, &fixture.native, &sent.mailbox_id)
        .await
        .unwrap()
        .receipt
        .unwrap();
    let before = cursors(central, &mailbox.id);
    let altered_locator = |encoded: &str, message: bool| {
        let locator = MailboxRelayLocator::decode(encoded, message)
            .unwrap()
            .unwrap();
        let mut id = locator.id;
        let tail = id.pop().unwrap();
        id.push(if tail == '0' { '1' } else { '0' });
        MailboxRelayLocator::encode(&locator.home, &id, message).unwrap()
    };
    let wrong_mailbox = altered_locator(&sent.mailbox_id, false);
    let wrong_message = altered_locator(&reply.message_id, true);
    let set_home_store = |id: &str| {
        assert_eq!(
            central
                .mailbox_store
                .connection()
                .unwrap()
                .execute(
                    "UPDATE mailbox_relay_store SET store_id=?1 WHERE singleton=1",
                    [id],
                )
                .unwrap(),
            1
        );
    };
    set_home_store(&Uuid::new_v4().to_string());
    let replaced_store_error = read_mailbox_message(
        AxumPath((fixture.native.clone(), wrong_message.clone())),
        State(satellite.clone()),
    )
    .await
    .unwrap_err();
    assert_eq!(replaced_store_error.status, StatusCode::CONFLICT);
    assert_eq!(replaced_store_error.kind, None);
    set_home_store(&mailbox.home.store_id);
    assert_eq!(cursors(central, &mailbox.id), before);

    let read_error = read_relay_page(central, &proxy.caller, &wrong_mailbox)
        .await
        .unwrap_err();
    let ack_error = ack(central, &proxy.caller, &wrong_mailbox, receipt.clone())
        .await
        .unwrap_err();
    let message_error = read_mailbox_message(
        AxumPath((proxy.caller.clone(), wrong_message.clone())),
        State(central.clone()),
    )
    .await
    .unwrap_err();
    let remote_message_error = read_mailbox_message(
        AxumPath((fixture.native.clone(), wrong_message)),
        State(satellite.clone()),
    )
    .await
    .unwrap_err();
    let remote_read_error = read_relay_page(satellite, &fixture.native, &wrong_mailbox)
        .await
        .unwrap_err();
    let remote_ack_error = ack(
        satellite,
        &fixture.native,
        &wrong_mailbox,
        remote_receipt.clone(),
    )
    .await
    .unwrap_err();
    for error in [
        &read_error,
        &ack_error,
        &message_error,
        &remote_message_error,
        &remote_read_error,
        &remote_ack_error,
    ] {
        assert_unknown_relay_locator(error);
    }
    assert_eq!(cursors(central, &mailbox.id), before);

    // A retained satellite association can route a stale ID to the actual
    // home. Move only the cache key so the real mailbox and receipts survive.
    let stale_mailbox = MailboxRelayLocator::decode(&wrong_mailbox, false)
        .unwrap()
        .unwrap();
    let move_cache_key = |from: &str, to: &str| {
        assert_eq!(
            satellite
                .mailbox_store
                .connection()
                .unwrap()
                .execute(
                    "UPDATE mailbox_relay_associations SET mailbox_id=?2 WHERE mailbox_id=?1",
                    [from, to],
                )
                .unwrap(),
            1
        );
    };
    move_cache_key(&mailbox.id, &stale_mailbox.id);
    let home_read_error = read_relay_page(satellite, &fixture.native, &wrong_mailbox)
        .await
        .unwrap_err();
    let home_ack_error = ack(
        satellite,
        &fixture.native,
        &wrong_mailbox,
        remote_receipt.clone(),
    )
    .await
    .unwrap_err();
    assert_unknown_relay_locator(&home_read_error);
    assert_unknown_relay_locator(&home_ack_error);

    let missing_cache = ack(
        satellite,
        &fixture.native,
        &sent.mailbox_id,
        remote_receipt.clone(),
    )
    .await
    .unwrap_err();
    assert_eq!(missing_cache.status, StatusCode::NOT_FOUND);
    assert_ne!(
        missing_cache.kind,
        Some(ApiErrorKind::UnknownNativeMailboxLocator)
    );
    assert_eq!(cursors(central, &mailbox.id), before);
    move_cache_key(&stale_mailbox.id, &mailbox.id);
    let acknowledged = ack(satellite, &fixture.native, &sent.mailbox_id, remote_receipt)
        .await
        .unwrap();
    let remote_caller = satellite
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap()
        .principal
        .participant_key();
    assert_eq!(
        acknowledged
            .participants
            .iter()
            .find(|participant| participant.session_id == remote_caller)
            .unwrap()
            .processed_through,
        reply.sequence
    );

    let valid_message = read_mailbox_message(
        AxumPath((proxy.caller.clone(), reply.message_id.clone())),
        State(central.clone()),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(valid_message.id, reply.message_id);
    assert_eq!(valid_message.mailbox_id, sent.mailbox_id);
    let summary = ack(central, &proxy.caller, &sent.mailbox_id, receipt)
        .await
        .unwrap();
    let caller = central
        .native_mailbox_relay_descriptor(&proxy.caller)
        .unwrap()
        .principal
        .participant_key();
    assert_eq!(
        summary
            .participants
            .iter()
            .find(|participant| participant.session_id == caller)
            .unwrap()
            .processed_through,
        reply.sequence
    );
    proxy.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_relay_ack_scopes_its_lookup_and_preserves_the_home_loss_reason() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let proxy = ProxySend::new(&fixture).await;
    let sent = proxy
        .send(&fixture, "home-association-loss-for-satellite")
        .await;
    let locator = MailboxRelayLocator::decode(&sent.mailbox_id, false)
        .unwrap()
        .unwrap();
    let satellite = &fixture.satellite;
    let central = &fixture.central;
    let receipt = read_relay_page(satellite, &fixture.native, &sent.mailbox_id)
        .await
        .unwrap()
        .receipt
        .unwrap();
    let before = cursors(central, &locator.id);
    assert_eq!(
        central
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
    let warm_read = read_relay_page(satellite, &fixture.native, &sent.mailbox_id)
        .await
        .unwrap_err();
    let warm_ack = ack(
        satellite,
        &fixture.native,
        &sent.mailbox_id,
        receipt.clone(),
    )
    .await
    .unwrap_err();

    let absent =
        MailboxRelayLocator::encode(&locator.home, &format!("mailbox-{}", Uuid::new_v4()), false)
            .unwrap();
    let absent_ack = ack(satellite, &fixture.native, &absent, receipt.clone())
        .await
        .unwrap_err();
    assert_unknown_relay_locator(&absent_ack);

    assert_eq!(
        satellite
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
    let cold_ack = ack(
        satellite,
        &fixture.native,
        &sent.mailbox_id,
        receipt.clone(),
    )
    .await
    .unwrap_err();
    for error in [warm_read, warm_ack, cold_ack] {
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert_eq!(
            error.kind,
            Some(ApiErrorKind::NativeMailboxHomeAssociationMissing)
        );
    }
    assert_eq!(cursors(central, &locator.id), before);

    let replay = proxy
        .send(&fixture, "home-association-loss-for-satellite")
        .await;
    assert!(replay.duplicate);
    assert_eq!(replay.mailbox_id, sent.mailbox_id);
    read_relay_page(satellite, &fixture.native, &sent.mailbox_id)
        .await
        .unwrap();
    ack(satellite, &fixture.native, &sent.mailbox_id, receipt)
        .await
        .unwrap();
    proxy.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_receipt_ack_after_association_loss_recovers_only_through_original_send() {
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
    let conversation = fixture
        .central
        .mailbox_store
        .mailbox_relay_association(&actor, &locator.id)
        .unwrap()
        .unwrap();
    let reply = |key: &str| SendMailboxMessageRequest {
        target_session_id: format!("host:{}::{}", actor.host.host_id, proxy.caller),
        message: "reply remains unread until its page is acknowledged".into(),
        idempotency_key: key.into(),
        topic: None,
        state_stamp: None,
        class: None,
    };
    let received = fixture
        .satellite
        .send_native_mailbox_relay(&fixture.native, &reply("association-loss-reply"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received.mailbox_id, sent.mailbox_id);
    let page = issued_page(&fixture, &proxy, &sent.mailbox_id).await;
    assert_eq!(page.messages.len(), 2);
    assert_eq!(page.messages[0].id, sent.message_id);
    assert_eq!(page.messages[1].id, received.message_id);
    let receipt = page.receipt.expect("the real read issued a receipt");
    let newer = fixture
        .satellite
        .send_native_mailbox_relay(&fixture.native, &reply("association-loss-newer-reply"))
        .await
        .unwrap()
        .unwrap();
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
    let read_error = read_relay_page(&fixture.central, &proxy.caller, &sent.mailbox_id)
        .await
        .unwrap_err();
    assert_eq!(read_error.status, StatusCode::CONFLICT);
    assert_eq!(cursors(&fixture.central, &locator.id), before);
    let result = ack(
        &fixture.central,
        &proxy.caller,
        &sent.mailbox_id,
        receipt.clone(),
    )
    .await;
    let after = cursors(&fixture.central, &locator.id);
    let absent = fixture
        .central
        .mailbox_store
        .mailbox_relay_association(&actor, &locator.id)
        .unwrap()
        .is_none();
    let error = result.expect_err("missing local metadata must not fabricate authority");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error.message.contains("stopped before acknowledgement"),
        "{}",
        error.message
    );
    assert!(
        error
            .message
            .contains("does not establish the current cursor"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("identical committed send request"),
        "{}",
        error.message
    );
    assert!(absent, "a refused ACK cannot invent an association");
    assert_eq!(
        after, before,
        "refusal changes neither participant's cursor"
    );
    let replay = proxy.send(&fixture, "local-receipt-association-loss").await;
    assert!(replay.duplicate);
    assert_eq!(
        (replay.mailbox_id, replay.message_id, replay.sequence),
        (sent.mailbox_id.clone(), sent.message_id, sent.sequence)
    );
    assert_eq!(
        fixture
            .central
            .mailbox_store
            .mailbox_relay_association(&actor, &locator.id)
            .unwrap(),
        Some(conversation)
    );
    assert_eq!(
        cursors(&fixture.central, &locator.id),
        before,
        "the exact send replay restores metadata without acknowledging replies"
    );
    let acknowledged = ack(&fixture.central, &proxy.caller, &sent.mailbox_id, receipt)
        .await
        .unwrap();
    assert_eq!(
        acknowledged
            .participants
            .iter()
            .find(|p| p.session_id == actor.participant_key())
            .unwrap()
            .processed_through,
        received.sequence
    );
    let remaining = read_mailbox(
        AxumPath((proxy.caller.clone(), sent.mailbox_id)),
        State(fixture.central.clone()),
        Json(ReadMailboxRequest {
            after_sequence: None,
            limit: 20,
            issue_receipt: false,
            read_key: None,
        }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(remaining.processed_through, received.sequence);
    assert_eq!(remaining.messages.len(), 1);
    assert_eq!(remaining.messages[0].id, newer.message_id);
    assert!(remaining.receipt.is_none());
    let wake = fixture
        .central
        .mailbox_store
        .unread_wakeup_for_mailbox(&actor.participant_key(), &locator.id)
        .unwrap()
        .unwrap();
    assert_eq!(wake.sequence, newer.sequence);
    assert_eq!(wake.unread_count, 1, "the newer reply still owes a wake");
    let after_ack = cursors(&fixture.central, &locator.id);
    for (participant, through) in before {
        let expected = if participant == actor.participant_key() {
            received.sequence
        } else {
            through
        };
        assert!(
            after_ack.contains(&(participant, expected)),
            "ACK changes only its own cursor"
        );
    }
    let connection = fixture.central.mailbox_store.connection().unwrap();
    let count: u64 = connection
        .query_row(
            "SELECT COUNT(*) FROM mailbox_messages WHERE mailbox_id=?1",
            [&locator.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 3,
        "canonical replay must not append a duplicate message"
    );
    drop(connection);
    proxy.finish().await;
    fixture.finish().await;
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
        !error
            .message
            .contains("no saved native mailbox association"),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn absent_association_does_not_prove_the_locator_store_or_caller_was_valid() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let proxy = ProxySend::new(&fixture).await;
    let sent = proxy.send(&fixture, "unverified-ack-locator").await;
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
        .unwrap()
        .unwrap();
    let receipt = issued_page(&fixture, &proxy, &sent.mailbox_id)
        .await
        .receipt
        .unwrap();
    let stranger = test_session_id(&fixture.central, Agent::Codex);
    let before = cursors(&fixture.central, &locator.id);
    let absent_id = format!("mailbox-{}", Uuid::new_v4());
    let unknown = MailboxRelayLocator::encode(&locator.home, &absent_id, false).unwrap();
    let mut wrong_store = locator.home.clone();
    wrong_store.store_id = Uuid::new_v4().to_string();
    let wrong_store = MailboxRelayLocator::encode(&wrong_store, &absent_id, false).unwrap();
    for mailbox in [unknown, wrong_store] {
        let error = ack(&fixture.central, &proxy.caller, &mailbox, receipt.clone())
            .await
            .unwrap_err();
        assert_unknown_relay_locator(&error);
        assert_eq!(cursors(&fixture.central, &locator.id), before);
        assert!(
            fixture
                .central
                .mailbox_store
                .mailbox_relay_association(&actor, &absent_id)
                .unwrap()
                .is_none(),
            "a locator cannot manufacture an association"
        );
        assert_eq!(
            fixture
                .central
                .mailbox_store
                .mailbox_relay_association(&actor, &locator.id)
                .unwrap(),
            Some(conversation.clone())
        );
    }
    // Deliberately remove only the real association; the stranger still has
    // neither membership nor an issued page, even though the row is absent.
    assert_eq!(
        fixture
            .central
            .mailbox_store
            .connection()
            .unwrap()
            .execute(
                "DELETE FROM mailbox_relay_associations WHERE mailbox_id=?1",
                [&locator.id]
            )
            .unwrap(),
        1
    );
    let error = ack(
        &fixture.central,
        &stranger,
        &sent.mailbox_id,
        receipt.clone(),
    )
    .await
    .unwrap_err();
    assert_unknown_relay_locator(&error);
    assert_eq!(cursors(&fixture.central, &locator.id), before);
    let stranger = fixture
        .central
        .native_mailbox_relay_descriptor(&stranger)
        .unwrap()
        .principal;
    assert!(
        fixture
            .central
            .mailbox_store
            .mailbox_relay_association(&stranger, &locator.id)
            .unwrap()
            .is_none()
    );
    let replay = proxy.send(&fixture, "unverified-ack-locator").await;
    assert!(replay.duplicate);
    assert_eq!(replay.message_id, sent.message_id);
    let valid = ack(&fixture.central, &proxy.caller, &sent.mailbox_id, receipt).await;
    assert!(
        valid.is_ok(),
        "negative attempts must preserve the real participant's receipt"
    );
    proxy.finish().await;
    fixture.finish().await;
}

fn assert_unknown_relay_locator(error: &ApiError) {
    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert_eq!(error.kind, Some(ApiErrorKind::UnknownNativeMailboxLocator));
}
