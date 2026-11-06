// Extends the existing MAIN reader-floor owner. No live migration command,
// new store, source measurement or verification authority is introduced.
const NATIVE_TEST_FACTS_FIELD: &str = "remoteTestRunFacts";

#[cfg(test)]
thread_local! {
    static TEST_NATIVE_TEST_FLOOR_FAULT: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
}

fn native_test_floor_fault(phase: &'static str) -> Result<()> {
    #[cfg(test)]
    if TEST_NATIVE_TEST_FLOOR_FAULT.with(|fault| fault.get()) == Some(phase) {
        bail!("injected native completion reader-floor fault at {phase}");
    }
    #[cfg(not(test))]
    let _ = phase;
    Ok(())
}

fn native_test_registration_has_target_keys(value: &Value) -> bool {
    value.as_object().is_some_and(|row|
        ["nativeOrigin", "nativeCompletion", "native_origin", "native_completion"]
            .iter().any(|key| row.contains_key(*key)))
}

fn native_test_wait_has_target_keys(value: &Value) -> bool {
    value.get("runs").and_then(Value::as_array).is_some_and(|runs| runs.iter().any(|run|
        run.as_object().is_some_and(|label| label.contains_key("remote"))))
}

fn native_test_field_has_target_keys(field: &str, value: &Value, entry: bool) -> bool {
    match field {
        "remoteTestRunFacts" | "remote_test_run_facts" => true,
        "testRunRegistrations" => if entry { native_test_registration_has_target_keys(value) }
            else { value.as_object().is_some_and(|rows| rows.values().any(native_test_registration_has_target_keys)) },
        "testRunCompacted" => {
            let has = |row: &Value| row.get("registration").is_some_and(native_test_registration_has_target_keys);
            if entry { has(value) } else { value.as_object().is_some_and(|rows| rows.values().any(has)) }
        }
        "testRunWaits" => if entry { native_test_wait_has_target_keys(value) }
            else { value.as_array().is_some_and(|waits| waits.iter().any(native_test_wait_has_target_keys)) },
        _ => false,
    }
}

// This runs under the migration owner's locked OLD header, before its typed
// load/normalization. Presence is refused even for null/empty target keys.
fn reject_unversioned_native_test_custody(connection: &rusqlite::Connection, version: &str) -> Result<()> {
    if version == SQLITE_SCHEMA_VERSION || version == SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION {
        return Ok(());
    }
    if version == "2" {
        let encoded: Option<String> = connection.query_row(
            "SELECT value_json FROM app_state WHERE key=?1", [SQLITE_METADATA_KEY], |row| row.get(0)).optional()?;
        if let Some(encoded) = encoded {
            let value: Value = serde_json::from_str(&encoded).map_err(|error| {
                reject_unsupported_state_schema(format!(
                    "persisted state metadata is not valid JSON: {error}"
                ))
            })?;
            if value.as_object().is_some_and(|fields| fields.iter().any(|(field, value)|
                native_test_field_has_target_keys(field, value, false))) {
                bail!("native test completion custody under an unsupported predecessor floor");
            }
        }
        return Ok(());
    }
    let invalid_units = |error: &dyn std::fmt::Display| {
        reject_unsupported_state_schema(format!("persisted state metadata units are invalid: {error}"))
    };
    let mut statement = connection.prepare(
        "SELECT field_name,item_key,value_json FROM state_metadata_units
         WHERE field_name IN ('remoteTestRunFacts','remote_test_run_facts','testRunRegistrations','testRunCompacted','testRunWaits')")
        .map_err(|error| invalid_units(&error))?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))
        .map_err(|error| invalid_units(&error))?;
    for row in rows {
        let (field, key, encoded) = row.map_err(|error| invalid_units(&error))?;
        let value: Value = serde_json::from_str(&encoded).map_err(|error| invalid_units(&error))?;
        if native_test_field_has_target_keys(&field, &value, !key.is_empty()) {
            bail!("native test completion custody under an unsupported predecessor floor");
        }
    }
    Ok(())
}

fn validate_native_test_predecessor_inventory(units: &PersistMetadataUnits, activated: bool) -> Result<()> {
    if !activated { return validate_metadata_unit_inventory_for_floor(units, true); }
    match units.get(&(ENGRAM_IMPORT_RECONCILIATION_FIELD.to_owned(), String::new())) {
        Some((kind, _)) if kind == "object" => {}
        _ => bail!("missing or invalid predecessor import journal descriptor"),
    }
    let ordinary = units.iter().filter(|((field, _), _)| field != ENGRAM_IMPORT_RECONCILIATION_FIELD)
        .map(|(key, value)| (key.clone(), value.clone())).collect();
    validate_metadata_unit_inventory_for_floor(&ordinary, true)?;
    let value = metadata_units_to_value(units)?;
    EngramImportReconciliationJournal::decode(&value[ENGRAM_IMPORT_RECONCILIATION_FIELD])?;
    Ok(())
}

// Called inside each existing predecessor transaction AFTER its older ordered
// steps, so a new field is never emitted under an intermediate frozen floor.
fn advance_sqlite_native_test_completion_reader_floor(connection: &rusqlite::Connection) -> Result<()> {
    let version = required_state_meta_value(connection, "schema_version")?;
    if matches!(version.as_str(), SQLITE_SCHEMA_VERSION | SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION) {
        validate_current_sqlite_state_schema(connection)?;
        read_metadata_units(connection)?;
        return Ok(());
    }
    let activated = match version.as_str() {
        SQLITE_NATIVE_TEST_COMPLETION_PREDECESSOR => false,
        SQLITE_NATIVE_TEST_COMPLETION_IMPORT_PREDECESSOR => true,
        _ => return Err(reject_unsupported_state_schema("unsupported native completion predecessor")),
    };
    validate_sqlite_state_schema_for_version(connection, &version)?;
    reject_unversioned_native_test_custody(connection, &version)?;
    let previous = read_metadata_units_unchecked(connection)?;
    validate_native_test_predecessor_inventory(&previous, activated)?;
    metadata_units_to_value(&previous)?;
    validate_no_legacy_embedded_state(connection)?
        .ok_or_else(|| reject_unsupported_state_schema("missing predecessor metadata authority"))?;
    native_test_floor_fault("validated")?;
    connection.execute(
        "INSERT INTO state_metadata_units(field_name,item_key,kind,value_json) VALUES(?1,'','object','0')",
        [NATIVE_TEST_FACTS_FIELD])?;
    native_test_floor_fault("descriptor")?;
    let generation = metadata_unit_generation(connection)?.checked_add(1)
        .ok_or_else(|| anyhow!("metadata generation exhausted"))?;
    connection.execute(
        "INSERT INTO meta(key,value) VALUES('metadata_units_generation',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [generation.to_string()])?;
    let successor = if activated { SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION } else { SQLITE_SCHEMA_VERSION };
    advance_sqlite_reader_floor(connection, &version, successor)?;
    native_test_floor_fault("header")?;
    validate_current_sqlite_state_schema(connection)?;
    let actual = read_metadata_units(connection)?;
    let mut expected = previous;
    expected.insert((NATIVE_TEST_FACTS_FIELD.to_owned(), String::new()), ("object".to_owned(), "0".to_owned()));
    if actual != expected || metadata_unit_generation(connection)? != generation {
        bail!("native completion migration read-back differs from its exact extension");
    }
    validate_no_legacy_embedded_state(connection)?;
    native_test_floor_fault("readback")?;
    Ok(())
}

fn prepare_sqlite_native_test_completion_reader_floor(connection: &rusqlite::Connection, for_load: bool) -> Result<Option<PersistedState>> {
    let transaction = rusqlite::Transaction::new_unchecked(connection, rusqlite::TransactionBehavior::Immediate)?;
    advance_sqlite_native_test_completion_reader_floor(&transaction)?;
    let persisted = if for_load { validate_no_legacy_embedded_state(&transaction)? }
        else { validate_sqlite_obligation_metadata(&transaction)?; None };
    transaction.commit()?;
    persist_observe_metadata_commit();
    Ok(persisted)
}

#[cfg(test)]
#[path = "tests/native_test_completion_reader_floor.rs"]
mod native_test_completion_reader_floor_tests;
