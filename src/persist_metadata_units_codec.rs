// Metadata unit shape, bounded inventory and validated read projections.

const METADATA_UNIT_FIELDS: &[&str] = &[
    "federation",
    "preferences",
    "revision",
    "nextSessionNumber",
    "nextMessageNumber",
    "projects",
    "engramRetiredWorkAuthorityGrants",
    "engramWorkSourceRoots",
    "engramNamedRootJournal",
    "engramWorkNamingHistory",
    "engramNamedRootReleaseAudit",
    "engramSourceSightings",
    "engramSourceRootGeneration",
    "pendingCoordinationScopeDeletions",
    "pendingResponseBoardProjectDetachments",
    "ignoredDiscoveredCodexThreadIds",
    "orchestratorInstances",
    "delegationWaits",
    "testRunCardsEpoch",
    "testRunCards",
    "testRunRegistrations",
    "testRunObservations",
    "testRunCompacted",
    "testRunWaits",
    "workspaceLayouts",
    "sessions",
    "codex",
];

fn project_metadata_units(metadata: &PersistedState) -> Result<PersistMetadataUnits> {
    // Project each field independently. There is no serialized global document
    // on the write path; collection members become independent durable rows.
    let mut units = BTreeMap::new();
    macro_rules! field {
        ($name:literal, $value:expr) => {{
            if !metadata.metadata_collection_is_omitted($name) {
                let mut one = serde_json::Map::new();
                one.insert($name.to_owned(), serde_json::to_value($value)?);
                units.extend(metadata_value_to_units(Value::Object(one))?);
            }
        }};
    }
    field!("federation", &metadata.federation);
    field!("preferences", &metadata.preferences);
    field!("revision", &metadata.revision);
    field!("nextSessionNumber", &metadata.next_session_number);
    field!("nextMessageNumber", &metadata.next_message_number);
    field!("projects", &metadata.projects);
    field!(
        "engramRetiredWorkAuthorityGrants",
        &metadata.engram_retired_work_authority_grants
    );
    field!("engramWorkSourceRoots", &metadata.engram_work_source_roots);
    field!(
        "engramNamedRootJournal",
        &metadata.engram_named_root_journal
    );
    field!(
        "engramWorkNamingHistory",
        &metadata.engram_work_naming_history
    );
    field!(
        "engramNamedRootReleaseAudit",
        &metadata.engram_named_root_release_audit
    );
    field!("engramSourceSightings", &metadata.engram_source_sightings);
    field!(
        "engramSourceRootGeneration",
        &metadata.engram_source_root_generation
    );
    field!(
        "pendingCoordinationScopeDeletions",
        &metadata.pending_coordination_scope_deletions
    );
    field!(
        "pendingResponseBoardProjectDetachments",
        &metadata.pending_response_board_project_detachments
    );
    field!(
        "ignoredDiscoveredCodexThreadIds",
        &metadata.ignored_discovered_codex_thread_ids
    );
    field!("orchestratorInstances", &metadata.orchestrator_instances);
    field!("delegationWaits", &metadata.delegation_waits);
    field!("testRunCardsEpoch", &metadata.test_run_cards_epoch);
    field!("testRunCards", &metadata.test_run_cards);
    field!("testRunRegistrations", &metadata.test_run_registrations);
    field!("testRunObservations", &metadata.test_run_observations);
    field!("testRunCompacted", &metadata.test_run_compacted);
    field!("testRunWaits", &metadata.test_run_waits);
    field!("workspaceLayouts", &metadata.workspace_layouts);
    field!("sessions", &metadata.sessions);
    field!("codex", &metadata.codex);
    Ok(units)
}

fn metadata_value_to_units(value: Value) -> Result<PersistMetadataUnits> {
    let Value::Object(fields) = value else {
        return Err(anyhow!("persisted metadata must be an object"));
    };
    let mut units = BTreeMap::new();
    for (field, value) in fields {
        match value {
            Value::Object(entries) => {
                units.insert(
                    (field.clone(), String::new()),
                    ("object".into(), entries.len().to_string()),
                );
                for (key, value) in entries {
                    units.insert(
                        (field.clone(), format!("/{key}")),
                        ("entry".into(), serde_json::to_string(&value)?),
                    );
                }
            }
            Value::Array(entries) => {
                units.insert(
                    (field.clone(), String::new()),
                    ("array".into(), entries.len().to_string()),
                );
                for (index, value) in entries.into_iter().enumerate() {
                    units.insert(
                        (field.clone(), format!("/{index:020}")),
                        ("entry".into(), serde_json::to_string(&value)?),
                    );
                }
            }
            value => {
                units.insert(
                    (field, String::new()),
                    ("value".into(), serde_json::to_string(&value)?),
                );
            }
        }
    }
    Ok(units)
}

fn read_metadata_units(connection: &rusqlite::Connection) -> Result<PersistMetadataUnits> {
    let units = read_metadata_units_unchecked(connection)?;
    if units.is_empty() && metadata_units_are_uninitialized(connection)? {
        return Ok(units);
    }
    validate_metadata_unit_inventory(&units)?;
    metadata_units_to_value(&units)?;
    Ok(units)
}

fn validate_metadata_unit_inventory(units: &PersistMetadataUnits) -> Result<()> {
    for field in METADATA_UNIT_FIELDS {
        let Some((kind, encoded)) = units.get(&(field.to_string(), String::new())) else {
            return Err(anyhow!("missing required metadata descriptor `{field}`"));
        };
        let expected = match *field {
            "revision"
            | "nextSessionNumber"
            | "nextMessageNumber"
            | "engramSourceRootGeneration"
            | "testRunCardsEpoch" => "value",
            "federation"
            | "preferences"
            | "pendingResponseBoardProjectDetachments"
            | "testRunCards"
            | "testRunRegistrations"
            | "testRunObservations"
            | "testRunCompacted"
            | "workspaceLayouts"
            | "codex" => "object",
            _ => "array",
        };
        if kind != expected {
            return Err(anyhow!("invalid metadata descriptor kind for `{field}`"));
        }
        if kind == "value" {
            if *field == "testRunCardsEpoch" {
                let _: Option<String> = serde_json::from_str(encoded)?;
            } else {
                let _: u64 = serde_json::from_str(encoded)?;
            }
        } else {
            let _: usize = serde_json::from_str(encoded)?;
        }
    }
    if units
        .keys()
        .any(|(field, _)| !METADATA_UNIT_FIELDS.contains(&field.as_str()))
    {
        return Err(anyhow!("unsupported metadata field"));
    }
    Ok(())
}

fn metadata_units_are_uninitialized(connection: &rusqlite::Connection) -> Result<bool> {
    if metadata_unit_generation(connection)? != 0 {
        return Ok(false);
    }
    let has_provenance: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM app_state WHERE key=?1)",
        [SQLITE_METADATA_KEY],
        |row| row.get(0),
    )?;
    Ok(!has_provenance && first_normalized_state_authority_table_with_rows(connection)?.is_none())
}

fn read_metadata_units_unchecked(
    connection: &rusqlite::Connection,
) -> Result<PersistMetadataUnits> {
    let mut statement = connection.prepare(
        "SELECT field_name, item_key, kind, value_json FROM state_metadata_units
         ORDER BY field_name, item_key",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
            (row.get::<_, String>(2)?, row.get::<_, String>(3)?),
        ))
    })?;
    rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()
        .context("failed to read normalized metadata units")
}

fn metadata_units_to_value(units: &PersistMetadataUnits) -> Result<Value> {
    let mut fields = serde_json::Map::new();
    let mut counts = BTreeMap::new();
    for ((field, key), (kind, encoded)) in units {
        if !key.is_empty() {
            continue;
        }
        if kind == "object" || kind == "array" {
            let count: usize =
                serde_json::from_str(encoded).context("invalid metadata collection count")?;
            counts.insert(field, count);
        }
        let value = match kind.as_str() {
            "object" => Value::Object(serde_json::Map::new()),
            "array" => Value::Array(Vec::new()),
            "value" => {
                let value: Value = serde_json::from_str(encoded)?;
                if value.is_object() || value.is_array() {
                    return Err(anyhow!("collection encoded as a scalar metadata unit"));
                }
                value
            }
            _ => return Err(anyhow!("invalid metadata field descriptor")),
        };
        fields.insert(field.clone(), value);
    }
    for ((field, key), (kind, encoded)) in units {
        if key.is_empty() {
            continue;
        }
        if kind != "entry" {
            return Err(anyhow!("invalid metadata entry kind"));
        }
        let key = key
            .strip_prefix('/')
            .ok_or_else(|| anyhow!("invalid metadata entry key"))?;
        let value = serde_json::from_str(encoded)?;
        match fields.get_mut(field) {
            Some(Value::Object(entries)) => {
                entries.insert(key.to_owned(), value);
            }
            Some(Value::Array(entries)) => {
                let position: usize = key.parse().context("invalid metadata array position")?;
                if key != format!("{position:020}") || position != entries.len() {
                    return Err(anyhow!("metadata array has a gap or noncanonical position"));
                }
                entries.push(value);
            }
            _ => return Err(anyhow!("metadata entry has no collection descriptor")),
        }
    }
    for (field, expected) in counts {
        let actual = match &fields[field] {
            Value::Array(entries) => entries.len(),
            Value::Object(entries) => entries.len(),
            _ => return Err(anyhow!("invalid metadata collection descriptor")),
        };
        if actual != expected {
            return Err(anyhow!(
                "metadata collection `{field}` has missing or extra entries"
            ));
        }
    }
    Ok(Value::Object(fields))
}

fn persisted_metadata_storage_version(connection: &rusqlite::Connection) -> Result<String> {
    let version: Option<String> = connection
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| {
            reject_unsupported_state_schema(format!(
                "cannot read metadata storage version: {error}"
            ))
        })?;
    match version.as_deref() {
        Some(
            version
            @ ("2" | "2-metadata-units-1" | SQLITE_METADATA_UNITS_VERSION | SQLITE_SCHEMA_VERSION),
        ) => Ok(version.to_owned()),
        Some(version) => Err(reject_unsupported_state_schema(format!(
            "unsupported metadata storage version `{version}`"
        ))),
        None => Err(reject_unsupported_state_schema(
            "missing metadata storage version",
        )),
    }
}

fn read_persisted_metadata_json(connection: &rusqlite::Connection) -> Result<Option<String>> {
    let version = persisted_metadata_storage_version(connection)?;
    if version == "2-metadata-units-1" {
        let units = read_metadata_units_unchecked(connection)?;
        if units.is_empty() {
            if metadata_units_are_uninitialized(connection)? {
                return Ok(None);
            }
            return Err(anyhow!("missing units-1 metadata authority"));
        }
        return Ok(Some(serde_json::to_string(
            &legacy_metadata_units_to_value(units)?,
        )?));
    }
    if version == "2" {
        return connection
            .query_row(
                "SELECT value_json FROM app_state WHERE key=?1",
                [SQLITE_METADATA_KEY],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into);
    }
    let units = read_metadata_units(connection)?;
    if units.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::to_string(&metadata_units_to_value(
        &units,
    )?)?))
}

fn read_persisted_metadata_field(
    connection: &rusqlite::Connection,
    field: &str,
) -> Result<Option<Value>> {
    let version = persisted_metadata_storage_version(connection)?;
    if !matches!(version.as_str(), SQLITE_METADATA_UNITS_VERSION | SQLITE_SCHEMA_VERSION) {
        return read_persisted_metadata_json(connection)?
            .map(|encoded| -> Result<Option<Value>> {
                let mut value: Value = serde_json::from_str(&encoded)?;
                Ok(value
                    .as_object_mut()
                    .and_then(|fields| fields.remove(field)))
            })
            .transpose()
            .map(Option::flatten);
    }
    let mut roots = connection.prepare(
        "SELECT field_name,kind,value_json FROM state_metadata_units WHERE item_key='' ORDER BY field_name")?;
    let root_rows = roots.query_map([], |row| {
        Ok((
            (row.get::<_, String>(0)?, String::new()),
            (row.get::<_, String>(1)?, row.get::<_, String>(2)?),
        ))
    })?;
    let inventory = root_rows.collect::<rusqlite::Result<PersistMetadataUnits>>()?;
    if inventory.is_empty() && metadata_units_are_uninitialized(connection)? {
        return Ok(None);
    }
    validate_metadata_unit_inventory(&inventory)?;
    let mut statement = connection.prepare(
        "SELECT item_key,kind,value_json FROM state_metadata_units WHERE field_name=?1 ORDER BY item_key")?;
    let rows = statement.query_map([field], |row| {
        Ok((
            (field.to_owned(), row.get::<_, String>(0)?),
            (row.get::<_, String>(1)?, row.get::<_, String>(2)?),
        ))
    })?;
    let units = rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
    let mut value = metadata_units_to_value(&units)?;
    Ok(value
        .as_object_mut()
        .and_then(|fields| fields.remove(field)))
}

fn metadata_unit_generation(connection: &rusqlite::Connection) -> Result<u64> {
    let encoded: Option<String> = connection
        .query_row(
            "SELECT value FROM meta WHERE key='metadata_units_generation'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    encoded
        .map(|value| value.parse().context("invalid metadata unit generation"))
        .transpose()
        .map(|value| value.unwrap_or(0))
}

// Caller holds a read/write transaction so multi-field fence proofs share one
// committed cut. Only required collections are decoded, never app_state.
fn read_metadata_for_fence(
    connection: &rusqlite::Connection,
    fields: &[&str],
) -> Result<Option<PersistedState>> {
    let mut value = serde_json::json!({
        "nextSessionNumber": 0, "nextMessageNumber": 0, "projects": [], "sessions": []
    });
    for field in fields {
        let Some(field_value) = read_persisted_metadata_field(connection, field)? else {
            return Ok(None);
        };
        value[*field] = field_value;
    }
    Ok(Some(serde_json::from_value(value)?))
}
