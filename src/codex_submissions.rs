// User-facing response submission handlers. These methods exist because
// some agent events need the *user's* response — not just a recorded
// message. The agent emits an approval / user-input / MCP elicitation /
// app-request event; the frontend surfaces it, the user clicks a
// button, the browser POSTs to the matching HTTP route (see
// `src/main.rs` for routes under `/api/sessions/{id}/approvals/`,
// `/user-input/`, `/mcp-elicitation/`, `/codex/requests/`), and these
// methods route the response back into the still-running agent.
//
// Per-agent split: Claude uses a `ClaudePermissionDecision` queued onto
// the runtime's input channel (ultimately an NDJSON control_request
// response over the CLI pipe); Codex uses a JSON-RPC `sendResponse`
// whose `result` shape depends on the approval kind — built here by
// `codex_approval_result` and sent via `send_codex_json_rpc_request`
// from `src/codex_rpc.rs`; ACP uses an `AcpRuntimeCommand::JsonRpcMessage`
// with the agent's option-id selection protocol.
//
// The pending lookup fans out across all three agents' maps:
// `update_approval` searches `pending_claude_approvals` (keyed by
// message_id), `pending_codex_approvals`, and `pending_acp_approvals`
// to find the right response channel. The `submit_codex_*` trio look
// up their Codex-specific maps (`pending_codex_user_inputs` /
// `pending_codex_mcp_elicitations` / `pending_codex_app_requests`),
// validate payloads through `src/codex_validation.rs`, then dispatch
// via `CodexRuntimeCommand::JsonRpcResponse`. See
// `src/turn_lifecycle.rs` for the `register_*_pending_*` methods that
// stash the entries these consume.
//
// `fail_turn` is the public catch-all for when a submission itself
// fails (e.g., the runtime command channel rejected our send). Unlike
// `fail_turn_if_runtime_matches` in `src/turn_lifecycle.rs`, it is not
// gated by a `RuntimeToken` — the error source is outside any specific
// runtime context, so token matching does not apply.
//
// Cross-refs: `src/wire.rs` for `ApprovalDecision`,
// `UserInputSubmissionRequest`, `McpElicitationSubmissionRequest`,
// `CodexAppRequestSubmissionRequest`; `src/session_interaction.rs` for
// `set_approval_decision_on_record` + the `set_*_request_state_on_record`
// helpers; `src/tests/http_routes.rs` for end-to-end route coverage.

fn interaction_message_update_parts(
    record: &SessionRecord,
    message_index: usize,
    message_id: &str,
) -> (Message, u32, String, SessionStatus, u64) {
    let message = record
        .session
        .messages
        .get(message_index)
        .cloned()
        .expect("commit_interaction_message_update closure returned an out-of-bounds index");
    assert_eq!(
        message.id(),
        message_id,
        "commit_interaction_message_update closure returned a stale message index"
    );

    (
        message,
        session_message_count(record),
        record.session.preview.clone(),
        record.session.status,
        record.mutation_stamp,
    )
}

impl AppState {
    /// Commits an interaction-card edit and publishes its replacement delta.
    /// The closure must return the in-bounds index of `message_id` after
    /// mutation; violating that contract panics because it is an internal
    /// state invariant, not a recoverable API error.
    fn commit_interaction_message_update<F>(
        &self,
        session_id: &str,
        message_id: &str,
        update_record: F,
    ) -> std::result::Result<StateResponse, ApiError>
    where
        F: FnOnce(&mut SessionRecord) -> std::result::Result<usize, ApiError>,
    {
        let snapshot = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let (message_index, message, message_count, preview, status, session_mutation_stamp) = {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                let message_index = update_record(record)?;
                let (message, message_count, preview, status, session_mutation_stamp) =
                    interaction_message_update_parts(record, message_index, message_id);
                (
                    message_index,
                    message,
                    message_count,
                    preview,
                    status,
                    session_mutation_stamp,
                )
            };
            let revision = self.commit_persisted_delta_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist session state: {err:#}"))
            })?;
            let event = DeltaEvent::MessageUpdated {
                revision,
                session_id: session_id.to_owned(),
                message_id: message_id.to_owned(),
                message_index,
                message_count,
                message,
                preview,
                status,
                session_mutation_stamp: Some(session_mutation_stamp),
            };
            self.publish_delta(&event);
            self.snapshot_from_inner_with_full_session(
                &inner,
                self.cached_agent_readiness(),
                session_id,
            )
        };
        Ok(snapshot)
    }

    /// Routes an approval decision back to the originating agent.
    /// Looks up the pending entry across all three agent pending maps
    /// on the `SessionRecord`: `pending_claude_approvals` for Claude
    /// (keyed by message_id, sent as a `ClaudePermissionDecision`
    /// through the runtime input channel),
    /// `pending_codex_approvals` for Codex (sent as a JSON-RPC
    /// `sendResponse` whose `result` is built by
    /// `codex_approval_result`), and `pending_acp_approvals` for
    /// Cursor/Gemini (sent as an ACP selected-option response). After
    /// delivery, updates the approval state on the record and
    /// publishes a delta so the UI shows the resolution immediately.
    fn update_approval(
        &self,
        session_id: &str,
        message_id: &str,
        decision: ApprovalDecision,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_update_approval(session_id, message_id, decision);
        }
        if matches!(
            decision,
            ApprovalDecision::Pending | ApprovalDecision::Interrupted | ApprovalDecision::Canceled
        ) {
            return Err(ApiError::bad_request(
                "approval decisions cannot be marked pending, interrupted, or canceled manually",
            ));
        }

        let mut claude_runtime_action: Option<(ClaudeRuntimeHandle, ClaudePendingApproval)> = None;
        let mut codex_runtime_action: Option<(CodexRuntimeHandle, CodexPendingApproval)> = None;
        let mut acp_runtime_action: Option<(AcpRuntimeHandle, AcpPendingApproval)> = None;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if record.session.status != SessionStatus::Approval {
            return Err(ApiError::conflict(
                "session is not currently awaiting approval",
            ));
        }

        if record.session.agent == Agent::Claude
            && matches!(
                decision,
                ApprovalDecision::Accepted
                    | ApprovalDecision::AcceptedForSession
                    | ApprovalDecision::Rejected
            )
        {
            let pending = record
                .pending_claude_approvals
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("approval request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Claude(handle) => handle.clone(),
                SessionRuntime::Codex(_) => {
                    return Err(ApiError::conflict(
                        "Claude session is not currently running",
                    ));
                }
                SessionRuntime::None => {
                    return Err(ApiError::conflict(
                        "Claude session is not currently running",
                    ));
                }
                SessionRuntime::Acp(_) => {
                    return Err(ApiError::conflict(
                        "Claude session is not currently running",
                    ));
                }
            };
            claude_runtime_action = Some((handle, pending));
        } else if record.session.agent == Agent::Codex
            && matches!(
                decision,
                ApprovalDecision::Accepted
                    | ApprovalDecision::AcceptedForSession
                    | ApprovalDecision::Rejected
            )
        {
            let pending = record
                .pending_codex_approvals
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("approval request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Codex(handle) => handle.clone(),
                SessionRuntime::Claude(_) | SessionRuntime::None | SessionRuntime::Acp(_) => {
                    return Err(ApiError::conflict("Codex session is not currently running"));
                }
            };
            codex_runtime_action = Some((handle, pending));
        } else if matches!(record.session.agent, Agent::Cursor | Agent::Gemini)
            && matches!(
                decision,
                ApprovalDecision::Accepted
                    | ApprovalDecision::AcceptedForSession
                    | ApprovalDecision::Rejected
            )
        {
            let pending = record
                .pending_acp_approvals
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("approval request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Acp(handle) => handle.clone(),
                SessionRuntime::Claude(_) | SessionRuntime::Codex(_) | SessionRuntime::None => {
                    return Err(ApiError::conflict("agent session is not currently running"));
                }
            };
            acp_runtime_action = Some((handle, pending));
        }

        drop(inner);

        if let Some((handle, pending)) = claude_runtime_action {
            if decision == ApprovalDecision::AcceptedForSession {
                if let Some(mode) = pending.permission_mode_for_session.clone() {
                    handle
                        .input_tx
                        .send(ClaudeRuntimeCommand::SetPermissionMode(mode))
                        .map_err(|err| {
                            ApiError::internal(format!(
                                "failed to update Claude permission mode: {err}"
                            ))
                        })?;
                }
            }

            let response = match decision {
                ApprovalDecision::Accepted | ApprovalDecision::AcceptedForSession => {
                    ClaudePermissionDecision::Allow {
                        request_id: pending.request_id.clone(),
                        updated_input: pending.tool_input.clone(),
                    }
                }
                ApprovalDecision::Rejected => ClaudePermissionDecision::Deny {
                    request_id: pending.request_id.clone(),
                    message: "User rejected this action in TermAl.".to_owned(),
                },
                ApprovalDecision::Pending
                | ApprovalDecision::Interrupted
                | ApprovalDecision::Canceled => {
                    unreachable!("non-deliverable approval decisions are not sent")
                }
            };

            handle
                .input_tx
                .send(ClaudeRuntimeCommand::PermissionResponse(response))
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to deliver approval response to Claude: {err}"
                    ))
                })?;
        }
        if let Some((handle, pending)) = codex_runtime_action {
            handle
                .input_tx
                .send(CodexRuntimeCommand::JsonRpcResponse {
                    response: CodexJsonRpcResponseCommand {
                        request_id: pending.request_id.clone(),
                        payload: CodexJsonRpcResponsePayload::Result(codex_approval_result(
                            &pending.kind,
                            decision,
                        )),
                    },
                })
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to deliver approval response to Codex: {err}"
                    ))
                })?;
        }
        if let Some((handle, pending)) = acp_runtime_action {
            let option_id = match decision {
                ApprovalDecision::Accepted => pending
                    .allow_once_option_id
                    .clone()
                    .or_else(|| pending.allow_always_option_id.clone())
                    .or_else(|| pending.reject_option_id.clone()),
                ApprovalDecision::AcceptedForSession => pending
                    .allow_always_option_id
                    .clone()
                    .or_else(|| pending.allow_once_option_id.clone())
                    .or_else(|| pending.reject_option_id.clone()),
                ApprovalDecision::Rejected => pending
                    .reject_option_id
                    .clone()
                    .or_else(|| pending.allow_once_option_id.clone())
                    .or_else(|| pending.allow_always_option_id.clone()),
                ApprovalDecision::Pending
                | ApprovalDecision::Interrupted
                | ApprovalDecision::Canceled => None,
            }
            .ok_or_else(|| {
                ApiError::conflict("no approval option is available for this request")
            })?;

            handle
                .input_tx
                .send(AcpRuntimeCommand::JsonRpcMessage(
                    json_rpc_result_response_message(
                        pending.request_id.clone(),
                        json!({
                            "outcome": {
                                "outcome": "selected",
                                "optionId": option_id,
                            }
                        }),
                    ),
                ))
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to deliver approval response to agent session: {err}"
                    ))
                })?;
        }

        self.commit_interaction_message_update(session_id, message_id, |record| {
            let message_index = set_approval_decision_on_record(record, message_id, decision)
                .map_err(|_| ApiError::not_found("approval message not found"))?;

            if decision != ApprovalDecision::Pending {
                record.pending_claude_approvals.remove(message_id);
                record.pending_codex_approvals.remove(message_id);
                record.pending_acp_approvals.remove(message_id);
            }
            sync_session_interaction_state(
                record,
                approval_preview_text(record.session.agent.name(), decision),
            );
            Ok(message_index)
        })
    }

    /// Submits user-input-request answers back to Codex. Looks up the
    /// pending entry in `pending_codex_user_inputs` by message_id,
    /// runs `validate_codex_user_input_answers` (from
    /// `src/codex_validation.rs`) to normalize + schema-check each
    /// answer against the questions, and dispatches a JSON-RPC
    /// `sendResponse` with `result = { "answers": <per-question
    /// answers map> }` via `CodexRuntimeCommand::JsonRpcResponse`.
    fn submit_codex_user_input(
        &self,
        session_id: &str,
        message_id: &str,
        answers: BTreeMap<String, Vec<String>>,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_submit_codex_user_input(session_id, message_id, answers);
        }

        let (handle, pending, response_answers, display_answers) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
            if record.session.status != SessionStatus::Approval {
                return Err(ApiError::conflict(
                    "session is not currently waiting for input",
                ));
            }
            if record.session.agent != Agent::Codex {
                return Err(ApiError::conflict(
                    "only Codex sessions currently support structured user input",
                ));
            }

            let pending = record
                .pending_codex_user_inputs
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("user input request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Codex(handle) => handle.clone(),
                SessionRuntime::Claude(_) | SessionRuntime::None | SessionRuntime::Acp(_) => {
                    return Err(ApiError::conflict("Codex session is not currently running"));
                }
            };
            let (response_answers, display_answers) =
                validate_codex_user_input_answers(&pending.questions, answers)?;
            (handle, pending, response_answers, display_answers)
        };

        handle
            .input_tx
            .send(CodexRuntimeCommand::JsonRpcResponse {
                response: CodexJsonRpcResponseCommand {
                    request_id: pending.request_id.clone(),
                    payload: CodexJsonRpcResponsePayload::Result(
                        json!({ "answers": response_answers }),
                    ),
                },
            })
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to deliver user input response to Codex: {err}"
                ))
            })?;

        self.commit_interaction_message_update(session_id, message_id, |record| {
            let message_index = set_user_input_request_state_on_record(
                record,
                message_id,
                InteractionRequestState::Submitted,
                Some(display_answers),
            )
            .map_err(|_| ApiError::not_found("user input request not found"))?;
            record.pending_codex_user_inputs.remove(message_id);
            sync_session_interaction_state(
                record,
                user_input_request_preview_text(
                    record.session.agent.name(),
                    InteractionRequestState::Submitted,
                ),
            );
            Ok(message_index)
        })
    }

    /// Submits an MCP elicitation response back to Codex. Looks up
    /// the pending entry in `pending_codex_mcp_elicitations` by
    /// message_id, runs
    /// `validate_codex_mcp_elicitation_submission` (from
    /// `src/codex_validation.rs`) to validate the action
    /// (Accept/Decline/Cancel) against the request mode (URL vs.
    /// Form) and, for Accept + Form, walks the form content against
    /// the requested schema. Dispatches a JSON-RPC `sendResponse`
    /// with `result = { "action": <action>, "content":
    /// <normalized_content> }`.
    fn submit_codex_mcp_elicitation(
        &self,
        session_id: &str,
        message_id: &str,
        action: McpElicitationAction,
        content: Option<Value>,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_submit_codex_mcp_elicitation(
                session_id, message_id, action, content,
            );
        }

        let (handle, pending, normalized_content) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
            if record.session.status != SessionStatus::Approval {
                return Err(ApiError::conflict(
                    "session is not currently waiting for input",
                ));
            }
            if record.session.agent != Agent::Codex {
                return Err(ApiError::conflict(
                    "only Codex sessions currently support MCP elicitation input",
                ));
            }

            let pending = record
                .pending_codex_mcp_elicitations
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("MCP elicitation request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Codex(handle) => handle.clone(),
                SessionRuntime::Claude(_) | SessionRuntime::None | SessionRuntime::Acp(_) => {
                    return Err(ApiError::conflict("Codex session is not currently running"));
                }
            };
            let normalized_content =
                validate_codex_mcp_elicitation_submission(&pending.request, action, content)?;
            (handle, pending, normalized_content)
        };

        handle
            .input_tx
            .send(CodexRuntimeCommand::JsonRpcResponse {
                response: CodexJsonRpcResponseCommand {
                    request_id: pending.request_id.clone(),
                    payload: CodexJsonRpcResponsePayload::Result(json!({
                        "action": action,
                        "content": normalized_content
                    })),
                },
            })
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to deliver MCP elicitation response to Codex: {err}"
                ))
            })?;

        self.commit_interaction_message_update(session_id, message_id, |record| {
            let message_index = set_mcp_elicitation_request_state_on_record(
                record,
                message_id,
                InteractionRequestState::Submitted,
                Some(action),
                normalized_content.clone(),
            )
            .map_err(|_| ApiError::not_found("MCP elicitation request not found"))?;
            record.pending_codex_mcp_elicitations.remove(message_id);
            sync_session_interaction_state(
                record,
                mcp_elicitation_request_preview_text(
                    record.session.agent.name(),
                    InteractionRequestState::Submitted,
                    Some(action),
                ),
            );
            Ok(message_index)
        })
    }

    /// Submits a generic Codex app-request result back. Runs
    /// `validate_codex_app_request_result` (from
    /// `src/codex_validation.rs`) first to enforce the byte-size +
    /// depth caps, then looks up the pending entry in
    /// `pending_codex_app_requests` by message_id and dispatches a
    /// JSON-RPC `sendResponse` carrying the caller's result as the
    /// `result` field verbatim.
    fn submit_codex_app_request(
        &self,
        session_id: &str,
        message_id: &str,
        result: Value,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_submit_codex_app_request(session_id, message_id, result);
        }
        let result = validate_codex_app_request_result(result)?;

        let (handle, pending) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
            if record.session.status != SessionStatus::Approval {
                return Err(ApiError::conflict(
                    "session is not currently waiting for a Codex request response",
                ));
            }
            if record.session.agent != Agent::Codex {
                return Err(ApiError::conflict(
                    "only Codex sessions currently support generic app-server requests",
                ));
            }

            let pending = record
                .pending_codex_app_requests
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("Codex app request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Codex(handle) => handle.clone(),
                SessionRuntime::Claude(_) | SessionRuntime::None | SessionRuntime::Acp(_) => {
                    return Err(ApiError::conflict("Codex session is not currently running"));
                }
            };
            (handle, pending)
        };

        handle
            .input_tx
            .send(CodexRuntimeCommand::JsonRpcResponse {
                response: CodexJsonRpcResponseCommand {
                    request_id: pending.request_id.clone(),
                    payload: CodexJsonRpcResponsePayload::Result(result.clone()),
                },
            })
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to deliver generic Codex app request response: {err}"
                ))
            })?;

        self.commit_interaction_message_update(session_id, message_id, |record| {
            let message_index = set_codex_app_request_state_on_record(
                record,
                message_id,
                InteractionRequestState::Submitted,
                Some(result),
            )
            .map_err(|_| ApiError::not_found("Codex app request not found"))?;
            record.pending_codex_app_requests.remove(message_id);
            sync_session_interaction_state(
                record,
                codex_app_request_preview_text(
                    record.session.agent.name(),
                    InteractionRequestState::Submitted,
                ),
            );
            Ok(message_index)
        })
    }

    /// Transitions the session to `SessionStatus::Error` with an
    /// error message and dispatches the next queued prompt. Distinct
    /// from `fail_turn_if_runtime_matches` in `src/turn_lifecycle.rs`:
    /// this variant fires regardless of any `RuntimeToken`, because
    /// the error source is outside a specific runtime context (e.g.,
    /// the submission itself failed — the runtime command channel
    /// rejected our send, or the runtime handle was cleared before
    /// delivery). The runtime-token-guarded variant is used from the
    /// runtime event handlers where stale tokens must silently no-op.
    fn fail_turn(&self, session_id: &str, error_message: &str) -> Result<()> {
        let cleaned = error_message.trim();
        if !cleaned.is_empty() {
            self.push_message(
                session_id,
                Message::Text {
                    attachments: Vec::new(),
                    id: self.allocate_message_id(),
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: format!("Turn failed: {cleaned}"),
                    expanded_text: None,
                },
            )?;
        }

        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let file_change_message_id = (!inner.sessions[index].active_turn_file_changes.is_empty())
                .then(|| inner.next_message_id());
            let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
            record.session.status = SessionStatus::Error;
            record.session.preview = make_preview(cleaned);
            if let Some(message_id) = file_change_message_id {
                push_active_turn_file_changes_on_record(record, message_id);
            }
            finish_active_turn_file_change_tracking(record);
            self.commit_locked(&mut inner)?;
        }

        if let Some(dispatch) = self.dispatch_next_queued_turn(session_id, false)? {
            deliver_turn_dispatch(self, dispatch).map_err(|err| {
                anyhow!("failed to deliver queued turn dispatch: {}", err.message)
            })?;
        }
        Ok(())
    }
}

/// Builds the Codex-shaped approval `result` payload for each
/// `CodexApprovalKind`. `CommandExecution` and `FileChange` produce
/// `{ "decision": "accept" | "acceptForSession" | "decline" }`.
/// `Permissions` produces `{ "permissions": <requested_permissions
/// on accept, {} on reject>, "scope": "session" | "turn" }`. Panics
/// (via `unreachable!`) on `Pending` / `Interrupted` / `Canceled` —
/// those decisions are never delivered to Codex; `update_approval`
/// rejects them at the entry point.
fn codex_approval_result(kind: &CodexApprovalKind, decision: ApprovalDecision) -> Value {
    match kind {
        CodexApprovalKind::CommandExecution => match decision {
            ApprovalDecision::Accepted => json!({ "decision": "accept" }),
            ApprovalDecision::AcceptedForSession => json!({ "decision": "acceptForSession" }),
            ApprovalDecision::Rejected => json!({ "decision": "decline" }),
            ApprovalDecision::Pending
            | ApprovalDecision::Interrupted
            | ApprovalDecision::Canceled => {
                unreachable!("non-deliverable approval decisions are not sent to Codex")
            }
        },
        CodexApprovalKind::FileChange => match decision {
            ApprovalDecision::Accepted => json!({ "decision": "accept" }),
            ApprovalDecision::AcceptedForSession => json!({ "decision": "acceptForSession" }),
            ApprovalDecision::Rejected => json!({ "decision": "decline" }),
            ApprovalDecision::Pending
            | ApprovalDecision::Interrupted
            | ApprovalDecision::Canceled => {
                unreachable!("non-deliverable approval decisions are not sent to Codex")
            }
        },
        CodexApprovalKind::Permissions {
            requested_permissions,
        } => {
            let permissions = match decision {
                ApprovalDecision::Accepted | ApprovalDecision::AcceptedForSession => {
                    requested_permissions.clone()
                }
                ApprovalDecision::Rejected => json!({}),
                ApprovalDecision::Pending
                | ApprovalDecision::Interrupted
                | ApprovalDecision::Canceled => {
                    unreachable!("non-deliverable approval decisions are not sent to Codex")
                }
            };
            let scope = match decision {
                ApprovalDecision::AcceptedForSession => "session",
                ApprovalDecision::Accepted | ApprovalDecision::Rejected => "turn",
                ApprovalDecision::Pending
                | ApprovalDecision::Interrupted
                | ApprovalDecision::Canceled => {
                    unreachable!("non-deliverable approval decisions are not sent to Codex")
                }
            };
            json!({
                "permissions": permissions,
                "scope": scope,
            })
        }
    }
}


