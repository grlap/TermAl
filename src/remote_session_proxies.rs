// Remote session action proxies — forward session-scoped mutations
// to the remote backend that owns the real record.
//
// Every method here follows the same three-step shape (the
// "uniform remote session proxy" pattern documented in
// `remote_routes.rs`):
//
// 1. Resolve the local proxy session id to a `RemoteSessionTarget`
//    (`remote_id` + `remote_session_id`); bad_request if the
//    session isn't remote-bound.
// 2. Forward the call to the remote's matching endpoint via one of
//    the `remote_get/post/put_json*` helpers in `remote_routes.rs`.
// 3. Fold the returned `StateResponse` back into local state via
//    `sync_remote_state_for_target`, then return `self.snapshot()`.
//
// Covered routes:
//
// - `proxy_remote_session_settings` — PUTs
//   `UpdateSessionSettingsRequest` to update the remote session's
//   per-session preferences.
// - `proxy_remote_refresh_session_model_options` — asks the remote
//   to re-read the list of models available to the session.
// - `proxy_remote_turn_dispatch` — forwards a `send_message`
//   payload + attachments to the remote's turn dispatcher.
// - `proxy_remote_cancel_queued_prompt` — cancels the queued
//   prompt at `message_id` on the remote.
// - `proxy_remote_stop_session` / `proxy_remote_kill_session` — the
//   graceful and hard-kill variants respectively.
// - `proxy_remote_update_approval` — answers a pending approval
//   with the user's decision.
// - `proxy_remote_submit_codex_user_input` /
//   `proxy_remote_submit_codex_mcp_elicitation` /
//   `proxy_remote_submit_codex_app_request` — forward the three
//   Codex-specific answer-back submission paths.
// - `proxy_remote_list_agent_commands` /
//   `proxy_remote_search_instructions` — the two read-side
//   proxies that don't mutate remote state (pure GETs, no
//   sync_remote_state_for_target call).

impl AppState {

    fn proxy_remote_session_settings(
        &self,
        session_id: &str,
        request: UpdateSessionSettingsRequest,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/settings",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            Some(json!({
                "name": request.name,
                "model": request.model,
                "approvalPolicy": request.approval_policy,
                "reasoningEffort": request.reasoning_effort,
                "sandboxMode": request.sandbox_mode,
                "cursorMode": request.cursor_mode,
                "claudeApprovalMode": request.claude_approval_mode,
                "claudeEffort": request.claude_effort,
                "geminiApprovalMode": request.gemini_approval_mode,
            })),
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    fn proxy_remote_refresh_session_model_options(
        &self,
        session_id: &str,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/model-options/refresh",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            None,
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    fn proxy_remote_turn_dispatch(
        &self,
        session_id: &str,
        request: SendMessageRequest,
    ) -> Result<(), ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/messages",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            Some(json!({
                "text": request.text,
                "expandedText": request.expanded_text,
                "attachments": request.attachments,
            })),
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(())
    }

    fn proxy_remote_cancel_queued_prompt(
        &self,
        session_id: &str,
        prompt_id: &str,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/queued-prompts/{}/cancel",
                encode_uri_component(&target.remote_session_id),
                encode_uri_component(prompt_id)
            ),
            &[],
            None,
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    fn proxy_remote_stop_session(&self, session_id: &str) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/stop",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            None,
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    /// Unlike sibling session-state proxies, `kill` removes the local
    /// `SessionRecord` on success in addition to applying the returned
    /// `StateResponse`, since the remote session no longer exists after
    /// this call.
    fn proxy_remote_kill_session(&self, session_id: &str) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/kill",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            None,
        )?;
        let (snapshot, revision, wait_refresh) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let applied_remote_revision = apply_remote_state_if_newer_locked(
                &mut inner,
                &target.remote.id,
                &remote_state,
                Some(&target.remote_session_id),
                false,
            );
            let removed = if let Some(index) = inner.find_session_index(&target.local_session_id) {
                inner.remove_session_at(index);
                true
            } else {
                false
            };
            let wait_refresh = if removed {
                refresh_delegation_waits_locked(&mut inner)
            } else {
                DelegationWaitRefresh::default()
            };
            if applied_remote_revision {
                inner.note_remote_applied_revision(&target.remote.id, remote_state.revision);
            }
            let revision = if applied_remote_revision || removed || wait_refresh.did_mutate() {
                Some(self.commit_locked(&mut inner).map_err(|err| {
                    ApiError::internal(format!("failed to persist remote session removal: {err:#}"))
                })?)
            } else {
                None
            };
            (self.snapshot_from_inner(&inner), revision, wait_refresh)
        };
        if let Some(revision) = revision {
            if wait_refresh.did_mutate() {
                self.publish_delegation_wait_consumed_deltas(
                    revision,
                    &wait_refresh.consumed_waits,
                );
            }
            self.dispatch_delegation_wait_resumes(revision, wait_refresh.dispatch_parents);
        }
        Ok(snapshot)
    }

    fn proxy_remote_update_approval(
        &self,
        session_id: &str,
        message_id: &str,
        decision: ApprovalDecision,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/approvals/{}",
                encode_uri_component(&target.remote_session_id),
                encode_uri_component(message_id)
            ),
            &[],
            Some(json!({ "decision": decision })),
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    fn proxy_remote_submit_codex_user_input(
        &self,
        session_id: &str,
        message_id: &str,
        answers: BTreeMap<String, Vec<String>>,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/user-input/{}",
                encode_uri_component(&target.remote_session_id),
                encode_uri_component(message_id)
            ),
            &[],
            Some(json!({ "answers": answers })),
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    fn proxy_remote_submit_codex_mcp_elicitation(
        &self,
        session_id: &str,
        message_id: &str,
        action: McpElicitationAction,
        content: Option<Value>,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/mcp-elicitation/{}",
                encode_uri_component(&target.remote_session_id),
                encode_uri_component(message_id)
            ),
            &[],
            Some(json!({
                "action": action,
                "content": content,
            })),
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    fn proxy_remote_submit_codex_app_request(
        &self,
        session_id: &str,
        message_id: &str,
        result: Value,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/codex/requests/{}",
                encode_uri_component(&target.remote_session_id),
                encode_uri_component(message_id)
            ),
            &[],
            Some(json!({ "result": result })),
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    fn proxy_remote_list_agent_commands(
        &self,
        session_id: &str,
    ) -> Result<AgentCommandsResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        self.remote_registry.request_json(
            &target.remote,
            Method::GET,
            &format!(
                "/api/sessions/{}/agent-commands",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            None,
        )
    }

    fn proxy_remote_resolve_agent_command(
        &self,
        session_id: &str,
        command_name: &str,
        request: ResolveAgentCommandRequest,
    ) -> Result<ResolveAgentCommandResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let body = serde_json::to_value(request).map_err(|err| {
            ApiError::internal(format!("failed to encode agent command resolve request: {err}"))
        })?;
        self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/agent-commands/{}/resolve",
                encode_uri_component(&target.remote_session_id),
                encode_uri_component(command_name)
            ),
            &[],
            Some(body),
        )
    }

    fn proxy_remote_search_instructions(
        &self,
        session_id: &str,
        query: &str,
    ) -> Result<InstructionSearchResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        self.remote_registry.request_json(
            &target.remote,
            Method::GET,
            "/api/instructions/search",
            &[
                ("q".to_owned(), query.to_owned()),
                ("sessionId".to_owned(), target.remote_session_id),
            ],
            None,
        )
    }
}
