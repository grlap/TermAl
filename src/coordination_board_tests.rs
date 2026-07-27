//! Store-level tests for the repository coordination board.
//!
//! The HTTP, MCP, and UI layers have their own contract tests. These tests pin
//! the durable invariants that make those surfaces safe: idempotency before
//! CAS, monotonic scope/key versions, tombstone ABA prevention, snapshot
//! pagination, bounded history, writer admission, and restart persistence.

use super::*;

struct CoordinationBoardTestRoot(PathBuf);

impl CoordinationBoardTestRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("termal-board-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("coordination board test root should exist");
        Self(path)
    }

    fn database_path(&self) -> PathBuf {
        self.0.join("termal.sqlite")
    }
}

impl Drop for CoordinationBoardTestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn board_input(
    key: &str,
    value: Option<Value>,
    expected_revision: u64,
    idempotency_key: &str,
) -> CoordinationBoardSetInput {
    CoordinationBoardSetInput {
        scope_project_id: "project-local".to_owned(),
        key: key.to_owned(),
        value,
        expected_revision,
        author_session_id: "session-author".to_owned(),
        author_name: "Author".to_owned(),
        idempotency_key: idempotency_key.to_owned(),
        state_stamp: Some("tree-abc".to_owned()),
    }
}

fn board_error_kind(error: &anyhow::Error) -> CoordinationBoardStoreErrorKind {
    error
        .downcast_ref::<CoordinationBoardStoreError>()
        .expect("coordination board errors should preserve their typed classification")
        .kind
}

#[test]
fn coordination_board_replays_original_receipt_before_evaluating_later_cas_state() {
    let root = CoordinationBoardTestRoot::new();
    let store =
        CoordinationBoardStore::open(&root.database_path()).expect("board store should open");
    let original_input = board_input(
        "freeze.current",
        Some(json!({"state": "frozen"})),
        0,
        "freeze-1",
    );
    let original = store
        .set(&original_input)
        .expect("initial board set should succeed");
    assert_eq!(original.revision, 1);
    assert_eq!(original.generation, 1);
    assert!(!original.duplicate);

    let updated = store
        .set(&board_input(
            "freeze.current",
            Some(json!({"state": "released"})),
            1,
            "freeze-2",
        ))
        .expect("later board update should succeed");
    assert_eq!(updated.revision, 2);
    assert_eq!(updated.generation, 2);

    let replay = store
        .set(&original_input)
        .expect("lost-success retry must return the original receipt");
    assert!(replay.duplicate);
    assert_eq!(replay.revision, 1);
    assert_eq!(replay.generation, 1);
    assert_eq!(replay.value, json!({"state": "frozen"}));
    assert!(!replay.deleted);

    let current = store
        .get("project-local", "freeze.current")
        .expect("current board head should remain readable");
    assert_eq!(current.revision, 2);
    assert_eq!(current.value, json!({"state": "released"}));
    assert!(!current.deleted);
}

#[test]
fn coordination_board_rejects_same_idempotency_key_for_different_intent() {
    let root = CoordinationBoardTestRoot::new();
    let store =
        CoordinationBoardStore::open(&root.database_path()).expect("board store should open");
    store
        .set(&board_input(
            "tree.status",
            Some(json!("clean")),
            0,
            "tree-1",
        ))
        .expect("initial board set should succeed");

    let error = store
        .set(&board_input(
            "tree.status",
            Some(json!("dirty")),
            1,
            "tree-1",
        ))
        .expect_err("changed intent must not reuse an idempotency key");
    assert_eq!(
        board_error_kind(&error),
        CoordinationBoardStoreErrorKind::Conflict
    );
    assert!(
        error
            .to_string()
            .contains("already used for a different update")
    );
    assert_eq!(
        store
            .get("project-local", "tree.status")
            .expect("original board value should remain")
            .value,
        json!("clean")
    );
}

#[test]
fn coordination_board_canonical_json_sorts_nested_object_keys_explicitly() {
    let mut nested = serde_json::Map::new();
    nested.insert("z".to_owned(), json!(2));
    nested.insert("a".to_owned(), json!(1));
    let mut root = serde_json::Map::new();
    root.insert("z".to_owned(), json!(0));
    root.insert("a".to_owned(), Value::Object(nested));

    let value = Value::Object(root);
    let canonical = canonical_coordination_board_value(&value)
        .expect("bounded JSON should canonicalize");
    assert_eq!(canonical, r#"{"a":{"a":1,"z":2},"z":0}"#);

    let test_root = CoordinationBoardTestRoot::new();
    let store =
        CoordinationBoardStore::open(&test_root.database_path()).expect("board store should open");
    let receipt = store
        .set(&board_input(
            "canonical.receipt",
            Some(value),
            0,
            "canonical-receipt",
        ))
        .expect("canonical board value should persist");
    assert_eq!(
        serde_json::to_string(&receipt.value).expect("receipt value should encode"),
        canonical,
        "the immediate receipt must echo the canonical value, not caller insertion order"
    );
}

#[test]
fn coordination_board_tombstone_revision_prevents_aba_resurrection() {
    let root = CoordinationBoardTestRoot::new();
    let store =
        CoordinationBoardStore::open(&root.database_path()).expect("board store should open");
    store
        .set(&board_input(
            "roles.committer",
            Some(json!("session-a")),
            0,
            "role-1",
        ))
        .expect("initial board set should succeed");
    let absent_delete = store
        .set(&board_input("never.created", None, 0, "invalid-delete"))
        .expect_err("delete must not create a tombstone for an absent key");
    assert_eq!(
        board_error_kind(&absent_delete),
        CoordinationBoardStoreErrorKind::NotFound
    );
    let deleted = store
        .set(&board_input("roles.committer", None, 1, "role-delete"))
        .expect("board delete should create a tombstone");
    assert_eq!(deleted.revision, 2);
    assert_eq!(deleted.generation, 2);
    assert_eq!(deleted.value, Value::Null);
    assert!(deleted.deleted);

    let missing = store
        .get("project-local", "roles.committer")
        .expect_err("tombstoned key should not be returned as active");
    let missing = missing
        .downcast_ref::<CoordinationBoardStoreError>()
        .expect("not-found result should be typed");
    assert_eq!(missing.kind, CoordinationBoardStoreErrorKind::NotFound);
    assert_eq!(
        missing.current.as_ref().map(|head| head.revision),
        Some(2),
        "the retained tombstone revision is required for reconciliation"
    );
    let duplicate_delete = store
        .set(&board_input(
            "roles.committer",
            None,
            2,
            "role-delete-again",
        ))
        .expect_err("deleting an exact tombstone revision must report already absent");
    assert_eq!(
        board_error_kind(&duplicate_delete),
        CoordinationBoardStoreErrorKind::NotFound
    );

    let stale_create = store
        .set(&board_input(
            "roles.committer",
            Some(json!("session-b")),
            0,
            "role-stale-create",
        ))
        .expect_err("create-only CAS must not erase a retained tombstone revision");
    let stale_create = stale_create
        .downcast_ref::<CoordinationBoardStoreError>()
        .expect("stale CAS should be typed");
    assert_eq!(stale_create.kind, CoordinationBoardStoreErrorKind::Conflict);
    assert_eq!(
        stale_create.current.as_ref().map(|head| head.revision),
        Some(2)
    );

    let resurrected = store
        .set(&board_input(
            "roles.committer",
            Some(json!("session-b")),
            2,
            "role-resurrect",
        ))
        .expect("caller may explicitly reconcile against the tombstone");
    assert_eq!(resurrected.revision, 3);
    assert_eq!(resurrected.generation, 3);
}

#[test]
fn coordination_board_json_null_is_a_value_not_a_delete() {
    let root = CoordinationBoardTestRoot::new();
    let store =
        CoordinationBoardStore::open(&root.database_path()).expect("board store should open");
    let input = board_input("rulings.active", Some(Value::Null), 0, "rulings-null");
    let receipt = store
        .set(&input)
        .expect("JSON null should be a valid board value");
    assert_eq!(receipt.value, Value::Null);
    assert!(!receipt.deleted);
    let replay = store
        .set(&input)
        .expect("JSON null receipt should survive idempotent serialization");
    assert!(replay.duplicate);
    assert_eq!(replay.value, Value::Null);
    assert!(!replay.deleted);
    assert_eq!(
        store
            .get("project-local", "rulings.active")
            .expect("JSON null key should remain active")
            .value,
        Value::Null
    );
}

#[test]
fn coordination_board_pagination_rejects_mixed_generation_snapshots() {
    let root = CoordinationBoardTestRoot::new();
    let store =
        CoordinationBoardStore::open(&root.database_path()).expect("board store should open");
    for (index, key) in ["alpha.one", "beta.two", "gamma.three"]
        .into_iter()
        .enumerate()
    {
        store
            .set(&board_input(
                key,
                Some(json!(index)),
                0,
                &format!("seed-{index}"),
            ))
            .expect("board seed should succeed");
    }

    let first = store
        .list(&CoordinationBoardListRequest {
            scope_project_id: "project-local".to_owned(),
            limit: Some(2),
            ..CoordinationBoardListRequest::default()
        })
        .expect("first board page should succeed");
    assert_eq!(first.generation, 3);
    assert_eq!(
        first
            .entries
            .iter()
            .map(|entry| entry.key.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha.one", "beta.two"]
    );
    assert_eq!(first.next_after_key.as_deref(), Some("beta.two"));

    store
        .set(&board_input(
            "gamma.three",
            Some(json!("changed")),
            1,
            "gamma-update",
        ))
        .expect("concurrent board mutation should succeed");
    let continuation = store
        .list(&CoordinationBoardListRequest {
            scope_project_id: "project-local".to_owned(),
            after_key: first.next_after_key,
            limit: Some(2),
            snapshot_generation: Some(first.generation),
            known_generation: None,
        })
        .expect_err("pagination must not mix two scope generations");
    let continuation = continuation
        .downcast_ref::<CoordinationBoardStoreError>()
        .expect("snapshot mismatch should be typed");
    assert_eq!(continuation.kind, CoordinationBoardStoreErrorKind::Conflict);
    assert_eq!(continuation.current_generation, Some(4));

    let unchanged = store
        .list(&CoordinationBoardListRequest {
            scope_project_id: "project-local".to_owned(),
            known_generation: Some(4),
            ..CoordinationBoardListRequest::default()
        })
        .expect("known generation probe should succeed");
    assert!(unchanged.unchanged);
    assert!(unchanged.entries.is_empty());
    assert_eq!(unchanged.generation, 4);
}

#[test]
fn coordination_board_history_is_bounded_while_recent_idempotency_receipts_replay() {
    let root = CoordinationBoardTestRoot::new();
    let store =
        CoordinationBoardStore::open(&root.database_path()).expect("board store should open");
    let original_input = board_input("gate.review.round", Some(json!(0)), 0, "round-0");
    store
        .set(&original_input)
        .expect("initial board round should succeed");
    let update_count = COORDINATION_BOARD_HISTORY_REVISIONS_PER_KEY + 5;
    for index in 1..=update_count {
        store
            .set(&board_input(
                "gate.review.round",
                Some(json!(index)),
                index as u64,
                &format!("round-{index}"),
            ))
            .expect("board round update should succeed");
    }

    let connection = store
        .connection()
        .expect("board connection should be enabled");
    let history_count = connection
        .query_row(
            "SELECT COUNT(*)
             FROM coordination_board_history
             WHERE scope_id = 'project-local' AND key = 'gate.review.round'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("history count should be readable");
    let idempotency_count = connection
        .query_row(
            "SELECT COUNT(*)
             FROM coordination_board_idempotency
             WHERE scope_id = 'project-local'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("idempotency count should be readable");
    drop(connection);
    assert_eq!(
        history_count,
        COORDINATION_BOARD_HISTORY_REVISIONS_PER_KEY as i64
    );
    assert_eq!(idempotency_count, (update_count + 1) as i64);

    let replay = store
        .set(&original_input)
        .expect("compaction must not remove the original idempotency receipt");
    assert!(replay.duplicate);
    assert_eq!(replay.revision, 1);
    assert_eq!(
        store
            .get("project-local", "gate.review.round")
            .expect("current board round should remain")
            .revision,
        (update_count + 1) as u64
    );

    let deleted = store
        .set(&board_input(
            "gate.review.round",
            None,
            (update_count + 1) as u64,
            "round-delete",
        ))
        .expect("deleting the retained key should succeed");
    let connection = store
        .connection()
        .expect("board connection should be enabled");
    let history_count = connection
        .query_row(
            "SELECT COUNT(*)
             FROM coordination_board_history
             WHERE scope_id = 'project-local' AND key = 'gate.review.round'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("deleted-key history count should be readable");
    drop(connection);
    assert_eq!(
        history_count, 0,
        "deletion must purge historical values instead of retaining storage behind a tombstone"
    );
    let tombstone = store
        .get("project-local", "gate.review.round")
        .expect_err("deleted key should remain hidden behind its tombstone");
    assert_eq!(
        tombstone
            .downcast_ref::<CoordinationBoardStoreError>()
            .and_then(|error| error.current.as_ref())
            .map(|head| (head.deleted, head.revision)),
        Some((true, deleted.revision)),
        "history purge must retain the current tombstone's ABA-safe revision"
    );
}

#[test]
fn coordination_board_idempotency_receipts_are_bounded_per_scope() {
    let root = CoordinationBoardTestRoot::new();
    let store =
        CoordinationBoardStore::open(&root.database_path()).expect("board store should open");
    let original_input = board_input("retention.original", Some(json!(true)), 0, "original");
    store
        .set(&original_input)
        .expect("initial retained receipt should succeed");

    {
        let connection = store
            .connection()
            .expect("board connection should be enabled");
        connection
            .execute(
                "UPDATE coordination_board_idempotency
                 SET created_at = '2999-01-01T00:00:00Z'
                 WHERE scope_id = 'project-local'
                   AND author_session_id = 'session-author'
                   AND idempotency_key = 'original'",
                [],
            )
            .expect("the oldest receipt should simulate a backwards-clock timestamp");
        let synthetic_count =
            i64::try_from(COORDINATION_BOARD_IDEMPOTENCY_RECEIPTS_PER_SCOPE - 1)
                .expect("test retention count should fit in i64");
        connection
            .execute(
                "WITH RECURSIVE sequence(value) AS (
                   SELECT 0
                   UNION ALL
                   SELECT value + 1 FROM sequence WHERE value + 1 < ?1
                 )
                 INSERT INTO coordination_board_idempotency(
                   scope_id, author_session_id, idempotency_key,
                   request_hash, receipt_json, created_at
                 )
                 SELECT
                   'project-local',
                   'synthetic-author',
                   printf('synthetic-%d', value),
                   printf('hash-%d', value),
                   '{}',
                   '2000-01-01T00:00:00Z'
                 FROM sequence",
                rusqlite::params![synthetic_count],
            )
            .expect("synthetic old receipts should seed the retention boundary");
    }

    let latest_input = board_input("retention.latest", Some(json!(true)), 0, "latest");
    store
        .set(&latest_input)
        .expect("write crossing the retention boundary should succeed");

    let connection = store
        .connection()
        .expect("board connection should remain enabled");
    let (receipt_count, original_exists, oldest_synthetic_exists): (i64, bool, bool) = connection
        .query_row(
            "SELECT
               COUNT(*),
               EXISTS(
                 SELECT 1
                 FROM coordination_board_idempotency
                 WHERE scope_id = 'project-local'
                   AND author_session_id = 'session-author'
                   AND idempotency_key = 'original'
               ),
               EXISTS(
                 SELECT 1
                 FROM coordination_board_idempotency
                 WHERE scope_id = 'project-local'
                   AND author_session_id = 'synthetic-author'
                   AND idempotency_key = 'synthetic-0'
               )
             FROM coordination_board_idempotency
             WHERE scope_id = 'project-local'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("bounded receipt state should be queryable");
    drop(connection);
    assert_eq!(
        receipt_count,
        COORDINATION_BOARD_IDEMPOTENCY_RECEIPTS_PER_SCOPE as i64
    );
    assert!(
        !original_exists,
        "the oldest inserted receipt must be compacted even when its wall-clock timestamp is newer"
    );
    assert!(
        oldest_synthetic_exists,
        "a newer insertion must survive even when its wall-clock timestamp is older"
    );
    let expired_replay = store
        .set(&original_input)
        .expect_err("an expired receipt should fall through to the current CAS outcome");
    assert_eq!(
        expired_replay
            .downcast_ref::<CoordinationBoardStoreError>()
            .expect("expired replay should return a typed conflict")
            .kind,
        CoordinationBoardStoreErrorKind::Conflict
    );
    assert!(
        store
            .set(&latest_input)
            .expect("the newest receipt should replay")
            .duplicate
    );
}

#[test]
fn board_writer_acquires_private_connection_before_shared_writer_admission() {
    let root = CoordinationBoardTestRoot::new();
    let connection_acquired = Arc::new(std::sync::Barrier::new(2));
    let continue_to_writer_admission = Arc::new(std::sync::Barrier::new(2));
    let mut store =
        CoordinationBoardStore::open(&root.database_path()).expect("board store should open");
    store.write_ordering_hook = Some(CoordinationBoardWriteOrderingHook {
        connection_acquired: connection_acquired.clone(),
        continue_to_writer_admission: continue_to_writer_admission.clone(),
    });
    let shared_write_lock = store.write_lock.clone();
    let store = Arc::new(store);
    let worker_store = store.clone();
    let worker = std::thread::spawn(move || {
        worker_store
            .set(&board_input(
                "ordering.probe",
                Some(json!(true)),
                0,
                "ordering-probe",
            ))
            .expect("board write should finish after the ordering probe");
    });

    connection_acquired.wait();
    let mailbox_admission_probe =
        lock_sqlite_state_writer_for(&shared_write_lock, Duration::from_millis(50));
    let mailbox_admission_was_available = mailbox_admission_probe.is_some();
    drop(mailbox_admission_probe);
    continue_to_writer_admission.wait();
    worker.join().expect("board writer thread should join");
    assert!(
        mailbox_admission_was_available,
        "a board writer paused after acquiring its private connection must not yet hold the shared mailbox writer admission"
    );
}

#[test]
fn coordination_board_rejects_invalid_keys_values_and_metadata() {
    let root = CoordinationBoardTestRoot::new();
    let store =
        CoordinationBoardStore::open(&root.database_path()).expect("board store should open");

    for key in [
        "",
        ".leading",
        "trailing.",
        "two..dots",
        "Upper.case",
        "_leading",
        "-leading",
        "one.two.three.four.five.six.seven.eight.nine",
    ] {
        let error = store
            .set(&board_input(key, Some(json!(true)), 0, "invalid-key"))
            .expect_err("invalid board key should fail");
        assert_eq!(
            board_error_kind(&error),
            CoordinationBoardStoreErrorKind::Validation,
            "unexpected error for key `{key}`: {error:#}"
        );
    }

    store
        .set(&board_input(
            "activity.rust-suite",
            Some(json!({"holder": "session-author"})),
            0,
            "valid-hyphen",
        ))
        .expect("honest advisory activity key should satisfy the grammar");

    let oversized = "x".repeat(COORDINATION_BOARD_MAX_VALUE_BYTES + 1);
    let error = store
        .set(&board_input(
            "oversized.value",
            Some(json!(oversized)),
            0,
            "oversized",
        ))
        .expect_err("oversized canonical JSON should fail");
    assert_eq!(
        board_error_kind(&error),
        CoordinationBoardStoreErrorKind::Validation
    );

    let mut deep = Value::Null;
    // Build this in process beyond serde_json's wire-parser depth so the
    // store's own iterative validation path is exercised.
    for _ in 0..=(COORDINATION_BOARD_MAX_VALUE_DEPTH * 8) {
        deep = json!([deep]);
    }
    let error = store
        .set(&board_input("deep.value", Some(deep), 0, "deep"))
        .expect_err("overly deep JSON should fail");
    assert_eq!(
        board_error_kind(&error),
        CoordinationBoardStoreErrorKind::Validation
    );

    for (field, mutate) in [
        ("scope", |input: &mut CoordinationBoardSetInput| {
            input.scope_project_id = "   ".to_owned()
        }),
        ("author session", |input: &mut CoordinationBoardSetInput| {
            input.author_session_id = "   ".to_owned()
        }),
        ("author name", |input: &mut CoordinationBoardSetInput| {
            input.author_name = "   ".to_owned()
        }),
        ("idempotency", |input: &mut CoordinationBoardSetInput| {
            input.idempotency_key = "   ".to_owned()
        }),
    ] as [(&str, fn(&mut CoordinationBoardSetInput)); 4]
    {
        let mut input = board_input("valid.metadata", Some(json!(true)), 0, "valid-metadata");
        mutate(&mut input);
        let error = store
            .set(&input)
            .expect_err("whitespace-only board metadata should fail");
        assert_eq!(
            board_error_kind(&error),
            CoordinationBoardStoreErrorKind::Validation,
            "unexpected error for whitespace-only {field}: {error:#}"
        );
    }

    let mut invalid_metadata = board_input("valid.key", Some(json!(true)), 0, "metadata");
    invalid_metadata.state_stamp = Some("x".repeat(COORDINATION_BOARD_MAX_STATE_STAMP_BYTES + 1));
    let error = store
        .set(&invalid_metadata)
        .expect_err("oversized state stamp should fail");
    assert_eq!(
        board_error_kind(&error),
        CoordinationBoardStoreErrorKind::Validation
    );
}

#[test]
fn coordination_board_tombstones_preserve_cas_without_consuming_live_capacity() {
    let root = CoordinationBoardTestRoot::new();
    let store =
        CoordinationBoardStore::open(&root.database_path()).expect("board store should open");
    {
        let connection = store
            .connection()
            .expect("board connection should be enabled");
        let transaction = rusqlite::Transaction::new_unchecked(
            &connection,
            rusqlite::TransactionBehavior::Immediate,
        )
        .expect("board cap fixture transaction should begin");
        transaction
            .execute(
                "INSERT INTO coordination_board_scopes(scope_id, generation)
                 VALUES('project-local', ?1)",
                rusqlite::params![COORDINATION_BOARD_MAX_LIVE_ENTRIES_PER_SCOPE as i64],
            )
            .expect("board cap fixture scope should insert");
        for index in 0..COORDINATION_BOARD_MAX_LIVE_ENTRIES_PER_SCOPE {
            transaction
                .execute(
                    "INSERT INTO coordination_board_entries(
                       scope_id, key, revision, generation, value_json,
                       author_session_id, author_name, updated_at, state_stamp
                     )
                     VALUES('project-local', ?1, 1, ?2, NULL,
                            'session-author', 'Author', '2026-07-26T00:00:00Z', NULL)",
                    rusqlite::params![format!("retired.{index}"), (index + 1) as i64],
                )
                .expect("board cap fixture head should insert");
        }
        transaction
            .commit()
            .expect("board cap fixture transaction should commit");
    }

    let absent_delete = store
        .set(&board_input("never.created", None, 0, "full-scope-delete"))
        .expect_err("a full scope must still report an absent delete truthfully");
    assert_eq!(
        board_error_kind(&absent_delete),
        CoordinationBoardStoreErrorKind::NotFound
    );

    let receipt = store
        .set(&board_input("new.key", Some(json!(true)), 0, "over-cap"))
        .expect("retained tombstones must not consume live board capacity");
    assert_eq!(receipt.revision, 1);
    let retained_tombstone = store
        .get("project-local", "retired.0")
        .expect_err("retired keys must remain tombstoned");
    assert_eq!(
        retained_tombstone
            .downcast_ref::<CoordinationBoardStoreError>()
            .and_then(|error| error.current.as_ref())
            .map(|head| (head.deleted, head.revision)),
        Some((true, 1)),
        "freeing live capacity must not discard the ABA-safe tombstone token"
    );
}

#[test]
fn coordination_board_bounds_lifetime_distinct_names_but_allows_tombstone_reuse() {
    let root = CoordinationBoardTestRoot::new();
    let store =
        CoordinationBoardStore::open(&root.database_path()).expect("board store should open");
    {
        let connection = store
            .connection()
            .expect("board connection should be enabled");
        let transaction = rusqlite::Transaction::new_unchecked(
            &connection,
            rusqlite::TransactionBehavior::Immediate,
        )
        .expect("distinct-key cap fixture transaction should begin");
        transaction
            .execute(
                "INSERT INTO coordination_board_scopes(scope_id, generation)
                 VALUES('project-local', ?1)",
                rusqlite::params![COORDINATION_BOARD_MAX_DISTINCT_KEYS_PER_SCOPE as i64],
            )
            .expect("distinct-key cap fixture scope should insert");
        for index in 0..COORDINATION_BOARD_MAX_DISTINCT_KEYS_PER_SCOPE {
            transaction
                .execute(
                    "INSERT INTO coordination_board_entries(
                       scope_id, key, revision, generation, value_json,
                       author_session_id, author_name, updated_at, state_stamp
                     )
                     VALUES('project-local', ?1, 1, ?2, NULL,
                            'session-author', 'Author', '2026-07-26T00:00:00Z', NULL)",
                    rusqlite::params![format!("retired.{index}"), (index + 1) as i64],
                )
                .expect("distinct-key cap fixture tombstone should insert");
        }
        transaction
            .commit()
            .expect("distinct-key cap fixture transaction should commit");
    }

    let error = store
        .set(&board_input(
            "brand.new",
            Some(json!(true)),
            0,
            "distinct-over-cap",
        ))
        .expect_err("a new name beyond the lifetime distinct-key cap must be rejected");
    assert_eq!(
        board_error_kind(&error),
        CoordinationBoardStoreErrorKind::Validation
    );
    assert!(error.to_string().contains("4096-distinct-key lifetime limit"));

    let restored = store
        .set(&board_input(
            "retired.0",
            Some(json!("restored")),
            1,
            "reuse-tombstone",
        ))
        .expect("a retained tombstone must remain reusable at the distinct-key cap");
    assert_eq!(restored.revision, 2);
    assert_eq!(restored.value, json!("restored"));
}

#[test]
fn coordination_board_caps_live_keys_and_delete_frees_capacity() {
    let root = CoordinationBoardTestRoot::new();
    let store =
        CoordinationBoardStore::open(&root.database_path()).expect("board store should open");
    {
        let connection = store
            .connection()
            .expect("board connection should be enabled");
        let transaction = rusqlite::Transaction::new_unchecked(
            &connection,
            rusqlite::TransactionBehavior::Immediate,
        )
        .expect("live-key cap fixture transaction should begin");
        transaction
            .execute(
                "INSERT INTO coordination_board_scopes(scope_id, generation)
                 VALUES('project-local', ?1)",
                rusqlite::params![COORDINATION_BOARD_MAX_LIVE_ENTRIES_PER_SCOPE as i64],
            )
            .expect("live-key cap fixture scope should insert");
        for index in 0..COORDINATION_BOARD_MAX_LIVE_ENTRIES_PER_SCOPE {
            transaction
                .execute(
                    "INSERT INTO coordination_board_entries(
                       scope_id, key, revision, generation, value_json,
                       author_session_id, author_name, updated_at, state_stamp
                     )
                     VALUES('project-local', ?1, 1, ?2, 'true',
                            'session-author', 'Author', '2026-07-26T00:00:00Z', NULL)",
                    rusqlite::params![format!("live.{index}"), (index + 1) as i64],
                )
                .expect("live-key cap fixture head should insert");
        }
        transaction
            .commit()
            .expect("live-key cap fixture transaction should commit");
    }

    let error = store
        .set(&board_input("new.key", Some(json!(true)), 0, "over-cap"))
        .expect_err("a 513th live key must be rejected");
    assert_eq!(
        board_error_kind(&error),
        CoordinationBoardStoreErrorKind::Validation
    );
    assert!(error.to_string().contains("512-live-key limit"));

    store
        .set(&board_input("live.0", None, 1, "free-live-slot"))
        .expect("deleting a live fact should free capacity");
    store
        .set(&board_input(
            "new.key",
            Some(json!(true)),
            0,
            "use-freed-slot",
        ))
        .expect("a new key should use capacity freed by a tombstone");
}

#[test]
fn coordination_board_scopes_are_isolated_and_scope_deletion_cascades() {
    let root = CoordinationBoardTestRoot::new();
    let store =
        CoordinationBoardStore::open(&root.database_path()).expect("board store should open");
    store
        .set(&board_input(
            "tree.status",
            Some(json!("project-a")),
            0,
            "project-a-tree",
        ))
        .expect("project A board write should succeed");
    let mut project_b = board_input("tree.status", Some(json!("project-b")), 0, "project-b-tree");
    project_b.scope_project_id = "project-other".to_owned();
    store
        .set(&project_b)
        .expect("project B board write should succeed");

    assert_eq!(
        store
            .get("project-local", "tree.status")
            .expect("project A board value should exist")
            .value,
        json!("project-a")
    );
    assert_eq!(
        store
            .get("project-other", "tree.status")
            .expect("project B board value should exist")
            .value,
        json!("project-b")
    );

    assert!(
        store
            .delete_scope("project-local")
            .expect("scope deletion should succeed")
    );
    assert!(
        !store
            .delete_scope("project-local")
            .expect("scope deletion replay should be harmless")
    );
    assert_eq!(
        board_error_kind(
            &store
                .get("project-local", "tree.status")
                .expect_err("deleted scope should have no active entries")
        ),
        CoordinationBoardStoreErrorKind::NotFound
    );
    assert_eq!(
        board_error_kind(
            &store
                .set(&board_input(
                    "tree.status",
                    Some(json!("stale-writer")),
                    1,
                    "stale-after-project-delete",
                ))
                .expect_err("a stale authorized writer must not recreate a deleted scope")
        ),
        CoordinationBoardStoreErrorKind::NotFound
    );
    assert_eq!(
        store
            .get("project-other", "tree.status")
            .expect("other scope must survive deletion")
            .value,
        json!("project-b")
    );

    let connection = store
        .connection()
        .expect("board connection should be enabled");
    for table in [
        "coordination_board_entries",
        "coordination_board_history",
        "coordination_board_idempotency",
    ] {
        let query = format!("SELECT COUNT(*) FROM {table} WHERE scope_id = 'project-local'");
        let count = connection
            .query_row(&query, [], |row| row.get::<_, i64>(0))
            .expect("cascaded board row count should be readable");
        assert_eq!(count, 0, "scope deletion should clear `{table}`");
    }
    let deletion_fence_count = connection
        .query_row(
            "SELECT COUNT(*)
             FROM coordination_board_deleted_scopes
             WHERE scope_id = 'project-local'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("scope deletion fence should be readable");
    assert_eq!(
        deletion_fence_count, 1,
        "scope deletion must leave an idempotent fence"
    );
}

#[test]
fn coordination_board_persists_across_store_reopen() {
    let root = CoordinationBoardTestRoot::new();
    let path = root.database_path();
    {
        let store = CoordinationBoardStore::open(&path).expect("board store should open");
        store
            .set(&board_input(
                "freeze.current",
                Some(json!({"digest": "abc"})),
                0,
                "freeze-persist",
            ))
            .expect("board write should succeed");
    }
    let reopened = CoordinationBoardStore::open(&path).expect("board store should reopen");
    let head = reopened
        .get("project-local", "freeze.current")
        .expect("reopened board should load its durable head");
    assert_eq!(head.revision, 1);
    assert_eq!(head.updated_at_generation, 1);
    assert_eq!(head.scope_generation, 1);
    assert_eq!(head.value, json!({"digest": "abc"}));
    assert!(!head.deleted);

    // The frontend finds the newest visible write with a string comparison, so
    // keep the persisted wire timestamp fixed-width, UTC, and millisecond exact.
    let parsed_updated_at = chrono::DateTime::parse_from_rfc3339(&head.updated_at)
        .expect("board timestamps should remain valid RFC 3339");
    assert!(
        head.updated_at.ends_with('Z'),
        "board timestamps must stay in UTC for lexical ordering"
    );
    assert_eq!(
        head.updated_at,
        parsed_updated_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    );
}

#[test]
fn coordination_board_lifecycle_cleanup_defers_when_connection_is_busy() {
    let root = CoordinationBoardTestRoot::new();
    let store =
        CoordinationBoardStore::open(&root.database_path()).expect("board store should open");
    let connection_blocker = store
        .connection()
        .expect("test should hold the private board connection");

    let error = store
        .delete_scope_for_project_lifecycle("project-local")
        .expect_err("durable lifecycle cleanup should defer instead of waiting indefinitely");
    assert_eq!(
        board_error_kind(&error),
        CoordinationBoardStoreErrorKind::Retryable
    );
    assert!(
        error
            .to_string()
            .contains("no coordination board write was committed")
    );
    drop(connection_blocker);

    assert!(
        !store
            .delete_scope_for_project_lifecycle("project-local")
            .expect("deferred cleanup should remain safely replayable"),
        "an absent scope has no rows to cascade"
    );
}

#[test]
fn coordination_board_lifecycle_deadline_reaches_sqlite_busy_wait() {
    let root = CoordinationBoardTestRoot::new();
    let path = root.database_path();
    let store = CoordinationBoardStore::open(&path).expect("board store should open");
    store
        .set(&board_input(
            "lifecycle.cleanup",
            Some(json!("pending")),
            0,
            "lifecycle-cleanup-seed",
        ))
        .expect("board scope should exist before cleanup");

    let mut external =
        rusqlite::Connection::open(&path).expect("external SQLite connection should open");
    let external_transaction = external
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .expect("external writer should hold SQLite's writer lock");

    let error = store
        .delete_scope_with_timeout("project-local", Duration::ZERO)
        .expect_err("lifecycle cleanup should honor its deadline at SQLite BEGIN");
    assert_eq!(
        board_error_kind(&error),
        CoordinationBoardStoreErrorKind::Retryable
    );
    assert!(
        error
            .to_string()
            .contains("no coordination board write was committed")
    );
    drop(external_transaction);
    assert!(
        store
            .delete_scope_with_timeout("project-local", Duration::ZERO)
            .expect("the same cleanup should succeed after the SQLite writer releases"),
        "the seeded scope should be deleted"
    );
}

#[test]
fn coordination_board_lifecycle_busy_timeout_is_scoped_and_restored() {
    let connection =
        rusqlite::Connection::open_in_memory().expect("in-memory SQLite should open");
    connection
        .busy_timeout(SQLITE_BUSY_TIMEOUT)
        .expect("ordinary busy timeout should install");

    let observed_timeout_ms =
        with_coordination_board_busy_timeout(&connection, Duration::ZERO, || {
            connection
                .query_row("PRAGMA busy_timeout", [], |row| row.get::<_, u64>(0))
                .context("lifecycle operation should read its effective busy timeout")
        })
        .expect("scoped lifecycle timeout should succeed");
    let restored_timeout_ms = connection
        .query_row("PRAGMA busy_timeout", [], |row| row.get::<_, u64>(0))
        .expect("restored busy timeout should be readable");

    assert_eq!(
        observed_timeout_ms, 0,
        "lifecycle operation must observe its remaining zero budget"
    );
    assert_eq!(
        restored_timeout_ms,
        SQLITE_BUSY_TIMEOUT.as_millis() as u64,
        "ordinary board operations must recover the connection-wide timeout"
    );
}

#[test]
fn coordination_board_writer_admission_failure_is_typed_and_pretransactional() {
    let root = CoordinationBoardTestRoot::new();
    let path = root.database_path();
    let store = CoordinationBoardStore::open_with_write_admission_timeout(&path, Duration::ZERO)
        .expect("board store should open");
    let shared_lock = sqlite_state_write_lock(&path);
    let mailbox_store = MailboxStore::open(&path).expect("mailbox store should share the file");
    assert!(
        Arc::ptr_eq(&shared_lock, &store.write_lock)
            && Arc::ptr_eq(&shared_lock, &mailbox_store.write_lock),
        "board and mailbox writers on one coordination file must share writer admission"
    );
    let blocker = lock_sqlite_state_writer(&shared_lock);
    let error = store
        .set(&board_input(
            "tree.status",
            Some(json!("clean")),
            0,
            "tree-blocked",
        ))
        .expect_err("writer admission exhaustion should fail");
    assert_eq!(
        board_error_kind(&error),
        CoordinationBoardStoreErrorKind::Retryable
    );
    assert!(
        error
            .to_string()
            .contains("no coordination board write was committed")
    );
    drop(blocker);

    let receipt = store
        .set(&board_input(
            "tree.status",
            Some(json!("clean")),
            0,
            "tree-blocked",
        ))
        .expect("same request should succeed after writer release");
    assert!(!receipt.duplicate);
    assert_eq!(receipt.revision, 1);
}

#[test]
fn coordination_board_private_connection_wait_is_bounded_for_reads_and_writes() {
    let root = CoordinationBoardTestRoot::new();
    let store = CoordinationBoardStore::open_with_write_admission_timeout(
        &root.database_path(),
        Duration::ZERO,
    )
    .expect("board store should open");
    let connection_blocker = store
        .connection()
        .expect("test should hold the private board connection");

    let error = store
        .set(&board_input(
            "tree.status",
            Some(json!("blocked")),
            0,
            "private-connection-blocked",
        ))
        .expect_err("private connection exhaustion should fail within the write deadline");
    assert_eq!(
        board_error_kind(&error),
        CoordinationBoardStoreErrorKind::Retryable
    );
    assert!(
        error
            .to_string()
            .contains("no coordination board write was committed")
    );

    for read_error in [
        store
            .get("project-local", "tree.status")
            .expect_err("key reads must not wait indefinitely for the private connection"),
        store
            .list(&CoordinationBoardListRequest {
                scope_project_id: "project-local".to_owned(),
                after_key: None,
                limit: None,
                snapshot_generation: None,
                known_generation: None,
            })
            .expect_err("list reads must not wait indefinitely for the private connection"),
    ] {
        assert_eq!(
            board_error_kind(&read_error),
            CoordinationBoardStoreErrorKind::Retryable
        );
        assert!(
            read_error
                .to_string()
                .contains("no mutation was attempted by this read operation")
        );
    }
    drop(connection_blocker);

    assert_eq!(
        store
            .set(&board_input(
                "tree.status",
                Some(json!("available")),
                0,
                "private-connection-blocked",
            ))
            .expect("same request should succeed after the private connection is released")
            .revision,
        1
    );
}

#[test]
fn blocked_state_writer_does_not_block_mailbox_or_board_writes() {
    let root = CoordinationBoardTestRoot::new();
    let state_path = root.0.join("termal.sqlite");
    let coordination_path = root.0.join("coordination.sqlite");
    let mailbox_store = MailboxStore::open_with_write_admission_timeout(
        &coordination_path,
        Duration::ZERO,
    )
    .expect("mailbox store should open");
    let board_store = CoordinationBoardStore::open_with_write_admission_timeout(
        &coordination_path,
        Duration::ZERO,
    )
    .expect("board store should open");
    let state_writer_lock = sqlite_state_write_lock(&state_path);
    assert!(
        !Arc::ptr_eq(&state_writer_lock, &mailbox_store.write_lock),
        "state and coordination databases must have distinct writer admission"
    );
    assert!(
        Arc::ptr_eq(&mailbox_store.write_lock, &board_store.write_lock),
        "mailbox and board stores must retain coordination-local FIFO ordering"
    );

    let state_writer_guard = lock_sqlite_state_writer(&state_writer_lock);
    let mailbox_receipt = mailbox_store
        .append(&MailboxAppendInput {
            sender_session_id: "session-state-block-sender".to_owned(),
            sender_name: "Sender".to_owned(),
            target_session_id: "session-state-block-target".to_owned(),
            target_name: "Target".to_owned(),
            body: "Coordination stays available.".to_owned(),
            idempotency_key: "state-block-mailbox".to_owned(),
            topic: Some("isolation".to_owned()),
            state_stamp: None,
        })
        .expect("mailbox append must ignore a blocked state writer");
    let board_receipt = board_store
        .set(&board_input(
            "state-writer.status",
            Some(json!("blocked")),
            0,
            "state-block-board",
        ))
        .expect("board write must ignore a blocked state writer");
    let acknowledged = mailbox_store
        .acknowledge(
            "session-state-block-target",
            &mailbox_receipt.mailbox_id,
            0,
            mailbox_receipt.sequence,
        )
        .expect("mailbox acknowledgement must ignore a blocked state writer");
    drop(state_writer_guard);

    assert_eq!(mailbox_receipt.sequence, 1);
    assert_eq!(board_receipt.generation, 1);
    assert_eq!(acknowledged.unread_count, 0);
}

#[test]
fn disabled_coordination_board_store_opens_no_connection_and_fails_every_operation_typed() {
    let store = CoordinationBoardStore::disabled_for_tests();
    assert!(
        store
            .connection
            .lock()
            .expect("coordination board connection mutex poisoned")
            .is_none(),
        "disabled test store must not own a SQLite connection"
    );

    let errors = [
        store
            .get("project-local", "tree.status")
            .expect_err("disabled get must fail"),
        store
            .list(&CoordinationBoardListRequest {
                scope_project_id: "project-local".to_owned(),
                ..CoordinationBoardListRequest::default()
            })
            .expect_err("disabled list must fail"),
        store
            .set(&board_input(
                "tree.status",
                Some(json!("clean")),
                0,
                "disabled-set",
            ))
            .expect_err("disabled set must fail"),
        store
            .delete_scope("project-local")
            .expect_err("disabled scope deletion must fail"),
    ];
    for error in errors {
        assert_eq!(
            board_error_kind(&error),
            CoordinationBoardStoreErrorKind::Disabled,
            "disabled store must never fall through to validation or persistence: {error:#}"
        );
    }
}
