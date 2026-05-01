// Git-related HTTP route handlers.
//
// Six Axum handlers for the session-workdir git workflow: read
// working-tree status, read per-file diffs, apply file-level actions
// (stage/unstage/discard), commit, push, sync (pull + fast-forward).
//
// Every route follows the same two-phase pattern:
//
// 1. Check if the session is remote-backed — if so, short-circuit to
//    the matching `proxy_remote_git_*` helper in `remote_routes.rs`
//    so the operation runs on the remote host where the checkout
//    actually lives.
// 2. Otherwise resolve the session's workdir and delegate to the
//    git backend in `src/git.rs` via `run_blocking_api` (shells out
//    to `git` — blocking, kept off the async runtime).
//
// The router wiring that binds URL paths to these handlers lives in
// `main.rs`; Axum's handler signatures (`State<AppState>`,
// `Query<FileQuery>`, `Json<...>`) are the only contract this file
// exposes.


/// Reads Git status.
async fn read_git_status(
    State(state): State<AppState>,
    Query(query): Query<FileQuery>,
) -> Result<Json<GitStatusResponse>, ApiError> {
    // Check remote scope first (brief mutex access).
    if let Some(scope) =
        state.remote_scope_for_request(query.session_id.as_deref(), query.project_id.as_deref())?
    {
        let response = run_blocking_api(move || {
            state.remote_get_json(
                &scope,
                "/api/git/status",
                vec![("path".to_owned(), query.path.clone())],
            )
        })
        .await?;
        return Ok(Json(response));
    }

    // Git status runs child processes -- use a dedicated spawn_blocking so it
    // doesn't compete with state-mutation tasks in run_blocking_api.
    let response = tokio::task::spawn_blocking(move || {
        let workdir = resolve_existing_requested_path(&query.path, "path")?;
        load_git_status_for_path(&workdir)
    })
    .await
    .map_err(|err| ApiError::internal(format!("git status task failed: {err}")))??;
    Ok(Json(response))
}

/// Reads Git diff.
async fn read_git_diff(
    State(state): State<AppState>,
    Json(request): Json<GitDiffRequest>,
) -> Result<Json<GitDiffResponse>, ApiError> {
    if let Some(scope) = state
        .remote_scope_for_request(request.session_id.as_deref(), request.project_id.as_deref())?
    {
        let response = run_blocking_api(move || {
            state.remote_post_json(
                &scope,
                "/api/git/diff",
                json!({
                    "originalPath": request.original_path,
                    "path": request.path,
                    "sectionId": request.section_id,
                    "statusCode": request.status_code,
                    "workdir": request.workdir,
                }),
            )
        })
        .await?;
        return Ok(Json(response));
    }

    let response = tokio::task::spawn_blocking(move || {
        let workdir = resolve_existing_requested_path(&request.workdir, "path")?;
        load_git_diff_for_request(&workdir, &request)
    })
    .await
    .map_err(|err| ApiError::internal(format!("git diff task failed: {err}")))??;
    Ok(Json(response))
}

/// Applies Git file action.
fn ensure_git_repo_root_write_allowed(
    state: &AppState,
    session_id: Option<&str>,
    project_id: Option<&str>,
    repo_root: &FsPath,
    action: &str,
) -> Result<(), ApiError> {
    let repo_root = repo_root.to_string_lossy();
    state.ensure_read_only_delegation_allows_write_action(
        session_id,
        project_id,
        Some(repo_root.as_ref()),
        action,
    )
}

fn ensure_git_path_write_allowed(
    state: &AppState,
    session_id: Option<&str>,
    project_id: Option<&str>,
    repo_root: &FsPath,
    relative_path: &FsPath,
    action: &str,
) -> Result<(), ApiError> {
    let target_path = repo_root.join(relative_path);
    let target_path = target_path.to_string_lossy();
    state.ensure_read_only_delegation_allows_write_action(
        session_id,
        project_id,
        Some(target_path.as_ref()),
        action,
    )
}

async fn apply_git_file_action(
    State(state): State<AppState>,
    Json(request): Json<GitFileActionRequest>,
) -> Result<Json<GitStatusResponse>, ApiError> {
    let response = run_blocking_api(move || {
        state.ensure_read_only_delegation_allows_session_write_action(
            request.session_id.as_deref(),
            "git file actions",
        )?;
        if let Some(scope) = state.remote_scope_for_request(
            request.session_id.as_deref(),
            request.project_id.as_deref(),
        )? {
            return state.remote_post_json(
                &scope,
                "/api/git/file",
                json!({
                    "action": request.action,
                    "originalPath": request.original_path,
                    "path": request.path,
                    "statusCode": request.status_code,
                    "workdir": request.workdir,
                }),
            );
        }

        state.ensure_read_only_delegation_allows_write_action(
            request.session_id.as_deref(),
            request.project_id.as_deref(),
            Some(&request.workdir),
            "git file actions",
        )?;

        let workdir = resolve_existing_requested_path(&request.workdir, "path")?;
        let workdir = normalize_git_workdir_path(&workdir)?;
        let Some(repo_root) = resolve_git_repo_root(&workdir)? else {
            return Err(ApiError::bad_request("no git repository found"));
        };
        ensure_git_repo_root_write_allowed(
            &state,
            request.session_id.as_deref(),
            request.project_id.as_deref(),
            &repo_root,
            "git file actions",
        )?;

        let current_path = normalize_git_repo_relative_path(&request.path)?;
        let original_path = request
            .original_path
            .as_deref()
            .map(normalize_git_repo_relative_path)
            .transpose()?;
        ensure_git_path_write_allowed(
            &state,
            request.session_id.as_deref(),
            request.project_id.as_deref(),
            &repo_root,
            FsPath::new(&current_path),
            "git file actions",
        )?;
        if let Some(original_path) = original_path.as_deref() {
            ensure_git_path_write_allowed(
                &state,
                request.session_id.as_deref(),
                request.project_id.as_deref(),
                &repo_root,
                FsPath::new(original_path),
                "git file actions",
            )?;
        }

        match request.action {
            GitFileAction::Stage => {
                let pathspecs = collect_git_pathspecs(&current_path, original_path.as_deref());
                run_git_pathspec_command(
                    &repo_root,
                    &["add", "-A"],
                    &pathspecs,
                    "failed to stage git changes",
                )?;
            }
            GitFileAction::Unstage => {
                let pathspecs = collect_git_pathspecs(&current_path, original_path.as_deref());
                run_git_pathspec_command(
                    &repo_root,
                    &["restore", "--staged"],
                    &pathspecs,
                    "failed to unstage git changes",
                )?;
            }
            GitFileAction::Revert => {
                revert_git_file_action(
                    &repo_root,
                    &current_path,
                    original_path.as_deref(),
                    request.status_code.as_deref(),
                )?;
            }
        }

        Ok(load_git_status_for_path(&workdir)?)
    })
    .await?;
    Ok(Json(response))
}

async fn commit_git_changes(
    State(state): State<AppState>,
    Json(request): Json<GitCommitRequest>,
) -> Result<Json<GitCommitResponse>, ApiError> {
    let response = run_blocking_api(move || {
        state.ensure_read_only_delegation_allows_session_write_action(
            request.session_id.as_deref(),
            "git commits",
        )?;
        if let Some(scope) = state.remote_scope_for_request(
            request.session_id.as_deref(),
            request.project_id.as_deref(),
        )? {
            return state.remote_post_json(
                &scope,
                "/api/git/commit",
                json!({
                    "message": request.message,
                    "workdir": request.workdir,
                }),
            );
        }

        state.ensure_read_only_delegation_allows_write_action(
            request.session_id.as_deref(),
            request.project_id.as_deref(),
            Some(&request.workdir),
            "git commits",
        )?;

        let workdir = resolve_existing_requested_path(&request.workdir, "path")?;
        let workdir = normalize_git_workdir_path(&workdir)?;
        let Some(repo_root) = resolve_git_repo_root(&workdir)? else {
            return Err(ApiError::bad_request("no git repository found"));
        };
        ensure_git_repo_root_write_allowed(
            &state,
            request.session_id.as_deref(),
            request.project_id.as_deref(),
            &repo_root,
            "git commits",
        )?;

        let message = request.message.trim();
        if message.is_empty() {
            return Err(ApiError::bad_request("commit message cannot be empty"));
        }

        let status_before = load_git_status_for_path(&workdir)?;
        if !has_staged_git_changes(&status_before) {
            return Err(ApiError::bad_request("no staged changes to commit"));
        }

        let output = git_command()
            .arg("-C")
            .arg(&repo_root)
            .args(["commit", "-m"])
            .arg(message)
            .output()
            .map_err(|err| ApiError::internal(format!("failed to create git commit: {err}")))?;

        if !output.status.success() {
            let detail = extract_git_command_error(&output);
            let message = if detail.is_empty() {
                "failed to create git commit".to_owned()
            } else {
                format!("failed to create git commit: {detail}")
            };
            return Err(ApiError::bad_request(message));
        }

        let status = load_git_status_for_path(&workdir)?;
        Ok(GitCommitResponse {
            status,
            summary: build_git_commit_summary(message),
        })
    })
    .await?;
    Ok(Json(response))
}

/// Pushes Git changes.
async fn push_git_changes(
    State(state): State<AppState>,
    Json(request): Json<GitRepoActionRequest>,
) -> Result<Json<GitRepoActionResponse>, ApiError> {
    let response = run_blocking_api(move || {
        state.ensure_read_only_delegation_allows_session_write_action(
            request.session_id.as_deref(),
            "git pushes",
        )?;
        if let Some(scope) = state.remote_scope_for_request(
            request.session_id.as_deref(),
            request.project_id.as_deref(),
        )? {
            return state.remote_post_json(
                &scope,
                "/api/git/push",
                json!({
                    "workdir": request.workdir,
                }),
            );
        }

        state.ensure_read_only_delegation_allows_write_action(
            request.session_id.as_deref(),
            request.project_id.as_deref(),
            Some(&request.workdir),
            "git pushes",
        )?;

        let workdir = resolve_existing_requested_path(&request.workdir, "path")?;
        if let Some(repo_root) = resolve_git_repo_root(&workdir)? {
            ensure_git_repo_root_write_allowed(
                &state,
                request.session_id.as_deref(),
                request.project_id.as_deref(),
                &repo_root,
                "git pushes",
            )?;
        }
        push_git_repo(&workdir)
    })
    .await?;
    Ok(Json(response))
}

/// Syncs Git changes.
async fn sync_git_changes(
    State(state): State<AppState>,
    Json(request): Json<GitRepoActionRequest>,
) -> Result<Json<GitRepoActionResponse>, ApiError> {
    let response = run_blocking_api(move || {
        state.ensure_read_only_delegation_allows_session_write_action(
            request.session_id.as_deref(),
            "git sync",
        )?;
        if let Some(scope) = state.remote_scope_for_request(
            request.session_id.as_deref(),
            request.project_id.as_deref(),
        )? {
            return state.remote_post_json(
                &scope,
                "/api/git/sync",
                json!({
                    "workdir": request.workdir,
                }),
            );
        }

        state.ensure_read_only_delegation_allows_write_action(
            request.session_id.as_deref(),
            request.project_id.as_deref(),
            Some(&request.workdir),
            "git sync",
        )?;

        let workdir = resolve_existing_requested_path(&request.workdir, "path")?;
        if let Some(repo_root) = resolve_git_repo_root(&workdir)? {
            ensure_git_repo_root_write_allowed(
                &state,
                request.session_id.as_deref(),
                request.project_id.as_deref(),
                &repo_root,
                "git sync",
            )?;
        }
        sync_git_repo(&workdir)
    })
    .await?;
    Ok(Json(response))
}
