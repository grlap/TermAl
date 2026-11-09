// Owns the lazy MAIN-state activation of the host import-reconciliation
// journal: its successor reader floor, the per-floor metadata inventory of
// its field, the writer's floor rule, the one-transaction first activation
// with a content read-back acknowledgement, and the strict read of an
// activated store. Does not own the journal's records or rules
// (engram_import_reconciliation.rs), any enrollment, hold or Ready path, or
// the coordination store, which it never touches. Open, load and ordinary
// saves never activate it.

/// The successor of the current MAIN reader floor: the predecessor's exact
/// inventory plus the required import-reconciliation journal field.
const SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION: &str =
    "2-metadata-units-2-handoff-1-unadmitted-outbox-1-obligations-1-checkpoint-result-1-native-test-completion-1-direct-prompt-1-import-reconciliation-1";
const ENGRAM_IMPORT_RECONCILIATION_FIELD: &str = "engramImportReconciliation";

/// The authority to make the first activation. Only a real enrollment or
/// hold intent may construct one; this child constructs it in tests alone.
#[cfg_attr(not(test), allow(dead_code))]
struct EngramImportActivationCapability {
    _private: (),
}

#[cfg_attr(not(test), allow(dead_code))]
impl EngramImportActivationCapability {
    #[cfg(test)]
    fn for_test() -> Self {
        Self { _private: () }
    }
}

#[cfg(test)]
thread_local! {
    /// The activation phase at which a test makes activation fail, as a
    /// crash or a failed write would at that point.
    static TEST_ENGRAM_IMPORT_ACTIVATION_FAULT: std::cell::Cell<Option<&'static str>>
        = const { std::cell::Cell::new(None) };
}

#[cfg_attr(not(test), allow(dead_code))]
fn engram_import_activation_fault(phase: &'static str) -> Result<()> {
    #[cfg(test)]
    if TEST_ENGRAM_IMPORT_ACTIVATION_FAULT.with(|fault| fault.get()) == Some(phase) {
        bail!("injected import-reconciliation activation fault at `{phase}`");
    }
    #[cfg(not(test))]
    let _ = phase;
    Ok(())
}

/// Whether the store's floor is the import-reconciliation successor. A store
/// still initializing, with no recorded floor yet, is not.
fn engram_import_reconciliation_floor_is_successor(connection: &rusqlite::Connection) -> Result<bool> {
    let version: Option<String> = connection
        .query_row("SELECT value FROM meta WHERE key='schema_version'", [], |row| row.get(0))
        .optional()
        .context("failed to read the state reader floor")?;
    Ok(matches!(version.as_deref(), Some(SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION
        | SQLITE_NATIVE_TEST_COMPLETION_IMPORT_PREDECESSOR | SQLITE_DIRECT_PROMPT_IMPORT_PREDECESSOR)))
}

/// The successor floor's inventory: the predecessor's exact fields plus the
/// journal's object descriptor.
fn validate_engram_import_successor_inventory(units: &PersistMetadataUnits) -> Result<()> {
    match units.get(&(ENGRAM_IMPORT_RECONCILIATION_FIELD.to_owned(), String::new())) {
        Some((kind, _)) if kind == "object" => {}
        Some(_) => bail!("invalid metadata descriptor kind for `{ENGRAM_IMPORT_RECONCILIATION_FIELD}`"),
        None => bail!("missing required metadata descriptor `{ENGRAM_IMPORT_RECONCILIATION_FIELD}`"),
    }
    let predecessor = units
        .iter()
        .filter(|((field, _), _)| field != ENGRAM_IMPORT_RECONCILIATION_FIELD)
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<PersistMetadataUnits>();
    validate_metadata_unit_inventory(&predecessor)
}

/// The current inventory of the store's own floor: predecessor or successor.
fn validate_metadata_unit_inventory_for_store(
    connection: &rusqlite::Connection,
    units: &PersistMetadataUnits,
) -> Result<()> {
    if engram_import_reconciliation_floor_is_successor(connection)? {
        if persisted_metadata_storage_version(connection)? == SQLITE_NATIVE_TEST_COMPLETION_IMPORT_PREDECESSOR {
            validate_native_test_predecessor_inventory(units, true)
        } else { validate_engram_import_successor_inventory(units) }
    } else if matches!(persisted_metadata_storage_version(connection)?.as_str(),
        SQLITE_NATIVE_TEST_COMPLETION_PREDECESSOR | SQLITE_CHECKPOINT_RESULT_PREDECESSOR) {
        validate_metadata_unit_inventory_for_floor(units, true)
    } else {
        validate_metadata_unit_inventory(units)
    }
}

/// The writer's rule, applied inside its writer transaction from the floor on
/// disk, never from an in-memory flag: under the predecessor floor no journal
/// unit may be written; under the successor floor a capture that does not
/// carry the journal (unchanged, or taken before activation) keeps the
/// committed journal units, so a save never drops or downgrades them.
fn engram_import_reconciliation_writer_floor(
    connection: &rusqlite::Connection,
    previous: &PersistMetadataUnits,
    desired: &mut PersistMetadataUnits,
) -> Result<()> {
    let projected = desired.keys().any(|(field, _)| field == ENGRAM_IMPORT_RECONCILIATION_FIELD);
    if !engram_import_reconciliation_floor_is_successor(connection)? {
        if projected {
            bail!("an import-reconciliation journal cannot be written under the predecessor reader floor");
        }
        return Ok(());
    }
    if !projected {
        for ((field, key), value) in previous {
            if field == ENGRAM_IMPORT_RECONCILIATION_FIELD {
                desired.insert((field.clone(), key.clone()), value.clone());
            }
        }
    }
    Ok(())
}

/// The journal's metadata units, one member per record.
fn engram_import_reconciliation_units(journal: &EngramImportReconciliationJournal) -> Result<PersistMetadataUnits> {
    let mut field = serde_json::Map::new();
    field.insert(ENGRAM_IMPORT_RECONCILIATION_FIELD.to_owned(), journal.to_value()?);
    metadata_value_to_units(Value::Object(field))
}

/// Strictly decode a persisted journal field into its activated state.
fn deserialize_engram_import_reconciliation<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<PersistTracked<EngramImportReconciliationState>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    let journal = EngramImportReconciliationJournal::decode(&value).map_err(serde::de::Error::custom)?;
    Ok(EngramImportReconciliationState::Activated(Arc::new(journal)).into())
}

/// The store's import-reconciliation state, read under the caller's
/// connection: `NotActivated` on the predecessor floor, the strictly decoded
/// journal on the successor floor.
#[cfg_attr(not(test), allow(dead_code))]
fn read_engram_import_reconciliation_state(
    connection: &rusqlite::Connection,
) -> Result<EngramImportReconciliationState> {
    if !engram_import_reconciliation_floor_is_successor(connection)? {
        let present: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM state_metadata_units WHERE field_name=?1)",
            [ENGRAM_IMPORT_RECONCILIATION_FIELD], |row| row.get(0))?;
        if present {
            bail!("an import-reconciliation journal is present under the predecessor reader floor");
        }
        return Ok(EngramImportReconciliationState::NotActivated);
    }
    let value = read_persisted_metadata_field(connection, ENGRAM_IMPORT_RECONCILIATION_FIELD)?
        .ok_or_else(|| anyhow!("the successor floor is missing its import-reconciliation journal"))?;
    Ok(EngramImportReconciliationState::Activated(Arc::new(
        EngramImportReconciliationJournal::decode(&value)?,
    )))
}

/// Adopt a committed, acknowledged activation into the live state, under its
/// lock and with no I/O: the activated journal and the metadata generation
/// the activation committed, so the next capture is not refused as stale and
/// an ordinary save carries the journal. The caller does this first in the
/// activation's effect, with the generation that effect receives. Only a matching activation is
/// adopted: a different journal already adopted is refused, and a late
/// acknowledgement never rolls back a newer generation. Captures taken
/// before it stay stale; pending changes stay pending, since only the
/// durable predecessor moves.
#[cfg_attr(not(test), allow(dead_code))]
fn adopt_engram_import_activation_locked(
    inner: &mut StateInner,
    journal: EngramImportReconciliationJournal,
    committed_generation: u64,
) -> Result<()> {
    let identical = match &*inner.engram_import_reconciliation {
        EngramImportReconciliationState::NotActivated => false,
        EngramImportReconciliationState::Activated(adopted) if **adopted == journal => true,
        EngramImportReconciliationState::Activated(_) => {
            bail!("a different import-reconciliation journal is already adopted")
        }
    };
    let adopted_generation = inner.metadata_predecessor.load(std::sync::atomic::Ordering::Acquire);
    if adopted_generation > committed_generation {
        bail!(
            "a late import-reconciliation acknowledgement for generation {committed_generation} \
             cannot replace the newer adopted generation {adopted_generation}"
        );
    }
    if !identical {
        *inner.engram_import_reconciliation = EngramImportReconciliationState::Activated(Arc::new(journal));
    }
    inner.metadata_predecessor.store(committed_generation, std::sync::atomic::Ordering::Release);
    Ok(())
}

/// The first activation: commit the journal descriptor, the activation
/// intent record and the successor floor in one immediate transaction under
/// the state writer lock, read that exact committed subset and its metadata
/// generation back under the same lock, release the lock, and only then run
/// `effect` with the journal and that generation, which is what
/// `adopt_engram_import_activation_locked` takes. The effect runs without
/// the writer lock, so it may persist. A failure before the commit leaves
/// the store wholly predecessor; a failure after it leaves it wholly
/// successor and runs no effect. An already activated store refuses,
/// unchanged.
#[cfg_attr(not(test), allow(dead_code))]
fn activate_engram_import_reconciliation_sqlite<R>(
    path: &FsPath,
    connection: &rusqlite::Connection,
    _capability: &EngramImportActivationCapability,
    intent: EngramImportReconciliationRecord,
    effect: impl FnOnce(&EngramImportReconciliationJournal, u64) -> R,
) -> Result<R> {
    let mut journal = EngramImportReconciliationJournal::activated_empty();
    journal.record(intent)?;
    let units = engram_import_reconciliation_units(&journal)?;
    let write_lock = sqlite_state_write_lock(path);
    let writer = lock_sqlite_state_writer(&write_lock);
    engram_import_activation_fault("begin")?;
    let transaction =
        rusqlite::Transaction::new_unchecked(connection, rusqlite::TransactionBehavior::Immediate)
            .context("failed to begin import-reconciliation activation")?;
    let version = required_state_meta_value(&transaction, "schema_version")?;
    if version != SQLITE_SCHEMA_VERSION {
        bail!("import-reconciliation activation needs the predecessor floor, found `{version}`");
    }
    validate_current_sqlite_state_schema(&transaction)?;
    let mut prospective = read_metadata_units_unchecked(&transaction)?;
    validate_metadata_unit_inventory(&prospective)?;
    for ((field, key), (kind, encoded)) in &units {
        transaction.execute(
            "INSERT INTO state_metadata_units(field_name,item_key,kind,value_json) VALUES(?1,?2,?3,?4)",
            rusqlite::params![field, key, kind, encoded],
        )?;
    }
    prospective.extend(units);
    engram_import_activation_fault("after_records")?;
    validate_engram_import_successor_inventory(&prospective)?;
    engram_import_activation_fault("before_floor")?;
    advance_sqlite_reader_floor(&transaction, SQLITE_SCHEMA_VERSION, SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION)?;
    let generation = metadata_unit_generation(&transaction)?
        .checked_add(1)
        .ok_or_else(|| anyhow!("metadata generation exhausted"))?;
    transaction.execute(
        "INSERT INTO meta(key,value) VALUES('metadata_units_generation',?1)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [generation.to_string()],
    )?;
    engram_import_activation_fault("before_commit")?;
    transaction.commit().context("failed to commit import-reconciliation activation")?;
    persist_observe_metadata_commit();
    // The acknowledgement is the committed content itself, read back from
    // storage, never the write's own success.
    engram_import_activation_fault("ack")?;
    let committed = read_engram_import_reconciliation_state(connection)?;
    if committed != EngramImportReconciliationState::Activated(Arc::new(journal.clone())) {
        bail!("the committed import-reconciliation journal does not match its activation");
    }
    // Still under the writer lock, so no later write can have moved it.
    let committed_generation = metadata_unit_generation(connection)?;
    if committed_generation != generation {
        bail!("the committed metadata generation does not match the import-reconciliation activation");
    }
    drop(writer);
    Ok(effect(&journal, committed_generation))
}
