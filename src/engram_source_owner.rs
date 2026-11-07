// Host-local executable provenance for the producer-owned source command.
// Metadata proves the executable/schema, never a source verdict or claim.
static ENGRAM_SOURCE_OWNER_PERMITS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramSourceOwnerPin {
    binary_path: String,
    sha256: String,
    binary_version: String,
    source_version: u32,
}

impl EngramSourceOwnerPin {
    fn validate(&self) -> Result<()> {
        if self.binary_path.len() > 4096 || self.binary_path.chars().any(char::is_control)
            || !FsPath::new(&self.binary_path).is_absolute()
            || !is_lowercase_sha256(&self.sha256) || self.source_version != 1
            || !engram_source_binary_version_valid(&self.binary_version)
        {
            bail!("Engram source owner pin is malformed or unsupported");
        }
        Ok(())
    }
}

fn test_run_request_source_owner(request: &Value) -> Result<Option<EngramSourceOwnerPin>, ApiError> {
    request.get("sourceOwner").map(|value| {
        let pin: EngramSourceOwnerPin = serde_json::from_value(value.clone())
            .map_err(|_| ApiError::bad_request("test run source owner pin is malformed"))?;
        pin.validate().map_err(|_| ApiError::bad_request("test run source owner pin is malformed or unsupported"))?;
        Ok(pin)
    }).transpose()
}

fn engram_source_binary_version_valid(version: &str) -> bool {
    version.starts_with("engram ") && version.len() > 7 && version.len() <= 512
        && version.bytes().all(|byte| (b' '..=b'~').contains(&byte))
}

fn engram_source_binary(configured: &FsPath, deadline: std::time::Instant) -> Result<PathBuf> {
    engram_source_before_deadline(deadline)?;
    if !configured.is_absolute() {
        bail!("Engram source owner requires an explicitly configured absolute binaryPath; PATH lookup is refused");
    }
    let binary = fs::canonicalize(configured).context("Engram source owner executable is unavailable")?;
    if !binary.is_file() {
        bail!("Engram source owner executable is not a file");
    }
    // The digest pins the program itself, not an interpreter's mutable script
    // or a command resolved through PATHEXT. Other Engram transports are unchanged.
    #[cfg(windows)]
    if !binary.extension().and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe")) {
        bail!("Engram source owner requires a native executable");
    }
    #[cfg(unix)]
    {
        let mut header = [0u8; 4];
        let mut file = fs::File::open(&binary)?;
        std::io::Read::read_exact(&mut file, &mut header)
            .context("Engram source owner native executable header is unavailable")?;
        if !engram_source_native_header(&header) {
            bail!("Engram source owner requires a native ELF or Mach-O executable; interpreter scripts are refused");
        }
    }
    engram_source_before_deadline(deadline)?;
    Ok(binary)
}

#[cfg(any(unix, test))]
fn engram_source_native_header(header: &[u8; 4]) -> bool {
    matches!(*header, [0x7f, b'E', b'L', b'F']
        | [0xfe, 0xed, 0xfa, 0xce] | [0xce, 0xfa, 0xed, 0xfe]
        | [0xfe, 0xed, 0xfa, 0xcf] | [0xcf, 0xfa, 0xed, 0xfe]
        | [0xca, 0xfe, 0xba, 0xbe] | [0xbe, 0xba, 0xfe, 0xca]
        | [0xca, 0xfe, 0xba, 0xbf] | [0xbf, 0xba, 0xfe, 0xca])
}

fn engram_source_binary_hash(binary: &FsPath, deadline: std::time::Instant) -> Result<String> {
    engram_source_before_deadline(deadline)?;
    let mut file = fs::File::open(binary).context("Engram source owner executable cannot be read")?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        engram_source_before_deadline(deadline)?;
        let count = std::io::Read::read(&mut file, &mut buffer)?;
        if count == 0 { break; }
        hash.update(&buffer[..count]);
    }
    engram_source_before_deadline(deadline)?;
    Ok(format!("{:x}", hash.finalize()))
}

fn engram_source_owner_verify_bytes(pin: &EngramSourceOwnerPin, deadline: std::time::Instant) -> Result<PathBuf> {
    pin.validate()?;
    let binary = engram_source_binary(FsPath::new(&pin.binary_path), deadline)
        .context("Engram source owner executable no longer matches the pin")?;
    if normalize_user_facing_path(&binary).to_str() != Some(pin.binary_path.as_str())
        || engram_source_binary_hash(&binary, deadline)? != pin.sha256
    {
        bail!("Engram source owner executable no longer matches the pin");
    }
    Ok(binary)
}

fn engram_source_owner_run(
    binary: &FsPath, cwd: &FsPath, args: &[String],
    deadline: std::time::Instant, limit: usize,
) -> Result<std::process::Output> {
    engram_source_before_deadline(deadline)?;
    let mut command = BoundedReadCommand::new(binary);
    command.current_dir(cwd).args(args);
    // Source opens no store. It owns Git isolation; inherited Git selectors
    // must not redirect even its initial repository discovery.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().to_ascii_uppercase().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    run_bounded_read_command(&mut command, deadline, limit, true)
}

fn engram_source_owner_metadata_with(
    configured: &FsPath, root: &FsPath, cwd: &FsPath, deadline: std::time::Instant,
    run: impl FnMut(&FsPath, &FsPath, &[String], std::time::Instant, usize) -> Result<std::process::Output>,
) -> Result<EngramSourceOwnerPin> {
    engram_source_owner_observe_with(configured, root, cwd, deadline, run)
        .map(|(pin, _)| pin)
}

// Bootstrap and the first measurement share this actual observation. Metadata-only
// callers discard its verdict; a caller measuring now consumes it without a
// second source invocation. It is never evidence for a later capture or root.
fn engram_source_owner_observe_with(
    configured: &FsPath, root: &FsPath, cwd: &FsPath, deadline: std::time::Instant,
    mut run: impl FnMut(&FsPath, &FsPath, &[String], std::time::Instant, usize) -> Result<std::process::Output>,
) -> Result<(EngramSourceOwnerPin, EngramSourceObservation)> {
    let deadline = deadline.min(std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET);
    let binary = resolve_engram_source_program(configured, root, deadline)?;
    let sha256 = engram_source_binary_hash(&binary, deadline)?;
    let version = run(&binary, cwd, &["--version".into()], deadline, 4096)?;
    engram_source_before_deadline(deadline)?;
    if !version.status.success() || !version.stderr.is_empty() || version.stdout.len() > 4096 {
        bail!("Engram source owner version probe failed");
    }
    let binary_version = std::str::from_utf8(&version.stdout)
        .context("Engram source owner version is not UTF-8")?.trim().to_owned();
    if !engram_source_binary_version_valid(&binary_version) {
        bail!("Engram source owner version probe returned unsupported output");
    }
    let root = fs::canonicalize(root).context("Engram source owner project root is unavailable")?;
    let args = engram_source_command_args(&root, deadline)?;
    let output = run(&binary, cwd, &args, deadline, ENGRAM_SOURCE_JSON_LIMIT + 1)?;
    if !output.stderr.is_empty() {
        bail!("Engram source owner schema probe emitted stderr");
    }
    // --version reports the durable-store schema, not the source schema.
    // Only an actual validated source envelope establishes sourceVersion.
    // A well-formed unavailable result establishes the schema too, but cannot
    // supply a complete source identity.
    let observed = parse_engram_source_observation(&output.stdout, output.status.code(), &root, deadline)?;
    if resolve_engram_source_program(configured, &root, deadline)
        .context("Engram source owner executable changed during probing")? != binary
        || engram_source_binary_hash(&binary, deadline)? != sha256
    {
        bail!("Engram source owner executable changed during probing");
    }
    let pin = EngramSourceOwnerPin {
        binary_path: normalize_user_facing_path(&binary).to_str()
            .context("Engram source owner executable path is not UTF-8")?.to_owned(),
        sha256, binary_version, source_version: observed.version,
    };
    pin.validate()?;
    engram_source_before_deadline(deadline)?;
    Ok((pin, observed))
}

// The caller owns source-root/claim/store authority and retains the same pin
// between captures. This function returns the entire owner value, including
// local changes; it imposes no clean-commit policy on future consumers.
#[cfg_attr(not(test), allow(dead_code))]
fn engram_source_id_until(
    pin: &EngramSourceOwnerPin, root: &FsPath, deadline: std::time::Instant,
) -> Result<EngramSourceId> {
    engram_source_id_with(pin, root, deadline, engram_source_owner_run)
}

fn engram_source_id_with(
    pin: &EngramSourceOwnerPin, root: &FsPath, deadline: std::time::Instant,
    mut run: impl FnMut(&FsPath, &FsPath, &[String], std::time::Instant, usize)
        -> Result<std::process::Output>,
) -> Result<EngramSourceId> {
    let deadline = deadline.min(std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET);
    let binary = engram_source_owner_verify_bytes(pin, deadline)?;
    let root = fs::canonicalize(root).context("Engram source root is unavailable")?;
    let args = engram_source_command_args(&root, deadline)?;
    let output = run(&binary, &root, &args, deadline, ENGRAM_SOURCE_JSON_LIMIT + 1)?;
    engram_source_before_deadline(deadline)?;
    if !output.stderr.is_empty() {
        bail!("Engram source measurement emitted stderr");
    }
    let observed = parse_engram_source_observation(&output.stdout, output.status.code(), &root, deadline)?;
    engram_source_owner_verify_bytes(pin, deadline)?;
    observed.into_source_id(deadline)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramSourceOwnerAuthority {
    project_id: String,
    project_root: String,
    settings: EngramProjectSettings,
    host_settings: EngramHostSettings,
    session_generation: u64,
    native_creation_id: String,
}

fn engram_source_owner_authority_locked(inner: &StateInner, session: &str)
    -> Result<EngramSourceOwnerAuthority, ApiError>
{
    if AppState::engram_session_has_child_binding_shape_locked(inner, session) {
        return Err(ApiError::conflict("source owner metadata requires a native local root session"));
    }
    let index = inner.find_session_index(session)
        .ok_or_else(|| ApiError::not_found("source owner session is unavailable"))?;
    let record = &inner.sessions[index];
    if record.hidden || !record.is_local_session() {
        return Err(ApiError::conflict("source owner metadata requires a visible native local root session"));
    }
    let project = engram_project_for_session_locked(inner, session)
        .filter(|project| project.remote_id == LOCAL_REMOTE_ID)
        .ok_or_else(|| ApiError::conflict("source owner session has no local project"))?;
    let settings = project.engram.clone()
        .ok_or_else(|| ApiError::conflict("Engram source owner binaryPath is not configured for this project"))?;
    Ok(EngramSourceOwnerAuthority {
        project_id: project.id.clone(), project_root: project.root_path.clone(), settings,
        host_settings: inner.preferences.engram.clone(),
        session_generation: record.active_turn_generation,
        native_creation_id: record.federation_native_creation_id.clone(),
    })
}

fn engram_source_owner_error(error: anyhow::Error) -> ApiError {
    let reason: String = error.to_string().chars().take(512)
        .map(|ch| if ch.is_control() { '?' } else { ch }).collect();
    ApiError::conflict(format!("source owner unavailable: {reason}"))
}

impl AppState {
    fn source_owner_metadata_until(&self, session: &str, deadline: std::time::Instant)
        -> Result<EngramSourceOwnerPin, ApiError>
    {
        self.source_owner_metadata_with(session, deadline, engram_source_owner_run)
    }

    fn source_owner_metadata_with(&self, session: &str, deadline: std::time::Instant,
        run: impl FnMut(&FsPath, &FsPath, &[String], std::time::Instant, usize) -> Result<std::process::Output>)
        -> Result<EngramSourceOwnerPin, ApiError>
    {
        self.source_owner_metadata_snapshot_with(session, deadline, run).map(|(pin, _)| pin)
    }

    fn source_owner_metadata_snapshot_with(&self, session: &str, deadline: std::time::Instant,
        run: impl FnMut(&FsPath, &FsPath, &[String], std::time::Instant, usize) -> Result<std::process::Output>)
        -> Result<(EngramSourceOwnerPin, EngramSourceOwnerAuthority), ApiError>
    {
        let authority = engram_source_owner_authority_locked(
            &self.inner.lock().expect("state mutex poisoned"), session)?;
        let configured = authority.settings.binary_path.as_deref()
            .filter(|path| !path.is_empty())
            .ok_or_else(|| ApiError::conflict("Engram source owner binaryPath is not configured for this project"))?;
        let cwd = engram_host_workdir(self.persistence_path.as_ref())
            .map_err(|error| engram_source_owner_error(error.into()))?;
        let pin = engram_source_owner_metadata_with(FsPath::new(configured),
            FsPath::new(&authority.project_root), &cwd, deadline, run)
            .map_err(engram_source_owner_error)?;
        let current = engram_source_owner_authority_locked(
            &self.inner.lock().expect("state mutex poisoned"), session)?;
        if current != authority {
            return Err(ApiError::conflict("source owner session or project configuration changed during probing"));
        }
        engram_source_before_deadline(deadline).map_err(engram_source_owner_error)?;
        Ok((pin, authority))
    }
}

async fn get_session_source_owner(
    State(state): State<AppState>, AxumPath(session): AxumPath<String>,
    request: axum::extract::Request,
) -> Result<Json<EngramSourceOwnerPin>, ApiError> {
    if request.uri().query().is_some() || axum::body::to_bytes(request.into_body(), 0).await.is_err() {
        return Err(ApiError::bad_request("source owner metadata accepts no query or body parameters"));
    }
    let _permit = ENGRAM_SOURCE_OWNER_PERMITS.clone().try_acquire_owned()
        .map_err(|_| ApiError::from_status(StatusCode::TOO_MANY_REQUESTS, "source owner probe busy"))?;
    let deadline = std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET;
    run_blocking_api(move || {
        let _permit = _permit;
        state.source_owner_metadata_until(&session, deadline).map(Json)
    }).await
}
