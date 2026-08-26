// Read-side of `AppState`: snapshot builders, the agent-readiness
// cache, remote SSE-fallback dedup, the workspace file watcher spawn,
// and a handful of session-state readers (`claude_approval_mode`,
// `cursor_mode`, `session_matches_runtime_token`, `clear_runtime`).
//
// Everything here either returns a value to a caller without
// mutating state, or performs targeted session-record cleanup
// (`clear_runtime`). Mutation-heavy routes live in their dedicated
// files — session CRUD in `session_crud.rs`, turn dispatch in
// `turn_dispatch.rs`, settings sync in `session_sync.rs`, and the
// commit/broadcast pipeline in `sse_broadcast.rs`.
//
// Snapshot semantics. `snapshot()` and `snapshot_from_inner()` both
// build metadata-first production-shaped state snapshots. `snapshot()`
// refreshes the agent-readiness cache via filesystem I/O *before*
// locking `inner`, then reads the freshly-populated cache under the
// lock, so the returned `StateResponse` reflects current CLI
// availability. `snapshot_from_inner()` is the hot-path builder used
// inside `commit_locked` / `publish_state_locked` where the lock is
// already held and filesystem I/O is not safe — it reuses whatever
// value `cached_agent_readiness()` happens to have. This is the
// cache-staleness tradeoff documented in `sse_broadcast.rs`.
//
// Tests that need to inspect internal transcript state must call the
// explicit `full_snapshot()` helper instead of relying on a cfg(test)
// behavior split in `snapshot()`.
//
// Remote SSE fallback dedup. When a remote-proxy SSE stream drops,
// this host falls back to polling the remote's `/state` endpoint.
// The three `_remote_sse_fallback_*` methods track which remotes
// already received a resync so the fallback loop doesn't re-push
// identical snapshots back to clients.

#[cfg(test)]
const REMOTE_VISIBLE_SESSION_HYDRATION_TIMEOUT: Duration = Duration::from_millis(100);
#[cfg(not(test))]
const REMOTE_VISIBLE_SESSION_HYDRATION_TIMEOUT: Duration = Duration::from_secs(5);
const SESSION_TAIL_DEFAULT_MESSAGES: usize = 20;
const SESSION_TAIL_HYDRATION_MAX_MESSAGES: usize = 64;
const SESSION_HISTORY_PAGE_MAX_MESSAGES: usize = 64;
const SESSION_OVERVIEW_DEFAULT_BUCKETS: usize = 200;
const SESSION_OVERVIEW_MAX_BUCKETS: usize = 512;
const SESSION_LIVE_ACTIVITY_SUMMARY_MAX_CHARS: usize = 160;

fn bounded_live_activity_summary_text(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= SESSION_LIVE_ACTIVITY_SUMMARY_MAX_CHARS {
        return normalized;
    }
    let mut bounded = normalized
        .chars()
        .take(SESSION_LIVE_ACTIVITY_SUMMARY_MAX_CHARS.saturating_sub(1))
        .collect::<String>();
    bounded.push('…');
    bounded
}

fn bounded_live_activity_summary(
    activity: &Option<SessionLiveActivity>,
) -> Option<SessionLiveActivity> {
    activity.as_ref().map(|activity| SessionLiveActivity {
        prompt: bounded_live_activity_summary_text(&activity.prompt),
        command: activity
            .command
            .as_deref()
            .map(bounded_live_activity_summary_text),
        command_status: activity.command_status,
    })
}

#[derive(Clone, Copy, Default)]
struct ConversationOverviewBucketCounts {
    kinds: [u32; 4],
    user_authored: u32,
    marker_present: bool,
}

fn conversation_overview_kind_index(kind: ConversationOverviewKind) -> usize {
    match kind {
        ConversationOverviewKind::Text => 0,
        ConversationOverviewKind::Command => 1,
        ConversationOverviewKind::Diff => 2,
        ConversationOverviewKind::Error => 3,
    }
}

fn dominant_conversation_overview_kind(
    counts: [u32; 4],
) -> ConversationOverviewKind {
    // Error wins ties, followed by command and diff. Equal-count buckets
    // should preserve the visually strongest signal instead of dissolving it
    // into ordinary text.
    let mut dominant = ConversationOverviewKind::Error;
    let mut dominant_count = counts[conversation_overview_kind_index(dominant)];
    for kind in [
        ConversationOverviewKind::Command,
        ConversationOverviewKind::Diff,
        ConversationOverviewKind::Text,
    ] {
        let count = counts[conversation_overview_kind_index(kind)];
        if count > dominant_count {
            dominant = kind;
            dominant_count = count;
        }
    }
    dominant
}

fn conversation_overview_message_metadata(
    message: &Message,
) -> (ConversationOverviewKind, bool) {
    let (author, kind) = match message {
        Message::Command {
            author,
            status: CommandStatus::Error,
            ..
        } => (author, ConversationOverviewKind::Error),
        Message::ParallelAgents { author, agents, .. }
            if agents
                .iter()
                .any(|agent| agent.status == ParallelAgentStatus::Error) =>
        {
            (author, ConversationOverviewKind::Error)
        }
        Message::Approval {
            author,
            decision:
                ApprovalDecision::Interrupted
                | ApprovalDecision::Canceled
                | ApprovalDecision::Rejected,
            ..
        } => (author, ConversationOverviewKind::Error),
        Message::UserInputRequest { author, state, .. }
        | Message::McpElicitationRequest { author, state, .. }
        | Message::CodexAppRequest { author, state, .. }
            if matches!(
                state,
                InteractionRequestState::Interrupted | InteractionRequestState::Canceled
            ) =>
        {
            (author, ConversationOverviewKind::Error)
        }
        Message::Command { author, .. } => (author, ConversationOverviewKind::Command),
        Message::Diff { author, .. } | Message::FileChanges { author, .. } => {
            (author, ConversationOverviewKind::Diff)
        }
        Message::Text { author, .. }
        | Message::Thinking { author, .. }
        | Message::Markdown { author, .. }
        | Message::SubagentResult { author, .. }
        | Message::ParallelAgents { author, .. }
        | Message::Approval { author, .. }
        | Message::UserInputRequest { author, .. }
        | Message::McpElicitationRequest { author, .. }
        | Message::CodexAppRequest { author, .. } => {
            (author, ConversationOverviewKind::Text)
        }
    };
    (kind, matches!(author, Author::You))
}

fn conversation_overview_bucket_index(
    position: usize,
    message_count: usize,
    bucket_count: usize,
) -> usize {
    debug_assert!(message_count > 0);
    debug_assert!(bucket_count > 0);
    position
        .saturating_mul(bucket_count)
        .checked_div(message_count)
        .unwrap_or_default()
        .min(bucket_count - 1)
}

/// A remote tail repair may fall back to applying the narrow SSE delta when
/// transport or freshness prevents the bounded fetch itself.
fn is_recoverable_remote_tail_miss(err: &ApiError) -> bool {
    matches!(
        err.kind,
        Some(ApiErrorKind::RemoteConnectionUnavailable)
    )
}

#[cfg(test)]
mod visible_session_hydration_error_tests {
    use super::*;

    struct TempStateDir {
        path: PathBuf,
    }

    impl TempStateDir {
        fn new(prefix: &str) -> Self {
            let path = std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&path).expect("test root should be created");
            Self { path }
        }

        fn path(&self) -> &FsPath {
            &self.path
        }
    }

    impl Drop for TempStateDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn wire_sessions_expose_remote_owner_metadata() {
        let root = TempStateDir::new("termal-remote-owner-wire");
        let state_path = root.path().join("state.json");
        let templates_path = root.path().join("orchestrators.json");
        let state = AppState::new_with_paths(
            root.path().to_string_lossy().into_owned(),
            state_path,
            templates_path,
        )
        .expect("test state should initialize");

        let local_session_id = state
            .create_session(CreateSessionRequest {
                name: Some("Local Session".to_owned()),
                agent: Some(Agent::Codex),
                workdir: Some(root.path().to_string_lossy().into_owned()),
                project_id: None,
                model: None,
                approval_policy: None,
                reasoning_effort: None,
                sandbox_mode: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            })
            .expect("local session should be created")
            .session_id;
        let remote_session = Session {
            id: "remote-session-1".to_owned(),
            name: "Remote Proxy".to_owned(),
            emoji: Agent::Codex.avatar().to_owned(),
            agent: Agent::Codex,
            workdir: root.path().to_string_lossy().into_owned(),
            project_id: None,
            remote_id: Some("untrusted-upstream-remote".to_owned()),
            model: Agent::Codex.default_model().to_owned(),
            model_options: Vec::new(),
            approval_policy: Some(default_codex_approval_policy()),
            reasoning_effort: Some(default_codex_reasoning_effort()),
            codex_fast_mode: false,
            sandbox_mode: Some(default_codex_sandbox_mode()),
            cursor_mode: None,
            claude_effort: None,
            claude_approval_mode: None,
            gemini_approval_mode: None,
            opencode_model: None,
            opencode_effort: None,
            opencode_current_effort: None,
            opencode_effort_options: Vec::new(),
            opencode_mode: None,
            opencode_current_mode: None,
            opencode_mode_options: Vec::new(),
            external_session_id: None,
            agent_commands_revision: 0,
            codex_thread_state: None,
            live_activity: None,
            status: SessionStatus::Idle,
            preview: "Remote session ready.".to_owned(),
            messages: Vec::new(),
            prompt_history: Vec::new(),
            prompt_history_redacted: false,
            messages_loaded: true,
            message_count: 0,
            markers: Vec::new(),
            pending_prompts: Vec::new(),
            session_mutation_stamp: Some(7),
            parent_delegation_id: None,
        };
        state
            .apply_remote_delta_event(
                "ssh-lab",
                DeltaEvent::SessionCreated {
                    revision: 1,
                    session_id: remote_session.id.clone(),
                    session: remote_session,
                },
            )
            .expect("remote session-created delta should localize");
        let remote_session_id = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_remote_session_index("ssh-lab", "remote-session-1")
                .expect("localized remote session should exist");
            inner.sessions[index].session.id.clone()
        };

        let summary = state.summary_snapshot();
        let remote_summary_session = summary
            .sessions
            .iter()
            .find(|session| session.id == remote_session_id)
            .expect("remote summary session should exist");
        assert_eq!(remote_summary_session.remote_id.as_deref(), Some("ssh-lab"));
        let local_summary_session = summary
            .sessions
            .iter()
            .find(|session| session.id == local_session_id)
            .expect("local summary session should exist");
        assert!(local_summary_session.remote_id.is_none());

        let remote_detail = state
            .get_session(&remote_session_id)
            .expect("remote session detail should be available");
        assert_eq!(
            remote_detail.session.remote_id.as_deref(),
            Some("ssh-lab")
        );
        let local_detail = state
            .get_session(&local_session_id)
            .expect("local session detail should be available");
        assert!(local_detail.session.remote_id.is_none());
    }

    #[test]
    fn summary_snapshot_omits_pending_prompt_content() {
        let root = TempStateDir::new("termal-summary-pending-prompts");
        let state_path = root.path().join("state.json");
        let templates_path = root.path().join("orchestrators.json");
        let state = AppState::new_with_paths(
            root.path().to_string_lossy().into_owned(),
            state_path,
            templates_path,
        )
        .expect("test state should initialize");
        let session_id = state
            .create_session(CreateSessionRequest {
                name: Some("Queued Session".to_owned()),
                agent: Some(Agent::Codex),
                workdir: Some(root.path().to_string_lossy().into_owned()),
                project_id: None,
                model: None,
                approval_policy: None,
                reasoning_effort: None,
                sandbox_mode: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            })
            .expect("session should be created")
            .session_id;
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&session_id)
                .expect("session should exist");
            inner.sessions[index].session.pending_prompts.push(PendingPrompt {
                attachments: Vec::new(),
                id: "pending-1".to_owned(),
                timestamp: "10:00".to_owned(),
                text: "Sensitive queued prompt".to_owned(),
                expanded_text: Some("Expanded sensitive queued prompt".to_owned()),
                source: None,
            });
            inner.sessions[index].session.prompt_history =
                vec!["Sensitive historical prompt".to_owned()];
        }

        let summary = state.summary_snapshot();
        let summary_session = summary
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("summary session should be present");
        assert!(summary_session.pending_prompts.is_empty());
        assert!(summary_session.prompt_history.is_empty());
        assert!(summary_session.prompt_history_redacted);

        let targeted = state.summary_snapshot_with_session_detail(&session_id);
        let targeted_session = targeted
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("targeted session should be present");
        assert_eq!(targeted_session.pending_prompts.len(), 1);
        assert_eq!(
            targeted_session.prompt_history,
            ["Sensitive historical prompt"]
        );
        assert!(!targeted_session.prompt_history_redacted);
        assert_eq!(
            targeted_session.pending_prompts[0].text,
            "Sensitive queued prompt"
        );
        assert_eq!(
            targeted_session.pending_prompts[0].expanded_text.as_deref(),
            Some("Expanded sensitive queued prompt")
        );
    }

    #[test]
    fn broad_state_snapshot_carries_delegation_links_and_mcp_capability() {
        let root = TempStateDir::new("termal-summary-delegation-links");
        let state = AppState::new_with_paths(
            root.path().to_string_lossy().into_owned(),
            root.path().join("state.sqlite"),
            root.path().join("orchestrators.json"),
        )
        .expect("test state should initialize");
        let record = DelegationRecord {
            id: "delegation-1".to_owned(),
            parent_session_id: "parent-session".to_owned(),
            child_session_id: "child-session".to_owned(),
            mode: DelegationMode::Reviewer,
            status: DelegationStatus::Completed,
            title: "Completed review".to_owned(),
            prompt: "/review-code".to_owned(),
            cwd: root.path().to_string_lossy().into_owned(),
            agent: Agent::Codex,
            model: None,
            write_policy: DelegationWritePolicy::ReadOnly,
            created_at: stamp_now(),
            started_at: Some(stamp_now()),
            completed_at: Some(stamp_now()),
            result: None,
            submitted_review_result: None,
            post_submission_transport_error: None,
            review_result_recovery_probe_attempt: None,
            review_result_recovery_error: None,
            review_result_schema_version: None,
            review_result_required: true,
            review_result_submission_attempt: 1,
            result_parser_version: 7,
        };
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .delegations
            .push(record.clone());

        let json = serde_json::to_value(state.summary_snapshot())
            .expect("summary snapshot should serialize");
        let delegation = json["delegations"][0]
            .as_object()
            .expect("delegation link should be an object");
        assert_eq!(delegation.len(), 4);
        assert_eq!(delegation["id"], record.id);
        assert_eq!(delegation["childSessionId"], record.child_session_id);
        assert_eq!(delegation["mode"], "reviewer");
        assert_eq!(delegation["reviewResultRequired"], true);
        assert!(!delegation.contains_key("status"));
        assert!(!delegation.contains_key("title"));
        assert!(!delegation.contains_key("result"));
        assert!(delegation_child_requires_structured_review_result(
            &json,
            &record.child_session_id,
        ));

        let scoped = serde_json::to_value(delegation_summary_from_record(&record))
            .expect("scoped delegation summary should serialize");
        assert_eq!(scoped["status"], "completed");
        assert_eq!(scoped["title"], "Completed review");
        assert_eq!(scoped["resultParserVersion"], 7);
    }

    #[test]
    fn summary_snapshot_bounds_live_activity_but_targeted_detail_is_complete() {
        let root = TempStateDir::new("termal-summary-live-activity");
        let state = AppState::new_with_paths(
            root.path().to_string_lossy().into_owned(),
            root.path().join("state.sqlite"),
            root.path().join("orchestrators.json"),
        )
        .expect("test state should initialize");
        let session_id = state
            .create_session(CreateSessionRequest {
                name: Some("Active Session".to_owned()),
                agent: Some(Agent::Codex),
                workdir: Some(root.path().to_string_lossy().into_owned()),
                project_id: None,
                model: None,
                approval_policy: None,
                reasoning_effort: None,
                sandbox_mode: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            })
            .expect("session should be created")
            .session_id;
        let full_prompt = format!("{} PROMPT-TAIL", "private prompt ".repeat(40));
        let full_command = format!("{} COMMAND-TAIL", "private command ".repeat(40));
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&session_id)
                .expect("session should exist");
            inner.sessions[index].session.status = SessionStatus::Active;
            inner.sessions[index].session.live_activity = Some(SessionLiveActivity {
                prompt: full_prompt.clone(),
                command: Some(full_command.clone()),
                command_status: Some(CommandStatus::Running),
            });
        }

        let summary = state.summary_snapshot();
        let summary_activity = summary
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| session.live_activity.as_ref())
            .expect("active summary should retain a bounded activity hint");
        assert!(
            summary_activity.prompt.chars().count()
                <= SESSION_LIVE_ACTIVITY_SUMMARY_MAX_CHARS
        );
        assert!(!summary_activity.prompt.contains("PROMPT-TAIL"));
        assert!(
            summary_activity
                .command
                .as_deref()
                .expect("summary command should remain available")
                .chars()
                .count()
                <= SESSION_LIVE_ACTIVITY_SUMMARY_MAX_CHARS
        );
        assert!(
            !summary_activity
                .command
                .as_deref()
                .expect("summary command should remain available")
                .contains("COMMAND-TAIL")
        );

        let targeted = state
            .get_session(&session_id)
            .expect("targeted session should load");
        let targeted_activity = targeted
            .session
            .live_activity
            .expect("targeted detail should retain activity");
        assert_eq!(targeted_activity.prompt, full_prompt);
        assert_eq!(targeted_activity.command.as_deref(), Some(full_command.as_str()));
    }
}

impl AppState {
    fn wire_session_from_record(record: &SessionRecord) -> Session {
        let mut session = record.session.clone();
        // The record owns remote-proxy identity; the wire field is a derived
        // UI/API projection and embedded session snapshots are not authoritative.
        session.remote_id = record.remote_id.clone();
        session.prompt_history_redacted = false;
        session.messages_loaded = record.session.messages_loaded;
        session.message_count = session_message_count(record);
        session.session_mutation_stamp = Some(record.mutation_stamp);
        if session.status != SessionStatus::Active {
            session.live_activity = None;
        }
        session
    }

    fn session_tail_start_index(record: &SessionRecord, message_limit: usize) -> usize {
        let retained_message_count =
            message_limit.min(SESSION_TAIL_HYDRATION_MAX_MESSAGES);
        record
            .session
            .messages
            .len()
            .saturating_sub(retained_message_count)
    }

    fn wire_session_tail_from_record(
        record: &SessionRecord,
        message_limit: usize,
        messages_loaded: bool,
    ) -> Session {
        // Targeted session detail is authorized to carry the session's live
        // interaction state, including queued prompt bodies. Start from the
        // complete projection so new targeted fields cannot be silently lost,
        // then bound only the transcript payload.
        let mut session = Self::wire_session_from_record(record);
        let source_messages = &record.session.messages;
        let start_index = Self::session_tail_start_index(record, message_limit);
        debug_assert!(
            !messages_loaded || start_index == 0,
            "tail projection cannot mark a strict suffix as fully loaded"
        );
        session.messages = source_messages[start_index..].to_vec();
        session.messages_loaded = messages_loaded;
        session
    }

    fn wire_session_summary_from_record(record: &SessionRecord) -> Session {
        let session = &record.session;
        let summary = Session {
            id: session.id.clone(),
            name: session.name.clone(),
            emoji: session.emoji.clone(),
            agent: session.agent,
            workdir: session.workdir.clone(),
            project_id: session.project_id.clone(),
            // Keep this in sync with `wire_session_from_record`: record metadata
            // is the source of truth for remote-proxy ownership.
            remote_id: record.remote_id.clone(),
            model: session.model.clone(),
            model_options: session.model_options.clone(),
            approval_policy: session.approval_policy,
            reasoning_effort: session.reasoning_effort,
            codex_fast_mode: session.codex_fast_mode,
            sandbox_mode: session.sandbox_mode,
            cursor_mode: session.cursor_mode,
            claude_effort: session.claude_effort,
            claude_approval_mode: session.claude_approval_mode,
            gemini_approval_mode: session.gemini_approval_mode,
            opencode_model: session.opencode_model.clone(),
            opencode_effort: session.opencode_effort.clone(),
            opencode_current_effort: session.opencode_current_effort.clone(),
            opencode_effort_options: session.opencode_effort_options.clone(),
            opencode_mode: session.opencode_mode.clone(),
            opencode_current_mode: session.opencode_current_mode.clone(),
            opencode_mode_options: session.opencode_mode_options.clone(),
            external_session_id: session.external_session_id.clone(),
            agent_commands_revision: session.agent_commands_revision,
            codex_thread_state: session.codex_thread_state,
            // Global metadata snapshots need only the short working hint used
            // by the session pane. Keep full user prompts and command bodies
            // on targeted session-detail responses.
            live_activity: (session.status == SessionStatus::Active)
                .then(|| bounded_live_activity_summary(&session.live_activity))
                .flatten(),
            status: session.status,
            preview: session.preview.clone(),
            messages: Vec::new(),
            // Composer history can contain substantial user text. It is
            // available on targeted session-tail responses, not global state.
            prompt_history: Vec::new(),
            prompt_history_redacted: true,
            messages_loaded: false,
            message_count: session_message_count(record),
            markers: session.markers.clone(),
            // Global state snapshots are metadata-first. Pending prompts can
            // contain user-authored prompt bodies, so expose them only through
            // targeted bounded session-detail responses.
            pending_prompts: Vec::new(),
            session_mutation_stamp: Some(record.mutation_stamp),
            parent_delegation_id: session.parent_delegation_id.clone(),
        };
        Self::debug_assert_session_summary_matches_full_projection(record, &summary);
        summary
    }

    #[cfg(debug_assertions)]
    fn debug_assert_session_summary_matches_full_projection(
        record: &SessionRecord,
        summary: &Session,
    ) {
        let full = Self::wire_session_from_record(record);
        debug_assert_eq!(summary.id, full.id);
        debug_assert_eq!(summary.name, full.name);
        debug_assert_eq!(summary.emoji, full.emoji);
        debug_assert_eq!(summary.agent, full.agent);
        debug_assert_eq!(summary.workdir, full.workdir);
        debug_assert_eq!(summary.project_id, full.project_id);
        debug_assert_eq!(summary.remote_id, full.remote_id);
        debug_assert_eq!(summary.model, full.model);
        debug_assert_eq!(summary.model_options, full.model_options);
        debug_assert_eq!(summary.approval_policy, full.approval_policy);
        debug_assert_eq!(summary.reasoning_effort, full.reasoning_effort);
        debug_assert_eq!(summary.codex_fast_mode, full.codex_fast_mode);
        debug_assert_eq!(summary.sandbox_mode, full.sandbox_mode);
        debug_assert_eq!(summary.cursor_mode, full.cursor_mode);
        debug_assert_eq!(summary.claude_effort, full.claude_effort);
        debug_assert_eq!(summary.claude_approval_mode, full.claude_approval_mode);
        debug_assert_eq!(summary.gemini_approval_mode, full.gemini_approval_mode);
        debug_assert_eq!(summary.opencode_model, full.opencode_model);
        debug_assert_eq!(summary.opencode_effort, full.opencode_effort);
        debug_assert_eq!(
            summary.opencode_current_effort,
            full.opencode_current_effort
        );
        debug_assert_eq!(
            summary.opencode_effort_options,
            full.opencode_effort_options
        );
        debug_assert_eq!(summary.opencode_mode, full.opencode_mode);
        debug_assert_eq!(
            summary.opencode_current_mode,
            full.opencode_current_mode
        );
        debug_assert_eq!(
            summary.opencode_mode_options,
            full.opencode_mode_options
        );
        debug_assert_eq!(summary.external_session_id, full.external_session_id);
        debug_assert_eq!(
            summary.agent_commands_revision,
            full.agent_commands_revision
        );
        debug_assert_eq!(summary.codex_thread_state, full.codex_thread_state);
        debug_assert_eq!(
            summary.live_activity,
            bounded_live_activity_summary(&full.live_activity)
        );
        debug_assert_eq!(summary.status, full.status);
        debug_assert_eq!(summary.preview, full.preview);
        debug_assert!(summary.prompt_history_redacted);
        debug_assert!(!full.prompt_history_redacted);
        debug_assert_eq!(summary.message_count, full.message_count);
        debug_assert_eq!(summary.markers, full.markers);
        debug_assert!(summary.pending_prompts.is_empty());
        debug_assert_eq!(summary.session_mutation_stamp, full.session_mutation_stamp);
        debug_assert_eq!(summary.parent_delegation_id, full.parent_delegation_id);
    }

    #[cfg(not(debug_assertions))]
    fn debug_assert_session_summary_matches_full_projection(
        _record: &SessionRecord,
        _summary: &Session,
    ) {
    }

    /// Builds a metadata-first state snapshot with guaranteed-fresh agent readiness.
    ///
    /// The cache is refreshed (filesystem I/O) *before* locking `inner`, then
    /// the snapshot reads `cached_agent_readiness()` *under* the `inner` lock —
    /// the same path used by `commit_locked` / `publish_state_locked`.  This
    /// ensures that a `snapshot()` call at revision N uses the same cached
    /// readiness value that was published in the SSE event for revision N.
    fn snapshot(&self) -> StateResponse {
        self.summary_snapshot()
    }

    fn summary_snapshot(&self) -> StateResponse {
        let _ = self.agent_readiness_snapshot();
        let inner = self.inner.lock().expect("state mutex poisoned");
        self.snapshot_from_inner(&inner)
    }

    fn summary_snapshot_with_session_detail(&self, session_id: &str) -> StateResponse {
        let agent_readiness = self.agent_readiness_snapshot();
        let inner = self.inner.lock().expect("state mutex poisoned");
        self.snapshot_from_inner_with_session_detail(&inner, agent_readiness, session_id)
    }

    /// Test-only full snapshot inspection helper.
    ///
    /// Production `/api/state`, action responses, and SSE state events are
    /// metadata-first. Tests that inspect retained session windows use this
    /// helper so `snapshot()` keeps the same shape in test and production.
    #[cfg(test)]
    fn full_snapshot(&self) -> StateResponse {
        let _ = self.agent_readiness_snapshot();
        let inner = self.inner.lock().expect("state mutex poisoned");
        self.full_snapshot_from_inner(&inner)
    }

    fn agent_readiness_snapshot(&self) -> Vec<AgentReadiness> {
        if let Some(snapshot) = self.cached_agent_readiness_if_fresh() {
            return snapshot;
        }

        let _refresh_lock = self
            .agent_readiness_refresh_lock
            .lock()
            .expect("agent readiness refresh mutex poisoned");
        if let Some(snapshot) = self.cached_agent_readiness_if_fresh() {
            return snapshot;
        }

        let snapshot = collect_agent_readiness(&self.default_workdir);
        let mut cache = self
            .agent_readiness_cache
            .write()
            .expect("agent readiness cache poisoned");
        *cache = AgentReadinessCache::fresh(snapshot);
        cache.snapshot.clone()
    }

    fn cached_agent_readiness_if_fresh(&self) -> Option<Vec<AgentReadiness>> {
        let cache = self
            .agent_readiness_cache
            .read()
            .expect("agent readiness cache poisoned");
        let now = std::time::Instant::now();
        (!cache.needs_refresh(now)).then(|| cache.snapshot.clone())
    }

    fn cached_agent_readiness(&self) -> Vec<AgentReadiness> {
        self.agent_readiness_cache
            .read()
            .expect("agent readiness cache poisoned")
            .snapshot
            .clone()
    }

    /// Test helper for the same bounded default tail exposed by the route.
    #[cfg(test)]
    fn get_session(&self, session_id: &str) -> Result<SessionResponse, ApiError> {
        self.get_session_tail(session_id, SESSION_TAIL_DEFAULT_MESSAGES)
    }

    /// Returns a visible session suffix without marking the transcript fully loaded.
    fn get_session_tail(
        &self,
        session_id: &str,
        message_limit: usize,
    ) -> Result<SessionResponse, ApiError> {
        let should_hydrate_remote_proxy = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(ApiError::local_session_missing)?;
            let record = &inner.sessions[index];
            record.is_remote_proxy()
                && !record.session.messages_loaded
                && record.session.messages.is_empty()
        };
        if should_hydrate_remote_proxy {
            let target = self
                .remote_session_target(session_id)?
                .ok_or_else(ApiError::local_session_missing)?;
            return self.fetch_remote_session_tail_target(
                &target,
                message_limit,
                None,
                None,
                REMOTE_VISIBLE_SESSION_HYDRATION_TIMEOUT,
                None,
                None,
                None,
            );
        }

        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(ApiError::local_session_missing)?;
        let record = &inner.sessions[index];
        let tail_start_index = Self::session_tail_start_index(record, message_limit);
        let messages_loaded = record.session.messages_loaded && tail_start_index == 0;
        Ok(SessionResponse {
            revision: inner.revision,
            session: Self::wire_session_tail_from_record(record, message_limit, messages_loaded),
            server_instance_id: self.server_instance_id.clone(),
        })
    }

    /// Returns a whole-conversation, position-linear overview independent of
    /// the transcript window currently retained in memory.
    fn get_session_overview(
        &self,
        session_id: &str,
        requested_bucket_count: usize,
    ) -> Result<SessionOverviewResponse, ApiError> {
        debug_assert!(
            (1..=SESSION_OVERVIEW_MAX_BUCKETS).contains(&requested_bucket_count)
        );
        if let Some(target) = self.remote_session_target(session_id)? {
            return self.fetch_remote_session_overview_target(
                &target,
                requested_bucket_count,
                REMOTE_VISIBLE_SESSION_HYDRATION_TIMEOUT,
            );
        }

        let (
            mut bucket_counts,
            mut observed_positions,
            resident_start_index,
            message_count,
            bucket_count,
            session_mutation_stamp,
            markers,
        ) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(ApiError::local_session_missing)?;
            let record = &inner.sessions[index];
            let resident_start_index = record.message_start_index;
            let message_count =
                usize::try_from(session_message_count(record)).unwrap_or(usize::MAX);
            let bucket_count = requested_bucket_count.min(message_count.max(1));
            let mut bucket_counts =
                vec![ConversationOverviewBucketCounts::default(); bucket_count];
            let mut observed_positions = 0usize;
            for (local_index, message) in record.session.messages.iter().enumerate() {
                let position = resident_start_index.saturating_add(local_index);
                if position >= message_count {
                    break;
                }
                let (kind, is_user) = conversation_overview_message_metadata(message);
                let bucket_index =
                    conversation_overview_bucket_index(position, message_count, bucket_count);
                let bucket = &mut bucket_counts[bucket_index];
                bucket.kinds[conversation_overview_kind_index(kind)] =
                    bucket.kinds[conversation_overview_kind_index(kind)].saturating_add(1);
                bucket.user_authored = bucket.user_authored.saturating_add(u32::from(is_user));
                observed_positions = observed_positions.saturating_add(1);
            }
            (
                bucket_counts,
                observed_positions,
                resident_start_index,
                message_count,
                bucket_count,
                record.mutation_stamp,
                record.session.markers.clone(),
            )
        };

        if resident_start_index > 0 {
            let connection = open_sqlite_history_snapshot(self.persistence_path.as_ref())
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to open session overview snapshot: {err:#}"
                    ))
                })?;
            let persisted = load_persisted_message_overview_with_connection(
                &connection,
                session_id,
                resident_start_index,
                message_count,
                bucket_count,
            )
            .map_err(|err| {
                ApiError::internal(format!("failed to load session overview: {err:#}"))
            })?;
            let mut persisted_positions = 0usize;
            for (bucket_index, kind, count, user_count) in persisted {
                let bucket = &mut bucket_counts[bucket_index];
                bucket.kinds[conversation_overview_kind_index(kind)] = bucket.kinds
                    [conversation_overview_kind_index(kind)]
                    .saturating_add(count);
                bucket.user_authored = bucket.user_authored.saturating_add(user_count);
                persisted_positions = persisted_positions.saturating_add(count as usize);
            }
            if persisted_positions != resident_start_index {
                return Err(ApiError::conflict(format!(
                    "session overview loaded {persisted_positions} persisted positions but expected {resident_start_index}; refresh the session"
                )));
            }
            observed_positions = observed_positions.saturating_add(persisted_positions);
        }

        if observed_positions != message_count {
            return Err(ApiError::conflict(format!(
                "session overview covered {observed_positions} positions but expected {message_count}; refresh the session"
            )));
        }

        let overview_markers = markers
            .into_iter()
            .map(|marker| {
                let position = if message_count == 0 {
                    0
                } else {
                    marker.message_index_hint.min(message_count - 1)
                };
                if message_count > 0 {
                    let bucket_index = conversation_overview_bucket_index(
                        position,
                        message_count,
                        bucket_count,
                    );
                    bucket_counts[bucket_index].marker_present = true;
                }
                ConversationOverviewMarker {
                    position,
                    kind: marker.kind,
                    label: (!marker.name.trim().is_empty()).then_some(marker.name),
                }
            })
            .collect();
        let buckets = bucket_counts
            .into_iter()
            .map(|counts| ConversationOverviewBucket {
                c: counts.kinds.into_iter().sum(),
                k: dominant_conversation_overview_kind(counts.kinds),
                u: counts.user_authored,
                m: counts.marker_present,
            })
            .collect();

        Ok(SessionOverviewResponse {
            session_id: session_id.to_owned(),
            message_count: u32::try_from(message_count).unwrap_or(u32::MAX),
            session_mutation_stamp,
            buckets,
            markers: overview_markers,
            latest_position: message_count.saturating_sub(1),
        })
    }

    /// Returns one exclusive-before, ascending transcript page.
    ///
    /// Remote proxies forward the same bounded cursor query to the upstream
    /// backend; they never materialize an unbounded transcript locally.
    fn get_session_history(
        &self,
        session_id: &str,
        before: Option<&str>,
        after: Option<&str>,
        around: Option<usize>,
        from_start: bool,
        message_limit: usize,
    ) -> Result<SessionHistoryResponse, ApiError> {
        debug_assert!((1..=SESSION_HISTORY_PAGE_MAX_MESSAGES).contains(&message_limit));
        if let Some(target) = self.remote_session_target(session_id)? {
            return self.fetch_remote_session_history_target(
                &target,
                before,
                after,
                around,
                from_start,
                message_limit,
                REMOTE_VISIBLE_SESSION_HYDRATION_TIMEOUT,
            );
        }

        let (
            local_messages,
            local_start_index,
            local_cursor_position,
            message_count,
            revision,
            session_mutation_stamp,
        ) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(ApiError::local_session_missing)?;
            let record = &inner.sessions[index];
            (
                record.session.messages.clone(),
                record.message_start_index,
                before.or(after).and_then(|cursor_id| {
                    record
                        .message_positions
                        .get(cursor_id)
                        .copied()
                        .map(|local_index| global_message_index(record, local_index))
                }),
                usize::try_from(session_message_count(record)).unwrap_or(usize::MAX),
                inner.revision,
                record.mutation_stamp,
            )
        };
        let cursor = before.or(after);
        // A local history request uses at most one read connection. Cursor
        // resolution and page loading must observe the same SQLite snapshot,
        // and opening the database twice adds avoidable filesystem/pragma work
        // to the scroll path.
        let mut persistence_connection = None;
        let cursor_position = match (cursor, local_cursor_position) {
            (_, Some(position)) => Some(position),
            (Some(cursor_id), None) => {
                let connection =
                    open_sqlite_history_snapshot(self.persistence_path.as_ref()).map_err(
                        |err| {
                            ApiError::internal(format!(
                                "failed to open session history snapshot: {err:#}"
                            ))
                        },
                    )?;
                let position = persisted_message_position_with_connection(
                    &connection,
                    session_id,
                    cursor_id,
                )
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to resolve session history cursor: {err:#}"
                    ))
                })?
                .ok_or_else(|| {
                    ApiError::conflict(
                        "session history cursor is no longer available; refresh the session tail",
                    )
                })?;
                persistence_connection = Some(connection);
                Some(position)
            }
            (None, None) => None,
        };
        let (start_index, end_index) = if let Some(around) = around {
            if around >= message_count && message_count > 0 {
                return Err(ApiError::conflict(
                    "session history position is beyond the current transcript; refresh the session overview",
                ));
            }
            let half_page = message_limit / 2;
            let mut start_index = around.saturating_sub(half_page);
            let end_index = start_index.saturating_add(message_limit).min(message_count);
            start_index = end_index.saturating_sub(message_limit);
            (start_index, end_index)
        } else if from_start {
            (0, message_limit.min(message_count))
        } else if after.is_some() {
            let start_index = cursor_position
                .unwrap_or(message_count)
                .saturating_add(1)
                .min(message_count);
            (
                start_index,
                start_index.saturating_add(message_limit).min(message_count),
            )
        } else {
            let end_index = cursor_position.unwrap_or(message_count);
            (end_index.saturating_sub(message_limit), end_index)
        };
        if end_index > message_count {
            return Err(ApiError::conflict(
                "session history cursor is beyond the current transcript; refresh the session tail",
            ));
        }
        let mut page = vec![None; end_index.saturating_sub(start_index)];
        for (local_index, message) in local_messages.into_iter().enumerate() {
            let global_index = local_start_index.saturating_add(local_index);
            if global_index < start_index || global_index >= end_index {
                continue;
            }
            page[global_index - start_index] = Some(message);
        }
        if page.iter().any(Option::is_none) {
            if persistence_connection.is_none() {
                persistence_connection = Some(
                    open_sqlite_history_snapshot(self.persistence_path.as_ref()).map_err(
                        |err| {
                            ApiError::internal(format!(
                                "failed to open session history snapshot: {err:#}"
                            ))
                        },
                    )?,
                );
            }
            let persisted_messages = load_persisted_message_range_with_connection(
                persistence_connection
                    .as_ref()
                    .expect("history snapshot should be open"),
                session_id,
                start_index,
                end_index,
            )
            .map_err(|err| {
                ApiError::internal(format!("failed to load session history page: {err:#}"))
            })?;
            for (position, message) in persisted_messages {
                if position >= start_index && position < end_index {
                    let slot = &mut page[position - start_index];
                    if slot.is_none() {
                        *slot = Some(message);
                    }
                }
            }
        }
        let page = page
            .into_iter()
            .enumerate()
            .map(|(offset, message)| {
                message.ok_or_else(|| {
                    ApiError::conflict(format!(
                        "session history is missing persisted position {}; refresh the session tail",
                        start_index + offset
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = start_index > 0 && !page.is_empty();
        let next_before = has_more
            .then(|| page.first().map(|message| message.id().to_owned()))
            .flatten();
        let has_newer = end_index < message_count && !page.is_empty();
        let next_after = has_newer
            .then(|| page.last().map(|message| message.id().to_owned()))
            .flatten();

        Ok(SessionHistoryResponse {
            messages: page,
            next_before,
            has_more,
            next_after,
            has_newer,
            message_start_index: start_index,
            message_count: u32::try_from(message_count).unwrap_or(u32::MAX),
            revision,
            session_mutation_stamp,
            server_instance_id: self.server_instance_id.clone(),
        })
    }

    fn invalidate_agent_readiness_cache(&self) {
        let _refresh_lock = self
            .agent_readiness_refresh_lock
            .lock()
            .expect("agent readiness refresh mutex poisoned");
        self.agent_readiness_cache
            .write()
            .expect("agent readiness cache poisoned")
            .invalidated = true;
    }

    /// Returns whether a remote fallback-driven /api/state resync can be
    /// skipped because the same or a newer fallback revision was already
    /// recovered for that remote within the current event-stream lifetime.
    fn should_skip_remote_sse_fallback_resync(
        &self,
        remote_id: &str,
        fallback_revision: u64,
    ) -> bool {
        self.remote_sse_fallback_resynced_revision
            .lock()
            .expect("remote fallback resync mutex poisoned")
            .get(remote_id)
            .is_some_and(|last_revision| *last_revision >= fallback_revision)
    }

    fn should_skip_remote_sse_fallback_resync_for_bridge(
        &self,
        remote: &RemoteConfig,
        connection: &RemoteConnection,
        fallback_revision: u64,
    ) -> Result<bool, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        self.ensure_remote_apply_authority_locked(&inner, remote, Some(connection))?;
        Ok(self.should_skip_remote_sse_fallback_resync(&remote.id, fallback_revision))
    }

    /// Records that a remote fallback-driven /api/state resync recovered the
    /// given fallback revision.
    fn note_remote_sse_fallback_resync(&self, remote_id: &str, fallback_revision: u64) {
        self.remote_sse_fallback_resynced_revision
            .lock()
            .expect("remote fallback resync mutex poisoned")
            .entry(remote_id.to_owned())
            .and_modify(|last_revision| {
                *last_revision = (*last_revision).max(fallback_revision);
            })
            .or_insert(fallback_revision);
    }

    fn note_remote_sse_fallback_resync_for_bridge(
        &self,
        remote: &RemoteConfig,
        connection: &RemoteConnection,
        fallback_revision: u64,
    ) -> Result<(), ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        self.ensure_remote_apply_authority_locked(&inner, remote, Some(connection))?;
        self.note_remote_sse_fallback_resync(&remote.id, fallback_revision);
        Ok(())
    }

    /// Clears the latest applied remote revision when event-stream continuity
    /// is lost, such as after a disconnect or restart.
    #[cfg(test)]
    fn clear_remote_applied_revision(&self, remote_id: &str) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let _ = self.clear_remote_applied_revision_locked(&mut inner, remote_id);
    }

    /// Locked counterpart used when settings publication must clear continuity
    /// state before releasing the same state guard.
    fn clear_remote_applied_revision_locked(
        &self,
        inner: &mut StateInner,
        remote_id: &str,
    ) -> bool {
        let mut cleared = inner.remote_applied_revisions.remove(remote_id).is_some();
        cleared |= inner
            .remote_snapshot_applied_revisions
            .remove(remote_id)
            .is_some();
        cleared |= inner
            .remote_session_transcript_applied_revisions
            .remove(remote_id)
            .is_some();
        cleared |= self
            .remote_delta_replay_cache
            .lock()
            .expect("remote delta replay cache mutex poisoned")
            .remove_remote(remote_id);
        // Do not clear remote_delta_hydrations_in_flight here: those markers
        // are owned by RAII guards in the in-flight hydration callers. Removing
        // them during continuity cleanup would allow duplicate fetches while
        // the original request is still running.
        cleared
    }

    /// Clears remote fallback resync tracking when event-stream continuity is
    /// lost, such as after a disconnect or restart.
    fn clear_remote_sse_fallback_resync(&self, remote_id: &str) -> bool {
        self.remote_sse_fallback_resynced_revision
            .lock()
            .expect("remote fallback resync mutex poisoned")
            .remove(remote_id)
            .is_some()
    }

    /// Clears bridge continuity only while that exact route and connection
    /// still own the remote id. The application-state guard linearizes this
    /// cleanup against settings publication, preventing a retiring worker from
    /// erasing watermarks established by its replacement.
    fn clear_remote_bridge_continuity_if_current(
        &self,
        remote: &RemoteConfig,
        connection: &RemoteConnection,
    ) -> bool {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if self
            .ensure_remote_apply_authority_locked(&inner, remote, Some(connection))
            .is_err()
        {
            return false;
        }
        let cleared_revision = self.clear_remote_applied_revision_locked(&mut inner, &remote.id);
        let cleared_fallback = self.clear_remote_sse_fallback_resync(&remote.id);
        if cleared_revision || cleared_fallback {
            connection.invalidate_state_continuity();
        }
        true
    }




    #[cfg(not(test))]
    fn spawn_workspace_file_watcher(&self) {
        let state = self.clone();
        std::thread::Builder::new()
            .name("termal-file-watch".to_owned())
            .spawn(move || run_workspace_file_watcher(state))
            .expect("failed to spawn file watcher thread");
    }


    /// Builds a snapshot using the latest cached agent readiness **without refreshing**.
    ///
    /// This is the hot-path builder used inside `commit_locked` / `publish_state_locked`
    /// where the `inner` mutex is held and filesystem I/O is not safe.  Callers that
    /// need guaranteed-fresh readiness (e.g. after an explicit cache invalidation) should
    /// drop the `inner` lock and use [`snapshot()`](Self::snapshot) instead.
    ///
    /// **Design tradeoff:** after the cache TTL expires, mutation paths through
    /// `commit_locked` will publish SSE events with stale readiness until a
    /// [`snapshot()`](Self::snapshot) call (e.g. `GET /api/state`, SSE reconnect)
    /// refreshes the cache.  This staleness can span multiple revisions — it is
    /// not bounded to a single mutation cycle.  This is acceptable because agent
    /// readiness changes only when CLI tools are installed or removed (extremely
    /// rare during an active session), and any `snapshot()` call refreshes the
    /// cache as a side effect even when the frontend drops the response, so the
    /// following mutation carries the fresh value.  Paths where freshness matters
    /// (`create_session`, `update_app_settings`) pre-refresh the cache before
    /// entering the critical section.
    fn snapshot_from_inner(&self, inner: &StateInner) -> StateResponse {
        self.snapshot_from_inner_with_agent_readiness(inner, self.cached_agent_readiness())
    }

    fn snapshot_from_inner_with_agent_readiness(
        &self,
        inner: &StateInner,
        agent_readiness: Vec<AgentReadiness>,
    ) -> StateResponse {
        StateResponse {
            revision: inner.revision,
            server_instance_id: self.server_instance_id.clone(),
            codex: inner.codex.clone(),
            agent_readiness,
            preferences: inner.preferences.clone(),
            projects: inner.projects.clone(),
            orchestrators: inner.orchestrator_instances.clone(),
            workspaces: collect_workspace_layout_summaries(inner.workspace_layouts.values()),
            sessions: inner
                .sessions
                .iter()
                .filter(|record| !record.hidden)
                .map(Self::wire_session_summary_from_record)
                .collect(),
            delegations: inner
                .delegations
                .iter()
                .map(delegation_state_summary_from_record)
                .collect(),
            delegation_waits: inner.delegation_waits.clone(),
        }
    }

    fn snapshot_from_inner_with_session_detail(
        &self,
        inner: &StateInner,
        agent_readiness: Vec<AgentReadiness>,
        full_session_id: &str,
    ) -> StateResponse {
        StateResponse {
            revision: inner.revision,
            server_instance_id: self.server_instance_id.clone(),
            codex: inner.codex.clone(),
            agent_readiness,
            preferences: inner.preferences.clone(),
            projects: inner.projects.clone(),
            orchestrators: inner.orchestrator_instances.clone(),
            workspaces: collect_workspace_layout_summaries(inner.workspace_layouts.values()),
            sessions: inner
                .sessions
                .iter()
                .filter(|record| !record.hidden)
                .map(|record| {
                    if record.session.id == full_session_id {
                        Self::wire_session_from_record(record)
                    } else {
                        Self::wire_session_summary_from_record(record)
                    }
                })
                .collect(),
            delegations: inner
                .delegations
                .iter()
                .map(delegation_state_summary_from_record)
                .collect(),
            delegation_waits: inner.delegation_waits.clone(),
        }
    }

    #[cfg(test)]
    fn full_snapshot_from_inner(&self, inner: &StateInner) -> StateResponse {
        self.full_snapshot_from_inner_with_agent_readiness(inner, self.cached_agent_readiness())
    }

    #[cfg(test)]
    fn full_snapshot_from_inner_with_agent_readiness(
        &self,
        inner: &StateInner,
        agent_readiness: Vec<AgentReadiness>,
    ) -> StateResponse {
        StateResponse {
            revision: inner.revision,
            server_instance_id: self.server_instance_id.clone(),
            codex: inner.codex.clone(),
            agent_readiness,
            preferences: inner.preferences.clone(),
            projects: inner.projects.clone(),
            orchestrators: inner.orchestrator_instances.clone(),
            workspaces: collect_workspace_layout_summaries(inner.workspace_layouts.values()),
            sessions: inner
                .sessions
                .iter()
                .filter(|record| !record.hidden)
                .map(Self::wire_session_from_record)
                .collect(),
            delegations: inner
                .delegations
                .iter()
                .map(delegation_state_summary_from_record)
                .collect(),
            delegation_waits: inner.delegation_waits.clone(),
        }
    }








    /// Returns the effective Claude approval mode for a session
    /// (falling back to the app default when the session hasn't
    /// overridden it). Used by the runtime spawn helpers and the
    /// "approve all" UI toggle.
    fn claude_approval_mode(&self, session_id: &str) -> Result<ClaudeApprovalMode> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        Ok(inner.sessions[index]
            .session
            .claude_approval_mode
            .unwrap_or_else(default_claude_approval_mode))
    }

    /// Returns the effective Cursor agent mode for a session
    /// (`Agent` / `Composer` / etc., falling back to the app
    /// default). Used by the Cursor spawn helpers.
    fn cursor_mode(&self, session_id: &str) -> Result<CursorMode> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        Ok(inner.sessions[index]
            .session
            .cursor_mode
            .unwrap_or_else(default_cursor_mode))
    }

    /// Compares the session's current `SessionRuntime` handle against
    /// an expected `RuntimeToken`. Used by the `_if_runtime_matches`
    /// guard wrappers in `turn_lifecycle.rs` to drop stray events
    /// from torn-down runtimes (see `session_runtime.rs` for the
    /// token lifecycle).
    fn session_matches_runtime_token(&self, session_id: &str, token: &RuntimeToken) -> bool {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner
            .find_session_index(session_id)
            .and_then(|index| inner.sessions.get(index))
            .is_some_and(|record| record.runtime.matches_runtime_token(token))
    }

    /// Zeros out the session's runtime state — drops the
    /// `SessionRuntime` handle, clears pending approvals / user
    /// inputs / file-change tracking / deferred stop callbacks —
    /// leaving the session at a clean `SessionStatus::Idle`. Invoked
    /// when a runtime exit has been fully processed and nothing
    /// should remain bound to the dead process.
    fn clear_runtime(&self, session_id: &str) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let had_changes = !matches!(record.runtime, SessionRuntime::None)
            || record.runtime_reset_required
            || record.runtime_stop_in_progress
            || has_pending_requests(record);
        if !had_changes {
            return Ok(());
        }

        record.runtime = SessionRuntime::None;
        record.runtime_reset_required = false;
        record.orchestrator_auto_dispatch_blocked = false;
        record.runtime_stop_in_progress = false;
        record.deferred_stop_callbacks.clear();
        clear_active_turn_file_change_tracking(record);
        clear_all_pending_requests(record);
        self.commit_locked(&mut inner)?;
        Ok(())
    }



}
