// Extends the existing MAIN reader floor before typed registration loading.
// Custody stays inside existing registrations and compacted survivors.

#[cfg(test)]
thread_local! {
    static TEST_REQUEST_CUSTODY_FLOOR_FAULT: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
}

fn test_request_custody_floor_fault(phase: &'static str) -> Result<()> {
    #[cfg(test)]
    if TEST_REQUEST_CUSTODY_FLOOR_FAULT.with(|fault| fault.get()) == Some(phase) {
        bail!("injected test-request custody migration failure at {phase}");
    }
    #[cfg(not(test))]
    let _ = phase;
    Ok(())
}

fn test_request_custody_key_present(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.iter().any(|(key, value)| {
            matches!(key.as_str(), "causalRequest" | "causal_request")
                || test_request_custody_key_present(value)
        }),
        Value::Array(values) => values.iter().any(test_request_custody_key_present),
        _ => false,
    }
}

fn test_request_custody_metadata_field(field: &str) -> bool {
    matches!(
        field,
        "testRunRegistrations"
            | "testRunCompacted"
            | "testRunWaits"
            | "remoteTestRunFacts"
            | "remote_test_run_facts"
    )
}

// Presence, including null, is rejected while the old header is still held.
// Inspect only custody-bearing metadata; ordinary session/message JSON is not
// interpreted as registration authority.
fn reject_unversioned_test_request_custody(
    connection: &rusqlite::Connection,
    version: &str,
) -> Result<()> {
    if matches!(
        version,
        SQLITE_SCHEMA_VERSION | SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION
    ) {
        return Ok(());
    }
    let refuse = || {
        reject_unsupported_state_schema(
            "test-request custody under an unsupported predecessor reader floor",
        )
    };
    if version == "2" {
        let encoded: Option<String> = connection
            .query_row(
                "SELECT value_json FROM app_state WHERE key=?1",
                [SQLITE_METADATA_KEY],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(encoded) = encoded {
            let value: Value = serde_json::from_str(&encoded).map_err(|error| {
                reject_unsupported_state_schema(format!(
                    "persisted state metadata is not valid JSON: {error}"
                ))
            })?;
            if value.as_object().is_some_and(|fields| {
                fields.iter().any(|(field, value)| {
                    test_request_custody_metadata_field(field)
                        && test_request_custody_key_present(value)
                })
            }) {
                return Err(refuse());
            }
        }
        return Ok(());
    }
    let invalid_units = |error: &dyn std::fmt::Display| {
        reject_unsupported_state_schema(format!(
            "persisted state metadata units are invalid: {error}"
        ))
    };
    let mut statement = connection.prepare(
        "SELECT field_name,value_json FROM state_metadata_units WHERE field_name IN
        ('testRunRegistrations','testRunCompacted','testRunWaits','remoteTestRunFacts','remote_test_run_facts')")
        .map_err(|error| invalid_units(&error))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| invalid_units(&error))?;
    for row in rows {
        let (_, encoded) = row.map_err(|error| invalid_units(&error))?;
        let value: Value = serde_json::from_str(&encoded).map_err(|error| invalid_units(&error))?;
        if test_request_custody_key_present(&value) {
            return Err(refuse());
        }
    }
    Ok(())
}

fn advance_sqlite_test_request_custody_reader_floor(
    connection: &rusqlite::Connection,
) -> Result<()> {
    let version = required_state_meta_value(connection, "schema_version")?;
    let successor = match version.as_str() {
        SQLITE_TEST_REQUEST_CUSTODY_PREDECESSOR => SQLITE_SCHEMA_VERSION,
        SQLITE_TEST_REQUEST_CUSTODY_IMPORT_PREDECESSOR => {
            SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION
        }
        _ => {
            return Err(reject_unsupported_state_schema(
                "unsupported test-request predecessor",
            ));
        }
    };
    validate_sqlite_state_schema_for_version(connection, &version)?;
    reject_unversioned_test_request_custody(connection, &version)?;
    let previous = read_metadata_units(connection)?;
    validate_no_legacy_embedded_state(connection)?;
    let generation = metadata_unit_generation(connection)?
        .checked_add(1)
        .ok_or_else(|| anyhow!("metadata generation exhausted"))?;
    test_request_custody_floor_fault("main_validated")?;
    connection.execute(
        "UPDATE meta SET value=?1 WHERE key='metadata_units_generation'",
        [generation.to_string()],
    )?;
    advance_sqlite_reader_floor(connection, &version, successor)?;
    test_request_custody_floor_fault("main_header")?;
    validate_current_sqlite_state_schema(connection)?;
    if read_metadata_units(connection)? != previous
        || metadata_unit_generation(connection)? != generation
    {
        bail!("test-request migration changed predecessor metadata");
    }
    validate_no_legacy_embedded_state(connection)?;
    test_request_custody_floor_fault("main_readback")?;
    Ok(())
}

#[cfg(test)]
#[path = "tests/test_request_custody_reader_floor.rs"]
mod test_request_custody_reader_floor_tests;
