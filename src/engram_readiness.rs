// Scoped project readiness and explicit Full Audit, separated from session_crud
// and engram_host_adapter. Owns receipt admission and diagnostic routes, not
// settings persistence, runtime revocation, or Engram store repair.

const ENGRAM_READINESS_TIMEOUT: Duration = Duration::from_secs(10);
const ENGRAM_DIAGNOSTIC_DECLARATION_LIMIT: usize = 4096;
const ENGRAM_DIAGNOSTIC_TEXT_LIMIT: usize = 4096;
const ENGRAM_DIAGNOSTIC_REPORT_LIMIT: usize = 16 * 1024;

// Transport capture has a larger budget. Only bounded text previews cross the
// API boundary; these are not redacted, and omitted output is not logged.
fn engram_diagnostic_preview(bytes: &[u8], limit: usize) -> (String, bool) {
    let mut end = bytes.len().min(limit);
    if let Err(error) = std::str::from_utf8(&bytes[..end])
        && error.error_len().is_none()
    {
        end = error.valid_up_to();
    }
    let mut text = String::from_utf8_lossy(&bytes[..end]).into_owned();
    let truncated = end < bytes.len() || text.len() > limit;
    if text.len() > limit {
        let mut boundary = limit;
        while !text.is_char_boundary(boundary) {
            boundary -= 1;
        }
        text.truncate(boundary);
    }
    (text, truncated)
}

fn engram_diagnostic_excerpt(bytes: &[u8]) -> String {
    const SUFFIX: &str = "\n[truncated]";
    let (mut text, truncated) =
        engram_diagnostic_preview(bytes, ENGRAM_DIAGNOSTIC_TEXT_LIMIT - SUFFIX.len());
    if truncated {
        text.push_str(SUFFIX);
    }
    text
}

fn read_engram_diagnostic_declaration(marker: &FsPath) -> Result<String, ApiError> {
    let read_error =
        |error| ApiError::bad_request(format!("Cannot read Engram declaration: {error}"));
    let validate_metadata = |metadata: fs::Metadata| {
        if !metadata.is_file()
            || metadata.len() == 0
            || metadata.len() > ENGRAM_DIAGNOSTIC_DECLARATION_LIMIT as u64
        {
            return Err(ApiError::bad_request(
                "Engram declaration must be a non-empty regular file of at most 4096 bytes",
            ));
        }
        Ok(())
    };
    // Reject special files before opening, then check the opened file as well.
    validate_metadata(fs::metadata(marker).map_err(read_error)?)?;
    let file = fs::File::open(marker).map_err(read_error)?;
    validate_metadata(file.metadata().map_err(read_error)?)?;
    read_engram_diagnostic_declaration_contents(file)
}

fn read_engram_diagnostic_declaration_contents(reader: impl io::Read) -> Result<String, ApiError> {
    // Metadata is only a preflight: the file may grow before or during the read.
    // One extra byte distinguishes the maximum valid size from oversized input.
    let mut declaration = String::new();
    io::Read::read_to_string(
        &mut io::Read::take(reader, (ENGRAM_DIAGNOSTIC_DECLARATION_LIMIT + 1) as u64),
        &mut declaration,
    )
    .map_err(|error| ApiError::bad_request(format!("Cannot read Engram declaration: {error}")))?;
    if declaration.len() > ENGRAM_DIAGNOSTIC_DECLARATION_LIMIT || declaration.trim().is_empty() {
        return Err(ApiError::bad_request(
            "Engram declaration must be non-empty and at most 4096 bytes",
        ));
    }
    Ok(declaration)
}

#[derive(Deserialize)]
struct EngramReadinessReceipt {
    schema_version: u32,
    scope: String,
    ready: bool,
    full_audit: String,
    mutation_enabled: bool,
    project_id: String,
    database: PathBuf,
    work_schema_version: u64,
    host_path_policy: EngramReadinessPathPolicy,
    control: EngramDoctorControl,
}

#[derive(Deserialize)]
struct EngramReadinessPathPolicy {
    stored: Option<String>,
    resolved: Option<String>,
    status: String,
}

fn run_engram_readiness(
    binary: &FsPath,
    marker: &FsPath,
    home: &FsPath,
    root: &FsPath,
) -> Result<EngramReadinessReceipt, ApiError> {
    let declaration = read_engram_diagnostic_declaration(marker)?;
    let output = run_engram_diagnostic_within(
        binary,
        marker,
        home,
        root,
        "readiness",
        ENGRAM_READINESS_TIMEOUT,
    )?;
    if read_engram_diagnostic_declaration(marker).ok().as_ref() != Some(&declaration) {
        return Err(ApiError::conflict(
            "Engram declaration changed during readiness; verify again",
        ));
    }
    if !output.status.success() {
        return Err(ApiError::bad_request(format!(
            "Engram readiness unavailable or refused ({}); no Full Audit fallback: {} {}",
            output.status,
            engram_diagnostic_excerpt(&output.stdout),
            engram_diagnostic_excerpt(&output.stderr),
        )));
    }
    if output.stdout.len() > ENGRAM_DIAGNOSTIC_REPORT_LIMIT {
        return Err(ApiError::bad_request(
            "Engram readiness receipt exceeds the 16384-byte admission limit; no Full Audit fallback",
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| ApiError::bad_request(format!(
        "Engram readiness returned an unsupported or invalid receipt: {}; update the binary, no Full Audit fallback",
        engram_diagnostic_excerpt(error.to_string().as_bytes()),
    )))
}

// Re-read routing after the process returns; the command's assertion alone is
// not authority to bind a different project or a different selected store.
fn validate_engram_diagnostic_identity(
    project_id: &str,
    database: &FsPath,
    marker: &FsPath,
    home: &FsPath,
) -> Result<EngramAuthorityStoreKey, ApiError> {
    let declaration = read_engram_diagnostic_declaration(marker)?;
    if project_id.trim().is_empty() || declaration.trim() != project_id {
        return Err(ApiError::bad_request(
            "Engram diagnostic project identity does not match the declaration",
        ));
    }
    if !database.is_absolute() {
        return Err(ApiError::bad_request(
            "Engram diagnostic database must be absolute",
        ));
    }
    let expected = fs::canonicalize(work_database_path(home, project_id)).map_err(|error| {
        ApiError::bad_request(format!("Expected Engram database is unavailable: {error}"))
    })?;
    // Engram readiness and doctor already emit fs::canonicalize(database), with
    // Windows verbatim prefixes stripped (producer canonical_database_path).
    // Preserve that receipt contract rather than admitting arbitrary aliases.
    if normalize_user_facing_path(database) != normalize_user_facing_path(&expected) {
        return Err(ApiError::bad_request(
            "Engram diagnostic database does not match the selected project/home",
        ));
    }
    Ok(EngramAuthorityStoreKey {
        project_id: project_id.to_owned(),
        database_path: normalize_user_facing_path(&expected),
    })
}

fn validate_engram_readiness(
    receipt: &EngramReadinessReceipt,
    marker: &FsPath,
    home: &FsPath,
    turn_gated: bool,
) -> Result<EngramAuthorityStoreKey, ApiError> {
    if receipt.schema_version != 1
        || receipt.scope != "readiness"
        || !receipt.ready
        || receipt.full_audit != "not_run"
        || receipt.mutation_enabled
        || receipt.work_schema_version == 0
    {
        return Err(ApiError::bad_request(
            "Engram readiness is not an admitted read-only v1 receipt",
        ));
    }
    let path = &receipt.host_path_policy;
    if path.status != "matched"
        || path.stored.as_ref().is_none_or(|s| s.is_empty())
        || path.stored != path.resolved
    {
        return Err(ApiError::bad_request(format!(
            "Engram host path policy is {}; resolved, matched identity is required (readiness does not bind or repair it)",
            engram_diagnostic_excerpt(path.status.as_bytes()),
        )));
    }
    let required = receipt.control.required_assurance.as_str();
    if (turn_gated && required != "turn_gated") || !matches!(required, "advisory" | "turn_gated") {
        return Err(ApiError::bad_request(format!(
            "cannot enable Engram: readiness requires `{}`, incompatible with the selected TermAl tier",
            engram_diagnostic_excerpt(required.as_bytes()),
        )));
    }
    validate_engram_diagnostic_identity(&receipt.project_id, &receipt.database, marker, home)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramFullAuditResponse {
    healthy: bool,
    project_id: String,
    database: String,
    checked_at: String,
    elapsed_ms: u64,
    warnings: String,
    // A possibly incomplete JSON text preview, never an authoritative receipt.
    report_preview: String,
    report_truncated: bool,
}

impl AppState {
    fn validate_engram_diagnostic_snapshot(
        &self,
        project: &Project,
        host: &EngramHostSettings,
    ) -> Result<(), ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let current = inner
            .find_project(&project.id)
            .ok_or_else(|| ApiError::not_found("project not found"))?;
        if current.root_path != project.root_path
            || current.remote_id != project.remote_id
            || current.engram != project.engram
            || inner.preferences.engram != *host
            || inner.engram_project_resets.contains(&project.id)
        {
            return Err(ApiError::conflict(
                "Engram settings changed during the diagnostic; run it again",
            ));
        }
        Ok(())
    }

    fn full_audit_project_engram(
        &self,
        project_id: &str,
    ) -> Result<EngramFullAuditResponse, ApiError> {
        let (project, host) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            (
                inner
                    .find_project(project_id)
                    .cloned()
                    .ok_or_else(|| ApiError::not_found("project not found"))?,
                inner.preferences.engram.clone(),
            )
        };
        if project.remote_id != LOCAL_REMOTE_ID {
            return Err(ApiError::bad_request(
                "Full Audit is available only for local projects",
            ));
        }
        self.validate_engram_diagnostic_snapshot(&project, &host)?;
        let settings = EngramProjectSettings {
            binary_path: Some(host.binary_path.clone()),
            home: Some(host.home.clone()),
            ..Default::default()
        };
        let (binary, marker, home) = validate_engram_project_connection_paths(&project, &settings)?;
        let declaration = read_engram_diagnostic_declaration(&marker)?;
        let expected_path = fs::canonicalize(work_database_path(&home, declaration.trim()))
            .map_err(|error| {
                ApiError::bad_request(format!(
                    "Full Audit requires an existing store (not initialized): {error}"
                ))
            })?;
        let expected = validate_engram_diagnostic_identity(
            declaration.trim(),
            &expected_path,
            &marker,
            &home,
        )?;
        let started = std::time::Instant::now();
        let output = run_engram_diagnostic_within(
            &binary,
            &marker,
            &home,
            FsPath::new(&project.root_path),
            "doctor",
            ENGRAM_ENABLEMENT_DOCTOR_TIMEOUT,
        )?;
        let report: serde_json::Value =
            serde_json::from_slice(&output.stdout).map_err(|error| {
                ApiError::bad_request(format!(
                    "Full Audit returned invalid JSON: {}; {}",
                    engram_diagnostic_excerpt(error.to_string().as_bytes()),
                    engram_diagnostic_excerpt(&output.stderr)
                ))
            })?;
        let reported_id = report
            .get("project_id")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::bad_request("Full Audit did not return a project identity"))?;
        let database = report
            .get("database")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ApiError::bad_request("Full Audit did not return a database identity")
            })?;
        let healthy = report
            .get("healthy")
            .and_then(Value::as_bool)
            .ok_or_else(|| ApiError::bad_request("Full Audit did not return a health result"))?
            && output.status.success();
        let identity = validate_engram_diagnostic_identity(
            reported_id,
            FsPath::new(database),
            &marker,
            &home,
        )?;
        if identity != expected {
            return Err(ApiError::conflict(
                "Engram store identity changed during Full Audit",
            ));
        }
        self.validate_engram_diagnostic_snapshot(&project, &host)?;
        let (report_preview, report_truncated) =
            engram_diagnostic_preview(&output.stdout, ENGRAM_DIAGNOSTIC_REPORT_LIMIT);
        Ok(EngramFullAuditResponse {
            healthy,
            project_id: identity.project_id,
            database: identity.database_path.to_string_lossy().into_owned(),
            checked_at: chrono::Utc::now().to_rfc3339(),
            elapsed_ms: started.elapsed().as_millis() as u64,
            warnings: engram_diagnostic_excerpt(&output.stderr).trim().to_owned(),
            report_preview,
            report_truncated,
        })
    }
}

// Long audits have their own nonwaiting slot, never consuming readiness slots.
#[derive(Clone)]
struct EngramAuditLimiter(Arc<tokio::sync::Semaphore>);

#[derive(Clone)]
struct EngramReadinessLimiter(Arc<tokio::sync::Semaphore>);

fn acquire_engram_readiness(
    limiter: EngramReadinessLimiter,
) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
    limiter.0.try_acquire_owned().map_err(|_| {
        ApiError::from_status(
            StatusCode::TOO_MANY_REQUESTS,
            "Engram readiness checks busy; retry after they finish",
        )
    })
}

async fn full_audit_project_engram(
    AxumPath(project_id): AxumPath<String>,
    State(state): State<AppState>,
    axum::Extension(limiter): axum::Extension<EngramAuditLimiter>,
    headers: axum::http::HeaderMap,
) -> Result<Json<EngramFullAuditResponse>, ApiError> {
    // Explicit browser intent, not authentication against privileged local code.
    if headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) != Some("same-origin")
        || headers
            .get("x-termal-operator-action")
            .and_then(|v| v.to_str().ok())
            != Some("engram-full-audit")
    {
        return Err(ApiError::from_status(
            StatusCode::FORBIDDEN,
            "Full Audit requires an explicit same-origin operator action",
        ));
    }
    let permit = limiter.0.try_acquire_owned().map_err(|_| {
        ApiError::from_status(
            StatusCode::TOO_MANY_REQUESTS,
            "Full Audit is already running; retry after it finishes",
        )
    })?;
    let result = run_blocking_api(move || {
        let _permit = permit;
        state.full_audit_project_engram(&project_id)
    })
    .await?;
    Ok(Json(result))
}
