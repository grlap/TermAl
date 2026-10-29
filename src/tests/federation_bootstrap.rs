// Owns black-box foundation regressions through the existing settings/schema seams.
// Does not own transport, live enrollment, or host provisioning. New coverage,
// mounted from tests/mod.rs; no production implementation was extracted.
use super::*;

#[test]
fn federation_defaults_to_disabled_standalone_without_adopting_local_remotes() {
    let preferences = serde_json::to_value(AppPreferences::default()).unwrap();
    assert_eq!(preferences["federation"]["enabled"], json!(false));
    assert_eq!(preferences["federation"]["role"], json!("standalone"));
    assert!(preferences["federation"]["central"].is_null());
    let legacy: AppPreferences = serde_json::from_value(json!({})).unwrap();
    assert_eq!(serde_json::to_value(legacy).unwrap(), preferences);
}

fn stored_host(connection: &rusqlite::Connection) -> (String, String) {
    let read = |key: &str| -> String {
        connection
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .unwrap()
            .unwrap_or_else(|| panic!("missing durable {key}"))
    };
    (
        read("federation_host_id"),
        read("federation_host_generation"),
    )
}

#[test]
fn federation_schema_mints_identity_once_and_preserves_it_on_reopen() {
    let root = TestTempRoot::create("federation-identity");
    let path = root.path().join("termal.sqlite");
    let connection = rusqlite::Connection::open(&path).unwrap();
    ensure_sqlite_state_schema(&connection).unwrap();
    let first = stored_host(&connection);
    assert_eq!(Uuid::parse_str(&first.0).unwrap().to_string(), first.0);
    assert_eq!(first.1, "1");
    drop(connection);
    let reopened = rusqlite::Connection::open(&path).unwrap();
    ensure_sqlite_state_schema(&reopened).unwrap();
    assert_eq!(stored_host(&reopened), first);
}

#[test]
fn federation_separate_stores_never_share_a_minted_host_identity() {
    let left = rusqlite::Connection::open_in_memory().unwrap();
    let right = rusqlite::Connection::open_in_memory().unwrap();
    ensure_sqlite_state_schema(&left).unwrap();
    ensure_sqlite_state_schema(&right).unwrap();
    assert_ne!(stored_host(&left).0, stored_host(&right).0);
}

#[test]
fn federation_legacy_schema_upgrade_preserves_local_metadata() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    seed_primary2_state_schema_for_test(&connection);
    let mut metadata =
        serde_json::to_value(PersistedState::metadata_from_inner(&StateInner::new())).unwrap();
    assert_eq!(
        metadata.as_object_mut().unwrap().remove("engramSessionObligations"),
        Some(json!(EngramSessionObligationLedger::empty())),
        "a primary-2 specimen cannot contain modern obligation custody"
    );
    connection.execute("INSERT INTO app_state(key,value_json) VALUES(?1,?2)",
        rusqlite::params![SQLITE_METADATA_KEY, serde_json::to_string(&metadata).unwrap()]).unwrap();
    let before: String = connection
        .query_row(
            "SELECT value_json FROM app_state WHERE key = ?1",
            [SQLITE_METADATA_KEY],
            |row| row.get(0),
        )
        .unwrap();
    ensure_sqlite_state_schema(&connection).unwrap();
    assert_eq!(stored_host(&connection).1, "1");
    let after: String = connection
        .query_row(
            "SELECT value_json FROM app_state WHERE key = ?1",
            [SQLITE_METADATA_KEY],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(before, after);
    let current = read_persisted_metadata_json(&connection).unwrap().unwrap();
    let restored: PersistedState = serde_json::from_str(&current).unwrap();
    let original: Value = serde_json::from_str(&before).unwrap();
    assert_eq!(json!(restored.next_session_number), original["nextSessionNumber"]);
    assert_eq!(json!(restored.next_message_number), original["nextMessageNumber"]);
    assert_eq!(serde_json::to_value(restored.projects).unwrap(), original["projects"]);
}
