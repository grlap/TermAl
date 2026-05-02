// `StateInner` implementation bulk: everything between the boot
// helpers (in `state_boot.rs`) and the core type / constructor /
// persist-revision helpers that stay in `state.rs`.
//
// What lives here:
//
// - **CRUD primitives**: `StateInner::create_project` and
//   `StateInner::create_session` are the under-lock constructors
//   that actually append rows to the `projects` and `sessions`
//   vectors. The `AppState::create_session` public surface in
//   `session_crud.rs` is a thin wrapper that resolves defaults,
//   refreshes the readiness cache, and then calls these.
//
// - **Codex thread discovery + hidden Claude spares**:
//   `ignore_discovered_codex_thread` / `allow_discovered_codex_thread`
//   (user explicitly hides/unhides imported threads),
//   `find_matching_hidden_claude_spare` /
//   `ensure_hidden_claude_spare` — the spare-matching logic keyed
//   on `(workdir, project, model, approval_mode, effort)` that
//   `session_crud.rs` and `claude_spares.rs` consume.
//
// - **Session-array primitives**: `next_message_id` + `next_mutation_stamp`
//   counters, the `session_mut*` accessors that bump `mutation_stamp`
//   on every hand-out (load-bearing for persist-delta correctness),
//   `push_session` / `remove_session_at` / `retain_sessions`, and
//   `collect_persist_delta` which the persist thread drains each tick
//   to write only sessions that have changed since its last watermark.
//
// - **Finders**: `find_session_index` (all sessions including hidden
//   spares), `find_visible_session_index` (user-facing only —
//   excludes hidden Claude spares), `find_remote_session_index`,
//   `find_remote_orchestrator_index`, `find_project`, `find_remote`,
//   `find_project_for_workdir`. The "visible" filter is a recurring
//   gotcha — most API routes want it to avoid accidentally operating
//   on a pre-warmed spare.
//
// The `#[warn(dead_code)]` exemption on `session_mut` and
// `stamp_session_at_index` is intentional: both are test-only
// helpers retained for mutation-stamp regression coverage. Keeping
// them callable from `#[cfg(test)]` code without a `#[cfg(test)]`
// gate here lets the shared helper stay one definition.

impl StateInner {
    /// Appends a new [`Project`] record to `self.projects` and
    /// returns a clone. The public `AppState::create_project` in
    /// `session_crud.rs` handles remote proxying, workdir
    /// normalization, idempotence checks, and broadcast; this helper
    /// is the under-lock critical-section.
    fn create_project(
        &mut self,
        name: Option<String>,
        root_path: String,
        remote_id: String,
    ) -> Project {
        if let Some(existing) = self
            .projects
            .iter()
            .find(|project| project.remote_id == remote_id && project.root_path == root_path)
            .cloned()
        {
            return existing;
        }

        let number = self.next_project_number;
        self.next_project_number += 1;
        let base_name = name.unwrap_or_else(|| default_project_name(&root_path));
        let project = Project {
            id: format!("project-{number}"),
            name: dedupe_project_name(&self.projects, &base_name),
            root_path,
            remote_id,
            remote_project_id: None,
        };
        self.projects.push(project.clone());
        project
    }

    /// Appends a new [`SessionRecord`] to `self.sessions`. Callers
    /// outside this file go through [`AppState::create_session`] in
    /// `session_crud.rs`, which resolves defaults, pre-refreshes the
    /// readiness cache, and broadcasts. This helper is the under-
    /// lock critical-section that actually builds the record +
    /// appends it.
    fn create_session(
        &mut self,
        agent: Agent,
        name: Option<String>,
        workdir: String,
        project_id: Option<String>,
        model: Option<String>,
    ) -> SessionRecord {
        let number = self.next_session_number;
        self.next_session_number += 1;

        let record = SessionRecord {
            active_codex_approval_policy: None,
            active_codex_reasoning_effort: None,
            active_codex_sandbox_mode: None,
            active_turn_start_message_count: None,
            active_turn_file_changes: BTreeMap::new(),
            active_turn_file_change_grace_deadline: None,
            agent_commands: Vec::new(),
            codex_approval_policy: default_codex_approval_policy(),
            codex_reasoning_effort: self.preferences.default_codex_reasoning_effort,
            codex_sandbox_mode: default_codex_sandbox_mode(),
            external_session_id: None,
            pending_claude_approvals: HashMap::new(),
            pending_codex_approvals: HashMap::new(),
            pending_codex_user_inputs: HashMap::new(),
            pending_codex_mcp_elicitations: HashMap::new(),
            pending_codex_app_requests: HashMap::new(),
            pending_acp_approvals: HashMap::new(),
            queued_prompts: VecDeque::new(),
            message_positions: HashMap::new(),
            remote_id: None,
            remote_session_id: None,
            runtime: SessionRuntime::None,
            runtime_reset_required: false,
            orchestrator_auto_dispatch_blocked: false,
            runtime_stop_in_progress: false,
            deferred_stop_callbacks: Vec::new(),
            hidden: false,
            // Freshly created records start unstamped; the call path
            // immediately inserts this record and then the caller routes
            // subsequent edits through `session_mut*`, which bumps the
            // stamp as soon as a mutation happens.
            mutation_stamp: 0,
            session: Session {
                id: format!("session-{number}"),
                name: name.unwrap_or_else(|| format!("{} {}", agent.name(), number)),
                emoji: agent.avatar().to_owned(),
                agent,
                workdir,
                project_id,
                model: model.unwrap_or_else(|| agent.default_model().to_owned()),
                model_options: Vec::new(),
                approval_policy: None,
                reasoning_effort: None,
                sandbox_mode: None,
                cursor_mode: agent
                    .supports_cursor_mode()
                    .then_some(default_cursor_mode()),
                claude_approval_mode: agent
                    .supports_claude_approval_mode()
                    .then_some(self.preferences.default_claude_approval_mode),
                claude_effort: agent
                    .supports_claude_approval_mode()
                    .then_some(self.preferences.default_claude_effort),
                gemini_approval_mode: agent
                    .supports_gemini_approval_mode()
                    .then_some(default_gemini_approval_mode()),
                external_session_id: None,
                agent_commands_revision: 0,
                codex_thread_state: None,
                status: SessionStatus::Idle,
                preview: "Ready for a prompt.".to_owned(),
                messages: Vec::new(),
                messages_loaded: true,
                message_count: 0,
                markers: Vec::new(),
                pending_prompts: Vec::new(),
                session_mutation_stamp: None,
                parent_delegation_id: None,
            },
        };

        let mut record = record;
        if record.session.agent.supports_codex_prompt_settings() {
            record.session.approval_policy = Some(record.codex_approval_policy);
            record.session.reasoning_effort = Some(record.codex_reasoning_effort);
            record.session.sandbox_mode = Some(record.codex_sandbox_mode);
        } else if record.session.agent.supports_claude_approval_mode() {
            record.session.claude_approval_mode = Some(self.preferences.default_claude_approval_mode);
            record.session.claude_effort = Some(self.preferences.default_claude_effort);
        }

        self.push_session(record.clone());
        record
    }

    /// Adds a Codex `threadId` to the user's ignore list so it is
    /// skipped on subsequent startup discovery scans (see
    /// `state_boot.rs::import_discovered_codex_threads`).
    fn ignore_discovered_codex_thread(&mut self, thread_id: Option<&str>) {
        if let Some(thread_id) = normalize_optional_identifier(thread_id) {
            self.ignored_discovered_codex_thread_ids
                .insert(thread_id.to_owned());
        }
    }

    /// Removes a Codex `threadId` from the ignore list so the next
    /// discovery scan re-imports it as a resumeable session.
    fn allow_discovered_codex_thread(&mut self, thread_id: Option<&str>) {
        if let Some(thread_id) = normalize_optional_identifier(thread_id) {
            self.ignored_discovered_codex_thread_ids.remove(thread_id);
        }
    }

    /// Returns the index of a hidden Claude spare whose dimensions
    /// `(workdir, project, model, approval_mode, effort)` match the
    /// requested tuple so [`AppState::create_session`] can promote
    /// the warmed runtime instead of cold-starting.
    fn find_matching_hidden_claude_spare(
        &self,
        workdir: &str,
        project_id: Option<&str>,
        model: &str,
        approval_mode: ClaudeApprovalMode,
        effort: ClaudeEffortLevel,
    ) -> Option<usize> {
        self.sessions.iter().position(|record| {
            record.hidden
                && !record.is_remote_proxy()
                && record.session.agent == Agent::Claude
                && record.session.workdir == workdir
                && record.session.project_id.as_deref() == project_id
                && record.session.model == model
                && record.session.claude_approval_mode == Some(approval_mode)
                && record.session.claude_effort == Some(effort)
        })
    }

    /// Reserves a placeholder hidden-spare `SessionRecord` for the
    /// requested shape if one doesn't already exist, returning the
    /// session id the caller should hand to
    /// `try_start_hidden_claude_spare` (see `claude_spares.rs`) to
    /// actually spawn the child process.
    fn ensure_hidden_claude_spare(
        &mut self,
        workdir: String,
        project_id: Option<String>,
        model: String,
        approval_mode: ClaudeApprovalMode,
        effort: ClaudeEffortLevel,
    ) -> Option<String> {
        if let Some(index) = self.find_matching_hidden_claude_spare(
            &workdir,
            project_id.as_deref(),
            &model,
            approval_mode,
            effort,
        ) {
            let record = self
                .session_mut_by_index(index)
                .expect("session index should be valid");
            reset_hidden_claude_spare_record(record);
            return matches!(record.runtime, SessionRuntime::None)
                .then(|| record.session.id.clone());
        }

        self.create_session(Agent::Claude, None, workdir, project_id, Some(model));
        let record = self
            .sessions
            .last_mut()
            .expect("create_session should append a session record");
        record.hidden = true;
        record.session.claude_approval_mode = Some(approval_mode);
        record.session.claude_effort = Some(effort);
        reset_hidden_claude_spare_record(record);
        Some(record.session.id.clone())
    }

    /// Returns the next message ID.
    fn next_message_id(&mut self) -> String {
        let id = format!("message-{}", self.next_message_number);
        self.next_message_number += 1;
        id
    }

    /// Returns a fresh monotonic mutation stamp.
    ///
    /// Every `session_mut*` helper calls this before handing out mutable
    /// access to a `SessionRecord`. The persist thread compares each
    /// record's `mutation_stamp` against its watermark to identify the
    /// exact subset of sessions that changed since the last successful
    /// persist, so a `commit_locked` on one session no longer re-
    /// serializes every other session row.
    fn next_mutation_stamp(&mut self) -> u64 {
        self.last_mutation_stamp = self.last_mutation_stamp.saturating_add(1);
        self.last_mutation_stamp
    }

    fn mark_delegations_mutated(&mut self) {
        self.delegation_mutation_stamp = self.next_mutation_stamp();
    }

    /// Stamps the session at `index` with the next mutation stamp.
    ///
    /// Use when the caller already has the index (e.g., from a loop or a
    /// prior `find_session_index`) and needs to re-stamp the slot
    /// WITHOUT using the resulting `&mut SessionRecord`. The sole
    /// production caller today is `import_discovered_codex_threads` in
    /// `state_boot.rs`, which swaps an owned record into the slot via
    /// `*slot = record` and then re-stamps the slot so the SQLite
    /// delta persist picks up the row. Returns the assigned stamp,
    /// or `None` if the index is out of bounds.
    fn stamp_session_at_index(&mut self, index: usize) -> Option<u64> {
        // Bounds check before the stamp so an OOB miss does not burn a
        // mutation stamp with no record to attach it to. Advancing
        // `last_mutation_stamp` without a matching record would grow the
        // global watermark gap by one per miss and break the invariant
        // "stamp implies an actual mutation".
        if index >= self.sessions.len() {
            return None;
        }
        let stamp = self.next_mutation_stamp();
        let record = &mut self.sessions[index];
        record.mutation_stamp = stamp;
        Some(stamp)
    }

    /// Finds a session by id and returns mutable access, stamping the
    /// record so the persist thread picks up the mutation on its next
    /// tick. Returns `None` if no session matches.
    ///
    /// Only called from tests today — retained for mutation-stamp
    /// regression coverage. Production code uses
    /// [`StateInner::session_mut_by_index`] after an explicit
    /// `find_session_index` to make the visibility filter explicit.
    #[cfg_attr(not(test), allow(dead_code))]
    fn session_mut(&mut self, session_id: &str) -> Option<&mut SessionRecord> {
        let index = self.find_session_index(session_id)?;
        let stamp = self.next_mutation_stamp();
        let record = self.sessions.get_mut(index)?;
        record.mutation_stamp = stamp;
        Some(record)
    }

    /// Read-only indexed access, the mirror of
    /// [`Self::session_mut_by_index`] without the stamp bump.
    ///
    /// Use when a caller needs to inspect a field (e.g. compare an
    /// incoming value to the current one and return early on no
    /// change) before deciding whether to mutate. The `session_mut*`
    /// helpers stamp eagerly — they hand out a `&mut` borrow and
    /// can't know whether the caller will actually change anything
    /// — so a check-then-early-return caller using `session_mut*`
    /// permanently marks the session dirty and forces
    /// `collect_persist_delta` to re-serialize its row on the next
    /// tick. Reading through this helper first keeps the stamp
    /// unchanged on the no-op path. Callers that decide to mutate
    /// after the read should re-borrow via `session_mut_by_index`
    /// to pick up a fresh stamp.
    #[cfg_attr(test, allow(dead_code))]
    fn session_by_index(&self, index: usize) -> Option<&SessionRecord> {
        self.sessions.get(index)
    }

    /// Like [`StateInner::session_mut`] but indexed directly. Returns
    /// `None` for out-of-bounds indices without advancing
    /// `last_mutation_stamp`. Callers should still obtain the index via
    /// `find_session_index` / `find_visible_session_index` where
    /// possible — the `None` return exists to keep the helper sound on
    /// stale indices, not to make "guess the index" patterns ergonomic.
    fn session_mut_by_index(&mut self, index: usize) -> Option<&mut SessionRecord> {
        // Bounds check before the stamp — see `stamp_session_at_index`
        // for the rationale. The by-id `session_mut` is already safe
        // because `find_session_index` short-circuits on miss before
        // `next_mutation_stamp` runs.
        if index >= self.sessions.len() {
            return None;
        }
        let stamp = self.next_mutation_stamp();
        let record = &mut self.sessions[index];
        record.mutation_stamp = stamp;
        Some(record)
    }

    /// Records that a session id has been removed from `sessions` since
    /// the last persist tick. Drained by the persist thread and applied
    /// as targeted `DELETE` statements so removed rows do not linger in
    /// SQLite after the move to delta persistence.
    fn record_removed_session(&mut self, session_id: String) {
        if !session_id.is_empty() {
            self.removed_session_ids.push(session_id);
        }
    }

    /// Restores explicit tombstones drained into a failed persist delta.
    ///
    /// Hidden-session deletes are synthesized from still-hidden records on each
    /// collection pass, so they must not be restored into this explicit queue.
    fn restore_drained_explicit_tombstones(&mut self, session_ids: &[String]) {
        let mut known_removed_ids: HashSet<&str> = self
            .removed_session_ids
            .iter()
            .map(String::as_str)
            .collect();
        let mut restored_session_ids = Vec::new();
        for session_id in session_ids {
            if known_removed_ids.insert(session_id.as_str()) {
                restored_session_ids.push(session_id.clone());
            }
        }
        drop(known_removed_ids);
        self.removed_session_ids.extend(restored_session_ids);
    }

    /// Inserts a new session record, stamping it so the persist thread
    /// picks it up on its next tick. Returns the index at which the
    /// record was inserted (end of the `sessions` vec).
    fn push_session(&mut self, mut record: SessionRecord) -> usize {
        let stamp = self.next_mutation_stamp();
        record.mutation_stamp = stamp;
        self.sessions.push(record);
        self.sessions.len() - 1
    }

    /// Removes the session at `index`, recording its id in
    /// `removed_session_ids` so the persist thread issues a `DELETE`
    /// on its next tick. Panics on out-of-bounds access like the
    /// underlying `Vec::remove` it wraps.
    fn remove_session_at(&mut self, index: usize) -> SessionRecord {
        let record = self.sessions.remove(index);
        let id = record.session.id.clone();
        self.record_removed_session(id);
        record
    }

    /// `Vec::retain`-style filter that records every dropped session id
    /// as a tombstone. The predicate is called once per record.
    fn retain_sessions<F>(&mut self, mut keep: F)
    where
        F: FnMut(&SessionRecord) -> bool,
    {
        let mut removed_ids: Vec<String> = Vec::new();
        self.sessions.retain(|record| {
            let retained = keep(record);
            if !retained {
                removed_ids.push(record.session.id.clone());
            }
            retained
        });
        for id in removed_ids {
            self.record_removed_session(id);
        }
    }

    /// Collects the subset of state that advanced past `watermark`.
    ///
    /// Called by the background persist thread while it briefly holds
    /// `AppState::inner`. Clones only:
    ///
    /// - App metadata (non-session fields; shallow clones, no transcripts).
    /// - Sessions whose `mutation_stamp > watermark`, filtered so
    ///   hidden sessions produce `DELETE`s instead of upserts (visible
    ///   sessions that have flipped to hidden since the last persist
    ///   need to disappear from SQLite, and hidden spares are
    ///   regenerated on startup rather than persisted).
    /// - The tombstone list of explicitly removed session ids, drained
    ///   from `removed_session_ids`.
    ///
    /// Returns the new watermark (`last_mutation_stamp` at collection
    /// time) that the caller should install after a successful write.
    ///
    /// The only call site is the background persist thread, which is
    /// `#[cfg(not(test))]`-gated — so under `cargo test` this looks
    /// unused. The `allow(dead_code)` silences that warning without
    /// hiding real dead code in release builds.
    #[cfg_attr(test, allow(dead_code))]
    fn collect_persist_delta(&mut self, watermark: u64) -> PersistDelta {
        let mut changed_sessions: Vec<PersistedSessionRecord> = Vec::new();
        let retry_removed_ids = std::mem::take(&mut self.removed_session_ids);
        let mut removed_ids = retry_removed_ids.clone();
        for record in &self.sessions {
            if record.mutation_stamp <= watermark {
                continue;
            }
            if record.hidden {
                // A session that changed and is now hidden must not
                // stay in SQLite — hidden spares are re-seeded on
                // startup, so ensure the row is removed.
                removed_ids.push(record.session.id.clone());
            } else {
                changed_sessions.push(PersistedSessionRecord::from_record(record));
            }
        }
        PersistDelta {
            metadata: PersistedState::metadata_from_inner(self),
            changed_sessions,
            removed_session_ids: removed_ids,
            changed_delegations: (self.delegation_mutation_stamp > watermark)
                .then(|| self.delegations.clone()),
            drained_explicit_tombstones: retry_removed_ids,
            watermark: self.last_mutation_stamp,
        }
    }

    /// Returns the index of any session (including hidden spares) by
    /// TermAl session id. For user-facing queries prefer
    /// [`StateInner::find_visible_session_index`] so a hidden spare
    /// doesn't accidentally respond to a routed request.
    fn find_session_index(&self, session_id: &str) -> Option<usize> {
        self.sessions
            .iter()
            .position(|record| record.session.id == session_id)
    }

    /// Returns the index of a *visible* (non-hidden) session by id.
    /// Hidden Claude spares are excluded so API routes can't
    /// accidentally operate on a pre-warmed spare.
    fn find_visible_session_index(&self, session_id: &str) -> Option<usize> {
        self.sessions
            .iter()
            .position(|record| !record.hidden && record.session.id == session_id)
    }

    /// Locates a remote-proxy session by its `(remote_id,
    /// remote_session_id)` pair — used by the SSE bridge in
    /// `remote_sync.rs` when replaying events arriving from a
    /// remote.
    fn find_remote_session_index(&self, remote_id: &str, remote_session_id: &str) -> Option<usize> {
        self.sessions.iter().position(|record| {
            record.remote_id.as_deref() == Some(remote_id)
                && record.remote_session_id.as_deref() == Some(remote_session_id)
        })
    }

    /// Locates a remote-proxy orchestrator instance by its
    /// `(remote_id, remote_orchestrator_instance_id)` pair.
    fn find_remote_orchestrator_index(
        &self,
        remote_id: &str,
        remote_orchestrator_id: &str,
    ) -> Option<usize> {
        self.orchestrator_instances.iter().position(|instance| {
            instance.remote_id.as_deref() == Some(remote_id)
                && instance.remote_orchestrator_id.as_deref() == Some(remote_orchestrator_id)
        })
    }

    /// Returns a reference to the [`Project`] with the given id, or
    /// `None` if not present.
    fn find_project(&self, project_id: &str) -> Option<&Project> {
        self.projects
            .iter()
            .find(|project| project.id == project_id)
    }

    /// Returns a reference to the [`RemoteConfig`] with the given
    /// id, or `None` if not present.
    fn find_remote(&self, remote_id: &str) -> Option<&RemoteConfig> {
        self.preferences
            .remotes
            .iter()
            .find(|remote| remote.id == remote_id)
    }

    /// Returns the deepest-matching local [`Project`] whose
    /// `root_path` is an ancestor of `workdir`, or `None` if no
    /// project owns the path. Used by `create_session` to auto-bind
    /// new sessions to an enclosing project.
    fn find_project_for_workdir(&self, workdir: &str) -> Option<&Project> {
        let target = FsPath::new(workdir);
        self.projects
            .iter()
            .filter(|project| {
                project.remote_id == LOCAL_REMOTE_ID
                    && codex_discovery_scope_contains(&project.root_path, target)
            })
            .max_by_key(|project| project.root_path.len())
    }
}
