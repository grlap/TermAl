// Owns canonical append/digest/wake atomicity and lost-reply retry controls.
// Does not deliver remote wakes or equate a send receipt with processing.
// New storage tests using real transactions through the central M2 adapter.
use super::*;

fn send(target: &str, key: &str) -> FederationMailboxOperation {
    FederationMailboxOperation::Send {
        target: target.to_owned(),
        message: "canonical UTF-8 ż body".to_owned(),
        key: key.to_owned(),
        topic: Some("topic".to_owned()),
        state_stamp: None,
    }
}

#[test]
fn federation_delivery_append_lost_reply_retry_recovers_one_digest_and_one_wake_after_reopen() {
    let state = federation_mailboxes::central_store();
    let (sender, actor) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    let request =
        federation_mailboxes::request(&state, &sender, send(&target.session_id, "lost-reply"));
    // Commit at home, then lose the response before the sender can settle it.
    let original = state
        .execute_federation_mailbox(request, None)
        .unwrap()
        .value;
    let path = resolve_coordination_persistence_path(state.persistence_path.as_path());
    let reloaded = AppState {
        mailbox_store: Arc::new(MailboxStore::open(&path).unwrap()),
        ..state.clone()
    };
    let duplicate = reloaded
        .execute_federation_mailbox(
            federation_mailboxes::request(
                &reloaded,
                &sender,
                send(&target.session_id, "lost-reply"),
            ),
            None,
        )
        .unwrap()
        .value;
    assert_eq!(duplicate["messageId"], original["messageId"]);
    assert_eq!(duplicate["sequence"], original["sequence"]);
    assert_eq!(duplicate["duplicate"], true);
    let connection = reloaded.mailbox_store.connection().unwrap();
    let row: (String, String, String, u64) = connection
        .query_row(
            "SELECT d.target_session_id, d.canonical_digest, w.message_id, m.sequence
         FROM federation_mailbox_dedup d
         JOIN mailbox_messages m ON m.id=d.message_id
         JOIN federation_mailbox_wake_obligations w ON w.message_id=m.id
         WHERE d.sender_host_id=?1 AND d.sender_session_id=?2 AND d.idempotency_key='lost-reply'",
            rusqlite::params![actor.binding.identity.host_id, actor.session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("canonical append must durably include its digest outcome and wake obligation");
    assert_eq!(row.0, target.session_id);
    assert_eq!(row.1.len(), 64);
    assert_eq!(row.2, original["messageId"].as_str().unwrap());
    assert_eq!(row.3, original["sequence"].as_u64().unwrap());
    for table in [
        "mailbox_messages",
        "federation_mailbox_dedup",
        "federation_mailbox_wake_obligations",
    ] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, u64>(0))
                .unwrap(),
            1
        );
    }
}

#[test]
fn federation_delivery_append_wake_write_failure_rolls_back_message_dedup_and_sequence() {
    let state = federation_mailboxes::central_store();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    state.mailbox_store.connection().unwrap().execute_batch(
        "CREATE TRIGGER reject_wake_obligation BEFORE INSERT ON federation_mailbox_wake_obligations
         BEGIN SELECT RAISE(ABORT, 'injected obligation write failure'); END;",
    ).expect("atomic wake-obligation storage must exist");
    assert!(
        state
            .execute_federation_mailbox(
                federation_mailboxes::request(
                    &state,
                    &sender,
                    send(&target.session_id, "rollback")
                ),
                None,
            )
            .is_err()
    );
    {
        let connection = state.mailbox_store.connection().unwrap();
        for table in [
            "mailboxes",
            "mailbox_messages",
            "federation_mailbox_dedup",
            "federation_mailbox_wake_obligations",
        ] {
            assert_eq!(
                connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                        .get::<_, u64>(0))
                    .unwrap(),
                0
            );
        }
        connection
            .execute_batch("DROP TRIGGER reject_wake_obligation")
            .unwrap();
    }
    let committed = state
        .execute_federation_mailbox(
            federation_mailboxes::request(&state, &sender, send(&target.session_id, "rollback")),
            None,
        )
        .unwrap()
        .value;
    assert_eq!(
        committed["sequence"], 1,
        "rollback cannot consume a dense mailbox sequence"
    );
    assert_eq!(committed["duplicate"], false);
}

#[test]
fn federation_delivery_append_parallel_same_key_has_one_canonical_message_and_wake() {
    let state = federation_mailboxes::central_store();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    let barrier = Arc::new(std::sync::Barrier::new(5));
    let mut workers = Vec::new();
    for _ in 0..4 {
        let app = state.clone();
        let sender = sender.clone();
        let target = target.session_id.clone();
        let ready = barrier.clone();
        workers.push(std::thread::spawn(move || {
            let request = federation_mailboxes::request(&app, &sender, send(&target, "concurrent"));
            ready.wait();
            app.execute_federation_mailbox(request, None).unwrap().value
        }));
    }
    barrier.wait();
    let responses: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert!(
        responses
            .iter()
            .all(|row| row["messageId"] == responses[0]["messageId"])
    );
    assert_eq!(
        responses
            .iter()
            .filter(|row| row["duplicate"] == false)
            .count(),
        1
    );
    let connection = state.mailbox_store.connection().unwrap();
    for table in [
        "mailbox_messages",
        "federation_mailbox_dedup",
        "federation_mailbox_wake_obligations",
    ] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, u64>(0))
                .expect("parallel retries must share a durable canonical outcome"),
            1
        );
    }
}
