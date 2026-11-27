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

/// Where a launch row's run comes from: the launcher's own locator lines in a
/// provider's output, or the run directory of the one host registration a
/// Claude background end matched (src/test_run_claude_launcher_locators.rs),
/// with that registration's request identity. Both reach the same on-disk
/// validation; a registration's run must still hold its own request.
#[derive(Clone, Copy, Debug)]
pub(crate) enum TestCommandArtifactLocator<'a> {
    Output(&'a str),
    Registration {
        directory: &'a FsPath,
        request_identity: &'a str,
    },
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
        self.observe_test_command_artifact_from(
            session_id,
            provenance,
            key,
            line,
            TestCommandArtifactLocator::Output(output),
            output_cut,
        );
    }

    /// Associates the launch row of `key` with the run `locator` names: the
    /// launcher locator lines of a provider's output, or the run directory of
    /// the one host registration a Claude background end matched. Either is
    /// validated on disk the same way.
    pub(crate) fn observe_test_command_artifact_from(
        &self,
        session_id: &str,
        provenance: &EngramObservationProvenance,
        key: &str,
        line: &str,
        locator: TestCommandArtifactLocator<'_>,
        output_cut: bool,
    ) {
        // A Claude line is read in the Bash dialect its launch row was
        // recognised in, so a quoted `|` reads alike; other providers' lines
        // keep the unknown dialect.
        let claude = matches!(provenance, EngramObservationProvenance::Claude(_));
        let recognized = if claude {
            engram_check_command_in(line, EngramShellDialect::Bash)
        } else {
            EngramHost::recognized_check(line)
        };
        let Some(command) = recognized
            .or_else(|| engram_observed_dart_launcher(line, provenance))
            .filter(|command| EngramHost::launcher_arguments(command).is_some())
        else {
            return;
        };
        // A Claude launcher line that is not one simple command attaches
        // nothing, and its launch row records why; another provider's is
        // passed over, as before.
        let compound = !command.simple;
        if compound && !(claude && claude_launcher_line(line)) {
            return;
        }
        if output_cut && !compound {
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
            });
            let Some(row) = rows.next().cloned() else {
                return;
            };
            if rows.next().is_some() {
                return;
            }
            row
        };
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
        if compound {
            if row.artifact.is_none() {
                refuse(ArtifactRequestMismatch, &[(ObservationMatches, false)]);
            }
            return;
        }
        let (output, registered, registered_request) = match locator {
            TestCommandArtifactLocator::Output(output) => (output, None, None),
            TestCommandArtifactLocator::Registration {
                directory,
                request_identity,
            } => ("", Some(directory.to_path_buf()), Some(request_identity)),
        };
        let console = output.lines().filter_map(|line| {
            let line = line.trim();
            if let Some(directory) = line
                .strip_prefix("RUN ")
                .or_else(|| line.strip_prefix("STARTED "))
            {
                Some(PathBuf::from(directory))
            } else if let Some(results) = line.strip_prefix("results: ") {
                let path = FsPath::new(results);
                if path.file_name().and_then(|name| name.to_str()) != Some("results.json") {
                    return None;
                }
                path.parent().map(FsPath::to_path_buf)
            } else {
                None
            }
        });
        let mut locators = std::collections::BTreeSet::new();
        for directory in registered.into_iter().chain(console) {
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
        let Some(expected_parent) =
            EngramHost::run_artifact_directory(FsPath::new(&row.summary.worktree))
        else {
            refuse(ArtifactRootUnavailable, &[(ArtifactRootAvailable, false)]);
            return;
        };
        let Ok(expected_parent) = fs::canonicalize(expected_parent) else {
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
        let dart_argv = engram_focused_launcher_words(&command.program, &command.argv[1..])
            .and_then(Result::ok)
            .filter(|argv| engram_supports_focused_dart_test(argv));
        let worktree_matches = if dart_argv.is_some() {
            engram_dart_worktree_matches(
                FsPath::new(&row.summary.worktree),
                FsPath::new(&disk.worktree),
            )
        } else {
            test_run_path_key(&disk.worktree) == test_run_path_key(&row.summary.worktree)
        };
        if disk.run_id != run_id
            || disk.owner.as_deref() != Some(session_id)
            || !worktree_matches
            || disk.request_identity.is_none()
        {
            refuse(
                ArtifactRequestMismatch,
                &[
                    (RunMatches, disk.run_id == run_id),
                    (
                        RequestOwnerMatches,
                        disk.owner.as_deref() == Some(session_id),
                    ),
                    (WorktreeMatches, worktree_matches),
                    (RequestIdentityPresent, disk.request_identity.is_some()),
                ],
            );
            return;
        }
        // A registration's run is attached only while its directory still
        // holds that registration's request, read here, once.
        if registered_request
            .is_some_and(|expected| disk.request_identity.as_deref() != Some(expected))
        {
            refuse(ArtifactRequestMismatch, &[(RegistrationMatches, false)]);
            return;
        }
        let mode = EngramHost::launcher_arguments(&command).and_then(|words| words.get(2).cloned());
        if (disk.preset == TestRunPreset::Full) != (mode.as_deref() == Some("full")) {
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
        // A registration's focused run is bound the same way to the command
        // its launch row planned, read as bash passes it, so another call's
        // run is never this row's. It is compared with the whole focused
        // command its request records, not the bounded summary; a request
        // whose args cannot be read matches no command.
        if let Some(expected) = registered_request
            && mode.as_deref() == Some("focused")
        {
            let planned = claude_bash_focused_argv(line);
            let recorded = test_run_request_focused_argv(&canonical, expected);
            if planned.is_none() || planned != recorded {
                refuse(ArtifactRequestMismatch, &[(ObservationMatches, false)]);
                return;
            }
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
        let artifact = TestRunObservedArtifact {
            run_id: run_id.to_owned(),
            run_directory: test_run_display_path(&canonical),
            request_identity: disk.request_identity.clone(),
            terminal_digest: None,
        };
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if inner
            .test_run_observations
            .get(&row.execution_id)
            .is_none_or(|current| !test_run_artifact_observation_matches_snapshot(current, &row))
        {
            return;
        }
        // The registration may have changed, or another launch row taken its
        // run, while the request was read off the lock.
        if let Some(expected) = registered_request {
            let registration_changed = inner
                .test_run_registrations
                .get(run_id)
                .is_none_or(|registration| registration.request_identity != expected);
            let carried_elsewhere = inner
                .test_run_observations
                .index()
                .artifact_run(run_id)
                .any(|key| key != &row.execution_id);
            if registration_changed || carried_elsewhere {
                drop(inner);
                if registration_changed {
                    refuse(ArtifactRequestMismatch, &[(RegistrationMatches, false)]);
                } else {
                    refuse(ArtifactAlreadyAssociated, &[(ArtifactUnchanged, false)]);
                }
                return;
            }
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
