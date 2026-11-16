// Native launcher settlement facts retained by the existing registration
// owner. These are observations, never verification qualification. No new
// result read, process probe, source capture or retrospective origin adoption.

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TestRunNativeCompletion {
    execution_id: String,
    completion: native_test_completion::Completion,
}

// Closed, factual diagnostics only. These never participate in completion
// validation, provider selection or verification qualification. No producer
// text, paths, runtime tokens or identities enter this public projection.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum TestRunNativeCompletionPhase {
    NativeCompletion,
    ArtifactAssociation,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TestRunNativeCompletionRefusalCode {
    NativeOriginMissing,
    NativeHostIdentityUnavailable,
    NativeSessionCreationUnavailable,
    ResultsUnavailable,
    ProviderObservationMissing,
    ProviderRuntimeMissing,
    RegistrationUnacknowledged,
    RegistrationProjectMismatch,
    RegistrationSessionMismatch,
    ProviderAssociationMismatch,
    RegistrationArtifactMismatch,
    ArtifactReadFailed,
    TerminalNotConfirmed,
    ProviderTerminalMismatch,
    ModeUnsupported,
    TerminalFactsInvalid,
    ProviderExecutionAmbiguous,
    ArtifactLocatorInvalid,
    ArtifactLocatorAmbiguous,
    ArtifactLocatorMissing,
    ArtifactRunIdInvalid,
    ArtifactWorktreeUnavailable,
    ArtifactRootUnavailable,
    ArtifactDirectoryUnavailable,
    ArtifactDirectoryMismatch,
    ArtifactRequestUnavailable,
    ArtifactRequestMismatch,
    ArtifactModeMismatch,
    ArtifactObservationTimeUnavailable,
    ArtifactRequestTimeUnavailable,
    ArtifactLaunchWindowInvalid,
    ArtifactObservationChanged,
    ArtifactAlreadyAssociated,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
enum TestRunNativeCompletionFact {
    NativeOriginPresent,
    CurrentHostIdentityValid,
    CurrentSessionCreationValid,
    ResultsPresent,
    ProviderPresent,
    RuntimeIdentityPresent,
    RegistrationAcknowledged,
    ProjectMatches,
    OwningSessionMatches,
    InitiatingSessionMatches,
    ObservationMatches,
    RegistrationMatches,
    ArtifactReadSucceeded,
    TerminalConfirmed,
    ProviderTerminalMatches,
    ModeSupported,
    TerminalFactsValid,
    SingleProviderExecution,
    LocatorAbsolute,
    LocatorWithinBound,
    LocatorLocal,
    SingleLocator,
    LocatorPresent,
    RunIdValid,
    WorktreePresent,
    ArtifactRootAvailable,
    ArtifactRootResolved,
    DirectoryResolved,
    DirectoryMatches,
    RequestReadable,
    RunMatches,
    RequestOwnerMatches,
    WorktreeMatches,
    RequestIdentityPresent,
    ModeMatches,
    ObservationTimeAvailable,
    RequestTimeAvailable,
    TimeFloorAvailable,
    WithinLaunchWindow,
    ObservationUnchanged,
    ArtifactUnchanged,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TestRunNativeCompletionOutcome {
    Refused,
    Pending,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
enum TestRunNativeCompletionRemedy {
    #[serde(
        rename = "Wait for the original durable registration acknowledgement; inspect its registration trace if it cannot finish."
    )]
    AwaitOriginalAcknowledgement,
    #[serde(
        rename = "Wait for the original launcher terminal results; inspect the run's current stage and owner if they remain unavailable."
    )]
    AwaitOriginalTerminal,
    #[serde(
        rename = "Use a new launcher run with native origin captured at registration; an old run cannot acquire origin retrospectively."
    )]
    CaptureOriginalOrigin,
    #[serde(
        rename = "Use a host with a validated native federation identity and start a new launcher run; this original run cannot acquire origin retrospectively."
    )]
    UseNativeHost,
    #[serde(
        rename = "Use a native session with a valid creation identity and start a new launcher run; this original run cannot acquire origin retrospectively."
    )]
    UseNativeSession,
    #[serde(
        rename = "Inspect the original provider observation on the runner. If its original correlation is unavailable or ambiguous, use a new run; do not infer an association."
    )]
    InspectOriginalProvider,
    #[serde(
        rename = "Inspect the original registration and its acknowledgement on the runner; preserve its request, project and session identity."
    )]
    InspectOriginalRegistration,
    #[serde(
        rename = "Inspect the original launcher results and provider terminal facts on the runner; correct malformed producer output before a new run."
    )]
    InspectOriginalTerminal,
    #[serde(rename = "Use the supported full or focused launcher mode for native completion.")]
    UseSupportedMode,
    #[serde(
        rename = "Inspect the current original provider observation; only an unchanged original event can attach its artifact."
    )]
    InspectCurrentObservation,
    #[serde(
        rename = "Inspect the original provider start and launcher request timestamps; do not select another run by time proximity."
    )]
    InspectOriginalTimestamps,
    #[serde(
        rename = "Inspect the original provider start and its single explicit RUN locator in the validated worktree; do not substitute a copied or foreign artifact."
    )]
    InspectOriginalLocator,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TestRunNativeCompletionRefusal {
    phase: TestRunNativeCompletionPhase,
    code: TestRunNativeCompletionRefusalCode,
    outcome: TestRunNativeCompletionOutcome,
    observed: BTreeMap<TestRunNativeCompletionFact, bool>,
    remedy: TestRunNativeCompletionRemedy,
}

impl TestRunNativeCompletionRefusal {
    fn new(
        phase: TestRunNativeCompletionPhase,
        code: TestRunNativeCompletionRefusalCode,
        observed: &[(TestRunNativeCompletionFact, bool)],
    ) -> Self {
        use TestRunNativeCompletionRefusalCode::*;
        let outcome = if matches!(code, RegistrationUnacknowledged | TerminalNotConfirmed) {
            TestRunNativeCompletionOutcome::Pending
        } else if matches!(
            code,
            ResultsUnavailable
                | ProviderObservationMissing
                | ArtifactRootUnavailable
                | ArtifactDirectoryUnavailable
                | ArtifactRequestUnavailable
                | ArtifactObservationChanged
                | ArtifactLocatorMissing
        ) {
            TestRunNativeCompletionOutcome::Unknown
        } else {
            TestRunNativeCompletionOutcome::Refused
        };
        Self {
            phase,
            code,
            outcome,
            observed: observed.iter().copied().collect(),
            remedy: Self::remedy(code),
        }
    }

    fn remedy(code: TestRunNativeCompletionRefusalCode) -> TestRunNativeCompletionRemedy {
        use TestRunNativeCompletionRefusalCode::*;
        use TestRunNativeCompletionRemedy::*;
        match code {
            NativeOriginMissing => CaptureOriginalOrigin,
            NativeHostIdentityUnavailable => UseNativeHost,
            NativeSessionCreationUnavailable => UseNativeSession,
            ProviderObservationMissing
            | ProviderRuntimeMissing
            | ProviderAssociationMismatch
            | ProviderExecutionAmbiguous => InspectOriginalProvider,
            RegistrationUnacknowledged => AwaitOriginalAcknowledgement,
            TerminalNotConfirmed => AwaitOriginalTerminal,
            RegistrationProjectMismatch
            | RegistrationSessionMismatch
            | RegistrationArtifactMismatch => InspectOriginalRegistration,
            ResultsUnavailable
            | ArtifactReadFailed
            | ProviderTerminalMismatch
            | TerminalFactsInvalid => InspectOriginalTerminal,
            ModeUnsupported => UseSupportedMode,
            ArtifactObservationChanged | ArtifactAlreadyAssociated => InspectCurrentObservation,
            ArtifactObservationTimeUnavailable
            | ArtifactRequestTimeUnavailable
            | ArtifactLaunchWindowInvalid => InspectOriginalTimestamps,
            _ => InspectOriginalLocator,
        }
    }
}

fn test_run_native_input(results: &Value) -> native_test_completion::Input {
    if results.get("sourceIdBefore").is_some() || results.get("sourceIdAfter").is_some() {
        return native_test_completion::Input::SourceId(native_test_completion::SourceInput {
            kind: native_test_completion::SourceInputKind::SourceId,
            source_id_before: test_run_source_id(results.get("sourceIdBefore")),
            source_id_after: test_run_source_id(results.get("sourceIdAfter")),
        });
    }
    let input = TestRunCommittedInput::read(results).filter(TestRunCommittedInput::coherent);
    let commit = input.and_then(|input| input.commit);
    let optional = |value: Option<&Value>, git: bool| {
        value.and_then(Value::as_str).filter(|text| if git {
            test_run_commit_id_valid(text)
        } else {
            text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }).map(str::to_owned)
    };
    // A checkout tree without an actual tested commit is not a tested-tree
    // fact. A plain/staged run does not acquire a committed-input identity.
    let tree = (commit.is_some() && results["source"]["commit"].as_str() == commit.as_deref())
        .then(|| optional(results.get("source").and_then(|source| source.get("tree")), true)).flatten();
    native_test_completion::Input::Legacy(native_test_completion::LegacyInput {
        commit, tree,
        before_fingerprint: optional(results.get("before"), false),
        after_fingerprint: optional(results.get("after"), false),
    })
}

fn test_run_native_completion_from_settlement(
    inner: &StateInner,
    registration: &TestRunRegistration,
    row: Option<&TestRunCommandObservation>,
    entry: &TestRunEntry,
) -> Result<TestRunNativeCompletion, TestRunNativeCompletionRefusal> {
    use TestRunNativeCompletionFact::*;
    use TestRunNativeCompletionRefusalCode::*;
    use native_test_completion::{Completion, Mode, NativeResult, Origin, Verdict};
    let refuse = |code, facts: &[_]| {
        TestRunNativeCompletionRefusal::new(
            TestRunNativeCompletionPhase::NativeCompletion,
            code,
            facts,
        )
    };
    let origin = registration
        .native_origin
        .as_ref()
        .ok_or_else(|| {
            // Current capture prerequisites explain what a new run needs;
            // they neither reconstruct nor replace the missing original origin.
            let host_valid = inner
                .federation
                .local
                .as_ref()
                .is_some_and(|host| host.validate().is_ok());
            let creation_valid = inner
                .find_session_index(&registration.initiating_session_id)
                .is_some_and(|index| {
                    mailbox_relay_uuid(&inner.sessions[index].federation_native_creation_id)
                });
            let code = if !host_valid {
                NativeHostIdentityUnavailable
            } else if !creation_valid {
                NativeSessionCreationUnavailable
            } else {
                NativeOriginMissing
            };
            refuse(
                code,
                &[
                    (NativeOriginPresent, false),
                    (CurrentHostIdentityValid, host_valid),
                    (CurrentSessionCreationValid, creation_valid),
                ],
            )
        })?;
    let results = entry
        .disk
        .results
        .as_ref()
        .ok_or_else(|| refuse(ResultsUnavailable, &[(ResultsPresent, false)]))?;
    if !inner.test_runs.registered.contains(&registration.run_id) {
        return Err(refuse(
            RegistrationUnacknowledged,
            &[(RegistrationAcknowledged, false)],
        ));
    }
    if registration.project_id.as_deref() != Some(origin.project_id.as_str()) {
        return Err(refuse(
            RegistrationProjectMismatch,
            &[(ProjectMatches, false)],
        ));
    }
    let owning_matches = registration.owning_session_id == origin.native_session_id;
    let initiating_matches = registration.initiating_session_id == origin.native_session_id;
    if !owning_matches || !initiating_matches {
        return Err(refuse(
            RegistrationSessionMismatch,
            &[
                (OwningSessionMatches, owning_matches),
                (InitiatingSessionMatches, initiating_matches),
            ],
        ));
    }
    if test_run_matching_registration(inner, &entry.disk) != Some(registration) {
        return Err(refuse(
            RegistrationArtifactMismatch,
            &[(RegistrationMatches, false)],
        ));
    }
    if entry.disk.read_failures != 0 {
        return Err(refuse(
            ArtifactReadFailed,
            &[(ArtifactReadSucceeded, false)],
        ));
    }
    if !entry.disk.is_terminal() {
        return Err(refuse(TerminalNotConfirmed, &[(TerminalConfirmed, false)]));
    }
    let row = row.ok_or_else(|| refuse(ProviderObservationMissing, &[(ProviderPresent, false)]))?;
    if row.runtime_identity.is_none() {
        return Err(refuse(
            ProviderRuntimeMissing,
            &[(RuntimeIdentityPresent, false)],
        ));
    }
    if !test_run_observation_matches_run(inner, &entry.summary, Some(&entry.disk), row) {
        return Err(refuse(
            ProviderAssociationMismatch,
            &[(ObservationMatches, false)],
        ));
    }
    if row.summary.ended_at.is_some()
        && (row.summary.state != entry.summary.state
            || row.summary.exit_code != entry.summary.exit_code)
    {
        return Err(refuse(
            ProviderTerminalMismatch,
            &[(ProviderTerminalMatches, false)],
        ));
    }
    let mode = match registration.preset {
        TestRunPreset::Full => Mode::Full,
        TestRunPreset::Focused => Mode::Focused,
        TestRunPreset::Live => return Err(refuse(ModeUnsupported, &[(ModeSupported, false)])),
    };
    let verdict = match results.terminal_state {
        Some(TestRunState::Passed) => Verdict::Pass,
        Some(TestRunState::Failed) if results.interrupted => Verdict::Interrupted,
        Some(TestRunState::Failed) => Verdict::Fail,
        _ => return Err(refuse(TerminalNotConfirmed, &[(TerminalConfirmed, false)])),
    };
    let completion = Completion {
        schema_version: 1,
        origin: Origin {
            host_id: origin.host.host_id.clone(),
            runner_session_id: origin.native_session_id.clone(),
        },
        run_id: registration.run_id.clone(),
        mode,
        result: NativeResult {
            verdict,
            terminal: true,
            exit: results.exit_code,
            reason: results
                .error
                .clone()
                .filter(|reason| !reason.is_empty())
                .unwrap_or_else(|| {
                    if results.interrupted {
                        "interrupted"
                    } else if verdict == Verdict::Pass {
                        "passed"
                    } else {
                        "failed"
                    }
                    .to_owned()
                }),
        },
        input: results.native_input.clone(),
        finished_at: results.ended.clone(),
    };
    completion
        .validate()
        .map_err(|_| refuse(TerminalFactsInvalid, &[(TerminalFactsValid, false)]))?;
    Ok(TestRunNativeCompletion {
        execution_id: row.execution_id.clone(),
        completion,
    })
}

// Detail-only projection of an existing durable owner row. It neither infers
// a provider association nor adds diagnostics to snapshot/SSE summaries.
fn test_run_native_completion_refusal_for_detail(
    inner: &StateInner,
    run: &TestRunSummary,
) -> Option<TestRunNativeCompletionRefusal> {
    if let Some(registration) = inner.test_run_registrations.get(&run.run_id) {
        return registration
            .native_completion
            .is_none()
            .then(|| registration.native_completion_refusal.clone())
            .flatten();
    }
    if let Some(retained) = inner.test_run_compacted.get(&run.run_id) {
        if let Some(registration) = &retained.registration {
            return registration
                .native_completion
                .is_none()
                .then(|| registration.native_completion_refusal.clone())
                .flatten();
        }
        // Provider-only history keeps its diagnostic on the original execution,
        // after the compaction owner has durably retired the live observation.
        if retained.compacted && test_run_compact_is_installed(inner, retained) {
            let mut owners = retained
                .observations
                .iter()
                .filter(|row| row.execution_id == run.run_id);
            let owner = owners.next()?;
            return (owners.next().is_none()
                && owner.summary.run_id == run.run_id
                && owner.artifact.is_none())
            .then(|| owner.native_completion_refusal.clone())
            .flatten();
        }
    }
    inner
        .test_run_observations
        .get(&run.run_id)
        .and_then(|row| row.native_completion_refusal.clone())
}
