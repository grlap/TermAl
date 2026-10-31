// Owns the joint obligation reader floor after the complete ordinary-handoff
// contract. Does not rewrite session rows, infer grant state or settle duties.

#[cfg(test)]
thread_local! {
    static TEST_BEFORE_OBLIGATION_MIGRATION_BEGIN: std::cell::RefCell<Option<Box<dyn FnOnce()>>>
        = const { std::cell::RefCell::new(None) };
}

// Safety metadata is read before ordinary session filtering. A discarded row
// cannot discard its metadata duty; a present mirror must match it exactly.
fn validate_sqlite_checkpoint_mirrors(
    connection: &rusqlite::Connection,
    ledger: &EngramSessionObligationLedger,
) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT id, json_extract(value_json, '$.engramCheckpointOperation') FROM sessions
         WHERE json_valid(value_json)
           AND json_type(value_json, '$.engramCheckpointOperation') IS NOT NULL
           AND json_type(value_json, '$.engramCheckpointOperation') != 'null'",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (session_id, encoded) = row?;
        let raw: Value = serde_json::from_str(&encoded)
            .map_err(reject_invalid_persisted_state_metadata_shape)?;
        let operation: EngramCheckpointOperation = serde_json::from_value(raw.clone())
            .map_err(reject_invalid_persisted_state_metadata_shape)?;
        if operation.protection.local_session_id != session_id
            || serde_json::to_value(&operation)? != raw
            || !ledger
                .checkpoint_operations
                .iter()
                .any(|protected| serde_json::to_value(protected).ok().as_ref() == Some(&raw))
        {
            return Err(reject_unsupported_state_schema(
                "existing checkpoint mirror lacks its exact metadata protection",
            ));
        }
    }
    Ok(())
}

fn validate_sqlite_obligation_metadata(connection: &rusqlite::Connection) -> Result<()> {
    let value = read_persisted_metadata_field(connection, "engramSessionObligations")?.ok_or_else(
        || {
            reject_unsupported_state_schema(
                "missing mandatory Engram session obligation ledger".to_owned(),
            )
        },
    )?;
    let encoded = serde_json::to_string(&value)?;
    if encoded.len() > ENGRAM_SESSION_OBLIGATION_MAX_BYTES {
        return Err(reject_unsupported_state_schema(
            "Engram obligation ledger capacity exceeded".to_owned(),
        ));
    }
    serde_json::from_str::<EngramSessionObligationLedger>(&encoded)
        .map_err(reject_invalid_persisted_state_metadata_shape)?;
    Ok(())
}

// The existing metadata cache keeps only owner/digest lookups for its exact
// ledger field. A warm unchanged field shares the index; changed units rebuild
// it once. This adds no session inventory scan to the writer.
type PersistCheckpointMirrorIndex = BTreeMap<String, (String, String)>;

#[cfg(test)]
thread_local! {
    static TEST_CHECKPOINT_WRITE_CUT_WORK: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

fn checkpoint_index_from_units(
    units: &PersistMetadataUnits,
) -> Result<Option<Arc<PersistCheckpointMirrorIndex>>> {
    let field = "engramSessionObligations";
    // Reuse the ordinary object/count/entry codec. The ledger is a descriptor
    // plus member units, including the aggregate checkpointOperations array.
    let selected = units.range((field.to_owned(), String::new())..)
        .take_while(|((name, _), _)| name == field)
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<PersistMetadataUnits>();
    if selected.is_empty() { return Ok(None) }
    let mut fields = metadata_units_to_value(&selected)?;
    let value = fields.as_object_mut().and_then(|fields| fields.remove(field))
        .ok_or_else(|| anyhow!("missing Engram obligation ledger descriptor"))?;
    if serde_json::to_vec(&value)?.len() > ENGRAM_SESSION_OBLIGATION_MAX_BYTES {
        bail!("Engram obligation ledger capacity exceeded");
    }
    #[cfg(test)]
    TEST_CHECKPOINT_WRITE_CUT_WORK.with(|work| {
        let (builds, probes) = work.get();
        work.set((builds + 1, probes));
    });
    let ledger: EngramSessionObligationLedger = serde_json::from_value(value)
        .map_err(reject_invalid_persisted_state_metadata_shape)?;
    let mut index = BTreeMap::new();
    for operation in ledger.checkpoint_operations {
        let raw = serde_json::to_value(&operation)?;
        index.insert(operation.original.id, (
            operation.protection.local_session_id,
            sha256_hex(&serde_json::to_vec(&raw)?),
        ));
    }
    Ok(Some(Arc::new(index)))
}

fn checkpoint_write_cut_index(
    previous: &PersistMetadataUnits,
    desired: &PersistMetadataUnits,
    cached: Option<&PersistMetadataUnitCache>,
) -> Result<(Option<Arc<PersistCheckpointMirrorIndex>>, BTreeSet<String>)> {
    let field = "engramSessionObligations";
    let old_units = previous.range((field.to_owned(), String::new())..)
        .take_while(|((name, _), _)| name == field);
    let new_units = desired.range((field.to_owned(), String::new())..)
        .take_while(|((name, _), _)| name == field);
    let previous_index = if let Some(cache) = cached {
        cache.checkpoint_index.clone()
    } else {
        checkpoint_index_from_units(previous)?
    };
    if old_units.eq(new_units) {
        return Ok((previous_index, BTreeSet::new()));
    }
    let next_index = checkpoint_index_from_units(desired)?;
    let mut owners = BTreeSet::new();
    if let Some(old) = &previous_index {
        for (id, value) in old.iter() {
            if next_index.as_ref().and_then(|next| next.get(id)) != Some(value) {
                owners.insert(value.0.clone());
            }
        }
    }
    if let Some(next) = &next_index {
        for (id, value) in next.iter() {
            if previous_index.as_ref().and_then(|old| old.get(id)) != Some(value) {
                owners.insert(value.0.clone());
            }
        }
    }
    Ok((next_index, owners))
}

// Validate the effective transaction after writes/deletes, before COMMIT.
// Probe only changed rows and owners of changed/retired ledger operations.
// Deferred and quarantined owners are included by the ledger diff. A rejected
// cut leaves source stamps/watermark unadopted for a fresh collector retry.
fn validate_sqlite_checkpoint_write_cut(
    connection: &rusqlite::Connection,
    metadata: &PersistMetadataUnitCache,
    sessions: &[SerializedPersistedSession],
) -> Result<()> {
    let index = metadata.checkpoint_index.as_ref()
        .ok_or_else(|| anyhow!("missing mandatory Engram session obligation ledger"))?;
    let mut owners = metadata.changed_checkpoint_owners.clone();
    owners.extend(sessions.iter().map(|session| session.session_id.clone()));
    let mut statement = connection.prepare_cached("SELECT value_json FROM sessions WHERE id=?1")?;
    for owner in owners {
        #[cfg(test)]
        TEST_CHECKPOINT_WRITE_CUT_WORK.with(|work| {
            let (builds, probes) = work.get();
            work.set((builds, probes + 1));
        });
        let encoded: Option<String> = statement.query_row([&owner], |row| row.get(0)).optional()?;
        // Missing rows can retain independent teardown duties; invalid rows
        // are quarantined by the existing startup reader.
        let Some(raw) = encoded.and_then(|encoded| serde_json::from_str::<Value>(&encoded).ok()) else { continue };
        let Some(mirror) = raw.get("engramCheckpointOperation").filter(|value| !value.is_null()) else { continue };
        let operation: EngramCheckpointOperation = serde_json::from_value(mirror.clone())
            .map_err(reject_invalid_persisted_state_metadata_shape)?;
        if operation.protection.local_session_id != owner
            || serde_json::to_value(&operation)? != *mirror
        {
            bail!("existing checkpoint mirror lacks its exact metadata protection");
        }
        // Owner-correct and canonical, yet not what this cut's ledger selects:
        // a relational race the next coherent collection repairs, typed so
        // its fences wait for that retry (`PersistCheckpointMirrorLedgerMismatch`).
        if index.get(&operation.original.id) != Some(&(
            owner, sha256_hex(&serde_json::to_vec(mirror)?),
        )) {
            return Err(PersistCheckpointMirrorLedgerMismatch.into());
        }
    }
    Ok(())
}

fn is_sqlite_obligation_predecessor(version: &str) -> bool {
    matches!(
        version,
        "2" | "2-metadata-units-1" | SQLITE_METADATA_UNITS_VERSION
            | SQLITE_OBSERVATION_PREDECESSOR | SQLITE_OBSERVATION_SCHEMA_VERSION
    )
}

fn prepare_sqlite_obligation_reader_floor(
    connection: &rusqlite::Connection,
    for_load: bool,
) -> Result<Option<PersistedState>> {
    let (_, version) = sqlite_state_schema_header(connection)?;
    if version == SQLITE_CHECKPOINT_RESULT_PREDECESSOR {
        return prepare_sqlite_checkpoint_result_reader_floor(connection, for_load);
    }
    if !is_sqlite_obligation_predecessor(&version) {
        validate_current_sqlite_state_schema(connection)?;
        return if for_load {
            validate_no_legacy_embedded_state(connection)
        } else {
            validate_sqlite_obligation_metadata(connection)?;
            Ok(None)
        };
    }
    #[cfg(test)]
    {
        let hook = TEST_BEFORE_OBLIGATION_MIGRATION_BEGIN.with(|hook| hook.borrow_mut().take());
        if let Some(hook) = hook {
            hook();
        }
    }
    let transaction =
        rusqlite::Transaction::new_unchecked(connection, rusqlite::TransactionBehavior::Immediate)
            .context("failed to begin joint state reader-floor migration")?;
    let (actual_tables, mut version) = sqlite_state_schema_header(&transaction)?;
    if is_sqlite_obligation_predecessor(&version) {
        let known = CURRENT_SQLITE_STATE_TABLE_COLUMNS.iter()
            .map(|(name, _)| (*name).to_owned()).collect::<BTreeSet<_>>();
        // Keep the landed ignored coordination-table exception; other unknown
        // tables can carry authority that this reader cannot safely interpret.
        if actual_tables.difference(&known).any(|table| !matches!(table.as_str(),
            "mailboxes" | "mailbox_participants" | "mailbox_messages")) {
            return Err(reject_unsupported_state_schema(
                "reader-floor predecessor has unknown state tables",
            ));
        }
        let tables: Vec<_> = CURRENT_SQLITE_STATE_TABLE_COLUMNS
            .iter()
            .filter(|(name, _)| version != "2" || *name != "state_metadata_units")
            .copied()
            .collect();
        validate_sqlite_state_table_columns(&transaction, &tables, true)?;
        if required_state_meta_value(&transaction, SQLITE_PROMPT_HISTORY_STORAGE_KEY)?
            != SQLITE_PROMPT_HISTORY_STORAGE_VERSION
        {
            return Err(reject_unsupported_state_schema(
                "invalid predecessor prompt-history storage",
            ));
        }
        validate_optional_federation_host_identity(&transaction)?;
        if version != SQLITE_OBSERVATION_SCHEMA_VERSION {
            // Check the locked original source before any legacy normalization
            // can erase even null or empty observation-custody keys.
            let unversioned: Option<String> = transaction.query_row(
                "SELECT id FROM sessions WHERE json_valid(value_json) AND (
                    json_type(value_json,'$.engramRuntimeCheckpoint') IS NOT NULL OR
                    json_type(value_json,'$.engramUnadmittedActiveTurn') IS NOT NULL OR
                    json_type(value_json,'$.engramUnadmittedClosingJobs') IS NOT NULL OR
                    json_type(value_json,'$.engramUnadmittedReports') IS NOT NULL) LIMIT 1",
                [], |row| row.get(0),
            ).optional()?;
            if unversioned.is_some() {
                return Err(reject_unsupported_state_schema(
                    "observation custody is present under an unsupported predecessor floor",
                ));
            }
        }
        // Temporary absence is admitted only under a known predecessor header.
        // The full typed scan (including safety mirrors) precedes all writes.
        let previous = validate_no_legacy_embedded_state(&transaction)?;
        if matches!(version.as_str(), "2" | "2-metadata-units-1") {
            // This inherited normalization targets units-2 exactly, without
            // exposing the later obligation inventory under its old marker.
            let raw = read_persisted_metadata_json(&transaction)?
                .map(|encoded| serde_json::from_str::<Value>(&encoded))
                .transpose()?;
            let typed =
                previous.unwrap_or_else(|| PersistedState::metadata_from_inner(&StateInner::new()));
            let mut complete = metadata_units_to_value(&project_metadata_units(&typed)?)?;
            complete
                .as_object_mut()
                .unwrap()
                .remove("engramSessionObligations");
            if let Some(raw) = raw {
                for (field, value) in raw
                    .as_object()
                    .ok_or_else(|| anyhow!("metadata is not an object"))?
                {
                    if field == "delegations" && value.as_array().is_some_and(Vec::is_empty) {
                        continue;
                    }
                    if !METADATA_UNIT_PREDECESSOR_FIELDS.contains(&field.as_str()) {
                        return Err(anyhow!("unsupported predecessor metadata field `{field}`"));
                    }
                    complete[field] = value.clone();
                }
            }
            let desired = metadata_value_to_units(complete)?;
            validate_metadata_unit_inventory_for_floor(&desired, false)?;
            metadata_units_to_value(&desired)?;
            if version == "2" {
                transaction.execute_batch(SQLITE_METADATA_UNITS_SCHEMA)?;
            }
            write_metadata_units(&transaction, desired, None, &[], None, false)?;
            advance_sqlite_reader_floor(&transaction, &version, "2-metadata-units-2")?;
            version = "2-metadata-units-2".to_owned();
        }
        if version == "2-metadata-units-2" {
            validate_sqlite_state_schema_for_version(&transaction, &version)?;
            validate_no_legacy_embedded_state(&transaction)?;
            advance_sqlite_reader_floor(&transaction, &version, "2-metadata-units-2-handoff-1")?;
            version = "2-metadata-units-2-handoff-1".to_owned();
        }
        // The landed outbox is already normalized. Its session rows and exact
        // request custody remain untouched while only ledger units are added.
        validate_sqlite_state_schema_for_version(&transaction, &version)?;
        validate_no_legacy_embedded_state(&transaction)?;
        let mut desired = read_metadata_units_unchecked(&transaction)?;
        let install_complete = desired.is_empty();
        if install_complete {
            // Only the validated genuinely uninitialized predecessor can reach
            // this branch. Its full current inventory is installed atomically.
            desired =
                project_metadata_units(&PersistedState::metadata_from_inner(&StateInner::new()))?;
        } else {
            validate_metadata_unit_inventory_for_floor(&desired, false)?;
            // Extend the raw map: unrelated rows remain byte exact.
            desired.extend(metadata_value_to_units(json!({
                "engramSessionObligations": EngramSessionObligationLedger::empty()
            }))?);
        }
        validate_metadata_unit_inventory(&desired)?;
        let prospective: PersistedState =
            serde_json::from_value(metadata_units_to_value(&desired)?)
                .map_err(reject_invalid_persisted_state_metadata_shape)?;
        validate_sqlite_checkpoint_mirrors(&transaction, &prospective.engram_session_obligations)?;
        let generation = metadata_unit_generation(&transaction)?
            .checked_add(1)
            .ok_or_else(|| anyhow!("metadata generation exhausted"))?;
        // Do not route this extension through ordinary counter reservation:
        // even valid noncanonical whitespace in unrelated values stays exact.
        for ((field, key), (kind, encoded)) in &desired {
            if field == "engramSessionObligations" || install_complete {
                transaction.execute(
                    "INSERT INTO state_metadata_units(field_name,item_key,kind,value_json) VALUES(?1,?2,?3,?4)",
                    rusqlite::params![field,key,kind,encoded],
                )?;
            }
        }
        transaction.execute(
            "INSERT INTO meta(key,value) VALUES('metadata_units_generation',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [generation.to_string()],
        )?;
        advance_sqlite_reader_floor(&transaction, &version, SQLITE_SCHEMA_VERSION)?;
    } else {
        // A concurrent opener may have finished the whole ordered migration.
        validate_current_sqlite_state_schema(&transaction)?;
    }
    validate_current_sqlite_state_schema(&transaction)?;
    let persisted = if for_load {
        validate_no_legacy_embedded_state(&transaction)?
    } else {
        validate_sqlite_obligation_metadata(&transaction)?;
        None
    };
    transaction
        .commit()
        .context("failed to commit joint state reader-floor migration")?;
    persist_observe_metadata_commit();
    Ok(persisted)
}

// This successor changes only the understood result vocabulary. The complete
// old ledger and safety mirrors are validated under the locked old header;
// their raw units, session rows and metadata generation are never rewritten.
fn prepare_sqlite_checkpoint_result_reader_floor(connection: &rusqlite::Connection,
    for_load: bool) -> Result<Option<PersistedState>> {
    let transaction = rusqlite::Transaction::new_unchecked(connection,
        rusqlite::TransactionBehavior::Immediate)?;
    let version = required_state_meta_value(&transaction, "schema_version")?;
    if version == SQLITE_CHECKPOINT_RESULT_PREDECESSOR {
        validate_sqlite_state_schema_for_version(&transaction, &version)?;
        let previous = validate_no_legacy_embedded_state(&transaction)?
            .ok_or_else(|| reject_unsupported_state_schema("missing predecessor obligation metadata"))?;
        let has_refused = |operation: &EngramCheckpointOperation| operation.attempts.iter()
            .any(|attempt| matches!(attempt.result, Some(EngramCheckpointAttemptResult::Refused { .. })));
        if previous.engram_session_obligations.checkpoint_operations.iter().any(has_refused)
            || previous.engram_session_obligations.entries.iter().any(|entry| {
                serde_json::from_value::<EngramUnsettledControlImage>(entry.frozen_request.clone())
                    .ok().and_then(|image| image.existing_checkpoint)
                    .as_ref().is_some_and(has_refused)
            }) {
            return Err(reject_unsupported_state_schema("checkpoint Refused result under predecessor floor"));
        }
        advance_sqlite_reader_floor(&transaction, &version, SQLITE_SCHEMA_VERSION)?;
    } else {
        validate_current_sqlite_state_schema(&transaction)?;
    }
    validate_current_sqlite_state_schema(&transaction)?;
    let persisted = if for_load { validate_no_legacy_embedded_state(&transaction)? }
        else { validate_sqlite_obligation_metadata(&transaction)?; None };
    transaction.commit()?;
    persist_observe_metadata_commit();
    Ok(persisted)
}

fn advance_sqlite_reader_floor(
    connection: &rusqlite::Connection,
    predecessor: &str,
    successor: &str,
) -> Result<()> {
    let updated = connection
        .execute(
            "UPDATE meta SET value=?1 WHERE key='schema_version' AND value=?2",
            rusqlite::params![successor, predecessor],
        )
        .context("failed to advance ordered state reader floor")?;
    if updated != 1 {
        bail!("state reader-floor predecessor changed");
    }
    Ok(())
}
