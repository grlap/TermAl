//! `PersistedState` load/serialize round-trip tests.
//!
//! `PersistedState` is the on-disk schema for TermAl's entire backend
//! state: preferences, projects, remotes, sessions, orchestrators, and
//! workspace layouts. Runtime persistence uses the SQLite-backed store in
//! `src/persist.rs`, while schema-shape tests deserialize in memory when they
//! need to corrupt individual fields deliberately.
//!
//! Schema validation on load is deliberately strict: any missing
//! required field produces an error rather than a silent default, so a
//! corrupted or partially-migrated file cannot resurrect sessions with
//! quietly-broken state. Path normalization additionally folds Windows
//! `\\?\` extended-length prefixes and legacy backslash forms to a
//! canonical form, so a file saved on one machine reloads cleanly on
//! another.
//!
//! The `persisted_state_load_error_after_mutation` helper takes a mutation
//! closure, deserializes the mutated value through the persisted-state schema,
//! and returns the error string. Each required-field test stays focused on a
//! single missing-field assertion without reviving the removed JSON file path.

use super::*;

// Disconnecting only `persist_tx` does not stop the worker created by
// `AppState::new_with_paths`: the worker retains its receiver and may write a
// captured boot delta after the test has switched to synchronous persistence.
// Keep this source lock so persistence tests use the production shutdown fence
// (`shutdown_persist_blocking`) instead of creating an orphan writer.
#[test]
fn tests_must_join_persist_worker_instead_of_disconnecting_its_sender() {
    fn assert_source_tree_has_no_orphan_worker_pattern(root: &FsPath, forbidden: &str) {
        for entry in fs::read_dir(root).expect("test source directory should be readable") {
            let entry = entry.expect("test source entry should be readable");
            let path = entry.path();
            if path.is_dir() {
                assert_source_tree_has_no_orphan_worker_pattern(&path, forbidden);
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                continue;
            }
            let source = fs::read_to_string(&path).expect("Rust test source should be readable");
            assert!(
                !source.contains(forbidden),
                "{} disconnects persist_tx without joining its existing worker; call shutdown_persist_blocking() instead",
                path.display()
            );
        }
    }

    let tests_root = FsPath::new(env!("CARGO_MANIFEST_DIR")).join("src/tests");
    let forbidden = ["persist_tx", " = mpsc::channel().0"].concat();
    assert_source_tree_has_no_orphan_worker_pattern(&tests_root, &forbidden);
}

struct PersistTestRoot(PathBuf);

impl PersistTestRoot {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("termal-persist-{label}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("persist test root should exist");
        Self(path)
    }

    fn path(&self) -> &FsPath {
        &self.0
    }
}

impl Drop for PersistTestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn persisted_state_load_error_after_mutation<F>(inner: StateInner, mutate: F) -> String
where
    F: FnOnce(&mut Value),
{
    let mut encoded = persisted_state_value(&inner);
    mutate(&mut encoded);

    let err = match state_inner_from_persisted_value(encoded) {
        Ok(_) => panic!("mutated persisted state should fail to load"),
        Err(err) => err,
    };
    format!("{err:#}")
}

fn persisted_state_value(inner: &StateInner) -> Value {
    serde_json::to_value(PersistedState::from_inner(inner))
        .expect("persisted state should serialize")
}

fn state_inner_from_persisted_value(encoded: Value) -> Result<StateInner> {
    let persisted: PersistedState =
        serde_json::from_value(encoded).context("failed to deserialize persisted state")?;
    persisted
        .into_inner()
        .context("failed to validate state from in-memory persisted state")
}

#[test]
fn app_state_boot_ignores_legacy_coordination_tables_and_initializes_current_store() {
    let state_root = PersistTestRoot::new("boot-current-coordination");
    let persistence_path = state_root.path().join("termal.sqlite");
    let coordination_path = resolve_coordination_persistence_path(&persistence_path);
    let templates_path = state_root.path().join("orchestrators.json");
    persist_state(&persistence_path, &StateInner::new())
        .expect("empty application state should persist");
    {
        let connection = rusqlite::Connection::open(&persistence_path)
            .expect("primary state database should reopen");
        connection
            .execute_batch(
                "
                CREATE TABLE mailboxes (
                  id TEXT PRIMARY KEY,
                  participant_key TEXT NOT NULL UNIQUE,
                  created_at TEXT NOT NULL,
                  next_sequence INTEGER NOT NULL
                );
                INSERT INTO mailboxes(id, participant_key, created_at, next_sequence)
                VALUES(
                  'mailbox-obsolete-boot',
                  'session-old-a\\nsession-old-b',
                  '2026-07-26T00:00:00Z',
                  2
                );
                ",
            )
            .expect("obsolete embedded coordination fixture should initialize");
    }

    let state = AppState::new_with_paths(
        state_root.path().to_string_lossy().into_owned(),
        persistence_path.clone(),
        templates_path,
    )
    .expect("AppState boot should ignore coordination tables in the primary database");
    assert!(
        coordination_path.exists(),
        "boot should create the sibling coordination database"
    );
    assert!(
        state
            .mailbox_store
            .list_for_session("session-old-b")
            .expect("current mailbox store should list")
            .is_empty(),
        "obsolete primary-database coordination rows must not be imported"
    );
    let primary_connection = rusqlite::Connection::open(&persistence_path)
        .expect("primary state database should remain readable during boot");
    let obsolete_mailbox_id: String = primary_connection
        .query_row(
            "SELECT id FROM mailboxes WHERE id = 'mailbox-obsolete-boot'",
            [],
            |row| row.get(0),
        )
        .expect("obsolete primary-database mailbox row should remain untouched");
    assert_eq!(obsolete_mailbox_id, "mailbox-obsolete-boot");
    drop(primary_connection);

    let connection = state
        .mailbox_store
        .connection()
        .expect("current mailbox connection should be available");
    let mailbox_count: u32 = connection
        .query_row("SELECT COUNT(*) FROM mailboxes", [], |row| row.get(0))
        .expect("current mailbox count should read");
    assert_eq!(
        mailbox_count, 0,
        "the independent current coordination database must start empty"
    );
    let mut metadata_statement = connection
        .prepare("SELECT key, value FROM meta ORDER BY key")
        .expect("metadata query should prepare");
    let metadata = metadata_statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("metadata query should run")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("metadata should decode");
    assert_eq!(
        metadata,
        vec![(
            "coordination_schema_version".to_owned(),
            COORDINATION_SQLITE_SCHEMA_VERSION.to_owned()
        )],
        "boot must create only current coordination metadata"
    );
    drop(metadata_statement);
    drop(connection);

    let first = state
        .mailbox_store
        .append(&MailboxAppendInput {
            sender_session_id: "session-current-sender".to_owned(),
            sender_name: "Sender".to_owned(),
            target_session_id: "session-current-target".to_owned(),
            target_name: "Target".to_owned(),
            body: "First current-schema message".to_owned(),
            idempotency_key: "current-boot-1".to_owned(),
            topic: Some("boot-order".to_owned()),
            state_stamp: None,
        })
        .expect("current mailbox append should succeed");
    assert_eq!(
        first.sequence, 1,
        "fresh coordination history starts independently of obsolete embedded rows"
    );
    drop(first);
    state.shutdown_persist_blocking();
}

#[test]
fn app_state_boot_rejects_obsolete_coordination_schema_without_rewrite() {
    let state_root = PersistTestRoot::new("boot-reject-obsolete-coordination");
    let persistence_path = state_root.path().join("termal.sqlite");
    let coordination_path = resolve_coordination_persistence_path(&persistence_path);
    let templates_path = state_root.path().join("orchestrators.json");
    persist_state(&persistence_path, &StateInner::new())
        .expect("empty application state should persist");
    {
        let connection = rusqlite::Connection::open(&coordination_path)
            .expect("obsolete coordination fixture should open");
        connection
            .execute_batch(
                "
                CREATE TABLE mailboxes (
                  id TEXT PRIMARY KEY,
                  participant_key TEXT NOT NULL UNIQUE,
                  created_at TEXT NOT NULL,
                  next_sequence INTEGER NOT NULL
                );
                INSERT INTO mailboxes(id, participant_key, created_at, next_sequence)
                VALUES(
                  'mailbox-obsolete-coordination',
                  'session-old-a\\nsession-old-b',
                  '2026-09-03T00:00:00Z',
                  2
                );
                ",
            )
            .expect("obsolete coordination fixture should initialize");
    }
    let before =
        fs::read(&coordination_path).expect("obsolete coordination bytes should read before boot");

    let error = match AppState::new_with_paths(
        state_root.path().to_string_lossy().into_owned(),
        persistence_path,
        templates_path,
    ) {
        Ok(state) => {
            state.shutdown_persist_blocking();
            panic!("AppState boot must reject an obsolete coordination schema");
        }
        Err(error) => error,
    };
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("unsupported coordination database schema"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Move or delete that coordination database"),
        "{rendered}"
    );
    assert!(
        rendered.contains(&coordination_path.display().to_string()),
        "boot error must name the actual coordination database path: {rendered}"
    );
    assert_eq!(
        fs::read(&coordination_path)
            .expect("obsolete coordination bytes should remain readable after rejection"),
        before,
        "boot validation must reject obsolete coordination state without rewriting it"
    );
}

#[test]
fn app_state_boot_hands_durable_project_board_cleanup_to_the_dedicated_worker() {
    let state_root = PersistTestRoot::new("board-cleanup-replay");
    let persistence_path = state_root.path().join("termal.sqlite");
    let coordination_path = resolve_coordination_persistence_path(&persistence_path);
    let templates_path = state_root.path().join("orchestrators.json");
    let scope_project_id = "project-deleted-before-crash";
    let mut inner = StateInner::new();
    inner
        .pending_coordination_scope_deletions
        .insert(scope_project_id.to_owned());
    persist_state(&persistence_path, &inner).expect("pending cleanup outbox should persist");
    bootstrap_coordination_database(&coordination_path)
        .expect("coordination database should initialize before writes");
    {
        let board_store =
            CoordinationBoardStore::open(&coordination_path).expect("board store should open");
        let mut input = CoordinationBoardSetInput {
            scope_project_id: scope_project_id.to_owned(),
            key: "status.before-crash".to_owned(),
            value: Some(json!("present")),
            expected_revision: 0,
            author_session_id: "session-before-crash".to_owned(),
            author_name: "Before Crash".to_owned(),
            idempotency_key: "before-crash-write".to_owned(),
            state_stamp: None,
        };
        board_store
            .set(&input)
            .expect("pre-crash board row should persist");
        input.key = "status.second".to_owned();
        input.idempotency_key = "before-crash-write-2".to_owned();
        board_store
            .set(&input)
            .expect("second pre-crash board row should persist");
    }

    let state = AppState::new_with_paths(
        state_root.path().to_string_lossy().into_owned(),
        persistence_path.clone(),
        templates_path,
    )
    .expect("boot should schedule the durable cleanup outbox");
    // Stop both background workers before asserting the cleanup result. If the
    // dedicated worker already won the race this pass is an idempotent no-op;
    // otherwise it deterministically completes the same durable work without
    // any wall-clock polling.
    state.shutdown_persist_blocking();
    let pass = process_pending_coordination_scope_deletions(&state.inner, |scope_project_id| {
        state
            .coordination_board_store
            .delete_scope_for_project_lifecycle(scope_project_id)
    });
    assert!(!pass.pending);
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        state
            .persist_internal_locked(&inner)
            .expect("completed cleanup should clear the durable outbox");
    }
    let error = state
        .coordination_board_store
        .set(&CoordinationBoardSetInput {
            scope_project_id: scope_project_id.to_owned(),
            key: "status.stale-writer".to_owned(),
            value: Some(json!("must-not-reappear")),
            expected_revision: 0,
            author_session_id: "session-stale".to_owned(),
            author_name: "Stale".to_owned(),
            idempotency_key: "stale-after-restart".to_owned(),
            state_stamp: None,
        })
        .expect_err("dedicated cleanup must install a durable deletion fence");
    assert_eq!(
        error
            .downcast_ref::<CoordinationBoardStoreError>()
            .expect("stale write should return a typed board error")
            .kind,
        CoordinationBoardStoreErrorKind::NotFound
    );
    assert!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .pending_coordination_scope_deletions
            .is_empty(),
        "cleanup should clear the in-memory outbox only after board cleanup"
    );
    drop(state);

    let reloaded = load_state(&persistence_path)
        .expect("primary state should reload")
        .expect("primary state should exist");
    assert!(
        reloaded.pending_coordination_scope_deletions.is_empty(),
        "cleanup persist should durably clear the completed cleanup outbox"
    );
}

#[test]
fn coordination_cleanup_pass_completes_available_scopes_and_retains_retryable_failures() {
    let retryable_scope = "project-retryable-cleanup";
    let completed_scope = "project-completed-cleanup";
    let mut inner = StateInner::new();
    inner
        .pending_coordination_scope_deletions
        .insert(retryable_scope.to_owned());
    inner
        .pending_coordination_scope_deletions
        .insert(completed_scope.to_owned());
    let inner = Arc::new(StateMutex::new(inner));

    let pass = process_pending_coordination_scope_deletions(&inner, |scope_project_id| {
        if scope_project_id == retryable_scope {
            Err(coordination_board_store_error(
                CoordinationBoardStoreErrorKind::Retryable,
                "coordination storage is temporarily busy; no write was committed",
            ))
        } else {
            Ok(true)
        }
    });

    assert_eq!(pass.completed, 1);
    assert!(pass.pending);
    let inner = inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .pending_coordination_scope_deletions
            .contains(retryable_scope),
        "retryable cleanup must remain durably queued for the cleanup worker"
    );
    assert!(
        !inner
            .pending_coordination_scope_deletions
            .contains(completed_scope),
        "completed cleanup should leave the outbox"
    );
}

#[test]
fn coordination_cleanup_pass_retains_non_retryable_failures_without_blocking_primary_work() {
    let scope_project_id = "project-permanent-cleanup-failure";
    let mut inner = StateInner::new();
    inner
        .pending_coordination_scope_deletions
        .insert(scope_project_id.to_owned());
    let inner = Arc::new(StateMutex::new(inner));

    let pass = process_pending_coordination_scope_deletions(&inner, |_| {
        Err(anyhow!("coordination database is corrupt"))
    });

    assert_eq!(pass.completed, 0);
    assert!(pass.pending);
    assert!(
        inner
            .lock()
            .expect("state mutex poisoned")
            .pending_coordination_scope_deletions
            .contains(scope_project_id),
        "a failed cleanup must never be removed from the durable outbox"
    );
}

#[test]
fn response_board_detachment_pass_retains_failures_and_retries_with_the_last_name() {
    let project_id = "project-response-board-detach-retry";
    let last_project_name = "Renamed before deletion";
    let mut inner = StateInner::new();
    inner
        .pending_response_board_project_detachments
        .insert(project_id.to_owned(), last_project_name.to_owned());
    let inner = Arc::new(StateMutex::new(inner));

    let failed = process_pending_response_board_project_detachments(
        &inner,
        |observed_project_id, observed_project_name| {
            assert_eq!(observed_project_id, project_id);
            assert_eq!(observed_project_name, last_project_name);
            Err("response-board database is temporarily unavailable".to_owned())
        },
    );
    assert_eq!(failed.completed, 0);
    assert!(failed.pending);
    assert_eq!(
        inner
            .lock()
            .expect("state mutex poisoned")
            .pending_response_board_project_detachments
            .get(project_id)
            .map(String::as_str),
        Some(last_project_name),
        "failed conversion must retain both the durable intent and final name"
    );

    let retried = process_pending_response_board_project_detachments(
        &inner,
        |observed_project_id, observed_project_name| {
            assert_eq!(observed_project_id, project_id);
            assert_eq!(observed_project_name, last_project_name);
            Ok(())
        },
    );
    assert_eq!(retried.completed, 1);
    assert!(!retried.pending);
    assert!(
        inner
            .lock()
            .expect("state mutex poisoned")
            .pending_response_board_project_detachments
            .is_empty()
    );
}

#[test]
fn coordination_cleanup_pass_handles_multiple_scopes_and_a_large_cascade_outside_primary_worker() {
    let state_root = PersistTestRoot::new("coordination-cleanup-large-cascade");
    let coordination_path = state_root.path().join("coordination.sqlite");
    let store = CoordinationBoardStore::open(&coordination_path).expect("board store should open");
    {
        let connection = store
            .connection()
            .expect("test should access the board connection");
        connection
            .execute_batch(
                "
                INSERT INTO coordination_board_scopes(scope_id, generation)
                VALUES('project-large-cleanup', 5000),
                      ('project-second-cleanup', 0);
                WITH RECURSIVE revisions(value) AS (
                  VALUES(1)
                  UNION ALL
                  SELECT value + 1 FROM revisions WHERE value < 5000
                )
                INSERT INTO coordination_board_history(
                  scope_id, key, revision, generation, value_json,
                  author_session_id, author_name, updated_at, state_stamp
                )
                SELECT
                  'project-large-cleanup', 'history.large', value, value, '\"retained\"',
                  'session-history', 'History', '2026-07-27T00:00:00.000Z', NULL
                FROM revisions;
                ",
            )
            .expect("large retained-history fixture should persist");
    }
    let mut inner = StateInner::new();
    inner
        .pending_coordination_scope_deletions
        .insert("project-large-cleanup".to_owned());
    inner
        .pending_coordination_scope_deletions
        .insert("project-second-cleanup".to_owned());
    let inner = Arc::new(StateMutex::new(inner));

    let pass = process_pending_coordination_scope_deletions(&inner, |scope_project_id| {
        store.delete_scope_for_project_lifecycle(scope_project_id)
    });

    assert_eq!(pass.completed, 2);
    assert!(!pass.pending);
    assert!(
        inner
            .lock()
            .expect("state mutex poisoned")
            .pending_coordination_scope_deletions
            .is_empty()
    );
    let connection = store
        .connection()
        .expect("test should access the board connection");
    let retained_rows: u64 = connection
        .query_row(
            "SELECT COUNT(*) FROM coordination_board_history",
            [],
            |row| row.get(0),
        )
        .expect("history count should query");
    assert_eq!(retained_rows, 0, "large scope cascade should be complete");
}

#[test]
fn project_deletion_outbox_survives_deferred_worker_cleanup_and_fences_across_restart() {
    let state_root = PersistTestRoot::new("board-cleanup-worker");
    let persistence_path = state_root.path().join("termal.sqlite");
    let templates_path = state_root.path().join("orchestrators.json");
    let project_id;
    {
        let state = AppState::new_with_paths(
            state_root.path().to_string_lossy().into_owned(),
            persistence_path.clone(),
            templates_path.clone(),
        )
        .expect("initial AppState should boot");
        project_id = create_test_project(&state, state_root.path(), "Durable Delete Project");
        state
            .coordination_board_store
            .set(&CoordinationBoardSetInput {
                scope_project_id: project_id.clone(),
                key: "status.before-delete".to_owned(),
                value: Some(json!("present")),
                expected_revision: 0,
                author_session_id: "session-before-delete".to_owned(),
                author_name: "Before Delete".to_owned(),
                idempotency_key: "worker-delete-seed".to_owned(),
                state_stamp: None,
            })
            .expect("board fixture should persist");
        let connection_blocker = state
            .coordination_board_store
            .connection()
            .expect("test should hold cleanup behind the private connection");

        state
            .delete_project(&project_id)
            .expect("project deletion should enqueue durable cleanup");
        state.shutdown_persist_blocking();

        let inner = state.inner.lock().expect("state mutex poisoned");
        assert!(
            inner
                .projects
                .iter()
                .all(|project| project.id != project_id),
            "the deleted project must remain absent after the persist drain"
        );
        assert!(
            inner
                .pending_coordination_scope_deletions
                .contains(&project_id),
            "shutdown must leave blocked cleanup durable for a later worker"
        );
        drop(inner);
        drop(connection_blocker);

        let durable = load_state(&persistence_path)
            .expect("primary state should reload")
            .expect("primary state should exist");
        assert!(
            durable
                .pending_coordination_scope_deletions
                .contains(&project_id),
            "the outbox must reach termal.sqlite before deferred cleanup"
        );
        let pass = process_pending_coordination_scope_deletions(&state.inner, |scope_project_id| {
            state
                .coordination_board_store
                .delete_scope_for_project_lifecycle(scope_project_id)
        });
        assert_eq!(pass.completed, 1);
        assert!(!pass.pending);
        {
            let inner = state.inner.lock().expect("state mutex poisoned");
            state
                .persist_internal_locked(&inner)
                .expect("completed cleanup should durably clear the outbox");
        }
        let error = state
            .coordination_board_store
            .get(&project_id, "status.before-delete")
            .expect_err("cleanup must fence and delete the board scope");
        assert_eq!(
            error
                .downcast_ref::<CoordinationBoardStoreError>()
                .expect("deleted scope should return a typed board error")
                .kind,
            CoordinationBoardStoreErrorKind::NotFound
        );
    }

    let restarted = AppState::new_with_paths(
        state_root.path().to_string_lossy().into_owned(),
        persistence_path,
        templates_path,
    )
    .expect("restarted AppState should boot");
    let inner = restarted.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .projects
            .iter()
            .all(|project| project.id != project_id),
        "the primary project deletion must survive restart"
    );
    assert!(
        inner.pending_coordination_scope_deletions.is_empty(),
        "completed cleanup must not reappear in the durable outbox"
    );
    drop(inner);
    let stale_write = restarted
        .coordination_board_store
        .set(&CoordinationBoardSetInput {
            scope_project_id: project_id,
            key: "status.after-restart".to_owned(),
            value: Some(json!("must-not-reappear")),
            expected_revision: 0,
            author_session_id: "session-stale-after-restart".to_owned(),
            author_name: "Stale".to_owned(),
            idempotency_key: "worker-delete-stale".to_owned(),
            state_stamp: None,
        })
        .expect_err("the coordination-side fence must survive restart");
    assert_eq!(
        stale_write
            .downcast_ref::<CoordinationBoardStoreError>()
            .expect("stale write should return a typed board error")
            .kind,
        CoordinationBoardStoreErrorKind::NotFound
    );
    restarted.shutdown_persist_blocking();
}

#[test]
fn explicit_state_paths_isolate_telegram_data_dir_from_process_home() {
    let state_root = PersistTestRoot::new("telegram-data-dir");
    let state = AppState::new_with_paths(
        state_root.path().to_string_lossy().into_owned(),
        state_root.path().join("termal.sqlite"),
        state_root.path().join("orchestrators.json"),
    )
    .expect("AppState should boot from explicit test paths");

    assert_eq!(
        state.telegram_data_dir(),
        state_root.path().join(".termal"),
        "an explicitly rooted test state must not depend on process-global HOME"
    );
    state.shutdown_persist_blocking();
}

#[test]
fn project_deletion_does_not_cleanup_board_before_a_queued_persist_is_durable() {
    let state_root = PersistTestRoot::new("board-cleanup-queued");
    let (base, persist_rx) = test_app_state_with_live_persist_channel();
    let board_store = Arc::new(
        CoordinationBoardStore::open(&state_root.path().join("coordination.sqlite"))
            .expect("board cleanup test store should open"),
    );
    let state = AppState {
        persistence_path: Arc::new(state_root.path().join("termal.sqlite")),
        coordination_board_store: board_store.clone(),
        ..base
    };

    let project_id = create_test_project(&state, state_root.path(), "Queued Delete Project");
    board_store
        .set(&CoordinationBoardSetInput {
            scope_project_id: project_id.clone(),
            key: "status.before-delete".to_owned(),
            value: Some(json!("present")),
            expected_revision: 0,
            author_session_id: "session-before-delete".to_owned(),
            author_name: "Before Delete".to_owned(),
            idempotency_key: "queued-delete-seed".to_owned(),
            state_stamp: None,
        })
        .expect("board fixture should persist");

    state
        .delete_project(&project_id)
        .expect("project deletion should queue primary persistence");

    assert_eq!(
        persist_rx.try_iter().count(),
        2,
        "project creation and deletion should each queue one persist wake"
    );
    assert!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .pending_coordination_scope_deletions
            .contains(&project_id),
        "cleanup must stay queued until the primary persist worker makes deletion durable"
    );
    assert_eq!(
        board_store
            .get(&project_id, "status.before-delete")
            .expect("board data must survive until queued primary persistence completes")
            .value,
        json!("present")
    );
}

// Pins that legacy Windows `\\?\` verbatim prefixes on a project
// `rootPath` and a session `workdir` are stripped back to their
// canonical form on load. Guards against stale files from older
// TermAl builds resurrecting duplicate or mismatched projects.
#[cfg(windows)]
#[test]
fn persisted_state_normalizes_legacy_local_verbatim_paths() {
    let project_root =
        std::env::temp_dir().join(format!("termal-legacy-verbatim-path-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let legacy_root = format!(r"\\?\{normalized_root}");
    let mut inner = StateInner::new();
    let project = inner.create_project(None, normalized_root.clone(), default_local_remote_id());
    inner.create_session(
        Agent::Claude,
        Some("Claude".to_owned()),
        normalized_root.clone(),
        Some(project.id),
        None,
    );
    let mut encoded = persisted_state_value(&inner);
    encoded["projects"][0]["rootPath"] = Value::String(legacy_root.clone());
    encoded["sessions"][0]["session"]["workdir"] = Value::String(legacy_root);

    let loaded = state_inner_from_persisted_value(encoded).expect("persisted state should load");
    assert_eq!(loaded.projects[0].root_path, normalized_root);
    assert_eq!(loaded.sessions[0].session.workdir, normalized_root);

    let _ = fs::remove_dir_all(project_root);
}

// Pins that legacy `\\?\` verbatim prefixes inside workspace layout
// tabs (filesystem `rootPath`, git/debug `workdir`, source `path`,
// diff `filePath`, pane `sourcePath`) are all normalized on load.
// Guards against tab paths drifting out of sync with canonical roots.
#[cfg(windows)]
#[test]
fn persisted_state_normalizes_legacy_workspace_layout_paths() {
    let project_root =
        std::env::temp_dir().join(format!("termal-layout-verbatim-path-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let legacy_root = format!(r"\\?\{normalized_root}");
    let normalized_file = format!(r"{normalized_root}\src\main.rs");
    let legacy_file = format!(r"\\?\{normalized_file}");
    let mut inner = StateInner::new();
    inner.workspace_layouts.insert(
        "workspace-1".to_owned(),
        WorkspaceLayoutDocument {
            id: "workspace-1".to_owned(),
            revision: 1,
            updated_at: "2026-04-01 12:00:00".to_owned(),
            control_panel_side: WorkspaceControlPanelSide::Left,
            theme_id: None,
            light_theme_id: None,
            dark_theme_id: None,
            theme_mode: None,
            style_id: None,
            font_size_px: None,
            editor_font_size_px: None,
            density_percent: None,
            workspace: json!({
                "root": {
                    "type": "pane",
                    "paneId": "pane-a"
                },
                "panes": [{
                    "id": "pane-a",
                    "tabs": [
                        {
                            "id": "tab-files",
                            "kind": "filesystem",
                            "rootPath": legacy_root,
                            "originSessionId": serde_json::Value::Null
                        },
                        {
                            "id": "tab-git",
                            "kind": "gitStatus",
                            "workdir": format!(r"\\?\{normalized_root}"),
                            "originSessionId": serde_json::Value::Null
                        },
                        {
                            "id": "tab-debug",
                            "kind": "instructionDebugger",
                            "workdir": format!(r"\\?\{normalized_root}"),
                            "originSessionId": serde_json::Value::Null
                        },
                        {
                            "id": "tab-source",
                            "kind": "source",
                            "path": legacy_file,
                            "originSessionId": serde_json::Value::Null
                        },
                        {
                            "id": "tab-diff",
                            "kind": "diffPreview",
                            "changeType": "edit",
                            "diff": "-before\n+after",
                            "diffMessageId": "message-1",
                            "filePath": format!(r"\\?\{normalized_file}"),
                            "originSessionId": serde_json::Value::Null,
                            "summary": "Updated file"
                        }
                    ],
                    "activeTabId": "tab-files",
                    "activeSessionId": serde_json::Value::Null,
                    "viewMode": "filesystem",
                    "lastSessionViewMode": "session",
                    "sourcePath": format!(r"\\?\{normalized_file}")
                }],
                "activePaneId": "pane-a"
            }),
        },
    );
    let loaded = state_inner_from_persisted_value(persisted_state_value(&inner))
        .expect("persisted state should load");
    let layout = loaded
        .workspace_layouts
        .get("workspace-1")
        .expect("workspace layout should load");
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/sourcePath")
            .and_then(Value::as_str),
        Some(normalized_file.as_str())
    );
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/tabs/0/rootPath")
            .and_then(Value::as_str),
        Some(normalized_root.as_str())
    );
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/tabs/1/workdir")
            .and_then(Value::as_str),
        Some(normalized_root.as_str())
    );
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/tabs/2/workdir")
            .and_then(Value::as_str),
        Some(normalized_root.as_str())
    );
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/tabs/3/path")
            .and_then(Value::as_str),
        Some(normalized_file.as_str())
    );
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/tabs/4/filePath")
            .and_then(Value::as_str),
        Some(normalized_file.as_str())
    );

    let _ = fs::remove_dir_all(project_root);
}

#[cfg(windows)]
#[test]
fn app_state_new_with_paths_normalizes_verbatim_bootstrap_workdirs() {
    let _env_lock = TEST_HOME_ENV_MUTEX
        .lock()
        .expect("test home env mutex poisoned");
    let project_root =
        std::env::temp_dir().join(format!("termal-bootstrap-verbatim-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let verbatim_root = format!(r"\\?\{normalized_root}");
    let state_root = std::env::temp_dir().join(format!(
        "termal-bootstrap-verbatim-state-{}",
        Uuid::new_v4()
    ));
    let _state_temp_root = TestTempRoot::own(state_root.clone());
    fs::create_dir_all(&state_root).expect("state root should exist");
    let _home = ScopedEnvVar::set_home_dir(&state_root);
    let persistence_path = state_root.join("termal.sqlite");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let state =
        AppState::new_with_paths(verbatim_root, persistence_path, orchestrator_templates_path)
            .expect("app state should bootstrap from verbatim default workdir");

    assert_eq!(state.default_workdir, normalized_root);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.projects.len(), 1);
    assert_eq!(inner.projects[0].root_path, normalized_root);
    for agent in [Agent::Codex, Agent::Claude] {
        let session = inner
            .sessions
            .iter()
            .find(|record| record.session.agent == agent)
            .expect("bootstrapped live session should exist");
        assert_eq!(session.session.workdir, normalized_root);
        assert_eq!(
            session.session.project_id.as_deref(),
            Some(inner.projects[0].id.as_str())
        );
    }
    drop(inner);
    state.shutdown_persist_blocking();

    let _ = fs::remove_dir_all(project_root);
}

#[cfg(windows)]
fn create_windows_file_symlink_or_skip(target: &FsPath, link: &FsPath) -> bool {
    match std::os::windows::fs::symlink_file(target, link) {
        Ok(()) => true,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("skipping Windows symlink assertion without symlink privilege: {err}");
            false
        }
        Err(err) => panic!("Windows file symlink should be created: {err}"),
    }
}

#[cfg(windows)]
fn create_windows_dir_reparse_point_or_skip(target: &FsPath, link: &FsPath) -> bool {
    match std::os::windows::fs::symlink_dir(target, link) {
        Ok(()) => true,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!(
                "skipping Windows directory reparse-point assertion without symlink privilege: {err}"
            );
            false
        }
        Err(err) => panic!("Windows directory reparse point should be created: {err}"),
    }
}

#[cfg(windows)]
fn assert_windows_state_redirection_rejected(error: anyhow::Error) {
    assert!(
        format!("{error:#}").contains("refusing to follow redirected state path"),
        "{error:#}"
    );
}

#[cfg(windows)]
#[test]
fn windows_sqlite_state_redirection_rejects_main_database_link() {
    let state_root = std::env::temp_dir().join(format!(
        "termal-windows-main-redirection-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let db = state_root.join("termal.sqlite");
    let main_target = state_root.join("main-target.sqlite");

    fs::write(&main_target, b"target").expect("main target should write");
    if create_windows_file_symlink_or_skip(&main_target, &db) {
        let main_error = reject_existing_sqlite_state_path_redirection(&db)
            .expect_err("main sqlite symlink should be rejected");
        assert_windows_state_redirection_rejected(main_error);
    }

    let _ = fs::remove_dir_all(state_root);
}

#[cfg(windows)]
#[test]
fn windows_sqlite_state_redirection_rejects_sidecar_link_independently() {
    let state_root = std::env::temp_dir().join(format!(
        "termal-windows-sidecar-redirection-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let db = state_root.join("termal.sqlite");
    let wal_target = state_root.join("wal-target");

    fs::write(&wal_target, b"wal").expect("wal target should write");
    let wal_link = sqlite_sidecar_path(&db, "-wal");
    if create_windows_file_symlink_or_skip(&wal_target, &wal_link) {
        let wal_error = reject_existing_sqlite_state_path_redirection(&db)
            .expect_err("sqlite sidecar symlink should be rejected");
        assert_windows_state_redirection_rejected(wal_error);
    }

    let _ = fs::remove_dir_all(state_root);
}

#[cfg(windows)]
#[test]
fn windows_sqlite_state_redirection_rejects_termal_directory_reparse_point_independently() {
    let state_root =
        std::env::temp_dir().join(format!("termal-windows-dir-redirection-{}", Uuid::new_v4()));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let redirected_target = state_root.join("redirected-termal-target");
    let termal_dir = state_root.join(".termal");

    fs::create_dir_all(&redirected_target).expect("redirected target should exist");
    if create_windows_dir_reparse_point_or_skip(&redirected_target, &termal_dir) {
        let directory_error =
            reject_existing_sqlite_state_path_redirection(&termal_dir.join("termal.sqlite"))
                .expect_err(".termal directory reparse point should be rejected");
        assert_windows_state_redirection_rejected(directory_error);
    }

    let _ = fs::remove_dir_all(state_root);
}

// Tests that persisted state preserves significant local path spaces.
#[cfg(not(windows))]
#[test]
fn persisted_state_preserves_significant_local_path_spaces() {
    let project_root =
        std::env::temp_dir().join(format!("termal-significant-path-space-{} ", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let state_root = std::env::temp_dir().join(format!(
        "termal-significant-path-space-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let path = state_root.join("termal.sqlite");

    assert!(normalized_root.ends_with(' '));

    let mut inner = StateInner::new();
    let project = inner.create_project(None, normalized_root.clone(), default_local_remote_id());
    inner.create_session(
        Agent::Claude,
        Some("Claude".to_owned()),
        normalized_root.clone(),
        Some(project.id),
        None,
    );
    persist_state(&path, &inner).expect("persisted state should be written");

    let loaded = load_state(&path)
        .expect("persisted state should load")
        .expect("persisted state should exist");
    assert_eq!(loaded.projects[0].root_path, normalized_root);
    assert_eq!(loaded.sessions[0].session.workdir, normalized_root);

    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

// Pins that stripping the top-level `projects` field causes
// `load_state` to fail with `missing field `projects``. Guards
// against sessions reloading against an empty project list and
// silently losing their project associations.
#[test]
fn persisted_state_requires_projects() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Codex,
        Some("Migrated".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        encoded
            .as_object_mut()
            .expect("persisted state should be an object")
            .remove("projects");
    });

    assert!(
        err_text.contains("missing field `projects`"),
        "unexpected load_state error: {err_text}"
    );
}

// Pins that stripping `remoteId` from a project entry causes
// `load_state` to fail with `missing field `remoteId``. Guards
// against projects reloading without a remote binding and being
// silently re-homed onto the default local remote.
#[test]
fn persisted_state_requires_project_remote_id() {
    let mut inner = StateInner::new();
    inner.create_project(None, "/tmp".to_owned(), default_local_remote_id());

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        encoded["projects"]
            .as_array_mut()
            .expect("persisted projects should be an array")[0]
            .as_object_mut()
            .expect("persisted project should be an object")
            .remove("remoteId");
    });

    assert!(
        err_text.contains("missing field `remoteId`"),
        "unexpected load_state error: {err_text}"
    );
}

// Pins that injecting two remotes sharing the same `id` fails load
// with a `duplicate remote id` validation error. Guards against the
// remote registry accepting ambiguous ids that would let sessions
// silently resolve to the wrong transport.
#[test]
fn persisted_state_requires_valid_remotes() {
    let inner = StateInner::new();

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        encoded["preferences"]["remotes"] = json!([
            {
                "id": "local",
                "name": "Local",
                "transport": "local",
                "enabled": true
            },
            {
                "id": "ssh-1",
                "name": "pop-os",
                "transport": "ssh",
                "enabled": true,
                "host": "pop-os.local",
                "port": 22,
                "user": "greg"
            },
            {
                "id": "ssh-1",
                "name": "backup",
                "transport": "ssh",
                "enabled": true,
                "host": "backup.local",
                "port": 22,
                "user": "greg"
            }
        ]);
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("duplicate remote id `ssh-1`"),
        "unexpected load_state error: {err_text}"
    );
}

// Pins that stripping `cursorMode` from a Cursor session fails load
// with a `missing cursorMode` validation error. Guards against
// Cursor sessions reloading with an ambiguous tool mode and
// executing under the wrong approval posture.
#[test]
fn persisted_state_requires_cursor_mode() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Cursor,
        Some("Cursor".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let session = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.remove("cursorMode");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing cursorMode"),
        "unexpected load_state error: {err_text}"
    );
}

// Pins that stripping `claudeApprovalMode` and `claudeEffort` from
// a Claude session fails load with `missing claudeApprovalMode`.
// Guards against Claude sessions losing their approval posture and
// silently reloading with default permissiveness.
#[test]
fn persisted_state_requires_claude_settings() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Claude,
        Some("Claude".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let session = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.remove("claudeApprovalMode");
        session.remove("claudeEffort");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing claudeApprovalMode"),
        "unexpected load_state error: {err_text}"
    );
}

// Pins that stripping `geminiApprovalMode` from a Gemini session
// fails load with `missing geminiApprovalMode`. Guards against
// Gemini sessions reloading with an unspecified approval mode and
// running with a different tool posture than the user configured.
#[test]
fn persisted_state_requires_gemini_approval_mode() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Gemini,
        Some("Gemini".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let session = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.remove("geminiApprovalMode");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing geminiApprovalMode"),
        "unexpected load_state error: {err_text}"
    );
}

// Pins that stripping the explicit OpenCode model/mode authority from an
// OpenCode session fails load. Guards against a persisted explicit choice
// silently reloading as agent-authoritative Auto after restart.
#[test]
fn persisted_state_requires_opencode_settings() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::OpenCode,
        Some("OpenCode".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let session = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.remove("opencodeModel");
        session.remove("opencodeMode");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing opencodeModel"),
        "unexpected load_state error: {err_text}"
    );
}

#[test]
fn sqlite_startup_backfills_pre_effort_opencode_session_to_auto() {
    let state_root = PersistTestRoot::new("opencode-effort-backfill");
    let path = state_root.path().join("termal.sqlite");
    let mut inner = StateInner::new();
    let session_id = inner
        .create_session(
            Agent::OpenCode,
            Some("Pre-effort OpenCode".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    persist_state(&path, &inner).expect("OpenCode fixture should persist");

    let mut encoded: Value = serde_json::from_str(
        &sqlite_row_json(&path, "sessions", &session_id)
            .expect("persisted OpenCode row should exist"),
    )
    .expect("persisted OpenCode row should decode");
    encoded["session"]
        .as_object_mut()
        .expect("persisted session should be an object")
        .remove("opencodeEffort");
    let connection = rusqlite::Connection::open(&path).expect("fixture database should reopen");
    connection
        .execute(
            "UPDATE sessions SET value_json = ?2 WHERE id = ?1",
            rusqlite::params![
                session_id,
                serde_json::to_string(&encoded).expect("legacy fixture should encode")
            ],
        )
        .expect("legacy OpenCode row should update");
    drop(connection);

    let loaded = load_state(&path)
        .expect("pre-effort OpenCode state should load")
        .expect("pre-effort OpenCode state should exist");
    let loaded_index = loaded
        .find_session_index(&session_id)
        .expect("pre-effort OpenCode session must not be quarantined");
    assert_eq!(
        loaded.sessions[loaded_index]
            .session
            .opencode_effort
            .as_deref(),
        Some(OPENCODE_CONFIG_AUTO)
    );

    persist_state(&path, &loaded).expect("backfilled state should persist");
    let healed: Value = serde_json::from_str(
        &sqlite_row_json(&path, "sessions", &session_id)
            .expect("healed OpenCode row should remain"),
    )
    .expect("healed OpenCode row should decode");
    assert_eq!(
        healed["session"]["opencodeEffort"],
        Value::String(OPENCODE_CONFIG_AUTO.to_owned())
    );
}

#[test]
fn persisted_state_rejects_unbounded_opencode_effective_values() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::OpenCode,
        Some("OpenCode".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let invalid_model = persisted_state_load_error_after_mutation(inner, |encoded| {
        encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]["session"]["model"] =
            json!("malicious\nmodel");
    });
    assert!(
        invalid_model.contains("OpenCode model cannot contain control characters"),
        "unexpected load_state error: {invalid_model}"
    );

    let mut inner = StateInner::new();
    inner.create_session(
        Agent::OpenCode,
        Some("OpenCode".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let invalid_effort = persisted_state_load_error_after_mutation(inner, |encoded| {
        encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]["session"]["opencodeCurrentEffort"] =
            json!("high\u{7}");
    });
    assert!(
        invalid_effort.contains("OpenCode reasoning variant cannot contain control characters"),
        "unexpected load_state error: {invalid_effort}"
    );

    let mut inner = StateInner::new();
    inner.create_session(
        Agent::OpenCode,
        Some("OpenCode".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let invalid_mode = persisted_state_load_error_after_mutation(inner, |encoded| {
        encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]["session"]["opencodeCurrentMode"] =
            json!("build\u{7}");
    });
    assert!(
        invalid_mode.contains("OpenCode mode cannot contain control characters"),
        "unexpected load_state error: {invalid_mode}"
    );
}

// Pins that stripping `approvalPolicy`, `reasoningEffort`, and
// `sandboxMode` from a Codex session fails load with `missing
// approvalPolicy`. Guards against Codex sessions reloading without
// the prompt-control triplet that gates each new turn.
#[test]
fn persisted_state_requires_codex_prompt_fields() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Codex,
        Some("Codex".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let session = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.remove("approvalPolicy");
        session.remove("reasoningEffort");
        session.remove("sandboxMode");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing approvalPolicy"),
        "unexpected load_state error: {err_text}"
    );
}

#[test]
fn persisted_state_round_trips_codex_fast_mode_and_defaults_legacy_rows_off() {
    let mut inner = StateInner::new();
    let record = inner.create_session(
        Agent::Codex,
        Some("Fast Codex".to_owned()),
        "/tmp".to_owned(),
        None,
        Some("gpt-5.5".to_owned()),
    );
    let session_id = record.session.id;
    let index = inner
        .find_session_index(&session_id)
        .expect("Codex session should exist");
    inner.sessions[index].session.codex_fast_mode = true;

    let encoded = persisted_state_value(&inner);
    assert_eq!(encoded["sessions"][0]["session"]["codexFastMode"], true);
    let loaded = state_inner_from_persisted_value(encoded.clone())
        .expect("Fast-mode state should round trip");
    assert!(loaded.sessions[0].session.codex_fast_mode);

    let mut legacy = encoded;
    legacy["sessions"][0]["session"]
        .as_object_mut()
        .expect("persisted session should be an object")
        .remove("codexFastMode");
    let loaded_legacy =
        state_inner_from_persisted_value(legacy).expect("pre-Fast session should load");
    assert!(!loaded_legacy.sessions[0].session.codex_fast_mode);
}

// Pins that a Codex session carrying an `externalSessionId` (a live
// thread) fails load when `codexThreadState` is stripped, with
// `missing codexThreadState`. Guards against a live thread coming
// back attached but with no resume state for the orchestrator.
#[test]
fn persisted_state_requires_codex_thread_state_for_live_threads() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Codex,
        Some("Codex".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let entry = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]
            .as_object_mut()
            .expect("persisted session record should be an object");
        entry.insert(
            "externalSessionId".to_owned(),
            Value::String("thread-live".to_owned()),
        );
        let session = entry["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.insert(
            "externalSessionId".to_owned(),
            Value::String("thread-live".to_owned()),
        );
        session.remove("codexThreadState");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing codexThreadState"),
        "unexpected load_state error: {err_text}"
    );
}

// Pins that stripping `source` from a persisted queued prompt fails
// load with `missing field `source``. Guards against queued prompts
// reloading with an unknown origin (user vs orchestrator) and being
// routed or billed against the wrong caller on resume.
#[test]
fn persisted_state_requires_queued_prompt_source() {
    let mut inner = StateInner::new();
    let record = inner.create_session(
        Agent::Codex,
        Some("Queued".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let session_id = record.session.id.clone();
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    queue_prompt_on_record(
        &mut inner.sessions[index],
        PendingPrompt {
            attachments: Vec::new(),
            id: "queued-prompt-1".to_owned(),
            timestamp: stamp_now(),
            text: "queued prompt".to_owned(),
            expanded_text: None,
            source: None,
        },
        Vec::new(),
    );
    let mut encoded = persisted_state_value(&inner);
    let sessions = encoded["sessions"]
        .as_array_mut()
        .expect("persisted sessions should be an array");
    let queued_prompts = sessions[0]["queuedPrompts"]
        .as_array_mut()
        .expect("persisted queued prompts should be an array");
    queued_prompts[0]
        .as_object_mut()
        .expect("queued prompt should be an object")
        .remove("source");

    let err = match state_inner_from_persisted_value(encoded) {
        Ok(_) => panic!("persisted state without queued prompt source should fail"),
        Err(err) => err,
    };
    let err_text = format!("{err:#}");
    assert!(
        err_text.contains("missing field `source`"),
        "unexpected load_state error: {err_text}"
    );
}

#[test]
fn persisted_state_omits_runtime_session_mutation_stamp_on_save() {
    let mut inner = StateInner::new();
    let record = inner.create_session(
        Agent::Claude,
        Some("Stamped".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let session_id = record.session.id.clone();
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    inner.sessions[index].mutation_stamp = 99;
    inner.sessions[index].session.session_mutation_stamp = Some(99);

    let encoded = persisted_state_value(&inner);
    {
        let persisted_session = encoded["sessions"][0]["session"]
            .as_object()
            .expect("persisted session should be an object");
        assert!(
            !persisted_session.contains_key("sessionMutationStamp"),
            "runtime mutation stamps must not be serialized into persisted sessions"
        );
    }
}

#[test]
fn persisted_state_clears_runtime_session_mutation_stamp_on_load() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Claude,
        Some("Stamped".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let mut encoded = persisted_state_value(&inner);
    encoded["sessions"][0]["session"]
        .as_object_mut()
        .expect("persisted session should be an object")
        .insert("sessionMutationStamp".to_owned(), Value::from(99));

    let loaded = state_inner_from_persisted_value(encoded).expect("persisted state should load");
    assert_eq!(loaded.sessions[0].session.session_mutation_stamp, None);
}

#[test]
fn persisted_state_round_trips_conversation_markers() {
    let mut inner = StateInner::new();
    let record = inner.create_session(
        Agent::Codex,
        Some("Marked".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let session_id = record.session.id.clone();
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    inner.sessions[index]
        .session
        .markers
        .push(ConversationMarker {
            id: "marker-1".to_owned(),
            session_id: session_id.clone(),
            kind: ConversationMarkerKind::Decision,
            name: "Use the overview rail".to_owned(),
            body: Some("User accepted the overview-map direction.".to_owned()),
            color: "#3b82f6".to_owned(),
            message_id: "message-1".to_owned(),
            message_index_hint: 0,
            end_message_id: Some("message-3".to_owned()),
            end_message_index_hint: Some(2),
            created_at: "2026-05-01 10:00:00".to_owned(),
            updated_at: "2026-05-01 10:05:00".to_owned(),
            created_by: ConversationMarkerAuthor::User,
        });

    let encoded = persisted_state_value(&inner);
    assert_eq!(
        encoded["sessions"][0]["session"]["markers"][0]["name"],
        Value::String("Use the overview rail".to_owned())
    );

    let loaded = state_inner_from_persisted_value(encoded).expect("persisted state should load");
    let markers = &loaded.sessions[0].session.markers;
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].id, "marker-1");
    assert_eq!(markers[0].session_id, session_id);
    assert_eq!(markers[0].kind, ConversationMarkerKind::Decision);
    assert_eq!(markers[0].created_by, ConversationMarkerAuthor::User);
    assert_eq!(markers[0].end_message_id.as_deref(), Some("message-3"));
}

#[test]
fn persisted_state_defaults_missing_conversation_markers() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Claude,
        Some("No Markers".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let mut encoded = persisted_state_value(&inner);
    encoded["sessions"][0]["session"]
        .as_object_mut()
        .expect("persisted session should be an object")
        .remove("markers");

    let loaded = state_inner_from_persisted_value(encoded).expect("persisted state should load");
    assert!(loaded.sessions[0].session.markers.is_empty());
}

#[test]
fn persisted_state_maps_unknown_conversation_marker_kind_to_custom() {
    let mut inner = StateInner::new();
    let record = inner.create_session(
        Agent::Codex,
        Some("Marked".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let session_id = record.session.id.clone();
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    inner.sessions[index]
        .session
        .markers
        .push(ConversationMarker {
            id: "marker-legacy".to_owned(),
            session_id,
            kind: ConversationMarkerKind::Custom,
            name: "Legacy marker".to_owned(),
            body: None,
            color: "#94a3b8".to_owned(),
            message_id: "message-1".to_owned(),
            message_index_hint: 0,
            end_message_id: None,
            end_message_index_hint: None,
            created_at: "2026-05-01 10:00:00".to_owned(),
            updated_at: "2026-05-01 10:00:00".to_owned(),
            created_by: ConversationMarkerAuthor::System,
        });
    let mut encoded = persisted_state_value(&inner);
    encoded["sessions"][0]["session"]["markers"][0]["kind"] =
        Value::String("obsoleteKind".to_owned());

    let loaded = state_inner_from_persisted_value(encoded).expect("persisted state should load");
    assert_eq!(
        loaded.sessions[0].session.markers[0].kind,
        ConversationMarkerKind::Custom
    );
}

// Builds an `AppState` like `test_app_state` but with a LIVE
// persist channel receiver so the caller can observe
// `PersistRequest` signals. The default `test_app_state` drops
// the receiver on construction so every `persist_tx.send(...)`
// returns `Err(Disconnected)` and tests automatically take the
// synchronous SQLite fallback path, which hides whether a code path
// correctly routes async.
fn test_app_state_with_live_persist_channel() -> (AppState, mpsc::Receiver<PersistRequest>) {
    let (persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();
    let test_temp_root = Arc::new(TestTempRoot::create("termal-test-state"));
    let persistence_path = test_temp_root.path().join("termal.sqlite");
    let mailbox_store = Arc::new(MailboxStore::disabled_for_tests());
    // Same fd-cascade rule as the mailbox store: retained test AppStates must
    // not hold real SQLite connections.
    let coordination_board_store = Arc::new(CoordinationBoardStore::disabled_for_tests());
    let state = AppState {
        server_instance_id: Uuid::new_v4().to_string(),
        default_workdir: "/tmp".to_owned(),
        local_http_base_url: Arc::new(Mutex::new(None)),
        persistence_path: Arc::new(persistence_path),
        mailbox_store,
        coordination_board_store,
        orchestrator_templates_path: Arc::new(test_temp_root.path().join("orchestrators.json")),
        orchestrator_templates_lock: Arc::new(Mutex::new(())),
        review_documents_lock: Arc::new(Mutex::new(())),
        state_events: broadcast::channel(16).0,
        delta_events: broadcast::channel(16).0,
        file_events: broadcast::channel(16).0,
        file_events_revision: Arc::new(AtomicU64::new(0)),
        persist_tx,
        // Live persist channel for the test; the worker thread is not
        // spawned by this constructor, so there's no JoinHandle to track.
        persist_thread_handle: Arc::new(Mutex::new(None)),
        persist_worker_alive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        shutdown_signal_tx: Arc::new(tokio::sync::watch::channel(false).0),
        state_broadcast_mailbox: None,
        telegram_relay_runtime: Arc::new(Mutex::new(TelegramRelayRuntime::default())),
        shared_codex_runtime: Arc::new(Mutex::new(None)),
        shared_codex_exit_claims: Arc::new(Mutex::new(HashSet::new())),
        agent_runtime_spawning_enabled: false,
        test_acp_runtime_overrides: Arc::new(Mutex::new(Vec::new())),
        test_agent_setup_failures: Arc::new(Mutex::new(Vec::new())),
        agent_readiness_cache: Arc::new(RwLock::new(fresh_agent_readiness_cache("/tmp"))),
        agent_readiness_refresh_lock: Arc::new(Mutex::new(())),
        remote_registry: test_remote_registry(),
        remote_sse_fallback_resynced_revision: Arc::new(Mutex::new(HashMap::new())),
        remote_delta_replay_cache: Arc::new(Mutex::new(RemoteDeltaReplayCache::default())),
        remote_delta_hydrations_in_flight: Arc::new(Mutex::new(HashSet::new())),
        remote_lifecycle_actions_in_flight: Arc::new(Mutex::new(HashSet::new())),
        terminal_local_command_semaphore: Arc::new(tokio::sync::Semaphore::new(
            TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT,
        )),
        terminal_remote_command_semaphore: Arc::new(tokio::sync::Semaphore::new(
            TERMINAL_REMOTE_COMMAND_CONCURRENCY_LIMIT,
        )),
        stopping_orchestrator_ids: Arc::new(Mutex::new(HashSet::new())),
        stopping_orchestrator_session_ids: Arc::new(Mutex::new(HashMap::new())),
        inner: Arc::new(StateMutex::new(StateInner::new())),
        test_temp_root: Some(test_temp_root),
    };
    (state, persist_rx)
}

#[test]
fn persist_worker_retry_state_doubles_and_resets_backoff() {
    let mut retry_state = PersistWorkerRetryState::default();

    let failure: Result<()> = Err(anyhow!("injected persist failure"));
    retry_state.record_result(&failure);
    assert!(retry_state.retry_after_failure);
    assert_eq!(
        retry_state.retry_delay,
        PERSIST_RETRY_SEED_DELAY * 2,
        "first failure should arm the first retry wait"
    );

    retry_state.record_result(&Ok(()));
    assert!(!retry_state.retry_after_failure);
    assert_eq!(
        retry_state.retry_delay, PERSIST_RETRY_SEED_DELAY,
        "successful retry should reset the next failure to the baseline backoff"
    );
}

#[test]
fn persist_worker_shutdown_waits_for_primary_durability_but_can_defer_cleanup() {
    let mut retry_state = PersistWorkerRetryState::default();

    let failure: Result<()> = Err(anyhow!("injected shutdown persist failure"));
    retry_state.record_result(&failure);
    assert!(
        !retry_state.should_exit_after_tick(true),
        "shutdown should keep retrying after a failed final persist tick"
    );
    assert!(
        !retry_state.should_exit_after_tick(false),
        "non-shutdown ticks should never exit the worker"
    );

    retry_state.record_result(&Ok(()));
    assert!(
        retry_state.should_exit_after_tick(true),
        "shutdown should exit once durability is confirmed"
    );
}

#[test]
fn persist_worker_retry_wait_times_out_without_new_delta() {
    let (_persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();
    let retry_state = PersistWorkerRetryState {
        retry_after_failure: true,
        retry_delay: Duration::from_millis(1),
    };

    assert_eq!(
        retry_state.wait_for_next_tick(&persist_rx),
        PersistWorkerWaitOutcome::Process,
        "timeout while the channel is still connected should trigger a retry tick"
    );
}

#[test]
fn persist_worker_retry_wait_accepts_new_delta_during_backoff() {
    let (persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();
    let retry_state = PersistWorkerRetryState {
        retry_after_failure: true,
        retry_delay: Duration::from_secs(30),
    };
    persist_tx
        .send(PersistRequest::Delta)
        .expect("test persist signal should send");

    assert_eq!(
        retry_state.wait_for_next_tick(&persist_rx),
        PersistWorkerWaitOutcome::Process,
        "new persist signals during backoff should wake the worker immediately"
    );
}

#[test]
fn persist_worker_retry_wait_observes_shutdown_during_backoff() {
    let (persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();
    drop(persist_tx);
    let retry_state = PersistWorkerRetryState {
        retry_after_failure: true,
        retry_delay: Duration::from_secs(30),
    };

    assert_eq!(
        retry_state.wait_for_next_tick(&persist_rx),
        PersistWorkerWaitOutcome::Exit,
        "disconnected retry wait should stop the worker instead of spinning"
    );
}

#[test]
fn persist_worker_wait_observes_explicit_shutdown_signal() {
    let (persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();
    persist_tx
        .send(PersistRequest::Shutdown)
        .expect("explicit shutdown signal should send");
    let retry_state = PersistWorkerRetryState::default();

    // The wait must distinguish a graceful shutdown signal from a
    // disconnected channel: shutdown still wants one final drain pass
    // (so the in-flight commit reaches SQLite) before the loop exits,
    // while disconnect aborts immediately. See `app_boot.rs`'s persist
    // loop for the corresponding `should_exit_after_tick` handling.
    assert_eq!(
        retry_state.wait_for_next_tick(&persist_rx),
        PersistWorkerWaitOutcome::Shutdown,
        "explicit shutdown signal must be reported as Shutdown, not Exit",
    );
}

#[test]
fn persist_worker_wait_observes_shutdown_during_retry_backoff() {
    let (persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();
    persist_tx
        .send(PersistRequest::Shutdown)
        .expect("explicit shutdown signal should send");
    let retry_state = PersistWorkerRetryState {
        retry_after_failure: true,
        retry_delay: Duration::from_secs(30),
    };

    assert_eq!(
        retry_state.wait_for_next_tick(&persist_rx),
        PersistWorkerWaitOutcome::Shutdown,
        "shutdown during a retry backoff should still drain one final tick before exit",
    );
}

#[test]
fn coordination_cleanup_retry_backoff_ignores_process_wakes_but_observes_shutdown() {
    let (cleanup_tx, cleanup_rx) = mpsc::channel::<CoordinationCleanupRequest>();
    let mut retry_state = CoordinationCleanupRetryState::default();
    retry_state.record_pending(true);
    cleanup_tx
        .send(CoordinationCleanupRequest::Process)
        .expect("redundant cleanup wake should send");
    cleanup_tx
        .send(CoordinationCleanupRequest::Shutdown)
        .expect("cleanup shutdown should send");

    assert_eq!(
        retry_state.wait_for_next_tick(&cleanup_rx),
        CoordinationCleanupWaitOutcome::Shutdown,
        "ordinary Process wakes must not shorten cleanup backoff, while shutdown must interrupt it"
    );
}

#[test]
fn shutdown_persist_blocking_is_idempotent_when_no_worker_handle() {
    // Test-only constructors don't spawn the persist worker thread, so
    // `persist_thread_handle` stays `None`. Calling
    // `shutdown_persist_blocking` must be a safe no-op the first time
    // (no thread to join) and on every subsequent call (the handle is
    // already taken / still `None`). The production caller in main.rs
    // is one-shot, but `AppState` is `Clone`-able and the handle is
    // shared — making the operation idempotent prevents a future
    // shutdown ordering bug from panicking.
    let (state, persist_rx) = test_app_state_with_live_persist_channel();
    state.shutdown_persist_blocking();
    assert!(
        state
            .persist_thread_handle
            .lock()
            .expect("persist handle mutex poisoned")
            .is_none(),
        "no-worker shutdown should leave the join handle absent",
    );
    assert!(
        !state.persist_worker_alive.load(Ordering::Acquire),
        "no-worker shutdown should publish the stopped worker state",
    );
    assert!(
        matches!(persist_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "no-worker shutdown should not enqueue a shutdown request",
    );
    state.shutdown_persist_blocking();
    assert!(
        state
            .persist_thread_handle
            .lock()
            .expect("persist handle mutex poisoned")
            .is_none(),
        "second no-worker shutdown should remain idempotent",
    );
    assert!(
        !state.persist_worker_alive.load(Ordering::Acquire),
        "second no-worker shutdown should keep the worker stopped",
    );
    assert!(
        matches!(persist_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "second no-worker shutdown should not enqueue a shutdown request",
    );
}

#[test]
fn concurrent_shutdown_waits_for_join_owner_before_publishing_stopped() {
    // Regression for the bug ledger entry "Concurrent shutdown callers can
    // flip `persist_worker_alive` before the join owner finishes". The first
    // caller takes the worker handle and blocks in `join()`. A concurrent
    // caller must block behind that full transition instead of seeing `None`
    // and publishing `alive == false` while the worker thread is still alive.
    let (persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();
    let (shutdown_seen_tx, shutdown_seen_rx) = mpsc::channel::<()>();
    let (release_worker_tx, release_worker_rx) = mpsc::channel::<()>();

    let worker = std::thread::Builder::new()
        .name("test-concurrent-persist-shutdown".to_owned())
        .spawn(move || {
            while let Ok(req) = persist_rx.recv() {
                if matches!(req, PersistRequest::Shutdown) {
                    let _ = shutdown_seen_tx.send(());
                    release_worker_rx
                        .recv()
                        .expect("test should release the blocked worker");
                    break;
                }
            }
        })
        .expect("test persist worker should spawn");

    let (state, _stale_rx) = test_app_state_with_live_persist_channel();
    let state = AppState {
        persist_tx: persist_tx.clone(),
        persist_thread_handle: Arc::new(Mutex::new(Some(worker))),
        ..state
    };

    let first = state.clone();
    let first_joiner = std::thread::spawn(move || {
        first.shutdown_persist_blocking();
    });

    shutdown_seen_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("first shutdown caller should signal the worker");
    assert!(
        state.persist_worker_alive.load(Ordering::Acquire),
        "alive must stay true while the join owner is still waiting",
    );

    let second = state.clone();
    let (second_done_tx, second_done_rx) = mpsc::channel::<()>();
    let second_joiner = std::thread::spawn(move || {
        second.shutdown_persist_blocking();
        let _ = second_done_tx.send(());
    });

    assert!(
        second_done_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "concurrent shutdown caller should block until the join owner finishes",
    );
    assert!(
        state.persist_worker_alive.load(Ordering::Acquire),
        "blocked concurrent shutdown must not publish stopped early",
    );

    release_worker_tx
        .send(())
        .expect("test should release worker join");
    first_joiner
        .join()
        .expect("first shutdown caller should not panic");
    second_joiner
        .join()
        .expect("second shutdown caller should not panic");
    second_done_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("second shutdown caller should return after join owner finishes");
    assert!(
        !state.persist_worker_alive.load(Ordering::Acquire),
        "alive should flip only after the worker has exited",
    );
}

#[test]
fn shutdown_persist_blocking_persists_delta_committed_while_joining_worker() {
    // Regression for the bug ledger entry "Post-shutdown persistence writes
    // still leave a post-collection-pre-join window". A delta-only mutation
    // can land while shutdown is waiting for the worker thread to exit. The
    // worker may already be past its final collection, so shutdown itself must
    // perform a final synchronized full-state persist after `join()`.
    let (persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();
    let (shutdown_seen_tx, shutdown_seen_rx) = mpsc::channel::<()>();
    let (release_worker_tx, release_worker_rx) = mpsc::channel::<()>();

    let worker = std::thread::Builder::new()
        .name("test-joining-persist-worker".to_owned())
        .spawn(move || {
            while let Ok(req) = persist_rx.recv() {
                if matches!(req, PersistRequest::Shutdown) {
                    let _ = shutdown_seen_tx.send(());
                    release_worker_rx
                        .recv()
                        .expect("test should release the blocked worker");
                    break;
                }
            }
        })
        .expect("test persist worker should spawn");

    let (state, _stale_rx) = test_app_state_with_live_persist_channel();
    let persistence_path = Arc::clone(&state.persistence_path);
    let state = AppState {
        persist_tx: persist_tx.clone(),
        persist_thread_handle: Arc::new(Mutex::new(Some(worker))),
        ..state
    };

    let session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let project = inner.create_project(
            Some("Join Persist Project".to_owned()),
            "/tmp".to_owned(),
            default_local_remote_id(),
        );
        inner
            .create_session(
                Agent::Claude,
                Some("Join Persist Session".to_owned()),
                "/tmp".to_owned(),
                Some(project.id),
                None,
            )
            .session
            .id
    };

    let shutdown_state = state.clone();
    let shutdown_joiner = std::thread::spawn(move || {
        shutdown_state.shutdown_persist_blocking();
    });

    shutdown_seen_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("shutdown should reach the worker before the late mutation");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let session_index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        inner
            .session_mut_by_index(session_index)
            .expect("test session should be mutable")
            .session
            .preview = "delta while shutdown is joining".to_owned();
        state
            .commit_delta_locked(&mut inner)
            .expect("late delta commit should succeed");
    }

    release_worker_tx
        .send(())
        .expect("test should release worker join");
    shutdown_joiner
        .join()
        .expect("shutdown caller should not panic");

    let persisted = load_state(&persistence_path)
        .expect("shutdown should persist final state")
        .expect("persisted state should exist");
    let persisted_session = persisted
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("late-mutated session should be persisted");
    assert_eq!(
        persisted_session.session.preview, "delta while shutdown is joining",
        "shutdown's final synchronized persist must include delta-only mutations that land while \
         the worker is joining",
    );

    let _ = fs::remove_file(&*persistence_path);
}

#[tokio::test]
async fn shutdown_signal_wakes_a_subscriber_registered_before_the_signal() {
    // Standard ordering: subscribe first, trigger second. The subscriber's
    // first `borrow_and_update()` reads the initial `false`, then it awaits
    // `changed()` which fires when production calls `trigger_shutdown_signal`.
    // This is the load-bearing invariant for graceful shutdown — without
    // it `with_graceful_shutdown` blocks forever on long-lived SSE streams.
    let state = test_app_state();
    let mut shutdown_rx = state.subscribe_shutdown_signal();
    let waiter = tokio::spawn(async move {
        // Mirror the production helper in `api_sse.rs::wait_for_shutdown_signal`:
        // returns immediately on the sticky-true case, otherwise loops until
        // the value flips.
        if *shutdown_rx.borrow_and_update() {
            return;
        }
        while shutdown_rx.changed().await.is_ok() {
            if *shutdown_rx.borrow_and_update() {
                return;
            }
        }
    });

    // Yield once so the spawned task has a chance to enter `changed().await`.
    tokio::task::yield_now().await;
    state.trigger_shutdown_signal();

    tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
        .await
        .expect("shutdown waiter must complete within the timeout")
        .expect("shutdown waiter task should not panic");
}

#[tokio::test]
async fn shutdown_signal_wakes_a_subscriber_registered_after_the_signal() {
    // The race that motivated switching from `Notify` to `watch`: an
    // `/api/events` request that begins handler setup AFTER Ctrl+C has
    // already fired must still observe the shutdown signal — otherwise
    // its loop runs forever and graceful shutdown blocks.
    //
    // Critically, this test uses NO in-test re-notify: it triggers
    // shutdown first, subscribes second, and the waiter is expected to
    // exit purely on the sticky `true` value the subscriber sees during
    // its initial `borrow_and_update()` pre-check. A `Notify`-based
    // implementation would HANG here because `notify_waiters` only wakes
    // currently-waiting tasks, and the `tokio::time::timeout` below
    // would fire and fail the test. See bugs.md "One-shot SSE shutdown
    // notification can be missed before waiter registration".
    let state = test_app_state();
    state.trigger_shutdown_signal();

    let mut shutdown_rx = state.subscribe_shutdown_signal();
    let waiter = tokio::spawn(async move {
        if *shutdown_rx.borrow_and_update() {
            return;
        }
        while shutdown_rx.changed().await.is_ok() {
            if *shutdown_rx.borrow_and_update() {
                return;
            }
        }
    });

    tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
        .await
        .expect(
            "shutdown waiter must complete within the timeout — the watch \
             channel's sticky semantics require the late subscriber to see \
             the prior trigger immediately",
        )
        .expect("shutdown waiter task should not panic");
}

#[tokio::test]
async fn shutdown_signal_is_idempotent_and_durable() {
    // Repeated `trigger_shutdown_signal()` calls are safe (no-op after the
    // first). Subscribers registered at any time after the first trigger
    // see the sticky `true` value. This is what lets the production
    // graceful-shutdown future call `trigger_shutdown_signal()` exactly
    // once without coordinating with the unknown number of `/api/events`
    // streams that may be concurrently subscribing.
    let state = test_app_state();
    state.trigger_shutdown_signal();
    state.trigger_shutdown_signal();
    state.trigger_shutdown_signal();

    for _ in 0..3 {
        let mut rx = state.subscribe_shutdown_signal();
        assert!(
            *rx.borrow_and_update(),
            "every late subscriber must see the sticky shutdown value",
        );
    }
}

#[test]
fn shutdown_persist_blocking_drains_and_joins_a_real_worker() {
    // Spawn a worker thread that mirrors the production `app_boot.rs`
    // loop semantics. Sending a Delta enqueues work; sending Shutdown
    // signals the loop to perform one final drain pass and exit.
    // `shutdown_persist_blocking` must wait until the thread actually
    // exits, so a subsequent `handle.join()` would not race with an
    // in-flight SQLite commit. This is the contract that closes the
    // bugs.md "Server restart without browser refresh can lose the
    // last streamed message" durability window.
    let (persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();
    let drained_ticks = Arc::new(AtomicU64::new(0));
    let drained_ticks_for_thread = Arc::clone(&drained_ticks);

    let worker = std::thread::Builder::new()
        .name("test-persist-shutdown-loop".to_owned())
        .spawn(move || {
            let mut retry_state = PersistWorkerRetryState::default();
            loop {
                let outcome = retry_state.wait_for_next_tick(&persist_rx);
                if matches!(outcome, PersistWorkerWaitOutcome::Exit) {
                    break;
                }
                let mut should_exit_after_tick =
                    matches!(outcome, PersistWorkerWaitOutcome::Shutdown);
                while let Ok(req) = persist_rx.try_recv() {
                    if matches!(req, PersistRequest::Shutdown) {
                        should_exit_after_tick = true;
                    }
                }
                drained_ticks_for_thread.fetch_add(1, Ordering::SeqCst);
                retry_state.record_result(&Ok(()));
                if should_exit_after_tick {
                    break;
                }
            }
        })
        .expect("test persist worker should spawn");

    let (state, _stale_rx) = test_app_state_with_live_persist_channel();
    let state = AppState {
        persist_tx: persist_tx.clone(),
        persist_thread_handle: Arc::new(Mutex::new(Some(worker))),
        ..state
    };

    persist_tx
        .send(PersistRequest::Delta)
        .expect("delta enqueue should succeed");
    state.shutdown_persist_blocking();

    // The worker exited cleanly: the handle was taken (so a second
    // shutdown is a no-op) and the thread processed at least one tick
    // (the queued Delta + the Shutdown drain pass). On a hard kill
    // before this fix, the queued Delta would never have been
    // processed.
    assert!(
        drained_ticks.load(Ordering::SeqCst) >= 1,
        "worker should have drained at least the queued Delta before exit",
    );
    state.shutdown_persist_blocking();
}

#[test]
fn commit_delta_locked_after_shutdown_falls_back_to_synchronous_persist() {
    // Regression for the bug ledger entry "Persist shutdown drain can run
    // before background mutation sources are quiesced". The HTTP server's
    // graceful-shutdown phase only waits for in-flight HTTP handlers, but
    // background agent runtime threads, remote SSE bridges, and the
    // orchestrator transition resumer can still hold `AppState` clones
    // and call `commit_delta_locked` AFTER `shutdown_persist_blocking`
    // has drained and exited the worker. `commit_delta_locked` doesn't
    // send its own `PersistRequest::Delta`; under normal operation the
    // worker drains the bumped mutation_stamps on a subsequent persist
    // signal, but post-shutdown there is no worker. Without this
    // synchronous fallback, those final mutations are kept only in
    // memory and lost when the process exits.
    let unique_suffix = Uuid::new_v4();
    let project_root =
        std::env::temp_dir().join(format!("termal-post-shutdown-commit-root-{unique_suffix}"));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let state_root =
        std::env::temp_dir().join(format!("termal-post-shutdown-commit-state-{unique_suffix}"));
    let _state_temp_root = TestTempRoot::own(state_root.clone());
    fs::create_dir_all(&state_root).expect("state root should exist");
    let persistence_path = state_root.join("termal.sqlite");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let durable_session_id;
    {
        let state = AppState::new_with_paths(
            project_root.to_string_lossy().into_owned(),
            persistence_path.clone(),
            orchestrator_templates_path.clone(),
        )
        .expect("initial state should boot");

        let project_id =
            create_test_project(&state, &project_root, "Post-Shutdown Commit Regression");
        durable_session_id =
            create_test_project_session(&state, Agent::Claude, &project_id, &project_root);

        // Run the production graceful-shutdown drain. After this returns
        // the worker is gone; subsequent commits cannot wake it.
        state.shutdown_persist_blocking();

        // Simulate a background mutation source (agent runtime / remote
        // bridge / orchestrator resumer) committing AFTER the persist
        // worker has exited. Bump the session's mutation_stamp via the
        // standard mutator path so the commit looks production-shaped,
        // then route through `commit_delta_locked` — exactly the pattern
        // a Claude/Codex stdio thread uses for streaming text chunks.
        // Without the post-shutdown synchronous fallback, the bumped
        // stamp would be queued for a worker that never returns, and
        // the in-memory mutation would be lost on the next reload.
        let post_shutdown_revision = {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let session_index = inner
                .find_session_index(&durable_session_id)
                .expect("session committed pre-shutdown should exist");
            // `session_mut_by_index` stamps the session's mutation_stamp
            // and is what real runtime threads would use.
            inner
                .session_mut_by_index(session_index)
                .expect("session_mut_by_index should return the existing session")
                .session
                .preview = "post-shutdown delta".to_owned();
            state
                .commit_delta_locked(&mut inner)
                .expect("commit_delta_locked must succeed even after persist shutdown")
        };
        assert!(
            post_shutdown_revision >= 1,
            "commit_delta_locked must continue to bump the revision after shutdown",
        );
    }

    // Reload from the same path and verify the post-shutdown delta is
    // durable. The synchronous fallback writes the full state, so the
    // session's preview should be the post-shutdown value.
    let restarted = AppState::new_with_paths(
        project_root.to_string_lossy().into_owned(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("restarted state should boot from the persisted file");

    let reloaded_inner = restarted.inner.lock().expect("state mutex poisoned");
    let session_index = reloaded_inner
        .find_session_index(&durable_session_id)
        .expect("session must reload");
    let preview = reloaded_inner.sessions[session_index]
        .session
        .preview
        .clone();
    drop(reloaded_inner);
    restarted.shutdown_persist_blocking();

    assert_eq!(
        preview, "post-shutdown delta",
        "the mutation that landed after `shutdown_persist_blocking` must reach disk via the \
         synchronous fallback path; otherwise the bug ledger entry \"Persist shutdown drain \
         can run before background mutation sources are quiesced\" remains open",
    );

    let _ = fs::remove_dir_all(&state_root);
    let _ = fs::remove_dir_all(&project_root);
}

#[test]
fn graceful_shutdown_drain_persists_final_mutation_across_reload() {
    // End-to-end durability regression for the bug ledger entry "Server
    // restart without browser refresh can lose the last streamed message"
    // and the follow-up gap "Graceful-shutdown durability regression does
    // not reload persisted state". This test exercises the REAL
    // production-shaped path: `AppState::new_with_paths` spawns the actual
    // background persist thread, `commit_locked` triggers a real
    // `PersistRequest::Delta` signal, `shutdown_persist_blocking` runs the
    // production drain-and-join, and a fresh `AppState::new_with_paths`
    // against the same persistence path verifies the mutation survived.
    //
    // Without this test, the prior unit-level coverage of `wait_for_next_tick`
    // and the fake-loop integration test could pass even if the real worker
    // failed to write the final delta or a restarted `AppState` silently
    // dropped the last record.
    let unique_suffix = Uuid::new_v4();
    let project_root =
        std::env::temp_dir().join(format!("termal-graceful-shutdown-root-{unique_suffix}"));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let state_root =
        std::env::temp_dir().join(format!("termal-graceful-shutdown-state-{unique_suffix}"));
    let _state_temp_root = TestTempRoot::own(state_root.clone());
    fs::create_dir_all(&state_root).expect("state root should exist");
    let persistence_path = state_root.join("termal.sqlite");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let durable_session_ids;
    {
        let state = AppState::new_with_paths(
            project_root.to_string_lossy().into_owned(),
            persistence_path.clone(),
            orchestrator_templates_path.clone(),
        )
        .expect("initial state should boot");

        // Commit a burst through the production path. Each `commit_locked`
        // bumps the revision and signals `PersistRequest::Delta` to the
        // background worker, which now uses the same SQLite delta path in
        // tests and production. A burst keeps this test sensitive to the
        // shutdown drain ordering instead of relying on a single mutation
        // that the worker might happen to flush before shutdown begins.
        let project = create_test_project(&state, &project_root, "Graceful Shutdown Durability");
        durable_session_ids = (0..50)
            .map(|_| create_test_project_session(&state, Agent::Claude, &project, &project_root))
            .collect::<Vec<_>>();

        // The commit's `Delta` signal may or may not have been processed
        // by the worker before this point — that's the durability window
        // the graceful drain closes. `shutdown_persist_blocking` sends
        // `PersistRequest::Shutdown` and joins; the worker's loop drains
        // every queued Delta + the Shutdown signal, runs one final tick
        // that captures the whole burst, and only then exits. After
        // the join returns, SQLite on disk MUST contain the sessions.
        state.shutdown_persist_blocking();
    }

    // Reload from the same path. The reborn `AppState` must observe the
    // sessions that the prior process committed just before shutdown.
    let restarted = AppState::new_with_paths(
        project_root.to_string_lossy().into_owned(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("restarted state should boot from the persisted file");

    let reloaded_inner = restarted.inner.lock().expect("state mutex poisoned");
    let missing_session_ids = durable_session_ids
        .iter()
        .filter(|session_id| reloaded_inner.find_session_index(session_id).is_none())
        .collect::<Vec<_>>();
    assert!(
        missing_session_ids.is_empty(),
        "every session committed just before graceful shutdown must be reloadable from the \
         persistence file — without `shutdown_persist_blocking`'s final drain, the \
         `PersistRequest::Delta` queued by `commit_locked` would be lost when the worker \
         exited and the next process boot would never see the full burst; missing: {missing_session_ids:?}",
    );
    drop(reloaded_inner);
    restarted.shutdown_persist_blocking();

    // Best-effort cleanup; tests that fail mid-flight intentionally leave
    // the temp files in place for postmortem inspection.
    let _ = fs::remove_dir_all(&state_root);
    let _ = fs::remove_dir_all(&project_root);
}

#[test]
fn persist_delta_restore_requeues_only_drained_explicit_tombstones() {
    let mut inner = StateInner::new();
    let removed_id = inner
        .create_session(
            Agent::Claude,
            Some("removed".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let hidden_id = inner
        .create_session(
            Agent::Claude,
            Some("hidden".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let hidden_index = inner
        .find_session_index(&hidden_id)
        .expect("hidden session should exist");
    inner
        .session_mut_by_index(hidden_index)
        .expect("hidden session should be mutable")
        .hidden = true;
    let removed_index = inner
        .find_session_index(&removed_id)
        .expect("removed session should exist");
    inner.remove_session_at(removed_index);

    let delta = inner.collect_persist_delta(0);

    assert_eq!(delta.drained_explicit_tombstones, vec![removed_id.clone()]);
    assert_eq!(
        delta
            .removed_session_ids
            .iter()
            .filter(|id| id.as_str() == removed_id.as_str())
            .count(),
        1
    );
    assert_eq!(
        delta
            .removed_session_ids
            .iter()
            .filter(|id| id.as_str() == hidden_id.as_str())
            .count(),
        1
    );
    assert!(inner.removed_session_ids.is_empty());

    inner.restore_drained_explicit_tombstones(&delta.drained_explicit_tombstones);
    inner.restore_drained_explicit_tombstones(&delta.drained_explicit_tombstones);
    assert_eq!(inner.removed_session_ids, vec![removed_id.clone()]);

    let retry_delta = inner.collect_persist_delta(0);

    assert_eq!(
        retry_delta.drained_explicit_tombstones,
        vec![removed_id.clone()]
    );
    assert_eq!(
        retry_delta
            .removed_session_ids
            .iter()
            .filter(|id| id.as_str() == hidden_id.as_str())
            .count(),
        1,
        "hidden-session deletes should be regenerated, not restored as explicit tombstones"
    );
}

fn sqlite_row_json(path: &FsPath, table: &str, id: &str) -> Option<String> {
    let connection = rusqlite::Connection::open(path).expect("sqlite state should open");
    let sql = format!("SELECT value_json FROM {table} WHERE id = ?1");
    match connection.query_row(&sql, rusqlite::params![id], |row| row.get(0)) {
        Ok(value) => Some(value),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(err) => panic!("sqlite row query should succeed: {err}"),
    }
}

fn sqlite_table_ids(path: &FsPath, table: &str) -> Vec<String> {
    let connection = rusqlite::Connection::open(path).expect("sqlite state should open");
    let sql = format!("SELECT id FROM {table} ORDER BY id");
    let mut statement = connection
        .prepare(&sql)
        .expect("sqlite id query should prepare");
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("sqlite id query should run");
    rows.map(|row| row.expect("sqlite id row should read"))
        .collect()
}

fn make_persist_test_delegation(
    id: &str,
    parent_session_id: &str,
    child_session_id: &str,
) -> DelegationRecord {
    DelegationRecord {
        id: id.to_owned(),
        parent_session_id: parent_session_id.to_owned(),
        child_session_id: child_session_id.to_owned(),
        mode: DelegationMode::Reviewer,
        status: DelegationStatus::Running,
        title: "Persisted Delegation".to_owned(),
        prompt: "/review-code".to_owned(),
        cwd: "/tmp".to_owned(),
        agent: Agent::Codex,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: Some(stamp_now()),
        completed_at: None,
        result: None,
        submitted_review_result: None,
        post_submission_transport_error: None,
        review_result_recovery_probe_attempt: None,
        review_result_recovery_error: None,
        review_result_schema_version: None,
        review_result_submission_attempt: 0,
    }
}

#[test]
fn sqlite_persist_connection_cache_reuses_matching_connection_until_invalidated() {
    let state_root =
        std::env::temp_dir().join(format!("termal-sqlite-cache-reuse-{}", Uuid::new_v4()));
    let _state_temp_root = TestTempRoot::own(state_root.clone());
    fs::create_dir_all(&state_root).expect("state root should exist");
    let path = state_root.join("termal.sqlite");
    let mut cache = SqlitePersistConnectionCache::new();

    {
        let connection = cache
            .connection_for(&path)
            .expect("cached sqlite connection should open");
        connection
            .execute("CREATE TEMP TABLE cache_probe(value TEXT)", [])
            .expect("connection-local temp table should be created");
        connection
            .execute("INSERT INTO cache_probe(value) VALUES('reused')", [])
            .expect("connection-local temp row should be inserted");
    }
    {
        let connection = cache
            .connection_for(&path)
            .expect("matching path should reuse cached sqlite connection");
        let value: String = connection
            .query_row("SELECT value FROM cache_probe", [], |row| row.get(0))
            .expect("connection-local temp table should survive cache reuse");
        assert_eq!(value, "reused");
    }

    cache.invalidate();

    {
        let connection = cache
            .connection_for(&path)
            .expect("invalidated cache should reopen sqlite connection");
        let error = connection
            .query_row("SELECT value FROM cache_probe", [], |row| {
                row.get::<_, String>(0)
            })
            .expect_err("fresh connection should not see the prior temp table");
        assert!(
            error.to_string().contains("no such table"),
            "unexpected cache invalidation error: {error}"
        );
    }

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn sqlite_boot_load_hands_its_validated_connection_to_the_persist_cache() {
    let state_root = PersistTestRoot::new("boot-connection-handoff");
    let path = state_root.path().join("termal.sqlite");
    persist_state(&path, &StateInner::new()).expect("initial state should persist");

    let (loaded, boot_connection) =
        load_state_for_boot(&path).expect("boot state and connection should load");
    assert!(loaded.is_some(), "persisted state should be present");
    let boot_connection = boot_connection.expect("existing state should retain its connection");
    boot_connection
        .execute("CREATE TEMP TABLE boot_connection_probe(value TEXT)", [])
        .expect("connection-local boot probe should be created");
    boot_connection
        .execute(
            "INSERT INTO boot_connection_probe(value) VALUES('retained')",
            [],
        )
        .expect("connection-local boot probe should be populated");

    let path_for_worker = path.clone();
    let value = std::thread::spawn(move || {
        let mut cache = SqlitePersistConnectionCache::from_validated_connection(Some((
            path_for_worker.clone(),
            boot_connection,
        )));
        cache
            .connection_for(&path_for_worker)
            .expect("persist cache should adopt the boot connection")
            .query_row("SELECT value FROM boot_connection_probe", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("the adopted connection should retain connection-local state")
    })
    .join()
    .expect("persist-connection handoff worker should not panic");
    assert_eq!(value, "retained");
}

#[test]
fn coordination_bootstrap_hands_its_connection_to_the_mailbox_store() {
    let state_root = PersistTestRoot::new("coordination-connection-handoff");
    let path = state_root.path().join("coordination.sqlite");
    let connection =
        bootstrap_coordination_database(&path).expect("coordination database should bootstrap");
    connection
        .execute("CREATE TEMP TABLE coordination_boot_probe(value TEXT)", [])
        .expect("connection-local coordination probe should be created");
    connection
        .execute(
            "INSERT INTO coordination_boot_probe(value) VALUES('retained')",
            [],
        )
        .expect("connection-local coordination probe should be populated");

    let store = MailboxStore::from_validated_connection(
        &path,
        connection,
        MAILBOX_WRITER_ADMISSION_TIMEOUT,
    )
    .expect("mailbox store should adopt the bootstrap connection");
    let value: String = store
        .connection()
        .expect("mailbox connection should remain available")
        .query_row("SELECT value FROM coordination_boot_probe", [], |row| {
            row.get(0)
        })
        .expect("the mailbox store should retain bootstrap connection-local state");
    assert_eq!(value, "retained");
}

#[test]
fn sqlite_delta_upserts_only_changed_session_rows_and_removes_hidden_or_deleted_rows() {
    let state_root = std::env::temp_dir().join(format!("termal-sqlite-delta-{}", Uuid::new_v4()));
    let _state_temp_root = TestTempRoot::own(state_root.clone());
    fs::create_dir_all(&state_root).expect("state root should exist");
    let path = state_root.join("termal.sqlite");
    let mut inner = StateInner::new();
    let changed_id = inner
        .create_session(
            Agent::Claude,
            Some("Changed".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let unchanged_id = inner
        .create_session(
            Agent::Claude,
            Some("Unchanged".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let hidden_id = inner
        .create_session(
            Agent::Claude,
            Some("Hidden".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let deleted_id = inner
        .create_session(
            Agent::Claude,
            Some("Deleted".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    persist_state(&path, &inner).expect("initial sqlite state should persist");
    let unchanged_before =
        sqlite_row_json(&path, "sessions", &unchanged_id).expect("unchanged row should exist");
    let watermark = inner.last_mutation_stamp;

    let changed_index = inner
        .find_session_index(&changed_id)
        .expect("changed session should exist");
    inner
        .session_mut_by_index(changed_index)
        .expect("changed session should be mutable")
        .session
        .preview = "Targeted changed preview".to_owned();
    let hidden_index = inner
        .find_session_index(&hidden_id)
        .expect("hidden session should exist");
    inner
        .session_mut_by_index(hidden_index)
        .expect("hidden session should be mutable")
        .hidden = true;
    let deleted_index = inner
        .find_session_index(&deleted_id)
        .expect("deleted session should exist");
    inner.remove_session_at(deleted_index);

    let delta = inner.collect_persist_delta(watermark);
    assert_eq!(delta.changed_sessions.len(), 1);
    assert_eq!(delta.removed_session_ids.len(), 2);
    let mut cache = SqlitePersistConnectionCache::new();
    persist_delta_via_cache(&mut cache, &path, &delta).expect("delta should persist");

    assert_eq!(
        sqlite_table_ids(&path, "sessions"),
        vec![changed_id.clone(), unchanged_id.clone()]
    );
    let changed_row =
        sqlite_row_json(&path, "sessions", &changed_id).expect("changed row should remain");
    let changed_value: Value =
        serde_json::from_str(&changed_row).expect("changed row should decode as json");
    assert_eq!(
        changed_value["session"]["preview"],
        Value::String("Targeted changed preview".to_owned())
    );
    assert_eq!(
        sqlite_row_json(&path, "sessions", &unchanged_id),
        Some(unchanged_before),
        "unchanged session row should not be rewritten by a targeted delta"
    );
    assert!(sqlite_row_json(&path, "sessions", &hidden_id).is_none());
    assert!(sqlite_row_json(&path, "sessions", &deleted_id).is_none());

    let loaded = load_state(&path)
        .expect("sqlite state should load")
        .expect("sqlite state should exist");
    assert!(loaded.find_session_index(&changed_id).is_some());
    assert!(loaded.find_session_index(&unchanged_id).is_some());
    assert!(loaded.find_session_index(&hidden_id).is_none());
    assert!(loaded.find_session_index(&deleted_id).is_none());

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn invalid_in_memory_remote_identity_isolated_from_full_and_delta_persistence() {
    let state_root = PersistTestRoot::new("invalid-in-memory-remote-identity");
    let path = state_root.path().join("termal.sqlite");
    let mut inner = StateInner::new();
    let invalid_id = inner
        .create_session(
            Agent::Claude,
            Some("Invalid identity".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let healthy_id = inner
        .create_session(
            Agent::Claude,
            Some("Healthy sibling".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    persist_state(&path, &inner).expect("valid baseline should persist");
    let invalid_baseline =
        sqlite_row_json(&path, "sessions", &invalid_id).expect("baseline row should exist");

    let invalid_index = inner
        .find_session_index(&invalid_id)
        .expect("invalid identity session should exist");
    {
        let invalid_record = inner
            .session_mut_by_index(invalid_index)
            .expect("invalid identity session should be mutable");
        invalid_record.remote_id = Some("remote-without-session".to_owned());
        assert!(
            !invalid_record.is_local_session() && !invalid_record.is_remote_proxy(),
            "a partial remote identity must be neither local nor remote"
        );
        assert!(
            invalid_record.remote_proxy_identity().is_err(),
            "the partial identity must retain an explicit validation error"
        );
    }
    let healthy_index = inner
        .find_session_index(&healthy_id)
        .expect("healthy sibling should exist");
    inner
        .session_mut_by_index(healthy_index)
        .expect("healthy sibling should be mutable")
        .session
        .preview = "healthy full update".to_owned();

    persist_state(&path, &inner).expect("one invalid session must not abort full persistence");
    assert_eq!(
        sqlite_row_json(&path, "sessions", &invalid_id),
        Some(invalid_baseline.clone()),
        "full persistence must preserve the invalid session's last good row"
    );
    let healthy_full: Value = serde_json::from_str(
        &sqlite_row_json(&path, "sessions", &healthy_id)
            .expect("healthy sibling row should remain"),
    )
    .expect("healthy sibling row should decode");
    assert_eq!(
        healthy_full["session"]["preview"],
        Value::String("healthy full update".to_owned())
    );

    let watermark = inner.last_mutation_stamp;
    inner
        .session_mut_by_index(invalid_index)
        .expect("invalid identity session should still be mutable")
        .remote_id = Some("remote-still-without-session".to_owned());
    inner
        .session_mut_by_index(healthy_index)
        .expect("healthy sibling should still be mutable")
        .session
        .preview = "healthy delta update".to_owned();
    let delta = inner.collect_persist_delta(watermark);
    assert_eq!(
        delta.changed_sessions.len(),
        2,
        "both changed sessions should reach the persistence boundary"
    );
    let mut cache = SqlitePersistConnectionCache::new();
    let persisted_session_ids = persist_delta_via_cache(&mut cache, &path, &delta)
        .expect("one invalid session must not abort delta persistence");
    assert_eq!(
        persisted_session_ids,
        vec![healthy_id.clone()],
        "only the successfully serialized sibling may be reported as persisted"
    );
    assert_eq!(
        sqlite_row_json(&path, "sessions", &invalid_id),
        Some(invalid_baseline),
        "delta persistence must preserve the invalid session's last good row"
    );
    let healthy_delta: Value = serde_json::from_str(
        &sqlite_row_json(&path, "sessions", &healthy_id)
            .expect("healthy sibling row should remain after delta"),
    )
    .expect("healthy sibling delta row should decode");
    assert_eq!(
        healthy_delta["session"]["preview"],
        Value::String("healthy delta update".to_owned())
    );

    let loaded = load_state(&path)
        .expect("isolated state should reload")
        .expect("isolated state should exist");
    assert!(loaded.find_session_index(&invalid_id).is_some());
    assert!(loaded.find_session_index(&healthy_id).is_some());
}

#[test]
fn sqlite_delta_upserts_changed_delegation_rows_and_removes_deleted_rows() {
    let state_root =
        std::env::temp_dir().join(format!("termal-sqlite-delegation-delta-{}", Uuid::new_v4()));
    let _state_temp_root = TestTempRoot::own(state_root.clone());
    fs::create_dir_all(&state_root).expect("state root should exist");
    let path = state_root.join("termal.sqlite");
    let mut inner = StateInner::new();
    let parent_id = inner
        .create_session(
            Agent::Codex,
            Some("Parent".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let child_id = inner
        .create_session(
            Agent::Codex,
            Some("Child".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let changed_id = "delegation-delta-changed";
    let unchanged_id = "delegation-delta-unchanged";
    let deleted_id = "delegation-delta-deleted";
    inner.delegations.push(make_persist_test_delegation(
        changed_id, &parent_id, &child_id,
    ));
    inner.delegations.push(make_persist_test_delegation(
        unchanged_id,
        &parent_id,
        &child_id,
    ));
    inner.delegations.push(make_persist_test_delegation(
        deleted_id, &parent_id, &child_id,
    ));
    persist_state(&path, &inner).expect("initial sqlite state should persist");
    let unchanged_before =
        sqlite_row_json(&path, "delegations", unchanged_id).expect("unchanged row should exist");
    let watermark = inner.last_mutation_stamp;

    let changed_index = inner
        .find_delegation_index(changed_id)
        .expect("changed delegation should exist");
    inner.delegations[changed_index].title = "Changed Delegation Row".to_owned();
    inner.mark_delegation_mutated(changed_index);
    let deleted_index = inner
        .find_delegation_index(deleted_id)
        .expect("deleted delegation should exist");
    inner.remove_delegation_at(deleted_index);

    let delta = inner.collect_persist_delta(watermark);
    assert_eq!(
        delta
            .changed_delegations
            .as_ref()
            .expect("changed delegation should be persisted")
            .iter()
            .map(|delegation| delegation.id.as_str())
            .collect::<Vec<_>>(),
        vec![changed_id]
    );
    assert_eq!(delta.removed_delegation_ids, vec![deleted_id.to_owned()]);
    let mut cache = SqlitePersistConnectionCache::new();
    persist_delta_via_cache(&mut cache, &path, &delta).expect("delegation delta should persist");

    assert_eq!(
        sqlite_table_ids(&path, "delegations"),
        vec![changed_id.to_owned(), unchanged_id.to_owned()]
    );
    let changed_row =
        sqlite_row_json(&path, "delegations", changed_id).expect("changed row should remain");
    let changed_value: Value =
        serde_json::from_str(&changed_row).expect("changed row should decode as json");
    assert_eq!(
        changed_value["title"],
        Value::String("Changed Delegation Row".to_owned())
    );
    assert_eq!(
        sqlite_row_json(&path, "delegations", unchanged_id),
        Some(unchanged_before),
        "unchanged delegation row should not be rewritten by a targeted delta"
    );
    assert!(sqlite_row_json(&path, "delegations", deleted_id).is_none());

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn sqlite_delta_metadata_only_update_does_not_rewrite_session_rows() {
    let state_root =
        std::env::temp_dir().join(format!("termal-sqlite-metadata-only-{}", Uuid::new_v4()));
    let _state_temp_root = TestTempRoot::own(state_root.clone());
    fs::create_dir_all(&state_root).expect("state root should exist");
    let path = state_root.join("termal.sqlite");
    let mut inner = StateInner::new();
    let session_id = inner
        .create_session(
            Agent::Claude,
            Some("Session".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    persist_state(&path, &inner).expect("initial sqlite state should persist");
    let session_before =
        sqlite_row_json(&path, "sessions", &session_id).expect("session row should exist");
    let watermark = inner.last_mutation_stamp;

    let project = inner.create_project(
        Some("Metadata Project".to_owned()),
        "/tmp/metadata-project".to_owned(),
        default_local_remote_id(),
    );
    let delta = inner.collect_persist_delta(watermark);
    assert!(delta.changed_sessions.is_empty());
    assert!(delta.removed_session_ids.is_empty());

    let mut cache = SqlitePersistConnectionCache::new();
    persist_delta_via_cache(&mut cache, &path, &delta).expect("metadata-only delta should persist");

    assert_eq!(
        sqlite_row_json(&path, "sessions", &session_id),
        Some(session_before),
        "metadata-only persist should leave session rows untouched"
    );
    let metadata = sqlite_metadata_state_value(&path);
    assert!(
        metadata["projects"]
            .as_array()
            .expect("projects should be encoded")
            .iter()
            .any(|value| value["id"] == Value::String(project.id.clone())),
        "metadata row should contain the newly-created project"
    );

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn sqlite_startup_loads_sessions_and_delegations_from_split_tables() {
    let state_root =
        std::env::temp_dir().join(format!("termal-sqlite-split-load-{}", Uuid::new_v4()));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let path = state_root.join("termal.sqlite");
    let mut inner = StateInner::new();
    let parent_id = inner
        .create_session(
            Agent::Codex,
            Some("Parent".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let child_id = inner
        .create_session(
            Agent::Codex,
            Some("Child".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let delegation = make_persist_test_delegation("delegation-split", &parent_id, &child_id);
    inner.delegations.push(delegation.clone());
    persist_state(&path, &inner).expect("split sqlite state should persist");

    let metadata = sqlite_metadata_state_value(&path);
    assert_eq!(metadata["sessions"], Value::Array(Vec::new()));
    assert!(
        metadata.get("delegations").is_none(),
        "delegations should be stored in the dedicated table, not embedded metadata"
    );

    let loaded = load_state(&path)
        .expect("sqlite state should load")
        .expect("sqlite state should exist");
    assert!(loaded.find_session_index(&parent_id).is_some());
    assert!(loaded.find_session_index(&child_id).is_some());
    assert_eq!(loaded.delegations.len(), 1);
    let loaded_delegation = &loaded.delegations[0];
    assert_eq!(loaded_delegation.id, delegation.id);
    assert_eq!(
        loaded_delegation.parent_session_id,
        delegation.parent_session_id
    );
    assert_eq!(
        loaded_delegation.child_session_id,
        delegation.child_session_id
    );
    assert_eq!(loaded_delegation.mode, delegation.mode);
    assert_eq!(loaded_delegation.title, delegation.title);
    assert_eq!(loaded_delegation.prompt, delegation.prompt);
    assert_eq!(loaded_delegation.cwd, delegation.cwd);
    assert_eq!(loaded_delegation.agent, delegation.agent);
    assert_eq!(loaded_delegation.write_policy, delegation.write_policy);
    assert_eq!(loaded_delegation.created_at, delegation.created_at);
    assert_eq!(loaded_delegation.started_at, delegation.started_at);
    assert_eq!(
        loaded_delegation.status,
        DelegationStatus::Running,
        "the state-only load must defer reviewer recovery until boot can inspect the coordination mailbox"
    );
    assert!(
        loaded_delegation.result.is_none(),
        "state-only loading must not invent a reviewer result before mailbox recovery"
    );

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn sqlite_startup_retains_only_the_latest_page_and_reads_older_history_by_index() {
    let state_root =
        std::env::temp_dir().join(format!("termal-sqlite-bounded-history-{}", Uuid::new_v4()));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let path = state_root.join("termal.sqlite");
    let mut inner = StateInner::new();
    let session_id = inner
        .create_session(
            Agent::Claude,
            Some("Bounded history".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let session_index = inner
        .find_session_index(&session_id)
        .expect("created session should exist");
    {
        let record = inner
            .session_mut_by_index(session_index)
            .expect("created session should be mutable");
        record.session.messages = (0..130)
            .map(|index| Message::Text {
                attachments: Vec::new(),
                id: format!("message-{index:03}"),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: format!("message {index}"),
                expanded_text: None,
                source: None,
            })
            .collect();
        record.session.message_count = 130;
        record.session.messages_loaded = true;
        record.message_start_index = 0;
        record.message_positions = build_message_positions(&record.session.messages);
    }
    persist_state(&path, &inner).expect("indexed transcript should persist");

    let loaded = load_state(&path)
        .expect("indexed transcript should load")
        .expect("persisted state should exist");
    let loaded_index = loaded
        .find_session_index(&session_id)
        .expect("loaded session should exist");
    let loaded_record = &loaded.sessions[loaded_index];
    assert_eq!(loaded_record.session.message_count, 130);
    assert!(!loaded_record.session.messages_loaded);
    assert_eq!(loaded_record.message_start_index, 66);
    assert_eq!(loaded_record.session.messages.len(), 64);
    assert_eq!(
        loaded_record.session.messages.first().map(Message::id),
        Some("message-066")
    );
    assert_eq!(
        loaded_record.session.messages.last().map(Message::id),
        Some("message-129")
    );

    assert_eq!(
        persisted_message_position(&path, &session_id, "message-002")
            .expect("indexed cursor lookup should succeed"),
        Some(2)
    );
    let older_page = load_persisted_message_range(&path, &session_id, 2, 66)
        .expect("older indexed history page should load");
    assert_eq!(older_page.len(), 64);
    assert_eq!(older_page.first().map(|(position, _)| *position), Some(2));
    assert_eq!(older_page.last().map(|(position, _)| *position), Some(65));
    assert_eq!(
        older_page.first().map(|(_, message)| message.id()),
        Some("message-002")
    );
    assert_eq!(
        older_page.last().map(|(_, message)| message.id()),
        Some("message-065")
    );

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn prompt_history_outlives_the_retained_transcript_window() {
    let mut inner = StateInner::new();
    let session_id = inner
        .create_session(
            Agent::Claude,
            Some("Prompt history".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let session_index = inner
        .find_session_index(&session_id)
        .expect("created session should exist");
    for index in 0..70 {
        let record = inner
            .session_mut_by_index(session_index)
            .expect("created session should remain mutable");
        push_message_on_record(
            record,
            Message::Text {
                attachments: Vec::new(),
                id: format!("user-{index:03}"),
                timestamp: stamp_now(),
                author: Author::You,
                text: format!("prompt {index}"),
                expanded_text: None,
                source: None,
            },
        );
        trim_retained_session_messages(record, 1);
    }

    let record = &inner.sessions[session_index];
    assert_eq!(record.session.messages.len(), 1);
    assert_eq!(
        record.session.prompt_history.len(),
        SESSION_PROMPT_HISTORY_LIMIT
    );
    assert_eq!(
        record.session.prompt_history.first().map(String::as_str),
        Some("prompt 6")
    );
    assert_eq!(
        record.session.prompt_history.last().map(String::as_str),
        Some("prompt 69")
    );
}

#[test]
fn sqlite_prompt_history_load_rejects_embedded_state_before_normalized_loading() {
    let state_root = PersistTestRoot::new("prompt-history-row-authority");
    let path = state_root.path().join("termal.sqlite");
    let mut inner = StateInner::new();
    let session_id = inner
        .create_session(
            Agent::Claude,
            Some("embedded".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let session_index = inner
        .find_session_index(&session_id)
        .expect("created session should exist");
    push_message_on_record(
        inner
            .session_mut_by_index(session_index)
            .expect("created session should remain mutable"),
        Message::Text {
            attachments: Vec::new(),
            id: "message-embedded".to_owned(),
            timestamp: stamp_now(),
            author: Author::You,
            text: "prompt from embedded".to_owned(),
            expanded_text: None,
            source: None,
        },
    );
    persist_state(&path, &inner).expect("current prompt histories should persist");

    let mut embedded_metadata: Value = serde_json::from_str(
        &sqlite_row_json(&path, "sessions", &session_id)
            .expect("embedded fixture session should exist"),
    )
    .expect("embedded fixture metadata should decode");
    embedded_metadata["session"]["promptHistory"] =
        Value::Array(vec![Value::String("obsolete embedded prompt".to_owned())]);
    let connection = rusqlite::Connection::open(&path).expect("fixture database should reopen");
    connection
        .execute(
            "UPDATE sessions SET value_json = ?2 WHERE id = ?1",
            rusqlite::params![
                session_id,
                serde_json::to_string(&embedded_metadata)
                    .expect("embedded fixture metadata should encode")
            ],
        )
        .expect("embedded fixture metadata should update");
    drop(connection);
    let before_rejection = fs::read(&path).expect("fixture bytes should be readable");

    let error = match load_state(&path) {
        Ok(_) => panic!("embedded prompt history must reject obsolete v2"),
        Err(error) => error,
    };
    let rendered = format!("{error:#}");
    assert!(rendered.contains("session.promptHistory"), "{rendered}");
    assert!(
        rendered.contains("Move or delete `termal.sqlite`"),
        "{rendered}"
    );
    assert_eq!(
        fs::read(&path).expect("rejected fixture bytes should remain readable"),
        before_rejection,
        "schema rejection must not rewrite the primary database"
    );
}

#[test]
fn sqlite_prompt_history_load_uses_empty_history_when_normalized_row_is_absent() {
    let state_root = PersistTestRoot::new("prompt-history-row-absent");
    let path = state_root.path().join("termal.sqlite");
    let mut inner = StateInner::new();
    let session_id = inner
        .create_session(
            Agent::Claude,
            Some("No normalized history".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let session_index = inner
        .find_session_index(&session_id)
        .expect("created session should exist");
    push_message_on_record(
        inner
            .session_mut_by_index(session_index)
            .expect("created session should remain mutable"),
        Message::Text {
            attachments: Vec::new(),
            id: "message-with-user-prompt".to_owned(),
            timestamp: stamp_now(),
            author: Author::You,
            text: "do not resurrect this from the transcript".to_owned(),
            expanded_text: None,
            source: None,
        },
    );
    persist_state(&path, &inner).expect("current prompt history should persist");

    let connection = rusqlite::Connection::open(&path).expect("fixture database should reopen");
    assert_eq!(
        connection
            .execute(
                "DELETE FROM session_prompt_histories WHERE session_id = ?1",
                rusqlite::params![session_id],
            )
            .expect("normalized prompt-history row should delete"),
        1
    );
    drop(connection);

    let loaded = load_state(&path)
        .expect("current schema without a history row should load")
        .expect("persisted state should exist");
    let loaded_index = loaded
        .find_session_index(&session_id)
        .expect("loaded session should exist");
    assert!(
        loaded.sessions[loaded_index]
            .session
            .prompt_history
            .is_empty(),
        "an absent normalized row means empty composer history; transcript prompts are not fallback authority"
    );
}

#[test]
fn appended_assistant_message_preserves_bounded_prompt_history_allocations() {
    let mut inner = StateInner::new();
    let session_id = inner
        .create_session(
            Agent::Claude,
            Some("Prompt history append".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let session_index = inner
        .find_session_index(&session_id)
        .expect("created session should exist");
    let record = inner
        .session_mut_by_index(session_index)
        .expect("created session should remain mutable");
    push_message_on_record(
        record,
        Message::Text {
            attachments: Vec::new(),
            id: "prompt".to_owned(),
            timestamp: stamp_now(),
            author: Author::You,
            text: "remember this".to_owned(),
            expanded_text: None,
            source: None,
        },
    );
    let retained_prompt_allocation = record.session.prompt_history[0].as_ptr();

    push_message_on_record(
        record,
        Message::Text {
            attachments: Vec::new(),
            id: "reply".to_owned(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: "streamed reply".to_owned(),
            expanded_text: None,
            source: None,
        },
    );

    assert_eq!(record.session.prompt_history, ["remember this"]);
    assert_eq!(
        record.session.prompt_history[0].as_ptr(),
        retained_prompt_allocation,
        "assistant appends must not rebuild and reallocate prompt history"
    );
}

#[test]
fn non_tail_insert_into_partial_transcript_preserves_authoritative_prompt_history() {
    let mut inner = StateInner::new();
    let session_id = inner
        .create_session(
            Agent::Claude,
            Some("Partial prompt history".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let session_index = inner
        .find_session_index(&session_id)
        .expect("created session should exist");
    let record = inner
        .session_mut_by_index(session_index)
        .expect("created session should remain mutable");
    record.session.prompt_history = vec!["authoritative older prompt".to_owned()];
    record.message_start_index = 10;
    record.session.messages_loaded = false;
    record.session.messages.push(Message::Text {
        attachments: Vec::new(),
        id: "resident-reply".to_owned(),
        timestamp: stamp_now(),
        author: Author::Assistant,
        text: "resident partial tail".to_owned(),
        expanded_text: None,
        source: None,
    });
    record.message_positions = build_message_positions(&record.session.messages);

    insert_message_on_record(
        record,
        0,
        Message::Text {
            attachments: Vec::new(),
            id: "late-user-card".to_owned(),
            timestamp: stamp_now(),
            author: Author::You,
            text: "cannot be canonically ordered".to_owned(),
            expanded_text: None,
            source: None,
        },
    );

    assert!(!record.session.messages_loaded);
    assert_eq!(
        record.session.prompt_history,
        ["authoritative older prompt"]
    );
}

#[test]
fn prompt_history_uses_an_independent_persist_delta_watermark() {
    let mut inner = StateInner::new();
    let session_id = inner
        .create_session(
            Agent::Claude,
            Some("Prompt history delta".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let session_index = inner
        .find_session_index(&session_id)
        .expect("created session should exist");
    push_message_on_record(
        inner
            .session_mut_by_index(session_index)
            .expect("created session should be mutable"),
        Message::Text {
            attachments: Vec::new(),
            id: "user-prompt".to_owned(),
            timestamp: stamp_now(),
            author: Author::You,
            text: "remember this".to_owned(),
            expanded_text: None,
            source: None,
        },
    );
    let watermark = inner.last_mutation_stamp;

    push_message_on_record(
        inner
            .session_mut_by_index(session_index)
            .expect("created session should remain mutable"),
        Message::Text {
            attachments: Vec::new(),
            id: "assistant-response".to_owned(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: "streamed response".to_owned(),
            expanded_text: None,
            source: None,
        },
    );
    let assistant_delta = inner.collect_persist_delta(watermark);
    let assistant_record = assistant_delta
        .changed_sessions
        .first()
        .expect("assistant message should change session metadata");
    assert!(
        !assistant_record.persist_prompt_history,
        "assistant streaming must not rewrite the independent history row"
    );
    let serialized =
        serialize_persisted_session(assistant_record).expect("assistant delta should serialize");
    assert!(serialized.prompt_history_value_json.is_none());
    let metadata: Value =
        serde_json::from_str(&serialized.value_json).expect("metadata should decode");
    assert!(metadata["session"].get("promptHistory").is_none());

    let assistant_watermark = inner.last_mutation_stamp;
    push_message_on_record(
        inner
            .session_mut_by_index(session_index)
            .expect("created session should remain mutable"),
        Message::Text {
            attachments: Vec::new(),
            id: "next-user-prompt".to_owned(),
            timestamp: stamp_now(),
            author: Author::You,
            text: "and this".to_owned(),
            expanded_text: None,
            source: None,
        },
    );
    let user_delta = inner.collect_persist_delta(assistant_watermark);
    let user_record = user_delta
        .changed_sessions
        .first()
        .expect("user prompt should change the session");
    assert!(user_record.persist_prompt_history);
    assert_eq!(
        serialize_persisted_session(user_record)
            .expect("user delta should serialize")
            .prompt_history_value_json
            .as_deref(),
        Some("[\"remember this\",\"and this\"]")
    );
}

#[test]
fn local_history_cursor_and_page_share_the_persisted_read_path() {
    let state = test_app_state();
    let mut inner = StateInner::new();
    let session_id = inner
        .create_session(
            Agent::Claude,
            Some("Combined history read".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let session_index = inner
        .find_session_index(&session_id)
        .expect("created session should exist");
    {
        let record = inner
            .session_mut_by_index(session_index)
            .expect("created session should be mutable");
        record.session.messages = (0..130)
            .map(|index| Message::Text {
                attachments: Vec::new(),
                id: format!("message-{index:03}"),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: format!("message {index}"),
                expanded_text: None,
                source: None,
            })
            .collect();
        record.session.message_count = 130;
        record.session.messages_loaded = true;
        record.message_start_index = 0;
        record.message_positions = build_message_positions(&record.session.messages);
    }
    persist_state(state.persistence_path.as_ref(), &inner)
        .expect("indexed transcript should persist");
    let loaded = load_state(state.persistence_path.as_ref())
        .expect("indexed transcript should load")
        .expect("persisted state should exist");
    *state.inner.lock().expect("state mutex poisoned") = loaded;

    // The cursor (65) and every returned row (1..64) are outside the retained
    // 66..129 in-memory suffix, forcing the request through the shared SQLite
    // snapshot for both cursor resolution and page loading.
    let page = state
        .get_session_history(&session_id, Some("message-065"), None, None, false, 64)
        .expect("combined persisted cursor/page read should succeed");
    assert_eq!(page.messages.len(), 64);
    assert_eq!(page.messages.first().map(Message::id), Some("message-001"));
    assert_eq!(page.messages.last().map(Message::id), Some("message-064"));
    assert_eq!(page.next_before.as_deref(), Some("message-001"));
    assert!(page.has_more);
    assert!(page.has_newer);
}

#[test]
fn sqlite_load_isolates_malformed_session_and_delegation_rows_but_rejects_metadata() {
    let state_root =
        std::env::temp_dir().join(format!("termal-sqlite-malformed-{}", Uuid::new_v4()));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let session_row_path = state_root.join("session-row.sqlite");
    let delegation_row_path = state_root.join("delegation-row.sqlite");
    let metadata_path = state_root.join("metadata.sqlite");

    let mut inner = StateInner::new();
    let parent_id = inner
        .create_session(
            Agent::Claude,
            Some("Parent".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let child_id = inner
        .create_session(
            Agent::Claude,
            Some("Child".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let session_id = inner
        .create_session(
            Agent::Claude,
            Some("Session".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let delegation = make_persist_test_delegation("delegation-malformed", &parent_id, &child_id);
    let mismatched_delegation =
        make_persist_test_delegation("delegation-mismatched", &parent_id, &child_id);
    inner.delegations.push(delegation.clone());
    inner.delegations.push(mismatched_delegation.clone());
    persist_state(&session_row_path, &inner).expect("session-row state should persist");
    persist_state(&delegation_row_path, &inner).expect("delegation-row state should persist");
    persist_state(&metadata_path, &inner).expect("metadata state should persist");

    {
        let connection =
            rusqlite::Connection::open(&session_row_path).expect("sqlite state should open");
        connection
            .execute(
                "UPDATE sessions SET value_json = '{ not json' WHERE id = ?1",
                rusqlite::params![session_id],
            )
            .expect("session row should be corrupted");
    }
    let session_state = load_state(&session_row_path)
        .expect("one malformed session row should not fail startup")
        .expect("session-row state should exist");
    assert!(
        session_state
            .sessions
            .iter()
            .all(|record| record.session.id != session_id),
        "only the malformed session row should be skipped"
    );
    assert_eq!(session_state.sessions.len(), 2);
    persist_state(&session_row_path, &session_state)
        .expect("full persistence must preserve a quarantined session row");
    assert!(
        sqlite_table_ids(&session_row_path, "sessions").contains(&session_id),
        "a skipped session row must remain durable for recovery"
    );

    {
        let connection =
            rusqlite::Connection::open(&delegation_row_path).expect("sqlite state should open");
        connection
            .execute(
                "UPDATE delegations SET value_json = '{ not json' WHERE id = ?1",
                rusqlite::params![delegation.id],
            )
            .expect("delegation row should be corrupted");
        let mut mismatched_value =
            serde_json::to_value(&mismatched_delegation).expect("delegation should encode");
        mismatched_value["id"] = Value::String("delegation-embedded-other".to_owned());
        connection
            .execute(
                "UPDATE delegations SET value_json = ?2 WHERE id = ?1",
                rusqlite::params![
                    mismatched_delegation.id,
                    serde_json::to_string(&mismatched_value)
                        .expect("mismatched delegation should serialize")
                ],
            )
            .expect("delegation identity should be corrupted");
    }
    let delegation_state = load_state(&delegation_row_path)
        .expect("one malformed delegation row should not fail startup")
        .expect("delegation-row state should exist");
    assert!(
        delegation_state
            .delegations
            .iter()
            .all(|record| { record.id != delegation.id && record.id != mismatched_delegation.id }),
        "invalid delegation rows should be skipped"
    );
    assert_eq!(delegation_state.sessions.len(), 3);
    persist_state(&delegation_row_path, &delegation_state)
        .expect("full persistence must preserve quarantined delegation rows");
    let persisted_delegation_ids = sqlite_table_ids(&delegation_row_path, "delegations");
    assert!(persisted_delegation_ids.contains(&delegation.id));
    assert!(persisted_delegation_ids.contains(&mismatched_delegation.id));

    {
        let connection =
            rusqlite::Connection::open(&metadata_path).expect("sqlite state should open");
        connection
            .execute(
                "UPDATE app_state SET value_json = '{ not json' WHERE key = 'metadataState'",
                [],
            )
            .expect("metadata row should be corrupted");
    }
    let metadata_error = match load_state(&metadata_path) {
        Ok(_) => panic!("malformed app_state row should fail startup load"),
        Err(error) => error,
    };
    let rendered = format!("{metadata_error:#}");
    assert!(
        rendered.contains("persisted state metadata is not valid JSON"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Move or delete `termal.sqlite`"),
        "{rendered}"
    );

    let _ = fs::remove_dir_all(state_root);
}

// Regression guard: `commit_session_created_locked` must route
// the persist work through the background channel rather than
// calling `persist_created_session` synchronously under the state
// mutex. Previously the mutex was held across a full SQLite
// transaction (connection open, schema-ensure, metadata + session
// upsert, commit with fsync) — every concurrent request that
// called `self.inner.lock()` blocked behind that I/O for 10-100 ms
// on slow disks.
//
// The sibling `persist_internal_locked` path has used the
// background channel since it was introduced; this test pins that
// the session-creation path shares the same contract. The
// crash-before-persist window is acceptable because a freshly-
// created `SessionRecord` has no user content (empty
// `messages: []`, no agent output) — see the commit message for
// the trade-off analysis.
#[test]
fn commit_session_created_locked_signals_background_persist_instead_of_blocking() {
    let (state, persist_rx) = test_app_state_with_live_persist_channel();
    let persistence_path = state.persistence_path.as_ref().to_path_buf();
    // Session record built under the same lock the caller would
    // hold, matching the real call site in `session_crud.rs`.
    let (revision, record_id) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Claude,
            Some("persist-channel-signal test".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        );
        // `create_session` already called `push_session` internally
        // (see `state_inner.rs`), so the record is in `inner.sessions`
        // at a known id — re-stamp it via `session_mut_by_index` to
        // mirror the real caller in `session_crud.rs`, which
        // overwrites the pushed slot with agent-specific field
        // defaults and then re-stamps so `collect_persist_delta`
        // picks up the rewrite on the next persist tick.
        let record_id = record.session.id.clone();
        let index = inner
            .find_session_index(&record_id)
            .expect("create_session should have pushed the record");
        let _ = inner.session_mut_by_index(index);
        let revision = state
            .commit_session_created_locked(&mut inner, &record)
            .expect("commit_session_created_locked should succeed");
        (revision, record_id)
    };

    // Primary assertion: the background channel received a `Delta`
    // wake. Reverting the fix (restoring the synchronous
    // `persist_created_session` call on every invocation) makes
    // the channel `try_recv` return `Err(Empty)` and this
    // assertion fails.
    let received = persist_rx
        .try_recv()
        .expect("commit_session_created_locked should have sent PersistRequest::Delta");
    // `PersistRequest` is a single-variant enum today; `matches!`
    // with an exhaustive pattern makes the assertion structural
    // (a future variant addition forces the reviewer to update
    // the test, which is the desired signal).
    assert!(matches!(received, PersistRequest::Delta));

    // Negative assertion: no synchronous state persist happened. Production
    // AppState construction opens the durable mailbox store and creates the
    // shared SQLite schema, while this lightweight test fixture deliberately
    // keeps that store disabled. Therefore an untouched database may either
    // have empty state tables or no state tables at all. A synchronous fallback
    // would necessarily create them and write both metadata and the session.
    let connection = open_sqlite_state_connection(&persistence_path)
        .expect("test SQLite state should remain readable");
    let app_state_table_exists = connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_master
                WHERE type = 'table' AND name = 'app_state'
            )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .expect("app_state table state should read");
    let app_state_rows = if app_state_table_exists {
        connection
            .query_row("SELECT COUNT(*) FROM app_state", [], |row| {
                row.get::<_, u64>(0)
            })
            .expect("app_state row count should read")
    } else {
        0
    };
    let sessions_table_exists = connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_master
                WHERE type = 'table' AND name = 'sessions'
            )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .expect("sessions table state should read");
    let session_rows = if sessions_table_exists {
        connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| {
                row.get::<_, u64>(0)
            })
            .expect("session row count should read")
    } else {
        0
    };
    assert_eq!(
        app_state_rows, 0,
        "synchronous fallback unexpectedly persisted app metadata"
    );
    assert_eq!(
        session_rows, 0,
        "synchronous fallback unexpectedly persisted the created session"
    );

    // Sanity: the revision did advance (the pre-persist increment
    // is unchanged by the fix).
    assert_eq!(revision, 1);
    // Fixture sanity — the record id should follow the
    // `session-<n>` shape `StateInner::create_session` mints.
    // Phrased as a `starts_with` so a future change to
    // `StateInner::new()`'s `next_session_number` seed (or a move
    // to UUID-shaped ids) doesn't false-fail this assertion.
    assert!(
        record_id.starts_with("session-"),
        "record id should follow `session-<n>` shape, got: {record_id}"
    );

    // Defensive cleanup for the per-test SQLite directory and sidecars.
    drop(connection);
    if let Some(state_root) = persistence_path.parent() {
        let _ = fs::remove_dir_all(state_root);
    }
}

/// Regression: a session whose metadata knows a remote transcript length but
/// holds no local message rows must not abort startup.
///
/// Remote-proxy sessions are legitimately in this shape: the transcript lives
/// on the remote host, so `message_count` is known from proxy metadata while
/// zero `messages` rows exist locally. `load_persisted_session_tail` derived
/// its expected tail purely from `message_count` and `bail!`ed when the row
/// count disagreed, and that error propagates through `load_state` ->
/// `AppState::new_with_paths` -> `main`, so ONE such session made TermAl
/// refuse to boot. Absent local rows are a hydration state, not corruption.
#[test]
fn sqlite_startup_tolerates_remote_metadata_message_count_without_local_rows() {
    let state_root =
        std::env::temp_dir().join(format!("termal-sqlite-proxy-boot-{}", Uuid::new_v4()));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let path = state_root.join("termal.sqlite");
    let mut inner = StateInner::new();
    let session_id = inner
        .create_session(
            Agent::Claude,
            Some("Remote proxy".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let session_index = inner
        .find_session_index(&session_id)
        .expect("created session should exist");
    {
        let record = inner
            .session_mut_by_index(session_index)
            .expect("created session should be mutable");
        // The remote-proxy shape: metadata knows the remote length, nothing
        // is hydrated locally. Well above SQLITE_SESSION_TAIL_MESSAGES (64)
        // so the loader computes a non-zero start_index and expects a tail.
        record.session.messages = Vec::new();
        record.session.message_count = 1408;
        record.session.messages_loaded = false;
        record.message_start_index = 0;
        record.message_positions = build_message_positions(&record.session.messages);
        record.remote_id = Some("remote-1".to_owned());
        record.remote_session_id = Some("remote-session-1".to_owned());
    }
    persist_state(&path, &inner).expect("remote-proxy metadata should persist");

    let loaded = load_state(&path)
        .expect("startup must not fail when a proxy transcript has no local rows")
        .expect("persisted state should exist");
    let loaded_index = loaded
        .find_session_index(&session_id)
        .expect("loaded session should exist");
    let loaded_record = &loaded.sessions[loaded_index];
    // The remote length is retained so the UI can still show a count, and the
    // session is reported as not locally hydrated rather than empty-and-loaded.
    assert_eq!(loaded_record.session.message_count, 1408);
    assert!(
        !loaded_record.session.messages_loaded,
        "a proxy with no local rows must not claim a fully loaded transcript"
    );
    assert!(
        loaded_record.session.messages.is_empty(),
        "no local rows exist, so no messages should be materialized"
    );

    let _ = fs::remove_dir_all(state_root);
}

/// Regression: a persisted remote proxy can own a bounded, nonzero transcript
/// suffix while deliberately omitting the local-only overview blob. Schema
/// startup must recognize that shape before attempting local overview
/// backfill.
#[test]
fn sqlite_startup_tolerates_bounded_remote_proxy_suffix_without_overview() {
    let state_root = PersistTestRoot::new("bounded-remote-proxy-boot");
    let path = state_root.path().join("termal.sqlite");
    let mut inner = StateInner::new();
    let session_id = inner
        .create_session(
            Agent::Claude,
            Some("Bounded remote proxy".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let session_index = inner
        .find_session_index(&session_id)
        .expect("created session should exist");
    {
        let record = inner
            .session_mut_by_index(session_index)
            .expect("created session should be mutable");
        record.session.messages = (936..1000)
            .map(|position| Message::Text {
                attachments: Vec::new(),
                id: format!("remote-message-{position}"),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: format!("remote message {position}"),
                expanded_text: None,
                source: None,
            })
            .collect();
        record.session.message_count = 1000;
        record.session.messages_loaded = false;
        record.message_start_index = 936;
        record.message_positions = build_message_positions(&record.session.messages);
        record.remote_id = Some("remote-1".to_owned());
        record.remote_session_id = Some("remote-session-1".to_owned());
    }
    persist_state(&path, &inner).expect("bounded remote proxy should persist");
    {
        let connection =
            rusqlite::Connection::open(&path).expect("remote proxy database should reopen");
        let overview_count: u32 = connection
            .query_row(
                "SELECT COUNT(*)
                 FROM session_overviews
                 WHERE session_id = ?1",
                rusqlite::params![session_id],
                |row| row.get(0),
            )
            .expect("remote overview count should be queryable");
        assert_eq!(
            overview_count, 0,
            "remote proxies must not persist local overview blobs"
        );
    }

    let loaded = load_state(&path)
        .expect("bounded remote proxy suffix must not abort startup")
        .expect("persisted state should exist");
    let loaded_index = loaded
        .find_session_index(&session_id)
        .expect("loaded remote proxy should exist");
    let loaded_record = &loaded.sessions[loaded_index];
    assert_eq!(loaded_record.message_start_index, 936);
    assert_eq!(loaded_record.session.message_count, 1000);
    assert!(!loaded_record.session.messages_loaded);
    assert_eq!(loaded_record.session.messages.len(), 64);
    assert_eq!(
        loaded_record.session.messages.first().map(Message::id),
        Some("remote-message-936")
    );
    assert_eq!(
        loaded_record.session.messages.last().map(Message::id),
        Some("remote-message-999")
    );
}

/// Regression: one structurally invalid session row must not make otherwise
/// healthy sessions unreachable at startup.
///
/// This exercises three independent per-session failure boundaries that used
/// to propagate out of `load_state`: the SQLite row key disagreeing with the
/// embedded session id, persisted session-field validation, and malformed
/// message JSON. The invalid rows stay on disk for recovery, but none may be
/// presented as a healthy in-memory session.
#[test]
fn sqlite_startup_skips_invalid_session_rows_and_loads_valid_sessions() {
    let state_root = std::env::temp_dir().join(format!(
        "termal-sqlite-session-isolation-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let path = state_root.join("termal.sqlite");
    let mut inner = StateInner::new();
    let valid_session_id = inner
        .create_session(
            Agent::Claude,
            Some("Valid".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let mismatched_session_id = inner
        .create_session(
            Agent::Claude,
            Some("Mismatched".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let invalid_settings_session_id = inner
        .create_session(
            Agent::Claude,
            Some("Invalid settings".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let malformed_transcript_session_id = inner
        .create_session(
            Agent::Claude,
            Some("Malformed transcript".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let mismatched_message_key_session_id = inner
        .create_session(
            Agent::Claude,
            Some("Mismatched message key".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let partial_remote_identity_session_id = inner
        .create_session(
            Agent::Claude,
            Some("Partial remote identity".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let malformed_index = inner
        .find_session_index(&malformed_transcript_session_id)
        .expect("malformed transcript fixture session should exist");
    push_message_on_record(
        inner
            .session_mut_by_index(malformed_index)
            .expect("malformed transcript fixture session should be mutable"),
        Message::Text {
            attachments: Vec::new(),
            id: "message-malformed".to_owned(),
            timestamp: "2026-07-30T00:00:00Z".to_owned(),
            author: Author::Assistant,
            text: "stored before corruption".to_owned(),
            expanded_text: None,
            source: None,
        },
    );
    let mismatched_message_index = inner
        .find_session_index(&mismatched_message_key_session_id)
        .expect("mismatched-message fixture session should exist");
    push_message_on_record(
        inner
            .session_mut_by_index(mismatched_message_index)
            .expect("mismatched-message fixture session should be mutable"),
        Message::Text {
            attachments: Vec::new(),
            id: "message-embedded-id".to_owned(),
            timestamp: "2026-07-30T00:00:00Z".to_owned(),
            author: Author::Assistant,
            text: "stored before key corruption".to_owned(),
            expanded_text: None,
            source: None,
        },
    );
    persist_state(&path, &inner).expect("session isolation fixture should persist");

    {
        let connection =
            rusqlite::Connection::open(&path).expect("session isolation fixture should reopen");
        let mismatched_json: String = connection
            .query_row(
                "SELECT value_json FROM sessions WHERE id = ?1",
                rusqlite::params![mismatched_session_id],
                |row| row.get(0),
            )
            .expect("mismatched fixture metadata should load");
        let mut mismatched_value: Value =
            serde_json::from_str(&mismatched_json).expect("fixture metadata should parse");
        mismatched_value["session"]["id"] = Value::String("session-embedded-other".to_owned());
        connection
            .execute(
                "UPDATE sessions SET value_json = ?2 WHERE id = ?1",
                rusqlite::params![
                    mismatched_session_id,
                    serde_json::to_string(&mismatched_value)
                        .expect("mismatched fixture metadata should serialize")
                ],
            )
            .expect("mismatched fixture metadata should update");

        let invalid_settings_json: String = connection
            .query_row(
                "SELECT value_json FROM sessions WHERE id = ?1",
                rusqlite::params![invalid_settings_session_id],
                |row| row.get(0),
            )
            .expect("invalid-settings fixture metadata should load");
        let mut invalid_settings_value: Value =
            serde_json::from_str(&invalid_settings_json).expect("fixture metadata should parse");
        invalid_settings_value["session"]["claudeApprovalMode"] = Value::Null;
        connection
            .execute(
                "UPDATE sessions SET value_json = ?2 WHERE id = ?1",
                rusqlite::params![
                    invalid_settings_session_id,
                    serde_json::to_string(&invalid_settings_value)
                        .expect("invalid-settings fixture metadata should serialize")
                ],
            )
            .expect("invalid-settings fixture metadata should update");

        connection
            .execute(
                "UPDATE messages SET value_json = '{'
                 WHERE session_id = ?1 AND message_id = 'message-malformed'",
                rusqlite::params![malformed_transcript_session_id],
            )
            .expect("malformed transcript fixture should update");
        connection
            .execute(
                "UPDATE messages SET message_id = 'message-row-id'
                 WHERE session_id = ?1 AND message_id = 'message-embedded-id'",
                rusqlite::params![mismatched_message_key_session_id],
            )
            .expect("message row key should update");

        let partial_remote_json: String = connection
            .query_row(
                "SELECT value_json FROM sessions WHERE id = ?1",
                rusqlite::params![partial_remote_identity_session_id],
                |row| row.get(0),
            )
            .expect("partial-remote fixture metadata should load");
        let mut partial_remote_value: Value =
            serde_json::from_str(&partial_remote_json).expect("fixture metadata should parse");
        partial_remote_value["remoteId"] = Value::String("remote-without-session".to_owned());
        connection
            .execute(
                "UPDATE sessions SET value_json = ?2 WHERE id = ?1",
                rusqlite::params![
                    partial_remote_identity_session_id,
                    serde_json::to_string(&partial_remote_value)
                        .expect("partial-remote fixture metadata should serialize")
                ],
            )
            .expect("partial-remote fixture metadata should update");
    }

    let loaded = load_state(&path)
        .expect("one invalid session must not abort startup")
        .expect("persisted state should exist");
    assert!(
        loaded.find_session_index(&valid_session_id).is_some(),
        "the valid sibling session must remain reachable"
    );
    for invalid_session_id in [
        &mismatched_session_id,
        &invalid_settings_session_id,
        &malformed_transcript_session_id,
        &mismatched_message_key_session_id,
        &partial_remote_identity_session_id,
    ] {
        assert!(
            loaded.find_session_index(invalid_session_id).is_none(),
            "invalid session `{invalid_session_id}` must be skipped rather than presented as healthy"
        );
    }
    persist_state(&path, &loaded)
        .expect("full persistence must preserve every quarantined session row");
    let stored_session_ids = sqlite_table_ids(&path, "sessions");
    for invalid_session_id in [
        &mismatched_session_id,
        &invalid_settings_session_id,
        &malformed_transcript_session_id,
        &mismatched_message_key_session_id,
        &partial_remote_identity_session_id,
    ] {
        assert!(
            stored_session_ids.contains(invalid_session_id),
            "quarantined session `{invalid_session_id}` must stay on disk"
        );
    }

    let _ = fs::remove_dir_all(state_root);
}
