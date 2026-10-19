// Physical storage prerequisites exercised through production initialization.
// No fixture creates the executor journal. Direct SQL below tests the durable
// constraints only; it is not an executor, admission API or recovery worker.
use super::*;

const JOURNAL: &str = "federation_executor_operations";

fn require_journal(connection: &rusqlite::Connection) {
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
            [JOURNAL],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        exists,
        "production coordination bootstrap must create the inert executor journal"
    );
}

fn open_journal(path: &FsPath) -> rusqlite::Connection {
    let connection = bootstrap_coordination_database(path).unwrap();
    require_journal(&connection);
    connection
}

fn insert_received(
    connection: &rusqlite::Connection,
    key: &str,
    digest: &str,
) -> rusqlite::Result<usize> {
    connection.execute(
        "INSERT INTO federation_executor_operations (
            central_host_id, central_epoch, operation_key, target_host_id,
            enrollment_generation, session_id, creation_id, placement_generation,
            command_kind, payload_digest, stage, effect_state
         ) VALUES ('central-fixture', 7, ?1, 'satellite-fixture', 3,
                   'session-fixture', 'creation-fixture', 5,
                   'wake', ?2, 'received', 'not_started')",
        rusqlite::params![key, digest],
    )
}

fn assert_constraint(result: rusqlite::Result<usize>) {
    match result {
        Err(rusqlite::Error::SqliteFailure(error, _)) => {
            assert_eq!(error.code, rusqlite::ErrorCode::ConstraintViolation);
        }
        other => panic!("expected a durable journal constraint violation, got {other:?}"),
    }
}

#[test]
fn federation_m4_journal_bootstrap_has_qualified_identity_and_distinct_stage_evidence() {
    let root = TestTempRoot::create("federation-journal-schema");
    let connection = open_journal(&root.path().join("coordination.sqlite"));
    let mut statement = connection.prepare("PRAGMA table_info(federation_executor_operations)").unwrap();
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<BTreeSet<_>>>()
        .unwrap();
    for required in [
        "central_host_id", "central_epoch", "operation_key", "target_host_id",
        "enrollment_generation", "session_id", "creation_id", "placement_generation",
        "command_kind", "payload_digest", "stage", "effect_state",
        "queue_entry_id", "delivery_attempt_id", "runtime_ack", "processed_evidence",
        "processed_evidence_kind",
    ] {
        assert!(columns.contains(required), "missing durable operation field {required}");
    }
    let version: String = connection.query_row(
        "SELECT value FROM meta WHERE key='coordination_schema_version'", [], |row| row.get(0),
    ).unwrap();
    assert_ne!(version, "2", "adding journal schema must explicitly migrate the schema version");
}

#[test]
fn federation_m4_journal_exact_v2_upgrade_preserves_mailbox_receipts_and_cursor() {
    let root = TestTempRoot::create("federation-journal-upgrade");
    let path = root.path().join("coordination.sqlite");
    {
        let old = rusqlite::Connection::open(&path).unwrap();
        // These are frozen historical production schemas, not a journal substitute.
        old.execute_batch(COORDINATION_SCHEMA_V1_SQL).unwrap();
        old.execute_batch(MAILBOX_READ_PAGES_SCHEMA_SQL).unwrap();
        old.execute_batch(
            "INSERT INTO meta VALUES('coordination_schema_version','2');
             INSERT INTO mailboxes VALUES('mailbox-fixture','pair-fixture','created',9);
             INSERT INTO mailbox_participants
               (mailbox_id,session_id,display_name,processed_through,joined_at)
               VALUES('mailbox-fixture','participant-fixture','Participant',4,'joined');
             INSERT INTO mailbox_read_pages VALUES
               ('receipt-fixture','mailbox-fixture','participant-fixture',4,8);",
        ).unwrap();
    }
    let upgraded = open_journal(&path);
    let observed: (i64, String, i64, i64) = upgraded.query_row(
        "SELECT p.processed_through,r.receipt,r.after_sequence,r.through_sequence
         FROM mailbox_participants p JOIN mailbox_read_pages r
           ON p.mailbox_id=r.mailbox_id AND p.session_id=r.session_id",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).unwrap();
    assert_eq!(observed, (4, "receipt-fixture".to_owned(), 4, 8));
    drop(upgraded);
    drop(open_journal(&path));
}

#[test]
fn federation_m4_journal_scoped_key_remains_unique_after_reopen_and_digest_conflict() {
    let root = TestTempRoot::create("federation-journal-key");
    let path = root.path().join("coordination.sqlite");
    let connection = open_journal(&path);
    insert_received(&connection, "operation-fixture", &"a".repeat(64)).unwrap();
    drop(connection);
    let reopened = open_journal(&path);
    assert_constraint(insert_received(&reopened, "operation-fixture", &"a".repeat(64)));
    assert_constraint(insert_received(&reopened, "operation-fixture", &"b".repeat(64)));
    let identity_columns = [
        "central_host_id", "central_epoch", "operation_key", "target_host_id",
        "enrollment_generation", "session_id", "creation_id", "placement_generation",
        "command_kind", "payload_digest", "stage", "effect_state",
    ];
    // Captured authority/target fields fence the original operation; changing
    // them must not manufacture another namespace for that operation's key.
    for (field, changed) in [
        ("central_epoch", "central_epoch+1"),
        ("target_host_id", "target_host_id||'-replacement'"),
        ("enrollment_generation", "enrollment_generation+1"),
        ("session_id", "session_id||'-replacement'"),
        ("creation_id", "creation_id||'-replacement'"),
        ("placement_generation", "placement_generation+1"),
        ("command_kind", "'stop'"),
    ] {
        let selected = identity_columns.iter()
            .map(|column| if *column == field { changed } else { *column })
            .collect::<Vec<_>>().join(",");
        assert_constraint(reopened.execute(
            &format!(
                "INSERT INTO federation_executor_operations ({})
                 SELECT {selected} FROM federation_executor_operations",
                identity_columns.join(","),
            ),
            [],
        ));
    }
    let rows: i64 = reopened.query_row(
        "SELECT COUNT(*) FROM federation_executor_operations", [], |row| row.get(0),
    ).unwrap();
    assert_eq!(rows, 1);
    let digest: String = reopened.query_row(
        "SELECT payload_digest FROM federation_executor_operations", [], |row| row.get(0),
    ).unwrap();
    assert_eq!(digest, "a".repeat(64));
}

#[test]
fn federation_m4_journal_stage_constraints_do_not_equate_received_with_processed() {
    let root = TestTempRoot::create("federation-journal-stages");
    let connection = open_journal(&root.path().join("coordination.sqlite"));
    insert_received(&connection, "operation-fixture", &"a".repeat(64)).unwrap();
    for stage in ["accepted", "processed"] {
        assert_constraint(connection.execute(
            "UPDATE federation_executor_operations SET stage=?1", [stage],
        ));
    }
    connection.execute(
        "UPDATE federation_executor_operations
         SET stage='queued', queue_entry_id='queue-fixture', delivery_attempt_id='attempt-fixture'",
        [],
    ).unwrap();
    connection.execute(
        "UPDATE federation_executor_operations SET effect_state='unknown'", [],
    ).unwrap();
    assert_constraint(connection.execute(
        "UPDATE federation_executor_operations SET stage='processed'", [],
    ));
    let observed: (String, String) = connection.query_row(
        "SELECT stage,effect_state FROM federation_executor_operations", [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(observed, ("queued".to_owned(), "unknown".to_owned()));
}

#[test]
fn federation_m4_journal_ack_evidence_needs_no_fabricated_runtime_acceptance() {
    let root = TestTempRoot::create("federation-journal-ack-evidence");
    let connection = open_journal(&root.path().join("coordination.sqlite"));
    insert_received(&connection, "operation-fixture", &"a".repeat(64)).unwrap();
    connection.execute(
        "UPDATE federation_executor_operations SET effect_state='unknown'", [],
    ).unwrap();
    // A central receipt can arrive from an already-active continuation. It
    // proves cursor processing, without settling an uncertain runtime effect.
    connection.execute(
        "UPDATE federation_executor_operations SET stage='processed',
             processed_evidence_kind='mailbox_ack',processed_evidence='central-issued-receipt'",
        [],
    ).unwrap();
    let observed: (Option<String>, Option<String>, String, String) = connection.query_row(
        "SELECT queue_entry_id,runtime_ack,processed_evidence_kind,effect_state
         FROM federation_executor_operations", [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).unwrap();
    assert_eq!(observed, (None, None, "mailbox_ack".to_owned(), "unknown".to_owned()));
}

#[test]
fn federation_m4_journal_transaction_rollback_and_committed_receive_survive_reopen() {
    let root = TestTempRoot::create("federation-journal-transaction");
    let path = root.path().join("coordination.sqlite");
    let mut connection = open_journal(&path);
    {
        let transaction = connection.transaction().unwrap();
        insert_received(&transaction, "rolled-back", &"a".repeat(64)).unwrap();
        // Drop without commit represents a cut before durable acceptance.
    }
    {
        let transaction = connection.transaction().unwrap();
        insert_received(&transaction, "durable-receive", &"b".repeat(64)).unwrap();
        transaction.commit().unwrap();
    }
    drop(connection);
    let reopened = open_journal(&path);
    let rows: Vec<(String, String)> = reopened.prepare(
        "SELECT operation_key,stage FROM federation_executor_operations ORDER BY operation_key",
    ).unwrap().query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(rows, vec![("durable-receive".to_owned(), "received".to_owned())]);
}

#[test]
fn federation_m4_journal_retains_more_than_the_prompt_receipt_display_limit() {
    let root = TestTempRoot::create("federation-journal-retention");
    let path = root.path().join("coordination.sqlite");
    let connection = open_journal(&path);
    for index in 0..70 {
        let key = format!("operation-{index}");
        insert_received(&connection, &key, &"a".repeat(64)).unwrap();
        connection.execute(
            "UPDATE federation_executor_operations
             SET stage='processed',effect_state='confirmed',queue_entry_id=?1,
                 delivery_attempt_id=?1,runtime_ack=?1,processed_evidence=?1,
                 processed_evidence_kind='mailbox_ack'
             WHERE operation_key=?1", [&key],
        ).unwrap();
    }
    drop(connection);
    let reopened = open_journal(&path);
    let rows: i64 = reopened.query_row(
        "SELECT COUNT(*) FROM federation_executor_operations WHERE stage='processed'", [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(rows, 70, "settled display receipt pruning must not erase operation dedup evidence");
    assert_constraint(insert_received(&reopened, "operation-0", &"b".repeat(64)));
}
