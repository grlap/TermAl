// Owns additive persisted-image and corruption controls using disposable stores.
// Does not migrate live stores or weaken the current-generation write fence.
// Split from federation_persistence.rs for the committed-image regression.
use super::*;

fn remove_defaulted_field(state: &AppState) -> (Value, String) {
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    let encoded = read_persisted_metadata_json(&connection).unwrap().unwrap();
    let mut raw: Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(
        raw["federation"]
            .as_object_mut()
            .unwrap()
            .remove("admissionQuarantine"),
        Some(Value::Null)
    );
    let stored_image =
        json!({"domain":raw["federation"], "settings":raw["preferences"]["federation"]});
    let digest = sha256_hex(&serde_json::to_vec(&stored_image).unwrap());
    let transaction = connection.unchecked_transaction().unwrap();
    replace_metadata_units_for_test(&transaction, raw.clone());
    transaction
        .execute(
            "UPDATE meta SET value=?1 WHERE key='federation_image_digest'",
            [&digest],
        )
        .unwrap();
    transaction.commit().unwrap();
    (raw, digest)
}

#[test]
fn federation_additive_default_loads_and_normalizes_on_the_next_same_version_write() {
    let state = central();
    let (raw, old_digest) = remove_defaulted_field(&state);
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(loaded.federation.admission_quarantine, None);
    let metadata = PersistedState::from_inner(&loaded);
    assert_eq!(
        metadata.federation.version,
        raw["federation"]["version"].as_u64().unwrap()
    );
    let mut changed = metadata.clone();
    changed.preferences.federation.enabled = false;
    assert!(persist_persisted_state_to_sqlite(state.persistence_path.as_path(), &changed).is_err());
    persist_persisted_state_to_sqlite(state.persistence_path.as_path(), &metadata).unwrap();
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    let digest: String = connection
        .query_row(
            "SELECT value FROM meta WHERE key='federation_image_digest'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(digest, old_digest);
    assert!(
        federation_committed_image_matches(&connection, &federation_persist_image(&metadata))
            .unwrap()
    );
    assert!(
        load_state(state.persistence_path.as_path())
            .unwrap()
            .is_some()
    );
}

#[test]
fn federation_raw_marker_rejects_content_tampering_before_default_expansion() {
    let state = central();
    let (mut raw, _) = remove_defaulted_field(&state);
    raw["preferences"]["federation"]["enabled"] = json!(false);
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    replace_metadata_units_for_test(&connection, raw);
    assert!(load_state(state.persistence_path.as_path()).is_err());
    let metadata = PersistedState::from_inner(&state.inner.lock().unwrap());
    assert!(
        persist_persisted_state_to_sqlite(state.persistence_path.as_path(), &metadata).is_err()
    );
}

#[test]
fn federation_additive_default_still_quarantines_a_moved_store() {
    let state = central();
    remove_defaulted_field(&state);
    let root = TestTempRoot::create("federation-additive-moved");
    let moved = root.path().join("termal.sqlite");
    fs::copy(state.persistence_path.as_path(), &moved).unwrap();
    let loaded = load_state(&moved).unwrap().unwrap();
    assert_eq!(
        loaded.federation.admission_quarantine,
        Some(FederationQuarantineReason::StoreBindingChanged)
    );
    let reopened = load_state(&moved).unwrap().unwrap();
    assert_eq!(reopened.federation, loaded.federation);
}
