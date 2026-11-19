// Provider-to-launcher artifact association beside test_run_observations.rs.
// Owns bounded validation of the original tool's explicit locator, retention
// of exactly associated terminal facts, and launch-only uncertainty projection.
// Provider lifecycle monitoring stays in test_run_observations.rs. Does not
// guess by PID/time/command similarity, scan processes, mint verification or
// final host proof, or manufacture a launcher registration ACK.

#[derive(Clone, PartialEq, Eq)]
struct TestRunLinkedArtifactSource {
    layout: delegation_git_boundary::ProviderLinkedWorktree,
    identity: TestRunLinkedArtifactIdentity,
    directory: String,
    terminal_digest: Option<String>,
}

impl TestRunLinkedArtifactSource {
    // The outer option identifies linked placement even when its exact proof
    // is unavailable. Those rows must never fall back to a pathname read.
    fn from_observation(row: &TestRunCommandObservation) -> Option<Option<Self>> {
        let TestRunExecutionPlacement::Linked(layout) = &row.linked_artifact_context()?.placement
        else {
            return None;
        };
        let artifact = row.artifact.as_ref()?;
        Some(row.linked_artifact_identity().map(|identity| Self {
            layout: layout.clone(),
            identity,
            directory: artifact.run_directory.clone(),
            terminal_digest: artifact.terminal_digest.clone(),
        }))
    }

    fn admits_disk(&self, disk: &TestRunDisk) -> bool {
        test_run_path_key(&test_run_display_path(&disk.run_dir)) == test_run_path_key(&self.directory)
            && test_run_linked_identity_admits(
                &self.identity,
                Some(&disk.run_id),
                disk.request_identity.as_deref(),
                disk.provider_directory_identity.as_ref(),
            )
    }
}

/// Exact artifact facts captured either at link time or at a scoped terminal read.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct TestRunLinkedArtifactIdentity {
    run_id: String,
    request_identity: String,
    directory_identity: FederationImageFileIdentity,
}

fn test_run_linked_identity_admits(
    expected: &TestRunLinkedArtifactIdentity,
    run_id: Option<&str>,
    request_identity: Option<&str>,
    directory_identity: Option<&FederationImageFileIdentity>,
) -> bool {
    run_id == Some(expected.run_id.as_str())
        && request_identity == Some(expected.request_identity.as_str())
        && directory_identity == Some(&expected.directory_identity)
}

fn test_run_linked_artifact_sources(
    inner: &StateInner,
) -> BTreeMap<String, Option<TestRunLinkedArtifactSource>> {
    let mut sources = BTreeMap::new();
    for execution in inner
        .test_run_observations
        .index()
        .linked_artifact_executions()
    {
        let Some(row) = inner.test_run_observations.get(execution) else {
            continue;
        };
        let (Some(source), Some(artifact)) =
            (TestRunLinkedArtifactSource::from_observation(row), row.artifact.as_ref())
        else {
            continue;
        };
        sources
            .entry(test_run_path_key(&artifact.run_directory))
            .and_modify(|ambiguous| *ambiguous = None)
            .or_insert(source);
    }
    sources
}

// A run ID stays reserved after the live observation moves into retained
// history. The outer option prevents an unproven or ambiguous linked ID
// from using ordinary discovery or diagnostic reads.
fn test_run_linked_artifact_by_id(
    inner: &StateInner,
    run_id: &str,
) -> Option<Option<TestRunLinkedArtifactSource>> {
    let mut live = inner.test_run_observations.index().artifact_run(run_id)
        .filter_map(|execution| inner.test_run_observations.get(execution))
        .filter_map(TestRunLinkedArtifactSource::from_observation);
    if let Some(source) = live.next() {
        return Some(if live.next().is_none() { source } else { None });
    }
    inner.test_run_compacted.get(run_id)?.linked_artifact_source()
}

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

fn test_run_artifact_observation_matches_snapshot(
    actual: &TestRunCommandObservation,
    expected: &TestRunCommandObservation,
) -> bool {
    let mut identity = actual.clone();
    identity.native_completion_refusal = expected.native_completion_refusal.clone();
    &identity == expected
}

impl AppState {
    fn retain_test_command_artifact_refusal(
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
    fn retain_test_command_artifact_terminals_locked(&self, inner: &mut StateInner) {
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
                        || row.linked_artifact_context().is_some_and(|context| {
                            context.terminal_artifact_identity.is_none()
                                && row.summary.state == entry.summary.state
                                && row.summary.exit_code == entry.summary.exit_code
                        })
                })
            {
                #[cfg(test)]
                TEST_TEST_RUN_TERMINAL_RETENTION_VISITS.with(|count| count.set(count.get() + 1));
                if test_run_observation_matches_run(inner, &entry.summary, Some(&entry.disk), row) {
                    terminal.push((
                        row.execution_id.clone(),
                        entry.summary.clone(),
                        digest.to_owned(),
                        row.linked_artifact_identity(),
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
        for (id, terminal, digest, linked_identity) in terminal {
            let Some(row) = inner.test_run_observations.update(&id, |row| {
                // Keep original execution identity, provenance and authority.
                // The result digest fences later artifact replacement
                // independently of the request match that established it.
                if let Some(artifact) = &mut row.artifact {
                    artifact.terminal_digest = Some(digest);
                }
                if let Some(context) = &mut row.execution_context {
                    context.terminal_artifact_identity = linked_identity;
                }
                if row.summary.ended_at.is_some() {
                    return row.clone();
                }
                if let Some(context) = row.execution_context.as_mut()
                    && context.phase == TestRunExecutionPhase::LauncherReported
                    && let (Some(ended_at), Some(exit)) = (&terminal.ended_at, terminal.exit_code)
                {
                    context.phase = TestRunExecutionPhase::Terminal {
                        ended_at: ended_at.clone(),
                        outcome: TestRunExecutionOutcome::Exited(exit),
                    };
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

    fn observe_test_command_artifact(
        &self,
        session_id: &str,
        provenance: &EngramObservationProvenance,
        key: &str,
        line: &str,
        output: &str,
        output_cut: bool,
    ) {
        self.observe_test_command_artifact_at(
            session_id, provenance, key, line, output, output_cut, None,
        );
    }

    fn observe_test_command_artifact_at(
        &self,
        session_id: &str,
        provenance: &EngramObservationProvenance,
        key: &str,
        line: &str,
        output: &str,
        output_cut: bool,
        execution_id: Option<&str>,
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
            let runtime_identity = test_run_runtime_identity(&inner.sessions[index], provenance);
            if let Some(id) = execution_id {
                let Some(row) = inner.test_run_observations.get(id).filter(|row| {
                    row.summary.owner_session_id.as_deref() == Some(session_id)
                        && row.runtime_identity.is_some()
                        && row.runtime_identity == runtime_identity
                        && (if row.managed_provider_execution() {
                            row.turn_generation == inner.sessions[index].active_turn_generation
                        } else {
                            matches!(provenance, EngramObservationProvenance::Runtime {
                                turn_generation, ..
                            } if row.turn_generation == *turn_generation)
                        })
                        && row.tool_identity == tool_identity
                        && row.command_fingerprint == fingerprint
                }) else {
                    return;
                };
                row.clone()
            } else {
                let mut rows = inner.test_run_observations.values().filter(|row| {
                    row.summary.owner_session_id.as_deref() == Some(session_id)
                        && row.runtime_identity == runtime_identity
                        && row.runtime_identity.is_some()
                        && row.tool_identity == tool_identity
                        && row.command_fingerprint == fingerprint
                        && !row.managed_provider_execution()
                });
                let Some(row) = rows.next().cloned() else {
                    return;
                };
                if rows.next().is_some() {
                    return;
                }
                row
            }
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
        let (canonical, pinned) = match row
            .execution_context
            .as_ref()
            .map(|context| &context.placement)
        {
            Some(TestRunExecutionPlacement::Linked(layout)) => {
                let Ok(pinned) = layout.open_run(&directory, run_id) else {
                    refuse(ArtifactDirectoryMismatch, &[(DirectoryMatches, false)]);
                    return;
                };
                (pinned.path().to_path_buf(), Some(pinned))
            }
            Some(TestRunExecutionPlacement::Refused(_)) => return,
            Some(TestRunExecutionPlacement::Ordinary) | None => {
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
                (canonical, None)
            }
        };
        let disk = match &pinned {
            Some(pinned) => TestRunDisk::read_provider_directory(pinned, None),
            None => TestRunDisk::read(&canonical, None),
        };
        let Some(disk) = disk else {
            refuse(ArtifactRequestUnavailable, &[(RequestReadable, false)]);
            return;
        };
        let directory_identity = disk.provider_directory_identity.clone();
        if pinned.is_some() && directory_identity.is_none() {
            refuse(ArtifactDirectoryMismatch, &[(DirectoryMatches, false)]);
            return;
        }
        if disk.run_id != run_id
            || disk.owner.as_deref() != Some(session_id)
            || test_run_path_key(&disk.worktree) != test_run_path_key(&row.summary.worktree)
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
                    (
                        WorktreeMatches,
                        test_run_path_key(&disk.worktree)
                            == test_run_path_key(&row.summary.worktree),
                    ),
                    (RequestIdentityPresent, disk.request_identity.is_some()),
                ],
            );
            return;
        }
        let mode = EngramHost::launcher_arguments(&command).and_then(|words| words.get(2).cloned());
        if (disk.preset == TestRunPreset::Full) != (mode.as_deref() == Some("full")) {
            refuse(ArtifactModeMismatch, &[(ModeMatches, false)]);
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
        let artifact = TestRunObservedArtifact {
            run_id: run_id.to_owned(),
            run_directory: test_run_display_path(&canonical),
            request_identity: disk.request_identity.clone(),
            terminal_digest: pinned
                .as_ref()
                .and_then(|_| disk.results.as_ref())
                .filter(|results| results.terminal_state.is_some())
                .and_then(|results| results.digest.strip_prefix("sha256:").map(str::to_owned)),
        };
        if let Some(pinned) = &pinned
            && let Some(TestRunExecutionPlacement::Linked(layout)) = row
                .execution_context
                .as_ref()
                .map(|context| &context.placement)
        {
            let still_bound = layout
                .open_run(&directory, run_id)
                .and_then(|current| current.same_directory(pinned))
                .unwrap_or(false);
            if !still_bound {
                refuse(ArtifactDirectoryMismatch, &[(DirectoryMatches, false)]);
                return;
            }
        }
        drop(pinned);
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if directory_identity.is_some()
            && inner.find_session_index(session_id).is_none_or(|index| {
                let session = &inner.sessions[index];
                session.active_turn_generation != row.turn_generation
                    || test_run_runtime_identity(session, &EngramObservationProvenance::Ambient)
                        != row.runtime_identity
            })
        {
            return;
        }
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
        if let Some(context) = &mut updated.execution_context {
            context.artifact_directory_identity = directory_identity;
        }
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
