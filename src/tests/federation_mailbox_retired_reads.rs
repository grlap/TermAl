// Owns departed-reader refusal and retained-page controls on disposable stores.
// Explicit departure fixtures test diagnostics, not a reproduction of a race.
use super::*;

async fn refusal_body(error: ApiError) -> (StatusCode, Value) {
    let response = error.into_response();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn receipt_count(state: &AppState) -> u64 {
    state
        .mailbox_store
        .connection()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM mailbox_read_pages", [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[tokio::test]
async fn local_departed_reader_is_distinct_from_missing_and_does_not_rejoin_or_issue() {
    let (state, sender, reader) = crate::tests::mailboxes::mailbox_test_state();
    let appended = state
        .mailbox_store
        .append(&MailboxAppendInput {
            sender_session_id: sender,
            sender_name: "sender".into(),
            target_session_id: reader.clone(),
            target_name: "reader".into(),
            body: "retained body".into(),
            idempotency_key: "departed-read-control".into(),
            topic: None,
            state_stamp: None,
        })
        .unwrap();
    let mailbox = appended.mailbox_id.clone();
    drop(appended);
    let active = state
        .mailbox_store
        .read_range(&reader, &mailbox, None, 20)
        .unwrap();
    state
        .mailbox_store
        .acknowledge_page(&reader, &mailbox, active.receipt.as_deref().unwrap())
        .unwrap();
    let receipts_before = receipt_count(&state);
    state.mailbox_store.mark_session_left(&reader).unwrap();
    let retired = state
        .mailbox_store
        .read_range(&reader, &mailbox, Some(0), 20)
        .unwrap_err();
    let (status, body) = refusal_body(mailbox_api_error(retired)).await;
    let exact = state
        .mailbox_store
        .read_message(&reader, &active.messages[0].id)
        .unwrap_err();
    let (exact_status, exact_body) = refusal_body(mailbox_api_error(exact)).await;
    let absent = state
        .mailbox_store
        .read_range(&reader, "mailbox-missing", None, 20)
        .unwrap_err();
    let (missing_status, missing_body) = refusal_body(mailbox_api_error(absent)).await;
    let (cursor, departed): (u64, bool) = state
        .mailbox_store
        .connection()
        .unwrap()
        .query_row(
            "SELECT processed_through, left_at IS NOT NULL FROM mailbox_participants
                    WHERE mailbox_id=?1 AND session_id=?2",
            rusqlite::params![mailbox, reader],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "participant_retired_during_read");
    assert_eq!(exact_status, StatusCode::CONFLICT);
    assert_eq!(exact_body["code"], "participant_retired_during_read");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("marked as departed")
    );
    assert_eq!(missing_status, StatusCode::NOT_FOUND);
    assert!(missing_body.get("code").is_none());
    assert!(departed, "a refused read must not re-admit its participant");
    assert_eq!(cursor, active.next_after_sequence.unwrap());
    assert_eq!(receipt_count(&state), receipts_before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_retirement_preserves_pre_page_and_selected_page_ownership() {
    let case = MailboxCase::new().await;
    let original = read_request(&case).await;
    let FederationMailboxOperation::Read { mailbox_id, .. } = &original.operation else {
        panic!("read fixture required");
    };
    let reader = original.actor.as_ref().unwrap().binding.participant_key();
    let depart = |left: bool| {
        case.pair.central.mailbox_store.connection().unwrap().execute(
            "UPDATE mailbox_participants SET left_at=CASE WHEN ?3 THEN 'departed-by-fixture' ELSE NULL END
             WHERE mailbox_id=?1 AND session_id=?2",
            rusqlite::params![mailbox_id, reader, left],
        ).unwrap();
    };
    let client = case.client(case.central_port());
    let lease = client.channel.current_lease().unwrap();
    let before = receipt_count(&case.pair.central);
    depart(true);
    let refused_key = new_key(&case);
    let initial = client
        .request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
            lease.clone(),
            keyed(&case, &original, &refused_key, false),
        ))
        .await
        .unwrap_err();
    let (initial_status, initial_body) = refusal_body(initial).await;
    let pre_page_unchanged = receipt_count(&case.pair.central) == before;
    let no_replacement = case
        .pair
        .central
        .mailbox_store
        .federation_reads
        .lock()
        .unwrap()
        .entries
        .is_empty();
    depart(false);
    let key = new_key(&case);
    let issued = client
        .request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
            lease.clone(),
            keyed(&case, &original, &key, false),
        ))
        .await
        .unwrap()
        .value;
    let issued_count = receipt_count(&case.pair.central);
    depart(true);
    let retained = client
        .request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
            lease.clone(),
            keyed(&case, &original, &key, true),
        ))
        .await
        .unwrap_err();
    let (retained_status, retained_body) = refusal_body(retained).await;
    let retained_count = receipt_count(&case.pair.central);
    let still_departed: bool = case.pair.central.mailbox_store.connection().unwrap().query_row(
        "SELECT left_at IS NOT NULL FROM mailbox_participants WHERE mailbox_id=?1 AND session_id=?2",
        rusqlite::params![mailbox_id, reader], |row| row.get(0),
    ).unwrap();
    depart(false);
    let resolved = client
        .request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
            lease,
            keyed(&case, &original, &key, true),
        ))
        .await
        .unwrap()
        .value;
    case.pair.finish().await;
    assert_eq!(initial_status, StatusCode::CONFLICT);
    assert_eq!(initial_body["code"], "participant_retired_during_read");
    assert_eq!(initial_body["effect"], "notEntered");
    assert!(pre_page_unchanged && no_replacement);
    assert_eq!(retained_status, StatusCode::CONFLICT);
    assert_eq!(retained_body["code"], "participant_retired_during_read");
    assert_eq!(retained_body["effect"], "unknown");
    assert!(
        still_departed,
        "the authenticated host must not re-admit a reader"
    );
    assert_eq!(retained_count, issued_count);
    assert_eq!(
        resolved, issued,
        "the same key must retain its original receipt and page"
    );
}

#[tokio::test]
async fn ordinary_retired_reads_keep_the_refusal_until_membership_is_explicitly_restored() {
    use crate::tests::federation_mailboxes::{central_store, local_participant, request};
    let state = central_store();
    let (sender, _) = local_participant(&state, "sender");
    let (native, participant) = local_participant(&state, "reader");
    let sent = state
        .execute_federation_mailbox(
            request(
                &state,
                &sender,
                FederationMailboxOperation::Send {
                    target: participant.session_id.clone(),
                    message: "retained incoming".into(),
                    key: "ordinary-retirement".into(),
                    topic: None,
                    state_stamp: None,
                },
            ),
            None,
        )
        .unwrap()
        .value;
    let mailbox = sent["mailboxId"].as_str().unwrap().to_owned();
    state
        .mailbox_store
        .mark_session_left(&participant.session_id)
        .unwrap();
    for _ in 0..2 {
        let error = read_mailbox(
            AxumPath((native.clone(), mailbox.clone())),
            State(state.clone()),
            Json(ReadMailboxRequest {
                read_key: None,
                issue_receipt: true,
                after_sequence: None,
                limit: 20,
            }),
        )
        .await
        .unwrap_err();
        let (status, body) = refusal_body(error).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "participant_retired_during_read");
        assert_eq!(body["effect"], "notEntered");
    }
    assert_eq!(receipt_count(&state), 0);
    assert!(
        state
            .mailbox_store
            .federation_read_intents
            .lock()
            .unwrap()
            .is_empty(),
        "a refused initial read cannot leave a consumed admission offer to resolve"
    );
    let departed: bool = state.mailbox_store.connection().unwrap().query_row(
        "SELECT left_at IS NOT NULL FROM mailbox_participants WHERE mailbox_id=?1 AND session_id=?2",
        rusqlite::params![mailbox, participant.session_id], |row| row.get(0),
    ).unwrap();
    assert!(
        departed,
        "ordinary host reads must not re-admit the domain principal"
    );
    state
        .mailbox_store
        .reactivate_session_rows(&participant.session_id)
        .unwrap();
    let page = read_mailbox(
        AxumPath((native, mailbox)),
        State(state),
        Json(ReadMailboxRequest {
            read_key: None,
            issue_receipt: true,
            after_sequence: None,
            limit: 20,
        }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].id, sent["messageId"].as_str().unwrap());
    assert!(page.receipt.is_some());
}
