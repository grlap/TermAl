// Owns read-only resolution of enrollment references through the OS secret store.
// Does not enroll peers, mint secrets, or change global keyring configuration.
// New federation support; OS backend choice matches telegram_settings.rs.

struct FederationTransportCredentials {
    capability: FederationPairCapability,
    known_hosts: Option<PathBuf>,
    material_fingerprint: [u8; 32],
}

const FEDERATION_KEYRING_SERVICE: &str = "TermAl.Federation";

fn resolve_federation_credentials(
    snapshot: &FederationPeerSnapshot,
) -> Result<FederationTransportCredentials, ApiError> {
    let store = federation_credential_store().map_err(|_| federation_credential_error())?;
    let read = |kind: &str, reference: &str| -> Result<String, ApiError> {
        validate_federation_reference(reference)?;
        let user = format!("{}:{kind}:{reference}", snapshot.local.host_id);
        let entry = store
            .build(FEDERATION_KEYRING_SERVICE, &user, None)
            .map_err(|_| federation_credential_error())?;
        entry
            .get_password()
            .map_err(|_| federation_credential_error())
    };
    resolve_federation_credentials_with(snapshot, read)
}

fn resolve_federation_credentials_with(
    snapshot: &FederationPeerSnapshot,
    read: impl Fn(&str, &str) -> Result<String, ApiError>,
) -> Result<FederationTransportCredentials, ApiError> {
    snapshot.validate()?;
    validate_federation_reference(&snapshot.pair_capability_ref)?;
    let capability = FederationPairCapability::new(read("pair", &snapshot.pair_capability_ref)?)?;
    let mut material = Sha256::new();
    material.update(capability.0.as_bytes());
    let known_hosts = if snapshot.local_role == FederationRole::Central {
        validate_federation_reference(&snapshot.endpoint.known_hosts_ref)?;
        validate_federation_reference(&snapshot.endpoint.host_key_ref)?;
        let path = PathBuf::from(read("known-hosts", &snapshot.endpoint.known_hosts_ref)?);
        if !path.is_absolute() {
            return Err(federation_credential_error());
        }
        let metadata = fs::metadata(&path).map_err(|_| federation_credential_error())?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 64 * 1024 {
            return Err(federation_credential_error());
        }
        let mut pinned = Vec::new();
        let file = fs::File::open(&path).map_err(|_| federation_credential_error())?;
        std::io::Read::take(file, 64 * 1024 + 1)
            .read_to_end(&mut pinned)
            .map_err(|_| federation_credential_error())?;
        if pinned.is_empty() || pinned.len() > 64 * 1024 {
            return Err(federation_credential_error());
        }
        material.update(snapshot.endpoint.host_key_ref.as_bytes());
        material.update(path.as_os_str().as_encoded_bytes());
        material.update(&pinned);
        Some(path)
    } else {
        None
    };
    Ok(FederationTransportCredentials {
        capability,
        known_hosts,
        material_fingerprint: material.finalize().into(),
    })
}

fn federation_credential_error() -> ApiError {
    ApiError::from_status(
        StatusCode::SERVICE_UNAVAILABLE,
        "federation enrollment credential unavailable",
    )
}

#[cfg(windows)]
fn federation_credential_store() -> keyring_core::Result<Arc<keyring_core::CredentialStore>> {
    Ok(windows_native_keyring_store::Store::new_with_configuration(
        &HashMap::<&str, &str>::new(),
    )?)
}

#[cfg(target_os = "macos")]
fn federation_credential_store() -> keyring_core::Result<Arc<keyring_core::CredentialStore>> {
    Ok(
        apple_native_keyring_store::keychain::Store::new_with_configuration(
            &HashMap::<&str, &str>::new(),
        )?,
    )
}

#[cfg(target_os = "linux")]
fn federation_credential_store() -> keyring_core::Result<Arc<keyring_core::CredentialStore>> {
    Ok(
        zbus_secret_service_keyring_store::Store::new_with_configuration(
            &HashMap::<&str, &str>::new(),
        )?,
    )
}
