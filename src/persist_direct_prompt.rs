// Owns the direct-prompt permission and primary-receipt reader extensions in the existing
// MAIN schema transaction. Does not issue permission, activate an import,
// schedule work or manage the coordination store. New extension of persist.rs.

#[cfg(test)]
thread_local! {
    static TEST_DIRECT_PROMPT_FLOOR_FAULT: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
}

fn direct_prompt_floor_fault(phase: &'static str) -> Result<()> {
    #[cfg(test)]
    if TEST_DIRECT_PROMPT_FLOOR_FAULT.with(|fault| fault.get()) == Some(phase) {
        bail!("injected direct-prompt floor failure at {phase}");
    }
    #[cfg(not(test))]
    let _ = phase;
    Ok(())
}

// New receipt custody must never be silently discarded by a predecessor's
// typed session load. Inspect raw JSON before any migration/normalization.
fn reject_unversioned_direct_prompt_delivery(
    connection: &rusqlite::Connection,
    version: &str,
) -> Result<()> {
    if matches!(
        version,
        SQLITE_SCHEMA_VERSION | SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION
    ) {
        return Ok(());
    }
    fn has_association(value: &Value) -> bool {
        ["promptDeliveries", "prompt_deliveries"]
            .iter()
            .any(|field| {
                value
                    .get(*field)
                    .and_then(Value::as_object)
                    .is_some_and(|deliveries| {
                        deliveries.values().any(|receipt| {
                            receipt.as_object().is_some_and(|fields| {
                                fields.contains_key("remotePrompt")
                                    || fields.contains_key("remote_prompt")
                            })
                        })
                    })
            })
    }
    let refuse = || {
        reject_unsupported_state_schema(
            "direct-prompt delivery association under an unsupported predecessor floor",
        )
    };
    let mut rows = connection.prepare("SELECT value_json FROM sessions")?;
    let encoded = rows.query_map([], |row| {
        // Unreadable value payloads are skipped by the session loader too.
        // Keep SQL stepping/access failures fatal; preserve raw rows untouched.
        Ok(row.get_ref(0)?.as_str().ok().map(str::to_owned))
    })?;
    for row in encoded {
        // Malformed unrelated rows retain the existing loader quarantine
        // behavior. They cannot be normalized into valid association data.
        let Some(encoded) = row? else {
            continue;
        };
        if let Ok(value) = serde_json::from_str::<Value>(&encoded) {
            if has_association(&value) {
                return Err(refuse());
            }
        }
    }
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
            if value
                .get("sessions")
                .and_then(Value::as_array)
                .is_some_and(|sessions| sessions.iter().any(has_association))
            {
                return Err(refuse());
            }
        }
    }
    Ok(())
}

// The caller holds the old schema's migration transaction. Reject presence,
// including null/empty values, before typed deserialization can discard data.
fn reject_unversioned_direct_prompt_policy(
    connection: &rusqlite::Connection,
    version: &str,
) -> Result<()> {
    reject_unversioned_direct_prompt_delivery(connection, version)?;
    if matches!(
        version,
        SQLITE_SCHEMA_VERSION
            | SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION
            | SQLITE_DIRECT_PROMPT_DELIVERY_PREDECESSOR
            | SQLITE_DIRECT_PROMPT_DELIVERY_IMPORT_PREDECESSOR
    ) {
        return Ok(());
    }
    let has_policy = |value: &Value| {
        value.as_object().is_some_and(|fields| {
            fields.contains_key("promptPermissions") || fields.contains_key("prompt_permissions")
        })
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
            if value.get("federation").is_some_and(has_policy) {
                bail!("direct-prompt policy under an unsupported predecessor floor");
            }
        }
    } else {
        let invalid_units = |error: &dyn std::fmt::Display| {
            reject_unsupported_state_schema(format!(
                "persisted state metadata units are invalid: {error}"
            ))
        };
        let mut statement = connection.prepare(
            "SELECT item_key,value_json FROM state_metadata_units WHERE field_name='federation'")
            .map_err(|error| invalid_units(&error))?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| invalid_units(&error))?;
        for row in rows {
            let (key, encoded) = row.map_err(|error| invalid_units(&error))?;
            if matches!(key.as_str(), "/promptPermissions" | "/prompt_permissions")
                || (key.is_empty()
                    && has_policy(
                        &serde_json::from_str(&encoded).map_err(|error| invalid_units(&error))?,
                    ))
            {
                bail!("direct-prompt policy under an unsupported predecessor floor");
            }
        }
    }
    Ok(())
}

// Called within the existing Immediate transaction. There is no new field
// backfill: absent permission remains ungranted, and activated import history
// is carried byte-for-byte to its sole corresponding successor.
fn advance_sqlite_direct_prompt_reader_floor(connection: &rusqlite::Connection) -> Result<()> {
    let version = required_state_meta_value(connection, "schema_version")?;
    let successor = match version.as_str() {
        SQLITE_DIRECT_PROMPT_PREDECESSOR | SQLITE_DIRECT_PROMPT_DELIVERY_PREDECESSOR => {
            SQLITE_SCHEMA_VERSION
        }
        SQLITE_DIRECT_PROMPT_IMPORT_PREDECESSOR
        | SQLITE_DIRECT_PROMPT_DELIVERY_IMPORT_PREDECESSOR => {
            SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION
        }
        _ => {
            return Err(reject_unsupported_state_schema(
                "unsupported direct-prompt predecessor",
            ));
        }
    };
    validate_sqlite_state_schema_for_version(connection, &version)?;
    reject_unversioned_direct_prompt_policy(connection, &version)?;
    let previous = read_metadata_units(connection).map_err(|error| {
        reject_unsupported_state_schema(format!(
            "persisted state metadata units are invalid: {error:#}"
        ))
    })?;
    validate_no_legacy_embedded_state(connection)?;
    let generation = metadata_unit_generation(connection)?
        .checked_add(1)
        .ok_or_else(|| anyhow!("metadata generation exhausted"))?;
    direct_prompt_floor_fault("validated")?;
    connection.execute(
        "INSERT INTO meta(key,value) VALUES('metadata_units_generation',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [generation.to_string()])?;
    advance_sqlite_reader_floor(connection, &version, successor)?;
    direct_prompt_floor_fault("header")?;
    validate_current_sqlite_state_schema(connection)?;
    if read_metadata_units(connection)? != previous
        || metadata_unit_generation(connection)? != generation
    {
        bail!("direct-prompt migration changed predecessor metadata");
    }
    validate_no_legacy_embedded_state(connection)?;
    direct_prompt_floor_fault("readback")?;
    Ok(())
}
