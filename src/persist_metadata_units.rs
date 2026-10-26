// Metadata is stored as independent root fields and collection entries. A
// session transaction writes its counters and changed entries, never app_state.
// The old metadata image is retained only as migration/rollback provenance.

type PersistMetadataUnitKey = (String, String);
type PersistMetadataUnitValue = (String, String);
type PersistMetadataUnits = BTreeMap<PersistMetadataUnitKey, PersistMetadataUnitValue>;

include!("persist_metadata_tracking.rs");

#[derive(Clone)]
struct PersistMetadataCollectionCapture {
    watermarks: Arc<Mutex<PersistMetadataWatermarks>>,
    selected: PersistMetadataWatermarks,
    omitted: BTreeSet<&'static str>,
    image_stamps: PersistMetadataWatermarks,
}

// The source's known durable predecessor is distinct from the writer cache.
// Capturing never samples a newer database beside an older desired image.
#[derive(Clone, Default)]
struct PersistMetadataCapture {
    known: Arc<std::sync::atomic::AtomicU64>,
    predecessor: u64,
    applied: Arc<std::sync::atomic::AtomicU64>,
    collections: Option<PersistMetadataCollectionCapture>,
}

impl PersistMetadataCapture {
    fn omits(&self, field: &str) -> bool {
        self.collections
            .as_ref()
            .is_some_and(|capture| capture.omitted.contains(field))
    }
    fn new(known: Arc<std::sync::atomic::AtomicU64>) -> Self {
        let predecessor = known.load(std::sync::atomic::Ordering::Acquire);
        Self {
            known,
            predecessor,
            applied: Arc::default(),
            collections: None,
        }
    }

    fn verify(&self, generation: u64) -> Result<()> {
        if generation != self.predecessor {
            return Err(anyhow!(
                "stale metadata snapshot: durable predecessor changed"
            ));
        }
        Ok(())
    }

    fn committed(&self, generation: u64) {
        self.applied
            .store(generation, std::sync::atomic::Ordering::Release);
    }

    fn adopt(&self, generation: u64, metadata: &PersistedState) {
        let mut watermarks = self.collections.as_ref().map(|capture| {
            capture
                .watermarks
                .lock()
                .expect("metadata watermarks poisoned")
        });
        let adopted = self
            .known
            .compare_exchange(
                self.predecessor,
                generation,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok();
        if adopted {
            if let (Some(capture), Some(watermarks)) = (&self.collections, &mut watermarks) {
                for (field, image_stamp) in &capture.image_stamps {
                    if metadata.metadata_collection_stamp(field).as_ref() != Some(image_stamp) {
                        // An edited snapshot no longer represents the captured
                        // source. Select that source field again next time.
                        watermarks.remove(field);
                    } else if let Some(stamp) = capture.selected.get(field) {
                        watermarks.insert(field, stamp.clone());
                    }
                }
            }
        }
    }

    // Only a caller still holding the authoritative StateInner may adopt a
    // synchronous snapshot. A post-commit integrity error still fails its
    // receipt; a fresh source capture can reapply it after normal checks.
    fn adopt_own_committed(&self, metadata: &PersistedState) {
        let applied = self.applied.load(std::sync::atomic::Ordering::Acquire);
        if applied != 0 {
            self.adopt(applied, metadata);
        }
    }
}

const SQLITE_METADATA_UNITS_SCHEMA: &str = "
    CREATE TABLE state_metadata_units (
      field_name TEXT NOT NULL,
      item_key TEXT NOT NULL,
      kind TEXT NOT NULL,
      value_json TEXT NOT NULL,
      PRIMARY KEY(field_name, item_key)
    ) WITHOUT ROWID;
";

struct PersistMetadataUnitCache {
    generation: u64,
    units: PersistMetadataUnits,
}

include!("persist_metadata_units_codec.rs");

fn metadata_counter(units: &PersistMetadataUnits, name: &str) -> Result<u64> {
    let Some((kind, encoded)) = units.get(&(name.to_owned(), String::new())) else {
        return Ok(0);
    };
    if kind != "value" {
        return Err(anyhow!("invalid allocation counter unit"));
    }
    serde_json::from_str(encoded).context("invalid allocation counter")
}

fn reserve_metadata_identity(
    units: &mut PersistMetadataUnits,
    name: &str,
    id: &str,
    prefix: &str,
) -> Result<()> {
    let Some(number) = id.strip_prefix(prefix) else {
        return Ok(());
    };
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(());
    }
    let next = number
        .parse::<u64>()
        .context("invalid numeric identity")?
        .checked_add(1)
        .ok_or_else(|| anyhow!("numeric identity space exhausted"))?;
    if next > metadata_counter(units, name)? {
        units.insert(
            (name.to_owned(), String::new()),
            ("value".into(), next.to_string()),
        );
    }
    Ok(())
}

fn write_metadata_units(
    connection: &rusqlite::Connection,
    mut desired: PersistMetadataUnits,
    cached: Option<&PersistMetadataUnitCache>,
    sessions: &[SerializedPersistedSession],
    capture: Option<&PersistMetadataCapture>,
    coupled_changes: bool,
) -> Result<PersistMetadataUnitCache> {
    let generation = metadata_unit_generation(connection)?;
    if let Some(capture) = capture {
        capture.verify(generation)?;
    }
    let loaded;
    let previous = if let Some(cache) = cached.filter(|cache| cache.generation == generation) {
        &cache.units
    } else {
        loaded = read_metadata_units_unchecked(connection)?;
        if capture.is_some() && loaded.is_empty() && !metadata_units_are_uninitialized(connection)?
        {
            return Err(anyhow!(
                "missing metadata authority in an initialized store"
            ));
        }
        if capture.is_some() && (!loaded.is_empty() || generation != 0) {
            validate_metadata_unit_inventory(&loaded)?;
            metadata_units_to_value(&loaded)?;
        }
        &loaded
    };
    if let Some(collections) = capture.and_then(|capture| capture.collections.as_ref()) {
        let omitted = collections
            .omitted
            .iter()
            .copied()
            .filter(|field| !desired.contains_key(&(field.to_string(), String::new())))
            .collect::<BTreeSet<_>>();
        // Reuse only the exact verified durable predecessor. These encoded
        // units stay in the existing writer cache, outside StateInner.
        for ((field, key), value) in previous {
            if omitted.contains(field.as_str()) {
                desired.insert((field.clone(), key.clone()), value.clone());
            }
        }
    }
    // Preserve consumed identities even when a full snapshot was captured
    // before a newer record was materialized, or a session has been deleted.
    for name in ["revision", "nextSessionNumber", "nextMessageNumber"] {
        let next = metadata_counter(&desired, name)?.max(metadata_counter(previous, name)?);
        desired.insert(
            (name.to_owned(), String::new()),
            ("value".into(), next.to_string()),
        );
    }
    for session in sessions {
        if !session.local_identity_namespace {
            continue;
        }
        reserve_metadata_identity(
            &mut desired,
            "nextSessionNumber",
            &session.session_id,
            "session-",
        )?;
        for message in &session.messages {
            reserve_metadata_identity(
                &mut desired,
                "nextMessageNumber",
                &message.message_id,
                "message-",
            )?;
        }
    }
    let mut changed = false;
    let mut observation = PersistMetadataUnitObservation::new(&desired);
    for (field, key) in previous.keys() {
        if !desired.contains_key(&(field.clone(), key.clone())) {
            observation.delete(field);
            connection.execute(
                "DELETE FROM state_metadata_units WHERE field_name=?1 AND item_key=?2",
                rusqlite::params![field, key],
            )?;
            changed = true;
        }
    }
    for ((field, key), (kind, encoded)) in &desired {
        if previous
            .get(&(field.clone(), key.clone()))
            .is_some_and(|(old_kind, old_encoded)| old_kind == kind && old_encoded == encoded)
        {
            continue;
        }
        observation.upsert(field, encoded.len());
        connection.execute(
            "INSERT INTO state_metadata_units(field_name,item_key,kind,value_json) VALUES(?1,?2,?3,?4)
             ON CONFLICT(field_name,item_key) DO UPDATE SET kind=excluded.kind,value_json=excluded.value_json",
            rusqlite::params![field, key, kind, encoded])?;
        changed = true;
    }
    let generation = if changed || coupled_changes {
        let next = generation
            .checked_add(1)
            .ok_or_else(|| anyhow!("metadata generation exhausted"))?;
        connection.execute(
            "INSERT INTO meta(key,value) VALUES('metadata_units_generation',?1)
            ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [next.to_string()],
        )?;
        next
    } else {
        generation
    };
    Ok(PersistMetadataUnitCache {
        generation,
        units: desired,
    })
}

// Only the known, validated primary-2 image is migrated. The transaction changes
// both the authority and its version marker, so an interrupted migration leaves
// either the entire old format or the entire new format. Old builds fail closed.
fn migrate_metadata_units(connection: &rusqlite::Connection) -> Result<()> {
    if !sqlite_state_user_table_names(connection)?.contains("meta") {
        // Preserve the typed compatibility refusal before probing an obsolete
        // database, and before any schema or persistent PRAGMA mutation.
        return validate_current_sqlite_state_schema(connection);
    }
    let version: Option<String> = connection
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(source_version @ ("2" | "2-metadata-units-1")) = version.as_deref() else {
        return Ok(());
    };
    let old_tables: Vec<_> = CURRENT_SQLITE_STATE_TABLE_COLUMNS
        .iter()
        .filter(|(name, _)| source_version != "2" || *name != "state_metadata_units")
        .copied()
        .collect();
    validate_sqlite_state_table_columns(connection, &old_tables, true)?;
    if required_state_meta_value(connection, SQLITE_PROMPT_HISTORY_STORAGE_KEY)?
        != SQLITE_PROMPT_HISTORY_STORAGE_VERSION
    {
        return Err(reject_unsupported_state_schema(
            "invalid legacy prompt-history storage",
        ));
    }
    validate_optional_federation_host_identity(connection)?;
    validate_no_legacy_embedded_state(connection)?;
    let tx =
        rusqlite::Transaction::new_unchecked(connection, rusqlite::TransactionBehavior::Immediate)?;
    // A second process may have completed the migration while we awaited BEGIN.
    if required_state_meta_value(&tx, "schema_version")? != source_version {
        tx.commit()?;
        return Ok(());
    }
    validate_no_legacy_embedded_state(&tx)?;
    let encoded = read_persisted_metadata_json(&tx)?;
    if source_version == "2" {
        tx.execute_batch(SQLITE_METADATA_UNITS_SCHEMA)?;
    }
    if let Some(encoded) = encoded {
        // Preserve the raw federation subtrees: their digest is an authority
        // marker, and serde defaults must not silently change that image.
        let value: Value = serde_json::from_str(&encoded)?;
        let typed: PersistedState = serde_json::from_value(value.clone())?;
        let mut complete = metadata_units_to_value(&project_metadata_units(&typed)?)?;
        let fields = value
            .as_object()
            .ok_or_else(|| anyhow!("metadata is not an object"))?;
        for (name, raw) in fields {
            if name == "delegations" && raw.as_array().is_some_and(Vec::is_empty) {
                continue;
            }
            if !METADATA_UNIT_FIELDS.contains(&name.as_str()) {
                return Err(anyhow!("unsupported legacy metadata field `{name}`"));
            }
            complete[name] = raw.clone();
        }
        let desired = metadata_value_to_units(complete)?;
        // Raw legacy values may deserialize through serde's sequence support
        // while violating this format's fixed kinds. Refuse before committing
        // either the units or their authoritative reader-floor marker.
        validate_metadata_unit_inventory(&desired)?;
        metadata_units_to_value(&desired)?;
        write_metadata_units(&tx, desired, None, &[], None, false)?;
    }
    tx.execute(
        "UPDATE meta SET value=?1 WHERE key='schema_version'",
        [SQLITE_METADATA_UNITS_VERSION],
    )?;
    tx.commit()
        .context("failed to migrate normalized metadata authority")?;
    persist_observe_metadata_commit();
    Ok(())
}

fn legacy_metadata_units_to_value(mut units: PersistMetadataUnits) -> Result<Value> {
    // Units-1 had no tail count. Validate all information it did retain; its
    // absent trailing members cannot be reconstructed from stale app_state.
    for field in [
        "nextSessionNumber",
        "nextMessageNumber",
        "projects",
        "sessions",
    ] {
        if !units.contains_key(&(field.to_owned(), String::new())) {
            return Err(anyhow!("missing required units-1 descriptor `{field}`"));
        }
    }
    let mut counts = BTreeMap::<String, usize>::new();
    for (field, key) in units.keys() {
        if !key.is_empty() {
            *counts.entry(field.clone()).or_default() += 1;
        }
        if !METADATA_UNIT_FIELDS.contains(&field.as_str()) {
            return Err(anyhow!("unsupported units-1 metadata field `{field}`"));
        }
    }
    for ((field, key), (kind, encoded)) in &mut units {
        if key.is_empty() && (kind == "array" || kind == "object") {
            if encoded != "null" {
                return Err(anyhow!("invalid units-1 collection descriptor"));
            }
            *encoded = counts.get(field).copied().unwrap_or(0).to_string();
        }
    }
    metadata_units_to_value(&units)
}
