// PersistedState + PersistedSessionRecord — the on-disk projections of
// StateInner / SessionRecord used for SQLite + legacy-JSON persistence.
//
// The "persisted" types are the single source of truth for the disk
// schema: they use `#[serde(rename_all = "camelCase")]`, `#[serde(default)]`,
// and `skip_serializing_if` annotations to keep on-disk state compact
// and forward-compatible. Every commit_locked() ultimately produces a
// PersistedState that gets written to SQLite via src/persist.rs; every
// startup load_state() reads it back and reconstructs StateInner.
//
// Strict validation: missing required fields on load produce an error
// rather than silent defaults (prevents sessions from coming back with
// quietly-broken state — see `validate_persisted_session_fields` and
// the `persisted_state_requires_*` tests in tests/persist.rs).
//
// Extracted from state.rs into its own `include!()` fragment so state.rs
// stays focused on the runtime model rather than the serde schema.

/// Tracks persisted state.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedState {
    #[serde(default, skip_serializing_if = "CodexState::is_empty")]
    codex: CodexState,
    #[serde(default)]
    preferences: AppPreferences,
    #[serde(default)]
    revision: u64,
    next_project_number: usize,
    next_session_number: usize,
    next_message_number: u64,
    projects: Vec<Project>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    ignored_discovered_codex_thread_ids: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    orchestrator_instances: Vec<OrchestratorInstance>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    workspace_layouts: BTreeMap<String, WorkspaceLayoutDocument>,
    sessions: Vec<PersistedSessionRecord>,
}

impl PersistedState {
    /// Builds the metadata-only value from inner.
    fn metadata_from_inner(inner: &StateInner) -> Self {
        Self {
            codex: inner.codex.clone(),
            preferences: inner.preferences.clone(),
            revision: inner.revision,
            next_project_number: inner.next_project_number,
            next_session_number: inner.next_session_number,
            next_message_number: inner.next_message_number,
            projects: inner.projects.clone(),
            ignored_discovered_codex_thread_ids: inner.ignored_discovered_codex_thread_ids.clone(),
            orchestrator_instances: inner.orchestrator_instances.clone(),
            workspace_layouts: inner.workspace_layouts.clone(),
            sessions: Vec::new(),
        }
    }

    /// Builds the value from inner.
    fn from_inner(inner: &StateInner) -> Self {
        let mut persisted = Self::metadata_from_inner(inner);
        persisted.sessions = inner
            .sessions
            .iter()
            .filter(|record| !record.hidden)
            .map(PersistedSessionRecord::from_record)
            .collect();
        persisted
    }

    /// Converts the value into inner.
    fn into_inner(self) -> Result<StateInner> {
        let mut inner = StateInner {
            codex: self.codex,
            preferences: AppPreferences {
                remotes: validate_persisted_remote_configs(self.preferences.remotes)?,
                ..self.preferences
            },
            revision: self.revision,
            next_project_number: self.next_project_number,
            next_session_number: self.next_session_number,
            next_message_number: self.next_message_number,
            projects: self.projects,
            ignored_discovered_codex_thread_ids: self.ignored_discovered_codex_thread_ids,
            remote_applied_revisions: HashMap::new(),
            orchestrator_instances: self.orchestrator_instances,
            workspace_layouts: self.workspace_layouts,
            sessions: self
                .sessions
                .into_iter()
                .map(PersistedSessionRecord::into_record)
                .collect::<Result<Vec<_>>>()?,
            // Mutation stamps are in-memory only — start at `0` on each
            // process lifetime. The persist thread's watermark also
            // starts at `0`, so a fresh load has no pending writes.
            last_mutation_stamp: 0,
            removed_session_ids: Vec::new(),
        };
        let persisted_non_running_session_ids = inner
            .sessions
            .iter()
            .filter(|record| {
                !matches!(
                    record.session.status,
                    SessionStatus::Active | SessionStatus::Approval
                )
            })
            .map(|record| record.session.id.clone())
            .collect::<HashSet<_>>();
        inner.normalize_local_paths();
        inner.validate_projects_consistent()?;
        inner.recover_interrupted_sessions();
        inner.normalize_orchestrator_instances_with_persisted_non_running(
            &persisted_non_running_session_ids,
        );
        Ok(inner)
    }
}

/// Handles session flag is false.
fn session_flag_is_false(value: &bool) -> bool {
    !*value
}

/// Returns whether the same Codex notice identity applies.
fn same_codex_notice_identity(left: &CodexNotice, right: &CodexNotice) -> bool {
    left.kind == right.kind
        && left.level == right.level
        && left.title == right.title
        && left.detail == right.detail
        && left.code == right.code
}

/// Represents a persisted session record.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedSessionRecord {
    active_codex_approval_policy: Option<CodexApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_codex_reasoning_effort: Option<CodexReasoningEffort>,
    active_codex_sandbox_mode: Option<CodexSandboxMode>,
    codex_approval_policy: CodexApprovalPolicy,
    codex_reasoning_effort: CodexReasoningEffort,
    codex_sandbox_mode: CodexSandboxMode,
    external_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "VecDeque::is_empty")]
    queued_prompts: VecDeque<QueuedPromptRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "session_flag_is_false")]
    orchestrator_auto_dispatch_blocked: bool,
    session: Session,
}

impl PersistedSessionRecord {
    /// Builds the value from record.
    fn from_record(record: &SessionRecord) -> Self {
        let mut session = record.session.clone();
        if !record.is_remote_proxy() {
            session.pending_prompts.clear();
        }

        Self {
            active_codex_approval_policy: record.active_codex_approval_policy,
            active_codex_reasoning_effort: record.active_codex_reasoning_effort,
            active_codex_sandbox_mode: record.active_codex_sandbox_mode,
            codex_approval_policy: record.codex_approval_policy,
            codex_reasoning_effort: record.codex_reasoning_effort,
            codex_sandbox_mode: record.codex_sandbox_mode,
            external_session_id: record.external_session_id.clone(),
            queued_prompts: record.queued_prompts.clone(),
            remote_id: record.remote_id.clone(),
            remote_session_id: record.remote_session_id.clone(),
            orchestrator_auto_dispatch_blocked: record.orchestrator_auto_dispatch_blocked,
            session,
        }
    }

    /// Converts the value into record.
    fn into_record(self) -> Result<SessionRecord> {
        let mut session = self.session;
        validate_persisted_session_fields(&session, self.external_session_id.as_deref())?;
        session.external_session_id = self.external_session_id.clone();
        if session.agent.acp_runtime().is_none() {
            session.model_options.clear();
        }
        if self.remote_id.is_none() {
            session.pending_prompts.clear();
        }

        let mut record = SessionRecord {
            active_codex_approval_policy: self.active_codex_approval_policy,
            active_codex_reasoning_effort: self.active_codex_reasoning_effort,
            active_codex_sandbox_mode: self.active_codex_sandbox_mode,
            active_turn_start_message_count: None,
            active_turn_file_changes: BTreeMap::new(),
            active_turn_file_change_grace_deadline: None,
            agent_commands: Vec::new(),
            codex_approval_policy: self.codex_approval_policy,
            codex_reasoning_effort: self.codex_reasoning_effort,
            codex_sandbox_mode: self.codex_sandbox_mode,
            external_session_id: self.external_session_id,
            pending_claude_approvals: HashMap::new(),
            pending_codex_approvals: HashMap::new(),
            pending_codex_user_inputs: HashMap::new(),
            pending_codex_mcp_elicitations: HashMap::new(),
            pending_codex_app_requests: HashMap::new(),
            pending_acp_approvals: HashMap::new(),
            queued_prompts: self.queued_prompts,
            message_positions: build_message_positions(&session.messages),
            remote_id: self.remote_id,
            remote_session_id: self.remote_session_id,
            runtime: SessionRuntime::None,
            runtime_reset_required: false,
            orchestrator_auto_dispatch_blocked: self.orchestrator_auto_dispatch_blocked,
            runtime_stop_in_progress: false,
            deferred_stop_callbacks: Vec::new(),
            hidden: false,
            // Freshly loaded records start unstamped; nothing has changed
            // since the on-disk snapshot so nothing needs to be persisted.
            mutation_stamp: 0,
            session,
        };
        sync_codex_thread_state(&mut record);
        sync_pending_prompts(&mut record);
        Ok(record)
    }
}

/// Validates persisted session fields.
fn validate_persisted_session_fields(
    session: &Session,
    external_session_id: Option<&str>,
) -> Result<()> {
    if session.external_session_id.as_deref() != external_session_id {
        return Err(anyhow!(
            "persisted session `{}` has mismatched externalSessionId",
            session.id
        ));
    }

    if session.agent.supports_cursor_mode() {
        if session.cursor_mode.is_none() {
            return Err(anyhow!(
                "persisted session `{}` is missing cursorMode",
                session.id
            ));
        }
    } else if session.cursor_mode.is_some() {
        return Err(anyhow!(
            "persisted session `{}` should not define cursorMode for {} sessions",
            session.id,
            session.agent.name()
        ));
    }

    if session.agent.supports_claude_approval_mode() {
        if session.claude_approval_mode.is_none() {
            return Err(anyhow!(
                "persisted session `{}` is missing claudeApprovalMode",
                session.id
            ));
        }
        if session.claude_effort.is_none() {
            return Err(anyhow!(
                "persisted session `{}` is missing claudeEffort",
                session.id
            ));
        }
    } else if session.claude_approval_mode.is_some() || session.claude_effort.is_some() {
        return Err(anyhow!(
            "persisted session `{}` should not define Claude settings for {} sessions",
            session.id,
            session.agent.name()
        ));
    }

    if session.agent.supports_gemini_approval_mode() {
        if session.gemini_approval_mode.is_none() {
            return Err(anyhow!(
                "persisted session `{}` is missing geminiApprovalMode",
                session.id
            ));
        }
    } else if session.gemini_approval_mode.is_some() {
        return Err(anyhow!(
            "persisted session `{}` should not define geminiApprovalMode for {} sessions",
            session.id,
            session.agent.name()
        ));
    }

    if session.agent.supports_codex_prompt_settings() {
        if session.approval_policy.is_none() {
            return Err(anyhow!(
                "persisted session `{}` is missing approvalPolicy",
                session.id
            ));
        }
        if session.reasoning_effort.is_none() {
            return Err(anyhow!(
                "persisted session `{}` is missing reasoningEffort",
                session.id
            ));
        }
        if session.sandbox_mode.is_none() {
            return Err(anyhow!(
                "persisted session `{}` is missing sandboxMode",
                session.id
            ));
        }
    } else if session.approval_policy.is_some()
        || session.reasoning_effort.is_some()
        || session.sandbox_mode.is_some()
    {
        return Err(anyhow!(
            "persisted session `{}` should not define Codex prompt settings for {} sessions",
            session.id,
            session.agent.name()
        ));
    }

    let expects_codex_thread_state =
        session.agent.supports_codex_prompt_settings() && external_session_id.is_some();
    if expects_codex_thread_state {
        if session.codex_thread_state.is_none() {
            return Err(anyhow!(
                "persisted session `{}` is missing codexThreadState",
                session.id
            ));
        }
    } else if session.codex_thread_state.is_some() {
        return Err(anyhow!(
            "persisted session `{}` should not define codexThreadState without an active Codex thread",
            session.id
        ));
    }

    Ok(())
}
