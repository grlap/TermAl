// User-facing response submission handlers. These methods exist because
// some agent events need the *user's* response — not just a recorded
// message. The agent emits an approval / user-input / MCP elicitation /
// app-request event; the frontend surfaces it, the user clicks a
// button, the browser POSTs to the matching HTTP route (see
// `src/main.rs` for routes under `/api/sessions/{id}/approvals/`,
// `/user-input/`, `/mcp-elicitation/`, `/codex/requests/`), and these
// methods route the response back into the still-running agent.
//
// Per-agent split: Claude uses a `ClaudePermissionDecision` or completed
// user-dialog response queued onto the runtime's input channel (ultimately
// an NDJSON control_response over the CLI pipe); Codex uses a JSON-RPC `sendResponse`
// whose `result` shape depends on the approval kind — built here by
// `codex_approval_result` and sent via `send_codex_json_rpc_request`
// from `src/codex_rpc.rs`; ACP uses an `AcpRuntimeCommand::JsonRpcMessage`
// with the agent's option-id selection protocol.
//
// The pending lookup fans out across all three agents' maps:
// `update_approval` searches `pending_claude_approvals` (keyed by
// message_id), `pending_codex_approvals`, and `pending_acp_approvals`
// to find the right response channel. `submit_user_input` handles both
// Claude and Codex; Claude answers are additionally routed by
// `ClaudeUserInputTransport` — the legacy `request_user_dialog` completion
// envelope, or (for questions that arrived as a `can_use_tool` permission
// request, the current AskUserQuestion contract) a permission allow whose
// `updatedInput.answers` carries the user's answers: exact question text
// as the key, a label string for single-select, and a label array for
// multi-select. The compatibility-only legacy dialog keeps its historical
// comma-joined multi-select encoding because that channel is not live-verified. A declined
// submission maps to a permission deny on that same channel (permission
// transport only) and resolves the card as declined.
// The remaining `submit_codex_*` methods look up their
// Codex-specific maps (`pending_codex_mcp_elicitations` / `pending_codex_app_requests`),
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

/// Written onto the declined card alongside the semantic `Declined` state so
/// the transcript clearly explains how a user Skip differs from an agent-side
/// cancel.
const CLAUDE_USER_SKIPPED_QUESTIONS_DETAIL: &str =
    "The user skipped these questions; Claude was asked to decide on its own.";

enum PendingUserInputRuntimeAction {
    Claude {
        handle: ClaudeRuntimeHandle,
        pending: ClaudePendingUserInput,
        command: ClaudeRuntimeCommand,
    },
    Codex {
        handle: CodexRuntimeHandle,
        pending: CodexPendingUserInput,
        response_answers: BTreeMap<String, BTreeMap<String, Vec<String>>>,
    },
}

/// Maps a completed Claude user-input response onto the stdin channel the
/// request arrived on: the legacy dialog envelope, or — for questions that
/// arrived as a `can_use_tool` permission request — an allow decision whose
/// `updatedInput` carries the answers.
fn claude_user_input_runtime_command(
    transport: ClaudeUserInputTransport,
    response: ClaudeUserInputResponse,
) -> ClaudeRuntimeCommand {
    match transport {
        ClaudeUserInputTransport::Dialog => ClaudeRuntimeCommand::UserInputResponse(response),
        ClaudeUserInputTransport::Permission => {
            ClaudeRuntimeCommand::PermissionResponse(ClaudePermissionDecision::Allow {
                request_id: response.request_id,
                updated_input: response.updated_input,
            })
        }
    }
}

/// Maps a declined Claude user-input request onto a permission deny. Only
/// the permission transport can carry a decline; callers must have checked
/// `pending_claude_user_input_is_declinable` first.
fn claude_user_input_decline_runtime_command(
    pending: &ClaudePendingUserInput,
) -> ClaudeRuntimeCommand {
    ClaudeRuntimeCommand::PermissionResponse(ClaudePermissionDecision::Deny {
        request_id: pending.request_id.clone(),
        message: CLAUDE_USER_DECLINED_QUESTION_MESSAGE.to_owned(),
    })
}

impl PendingUserInputRuntimeAction {
    fn send(&self) -> std::result::Result<(), ApiError> {
        match self {
            Self::Claude {
                handle,
                command,
                pending: _,
            } => handle.input_tx.send(command.clone()).map_err(|err| {
                ApiError::internal(format!(
                    "failed to deliver user input response to Claude: {err}"
                ))
            }),
            Self::Codex {
                handle,
                pending,
                response_answers,
            } => handle
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
                }),
        }
    }

    fn restore_claim(&self, record: &mut SessionRecord, message_id: &str) {
        match self {
            Self::Claude { pending, .. } => {
                record
                    .pending_claude_user_inputs
                    .entry(message_id.to_owned())
                    .or_insert_with(|| pending.clone());
            }
            Self::Codex { pending, .. } => {
                record
                    .pending_codex_user_inputs
                    .entry(message_id.to_owned())
                    .or_insert_with(|| pending.clone());
            }
        }
    }
}

fn validate_claude_user_input_answers(
    pending: &ClaudePendingUserInput,
    answers: BTreeMap<String, Vec<String>>,
) -> std::result::Result<(Value, BTreeMap<String, Vec<String>>), ApiError> {
    let question_ids: HashSet<&str> = pending
        .questions
        .iter()
        .map(|question| question.id.as_str())
        .collect();
    for answer_id in answers.keys() {
        if !question_ids.contains(answer_id.as_str()) {
            return Err(ApiError::bad_request(format!(
                "answer `{answer_id}` does not match any requested question"
            )));
        }
    }

    let mut claude_answers = serde_json::Map::new();
    let mut display_answers = BTreeMap::new();
    for question in &pending.questions {
        let raw_answers = answers.get(&question.id).ok_or_else(|| {
            ApiError::bad_request(format!(
                "question `{}` is missing an answer",
                question.header
            ))
        })?;
        let mut normalized_answers = Vec::new();
        for answer in raw_answers {
            // Option labels are protocol values on the permission transport:
            // preserve an exact label byte-for-byte so Claude can match it
            // back to its own option list. Free-form "Other" answers keep the
            // historical surrounding-whitespace normalization.
            let answer = if question.options.as_ref().is_some_and(|options| {
                options
                    .iter()
                    .any(|option| option.label == answer.as_str())
            }) {
                answer.as_str()
            } else {
                answer.trim()
            };
            if !answer.is_empty() && !normalized_answers.iter().any(|seen| seen == answer) {
                normalized_answers.push(answer.to_owned());
            }
        }
        let valid_count = if question.multi_select {
            !normalized_answers.is_empty()
        } else {
            normalized_answers.len() == 1
        };
        if !valid_count {
            return Err(ApiError::bad_request(format!(
                "question `{}` requires {}",
                question.header,
                if question.multi_select {
                    "at least one answer"
                } else {
                    "exactly one answer"
                }
            )));
        }

        if let Some(options) = question.options.as_ref() {
            let unknown_count = normalized_answers
                .iter()
                .filter(|answer| !options.iter().any(|option| option.label == answer.as_str()))
                .count();
            if unknown_count > usize::from(question.is_other) {
                return Err(ApiError::bad_request(format!(
                    "question `{}` contains answers outside the provided options",
                    question.header
                )));
            }
        }

        // The permission transport uses the live-verified answer encoding:
        // a label string for single-select and a JSON label array for
        // multi-select, keyed by exact question text. The compatibility-only
        // dialog channel has no current live capture, so preserve its
        // historical comma-joined multi-select shape rather than changing an
        // unverified contract.
        let answer_value = if question.multi_select {
            match pending.transport {
                ClaudeUserInputTransport::Permission => Value::Array(
                    normalized_answers
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
                ClaudeUserInputTransport::Dialog => {
                    Value::String(normalized_answers.join(", "))
                }
            }
        } else {
            Value::String(normalized_answers.first().cloned().ok_or_else(|| {
                ApiError::internal("Claude single-select answer normalization produced no answer")
            })?)
        };
        claude_answers.insert(question.question.clone(), answer_value);
        display_answers.insert(
            question.id.clone(),
            if question.is_secret {
                vec!["[secret provided]".to_owned()]
            } else {
                normalized_answers
            },
        );
    }

    let mut updated_input = pending.input.clone();
    let updated_input_object = updated_input.as_object_mut().ok_or_else(|| {
        // Pending input is runtime-owned, but return a typed error instead of
        // panicking under the AppState mutex if a future parser refactor ever
        // violates the object invariant.
        ApiError::internal("Claude pending user input is not a JSON object")
    })?;
    updated_input_object.insert("answers".to_owned(), Value::Object(claude_answers));
    Ok((updated_input, display_answers))
}

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
        {
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
                    global_message_index(record, message_index),
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
        };
        if let Err(err) = self.refresh_delegation_for_child_session(session_id) {
            eprintln!(
                "state warning> failed to refresh delegation after interaction response: {err:#}"
            );
        }
        Ok(self.summary_snapshot_with_session_detail(session_id))
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
        if matches!(
            decision,
            ApprovalDecision::Accepted | ApprovalDecision::AcceptedForSession
        ) {
            self.ensure_read_only_delegation_allows_write_action(
                Some(session_id),
                None,
                None,
                "approval acceptance",
            )?;
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
        } else if matches!(
            record.session.agent,
            Agent::Cursor | Agent::Gemini | Agent::OpenCode
        )
            && matches!(
                decision,
                ApprovalDecision::Accepted
                    | ApprovalDecision::AcceptedForSession
                    | ApprovalDecision::Rejected
            )
        {
            if record.session.agent == Agent::OpenCode {
                let next_message_id = record
                    .pending_acp_approval_order
                    .front()
                    .ok_or_else(|| ApiError::conflict("approval request is no longer live"))?;
                if next_message_id != message_id {
                    return Err(ApiError::conflict(
                        "an earlier agent approval request must be resolved first",
                    ));
                }
            }
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
            let option_id = acp_approval_option_id(&pending, decision).ok_or_else(|| {
                ApiError::conflict(
                    "the agent did not offer an option matching this approval decision",
                )
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
                record
                    .pending_acp_approval_order
                    .retain(|pending_message_id| pending_message_id != message_id);
            }
            sync_session_interaction_state(
                record,
                approval_preview_text(record.session.agent.name(), decision),
            );
            Ok(message_index)
        })
    }

    /// Submits a structured user-input response to the live Claude or Codex
    /// runtime that owns the transcript card. Claude receives either a
    /// completed `request_user_dialog` response or a permission allow/deny,
    /// according to the pending request's transport; Codex receives its
    /// JSON-RPC result.
    fn submit_user_input(
        &self,
        session_id: &str,
        message_id: &str,
        answers: BTreeMap<String, Vec<String>>,
        declined: bool,
    ) -> std::result::Result<StateResponse, ApiError> {
        // Validated before remote proxying so local and remote submissions
        // reject an answer-carrying decline identically.
        if declined && !answers.is_empty() {
            return Err(ApiError::bad_request(
                "a declined user input request cannot carry answers",
            ));
        }

        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_submit_user_input(session_id, message_id, answers, declined);
        }

        let display_answers = {
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

            let (action, display_answers) = match record.session.agent {
                Agent::Claude => {
                    let pending = record
                        .pending_claude_user_inputs
                        .get(message_id)
                        .cloned()
                        .ok_or_else(|| {
                            ApiError::conflict("user input request is no longer live")
                        })?;
                    let handle = match &record.runtime {
                        SessionRuntime::Claude(handle) => handle.clone(),
                        SessionRuntime::Codex(_)
                        | SessionRuntime::None
                        | SessionRuntime::Acp(_) => {
                            return Err(ApiError::conflict(
                                "Claude session is not currently running",
                            ));
                        }
                    };
                    let (command, display_answers) = if declined {
                        if !pending_claude_user_input_is_declinable(&pending) {
                            // Conflict, not bad-request: the payload is
                            // well-formed, it just targets a request whose
                            // transport has no decline envelope.
                            return Err(ApiError::conflict(
                                "the legacy Claude question dialog cannot be declined; answer the questions instead",
                            ));
                        }
                        (claude_user_input_decline_runtime_command(&pending), None)
                    } else {
                        let (updated_input, display_answers) =
                            validate_claude_user_input_answers(&pending, answers)?;
                        (
                            claude_user_input_runtime_command(
                                pending.transport,
                                ClaudeUserInputResponse {
                                    request_id: pending.request_id.clone(),
                                    updated_input,
                                },
                            ),
                            Some(display_answers),
                        )
                    };
                    let pending = record
                        .pending_claude_user_inputs
                        .remove(message_id)
                        .expect("validated Claude input should still be pending");
                    (
                        PendingUserInputRuntimeAction::Claude {
                            handle,
                            pending,
                            command,
                        },
                        display_answers,
                    )
                }
                Agent::Codex => {
                    if declined {
                        // Conflict, not bad-request: see the dialog-transport
                        // decline rejection above.
                        return Err(ApiError::conflict(
                            "Codex user input requests do not support declining; answer the questions instead",
                        ));
                    }
                    let pending = record
                        .pending_codex_user_inputs
                        .get(message_id)
                        .cloned()
                        .ok_or_else(|| {
                            ApiError::conflict("user input request is no longer live")
                        })?;
                    let handle = match &record.runtime {
                        SessionRuntime::Codex(handle) => handle.clone(),
                        SessionRuntime::Claude(_)
                        | SessionRuntime::None
                        | SessionRuntime::Acp(_) => {
                            return Err(ApiError::conflict(
                                "Codex session is not currently running",
                            ));
                        }
                    };
                    let (response_answers, display_answers) =
                        validate_codex_user_input_answers(&pending.questions, answers)?;
                    let pending = record
                        .pending_codex_user_inputs
                        .remove(message_id)
                        .expect("validated Codex input should still be pending");
                    (
                        PendingUserInputRuntimeAction::Codex {
                            handle,
                            pending,
                            response_answers,
                        },
                        Some(display_answers),
                    )
                }
                Agent::Cursor | Agent::Gemini | Agent::OpenCode => {
                    return Err(ApiError::conflict(
                        "this agent does not support structured user input",
                    ));
                }
            };
            // Delivery and claim removal form one critical section. Runtime
            // channels are nonblocking std mpsc senders, so keeping the state
            // mutex here prevents a second HTTP request from observing the
            // same pending interaction without introducing an await/lock
            // inversion. A failed send restores the claim before unlock.
            if let Err(err) = action.send() {
                action.restore_claim(record, message_id);
                return Err(err);
            }
            display_answers
        };

        // A decline resolves the card as declined with no recorded answers —
        // a durable state distinct from agent-side cancellation; an answered
        // submission resolves it as submitted with the answers.
        let submitted_state = if declined {
            InteractionRequestState::Declined
        } else {
            InteractionRequestState::Submitted
        };
        self.commit_interaction_message_update(session_id, message_id, |record| {
            let message_index = set_user_input_request_state_on_record(
                record,
                message_id,
                submitted_state,
                display_answers,
            )
            .map_err(|_| ApiError::not_found("user input request not found"))?;
            if declined {
                // The semantic Declined state is the durable audit
                // distinction; this detail explains that choice to the user.
                // Turn-end cancellations remain Canceled and keep their
                // original detail.
                if let Some(Message::UserInputRequest { detail, .. }) =
                    record.session.messages.get_mut(message_index)
                {
                    *detail = CLAUDE_USER_SKIPPED_QUESTIONS_DETAIL.to_owned();
                }
            }
            sync_session_interaction_state(
                record,
                user_input_request_preview_text(record.session.agent.name(), submitted_state),
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

}

/// Maps a user decision only to an ACP option with the same authorization
/// semantics. Missing protocol options fail closed at the API boundary:
/// rejection must never select an allow option, and approval must never select
/// a reject option.
fn acp_approval_option_id(
    pending: &AcpPendingApproval,
    decision: ApprovalDecision,
) -> Option<String> {
    match decision {
        ApprovalDecision::Accepted => pending.allow_once_option_id.clone(),
        ApprovalDecision::AcceptedForSession => pending.allow_always_option_id.clone(),
        ApprovalDecision::Rejected => pending.reject_option_id.clone(),
        ApprovalDecision::Pending
        | ApprovalDecision::Interrupted
        | ApprovalDecision::Canceled => None,
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
