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

/// Represents the API error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApiErrorKind {
    #[cfg_attr(not(unix), allow(dead_code))]
    GitDocumentBecameSymlink,
    GitDocumentInvalidUtf8,
    GitDocumentNotFile,
    GitDocumentNotFound,
    GitDocumentTooLarge,
}

#[derive(Debug)]
struct ApiError {
    message: String,
    status: StatusCode,
    kind: Option<ApiErrorKind>,
}

impl ApiError {
    /// Handles bad request.
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::BAD_REQUEST,
            kind: None,
        }
    }

    /// Handles conflict.
    fn conflict(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::CONFLICT,
            kind: None,
        }
    }

    /// Handles not found.
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::NOT_FOUND,
            kind: None,
        }
    }

    /// Handles internal.
    fn internal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::INTERNAL_SERVER_ERROR,
            kind: None,
        }
    }

    /// Handles bad gateway.
    fn bad_gateway(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::BAD_GATEWAY,
            kind: None,
        }
    }

    /// Builds the value from status.
    fn from_status(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status,
            kind: None,
        }
    }

    /// Tags internal error handling without changing the wire response.
    fn with_kind(mut self, kind: ApiErrorKind) -> Self {
        self.kind = Some(kind);
        self
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
            Self::Claude => "default",
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
    #[serde(rename = "fileChanges")]
    FileChanges {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        files: Vec<FileChangeSummaryEntry>,
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
            | Self::FileChanges { id, .. }
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
            Self::FileChanges { .. } => None,
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
    default_claude_approval_mode: Option<ClaudeApprovalMode>,
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
    base_hash: Option<String>,
    #[serde(default)]
    overwrite: bool,
    project_id: Option<String>,
    session_id: Option<String>,
}

/// Represents the file response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileResponse {
    path: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mtime_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size_bytes: Option<u64>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    document_enrichment_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    document_content: Option<GitDiffDocumentContent>,
    summary: String,
}

/// Represents full document sides for a Git diff.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitDiffDocumentContent {
    before: GitDiffDocumentSide,
    after: GitDiffDocumentSide,
    can_edit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    edit_blocked_reason: Option<String>,
    is_complete_document: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

/// Represents one full document side for a Git diff.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitDiffDocumentSide {
    content: String,
    source: GitDiffDocumentSideSource,
}

/// Defines where a full document side came from.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum GitDiffDocumentSideSource {
    Head,
    Index,
    Worktree,
    Empty,
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

const TERMINAL_COMMAND_MAX_CHARS: usize = 20_000;
/// Upper bound on the `workdir` field of a terminal command request. Real
/// filesystem paths stay well under this; the cap is a defense-in-depth
/// limit so a client cannot POST a megabyte of whitespace-stripped text
/// that then flows into `resolve_project_scoped_requested_path` or over
/// the wire to the remote proxy. Paired with explicit NUL-byte rejection
/// in `validate_terminal_workdir`.
const TERMINAL_WORKDIR_MAX_CHARS: usize = 4_096;
/// Maximum captured terminal output per stream. Stdout and stderr each get
/// their own budget on both local runs and remote JSON proxy responses.
const TERMINAL_OUTPUT_MAX_BYTES: usize = 512 * 1024;
const TERMINAL_STREAM_EVENT_QUEUE_CAPACITY: usize = 256;
const TERMINAL_COMMAND_CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Upper bound on the bytes the remote proxy will buffer while waiting to
/// find the next SSE frame delimiter. A completion frame carries the full
/// `TerminalCommandResponse`, including up to `TERMINAL_OUTPUT_MAX_BYTES` of
/// stdout plus the same of stderr, plus the echoed command string and
/// workdir. JSON encoding can expand each byte up to 6× (ASCII control
/// characters become `\u00XX`) and SSE framing adds further overhead, so the
/// worst-case legitimate completion frame is roughly
/// `TERMINAL_OUTPUT_MAX_BYTES * 12 + ~200 KiB`. Cap at 16× the raw output
/// limit (8 MiB) so that envelope fits with comfortable headroom while still
/// bounding memory if a remote misbehaves and never emits a delimiter.
const TERMINAL_REMOTE_SSE_PENDING_MAX_BYTES: usize = TERMINAL_OUTPUT_MAX_BYTES * 16;

/// Remote proxy timeout for terminal commands. This must cover the remote child
/// wait, post-timeout process cleanup, stdout/stderr reader joins, JSON
/// encoding/decoding, and a small network scheduling margin.
const REMOTE_TERMINAL_COMMAND_TIMEOUT: Duration = Duration::from_secs(90);

/// Maximum time to wait for terminal output-reader threads after the child
/// process exits. Background children that inherit stdout/stderr can keep the
/// pipe open indefinitely; this prevents the request from blocking forever.
const TERMINAL_OUTPUT_READER_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Represents a terminal command request.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TerminalCommandRequest {
    command: String,
    workdir: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

/// Represents a terminal command response.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalCommandResponse {
    command: String,
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    output_truncated: bool,
    shell: String,
    stderr: String,
    stdout: String,
    success: bool,
    timed_out: bool,
    workdir: String,
}

type TerminalCommandStreamSender = tokio::sync::mpsc::Sender<TerminalCommandStreamEvent>;

struct TerminalStreamCancelGuard {
    cancellation: Arc<AtomicBool>,
}

impl Drop for TerminalStreamCancelGuard {
    fn drop(&mut self) {
        self.cancellation.store(true, Ordering::SeqCst);
    }
}

/// SSE stream adapter for a streaming terminal command.
///
/// **Field drop order is load-bearing.** Rust drops struct fields in
/// declaration order, so `event_rx` is dropped *before* `_cancel_on_drop`.
/// That order is required so that any worker still parked inside
/// `blocking_send(..)` on the matching sender observes the channel closing
/// and returns immediately — the spawned worker then releases its
/// concurrency permit and exits. Only after `event_rx` is torn down does
/// the cancellation guard flip, which asks other parts of the pipeline
/// (the SSE forwarder, the remote read adapter, the streaming child wait)
/// to stop. Swapping the field order to "flip the cancellation flag
/// first" would leave the worker parked inside `blocking_send` for up to
/// one `TERMINAL_COMMAND_CANCEL_POLL_INTERVAL` tick before the next
/// `try_send` sees the flag, regressing cancellation latency without
/// failing any existing test.
struct TerminalCommandSseStream {
    event_rx: tokio::sync::mpsc::Receiver<TerminalCommandStreamEvent>,
    _cancel_on_drop: TerminalStreamCancelGuard,
}

impl futures_core::Stream for TerminalCommandSseStream {
    type Item = std::result::Result<Event, Infallible>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match std::pin::Pin::new(&mut this.event_rx).poll_recv(cx) {
            std::task::Poll::Ready(Some(event)) => {
                std::task::Poll::Ready(Some(Ok(terminal_command_sse_event(event))))
            }
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

enum TerminalCommandStreamEvent {
    Output {
        stream: TerminalOutputStream,
        text: String,
    },
    Complete(TerminalCommandResponse),
    Error {
        error: String,
        status: u16,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum TerminalOutputStream {
    Stdout,
    Stderr,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalOutputStreamPayload {
    stream: TerminalOutputStream,
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TerminalStreamErrorPayload {
    error: String,
    #[serde(default)]
    status: Option<u16>,
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

/// Represents one full session response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionResponse {
    revision: u64,
    session: Session,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<Session>,
    #[serde(default)]
    revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state: Option<StateResponse>,
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
        Message::FileChanges { .. } => None,
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
    SessionCreated {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        session: Session,
    },
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


