// Authority and compiled-worker observation for raw review identities.
// No caller chooses a process, command, environment or executable.
const TERMAL_REVIEW_IDENTITY_TOOL_NAME: &str = "termal_review_identity";
const TERMAL_REVIEW_IDENTITY_QUALIFIED_TOOL_NAME: &str =
    "mcp__termal-delegation__termal_review_identity";
const TERMAL_REVIEW_IDENTITY_TOOL_DESCRIPTION: &str = "Check a termal-review-identity-v1 candidate using the host-owned bounded raw inventory engine. Required check fields: operation=\"check\", schemaVersion=1, expectedFingerprint (the parent's independent lowercase SHA-256 literal), platformProfile (\"windows\" or \"unix\"). Optional pageLimit=1..100 and cursor. The host derives a read-only review child's cwd; capture and root overrides are forbidden to children. Root sessions may capture an admitted project worktree with operation=\"capture\" and optional root. Returns complete, verified, native platform coverage, counts and a fingerprint-bound inventory page. No shell, scripts, writes or fallback to Engram schema-1.";

#[derive(Clone, Debug, PartialEq, Eq)]
enum ReviewIdentityAuthority {
    Child((String, String, u32)),
    Parent {
        cwd: String,
        project: String,
        generation: u64,
        named: Vec<(String, u64)>,
    },
}
fn review_identity_authority_locked(
    inner: &StateInner,
    session: &str,
    request: &ReviewIdentityRequest,
) -> Result<ReviewIdentityAuthority, ApiError> {
    if AppState::engram_session_has_child_binding_shape_locked(inner, session) {
        if request.operation != "check" || request.root.is_some() {
            return Err(ApiError::conflict(
                "a reviewer may only check its admitted directory",
            ));
        }
        return review_freeze_authority_locked(inner, session).map(ReviewIdentityAuthority::Child);
    }
    if request.operation != "capture" {
        return Err(ApiError::conflict(
            "root sessions capture; reviewer children check",
        ));
    }
    let index = inner
        .find_session_index(session)
        .ok_or_else(|| ApiError::not_found("session not found"))?;
    let record = &inner.sessions[index];
    if record.hidden || !record.is_local_session() {
        return Err(ApiError::conflict("capture requires a local root session"));
    }
    let project = engram_project_for_session_locked(inner, session)
        .map(|p| p.root_path.clone())
        .unwrap_or_else(|| record.session.workdir.clone());
    Ok(ReviewIdentityAuthority::Parent {
        cwd: record.session.workdir.clone(),
        project,
        generation: record.active_turn_generation,
        named: inner
            .engram_work_source_roots
            .iter()
            .filter(|r| r.named_by_session == session)
            .map(|r| (r.root.clone(), r.generation))
            .collect(),
    })
}
impl AppState {
    fn review_identity_authority(
        &self,
        session: &str,
        request: &ReviewIdentityRequest,
    ) -> Result<ReviewIdentityAuthority, ApiError> {
        review_identity_authority_locked(
            &self.inner.lock().expect("state mutex poisoned"),
            session,
            request,
        )
    }
    fn observe_review_identity(
        &self,
        session: &str,
        request: ReviewIdentityRequest,
    ) -> Result<Value, ApiError> {
        self.observe_review_identity_with(session, request, |command| {
            run_bounded_read_command(
                command,
                std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT + REVIEW_FREEZE_OBSERVER_GRACE,
                128 * 1024,
                true,
            )
        })
    }
    fn observe_review_identity_with(
        &self,
        session: &str,
        request: ReviewIdentityRequest,
        run: impl FnOnce(&mut BoundedReadCommand) -> Result<std::process::Output>,
    ) -> Result<Value, ApiError> {
        request
            .validate()
            .map_err(|e| ApiError::bad_request(identity_error_code(&e)))?;
        let authority = self.review_identity_authority(session, &request)?;
        let root = match &authority {
            ReviewIdentityAuthority::Child((cwd, _, _)) => cwd.clone(),
            ReviewIdentityAuthority::Parent { cwd, .. } => {
                request.root.as_deref().unwrap_or(cwd).to_owned()
            }
        };
        if !FsPath::new(&root).is_absolute() {
            return Err(ApiError::bad_request("capture root must be absolute"));
        }
        let executable = std::env::current_exe().map_err(|e| ApiError::internal(e.to_string()))?;
        let body =
            serde_json::to_string(&request).map_err(|e| ApiError::internal(e.to_string()))?;
        let mut command = BoundedReadCommand::new(&executable);
        command.args(["review-identity", &root, &body]);
        if let ReviewIdentityAuthority::Parent {
            cwd,
            project,
            named,
            ..
        } = &authority
        {
            let scope = ReviewIdentityScope {
                cwd: cwd.clone(),
                project: project.clone(),
                named: named
                    .iter()
                    .filter(|(candidate, _)| candidate == &root)
                    .map(|(root, _)| root.clone())
                    .collect(),
            };
            command
                .arg(serde_json::to_string(&scope).map_err(|e| ApiError::internal(e.to_string()))?);
        }
        let output = run(&mut command)
            .map_err(|_| ApiError::internal("review identity worker did not complete"))?;
        if self.review_identity_authority(session, &request)? != authority {
            return Err(ApiError::conflict(
                "review identity authority changed during observation",
            ));
        }
        let mut value: Value = serde_json::from_slice(&output.stdout)
            .map_err(|_| ApiError::internal("review identity worker returned invalid JSON"))?;
        let complete = value["complete"] == true;
        if value["schemaVersion"] != 1
            || value["algorithm"] != REVIEW_IDENTITY_ALGORITHM
            || (complete
                && (!output.status.success()
                    || value["requestedRoot"].as_str() != Some(root.as_str())
                    || !value["canonicalRoot"]
                        .as_str()
                        .is_some_and(|root| FsPath::new(root).is_absolute())
                    || value["platformProfile"] != identity_platform()
                    || !matches!(value["objectFormat"].as_str(), Some("sha1" | "sha256"))
                    || !value["fingerprint"]
                        .as_str()
                        .is_some_and(is_lowercase_sha256)))
            || (!complete
                && (output.status.success()
                    || value["verified"] != false
                    || value.get("fingerprint").is_some_and(|f| !f.is_null())
                    || value["reason"]["code"].as_str().is_none()))
        {
            return Err(ApiError::internal("review identity worker contract failed"));
        }
        if complete
            && request.operation == "check"
            && (value["fingerprint"].as_str() != request.expected_fingerprint.as_deref()
                || value["verified"] != true)
        {
            return Err(ApiError::internal(
                "review identity worker did not check the expected fingerprint",
            ));
        }
        let code = value["reason"]["code"].as_str().unwrap_or("");
        if matches!(
            code,
            "concurrent_change" | "stale_cursor" | "root_not_admitted"
        ) || (request.cursor.is_some() && code == "fingerprint_mismatch")
        {
            return Err(ApiError::conflict(code));
        }
        // stdout is structured protocol, stderr is a separate bounded native
        // observation. Do not merge containment diagnostics into JSON/hash data.
        value["observer"] = json!({"status":output.status.code(),"stdoutLength":output.stdout.len(),
            "stderrLength":output.stderr.len(),"stderrBase64":base64::engine::general_purpose::STANDARD.encode(output.stderr)});
        Ok(value)
    }
}

// This separate fixed-worker argument is constructed solely from the locked
// session snapshot. It is never part of the HTTP/MCP caller's request schema.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReviewIdentityScope {
    cwd: String,
    project: String,
    named: Vec<String>,
}
impl ReviewIdentityScope {
    fn admit(&self, root: &FsPath) -> Result<PathBuf> {
        let canonical = identity_canonical(root)?;
        let cwd = identity_canonical(FsPath::new(&self.cwd))?;
        let project = identity_canonical(FsPath::new(&self.project))?;
        if !canonical.starts_with(&project) {
            return Err(identity_refusal("root_not_admitted"));
        }
        let common = identity_registered_common(&canonical)?;
        let named = self.named.iter().any(|root| FsPath::new(root) == canonical);
        let same_repository =
            identity_registered_common(&cwd).is_ok_and(|session| session == common);
        if canonical != cwd && canonical != project && !named && !same_repository {
            return Err(identity_refusal("root_not_admitted"));
        }
        Ok(common)
    }
}
async fn review_identity_api(
    AxumPath(session): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<ReviewIdentityRequest>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(request) = request.map_err(|e| api_json_rejection("review identity", e))?;
    let permit = REVIEW_FREEZE_PERMITS
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ApiError::from_status(StatusCode::TOO_MANY_REQUESTS, "review identity busy")
        })?;
    run_blocking_api(move || {
        let _permit = permit;
        state.observe_review_identity(&session, request).map(Json)
    })
    .await
}

fn review_identity_tool_definition() -> Value {
    json!({
        "name": TERMAL_REVIEW_IDENTITY_TOOL_NAME,
        "description": TERMAL_REVIEW_IDENTITY_TOOL_DESCRIPTION,
        "annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false},
        "inputSchema":{
            "type":"object","additionalProperties":false,
            "required":["operation","schemaVersion"],
            "properties":{
                "operation":{"type":"string","enum":["capture","check"]},
                "schemaVersion":{"type":"integer","const":1},
                "root":{"type":"string","maxLength":4096},
                "expectedFingerprint":{"type":"string","pattern":"^[a-f0-9]{64}$"},
                "platformProfile":{"type":"string","enum":["windows","unix"]},
                "pageLimit":{"type":"integer","minimum":1,"maximum":100},
                "cursor":{"type":"string","maxLength":8192}
            }
        }
    })
}

impl TermalDelegationMcpBridge {
    fn tool_review_identity(&self, arguments: Value) -> Result<Value> {
        let request: ReviewIdentityRequest = serde_json::from_value(arguments.clone())?;
        request.validate()?;
        if self.caller_is_delegation_child()
            && (request.operation != "check"
                || request.root.is_some()
                || !self.caller_allows_review_identity())
        {
            bail!("review identity requires a current read-only reviewer check");
        }
        self.post_long_call(
            DelegationLongCall::ReviewFreeze,
            &format!("/api/sessions/{}/review-identity", self.serving_session_id),
            &arguments,
        )
    }
}

fn review_identity_snapshot_allows(state: &Value, session: &str) -> bool {
    state
        .get("delegations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|d| {
            d.get("childSessionId").and_then(Value::as_str) == Some(session)
                && d.get("reviewIdentityAllowed").and_then(Value::as_bool) == Some(true)
        })
}

// Validate exactly one requested registration, with bounded metadata reads.
// Never enumerate a filesystem or accept an arbitrary .git pointer.
fn identity_registered_common(root: &FsPath) -> Result<PathBuf> {
    identity_no_links(root)?;
    let dotgit = root.join(".git");
    let metadata = identity_metadata(&dotgit)?;
    identity_plain_metadata(&metadata)?;
    if metadata.is_dir() {
        return identity_canonical(&dotgit);
    }
    let bytes = identity_read_file(&dotgit, 4096)?.0;
    let pointer = std::str::from_utf8(&bytes)?
        .trim()
        .strip_prefix("gitdir:")
        .ok_or_else(|| identity_refusal("git_metadata_escape"))?
        .trim();
    let own = root.join(pointer);
    identity_no_links(&own)?;
    let own = identity_canonical(&own)?;
    let bytes = identity_read_file(&own.join("commondir"), 4096)?.0;
    let common = own.join(std::str::from_utf8(&bytes)?.trim());
    identity_no_links(&common)?;
    let common = identity_canonical(&common)?;
    if own.parent().and_then(FsPath::file_name) != Some(std::ffi::OsStr::new("worktrees"))
        || own.parent().and_then(FsPath::parent) != Some(common.as_path())
    {
        return Err(identity_refusal("git_metadata_escape"));
    }
    let back = identity_read_file(&own.join("gitdir"), 4096)?.0;
    let back = own.join(std::str::from_utf8(&back)?.trim());
    identity_no_links(&back)?;
    if identity_canonical(&back)? != identity_canonical(&dotgit)? {
        return Err(identity_refusal("git_metadata_escape"));
    }
    Ok(common)
}
