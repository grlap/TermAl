// codex thread actions — fork, archive, unarchive, compact, rollback.
//
// codex threads are codex cli's native unit of session history: each
// codex conversation is a thread identified by a stable id and codex
// owns the canonical transcript. termal mirrors it locally and exposes
// thread-level operations in the ui — "fork thread" branches a new
// local session off an existing thread (like a git branch on a
// conversation), "archive" / "unarchive" hide a thread without deleting
// it, "compact" asks codex to summarize old turns to free context, and
// "rollback" rewinds the thread n turns (conversation undo).
//
// all five actions fan out through `perform_codex_json_rpc_request`
// (see `src/state.rs`), which issues one json-rpc call over the shared
// codex app-server runtime (see `src/codex.rs` for the spawn path and
// `src/codex_rpc.rs` for the request/response helpers). remote-backed
// sessions short-circuit to an upstream termal before local dispatch.
//
// live idle thread guard. every action resolves its source through
// `resolve_codex_thread_action_context` (see `src/state.rs`), which
// rejects requests against sessions that are running a turn, awaiting
// approval, have queued prompts, or never started a thread — so a
// thread can't be mutated mid-turn with codex and the local transcript
// drifting apart.
//
// fork history-unavailable fallback. if codex returns a forked thread
// without a `turns` array, termal still creates the new local session
// but pushes a markdown note explaining the earlier transcript could
// not be backfilled and new prompts continue from this point.
//
// rollback contract. `numTurns` counts trailing turns to drop. codex
// truncates server-side and returns the updated thread; termal replaces
// the local message list with the returned history (or falls back to a
// note) and forces status back to `Idle`. the next prompt reattaches
// via `thread/resume` (see `src/turns.rs`).
//
// the methods below sit in their own `impl AppState { ... }` block —
// rust allows multiple impl blocks per type across `include!()`
// fragments, which is how this flat backend module is assembled. tests
// pinning these behaviors live in `src/tests/codex_threads.rs`.

impl AppState {
    /// Branches a new local session off an existing codex thread.
    ///
    /// sends `thread/fork` to codex with the source `threadId`; codex
    /// returns the new thread's id plus (optionally) its replayed turn
    /// history, inherited model / approval policy / sandbox mode /
    /// reasoning effort, and working directory. termal mints a fresh
    /// local codex session, rebuilds typed messages from the returned
    /// turns via `codex_thread_messages_from_json`, and persists a
    /// `SessionCreated` delta.
    ///
    /// if codex omits the turn history, the forked session is created
    /// anyway and a markdown note is pushed onto it explaining that the
    /// earlier transcript could not be replayed and that new prompts
    /// continue on the forked thread from this point.
    ///
    /// the live-idle-thread guard rejects forks from a session that is
    /// mid-turn, awaiting approval, has queued prompts, or has never
    /// started a thread — all cases where the source thread id or
    /// transcript would be inconsistent with the fork result.
    fn fork_codex_thread(
        &self,
        session_id: &str,
    ) -> std::result::Result<CreateSessionResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_fork_codex_thread(session_id);
        }

        let context = self.resolve_codex_thread_action_context(session_id)?;
        let fork_result = self.perform_codex_json_rpc_request(
            "thread/fork",
            json!({
                "threadId": context.thread_id,
            }),
            Duration::from_secs(30),
        )?;

        let fork_thread_id = fork_result
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::internal("Codex thread fork did not return a thread id"))?
            .to_owned();
        let fork_name = default_forked_codex_session_name(
            &context.name,
            fork_result.pointer("/thread/name").and_then(Value::as_str),
        );
        let fork_model = fork_result
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(&context.model)
            .to_owned();
        let fork_workdir = resolve_forked_codex_workdir(
            fork_result.get("cwd").and_then(Value::as_str),
            &context.workdir,
            context.project_id.as_deref(),
            self,
        )?;
        let approval_policy = fork_result
            .get("approvalPolicy")
            .and_then(codex_approval_policy_from_json_value)
            .unwrap_or(context.approval_policy);
        let sandbox_mode = fork_result
            .get("sandbox")
            .and_then(codex_sandbox_mode_from_json_value)
            .unwrap_or(context.sandbox_mode);
        let reasoning_effort = fork_result
            .get("reasoningEffort")
            .and_then(codex_reasoning_effort_from_json_value)
            .unwrap_or(context.reasoning_effort);
        let fork_preview = fork_result
            .pointer("/thread/preview")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(make_preview);
        let fork_thread = fork_result
            .get("thread")
            .ok_or_else(|| ApiError::internal("Codex thread fork did not return a thread"))?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let fork_messages = codex_thread_messages_from_json(&mut inner, fork_thread);
        let mut record = inner.create_session(
            Agent::Codex,
            Some(fork_name),
            fork_workdir,
            context.project_id.clone(),
            Some(fork_model),
        );
        record.session.model_options = context.model_options.clone();
        record.codex_approval_policy = approval_policy;
        record.session.approval_policy = Some(approval_policy);
        record.codex_sandbox_mode = sandbox_mode;
        record.session.sandbox_mode = Some(sandbox_mode);
        record.codex_reasoning_effort = reasoning_effort;
        record.session.reasoning_effort = Some(reasoning_effort);
        set_record_external_session_id(&mut record, Some(fork_thread_id.clone()));
        if let Some(fork_messages) = fork_messages {
            replace_session_messages_on_record(&mut record, fork_messages, fork_preview);
        } else {
            let note_message_id = inner.next_message_id();
            push_session_markdown_note_on_record(
                &mut record,
                note_message_id,
                "Forked Codex thread",
                format!(
                    "Forked from `{}` into live Codex thread `{}`.\n\nPreview: {}\n\nCodex did not return the earlier thread history for this fork, so TermAl could not backfill the transcript. New prompts here continue on the forked thread from this point forward.",
                    context.name,
                    fork_thread_id,
                    fork_preview
                        .as_deref()
                        .unwrap_or("No thread preview was returned.")
                ),
            );
        }

        if let Some(index) = inner.find_session_index(&record.session.id) {
            if let Some(slot) = inner.sessions.get_mut(index) {
                *slot = record.clone();
            }
            // See `create_session`: re-stamp the record after the
            // whole-struct replace so the persist thread picks up the
            // rewrite instead of skipping it at the delta watermark.
            let _ = inner.session_mut_by_index(index);
        }
        let revision = self.commit_session_created_locked(&mut inner, &record).map_err(|err| {
            ApiError::internal(format!("failed to persist forked Codex session: {err:#}"))
        })?;
        let session = record.session.clone();
        drop(inner);
        self.publish_delta(&DeltaEvent::SessionCreated {
            revision,
            session_id: session.id.clone(),
            session: session.clone(),
        });

        Ok(CreateSessionResponse {
            session_id: session.id.clone(),
            session: Some(session),
            revision,
            state: None,
        })
    }

    /// Hides a codex thread from the ui without killing its history.
    ///
    /// sends `thread/archive` to codex for the session's thread id and,
    /// on success, flips the local `codex_thread_state` to `Archived`
    /// and pushes a markdown note telling the user to unarchive before
    /// sending more prompts. the codex-side thread is preserved; only
    /// the termal-side visibility and dispatch eligibility change.
    ///
    /// the live-idle-thread guard rejects archiving while a turn is in
    /// flight — archiving mid-turn would let a turn complete against a
    /// thread the ui has already hidden. an already-archived thread
    /// returns a 409 conflict rather than being re-archived.
    fn archive_codex_thread(
        &self,
        session_id: &str,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_archive_codex_thread(session_id);
        }

        let context = self.resolve_codex_thread_action_context(session_id)?;
        if context.thread_state == Some(CodexThreadState::Archived) {
            return Err(ApiError::conflict(
                "the current Codex thread is already archived",
            ));
        }
        self.perform_codex_json_rpc_request(
            "thread/archive",
            json!({
                "threadId": context.thread_id,
            }),
            Duration::from_secs(30),
        )?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let note_message_id = inner.next_message_id();
        set_record_codex_thread_state(inner
            .session_mut_by_index(index)
            .expect("session index should be valid"), CodexThreadState::Archived);
        push_session_markdown_note_on_record(
            inner
            .session_mut_by_index(index)
            .expect("session index should be valid"),
            note_message_id,
            "Archived Codex thread",
            format!(
                "Archived the live Codex thread `{}`.\n\nUse **Unarchive** to restore it later before sending more prompts.",
                context.thread_id
            ),
        );
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!(
                "failed to persist archived Codex thread note: {err:#}"
            ))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    /// Restores a previously archived codex thread.
    ///
    /// sends `thread/unarchive` to codex, flips the local
    /// `codex_thread_state` back to `Active`, and pushes a markdown
    /// note confirming the restoration so the session can resume taking
    /// prompts. a session whose thread is not currently archived returns
    /// a 409 conflict rather than issuing the request.
    ///
    /// the live-idle-thread guard still applies — the source session
    /// must be idle with no queued prompts so the state transition
    /// cannot race a concurrent turn or dispatch.
    fn unarchive_codex_thread(
        &self,
        session_id: &str,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_unarchive_codex_thread(session_id);
        }

        let context = self.resolve_codex_thread_action_context(session_id)?;
        if context.thread_state != Some(CodexThreadState::Archived) {
            return Err(ApiError::conflict(
                "the current Codex thread is not archived",
            ));
        }
        self.perform_codex_json_rpc_request(
            "thread/unarchive",
            json!({
                "threadId": context.thread_id,
            }),
            Duration::from_secs(30),
        )?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let note_message_id = inner.next_message_id();
        set_record_codex_thread_state(inner
            .session_mut_by_index(index)
            .expect("session index should be valid"), CodexThreadState::Active);
        push_session_markdown_note_on_record(
            inner
            .session_mut_by_index(index)
            .expect("session index should be valid"),
            note_message_id,
            "Restored Codex thread",
            format!(
                "Restored the archived Codex thread `{}` so the session can continue using it.",
                context.thread_id
            ),
        );
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!(
                "failed to persist restored Codex thread note: {err:#}"
            ))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    /// Asks codex to summarize older turns to free context window.
    ///
    /// sends `thread/compact/start` to codex; the server-side thread
    /// transitions to a compacted representation that relies on a
    /// summary for its older turns. the local termal transcript is left
    /// untouched (so the user still sees the full history in the ui)
    /// and a markdown note is appended explaining that the live codex
    /// thread now relies on a compacted summary internally.
    ///
    /// the live-idle-thread guard prevents compacting while a turn is
    /// in flight — compacting mid-turn would change the context the
    /// turn is running against.
    fn compact_codex_thread(
        &self,
        session_id: &str,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_compact_codex_thread(session_id);
        }

        let context = self.resolve_codex_thread_action_context(session_id)?;
        self.perform_codex_json_rpc_request(
            "thread/compact/start",
            json!({
                "threadId": context.thread_id,
            }),
            Duration::from_secs(30),
        )?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let note_message_id = inner.next_message_id();
        push_session_markdown_note_on_record(
            inner
            .session_mut_by_index(index)
            .expect("session index should be valid"),
            note_message_id,
            "Started Codex compaction",
            format!(
                "Started Codex context compaction for live thread `{}`.\n\nThe TermAl transcript stays intact, but the live Codex thread may now rely on a compacted summary internally.",
                context.thread_id
            ),
        );
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist Codex compaction note: {err:#}"))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    /// Rewinds the codex thread by `num_turns` turns (conversation undo).
    ///
    /// `num_turns` counts how many trailing turns to drop; zero is
    /// rejected as a bad request. sends `thread/rollback` with the
    /// thread id and turn count; codex truncates the server-side thread
    /// and returns the updated `thread` payload. termal rebuilds the
    /// local message list from the returned turns and replaces the
    /// session's transcript, then forces the session status back to
    /// `Idle` so the user can submit the next prompt (which reattaches
    /// to the shortened thread via `thread/resume`; see `src/turns.rs`).
    ///
    /// if codex returns no turn history, the local transcript is left
    /// in place and a markdown note is appended warning that the local
    /// transcript may no longer exactly match the live codex thread.
    ///
    /// the live-idle-thread guard rejects rollbacks from a session that
    /// is running, awaiting approval, or has queued prompts, so the
    /// truncation cannot race an in-flight turn.
    fn rollback_codex_thread(
        &self,
        session_id: &str,
        num_turns: usize,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_rollback_codex_thread(session_id, num_turns);
        }
        if num_turns == 0 {
            return Err(ApiError::bad_request("rollback requires at least one turn"));
        }

        let context = self.resolve_codex_thread_action_context(session_id)?;
        let rollback_result = self.perform_codex_json_rpc_request(
            "thread/rollback",
            json!({
                "threadId": context.thread_id,
                "numTurns": num_turns,
            }),
            Duration::from_secs(30),
        )?;
        let rollback_thread = rollback_result
            .get("thread")
            .ok_or_else(|| ApiError::internal("Codex thread rollback did not return a thread"))?;
        let rollback_preview = rollback_thread
            .get("preview")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(make_preview);

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let rollback_messages = codex_thread_messages_from_json(&mut inner, rollback_thread);
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        if let Some(rollback_messages) = rollback_messages {
            replace_session_messages_on_record(
                inner
            .session_mut_by_index(index)
            .expect("session index should be valid"),
                rollback_messages,
                rollback_preview,
            );
        } else {
            let turn_label = if num_turns == 1 { "turn" } else { "turns" };
            let note_message_id = inner.next_message_id();
            push_session_markdown_note_on_record(
                inner
            .session_mut_by_index(index)
            .expect("session index should be valid"),
                note_message_id,
                "Rolled back Codex thread",
                format!(
                    "Rolled back the live Codex thread `{}` by {} {}.\n\nCodex did not return the updated thread history for this rollback, so TermAl kept the earlier local transcript above. It may not exactly match the live Codex thread after this point.",
                    context.thread_id, num_turns, turn_label
                ),
            );
        }
        inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .session
            .status = SessionStatus::Idle;
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist Codex rollback state: {err:#}"))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }
}
