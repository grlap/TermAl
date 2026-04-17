// Boot-time `StateInner` helpers that run during `AppState::new` or
// `AppState::new_with_paths` to bring persisted state into a healthy,
// internally-consistent shape before the server begins serving
// requests.
//
// These methods are intentionally grouped together because they all
// run *before* the runtime is live — before any runtime is spawned,
// before SSE subscribers connect, before the persist thread starts
// — so they can mutate `StateInner` freely without worrying about
// RuntimeToken guards, mutation stamps propagating to clients, or
// concurrent writers. That's also why they are plain `&mut self`
// methods on `StateInner` rather than `&self` on `AppState` like
// most of the other mutation paths.
//
// The four helpers:
//
// - `import_discovered_codex_threads`: on startup we scan the
//   Codex state dir for pre-existing `threadId`s and merge them into
//   `self.sessions` as resumeable ghost-sessions so users don't lose
//   history from Codex runs that happened outside TermAl. Threads
//   that the user has explicitly ignored (via
//   `ignore_discovered_codex_thread`) are skipped.
// - `validate_projects_consistent`: runs a defensive check that
//   every `session.project_id` still points at a project in
//   `self.projects`. Inconsistencies from stale-state files surface
//   as errors rather than silent orphaned references.
// - `normalize_local_paths`: canonicalizes on-disk paths so
//   `workdir` comparisons across sessions / projects / file watchers
//   don't mismatch due to casing or trailing slashes.
// - `recover_interrupted_sessions`: any session that was `Active` or
//   `Approval` at shutdown (process crash, OS reboot, etc.) is
//   transitioned back to `Idle` + flagged `runtime_reset_required`
//   so the next turn starts a fresh runtime instead of inheriting a
//   half-dead state machine.

impl StateInner {
    /// Merges Codex threads discovered on disk into `self.sessions` as
    /// ghost-sessions that resume the existing `threadId` rather than
    /// opening a fresh Codex conversation. Threads the user has
    /// previously ignored (via `ignore_discovered_codex_thread`) are
    /// skipped. `default_workdir` is the fallback cwd for imported
    /// threads that have no recorded workdir in Codex's state file.
    fn import_discovered_codex_threads(
        &mut self,
        default_workdir: &str,
        threads: Vec<DiscoveredCodexThread>,
    ) {
        let discovered_thread_ids = threads
            .iter()
            .filter_map(|thread| normalize_optional_identifier(Some(thread.id.as_str())))
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        self.ignored_discovered_codex_thread_ids
            .retain(|thread_id| discovered_thread_ids.contains(thread_id));

        for thread in threads {
            let thread = DiscoveredCodexThread {
                cwd: normalize_local_user_facing_path(&thread.cwd),
                ..thread
            };
            let target_path = FsPath::new(&thread.cwd);
            let within_scope = codex_discovery_scope_contains(default_workdir, target_path)
                || self.projects.iter().any(|project| {
                    project.remote_id == LOCAL_REMOTE_ID
                        && codex_discovery_scope_contains(&project.root_path, target_path)
                });
            if !within_scope {
                continue;
            }

            let project_id = self
                .find_project_for_workdir(&thread.cwd)
                .map(|project| project.id.clone())
                .unwrap_or_else(|| {
                    self.create_project(None, thread.cwd.clone(), default_local_remote_id())
                        .id
                });

            let existing_index = self.sessions.iter().position(|record| {
                !record.is_remote_proxy()
                    && record.session.agent == Agent::Codex
                    && record.external_session_id.as_deref() == Some(thread.id.as_str())
            });

            if let Some(index) = existing_index {
                self.allow_discovered_codex_thread(Some(thread.id.as_str()));
                let record = self
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                if record.session.workdir != thread.cwd {
                    record.session.workdir = thread.cwd.clone();
                }
                if record.session.project_id.as_deref() != Some(project_id.as_str()) {
                    record.session.project_id = Some(project_id);
                }
                apply_discovered_codex_thread(record, &thread, false);
                continue;
            }

            if self
                .ignored_discovered_codex_thread_ids
                .contains(thread.id.as_str())
            {
                continue;
            }

            let mut record = self.create_session(
                Agent::Codex,
                Some(thread.title.clone()),
                thread.cwd.clone(),
                Some(project_id),
                thread.model.clone(),
            );
            apply_discovered_codex_thread(&mut record, &thread, true);
            // Whole-struct slot replace then re-stamp. `create_session`
            // stamps the record when `push_session` inserts it, but
            // returns an owned `SessionRecord` whose `mutation_stamp`
            // is still the construction-time default — replacing the
            // stamped slot with that owned value would erase the
            // stamp and make the import invisible to the SQLite delta
            // persist (the row's stamp would sit at or below the
            // watermark, so `collect_persist_delta` would skip it).
            // `session_mut_by_index` restamps the slot in place after
            // the replace, mirroring the pattern used by
            // `create_session` + `create_session_from_fork` in
            // `src/session_crud.rs`.
            if let Some(index) = self.find_session_index(&record.session.id) {
                if let Some(slot) = self.sessions.get_mut(index) {
                    *slot = record;
                }
                let _ = self.session_mut_by_index(index);
            }
        }
    }

    /// Asserts that every `session.project_id` references a project
    /// that actually exists in `self.projects`. Returns an error when
    /// a persisted state file is internally inconsistent — boot aborts
    /// rather than loading garbage into memory.
    fn validate_projects_consistent(&self) -> Result<()> {
        for project in &self.projects {
            let remote_id = project.remote_id.trim();
            if remote_id.is_empty() {
                return Err(anyhow!(
                    "persisted project `{}` is missing remoteId",
                    project.id
                ));
            }
            if self.find_remote(remote_id).is_none() {
                return Err(anyhow!(
                    "persisted project `{}` references unknown remote `{remote_id}`",
                    project.id
                ));
            }
        }

        if self.next_project_number < 1 {
            return Err(anyhow!("persisted nextProjectNumber must be at least 1"));
        }

        let highest_project_number = self
            .projects
            .iter()
            .filter_map(|project| {
                project
                    .id
                    .strip_prefix("project-")
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .max()
            .unwrap_or(0);
        if self.next_project_number <= highest_project_number {
            return Err(anyhow!(
                "persisted nextProjectNumber `{}` must be greater than existing project ids",
                self.next_project_number
            ));
        }

        for record in &self.sessions {
            let Some(project_id) = record.session.project_id.as_deref() else {
                continue;
            };
            if self.find_project(project_id).is_none() {
                return Err(anyhow!(
                    "persisted session `{}` references unknown project `{project_id}`",
                    record.session.id
                ));
            }
        }

        Ok(())
    }

    /// Canonicalizes on-disk paths (workdir, project root, etc.) so
    /// equality comparisons across sessions / projects / file watchers
    /// don't mismatch due to casing, trailing separators, or
    /// Windows/Unix path flavour differences.
    fn normalize_local_paths(&mut self) {
        let local_project_ids = self
            .projects
            .iter_mut()
            .filter_map(|project| {
                if project.remote_id == LOCAL_REMOTE_ID {
                    project.root_path = normalize_local_user_facing_path(&project.root_path);
                    Some(project.id.clone())
                } else {
                    None
                }
            })
            .collect::<HashSet<_>>();

        for record in &mut self.sessions {
            let should_normalize = record.remote_id.is_none()
                && record
                    .session
                    .project_id
                    .as_deref()
                    .map(|project_id| local_project_ids.contains(project_id))
                    .unwrap_or(true);
            if should_normalize {
                record.session.workdir = normalize_local_user_facing_path(&record.session.workdir);
            }
        }
        for layout in self.workspace_layouts.values_mut() {
            normalize_workspace_layout_paths(layout);
        }
    }

    /// Transitions any session that was `Active` or `Approval` at
    /// shutdown back to `Idle` and flags `runtime_reset_required` so
    /// the next turn starts a fresh runtime. Also drains pending
    /// approvals, pending user inputs, and queued prompts that are no
    /// longer reachable. Called during boot; runtime crashes mid-
    /// turn during normal operation are handled by
    /// `handle_runtime_exit_if_matches` in `turn_lifecycle.rs` instead.
    fn recover_interrupted_sessions(&mut self) {
        for index in 0..self.sessions.len() {
            if self.sessions[index].is_remote_proxy() {
                continue;
            }
            let recovery = {
                let record = self
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                recover_interrupted_session_record(record)
            };

            let Some(recovery) = recovery else {
                continue;
            };

            let message_id = self.next_message_id();
            let record = self
                .session_mut_by_index(index)
                .expect("session index should be valid");
            push_message_on_record(
                record,
                Message::Text {
                    attachments: Vec::new(),
                    id: message_id,
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: recovery,
                    expanded_text: None,
                },
            );
            record.session.status = SessionStatus::Error;
            if let Some(message) = record.session.messages.last() {
                if let Some(preview) = message.preview_text() {
                    record.session.preview = preview;
                }
            }
        }
    }
}
