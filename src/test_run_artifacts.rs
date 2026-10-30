// Provider-to-launcher artifact association beside test_run_observations.rs.
// Owns bounded validation of the original tool's explicit locator, retention
// of exactly associated terminal facts, and launch-only uncertainty projection.
// Provider lifecycle monitoring stays in test_run_observations.rs. Does not
// guess by PID/time/command similarity, scan processes, mint verification or
// final host proof, or manufacture a launcher registration ACK.

#[cfg(test)]
thread_local! {
    static TEST_TEST_RUN_TERMINAL_RETENTION_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static TEST_TEST_RUN_TERMINAL_RETENTION_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn test_run_observation_launch_unconfirmed(row: &mut TestRunCommandObservation) {
    row.summary.state = TestRunState::Unknown;
    row.summary.unknown_reason = Some(TestRunUnknownReason::ResultsUnreadable);
    row.summary.error = Some("Launcher result describes only the launch; the test run's terminal outcome is not confirmed.".to_owned());
    row.summary.exit_code = None;
}

impl AppState {
    // Retain factual terminal results before artifacts can disappear. Use the
    // same positive association as API/SSE alias suppression, with no I/O or
    // evidence settlement here. A native terminal proof is never rewritten.
    fn retain_test_command_artifact_terminals_locked(&self, inner: &mut StateInner) {
        // Reuse the fresh per-cut projection's candidate index and BTreeMap
        // row order. Its linear preparation still sees completed history,
        // but only unfinished same-run candidates reach exact matching.
        let projection = TestRunProjection::new(inner);
        let mut terminal = Vec::new();
        for entry in &inner.test_runs.entries {
            if !entry.disk.is_terminal()
                || !matches!(
                    entry.summary.state,
                    TestRunState::Passed | TestRunState::Failed
                )
            {
                continue;
            }
            let Some(digest) = entry
                .summary
                .detail_version
                .as_deref()
                .and_then(|version| version.strip_prefix("sha256:"))
            else {
                continue;
            };
            for row in projection
                .observations(&entry.summary.run_id)
                .iter()
                .copied()
                .filter(|row| {
                    #[cfg(test)]
                    TEST_TEST_RUN_TERMINAL_RETENTION_ROWS.with(|count| count.set(count.get() + 1));
                    row.summary.ended_at.is_none()
                })
            {
                #[cfg(test)]
                TEST_TEST_RUN_TERMINAL_RETENTION_VISITS.with(|count| count.set(count.get() + 1));
                if test_run_observation_matches_run(inner, &entry.summary, Some(&entry.disk), row) {
                    terminal.push((
                        row.execution_id.clone(),
                        entry.summary.clone(),
                        digest.to_owned(),
                    ));
                }
            }
        }
        drop(projection);
        for (id, terminal, digest) in terminal {
            let Some(row) = inner.test_run_observations.get_mut(&id) else {
                continue;
            };
            // Keep original execution identity, provenance and authority. The
            // result digest fences later artifact replacement independently
            // of the request match that established the association.
            if let Some(artifact) = &mut row.artifact {
                artifact.terminal_digest = Some(digest);
            }
            row.summary.state = terminal.state;
            row.summary.ended_at = terminal.ended_at;
            row.summary.exit_code = terminal.exit_code;
            row.summary.interrupted = terminal.interrupted;
            row.summary.unknown_reason = terminal.unknown_reason;
            row.summary.error = terminal.error;
            row.summary.current_stage = terminal.current_stage;
            row.summary.stages = terminal.stages;
            row.summary.detail_version = terminal.detail_version;
            let row = row.clone();
            self.persist_test_command_observation_locked(inner, row);
        }
    }

    fn observe_test_command_artifact(
        &self,
        session_id: &str,
        provenance: &EngramObservationProvenance,
        key: &str,
        line: &str,
        output: &str,
        output_cut: bool,
    ) {
        if output_cut {
            return;
        }
        let Some(command) = EngramHost::recognized_check(line)
            .filter(|command| command.simple && EngramHost::launcher_arguments(command).is_some())
        else {
            return;
        };
        let fingerprint = EngramHost::check_fingerprint(&command);
        let tool_identity = sha256_hex(key.as_bytes());
        let row = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            if !test_run_producing_runtime_is_current(&inner.sessions[index], provenance) { return; }
            let runtime_identity = test_run_runtime_identity(&inner.sessions[index], provenance);
            let mut rows = inner.test_run_observations.values().filter(|row| {
                row.summary.owner_session_id.as_deref() == Some(session_id)
                    && row.runtime_identity == runtime_identity
                    && row.runtime_identity.is_some()
                    && row.tool_identity == tool_identity
                    && row.command_fingerprint == fingerprint
            });
            let Some(row) = rows.next().cloned() else {
                return;
            };
            if rows.next().is_some() {
                return;
            }
            row
        };
        let mut locators = std::collections::BTreeSet::new();
        for line in output.lines() {
            let line = line.trim();
            let directory = if let Some(directory) = line
                .strip_prefix("RUN ")
                .or_else(|| line.strip_prefix("STARTED "))
            {
                PathBuf::from(directory)
            } else if let Some(results) = line.strip_prefix("results: ") {
                let path = FsPath::new(results);
                if path.file_name().and_then(|name| name.to_str()) != Some("results.json") {
                    continue;
                }
                let Some(directory) = path.parent() else {
                    continue;
                };
                directory.to_path_buf()
            } else {
                continue;
            };
            if !directory.is_absolute()
                || directory.to_string_lossy().len() > TEST_RUN_PATH_MAX_BYTES
                || engram_network_path(&directory.to_string_lossy())
            {
                return;
            }
            locators.insert(directory);
            if locators.len() > 1 {
                return;
            }
        }
        let Some(directory) = locators.into_iter().next() else {
            return;
        };
        let Some(run_id) = directory.file_name().and_then(|name| name.to_str()) else {
            return;
        };
        let Some(uuid) = run_id
            .strip_prefix("test-")
            .and_then(|id| Uuid::parse_str(id).ok())
        else {
            return;
        };
        if run_id != format!("test-{uuid}") {
            return;
        }
        // All filesystem work remains off the global lock. A copied or linked
        // foreign directory cannot substitute for this worktree's exact run.
        let Some(expected_parent) =
            EngramHost::run_artifact_directory(FsPath::new(&row.summary.worktree))
        else {
            return;
        };
        let Ok(expected_parent) = fs::canonicalize(expected_parent) else {
            return;
        };
        let Ok(canonical) = fs::canonicalize(&directory) else {
            return;
        };
        if canonical != expected_parent.join(run_id) {
            return;
        }
        let Some(disk) = TestRunDisk::read(&canonical, None) else {
            return;
        };
        if disk.run_id != run_id
            || disk.owner.as_deref() != Some(session_id)
            || test_run_path_key(&disk.worktree) != test_run_path_key(&row.summary.worktree)
            || disk.request_identity.is_none()
        {
            return;
        }
        let mode = EngramHost::launcher_arguments(&command).and_then(|words| words.get(2).cloned());
        if (disk.preset == TestRunPreset::Full) != (mode.as_deref() == Some("full")) {
            return;
        }
        // The locator is explicit; this interval is only an anti-replay guard,
        // never a way of choosing a nearby run from a directory scan.
        let Some(observed) = row
            .summary
            .started_at
            .as_deref()
            .and_then(EngramHost::check_timestamp)
        else {
            return;
        };
        let Some(requested) = disk
            .started_at
            .as_deref()
            .and_then(EngramHost::check_timestamp)
        else {
            return;
        };
        let Some(floor) = observed.checked_sub_signed(chrono::TimeDelta::seconds(
            ENGRAM_CARRIED_LAUNCH_SLACK_SECONDS,
        )) else {
            return;
        };
        if requested < floor || requested > chrono::Utc::now() {
            return;
        }
        let artifact = TestRunObservedArtifact {
            run_id: run_id.to_owned(),
            run_directory: test_run_display_path(&canonical),
            request_identity: disk.request_identity.clone(),
            terminal_digest: None,
        };
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else { return; };
        if !test_run_producing_runtime_is_current(&inner.sessions[index], provenance) { return; }
        if inner.test_run_observations.get(&row.execution_id) != Some(&row) {
            return;
        }
        let mut updated = row;
        if updated
            .artifact
            .as_ref()
            .is_some_and(|earlier| earlier != &artifact)
        {
            return;
        }
        updated.artifact = Some(artifact);
        inner
            .test_run_observations
            .insert(updated.execution_id.clone(), updated.clone());
        self.persist_test_command_observation_locked(&mut inner, updated);
        drop(inner);
        // Reuse the existing artifact reader/index immediately, including for
        // a provider's still-running/background reply. No new process probe.
        self.refresh_test_runs();
    }
}
