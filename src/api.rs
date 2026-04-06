/*
HTTP handler layer
Request flow:
axum route
  -> parse path/query/json
  -> run_blocking_api(...)
  -> AppState method
  -> optional runtime or remote dispatch
  -> JSON response or SSE payload
This file stays intentionally thin. Transport details live here, durable state
changes live in state.rs, runtime process logic lives in runtime.rs, and the
turn normalization layer lives in turns.rs.
*/

/// Resolves persistence path.
fn resolve_persistence_path(default_workdir: &str) -> PathBuf {
    resolve_termal_data_dir(default_workdir).join("sessions.json")
}

/// Loads state.
fn load_state(path: &FsPath) -> Result<Option<StateInner>> {
    if !path.exists() {
        return Ok(None);
    }

    let raw = fs::read(path).with_context(|| format!("failed to read `{}`", path.display()))?;
    let encoded: Value = serde_json::from_slice(&raw)
        .with_context(|| format!("failed to parse `{}`", path.display()))?;
    let persisted: PersistedState = serde_json::from_value(encoded)
        .with_context(|| format!("failed to deserialize state from `{}`", path.display()))?;
    Ok(Some(persisted.into_inner().with_context(|| {
        format!("failed to validate state from `{}`", path.display())
    })?))
}

/// Persists state.
fn persist_state(path: &FsPath, inner: &StateInner) -> Result<()> {
    let persisted = PersistedState::from_inner(inner);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    let encoded =
        serde_json::to_vec_pretty(&persisted).context("failed to serialize persisted state")?;
    fs::write(path, encoded).with_context(|| format!("failed to write `{}`", path.display()))
}

/// Delivers turn dispatch.
fn deliver_turn_dispatch(state: &AppState, dispatch: TurnDispatch) -> Result<(), ApiError> {
    match dispatch {
        TurnDispatch::PersistentClaude {
            command,
            sender,
            session_id,
        } => {
            if let Err(err) = sender.send(ClaudeRuntimeCommand::Prompt(command)) {
                let _ = state.clear_runtime(&session_id);
                let _ = state.fail_turn(
                    &session_id,
                    &format!("failed to queue prompt for Claude session: {err}"),
                );
                return Err(ApiError::internal(
                    "failed to queue prompt for Claude session",
                ));
            }
        }
        TurnDispatch::PersistentCodex {
            command,
            sender,
            session_id,
        } => {
            if let Err(err) = sender.send(CodexRuntimeCommand::Prompt {
                session_id: session_id.clone(),
                command,
            }) {
                let _ = state.clear_runtime(&session_id);
                let _ = state.fail_turn(
                    &session_id,
                    &format!("failed to queue prompt for Codex session: {err}"),
                );
                return Err(ApiError::internal(
                    "failed to queue prompt for Codex session",
                ));
            }
        }
        TurnDispatch::PersistentAcp {
            command,
            sender,
            session_id,
        } => {
            if let Err(err) = sender.send(AcpRuntimeCommand::Prompt(command)) {
                let _ = state.clear_runtime(&session_id);
                let _ = state.fail_turn(
                    &session_id,
                    &format!("failed to queue prompt for ACP session: {err}"),
                );
                return Err(ApiError::internal(
                    "failed to queue prompt for agent session",
                ));
            }
        }
    }

    Ok(())
}

/// Returns the backend health response.
async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        supports_inline_orchestrator_templates: true,
    })
}

/// Gets state.
async fn get_state(State(state): State<AppState>) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || Ok(state.snapshot())).await?;
    Ok(Json(response))
}

/// Lists workspace layouts.
async fn list_workspace_layouts(
    State(state): State<AppState>,
) -> Result<Json<WorkspaceLayoutsResponse>, ApiError> {
    let response = run_blocking_api(move || state.list_workspace_layouts()).await?;
    Ok(Json(response))
}

/// Gets workspace layout.
async fn get_workspace_layout(
    State(state): State<AppState>,
    AxumPath(workspace_id): AxumPath<String>,
) -> Result<Json<WorkspaceLayoutResponse>, ApiError> {
    let response = run_blocking_api(move || state.get_workspace_layout(&workspace_id)).await?;
    Ok(Json(response))
}

/// Stores a workspace layout.
///
/// Intentionally returns the saved document, while DELETE on the same route
/// returns the remaining summaries. Save callers may need the full persisted
/// document; delete callers only need the switcher list.
async fn put_workspace_layout(
    State(state): State<AppState>,
    AxumPath(workspace_id): AxumPath<String>,
    Json(request): Json<PutWorkspaceLayoutRequest>,
) -> Result<Json<WorkspaceLayoutResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.put_workspace_layout(&workspace_id, request)).await?;
    Ok(Json(response))
}

/// Deletes a workspace layout.
///
/// Intentionally returns the remaining workspace summaries, while PUT on the
/// same route returns the single saved document. See put_workspace_layout for
/// the rationale behind the asymmetric response shapes.
async fn delete_workspace_layout(
    State(state): State<AppState>,
    AxumPath(workspace_id): AxumPath<String>,
) -> Result<Json<WorkspaceLayoutsResponse>, ApiError> {
    let response = run_blocking_api(move || state.delete_workspace_layout(&workspace_id)).await?;
    Ok(Json(response))
}

impl AppState {
    /// Handles project digest.
    fn project_digest(&self, project_id: &str) -> Result<ProjectDigestResponse, ApiError> {
        Ok(self
            .build_project_digest_summary(project_id)?
            .into_response())
    }

    /// Handles execute project action.
    fn execute_project_action(
        &self,
        project_id: &str,
        action_id: &str,
    ) -> Result<ProjectDigestResponse, ApiError> {
        let action = ProjectActionId::parse(action_id)?;
        let summary = self.build_project_digest_summary(project_id)?;
        if !summary.proposed_actions.contains(&action) {
            return Err(ApiError::conflict(format!(
                "action `{}` is not currently available for project `{}`",
                action.as_str(),
                summary.headline
            )));
        }

        match action {
            ProjectActionId::Approve => {
                let target = summary
                    .pending_approval_target
                    .ok_or_else(|| ApiError::conflict("project does not have a live approval"))?;
                let _ = self.update_approval(
                    &target.session_id,
                    &target.message_id,
                    ApprovalDecision::Accepted,
                )?;
            }
            ProjectActionId::Reject => {
                let target = summary
                    .pending_approval_target
                    .ok_or_else(|| ApiError::conflict("project does not have a live approval"))?;
                let _ = self.update_approval(
                    &target.session_id,
                    &target.message_id,
                    ApprovalDecision::Rejected,
                )?;
            }
            ProjectActionId::Continue
            | ProjectActionId::FixIt
            | ProjectActionId::KeepIterating
            | ProjectActionId::AskAgentToCommit => {
                let session_id = summary.primary_session_id.clone().ok_or_else(|| {
                    ApiError::conflict("project does not have a session to target")
                })?;
                let prompt = action
                    .prompt()
                    .ok_or_else(|| ApiError::internal("project action prompt is missing"))?;
                let dispatch = self.dispatch_turn(
                    &session_id,
                    SendMessageRequest {
                        text: prompt.to_owned(),
                        expanded_text: None,
                        attachments: Vec::new(),
                    },
                )?;
                if let DispatchTurnResult::Dispatched(dispatch) = dispatch {
                    deliver_turn_dispatch(self, dispatch)?;
                }
            }
            ProjectActionId::Stop => {
                let session_id = summary
                    .primary_session_id
                    .clone()
                    .ok_or_else(|| ApiError::conflict("project does not have a session to stop"))?;
                let _ = self.stop_session(&session_id)?;
            }
            ProjectActionId::ReviewInTermal => {}
        }

        self.project_digest(project_id)
    }

    /// Builds project digest summary.
    fn build_project_digest_summary(
        &self,
        project_id: &str,
    ) -> Result<ProjectDigestSummary, ApiError> {
        let inputs = self.project_digest_inputs(project_id)?;
        let git_status = self.load_project_git_status_best_effort(&inputs.project);
        let pending_approval = find_latest_project_pending_approval(&inputs.sessions);
        let pending_interaction = if pending_approval.is_none() {
            find_latest_project_pending_nonapproval_interaction(&inputs.sessions)
        } else {
            None
        };
        let error_session = if pending_approval.is_none() && pending_interaction.is_none() {
            inputs
                .sessions
                .iter()
                .rev()
                .find(|record| record.session.status == SessionStatus::Error)
        } else {
            None
        };
        let active_session = if pending_approval.is_none()
            && pending_interaction.is_none()
            && error_session.is_none()
        {
            inputs
                .sessions
                .iter()
                .rev()
                .find(|record| record.session.status == SessionStatus::Active)
        } else {
            None
        };
        let primary_session = pending_approval
            .as_ref()
            .map(|(record, _)| *record)
            .or_else(|| pending_interaction.as_ref().map(|(record, _)| *record))
            .or(error_session)
            .or(active_session)
            .or_else(|| {
                inputs
                    .sessions
                    .iter()
                    .rev()
                    .find(|record| !record.session.messages.is_empty())
            })
            .or_else(|| inputs.sessions.last());
        let primary_session_id = primary_session.map(|record| record.session.id.clone());
        let deep_link = Some(build_project_deep_link(
            &inputs.project.id,
            primary_session_id.as_deref(),
        ));
        let worktree_dirty = git_status.as_ref().is_some_and(|status| !status.is_clean);

        if let Some((record, message_id)) = pending_approval {
            let (done_summary, mut source_message_ids) =
                select_project_done_summary(primary_session, git_status.as_ref(), false);
            if !source_message_ids.contains(&message_id) {
                source_message_ids.insert(0, message_id.clone());
            }
            return Ok(ProjectDigestSummary {
                headline: inputs.project.name,
                project_id: inputs.project.id,
                primary_session_id,
                done_summary: normalize_project_text(
                    &done_summary,
                    "Work paused while waiting for approval.",
                ),
                current_status: "Waiting on your decision.".to_owned(),
                proposed_actions: vec![
                    ProjectActionId::Approve,
                    ProjectActionId::Reject,
                    ProjectActionId::ReviewInTermal,
                ],
                deep_link,
                pending_approval_target: Some(ProjectApprovalTarget {
                    session_id: record.session.id.clone(),
                    message_id,
                }),
                source_message_ids,
            });
        }

        if let Some((record, message_id)) = pending_interaction {
            let (done_summary, mut source_message_ids) =
                select_project_done_summary(primary_session, git_status.as_ref(), false);
            if !source_message_ids.contains(&message_id) {
                source_message_ids.insert(0, message_id);
            }
            let mut proposed_actions = vec![ProjectActionId::ReviewInTermal];
            if primary_session_id.is_some() {
                proposed_actions.push(ProjectActionId::Stop);
            }
            return Ok(ProjectDigestSummary {
                headline: inputs.project.name,
                project_id: inputs.project.id,
                primary_session_id,
                done_summary: normalize_project_text(
                    &done_summary,
                    "Work is waiting on a response in TermAl.",
                ),
                current_status: normalize_project_text(
                    &record.session.preview,
                    "Waiting on input in TermAl.",
                ),
                proposed_actions,
                deep_link,
                pending_approval_target: None,
                source_message_ids,
            });
        }

        if let Some(record) = error_session {
            let (done_summary, source_message_ids) =
                select_project_done_summary(primary_session, git_status.as_ref(), false);
            let mut proposed_actions = vec![ProjectActionId::ReviewInTermal];
            if primary_session_id.is_some() {
                proposed_actions.insert(0, ProjectActionId::FixIt);
            }
            return Ok(ProjectDigestSummary {
                headline: inputs.project.name,
                project_id: inputs.project.id,
                primary_session_id,
                done_summary: normalize_project_text(
                    &done_summary,
                    "The last turn ended in an error.",
                ),
                current_status: normalize_project_text(&record.session.preview, "Needs attention."),
                proposed_actions,
                deep_link,
                pending_approval_target: None,
                source_message_ids,
            });
        }

        if let Some(record) = active_session {
            let (done_summary, source_message_ids) =
                select_project_done_summary(primary_session, git_status.as_ref(), false);
            return Ok(ProjectDigestSummary {
                headline: inputs.project.name,
                project_id: inputs.project.id,
                primary_session_id,
                done_summary: normalize_project_text(&done_summary, "The agent is still working."),
                current_status: active_project_status_text(record),
                proposed_actions: vec![ProjectActionId::Stop, ProjectActionId::ReviewInTermal],
                deep_link,
                pending_approval_target: None,
                source_message_ids,
            });
        }

        if worktree_dirty {
            let (done_summary, source_message_ids) =
                select_project_done_summary(primary_session, git_status.as_ref(), true);
            let mut proposed_actions = vec![ProjectActionId::ReviewInTermal];
            if primary_session_id.is_some() {
                proposed_actions.push(ProjectActionId::AskAgentToCommit);
                proposed_actions.push(ProjectActionId::KeepIterating);
            }
            return Ok(ProjectDigestSummary {
                headline: inputs.project.name,
                project_id: inputs.project.id,
                primary_session_id,
                done_summary: normalize_project_text(
                    &done_summary,
                    "The working tree has changes ready for review.",
                ),
                current_status: "Changes are ready for review.".to_owned(),
                proposed_actions,
                deep_link,
                pending_approval_target: None,
                source_message_ids,
            });
        }

        let (done_summary, source_message_ids) =
            select_project_done_summary(primary_session, git_status.as_ref(), false);
        let proposed_actions = if primary_session_id.is_some() {
            vec![ProjectActionId::Continue, ProjectActionId::ReviewInTermal]
        } else {
            vec![ProjectActionId::ReviewInTermal]
        };
        Ok(ProjectDigestSummary {
            headline: inputs.project.name,
            project_id: inputs.project.id,
            primary_session_id,
            done_summary: normalize_project_text(&done_summary, "No agent work has started yet."),
            current_status: "Idle and unblocked.".to_owned(),
            proposed_actions,
            deep_link,
            pending_approval_target: None,
            source_message_ids,
        })
    }

    /// Handles project digest inputs.
    fn project_digest_inputs(&self, project_id: &str) -> Result<ProjectDigestInputs, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let project = inner
            .find_project(project_id)
            .cloned()
            .ok_or_else(|| ApiError::not_found("project not found"))?;
        let sessions = inner
            .sessions
            .iter()
            .filter(|record| {
                !record.hidden && record.session.project_id.as_deref() == Some(project_id)
            })
            .cloned()
            .collect();
        Ok(ProjectDigestInputs { project, sessions })
    }

    /// Loads project Git status best effort.
    fn load_project_git_status_best_effort(&self, project: &Project) -> Option<GitStatusResponse> {
        if project.remote_id == LOCAL_REMOTE_ID {
            return load_git_status_for_path(FsPath::new(&project.root_path)).ok();
        }
        let scope = self
            .remote_scope_for_request(None, Some(project.id.as_str()))
            .ok()
            .flatten()?;
        self.remote_get_json(
            &scope,
            "/api/git/status",
            vec![("path".to_owned(), project.root_path.clone())],
        )
        .ok()
    }
}

/// Runs blocking API.
async fn run_blocking_api<T, F>(operation: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|err| ApiError::internal(format!("blocking task failed: {err}")))?
}

/// Gets review.
async fn get_review(
    AxumPath(change_set_id): AxumPath<String>,
    Query(query): Query<ReviewQuery>,
    State(state): State<AppState>,
) -> Result<Json<ReviewDocumentResponse>, ApiError> {
    validate_review_change_set_id(&change_set_id)?;
    let response = run_blocking_api(move || {
        if let Some(scope) = state
            .remote_scope_for_request(query.session_id.as_deref(), query.project_id.as_deref())?
        {
            return state.remote_get_json(
                &scope,
                &format!("/api/reviews/{}", encode_uri_component(&change_set_id)),
                Vec::new(),
            );
        }

        let review_root = resolve_review_storage_root(
            &state,
            query.session_id.as_deref(),
            query.project_id.as_deref(),
        )?;
        let review_path = resolve_review_document_path(&review_root, &change_set_id)?;
        let review = {
            let _review_guard = state
                .review_documents_lock
                .lock()
                .expect("review documents mutex poisoned");
            load_review_document(&review_path, &change_set_id)?
        };
        Ok(ReviewDocumentResponse {
            review_file_path: review_path.to_string_lossy().into_owned(),
            review,
        })
    })
    .await?;
    Ok(Json(response))
}

/// Stores review.
async fn put_review(
    AxumPath(change_set_id): AxumPath<String>,
    Query(query): Query<ReviewQuery>,
    State(state): State<AppState>,
    Json(review): Json<ReviewDocument>,
) -> Result<Json<ReviewDocumentResponse>, ApiError> {
    validate_review_change_set_id(&change_set_id)?;
    let response = run_blocking_api(move || {
        if let Some(scope) = state
            .remote_scope_for_request(query.session_id.as_deref(), query.project_id.as_deref())?
        {
            return state.remote_put_json_with_query_scope(
                &scope,
                &format!("/api/reviews/{}", encode_uri_component(&change_set_id)),
                Vec::new(),
                serde_json::to_value(&review).map_err(|err| {
                    ApiError::internal(format!("failed to encode review payload: {err}"))
                })?,
            );
        }

        let review_root = resolve_review_storage_root(
            &state,
            query.session_id.as_deref(),
            query.project_id.as_deref(),
        )?;
        let review_path = resolve_review_document_path(&review_root, &change_set_id)?;
        let persisted_review = {
            let _review_guard = state
                .review_documents_lock
                .lock()
                .expect("review documents mutex poisoned");
            let persisted =
                prepare_review_document_for_write(&review_path, &change_set_id, review)?;
            persist_review_document(&review_path, &persisted)?;
            persisted
        };
        Ok(ReviewDocumentResponse {
            review_file_path: review_path.to_string_lossy().into_owned(),
            review: persisted_review,
        })
    })
    .await?;
    Ok(Json(response))
}

/// Gets review summary.
async fn get_review_summary(
    AxumPath(change_set_id): AxumPath<String>,
    Query(query): Query<ReviewQuery>,
    State(state): State<AppState>,
) -> Result<Json<ReviewSummaryResponse>, ApiError> {
    validate_review_change_set_id(&change_set_id)?;
    let response = run_blocking_api(move || {
        if let Some(scope) = state
            .remote_scope_for_request(query.session_id.as_deref(), query.project_id.as_deref())?
        {
            return state.remote_get_json(
                &scope,
                &format!(
                    "/api/reviews/{}/summary",
                    encode_uri_component(&change_set_id)
                ),
                Vec::new(),
            );
        }

        let review_root = resolve_review_storage_root(
            &state,
            query.session_id.as_deref(),
            query.project_id.as_deref(),
        )?;
        let review_path = resolve_review_document_path(&review_root, &change_set_id)?;
        let review = {
            let _review_guard = state
                .review_documents_lock
                .lock()
                .expect("review documents mutex poisoned");
            load_review_document(&review_path, &change_set_id)?
        };
        let summary = summarize_review_document(&review);

        Ok(ReviewSummaryResponse {
            change_set_id: review.change_set_id,
            review_file_path: review_path.to_string_lossy().into_owned(),
            thread_count: summary.thread_count,
            open_thread_count: summary.open_thread_count,
            resolved_thread_count: summary.resolved_thread_count,
            comment_count: summary.comment_count,
            has_threads: summary.thread_count > 0,
        })
    })
    .await?;
    Ok(Json(response))
}

/// Reads file.
async fn read_file(
    State(state): State<AppState>,
    Query(query): Query<FileQuery>,
) -> Result<Json<FileResponse>, ApiError> {
    // Step 1: resolve the path (needs brief mutex access). Use a small blocking
    // scope so we don't compete with streaming delta persists for pool time.
    let resolved_path = {
        let remote_scope = state
            .remote_scope_for_request(query.session_id.as_deref(), query.project_id.as_deref())?;
        if let Some(scope) = remote_scope {
            let response: FileResponse = run_blocking_api({
                let state = state.clone();
                let query_path = query.path.clone();
                move || {
                    state.remote_get_json(
                        &scope,
                        "/api/file",
                        vec![("path".to_owned(), query_path)],
                    )
                }
            })
            .await?;
            return Ok(Json(response));
        }

        resolve_project_scoped_requested_path(
            &state,
            query.session_id.as_deref(),
            query.project_id.as_deref(),
            &query.path,
            ScopedPathMode::ExistingFile,
        )?
    };

    // Step 2: read the file using tokio::fs (async, doesn't block the spawn_blocking pool).
    let metadata = tokio::fs::metadata(&resolved_path)
        .await
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => {
                ApiError::not_found(format!("file not found: {}", resolved_path.display()))
            }
            _ => ApiError::internal(format!(
                "failed to stat file {}: {err}",
                resolved_path.display()
            )),
        })?;
    if metadata.len() > MAX_FILE_CONTENT_BYTES as u64 {
        return Err(ApiError::bad_request(format!(
            "file exceeds the {} MB read limit: {}",
            MAX_FILE_CONTENT_BYTES / (1024 * 1024),
            resolved_path.display()
        )));
    }
    let content = tokio::fs::read_to_string(&resolved_path)
        .await
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => {
                ApiError::not_found(format!("file not found: {}", resolved_path.display()))
            }
            io::ErrorKind::InvalidData => ApiError::bad_request(format!(
                "file is not valid UTF-8: {}",
                resolved_path.display()
            )),
            _ => ApiError::internal(format!(
                "failed to read file {}: {err}",
                resolved_path.display()
            )),
        })?;

    let response = FileResponse {
        path: resolved_path.to_string_lossy().into_owned(),
        content,
        language: infer_language_from_path(&resolved_path).map(str::to_owned),
    };
    Ok(Json(response))
}

/// Writes file.
async fn write_file(
    State(state): State<AppState>,
    Json(request): Json<WriteFileRequest>,
) -> Result<Json<FileResponse>, ApiError> {
    let response = run_blocking_api(move || {
        if let Some(scope) = state.remote_scope_for_request(
            request.session_id.as_deref(),
            request.project_id.as_deref(),
        )? {
            return state.remote_put_json(
                &scope,
                "/api/file",
                json!({
                    "path": request.path.clone(),
                    "content": request.content.clone(),
                }),
            );
        }

        if request.content.as_bytes().len() > MAX_FILE_CONTENT_BYTES {
            return Err(ApiError::bad_request(format!(
                "file content exceeds the {} MB write limit",
                MAX_FILE_CONTENT_BYTES / (1024 * 1024)
            )));
        }

        let resolved_path = resolve_project_scoped_requested_path(
            &state,
            request.session_id.as_deref(),
            request.project_id.as_deref(),
            &request.path,
            ScopedPathMode::AllowMissingLeaf,
        )?;
        if let Some(parent) = resolved_path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                ApiError::internal(format!(
                    "failed to create parent directory for {}: {err}",
                    resolved_path.display()
                ))
            })?;
        }

        fs::write(&resolved_path, request.content.as_bytes()).map_err(|err| {
            ApiError::internal(format!(
                "failed to write file {}: {err}",
                resolved_path.display()
            ))
        })?;

        Ok(FileResponse {
            path: resolved_path.to_string_lossy().into_owned(),
            content: request.content,
            language: infer_language_from_path(&resolved_path).map(str::to_owned),
        })
    })
    .await?;
    Ok(Json(response))
}

/// Reads directory.
async fn read_directory(
    State(state): State<AppState>,
    Query(query): Query<FileQuery>,
) -> Result<Json<DirectoryResponse>, ApiError> {
    let response = run_blocking_api(move || {
        if let Some(scope) = state
            .remote_scope_for_request(query.session_id.as_deref(), query.project_id.as_deref())?
        {
            return state.remote_get_json(
                &scope,
                "/api/fs",
                vec![("path".to_owned(), query.path.clone())],
            );
        }

        let resolved_path = resolve_project_scoped_requested_path(
            &state,
            query.session_id.as_deref(),
            query.project_id.as_deref(),
            &query.path,
            ScopedPathMode::ExistingPath,
        )?;
        let metadata = fs::metadata(&resolved_path).map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => {
                ApiError::not_found(format!("path not found: {}", resolved_path.display()))
            }
            _ => ApiError::internal(format!(
                "failed to stat path {}: {err}",
                resolved_path.display()
            )),
        })?;

        if !metadata.is_dir() {
            return Err(ApiError::bad_request(format!(
                "path is not a directory: {}",
                resolved_path.display()
            )));
        }

        let mut entries = fs::read_dir(&resolved_path)
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to read directory {}: {err}",
                    resolved_path.display()
                ))
            })?
            .map(|entry| {
                let entry = entry.map_err(|err| {
                    ApiError::internal(format!(
                        "failed to read directory entry in {}: {err}",
                        resolved_path.display()
                    ))
                })?;
                let path = entry.path();
                let metadata = entry.metadata().map_err(|err| {
                    ApiError::internal(format!(
                        "failed to stat directory entry {}: {err}",
                        path.display()
                    ))
                })?;
                let name = entry.file_name().to_string_lossy().into_owned();

                Ok(DirectoryEntry {
                    kind: if metadata.is_dir() {
                        FileSystemEntryKind::Directory
                    } else {
                        FileSystemEntryKind::File
                    },
                    name,
                    path: path.to_string_lossy().into_owned(),
                })
            })
            .collect::<Result<Vec<_>, ApiError>>()?;

        entries.sort_by(|left, right| {
            let kind_order = match (&left.kind, &right.kind) {
                (FileSystemEntryKind::Directory, FileSystemEntryKind::File) => {
                    std::cmp::Ordering::Less
                }
                (FileSystemEntryKind::File, FileSystemEntryKind::Directory) => {
                    std::cmp::Ordering::Greater
                }
                _ => std::cmp::Ordering::Equal,
            };

            if kind_order == std::cmp::Ordering::Equal {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            } else {
                kind_order
            }
        });

        Ok(DirectoryResponse {
            name: resolved_path
                .file_name()
                .and_then(|value| value.to_str())
                .map(str::to_owned)
                .unwrap_or_else(|| resolved_path.to_string_lossy().into_owned()),
            path: resolved_path.to_string_lossy().into_owned(),
            entries,
        })
    })
    .await?;
    Ok(Json(response))
}

/// Lists agent commands.
async fn list_agent_commands(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<AgentCommandsResponse>, ApiError> {
    let response = run_blocking_api(move || state.list_agent_commands(&session_id)).await?;
    Ok(Json(response))
}

/// Searches instructions.
async fn search_instructions(
    Query(query): Query<InstructionSearchQuery>,
    State(state): State<AppState>,
) -> Result<Json<InstructionSearchResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.search_instructions(&query.session_id, &query.q)).await?;
    Ok(Json(response))
}

/// Reads Claude agent commands.
fn read_claude_agent_commands(workdir: &FsPath) -> Result<Vec<AgentCommand>, ApiError> {
    let commands_dir = workdir.join(".claude").join("commands");
    let entries = match fs::read_dir(&commands_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(ApiError::internal(format!(
                "failed to read agent commands in {}: {err}",
                commands_dir.display()
            )));
        }
    };

    let mut commands = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| {
            ApiError::internal(format!(
                "failed to read agent command entry in {}: {err}",
                commands_dir.display()
            ))
        })?;
        let path = entry.path();
        let metadata = entry.metadata().map_err(|err| {
            ApiError::internal(format!(
                "failed to stat agent command {}: {err}",
                path.display()
            ))
        })?;
        if !metadata.is_file() {
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }

        let content = fs::read_to_string(&path).map_err(|err| {
            ApiError::internal(format!(
                "failed to read agent command {}: {err}",
                path.display()
            ))
        })?;
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        let description = content
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("")
            .to_owned();

        commands.push(AgentCommand {
            kind: AgentCommandKind::PromptTemplate,
            name: stem.to_owned(),
            description,
            content,
            source: format!(".claude/commands/{}.md", stem),
            argument_hint: None,
        });
    }

    commands.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
    });
    Ok(commands)
}

/// Searches instruction phrase.
fn search_instruction_phrase(
    workdir: &FsPath,
    query: &str,
) -> Result<InstructionSearchResponse, ApiError> {
    let trimmed_query = query.trim();
    if trimmed_query.is_empty() {
        return Err(ApiError::bad_request(
            "instruction search query cannot be empty",
        ));
    }

    let graph = build_instruction_search_graph(workdir)?;
    let normalized_query = trimmed_query.to_ascii_lowercase();
    let mut matches = Vec::new();
    let mut documents = graph.documents.values().collect::<Vec<_>>();
    documents.sort_by(|left, right| {
        left.path
            .to_string_lossy()
            .to_ascii_lowercase()
            .cmp(&right.path.to_string_lossy().to_ascii_lowercase())
    });

    for document in documents {
        for (line_index, line) in document.lines.iter().enumerate() {
            if !line.to_ascii_lowercase().contains(&normalized_query) {
                continue;
            }

            matches.push(InstructionSearchMatch {
                line: line_index + 1,
                path: document.path.to_string_lossy().into_owned(),
                root_paths: trace_instruction_roots(&graph, &document.path),
                text: line.trim().to_owned(),
            });
        }
    }

    matches.sort_by(|left, right| {
        left.path
            .to_ascii_lowercase()
            .cmp(&right.path.to_ascii_lowercase())
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| {
                left.text
                    .to_ascii_lowercase()
                    .cmp(&right.text.to_ascii_lowercase())
            })
    });

    Ok(InstructionSearchResponse {
        matches,
        query: trimmed_query.to_owned(),
        workdir: workdir.to_string_lossy().into_owned(),
    })
}

/// Builds instruction search graph.
fn build_instruction_search_graph(workdir: &FsPath) -> Result<InstructionSearchGraph, ApiError> {
    let normalized_workdir = normalize_path_best_effort(workdir);
    let seed_paths = discover_instruction_seed_paths(&normalized_workdir)?;
    let mut documents = HashMap::new();
    let mut outgoing = HashMap::<String, Vec<InstructionPathStep>>::new();
    let mut queued = HashSet::new();
    let mut pending = VecDeque::new();

    for path in seed_paths {
        let normalized = normalize_path_best_effort(&path);
        let normalized_string = normalized.to_string_lossy().into_owned();
        if queued.insert(normalized_string.clone()) {
            pending.push_back(normalized);
        }
    }

    while let Some(path) = pending.pop_front() {
        let normalized_path = normalize_path_best_effort(&path);
        let normalized_path_string = normalized_path.to_string_lossy().into_owned();
        if documents.contains_key(&normalized_path_string) {
            continue;
        }

        let document = read_instruction_document(&normalized_path, &normalized_workdir)?;
        let edges = extract_instruction_edges(&document, &normalized_workdir)?;
        for edge in &edges {
            if queued.insert(edge.to_path.clone()) {
                pending.push_back(PathBuf::from(&edge.to_path));
            }
        }

        outgoing.insert(normalized_path_string.clone(), edges);
        documents.insert(normalized_path_string, document);
    }

    let mut incoming = HashMap::<String, Vec<InstructionPathStep>>::new();
    for steps in outgoing.values() {
        for step in steps {
            incoming
                .entry(step.to_path.clone())
                .or_default()
                .push(step.clone());
        }
    }

    for steps in outgoing.values_mut() {
        steps.sort_by(|left, right| {
            left.from_path
                .to_ascii_lowercase()
                .cmp(&right.from_path.to_ascii_lowercase())
                .then_with(|| left.line.cmp(&right.line))
                .then_with(|| {
                    left.to_path
                        .to_ascii_lowercase()
                        .cmp(&right.to_path.to_ascii_lowercase())
                })
        });
    }

    for steps in incoming.values_mut() {
        steps.sort_by(|left, right| {
            left.from_path
                .to_ascii_lowercase()
                .cmp(&right.from_path.to_ascii_lowercase())
                .then_with(|| left.line.cmp(&right.line))
                .then_with(|| {
                    left.to_path
                        .to_ascii_lowercase()
                        .cmp(&right.to_path.to_ascii_lowercase())
                })
        });
    }

    Ok(InstructionSearchGraph {
        documents,
        incoming,
    })
}

/// Handles discover instruction seed paths.
fn discover_instruction_seed_paths(workdir: &FsPath) -> Result<Vec<PathBuf>, ApiError> {
    let mut pending_directories = vec![workdir.to_path_buf()];
    let mut paths = Vec::new();

    while let Some(directory) = pending_directories.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(ApiError::internal(format!(
                    "failed to read instruction directory {}: {err}",
                    directory.display()
                )));
            }
        };

        for entry in entries {
            let entry = entry.map_err(|err| {
                ApiError::internal(format!(
                    "failed to read instruction directory entry in {}: {err}",
                    directory.display()
                ))
            })?;
            let path = entry.path();
            let metadata = entry.metadata().map_err(|err| {
                ApiError::internal(format!(
                    "failed to stat instruction path {}: {err}",
                    path.display()
                ))
            })?;

            if metadata.is_dir() {
                if should_skip_instruction_directory(&path) {
                    continue;
                }
                pending_directories.push(path);
                continue;
            }

            if is_instruction_seed_path(&path, workdir) {
                paths.push(path);
            }
        }
    }

    paths.sort_by(|left, right| {
        left.to_string_lossy()
            .to_ascii_lowercase()
            .cmp(&right.to_string_lossy().to_ascii_lowercase())
    });
    paths.dedup_by(|left, right| {
        normalize_path_best_effort(left) == normalize_path_best_effort(right)
    });
    Ok(paths)
}

/// Returns whether skip instruction directory.
fn should_skip_instruction_directory(path: &FsPath) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    should_skip_instruction_directory_name(name)
}

/// Returns whether skip instruction directory name.
fn should_skip_instruction_directory_name(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".idea" | ".termal" | "node_modules" | "target" | "dist" | "build" | ".next"
    )
}

/// Returns whether instruction seed path.
fn is_instruction_seed_path(path: &FsPath, workdir: &FsPath) -> bool {
    if !path.is_file() {
        return false;
    }

    let normalized = normalize_path_best_effort(path);
    if !path_contains(&workdir.to_string_lossy(), &normalized)
        || path_is_in_skipped_instruction_directory(&normalized, workdir)
    {
        return false;
    }

    let lower_file_name = normalized
        .file_name()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    let lower_relative = normalized
        .strip_prefix(workdir)
        .ok()
        .map(|value| {
            value
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
        })
        .unwrap_or_else(|| {
            normalized
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
        });

    if matches!(
        lower_file_name.as_deref(),
        Some("agents.md")
            | Some("claude.md")
            | Some("gemini.md")
            | Some("skills.md")
            | Some("skill.md")
            | Some("rules.md")
            | Some("agent.md")
            | Some(".claude.md")
    ) {
        return true;
    }

    if lower_relative.starts_with(".claude/commands/") && lower_relative.ends_with(".md") {
        return true;
    }

    if lower_relative.starts_with(".claude/reviewers/") && lower_relative.ends_with(".md") {
        return true;
    }

    if lower_relative == ".cursor/rules" {
        return true;
    }

    if lower_relative.starts_with(".cursor/rules/")
        && (lower_relative.ends_with(".md") || lower_relative.ends_with(".mdc"))
    {
        return true;
    }

    normalized
        .ancestors()
        .skip(1)
        .filter_map(|ancestor| ancestor.file_name().and_then(|value| value.to_str()))
        .map(|value| value.to_ascii_lowercase())
        .any(|value| value == "skills" || value == "rules")
        && lower_relative.ends_with(".md")
}

/// Reads instruction document.
fn read_instruction_document(
    path: &FsPath,
    workdir: &FsPath,
) -> Result<InstructionDocumentInternal, ApiError> {
    let content = fs::read_to_string(path).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => {
            ApiError::not_found(format!("instruction file not found: {}", path.display()))
        }
        io::ErrorKind::InvalidData => ApiError::bad_request(format!(
            "instruction file is not valid UTF-8: {}",
            path.display()
        )),
        _ => ApiError::internal(format!(
            "failed to read instruction file {}: {err}",
            path.display()
        )),
    })?;

    Ok(InstructionDocumentInternal {
        kind: classify_instruction_document_kind(path, workdir),
        lines: content.lines().map(str::to_owned).collect(),
        path: path.to_path_buf(),
    })
}

/// Classifies instruction document kind.
fn classify_instruction_document_kind(path: &FsPath, workdir: &FsPath) -> InstructionDocumentKind {
    let lower_file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    let lower_relative = path
        .strip_prefix(workdir)
        .ok()
        .map(|value| {
            value
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
        })
        .unwrap_or_else(|| {
            path.to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase()
        });

    if matches!(lower_file_name.as_deref(), Some("skill.md")) {
        return InstructionDocumentKind::SkillInstruction;
    }

    if matches!(
        lower_file_name.as_deref(),
        Some("agents.md")
            | Some("claude.md")
            | Some("gemini.md")
            | Some("rules.md")
            | Some("skills.md")
            | Some("agent.md")
            | Some(".claude.md")
    ) {
        return InstructionDocumentKind::RootInstruction;
    }

    if lower_relative.starts_with(".claude/commands/") {
        return InstructionDocumentKind::CommandInstruction;
    }

    if lower_relative.starts_with(".claude/reviewers/") {
        return InstructionDocumentKind::ReviewerInstruction;
    }

    if lower_relative.starts_with(".cursor/rules/")
        || path
            .ancestors()
            .skip(1)
            .filter_map(|ancestor| ancestor.file_name().and_then(|value| value.to_str()))
            .any(|value| {
                value.eq_ignore_ascii_case("rules")
                    || value.eq_ignore_ascii_case("instructions")
                    || value.eq_ignore_ascii_case("agents")
            })
    {
        return InstructionDocumentKind::RulesInstruction;
    }

    if path
        .ancestors()
        .skip(1)
        .filter_map(|ancestor| ancestor.file_name().and_then(|value| value.to_str()))
        .any(|value| value.eq_ignore_ascii_case("skills"))
    {
        return InstructionDocumentKind::SkillInstruction;
    }

    InstructionDocumentKind::ReferencedInstruction
}

/// Extracts instruction edges.
fn extract_instruction_edges(
    document: &InstructionDocumentInternal,
    workdir: &FsPath,
) -> Result<Vec<InstructionPathStep>, ApiError> {
    if !supports_transitive_instruction_edges(document.kind) {
        return Ok(Vec::new());
    }

    let mut edges = Vec::new();
    let mut seen = HashSet::new();

    for (line_index, line) in document.lines.iter().enumerate() {
        let line_number = line_index + 1;

        for raw_target in extract_markdown_link_targets(line) {
            maybe_push_instruction_file_edge(
                &mut edges,
                &mut seen,
                document,
                line_number,
                line,
                workdir,
                &raw_target,
                InstructionRelation::MarkdownLink,
            );
        }

        for raw_target in extract_instruction_path_tokens(line) {
            maybe_push_instruction_file_edge(
                &mut edges,
                &mut seen,
                document,
                line_number,
                line,
                workdir,
                &raw_target,
                InstructionRelation::FileReference,
            );
        }

        for raw_directory in [".claude/reviewers", ".claude/commands", ".cursor/rules"] {
            if !line.contains(raw_directory) {
                continue;
            }

            if let Some(directory) =
                resolve_instruction_reference_directory(&document.path, workdir, raw_directory)
            {
                let markdown_files = collect_markdown_files_in_directory(&directory)?;
                for markdown_file in markdown_files {
                    maybe_push_instruction_edge(
                        &mut edges,
                        &mut seen,
                        line_number,
                        line,
                        &document.path,
                        &markdown_file,
                        InstructionRelation::DirectoryDiscovery,
                    );
                }
            }
        }
    }

    Ok(edges)
}

/// Handles maybe push instruction file edge.
fn maybe_push_instruction_file_edge(
    edges: &mut Vec<InstructionPathStep>,
    seen: &mut HashSet<(String, usize, InstructionRelation)>,
    document: &InstructionDocumentInternal,
    line_number: usize,
    line: &str,
    workdir: &FsPath,
    raw_target: &str,
    relation: InstructionRelation,
) {
    let Some(target_path) = resolve_instruction_reference_file(&document.path, workdir, raw_target)
    else {
        return;
    };

    maybe_push_instruction_edge(
        edges,
        seen,
        line_number,
        line,
        &document.path,
        &target_path,
        relation,
    );
}

/// Handles maybe push instruction edge.
fn maybe_push_instruction_edge(
    edges: &mut Vec<InstructionPathStep>,
    seen: &mut HashSet<(String, usize, InstructionRelation)>,
    line_number: usize,
    line: &str,
    from_path: &FsPath,
    to_path: &FsPath,
    relation: InstructionRelation,
) {
    let normalized_from = normalize_path_best_effort(from_path)
        .to_string_lossy()
        .into_owned();
    let normalized_to = normalize_path_best_effort(to_path)
        .to_string_lossy()
        .into_owned();
    if normalized_from == normalized_to {
        return;
    }

    let dedupe_key = (normalized_to.clone(), line_number, relation);
    if !seen.insert(dedupe_key) {
        return;
    }

    edges.push(InstructionPathStep {
        excerpt: line.trim().to_owned(),
        from_path: normalized_from,
        line: line_number,
        relation,
        to_path: normalized_to,
    });
}

/// Resolves instruction reference file.
fn resolve_instruction_reference_file(
    source_path: &FsPath,
    workdir: &FsPath,
    raw_target: &str,
) -> Option<PathBuf> {
    let sanitized_target = sanitize_instruction_reference(raw_target);
    if sanitized_target.is_empty() {
        return None;
    }

    let path_only = sanitized_target
        .split_once('#')
        .map(|(prefix, _)| prefix)
        .unwrap_or(sanitized_target.as_str())
        .split_once('?')
        .map(|(prefix, _)| prefix)
        .unwrap_or(sanitized_target.as_str())
        .trim();
    if path_only.is_empty() {
        return None;
    }

    for candidate in instruction_reference_candidates(source_path, workdir, path_only) {
        if !candidate.is_file() {
            continue;
        }

        let lower_name = candidate
            .file_name()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase());
        let extension = candidate.extension().and_then(|value| value.to_str());
        if matches!(extension, Some("md") | Some("mdc"))
            || matches!(lower_name.as_deref(), Some("rules") | Some(".claude"))
        {
            return Some(candidate);
        }
    }

    None
}

/// Resolves instruction reference directory.
fn resolve_instruction_reference_directory(
    source_path: &FsPath,
    workdir: &FsPath,
    raw_target: &str,
) -> Option<PathBuf> {
    let sanitized_target = sanitize_instruction_reference(raw_target);
    if sanitized_target.is_empty() {
        return None;
    }

    instruction_reference_candidates(source_path, workdir, &sanitized_target)
        .into_iter()
        .find(|candidate| candidate.is_dir())
}

/// Handles instruction reference candidates.
fn instruction_reference_candidates(
    source_path: &FsPath,
    workdir: &FsPath,
    raw_target: &str,
) -> Vec<PathBuf> {
    let target_path = FsPath::new(raw_target);
    let mut candidates = Vec::new();

    if target_path.is_absolute() {
        let normalized = normalize_path_best_effort(target_path);
        if path_contains(&workdir.to_string_lossy(), &normalized)
            && !path_is_in_skipped_instruction_directory(&normalized, workdir)
        {
            candidates.push(normalized);
        }
        return candidates;
    }

    if let Some(parent) = source_path.parent() {
        let normalized = normalize_path_best_effort(&parent.join(target_path));
        if path_contains(&workdir.to_string_lossy(), &normalized)
            && !path_is_in_skipped_instruction_directory(&normalized, workdir)
        {
            candidates.push(normalized);
        }
    }

    let workdir_relative = normalize_path_best_effort(&workdir.join(target_path));
    if path_contains(&workdir.to_string_lossy(), &workdir_relative)
        && !path_is_in_skipped_instruction_directory(&workdir_relative, workdir)
        && !candidates
            .iter()
            .any(|candidate| candidate == &workdir_relative)
    {
        candidates.push(workdir_relative);
    }

    candidates
}

/// Extracts markdown link targets.
fn extract_markdown_link_targets(line: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut start = 0usize;

    while let Some(relative_open) = line[start..].find("](") {
        let target_start = start + relative_open + 2;
        let Some(relative_close) = line[target_start..].find(')') else {
            break;
        };
        let target_end = target_start + relative_close;
        targets.push(line[target_start..target_end].to_owned());
        start = target_end + 1;
    }

    targets
}

/// Extracts instruction path tokens.
fn extract_instruction_path_tokens(line: &str) -> Vec<String> {
    line.split_whitespace()
        .map(sanitize_instruction_reference)
        .filter(|token| {
            if token.is_empty() {
                return false;
            }

            let lower = token.to_ascii_lowercase();
            lower.contains(".md")
                || lower.contains(".mdc")
                || matches!(
                    lower.as_str(),
                    "agents.md"
                        | "claude.md"
                        | "gemini.md"
                        | "skills.md"
                        | "skill.md"
                        | "rules.md"
                        | "agent.md"
                        | ".claude.md"
                        | ".cursor/rules"
                )
        })
        .collect()
}

/// Sanitizes instruction reference.
fn sanitize_instruction_reference(raw: &str) -> String {
    raw.trim()
        .trim_matches(|character: char| {
            matches!(
                character,
                '`' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';' | ':'
            )
        })
        .trim_end_matches('.')
        .trim()
        .to_owned()
}

/// Collects markdown files in directory.
fn collect_markdown_files_in_directory(directory: &FsPath) -> Result<Vec<PathBuf>, ApiError> {
    let mut pending_directories = vec![directory.to_path_buf()];
    let mut paths = Vec::new();

    while let Some(current) = pending_directories.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(ApiError::internal(format!(
                    "failed to read instruction directory {}: {err}",
                    current.display()
                )));
            }
        };

        for entry in entries {
            let entry = entry.map_err(|err| {
                ApiError::internal(format!(
                    "failed to read instruction directory entry in {}: {err}",
                    current.display()
                ))
            })?;
            let path = entry.path();
            let metadata = entry.metadata().map_err(|err| {
                ApiError::internal(format!(
                    "failed to stat instruction directory path {}: {err}",
                    path.display()
                ))
            })?;

            if metadata.is_dir() {
                if should_skip_instruction_directory(&path) {
                    continue;
                }
                pending_directories.push(path);
                continue;
            }

            if matches!(
                path.extension().and_then(|value| value.to_str()),
                Some("md") | Some("mdc")
            ) {
                paths.push(normalize_path_best_effort(&path));
            }
        }
    }

    paths.sort_by(|left, right| {
        left.to_string_lossy()
            .to_ascii_lowercase()
            .cmp(&right.to_string_lossy().to_ascii_lowercase())
    });
    paths.dedup();
    Ok(paths)
}

/// Returns whether transitive instruction edges.
fn supports_transitive_instruction_edges(kind: InstructionDocumentKind) -> bool {
    !matches!(kind, InstructionDocumentKind::ReferencedInstruction)
}

/// Handles path is in skipped instruction directory.
fn path_is_in_skipped_instruction_directory(path: &FsPath, workdir: &FsPath) -> bool {
    let normalized = normalize_path_best_effort(path);
    let Ok(relative) = normalized.strip_prefix(workdir) else {
        return false;
    };

    relative.components().any(|component| match component {
        std::path::Component::Normal(value) => value
            .to_str()
            .map(should_skip_instruction_directory_name)
            .unwrap_or(false),
        _ => false,
    })
}

/// Traces instruction roots.
fn trace_instruction_roots(
    graph: &InstructionSearchGraph,
    target_path: &FsPath,
) -> Vec<InstructionRootPath> {
    let normalized_target = normalize_path_best_effort(target_path);
    let normalized_target_string = normalized_target.to_string_lossy().into_owned();
    let mut results = Vec::new();
    let mut current_steps = Vec::new();
    let mut visited = HashSet::new();

    trace_instruction_roots_recursive(
        graph,
        &normalized_target_string,
        &mut current_steps,
        &mut visited,
        &mut results,
    );

    results.sort_by(|left, right| {
        left.root_path
            .to_ascii_lowercase()
            .cmp(&right.root_path.to_ascii_lowercase())
            .then_with(|| left.steps.len().cmp(&right.steps.len()))
    });
    results.dedup_by(|left, right| {
        left.root_path == right.root_path
            && left.root_kind == right.root_kind
            && left.steps == right.steps
    });
    results
}

/// Traces instruction roots recursive.
fn trace_instruction_roots_recursive(
    graph: &InstructionSearchGraph,
    current_path: &str,
    current_steps: &mut Vec<InstructionPathStep>,
    visited: &mut HashSet<String>,
    results: &mut Vec<InstructionRootPath>,
) {
    if !visited.insert(current_path.to_owned()) {
        return;
    }

    let incoming = graph
        .incoming
        .get(current_path)
        .cloned()
        .unwrap_or_default();
    if incoming.is_empty() {
        if let Some(document) = graph.documents.get(current_path) {
            let mut steps = current_steps.clone();
            steps.reverse();
            results.push(InstructionRootPath {
                root_kind: document.kind,
                root_path: document.path.to_string_lossy().into_owned(),
                steps,
            });
        }
        visited.remove(current_path);
        return;
    }

    for edge in incoming {
        current_steps.push(edge.clone());
        trace_instruction_roots_recursive(graph, &edge.from_path, current_steps, visited, results);
        current_steps.pop();
    }

    visited.remove(current_path);
}

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
async fn apply_git_file_action(
    State(state): State<AppState>,
    Json(request): Json<GitFileActionRequest>,
) -> Result<Json<GitStatusResponse>, ApiError> {
    let response = run_blocking_api(move || {
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

        let workdir = resolve_existing_requested_path(&request.workdir, "path")?;
        let workdir = normalize_git_workdir_path(&workdir)?;
        let Some(repo_root) = resolve_git_repo_root(&workdir)? else {
            return Err(ApiError::bad_request("no git repository found"));
        };

        let current_path = normalize_git_repo_relative_path(&request.path)?;
        let original_path = request
            .original_path
            .as_deref()
            .map(normalize_git_repo_relative_path)
            .transpose()?;

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

/// Handles commit Git changes.
async fn commit_git_changes(
    State(state): State<AppState>,
    Json(request): Json<GitCommitRequest>,
) -> Result<Json<GitCommitResponse>, ApiError> {
    let response = run_blocking_api(move || {
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

        let workdir = resolve_existing_requested_path(&request.workdir, "path")?;
        let workdir = normalize_git_workdir_path(&workdir)?;
        let Some(repo_root) = resolve_git_repo_root(&workdir)? else {
            return Err(ApiError::bad_request("no git repository found"));
        };

        let message = request.message.trim();
        if message.is_empty() {
            return Err(ApiError::bad_request("commit message cannot be empty"));
        }

        let status_before = load_git_status_for_path(&workdir)?;
        if !has_staged_git_changes(&status_before) {
            return Err(ApiError::bad_request("no staged changes to commit"));
        }

        let output = Command::new("git")
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

        let workdir = resolve_existing_requested_path(&request.workdir, "path")?;
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

        let workdir = resolve_existing_requested_path(&request.workdir, "path")?;
        sync_git_repo(&workdir)
    })
    .await?;
    Ok(Json(response))
}
/// Loads Git diff for request.
fn load_git_diff_for_request(
    workdir: &FsPath,
    request: &GitDiffRequest,
) -> Result<GitDiffResponse, ApiError> {
    let workdir = normalize_git_workdir_path(workdir)?;
    let Some(repo_root) = resolve_git_repo_root(&workdir)? else {
        return Err(ApiError::bad_request("no git repository found"));
    };

    let current_path = normalize_git_repo_relative_path(&request.path)?;
    let original_path = request
        .original_path
        .as_deref()
        .map(normalize_git_repo_relative_path)
        .transpose()?;
    let status_code = request
        .status_code
        .as_deref()
        .and_then(|value| value.chars().next())
        .and_then(normalize_git_status_code);
    let diff = load_git_file_diff_text(
        &repo_root,
        &current_path,
        original_path.as_deref(),
        status_code.as_deref(),
        request.section_id,
    )?;

    if diff.trim().is_empty() {
        return Err(ApiError::bad_request(format!(
            "no diff available for {}",
            current_path
        )));
    }

    let file_path = repo_root.join(&current_path);
    let diff_identity = [
        repo_root.to_string_lossy().as_ref(),
        request.section_id.as_key(),
        current_path.as_str(),
        original_path.as_deref().unwrap_or(""),
        diff.as_str(),
    ]
    .join("\n");

    let diff_hash = stable_text_hash(&diff_identity);

    Ok(GitDiffResponse {
        change_type: if matches!(status_code.as_deref(), Some("A" | "?")) {
            GitDiffChangeType::Create
        } else {
            GitDiffChangeType::Edit
        },
        change_set_id: format!("git-diff-{diff_hash}"),
        diff,
        diff_id: format!("git:{diff_hash}"),
        file_path: file_path
            .exists()
            .then(|| file_path.to_string_lossy().into_owned()),
        language: infer_language_from_path(FsPath::new(&current_path)).map(str::to_owned),
        summary: format!(
            "{} changes in {}",
            request.section_id.summary_label(),
            current_path
        ),
    })
}

/// Loads Git status for path.
fn load_git_status_for_path(path: &FsPath) -> Result<GitStatusResponse, ApiError> {
    let workdir = normalize_git_workdir_path(path)?;
    let Some(repo_root) = resolve_git_repo_root(&workdir)? else {
        return Ok(GitStatusResponse {
            ahead: 0,
            behind: 0,
            branch: None,
            files: Vec::new(),
            is_clean: true,
            repo_root: None,
            upstream: None,
            workdir: workdir.to_string_lossy().into_owned(),
        });
    };

    let output = Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .args(["status", "--porcelain=v1", "--branch", "-uall"])
        .output()
        .map_err(|err| ApiError::internal(format!("failed to run git status: {err}")))?;

    if !output.status.success() {
        return Err(ApiError::internal(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut branch = None;
    let mut upstream = None;
    let mut ahead = 0;
    let mut behind = 0;
    let mut files = Vec::new();

    for line in stdout.lines() {
        if let Some(branch_info) = line.strip_prefix("## ") {
            let parsed = parse_git_branch_status(branch_info);
            branch = parsed.branch;
            upstream = parsed.upstream;
            ahead = parsed.ahead;
            behind = parsed.behind;
            continue;
        }

        if line.len() < 3 {
            continue;
        }

        let status = &line[..2];
        let path_payload = line[3..].trim();
        let (original_path, path) = parse_git_status_paths(path_payload);
        let index_status = status.chars().next().and_then(normalize_git_status_code);
        let worktree_status = status.chars().nth(1).and_then(normalize_git_status_code);

        files.push(GitStatusFile {
            index_status,
            original_path,
            path,
            worktree_status,
        });
    }

    let is_clean = files.is_empty();

    Ok(GitStatusResponse {
        ahead,
        behind,
        branch,
        files,
        is_clean,
        repo_root: Some(repo_root.to_string_lossy().into_owned()),
        upstream,
        workdir: workdir.to_string_lossy().into_owned(),
    })
}

/// Normalizes Git working directory path.
fn normalize_git_workdir_path(path: &FsPath) -> Result<PathBuf, ApiError> {
    if path.is_dir() {
        return Ok(path.to_path_buf());
    }

    path.parent()
        .map(FsPath::to_path_buf)
        .ok_or_else(|| ApiError::bad_request("cannot inspect git status for a root file path"))
}

/// Normalizes Git repo relative path.
fn normalize_git_repo_relative_path(path: &str) -> Result<String, ApiError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request("git file path cannot be empty"));
    }

    if FsPath::new(trimmed).is_absolute() {
        return Err(ApiError::bad_request(
            "git file actions require repository-relative paths",
        ));
    }

    if trimmed.contains('\0') {
        return Err(ApiError::bad_request(
            "git file path contains invalid characters",
        ));
    }

    if trimmed
        .split(['/', '\\'])
        .any(|component| component == "..")
    {
        return Err(ApiError::bad_request(
            "git file path cannot contain parent-directory traversal",
        ));
    }

    Ok(trimmed.to_owned())
}

/// Collects Git pathspecs.
fn collect_git_pathspecs(current_path: &str, original_path: Option<&str>) -> Vec<String> {
    let mut pathspecs = Vec::new();
    if let Some(original_path) = original_path.filter(|original| *original != current_path) {
        pathspecs.push(original_path.to_owned());
    }
    pathspecs.push(current_path.to_owned());
    pathspecs
}

/// Loads Git file diff text.
fn load_git_file_diff_text(
    repo_root: &FsPath,
    current_path: &str,
    original_path: Option<&str>,
    status_code: Option<&str>,
    section_id: GitDiffSection,
) -> Result<String, ApiError> {
    if matches!(section_id, GitDiffSection::Unstaged) && status_code == Some("?") {
        return build_untracked_git_diff(repo_root, current_path);
    }

    let pathspecs = collect_git_pathspecs(current_path, original_path);
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo_root)
        .arg("diff")
        .arg("--find-renames");

    if matches!(section_id, GitDiffSection::Staged) {
        command.arg("--cached");
    }

    let output = command
        .arg("--")
        .args(&pathspecs)
        .output()
        .map_err(|err| ApiError::internal(format!("failed to load git diff: {err}")))?;

    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        Err(ApiError::internal("failed to load git diff"))
    } else {
        Err(ApiError::internal(format!(
            "failed to load git diff: {stderr}"
        )))
    }
}

/// Builds untracked Git diff.
fn build_untracked_git_diff(repo_root: &FsPath, current_path: &str) -> Result<String, ApiError> {
    let file_path = repo_root.join(current_path);
    let content = fs::read(&file_path).map_err(|err| {
        ApiError::internal(format!(
            "failed to read untracked file {}: {err}",
            file_path.display()
        ))
    })?;
    let content = String::from_utf8_lossy(&content);
    let lines: Vec<&str> = content.lines().collect();
    let mut diff_lines = vec![
        format!("diff --git a/{current_path} b/{current_path}"),
        "new file mode 100644".to_owned(),
        "--- /dev/null".to_owned(),
        format!("+++ b/{current_path}"),
    ];

    if !lines.is_empty() {
        diff_lines.push(format!("@@ -0,0 +1,{} @@", lines.len()));
        diff_lines.extend(lines.into_iter().map(|line| format!("+{line}")));
    }

    if !content.is_empty() && !content.ends_with('\n') {
        diff_lines.push(r"\ No newline at end of file".to_owned());
    }

    Ok(diff_lines.join("\n"))
}

/// Reverts Git file action.
fn revert_git_file_action(
    repo_root: &FsPath,
    current_path: &str,
    original_path: Option<&str>,
    status_code: Option<&str>,
) -> Result<(), ApiError> {
    if let Some(original_path) = original_path.filter(|original| *original != current_path) {
        run_git_pathspec_command(
            repo_root,
            &["restore", "--worktree", "--source=HEAD"],
            &[original_path.to_owned()],
            "failed to restore the original git path",
        )?;
    }

    if status_code.is_some_and(|status| status.trim() == "?") {
        run_git_pathspec_command(
            repo_root,
            &["clean", "-f"],
            &[current_path.to_owned()],
            "failed to remove untracked git path",
        )?;
    } else {
        run_git_pathspec_command(
            repo_root,
            &["restore", "--worktree", "--source=HEAD"],
            &[current_path.to_owned()],
            "failed to revert git changes",
        )?;
    }

    Ok(())
}

/// Runs Git pathspec command.
fn run_git_pathspec_command(
    repo_root: &FsPath,
    args: &[&str],
    pathspecs: &[String],
    error_context: &str,
) -> Result<(), ApiError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .arg("--")
        .args(pathspecs)
        .output()
        .map_err(|err| ApiError::internal(format!("{error_context}: {err}")))?;

    if output.status.success() {
        return Ok(());
    }

    let detail = extract_git_command_error(&output);

    if detail.is_empty() {
        Err(ApiError::internal(error_context))
    } else {
        Err(ApiError::internal(format!("{error_context}: {detail}")))
    }
}

/// Returns whether staged Git changes.
fn has_staged_git_changes(status: &GitStatusResponse) -> bool {
    status.files.iter().any(|file| file.index_status.is_some())
}

/// Extracts Git command error.
fn extract_git_command_error(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !stderr.is_empty() {
        return stderr;
    }

    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Builds Git commit summary.
fn build_git_commit_summary(message: &str) -> String {
    let headline = message.lines().next().unwrap_or("").trim();
    if headline.is_empty() {
        "Created commit.".to_owned()
    } else {
        format!("Created commit: {headline}")
    }
}

/// Pushes Git repo.
fn push_git_repo(workdir: &FsPath) -> Result<GitRepoActionResponse, ApiError> {
    let workdir = normalize_git_workdir_path(workdir)?;
    let Some(repo_root) = resolve_git_repo_root(&workdir)? else {
        return Err(ApiError::bad_request("no git repository found"));
    };

    let status_before = load_git_status_for_path(&workdir)?;
    run_git_repo_command(&repo_root, &["push"], "failed to push git changes")?;
    let status = load_git_status_for_path(&workdir)?;
    Ok(GitRepoActionResponse {
        summary: build_git_push_summary(&status_before, &status),
        status,
    })
}

/// Syncs Git repo.
fn sync_git_repo(workdir: &FsPath) -> Result<GitRepoActionResponse, ApiError> {
    let workdir = normalize_git_workdir_path(workdir)?;
    let Some(repo_root) = resolve_git_repo_root(&workdir)? else {
        return Err(ApiError::bad_request("no git repository found"));
    };

    let status_before = load_git_status_for_path(&workdir)?;
    if status_before.upstream.is_none() {
        return Err(ApiError::bad_request(
            "git sync requires a tracking upstream branch",
        ));
    }

    run_git_repo_command(
        &repo_root,
        &["pull", "--ff-only"],
        "failed to pull git changes",
    )?;
    run_git_repo_command(&repo_root, &["push"], "failed to push git changes")?;
    let status = load_git_status_for_path(&workdir)?;
    Ok(GitRepoActionResponse {
        summary: build_git_sync_summary(&status_before, &status),
        status,
    })
}

/// Runs Git repo command.
fn run_git_repo_command(
    repo_root: &FsPath,
    args: &[&str],
    error_context: &str,
) -> Result<(), ApiError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .map_err(|err| ApiError::internal(format!("{error_context}: {err}")))?;

    if output.status.success() {
        return Ok(());
    }

    let detail = extract_git_command_error(&output);
    if detail.is_empty() {
        Err(ApiError::bad_request(error_context))
    } else {
        Err(ApiError::bad_request(format!("{error_context}: {detail}")))
    }
}

/// Builds Git push summary.
fn build_git_push_summary(
    status_before: &GitStatusResponse,
    status_after: &GitStatusResponse,
) -> String {
    let branch = status_after
        .branch
        .as_deref()
        .or(status_before.branch.as_deref());
    let upstream = status_after
        .upstream
        .as_deref()
        .or(status_before.upstream.as_deref());
    build_git_repo_action_summary("Pushed", branch, upstream)
}

/// Builds Git sync summary.
fn build_git_sync_summary(
    status_before: &GitStatusResponse,
    status_after: &GitStatusResponse,
) -> String {
    let branch = status_after
        .branch
        .as_deref()
        .or(status_before.branch.as_deref());
    let upstream = status_after
        .upstream
        .as_deref()
        .or(status_before.upstream.as_deref());
    build_git_repo_action_summary("Synced", branch, upstream)
}

/// Builds Git repo action summary.
fn build_git_repo_action_summary(
    verb: &str,
    branch: Option<&str>,
    upstream: Option<&str>,
) -> String {
    match (branch, upstream) {
        (Some(branch), Some(upstream)) => format!("{verb} {branch} with {upstream}."),
        (Some(branch), None) => format!("{verb} {branch}."),
        _ => format!("{verb} git repository."),
    }
}
/// Handles stable text hash.
fn stable_text_hash(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn empty_state_events_response() -> StateResponse {
    StateResponse {
        revision: 0,
        codex: CodexState::default(),
        agent_readiness: Vec::new(),
        preferences: AppPreferences::default(),
        projects: Vec::new(),
        orchestrators: Vec::new(),
        workspaces: Vec::new(),
        sessions: Vec::new(),
    }
}

#[derive(Deserialize)]
struct StateEventPayload {
    #[serde(default, rename = "_sseFallback")]
    sse_fallback: bool,
    #[serde(flatten)]
    state: StateResponse,
}

#[derive(Serialize)]
struct FallbackStateEventPayload {
    #[serde(rename = "_sseFallback")]
    sse_fallback: bool,
    #[serde(flatten)]
    state: StateResponse,
}

fn fallback_state_events_response(revision: u64) -> FallbackStateEventPayload {
    let mut state = empty_state_events_response();
    state.revision = revision;
    FallbackStateEventPayload {
        sse_fallback: true,
        state,
    }
}

fn fallback_state_events_payload(revision: u64) -> Result<String, ApiError> {
    serde_json::to_string(&fallback_state_events_response(revision)).map_err(|err| {
        ApiError::internal(format!(
            "failed to serialize fallback SSE state snapshot: {err}"
        ))
    })
}

static EMPTY_STATE_EVENTS_PAYLOAD: LazyLock<String> = LazyLock::new(|| {
    fallback_state_events_payload(0).expect("empty SSE state payload should serialize")
});

/// Serializes a full state snapshot for SSE on the blocking pool because snapshot()
/// acquires the synchronous app-state mutex.
async fn state_snapshot_payload_for_sse(state: AppState) -> String {
    run_blocking_api(move || {
        let snapshot = state.snapshot();
        match serde_json::to_string(&snapshot) {
            Ok(payload) => Ok(payload),
            Err(err) => {
                eprintln!(
                    "state events warning> failed to serialize SSE state snapshot at revision {}: {}",
                    snapshot.revision,
                    err
                );
                fallback_state_events_payload(snapshot.revision)
            }
        }
    })
    .await
    .unwrap_or_else(|err| {
        eprintln!(
            "state events warning> failed to build SSE fallback state snapshot: {}",
            err.message
        );
        EMPTY_STATE_EVENTS_PAYLOAD.clone()
    })
}

/// Streams state and delta events over SSE.
async fn state_events(
    State(state): State<AppState>,
) -> Sse<impl futures_core::Stream<Item = std::result::Result<Event, Infallible>>> {
    let mut state_receiver = state.subscribe_events();
    let mut delta_receiver = state.subscribe_delta_events();
    let initial_payload = state_snapshot_payload_for_sse(state.clone()).await;

    let stream = async_stream::stream! {
        yield Ok(Event::default().event("state").data(initial_payload));

        loop {
            tokio::select! {
                biased;

                result = state_receiver.recv() => {
                    match result {
                        Ok(payload) => yield Ok(Event::default().event("state").data(payload)),
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            let payload = state_snapshot_payload_for_sse(state.clone()).await;
                            yield Ok(Event::default().event("state").data(payload));
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }

                result = delta_receiver.recv() => {
                    match result {
                        Ok(payload) => yield Ok(Event::default().event("delta").data(payload)),
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            let payload = state_snapshot_payload_for_sse(state.clone()).await;
                            yield Ok(Event::default().event("state").data(payload));
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Creates session.
async fn create_session(
    State(state): State<AppState>,
    Json(request): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<CreateSessionResponse>), ApiError> {
    let response = run_blocking_api(move || state.create_session(request)).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// Creates project.
async fn create_project(
    State(state): State<AppState>,
    Json(request): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<CreateProjectResponse>), ApiError> {
    let response = run_blocking_api(move || state.create_project(request)).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// Gets project digest.
async fn get_project_digest(
    AxumPath(project_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<ProjectDigestResponse>, ApiError> {
    let response = run_blocking_api(move || state.project_digest(&project_id)).await?;
    Ok(Json(response))
}

/// Dispatches project action.
async fn dispatch_project_action(
    AxumPath((project_id, action_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<ProjectDigestResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.execute_project_action(&project_id, &action_id)).await?;
    Ok(Json(response))
}

/// Updates app settings.
async fn update_app_settings(
    State(state): State<AppState>,
    Json(request): Json<UpdateAppSettingsRequest>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || state.update_app_settings(request)).await?;
    Ok(Json(response))
}

/// Picks project root.
async fn pick_project_root(
    State(state): State<AppState>,
) -> Result<Json<PickProjectRootResponse>, ApiError> {
    let default_workdir = state.default_workdir.clone();
    let path = tokio::task::spawn_blocking(move || pick_project_root_path(&default_workdir))
        .await
        .map_err(|err| ApiError::internal(format!("folder picker task failed: {err}")))??;
    Ok(Json(PickProjectRootResponse { path }))
}

/// Updates session settings.
async fn update_session_settings(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    Json(request): Json<UpdateSessionSettingsRequest>,
) -> Result<Json<StateResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.update_session_settings(&session_id, request)).await?;
    Ok(Json(response))
}

/// Refreshes session model options.
async fn refresh_session_model_options(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.refresh_session_model_options(&session_id)).await?;
    Ok(Json(response))
}

/// Forks Codex thread.
async fn fork_codex_thread(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<(StatusCode, Json<CreateSessionResponse>), ApiError> {
    let response = run_blocking_api(move || state.fork_codex_thread(&session_id)).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// Archives Codex thread.
async fn archive_codex_thread(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || state.archive_codex_thread(&session_id)).await?;
    Ok(Json(response))
}

/// Unarchives Codex thread.
async fn unarchive_codex_thread(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || state.unarchive_codex_thread(&session_id)).await?;
    Ok(Json(response))
}

/// Compacts Codex thread.
async fn compact_codex_thread(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || state.compact_codex_thread(&session_id)).await?;
    Ok(Json(response))
}

/// Rolls back Codex thread.
async fn rollback_codex_thread(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    Json(request): Json<CodexThreadRollbackRequest>,
) -> Result<Json<StateResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.rollback_codex_thread(&session_id, request.num_turns))
            .await?;
    Ok(Json(response))
}

/// Handles send message.
async fn send_message(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    Json(request): Json<SendMessageRequest>,
) -> Result<(StatusCode, Json<StateResponse>), ApiError> {
    let dispatch = run_blocking_api({
        let state = state.clone();
        let session_id = session_id.clone();
        move || state.dispatch_turn(&session_id, request)
    })
    .await?;

    if let DispatchTurnResult::Dispatched(dispatch) = dispatch {
        deliver_turn_dispatch(&state, dispatch)?;
    }

    let snapshot = run_blocking_api({
        let state = state.clone();
        move || Ok(state.snapshot())
    })
    .await?;

    Ok((StatusCode::ACCEPTED, Json(snapshot)))
}

/// Cancels queued prompt.
async fn cancel_queued_prompt(
    AxumPath((session_id, prompt_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.cancel_queued_prompt(&session_id, &prompt_id)).await?;
    Ok(Json(response))
}

/// Stops session.
async fn stop_session(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || state.stop_session(&session_id)).await?;
    Ok(Json(response))
}

/// Kills session.
async fn kill_session(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || state.kill_session(&session_id)).await?;
    Ok(Json(response))
}

/// Submits approval.
async fn submit_approval(
    AxumPath((session_id, message_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<ApprovalRequest>,
) -> Result<Json<StateResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.update_approval(&session_id, &message_id, request.decision))
            .await?;
    Ok(Json(response))
}

/// Submits user input.
async fn submit_user_input(
    AxumPath((session_id, message_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<UserInputSubmissionRequest>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || {
        state.submit_codex_user_input(&session_id, &message_id, request.answers)
    })
    .await?;
    Ok(Json(response))
}

/// Submits MCP elicitation.
async fn submit_mcp_elicitation(
    AxumPath((session_id, message_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<McpElicitationSubmissionRequest>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || {
        state.submit_codex_mcp_elicitation(
            &session_id,
            &message_id,
            request.action,
            request.content,
        )
    })
    .await?;
    Ok(Json(response))
}

/// Submits Codex app request.
async fn submit_codex_app_request(
    AxumPath((session_id, message_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<CodexAppRequestSubmissionRequest>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || {
        state.submit_codex_app_request(&session_id, &message_id, request.result)
    })
    .await?;
    Ok(Json(response))
}

/// Represents the API error.
#[derive(Debug)]
struct ApiError {
    message: String,
    status: StatusCode,
}

impl ApiError {
    /// Handles bad request.
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::BAD_REQUEST,
        }
    }

    /// Handles conflict.
    fn conflict(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::CONFLICT,
        }
    }

    /// Handles not found.
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::NOT_FOUND,
        }
    }

    /// Handles internal.
    fn internal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Handles bad gateway.
    fn bad_gateway(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::BAD_GATEWAY,
        }
    }

    /// Builds the value from status.
    fn from_status(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status,
        }
    }
}

impl IntoResponse for ApiError {
    /// Converts the value into response.
    fn into_response(self) -> Response {
        let body = Json(ErrorResponse {
            error: self.message,
        });
        (self.status, body).into_response()
    }
}

/// Resolves review storage root.
fn resolve_review_storage_root(
    state: &AppState,
    session_id: Option<&str>,
    project_id: Option<&str>,
) -> Result<PathBuf, ApiError> {
    resolve_request_project_root_path(state, session_id, project_id)
}

/// Resolves review document path.
fn resolve_review_document_path(
    review_root: &FsPath,
    change_set_id: &str,
) -> Result<PathBuf, ApiError> {
    validate_review_change_set_id(change_set_id)?;
    Ok(review_root
        .join(".termal")
        .join("reviews")
        .join(format!("{change_set_id}.json")))
}

/// Loads review document.
fn load_review_document(path: &FsPath, change_set_id: &str) -> Result<ReviewDocument, ApiError> {
    if !path.exists() {
        return Ok(default_review_document(change_set_id));
    }

    let raw = fs::read(path).map_err(|err| {
        ApiError::internal(format!(
            "failed to read review file {}: {err}",
            path.display()
        ))
    })?;
    let review: ReviewDocument = serde_json::from_slice(&raw).map_err(|err| {
        ApiError::internal(format!(
            "failed to parse review file {}: {err}",
            path.display()
        ))
    })?;
    validate_review_document(change_set_id, &review)?;
    Ok(review)
}

/// Persists review document.
fn persist_review_document(path: &FsPath, review: &ReviewDocument) -> Result<(), ApiError> {
    persist_review_document_with_directory_sync(path, review, sync_review_document_directory)
}

fn persist_review_document_with_directory_sync<F>(
    path: &FsPath,
    review: &ReviewDocument,
    sync_directory: F,
) -> Result<(), ApiError>
where
    F: FnOnce(&FsPath) -> Result<(), ApiError>,
{
    let parent = path.parent().ok_or_else(|| {
        ApiError::internal(format!(
            "review file {} has no parent directory",
            path.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(|err| {
        ApiError::internal(format!(
            "failed to create review directory {}: {err}",
            parent.display()
        ))
    })?;

    let encoded = serde_json::to_vec_pretty(review)
        .map_err(|err| ApiError::internal(format!("failed to serialize review document: {err}")))?;
    let file_name = path.file_name().ok_or_else(|| {
        ApiError::internal(format!(
            "review file {} is missing a file name",
            path.display()
        ))
    })?;
    let temp_path = parent.join(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        Uuid::new_v4()
    ));

    let write_result = (|| {
        let mut temp_file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to create temp review file {}: {err}",
                    temp_path.display()
                ))
            })?;
        temp_file.write_all(&encoded).map_err(|err| {
            ApiError::internal(format!(
                "failed to write temp review file {}: {err}",
                temp_path.display()
            ))
        })?;
        temp_file.sync_all().map_err(|err| {
            ApiError::internal(format!(
                "failed to flush temp review file {}: {err}",
                temp_path.display()
            ))
        })?;
        drop(temp_file);

        replace_review_document_file(&temp_path, path)?;
        Ok(())
    })();

    if let Err(err) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(err);
    }

    if let Err(err) = sync_directory(parent) {
        eprintln!(
            "review warning> review file {} replaced but parent directory sync failed: {}",
            path.display(),
            err.message
        );
    }

    Ok(())
}

#[cfg(windows)]
const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;

#[cfg(windows)]
const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

#[cfg(windows)]
#[link(name = "Kernel32")]
unsafe extern "system" {
    fn MoveFileExW(existing_file_name: *const u16, new_file_name: *const u16, flags: u32) -> i32;
}

#[cfg(not(windows))]
fn replace_review_document_file(temp_path: &FsPath, path: &FsPath) -> Result<(), ApiError> {
    fs::rename(temp_path, path).map_err(|err| {
        ApiError::internal(format!(
            "failed to replace review file {}: {err}",
            path.display()
        ))
    })
}

#[cfg(windows)]
fn replace_review_document_file(temp_path: &FsPath, path: &FsPath) -> Result<(), ApiError> {
    use std::os::windows::ffi::OsStrExt as _;

    let temp_path_wide = temp_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let path_wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();

    let moved = unsafe {
        MoveFileExW(
            temp_path_wide.as_ptr(),
            path_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(ApiError::internal(format!(
            "failed to replace review file {}: {}",
            path.display(),
            io::Error::last_os_error()
        )));
    }

    Ok(())
}

#[cfg(not(windows))]
fn sync_review_document_directory(path: &FsPath) -> Result<(), ApiError> {
    let directory = fs::File::open(path).map_err(|err| {
        ApiError::internal(format!(
            "failed to open review directory {} for sync: {err}",
            path.display()
        ))
    })?;
    directory.sync_all().map_err(|err| {
        ApiError::internal(format!(
            "failed to flush review directory {}: {err}",
            path.display()
        ))
    })
}

#[cfg(windows)]
fn sync_review_document_directory(_path: &FsPath) -> Result<(), ApiError> {
    Ok(())
}

/// Prepares review document for write.
fn prepare_review_document_for_write(
    path: &FsPath,
    change_set_id: &str,
    review: ReviewDocument,
) -> Result<ReviewDocument, ApiError> {
    validate_review_document(change_set_id, &review)?;
    let current = load_review_document(path, change_set_id)?;
    if review.revision != current.revision {
        return Err(ApiError::conflict(format!(
            "review document is out of date: expected revision {}, got {}",
            current.revision, review.revision
        )));
    }

    let next_revision = current
        .revision
        .checked_add(1)
        .ok_or_else(|| ApiError::internal("review revision overflow"))?;
    let mut next = review;
    next.revision = next_revision;
    Ok(next)
}

/// Returns the default review document.
fn default_review_document(change_set_id: &str) -> ReviewDocument {
    ReviewDocument {
        version: REVIEW_DOCUMENT_VERSION,
        revision: 0,
        change_set_id: change_set_id.to_owned(),
        origin: None,
        files: Vec::new(),
        threads: Vec::new(),
    }
}

/// Summarizes review document.
fn summarize_review_document(review: &ReviewDocument) -> ReviewDocumentSummary {
    let mut summary = ReviewDocumentSummary::default();
    summary.thread_count = review.threads.len();

    for thread in &review.threads {
        summary.comment_count += thread.comments.len();
        match thread.status {
            ReviewThreadStatus::Open => summary.open_thread_count += 1,
            ReviewThreadStatus::Resolved => summary.resolved_thread_count += 1,
            ReviewThreadStatus::Applied | ReviewThreadStatus::Dismissed => {}
        }
    }

    summary
}

const MAX_REVIEW_CHANGE_SET_ID_LEN: usize = 200;

/// Validates review change set ID.
fn validate_review_change_set_id(change_set_id: &str) -> Result<(), ApiError> {
    let trimmed = change_set_id.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request("changeSetId cannot be empty"));
    }

    if trimmed != change_set_id {
        return Err(ApiError::bad_request(
            "changeSetId may not have leading or trailing whitespace",
        ));
    }

    if change_set_id.len() > MAX_REVIEW_CHANGE_SET_ID_LEN {
        return Err(ApiError::bad_request(format!(
            "changeSetId is too long (max {MAX_REVIEW_CHANGE_SET_ID_LEN} bytes)"
        )));
    }

    if change_set_id.chars().all(|character| character == '.') {
        return Err(ApiError::bad_request(
            "changeSetId must not consist entirely of dots",
        ));
    }

    if change_set_id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Ok(());
    }

    Err(ApiError::bad_request(
        "changeSetId may only contain letters, numbers, '.', '-', and '_'",
    ))
}

/// Validates review document.
fn validate_review_document(change_set_id: &str, review: &ReviewDocument) -> Result<(), ApiError> {
    if review.version != REVIEW_DOCUMENT_VERSION {
        return Err(ApiError::bad_request(format!(
            "unsupported review document version {}",
            review.version
        )));
    }

    if review.change_set_id != change_set_id {
        return Err(ApiError::bad_request(format!(
            "review changeSetId `{}` does not match route `{change_set_id}`",
            review.change_set_id
        )));
    }

    if let Some(origin) = review.origin.as_ref() {
        validate_non_empty_review_field("origin.sessionId", &origin.session_id)?;
        validate_non_empty_review_field("origin.messageId", &origin.message_id)?;
        validate_non_empty_review_field("origin.agent", &origin.agent)?;
        validate_non_empty_review_field("origin.workdir", &origin.workdir)?;
        validate_non_empty_review_field("origin.createdAt", &origin.created_at)?;
    }

    for file in &review.files {
        validate_non_empty_review_field("files[].filePath", &file.file_path)?;
    }

    for thread in &review.threads {
        validate_review_thread(thread)?;
    }

    Ok(())
}

/// Validates review thread.
fn validate_review_thread(thread: &ReviewThread) -> Result<(), ApiError> {
    validate_non_empty_review_field("threads[].id", &thread.id)?;
    validate_review_anchor(&thread.anchor)?;

    if thread.comments.is_empty() {
        return Err(ApiError::bad_request(
            "review threads must contain at least one comment",
        ));
    }

    for comment in &thread.comments {
        validate_non_empty_review_field("threads[].comments[].id", &comment.id)?;
        validate_non_empty_review_field("threads[].comments[].body", &comment.body)?;
        validate_non_empty_review_field("threads[].comments[].createdAt", &comment.created_at)?;
        validate_non_empty_review_field("threads[].comments[].updatedAt", &comment.updated_at)?;
    }

    Ok(())
}

/// Validates review anchor.
fn validate_review_anchor(anchor: &ReviewAnchor) -> Result<(), ApiError> {
    match anchor {
        ReviewAnchor::ChangeSet => Ok(()),
        ReviewAnchor::File { file_path } => {
            validate_non_empty_review_field("comments[].anchor.filePath", file_path)
        }
        ReviewAnchor::Hunk {
            file_path,
            hunk_header,
        } => {
            validate_non_empty_review_field("comments[].anchor.filePath", file_path)?;
            validate_non_empty_review_field("comments[].anchor.hunkHeader", hunk_header)
        }
        ReviewAnchor::Line {
            file_path,
            hunk_header,
            old_line,
            new_line,
        } => {
            validate_non_empty_review_field("comments[].anchor.filePath", file_path)?;
            validate_non_empty_review_field("comments[].anchor.hunkHeader", hunk_header)?;
            if old_line.is_none() && new_line.is_none() {
                return Err(ApiError::bad_request(
                    "line review anchors must include oldLine, newLine, or both",
                ));
            }
            Ok(())
        }
    }
}

/// Validates non empty review field.
fn validate_non_empty_review_field(label: &str, value: &str) -> Result<(), ApiError> {
    if value.trim().is_empty() {
        return Err(ApiError::bad_request(format!("{label} cannot be empty")));
    }

    Ok(())
}

/// Defines the agent variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
enum Agent {
    Codex,
    Claude,
    Cursor,
    Gemini,
}

impl Agent {
    /// Handles parse.
    fn parse(args: impl Iterator<Item = String>) -> Result<Self> {
        let mut args = args;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--agent" => {
                    let value = args.next().context("missing value after `--agent`")?;
                    return Self::from_str(&value);
                }
                "codex" => return Ok(Self::Codex),
                "claude" => return Ok(Self::Claude),
                "cursor" | "cursor-agent" => return Ok(Self::Cursor),
                "gemini" | "gemini-cli" => return Ok(Self::Gemini),
                other => bail!("unknown argument `{other}`"),
            }
        }

        Ok(Self::Codex)
    }

    /// Builds the value from str.
    fn from_str(value: &str) -> Result<Self> {
        match value {
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            "cursor" | "cursor-agent" => Ok(Self::Cursor),
            "gemini" | "gemini-cli" => Ok(Self::Gemini),
            other => {
                bail!("unknown agent `{other}`; expected `codex`, `claude`, `cursor`, or `gemini`")
            }
        }
    }

    /// Handles name.
    fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::Cursor => "Cursor",
            Self::Gemini => "Gemini",
        }
    }

    /// Handles avatar.
    fn avatar(self) -> &'static str {
        match self {
            Self::Codex => "CX",
            Self::Claude => "CL",
            Self::Cursor => "CR",
            Self::Gemini => "GM",
        }
    }

    /// Returns the default model.
    fn default_model(self) -> &'static str {
        match self {
            Self::Codex => "gpt-5.4",
            Self::Claude => "sonnet",
            Self::Cursor => "auto",
            Self::Gemini => "auto",
        }
    }

    /// Returns whether Codex prompt settings.
    fn supports_codex_prompt_settings(self) -> bool {
        matches!(self, Self::Codex)
    }

    /// Returns whether Claude approval mode.
    fn supports_claude_approval_mode(self) -> bool {
        matches!(self, Self::Claude)
    }

    /// Returns whether cursor mode.
    fn supports_cursor_mode(self) -> bool {
        matches!(self, Self::Cursor)
    }

    /// Returns whether Gemini approval mode.
    fn supports_gemini_approval_mode(self) -> bool {
        matches!(self, Self::Gemini)
    }

    /// Handles ACP runtime.
    fn acp_runtime(self) -> Option<AcpAgent> {
        match self {
            Self::Cursor => Some(AcpAgent::Cursor),
            Self::Gemini => Some(AcpAgent::Gemini),
            _ => None,
        }
    }
}

/// Represents project.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    id: String,
    name: String,
    root_path: String,
    remote_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_project_id: Option<String>,
}

/// Represents session.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    id: String,
    name: String,
    emoji: String,
    agent: Agent,
    workdir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_id: Option<String>,
    model: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    model_options: Vec<SessionModelOption>,
    approval_policy: Option<CodexApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<CodexReasoningEffort>,
    sandbox_mode: Option<CodexSandboxMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor_mode: Option<CursorMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claude_effort: Option<ClaudeEffortLevel>,
    claude_approval_mode: Option<ClaudeApprovalMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gemini_approval_mode: Option<GeminiApprovalMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    external_session_id: Option<String>,
    #[serde(default)]
    agent_commands_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    codex_thread_state: Option<CodexThreadState>,
    status: SessionStatus,
    preview: String,
    messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_prompts: Vec<PendingPrompt>,
}

/// Tracks Codex thread state.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum CodexThreadState {
    Active,
    Archived,
}

/// Defines the Codex approval policy variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CodexApprovalPolicy {
    Untrusted,
    OnFailure,
    OnRequest,
    Never,
}

impl CodexApprovalPolicy {
    /// Returns the cli value representation.
    fn as_cli_value(self) -> &'static str {
        match self {
            Self::Untrusted => "untrusted",
            Self::OnFailure => "on-failure",
            Self::OnRequest => "on-request",
            Self::Never => "never",
        }
    }
}

/// Enumerates Codex sandbox modes.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CodexSandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl CodexSandboxMode {
    /// Returns the cli value representation.
    fn as_cli_value(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }
}

/// Defines the Codex reasoning effort variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CodexReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
}

impl CodexReasoningEffort {
    /// Returns the API value representation.
    fn as_api_value(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
        }
    }
}

/// Enumerates Claude approval modes.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ClaudeApprovalMode {
    Ask,
    AutoApprove,
    Plan,
}

/// Defines the Claude effort level variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ClaudeEffortLevel {
    Default,
    Low,
    Medium,
    High,
    Max,
}

impl ClaudeEffortLevel {
    /// Returns the cli value representation.
    fn as_cli_value(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::Max => Some("max"),
        }
    }
}

impl ClaudeApprovalMode {
    /// Handles initial cli permission mode.
    fn initial_cli_permission_mode(self) -> Option<&'static str> {
        match self {
            Self::Plan => Some("plan"),
            Self::Ask | Self::AutoApprove => None,
        }
    }

    /// Handles session cli permission mode.
    fn session_cli_permission_mode(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Ask | Self::AutoApprove => "default",
        }
    }
}

/// Enumerates cursor modes.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum CursorMode {
    Agent,
    Plan,
    Ask,
}

impl CursorMode {
    /// Returns the ACP value representation.
    fn as_acp_value(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Plan => "plan",
            Self::Ask => "ask",
        }
    }
}

/// Enumerates Gemini approval modes.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum GeminiApprovalMode {
    Default,
    AutoEdit,
    Yolo,
    Plan,
}

impl GeminiApprovalMode {
    /// Returns the cli value representation.
    fn as_cli_value(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AutoEdit => "auto_edit",
            Self::Yolo => "yolo",
            Self::Plan => "plan",
        }
    }
}

/// Enumerates session states.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum SessionStatus {
    Active,
    Idle,
    Approval,
    Error,
}

/// Defines the author variants.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Author {
    You,
    Assistant,
}

/// Enumerates command states.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum CommandStatus {
    Running,
    Success,
    Error,
}

impl CommandStatus {
    /// Handles label.
    fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Success => "success",
            Self::Error => "error",
        }
    }
}

/// Defines the change type variants.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum ChangeType {
    Edit,
    Create,
}

/// Enumerates approval decisions.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum ApprovalDecision {
    Pending,
    Interrupted,
    Canceled,
    Accepted,
    AcceptedForSession,
    Rejected,
}

/// Enumerates parallel agent states.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum ParallelAgentStatus {
    Initializing,
    Running,
    Completed,
    Error,
}

/// Represents user input question option.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct UserInputQuestionOption {
    description: String,
    label: String,
}

/// Represents user input question.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct UserInputQuestion {
    header: String,
    id: String,
    #[serde(default, rename = "isOther")]
    is_other: bool,
    #[serde(default, rename = "isSecret")]
    is_secret: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    options: Option<Vec<UserInputQuestionOption>>,
    question: String,
}

/// Enumerates MCP elicitation actions.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum McpElicitationAction {
    Accept,
    Decline,
    Cancel,
}

/// Enumerates MCP elicitation request modes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "mode")]
enum McpElicitationRequestMode {
    Form {
        #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
        meta: Option<Value>,
        message: String,
        #[serde(rename = "requestedSchema")]
        requested_schema: Value,
    },
    Url {
        #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
        meta: Option<Value>,
        #[serde(rename = "elicitationId")]
        elicitation_id: String,
        message: String,
        url: String,
    },
}

/// Represents the MCP elicitation request payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct McpElicitationRequestPayload {
    #[serde(rename = "threadId")]
    thread_id: String,
    #[serde(default, rename = "turnId", skip_serializing_if = "Option::is_none")]
    turn_id: Option<String>,
    server_name: String,
    #[serde(flatten)]
    mode: McpElicitationRequestMode,
}

/// Tracks interaction request state.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum InteractionRequestState {
    Pending,
    Submitted,
    Interrupted,
    Canceled,
}

/// Represents parallel agent progress.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ParallelAgentProgress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    id: String,
    status: ParallelAgentStatus,
    title: String,
}

/// Represents message image attachment.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MessageImageAttachment {
    byte_size: usize,
    file_name: String,
    media_type: String,
}

/// Represents pending prompt.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingPrompt {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<MessageImageAttachment>,
    id: String,
    timestamp: String,
    text: String,
    #[serde(
        default,
        rename = "expandedText",
        skip_serializing_if = "Option::is_none"
    )]
    expanded_text: Option<String>,
}

/// Defines the message variants.
#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Message {
    Text {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<MessageImageAttachment>,
        id: String,
        timestamp: String,
        author: Author,
        text: String,
        #[serde(
            default,
            rename = "expandedText",
            skip_serializing_if = "Option::is_none"
        )]
        expanded_text: Option<String>,
    },
    Thinking {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        lines: Vec<String>,
    },
    Command {
        id: String,
        timestamp: String,
        author: Author,
        command: String,
        #[serde(
            default,
            rename = "commandLanguage",
            skip_serializing_if = "Option::is_none"
        )]
        command_language: Option<String>,
        output: String,
        #[serde(
            default,
            rename = "outputLanguage",
            skip_serializing_if = "Option::is_none"
        )]
        output_language: Option<String>,
        status: CommandStatus,
    },
    Diff {
        id: String,
        timestamp: String,
        author: Author,
        #[serde(
            default,
            rename = "changeSetId",
            skip_serializing_if = "Option::is_none"
        )]
        change_set_id: Option<String>,
        #[serde(rename = "filePath")]
        file_path: String,
        summary: String,
        diff: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        #[serde(rename = "changeType")]
        change_type: ChangeType,
    },
    Markdown {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        markdown: String,
    },
    #[serde(rename = "subagentResult")]
    SubagentResult {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        summary: String,
        #[serde(
            default,
            rename = "conversationId",
            skip_serializing_if = "Option::is_none"
        )]
        conversation_id: Option<String>,
        #[serde(default, rename = "turnId", skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
    },
    #[serde(rename = "parallelAgents")]
    ParallelAgents {
        id: String,
        timestamp: String,
        author: Author,
        agents: Vec<ParallelAgentProgress>,
    },
    Approval {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        command: String,
        #[serde(
            default,
            rename = "commandLanguage",
            skip_serializing_if = "Option::is_none"
        )]
        command_language: Option<String>,
        detail: String,
        decision: ApprovalDecision,
    },
    #[serde(rename = "userInputRequest")]
    UserInputRequest {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        detail: String,
        questions: Vec<UserInputQuestion>,
        state: InteractionRequestState,
        #[serde(
            default,
            rename = "submittedAnswers",
            skip_serializing_if = "Option::is_none"
        )]
        submitted_answers: Option<BTreeMap<String, Vec<String>>>,
    },
    #[serde(rename = "mcpElicitationRequest")]
    McpElicitationRequest {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        detail: String,
        request: McpElicitationRequestPayload,
        state: InteractionRequestState,
        #[serde(
            default,
            rename = "submittedAction",
            skip_serializing_if = "Option::is_none"
        )]
        submitted_action: Option<McpElicitationAction>,
        #[serde(
            default,
            rename = "submittedContent",
            skip_serializing_if = "Option::is_none"
        )]
        submitted_content: Option<Value>,
    },
    #[serde(rename = "codexAppRequest")]
    CodexAppRequest {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        detail: String,
        method: String,
        params: Value,
        state: InteractionRequestState,
        #[serde(
            default,
            rename = "submittedResult",
            skip_serializing_if = "Option::is_none"
        )]
        submitted_result: Option<Value>,
    },
}

impl Message {
    /// Handles ID.
    fn id(&self) -> &str {
        match self {
            Self::Text { id, .. }
            | Self::Thinking { id, .. }
            | Self::Command { id, .. }
            | Self::Diff { id, .. }
            | Self::Markdown { id, .. }
            | Self::SubagentResult { id, .. }
            | Self::ParallelAgents { id, .. }
            | Self::Approval { id, .. }
            | Self::UserInputRequest { id, .. }
            | Self::McpElicitationRequest { id, .. }
            | Self::CodexAppRequest { id, .. } => id,
        }
    }

    /// Handles preview text.
    fn preview_text(&self) -> Option<String> {
        match self {
            Self::Text {
                text, attachments, ..
            } => Some(prompt_preview_text(text, attachments)),
            Self::Thinking { title, .. } => Some(make_preview(title)),
            Self::Markdown { title, .. } => Some(make_preview(title)),
            Self::Approval { title, .. } => Some(make_preview(title)),
            Self::UserInputRequest { title, .. } => Some(make_preview(title)),
            Self::McpElicitationRequest { title, .. } => Some(make_preview(title)),
            Self::CodexAppRequest { title, .. } => Some(make_preview(title)),
            Self::Diff { summary, .. } => Some(make_preview(summary)),
            Self::SubagentResult { .. } => None,
            Self::ParallelAgents { agents, .. } => Some(parallel_agents_preview_text(agents)),
            Self::Command { .. } => None,
        }
    }
}

/// Handles parallel agents preview text.
fn parallel_agents_preview_text(agents: &[ParallelAgentProgress]) -> String {
    let count = agents.len();
    let label = if count == 1 { "agent" } else { "agents" };
    let active_count = agents
        .iter()
        .filter(|agent| {
            matches!(
                agent.status,
                ParallelAgentStatus::Initializing | ParallelAgentStatus::Running
            )
        })
        .count();

    if active_count > 0 {
        return make_preview(&format!("Running {count} {label}"));
    }

    let error_count = agents
        .iter()
        .filter(|agent| agent.status == ParallelAgentStatus::Error)
        .count();
    if error_count > 0 {
        let errors = if error_count == 1 { "error" } else { "errors" };
        return make_preview(&format!(
            "{count} {label} finished with {error_count} {errors}"
        ));
    }

    make_preview(&format!("Completed {count} {label}"))
}

/// Represents the approval request payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalRequest {
    decision: ApprovalDecision,
}

/// Represents the user input submission request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserInputSubmissionRequest {
    answers: BTreeMap<String, Vec<String>>,
}

/// Represents the MCP elicitation submission request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpElicitationSubmissionRequest {
    action: McpElicitationAction,
    #[serde(default)]
    content: Option<Value>,
}

/// Represents the Codex app request submission request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexAppRequestSubmissionRequest {
    result: Value,
}

/// Represents the create session request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionRequest {
    agent: Option<Agent>,
    name: Option<String>,
    workdir: Option<String>,
    project_id: Option<String>,
    model: Option<String>,
    approval_policy: Option<CodexApprovalPolicy>,
    reasoning_effort: Option<CodexReasoningEffort>,
    sandbox_mode: Option<CodexSandboxMode>,
    cursor_mode: Option<CursorMode>,
    claude_approval_mode: Option<ClaudeApprovalMode>,
    claude_effort: Option<ClaudeEffortLevel>,
    gemini_approval_mode: Option<GeminiApprovalMode>,
}

/// Represents the create project request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateProjectRequest {
    name: Option<String>,
    root_path: String,
    #[serde(default = "default_local_remote_id")]
    remote_id: String,
}

/// Represents the update app settings request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateAppSettingsRequest {
    default_codex_reasoning_effort: Option<CodexReasoningEffort>,
    default_claude_effort: Option<ClaudeEffortLevel>,
    remotes: Option<Vec<RemoteConfig>>,
}

/// Represents file query.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileQuery {
    path: String,
    project_id: Option<String>,
    session_id: Option<String>,
}

/// Represents instruction search query.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstructionSearchQuery {
    q: String,
    session_id: String,
}

/// Represents review query.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewQuery {
    project_id: Option<String>,
    session_id: Option<String>,
}

/// Represents the Codex thread rollback request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexThreadRollbackRequest {
    #[serde(default = "default_codex_thread_rollback_turns")]
    num_turns: usize,
}

/// Returns the default Codex thread rollback turns.
fn default_codex_thread_rollback_turns() -> usize {
    1
}

/// Represents the write file request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteFileRequest {
    path: String,
    content: String,
    project_id: Option<String>,
    session_id: Option<String>,
}

/// Represents the file response payload.
#[derive(Deserialize, Serialize)]
struct FileResponse {
    path: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
}

/// Enumerates file system entry kinds.
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum FileSystemEntryKind {
    Directory,
    File,
}

/// Represents directory entry.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryEntry {
    kind: FileSystemEntryKind,
    name: String,
    path: String,
}

/// Represents the directory response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryResponse {
    entries: Vec<DirectoryEntry>,
    name: String,
    path: String,
}

/// Represents Git status file.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitStatusFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    index_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_path: Option<String>,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    worktree_status: Option<String>,
}

/// Represents the Git status response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitStatusResponse {
    ahead: usize,
    behind: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    files: Vec<GitStatusFile>,
    is_clean: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream: Option<String>,
    workdir: String,
}

/// Defines the Git diff section variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum GitDiffSection {
    Staged,
    Unstaged,
}

impl GitDiffSection {
    /// Returns the key representation.
    fn as_key(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Unstaged => "unstaged",
        }
    }

    /// Handles summary label.
    fn summary_label(self) -> &'static str {
        match self {
            Self::Staged => "Staged",
            Self::Unstaged => "Unstaged",
        }
    }
}

/// Enumerates Git file actions.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum GitFileAction {
    Stage,
    Unstage,
    Revert,
}

/// Represents the Git diff request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitDiffRequest {
    original_path: Option<String>,
    path: String,
    section_id: GitDiffSection,
    #[serde(default)]
    status_code: Option<String>,
    workdir: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

/// Defines the Git diff change type variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum GitDiffChangeType {
    Edit,
    Create,
}

/// Represents the Git diff response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitDiffResponse {
    change_type: GitDiffChangeType,
    change_set_id: String,
    diff: String,
    diff_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    summary: String,
}

/// Represents the Git commit response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitCommitResponse {
    status: GitStatusResponse,
    summary: String,
}

/// Represents the Git repo action response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitRepoActionResponse {
    status: GitStatusResponse,
    summary: String,
}

const REVIEW_DOCUMENT_VERSION: u32 = 1;

/// Represents the review document.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewDocument {
    version: u32,
    #[serde(default)]
    revision: u64,
    change_set_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<ReviewOrigin>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    files: Vec<ReviewFileEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    threads: Vec<ReviewThread>,
}

/// Represents review origin.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewOrigin {
    session_id: String,
    message_id: String,
    agent: String,
    workdir: String,
    created_at: String,
}

/// Represents review file entry.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewFileEntry {
    file_path: String,
    change_type: ChangeType,
}

/// Represents review thread.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewThread {
    id: String,
    anchor: ReviewAnchor,
    status: ReviewThreadStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    comments: Vec<ReviewThreadComment>,
}

/// Represents review thread comment.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewThreadComment {
    id: String,
    author: ReviewCommentAuthor,
    body: String,
    created_at: String,
    updated_at: String,
}

/// Defines the review anchor variants.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum ReviewAnchor {
    ChangeSet,
    File {
        file_path: String,
    },
    Hunk {
        file_path: String,
        hunk_header: String,
    },
    Line {
        file_path: String,
        hunk_header: String,
        old_line: Option<usize>,
        new_line: Option<usize>,
    },
}

/// Defines the review comment author variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum ReviewCommentAuthor {
    User,
    Agent,
}

/// Enumerates review thread states.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum ReviewThreadStatus {
    Open,
    Resolved,
    Applied,
    Dismissed,
}

/// Represents the review document response payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewDocumentResponse {
    review_file_path: String,
    review: ReviewDocument,
}

/// Represents the review summary response payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewSummaryResponse {
    change_set_id: String,
    review_file_path: String,
    thread_count: usize,
    open_thread_count: usize,
    resolved_thread_count: usize,
    comment_count: usize,
    has_threads: bool,
}

/// Summarizes review document.
#[derive(Default)]
struct ReviewDocumentSummary {
    thread_count: usize,
    open_thread_count: usize,
    resolved_thread_count: usize,
    comment_count: usize,
}

/// Represents the Git file action request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitFileActionRequest {
    action: GitFileAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_path: Option<String>,
    path: String,
    #[serde(default)]
    status_code: Option<String>,
    workdir: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

/// Represents the Git commit request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitCommitRequest {
    message: String,
    workdir: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

/// Represents the Git repo action request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitRepoActionRequest {
    workdir: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

/// Represents the error response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorResponse {
    error: String,
}

/// Represents the health response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    ok: bool,
    #[serde(default)]
    supports_inline_orchestrator_templates: bool,
}

/// Represents the send message request payload.
#[derive(Deserialize, Serialize)]
struct SendMessageRequest {
    text: String,
    #[serde(default, rename = "expandedText")]
    expanded_text: Option<String>,
    #[serde(default)]
    attachments: Vec<SendMessageAttachmentRequest>,
}

/// Represents the send message attachment request payload.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SendMessageAttachmentRequest {
    data: String,
    file_name: Option<String>,
    media_type: String,
}

/// Represents the update session settings request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateSessionSettingsRequest {
    name: Option<String>,
    model: Option<String>,
    approval_policy: Option<CodexApprovalPolicy>,
    reasoning_effort: Option<CodexReasoningEffort>,
    sandbox_mode: Option<CodexSandboxMode>,
    cursor_mode: Option<CursorMode>,
    claude_approval_mode: Option<ClaudeApprovalMode>,
    claude_effort: Option<ClaudeEffortLevel>,
    gemini_approval_mode: Option<GeminiApprovalMode>,
}

/// Represents session model option.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionModelOption {
    label: String,
    value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    badges: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    supported_claude_effort_levels: Vec<ClaudeEffortLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_reasoning_effort: Option<CodexReasoningEffort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    supported_reasoning_efforts: Vec<CodexReasoningEffort>,
}

impl SessionModelOption {
    /// Builds the plain response value.
    #[cfg(test)]
    fn plain(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            description: None,
            badges: Vec::new(),
            supported_claude_effort_levels: Vec::new(),
            default_reasoning_effort: None,
            supported_reasoning_efforts: Vec::new(),
        }
    }
}

/// Tracks Codex state.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rate_limits: Option<CodexRateLimits>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    notices: Vec<CodexNotice>,
}

impl CodexState {
    /// Returns whether empty.
    fn is_empty(&self) -> bool {
        self.rate_limits.is_none() && self.notices.is_empty()
    }
}

/// Enumerates Codex notice kinds.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum CodexNoticeKind {
    ConfigWarning,
    DeprecationNotice,
    RuntimeNotice,
}

/// Defines the Codex notice level variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum CodexNoticeLevel {
    Info,
    Warning,
}

/// Represents Codex notice.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexNotice {
    kind: CodexNoticeKind,
    level: CodexNoticeLevel,
    title: String,
    detail: String,
    timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    code: Option<String>,
}

/// Represents Codex rate limits.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexRateLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credits: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    primary: Option<CodexRateLimitWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secondary: Option<CodexRateLimitWindow>,
}

/// Represents Codex rate limit window.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexRateLimitWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resets_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    used_percent: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window_duration_mins: Option<u64>,
}

/// Enumerates agent readiness states.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum AgentReadinessStatus {
    Ready,
    Missing,
    NeedsSetup,
}

/// Represents agent readiness.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentReadiness {
    agent: Agent,
    status: AgentReadinessStatus,
    blocking: bool,
    detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    warning_detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command_path: Option<String>,
}

/// Represents the state response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StateResponse {
    revision: u64,
    #[serde(default)]
    codex: CodexState,
    #[serde(default)]
    agent_readiness: Vec<AgentReadiness>,
    preferences: AppPreferences,
    #[serde(default)]
    projects: Vec<Project>,
    #[serde(default)]
    orchestrators: Vec<OrchestratorInstance>,
    #[serde(default)]
    workspaces: Vec<WorkspaceLayoutSummary>,
    #[serde(default)]
    sessions: Vec<Session>,
}

/// Defines the workspace control panel side variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum WorkspaceControlPanelSide {
    Left,
    Right,
}

/// Represents the workspace layout document.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceLayoutDocument {
    id: String,
    revision: u64,
    updated_at: String,
    control_panel_side: WorkspaceControlPanelSide,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    theme_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    style_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    font_size_px: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    editor_font_size_px: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    density_percent: Option<u32>,
    workspace: Value,
}

/// Represents the workspace layout response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceLayoutResponse {
    layout: WorkspaceLayoutDocument,
}

/// Summarizes workspace layout.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceLayoutSummary {
    id: String,
    revision: u64,
    updated_at: String,
    control_panel_side: WorkspaceControlPanelSide,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    theme_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    style_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    font_size_px: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    editor_font_size_px: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    density_percent: Option<u32>,
}

/// Represents the workspace layouts response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceLayoutsResponse {
    workspaces: Vec<WorkspaceLayoutSummary>,
}

/// Represents the put workspace layout request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PutWorkspaceLayoutRequest {
    control_panel_side: WorkspaceControlPanelSide,
    #[serde(default)]
    theme_id: Option<String>,
    #[serde(default)]
    style_id: Option<String>,
    #[serde(default)]
    font_size_px: Option<u32>,
    #[serde(default)]
    editor_font_size_px: Option<u32>,
    #[serde(default)]
    density_percent: Option<u32>,
    workspace: Value,
}

/// Represents the create session response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionResponse {
    session_id: String,
    state: StateResponse,
}

/// Represents the create project response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateProjectResponse {
    project_id: String,
    state: StateResponse,
}

/// Enumerates project digest actions.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectDigestAction {
    id: String,
    label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    requires_confirmation: bool,
}

/// Represents the project digest response payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectDigestResponse {
    project_id: String,
    headline: String,
    done_summary: String,
    current_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    primary_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    proposed_actions: Vec<ProjectDigestAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deep_link: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    source_message_ids: Vec<String>,
}

/// Represents project digest inputs.
#[derive(Clone)]
struct ProjectDigestInputs {
    project: Project,
    sessions: Vec<SessionRecord>,
}

/// Represents the project approval target.
#[derive(Clone)]
struct ProjectApprovalTarget {
    session_id: String,
    message_id: String,
}

/// Summarizes project digest.
struct ProjectDigestSummary {
    project_id: String,
    headline: String,
    done_summary: String,
    current_status: String,
    primary_session_id: Option<String>,
    proposed_actions: Vec<ProjectActionId>,
    deep_link: Option<String>,
    pending_approval_target: Option<ProjectApprovalTarget>,
    source_message_ids: Vec<String>,
}

impl ProjectDigestSummary {
    /// Converts the value into response.
    fn into_response(self) -> ProjectDigestResponse {
        ProjectDigestResponse {
            project_id: self.project_id,
            headline: self.headline,
            done_summary: self.done_summary,
            current_status: self.current_status,
            primary_session_id: self.primary_session_id,
            proposed_actions: self
                .proposed_actions
                .into_iter()
                .map(ProjectActionId::into_digest_action)
                .collect(),
            deep_link: self.deep_link,
            source_message_ids: self.source_message_ids,
        }
    }
}

/// Defines the project action ID variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectActionId {
    Approve,
    Reject,
    ReviewInTermal,
    FixIt,
    Stop,
    AskAgentToCommit,
    KeepIterating,
    Continue,
}

impl ProjectActionId {
    /// Handles parse.
    fn parse(value: &str) -> Result<Self, ApiError> {
        match value.trim() {
            "approve" => Ok(Self::Approve),
            "reject" => Ok(Self::Reject),
            "review-in-termal" => Ok(Self::ReviewInTermal),
            "fix-it" => Ok(Self::FixIt),
            "stop" => Ok(Self::Stop),
            "ask-agent-to-commit" => Ok(Self::AskAgentToCommit),
            "keep-iterating" => Ok(Self::KeepIterating),
            "continue" => Ok(Self::Continue),
            other => Err(ApiError::bad_request(format!(
                "unknown project action `{other}`"
            ))),
        }
    }

    /// Returns the str representation.
    fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Reject => "reject",
            Self::ReviewInTermal => "review-in-termal",
            Self::FixIt => "fix-it",
            Self::Stop => "stop",
            Self::AskAgentToCommit => "ask-agent-to-commit",
            Self::KeepIterating => "keep-iterating",
            Self::Continue => "continue",
        }
    }

    /// Handles label.
    fn label(self) -> &'static str {
        match self {
            Self::Approve => "Approve",
            Self::Reject => "Reject",
            Self::ReviewInTermal => "Review in TermAl",
            Self::FixIt => "Fix It",
            Self::Stop => "Stop",
            Self::AskAgentToCommit => "Ask Agent to Commit",
            Self::KeepIterating => "Keep Iterating",
            Self::Continue => "Continue",
        }
    }

    /// Handles prompt.
    fn prompt(self) -> Option<&'static str> {
        match self {
            Self::FixIt => Some(
                "The last run failed. Fix the issue, rerun the relevant verification, and summarize what changed.",
            ),
            Self::AskAgentToCommit => Some(
                "If the current changes are ready, create a git commit with a concise message and summarize the result.",
            ),
            Self::KeepIterating => Some(
                "Keep iterating on the current task and report back when the next review point is ready.",
            ),
            Self::Continue => Some(
                "Continue the work on this project and report back when the next review point is ready.",
            ),
            Self::Approve | Self::Reject | Self::ReviewInTermal | Self::Stop => None,
        }
    }

    /// Handles requires confirmation.
    fn requires_confirmation(self) -> bool {
        matches!(self, Self::Stop)
    }

    /// Converts the value into digest action.
    fn into_digest_action(self) -> ProjectDigestAction {
        ProjectDigestAction {
            id: self.as_str().to_owned(),
            label: self.label().to_owned(),
            prompt: self.prompt().map(str::to_owned),
            requires_confirmation: self.requires_confirmation(),
        }
    }
}

/// Represents a agent command.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentCommand {
    #[serde(default)]
    kind: AgentCommandKind,
    name: String,
    description: String,
    content: String,
    source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    argument_hint: Option<String>,
}

/// Represents the agent commands response payload.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentCommandsResponse {
    commands: Vec<AgentCommand>,
}

/// Enumerates agent command kinds.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum AgentCommandKind {
    PromptTemplate,
    NativeSlash,
}

impl Default for AgentCommandKind {
    /// Builds the default value.
    fn default() -> Self {
        Self::PromptTemplate
    }
}

/// Enumerates instruction document kinds.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum InstructionDocumentKind {
    RootInstruction,
    CommandInstruction,
    ReviewerInstruction,
    RulesInstruction,
    SkillInstruction,
    ReferencedInstruction,
}

/// Defines the instruction relation variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
enum InstructionRelation {
    MarkdownLink,
    FileReference,
    DirectoryDiscovery,
}

/// Represents the instruction search response payload.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstructionSearchResponse {
    matches: Vec<InstructionSearchMatch>,
    query: String,
    workdir: String,
}

/// Represents instruction search match.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstructionSearchMatch {
    line: usize,
    path: String,
    root_paths: Vec<InstructionRootPath>,
    text: String,
}

/// Represents instruction root path.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstructionRootPath {
    root_kind: InstructionDocumentKind,
    root_path: String,
    steps: Vec<InstructionPathStep>,
}

/// Represents instruction path step.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstructionPathStep {
    excerpt: String,
    from_path: String,
    line: usize,
    relation: InstructionRelation,
    to_path: String,
}

/// Represents instruction document internal.
#[derive(Clone, Debug)]
struct InstructionDocumentInternal {
    kind: InstructionDocumentKind,
    lines: Vec<String>,
    path: PathBuf,
}

/// Represents instruction search graph.
#[derive(Clone, Debug, Default)]
struct InstructionSearchGraph {
    documents: HashMap<String, InstructionDocumentInternal>,
    incoming: HashMap<String, Vec<InstructionPathStep>>,
}

/// Represents the pick project root response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PickProjectRootResponse {
    path: Option<String>,
}

/// Builds project deep link.
fn build_project_deep_link(project_id: &str, session_id: Option<&str>) -> String {
    let mut query = format!("/?projectId={}", encode_uri_component(project_id));
    if let Some(session_id) = session_id {
        query.push_str("&sessionId=");
        query.push_str(&encode_uri_component(session_id));
    }
    query
}

/// Normalizes project text.
fn normalize_project_text(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        make_preview(trimmed)
    }
}

/// Returns the active project status text.
fn active_project_status_text(record: &SessionRecord) -> String {
    let queued_count = record.session.pending_prompts.len();
    match queued_count {
        0 => "Agent is working.".to_owned(),
        1 => "Agent is working with 1 queued follow-up.".to_owned(),
        count => format!("Agent is working with {count} queued follow-ups."),
    }
}

/// Handles select project done summary.
fn select_project_done_summary(
    primary_session: Option<&SessionRecord>,
    git_status: Option<&GitStatusResponse>,
    prefer_git: bool,
) -> (String, Vec<String>) {
    let message_summary = primary_session.and_then(latest_project_progress_summary);
    let git_summary = git_status.and_then(project_git_done_summary);
    if prefer_git {
        if let Some(summary) = git_summary.clone() {
            return (summary, Vec::new());
        }
    }
    if let Some((message_id, summary)) = message_summary {
        return (summary, vec![message_id]);
    }
    if let Some(summary) = git_summary {
        return (summary, Vec::new());
    }
    (
        primary_session
            .map(default_project_done_summary)
            .unwrap_or_else(|| "No agent work has started yet.".to_owned()),
        Vec::new(),
    )
}

/// Returns the default project done summary.
fn default_project_done_summary(record: &SessionRecord) -> String {
    if record.session.messages.is_empty() {
        return "Ready for the next prompt.".to_owned();
    }
    let preview = record.session.preview.trim();
    if preview.is_empty() {
        "Ready for the next prompt.".to_owned()
    } else {
        make_preview(preview)
    }
}

/// Handles project Git done summary.
fn project_git_done_summary(status: &GitStatusResponse) -> Option<String> {
    let changed_files = status.files.len();
    if changed_files == 0 {
        return None;
    }
    Some(match changed_files {
        1 => "Working tree has 1 changed file ready for review.".to_owned(),
        count => format!("Working tree has {count} changed files ready for review."),
    })
}

/// Returns the latest project progress summary.
fn latest_project_progress_summary(record: &SessionRecord) -> Option<(String, String)> {
    record.session.messages.iter().rev().find_map(|message| {
        project_progress_summary_for_message(message)
            .map(|summary| (message.id().to_owned(), summary))
    })
}

/// Handles project progress summary for message.
fn project_progress_summary_for_message(message: &Message) -> Option<String> {
    match message {
        Message::Text {
            author: Author::Assistant,
            text,
            attachments,
            ..
        } => Some(prompt_preview_text(text, attachments)),
        Message::Thinking { title, .. } => Some(make_preview(title)),
        Message::Command {
            command, status, ..
        } => match status {
            CommandStatus::Running => None,
            CommandStatus::Success => Some(format!("Ran {} successfully.", make_preview(command))),
            CommandStatus::Error => Some(format!("Command failed: {}.", make_preview(command))),
        },
        Message::Diff { summary, .. } => Some(make_preview(summary)),
        Message::Markdown { title, .. } => Some(make_preview(title)),
        Message::SubagentResult { summary, title, .. } => {
            let detail = summary.trim();
            if detail.is_empty() {
                Some(make_preview(title))
            } else {
                Some(make_preview(detail))
            }
        }
        Message::ParallelAgents { agents, .. } => Some(parallel_agents_preview_text(agents)),
        Message::Approval { .. }
        | Message::UserInputRequest { .. }
        | Message::McpElicitationRequest { .. }
        | Message::CodexAppRequest { .. }
        | Message::Text {
            author: Author::You,
            ..
        } => None,
    }
}

/// Finds latest project pending approval.
fn find_latest_project_pending_approval<'a>(
    sessions: &'a [SessionRecord],
) -> Option<(&'a SessionRecord, String)> {
    sessions.iter().rev().find_map(|record| {
        record
            .session
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::Approval { id, decision, .. }
                    if *decision == ApprovalDecision::Pending
                        && has_live_pending_approval(record, id) =>
                {
                    Some((record, id.clone()))
                }
                _ => None,
            })
    })
}

/// Returns whether live pending approval.
fn has_live_pending_approval(record: &SessionRecord, message_id: &str) -> bool {
    record.pending_claude_approvals.contains_key(message_id)
        || record.pending_codex_approvals.contains_key(message_id)
        || record.pending_acp_approvals.contains_key(message_id)
}

/// Finds latest project pending nonapproval interaction.
fn find_latest_project_pending_nonapproval_interaction<'a>(
    sessions: &'a [SessionRecord],
) -> Option<(&'a SessionRecord, String)> {
    sessions.iter().rev().find_map(|record| {
        record
            .session
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::UserInputRequest { id, state, .. }
                    if *state == InteractionRequestState::Pending =>
                {
                    Some((record, id.clone()))
                }
                Message::McpElicitationRequest { id, state, .. }
                    if *state == InteractionRequestState::Pending =>
                {
                    Some((record, id.clone()))
                }
                Message::CodexAppRequest { id, state, .. }
                    if *state == InteractionRequestState::Pending =>
                {
                    Some((record, id.clone()))
                }
                _ => None,
            })
    })
}

/// Defines the delta event variants.
#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum DeltaEvent {
    MessageCreated {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "messageIndex")]
        message_index: usize,
        message: Message,
        preview: String,
        status: SessionStatus,
    },
    TextDelta {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "messageIndex")]
        message_index: usize,
        delta: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        preview: Option<String>,
    },
    TextReplace {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "messageIndex")]
        message_index: usize,
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        preview: Option<String>,
    },
    CommandUpdate {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "messageIndex")]
        message_index: usize,
        command: String,
        #[serde(rename = "commandLanguage", skip_serializing_if = "Option::is_none")]
        command_language: Option<String>,
        output: String,
        #[serde(rename = "outputLanguage", skip_serializing_if = "Option::is_none")]
        output_language: Option<String>,
        status: CommandStatus,
        preview: String,
    },
    ParallelAgentsUpdate {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "messageIndex")]
        message_index: usize,
        agents: Vec<ParallelAgentProgress>,
        preview: String,
    },
    OrchestratorsUpdated {
        revision: u64,
        orchestrators: Vec<OrchestratorInstance>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sessions: Vec<Session>,
    },
}

/// Resolves requested path.
fn resolve_requested_path(path: &str) -> Result<PathBuf, ApiError> {
    let raw_path = FsPath::new(path);
    let resolved = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|err| ApiError::internal(format!("failed to resolve cwd: {err}")))?
            .join(raw_path)
    };

    Ok(resolved)
}

/// Resolves existing requested path.
fn resolve_existing_requested_path(path: &str, label: &str) -> Result<PathBuf, ApiError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request(format!("{label} cannot be empty")));
    }

    let requested_path = resolve_requested_path(trimmed)?;
    let resolved_path = canonicalize_existing_path(&requested_path, label)?;
    Ok(normalize_user_facing_path(&resolved_path))
}

/// Enumerates scoped path modes.
#[derive(Clone, Copy)]
enum ScopedPathMode {
    ExistingFile,
    ExistingPath,
    AllowMissingLeaf,
}

/// Normalizes optional identifier.
fn normalize_optional_identifier(value: Option<&str>) -> Option<&str> {
    value
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
}

/// Resolves session project root path.
fn resolve_session_project_root_path(
    state: &AppState,
    session_id: &str,
) -> Result<PathBuf, ApiError> {
    let root_path = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = &inner.sessions[index];
        if record.hidden {
            return Err(ApiError::not_found("session not found"));
        }
        if let Some(project) = record
            .session
            .project_id
            .as_deref()
            .and_then(|project_id| inner.find_project(project_id))
        {
            if project.remote_id != LOCAL_REMOTE_ID {
                return Err(ApiError::bad_request(format!(
                    "project `{}` is assigned to remote `{}`. Remote file access is not implemented yet.",
                    project.name, project.remote_id
                )));
            }
            project.root_path.clone()
        } else {
            inner
                .find_project_for_workdir(&record.session.workdir)
                .map(|project| project.root_path.clone())
                .unwrap_or_else(|| record.session.workdir.clone())
        }
    };

    fs::canonicalize(&root_path)
        .map(|path| normalize_user_facing_path(&path))
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => {
                ApiError::bad_request(format!("project root not found: {root_path}"))
            }
            _ => ApiError::internal(format!(
                "failed to resolve project root {}: {err}",
                root_path
            )),
        })
}

/// Resolves project root path by ID.
fn resolve_project_root_path_by_id(
    state: &AppState,
    project_id: &str,
) -> Result<PathBuf, ApiError> {
    let root_path = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let project = inner
            .find_project(project_id)
            .ok_or_else(|| ApiError::not_found("project not found"))?;
        if project.remote_id != LOCAL_REMOTE_ID {
            return Err(ApiError::bad_request(format!(
                "project `{}` is assigned to remote `{}`. Remote file access is not implemented yet.",
                project.name, project.remote_id
            )));
        }
        project.root_path.clone()
    };

    fs::canonicalize(&root_path)
        .map(|path| normalize_user_facing_path(&path))
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => {
                ApiError::bad_request(format!("project root not found: {root_path}"))
            }
            _ => ApiError::internal(format!(
                "failed to resolve project root {}: {err}",
                root_path
            )),
        })
}

/// Resolves request project root path.
fn resolve_request_project_root_path(
    state: &AppState,
    session_id: Option<&str>,
    project_id: Option<&str>,
) -> Result<PathBuf, ApiError> {
    if let Some(session_id) = normalize_optional_identifier(session_id) {
        return resolve_session_project_root_path(state, session_id);
    }

    if let Some(project_id) = normalize_optional_identifier(project_id) {
        return resolve_project_root_path_by_id(state, project_id);
    }

    Err(ApiError::bad_request("sessionId or projectId is required"))
}

/// Resolves project scoped requested path.
fn resolve_project_scoped_requested_path(
    state: &AppState,
    session_id: Option<&str>,
    project_id: Option<&str>,
    path: &str,
    mode: ScopedPathMode,
) -> Result<PathBuf, ApiError> {
    let project_root = resolve_request_project_root_path(state, session_id, project_id)?;
    let requested_path = resolve_requested_path(path)?;
    let resolved_path = match mode {
        ScopedPathMode::ExistingFile => canonicalize_existing_path(&requested_path, "file")?,
        ScopedPathMode::ExistingPath => canonicalize_existing_path(&requested_path, "path")?,
        ScopedPathMode::AllowMissingLeaf => {
            canonicalize_path_with_existing_ancestor(&requested_path)?
        }
    };

    if resolved_path != project_root && !resolved_path.starts_with(&project_root) {
        return Err(ApiError::bad_request(format!(
            "path `{}` must stay inside project `{}`",
            requested_path.display(),
            project_root.display()
        )));
    }

    Ok(normalize_user_facing_path(&resolved_path))
}

/// Resolves session scoped requested path.
#[cfg(test)]
fn resolve_session_scoped_requested_path(
    state: &AppState,
    session_id: &str,
    path: &str,
    mode: ScopedPathMode,
) -> Result<PathBuf, ApiError> {
    resolve_project_scoped_requested_path(state, Some(session_id), None, path, mode)
}

/// Canonicalizes existing path.
fn canonicalize_existing_path(path: &FsPath, label: &str) -> Result<PathBuf, ApiError> {
    fs::canonicalize(path)
        .map(|path| normalize_user_facing_path(&path))
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => {
                ApiError::not_found(format!("{label} not found: {}", path.display()))
            }
            _ => ApiError::internal(format!(
                "failed to resolve {label} {}: {err}",
                path.display()
            )),
        })
}

/// Canonicalizes path with existing ancestor.
fn canonicalize_path_with_existing_ancestor(path: &FsPath) -> Result<PathBuf, ApiError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|err| ApiError::internal(format!("failed to resolve cwd: {err}")))?
            .join(path)
    };

    let mut suffix = Vec::new();
    let mut probe = absolute.as_path();

    loop {
        match fs::metadata(probe) {
            Ok(_) => {
                let mut canonical = fs::canonicalize(probe).map_err(|err| {
                    ApiError::internal(format!("failed to resolve path {}: {err}", probe.display()))
                })?;
                for component in suffix.iter().rev() {
                    canonical.push(component);
                }
                return Ok(normalize_user_facing_path(&canonical));
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let Some(name) = probe.file_name().map(|value| value.to_os_string()) else {
                    return Err(ApiError::bad_request(format!(
                        "path not found: {}",
                        absolute.display()
                    )));
                };
                suffix.push(name);
                let Some(parent) = probe.parent() else {
                    return Err(ApiError::bad_request(format!(
                        "path not found: {}",
                        absolute.display()
                    )));
                };
                probe = parent;
            }
            Err(err) => {
                return Err(ApiError::internal(format!(
                    "failed to resolve path {}: {err}",
                    probe.display()
                )));
            }
        }
    }
}

/// Normalizes user facing path.
fn normalize_user_facing_path(path: &FsPath) -> PathBuf {
    #[cfg(windows)]
    {
        let raw = path.to_string_lossy();
        if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = raw.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }

    path.to_path_buf()
}

/// Resolves directory path.
fn resolve_directory_path(path: &str, label: &str) -> Result<String, ApiError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request(format!("{label} cannot be empty")));
    }

    let resolved = resolve_requested_path(trimmed)?;
    let directory = if resolved.is_dir() {
        resolved
    } else {
        return Err(ApiError::bad_request(format!(
            "`{}` is not a directory",
            trimmed
        )));
    };
    let canonical = fs::canonicalize(&directory).unwrap_or(directory);
    Ok(normalize_user_facing_path(&canonical)
        .to_string_lossy()
        .into_owned())
}

/// Resolves project root path.
fn resolve_project_root_path(path: &str) -> Result<String, ApiError> {
    resolve_directory_path(path, "project root path")
}

/// Resolves session working directory.
fn resolve_session_workdir(path: &str) -> Result<String, ApiError> {
    resolve_directory_path(path, "session workdir")
}

/// Picks project root path.
fn pick_project_root_path(default_workdir: &str) -> Result<Option<String>, ApiError> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("osascript")
            .arg("-e")
            .arg("on run argv")
            .arg("-e")
            .arg("set defaultLocation to POSIX file (item 1 of argv)")
            .arg("-e")
            .arg("try")
            .arg("-e")
            .arg(
                "set chosenFolder to choose folder with prompt \"Choose a folder for this project\" default location defaultLocation",
            )
            .arg("-e")
            .arg("return POSIX path of chosenFolder")
            .arg("-e")
            .arg("on error number -128")
            .arg("-e")
            .arg("return \"\"")
            .arg("-e")
            .arg("end try")
            .arg("-e")
            .arg("end run")
            .arg(default_workdir)
            .output()
            .map_err(|err| ApiError::internal(format!("failed to open folder picker: {err}")))?;

        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let message = if detail.is_empty() {
                "folder picker failed".to_owned()
            } else {
                format!("folder picker failed: {detail}")
            };
            return Err(ApiError::internal(message));
        }

        let selected = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if selected.is_empty() {
            return Ok(None);
        }

        return resolve_project_root_path(&selected).map(Some);
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = default_workdir;
        Err(ApiError::bad_request(
            "Folder picker is unavailable on this platform. Enter the path manually.",
        ))
    }
}

/// Returns the default project name.
fn default_project_name(root_path: &str) -> String {
    let path = FsPath::new(root_path);
    path.file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| root_path.to_owned())
}

/// Deduplicates project name.
fn dedupe_project_name(existing: &[Project], base_name: &str) -> String {
    let existing_names = existing
        .iter()
        .map(|project| project.name.as_str())
        .collect::<HashSet<_>>();
    if !existing_names.contains(base_name) {
        return base_name.to_owned();
    }

    let mut suffix = 2usize;
    loop {
        let candidate = format!("{base_name} {suffix}");
        if !existing_names.contains(candidate.as_str()) {
            return candidate;
        }
        suffix += 1;
    }
}

/// Handles path contains.
fn path_contains(root_path: &str, candidate_path: &FsPath) -> bool {
    let root = normalize_path_best_effort(FsPath::new(root_path));
    let candidate = normalize_path_best_effort(candidate_path);
    candidate == root || candidate.starts_with(root)
}

/// Normalizes path best effort.
fn normalize_path_best_effort(path: &FsPath) -> PathBuf {
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let canonical = fs::canonicalize(&resolved).unwrap_or(resolved);
    normalize_user_facing_path(&canonical)
}

/// Enumerates parsed Git branch states.
struct ParsedGitBranchStatus {
    ahead: usize,
    behind: usize,
    branch: Option<String>,
    upstream: Option<String>,
}

/// Resolves Git repo root.
fn resolve_git_repo_root(workdir: &FsPath) -> Result<Option<PathBuf>, ApiError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|err| ApiError::internal(format!("failed to run git rev-parse: {err}")))?;

    if output.status.success() {
        let repo_root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        return Ok((!repo_root.is_empty()).then(|| PathBuf::from(repo_root)));
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let trimmed = stderr.trim();
    if trimmed.contains("not a git repository") {
        return Ok(None);
    }

    Err(ApiError::internal(format!(
        "git rev-parse failed: {trimmed}"
    )))
}

/// Parses Git branch status.
fn parse_git_branch_status(line: &str) -> ParsedGitBranchStatus {
    let mut branch = None;
    let mut upstream = None;
    let mut ahead = 0;
    let mut behind = 0;

    let (head_segment, counts_segment) = match line.split_once(" [") {
        Some((head, counts)) => (head, Some(counts.trim_end_matches(']'))),
        None => (line, None),
    };

    if let Some((local_branch, upstream_branch)) = head_segment.split_once("...") {
        branch = Some(local_branch.trim().to_owned());
        let upstream_name = upstream_branch.trim();
        if !upstream_name.is_empty() {
            upstream = Some(upstream_name.to_owned());
        }
    } else {
        let trimmed = head_segment.trim();
        if !trimmed.is_empty() {
            branch = Some(trimmed.to_owned());
        }
    }

    if let Some(counts_segment) = counts_segment {
        for item in counts_segment.split(',') {
            let trimmed = item.trim();
            if let Some(value) = trimmed.strip_prefix("ahead ") {
                ahead = value.parse::<usize>().unwrap_or(0);
            } else if let Some(value) = trimmed.strip_prefix("behind ") {
                behind = value.parse::<usize>().unwrap_or(0);
            }
        }
    }

    ParsedGitBranchStatus {
        ahead,
        behind,
        branch,
        upstream,
    }
}

/// Parses Git status paths.
fn parse_git_status_paths(path_payload: &str) -> (Option<String>, String) {
    if let Some(separator_index) = find_git_status_rename_separator(path_payload) {
        let original_path = decode_git_status_path(&path_payload[..separator_index]);
        let path = decode_git_status_path(&path_payload[separator_index + 4..]);
        return (Some(original_path), path);
    }

    (None, decode_git_status_path(path_payload))
}

/// Finds Git status rename separator.
fn find_git_status_rename_separator(path_payload: &str) -> Option<usize> {
    let bytes = path_payload.as_bytes();
    let mut index = 0;
    let mut in_quotes = false;

    while index < bytes.len() {
        match bytes[index] {
            b'\\' if in_quotes => {
                index += 2;
            }
            b'"' => {
                in_quotes = !in_quotes;
                index += 1;
            }
            b' ' if !in_quotes && bytes[index..].starts_with(b" -> ") => {
                return Some(index);
            }
            _ => {
                index += 1;
            }
        }
    }

    None
}

/// Decodes Git status path.
fn decode_git_status_path(path: &str) -> String {
    let trimmed = path.trim();
    decode_git_status_quoted_path(trimmed).unwrap_or_else(|| trimmed.to_owned())
}

/// Decodes Git status quoted path.
fn decode_git_status_quoted_path(path: &str) -> Option<String> {
    if !path.starts_with('"') || !path.ends_with('"') || path.len() < 2 {
        return None;
    }

    let inner = &path[1..path.len() - 1];
    let bytes = inner.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] != b'\\' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }

        index += 1;
        if index >= bytes.len() {
            return None;
        }

        match bytes[index] {
            b'"' | b'\\' => {
                decoded.push(bytes[index]);
                index += 1;
            }
            b'a' => {
                decoded.push(0x07);
                index += 1;
            }
            b'b' => {
                decoded.push(0x08);
                index += 1;
            }
            b'f' => {
                decoded.push(0x0c);
                index += 1;
            }
            b'n' => {
                decoded.push(b'\n');
                index += 1;
            }
            b'r' => {
                decoded.push(b'\r');
                index += 1;
            }
            b't' => {
                decoded.push(b'\t');
                index += 1;
            }
            b'v' => {
                decoded.push(0x0b);
                index += 1;
            }
            b'0'..=b'7' => {
                let mut value = bytes[index] - b'0';
                index += 1;

                for _ in 0..2 {
                    if index >= bytes.len() {
                        break;
                    }
                    let next = bytes[index];
                    if !matches!(next, b'0'..=b'7') {
                        break;
                    }
                    value = value.saturating_mul(8).saturating_add(next - b'0');
                    index += 1;
                }

                decoded.push(value);
            }
            other => {
                decoded.push(other);
                index += 1;
            }
        }
    }

    Some(String::from_utf8_lossy(&decoded).into_owned())
}

/// Normalizes Git status code.
fn normalize_git_status_code(code: char) -> Option<String> {
    match code {
        ' ' => None,
        other => Some(other.to_string()),
    }
}
