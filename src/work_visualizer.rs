// New project Work read boundary. Owns metadata-only discovery and the host
// reader: an identity built from the operator-validated project settings, so
// the view reads the established store whether or not an agent session is
// bound. Never owns integration enablement, doctor runs, binding creation,
// tracker writes, SQLite access, or agent-turn scheduling.

/// The seat kind the host itself occupies in Engram's `{developer}/{kind}`
/// actor grammar. Reads never assign, claim or hand off, so this identity only
/// labels the reader; it never appears as a holder.
const WORK_HOST_READER_KIND: &str = "termal";

/// One stable session for every host read. Engram derives `--after` cursors
/// from the session, so a constant keeps continuations valid across host
/// restarts. Engram's read words validate the session without registering it:
/// only stateful words register sessions, and only inside Engram's
/// process-default namespace, which this name is outside of.
const WORK_HOST_READER_SESSION_ID: &str = "termal-work-view";

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkReadTarget {
    connection: EngramConnectionConfig,
    store: EngramAuthorityStoreKey,
    /// The reader identity on the wire: a digest of the configuration the read
    /// runs under. A continuation or detail read presenting another key was
    /// issued against another configuration and is refused, never served
    /// from a store the caller did not page through.
    reader_key: String,
}

/// The host reader for a project, or the reason there is none. Every input
/// is operator-established project state that survives a host restart; no
/// agent session, binding or runtime descriptor takes part.
fn work_host_reader(inner: &StateInner, project: &Project) -> Result<WorkReadTarget, &'static str> {
    if project.remote_id != LOCAL_REMOTE_ID {
        return Err("Remote project Work reads are not supported yet");
    }
    if inner.engram_project_resets.contains(&project.id) {
        return Err("Engram project reset is in progress; retry when it completes");
    }
    let settings = project
        .engram
        .as_ref()
        .filter(|settings| settings.is_base_enabled())
        .ok_or("Engram is not enabled by the operator; this view does not enable it")?;
    let (Some(binary_path), Some(home)) =
        (settings.binary_path.as_deref(), settings.home.as_deref())
    else {
        return Err(
            "Engram binary or home is not configured for this project; verify the integration first",
        );
    };
    let store = settings.authority_store_key.clone().ok_or(
        "Engram store identity is not established for this project; verify the integration first",
    )?;
    let developer_name = inner.preferences.engram.developer_name.trim();
    if developer_name.is_empty() {
        return Err("Engram developer name is not set; the host reader has no identity");
    }
    let connection = EngramConnectionConfig {
        binary_path: PathBuf::from(binary_path),
        project_root: PathBuf::from(&project.root_path),
        project_file: PathBuf::from(&project.root_path).join(".engram-project"),
        home: PathBuf::from(home),
        actor_id: format!("{developer_name}/{WORK_HOST_READER_KIND}"),
        actor_context: None,
        session_id: WORK_HOST_READER_SESSION_ID.to_owned(),
    };
    let reader_key = work_reader_key(&connection, &store);
    Ok(WorkReadTarget {
        connection,
        store,
        reader_key,
    })
}

/// `host:` plus a digest of everything a read runs under. The same
/// configuration yields the same key after a restart; any change to the
/// binary, home, store or identity yields another.
fn work_reader_key(connection: &EngramConnectionConfig, store: &EngramAuthorityStoreKey) -> String {
    let mut digest = Sha256::new();
    for part in [
        connection.binary_path.as_os_str(),
        connection.home.as_os_str(),
        connection.project_root.as_os_str(),
        connection.project_file.as_os_str(),
        store.database_path.as_os_str(),
        std::ffi::OsStr::new(&store.project_id),
        std::ffi::OsStr::new(&connection.actor_id),
        std::ffi::OsStr::new(&connection.session_id),
    ] {
        digest.update(part.as_encoded_bytes());
        digest.update([0]);
    }
    let hex = format!("{:x}", digest.finalize());
    format!("host:{}", &hex[..16])
}

fn work_source_status(
    source: &'static str,
    state: &'static str,
    message: impl Into<String>,
) -> WorkSourceStatus {
    WorkSourceStatus {
        source,
        state,
        message: message.into(),
    }
}

impl AppState {
    /// The project and its current host reader, or the reason there is none.
    /// A `reader` key supplied by the caller must name that reader exactly:
    /// pages and detail windows belong to the configuration they were read
    /// under.
    fn work_read_snapshot(
        &self,
        project_id: &str,
        reader: Option<&str>,
    ) -> Result<(Project, Result<WorkReadTarget, &'static str>), ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let project = inner
            .find_project(project_id)
            .ok_or_else(|| ApiError::not_found("Work project not found"))?
            .clone();
        let target = work_host_reader(&inner, &project).and_then(|target| {
            if reader.is_some_and(|key| key != target.reader_key) {
                Err("Work reader changed; discard pages and refresh")
            } else {
                Ok(target)
            }
        });
        Ok((project, target))
    }

    #[cfg(test)]
    fn list_project_work(
        &self,
        project_id: &str,
        query: WorkListQuery,
    ) -> Result<WorkListResponse, ApiError> {
        self.list_project_work_with_admission(project_id, query, || Ok(()), || Ok(()))
    }

    /// Test seam: the production admission closures with the test read budget
    /// (no launch reserve), so fixture coverage is never decided by the clock.
    #[cfg(test)]
    fn list_project_work_with_admission<P, B>(
        &self,
        project_id: &str,
        query: WorkListQuery,
        admit: impl FnOnce() -> Result<P, ApiError>,
        admit_beads: impl FnOnce() -> Result<B, ApiError>,
    ) -> Result<WorkListResponse, ApiError> {
        self.list_project_work_with_options(
            project_id,
            query,
            admit,
            admit_beads,
            BeadsReadOptions::for_tests(),
            &std::sync::atomic::AtomicBool::new(false),
        )
    }

    fn list_project_work_with_options<P, B>(
        &self,
        project_id: &str,
        query: WorkListQuery,
        admit: impl FnOnce() -> Result<P, ApiError>,
        admit_beads: impl FnOnce() -> Result<B, ApiError>,
        beads_options: BeadsReadOptions,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<WorkListResponse, ApiError> {
        query.validate()?;
        let (project, target) = self.work_read_snapshot(project_id, query.reader_id.as_deref())?;
        let mut sources = Vec::new();
        let mut beads_target = None;
        if project.remote_id != LOCAL_REMOTE_ID {
            sources.push(work_source_status(
                "engram",
                "unavailable",
                "Remote project Work reads are not supported yet",
            ));
        } else {
            let root = FsPath::new(&project.root_path);
            let (beads_status, detected) = beads_source_status(&project, &query);
            beads_target = detected;
            sources.push(beads_status);
            let status = match fs::metadata(root.join(".engram-project")) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    work_source_status("engram", "absent", "No .engram-project declaration")
                }
                Err(e) => work_source_status(
                    "engram",
                    "unavailable",
                    format!("Cannot inspect .engram-project: {e}"),
                ),
                Ok(_)
                    if !project
                        .engram
                        .as_ref()
                        .is_some_and(EngramProjectSettings::is_base_enabled) =>
                {
                    work_source_status(
                        "engram",
                        "disabled",
                        "Engram is not enabled by the operator; this view does not enable it",
                    )
                }
                Ok(_) => match &target {
                    Err(reason) => work_source_status("engram", "unavailable", *reason),
                    Ok(target) => match validate_work_read_target(target) {
                        Ok(()) => work_source_status(
                            "engram",
                            "ready",
                            "Engram reads use the host reader over the validated project store",
                        ),
                        Err(e) => work_source_status("engram", "unavailable", e.message),
                    },
                },
            };
            sources.push(status);
        }
        let ready = sources
            .iter()
            .any(|s| s.source == "engram" && s.state == "ready");
        if !ready && query.after.is_some() {
            return Err(ApiError::conflict(
                "Work reader is no longer available; discard pages and refresh",
            ));
        }
        // A continuation only re-reads the Engram page; the Beads page has no
        // cursor and is loaded once with the first page.
        let read_beads = beads_target.is_some() && query.after.is_none();
        let (page, reader_id) = if ready {
            let read_engram = || -> Result<(WorkPage, String), ApiError> {
                // A reader that vanished between detection and read is a
                // source condition like any other 409 below.
                let target = target.map_err(ApiError::conflict)?;
                // Engram admission covers exactly one bounded CLI read and is
                // released before the Beads snapshot starts. The caller may
                // have gone while the read waited for it.
                let _permit = admit()?;
                work_read_not_abandoned(cancelled)?;
                // Waiting for admission may outlive the installed reader.
                self.validate_work_read_still_current(project_id, &target)?;
                let value = run_work_read_command(&target.connection, &query.arguments())?;
                self.validate_work_read_still_current(project_id, &target)?;
                let page = normalize_engram_work_page(value)?;
                if query.after.is_none() && page.shown_before != 0 {
                    return Err(ApiError::bad_gateway(
                        "engram work ls: unexpected continuation page",
                    ));
                }
                Ok((page, target.reader_key.clone()))
            };
            match read_engram() {
                Ok((page, reader_key)) => (Some(page), Some(reader_key)),
                // A continuation serves only the Engram page: its busy, stale
                // or malformed outcome stays a hard error for the caller. So
                // does anything that is not a source condition (a closed
                // limiter or another internal failure).
                Err(error) if query.after.is_some() || !is_work_source_condition(error.status) => {
                    return Err(error);
                }
                // A first page still serves the other source: Engram becomes
                // an explicit per-source error, never a hidden Beads snapshot.
                Err(error) => {
                    if let Some(status) = sources.iter_mut().find(|s| s.source == "engram") {
                        *status = work_source_status("engram", "error", error.message);
                    }
                    (None, None)
                }
            }
        } else {
            (None, None)
        };
        let beads = match beads_target.filter(|_| read_beads) {
            // Beads has its own admission and deadline. A busy or failed Beads
            // read is an explicit per-source error, never an empty page and
            // never a lost Engram result.
            Some(target) => {
                match admit_beads().and_then(|_permit| {
                    read_beads_work_page(&target, &query, beads_options, cancelled)
                }) {
                    Ok(page) => Some(page),
                    // The same rule as Engram: only source conditions become a
                    // per-source error; an internal failure fails the request.
                    Err(error) if !is_work_source_condition(error.status) => {
                        return Err(error);
                    }
                    Err(error) => {
                        if let Some(status) = sources.iter_mut().find(|s| s.source == "beads") {
                            *status = work_source_status("beads", "error", error.message);
                        }
                        None
                    }
                }
            }
            None => None,
        };
        Ok(WorkListResponse {
            sources,
            reader_id,
            page,
            beads,
            observed_at: chrono::Utc::now().to_rfc3339(),
        })
    }

    fn validate_work_read_still_current(
        &self,
        project_id: &str,
        target: &WorkReadTarget,
    ) -> Result<(), ApiError> {
        let (_, current) = self.work_read_snapshot(project_id, Some(&target.reader_key))?;
        if current.as_ref().ok() != Some(target) {
            return Err(ApiError::conflict(
                "Work reader changed during read; discard pages and refresh",
            ));
        }
        validate_work_read_target(target)
    }
}

fn validate_work_read_target(target: &WorkReadTarget) -> Result<(), ApiError> {
    let c = &target.connection;
    validate_work_read_binary(c)?;
    if !c.home.is_absolute()
        || !c.project_root.is_absolute()
        || !c.project_file.is_absolute()
        || !target.store.database_path.is_absolute()
    {
        return Err(ApiError::conflict(
            "Work host binding must use absolute paths",
        ));
    }
    let metadata = fs::metadata(&c.project_file).map_err(|e| {
        ApiError::conflict(format!("Cannot read established Work declaration: {e}"))
    })?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 4096 {
        return Err(ApiError::conflict(
            "Work declaration is empty, oversized, or not a file",
        ));
    }
    let mut declaration = String::new();
    fs::File::open(&c.project_file)
        .and_then(|file| {
            io::Read::read_to_string(&mut io::Read::take(file, 4097), &mut declaration)
        })
        .map_err(|e| ApiError::conflict(format!("Cannot read Work declaration: {e}")))?;
    if declaration.len() > 4096 || declaration.trim() != target.store.project_id {
        return Err(ApiError::conflict(
            "Work declaration no longer matches the established store; verify the integration first",
        ));
    }
    // Engram schema-1 resolves the project store from SHA-256(project id).
    // Check that actual resolution, not only the old admitted path's existence.
    let resolved = fs::canonicalize(work_database_path(&c.home, &target.store.project_id))
        .map_err(|e| {
            ApiError::conflict(format!("Established Work store cannot be resolved: {e}"))
        })?;
    let admitted = fs::canonicalize(&target.store.database_path)
        .map_err(|e| ApiError::conflict(format!("Admitted Work store cannot be resolved: {e}")))?;
    if resolved != admitted || normalize_user_facing_path(&admitted) != target.store.database_path {
        return Err(ApiError::conflict(
            "Work store resolution changed; verify the integration first",
        ));
    }
    let store_metadata = fs::metadata(&target.store.database_path).map_err(|e| {
        ApiError::conflict(format!(
            "Established Engram store is unavailable (not created): {e}"
        ))
    })?;
    if !store_metadata.is_file() || store_metadata.len() == 0 {
        return Err(ApiError::conflict(
            "Established Engram store is missing or empty; it will not be initialized",
        ));
    }
    Ok(())
}

fn work_database_path(home: &FsPath, project_id: &str) -> PathBuf {
    home.join("projects")
        .join(format!("{:x}", Sha256::digest(project_id.as_bytes())))
        .join("engram.db")
}

// Bound simultaneous CLI reads. A waiting client can retry explicitly; no
// request grows an unbounded background process queue.
static WORK_READ_PERMITS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));

// Router-owned admission dependency, never supplied by HTTP query/body. Tests
// install private limiters while exercising the actual production handlers.
#[derive(Clone)]
struct WorkReadLimiter(Arc<tokio::sync::Semaphore>);

async fn acquire_work_read_permit_from(
    limiter: Arc<tokio::sync::Semaphore>,
) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
    // Abandoned reads still have bounded child lifetimes. Wait for that bound
    // before rejecting a replacement view; no process is spawned while waiting.
    tokio::time::timeout(
        ENGRAM_WORK_BINDING_COMMAND_TIMEOUT + Duration::from_secs(1),
        limiter.acquire_owned(),
    )
    .await
    .map_err(|_| {
        ApiError::from_status(
            StatusCode::TOO_MANY_REQUESTS,
            "Work reads busy; retry shortly",
        )
    })?
    .map_err(|_| ApiError::internal("Work read limiter closed"))
}

/// Metadata-only detection: a `.beads` directory plus a resolvable native
/// `bd` binary. Store health is learned from the read itself. A continuation
/// re-reads only the Engram page: the Beads snapshot and its status came with
/// the first page, so nothing is detected and the status says so.
fn beads_source_status(
    project: &Project,
    query: &WorkListQuery,
) -> (WorkSourceStatus, Option<BeadsReadTarget>) {
    if query.after.is_some() {
        return (
            work_source_status(
                "beads",
                "skipped",
                "Beads is read with the first page only; this continuation re-reads Engram",
            ),
            None,
        );
    }
    match beads_read_target(project) {
        Ok(Some(target)) => {
            let message = format!(
                "Beads reads use the native bd binary at {}",
                target.binary_path.display()
            );
            (work_source_status("beads", "ready", message), Some(target))
        }
        Ok(None) => (
            work_source_status("beads", "absent", "No .beads directory"),
            None,
        ),
        Err(message) => (work_source_status("beads", "unavailable", message), None),
    }
}

/// The one rule both sources share on a first page: a busy limiter, a stale
/// or missing store, or a failed read becomes that source's `error` status;
/// anything else (a closed limiter, an internal failure) fails the request.
fn is_work_source_condition(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::CONFLICT | StatusCode::TOO_MANY_REQUESTS | StatusCode::BAD_GATEWAY
    )
}

/// Set when the handler future that asked for a read is dropped (the browser
/// aborted the request): the client is gone, so the blocking worker admits
/// and launches nothing more on its behalf.
struct WorkReadAbandonGuard(Arc<std::sync::atomic::AtomicBool>);

impl Drop for WorkReadAbandonGuard {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// 499 (nginx's "client closed request"): nobody receives it, it only stops
/// the worker. It is not a source condition, so the whole read ends.
fn work_read_not_abandoned(cancelled: &std::sync::atomic::AtomicBool) -> Result<(), ApiError> {
    if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(ApiError::from_status(
            StatusCode::from_u16(499).expect("499 is a valid status code"),
            "Work read abandoned by the client",
        ));
    }
    Ok(())
}

async fn get_project_work(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
    limiter: Option<axum::Extension<WorkReadLimiter>>,
    beads_limiter: Option<axum::Extension<BeadsReadLimiter>>,
    beads_options: Option<axum::Extension<BeadsReadOptions>>,
    query: Result<Query<WorkListQuery>, QueryRejection>,
) -> Result<Json<WorkListResponse>, ApiError> {
    let Query(query) = query.map_err(|e| api_query_rejection("work list", e))?;
    let limiter = limiter.map_or_else(|| WORK_READ_PERMITS.clone(), |limiter| limiter.0.0);
    let beads_limiter =
        beads_limiter.map_or_else(|| BEADS_READ_PERMITS.clone(), |limiter| limiter.0.0);
    let beads_options = beads_options.map_or_else(BeadsReadOptions::default, |options| options.0);
    let runtime = tokio::runtime::Handle::current();
    let beads_runtime = runtime.clone();
    // An abandoned request (the browser aborted it) must not start reads or
    // hold permits for a caller who is gone: dropping this future sets the
    // flag the worker checks before each admission and each launch.
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _abandoned = WorkReadAbandonGuard(cancelled.clone());
    run_blocking_api(move || {
        // Detection and validation do not consume CLI capacity. Only the
        // blocking read paths wait on their async semaphores, off runtime
        // threads; each source has its own.
        state
            .list_project_work_with_options(
                &project_id,
                query,
                || {
                    work_read_not_abandoned(&cancelled)?;
                    runtime.block_on(acquire_work_read_permit_from(limiter))
                },
                || {
                    work_read_not_abandoned(&cancelled)?;
                    beads_runtime.block_on(acquire_beads_read_permit_from(
                        beads_limiter,
                        beads_options.timeout,
                    ))
                },
                beads_options,
                &cancelled,
            )
            .map(Json)
    })
    .await
}
