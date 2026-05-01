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
async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        supports_inline_orchestrator_templates: true,
        server_instance_id: state.server_instance_id.clone(),
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
        let snapshot = state.summary_snapshot();
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
    /// Returns the project digest payload rendered for the Telegram
    /// bot and mobile-dashboard surfaces. Thin wrapper around
    /// [`Self::build_project_digest_summary`] that converts into
    /// the wire response shape.
    fn project_digest(&self, project_id: &str) -> Result<ProjectDigestResponse, ApiError> {
        Ok(self
            .build_project_digest_summary(project_id)?
            .into_response())
    }

    /// Runs a digest action (approve / reject / fix-it / continue / stop)
    /// for a project. Validates the action is still in the current
    /// `proposed_actions` set before dispatching — rejects with 409
    /// if the project state has advanced and the requested action is
    /// no longer valid.
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

    /// Collects the `ProjectDigestInputs` bundle (project metadata
    /// + visible sessions + orchestrator instances) under a single
    /// state-mutex acquisition so the caller can compute the digest
    /// without re-locking per field.
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

/// Creates session.
async fn create_session(
    State(state): State<AppState>,
    Json(request): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<CreateSessionResponse>), ApiError> {
    let response = run_blocking_api(move || state.create_session(request)).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// Creates a Phase 1 read-only child delegation session.
async fn create_session_delegation(
    AxumPath(parent_session_id): AxumPath<String>,
    State(state): State<AppState>,
    Json(request): Json<CreateDelegationRequest>,
) -> Result<(StatusCode, Json<DelegationResponse>), ApiError> {
    let response =
        run_blocking_api(move || state.create_read_only_delegation(&parent_session_id, request))
            .await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// Gets delegation status and metadata.
async fn get_delegation_status(
    AxumPath(delegation_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<DelegationStatusResponse>, ApiError> {
    let response = run_blocking_api(move || state.get_delegation(&delegation_id)).await?;
    Ok(Json(response))
}

/// Gets a completed delegation result packet.
async fn get_delegation_result(
    AxumPath(delegation_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<DelegationResultResponse>, ApiError> {
    let response = run_blocking_api(move || state.get_delegation_result(&delegation_id)).await?;
    Ok(Json(response))
}

/// Cancels a running delegation child session.
async fn cancel_delegation(
    AxumPath(delegation_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<DelegationStatusResponse>, ApiError> {
    let response = run_blocking_api(move || state.cancel_delegation(&delegation_id)).await?;
    Ok(Json(response))
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
        let session_id = session_id.clone();
        move || Ok(state.summary_snapshot_with_full_session(&session_id))
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
