// The committed-input decision of an acceptance evaluation. A task whose
// claim has a durably registered committed-input run (`--input commit`, which
// selects that flow explicitly whatever the launcher's default) pinned to its
// named source root, which started on a clean checkout (a run refused before
// any stage selects nothing), is in the committed flow: the request is admitted only
// when the root is clean at a commit, the newest applicable terminal result
// passed eligibly on exactly that commit, and Engram acknowledged that run's
// verification record (received or settled) for that commit, as the host
// observed it and keeps it on the run's registration; otherwise it is
// refused with a typed reason, and Engram's named-root sighting is never read
// in this flow. A task with no such run is in the legacy flow, where the
// sighting preflight decides as before. Unknown results ahead of a selected
// terminal result refuse; coherent unfinished runs are skipped. A scan that
// exhausts its bound without selecting a result, or reaches its deadline,
// decides neither flow and is refused. Owns reading the judged
// commit of the named root (HEAD, only when it is clean under the
// committed-input rule of src/ordinary_commit_input.rs), finding the claim's
// committed-input runs among the registrations, reading whether Engram
// acknowledged a run's record, the decision itself, and the committed source
// revision (`git-commit:<commit>`) an admitted evaluation judges, declares and
// is judged on again at its first submission. Does not own the
// sighting preflight (src/acceptance_named_root_sighting.rs); the rest of the
// evaluation request (src/acceptance_evaluation_api.rs); the registration and
// its pinned binding (src/test_run_registration.rs,
// src/test_run_named_root.rs); the committed-input record
// (src/test_run_committed_input.rs); or the verification record's sender and
// its files (src/engram_verification_record.rs), which it only reads. It
// grants no verdict. New module, included beside
// src/acceptance_named_root_sighting.rs.

/// The most matching registrations, newest first, whose results one request
/// reads for a start: the work's own claim narrows them to a handful, and the
/// bound keeps a request's reads bounded however many runs the host retains.
/// A claim with more, none of the read ones started, is refused as an
/// incomplete scan rather than taken as the legacy flow.
const ACCEPTANCE_COMMITTED_INPUT_CANDIDATES_MAX: usize = 32;

#[cfg(test)]
thread_local! {
    // Runs once the run and its record were read, before the root's HEAD is
    // read again: a test changes the root there.
    static TEST_ACCEPTANCE_COMMITTED_INPUT_BEFORE_RECHECK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    // Counts the runs' results files the decision reads on this thread.
    static TEST_ACCEPTANCE_COMMITTED_INPUT_RESULTS_READS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    // A test changes a result between the start scan and terminal selection.
    static TEST_ACCEPTANCE_COMMITTED_INPUT_BEFORE_SELECTION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// The run that admitted an evaluation, and the commit it was judged on.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AcceptanceCommittedInputAdmission {
    run_id: String,
    commit: String,
}

/// Which flow an evaluation request is in, when it is not refused.
#[derive(Clone, Debug, PartialEq, Eq)]
enum AcceptanceCommittedInputDecision {
    /// The task's claim has no registered committed-input run pinned to its
    /// named root: the sighting preflight decides, exactly as before.
    Legacy,
    /// The committed flow admitted the request on this run and commit; no
    /// sighting is read.
    Admitted(AcceptanceCommittedInputAdmission),
}

/// Why the committed flow refused an evaluation request. The sighting is
/// never read in its place.
#[derive(Clone, Debug, PartialEq, Eq)]
enum AcceptanceCommittedInputRefusal {
    /// The named root is not clean at a commit: a staged, unstaged or
    /// untracked path, a submodule gitlink, an index-skipped tracked path, an
    /// unborn HEAD, or a tree Git cannot read.
    NotClean,
    /// The newest applicable terminal result did not pass eligibly and
    /// cleanly on HEAD `commit`, or no terminal result matches that commit.
    NoEligibleRun { commit: String },
    /// A run passed eligibly on `commit`, but Engram has not acknowledged its
    /// verification record for that commit; `state` says where it stands.
    RecordNotAcknowledged {
        run_id: String,
        commit: String,
        state: String,
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
    fn code(&self) -> &'static str {
        match self {
            Self::NotClean => "committed_input_not_clean",
            Self::NoEligibleRun { .. } => "committed_input_no_eligible_run",
            Self::RecordNotAcknowledged { .. } => "verification_record_not_acknowledged",
            Self::Changed => "committed_input_changed",
            Self::ScanIncomplete => "committed_input_scan_incomplete",
        }
    }

    fn into_api_error(self, work_ref: &str) -> ApiError {
        let detail = match &self {
            Self::NotClean => format!(
                "`{work_ref}`'s named source root is not clean at a commit; commit or remove \
                 its changes, run the gate on that commit, and request again"
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

/// The commit `root` is judged on: its HEAD when the worktree is clean under
/// the committed-input rule the source revision uses
/// ([`committed_source_clean_head`] in src/ordinary_commit_input.rs): no
/// staged, unstaged or untracked path outside the repository's own ignore
/// rules, no submodule gitlink (refused unread, as the freeze verifier
/// does), and no tracked path the index tells Git to skip (assume-unchanged
/// or skip-worktree). None
/// otherwise, for an unborn HEAD, or a tree Git cannot read, including one
/// Git cannot finish reading before the request's `deadline`.
fn acceptance_judged_commit(root: &FsPath, deadline: std::time::Instant) -> Option<String> {
    committed_source_clean_head_within(root, Some(deadline))
        .filter(|head| test_run_commit_id_valid(head))
}

/// Reads a run's `results.json`; the one read of a run's results this
/// decision makes, counted in test builds.
fn acceptance_committed_input_read_results(run_directory: &str) -> TestRunJson {
    #[cfg(test)]
    TEST_ACCEPTANCE_COMMITTED_INPUT_RESULTS_READS.with(|reads| reads.set(reads.get() + 1));
    test_run_read_json(&FsPath::new(run_directory).join("results.json"))
}

/// The admission outcome of a terminal result of `run_id` on `commit`.
/// Ok(None) skips a coherent unfinished or other-commit result; Ok(Some(false))
/// selects a failed or ineligible result. An unreadable or incoherent result
/// refuses selection rather than falling back to an older PASS.
fn acceptance_committed_input_run_outcome(
    run_id: &str,
    run_directory: &str,
    commit: &str,
) -> Result<Option<bool>, AcceptanceCommittedInputRefusal> {
    let TestRunJson::Parsed(results, _, _) = acceptance_committed_input_read_results(run_directory)
    else {
        return Err(AcceptanceCommittedInputRefusal::ScanIncomplete);
    };
    match acceptance_committed_input_result_start(run_id, &results) {
        AcceptanceCommittedInputRunStart::Unknown => {
            return Err(AcceptanceCommittedInputRefusal::ScanIncomplete)
        }
        AcceptanceCommittedInputRunStart::Pending
        | AcceptanceCommittedInputRunStart::NotStarted => return Ok(None),
        AcceptanceCommittedInputRunStart::Started => {}
    }
    let input = TestRunCommittedInput::read(&results)
        .ok_or(AcceptanceCommittedInputRefusal::ScanIncomplete)?;
    if results.get("state").and_then(Value::as_str) == Some("running") {
        return Ok(None);
    }
    let terminal =
        test_run_terminal_state(&results).ok_or(AcceptanceCommittedInputRefusal::ScanIncomplete)?;
    if input.commit.as_deref() != Some(commit) {
        return Ok(None);
    }
    Ok(Some(
        terminal == TestRunState::Passed
            && input.clean_before
            && input.outcome == Some(TestRunCommittedInputOutcome::Clean)
            && input.eligible == Some(true)
            && input
                .after
                .as_ref()
                .is_some_and(|after| after.clean && after.commit == commit),
    ))
}

/// What a matching committed-input run's results show about its start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AcceptanceCommittedInputRunStart {
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
    /// run was registered as one (only an `input: "commit"` run is pinned to
    /// a named root). Never taken as `NotStarted`.
    Unknown,
}

/// Whether the results in `run_directory` show the run started on a clean
/// checkout, never started, or neither ([`AcceptanceCommittedInputRunStart`]).
fn acceptance_committed_input_run_start(
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
/// with the receipt), and that receipt answers that body. `Err` says where the record
/// stands otherwise; a pending reason is read from the state file for that
/// message alone.
fn acceptance_committed_input_record_acknowledged(
    registration: &TestRunRegistration,
    commit: &str,
) -> Result<(), String> {
    let run_id = registration.run_id.as_str();
    let run_dir = FsPath::new(&registration.run_directory);
    let (receipt, body_digest) = match &registration.verification_record {
        Some(
            TestRunVerificationAck::Received { receipt, body_digest }
            | TestRunVerificationAck::Settled { receipt, body_digest },
        ) => (receipt.clone(), body_digest.clone()),
        Some(TestRunVerificationAck::Rejected { detail }) => {
            return Err(format!("rejected: {detail}"));
        }
        None => {
            return Err(match engram_verification_record_progress(run_dir) {
                Some(EngramVerificationRecordProgress::Pending { reason }) => {
                    let kind = serde_json::to_value(&reason)
                        .ok()
                        .and_then(|value| value.get("kind").and_then(Value::as_str).map(str::to_owned))
                        .unwrap_or_else(|| "unknown".to_owned());
                    format!("pending: {kind}")
                }
                _ => "the host has received no receipt for it".to_owned(),
            });
        }
    };
    let Some(frozen) = engram_verification_record_frozen(run_dir, run_id) else {
        return Err("acknowledged, but its frozen body cannot be read back".to_owned());
    };
    if !engram_verification_record_is_host_body(registration, &frozen.body) {
        return Err("acknowledged, but its frozen body is not the host's own record of the run".to_owned());
    }
    // The run's files and its body may have been rewritten together after
    // Engram's answer: only the very body the receipt answered is admitted.
    if !engram_verification_record_digest_answers(&body_digest, &frozen.body) {
        return Err("acknowledged for another body: the frozen body changed after Engram's answer".to_owned());
    }
    if frozen.body.registration.host_run_id != run_id
        || frozen.body.input
            != (EngramVerificationRecordInput::Commit {
                commit: commit.to_owned(),
            })
    {
        return Err("acknowledged for another run or input".to_owned());
    }
    engram_verification_record_receipt_check(&frozen.body, &receipt)
        .map_err(|error| format!("acknowledged with a receipt that does not answer it: {error}"))
}

/// The evaluated work's claim as the requester's live held-claims read
/// (`engram work core held`) showed it when the evaluation was requested: its
/// id, its current fence and, when Engram listed one, the binding it would
/// accept. A live renewal by the same holder keeps the fence; a lapse and
/// recovery keeps the claim id but raises the fence. Existing evidence keeps
/// its registered binding; new registrations still require a current name.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AcceptanceCurrentClaim {
    claim_id: String,
    claim_fence: i64,
    binding: Option<EngramControlWorkBinding>,
}

impl AcceptanceCurrentClaim {
    fn of_held(claim: &EngramHeldClaim) -> Self {
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

/// Whether `registration` is a durably registered committed-input run of the
/// work `work_id` under the claim that named `entry`, pinned to that root and
/// generation, in `store`. Its original naming fence stays pinned; recovery
/// may advance the live fence only when the live binding proves the same
/// work run and root execution. Evidence is never relabelled to that fence.
fn acceptance_committed_input_registration_matches(
    registration: &TestRunRegistration,
    store: &EngramAuthorityStoreKey,
    work_id: &str,
    entry: &EngramWorkSourceRoot,
    current: &AcceptanceCurrentClaim,
) -> bool {
    registration.authority_store.as_ref() == Some(store)
        && current.same_claim(entry)
        && registration.named_root.as_ref().is_some_and(|named| {
            named.workspace_id == entry.root && named.generation == entry.generation
        })
        && registration.work_binding.as_ref().is_some_and(|binding| {
            binding.work_id == work_id
                && binding.claim_id == entry.claim_id
                && binding.claim_fence == entry.claim_fence
                && current.binding.as_ref().map_or(
                    current.claim_fence == binding.claim_fence,
                    |live| {
                        engram_same_recovery_run(live, binding)
                            && live.claim_fence == current.claim_fence
                            && live.claim_fence >= binding.claim_fence
                            && live.work_revision >= binding.work_revision
                    },
                )
        })
}

/// Whether `row`, the registration as it stands now, is still the run
/// `admitted` was read as: the same run, directory, request, tracker, binding
/// and named root, acknowledged for the same body. Received becoming settled
/// for that body is no change; any other field the host fills in meanwhile is
/// not looked at.
fn acceptance_committed_input_registration_unchanged(
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
        && digest(row).is_some()
        && digest(row) == digest(admitted)
}

/// The source revision an evaluation admitted on `commit` judges and
/// declares: `git-commit:<commit>`, spelled as the run's verification record
/// (`engram_verification_record_basis`) and every committed source revision
/// (`source_revision`) spell it, whatever the committed-input default.
fn acceptance_committed_judged_revision(commit: &str) -> String {
    format!("{COMMITTED_SOURCE_REVISION_SCHEME}:{commit}")
}

/// The commit a declared source revision names, when it is a committed one
/// ([`acceptance_committed_judged_revision`]); None for a content revision.
fn acceptance_committed_revision_commit(revision: &str) -> Option<&str> {
    revision
        .strip_prefix(COMMITTED_SOURCE_REVISION_SCHEME)?
        .strip_prefix(':')
}

impl AppState {
    /// The source preflight of an evaluation of `task` in its named source
    /// root `root`: the committed flow's decision, and in the legacy flow
    /// alone the sighting preflight. `Some` with the admission when the
    /// committed flow admitted the request, whose commit the evaluation then
    /// judges; `None` in the legacy flow.
    fn require_acceptance_source_preflight(
        &self,
        parent_session_id: &str,
        reader: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey,
        task: &AcceptanceEvaluationTask,
        root: Option<&AcceptanceEvaluationSourceRoot>,
        deadline: std::time::Instant,
        now: &impl Fn() -> std::time::Instant,
    ) -> Result<Option<AcceptanceCommittedInputAdmission>, ApiError> {
        match self
            .acceptance_committed_input_decision(
                parent_session_id,
                reader,
                store,
                task,
                root,
                deadline,
                now,
            )
            .map_err(|refusal| refusal.into_api_error(&task.work_ref))?
        {
            AcceptanceCommittedInputDecision::Admitted(admission) => Ok(Some(admission)),
            AcceptanceCommittedInputDecision::Legacy => self
                .require_acceptance_named_root_sighting(
                    parent_session_id,
                    reader,
                    store,
                    task,
                    root,
                    deadline,
                    now,
                )
                .map(|()| None),
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
    /// `reader` and `store`, or the task's current claim is unknown or is
    /// not the claim that named the root).
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
    fn acceptance_committed_input_decision(
        &self,
        parent_session_id: &str,
        reader: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey,
        task: &AcceptanceEvaluationTask,
        root: Option<&AcceptanceEvaluationSourceRoot>,
        deadline: std::time::Instant,
        now: &impl Fn() -> std::time::Instant,
    ) -> Result<AcceptanceCommittedInputDecision, AcceptanceCommittedInputRefusal> {
        use AcceptanceCommittedInputDecision as Decision;
        use AcceptanceCommittedInputRefusal as Refusal;
        let Some(root) = root else {
            return Ok(Decision::Legacy);
        };
        let Some(work_id) = task
            .canonical_work_id()
            .filter(|work_id| *work_id == root.work_id)
        else {
            return Ok(Decision::Legacy);
        };
        let Some(claim) = task.current_claim.as_ref() else {
            return Ok(Decision::Legacy);
        };
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
                .is_some_and(|entry| claim.same_claim(entry))
        };
        let candidates = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if !current(&inner) {
                return Ok(Decision::Legacy);
            }
            let Some(entry) = engram_work_source_root_for_claim(
                &inner.engram_work_source_roots,
                store,
                &root.work_id,
                &root.claim_id,
            ) else {
                return Ok(Decision::Legacy);
            };
            let indexed = test_run_indexed_ids(&inner.test_runs);
            let mut candidates = inner
                .test_run_registrations
                .values()
                .filter(|registration| inner.test_runs.registered.contains(&registration.run_id))
                // Compaction retains the original registration and artifacts;
                // removing full detail must not erase a newer terminal result.
                .chain(
                    inner
                        .test_run_compacted
                        .values()
                        .filter(|row| {
                            row.compacted
                                && test_run_compact_is_installed_with(&*inner, row, |id| {
                                    indexed.contains(id)
                                })
                        })
                        .filter_map(|row| row.registration.as_ref()),
                )
                .filter(|registration| {
                    acceptance_committed_input_registration_matches(
                        registration,
                        store,
                        work_id,
                        entry,
                        claim,
                    )
                })
                .cloned()
                .collect::<Vec<_>>();
            // This early check keeps registration order; overlapping runs can
            // finish in another order. Engram rechecks its newest result at
            // evaluation submission and completion.
            candidates.sort_by(|a, b| {
                b.registered_at
                    .cmp(&a.registered_at)
                    .then_with(|| a.run_id.cmp(&b.run_id))
            });
            candidates
        };
        // Only a run that started on a clean checkout selects the committed
        // flow; a refused or never-started one leaves the task where it was.
        // Read off the lock, newest first, at most the bound's results files
        // in all, each only before the request's deadline.
        let total = candidates.len();
        let mut scanned = 0;
        let mut observed = Vec::new();
        let mut any_started = false;
        for registration in candidates {
            if scanned == ACCEPTANCE_COMMITTED_INPUT_CANDIDATES_MAX || now() >= deadline {
                break;
            }
            scanned += 1;
            let start = acceptance_committed_input_run_start(
                &registration.run_id,
                &registration.run_directory,
            );
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
                Ok(Decision::Legacy)
            } else {
                Err(Refusal::ScanIncomplete)
            };
        }
        let commit = match acceptance_judged_commit(FsPath::new(&root.root), deadline) {
            Some(commit) => commit,
            None if now() >= deadline => return Err(Refusal::ScanIncomplete),
            None => return Err(Refusal::NotClean),
        };
        let mut admitting: Option<TestRunRegistration> = None;
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
            let Some(admits) = acceptance_committed_input_run_outcome(
                &registration.run_id,
                &registration.run_directory,
                &commit,
            )?
            else {
                continue;
            };
            if !admits {
                return Err(Refusal::NoEligibleRun { commit });
            }
            acceptance_committed_input_record_acknowledged(&registration, &commit).map_err(
                |state| Refusal::RecordNotAcknowledged {
                    run_id: registration.run_id.clone(),
                    commit: commit.clone(),
                    state,
                },
            )?;
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
        // The root may have moved or changed while the run and its record
        // were read: it must still be clean at the very commit they admit.
        #[cfg(test)]
        TEST_ACCEPTANCE_COMMITTED_INPUT_BEFORE_RECHECK.with(|slot| {
            if let Some(step) = slot.borrow_mut().take() {
                step();
            }
        });
        if acceptance_judged_commit(FsPath::new(&root.root), deadline).as_deref()
            != Some(commit.as_str())
        {
            return Err(if now() >= deadline {
                Refusal::ScanIncomplete
            } else {
                Refusal::Changed
            });
        }
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
            || !retained.is_some_and(|row| {
                acceptance_committed_input_registration_unchanged(&admitting, row)
            })
        {
            return Err(Refusal::Changed);
        }
        drop(inner);
        let admission = AcceptanceCommittedInputAdmission {
            run_id: admitting.run_id,
            commit,
        };
        eprintln!(
            "acceptance evaluation> `{}` admitted on committed-input run {} at {}, its \
             verification record acknowledged by Engram; the named-root sighting is not read",
            task.work_ref, admission.run_id, admission.commit
        );
        Ok(Decision::Admitted(admission))
    }
}
