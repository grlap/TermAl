// Owns the lazy-activation witnesses of the import-reconciliation journal on
// disposable SQLite stores (persist_import_reconciliation.rs): predecessor
// saves stay predecessor, the one-transaction activation with its read-back
// acknowledgement before any effect, crash points, saves after activation and
// old-reader refusal. Does not cover the record rules (engram_import_
// reconciliation.rs tests) or any enrollment, hold or Ready path.
use super::*;

fn store_path(fixture: &FenceFixture) -> PathBuf {
    fixture.root.path().join("termal.sqlite")
}

fn connection(fixture: &FenceFixture) -> &rusqlite::Connection {
    fixture.cache.connection.as_ref().unwrap()
}

fn floor(fixture: &FenceFixture) -> String {
    required_state_meta_value(connection(fixture), "schema_version").unwrap()
}

/// Every persisted metadata unit of the journal field.
fn journal_units(fixture: &FenceFixture) -> Vec<(String, String)> {
    read_metadata_units_unchecked(connection(fixture))
        .unwrap()
        .into_iter()
        .filter(|((field, _), _)| field == ENGRAM_IMPORT_RECONCILIATION_FIELD)
        .map(|((_, key), (kind, _))| (key, kind))
        .collect()
}

fn intent() -> EngramImportReconciliationRecord {
    EngramImportReconciliationRecord::LocalImportFence(EngramImportLocalFence {
        schema_version: ENGRAM_IMPORT_RECONCILIATION_RECORD_VERSION,
        record_id: "activation-intent-1".to_owned(),
        scope: EngramImportReconciliationScope {
            store_association: "store-association-1".to_owned(),
            project_id: "github.com/example/project".to_owned(),
            association_id: "association-receipt-1".to_owned(),
            owner_id: None,
        },
        hold_id: "hold-1".to_owned(),
        candidate_id: "candidate-1".to_owned(),
        candidate_revision: 1,
        scope_revision: 1,
        hold_revision: 1,
        affected_scope_ref: "affected-scope-page-1".to_owned(),
        host_incarnation: "boot-1".to_owned(),
        fence_generation: 1,
        phase: EngramImportFencePhase::Installing,
        prepared_manifest_ref: None,
        attestation_ref: None,
        terminal_decision_ref: None,
    })
}

/// A fresh predecessor store that has been saved once.
fn saved_predecessor() -> FenceFixture {
    let mut fixture = FenceFixture::new();
    fixture.inner.lock().unwrap().revision = 7;
    fixture.write(&fixture.collect()).unwrap();
    assert_eq!(floor(&fixture), SQLITE_SCHEMA_VERSION);
    fixture
}

/// Activate, returning the acknowledged journal and the metadata generation
/// the activation committed, as its effect receives them.
fn activate(fixture: &FenceFixture) -> Result<(EngramImportReconciliationJournal, u64)> {
    activate_engram_import_reconciliation_sqlite(&store_path(fixture), connection(fixture),
        &EngramImportActivationCapability::for_test(), intent(), |journal, generation| (journal.clone(), generation))
}

fn with_fault<T>(phase: &'static str, body: impl FnOnce() -> T) -> T {
    TEST_ENGRAM_IMPORT_ACTIVATION_FAULT.with(|fault| fault.set(Some(phase)));
    let result = body();
    TEST_ENGRAM_IMPORT_ACTIVATION_FAULT.with(|fault| fault.set(None));
    result
}

#[test]
fn records_are_read_in_key_order() {
    let mut journal = EngramImportReconciliationJournal::activated_empty();
    journal.record(intent()).unwrap();
    let keys = journal.records().map(|(key, _)| key.to_owned()).collect::<Vec<_>>();
    assert_eq!(keys, vec!["local_import_fence/activation-intent-1"]);
    assert_eq!(journal.records().next().map(|(_, record)| record), Some(&intent()));
}

#[test]
fn the_successor_floor_is_the_predecessor_with_the_journal_suffix() {
    assert_eq!(SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION, format!("{SQLITE_SCHEMA_VERSION}-import-reconciliation-1"));
}

#[test]
fn ordinary_saves_on_a_predecessor_store_keep_its_floor_and_write_no_journal() {
    let mut fixture = saved_predecessor();
    let before = read_metadata_units_unchecked(connection(&fixture)).unwrap();
    fixture.inner.lock().unwrap().revision = 8;
    fixture.write(&fixture.collect()).unwrap();
    assert_eq!(floor(&fixture), SQLITE_SCHEMA_VERSION, "an ordinary save never activates");
    assert!(journal_units(&fixture).is_empty());
    let after = read_metadata_units_unchecked(connection(&fixture)).unwrap();
    for (key, value) in &before {
        if key.0 != "revision" {
            assert_eq!(after.get(key), Some(value), "{key:?} unchanged byte for byte");
        }
    }
    assert_eq!(after.keys().filter(|key| !before.contains_key(*key)).count(), 0, "no new units");
    assert_eq!(*fixture.inner.lock().unwrap().engram_import_reconciliation, EngramImportReconciliationState::NotActivated);
}

#[test]
fn the_predecessor_inventory_refuses_the_journal_field_and_the_successor_requires_it() {
    let fixture = saved_predecessor();
    let predecessor = read_metadata_units_unchecked(connection(&fixture)).unwrap();
    let mut with_journal = predecessor.clone();
    let mut field = serde_json::Map::new();
    field.insert(ENGRAM_IMPORT_RECONCILIATION_FIELD.to_owned(), json!({}));
    with_journal.extend(metadata_value_to_units(Value::Object(field)).unwrap());
    assert!(validate_metadata_unit_inventory(&with_journal).is_err(), "the predecessor floor forbids the field");
    assert!(validate_engram_import_successor_inventory(&predecessor).is_err(), "the successor floor requires it");
    validate_engram_import_successor_inventory(&with_journal).unwrap();
}

#[test]
fn activation_commits_the_descriptor_intent_and_floor_then_acknowledges_before_the_effect() {
    let fixture = saved_predecessor();
    let path = store_path(&fixture);
    let observed = activate_engram_import_reconciliation_sqlite(&path, connection(&fixture),
        &EngramImportActivationCapability::for_test(), intent(), |journal, generation| {
            // The effect sees the committed successor store, read back, and
            // runs with the writer lock released: a zero-wait writer is served.
            let reader = open_sqlite_state_connection_unconfigured(&path).unwrap();
            let writer_free = lock_sqlite_state_writer_for(&sqlite_state_write_lock(&path),
                std::time::Duration::ZERO).is_some();
            (journal.clone(), generation, metadata_unit_generation(&reader).unwrap(), writer_free,
                required_state_meta_value(&reader, "schema_version").unwrap(),
                read_engram_import_reconciliation_state(&reader).unwrap())
        }).unwrap();
    let (journal, generation, stored_generation, writer_free, committed_floor, committed_state) = observed;
    assert_eq!(generation, stored_generation, "the effect receives the generation the activation committed");
    assert!(writer_free, "the effect runs without the state writer lock");
    assert_eq!(committed_floor, SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION);
    assert_eq!(committed_state, EngramImportReconciliationState::Activated(Arc::new(journal.clone())));
    assert_eq!(journal.records().count(), 1);
    assert_eq!(journal_units(&fixture), vec![
        (String::new(), "object".to_owned()),
        ("/local_import_fence/activation-intent-1".to_owned(), "entry".to_owned()),
    ]);
}

#[test]
fn a_failed_acknowledgement_runs_no_effect() {
    let fixture = saved_predecessor();
    let ran = std::cell::Cell::new(false);
    let result = with_fault("ack", || activate_engram_import_reconciliation_sqlite(&store_path(&fixture),
        connection(&fixture), &EngramImportActivationCapability::for_test(), intent(), |_, _| ran.set(true)));
    assert!(result.is_err(), "an unacknowledged activation is refused");
    assert!(!ran.get(), "no effect runs without the acknowledgement");
}

#[test]
fn a_crash_at_each_activation_step_leaves_the_store_wholly_predecessor_or_wholly_successor() {
    for phase in ["begin", "after_records", "before_floor", "before_commit"] {
        let fixture = saved_predecessor();
        let before = read_metadata_units_unchecked(connection(&fixture)).unwrap();
        assert!(with_fault(phase, || activate(&fixture)).is_err(), "{phase}");
        assert_eq!(floor(&fixture), SQLITE_SCHEMA_VERSION, "{phase}: wholly predecessor");
        assert_eq!(read_metadata_units_unchecked(connection(&fixture)).unwrap(), before, "{phase}: every unit as before");
    }
    // A crash after the commit, before its acknowledgement: wholly successor,
    // and recoverable as such, never rolled back.
    let fixture = saved_predecessor();
    assert!(with_fault("ack", || activate(&fixture)).is_err());
    assert_eq!(floor(&fixture), SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION);
    assert_eq!(journal_units(&fixture).len(), 2);
    assert!(matches!(read_engram_import_reconciliation_state(connection(&fixture)).unwrap(),
        EngramImportReconciliationState::Activated(_)));
}

#[test]
fn an_ordinary_save_after_activation_preserves_the_journal_and_the_floor() {
    let mut fixture = saved_predecessor();
    let (journal, committed) = activate(&fixture).unwrap();
    adopt_engram_import_activation_locked(&mut fixture.inner.lock().unwrap(), journal, committed).unwrap();
    fixture.inner.lock().unwrap().revision = 9;
    fixture.write(&fixture.collect()).unwrap();
    assert_eq!(floor(&fixture), SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION);
    assert_eq!(journal_units(&fixture).len(), 2, "the descriptor and the intent record are kept");
}

#[test]
fn a_save_from_state_captured_before_activation_never_drops_the_journal_or_the_floor() {
    let mut fixture = saved_predecessor();
    fixture.inner.lock().unwrap().revision = 10;
    let stale = fixture.collect();
    activate(&fixture).unwrap();
    let refused = fixture.write(&stale).expect_err("a capture taken before activation stays stale and is refused");
    assert!(format!("{refused:#}").contains("stale metadata snapshot"), "refused as stale: {refused:#}");
    // A refused write invalidates the writer's cached connection, so the
    // store is read through a fresh connection.
    let reader = open_sqlite_state_connection_unconfigured(&store_path(&fixture)).unwrap();
    assert_eq!(required_state_meta_value(&reader, "schema_version").unwrap(),
        SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION, "never downgraded");
    let journal_rows = read_metadata_units_unchecked(&reader).unwrap().into_iter()
        .filter(|((field, _), _)| field == ENGRAM_IMPORT_RECONCILIATION_FIELD).count();
    assert_eq!(journal_rows, 2, "never deleted");
}

#[test]
fn adoption_takes_only_a_matching_activation_and_never_rolls_back_a_newer_generation() {
    let fixture = saved_predecessor();
    let (journal, committed) = activate(&fixture).unwrap();
    assert_eq!(committed, metadata_unit_generation(connection(&fixture)).unwrap());
    let mut inner = fixture.inner.lock().unwrap();
    // A late acknowledgement cannot replace a newer adopted generation.
    inner.metadata_predecessor.store(committed + 1, std::sync::atomic::Ordering::Release);
    assert!(adopt_engram_import_activation_locked(&mut inner, journal.clone(), committed).is_err());
    assert_eq!(*inner.engram_import_reconciliation, EngramImportReconciliationState::NotActivated);
    assert_eq!(inner.metadata_predecessor.load(std::sync::atomic::Ordering::Acquire), committed + 1);
    inner.metadata_predecessor.store(committed - 1, std::sync::atomic::Ordering::Release);
    adopt_engram_import_activation_locked(&mut inner, journal.clone(), committed).unwrap();
    // Adopting the same activation again changes nothing, not even the stamp.
    let stamp = inner.engram_import_reconciliation.stamp();
    adopt_engram_import_activation_locked(&mut inner, journal, committed).unwrap();
    assert!(inner.engram_import_reconciliation.stamp() == stamp);
    // A different journal is refused, and the adopted one stays.
    let mut other = EngramImportReconciliationJournal::activated_empty();
    let EngramImportReconciliationRecord::LocalImportFence(mut fence) = intent() else { unreachable!() };
    fence.record_id = "another-intent".to_owned();
    other.record(EngramImportReconciliationRecord::LocalImportFence(fence)).unwrap();
    assert!(adopt_engram_import_activation_locked(&mut inner, other, committed).is_err());
    assert!(matches!(&*inner.engram_import_reconciliation,
        EngramImportReconciliationState::Activated(adopted)
            if adopted.records().map(|(key, _)| key).collect::<Vec<_>>() == vec!["local_import_fence/activation-intent-1"]));
}

#[test]
fn a_second_activation_is_refused_and_changes_nothing() {
    let fixture = saved_predecessor();
    activate(&fixture).unwrap();
    let before = read_metadata_units_unchecked(connection(&fixture)).unwrap();
    assert!(activate(&fixture).is_err());
    assert_eq!(read_metadata_units_unchecked(connection(&fixture)).unwrap(), before);
}

#[test]
fn a_predecessor_only_reader_refuses_the_successor_floor() {
    // Frozen copy of the predecessor build's accepted storage versions
    // (persisted_metadata_storage_version at the predecessor floor).
    let predecessor_accepts = |version: &str| matches!(version,
        "2" | "2-metadata-units-1" | "2-metadata-units-2"
            | "2-metadata-units-2-handoff-1"
            | "2-metadata-units-2-handoff-1-unadmitted-outbox-1"
            | "2-metadata-units-2-handoff-1-unadmitted-outbox-1-obligations-1"
            | "2-metadata-units-2-handoff-1-unadmitted-outbox-1-obligations-1-checkpoint-result-1");
    let fixture = saved_predecessor();
    activate(&fixture).unwrap();
    let version = floor(&fixture);
    assert_eq!(version, SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION);
    assert!(!predecessor_accepts(&version), "the predecessor build refuses this floor before any effect");
    assert!(predecessor_accepts(SQLITE_SCHEMA_VERSION), "control: it accepts its own floor");
    // The successor build loads it, with the journal established.
    let loaded = load_state(&store_path(&fixture)).unwrap().unwrap();
    assert!(matches!(*loaded.engram_import_reconciliation, EngramImportReconciliationState::Activated(_)));
}

/// Turn a saved predecessor store into a successor store holding exactly
/// `record`, written as raw unit rows, independently of activation.
fn install_raw_successor(fixture: &FenceFixture, record: &Value) {
    let mut journal = serde_json::Map::new();
    journal.insert("local_import_fence/activation-intent-1".to_owned(), record.clone());
    let mut field = serde_json::Map::new();
    field.insert(ENGRAM_IMPORT_RECONCILIATION_FIELD.to_owned(), Value::Object(journal));
    let units = metadata_value_to_units(Value::Object(field)).unwrap();
    let connection = connection(fixture);
    for ((field, key), (kind, encoded)) in units {
        connection.execute(
            "INSERT INTO state_metadata_units(field_name,item_key,kind,value_json) VALUES(?1,?2,?3,?4)",
            rusqlite::params![field, key, kind, encoded]).unwrap();
    }
    connection.execute("UPDATE meta SET value=?1 WHERE key='schema_version' AND value=?2",
        rusqlite::params![SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION, SQLITE_SCHEMA_VERSION]).unwrap();
}

#[test]
fn a_corrupt_or_future_journal_refuses_load_without_reset() {
    // Control: a valid raw successor store loads, with its journal established.
    let valid = saved_predecessor();
    install_raw_successor(&valid, &serde_json::to_value(intent()).unwrap());
    let loaded = load_state(&store_path(&valid)).expect("a valid successor store loads").unwrap();
    assert!(matches!(*loaded.engram_import_reconciliation, EngramImportReconciliationState::Activated(_)));

    let fixture = saved_predecessor();
    let mut future = serde_json::to_value(intent()).unwrap();
    future["schemaVersion"] = json!(ENGRAM_IMPORT_RECONCILIATION_RECORD_VERSION + 1);
    install_raw_successor(&fixture, &future);
    let before = read_metadata_units_unchecked(connection(&fixture)).unwrap();
    assert!(load_state(&store_path(&fixture)).is_err(), "a journal it cannot establish refuses the load");
    assert_eq!(read_metadata_units_unchecked(connection(&fixture)).unwrap(), before, "nothing is reset");
    assert_eq!(floor(&fixture), SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION);
}
