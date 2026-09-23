// New reviewer-only host control-plane entry point. Owns current-child
// authorization and a real subprocess observer, not shell/interpreter approval.

const TERMAL_REVIEW_FREEZE_TOOL_NAME: &str = "termal_review_freeze_check";
const TERMAL_REVIEW_FREEZE_QUALIFIED_TOOL_NAME: &str =
    "mcp__termal-delegation__termal_review_freeze_check";
const TERMAL_REVIEW_FREEZE_TOOL_DESCRIPTION: &str = "Independently verify an Engram schema-1 frozen review using TermAl's compiled checker, not repository scripts. Supply the manifest path and the parent's separately recorded SHA-256 literal. The host derives the current review worktree; observes real checker exit status, stdout and stderr separately; and requires exact fingerprint plus LF. Call before and after inspection. Does not allow Node, shell scripts, writes, builds, or tracker access. A mismatch or unsupported input is a failed verification, never a clean review.";
static REVIEW_FREEZE_PERMITS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));

#[derive(Clone)]
struct ReviewFreezeLimiter(Arc<tokio::sync::Semaphore>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewFreezeResponse {
    schema_version: u32,
    algorithm: &'static str,
    root: String,
    expected_fingerprint: String,
    verified: bool,
    observer: ReviewFreezeObservation,
}

// One locked authority boundary for both permission admission and execution.
fn review_freeze_authority_locked(
    inner: &StateInner,
    child: &str,
) -> Result<(String, String, u32), ApiError> {
    let index = inner
        .find_delegation_index_by_child_session_id(child)
        .ok_or_else(|| {
            ApiError::conflict("freeze verification requires an active reviewer child")
        })?;
    let delegation = &inner.delegations[index];
    let record = inner
        .find_session_index(child)
        .map(|i| &inner.sessions[i])
        .ok_or_else(|| ApiError::not_found("review child no longer exists"))?;
    if delegation.mode != DelegationMode::Reviewer
        || delegation.status != DelegationStatus::Running
        || !matches!(delegation.write_policy, DelegationWritePolicy::ReadOnly)
        || record.hidden
        || !record.is_local_session()
        || record.session.parent_delegation_id.as_deref() != Some(delegation.id.as_str())
    {
        return Err(ApiError::conflict(
            "freeze verification requires an active local read-only reviewer",
        ));
    }
    if record.session.workdir != delegation.cwd {
        return Err(ApiError::conflict(
            "review child directory differs from the admitted review directory",
        ));
    }
    Ok((
        record.session.workdir.clone(),
        delegation.id.clone(),
        delegation.review_result_submission_attempt,
    ))
}

impl AppState {
    fn review_freeze_child_identity(&self, child: &str) -> Result<(String, String, u32), ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        review_freeze_authority_locked(&inner, child)
    }

    fn verify_review_freeze(
        &self,
        child: &str,
        request: ReviewFreezeRequest,
    ) -> Result<ReviewFreezeResponse, ApiError> {
        self.verify_review_freeze_with_runner(child, request, run_review_freeze_checker)
    }

    // Internal executor boundary permits deterministic transport/identity
    // integration tests. It is not a caller-controlled executable or API field.
    fn verify_review_freeze_with_runner(
        &self,
        child: &str,
        request: ReviewFreezeRequest,
        run: impl FnOnce(&mut Command) -> Result<std::process::Output>,
    ) -> Result<ReviewFreezeResponse, ApiError> {
        request
            .validate()
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        let identity = self.review_freeze_child_identity(child)?;
        let mut command =
            Command::new(std::env::current_exe().map_err(|e| ApiError::internal(e.to_string()))?);
        command.args([
            "review-freeze-check",
            &identity.0,
            &request.manifest_path,
            &request.expected_fingerprint,
        ]);
        // No executable or cwd may be supplied by the tool caller. The worker
        // never starts server mode or opens TermAl's persistence stores.
        let output = run(&mut command)
            .map_err(|e| ApiError::internal(format!("review checker did not complete: {e:#}")))?;
        if self.review_freeze_child_identity(child)? != identity {
            return Err(ApiError::conflict(
                "review attempt changed during verification",
            ));
        }
        let observer = review_freeze_observation(output, &request.expected_fingerprint);
        Ok(ReviewFreezeResponse {
            schema_version: 1,
            algorithm: "engram-review-freeze-v1",
            root: identity.0,
            expected_fingerprint: request.expected_fingerprint,
            verified: observer.stdout_exact,
            observer,
        })
    }
}

fn run_review_freeze_checker(command: &mut Command) -> Result<std::process::Output> {
    run_bounded_read_process(
        command,
        std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT + Duration::from_secs(5),
        4096,
        true,
    )
}

async fn verify_delegation_review_freeze(
    AxumPath(child): AxumPath<String>,
    State(state): State<AppState>,
    limiter: Option<axum::Extension<ReviewFreezeLimiter>>,
    request: Result<Json<ReviewFreezeRequest>, JsonRejection>,
) -> Result<Json<ReviewFreezeResponse>, ApiError> {
    run_review_freeze_request(
        child,
        state,
        request,
        limiter.map_or_else(|| REVIEW_FREEZE_PERMITS.clone(), |limiter| limiter.0.0),
        |state, child, request| state.verify_review_freeze(child, request),
    )
    .await
}

async fn run_review_freeze_request<V>(
    child: String,
    state: AppState,
    request: Result<Json<ReviewFreezeRequest>, JsonRejection>,
    permits: Arc<tokio::sync::Semaphore>,
    verify: V,
) -> Result<Json<ReviewFreezeResponse>, ApiError>
where
    V: FnOnce(AppState, &str, ReviewFreezeRequest) -> Result<ReviewFreezeResponse, ApiError>
        + Send
        + 'static,
{
    let Json(request) = request.map_err(|e| api_json_rejection("review freeze", e))?;
    let permit = permits.try_acquire_owned().map_err(|_| {
        ApiError::from_status(
            StatusCode::TOO_MANY_REQUESTS,
            "review verification busy; retry the same read",
        )
    })?;
    // The owned permit stays with the blocking operation, even if HTTP drops.
    run_blocking_api(move || {
        let _permit = permit;
        verify(state, &child, request).map(Json)
    })
    .await
}
