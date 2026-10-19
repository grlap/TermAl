// Owns stable host metadata and validation of the domain's persisted projection.
// Does not own a writer, credentials, transport, or session execution. New
// persistence fragment used by persist.rs and the ordinary state writer.

fn read_federation_host_identity(
    connection: &rusqlite::Connection,
) -> Result<FederationHostIdentity> {
    let read = |key: &str| -> Result<String> {
        connection
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .with_context(|| format!("missing federation metadata `{key}`"))
    };
    let identity = FederationHostIdentity {
        host_id: read("federation_host_id")?,
        host_generation: read("federation_host_generation")?
            .parse()
            .context("invalid federation host generation")?,
    };
    identity
        .validate()
        .map_err(|_| anyhow!("invalid persisted federation host identity"))?;
    Ok(identity)
}

fn validate_optional_federation_host_identity(connection: &rusqlite::Connection) -> Result<()> {
    let count: u64 = connection.query_row(
        "SELECT count(*) FROM meta WHERE key IN ('federation_host_id', 'federation_host_generation')",
        [], |row| row.get(0),
    )?;
    if count != 0 {
        read_federation_host_identity(connection)?;
    }
    Ok(())
}

/// Called within the schema transaction. A partial identity is corruption,
/// never a reason to mint a replacement identity for an existing installation.
fn seed_federation_host_identity(connection: &rusqlite::Connection) -> Result<()> {
    let count: u64 = connection.query_row(
        "SELECT count(*) FROM meta WHERE key IN ('federation_host_id', 'federation_host_generation')",
        [], |row| row.get(0),
    )?;
    if count == 0 {
        connection.execute(
            "INSERT INTO meta(key,value) VALUES('federation_host_id',?1)",
            [Uuid::new_v4().to_string()],
        )?;
        connection.execute(
            "INSERT INTO meta(key,value) VALUES('federation_host_generation','1')",
            [],
        )?;
    }
    read_federation_host_identity(connection)?;
    Ok(())
}

fn ensure_federation_host_identity(connection: &rusqlite::Connection) -> Result<()> {
    let transaction =
        rusqlite::Transaction::new_unchecked(connection, rusqlite::TransactionBehavior::Immediate)?;
    seed_federation_host_identity(&transaction)?;
    transaction.commit()?;
    Ok(())
}

/// The epoch and the domain mutation commit in the SAME existing state writer
/// transaction. A stale snapshot cannot lower the epoch or change host_id.
fn write_federation_host_generation(
    transaction: &rusqlite::Transaction<'_>,
    metadata: &PersistedState,
) -> Result<()> {
    let version = federation_meta_version(transaction)?;
    let image = federation_persist_image(metadata);
    if version > metadata.federation.version {
        return Err(anyhow!("stale federation content in state write"));
    }
    if version > 0 {
        let raw = read_federation_persist_image(transaction)?
            .ok_or_else(|| anyhow!("missing committed federation image"))?;
        verify_federation_image_marker(transaction, &raw)?;
        if version == metadata.federation.version
            && normalize_federation_persist_image(&raw)? != image
        {
            return Err(anyhow!("stale federation content in state write"));
        }
    }
    let digest = sha256_hex(&serde_json::to_vec(&image)?);
    let Some(expected) = metadata.federation.local.as_ref() else {
        return Ok(());
    };
    let stored = read_federation_host_identity(transaction)?;
    if stored.host_id != expected.host_id || stored.host_generation > expected.host_generation {
        return Err(anyhow!("stale federation identity in state write"));
    }
    expected
        .validate()
        .map_err(|_| anyhow!("invalid federation identity in state write"))?;
    transaction.execute(
        "UPDATE meta SET value=?1 WHERE key='federation_host_generation'",
        [expected.host_generation.to_string()],
    )?;
    if metadata.federation.version > 0 {
        for (key, value) in [
            (
                "federation_state_version",
                metadata.federation.version.to_string(),
            ),
            ("federation_image_digest", digest),
        ] {
            transaction.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                rusqlite::params![key, value])?;
        }
    }
    Ok(())
}

fn bind_loaded_federation(
    inner: &mut StateInner,
    connection: &rusqlite::Connection,
    path: &FsPath,
) -> Result<()> {
    let local = read_federation_host_identity(connection)?;
    if let Some(saved) = &inner.federation.local {
        if saved != &local {
            return Err(anyhow!("federation metadata identity mismatch"));
        }
    } else if inner.federation.version != 0
        || !inner.federation.peers.is_empty()
        || !inner.federation.placements.is_empty()
    {
        return Err(anyhow!("federation state has no host identity"));
    }
    inner.federation.local = Some(local.clone());
    let stored_version = federation_meta_version(connection)?;
    if inner.federation.version > 0 || stored_version > 0 {
        let raw = read_federation_persist_image(connection)?
            .ok_or_else(|| anyhow!("missing committed federation image"))?;
        verify_federation_image_marker(connection, &raw)?;
        if stored_version != inner.federation.version {
            return Err(anyhow!("federation committed content marker mismatch"));
        }
    }
    let binding = normalize_local_user_facing_path(&fs::canonicalize(path)?.to_string_lossy());
    if inner.federation.store_binding.is_empty() {
        if !inner.federation.peers.is_empty() || !inner.federation.placements.is_empty() {
            return Err(anyhow!("enrolled federation state has no store binding"));
        }
        inner.federation.store_binding = binding;
    } else {
        inner.federation_continuity_lost = inner.federation.store_binding != binding;
    }
    inner
        .preferences
        .federation
        .validate(&local)
        .map_err(|_| anyhow!("invalid persisted federation configuration"))?;
    inner.federation.validate()?;
    restore_federation_reference_publications(inner, connection)?;
    if inner.federation_continuity_lost && inner.federation.admission_quarantine.is_none() {
        persist_detected_federation_quarantine(inner, connection, path)?;
    }
    inner.federation_durable = true;
    Ok(())
}

/// Boot reuses its validated connection and the existing writer admission.
/// Persist the loss before publishing any state, even if the store is later
/// moved back to its original pathname. Preserve unrelated metadata fields.
fn persist_detected_federation_quarantine(
    inner: &mut StateInner,
    connection: &rusqlite::Connection,
    path: &FsPath,
) -> Result<()> {
    reject_existing_sqlite_state_path_redirection(path)?;
    let writer = sqlite_state_write_lock(path);
    let guard = lock_sqlite_state_writer(&writer);
    let transaction =
        rusqlite::Transaction::new_unchecked(connection, rusqlite::TransactionBehavior::Immediate)?;
    let encoded: String = transaction.query_row(
        "SELECT value_json FROM app_state WHERE key=?1",
        [SQLITE_METADATA_KEY],
        |row| row.get(0),
    )?;
    let mut raw: Value = serde_json::from_str(&encoded)?;
    let mut current: PersistedState = serde_json::from_value(raw.clone())?;
    if federation_persist_image(&current)
        != federation_persist_image(&PersistedState::metadata_from_inner(inner))
    {
        return Err(anyhow!("federation changed during continuity quarantine"));
    }
    let mut quarantined = inner.federation.clone();
    quarantined.admission_quarantine = Some(FederationQuarantineReason::StoreBindingChanged);
    quarantined.version = federation_next_generation(quarantined.version)
        .map_err(|_| anyhow!("federation generation exhausted during quarantine"))?;
    current.federation = quarantined.clone();
    raw["federation"] = serde_json::to_value(&quarantined)?;
    raw["preferences"]["federation"] = serde_json::to_value(&current.preferences.federation)?;
    write_federation_host_generation(&transaction, &current)?;
    transaction.execute(
        "UPDATE app_state SET value_json=?1 WHERE key=?2",
        rusqlite::params![serde_json::to_string(&raw)?, SQLITE_METADATA_KEY],
    )?;
    transaction.commit()?;
    verify_federation_write(connection, &current)?;
    drop(guard);
    verify_persist_commit_integrity(path)?;
    inner.federation = quarantined;
    Ok(())
}

fn prepare_federation_boot(
    inner: &mut StateInner,
    path: &FsPath,
    connection: &mut Option<rusqlite::Connection>,
) -> Result<()> {
    if connection.is_none() {
        if let Some(parent) = path.parent() {
            create_local_state_directory(parent)?;
        }
        let fresh = open_sqlite_state_connection_unconfigured(path)?;
        ensure_sqlite_state_schema_for_path(&fresh, path)?;
        *connection = Some(fresh);
    }
    bind_loaded_federation(
        inner,
        connection.as_ref().expect("boot connection prepared"),
        path,
    )
}
