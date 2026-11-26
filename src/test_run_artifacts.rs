// Provider-to-launcher artifact association beside test_run_observations.rs.
// Owns bounded validation of the original tool's explicit locator, retention
// of exactly associated terminal facts, and launch-only uncertainty projection.
// Provider lifecycle monitoring stays in test_run_observations.rs. Does not
// guess by PID/time/command similarity, scan processes, mint verification or
// final host proof, or manufacture a launcher registration ACK.

use super::*;

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_TEST_RUN_TERMINAL_RETENTION_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static TEST_TEST_RUN_TERMINAL_RETENTION_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(crate) fn test_run_observation_launch_unconfirmed(row: &mut TestRunCommandObservation) {
    row.summary.state = TestRunState::Unknown;
    row.summary.unknown_reason = Some(TestRunUnknownReason::ResultsUnreadable);
    row.summary.error = Some("Launcher result describes only the launch; the test run's terminal outcome is not confirmed.".to_owned());
    row.summary.exit_code = None;
}

fn test_run_artifact_observation_matches_snapshot(
    actual: &TestRunCommandObservation,
    expected: &TestRunCommandObservation,
) -> bool {
    let mut identity = actual.clone();
    identity.native_completion_refusal = expected.native_completion_refusal.clone();
    &identity == expected
}

impl AppState {
    pub(crate) fn retain_test_command_artifact_refusal(
        &self,
        expected: &TestRunCommandObservation,
        code: TestRunNativeCompletionRefusalCode,
        facts: &[(TestRunNativeCompletionFact, bool)],
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(current) = inner.test_run_observations.get(&expected.execution_id) else {
            return;
        };
        // An off-lock callback cannot rewrite a newer row or an established
        // association, including the exact image held by a receipt's fence.
        if current.artifact.is_some()
            || !test_run_artifact_observation_matches_snapshot(current, expected)
        {
            return;
        }
        let refusal = TestRunNativeCompletionRefusal::new(
            TestRunNativeCompletionPhase::ArtifactAssociation,
            code,
            facts,
        );
        if current.native_completion_refusal.as_ref() == Some(&refusal) {
            return;
        }
        let Some(updated) = inner
            .test_run_observations
            .update(&expected.execution_id, |row| {
                row.native_completion_refusal = Some(refusal);
                row.clone()
            })
        else {
            return;
        };
        self.persist_test_command_observation_locked(&mut inner, updated);
    }

    // Retain factual terminal results before artifacts can disappear. Use the
    // same positive association as API/SSE alias suppression, with no I/O or
    // evidence settlement here. A native terminal proof is never rewritten.
    pub(crate) fn retain_test_command_artifact_terminals_locked(&self, inner: &mut StateInner) {
        // Reuse the fresh per-cut projection's candidate index and BTreeMap
        // row order. Its linear preparation still sees completed history,
        // but only unfinished same-run candidates reach exact matching.
        let projection = TestRunProjection::new(inner);
        let mut terminal = Vec::new();
        let mut native_outcomes = Vec::new();
        for entry in &inner.test_runs.entries {
            if !entry.disk.is_terminal()
                || !matches!(
                    entry.summary.state,
                    TestRunState::Passed | TestRunState::Failed
                )
            {
                continue;
            }
            // An exact existing run id is enough to diagnose a refusal, never
            // enough to retain completion. The helper still applies every
            // original registration/provider/artifact predicate below.
            if let Some(registration) = inner
                .test_run_registrations
                .get(&entry.summary.run_id)
                .filter(|row| row.native_completion.is_none())
            {
                let mut completion = None;
                let mut refusal = None;
                for row in projection.observations(&entry.summary.run_id) {
                    match test_run_native_completion_from_settlement(
                        inner,
                        registration,
                        Some(row),
                        entry,
                    ) {
                        Ok(candidate) if completion.is_none() => completion = Some(candidate),
                        Ok(_) => {
                            // Preserve original ambiguity semantics: only
                            // qualifying provider executions count here.
                            completion = None;
                            refusal = Some(TestRunNativeCompletionRefusal::new(
                                TestRunNativeCompletionPhase::NativeCompletion,
                                TestRunNativeCompletionRefusalCode::ProviderExecutionAmbiguous,
                                &[(TestRunNativeCompletionFact::SingleProviderExecution, false)],
                            ));
                            break;
                        }
                        Err(reason) if refusal.is_none() => refusal = Some(reason),
                        Err(_) => {}
                    }
                }
                if completion.is_some() {
                    refusal = None;
                }
                if completion.is_none() && refusal.is_none() {
                    refusal = test_run_native_completion_from_settlement(
                        inner,
                        registration,
                        None,
                        entry,
                    )
                    .err();
                }
                native_outcomes.push((registration.run_id.clone(), completion, refusal));
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
        let mut native_changed = false;
        let mut native_completed = false;
        for (run_id, completion, refusal) in native_outcomes {
            if inner.test_run_registrations.get(&run_id).is_none_or(|row| {
                row.native_completion.is_some()
                    || row.native_completion_refusal == refusal && completion.is_none()
            }) {
                continue;
            }
            native_changed = true;
            inner.test_run_registrations.update(&run_id, |row| {
                if row.native_completion.is_none() {
                    row.native_completion = completion;
                    row.native_completion_refusal = refusal;
                    if row.native_completion.is_some() {
                        self.queue_causal_test_completion_notification(row);
                        native_completed = true;
                    }
                }
            });
        }
        let observation_changed = !terminal.is_empty();
        for (id, terminal, digest) in terminal {
            let Some(row) = inner.test_run_observations.update(&id, |row| {
                // Keep original execution identity, provenance and authority.
                // The result digest fences later artifact replacement
                // independently of the request match that established it.
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
                row.clone()
            }) else {
                continue;
            };
            self.persist_test_command_observation_locked(inner, row);
        }
        if native_changed && !observation_changed {
            // A foreground provider may already have persisted its terminal.
            // Remote publication still requires exact durable read-back of
            // this registration and its original observation.
            if let Err(error) = self.persist_internal_locked(inner) {
                eprintln!("native completion persistence deferred: {error:#}");
            }
        }
        if native_completed {
            // Diagnostic-only updates keep their existing persistence path.
            // Publication still requires exact durable original custody.
            self.federation_executor_wake_signal.kick();
        }
    }

    pub(crate) fn observe_test_command_artifact(
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
            .or_else(|| engram_observed_dart_launcher(line, provenance))
            .filter(|command| command.simple)
        else {
            return;
        };
        if EngramHost::launcher_arguments(&command).is_none()
            || engram_observed_dart_words(&command.program, &command.argv[1..])
                .is_some_and(|dart| dart.external.is_some())
        {
            return;
        }
        let fingerprint = EngramHost::check_fingerprint(&command);
        let tool_identity = sha256_hex(key.as_bytes());
        let row = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            let runtime_identity = test_run_runtime_identity(&inner.sessions[index], provenance);
            let mut rows = inner.test_run_observations.values().filter(|row| {
                row.summary.owner_session_id.as_deref() == Some(session_id)
                    && row.runtime_identity == runtime_identity
                    && row.runtime_identity.is_some()
                    && row.tool_identity == tool_identity
                    && row.command_fingerprint == fingerprint
                    && row.external_dart_launch.is_none()
            });
            let Some(row) = rows.next().cloned() else {
                return;
            };
            if rows.next().is_some() {
                return;
            }
            row
        };
        self.associate_test_command_artifact(row, output, Some(&command));
    }

    /// Only the unique original observation ended by this provider result can
    /// supply the external command. Any explicit echo must agree with the
    /// decoded launch, and any reported location with the original invocation.
    pub(crate) fn observe_external_dart_artifact_from_end(
        &self,
        ended: &[String],
        provenance: &EngramObservationProvenance,
        reported_command: Option<&str>,
        reported_cwd: Option<&str>,
        output: &str,
        output_cut: bool,
    ) {
        if output_cut {
            return;
        }
        let [id] = ended else {
            return;
        };
        let row = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .test_run_observations
                .get(id)
                .filter(|row| {
                    row.monitoring_interrupted_at.is_none()
                        && row.artifact.is_none()
                        && row
                            .external_dart_launch
                            .as_ref()
                            .is_some_and(|launch| launch.provider_ended)
                })
                .cloned()
        };
        let Some(row) = row else {
            return;
        };
        let Some(launch) = row.external_dart_launch.as_ref() else {
            return;
        };
        if reported_cwd.is_some_and(|cwd| {
            !launch
                .provider_location
                .as_ref()
                .is_some_and(|location| location.matches(cwd))
        }) {
            return;
        }
        if let Some(line) = reported_command {
            let Some(command) = engram_observed_dart_launcher(line, provenance) else {
                return;
            };
            let Some(words) = engram_observed_dart_words(&command.program, &command.argv[1..])
            else {
                return;
            };
            let Some(external) = words.external else {
                return;
            };
            let root = FsPath::new(&row.summary.worktree);
            let directory_matches = command.directory.as_deref().is_none_or(|directory| {
                engram_resolve_shell_move(None, directory).is_some_and(|directory| {
                    engram_dart_worktree_matches(root, FsPath::new(&directory))
                })
            });
            if EngramHost::check_fingerprint(&command) != row.command_fingerprint
                || !directory_matches
                || !engram_dart_worktree_matches(root, FsPath::new(external.root))
                || engram_external_dart_command_digest(external.script, words.sdk).as_deref()
                    != Some(launch.command_digest.as_str())
            {
                return;
            }
        }
        self.associate_test_command_artifact(row, output, None);
    }

    fn associate_test_command_artifact(
        &self,
        row: TestRunCommandObservation,
        output: &str,
        command: Option<&EngramCheckCommand>,
    ) {
        let dart = command
            .and_then(|command| engram_observed_dart_words(&command.program, &command.argv[1..]));
        let is_external = command.is_none();
        let launcher_words = command.and_then(EngramHost::launcher_arguments);
        // Native runs already own their original reservation and artifact.
        // Console locators, previews and persisted-output metadata cannot
        // replace that association.
        if row.declared_native.is_some() {
            return;
        }
        use TestRunNativeCompletionFact::*;
        use TestRunNativeCompletionRefusalCode::*;
        let refuse =
            |code, facts: &[_]| self.retain_test_command_artifact_refusal(&row, code, facts);
        let mut locators = std::collections::BTreeSet::new();
        let mut has_run_locator = false;
        for line in output.lines() {
            let line = line.trim();
            let directory = if let Some(directory) = line.strip_prefix("RUN ") {
                has_run_locator = true;
                PathBuf::from(directory)
            } else if let Some(directory) = line.strip_prefix("STARTED ") {
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
                refuse(
                    ArtifactLocatorInvalid,
                    &[
                        (LocatorAbsolute, directory.is_absolute()),
                        (
                            LocatorWithinBound,
                            directory.to_string_lossy().len() <= TEST_RUN_PATH_MAX_BYTES,
                        ),
                        (
                            LocatorLocal,
                            !engram_network_path(&directory.to_string_lossy()),
                        ),
                    ],
                );
                return;
            }
            locators.insert(directory);
            if locators.len() > 1 {
                refuse(ArtifactLocatorAmbiguous, &[(SingleLocator, false)]);
                return;
            }
        }
        if is_external && !has_run_locator {
            return;
        }
        let Some(directory) = locators.into_iter().next() else {
            // No explicit locator was offered for association. In particular,
            // a non-locator frame cannot rewrite an original observation.
            return;
        };
        let Some(run_id) = directory.file_name().and_then(|name| name.to_str()) else {
            refuse(ArtifactRunIdInvalid, &[(RunIdValid, false)]);
            return;
        };
        let Some(uuid) = run_id
            .strip_prefix("test-")
            .and_then(|id| Uuid::parse_str(id).ok())
        else {
            refuse(ArtifactRunIdInvalid, &[(RunIdValid, false)]);
            return;
        };
        if run_id != format!("test-{uuid}") {
            refuse(ArtifactRunIdInvalid, &[(RunIdValid, false)]);
            return;
        }
        // All filesystem work remains off the global lock. A copied or linked
        // foreign directory cannot substitute for this worktree's exact run.
        if row.summary.worktree.is_empty() {
            refuse(ArtifactWorktreeUnavailable, &[(WorktreePresent, false)]);
            return;
        }
        let worktree = FsPath::new(&row.summary.worktree);
        let Some(expected_parent) = EngramHost::run_artifact_directory(worktree) else {
            refuse(ArtifactRootUnavailable, &[(ArtifactRootAvailable, false)]);
            return;
        };
        let Some(expected_parent) = (if is_external {
            engram_external_launcher_run_parent(worktree)
        } else {
            fs::canonicalize(expected_parent).ok()
        }) else {
            refuse(ArtifactRootUnavailable, &[(ArtifactRootResolved, false)]);
            return;
        };
        let Ok(canonical) = fs::canonicalize(&directory) else {
            refuse(ArtifactDirectoryUnavailable, &[(DirectoryResolved, false)]);
            return;
        };
        if canonical != expected_parent.join(run_id) {
            refuse(ArtifactDirectoryMismatch, &[(DirectoryMatches, false)]);
            return;
        }
        let Some(disk) = TestRunDisk::read(&canonical, None) else {
            refuse(ArtifactRequestUnavailable, &[(RequestReadable, false)]);
            return;
        };
        let external_matches = match (is_external, disk.external_launcher.as_ref()) {
            (false, None) => true,
            (true, Some(launcher)) => {
                !disk.command_truncated
                    && row.external_dart_launch.as_ref().is_some_and(|launch| {
                        disk.command
                            .as_deref()
                            .and_then(|sdk| {
                                engram_external_dart_command_digest(&launcher.script, sdk)
                            })
                            .as_deref()
                            == Some(launch.command_digest.as_str())
                    })
            }
            _ => false,
        };
        if !external_matches
            || (is_external
                && !row.source_root.as_ref().is_some_and(|named| {
                    engram_dart_worktree_matches(FsPath::new(&named.root), worktree)
                }))
        {
            refuse(ArtifactRequestMismatch, &[(ObservationMatches, false)]);
            return;
        }
        let dart_argv = dart.as_ref().map(|dart| dart.sdk).or_else(|| {
            let command = command?;
            engram_focused_launcher_words(&command.program, &command.argv[1..])
                .and_then(Result::ok)
                .filter(|argv| engram_supports_focused_dart_test(argv))
        });
        let worktree_matches = if is_external || dart_argv.is_some() {
            engram_dart_worktree_matches(
                FsPath::new(&row.summary.worktree),
                FsPath::new(&disk.worktree),
            )
        } else {
            test_run_path_key(&disk.worktree) == test_run_path_key(&row.summary.worktree)
        };
        let owner_matches =
            row.summary.owner_session_id.is_some() && disk.owner == row.summary.owner_session_id;
        if disk.run_id != run_id
            || !owner_matches
            || !worktree_matches
            || disk.request_identity.is_none()
        {
            refuse(
                ArtifactRequestMismatch,
                &[
                    (RunMatches, disk.run_id == run_id),
                    (RequestOwnerMatches, owner_matches),
                    (WorktreeMatches, worktree_matches),
                    (RequestIdentityPresent, disk.request_identity.is_some()),
                ],
            );
            return;
        }
        let mode = launcher_words
            .as_ref()
            .and_then(|words| words.get(2))
            .map(String::as_str);
        if (disk.preset == TestRunPreset::Full) != (mode == Some("full")) {
            refuse(ArtifactModeMismatch, &[(ModeMatches, false)]);
            return;
        }
        // Bind the launch observation to the actual focused Dart request,
        // even if the shell decoded a word differently.
        if let Some(planned) = dart_argv
            && (disk.command_truncated || disk.command.as_deref() != Some(planned))
        {
            refuse(ArtifactRequestMismatch, &[(ObservationMatches, false)]);
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
            refuse(
                ArtifactObservationTimeUnavailable,
                &[(ObservationTimeAvailable, false)],
            );
            return;
        };
        let Some(requested) = disk
            .started_at
            .as_deref()
            .and_then(EngramHost::check_timestamp)
        else {
            refuse(
                ArtifactRequestTimeUnavailable,
                &[(RequestTimeAvailable, false)],
            );
            return;
        };
        let Some(floor) = observed.checked_sub_signed(chrono::TimeDelta::seconds(
            ENGRAM_CARRIED_LAUNCH_SLACK_SECONDS,
        )) else {
            refuse(ArtifactLaunchWindowInvalid, &[(TimeFloorAvailable, false)]);
            return;
        };
        if requested < floor || requested > chrono::Utc::now() {
            refuse(ArtifactLaunchWindowInvalid, &[(WithinLaunchWindow, false)]);
            return;
        }
        // The external foreground launch must own the terminal image read
        // during association. A subsequent scan cannot supply its identity.
        let terminal_digest = if is_external {
            let Some(results) = disk.results.as_ref().filter(|_| disk.is_terminal()) else {
                return;
            };
            Some(results.digest.clone())
        } else {
            None
        };
        let artifact = TestRunObservedArtifact {
            run_id: run_id.to_owned(),
            run_directory: test_run_display_path(&canonical),
            request_identity: disk.request_identity.clone(),
            terminal_digest,
        };
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if inner
            .test_run_observations
            .get(&row.execution_id)
            .is_none_or(|current| !test_run_artifact_observation_matches_snapshot(current, &row))
        {
            return;
        }
        let mut updated = row.clone();
        if updated
            .artifact
            .as_ref()
            .is_some_and(|earlier| earlier != &artifact)
        {
            return;
        }
        updated.artifact = Some(artifact);
        updated.native_completion_refusal = None;
        inner
            .test_run_observations
            .insert(updated.execution_id.clone(), updated.clone());
        self.persist_test_command_observation_locked(&mut inner, updated);
        drop(inner);
        // Reuse the existing artifact reader/index immediately, including for
        // a provider's still-running/background reply. No new process probe.
        self.refresh_test_runs_from("command-artifact");
    }
}
