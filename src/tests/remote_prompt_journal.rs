// Disposable SQLite admission-data controls. No model provider, Phase or
// synthetic permission snapshot supplies lifecycle/acceptance evidence here.
// Recovered from the retained journal component; boot refusals use current admission.
use super::*;
use crate::remote_prompt_authority::{AuthorityBasis, DecisionIdentity, Intent, NativeRoot};
use std::num::NonZeroU64;

pub(super) fn intent(key: &str, text: &str) -> Intent {
    Intent::new(
        AuthorityBasis {
            source: NativeRoot::new(
                Uuid::from_u128(1),
                1,
                "session-source".into(),
                Uuid::from_u128(11),
            )
            .unwrap(),
            target: NativeRoot::new(
                Uuid::from_u128(2),
                1,
                "session-target".into(),
                Uuid::from_u128(22),
            )
            .unwrap(),
            grant: DecisionIdentity::new(Uuid::from_u128(3), 1).unwrap(),
            consent: DecisionIdentity::new(Uuid::from_u128(4), 1).unwrap(),
            enrollment_generation: NonZeroU64::new(1).unwrap(),
            placement_generation: None,
        },
        Uuid::from_u128(5),
        key.into(),
        text.into(),
    )
    .unwrap()
}

fn prepare(
    store: &MailboxStore,
    side: RemotePromptJournalSide,
    value: &Intent,
) -> RemotePromptJournalRecord {
    store
        .admit_remote_prompt_journal(RemotePromptJournalWriteMode::Fixture, side, value, |_| Ok(()))
        .unwrap()
        .record
}
fn store() -> (TestTempRoot, PathBuf, MailboxStore) {
    let root = TestTempRoot::create("remote-prompt-journal");
    let path = root.path().join("coordination.sqlite");
    let store = MailboxStore::open(&path).unwrap();
    (root, path, store)
}

#[test]
fn remote_prompt_journal_exact_retry_and_reopen_keep_body_identity() {
    let (_root, path, store) = store();
    let value = intent("same-key", "complete å\r\nbody\n");
    let admitted = store
        .admit_remote_prompt_journal(RemotePromptJournalWriteMode::Fixture, RemotePromptJournalSide::Sender, &value, |new| {
            assert!(new);
            Ok(())
        })
        .unwrap();
    assert!(admitted.inserted);
    let first = admitted.record;
    assert!(!first.evidence.may_have_entered);
    drop(store);
    let reopened = MailboxStore::open(&path).unwrap();
    let retry = reopened
        .admit_remote_prompt_journal(RemotePromptJournalWriteMode::Fixture, RemotePromptJournalSide::Sender, &value, |new| {
            assert!(!new);
            Ok(())
        })
        .unwrap();
    assert!(!retry.inserted);
    assert_eq!(retry.record, first);
    assert_eq!(
        retry.record.intent.components().3.as_bytes(),
        value.components().3.as_bytes()
    );
}

#[test]
fn remote_prompt_journal_first_receive_is_not_recaptured_by_duplicate_or_reopen() {
    let (_root, path, store) = store();
    let value = intent("received", "body");
    let first = store
        .admit_remote_prompt_journal(RemotePromptJournalWriteMode::Fixture, RemotePromptJournalSide::Receiver, &value, |new| {
            assert!(new);
            Ok(())
        })
        .unwrap();
    assert!(first.inserted);
    assert!(first.record.evidence.may_have_entered);
    let duplicate = store
        .admit_remote_prompt_journal(RemotePromptJournalWriteMode::Fixture, RemotePromptJournalSide::Receiver, &value, |new| {
            assert!(!new);
            Ok(())
        })
        .unwrap();
    assert!(!duplicate.inserted);
    assert_eq!(duplicate.record, first.record);
    drop(store);
    let reopened = MailboxStore::open(&path).unwrap();
    assert!(
        !reopened
            .admit_remote_prompt_journal(RemotePromptJournalWriteMode::Fixture, RemotePromptJournalSide::Receiver, &value, |_| Ok(()))
            .unwrap()
            .inserted
    );
}

#[test]
fn remote_prompt_journal_commit_capture_keeps_authority_and_is_first_receive_only() {
    struct Authority<'a>(&'a std::cell::Cell<bool>);
    impl Drop for Authority<'_> {
        fn drop(&mut self) { self.0.set(false); }
    }
    let (_root, path, store) = store();
    let value = intent("commit-capture", "exact body\r\nå\n");
    let held = std::cell::Cell::new(false);
    let captures = std::cell::Cell::new(0);
    let first = store.admit_remote_prompt_journal_while_running(RemotePromptJournalWriteMode::Fixture,
        RemotePromptJournalSide::Receiver, &value, || Ok(()), |new| {
            assert!(new);
            assert!(remote_prompt_read_row(&rusqlite::Connection::open(&path).unwrap(),
                RemotePromptJournalSide::Receiver, &value.components().0.source,
                value.components().2).unwrap().is_none(), "not visible before COMMIT");
            held.set(true);
            Ok(Authority(&held))
        }, |guard, record| {
            assert!(guard.0.get(), "commit authority must survive capture");
            assert_eq!(remote_prompt_read_row(&rusqlite::Connection::open(&path).unwrap(),
                RemotePromptJournalSide::Receiver, &value.components().0.source,
                value.components().2).unwrap().as_ref(), Some(record),
                "capture must follow actual SQLite COMMIT");
            captures.set(captures.get() + 1);
        },
    ).unwrap();
    assert!(first.inserted);
    assert!(!held.get());
    assert_eq!(captures.get(), 1);
    drop(store);
    let reopened = MailboxStore::open(&path).unwrap();
    let duplicate = reopened.admit_remote_prompt_journal_while_running(RemotePromptJournalWriteMode::Fixture,
        RemotePromptJournalSide::Receiver, &value, || Ok(()), |new| {
            assert!(!new);
            Ok(())
        }, |_, _| panic!("duplicate/reload cannot capture activation"),
    ).unwrap();
    assert!(!duplicate.inserted);
    assert_eq!(duplicate.record, first.record);
}

#[test]
fn remote_prompt_journal_commit_cuts_never_lose_or_repeat_capture() {
    for (phase, committed) in [
        ("beforePromptJournalCommit", false),
        ("afterPromptJournalCommit", true),
    ] {
        let (_root, path, store) = store();
        let value = intent("commit-cut", "retained exact body");
        let captures = std::cell::Cell::new(0);
        FEDERATION_EXECUTOR_TEST_CUT.with(|cut| *cut.borrow_mut() = Some(phase));
        let result = store.admit_remote_prompt_journal_while_running(RemotePromptJournalWriteMode::Fixture,
            RemotePromptJournalSide::Receiver, &value, || Ok(()), |_| Ok(()),
            |_, _| captures.set(captures.get() + 1),
        );
        let error = result.unwrap_err();
        assert!(error.to_string().contains(phase), "{error:#}");
        assert_eq!(captures.get(), usize::from(committed));
        drop(store);
        let reopened = MailboxStore::open(&path).unwrap();
        let retained = remote_prompt_read_row(&reopened.connection().unwrap(),
            RemotePromptJournalSide::Receiver, &value.components().0.source,
            value.components().2).unwrap();
        assert_eq!(retained.is_some(), committed);
        let retry = reopened.admit_remote_prompt_journal_while_running(RemotePromptJournalWriteMode::Fixture,
            RemotePromptJournalSide::Receiver, &value, || Ok(()), |new| {
                assert_eq!(new, !committed);
                Ok(())
            }, |_, _| captures.set(captures.get() + 1),
        ).unwrap();
        assert_eq!(retry.inserted, !committed);
        assert_eq!(captures.get(), 1, "lost reply must not capture twice");
    }
}

#[test]
fn remote_prompt_journal_stop_after_writer_admission_refuses_before_authorization() {
    let (_root, _path, store) = store();
    let value = intent("stop-at-writer", "not received");
    let checks = std::cell::Cell::new(0);
    let result = store.admit_remote_prompt_journal_while_running(RemotePromptJournalWriteMode::Fixture,
        RemotePromptJournalSide::Receiver, &value, || {
            checks.set(checks.get() + 1);
            if checks.get() == 2 { bail!("executor stopping after writer admission"); }
            Ok(())
        }, |_| -> Result<()> { panic!("stopped receive must not acquire commit authority") },
        |_, _| panic!("stopped receive must not capture activation"),
    );
    assert!(result.unwrap_err().to_string().contains("executor stopping"));
    assert_eq!(checks.get(), 2);
    assert!(remote_prompt_read_row(&store.connection().unwrap(),
        RemotePromptJournalSide::Receiver, &value.components().0.source,
        value.components().2).unwrap().is_none());
}

#[test]
fn remote_prompt_journal_failed_sqlite_commit_cannot_capture_receive() {
    let (_root, _path, store) = store();
    // A deferred FK reaches the actual COMMIT failure, rather than merely
    // failing statement execution or simulating a failure after success.
    store.connection().unwrap().execute_batch(
        "CREATE TABLE prompt_commit_parent(id INTEGER PRIMARY KEY);
         CREATE TABLE prompt_commit_child(parent_id INTEGER REFERENCES
             prompt_commit_parent(id) DEFERRABLE INITIALLY DEFERRED);
         CREATE TRIGGER fail_prompt_receive_commit AFTER INSERT ON remote_prompt_receives
         BEGIN INSERT INTO prompt_commit_child VALUES(1); END;"
    ).unwrap();
    let value = intent("failed-real-commit", "must remain unreceived");
    let authorized = std::cell::Cell::new(false);
    let error = store.admit_remote_prompt_journal_while_running(RemotePromptJournalWriteMode::Fixture,
        RemotePromptJournalSide::Receiver, &value, || Ok(()), |new| {
            assert!(new);
            authorized.set(true);
            Ok(())
        }, |_, _| panic!("failed COMMIT must not capture activation"),
    ).unwrap_err();
    assert!(authorized.get(), "failure must follow commit authorization");
    assert!(error.to_string().contains("FOREIGN KEY"), "{error:#}");
    let connection = store.connection().unwrap();
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM prompt_commit_child", [],
        |row| row.get::<_, u64>(0)).unwrap(), 0);
    assert!(remote_prompt_read_row(&connection, RemotePromptJournalSide::Receiver,
        &value.components().0.source, value.components().2).unwrap().is_none());
}

#[test]
fn remote_prompt_journal_changed_intent_cannot_reuse_original_key() {
    let (_root, _path, store) = store();
    let original = intent("same-key", "original");
    let first = prepare(&store, RemotePromptJournalSide::Sender, &original);
    assert!(
        store
            .admit_remote_prompt_journal(RemotePromptJournalWriteMode::Fixture,
                RemotePromptJournalSide::Sender,
                &intent("same-key", "changed"),
                |_| Ok(())
            )
            .is_err()
    );
    assert_eq!(
        prepare(&store, RemotePromptJournalSide::Sender, &original),
        first
    );
}

#[test]
fn remote_prompt_journal_authorization_refusal_rolls_back_before_commit() {
    let (_root, _path, store) = store();
    let value = intent("guarded", "must not commit");
    for side in [
        RemotePromptJournalSide::Sender,
        RemotePromptJournalSide::Receiver,
    ] {
        let reached = std::cell::Cell::new(false);
        assert!(
            store
                .admit_remote_prompt_journal(RemotePromptJournalWriteMode::Fixture, side, &value, |new| {
                    assert!(new);
                    reached.set(true);
                    Err::<(), _>(anyhow!("authority retired at commit"))
                })
                .is_err()
        );
        assert!(reached.get());
        assert!(
            store
                .read_remote_prompt_journal(
                    side,
                    &value.components().0.source,
                    "guarded",
                    || Ok(())
                )
                .unwrap()
                .is_none()
        );
        let first = prepare(&store, side, &value);
        assert!(
            store
                .admit_remote_prompt_journal(RemotePromptJournalWriteMode::Fixture, side, &value, |new| {
                    assert!(!new);
                    Err::<(), _>(anyhow!("foreign original owner"))
                })
                .is_err()
        );
        assert_eq!(prepare(&store, side, &value), first);
    }
}

#[test]
fn remote_prompt_journal_possible_entry_is_monotonic_and_survives_reopen() {
    let (_root, path, store) = store();
    let value = intent("possible-entry", "original");
    let first = prepare(&store, RemotePromptJournalSide::Sender, &value);
    let entered = store
        .mark_remote_prompt_possible_entry(RemotePromptJournalWriteMode::Fixture, &first, || Ok(()))
        .unwrap();
    assert!(entered.evidence.may_have_entered);
    assert_eq!(
        store
            .mark_remote_prompt_possible_entry(RemotePromptJournalWriteMode::Fixture, &first, || Ok(()))
            .unwrap(),
        entered
    );
    assert!(
        store
            .record_remote_prompt_journal_evidence(RemotePromptJournalWriteMode::Fixture, &entered, first.evidence, || Ok(()))
            .is_err()
    );
    drop(store);
    let reopened = MailboxStore::open(&path).unwrap();
    let retry = reopened
        .admit_remote_prompt_journal(RemotePromptJournalWriteMode::Fixture, RemotePromptJournalSide::Sender, &value, |new| {
            assert!(!new);
            Ok(())
        })
        .unwrap();
    assert!(!retry.inserted);
    assert_eq!(retry.record, entered);
    assert!(
        reopened
            .read_remote_prompt_journal(
                RemotePromptJournalSide::Sender,
                &value.components().0.source,
                "possible-entry",
                || Err::<(), _>(anyhow!("foreign owner"))
            )
            .is_err()
    );
}

#[test]
fn remote_prompt_journal_failed_evidence_guard_preserves_committed_record() {
    let (_root, _path, store) = store();
    let value = intent("guard", "private body");
    let first = prepare(&store, RemotePromptJournalSide::Sender, &value);
    assert!(
        store
            .mark_remote_prompt_possible_entry(RemotePromptJournalWriteMode::Fixture, &first, || Err::<(), _>(anyhow!("retired")))
            .is_err()
    );
    assert_eq!(
        prepare(&store, RemotePromptJournalSide::Sender, &value),
        first
    );
}

#[test]
fn remote_prompt_journal_history_does_not_need_a_regrant_or_authorize_new_send() {
    let (_root, _path, store) = store();
    let value = intent("history", "original");
    let first = prepare(&store, RemotePromptJournalSide::Sender, &value);
    let entered = store
        .mark_remote_prompt_possible_entry(RemotePromptJournalWriteMode::Fixture, &first, || Ok(()))
        .unwrap();
    let new_policy = |new| {
        if new {
            Err(anyhow!("new dispatch grant revoked"))
        } else {
            Ok(())
        }
    };
    assert_eq!(
        store
            .admit_remote_prompt_journal(RemotePromptJournalWriteMode::Fixture, RemotePromptJournalSide::Sender, &value, new_policy)
            .unwrap()
            .record,
        entered
    );
    assert!(
        store
            .admit_remote_prompt_journal(RemotePromptJournalWriteMode::Fixture,
                RemotePromptJournalSide::Sender,
                &intent("new-after-revoke", "new"),
                new_policy
            )
            .is_err()
    );
}

#[test]
fn remote_prompt_journal_receiver_cannot_become_a_source_retry_instruction() {
    let (_root, _path, store) = store();
    let received = prepare(
        &store,
        RemotePromptJournalSide::Receiver,
        &intent("side", "text"),
    );
    assert!(
        store
            .mark_remote_prompt_possible_entry(RemotePromptJournalWriteMode::Fixture, &received, || Ok(()))
            .is_err()
    );
    let mut reset = received.evidence.clone();
    reset.may_have_entered = false;
    assert!(
        store
            .record_remote_prompt_journal_evidence(RemotePromptJournalWriteMode::Fixture, &received, reset, || Ok(()))
            .is_err()
    );
}

#[test]
fn remote_prompt_journal_strict_data_rejects_model_lifecycle_or_fabricated_queue() {
    for evidence in [
        json!({"schemaVersion":999,"mayHaveEntered":true,"queue":null}),
        json!({"schemaVersion":1,"mayHaveEntered":true,"queue":null,"phase":"accepted"}),
        json!({"schemaVersion":1,"mayHaveEntered":true,"queue":{"entryId":Uuid::from_u128(999),"writeAhead":Uuid::from_u128(1)}}),
    ] {
        let (_root, path, store) = store();
        let value = intent("corruption", "private body not diagnostic");
        prepare(&store, RemotePromptJournalSide::Receiver, &value);
        let encoded = serde_json::to_string(&evidence).unwrap();
        store.connection().unwrap().execute(
            "UPDATE remote_prompt_receives SET evidence_json=?1,retained_bytes=retained_bytes-length(CAST(evidence_json AS BLOB))+length(CAST(?1 AS BLOB))", [encoded]).unwrap();
        drop(store);
        let error = match MailboxStore::open(&path) {
            Err(error) => error,
            Ok(_) => panic!("corrupt retained evidence must refuse boot before use"),
        };
        assert!(!format!("{error:#}").contains("private body"));
    }
}

// Valid immutable rows seed only the isolated capacity fixture. No permission,
// provider acceptance, handoff or synthetic adapter lifecycle is seeded.
fn seed_prompt_rows(store: &MailboxStore, side: RemotePromptJournalSide, count: usize) {
    let mut connection = store.connection().unwrap();
    let transaction =
        begin_mailbox_write(&mut connection, "seeding isolated journal capacity").unwrap();
    for index in 0..count {
        let value = intent(&format!("seed-{index}"), "retained body");
        let (basis, _, key, _) = value.components();
        let (host, epoch, session, creation) = basis.source.components();
        let record = RemotePromptJournalRecord::new(side, value.clone());
        record.validate().unwrap();
        let encoded =
            String::from_utf8(crate::remote_prompt_protocol::encode_stored_intent(&value).unwrap())
                .unwrap();
        let evidence = record.evidence_json().unwrap();
        let bytes = remote_prompt_retained_bytes(&value, &encoded, &evidence);
        transaction
            .execute(
                &format!(
                    "INSERT INTO {} VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                    side.table()
                ),
                rusqlite::params![
                    host.to_string(),
                    epoch,
                    session,
                    creation.to_string(),
                    key,
                    record.operation_id.to_string(),
                    encoded,
                    remote_prompt_intent_digest(&value),
                    evidence,
                    bytes
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
}

#[test]
fn remote_prompt_journal_sender_limit_is_separate_and_exact_retry_is_free() {
    let (_root, _path, store) = store();
    let original = intent("retained", "original");
    let first = prepare(&store, RemotePromptJournalSide::Sender, &original);
    fill_old_sender_rows(&store, 1000);
    prepare(&store, RemotePromptJournalSide::Sender, &intent("independent", "new"));
    seed_prompt_rows(&store, RemotePromptJournalSide::Sender, 998);
    assert_eq!(
        prepare(&store, RemotePromptJournalSide::Sender, &original),
        first
    );
    let new = intent("new-key", "new");
    let error = store
        .admit_remote_prompt_journal(RemotePromptJournalWriteMode::Fixture, RemotePromptJournalSide::Sender, &new, |_| Ok(()))
        .unwrap_err();
    assert!(format!("{error:#}").contains("full"), "{error:#}");
}

#[test]
fn remote_prompt_journal_receiver_byte_limit_does_not_block_old_executor() {
    let (_root, _path, store) = store();
    let value = intent("existing", "original");
    let first = prepare(&store, RemotePromptJournalSide::Receiver, &value);
    seed_prompt_rows(&store, RemotePromptJournalSide::Receiver, 5000);
    let (count, bytes) = remote_prompt_retained_usage(
        &store.connection().unwrap(),
        RemotePromptJournalSide::Receiver,
    )
    .unwrap();
    assert!(count < FEDERATION_EXECUTOR_MAX_COUNT);
    assert!(
        bytes > FEDERATION_EXECUTOR_MAX_BYTES,
        "test discriminates byte bound, not row count"
    );
    assert_eq!(
        prepare(&store, RemotePromptJournalSide::Receiver, &value),
        first
    );
    let new = intent("cannot-fit", "new");
    assert!(
        store
            .admit_remote_prompt_journal(RemotePromptJournalWriteMode::Fixture, RemotePromptJournalSide::Receiver, &new, |_| Ok(()))
            .is_err()
    );
    let old = FederationExecutorOperation {
        central_host_id: "old-central".into(),
        central_epoch: 1,
        operation_key: "old-key".into(),
        target_host_id: "old-target".into(),
        enrollment_generation: 1,
        session_id: "old-session".into(),
        creation_id: "old-creation".into(),
        placement_generation: 1,
        command_kind: "wake".into(),
        payload: "old payload".into(),
    };
    let admitted = store
        .admit_federation_executor_operation(&old, || Ok(()))
        .unwrap();
    assert_eq!(admitted.operation, old);
}

fn fill_old_sender_rows(store: &MailboxStore, count: usize) {
    store.connection().unwrap().execute_batch(&format!(
        "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<{count})
         INSERT INTO federation_mailbox_intents(sender_session_id,idempotency_key,sender_host_id,
          owner_session_id,owner_creation_id,target_session_id,canonical_digest,envelope_json,state,payload,payload_bytes)
         SELECT 'old-sender',printf('old-%d',x),'host','owner','creation','target','digest','envelope','appendOutcomeUnknown','payload',7 FROM n;"
    )).unwrap();
}

#[test]
fn remote_prompt_journal_retention_does_not_spend_old_sender_capacity() {
    let state = federation_mailboxes::central_store();
    let (owner, sender) = federation_mailboxes::local_participant(&state, "sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "target");
    seed_prompt_rows(&state.mailbox_store, RemotePromptJournalSide::Sender, 1000);
    let creation = sender.binding.identity.creation_id.clone();
    let delivery = FederationDelivery {
        version: 1,
        sender: sender.binding,
        target: target.binding,
        key: "new-old-key".into(),
        message: "mailbox flow keeps its independent budget".into(),
        topic: None,
        state_stamp: None,
    };
    let result =
        state
            .mailbox_store
            .prepare_federation_intent(&delivery, &owner, &creation, || Ok(()));
    assert!(result.is_ok(), "{:#}", result.err().unwrap());
}

#[test]
fn remote_prompt_journal_production_writes_refuse_before_callbacks_or_storage() {
    let (_root, _path, store) = store();
    let value = intent("production-block", "retained body");
    let called = std::cell::Cell::new(false);
    for side in [RemotePromptJournalSide::Sender, RemotePromptJournalSide::Receiver] {
        let error = store.admit_remote_prompt_journal(RemotePromptJournalWriteMode::Production,
            side, &value, |_| { called.set(true); Ok(()) }).unwrap_err();
        assert!(format!("{error:#}").contains("production writes unavailable"));
        assert!(store.admit_remote_prompt_journal_while_running(
            RemotePromptJournalWriteMode::Production, side, &value,
            || { called.set(true); Ok(()) }, |_| { called.set(true); Ok(()) },
            |_, _| called.set(true)).is_err());
        assert_eq!(store.connection().unwrap().query_row(
            &format!("SELECT COUNT(*) FROM {}", side.table()), [],
            |row| row.get::<_, u64>(0)).unwrap(), 0);
    }
    let retained = prepare(&store, RemotePromptJournalSide::Sender, &value);
    assert!(store.admit_remote_prompt_journal(RemotePromptJournalWriteMode::Production,
        RemotePromptJournalSide::Sender, &value, |_| { called.set(true); Ok(()) }).is_err());
    assert!(store.mark_remote_prompt_possible_entry(RemotePromptJournalWriteMode::Production,
        &retained, || { called.set(true); Ok(()) }).is_err());
    assert!(store.record_remote_prompt_journal_evidence(RemotePromptJournalWriteMode::Production,
        &retained, retained.evidence.clone(), || { called.set(true); Ok(()) }).is_err());
    assert!(!called.get());
    assert_eq!(store.read_remote_prompt_journal(RemotePromptJournalSide::Sender,
        &value.components().0.source, value.components().2, || Ok(())).unwrap(), Some(retained));
}

#[test]
fn remote_prompt_journal_stored_intent_is_strict_and_excludes_transport_proof() {
    let value = intent("stored", "unchanged\r\nå\n");
    let encoded = crate::remote_prompt_protocol::encode_stored_intent(&value).unwrap();
    assert_eq!(
        crate::remote_prompt_protocol::decode_stored_intent(&encoded).unwrap(),
        value
    );
    let parsed: Value = serde_json::from_slice(&encoded).unwrap();
    assert!(parsed.get("proof").is_none());
    let mut invalid = parsed.clone();
    invalid["source"]["hostId"] = json!(Uuid::nil());
    assert!(
        crate::remote_prompt_protocol::decode_stored_intent(&serde_json::to_vec(&invalid).unwrap())
            .is_err()
    );
    invalid = parsed.clone();
    invalid["unexpected"] = json!(true);
    assert!(
        crate::remote_prompt_protocol::decode_stored_intent(&serde_json::to_vec(&invalid).unwrap())
            .is_err()
    );
    invalid = parsed;
    invalid["schemaVersion"] = json!(2);
    assert!(
        crate::remote_prompt_protocol::decode_stored_intent(&serde_json::to_vec(&invalid).unwrap())
            .is_err()
    );
    assert!(
        crate::remote_prompt_protocol::encode_stored_intent(&intent(
            "oversized",
            &"x".repeat(65_537)
        ))
        .is_err()
    );
}
