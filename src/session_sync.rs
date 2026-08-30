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

fn ensure_opencode_sync_deadline(
    execution_deadline: Option<std::time::Instant>,
) -> Result<()> {
    if execution_deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
        bail!("OpenCode config request deadline expired before committing session state");
    }
    Ok(())
}

impl AppState {
    /// Builds the persisted OpenCode selection used when a live runtime
    /// reports changed config options. The ACP reader cannot write protocol
    /// requests itself, so it snapshots the user's selections here and queues
    /// reconciliation back to the writer thread.
    fn opencode_config_command(&self, session_id: &str) -> Result<AcpPromptCommand> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        if !record.session.agent.supports_opencode_settings() {
            return Err(anyhow!(
                "session `{session_id}` is not an OpenCode session"
            ));
        }
        Ok(AcpPromptCommand {
            cwd: record.session.workdir.clone(),
            cursor_mode: None,
            model: record
                .session
                .opencode_model
                .clone()
                .unwrap_or_else(|| OPENCODE_CONFIG_AUTO.to_owned()),
            opencode_effort: record.session.opencode_effort.clone(),
            opencode_mode: record.session.opencode_mode.clone(),
            prompt: String::new(),
            resume_session_id: record.external_session_id.clone(),
        })
    }

    /// Reads the latest OpenCode authority and option state immediately before
    /// the serialized ACP writer applies a user setting. Reader notifications
    /// and user requests share that writer, so this snapshot cannot be stale
    /// relative to an earlier queued config reconciliation.
    fn opencode_config_snapshot(&self, session_id: &str) -> Result<OpenCodeConfigSnapshot> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        if !record.session.agent.supports_opencode_settings() {
            return Err(anyhow!(
                "session `{session_id}` is not an OpenCode session"
            ));
        }
        Ok(OpenCodeConfigSnapshot {
            model_selection: record
                .session
                .opencode_model
                .clone()
                .unwrap_or_else(|| OPENCODE_CONFIG_AUTO.to_owned()),
            effective_model: record.session.model.clone(),
            model_options: record.session.model_options.clone(),
            effort_selection: record
                .session
                .opencode_effort
                .clone()
                .unwrap_or_else(|| OPENCODE_CONFIG_AUTO.to_owned()),
            current_effort: record.session.opencode_current_effort.clone(),
            effort_options: record.session.opencode_effort_options.clone(),
            mode_selection: record
                .session
                .opencode_mode
                .clone()
                .unwrap_or_else(|| OPENCODE_CONFIG_AUTO.to_owned()),
            current_mode: record.session.opencode_current_mode.clone(),
            mode_options: record.session.opencode_mode_options.clone(),
        })
    }

    /// Commits one agent-acknowledged OpenCode authority change before the
    /// owning API request expires. Explicit choices become both selected and
    /// effective; `auto` changes only the selected authority and preserves the
    /// agent-reported effective value.
    fn sync_session_opencode_selection_before_deadline(
        &self,
        session_id: &str,
        option_id: &str,
        selection: String,
        execution_deadline: std::time::Instant,
    ) -> Result<()> {
        self.sync_session_opencode_selection_with_deadline(
            session_id,
            option_id,
            selection,
            Some(execution_deadline),
        )
    }

    fn sync_session_opencode_selection_with_deadline(
        &self,
        session_id: &str,
        option_id: &str,
        selection: String,
        execution_deadline: Option<std::time::Instant>,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        ensure_opencode_sync_deadline(execution_deadline)?;
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if !record.session.agent.supports_opencode_settings() {
            return Err(anyhow!(
                "session `{session_id}` is not an OpenCode session"
            ));
        }

        match option_id {
            "model" => {
                record.session.opencode_model = Some(selection.clone());
                if selection != OPENCODE_CONFIG_AUTO {
                    record.session.model = selection;
                }
            }
            "effort" => {
                record.session.opencode_effort = Some(selection.clone());
                if selection != OPENCODE_CONFIG_AUTO {
                    record.session.opencode_current_effort = Some(selection);
                }
            }
            "mode" => {
                record.session.opencode_mode = Some(selection.clone());
                if selection != OPENCODE_CONFIG_AUTO {
                    record.session.opencode_current_mode = Some(selection);
                }
            }
            _ => bail!("unsupported OpenCode config option `{option_id}`"),
        }
        self.commit_locked(&mut inner).map(|_| ())
    }

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
            if record.session.codex_fast_mode
                && codex_model_option(&record.session.model, &record.session.model_options)
                    .is_some()
                && !codex_model_supports_fast(
                    &record.session.model,
                    &record.session.model_options,
                )
            {
                record.session.codex_fast_mode = false;
                changed = true;
            }
        }

        if changed {
            self.commit_locked(&mut inner)?;
        }
        Ok(())
    }

    /// Reconciles OpenCode's dynamic model/effort/mode config after new, resume,
    /// load, or a config-options update. The selected values preserve the
    /// TermAl authority boundary (`auto` delegates to the agent; an explicit
    /// live value is TermAl-authoritative), while the effective fields mirror
    /// what OpenCode is actually running.
    fn sync_session_opencode_config(
        &self,
        session_id: &str,
        update: OpenCodeConfigUpdate,
    ) -> Result<()> {
        self.sync_session_opencode_config_with_deadline(session_id, update, None)
    }

    fn sync_session_opencode_config_before_deadline(
        &self,
        session_id: &str,
        update: OpenCodeConfigUpdate,
        execution_deadline: std::time::Instant,
    ) -> Result<()> {
        self.sync_session_opencode_config_with_deadline(
            session_id,
            update,
            Some(execution_deadline),
        )
    }

    fn sync_session_opencode_config_with_deadline(
        &self,
        session_id: &str,
        update: OpenCodeConfigUpdate,
        execution_deadline: Option<std::time::Instant>,
    ) -> Result<()> {
        let model_update = update
            .model
            .map(|update| {
                Ok::<_, anyhow::Error>(OpenCodeConfigOptionUpdate {
                    selection: normalize_opencode_model(&update.selection)?,
                    current: update
                        .current
                        .as_deref()
                        .map(normalize_opencode_model)
                        .transpose()?,
                    options: update.options,
                })
            })
            .transpose()?;
        let effort_update = update
            .effort
            .map(|update| {
                Ok::<_, anyhow::Error>(OpenCodeConfigOptionUpdate {
                    selection: normalize_opencode_effort(&update.selection)?,
                    current: update
                        .current
                        .as_deref()
                        .map(normalize_opencode_effort)
                        .transpose()?,
                    options: update.options,
                })
            })
            .transpose()?;
        let mode_update = update
            .mode
            .map(|update| {
                Ok::<_, anyhow::Error>(OpenCodeConfigOptionUpdate {
                    selection: normalize_opencode_mode(&update.selection)?,
                    current: update
                        .current
                        .as_deref()
                        .map(normalize_opencode_mode)
                        .transpose()?,
                    options: update.options,
                })
            })
            .transpose()?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        ensure_opencode_sync_deadline(execution_deadline)?;
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if !record.session.agent.supports_opencode_settings() {
            return Err(anyhow!(
                "session `{session_id}` is not an OpenCode session"
            ));
        }

        if let Some(update) = model_update {
            record.session.opencode_model = Some(update.selection);
            if let Some(effective_model) = update.current {
                record.session.model = effective_model;
            }
            record.session.model_options = update.options;
        }
        if let Some(update) = effort_update {
            record.session.opencode_effort = Some(update.selection);
            record.session.opencode_current_effort = update.current;
            record.session.opencode_effort_options = update.options;
        }
        if let Some(update) = mode_update {
            record.session.opencode_mode = Some(update.selection);
            record.session.opencode_current_mode = update.current;
            record.session.opencode_mode_options = update.options;
        }
        self.commit_locked(&mut inner)?;
        drop(inner);

        for notice in update.notices {
            self.push_message(
                session_id,
                Message::Text {
                    attachments: Vec::new(),
                    id: self.allocate_message_id(),
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: notice,
                    expanded_text: None,
                    source: None,
                },
            )?;
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
        let next_commands = dedupe_agent_commands(
            agent_commands
                .into_iter()
                .filter(|command| command.kind == AgentCommandKind::NativeSlash)
                .collect(),
        );
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
        active_turn_generation: u64,
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
        if !record.runtime.matches_runtime_token(token)
            || record.active_turn_generation != active_turn_generation
        {
            return Ok(RuntimeMatchOutcome::RuntimeMismatch);
        }
        record.active_codex_sandbox_mode = Some(sandbox_mode);
        record.active_codex_approval_policy = Some(approval_policy);
        record.active_codex_reasoning_effort = Some(reasoning_effort);
        self.persist_internal_locked(&inner)?;
        Ok(RuntimeMatchOutcome::Applied)
    }
}
