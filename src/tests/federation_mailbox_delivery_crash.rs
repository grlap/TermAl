// Owns crash/reopen invariants at sender and central durability boundaries.
// Uses real transaction preparation hooks before local and central COMMIT;
// no graceful shutdown or synthetic rollback substitutes for native death.
use super::federation_mailbox_delivery_process::CrashChild;
use super::*;

#[test]
fn federation_delivery_process_death_before_local_durability_has_no_intent_or_effect() {
    let (mut child, receipt) = CrashChild::spawn("beforeLocalDurability");
    child.kill_at_cut();
    let reopened = MailboxStore::open(&receipt.path).unwrap();
    let connection = reopened.connection().unwrap();
    for table in [
        "federation_mailbox_intents",
        "mailbox_messages",
        "federation_mailbox_dedup",
        "federation_mailbox_wake_obligations",
    ] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, u64>(0))
                .expect(
                    "current durable schema must survive a process death before the first intent"
                ),
            0
        );
    }
}

#[test]
fn federation_delivery_process_death_after_local_durability_preserves_pending_not_receipt() {
    let (mut child, receipt) = CrashChild::spawn("afterLocalDurability");
    child.kill_at_cut();
    let reopened = MailboxStore::open(&receipt.path).unwrap();
    let connection = reopened.connection().unwrap();
    let row: (String, String, Option<String>, Option<u64>) = connection
        .query_row(
            "SELECT state, target_session_id, message_id, sequence FROM federation_mailbox_intents
         WHERE sender_session_id=?1 AND idempotency_key='crash-key'",
            [&receipt.sender],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(row.0, "localIntentDurable");
    assert_eq!(row.1, receipt.target);
    assert_eq!(row.2, None);
    assert_eq!(row.3, None);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    drop(connection);
    assert!(
        reopened
            .list_for_session(&receipt.sender)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn federation_delivery_process_death_after_central_commit_retains_append_digest_and_wake() {
    let (mut child, receipt) = CrashChild::spawn("afterCentralCommit");
    child.kill_at_cut();
    let reopened = MailboxStore::open(&receipt.path).unwrap();
    let connection = reopened.connection().unwrap();
    let row: (String, String, String, String, String) = connection
        .query_row(
            "SELECT d.state, d.canonical_digest, m.id, w.message_id, w.target_session_id
         FROM federation_mailbox_dedup d JOIN mailbox_messages m ON m.id=d.message_id
         JOIN federation_mailbox_wake_obligations w ON w.message_id=m.id
         WHERE d.sender_session_id=?1 AND d.idempotency_key='crash-key'",
            [&receipt.sender],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .expect(
            "a killed process cannot lose a committed canonical outcome or its wake obligation",
        );
    assert_eq!(row.0, "homeCommitted");
    assert_eq!(row.1.len(), 64);
    assert_eq!(row.2, row.3);
    assert_eq!(row.4, receipt.target);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn federation_delivery_process_death_before_central_commit_rolls_back_append_dedup_wake_and_sequence()
 {
    let (mut child, receipt) = CrashChild::spawn("beforeCentralCommit");
    child.kill_at_cut();
    let reopened = MailboxStore::open(&receipt.path).unwrap();
    let connection = reopened.connection().unwrap();
    for table in [
        "mailboxes",
        "mailbox_participants",
        "mailbox_messages",
        "federation_mailbox_dedup",
        "federation_mailbox_wake_obligations",
    ] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, u64>(0))
                .unwrap(),
            0,
            "{table} must roll back after real process death"
        );
    }
}
