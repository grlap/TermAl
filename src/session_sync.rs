// Per-session runtime-driven state syncers + Codex-specific notices.
//
// "Sync" here means: the runtime is reporting some fact about the
// session (the list of models it knows, the command palette it
// exposes, the cursor-mode it's running in) that TermAl mirrors on
// the `SessionRecord` so the UI can show it without another round
// trip to the runtime. These are idempotent upserts — running them
// again with the same payload is a no-op, running them with a
// different payload stamps the record and broadcasts a delta.
//
// The Codex-specific methods (`note_codex_rate_limits`,
// `note_codex_notice`, `record_codex_runtime_config_if_runtime_matches`)
// react to shared-runtime notifications: rate-limit headers from the
// upstream API, UI notices pushed by the Codex app-server, and the
// per-session model / reasoning-effort / approval-policy configuration
// that Codex confirms back after a `thread/start`. The last one is
// `_if_runtime_matches` guarded because it arrives asynchronously
// and must not land on a session whose Codex runtime has since been
// replaced.

impl AppState {
    /// Records the set of models a live Claude/ACP runtime knows about
    /// plus which one it's actively using, so the UI's model-picker
    /// dropdown matches what the runtime will actually accept. Noop
    /// when nothing changed.
    fn sync_session_model_options(
        &self,
        session_id: &str,
        current_model: Option<String>,
        model_options: Vec<SessionModelOption>,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");

        let mut changed = false;
        if let Some(current_model) = current_model
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
        {
            if record.session.model != current_model {
                record.session.model = current_model;
                changed = true;
            }
        }
        if record.session.model_options != model_options {
            record.session.model_options = model_options;
            changed = true;
        }
        if record.session.agent.supports_codex_prompt_settings() {
            if let Some(normalized_effort) = normalized_codex_reasoning_effort(
                &record.session.model,
                record.codex_reasoning_effort,
                &record.session.model_options,
            ) {
                if record.codex_reasoning_effort != normalized_effort {
                    record.codex_reasoning_effort = normalized_effort;
                    record.session.reasoning_effort = Some(normalized_effort);
                    changed = true;
                }
            }
        }

        if changed {
            self.commit_locked(&mut inner)?;
        }
        Ok(())
    }

    /// Mirrors the runtime's advertised agent command palette (slash
    /// commands, custom commands, etc.) onto the `SessionRecord`. The
    /// UI's command popover renders directly from this cached list so
    /// every user keystroke doesn't round-trip to the runtime.
    fn sync_session_agent_commands(
        &self,
        session_id: &str,
        agent_commands: Vec<AgentCommand>,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let next_commands = dedupe_agent_commands(agent_commands);
        // Read-only check first: if the commands haven't changed,
        // return without bumping the mutation stamp. Using
        // `session_mut_by_index` up-front would mark this session
        // dirty on every duplicate announce, forcing
        // `collect_persist_delta` to re-serialize its row for no
        // real change.
        if inner
            .session_by_index(index)
            .expect("session index should be valid")
            .agent_commands
            == next_commands
        {
            return Ok(());
        }
        let should_publish = {
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            record.agent_commands = next_commands;
            if record.hidden {
                false
            } else {
                record.session.agent_commands_revision =
                    record.session.agent_commands_revision.saturating_add(1);
                true
            }
        };
        if should_publish {
            self.commit_locked(&mut inner)?;
        }
        Ok(())
    }

    /// Mirrors the runtime's reported cursor mode (Cursor agent
    /// specifically) onto the session so the UI's mode indicator
    /// reflects what the runtime is actually running under.
    fn sync_session_cursor_mode(
        &self,
        session_id: &str,
        cursor_mode: Option<CursorMode>,
    ) -> Result<()> {
        let Some(cursor_mode) = cursor_mode else {
            return Ok(());
        };

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        // Read-only check before the stamp bump: exit on the
        // no-op path (agent doesn't support cursor mode, or the
        // mode already matches) without marking the session
        // dirty. `session_mut_by_index` would advance the
        // mutation stamp permanently, forcing
        // `collect_persist_delta` to re-serialize the session
        // row on the next tick for no real change.
        {
            let record = inner
                .session_by_index(index)
                .expect("session index should be valid");
            if !record.session.agent.supports_cursor_mode()
                || record.session.cursor_mode == Some(cursor_mode)
            {
                return Ok(());
            }
        }
        // A mutation is required — re-borrow mutably to take a
        // fresh stamp.
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        record.session.cursor_mode = Some(cursor_mode);
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    /// Caches the most recent rate-limit snapshot reported by the
    /// shared Codex runtime so the UI can render "N requests remaining"
    /// / "resets at T" without polling the upstream API itself.
    fn note_codex_rate_limits(&self, rate_limits: CodexRateLimits) -> Result<()> {
        let (revision, codex) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if inner.codex.rate_limits.as_ref() == Some(&rate_limits) {
                return Ok(());
            }

            inner.codex.rate_limits = Some(rate_limits);
            let revision = self.commit_persisted_delta_locked(&mut inner)?;
            (revision, inner.codex.clone())
        };
        self.publish_delta(&DeltaEvent::CodexUpdated { revision, codex });
        Ok(())
    }

    /// Stores a pushed `CodexNotice` from the shared runtime (version
    /// update hints, login reminders, etc.) on `AppState` so the UI
    /// can render it as a banner on the next state broadcast.
    fn note_codex_notice(&self, notice: CodexNotice) -> Result<()> {
        let (revision, codex) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if inner
                .codex
                .notices
                .first()
                .is_some_and(|existing| same_codex_notice_identity(existing, &notice))
            {
                return Ok(());
            }

            if let Some(index) = inner
                .codex
                .notices
                .iter()
                .position(|existing| same_codex_notice_identity(existing, &notice))
            {
                inner.codex.notices.remove(index);
            }

            inner.codex.notices.insert(0, notice);
            inner.codex.notices.truncate(CODEX_NOTICE_CAP);
            let revision = self.commit_persisted_delta_locked(&mut inner)?;
            (revision, inner.codex.clone())
        };
        self.publish_delta(&DeltaEvent::CodexUpdated { revision, codex });
        Ok(())
    }

    /// Records Codex runtime config when the expected runtime still owns the session.
    fn record_codex_runtime_config_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        sandbox_mode: CodexSandboxMode,
        approval_policy: CodexApprovalPolicy,
        reasoning_effort: CodexReasoningEffort,
    ) -> Result<RuntimeMatchOutcome> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return Ok(RuntimeMatchOutcome::SessionMissing);
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if !record.runtime.matches_runtime_token(token) {
            return Ok(RuntimeMatchOutcome::RuntimeMismatch);
        }
        record.active_codex_sandbox_mode = Some(sandbox_mode);
        record.active_codex_approval_policy = Some(approval_policy);
        record.active_codex_reasoning_effort = Some(reasoning_effort);
        self.persist_internal_locked(&inner)?;
        Ok(RuntimeMatchOutcome::Applied)
    }
}
