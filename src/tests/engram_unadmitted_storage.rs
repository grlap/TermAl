//! Primary reader-floor compatibility for retained observation custody.
use super::*;

const PREDECESSOR: &str = "2-metadata-units-2-handoff-1";
const SUCCESSOR: &str = "2-metadata-units-2-handoff-1-unadmitted-outbox-1";

fn predecessor() -> rusqlite::Connection {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    seed_current_state_core_tables(&connection);
    seed_current_state_auxiliary_tables(&connection);
    seed_current_state_metadata(&connection);
    connection.execute("UPDATE meta SET value=?1 WHERE key='schema_version'", [PREDECESSOR]).unwrap();
    connection
}

#[test]
fn unadmitted_runtime_reporting_storage_upgrades_exact_predecessor_before_new_custody() {
    let connection = predecessor();
    let metadata = read_metadata_units_unchecked(&connection).unwrap();
    let generation = metadata_unit_generation(&connection).unwrap();
    ensure_sqlite_state_schema_for_load(&connection).unwrap();
    assert_eq!(required_state_meta_value(&connection, "schema_version").unwrap(), SUCCESSOR);
    assert_eq!(SQLITE_SCHEMA_VERSION, SUCCESSOR);
    assert_eq!(read_metadata_units_unchecked(&connection).unwrap(), metadata);
    assert_eq!(metadata_unit_generation(&connection).unwrap(), generation);
    ensure_sqlite_state_schema_for_load(&connection).unwrap();
}

#[test]
fn unadmitted_runtime_reporting_storage_marker_failure_rolls_back_without_data_change() {
    let connection = predecessor();
    let metadata = read_metadata_units_unchecked(&connection).unwrap();
    let generation = metadata_unit_generation(&connection).unwrap();
    connection.execute_batch("CREATE TEMP TRIGGER refuse_observation_floor BEFORE UPDATE ON meta
        WHEN old.key='schema_version' BEGIN SELECT RAISE(ABORT,'observation-floor-fault'); END;").unwrap();
    let result = ensure_sqlite_state_schema_for_load(&connection);
    let error = match result { Ok(_) => panic!("marker fault must refuse migration"), Err(error) => error };
    assert!(format!("{error:#}").contains("observation-floor-fault"));
    assert!(connection.is_autocommit());
    assert_eq!(required_state_meta_value(&connection, "schema_version").unwrap(), PREDECESSOR);
    assert_eq!(read_metadata_units_unchecked(&connection).unwrap(), metadata);
    assert_eq!(metadata_unit_generation(&connection).unwrap(), generation);
    connection.execute_batch("DROP TRIGGER refuse_observation_floor").unwrap();
    ensure_sqlite_state_schema_for_load(&connection).unwrap();
    assert_eq!(required_state_meta_value(&connection, "schema_version").unwrap(), SUCCESSOR);
}

#[test]
fn unadmitted_runtime_reporting_storage_future_or_unversioned_custody_is_not_adopted() {
    for future in [false, true] {
        let connection = predecessor();
        // Deliberately unsupported draft, not a fabricated valid report.
        let raw = r#"{"engramUnadmittedReports":[{"unknown":"retained-for-diagnosis"}]}"#;
        connection.execute("INSERT INTO sessions(id,value_json) VALUES('retained',?1)", [raw]).unwrap();
        if future {
            connection.execute("UPDATE meta SET value='future-observation-format' WHERE key='schema_version'", []).unwrap();
        }
        let version = required_state_meta_value(&connection, "schema_version").unwrap();
        let metadata = read_metadata_units_unchecked(&connection).unwrap();
        let changes = connection.total_changes();
        assert!(ensure_sqlite_state_schema_for_load(&connection).is_err());
        assert_eq!(connection.total_changes(), changes);
        assert_eq!(required_state_meta_value(&connection, "schema_version").unwrap(), version);
        assert_eq!(read_metadata_units_unchecked(&connection).unwrap(), metadata);
        let retained: String = connection.query_row("SELECT value_json FROM sessions WHERE id='retained'", [], |r| r.get(0)).unwrap();
        assert_eq!(retained, raw, "do not discard or rewrite unknown custody");
    }
}

#[test]
fn unadmitted_runtime_reporting_storage_unknown_predecessor_table_is_preserved() {
    let connection = predecessor();
    connection.execute_batch("CREATE TABLE unknown_custody(data TEXT);
        INSERT INTO unknown_custody VALUES('retained');").unwrap();
    let changes = connection.total_changes();
    assert!(ensure_sqlite_state_schema_for_load(&connection).is_err());
    assert_eq!(connection.total_changes(), changes);
    assert_eq!(required_state_meta_value(&connection, "schema_version").unwrap(), PREDECESSOR);
    assert_eq!(connection.query_row("SELECT data FROM unknown_custody", [], |r| r.get::<_, String>(0)).unwrap(), "retained");
}

#[test]
fn unadmitted_review_predecessor_upgrade_keeps_ignored_legacy_mailboxes() {
    for version in [SQLITE_METADATA_UNITS_VERSION, PREDECESSOR] {
        let connection = predecessor();
        connection.execute("UPDATE meta SET value=?1 WHERE key='schema_version'", [version]).unwrap();
        connection.execute_batch("CREATE TABLE mailboxes(id TEXT PRIMARY KEY);
            INSERT INTO mailboxes VALUES('obsolete-but-retained');").unwrap();
        let metadata = read_metadata_units_unchecked(&connection).unwrap();
        let generation = metadata_unit_generation(&connection).unwrap();
        ensure_sqlite_state_schema_for_load(&connection)
            .expect("a supported predecessor's ignored legacy table must not prevent upgrade");
        assert_eq!(required_state_meta_value(&connection, "schema_version").unwrap(), SUCCESSOR);
        assert_eq!(read_metadata_units_unchecked(&connection).unwrap(), metadata);
        assert_eq!(metadata_unit_generation(&connection).unwrap(), generation);
        assert_eq!(connection.query_row("SELECT id FROM mailboxes", [], |row| row.get::<_, String>(0)).unwrap(),
            "obsolete-but-retained");
    }
}
