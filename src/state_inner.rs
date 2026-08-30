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
// - **Codex thread discovery**:
//   `ignore_discovered_codex_thread` / `allow_discovered_codex_thread`
//   (user explicitly hides/unhides imported threads).
//
// - **Session-array primitives**: `next_message_id` + `next_mutation_stamp`
//   counters, the `session_mut*` accessors that bump `mutation_stamp`
//   on every hand-out (load-bearing for persist-delta correctness),
//   `push_session` / `remove_session_at` / `retain_sessions`, and
//   the persist-delta planner which the persist thread drains each tick
//   to write only sessions that have changed since its last watermark.
//
// - **Finders**: `find_session_index` (all internal sessions),
//   `find_visible_session_index` (user-facing only), `find_remote_session_index`,
//   `find_remote_orchestrator_index`, `find_project`, `find_remote`,
//   `find_project_for_workdir`. The "visible" filter is a recurring
//   gotcha — most API routes should not operate on internal records.
//
// The `#[warn(dead_code)]` exemption on `session_mut` and
// `stamp_session_at_index` is intentional: both are test-only
// helpers retained for mutation-stamp regression coverage. Keeping
// them callable from `#[cfg(test)]` code without a `#[cfg(test)]`
// gate here lets the shared helper stay one definition.

/// Returns the privacy-safe default label for a session id. Keeping this
/// contract next to `create_session` lets startup import/migration paths reuse
/// the exact same label instead of reconstructing `{agent} {number}`.
fn generated_session_name(agent: Agent, session_id: &str) -> String {
    session_id
        .strip_prefix("session-")
        .filter(|suffix| {
            !suffix.is_empty() && suffix.chars().all(|candidate| candidate.is_ascii_digit())
        })
        .map(|suffix| format!("{} {suffix}", agent.name()))
        .unwrap_or_else(|| format!("{} session", agent.name()))
}

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

        // Project ids cross the termal.sqlite / coordination.sqlite boundary:
        // the latter can outlive a restored or freshly recreated primary
        // database. A rewindable `project-{number}` id can therefore inherit an
        // unrelated live board scope or permanent deletion fence. Keep the
        // legacy counter for persisted-state compatibility and validation,
        // but give every newly created project a collision-resistant identity.
        let project_id = loop {
            let candidate = format!("project-{}", Uuid::new_v4());
            if self.projects.iter().all(|project| project.id != candidate) {
                break candidate;
            }
        };
        // Persist the legacy allocation watermark even though UUIDs now own
        // identity; load-time validation still checks this counter.
        self.next_project_number += 1;
        let base_name = name.unwrap_or_else(|| default_project_name(&root_path));
        let project = Project {
            id: project_id,
            name: dedupe_project_name(&self.projects, &base_name),
            root_path,
            remote_id,
            remote_project_id: None,
            engram: None,
            engram_cleanup_warning: None,
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
        let session_id = format!("session-{number}");
        let session_name =
            name.unwrap_or_else(|| generated_session_name(agent, session_id.as_str()));
        let session_model = model.unwrap_or_else(|| self.preferences.default_model_for_agent(agent));
        let opencode_model = agent
            .supports_opencode_settings()
            .then(|| session_model.clone());

        let record = SessionRecord {
            active_codex_approval_policy: None,
            active_codex_reasoning_effort: None,
            active_codex_sandbox_mode: None,
            active_turn_generation: 0,
            active_turn_mailbox_notification: None,
            active_turn_start_message_count: None,
            active_turn_file_changes: BTreeMap::new(),
            active_turn_file_change_grace_deadline: None,
            agent_commands: Vec::new(),
            codex_approval_policy: self.preferences.default_codex_approval_policy,
            codex_reasoning_effort: self.preferences.default_codex_reasoning_effort,
            codex_sandbox_mode: self.preferences.default_codex_sandbox_mode,
            external_session_id: None,
            pending_claude_approvals: HashMap::new(),
            pending_claude_user_inputs: HashMap::new(),
            pending_codex_approvals: HashMap::new(),
            pending_codex_user_inputs: HashMap::new(),
            pending_codex_mcp_elicitations: HashMap::new(),
            pending_codex_app_requests: HashMap::new(),
            pending_acp_approvals: HashMap::new(),
            pending_acp_approval_order: VecDeque::new(),
            queued_prompts: VecDeque::new(),
            queued_peer_messages: HashMap::new(),
            message_start_index: 0,
            message_positions: HashMap::new(),
            remote_id: None,
            remote_session_id: None,
            runtime: SessionRuntime::None,
            engram_mcp_installed: None,
            runtime_reset_required: false,
            engram_mcp_runtime_quarantined: false,
            orchestrator_auto_dispatch_blocked: false,
            runtime_stop_in_progress: false,
            runtime_stop_owner: None,
            runtime_stop_generation: 0,
            engram_mcp_revocation_pending: false,
            deferred_stop_callbacks: Vec::new(),
            engram: EngramSessionState::default(),
            hidden: false,
            // Freshly created records start unstamped; the call path
            // immediately inserts this record and then the caller routes
            // subsequent edits through `session_mut*`, which bumps the
            // stamp as soon as a mutation happens.
            mutation_stamp: 0,
            prompt_history_mutation_stamp: 0,
            session: Session {
                id: session_id,
                name: session_name,
                emoji: agent.avatar().to_owned(),
                agent,
                workdir,
                project_id,
                remote_id: None,
                model: session_model,
                model_options: Vec::new(),
                approval_policy: None,
                reasoning_effort: None,
                codex_fast_mode: false,
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
                opencode_model,
                opencode_effort: agent
                    .supports_opencode_settings()
                    .then(|| OPENCODE_CONFIG_AUTO.to_owned()),
                opencode_current_effort: None,
                opencode_effort_options: Vec::new(),
                opencode_mode: agent
                    .supports_opencode_settings()
                    .then(|| OPENCODE_CONFIG_AUTO.to_owned()),
                opencode_current_mode: None,
                opencode_mode_options: Vec::new(),
                external_session_id: None,
                agent_commands_revision: 0,
                codex_thread_state: None,
                live_activity: None,
                status: SessionStatus::Idle,
                preview: "Ready for a prompt.".to_owned(),
                messages: Vec::new(),
                prompt_history: Vec::new(),
                prompt_history_redacted: false,
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

    /// Backfills the child-session parent pointer from persisted delegation
    /// records. This keeps older SQLite rows hidden in session lists even if
    /// the `Session` row predates `parent_delegation_id`. As a fallback, also
    /// recover the link from the delegated-child marker persisted in the child
    /// prompt; this handles historical rows where the child session survived
    /// but the dedicated delegation table row did not.
    ///
    /// `session_mut_by_index` eagerly bumps the mutation stamp, so this repair
    /// only calls it after deciding the row must change.
    fn repair_delegation_child_session_links(&mut self) {
        let mut links = self
            .delegations
            .iter()
            .map(|delegation| {
                (
                    delegation.child_session_id.clone(),
                    delegation.id.clone(),
                )
            })
            .collect::<BTreeMap<_, _>>();

        for record in &mut self.sessions {
            if links.contains_key(&record.session.id) {
                continue;
            }
            if let Some(delegation_id) = delegation_id_from_child_session_marker(record) {
                links.insert(record.session.id.clone(), delegation_id);
            }
        }

        for child_index in 0..self.sessions.len() {
            let child_session_id = self.sessions[child_index].session.id.clone();
            let expected_delegation_id = links.get(&child_session_id);
            if let Some(delegation_id) = expected_delegation_id {
                if self.sessions[child_index]
                    .session
                    .parent_delegation_id
                    .as_deref()
                    == Some(delegation_id.as_str())
                {
                    continue;
                }
                if let Some(record) = self.session_mut_by_index(child_index) {
                    record.session.parent_delegation_id = Some(delegation_id.clone());
                }
                continue;
            }

            let has_invalid_parent_id = self.sessions[child_index]
                .session
                .parent_delegation_id
                .as_deref()
                .is_some_and(|delegation_id| !is_valid_delegation_marker_id(delegation_id));
            if !has_invalid_parent_id {
                continue;
            }
            // Marker-derived links use the same validator. Clearing only invalid
            // ids preserves legitimate-looking parent ids that may be repaired
            // once their delegation row returns.
            if let Some(record) = self.session_mut_by_index(child_index) {
                record.session.parent_delegation_id = None;
            }
        }
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

    fn mark_delegation_mutated(&mut self, delegation_index: usize) -> Option<u64> {
        let delegation_id = self.delegations.get(delegation_index)?.id.clone();
        Some(self.mark_delegation_id_mutated(delegation_id))
    }

    fn mark_delegation_id_mutated(&mut self, delegation_id: String) -> u64 {
        let stamp = self.next_mutation_stamp();
        self.delegation_mutation_stamps.insert(delegation_id, stamp);
        stamp
    }

    fn record_removed_delegation(&mut self, delegation_id: String) {
        let stamp = self.next_mutation_stamp();
        self.delegation_mutation_stamps.remove(&delegation_id);
        self.removed_delegation_ids.insert(delegation_id, stamp);
    }

    fn restore_drained_delegation_tombstones(&mut self, tombstones: &BTreeMap<String, u64>) {
        for (delegation_id, stamp) in tombstones {
            self.removed_delegation_ids
                .entry(delegation_id.clone())
                .and_modify(|existing| *existing = (*existing).max(*stamp))
                .or_insert(*stamp);
        }
    }

    fn mark_loaded_delegations_for_sqlite_migration(&mut self) {
        let delegation_ids = self
            .delegations
            .iter()
            .map(|delegation| delegation.id.clone())
            .collect::<Vec<_>>();
        for delegation_id in delegation_ids {
            self.mark_delegation_id_mutated(delegation_id);
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn remove_delegation_at(&mut self, index: usize) -> DelegationRecord {
        let record = self.delegations.remove(index);
        self.record_removed_delegation(record.id.clone());
        self.rebuild_running_read_only_delegations();
        record
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
    /// persist-delta planner to re-serialize its row on the next
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
        if record.prompt_history_mutation_stamp == 0
            && !record.session.prompt_history.is_empty()
        {
            record.prompt_history_mutation_stamp = stamp;
        }
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

    /// Selects the subset of state that advanced past `watermark`.
    ///
    /// Called by the background persist thread while it briefly holds
    /// `AppState::inner`. Clones only:
    ///
    /// - App metadata (non-session fields; shallow clones, no transcripts).
    /// - Ids and prompt-history flags for sessions whose
    ///   `mutation_stamp > watermark`. Hidden internal records produce
    ///   `DELETE`s instead of candidates. A visible session that becomes hidden
    ///   must disappear from SQLite rather than leave a stale user-facing row.
    /// - The tombstone list of explicitly removed session ids, drained
    ///   from `removed_session_ids`.
    /// - Delegation ids and delegation tombstones whose runtime-only mutation
    ///   stamps advanced past the watermark.
    ///
    /// Returns a plan carrying the new watermark (`last_mutation_stamp` at
    /// selection time). Large session and delegation values are materialized
    /// separately so a dirty batch cannot monopolize the global mutex.
    ///
    /// Tests also use this selection boundary directly to verify that
    /// concurrent mutations are deferred rather than mixed across revisions.
    #[cfg_attr(test, allow(dead_code))]
    fn collect_persist_delta_plan(&mut self, watermark: u64) -> PersistDeltaPlan {
        let mut changed_sessions = Vec::new();
        let retry_removed_ids = std::mem::take(&mut self.removed_session_ids);
        let retry_removed_delegation_ids = std::mem::take(&mut self.removed_delegation_ids);
        let mut removed_ids = retry_removed_ids.clone();
        for record in &self.sessions {
            if record.mutation_stamp <= watermark {
                continue;
            }
            if record.hidden {
                // A session that changed and is now hidden must not
                // stay in SQLite, so ensure any prior visible row is
                // removed.
                removed_ids.push(record.session.id.clone());
            } else {
                changed_sessions.push(PersistSessionCandidate {
                    session_id: record.session.id.clone(),
                    mutation_stamp: record.mutation_stamp,
                    persist_prompt_history: record.prompt_history_mutation_stamp > watermark,
                });
            }
        }
        let changed_delegations = self
            .delegations
            .iter()
            .filter_map(|delegation| {
                let mutation_stamp = self
                    .delegation_mutation_stamps
                    .get(&delegation.id)
                    .copied()?;
                (mutation_stamp > watermark).then(|| PersistDelegationCandidate {
                    delegation_id: delegation.id.clone(),
                    mutation_stamp,
                })
            })
            .collect::<Vec<_>>();
        let removed_delegation_ids = retry_removed_delegation_ids
            .iter()
            .filter_map(|(delegation_id, stamp)| {
                (*stamp > watermark).then(|| delegation_id.clone())
            })
            .collect::<Vec<_>>();

        PersistDeltaPlan {
            metadata: PersistedState::metadata_from_inner(self),
            changed_sessions,
            removed_session_ids: removed_ids,
            changed_delegations,
            removed_delegation_ids,
            drained_delegation_tombstones: retry_removed_delegation_ids,
            drained_explicit_tombstones: retry_removed_ids,
            watermark: self.last_mutation_stamp,
        }
    }

    /// Materializes a persist plan against this state value.
    ///
    /// Tests and synchronous helpers use this form directly. The production
    /// worker uses [`collect_persist_delta_from_shared_state`] so it can release
    /// the global mutex between individual record snapshots.
    #[cfg(test)]
    fn materialize_persist_delta(&self, plan: PersistDeltaPlan) -> PersistDelta {
        materialize_persist_delta_plan(
            plan,
            |candidate| {
                let Some(record) = self
                    .sessions
                    .iter()
                    .find(|record| record.session.id == candidate.session_id)
                else {
                    return PersistCandidateMaterialization::Changed;
                };
                if record.hidden || record.mutation_stamp != candidate.mutation_stamp {
                    return PersistCandidateMaterialization::Changed;
                }
                let mut persisted = PersistedSessionRecord::from_record(record);
                persisted.persist_prompt_history = candidate.persist_prompt_history;
                PersistCandidateMaterialization::Snapshot(persisted)
            },
            |candidate| {
                let Some(delegation) = self
                    .delegations
                    .iter()
                    .find(|delegation| delegation.id == candidate.delegation_id)
                else {
                    return PersistCandidateMaterialization::Changed;
                };
                if self
                    .delegation_mutation_stamps
                    .get(&candidate.delegation_id)
                    .copied()
                    != Some(candidate.mutation_stamp)
                {
                    return PersistCandidateMaterialization::Changed;
                }
                PersistCandidateMaterialization::Snapshot(delegation.clone())
            },
        )
    }

    /// Collects and materializes one delta without an external state mutex.
    ///
    /// This remains the convenient test/synchronous surface. Production must
    /// call [`collect_persist_delta_from_shared_state`] to avoid one cumulative
    /// lock hold across all large records.
    #[cfg(test)]
    fn collect_persist_delta(&mut self, watermark: u64) -> PersistDelta {
        let plan = self.collect_persist_delta_plan(watermark);
        self.materialize_persist_delta(plan)
    }

    fn trim_persisted_session_tails(
        &mut self,
        watermark: u64,
        persisted_session_ids: &[String],
    ) {
        for session_id in persisted_session_ids {
            let Some(index) = self.find_session_index(session_id) else {
                continue;
            };
            let record = &mut self.sessions[index];
            if record.mutation_stamp > watermark
                || matches!(
                    record.session.status,
                    SessionStatus::Active | SessionStatus::Approval
                )
            {
                continue;
            }
            trim_retained_session_messages(record, SESSION_IN_MEMORY_MESSAGE_LIMIT);
        }
    }

    /// Returns the index of any session, including internal records, by
    /// TermAl session id. User-facing queries should use
    /// [`StateInner::find_visible_session_index`].
    fn find_session_index(&self, session_id: &str) -> Option<usize> {
        self.sessions
            .iter()
            .position(|record| record.session.id == session_id)
    }

    /// Returns the index of a visible (non-hidden) session by id.
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

/// Converts a lightweight persist plan into owned rows.
///
/// A candidate that changes, disappears, or becomes hidden after selection is
/// omitted instead of mixing state from two revisions. A later mutation stamp
/// or an explicit removal tombstone selects that record again without
/// rewriting stable snapshots from this batch.
fn materialize_persist_delta_plan(
    plan: PersistDeltaPlan,
    mut snapshot_session: impl FnMut(
        &PersistSessionCandidate,
    ) -> PersistCandidateMaterialization<PersistedSessionRecord>,
    mut snapshot_delegation: impl FnMut(
        &PersistDelegationCandidate,
    ) -> PersistCandidateMaterialization<DelegationRecord>,
) -> PersistDelta {
    let PersistDeltaPlan {
        metadata,
        changed_sessions: session_candidates,
        removed_session_ids,
        changed_delegations: delegation_candidates,
        removed_delegation_ids,
        drained_delegation_tombstones,
        drained_explicit_tombstones,
        watermark,
    } = plan;

    let mut changed_sessions = Vec::with_capacity(session_candidates.len());
    let mut deferred_session_ids = Vec::new();
    let mut deferred_prompt_history_session_ids = Vec::new();
    for candidate in session_candidates {
        match snapshot_session(&candidate) {
            PersistCandidateMaterialization::Snapshot(record) => changed_sessions.push(record),
            PersistCandidateMaterialization::Changed => {
                if candidate.persist_prompt_history {
                    deferred_prompt_history_session_ids.push(candidate.session_id.clone());
                }
                deferred_session_ids.push(candidate.session_id);
            }
        }
        std::thread::yield_now();
    }

    let mut changed_delegations = Vec::with_capacity(delegation_candidates.len());
    let mut deferred_delegation_ids = Vec::new();
    for candidate in delegation_candidates {
        match snapshot_delegation(&candidate) {
            PersistCandidateMaterialization::Snapshot(record) => changed_delegations.push(record),
            PersistCandidateMaterialization::Changed => {
                deferred_delegation_ids.push(candidate.delegation_id)
            }
        }
        std::thread::yield_now();
    }

    PersistDelta {
        metadata,
        changed_sessions,
        removed_session_ids,
        changed_delegations: (!changed_delegations.is_empty()).then_some(changed_delegations),
        removed_delegation_ids,
        deferred_session_ids,
        deferred_prompt_history_session_ids,
        deferred_delegation_ids,
        drained_delegation_tombstones,
        drained_explicit_tombstones,
        watermark,
    }
}

/// Applies a later materialization pass over an earlier one.
///
/// The later pass is authoritative for any repeated id. A newer snapshot
/// cancels an earlier delete, while a newer delete removes an earlier
/// snapshot. Session prompt-history persistence is cumulative across the two
/// passes so replacing an older row snapshot cannot lose a history write that
/// was selected by the first watermark.
fn merge_persist_delta_passes(mut earlier: PersistDelta, later: PersistDelta) -> PersistDelta {
    let PersistDelta {
        metadata,
        changed_sessions: later_changed_sessions,
        removed_session_ids: later_removed_session_ids,
        changed_delegations: later_changed_delegations,
        removed_delegation_ids: later_removed_delegation_ids,
        deferred_session_ids,
        deferred_prompt_history_session_ids,
        deferred_delegation_ids,
        drained_delegation_tombstones,
        drained_explicit_tombstones,
        watermark,
    } = later;

    for session_id in later_removed_session_ids {
        earlier
            .changed_sessions
            .retain(|record| record.session.id != session_id);
        if !earlier.removed_session_ids.contains(&session_id) {
            earlier.removed_session_ids.push(session_id);
        }
    }
    for mut record in later_changed_sessions {
        let session_id = record.session.id.clone();
        earlier
            .removed_session_ids
            .retain(|removed_id| removed_id != &session_id);
        if let Some(existing) = earlier
            .changed_sessions
            .iter_mut()
            .find(|existing| existing.session.id == session_id)
        {
            record.persist_prompt_history |= existing.persist_prompt_history;
            *existing = record;
        } else {
            earlier.changed_sessions.push(record);
        }
    }

    let mut changed_delegations = earlier.changed_delegations.take().unwrap_or_default();
    for delegation_id in later_removed_delegation_ids {
        changed_delegations.retain(|delegation| delegation.id != delegation_id);
        if !earlier.removed_delegation_ids.contains(&delegation_id) {
            earlier.removed_delegation_ids.push(delegation_id);
        }
    }
    for delegation in later_changed_delegations.unwrap_or_default() {
        earlier
            .removed_delegation_ids
            .retain(|removed_id| removed_id != &delegation.id);
        if let Some(existing) = changed_delegations
            .iter_mut()
            .find(|existing| existing.id == delegation.id)
        {
            *existing = delegation;
        } else {
            changed_delegations.push(delegation);
        }
    }
    earlier.changed_delegations = (!changed_delegations.is_empty()).then_some(changed_delegations);

    for session_id in drained_explicit_tombstones {
        if !earlier.drained_explicit_tombstones.contains(&session_id) {
            earlier.drained_explicit_tombstones.push(session_id);
        }
    }
    for (delegation_id, stamp) in drained_delegation_tombstones {
        earlier
            .drained_delegation_tombstones
            .entry(delegation_id)
            .and_modify(|existing| *existing = (*existing).max(stamp))
            .or_insert(stamp);
    }

    earlier.metadata = metadata;
    earlier.deferred_session_ids = deferred_session_ids;
    earlier.deferred_prompt_history_session_ids = deferred_prompt_history_session_ids;
    earlier.deferred_delegation_ids = deferred_delegation_ids;
    earlier.watermark = watermark;
    earlier
}

/// Builds a persistence delta without holding the global mutex across the
/// whole dirty batch.
///
/// The first acquisition captures only metadata, ids, flags, tombstones, and
/// the target watermark. Each large session/delegation snapshot gets its own
/// acquisition, allowing unrelated state work to run between records.
#[cfg(test)]
fn collect_persist_delta_from_shared_state(
    inner: &StateMutex<StateInner>,
    watermark: u64,
) -> PersistDelta {
    collect_persist_delta_from_shared_state_with_prompt_history_carry(
        inner,
        watermark,
        &BTreeSet::new(),
    )
}

fn collect_persist_delta_from_shared_state_with_prompt_history_carry(
    inner: &StateMutex<StateInner>,
    watermark: u64,
    prompt_history_carry: &BTreeSet<String>,
) -> PersistDelta {
    collect_persist_delta_from_shared_state_with_carry_and_first_plan_hook(
        inner,
        watermark,
        prompt_history_carry,
        || {},
    )
}

#[cfg(test)]
fn collect_persist_delta_from_shared_state_with_first_plan_hook(
    inner: &StateMutex<StateInner>,
    watermark: u64,
    after_first_plan: impl FnOnce(),
) -> PersistDelta {
    collect_persist_delta_from_shared_state_with_carry_and_first_plan_hook(
        inner,
        watermark,
        &BTreeSet::new(),
        after_first_plan,
    )
}

fn collect_persist_delta_from_shared_state_with_carry_and_first_plan_hook(
    inner: &StateMutex<StateInner>,
    watermark: u64,
    prompt_history_carry: &BTreeSet<String>,
    after_first_plan: impl FnOnce(),
) -> PersistDelta {
    let first = collect_persist_delta_pass_from_shared_state(
        inner,
        watermark,
        prompt_history_carry,
        after_first_plan,
    );
    if first.deferred_session_ids.is_empty() && first.deferred_delegation_ids.is_empty() {
        return first;
    }

    // A single bounded re-plan closes the common race where one mutation lands
    // between selection and that record's snapshot lock, without turning a hot
    // session into an unbounded in-tick loop. The second pass starts at the
    // first plan's watermark, so already-stable rows are not selected again.
    let mut retry_prompt_history_carry = prompt_history_carry.clone();
    retry_prompt_history_carry.extend(first.deferred_prompt_history_session_ids.iter().cloned());
    let retry = collect_persist_delta_pass_from_shared_state(
        inner,
        first.watermark,
        &retry_prompt_history_carry,
        || {},
    );
    let merged = merge_persist_delta_passes(first, retry);
    if !merged.deferred_session_ids.is_empty() || !merged.deferred_delegation_ids.is_empty() {
        eprintln!(
            "[termal] persist snapshot still deferred after bounded retry: {} session(s), {} delegation(s)",
            merged.deferred_session_ids.len(),
            merged.deferred_delegation_ids.len(),
        );
    }
    merged
}

fn collect_persist_delta_pass_from_shared_state(
    inner: &StateMutex<StateInner>,
    watermark: u64,
    prompt_history_carry: &BTreeSet<String>,
    after_plan: impl FnOnce(),
) -> PersistDelta {
    let mut plan = {
        let mut state = inner.lock().expect("state mutex poisoned");
        state.collect_persist_delta_plan(watermark)
    };
    for candidate in &mut plan.changed_sessions {
        candidate.persist_prompt_history |=
            prompt_history_carry.contains(&candidate.session_id);
    }
    after_plan();

    materialize_persist_delta_plan(
        plan,
        |candidate| {
            let state = inner.lock().expect("state mutex poisoned");
            let Some(record) = state
                .sessions
                .iter()
                .find(|record| record.session.id == candidate.session_id)
            else {
                return PersistCandidateMaterialization::Changed;
            };
            if record.hidden || record.mutation_stamp != candidate.mutation_stamp {
                return PersistCandidateMaterialization::Changed;
            }
            let mut persisted = PersistedSessionRecord::from_record(record);
            persisted.persist_prompt_history = candidate.persist_prompt_history;
            PersistCandidateMaterialization::Snapshot(persisted)
        },
        |candidate| {
            let state = inner.lock().expect("state mutex poisoned");
            let Some(delegation) = state
                .delegations
                .iter()
                .find(|delegation| delegation.id == candidate.delegation_id)
            else {
                return PersistCandidateMaterialization::Changed;
            };
            if state
                .delegation_mutation_stamps
                .get(&candidate.delegation_id)
                .copied()
                != Some(candidate.mutation_stamp)
            {
                return PersistCandidateMaterialization::Changed;
            }
            PersistCandidateMaterialization::Snapshot(delegation.clone())
        },
    )
}

fn delegation_id_from_child_session_marker(record: &SessionRecord) -> Option<String> {
    record
        .session
        .messages
        .first()
        .and_then(|message| delegation_id_from_message_marker(message, &record.session.id))
}

fn delegation_id_from_message_marker(message: &Message, expected_child_session_id: &str) -> Option<String> {
    match message {
        Message::Text {
            author: Author::You,
            text,
            ..
        } => delegation_id_from_delegated_child_marker(text, expected_child_session_id),
        _ => None,
    }
}

fn delegation_id_from_delegated_child_marker(
    text: &str,
    expected_child_session_id: &str,
) -> Option<String> {
    let (delegation_id, child_session_id) = delegated_child_marker_parts(text)?;
    (child_session_id == expected_child_session_id).then(|| delegation_id.to_owned())
}

/// Parses the identity-bearing fields from the reserved delegation bootstrap
/// prefix once. Callers that only need classification can test this helper,
/// while durable parent-link repair additionally compares the child id.
fn delegated_child_marker_parts(text: &str) -> Option<(&str, &str)> {
    // This mirrors `build_delegation_prompt`: the marker must be the leading
    // prompt text and both identity fields must use the reserved shape. The
    // caller that repairs a durable parent link additionally checks that the
    // parsed child id matches the session being repaired.
    let after_marker = text.trim_start().strip_prefix(DELEGATED_CHILD_SESSION_MARKER)?;
    let after_opening_tick = after_marker.strip_prefix(" `")?;
    let closing_tick = after_opening_tick.find('`')?;
    let after_closing_tick = &after_opening_tick[closing_tick + 1..];
    if !after_closing_tick.starts_with(".\n") && !after_closing_tick.starts_with(".\r\n") {
        return None;
    }
    let delegation_id = normalize_optional_identifier(Some(&after_opening_tick[..closing_tick]))?;
    if !is_valid_delegation_marker_id(delegation_id) {
        return None;
    }
    let child_session_id = child_session_id_from_delegated_child_marker(text)?;
    Some((delegation_id, child_session_id))
}

fn is_delegated_child_bootstrap_title(text: &str) -> bool {
    delegated_child_marker_parts(text).is_some()
}

fn child_session_id_from_delegated_child_marker(text: &str) -> Option<&str> {
    text.lines().find_map(|line| {
        let after_label = line.strip_prefix("Child session:")?;
        let after_opening_tick = after_label.strip_prefix(" `")?;
        let closing_tick = after_opening_tick.find('`')?;
        let after_closing_tick = &after_opening_tick[closing_tick + 1..];
        after_closing_tick
            .is_empty()
            .then_some(&after_opening_tick[..closing_tick])
    })
}

fn is_valid_delegation_marker_id(delegation_id: &str) -> bool {
    let Some(suffix) = delegation_id.strip_prefix("delegation-") else {
        return false;
    };
    !suffix.is_empty()
        && suffix
            .chars()
            .all(|candidate| candidate.is_ascii_alphanumeric() || candidate == '-')
}
