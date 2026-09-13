// New project Work read boundary. Owns metadata-only discovery and selection of
// an already installed host binding. Never owns integration enablement, binding
// creation, tracker writes, SQLite access, or agent-turn scheduling.

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkReadTarget {
    connection: EngramConnectionConfig,
    store: EngramAuthorityStoreKey,
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
    fn work_read_snapshot(
        &self,
        project_id: &str,
        reader: Option<&str>,
    ) -> Result<(Project, Option<WorkReadTarget>), ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let project = inner
            .find_project(project_id)
            .ok_or_else(|| ApiError::not_found("Work project not found"))?
            .clone();
        let mut candidates = inner
            .sessions
            .iter()
            .filter_map(|record| {
                if reader.is_some_and(|id| record.session.id != id)
                    || record.session.project_id.as_deref() != Some(project_id)
                    || !record.is_local_session()
                    || record.engram_mcp_revocation_pending
                {
                    return None;
                }
                let installed = record.engram_mcp_installed.as_ref()?;
                let config =
                    engram_mcp_runtime_config_for_session_locked(&inner, &record.session.id)?;
                if installed != &config.installed {
                    return None;
                }
                let store = installed.store_key.clone()?;
                Some(WorkReadTarget {
                    connection: EngramConnectionConfig {
                        binary_path: PathBuf::from(&installed.binary_path),
                        project_root: PathBuf::from(&project.root_path),
                        project_file: PathBuf::from(&project.root_path).join(".engram-project"),
                        home: PathBuf::from(&installed.home),
                        actor_id: installed.actor_id.clone(),
                        actor_context: installed.actor_context.clone(),
                        session_id: record.session.id.clone(),
                    },
                    store,
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|a, b| a.connection.session_id.cmp(&b.connection.session_id));
        Ok((project, candidates.into_iter().next()))
    }

    #[cfg(test)]
    fn list_project_work(
        &self,
        project_id: &str,
        query: WorkListQuery,
    ) -> Result<WorkListResponse, ApiError> {
        self.list_project_work_with_admission(project_id, query, || Ok(()))
    }

    fn list_project_work_with_admission<P>(
        &self,
        project_id: &str,
        query: WorkListQuery,
        admit: impl FnOnce() -> Result<P, ApiError>,
    ) -> Result<WorkListResponse, ApiError> {
        query.validate()?;
        let (project, target) =
            self.work_read_snapshot(project_id, query.reader_session_id.as_deref())?;
        let mut sources = Vec::new();
        if project.remote_id != LOCAL_REMOTE_ID {
            sources.push(work_source_status(
                "engram",
                "unavailable",
                "Remote project Work reads are not supported yet",
            ));
        } else {
            let root = FsPath::new(&project.root_path);
            sources.push(match fs::metadata(root.join(".beads")) {
                Ok(metadata) if metadata.is_dir() => work_source_status("beads", "notSupported", "Beads directory detected; adapter is planned for phase 2 (store health not checked)"),
                Ok(_) => work_source_status("beads", "unavailable", ".beads is not a directory"),
                Err(e) if e.kind() == io::ErrorKind::NotFound => work_source_status("beads", "absent", "No .beads directory"),
                Err(e) => work_source_status("beads", "unavailable", format!("Cannot inspect .beads: {e}")),
            });
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
                    None => work_source_status(
                        "engram",
                        "unavailable",
                        "No established matching host session binding/store; this view does not create one",
                    ),
                    Some(target) => match validate_work_read_target(target) {
                        Ok(()) => work_source_status(
                            "engram",
                            "ready",
                            "Engram reads use an established host binding",
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
        let (page, reader_session_id) = if ready {
            let target =
                target.ok_or_else(|| ApiError::conflict("Work reader changed; refresh"))?;
            let _permit = admit()?;
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
            (Some(page), Some(target.connection.session_id))
        } else {
            (None, None)
        };
        Ok(WorkListResponse {
            sources,
            reader_session_id,
            page,
            observed_at: chrono::Utc::now().to_rfc3339(),
        })
    }

    fn validate_work_read_still_current(
        &self,
        project_id: &str,
        target: &WorkReadTarget,
    ) -> Result<(), ApiError> {
        let (_, current) =
            self.work_read_snapshot(project_id, Some(&target.connection.session_id))?;
        if current.as_ref() != Some(target) {
            return Err(ApiError::conflict(
                "Work binding changed during read; discard pages and refresh",
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

async fn get_project_work(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
    limiter: Option<axum::Extension<WorkReadLimiter>>,
    query: Result<Query<WorkListQuery>, QueryRejection>,
) -> Result<Json<WorkListResponse>, ApiError> {
    let Query(query) = query.map_err(|e| api_query_rejection("work list", e))?;
    let limiter = limiter.map_or_else(|| WORK_READ_PERMITS.clone(), |limiter| limiter.0.0);
    let runtime = tokio::runtime::Handle::current();
    run_blocking_api(move || {
        // Detection and validation do not consume CLI capacity. Only the
        // blocking read path waits on the async semaphore, off runtime threads.
        state
            .list_project_work_with_admission(&project_id, query, || {
                runtime.block_on(acquire_work_read_permit_from(limiter))
            })
            .map(Json)
    })
    .await
}
