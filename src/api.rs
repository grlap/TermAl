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

/// Returns a stable content identity for source-editor conflict detection.
fn file_content_hash(content: &[u8]) -> String {
    let digest = Sha256::digest(content);
    format!("sha256:{digest:x}")
}

/// Converts filesystem modified time to JavaScript-friendly milliseconds.
fn file_metadata_mtime_ms(metadata: &fs::Metadata) -> Option<u64> {
    let millis = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis();
    Some(millis.min(u128::from(u64::MAX)) as u64)
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
///
/// Builds AND serializes the snapshot inside `spawn_blocking` so the tokio
/// worker does not spend milliseconds-to-seconds of CPU running
/// `serde_json::to_writer` on a `Vec<Session>` that contains every session's
/// full `Vec<Message>`. The worker thread only handles the pre-serialized
/// `Vec<u8>` body, which is a fixed-cost hand-off to hyper.
async fn get_state(State(state): State<AppState>) -> Result<Response, ApiError> {
    let body = run_blocking_api(move || {
        let snapshot = state.snapshot();
        serde_json::to_vec(&snapshot)
            .map_err(|err| ApiError::internal(format!("failed to serialize state: {err}")))
    })
    .await?;
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        body,
    )
        .into_response())
}

/// Gets one full session.
async fn get_session(
    State(state): State<AppState>,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<SessionResponse>, ApiError> {
    let response = run_blocking_api(move || state.get_session(&session_id)).await?;
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

    let response_metadata = tokio::fs::metadata(&resolved_path).await.ok();
    let metadata_for_response = response_metadata.as_ref().unwrap_or(&metadata);
    let response = FileResponse {
        path: resolved_path.to_string_lossy().into_owned(),
        content_hash: Some(file_content_hash(content.as_bytes())),
        mtime_ms: file_metadata_mtime_ms(metadata_for_response),
        size_bytes: Some(metadata_for_response.len()),
        content,
        language: infer_language_from_path(&resolved_path).map(str::to_owned),
    };
    Ok(Json(response))
}

/// Writes file.
fn validate_file_base_hash(resolved_path: &FsPath, base_hash: &str) -> Result<(), ApiError> {
    // The editor sends the content hash it originally loaded. Refuse stale
    // saves by default so agent/user edits do not silently clobber each other;
    // explicit overwrite is the only bypass.
    let current_metadata = fs::metadata(resolved_path).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => ApiError::conflict(format!(
            "file changed on disk before save: {} was deleted",
            resolved_path.display()
        )),
        _ => ApiError::internal(format!(
            "failed to stat file before save {}: {err}",
            resolved_path.display()
        )),
    })?;
    if current_metadata.len() > MAX_FILE_CONTENT_BYTES as u64 {
        return Err(ApiError::conflict(format!(
            "file changed on disk before save: {} exceeds the read limit",
            resolved_path.display()
        )));
    }

    let current_content = fs::read(resolved_path).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => ApiError::conflict(format!(
            "file changed on disk before save: {} was deleted",
            resolved_path.display()
        )),
        _ => ApiError::internal(format!(
            "failed to read file before save {}: {err}",
            resolved_path.display()
        )),
    })?;
    let current_hash = file_content_hash(&current_content);
    if current_hash != base_hash {
        return Err(ApiError::conflict(format!(
            "file changed on disk before save: {}",
            resolved_path.display()
        )));
    }

    Ok(())
}

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
                    "baseHash": request.base_hash.clone(),
                    "overwrite": request.overwrite,
                }),
            );
        }

        if request.content.as_bytes().len() > MAX_FILE_CONTENT_BYTES {
            return Err(ApiError::bad_request(format!(
                "file content exceeds the {} MB write limit",
                MAX_FILE_CONTENT_BYTES / (1024 * 1024)
            )));
        }

        let mut resolved_path = resolve_project_scoped_requested_path(
            &state,
            request.session_id.as_deref(),
            request.project_id.as_deref(),
            &request.path,
            ScopedPathMode::AllowMissingLeaf,
        )?;
        let should_check_base = request
            .base_hash
            .as_deref()
            .map(str::trim)
            .filter(|hash| !hash.is_empty())
            .filter(|_| !request.overwrite);
        if let Some(base_hash) = should_check_base {
            validate_file_base_hash(&resolved_path, base_hash)?;
        }
        if let Some(parent) = resolved_path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                ApiError::internal(format!(
                    "failed to create parent directory for {}: {err}",
                    resolved_path.display()
                ))
            })?;
        }
        resolved_path = verify_scoped_write_path_after_parent_creation(
            &state,
            request.session_id.as_deref(),
            request.project_id.as_deref(),
            &resolved_path,
        )?;

        let content_hash = file_content_hash(request.content.as_bytes());
        if let Some(base_hash) = should_check_base {
            validate_file_base_hash(&resolved_path, base_hash)?;
        }
        fs::write(&resolved_path, request.content.as_bytes()).map_err(|err| {
            ApiError::internal(format!(
                "failed to write file {}: {err}",
                resolved_path.display()
            ))
        })?;
        let metadata = fs::metadata(&resolved_path).ok();

        Ok(FileResponse {
            path: resolved_path.to_string_lossy().into_owned(),
            content_hash: Some(content_hash),
            mtime_ms: metadata.as_ref().and_then(file_metadata_mtime_ms),
            size_bytes: metadata.map(|metadata| metadata.len()),
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

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(test, allow(dead_code))]
#[serde(rename_all = "camelCase")]
enum WorkspaceFileChangeKind {
    Created,
    Modified,
    Deleted,
    Other,
}

/// Represents a file changed during an agent turn.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileChangeSummaryEntry {
    path: String,
    kind: WorkspaceFileChangeKind,
}

#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(test, allow(dead_code))]
#[serde(rename_all = "camelCase")]
struct WorkspaceFileChangeEvent {
    path: String,
    kind: WorkspaceFileChangeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    root_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mtime_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size_bytes: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[cfg_attr(test, allow(dead_code))]
#[serde(rename_all = "camelCase")]
struct WorkspaceFilesChangedEvent {
    revision: u64,
    changes: Vec<WorkspaceFileChangeEvent>,
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
    let mut file_receiver = state.subscribe_file_events();
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

                result = file_receiver.recv() => {
                    match result {
                        Ok(payload) => yield Ok(Event::default().event("workspaceFilesChanged").data(payload)),
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
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

/// Deletes project.
async fn delete_project(
    AxumPath(project_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || state.delete_project(&project_id)).await?;
    Ok(Json(response))
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

