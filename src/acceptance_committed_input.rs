// The committed-input decision of an acceptance evaluation.
// Once a claim has a registered committed-input run that started on a clean
// checkout, whether selected automatically or by `--input commit`, and whatever
// its result, evaluations of that claim follow the committed flow for that
// claim, fence and naming. A later plain run or `--input plain` does not undo
// this. A dirty root is refused until the work is committed and a run passes
// on that commit. A claim with no such run stays in the legacy flow, unless
// an acknowledged check belongs to a different name of that claim's root.
// A new sighting cannot repair that check. A held recovery on the same path
// can retain an older generation's evidence only with the same run identity
// and the complete unchanged source from its acknowledged original body.
// The request is admitted only
// when the root is clean at a commit, the newest applicable terminal result
// passed eligibly on exactly that commit, and Engram acknowledged that run's
// verification record (received or settled) for that commit, as the host
// observed it and keeps it on the run's registration; otherwise it is
// refused with a typed reason. A holder's committed flow does not read the
// sighting; an unheld retained-root requester additionally needs it. Native
// declared runs use their authenticated terminal and complete SourceId, never
// launcher commit decoding. Only unchanged clean native source is admitted;
// local changes require committing and running the declared test again.
// A task with no such run is in the legacy flow, where the
// sighting preflight decides as before. Unknown results ahead of a selected
// terminal result refuse; coherent unfinished runs are skipped. A scan that
// exhausts its bound without selecting a result, or reaches its deadline,
// decides neither flow and is refused. Owns reading the judged
// commit of the named root (only when the pinned source owner reports it
// clean), finding the claim's
// committed-input runs among the registrations, reading whether Engram
// acknowledged a run's record, the decision itself, and the committed source
// pin. Native admission also retains the complete SourceId through owner
// capture; evaluation submission rechecks that full identity. Does not own the
// sighting preflight (src/acceptance_named_root_sighting.rs); the rest of the
// evaluation request (src/acceptance_evaluation_api.rs); the registration and
// its pinned binding (src/test_run_registration.rs,
// src/test_run_named_root.rs); the committed-input record
// (src/test_run_committed_input.rs); or the verification record's sender and
// its files (src/engram_verification_record.rs), which it only reads. It
// grants no verdict. New module, declared beside
// src/acceptance_named_root_sighting.rs.

use super::*;

/// The most matching registrations, newest first, whose results one request
/// reads for a start: the work's own claim narrows them to a handful, and the
/// bound keeps a request's reads bounded however many runs the host retains.
/// A claim with more, none of the read ones started, is refused as an
/// incomplete scan rather than taken as the legacy flow.
pub(crate) const ACCEPTANCE_COMMITTED_INPUT_CANDIDATES_MAX: usize = 32;

#[cfg(test)]
thread_local! {
    // Runs once the run and its record were read, before the root's HEAD is
    // read again: a test changes the root there.
    pub(crate) static TEST_ACCEPTANCE_COMMITTED_INPUT_BEFORE_RECHECK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    // Counts the runs' results files the decision reads on this thread.
    pub(crate) static TEST_ACCEPTANCE_COMMITTED_INPUT_RESULTS_READS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    // A test changes a result between the start scan and terminal selection.
    pub(crate) static TEST_ACCEPTANCE_COMMITTED_INPUT_BEFORE_SELECTION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// The run that admitted an evaluation, and the commit it was judged on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AcceptanceCommittedInputAdmission {
    pub(crate) run_id: String,
    pub(crate) commit: String,
    // Native and predecessor-generation evidence retain their complete identity
    // through the request's independent owner capture; commit equality alone
    // never admits either route.
    pub(crate) recorded_source: Option<EngramSourceId>,
}

/// Which flow an evaluation request is in, when it is not refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AcceptanceCommittedInputDecision {
    /// The task's claim has no registered committed-input run pinned to its
    /// named root: the sighting preflight decides, exactly as before.
    Legacy,
    /// The clean committed flow admitted the request on this run. An unheld
    /// retained-root requester additionally needs the canonical sighting.
    Admitted(AcceptanceCommittedInputAdmission),
}

/// Why the committed flow refused an evaluation request. The sighting is
/// never read in its place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AcceptanceCommittedInputRefusal {
    /// An acknowledged check belongs to a different name of this claim's root.
    RootRebound {
        run_id: String,
        checked: TestRunNamedRoot,
        current: TestRunNamedRoot,
    },
    /// The owner cannot establish the named root as clean at a commit.
    /// Dirty or unavailable input and unavailable ownership all refuse.
    NotClean,
    NativeLocalChanges {
        run_id: String,
    },
    /// The newest applicable terminal result did not pass eligibly and
    /// cleanly on HEAD `commit`, or no terminal result matches that commit.
    NoEligibleRun {
        commit: String,
    },
    /// A run passed eligibly on `commit`, but Engram has not acknowledged its
    /// verification record for that commit; `state` says where it stands.
    RecordNotAcknowledged {
        run_id: String,
        commit: String,
        state: String,
    },
    RecordRejected {
        run_id: String,
        commit: String,
        detail: String,
        by_producer: bool,
    },
    RejectedCheckNotSelected {
        run_id: String,
    },
    RejectedCheckNewerRunPending {
        run_id: String,
    },
    RejectedCheckEvidenceMismatch {
        run_id: String,
    },
    /// The producer cannot build a record for these unsupported check stages.
    UnclassifiedStages {
        run_id: String,
        commit: String,
    },
    /// The tracker, the named root or the admitting run changed while it was
    /// checked.
    Changed,
    /// The check could not finish: the claim's registrations held more runs
    /// than one request reads without finding a deciding result, a newer
    /// run's results cannot be classified coherently, or the
    /// request's deadline passed. Never taken as the legacy flow.
    ScanIncomplete,
}

impl AcceptanceCommittedInputRefusal {
    /// The refusal's stable code, which leads its message.
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::RootRebound { .. } => "registered_check_root_rebound",
            Self::NotClean => "committed_input_not_clean",
            Self::NativeLocalChanges { .. } => "native_input_local_changes",
            Self::NoEligibleRun { .. } => "committed_input_no_eligible_run",
            Self::RecordNotAcknowledged { .. } | Self::RecordRejected { .. } => {
                "verification_record_not_acknowledged"
            }
            Self::RejectedCheckNotSelected { .. } => "rejected_check_not_selected",
            Self::RejectedCheckNewerRunPending { .. } => "rejected_check_newer_run_pending",
            Self::RejectedCheckEvidenceMismatch { .. } => "rejected_check_evidence_mismatch",
            Self::UnclassifiedStages { .. } => "verification_record_unclassified_stages",
            Self::Changed => "committed_input_changed",
            Self::ScanIncomplete => "committed_input_scan_incomplete",
        }
    }

    pub(crate) fn into_api_error(self, work_ref: &str) -> ApiError {
        let detail = match &self {
            Self::RootRebound {
                run_id,
                checked,
                current,
            } => format!(
                "registered check {run_id} for `{work_ref}` belongs to root `{}` generation {}; \
                 the current root is `{}` generation {}. Run a new registered check in the \
                 current root, then request evaluation again",
                checked.workspace_id, checked.generation, current.workspace_id, current.generation,
            ),
            Self::NotClean => format!(
                "`{work_ref}`'s named source root is not clean at a commit; commit or remove \
                 its changes, run the gate on that commit, and request again"
            ),
            Self::NativeLocalChanges { run_id } => format!(
                "native run {run_id} carries local source changes; acceptance admission \
                 requires a clean committed source. Commit the intended changes and \
                 re-run the declared test on that commit, then request evaluation again"
            ),
            Self::NoEligibleRun { commit } => format!(
                "the newest applicable terminal result of `{work_ref}`'s claim did not pass \
                 eligibly on its named root's HEAD {commit}, or no terminal result matches \
                 that commit; run the gate on that commit and request again"
            ),
            Self::RecordNotAcknowledged {
                run_id,
                commit,
                state,
            } => format!(
                "the verification record of run {run_id} at {commit} is not yet acknowledged \
                 by Engram ({state}); request again once Engram has received it"
            ),
            Self::RecordRejected {
                run_id,
                commit,
                detail,
                by_producer: true,
            } => format!(
                "Engram terminally rejected the verification record of run {run_id} at {commit} \
                 ({detail}); the refusal is final. For an eligible passing run on the current \
                 named root, request an explicit rejectedCheckDisposition with its runId and \
                 reason for context only, or use a new registered run if verification credit is required"
            ),
            Self::RecordRejected {
                run_id,
                commit,
                detail,
                by_producer: false,
            } => format!(
                "the verification record of run {run_id} at {commit} was refused ({detail}); \
                 the refusal is final. Use a new registered run if verification credit is required"
            ),
            Self::RejectedCheckNotSelected { run_id } => format!(
                "run {run_id} is not the newest applicable eligible passing result for the named \
                 root's current HEAD; rejectedCheckDisposition cannot admit it"
            ),
            Self::RejectedCheckNewerRunPending { run_id } => format!(
                "newer matching run {run_id} is unfinished; wait for its terminal result before \
                 requesting rejectedCheckDisposition again"
            ),
            Self::RejectedCheckEvidenceMismatch { run_id } => format!(
                "run {run_id} does not match host-retained verification or terminal evidence; \
                 rejectedCheckDisposition cannot authenticate the passing result"
            ),
            Self::UnclassifiedStages { run_id, commit } => format!(
                "run {run_id} at {commit} contains checks TermAl cannot record as test or build; \
                 this run has no classified verification route today. Waiting or requesting \
                 evaluation again will not send its record. For supported verification, run a \
                 recognized real test or build on this commit as a new registered run. \
                 termal-tests.toml supports native \
                 declared tests with supported result artifacts, not arbitrary focused checks; \
                 do not relabel this check as a test or build"
            ),
            Self::Changed => format!(
                "`{work_ref}`'s named source root or its committed-input run changed during \
                 the check; request again"
            ),
            Self::ScanIncomplete => format!(
                "the committed-input check of `{work_ref}` did not finish: its claim has more \
                 registered runs than one request reads, a registered committed-input run's \
                 results are missing, unreadable, unfinished or incoherent, or the request's \
                 deadline passed; neither flow is chosen on an incomplete check"
            ),
        };
        ApiError::conflict(format!("{}: {detail}", self.code()))
    }
}

impl AppState {
    /// Acceptance retains its clean-commit policy; the initiating project's
    /// pinned owner supplies that verdict. Unavailable ownership or input
    /// supplies no judged commit, without a local Git fallback.
    pub(crate) fn acceptance_judged_commit(
        &self,
        session: &str,
        root: &FsPath,
        deadline: std::time::Instant,
    ) -> Option<String> {
        self.acceptance_judged_clean_source(session, root, deadline)
            .map(|source| source.commit)
    }

    fn acceptance_judged_clean_source(
        &self,
        session: &str,
        root: &FsPath,
        deadline: std::time::Instant,
    ) -> Option<EngramSourceId> {
        let deadline = EngramSourcePhasePlan::from(deadline);
        deadline.check().ok()?;
        #[cfg(test)]
        let metadata = self
            .inner
            .lock()
            .expect("state mutex poisoned")
            .test_native_source_owner_metadata
            .clone();
        #[cfg(test)]
        let measured = match metadata {
            Some(measure) => measure(self, session, deadline.probe_deadline),
            None => {
                self.source_owner_metadata_snapshot_with(session, deadline, engram_source_owner_run)
            }
        };
        #[cfg(not(test))]
        let measured =
            self.source_owner_metadata_snapshot_with(session, deadline, engram_source_owner_run);
        let (pin, authority) = measured.ok()?;
        let deadline = deadline.following_observation();
        #[cfg(test)]
        let capture = self
            .inner
            .lock()
            .expect("state mutex poisoned")
            .test_acceptance_source_owner_capture
            .clone();
        #[cfg(test)]
        let observed = engram_source_observation_with(
            &pin,
            root,
            deadline,
            |binary, cwd, args, until, limit| match &capture {
                Some(capture) => capture(binary, cwd, args, until, limit),
                None => engram_source_owner_run(binary, cwd, args, until, limit),
            },
        )
        .ok()?;
        #[cfg(not(test))]
        let observed =
            engram_source_observation_with(&pin, root, deadline, engram_source_owner_run).ok()?;
        if engram_source_owner_authority_locked(
            &self.inner.lock().expect("state mutex poisoned"),
            session,
        )
        .ok()?
            != authority
            || observed.status != EngramSourceStatus::Clean
        {
            return None;
        }
        let source = observed.into_source_id(deadline.completion_deadline).ok()?;
        test_run_commit_id_valid(&source.commit).then_some(source)
    }
}

/// Reads a run's `results.json`; the one read of a run's results this
/// decision makes, counted in test builds.
pub(crate) fn acceptance_committed_input_read_results(run_directory: &str) -> TestRunJson {
    #[cfg(test)]
    TEST_ACCEPTANCE_COMMITTED_INPUT_RESULTS_READS.with(|reads| reads.set(reads.get() + 1));
    test_run_read_json(&FsPath::new(run_directory).join("results.json"))
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AcceptanceRegisteredInputOutcome {
    Pending,
    NotApplicable,
    Terminal(bool),
}

/// The newest applicable terminal result decides. Only a rejected-check
/// request must wait for a newer unfinished run. Unreadable results refuse.
pub(crate) fn acceptance_committed_input_run_outcome(
    run_id: &str,
    run_directory: &str,
    current: &EngramSourceId,
) -> Result<AcceptanceRegisteredInputOutcome, AcceptanceCommittedInputRefusal> {
    use AcceptanceRegisteredInputOutcome as Outcome;
    let TestRunJson::Parsed(results, _, _) = acceptance_committed_input_read_results(run_directory)
    else {
        return Err(AcceptanceCommittedInputRefusal::ScanIncomplete);
    };
    match acceptance_committed_input_result_start(run_id, &results) {
        AcceptanceCommittedInputRunStart::Unknown => {
            return Err(AcceptanceCommittedInputRefusal::ScanIncomplete);
        }
        AcceptanceCommittedInputRunStart::Pending => return Ok(Outcome::Pending),
        AcceptanceCommittedInputRunStart::NotStarted => return Ok(Outcome::NotApplicable),
        AcceptanceCommittedInputRunStart::Started => {}
    }
    let input = TestRunCommittedInput::read(&results)
        .ok_or(AcceptanceCommittedInputRefusal::ScanIncomplete)?;
    if results.get("state").and_then(Value::as_str) == Some("running") {
        return Ok(Outcome::Pending);
    }
    let terminal =
        test_run_terminal_state(&results).ok_or(AcceptanceCommittedInputRefusal::ScanIncomplete)?;
    if input.commit.as_deref() != Some(current.commit.as_str()) {
        return Ok(Outcome::NotApplicable);
    }
    if terminal != TestRunState::Passed {
        return Ok(Outcome::Terminal(false));
    }
    let source_matches =
        if results.get("sourceIdBefore").is_some() || results.get("sourceIdAfter").is_some() {
            let (before, after) = test_run_source_endpoints(&results)
                .ok_or(AcceptanceCommittedInputRefusal::ScanIncomplete)?;
            before == *current && after == *current
        } else {
            // A genuine historical Commit record retains its original path.
            // A full source_id receipt is checked separately against current.
            true
        };
    Ok(Outcome::Terminal(
        source_matches
            && input.clean_before
            && input.outcome == Some(TestRunCommittedInputOutcome::Clean)
            && input.eligible == Some(true)
            && input
                .after
                .as_ref()
                .is_some_and(|after| after.clean && after.commit == current.commit),
    ))
}

/// What a matching committed-input run's results show about its start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AcceptanceCommittedInputRunStart {
    /// A coherent committed-input record, clean before the run, and a stage
    /// started: the run selects the committed flow.
    Started,
    /// Positively shown never to have started: failed terminal results with
    /// a coherent record and no stage started, refused as not clean before
    /// the run, or clean before it with nothing recorded after (HEAD moved
    /// while its input was captured, an input drift).
    NotStarted,
    /// Coherent running results before any stage. No completed outcome yet;
    /// they neither supersede a terminal result nor establish the legacy flow.
    Pending,
    /// Neither shown: the results are missing, unreadable or incoherent, or
    /// their committed-input record is missing or incoherent although the
    /// run was registered as one. Native registrations are classified from
    /// their own host reservation and terminal instead. Never `NotStarted`.
    Unknown,
}

/// Whether the results in `run_directory` show the run started on a clean
/// checkout, never started, or neither ([`AcceptanceCommittedInputRunStart`]).
pub(crate) fn acceptance_committed_input_run_start(
    run_id: &str,
    run_directory: &str,
) -> AcceptanceCommittedInputRunStart {
    use AcceptanceCommittedInputRunStart as Start;
    let TestRunJson::Parsed(results, _, _) = acceptance_committed_input_read_results(run_directory)
    else {
        return Start::Unknown;
    };
    acceptance_committed_input_result_start(run_id, &results)
}

fn acceptance_committed_input_result_start(
    run_id: &str,
    results: &Value,
) -> AcceptanceCommittedInputRunStart {
    use AcceptanceCommittedInputRunStart as Start;
    if results.get("runId").and_then(Value::as_str) != Some(run_id) {
        return Start::Unknown;
    }
    // The launcher registers before capturing input. Only its coherent
    // initial running shape is pending; a malformed or null input is unknown.
    if results.get("state").and_then(Value::as_str) == Some("running")
        && results.get("input").is_none()
        && results
            .get("stages")
            .and_then(Value::as_array)
            .is_some_and(|stages| {
                stages.iter().all(|stage| {
                    stage.get("state").and_then(Value::as_str) == Some("unrun")
                        && stage.get("started").is_none_or(Value::is_null)
                })
            })
        && results.get("ended").is_none_or(Value::is_null)
    {
        return Start::Pending;
    }
    // Source capture can fail after registration but before an input record
    // or any stage exists. Only the launcher's coherent terminal shape proves
    // that this run never started; absent or malformed evidence stays unknown.
    if results.get("input").is_none()
        && results.get("state").and_then(Value::as_str) == Some("failed")
        && results
            .get("exitCode")
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 0)
        && results
            .get("ended")
            .and_then(Value::as_str)
            .is_some_and(|ended| !ended.is_empty())
        && results
            .get("stages")
            .and_then(Value::as_array)
            .is_some_and(|stages| {
                !stages.is_empty()
                    && stages.iter().all(|stage| {
                        stage.get("state").and_then(Value::as_str) == Some("unrun")
                            && stage.get("started").is_none_or(Value::is_null)
                            && stage.get("ended").is_none_or(Value::is_null)
                    })
            })
    {
        return Start::NotStarted;
    }
    let Some(input) = TestRunCommittedInput::read(&results) else {
        return Start::Unknown;
    };
    let stage_started = results
        .get("stages")
        .and_then(Value::as_array)
        .is_some_and(|stages| {
            stages
                .iter()
                .any(|stage| stage.get("started").and_then(Value::as_str).is_some())
        });
    if input.clean_before && stage_started {
        return Start::Started;
    }
    if input.clean_before && results.get("state").and_then(Value::as_str) == Some("running") {
        return Start::Pending;
    }
    if !stage_started
        && input.after.is_none()
        && input.outcome.is_none()
        && test_run_terminal_state(&results) == Some(TestRunState::Failed)
    {
        return Start::NotStarted;
    }
    Start::Unknown
}

/// Whether Engram acknowledged the verification record of `run_id` for
/// `commit`: the host itself received Engram's receipt for it, as kept on
/// `registration` (`TestRunVerificationAck`, received or settled; never the
/// state file beside the run, which an agent can write), its frozen body is
/// the host's own record of `registration` (rebuilt from it and the run's
/// files, never trusted as written) and names this run and this commit as its
/// input, it is the very body the receipt answered (the digest the host kept
/// with the receipt), and that receipt answers that body. Unsupported stages
/// are read directly from the run's request and terminal results, without
/// waiting for or writing sender progress. Other pending reasons come from
/// the state file and affect refusal guidance only.
pub(crate) fn acceptance_committed_input_record_acknowledged(
    registration: &TestRunRegistration,
    commit: &str,
) -> Result<EngramVerificationRecordBody, AcceptanceCommittedInputRefusal> {
    let run_id = registration.run_id.as_str();
    let run_dir = FsPath::new(&registration.run_directory);
    let unacknowledged = |state: String| AcceptanceCommittedInputRefusal::RecordNotAcknowledged {
        run_id: run_id.to_owned(),
        commit: commit.to_owned(),
        state,
    };
    let (receipt, body_digest) = match &registration.verification_record {
        Some(
            TestRunVerificationAck::Received {
                receipt,
                body_digest,
            }
            | TestRunVerificationAck::Settled {
                receipt,
                body_digest,
            }
            | TestRunVerificationAck::Unassociated {
                receipt,
                body_digest,
                ..
            },
        ) => (receipt.clone(), body_digest.clone()),
        Some(TestRunVerificationAck::Rejected { detail, .. }) => {
            return Err(AcceptanceCommittedInputRefusal::RecordRejected {
                run_id: run_id.to_owned(),
                commit: commit.to_owned(),
                detail: detail.clone(),
                by_producer: engram_verification_rejected_by_producer(
                    registration.verification_record.as_ref(),
                ),
            });
        }
        Some(TestRunVerificationAck::LocalUnclassified { .. }) => {
            return Err(AcceptanceCommittedInputRefusal::UnclassifiedStages {
                run_id: run_id.to_owned(),
                commit: commit.to_owned(),
            });
        }
        Some(TestRunVerificationAck::BindingRetry { wait, .. }) => {
            return Err(unacknowledged(verification_binding_retry_detail(*wait)));
        }
        None => {
            let request = test_run_read_json(&run_dir.join("request.json"));
            let results = test_run_read_json(&run_dir.join("results.json"));
            if let (TestRunJson::Parsed(request, _, _), TestRunJson::Parsed(results, _, _)) =
                (request, results)
                && test_run_registration_identity(&request).ok().as_ref()
                    == Some(&registration.request_identity)
                && engram_verification_record_results_are_terminal(registration, &results)
                && !engram_verification_record_registration_unclassified_stages(
                    registration,
                    &request,
                    &results,
                )
                .is_empty()
            {
                return Err(AcceptanceCommittedInputRefusal::UnclassifiedStages {
                    run_id: run_id.to_owned(),
                    commit: commit.to_owned(),
                });
            }
            let state = match engram_verification_record_progress(run_dir) {
                Some(EngramVerificationRecordProgress::Pending { reason }) => {
                    let kind = serde_json::to_value(&reason)
                        .ok()
                        .and_then(|value| {
                            value.get("kind").and_then(Value::as_str).map(str::to_owned)
                        })
                        .unwrap_or_else(|| "unknown".to_owned());
                    format!("pending: {kind}")
                }
                _ => "the host has received no receipt for it".to_owned(),
            };
            return Err(unacknowledged(state));
        }
    };
    let Some(frozen) = engram_verification_record_frozen(run_dir, run_id) else {
        return Err(unacknowledged(
            "acknowledged, but its frozen body cannot be read back".to_owned(),
        ));
    };
    if !engram_verification_record_is_host_body(registration, &frozen.body) {
        return Err(unacknowledged(
            "acknowledged, but its frozen body is not the host's own record of the run".to_owned(),
        ));
    }
    // The run's files and its body may have been rewritten together after
    // Engram's answer: only the very body the receipt answered is admitted.
    if !engram_verification_record_digest_answers(&body_digest, &frozen.body) {
        return Err(unacknowledged(
            "acknowledged for another body: the frozen body changed after Engram's answer"
                .to_owned(),
        ));
    }
    if frozen.body.registration.host_run_id != run_id
        || frozen.body.input.clean_commit() != Some(commit)
    {
        return Err(unacknowledged(
            "acknowledged for another run or input".to_owned(),
        ));
    }
    engram_verification_record_receipt_check(&frozen.body, &receipt).map_err(|error| {
        unacknowledged(format!(
            "acknowledged with a receipt that does not answer it: {error}"
        ))
    })?;
    Ok(frozen.body)
}

/// The evaluated work's claim as the requester's live held-claims read
/// (`engram work core held`) showed it when the evaluation was requested: its
/// id, its current fence and, when Engram listed one, the binding it would
/// accept. A live renewal by the same holder keeps the fence; a lapse and
/// recovery keeps the claim id but raises the fence. Existing evidence keeps
/// its registered binding; new registrations still require a current name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AcceptanceCurrentClaim {
    pub(crate) claim_id: String,
    pub(crate) claim_fence: i64,
    pub(crate) binding: Option<EngramControlWorkBinding>,
}

impl AcceptanceCurrentClaim {
    pub(crate) fn of_held(claim: &EngramHeldClaim) -> Self {
        Self {
            claim_id: claim.claim_id.clone(),
            claim_fence: claim.claim_fence,
            binding: claim.control_binding.clone(),
        }
    }

    /// Recovery changes the claim's fence, not the identity that named the root.
    fn same_claim(&self, entry: &EngramWorkSourceRoot) -> bool {
        entry.claim_id == self.claim_id
    }
}

// An unheld requester can inspect retained evidence without claiming its
// live fence. The original naming claim/fence and exact root still apply.
fn acceptance_committed_input_registration_matches_requester(
    registration: &TestRunRegistration,
    store: &EngramAuthorityStoreKey,
    work_id: &str,
    entry: &EngramWorkSourceRoot,
    current: Option<&AcceptanceCurrentClaim>,
) -> bool {
    registration.authority_store.as_ref() == Some(store)
        && current.is_none_or(|current| current.same_claim(entry))
        && registration.named_root.as_ref().is_some_and(|named| {
            named.workspace_id == entry.root && named.generation == entry.generation
        })
        && registration.work_binding.as_ref().is_some_and(|binding| {
            binding.work_id == work_id
                && binding.claim_id == entry.claim_id
                && binding.claim_fence == entry.claim_fence
                && current.is_none_or(|current| {
                    current.binding.as_ref().map_or(
                        current.claim_fence == binding.claim_fence,
                        |live| {
                            engram_same_recovery_run(live, binding)
                                && live.claim_fence == current.claim_fence
                                && live.claim_fence >= binding.claim_fence
                                && live.work_revision >= binding.work_revision
                        },
                    )
                })
        })
}

/// A held recovery may inspect the original registration at a strictly older
/// generation of the exact same workspace. This only selects candidates:
/// their terminal result, acknowledged body and complete source are checked
/// off-lock before admission. No receipt or binding is relabelled.
fn acceptance_committed_input_predecessor_matches(
    registration: &TestRunRegistration,
    store: &EngramAuthorityStoreKey,
    work_id: &str,
    entry: &EngramWorkSourceRoot,
    current: Option<&AcceptanceCurrentClaim>,
) -> bool {
    let Some(current) = current else { return false };
    let Some(live) = current.binding.as_ref() else {
        return false;
    };
    registration.authority_store.as_ref() == Some(store)
        && current.same_claim(entry)
        && live.work_id == work_id
        && live.claim_fence == current.claim_fence
        && live.claim_fence >= entry.claim_fence
        && registration.named_root.as_ref().is_some_and(|named| {
            named.workspace_id == entry.root && named.generation < entry.generation
        })
        && registration.work_binding.as_ref().is_some_and(|binding| {
            binding.work_id == work_id
                && binding.claim_id == entry.claim_id
                && binding.claim_fence <= entry.claim_fence
                && engram_same_recovery_run(live, binding)
                && live.work_revision >= binding.work_revision
        })
}

fn acceptance_committed_candidate(
    registration: &TestRunRegistration,
    store: &EngramAuthorityStoreKey,
    entry: &EngramWorkSourceRoot,
    claim: Option<&AcceptanceCurrentClaim>,
) -> bool {
    acceptance_committed_input_registration_matches_requester(
        registration,
        store,
        &entry.work_id,
        entry,
        claim,
    ) || acceptance_committed_input_predecessor_matches(
        registration,
        store,
        &entry.work_id,
        entry,
        claim,
    )
}

fn acceptance_committed_registration_order(
    left: &TestRunRegistration,
    right: &TestRunRegistration,
) -> std::cmp::Ordering {
    right
        .registered_at
        .cmp(&left.registered_at)
        .then_with(|| left.run_id.cmp(&right.run_id))
}

fn acceptance_published_registrations<'a>(
    inner: &'a StateInner,
    indexed: &'a HashSet<&'a str>,
) -> impl Iterator<Item = &'a TestRunRegistration> + Clone {
    inner
        .test_run_registrations
        .values()
        .filter(move |row| inner.test_runs.registered.contains(&row.run_id))
        .chain(
            inner
                .test_run_compacted
                .values()
                .filter(move |row| {
                    row.compacted
                        && test_run_compact_is_installed_with(inner, row, |id| indexed.contains(id))
                })
                .filter_map(|row| row.registration.as_ref()),
        )
}

/// In-memory admission witness only. A rejected record never reaches the
/// producer as verification evidence that could supersede the selection.
pub(crate) struct AcceptanceRejectedCheckSelection {
    store: EngramAuthorityStoreKey,
    root: AcceptanceEvaluationSourceRoot,
    claim: Option<AcceptanceCurrentClaim>,
    admitted: TestRunRegistration,
    candidates: HashSet<String>,
    retired: Vec<TestRunRegistration>,
}

impl AcceptanceRejectedCheckSelection {
    fn unchanged_locked(&self, inner: &StateInner) -> bool {
        let Some(entry) = engram_work_source_root_for_claim(
            &inner.engram_work_source_roots,
            &self.store,
            &self.root.work_id,
            &self.root.claim_id,
        )
        .filter(|entry| entry.generation == self.root.generation && entry.root == self.root.root) else {
            return false;
        };
        let indexed = test_run_indexed_ids(&inner.test_runs);
        let mut count = 0;
        acceptance_published_registrations(inner, &indexed)
            .filter(|row| {
                acceptance_committed_candidate(row, &self.store, entry, self.claim.as_ref())
            })
            .filter(|row| {
                !self
                    .retired
                    .iter()
                    .any(|old| acceptance_committed_input_registration_unchanged(old, row))
            })
            .all(|row| {
                count += 1;
                self.candidates.contains(&row.run_id)
                    && (row.run_id != self.admitted.run_id
                        || row.matches_registration_snapshot(&self.admitted))
            })
            && count == self.candidates.len()
    }

    pub(crate) fn require_unchanged_locked(
        &self,
        inner: &StateInner,
        work_ref: &str,
    ) -> Result<(), ApiError> {
        if self.unchanged_locked(inner) {
            Ok(())
        } else {
            Err(AcceptanceCommittedInputRefusal::Changed.into_api_error(work_ref))
        }
    }
}

/// Whether `row`, the registration as it stands now, is still the run
/// `admitted` was read as: the same run, directory, request, tracker, binding
/// and named root, acknowledged for the same body. Received becoming settled
/// for that body is no change; any other field the host fills in meanwhile is
/// not looked at.
pub(crate) fn acceptance_committed_input_registration_unchanged(
    admitted: &TestRunRegistration,
    row: &TestRunRegistration,
) -> bool {
    let digest = |registration: &TestRunRegistration| {
        registration
            .verification_record
            .as_ref()
            .and_then(TestRunVerificationAck::body_digest)
            .map(str::to_owned)
    };
    row.run_id == admitted.run_id
        && row.run_directory == admitted.run_directory
        && row.locator_identity == admitted.locator_identity
        && row.request_identity == admitted.request_identity
        && row.authority_store == admitted.authority_store
        && row.work_binding == admitted.work_binding
        && row.named_root == admitted.named_root
        && row.declared_native == admitted.declared_native
        && row.declared_terminal == admitted.declared_terminal
        && row.source_owner == admitted.source_owner
        && digest(row).is_some()
        && digest(row) == digest(admitted)
}

/// The scheme used by retained committed source revisions and verification records.
const COMMITTED_SOURCE_REVISION_SCHEME: &str = "git-commit";

/// The source revision an evaluation admitted on `commit` judges and declares.
pub(crate) fn acceptance_committed_judged_revision(commit: &str) -> String {
    format!("{COMMITTED_SOURCE_REVISION_SCHEME}:{commit}")
}

/// The commit a declared source revision names, when it is a committed one
/// ([`acceptance_committed_judged_revision`]); None for a content revision.
pub(crate) fn acceptance_committed_revision_commit(revision: &str) -> Option<&str> {
    revision
        .strip_prefix(COMMITTED_SOURCE_REVISION_SCHEME)?
        .strip_prefix(':')
}

impl AppState {
    /// The source preflight of an evaluation of `task` in its named source
    /// root `root`: the committed flow's decision, and for a legacy or unheld
    /// retained-root request the sighting preflight. `Some` with the admission when the
    /// committed flow admitted the request, whose commit the evaluation then
    /// judges; `None` in the legacy flow.
    pub(crate) fn require_acceptance_source_preflight_excluding(
        &self,
        parent_session_id: &str,
        reader: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey,
        task: &AcceptanceEvaluationTask,
        root: Option<&AcceptanceEvaluationSourceRoot>,
        deadline: std::time::Instant,
        now: &impl Fn() -> std::time::Instant,
        dispositions: &[AcceptanceRetiredCheckDisposition],
    ) -> Result<
        (
            Option<AcceptanceCommittedInputAdmission>,
            Option<AcceptanceRejectedCheckSelection>,
        ),
        ApiError,
    > {
        let rejected = dispositions.iter().find(|d| d.rejected_source.is_some());
        let (decision, selection) = self
            .acceptance_committed_input_decision_with_dispositions(
                parent_session_id,
                reader,
                store,
                task,
                root,
                deadline,
                now,
                dispositions,
                rejected.map(|d| &d.registration),
                false,
            )
            .map_err(|refusal| refusal.into_api_error(&task.work_ref))?;
        match decision {
            AcceptanceCommittedInputDecision::Admitted(admission) => {
                if let Some(disposition) = rejected
                    && disposition.rejected_source != admission.recorded_source
                {
                    return Err(
                        AcceptanceCommittedInputRefusal::Changed.into_api_error(&task.work_ref)
                    );
                }
                // A coordinator's retained selection is also confirmed by
                // Engram's canonical sighting; a passing run cannot replace
                // that path/generation check for this newly admitted route.
                if task.current_claim.is_none() {
                    self.require_acceptance_named_root_sighting(
                        parent_session_id,
                        reader,
                        store,
                        task,
                        root,
                        deadline,
                        now,
                    )?;
                }
                Ok((Some(admission), selection))
            }
            AcceptanceCommittedInputDecision::Legacy => {
                if let Some(disposition) = rejected {
                    return Err(AcceptanceCommittedInputRefusal::RejectedCheckNotSelected {
                        run_id: disposition.registration.run_id.clone(),
                    }
                    .into_api_error(&task.work_ref));
                }
                self.require_acceptance_named_root_sighting(
                    parent_session_id,
                    reader,
                    store,
                    task,
                    root,
                    deadline,
                    now,
                )
                .map(|()| (None, None))
            }
        }
    }

    /// The committed-input decision for an evaluation of `task` in its named
    /// source root `root`. The flow is decided by the registrations alone,
    /// never by the launcher's committed-input default.
    ///
    /// `Legacy` when no durably registered committed-input run bound to the
    /// task's work run and original naming fence and pinned to the root's
    /// generation,
    /// whose results show it started on a clean checkout
    /// (`acceptance_committed_input_run_start`), exists, and every matching
    /// run read was shown never to have started (or no root is named, or the session's tracker is no longer
    /// `reader` and `store`, or a held claim is not the naming claim).
    /// An unheld requester scans the retained naming claim/fence instead;
    /// absence from its held list does not waive committed-input admission.
    /// Before choosing Legacy, an acknowledged check for a different name of
    /// the same claim's root refuses with both identities and a new-check remedy.
    /// A held same-path predecessor may instead join the candidate scan, but
    /// only its verified complete SourceId can prove unchanged input; a
    /// commit-only historical body never establishes that equivalence.
    /// Otherwise the request is in the committed flow: `Admitted` when the
    /// root is clean at HEAD, the newest applicable terminal result passed
    /// eligibly and cleanly on that very commit, and Engram acknowledged its
    /// verification record for that commit; a typed refusal otherwise. Git and the run files are read off
    /// the state lock; after them the root's clean HEAD is read again and
    /// must be the same commit, and the root and the run's registration
    /// (with the acknowledgment the host keeps on it) are checked again under
    /// the lock.
    ///
    /// At most [`ACCEPTANCE_COMMITTED_INPUT_CANDIDATES_MAX`] matching
    /// registrations, newest first, are read for a start, each only before
    /// `deadline` (as `now` tells the time); Git reads the root under the
    /// same deadline. Selection re-reads started and pending candidates, so
    /// a pending run that has since finished decides in the same order.
    /// Unknown results before the selected terminal result, an exhausted
    /// truncated scan, or an expired deadline refuse as `ScanIncomplete`.
    /// Only a complete scan of coherent never-started runs selects `Legacy`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn acceptance_committed_input_decision(
        &self,
        parent_session_id: &str,
        reader: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey,
        task: &AcceptanceEvaluationTask,
        root: Option<&AcceptanceEvaluationSourceRoot>,
        deadline: std::time::Instant,
        now: &impl Fn() -> std::time::Instant,
    ) -> Result<AcceptanceCommittedInputDecision, AcceptanceCommittedInputRefusal> {
        self.acceptance_committed_input_decision_with_dispositions(
            parent_session_id,
            reader,
            store,
            task,
            root,
            deadline,
            now,
            &[],
            None,
            false,
        )
        .map(|(decision, _)| decision)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn acceptance_committed_input_decision_with_dispositions(
        &self,
        parent_session_id: &str,
        reader: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey,
        task: &AcceptanceEvaluationTask,
        root: Option<&AcceptanceEvaluationSourceRoot>,
        deadline: std::time::Instant,
        now: &impl Fn() -> std::time::Instant,
        dispositions: &[AcceptanceRetiredCheckDisposition],
        rejected: Option<&TestRunRegistration>,
        defer_source_recheck: bool,
    ) -> Result<
        (
            AcceptanceCommittedInputDecision,
            Option<AcceptanceRejectedCheckSelection>,
        ),
        AcceptanceCommittedInputRefusal,
    > {
        use AcceptanceCommittedInputDecision as Decision;
        use AcceptanceCommittedInputRefusal as Refusal;
        use AcceptanceRegisteredInputOutcome as Outcome;
        let Some(root) = root else {
            return Ok((Decision::Legacy, None));
        };
        let Some(work_id) = task
            .canonical_work_id()
            .filter(|work_id| *work_id == root.work_id)
        else {
            return Ok((Decision::Legacy, None));
        };
        let claim = task.current_claim.as_ref();
        let current = |inner: &StateInner| -> bool {
            acceptance_evaluation_host_target_locked(inner, parent_session_id)
                .is_ok_and(|target| target.store == *store && target.connection == *reader)
                && root.still_named(&inner.engram_work_source_roots, store)
                && engram_work_source_root_for_claim(
                    &inner.engram_work_source_roots,
                    store,
                    &root.work_id,
                    &root.claim_id,
                )
                .is_some_and(|entry| claim.is_none_or(|claim| claim.same_claim(entry)))
        };
        let excluded = |row: &TestRunRegistration| {
            dispositions.iter().any(|d| {
                d.rejected_source.is_none()
                    && acceptance_committed_input_registration_unchanged(&d.registration, row)
            })
        };
        let (candidates, rebound) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if !current(&inner) {
                return Ok((Decision::Legacy, None));
            }
            let Some(entry) = engram_work_source_root_for_claim(
                &inner.engram_work_source_roots,
                store,
                &root.work_id,
                &root.claim_id,
            ) else {
                return Ok((Decision::Legacy, None));
            };
            let indexed = test_run_indexed_ids(&inner.test_runs);
            let registrations =
                acceptance_published_registrations(&inner, &indexed).filter(|row| !excluded(row));
            // Filtering by the new name must not erase an acknowledged check
            // and turn it into a legacy request. A current-name run still has
            // its ordinary admission rules; this guard only closes fallback.
            let rebound = registrations
                .clone()
                .filter(|registration| {
                    registration.authority_store.as_ref() == Some(store)
                        && registration.work_binding.as_ref().is_some_and(|binding| {
                            binding.work_id == work_id && binding.claim_id == entry.claim_id
                        })
                        && registration.named_root.as_ref().is_some_and(|named| {
                            named.workspace_id != entry.root || named.generation != entry.generation
                        })
                        && registration
                            .verification_record
                            .as_ref()
                            .is_some_and(|ack| {
                                matches!(
                                    ack,
                                    TestRunVerificationAck::Received { .. }
                                        | TestRunVerificationAck::Settled { .. }
                                        | TestRunVerificationAck::Unassociated { .. }
                                ) && ack.body_digest().is_some()
                            })
                })
                .max_by(|a, b| {
                    a.registered_at
                        .cmp(&b.registered_at)
                        .then_with(|| b.run_id.cmp(&a.run_id))
                })
                .and_then(|registration| {
                    registration
                        .named_root
                        .as_ref()
                        .map(|checked| Refusal::RootRebound {
                            run_id: registration.run_id.clone(),
                            checked: checked.clone(),
                            current: TestRunNamedRoot {
                                workspace_id: entry.root.clone(),
                                generation: entry.generation,
                            },
                        })
                });
            let mut candidates = registrations
                .filter(|registration| {
                    acceptance_committed_candidate(registration, store, entry, claim)
                })
                .cloned()
                .collect::<Vec<_>>();
            // This early check keeps registration order; overlapping runs can
            // finish in another order. Engram rechecks its newest result at
            // evaluation submission and completion.
            candidates.sort_by(acceptance_committed_registration_order);
            (candidates, rebound)
        };
        // Only a run that started on a clean checkout selects the committed
        // flow; a refused or never-started one leaves the task where it was.
        // Read off the lock, newest first, at most the bound's results files
        // in all, each only before the request's deadline.
        let rejected_candidates = rejected.map(|_| {
            candidates
                .iter()
                .map(|registration| registration.run_id.clone())
                .collect::<HashSet<_>>()
        });
        let total = candidates.len();
        let mut scanned = 0;
        let mut observed = Vec::new();
        let mut any_started = false;
        for registration in candidates {
            if scanned == ACCEPTANCE_COMMITTED_INPUT_CANDIDATES_MAX || now() >= deadline {
                break;
            }
            scanned += 1;
            let start = acceptance_registered_input_start(&registration);
            if start != AcceptanceCommittedInputRunStart::NotStarted {
                any_started |= start == AcceptanceCommittedInputRunStart::Started;
                observed.push((registration, start));
            }
        }
        if !any_started {
            // The legacy flow only when every matching run was read and each
            // was shown never to have started; an incomplete scan, or a run
            // whose results show neither, never decides it.
            return if scanned == total && observed.is_empty() {
                rebound.map_or(Ok((Decision::Legacy, None)), Err)
            } else {
                Err(Refusal::ScanIncomplete)
            };
        }
        let source = match self.acceptance_judged_clean_source(
            parent_session_id,
            FsPath::new(&root.root),
            deadline,
        ) {
            Some(source) => source,
            None if now() >= deadline => return Err(Refusal::ScanIncomplete),
            None => {
                return Err(observed
                    .iter()
                    .find_map(|(registration, _)| acceptance_native_local_changes(registration))
                    .unwrap_or(Refusal::NotClean));
            }
        };
        let commit = source.commit.clone();
        let mut admitting: Option<TestRunRegistration> = None;
        let mut recorded_source = None;
        #[cfg(test)]
        TEST_ACCEPTANCE_COMMITTED_INPUT_BEFORE_SELECTION.with(|slot| {
            if let Some(step) = slot.borrow_mut().take() {
                step();
            }
        });
        for (registration, start) in observed {
            if now() >= deadline {
                return Err(Refusal::ScanIncomplete);
            }
            if start == AcceptanceCommittedInputRunStart::Unknown {
                return Err(Refusal::ScanIncomplete);
            }
            let admits = match acceptance_registered_input_outcome(&registration, &source)? {
                Outcome::Terminal(admits) => admits,
                Outcome::Pending
                    if rejected.is_some_and(|requested| {
                        acceptance_committed_registration_order(&registration, requested).is_lt()
                    }) =>
                {
                    return Err(Refusal::RejectedCheckNewerRunPending {
                        run_id: registration.run_id,
                    });
                }
                Outcome::Pending | Outcome::NotApplicable => continue,
            };
            if !admits {
                return Err(Refusal::NoEligibleRun { commit });
            }
            if let Some(rejected) = rejected {
                if !registration.matches_registration_snapshot(rejected)
                    || !engram_verification_rejected_by_producer(
                        registration.verification_record.as_ref(),
                    )
                {
                    return Err(Refusal::RejectedCheckNotSelected {
                        run_id: rejected.run_id.clone(),
                    });
                }
                let body = engram_verification_record_frozen(
                    FsPath::new(&registration.run_directory),
                    &registration.run_id,
                )
                .ok_or(Refusal::ScanIncomplete)?
                .body;
                if body.run.outcome != EngramVerificationRecordRunOutcome::Passed
                    || body.input.clean_commit() != Some(commit.as_str())
                    || matches!(&body.input, EngramVerificationRecordInput::SourceId { source_id } if *source_id != source)
                {
                    return Err(Refusal::NoEligibleRun { commit });
                }
                if !self.engram_rejected_verification_body_matches(&registration, &body) {
                    return Err(Refusal::RejectedCheckEvidenceMismatch {
                        run_id: registration.run_id.clone(),
                    });
                }
                // The ordinary selection checked the newest eligible terminal
                // outcome. A historical Commit body stays in its own format.
                // The independently measured source is the evaluator's basis.
                recorded_source = Some(source.clone());
                admitting = Some(registration);
                break;
            }
            let body = acceptance_committed_input_record_acknowledged(&registration, &commit)?;
            // The frozen body and its acknowledgment bind the original
            // source, including the full identity for launcher records.
            recorded_source = match body.input {
                EngramVerificationRecordInput::SourceId { source_id } if source_id == source => {
                    Some(source_id)
                }
                EngramVerificationRecordInput::SourceId { .. } => {
                    return Err(Refusal::NoEligibleRun { commit });
                }
                _ => None,
            };
            if recorded_source.is_none()
                && let Some(checked) = registration
                    .named_root
                    .as_ref()
                    .filter(|checked| checked.generation < root.generation)
            {
                return Err(Refusal::RootRebound {
                    run_id: registration.run_id.clone(),
                    checked: checked.clone(),
                    current: TestRunNamedRoot {
                        workspace_id: root.root.clone(),
                        generation: root.generation,
                    },
                });
            }
            admitting = Some(registration);
            break;
        }
        let Some(admitting) = admitting else {
            return Err(if scanned < total {
                Refusal::ScanIncomplete
            } else {
                Refusal::NoEligibleRun { commit }
            });
        };
        if !defer_source_recheck {
            self.require_acceptance_committed_source_unchanged(
                parent_session_id,
                FsPath::new(&root.root),
                &commit,
                recorded_source.as_ref(),
                deadline,
                now,
            )?;
        }
        let selection = rejected_candidates.map(|candidates| AcceptanceRejectedCheckSelection {
            store: store.clone(),
            root: root.clone(),
            claim: claim.cloned(),
            admitted: admitting.clone(),
            candidates,
            retired: dispositions
                .iter()
                .filter(|d| d.rejected_source.is_none())
                .map(|d| d.registration.clone())
                .collect(),
        });
        let inner = self.inner.lock().expect("state mutex poisoned");
        let retained = inner
            .test_run_registrations
            .get(&admitting.run_id)
            .filter(|_| inner.test_runs.registered.contains(&admitting.run_id))
            .or_else(|| {
                inner
                    .test_run_compacted
                    .get(&admitting.run_id)
                    .filter(|row| row.compacted && test_run_compact_is_installed(&*inner, row))
                    .and_then(|row| row.registration.as_ref())
            });
        if !current(&inner)
            || !selection.as_ref().map_or_else(
                || {
                    retained.is_some_and(|row| {
                        acceptance_committed_input_registration_unchanged(&admitting, row)
                    })
                },
                |selection| selection.unchanged_locked(&inner),
            )
        {
            return Err(Refusal::Changed);
        }
        drop(inner);
        let admission = AcceptanceCommittedInputAdmission {
            run_id: admitting.run_id,
            commit,
            recorded_source,
        };
        if rejected.is_none() {
            eprintln!(
                "acceptance evaluation> `{}` admitted on committed-input run {} at {}, its \
                 verification record acknowledged by Engram",
                task.work_ref, admission.run_id, admission.commit
            );
        }
        Ok((Decision::Admitted(admission), selection))
    }

    pub(crate) fn require_acceptance_committed_source_unchanged(
        &self,
        parent_session_id: &str,
        root: &FsPath,
        commit: &str,
        recorded_source: Option<&EngramSourceId>,
        deadline: std::time::Instant,
        now: &impl Fn() -> std::time::Instant,
    ) -> Result<(), AcceptanceCommittedInputRefusal> {
        use AcceptanceCommittedInputRefusal as Refusal;

        #[cfg(test)]
        TEST_ACCEPTANCE_COMMITTED_INPUT_BEFORE_RECHECK.with(|slot| {
            if let Some(step) = slot.borrow_mut().take() {
                step();
            }
        });
        let rechecked = self.acceptance_judged_clean_source(parent_session_id, root, deadline);
        if rechecked
            .as_ref()
            .zip(recorded_source)
            .is_some_and(|(current, expected)| !current.same_format(expected))
        {
            return Err(Refusal::ScanIncomplete);
        }
        if rechecked.is_none_or(|current| {
            if let Some(expected) = recorded_source {
                &current != expected
            } else {
                current.commit != commit
            }
        }) {
            return Err(if now() >= deadline {
                Refusal::ScanIncomplete
            } else {
                Refusal::Changed
            });
        }
        Ok(())
    }
}
