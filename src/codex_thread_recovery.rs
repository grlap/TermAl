// Owns fail-closed recovery from definitive thread-store resume failures.
// Never edits rollout files or treats timeouts/transport loss as lost history.

fn is_lost_codex_rollout_error(error: &CodexResponseError) -> bool {
    let CodexResponseError::JsonRpc(detail) = error else {
        return false;
    };
    let detail = detail
        .strip_prefix("failed to read thread: ")
        .unwrap_or(detail);
    // Explicit thread-store diagnostics, not generic JSON-RPC/server errors.
    if detail.starts_with("no rollout found for thread id ") {
        return true;
    }
    let Some(detail) = detail.strip_prefix("thread-store internal error: ") else {
        return false;
    };
    let lower = detail.to_ascii_lowercase();
    if ["busy", "locked", "temporarily", "timed out", "timeout"]
        .iter()
        .any(|s| lower.contains(s))
    {
        return false;
    }
    if detail.starts_with("no rollout found for thread id ") {
        return true;
    }
    detail.starts_with("failed to read session metadata ")
        && ((detail.contains(": rollout at ") && detail.ends_with(" is empty"))
            || detail.contains("session metadata is missing")
            || detail.contains("failed to parse rollout line:")
            || detail.contains("failed to parse line as JSON:")
            || detail.ends_with("(os error 2)")
            || detail.ends_with("(os error 3)"))
}

fn lost_codex_thread_recovery_prompt(thread_id: &str, prompt: &str) -> String {
    format!(
        "<termal-thread-recovery>\nThe previous Codex thread `{thread_id}` could not be read. This is a fresh thread, not a restoration of its model context. The TermAl session and transcript are retained. Recover relevant context from the session transcript and configured project memory/coordination tools before continuing; do not invent missing history. Follow the project's normal startup instructions.\n</termal-thread-recovery>\n\n{prompt}"
    )
}

fn handle_shared_codex_lost_thread_recovery(
    writer: &mut impl Write,
    pending: &CodexPendingRequestMap,
    state: &AppState,
    runtime_id: &str,
    codex_home: &FsPath,
    sessions: &SharedCodexSessionMap,
    thread_sessions: &SharedCodexThreadMap,
    input_tx: &Sender<CodexRuntimeCommand>,
    writer_context: Option<&SharedCodexStdinContextState>,
    session_id: &str,
    old_request_id: &str,
    failed_thread_id: &str,
    detail: &str,
) -> Result<()> {
    // The failed resume keeps its slot while recovery is queued. Stop/detach
    // removes it; a newer prompt can replace only its parked command, not its
    // identity. Revalidate both identities before clearing persistent state.
    let transition = (|| -> Result<Option<(String, CodexThreadSetupRequest, u64)>> {
        let mut sessions = sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let Some(shared) = sessions.get_mut(session_id) else {
            return Ok(None);
        };
        let Some(setup) = shared
            .pending_thread_setup
            .as_mut()
            .filter(|setup| setup.request_id == old_request_id)
        else {
            return Ok(None);
        };
        let Some(lost_id) = setup.command.resume_thread_id.clone() else {
            return Ok(None);
        };
        if lost_id != failed_thread_id {
            return Ok(None);
        }
        let generation = setup.command.active_turn_generation;
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_visible_session_index(session_id) else {
            return Ok(None);
        };
        let record = &inner.sessions[index];
        if !record
            .runtime
            .matches_runtime_token(&RuntimeToken::Codex(runtime_id.to_owned()))
            || record.runtime_stop_in_progress
            || record.session.status != SessionStatus::Active
            || record.active_turn_generation != generation
            || record.external_session_id.as_deref() != Some(&lost_id)
        {
            return Ok(None);
        }
        let note_id = inner.next_message_id();
        let record = inner
            .session_mut_by_index(index)
            .expect("validated recovery session");
        set_record_external_session_id(record, None);
        push_session_markdown_note_on_record(
            record,
            note_id,
            "Recovered unreadable Codex thread",
            format!(
                "Codex could not resume thread `{lost_id}`: {detail}\n\nThe TermAl transcript is retained. Starting a fresh Codex thread for the current prompt; the old model context is unavailable."
            ),
        );
        state.commit_locked(&mut inner)?;
        drop(inner);
        setup.command.resume_thread_id = None;
        setup.command.prompt = lost_codex_thread_recovery_prompt(&lost_id, &setup.command.prompt);
        setup.request_id = Uuid::new_v4().to_string();
        let request = CodexThreadSetupRequest {
            approval_policy: setup.command.approval_policy,
            cwd: setup.command.cwd.clone(),
            model: setup.command.model.clone(),
            service_tier: setup.command.service_tier.clone(),
            resume_thread_id: None,
            sandbox_mode: setup.command.sandbox_mode,
        };
        let request_id = setup.request_id.clone();
        if shared.thread_id.as_deref() == Some(&lost_id) {
            shared.thread_id = None;
        }
        let mut mappings = thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned");
        if mappings
            .get(&lost_id)
            .is_some_and(|mapped| mapped == session_id)
        {
            mappings.remove(&lost_id);
        }
        Ok(Some((request_id, request, generation)))
    })();
    let (request_id, request, generation) = match transition {
        Ok(Some(next)) => next,
        Ok(None) => {
            // This releases only the failed request's slot and visibly fails
            // a still-current turn. Stop, newer generations and superseding
            // setup requests remain protected by the ordinary failure guards.
            handle_shared_codex_thread_setup_response_error_if_current(
                sessions,
                state,
                runtime_id,
                session_id,
                old_request_id,
                SHARED_CODEX_THREAD_SETUP_TIMEOUT,
                CodexResponseError::JsonRpc(detail.to_owned()),
            );
            return Ok(());
        }
        Err(error) => {
            handle_shared_codex_thread_setup_response_error_if_current(
                sessions,
                state,
                runtime_id,
                session_id,
                old_request_id,
                SHARED_CODEX_THREAD_SETUP_TIMEOUT,
                CodexResponseError::JsonRpc(format!(
                    "failed to persist lost-thread recovery: {error:#}"
                )),
            );
            return Ok(());
        }
    };
    // Reuse normal new-thread setup, including MCP configuration and the
    // config/read merge that preserves effective instructions for Engram.
    let result = start_shared_codex_thread_setup_request(
        writer,
        pending,
        state,
        runtime_id,
        codex_home,
        sessions,
        thread_sessions,
        input_tx,
        writer_context,
        session_id,
        request_id,
        request,
    );
    handle_shared_codex_prompt_command_result(
        state,
        session_id,
        &RuntimeToken::Codex(runtime_id.to_owned()),
        generation,
        result,
    )
}
