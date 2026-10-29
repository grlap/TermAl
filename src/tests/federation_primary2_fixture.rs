use super::*;

// Build a historical fixture beside the current disposable store. Never
// downgrade the current reader floor or bypass the offline rollback guard.
fn primary2_fixture_copy(app: &AppState) -> PathBuf {
    let source = rusqlite::Connection::open_with_flags(
        app.persistence_path.as_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        required_state_meta_value(&source, "schema_version").unwrap(),
        SQLITE_SCHEMA_VERSION
    );
    let mut metadata: Value =
        serde_json::from_str(&read_persisted_metadata_json(&source).unwrap().unwrap()).unwrap();
    assert_eq!(
        metadata.as_object_mut().unwrap().remove("engramSessionObligations"),
        Some(json!(EngramSessionObligationLedger::empty())),
        "a historical specimen cannot discard modern obligation custody"
    );
    let metadata = serde_json::to_string(&metadata).unwrap();
    let mut rows = source.prepare("SELECT value_json FROM sessions").unwrap();
    for encoded in rows.query_map([], |row| row.get::<_, String>(0)).unwrap() {
        let record: PersistedSessionRecord = serde_json::from_str(&encoded.unwrap()).unwrap();
        assert!(
            record.queued_prompts.is_empty(),
            "historical fixture has no queued work"
        );
        assert!(
            record.prompt_deliveries.is_empty(),
            "historical fixture has no delivery receipts"
        );
    }
    let path = app
        .persistence_path
        .with_file_name("primary2-reload.sqlite");
    assert!(!path.exists(), "the historical fixture must be new");
    let mut legacy = rusqlite::Connection::open(&path).unwrap();
    seed_primary2_state_schema_for_test(&legacy);
    legacy
        .execute(
            "ATTACH DATABASE ?1 AS fixture_source",
            [app.persistence_path.to_string_lossy().as_ref()],
        )
        .unwrap();
    let tx = legacy.transaction().unwrap();
    // Copy normalized payloads byte-for-byte using the frozen primary-2
    // inventory. Metadata is reconstructed from current units, not provenance.
    for &(table, columns) in CURRENT_SQLITE_STATE_TABLE_COLUMNS {
        if matches!(table, "meta" | "app_state" | "state_metadata_units") {
            continue;
        }
        let columns = columns.join(",");
        tx.execute(
            &format!("INSERT INTO {table}({columns}) SELECT {columns} FROM fixture_source.{table}"),
            [],
        )
        .unwrap();
    }
    tx.execute("DELETE FROM meta", []).unwrap();
    tx.execute(
        "INSERT INTO meta SELECT key,value FROM fixture_source.meta
         WHERE key NOT IN ('schema_version','metadata_units_generation')",
        [],
    )
    .unwrap();
    tx.execute("INSERT INTO meta VALUES('schema_version','2')", [])
        .unwrap();
    tx.execute(
        "INSERT INTO app_state VALUES(?1,?2)",
        rusqlite::params![SQLITE_METADATA_KEY, metadata],
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(
        read_federation_host_identity(&legacy).unwrap(),
        read_federation_host_identity(&source).unwrap()
    );
    assert_eq!(
        required_state_meta_value(&legacy, "schema_version").unwrap(),
        "2"
    );
    assert_eq!(
        required_state_meta_value(&source, "schema_version").unwrap(),
        SQLITE_SCHEMA_VERSION,
        "constructing history cannot downgrade the source"
    );
    path
}

fn install_primary2_fixture(app: &AppState, historical: &FsPath) {
    // The enrolled authority includes the canonical store pathname. Keep that
    // pathname while replacing only this stopped, disposable fixture image.
    let original = app.persistence_path.as_path();
    let connection = rusqlite::Connection::open(original).unwrap();
    let (busy, _, _): (i64, i64, i64) = connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap();
    assert_eq!(busy, 0, "stopped fixture must allow a complete checkpoint");
    drop(connection);
    for suffix in ["-wal", "-shm"] {
        assert!(
            !PathBuf::from(format!("{}{suffix}", original.display())).exists(),
            "all fixture database handles must close before replacing the image"
        );
    }
    let archive = original.with_file_name("current-before-primary2-reload.sqlite");
    assert!(!archive.exists());
    fs::rename(original, &archive).unwrap();
    if let Err(error) = fs::rename(historical, original) {
        fs::rename(&archive, original).expect("restore the unchanged current fixture");
        panic!("install the historical fixture at its enrolled path: {error}");
    }
    let preserved =
        rusqlite::Connection::open_with_flags(&archive, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    assert_eq!(
        required_state_meta_value(&preserved, "schema_version").unwrap(),
        SQLITE_SCHEMA_VERSION,
        "the original image retains its stronger reader floor"
    );
}

pub(super) fn reload_primary2_host(app: &AppState) {
    let before = {
        let inner = app.inner.lock().unwrap();
        (
            inner.federation.local.clone(),
            inner.federation.peers.clone(),
            serde_json::to_value(&inner.federation.onboarding).unwrap(),
        )
    };
    // The fixture's writer and listener have been joined. Restart from a
    // separate real primary-2 image through the normal boot migration.
    let historical = primary2_fixture_copy(app);
    install_primary2_fixture(app, &historical);
    let path = app.persistence_path.as_path();
    let connection = rusqlite::Connection::open(path).unwrap();
    let encoded: String = connection
        .query_row(
            "SELECT value_json FROM app_state WHERE key='metadataState'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let primary2: Value = serde_json::from_str(&encoded).unwrap();
    for peer in primary2["federation"]["peers"]
        .as_object()
        .unwrap()
        .values()
    {
        assert_eq!(peer["profile"]["schemaVersion"], "2");
    }
    drop(connection);
    let restored = load_state(path).unwrap().unwrap();
    assert!(!restored.federation_continuity_lost);
    assert!(restored.federation.admission_quarantine.is_none());
    assert_eq!(restored.federation.local, before.0);
    assert_eq!(
        serde_json::to_value(&restored.federation.onboarding).unwrap(),
        before.2
    );
    for (id, old) in &before.1 {
        let loaded = &restored.federation.peers[id];
        assert_eq!(loaded.profile, old.profile);
        assert_eq!(loaded.identity, old.identity);
        assert_eq!(loaded.enrollment_generation, old.enrollment_generation);
        assert_eq!(loaded.pair_capability_ref, old.pair_capability_ref);
        assert_eq!(loaded.verification, old.verification);
    }
    let connection = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        required_state_meta_value(&connection, "schema_version").unwrap(),
        SQLITE_SCHEMA_VERSION
    );
    *app.inner.lock().unwrap() = restored;
}
