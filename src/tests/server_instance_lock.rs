//! Server instance lock: a server-mode boot on a data directory that another
//! server holds must refuse before it loads, recovers or persists anything
//! there, and say which data directory and lock file are in use. A free data
//! directory boots as before, and again once the first server let it go. The
//! lock belongs to its holder, not to the server's state.

use super::*;

/// Every row of every table, each rendered from its typed SQLite values, so
/// equal snapshots mean byte-identical rows.
fn state_database_rows(path: &FsPath) -> BTreeMap<String, Vec<String>> {
    let connection = rusqlite::Connection::open(path).expect("state database should open");
    let tables = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .expect("table query should prepare")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("table query should run")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("table names should decode");
    let mut snapshot = BTreeMap::new();
    for table in tables {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM \"{table}\""))
            .expect("row query should prepare");
        let columns = statement.column_count();
        let mut rows = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|column| row.get::<_, rusqlite::types::Value>(column))
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map(|values| format!("{values:?}"))
            })
            .expect("row query should run")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("rows should decode");
        rows.sort();
        snapshot.insert(table, rows);
    }
    snapshot
}

#[test]
fn second_server_boot_refuses_a_held_data_directory_before_touching_its_store() {
    let root = TestTempRoot::create("termal-second-server");
    let data_dir = std::path::absolute(root.path()).expect("test root should resolve");
    let persistence_path = root.path().join("termal.sqlite");
    let coordination_path = resolve_coordination_persistence_path(&persistence_path);
    let templates_path = root.path().join("orchestrators.json");

    // The running host: it holds the instance lock, and one of its sessions
    // is stored mid-turn, which boot recovery would rewrite as interrupted.
    let running_host =
        ServerInstanceLock::acquire(&data_dir).expect("the running host should take the lock");
    let mut inner = StateInner::new();
    let session_id = inner
        .create_session(
            Agent::Claude,
            None,
            root.path().to_string_lossy().into_owned(),
            None,
            None,
        )
        .session
        .id
        .clone();
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    inner.sessions[index].session.status = SessionStatus::Active;
    persist_state(&persistence_path, &inner).expect("the running host's state should persist");
    let before = state_database_rows(&persistence_path);
    assert!(!coordination_path.exists());

    let second_holder = OnceLock::new();
    let refusal = match AppState::new_server_with_paths(
        root.path().to_string_lossy().into_owned(),
        persistence_path.clone(),
        templates_path,
        &second_holder,
    ) {
        Ok(state) => {
            state.shutdown_persist_blocking();
            panic!("a second server must not boot on a held data directory");
        }
        Err(error) => format!("{error:#}"),
    };
    assert!(
        second_holder.get().is_none(),
        "the refused start holds no lock"
    );

    assert!(
        !coordination_path.exists(),
        "the refused boot must stop before it bootstraps the coordination database: {refusal}"
    );
    assert_eq!(
        state_database_rows(&persistence_path),
        before,
        "the refused boot must leave every stored row byte-identical"
    );
    assert!(
        refusal.contains("another TermAl server is already using the data directory"),
        "{refusal}"
    );
    assert!(
        refusal.contains(&format!("`{}`", data_dir.display())),
        "the refusal must name the data directory: {refusal}"
    );
    assert!(
        refusal.contains(
            &data_dir
                .join(SERVER_INSTANCE_LOCK_FILE_NAME)
                .display()
                .to_string()
        ),
        "the refusal must name the lock file: {refusal}"
    );
    assert!(
        refusal.contains(&format!("pid {}", std::process::id())),
        "the refusal must name the running instance: {refusal}"
    );
    drop(running_host);
}

#[test]
fn server_instance_refusal_names_the_current_holder_or_says_none_is_recorded() {
    let root = TestTempRoot::create("termal-server-owner");
    let data_dir = std::path::absolute(root.path()).expect("test root should resolve");
    let owner_path = data_dir.join(SERVER_INSTANCE_OWNER_FILE_NAME);
    // A previous holder's record must not survive into the new holder's term.
    fs::write(
        &owner_path,
        r#"{"pid":0,"startedAt":"2000-01-01T00:00:00+00:00","workdir":"previous"}"#,
    )
    .expect("a previous holder's record should be written");

    let holder = ServerInstanceLock::acquire(&data_dir).expect("the lock should be free");
    let refusal = match ServerInstanceLock::acquire(&data_dir) {
        Ok(_) => panic!("a held lock must refuse a second holder"),
        Err(error) => format!("{error:#}"),
    };
    assert!(
        refusal.contains(&format!("held by pid {},", std::process::id())),
        "{refusal}"
    );
    assert!(!refusal.contains("previous"), "{refusal}");
    assert!(
        !data_dir
            .join(format!("{SERVER_INSTANCE_OWNER_FILE_NAME}.pending"))
            .exists(),
        "the owner record is renamed into place, leaving no pending file"
    );

    fs::remove_file(&owner_path).expect("the owner record should be removable");
    let refusal = match ServerInstanceLock::acquire(&data_dir) {
        Ok(_) => panic!("a held lock must refuse a second holder"),
        Err(error) => format!("{error:#}"),
    };
    assert!(
        refusal.contains("its holder is not described in"),
        "{refusal}"
    );
    assert!(
        refusal.contains(
            &data_dir
                .join(SERVER_INSTANCE_LOCK_FILE_NAME)
                .display()
                .to_string()
        ),
        "{refusal}"
    );
    drop(holder);
}

#[test]
fn server_boot_takes_a_free_data_directory_and_boots_again_after_release() {
    let root = TestTempRoot::create("termal-first-server");
    let persistence_path = root.path().join("termal.sqlite");
    let templates_path = root.path().join("orchestrators.json");
    let boot = |holder: &OnceLock<ServerInstanceLock>| {
        AppState::new_server_with_paths(
            root.path().to_string_lossy().into_owned(),
            persistence_path.clone(),
            templates_path.clone(),
            holder,
        )
    };

    let first_holder = OnceLock::new();
    let first_state = boot(&first_holder).expect("a free data directory should boot");
    first_state.shutdown_persist_blocking();
    drop(first_state);
    drop(first_holder);

    let second_holder = OnceLock::new();
    let second_state = boot(&second_holder)
        .expect("a restart should boot once the first server released the lock");
    second_state.shutdown_persist_blocking();
    drop(second_state);
    drop(second_holder);
}

// The lock belongs to its holder, which the running server keeps until the
// process exits, not to any `AppState`: when the server's own state is gone
// but a background writer still holds a clone, a competing start is refused.
#[test]
fn server_lock_outlives_the_server_state_while_a_background_writer_holds_a_clone() {
    let root = TestTempRoot::create("termal-server-writer");
    let data_dir = std::path::absolute(root.path()).expect("test root should resolve");
    let holder = OnceLock::new();
    let state = AppState::new_server_with_paths(
        root.path().to_string_lossy().into_owned(),
        root.path().join("termal.sqlite"),
        root.path().join("orchestrators.json"),
        &holder,
    )
    .expect("a free data directory should boot");
    let background_writer = state.clone();
    drop(state);

    let refusal = match ServerInstanceLock::acquire(&data_dir) {
        Ok(_) => panic!("the lock must stay held while a writer can still persist"),
        Err(error) => format!("{error:#}"),
    };
    assert!(
        refusal.contains("another TermAl server is already using the data directory"),
        "{refusal}"
    );

    background_writer.shutdown_persist_blocking();
    drop(background_writer);
    drop(holder);
    let restarted =
        ServerInstanceLock::acquire(&data_dir).expect("the released lock should be free again");
    drop(restarted);
}
