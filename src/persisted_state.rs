// PersistedState + PersistedSessionRecord — the on-disk projections of
// StateInner / SessionRecord used for normalized SQLite persistence.
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
    next_session_number: usize,
    next_message_number: u64,
    projects: Vec<Project>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    engram_retired_work_authority_grants: Vec<EngramRetiredWorkAuthorityGrant>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pending_coordination_scope_deletions: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pending_response_board_project_detachments: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    ignored_discovered_codex_thread_ids: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    orchestrator_instances: Vec<OrchestratorInstance>,
    /// Snapshot-only carrier for the normalized `delegations` table.
    /// Delegations are never read from or written into `app_state` metadata.
    #[serde(skip)]
    delegations: Vec<DelegationRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    delegation_waits: Vec<DelegationWaitRecord>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    workspace_layouts: BTreeMap<String, WorkspaceLayoutDocument>,
    sessions: Vec<PersistedSessionRecord>,
    /// Runtime-only ids whose normalized SQLite rows failed startup
    /// validation. They are intentionally excluded from the healthy in-memory
    /// model, but synchronous full-snapshot persistence must preserve the
    /// original rows instead of interpreting their absence as deletion.
    #[serde(skip)]
    quarantined_persisted_session_ids: BTreeSet<String>,
    #[serde(skip)]
    quarantined_persisted_delegation_ids: BTreeSet<String>,
}

impl PersistedState {
    /// Builds the metadata-only value from inner.
    ///
    /// Keep field-list in sync with [`Self::metadata_only`], which
    /// produces the same shape but clones from an existing
    /// `PersistedState` rather than a `StateInner`. Adding a new
    /// top-level field to `PersistedState` requires updating both —
    /// missing one would silently drop the field from either the
    /// snapshot-from-inner path or the synchronous persist fallback
    /// path.
    fn metadata_from_inner(inner: &StateInner) -> Self {
        Self {
            codex: inner.codex.clone(),
            preferences: inner.preferences.clone(),
            revision: inner.revision,
            next_session_number: inner.next_session_number,
            next_message_number: inner.next_message_number,
            projects: inner.projects.clone(),
            engram_retired_work_authority_grants: inner
                .engram_retired_work_authority_grants
                .clone(),
            pending_coordination_scope_deletions: inner
                .pending_coordination_scope_deletions
                .clone(),
            pending_response_board_project_detachments: inner
                .pending_response_board_project_detachments
                .clone(),
            ignored_discovered_codex_thread_ids: inner.ignored_discovered_codex_thread_ids.clone(),
            orchestrator_instances: inner.orchestrator_instances.clone(),
            delegations: Vec::new(),
            delegation_waits: inner.delegation_waits.clone(),
            workspace_layouts: inner.workspace_layouts.clone(),
            sessions: Vec::new(),
            quarantined_persisted_session_ids: inner
                .quarantined_persisted_session_ids
                .clone(),
            quarantined_persisted_delegation_ids: inner
                .quarantined_persisted_delegation_ids
                .clone(),
        }
    }

    /// Returns a metadata-only copy of this persisted state with empty
    /// `sessions` and `delegations` vecs.
    ///
    /// Used by the synchronous persist path
    /// (`persist_persisted_state_to_sqlite`) to serialize the
    /// `app_state` metadata row without the sessions or delegation
    /// payloads. The
    /// previous `persisted.clone(); metadata.sessions.clear();`
    /// pattern deep-cloned every session transcript just to drop the
    /// clone, which is wasteful for long transcripts. Cloning the
    /// metadata fields explicitly avoids that waste while keeping the
    /// call-site shape unchanged.
    ///
    /// Keep field-list in sync with [`Self::metadata_from_inner`]
    /// — the two methods produce the same shape from different
    /// source types (a `StateInner` vs. a sibling `PersistedState`).
    /// Adding a new top-level field to `PersistedState` requires
    /// updating both.
    fn metadata_only(&self) -> Self {
        Self {
            codex: self.codex.clone(),
            preferences: self.preferences.clone(),
            revision: self.revision,
            next_session_number: self.next_session_number,
            next_message_number: self.next_message_number,
            projects: self.projects.clone(),
            engram_retired_work_authority_grants: self
                .engram_retired_work_authority_grants
                .clone(),
            pending_coordination_scope_deletions: self
                .pending_coordination_scope_deletions
                .clone(),
            pending_response_board_project_detachments: self
                .pending_response_board_project_detachments
                .clone(),
            ignored_discovered_codex_thread_ids: self.ignored_discovered_codex_thread_ids.clone(),
            orchestrator_instances: self.orchestrator_instances.clone(),
            delegations: Vec::new(),
            delegation_waits: self.delegation_waits.clone(),
            workspace_layouts: self.workspace_layouts.clone(),
            sessions: Vec::new(),
            quarantined_persisted_session_ids: self
                .quarantined_persisted_session_ids
                .clone(),
            quarantined_persisted_delegation_ids: self
                .quarantined_persisted_delegation_ids
                .clone(),
        }
    }

    /// Builds the value from inner.
    fn from_inner(inner: &StateInner) -> Self {
        let mut persisted = Self::metadata_from_inner(inner);
        persisted.delegations = inner.delegations.clone();
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
        let mut preferences = self.preferences;
        preferences.remotes = validate_persisted_remote_configs(preferences.remotes)?;
        let mut inner = StateInner {
            codex: self.codex,
            engram_host_adapter: Arc::new(EngramHostAdapter::default()),
            #[cfg(test)]
            test_engram_dispatch_budget: None,
            engram_declared_project_ids: HashSet::new(),
            engram_declaration_checked_project_ids: HashSet::new(),
            engram_project_resets: EngramProjectResetFences::default(),
            preferences,
            revision: self.revision,
            next_session_number: self.next_session_number,
            next_message_number: self.next_message_number,
            projects: self.projects,
            engram_retired_work_authority_grants: self
                .engram_retired_work_authority_grants,
            pending_coordination_scope_deletions: self
                .pending_coordination_scope_deletions,
            pending_response_board_project_detachments: self
                .pending_response_board_project_detachments,
            ignored_discovered_codex_thread_ids: self.ignored_discovered_codex_thread_ids,
            remote_applied_revisions: HashMap::new(),
            remote_snapshot_applied_revisions: HashMap::new(),
            remote_session_transcript_applied_revisions: HashMap::new(),
            orchestrator_instances: self.orchestrator_instances,
            delegations: self.delegations,
            delegation_waits: self.delegation_waits,
            delegation_mutation_stamps: BTreeMap::new(),
            removed_delegation_ids: BTreeMap::new(),
            running_read_only_delegations: BTreeSet::new(),
            workspace_layouts: self.workspace_layouts,
            sessions: self
                .sessions
                .into_iter()
                .map(PersistedSessionRecord::into_record)
                .collect::<Result<Vec<_>>>()?,
            quarantined_persisted_session_ids: self.quarantined_persisted_session_ids,
            quarantined_persisted_delegation_ids: self.quarantined_persisted_delegation_ids,
            // Mutation stamps are in-memory only — start at `0` on each
            // process lifetime. The persist thread's watermark also
            // starts at `0`, so a fresh load has no pending writes.
            last_mutation_stamp: 0,
            removed_session_ids: Vec::new(),
            settings_persist_dirty: false,
            remote_settings_persist_dirty: false,
            remote_delta_persist_dirty: false,
        };
        let persisted_non_running_session_ids = inner
            .sessions
            .iter()
            .filter(|record| {
                !matches!(
                    record.session.status,
                    SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
                )
            })
            .map(|record| record.session.id.clone())
            .collect::<HashSet<_>>();
        inner.normalize_local_paths();
        inner.validate_projects_consistent()?;
        inner.recover_interrupted_sessions();
        inner.repair_delegation_child_session_links();
        inner.rebuild_running_read_only_delegations();
        inner.normalize_orchestrator_instances_with_persisted_non_running(
            &persisted_non_running_session_ids,
        );
        Ok(inner)
    }
}

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
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    queued_peer_messages: HashMap<String, Vec<PendingPrompt>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "session_flag_is_false")]
    orchestrator_auto_dispatch_blocked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    engram_routing_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    engram_open_grant_id: Option<String>,
    #[serde(skip)]
    message_start_index: usize,
    /// Runtime-only instruction for the SQLite serializer. Full snapshots set
    /// it; delta snapshots clear it unless the independent history stamp moved.
    #[serde(skip)]
    persist_prompt_history: bool,
    session: Session,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedRemoteProxyIdentity {
    #[serde(default)]
    remote_id: Option<String>,
    #[serde(default)]
    remote_session_id: Option<String>,
}

impl PersistedRemoteProxyIdentity {
    fn from_session_json(encoded: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(encoded)
            .context("failed to parse persisted remote proxy identity")?;
        if !value.is_object() {
            bail!("persisted session metadata is not an object");
        }
        serde_json::from_value(value)
            .context("failed to decode persisted remote proxy identity")
    }

    fn remote_proxy_identity(&self) -> Result<Option<(&str, &str)>> {
        validate_remote_proxy_identity(
            self.remote_id.as_deref(),
            self.remote_session_id.as_deref(),
        )
    }

    fn is_remote_proxy(&self) -> Result<bool> {
        self.remote_proxy_identity()
            .map(|identity| identity.is_some())
    }
}

fn validate_remote_proxy_identity<'a>(
    remote_id: Option<&'a str>,
    remote_session_id: Option<&'a str>,
) -> Result<Option<(&'a str, &'a str)>> {
    match (remote_id, remote_session_id) {
        (None, None) => Ok(None),
        (Some(remote_id), Some(_)) if remote_id.trim().is_empty() => {
            bail!("remoteId must not be empty")
        }
        (Some(_), Some(remote_session_id)) if remote_session_id.trim().is_empty() => {
            bail!("remoteSessionId must not be empty")
        }
        (Some(remote_id), Some(remote_session_id)) => Ok(Some((remote_id, remote_session_id))),
        (Some(_), None) => bail!("remoteId requires remoteSessionId"),
        (None, Some(_)) => bail!("remoteSessionId requires remoteId"),
    }
}

/// Applies intentionally-supported defaults from earlier persisted session
/// shapes before strict current-schema validation. Keep this narrow: missing
/// required fields remain corruption unless a concrete upgrade is named here.
fn backfill_persisted_session_defaults(session: &mut Session) {
    if session.agent.supports_opencode_settings() && session.opencode_effort.is_none() {
        session.opencode_effort = Some(OPENCODE_CONFIG_AUTO.to_owned());
    }
    session.prompt_history = normalize_prompt_history(std::mem::take(&mut session.prompt_history));
}

impl PersistedSessionRecord {
    /// Builds the value from record.
    fn from_record(record: &SessionRecord) -> Self {
        let mut session = record.session.clone();
        if record.is_local_session() {
            session.pending_prompts.clear();
        }
        session.live_activity = None;
        session.session_mutation_stamp = None;

        Self {
            active_codex_approval_policy: record.active_codex_approval_policy,
            active_codex_reasoning_effort: record.active_codex_reasoning_effort,
            active_codex_sandbox_mode: record.active_codex_sandbox_mode,
            codex_approval_policy: record.codex_approval_policy,
            codex_reasoning_effort: record.codex_reasoning_effort,
            codex_sandbox_mode: record.codex_sandbox_mode,
            external_session_id: record.external_session_id.clone(),
            queued_prompts: record.queued_prompts.clone(),
            queued_peer_messages: record.queued_peer_messages.clone(),
            remote_id: record.remote_id.clone(),
            remote_session_id: record.remote_session_id.clone(),
            orchestrator_auto_dispatch_blocked: record.orchestrator_auto_dispatch_blocked,
            engram_routing_token: record.engram.routing_token.clone(),
            engram_open_grant_id: record.engram.active_grant_id.clone(),
            message_start_index: record.message_start_index,
            persist_prompt_history: true,
            session,
        }
    }

    /// Converts the value into record.
    fn into_record(self) -> Result<SessionRecord> {
        let remote_proxy_identity = validate_remote_proxy_identity(
            self.remote_id.as_deref(),
            self.remote_session_id.as_deref(),
        )
        .with_context(|| {
            format!(
                "persisted session `{}` has invalid remote proxy identity",
                self.session.id
            )
        })?;
        let mut session = self.session;
        backfill_persisted_session_defaults(&mut session);
        validate_persisted_session_fields(&session, self.external_session_id.as_deref())?;
        session.session_mutation_stamp = None;
        session.external_session_id = self.external_session_id.clone();
        session.live_activity = None;
        if session.agent.acp_runtime().is_none() {
            session.model_options.clear();
            session.opencode_effort_options.clear();
            session.opencode_current_effort = None;
            session.opencode_mode_options.clear();
            session.opencode_current_mode = None;
        }
        if remote_proxy_identity.is_none() {
            session.pending_prompts.clear();
        }

        let mut record = SessionRecord {
            active_codex_approval_policy: self.active_codex_approval_policy,
            active_codex_reasoning_effort: self.active_codex_reasoning_effort,
            active_codex_sandbox_mode: self.active_codex_sandbox_mode,
            active_turn_generation: 0,
            active_turn_mailbox_notification: None,
            active_turn_start_message_count: None,
            active_turn_file_changes: BTreeMap::new(),
            active_turn_file_change_grace_deadline: None,
            agent_commands: Vec::new(),
            codex_approval_policy: self.codex_approval_policy,
            codex_reasoning_effort: self.codex_reasoning_effort,
            codex_sandbox_mode: self.codex_sandbox_mode,
            external_session_id: self.external_session_id,
            pending_claude_approvals: HashMap::new(),
            pending_claude_user_inputs: HashMap::new(),
            pending_codex_approvals: HashMap::new(),
            pending_codex_user_inputs: HashMap::new(),
            pending_codex_mcp_elicitations: HashMap::new(),
            pending_codex_app_requests: HashMap::new(),
            pending_acp_approvals: HashMap::new(),
            pending_acp_approval_order: VecDeque::new(),
            queued_prompts: self.queued_prompts,
            queued_peer_messages: self.queued_peer_messages,
            message_start_index: self.message_start_index,
            message_positions: build_message_positions(&session.messages),
            remote_id: self.remote_id,
            remote_session_id: self.remote_session_id,
            runtime: SessionRuntime::None,
            engram_mcp_installed: None,
            runtime_reset_required: false,
            engram_mcp_runtime_quarantined: false,
            orchestrator_auto_dispatch_blocked: self.orchestrator_auto_dispatch_blocked,
            engram: EngramSessionState {
                routing_token: self.engram_routing_token.clone(),
                active_grant_id: self.engram_open_grant_id,
                rebind_required: self.engram_routing_token.is_some(),
                ..EngramSessionState::default()
            },
            engram_boot_recovery_pending: false,
            engram_boot_recovery_dispatch_pending: false,
            engram_boot_recovery_retry_in_progress: false,
            runtime_stop_in_progress: false,
            runtime_stop_owner: None,
            runtime_stop_generation: 0,
            engram_mcp_revocation_pending: false,
            deferred_stop_callbacks: Vec::new(),
            hidden: false,
            // Freshly loaded records start unstamped; nothing has changed
            // since the on-disk snapshot so nothing needs to be persisted.
            mutation_stamp: 0,
            prompt_history_mutation_stamp: 0,
            session,
        };
        // The persisted record flag is the authority; the wire projection
        // is rebuilt from it so a loaded session never advertises a stale
        // paused/unpaused state from an older snapshot.
        record.session.queue_paused = record.orchestrator_auto_dispatch_blocked;
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

    if session.agent.supports_opencode_settings() {
        let model = session
            .opencode_model
            .as_deref()
            .ok_or_else(|| {
                anyhow!(
                    "persisted session `{}` is missing opencodeModel",
                    session.id
                )
            })
            .and_then(normalize_opencode_model)?;
        let mode = session
            .opencode_mode
            .as_deref()
            .ok_or_else(|| {
                anyhow!(
                    "persisted session `{}` is missing opencodeMode",
                    session.id
                )
            })
            .and_then(normalize_opencode_mode)?;
        let effort = session
            .opencode_effort
            .as_deref()
            .ok_or_else(|| {
                anyhow!(
                    "persisted session `{}` is missing opencodeEffort",
                    session.id
                )
            })
            .and_then(normalize_opencode_effort)?;
        let effective_model = normalize_opencode_model(&session.model)?;
        let current_effort = session
            .opencode_current_effort
            .as_deref()
            .map(normalize_opencode_effort)
            .transpose()?;
        let current_mode = session
            .opencode_current_mode
            .as_deref()
            .map(normalize_opencode_mode)
            .transpose()?;
        if model != session.opencode_model.as_deref().unwrap_or_default()
            || effort != session.opencode_effort.as_deref().unwrap_or_default()
            || mode != session.opencode_mode.as_deref().unwrap_or_default()
            || effective_model != session.model
            || current_effort.as_deref() != session.opencode_current_effort.as_deref()
            || current_mode.as_deref() != session.opencode_current_mode.as_deref()
        {
            return Err(anyhow!(
                "persisted session `{}` has unnormalized OpenCode settings",
                session.id
            ));
        }
    } else if session.opencode_model.is_some()
        || session.opencode_effort.is_some()
        || session.opencode_current_effort.is_some()
        || !session.opencode_effort_options.is_empty()
        || session.opencode_mode.is_some()
        || session.opencode_current_mode.is_some()
        || !session.opencode_mode_options.is_empty()
    {
        return Err(anyhow!(
            "persisted session `{}` should not define OpenCode settings for {} sessions",
            session.id,
            session.agent.name()
        ));
    }

    Ok(())
}
