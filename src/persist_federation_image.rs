// Owns raw committed federation-image validation and exact writer readback.
// Does not own schema changes, transport or an independent rollback witness.
// Split from persist_federation.rs to keep stored values separate from defaults.

fn federation_persist_image(metadata: &PersistedState) -> Value {
    json!({"domain": metadata.federation, "settings": metadata.preferences.federation})
}

fn read_federation_persist_image(connection: &rusqlite::Connection) -> Result<Option<Value>> {
    let encoded: Option<String> = connection
        .query_row(
            "SELECT value_json FROM app_state WHERE key=?1",
            [SQLITE_METADATA_KEY],
            |row| row.get(0),
        )
        .optional()?;
    encoded
        .map(|encoded| {
            let raw: Value = serde_json::from_str(&encoded)?;
            // Preserve the stored subtrees exactly: serde defaults belong only in
            // the loaded domain, never in the digest of a previous schema image.
            Ok(json!({"domain":raw["federation"], "settings":raw["preferences"]["federation"]}))
        })
        .transpose()
}

fn federation_meta_version(connection: &rusqlite::Connection) -> Result<u64> {
    let version: Option<String> = connection
        .query_row(
            "SELECT value FROM meta WHERE key='federation_state_version'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(version
        .map(|value| value.parse::<u64>())
        .transpose()?
        .unwrap_or(0))
}

fn verify_federation_image_marker(connection: &rusqlite::Connection, raw: &Value) -> Result<()> {
    let version = federation_meta_version(connection)?;
    let image_version = raw["domain"]["version"].as_u64().unwrap_or(0);
    if version == 0 && image_version == 0 {
        return Ok(());
    }
    let digest: String = connection.query_row(
        "SELECT value FROM meta WHERE key='federation_image_digest'",
        [],
        |row| row.get(0),
    )?;
    if version != image_version || digest != sha256_hex(&serde_json::to_vec(raw)?) {
        return Err(anyhow!("federation committed content marker mismatch"));
    }
    Ok(())
}

/// Used only after the raw digest has been checked. Same-version writes may
/// add current serde defaults, but cannot change any loaded domain authority.
fn normalize_federation_persist_image(raw: &Value) -> Result<Value> {
    let domain: FederationState = serde_json::from_value(raw["domain"].clone())?;
    let settings: FederationSettings = serde_json::from_value(raw["settings"].clone())?;
    Ok(json!({"domain":domain, "settings":settings}))
}

fn verify_federation_write(
    connection: &rusqlite::Connection,
    metadata: &PersistedState,
) -> Result<()> {
    if metadata.federation.local.is_some()
        && !federation_committed_image_matches(connection, &federation_persist_image(metadata))?
    {
        return Err(anyhow!("federation commit readback mismatch"));
    }
    Ok(())
}

fn federation_committed_image_matches(
    connection: &rusqlite::Connection,
    expected: &Value,
) -> Result<bool> {
    let Some(raw) = read_federation_persist_image(connection)? else {
        return Ok(false);
    };
    if raw != *expected
        || raw["domain"]["local"]
            != serde_json::to_value(read_federation_host_identity(connection)?)?
    {
        return Ok(false);
    }
    verify_federation_image_marker(connection, &raw)?;
    Ok(true)
}
