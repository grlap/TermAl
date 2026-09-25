// Checks a mediated turn ran, as TermAl saw them, and the verification and
// environment evidence the checkpoint that closes the turn reports for them
// (Engram w-108a13d58018 criterion 2, tm-winf step 2). Owns the evidence
// wire types, the turn report that carries them beside the observations, the
// check lifecycle the recorder hooks drive (start, description, end,
// abandonment), the per-check source snapshots and their capture workers,
// the overlap marks kept on the record and the worktrees writers may write
// in, the environment fingerprint, and the assembly of a turn's report in
// feed order. Does not own command recognition and what an end may claim
// (`engram_check_recognition.rs`), where a command ran and which worktree a
// check tested (`engram_check_paths.rs`), the toolchain label
// (`engram_check_toolchain.rs`), the checkpoint request and its lifecycle
// (`engram_host_adapter.rs`), the turn's own observation and the report
// cache (`engram_turn_observations.rs`), or the runtimes' event parsing
// (`claude.rs`, `codex_app_requests.rs`, `codex_events.rs`, `acp.rs`), which
// only hand over what a command's end said. New fragment beside
// `engram_host_adapter.rs`, created instead of growing it.

/// Everything one checkpoint reports about the turn it closes: the turn's
/// observations in feed order, and the verification and environment evidence
/// minted from them. Sent as the request's own three lists, each left out
/// when empty, and cached per grant so every retry repeats it verbatim.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
struct EngramTurnReport {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    observations: Vec<EngramExecutionObservationInput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    verification_evidence: Vec<EngramVerificationEvidenceInput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    environment_evidence: Vec<EngramEnvironmentEvidenceInput>,
}

impl EngramTurnReport {
    fn is_empty(&self) -> bool {
        self.observations.is_empty()
            && self.verification_evidence.is_empty()
            && self.environment_evidence.is_empty()
    }
}

/// Engram mints one typed verification record from a producer observation in
/// the same request. It derives the result from that observation's outcome,
/// the check fingerprint from its action fingerprint, and the source basis
/// and time from it too; the host adds only the classification and text.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramVerificationEvidenceInput {
    producer_observation: EngramObservationReference,
    check_kind: EngramVerificationKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    environment: Option<EngramEnvironmentReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    refs: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum EngramObservationReference {
    ObservationId { observation_id: String },
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramVerificationKind {
    Test,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum EngramEnvironmentReference {
    Index { index: usize },
}

/// The environment one check ran in, on the check's own run and source
/// revision. Engram recomputes the fingerprint from the components and
/// refuses a mismatch.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramEnvironmentEvidenceInput {
    source_basis: EngramExecutionSourceBasis,
    environment_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    components: Option<EngramEnvironmentComponents>,
    observed_at: String,
}

/// Asserted labels, never credentials: the toolchain that ran the check, the
/// workspace, and the capability map revision the session was bound with.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramEnvironmentComponents {
    toolchain: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sandbox: Option<String>,
    workspace_id: String,
    capability_map_revision: i64,
}

/// At most this many checks are reported per turn, the latest kept: Engram
/// takes at most sixteen verification records per checkpoint, and a check
/// adds up to two observations to its limit of sixty-four.
const ENGRAM_TURN_CHECK_LIMIT: usize = 16;
/// Engram takes at most four environment records per checkpoint; a check
/// past them is reported without one, which an unpinned requirement allows.
const ENGRAM_TURN_ENVIRONMENT_LIMIT: usize = 4;
/// Engram's bound on a verification summary.
const ENGRAM_CHECK_SUMMARY_MAX_BYTES: usize = 4096;
/// Engram's bound on one verification reference.
const ENGRAM_CHECK_REF_MAX_BYTES: usize = 1024;
/// Engram's bound on one environment component label.
const ENGRAM_ENVIRONMENT_LABEL_MAX_BYTES: usize = 256;
/// The environment components of one check, or `None` when a label would
/// break Engram's bounds: each is trimmed, non-empty and at most 256 bytes,
/// and `workspace_id` must equal the basis root, so a longer root leaves the
/// check without environment evidence, which an unpinned requirement allows.
fn engram_environment_components(
    toolchain: &str,
    sandbox: Option<&str>,
    workspace_id: &str,
) -> Option<EngramEnvironmentComponents> {
    let label = |value: &str| {
        let value = value.trim();
        (!value.is_empty() && value.len() <= ENGRAM_ENVIRONMENT_LABEL_MAX_BYTES)
            .then(|| value.to_owned())
    };
    if workspace_id.trim() != workspace_id {
        return None;
    }
    Some(EngramEnvironmentComponents {
        toolchain: label(toolchain)?,
        sandbox: match sandbox {
            Some(sandbox) => Some(label(sandbox)?),
            None => None,
        },
        workspace_id: label(workspace_id)?,
        capability_map_revision: ENGRAM_CAPABILITY_MAP_REVISION,
    })
}

/// The environment fingerprint Engram recomputes: the SHA-256 of the RFC 8785
/// canonical JSON of the components, members in key order with no
/// whitespace. The members are strings and one integer, whose canonical
/// forms are serde_json's.
fn engram_environment_fingerprint(components: &EngramEnvironmentComponents) -> String {
    let string = |value: &str| serde_json::to_string(value).expect("a string serializes");
    let mut canonical = format!(
        "{{\"capability_map_revision\":{}",
        components.capability_map_revision
    );
    if let Some(sandbox) = &components.sandbox {
        canonical.push_str(&format!(",\"sandbox\":{}", string(sandbox)));
    }
    canonical.push_str(&format!(
        ",\"toolchain\":{},\"workspace_id\":{}}}",
        string(&components.toolchain),
        string(&components.workspace_id)
    ));
    sha256_hex(canonical.as_bytes())
}

/// Something a check captures on its own thread, so a runtime's event reader
/// is never held up by the processes it spawns: a source snapshot
/// (`EngramBasisCapture`) or a toolchain label (`EngramToolchainCapture`).
#[derive(Debug, Default)]
struct EngramCapture<T> {
    /// The captured value once the capture finished; unset while it runs.
    result: Mutex<Option<T>>,
    ready: std::sync::Condvar,
}

/// A review-freeze source basis, or `None` when it could not be taken.
type EngramBasisCapture = EngramCapture<Option<EngramExecutionSourceBasis>>;

/// A toolchain label (`engram_toolchain_label`), or `None` when TermAl
/// cannot name the toolchain.
type EngramToolchainCapture = EngramCapture<Option<String>>;

/// At most this many capture workers of one session run at once
/// (`capture_workers`): a new check starts only while fewer run. A kept check
/// has at most three (its two snapshots and a toolchain probe), but a check
/// dropped while they run (past the check limit, invalidated, abandoned, or
/// left by its grant) keeps its workers until they finish, so without this a
/// stream of fast tests on a slow worktree could pile up snapshots without
/// bound.
const ENGRAM_CAPTURE_WORKER_LIMIT: usize = 2 * ENGRAM_TURN_CHECK_LIMIT;

/// The source basis of the worktree `workdir` lies in, taken on its own
/// thread counted in `workers`.
fn engram_spawn_basis_capture(
    workdir: String,
    workers: &Arc<std::sync::atomic::AtomicUsize>,
) -> Arc<EngramBasisCapture> {
    EngramCapture::spawn(workers, move || {
        engram_execution_source_basis(FsPath::new(&workdir))
    })
}

#[cfg(test)]
thread_local! {
    /// The toolchain label a check started on this test thread gets in place
    /// of the host's (`engram_toolchain_label`), so a test's evidence does
    /// not depend on the toolchain the machine running it has installed.
    static TEST_ENGRAM_TOOLCHAIN_LABEL: std::cell::RefCell<Option<Option<String>>> =
        const { std::cell::RefCell::new(None) };
}

/// The toolchain label of the check `command` run in `directory`, taken on
/// its own thread counted in `workers`.
fn engram_spawn_toolchain_capture(
    command: EngramCheckCommand,
    directory: PathBuf,
    workers: &Arc<std::sync::atomic::AtomicUsize>,
) -> Arc<EngramToolchainCapture> {
    #[cfg(test)]
    if let Some(label) = TEST_ENGRAM_TOOLCHAIN_LABEL.with(|label| label.borrow().clone()) {
        return EngramToolchainCapture::settled(label);
    }
    EngramCapture::spawn(workers, move || {
        engram_toolchain_label(
            &command,
            &directory,
            std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT,
        )
    })
}

/// One running capture worker, counted in its session's `capture_workers`
/// until it is dropped: when its capture returns, or unwinds.
struct EngramCaptureWorker(Arc<std::sync::atomic::AtomicUsize>);

impl EngramCaptureWorker {
    fn start(workers: &Arc<std::sync::atomic::AtomicUsize>) -> Self {
        workers.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self(workers.clone())
    }
}

impl Drop for EngramCaptureWorker {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl<T: Clone + Send + 'static> EngramCapture<T> {
    /// Runs `capture` on its own thread, counted in `workers` while it runs:
    /// the count drops before the result is published, so a caller that saw
    /// the capture ready also sees it no longer counted.
    fn spawn(
        workers: &Arc<std::sync::atomic::AtomicUsize>,
        capture: impl FnOnce() -> T + Send + 'static,
    ) -> Arc<Self> {
        let pending = Arc::new(Self {
            result: Mutex::new(None),
            ready: std::sync::Condvar::new(),
        });
        let filling = pending.clone();
        let worker = EngramCaptureWorker::start(workers);
        std::thread::spawn(move || {
            let value = capture();
            drop(worker);
            filling.finish(value);
        });
        pending
    }

    /// A capture that is already finished with `value`.
    fn settled(value: T) -> Arc<Self> {
        let settled = Arc::new(Self {
            result: Mutex::new(None),
            ready: std::sync::Condvar::new(),
        });
        settled.finish(value);
        settled
    }

    fn finish(&self, value: T) {
        *self.result.lock().expect("Engram capture mutex poisoned") = Some(value);
        self.ready.notify_all();
    }

    /// Whether the capture has finished, with or without a value.
    fn is_ready(&self) -> bool {
        self.result
            .lock()
            .expect("Engram capture mutex poisoned")
            .is_some()
    }

    /// The captured value, waiting for it until `deadline`; `None` when it is
    /// not ready in time.
    fn wait_until(&self, deadline: std::time::Instant) -> Option<T> {
        let mut result = self.result.lock().expect("Engram capture mutex poisoned");
        loop {
            if let Some(captured) = result.as_ref() {
                return Some(captured.clone());
            }
            let remaining = deadline.checked_duration_since(std::time::Instant::now())?;
            result = self
                .ready
                .wait_timeout(result, remaining)
                .expect("Engram capture mutex poisoned")
                .0;
        }
    }
}

/// A test the agent ran during the turn `grant_id` mediates, as TermAl saw it
/// start and end.
#[derive(Clone, Debug)]
struct EngramTurnCheck {
    grant_id: String,
    key: String,
    /// The check's place in the turn, which keeps its observation ids unique
    /// even when a runtime reuses a key (Codex falls back to the command).
    sequence: usize,
    command: EngramCheckCommand,
    /// Where the command ran and which worktree it is credited to; a repeated
    /// start that says otherwise drops the check.
    target: EngramCheckTarget,
    /// The toolchain label, taken as the check started from the directory
    /// the command ran in, whose toolchain overrides chose what it ran with
    /// (`engram_toolchain_label`), so a toolchain file edited after the check
    /// cannot relabel it.
    toolchain: Arc<EngramToolchainCapture>,
    started_at: String,
    /// The Codex sandbox mode the turn ran under, when known. Other
    /// runtimes' approval modes are not sandboxes, so they have none.
    sandbox: Option<String>,
    start_basis: Arc<EngramBasisCapture>,
    /// Another command or a file edit from the same agent, or another
    /// writable session in the same worktree, overlapped the check while it
    /// was open to writes (`open_to_writes`), so its snapshots may have read
    /// content it did not test: its outcome is unknown. Set when the overlap
    /// happens, never reconstructed later.
    overlapped: bool,
    end: Option<EngramTurnCheckEnd>,
}

#[derive(Clone, Debug)]
struct EngramTurnCheckEnd {
    completed_at: String,
    exit: EngramCommandExit,
    /// The runner's result lines (`engram_check_result_lines`).
    result_lines: Vec<String>,
    /// The result lines show at least one test passed
    /// (`engram_check_showed_passing_tests`); without that a check cannot
    /// claim success.
    showed_passing_tests: bool,
    end_basis: Arc<EngramBasisCapture>,
}

/// A finished check with its snapshots resolved, ready for the report.
struct EngramResolvedCheck {
    check: EngramTurnCheck,
    end: EngramTurnCheckEnd,
    outcome: EngramExecutionOutcome,
    basis: EngramExecutionSourceBasis,
    /// The toolchain that ran the check, or `None` when TermAl cannot say
    /// (`engram_toolchain_label`); the check then has no environment
    /// evidence.
    toolchain: Option<String>,
}

/// The checks of `grant_id` that can be reported, with their snapshots
/// waited for until `deadline`: finished, ending in a result that marks the
/// end, and taken on one unchanged source revision. A check whose snapshots
/// are missing or differ is withheld, and logged: the source moved while it
/// ran, so its result belongs to no one revision. Each kept check carries
/// the toolchain label it captured as it started, waited for until
/// `deadline` too.
fn engram_resolve_turn_checks(
    session_id: &str,
    grant_id: &str,
    checks: Vec<EngramTurnCheck>,
    deadline: std::time::Instant,
) -> Vec<EngramResolvedCheck> {
    let mut resolved = Vec::new();
    for check in checks {
        if check.grant_id != grant_id {
            continue;
        }
        let Some(end) = check.end.clone() else {
            continue;
        };
        let Some(outcome) = engram_check_outcome(end.exit, check.command.simple) else {
            continue;
        };
        let start = check.start_basis.wait_until(deadline);
        let finish = end.end_basis.wait_until(deadline);
        let basis = match (start, finish) {
            (Some(Some(start)), Some(Some(finish))) if start == finish => start,
            _ => {
                // Named by program and fingerprint: the command line itself can
                // carry a secret, and it goes to Engram only in the report.
                eprintln!(
                    "engram> session={session_id} {} check {} is not reported: its source \
                     snapshots are missing or changed while it ran",
                    check.command.program,
                    engram_check_fingerprint(&check.command)
                );
                continue;
            }
        };
        // Success needs positive evidence that tests ran and passed; an
        // overlapped check may have run on content no snapshot saw.
        let outcome = if check.overlapped
            || (outcome == EngramExecutionOutcome::Succeeded && !end.showed_passing_tests)
        {
            EngramExecutionOutcome::Unknown
        } else {
            outcome
        };
        resolved.push(EngramResolvedCheck {
            check,
            end,
            outcome,
            basis,
            toolchain: None,
        });
    }
    let excess = resolved.len().saturating_sub(ENGRAM_TURN_CHECK_LIMIT);
    resolved.drain(..excess);
    for kept in &mut resolved {
        kept.toolchain = kept.check.toolchain.wait_until(deadline).flatten();
    }
    resolved
}

/// Carries into `resolved` the overlap marks the live records of its checks
/// gained after they were copied for resolution: the snapshots were waited
/// for off the lock, and a write another session reported meanwhile marked
/// the live record, not the copy.
fn engram_merge_live_overlaps(live: &[EngramTurnCheck], resolved: &mut [EngramResolvedCheck]) {
    for resolved in resolved {
        if live.iter().any(|check| {
            check.overlapped
                && check.grant_id == resolved.check.grant_id
                && check.sequence == resolved.check.sequence
        }) {
            resolved.check.overlapped = true;
            resolved.outcome = EngramExecutionOutcome::Unknown;
        }
    }
}

/// Whether the session at `index` may write: a read-only delegation child
/// cannot.
fn engram_session_may_write(inner: &StateInner, index: usize) -> bool {
    let session_id = &inner.sessions[index].session.id;
    !inner.delegations.iter().any(|delegation| {
        &delegation.child_session_id == session_id
            && matches!(delegation.write_policy, DelegationWritePolicy::ReadOnly)
    })
}

/// At most this many running commands of one session keep the worktrees
/// they may write in (`running_command_worktrees`); past it the oldest is
/// forgotten. A command whose end its runtime never reports would otherwise
/// be kept until the session's next turn.
const ENGRAM_RUNNING_COMMAND_LIMIT: usize = 64;

/// The worktrees a command may write in, `None` for one TermAl cannot name:
/// that of each directory it may run in (`engram_command_directories`, `None`
/// for the workdir; none TermAl can name when it lost the runtime's shell),
/// and that of each directory its own `cd` leads to from there, a `cd` in the
/// script a shell wrapper runs (`bash -lc 'cd x && …'`) included; a
/// PowerShell wrapper that starts its script elsewhere (`-WorkingDirectory`)
/// may write in any. A command that writes elsewhere by path (`git -C`, a
/// redirection) is not seen.
/// Resolves on the file system, so never under the state lock.
fn engram_command_worktrees(
    workdir: &str,
    directories: Option<&[Option<String>]>,
    ran: Option<&str>,
) -> Vec<Option<String>> {
    let Some(directories) = directories else {
        return vec![None];
    };
    let places = directories
        .iter()
        .map(|directory| match directory {
            Some(directory) => FsPath::new(workdir).join(directory),
            None => PathBuf::from(workdir),
        })
        .collect::<Vec<_>>();
    let mut worktrees = places
        .iter()
        .map(|place| Some(engram_worktree_root(place)))
        .collect::<Vec<_>>();
    if let Some(ran) = ran {
        let (script, _) = engram_unwrap_shell_command(ran);
        let lines = if script == ran {
            vec![ran]
        } else {
            vec![ran, script.as_str()]
        };
        for line in lines {
            // A PowerShell wrapper told to start elsewhere may write there.
            if engram_wrapper_sets_directory(line) {
                worktrees.push(None);
            }
            match engram_shell_move(line) {
                EngramShellMove::Stays => {}
                EngramShellMove::To(to) => {
                    for place in &places {
                        let place = place.to_string_lossy();
                        worktrees.push(
                            (!engram_network_path(&place))
                                .then(|| engram_resolve_shell_move(Some(&place), &to))
                                .flatten()
                                .map(|moved| engram_worktree_root(FsPath::new(&moved))),
                        );
                    }
                }
                EngramShellMove::Lost => worktrees.push(None),
            }
        }
    }
    worktrees.sort();
    worktrees.dedup();
    worktrees
}

/// Records in `record` that its command `key` may write in `worktrees`,
/// beside any recorded for it already: a later report of where it runs does
/// not undo what it may have written where it ran before.
fn engram_note_command_worktrees(
    record: &mut SessionRecord,
    key: &str,
    worktrees: Vec<Option<String>>,
) {
    let running = &mut record.engram.running_command_worktrees;
    match running.iter_mut().find(|(running, _)| running == key) {
        Some((_, known)) => {
            for worktree in worktrees {
                if !known.contains(&worktree) {
                    known.push(worktree);
                }
            }
        }
        None => {
            if running.len() >= ENGRAM_RUNNING_COMMAND_LIMIT {
                running.remove(0);
            }
            running.push((key.to_owned(), worktrees));
        }
    }
}

/// The session at `index` starts a turn: the commands of its earlier turn are
/// over (a background one it launched then is no longer followed), and it
/// may now write under a check another session of its worktree has open.
fn engram_note_turn_started(inner: &mut StateInner, index: usize) {
    inner.sessions[index]
        .engram
        .running_command_worktrees
        .clear();
    engram_mark_checks_overlapped_by(inner, index);
}

/// The key of the worktree the session in `record` works in, as last resolved
/// for its current workdir (`workdir_worktree`), however long ago, without
/// touching the file system: overlap marking runs under the state lock, where
/// resolving a path on a slow or unreachable volume would stall every other
/// session. `None` when it was never resolved for this workdir; such a
/// worktree is taken to be the one a check ran in, which can only make the
/// check unknown.
fn engram_session_worktree(record: &SessionRecord) -> Option<String> {
    record
        .engram
        .workdir_worktree
        .as_ref()
        .filter(|(workdir, _)| *workdir == record.session.workdir)
        .map(|(_, key)| key.clone())
}

/// Keeps on `record` the key of the worktree `workdir` lies in, resolved off
/// the lock (`engram_worktree_root`), while that is still its workdir.
fn engram_note_session_worktree(record: &mut SessionRecord, workdir: &str, key: String) {
    if record.session.workdir == workdir {
        record.engram.workdir_worktree = Some((workdir.to_owned(), key));
    }
}

/// The worktrees the session at `index` may write in, `None` for one TermAl
/// could not name: its workdir's, as last resolved (`engram_session_worktree`),
/// and those of its running commands (`running_command_worktrees`), which may
/// run elsewhere: in a directory their runtime reports, or where their shell
/// was moved.
fn engram_writer_worktrees(inner: &StateInner, index: usize) -> Vec<Option<String>> {
    let record = &inner.sessions[index];
    std::iter::once(engram_session_worktree(record))
        .chain(
            record
                .engram
                .running_command_worktrees
                .iter()
                .flat_map(|(_, worktrees)| worktrees.iter().cloned()),
        )
        .collect()
}

/// Whether `worktrees` may hold the worktree with key `root`: one of them is
/// it, or one is a worktree TermAl could not name.
fn engram_worktrees_may_hold(worktrees: &[Option<String>], root: &str) -> bool {
    worktrees
        .iter()
        .any(|worktree| worktree.as_deref().is_none_or(|worktree| worktree == root))
}

/// Whether another session that may write is in a turn now (running,
/// awaiting approval or stopping) in the worktree with key `root`, where a
/// check of the session at `index` runs, or runs a command there wherever it
/// works (`engram_writer_worktrees`): its writes could land under the check
/// without the check's agent making them. Whatever such a session does later
/// while the check is open marks the check itself
/// (`engram_mark_checks_overlapped_by`). Runs under the state lock, on
/// worktrees resolved before; one never resolved counts as the same.
fn engram_other_writer_in(inner: &StateInner, index: usize, root: &str) -> bool {
    inner.sessions.iter().enumerate().any(|(other, record)| {
        other != index
            && matches!(
                record.session.status,
                SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
            )
            && engram_session_may_write(inner, other)
            && engram_worktrees_may_hold(&engram_writer_worktrees(inner, other), root)
    })
}

/// Marks every check of another session that is still open to writes, in a
/// worktree the session at `writer` may write in (`engram_writer_worktrees`),
/// as overlapped: the writer is about to write there, or just did, where the
/// check's snapshots may still read it. Called when the writer starts a turn
/// and whenever it reports a command or an edit, so an overlap is counted
/// while it can matter rather than reconstructed from a timestamp later. A
/// read-only delegation child cannot write. Runs under the state lock, on
/// worktrees resolved before; one never resolved marks every open check.
fn engram_mark_checks_overlapped_by(inner: &mut StateInner, writer: usize) {
    if engram_session_may_write(inner, writer) {
        let worktrees = engram_writer_worktrees(inner, writer);
        engram_mark_open_checks_in_worktrees(inner, &worktrees, Some(writer));
    }
}

/// Marks as overlapped every check still open to writes, but those of the
/// session at `except`, that ran in one of `worktrees` (every one, for a
/// worktree TermAl could not name). A check carries the worktree it ran in,
/// so this touches no file system.
fn engram_mark_open_checks_in_worktrees(
    inner: &mut StateInner,
    worktrees: &[Option<String>],
    except: Option<usize>,
) {
    for (other, record) in inner.sessions.iter_mut().enumerate() {
        if Some(other) == except {
            continue;
        }
        for check in &mut record.engram.active_turn_checks {
            if check.open_to_writes()
                && engram_worktrees_may_hold(worktrees, &engram_path_key(&check.target.root))
            {
                check.overlapped = true;
            }
        }
    }
}

impl EngramTurnCheck {
    /// Whether a write now could still reach what the check's snapshots see:
    /// the check is running, or one of its snapshots is still being taken.
    fn open_to_writes(&self) -> bool {
        !self.start_basis.is_ready()
            || self
                .end
                .as_ref()
                .is_none_or(|end| !end.end_basis.is_ready())
    }
}

/// Whether `record` holds a check of the grant it runs under that is still
/// open to writes. A grant can end without its report (a compensating close,
/// a project reset), leaving its checks until the next grant clears them;
/// they will never be reported, so they no longer count.
fn engram_has_open_check(record: &SessionRecord) -> bool {
    let engram = &record.engram;
    engram.active_turn_checks.iter().any(|check| {
        engram.active_grant_id.as_deref() == Some(check.grant_id.as_str()) && check.open_to_writes()
    })
}

impl AppState {
    /// A command started in `session_id`: `ran` is the command line the
    /// runtime says runs, when it says, and `cwd` where, when it says. While
    /// a mediated turn runs on claimed work, a recognised test of the
    /// session's own worktree (`engram_check_worktree`) starts a check, with
    /// its source snapshot taken at once on its own thread, and any command
    /// marks the checks still open to writes as overlapped: it may write
    /// under them. Outside such a turn there is nothing to report and
    /// nothing is kept.
    ///
    /// A runtime may start the same command more than once (ACP reports
    /// pending, then in progress), giving what it runs or where only in one
    /// of them: its key's last reported directory stands until another is
    /// reported, a start that gives no command line (a title alone) leaves
    /// the command as another start named it, one that gives a line names
    /// what runs now, and a check stands only while it still names the same
    /// test in the same place.
    ///
    /// A runtime that reports no directory (Claude) may run its commands in
    /// one shell that keeps a `cd`: such a command is judged from the workdir
    /// and from where the commands before it left that shell
    /// (`engram_command_directories`), a `cd` counting once its command has
    /// run; where TermAl could not follow them no check is kept.
    fn note_engram_command_started(
        &self,
        session_id: &str,
        key: &str,
        ran: Option<&str>,
        cwd: Option<&str>,
    ) {
        let recognised = ran.and_then(engram_check_command);
        // A runtime that reports no directory may run its commands in one
        // shell, which keeps a `cd` for the commands after it.
        let shell_move = match (cwd, ran) {
            (None, Some(ran)) => engram_shell_move(ran),
            _ => EngramShellMove::Stays,
        };
        // What this key's earlier starts said and where the command may run,
        // read under a brief lock, so the test's target can be resolved on
        // the file system off it.
        let (workdir, runtime, reported, places, position, started) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            let record = &inner.sessions[index];
            let engram = &record.engram;
            let mediated = engram.work_binding.is_some() && engram.active_grant_id.is_some();
            (
                record.session.workdir.clone(),
                record.runtime.runtime_token(),
                cwd.map(str::to_owned)
                    .or_else(|| engram.running_command_keys.get(key).cloned().flatten()),
                engram_command_directories(record, key, cwd),
                engram_shell_position(record, key),
                mediated.then(|| {
                    engram
                        .active_turn_checks
                        .iter()
                        .find(|check| check.key == key && check.end.is_none())
                        .map(|check| check.command.clone())
                }),
            )
        };
        // The session's worktree and those the command may write in, resolved
        // here off the lock, for overlap marking under it.
        let workdir_worktree = engram_worktree_root(FsPath::new(&workdir));
        let worktrees = engram_command_worktrees(&workdir, places.as_deref(), ran);
        let target = started.and_then(|started| {
            let command = match ran {
                Some(_) => recognised.clone()?,
                None => started?,
            };
            // Where the shell cannot be followed, no check can say where it ran.
            let target =
                engram_check_worktree_from(&command, FsPath::new(&workdir), places.as_ref()?)?;
            Some((workdir.clone(), command, target))
        });
        // Where the command's `cd` leads, from where its shell is presumed to
        // be; it moves the shell only once the command has run.
        let moving_to = match &shell_move {
            EngramShellMove::To(to) => engram_resolve_shell_move(position.as_deref(), to),
            _ => None,
        };
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        // Any session's command may write, under a check another session
        // still has open in a worktree the command runs in.
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        engram_note_session_worktree(record, &workdir, workdir_worktree);
        engram_note_command_worktrees(record, key, worktrees);
        engram_note_shell_move(record, key, &runtime, shell_move, moving_to);
        engram_mark_checks_overlapped_by(&mut inner, index);
        let engram = &inner.sessions[index].engram;
        if engram.work_binding.is_none() {
            return;
        }
        let Some(grant_id) = engram.active_grant_id.clone() else {
            return;
        };
        let other_writer = target.as_ref().is_some_and(|(_, _, target)| {
            engram_other_writer_in(&inner, index, &engram_path_key(&target.root))
        });
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let engram = &mut record.engram;
        let others_running = engram
            .running_command_keys
            .keys()
            .any(|running| running != key);
        let fresh = engram
            .running_command_keys
            .insert(key.to_owned(), reported)
            .is_none();
        if fresh {
            // A new command, even one reusing an earlier command's key, may
            // write under every check still open.
            for check in &mut engram.active_turn_checks {
                if check.open_to_writes() {
                    check.overlapped = true;
                }
            }
            engram.withheld_command_keys.remove(key);
        }
        let started = engram
            .active_turn_checks
            .iter()
            .position(|check| check.key == key && check.end.is_none());
        match started {
            Some(position) => {
                let check = &engram.active_turn_checks[position];
                if target.as_ref().is_some_and(|(workdir, command, target)| {
                    *workdir == record.session.workdir
                        && *command == check.command
                        && *target == check.target
                }) {
                    return;
                }
                // It names another test or another place now: the check it
                // started no longer says what ran where, and no new one can
                // for the rest of the command, since it would begin its
                // record after writes the old one saw.
                engram.active_turn_checks.remove(position);
                engram.withheld_command_keys.insert(key.to_owned());
                return;
            }
            // A repeated start that names no test adds nothing.
            None if !fresh && recognised.is_none() => return,
            None if engram.withheld_command_keys.contains(key) => return,
            None => {}
        }
        // A test named only by a later start of its command (an ACP title,
        // then an update) began before its first snapshot: whatever was
        // written in between may be in what it ran, so its outcome is
        // unknown.
        let named_late = !fresh;
        let Some((workdir, command, target)) = target else {
            return;
        };
        if record.session.workdir != workdir {
            // The session moved while the target was resolved.
            return;
        }
        // While the session's capture workers are at their limit, a new test
        // starts no check: its snapshots would add to work that has not
        // finished, whether or not the checks it serves are still kept.
        let workers = record.engram.capture_workers.clone();
        if workers.load(std::sync::atomic::Ordering::SeqCst) >= ENGRAM_CAPTURE_WORKER_LIMIT {
            return;
        }
        // Only the latest checks are reported, so a finished check past the
        // limit is dropped now rather than kept, with its snapshots, until
        // the checkpoint drops it. While every kept check still runs, a new
        // test starts none: each would take snapshots no report can hold.
        if record.engram.active_turn_checks.len() >= ENGRAM_TURN_CHECK_LIMIT {
            let Some(oldest) = record
                .engram
                .active_turn_checks
                .iter()
                .position(|check| check.end.is_some())
            else {
                return;
            };
            record.engram.active_turn_checks.remove(oldest);
        }
        let start_basis =
            engram_spawn_basis_capture(target.root.to_string_lossy().into_owned(), &workers);
        // The toolchain is named as the check starts, from the overrides it
        // ran under, not from whatever they say when the turn closes.
        let toolchain = if engram_cargo_toolchain_selector(&command).is_some() {
            engram_spawn_toolchain_capture(command.clone(), target.directory.clone(), &workers)
        } else {
            EngramToolchainCapture::settled(None)
        };
        let sandbox = record
            .active_codex_sandbox_mode
            .map(|mode| mode.as_cli_value().to_owned());
        let sequence = record.engram.next_turn_check_sequence;
        record.engram.next_turn_check_sequence += 1;
        record.engram.active_turn_checks.push(EngramTurnCheck {
            grant_id,
            key: key.to_owned(),
            sequence,
            command,
            target,
            toolchain,
            started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            sandbox,
            start_basis,
            overlapped: others_running || other_writer || named_late,
            end: None,
        });
    }

    /// The runtime of `session_id` said again what the running command `key`
    /// runs (`ran`) or where (`cwd`), without starting or ending it (an ACP
    /// update without a status, or the one that ends it). A `cd` the command
    /// line makes counts for the runtime's shell as at a start
    /// (`engram_note_shell_move`), whether or not a check is running. The
    /// key's directory is remembered, and a check it started stands only
    /// while it still names the same test in the same place. Nothing starts
    /// here: a check needs its first snapshot from before its test runs.
    fn note_engram_command_described(
        &self,
        session_id: &str,
        key: &str,
        ran: Option<&str>,
        cwd: Option<&str>,
    ) {
        if ran.is_none() && cwd.is_none() {
            return;
        }
        let shell_move = match (cwd, ran) {
            (None, Some(ran)) => engram_shell_move(ran),
            _ => EngramShellMove::Stays,
        };
        // The check the key started, where its shell is and where the command
        // may run, read under a brief lock, so what the description names can
        // be resolved on the file system off it.
        let (runtime, position, running, started) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            let record = &inner.sessions[index];
            let engram = &record.engram;
            (
                record.runtime.runtime_token(),
                engram_shell_position(record, key),
                engram
                    .running_command_worktrees
                    .iter()
                    .any(|(running, _)| running == key)
                    .then(|| {
                        (
                            record.session.workdir.clone(),
                            engram_command_directories(record, key, cwd),
                        )
                    }),
                engram
                    .running_command_keys
                    .contains_key(key)
                    .then(|| {
                        engram
                            .active_turn_checks
                            .iter()
                            .find(|check| check.key == key && check.end.is_none())
                            .map(|check| {
                                (
                                    check.sequence,
                                    check.command.clone(),
                                    check.target.clone(),
                                    record.session.workdir.clone(),
                                    engram_command_directories(record, key, cwd),
                                )
                            })
                    })
                    .flatten(),
            )
        };
        let moving_to = match &shell_move {
            EngramShellMove::To(to) => engram_resolve_shell_move(position.as_deref(), to),
            _ => None,
        };
        // Where the running command may write now, as it is described.
        let worktrees = running.map(|(workdir, directories)| {
            engram_command_worktrees(&workdir, directories.as_deref(), ran)
        });
        let stands = started
            .as_ref()
            .map(|(_, command, target, workdir, directories)| {
                let named = match ran {
                    Some(ran) => engram_check_command(ran),
                    None => Some(command.clone()),
                };
                named.is_some_and(|named| {
                    named == *command
                        && directories.as_ref().is_some_and(|directories| {
                            engram_check_worktree_from(&named, FsPath::new(workdir), directories)
                                .is_some_and(|now| now == *target)
                        })
                })
            });
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        engram_note_shell_move(record, key, &runtime, shell_move, moving_to);
        if let (Some(cwd), Some(remembered)) =
            (cwd, record.engram.running_command_keys.get_mut(key))
        {
            *remembered = Some(cwd.to_owned());
        }
        if let (Some((sequence, _, _, workdir, _)), Some(stands)) = (started, stands)
            && !(stands && workdir == record.session.workdir)
        {
            // It names another test or another place now: the check no
            // longer says what ran where, and no new one can for the rest
            // of the command.
            record.engram.active_turn_checks.retain(|check| {
                !(check.key == key && check.sequence == sequence && check.end.is_none())
            });
            record.engram.withheld_command_keys.insert(key.to_owned());
        }
        if let Some(worktrees) = worktrees
            && record
                .engram
                .running_command_worktrees
                .iter()
                .any(|(running, _)| running == key)
        {
            // It may write where it is now said to run, under a check
            // another session has open there (unless it ended meanwhile).
            engram_note_command_worktrees(record, key, worktrees);
            engram_mark_checks_overlapped_by(&mut inner, index);
        }
    }

    /// `command` finished in `session_id`. A running check ends with what the
    /// runtime said about its end, and its closing snapshot is taken on its
    /// own thread. `exit` is `None` from a runtime that says nothing usable.
    /// A background command's result marks its launch: it keeps counting as
    /// running for the rest of the turn, since TermAl never sees it end.
    fn note_engram_command_finished(
        &self,
        session_id: &str,
        key: &str,
        command: &str,
        output: &str,
        exit: Option<EngramCommandExit>,
    ) {
        // A test's output can be long, so its result lines are read before
        // the lock is taken, for the test the runtime says finished, and only
        // when a check of the command is running: most commands have none.
        let (checked, workdir) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            let record = &inner.sessions[index];
            (
                record
                    .engram
                    .active_turn_checks
                    .iter()
                    .any(|check| check.key == key && check.end.is_none()),
                record.session.workdir.clone(),
            )
        };
        // The session's worktree, resolved off the lock for overlap marking.
        let workdir_worktree = engram_worktree_root(FsPath::new(&workdir));
        let parsed = checked
            .then(|| engram_check_command(command))
            .flatten()
            .map(|finished| {
                let result_lines = engram_check_result_lines(&finished.program, output);
                let showed_passing_tests =
                    engram_check_showed_passing_tests(&finished, &result_lines);
                (finished, result_lines, showed_passing_tests)
            });
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        engram_note_session_worktree(
            inner
                .session_mut_by_index(index)
                .expect("session index should be valid"),
            &workdir,
            workdir_worktree,
        );
        engram_mark_checks_overlapped_by(&mut inner, index);
        // Only a check of the grant the session runs under can be reported;
        // one a grant left behind as it ended takes no closing snapshot.
        let grant_id = inner.sessions[index].engram.active_grant_id.clone();
        let running_check = |check: &EngramTurnCheck| {
            check.key == key
                && check.end.is_none()
                && grant_id.as_deref() == Some(check.grant_id.as_str())
        };
        // A session in a turn now was in one while the check ran. Its turn
        // start or its own reports marked the check already; this catches a
        // turn that began where no mark was made.
        let other_writer = inner.sessions[index]
            .engram
            .active_turn_checks
            .iter()
            .find(|check| running_check(check))
            .is_some_and(|check| {
                engram_other_writer_in(&inner, index, &engram_path_key(&check.target.root))
            });
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        // A `cd` the command made counts from now on, if it ran.
        engram_settle_shell_move(record, key, exit);
        if exit != Some(EngramCommandExit::NotFinished) {
            record.engram.running_command_keys.remove(key);
            record.engram.withheld_command_keys.remove(key);
            record
                .engram
                .running_command_worktrees
                .retain(|(running, _)| running != key);
        }
        let workers = record.engram.capture_workers.clone();
        let Some(check) = record
            .engram
            .active_turn_checks
            .iter_mut()
            .find(|check| running_check(check))
        else {
            return;
        };
        let (result_lines, showed_passing_tests) = match parsed {
            Some((finished, result_lines, showed_passing_tests)) if finished == check.command => {
                (result_lines, showed_passing_tests)
            }
            // The end names another command than the check's start (a reused
            // key): the check's own command decides how its output reads.
            _ => {
                let result_lines = engram_check_result_lines(&check.command.program, output);
                let showed_passing_tests =
                    engram_check_showed_passing_tests(&check.command, &result_lines);
                (result_lines, showed_passing_tests)
            }
        };
        check.overlapped |= other_writer;
        let end_basis = if exit == Some(EngramCommandExit::NotFinished) {
            // A background run's result marks its launch; it is never
            // reported, so it takes no closing snapshot.
            EngramBasisCapture::settled(None)
        } else {
            // The worktree the check ran in, as its opening snapshot was.
            engram_spawn_basis_capture(check.target.root.to_string_lossy().into_owned(), &workers)
        };
        check.end = Some(EngramTurnCheckEnd {
            completed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            exit: exit.unwrap_or(EngramCommandExit::Unknown),
            result_lines,
            showed_passing_tests,
            end_basis,
        });
    }

    /// A command in `session_id` will never report its end (Claude's Bash call
    /// was denied): it no longer counts as running beside later checks, a
    /// check it started is dropped, since it never ran and would otherwise
    /// stay open to writes until the next grant, and a `cd` it would have made
    /// moves nothing.
    fn note_engram_command_abandoned(&self, session_id: &str, key: &str) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(index) = inner.find_session_index(session_id) {
            let engram = &mut inner.sessions[index].engram;
            engram.running_command_keys.remove(key);
            engram.withheld_command_keys.remove(key);
            engram
                .running_command_worktrees
                .retain(|(running, _)| running != key);
            engram
                .active_turn_checks
                .retain(|check| check.key != key || check.end.is_some());
            if let Some(shell) = engram.shell_directory.as_mut()
                && shell
                    .pending
                    .as_ref()
                    .is_some_and(|(moving, _)| moving == key)
            {
                shell.pending = None;
            }
        }
    }

    /// The user wrote at `path` through TermAl itself (a file save, a Git
    /// file action, a terminal command starting or ending): a check still
    /// open to writes in any session of that worktree may read what changed.
    fn note_engram_host_write(&self, path: &FsPath) {
        // The written path's worktree is resolved off the lock, and only
        // while some check is open. A check that opens after this look is
        // caught by the mark each write takes as it ends.
        let any_open = self
            .inner
            .lock()
            .expect("state mutex poisoned")
            .sessions
            .iter()
            .any(engram_has_open_check);
        if !any_open {
            return;
        }
        let workspace = engram_worktree_root(path);
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        engram_mark_open_checks_in_worktrees(&mut inner, &[Some(workspace)], None);
    }

    /// Resolves off the state lock the worktree `session_id` works in and
    /// keeps it on its record (`workdir_worktree`), so the marks its turn
    /// start makes under the lock (`engram_note_turn_started`), and its count
    /// as a writer while the turn runs (`engram_other_writer_in`), name its
    /// own worktree rather than every one: a session that has reported no
    /// command or edit since TermAl started (a chat-only turn) would
    /// otherwise count as writing anywhere. Called before a turn starts.
    fn note_engram_session_worktree_off_lock(&self, session_id: &str) {
        let workdir = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            inner.sessions[index].session.workdir.clone()
        };
        let workdir_worktree = engram_worktree_root(FsPath::new(&workdir));
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        engram_note_session_worktree(
            inner
                .session_mut_by_index(index)
                .expect("session index should be valid"),
            &workdir,
            workdir_worktree,
        );
    }

    /// The agent reported a file edit in `session_id`: any check still open to
    /// writes may see content the edit changed, in this session or in
    /// another of the same worktree.
    fn note_engram_workspace_edit(&self, session_id: &str) {
        // The session's worktree, resolved off the lock for overlap marking.
        let workdir = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            inner.sessions[index].session.workdir.clone()
        };
        let workdir_worktree = engram_worktree_root(FsPath::new(&workdir));
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        engram_note_session_worktree(
            inner
                .session_mut_by_index(index)
                .expect("session index should be valid"),
            &workdir,
            workdir_worktree,
        );
        engram_mark_checks_overlapped_by(&mut inner, index);
        for check in &mut inner.sessions[index].engram.active_turn_checks {
            if check.open_to_writes() {
                check.overlapped = true;
            }
        }
    }
}

/// The turn's report in the order Engram's timing rules need. For each
/// reported check, in the order the checks started:
/// 1. when its revision differs from the last one reported (the turn's
///    begin-time basis at first), a change observation at that revision,
///    timed at the check's start, so the evidence that follows is at or
///    after the change it answers (only when the grant mediates local
///    mutation, which a change needs);
/// 2. its producer observation, `observe`, timed at its completion;
/// 3. its environment evidence, shared by checks on the same revision and
///    toolchain, at most four;
/// 4. its verification evidence, citing both.
///
/// Then the turn's own observation (`engram_turn_execution_observation`),
/// which after reported checks under a mutating grant reports a change only
/// if the source moved again after the last of them, so a change is never
/// recorded after the evidence that answers it unless it really came later.
/// Under a grant that mediates no local mutation it is judged against the
/// turn's begin-time basis, as without checks, and withheld when the source
/// changed.
///
/// Also returns, when the report carries checks, the report the turn would
/// make without them: its own observation alone, judged against its
/// begin-time basis as before evidence existed. Engram refuses a report
/// whole, and evidence adds ways to be refused (a redacted summary, a bounds
/// or basis mismatch), so a refused report falls back to that one rather
/// than to nothing, and the turn's change still opens its obligation.
fn engram_turn_report(
    record: &SessionRecord,
    session_id: &str,
    grant_id: &str,
    outcome: EngramExecutionOutcome,
    mutation_granted: bool,
    end_basis: Option<EngramExecutionSourceBasis>,
    checks: Vec<EngramResolvedCheck>,
) -> (EngramTurnReport, Option<EngramTurnReport>) {
    let mut report = EngramTurnReport::default();
    let intent_fingerprint = record
        .engram
        .active_turn_intent_fingerprint
        .clone()
        .unwrap_or_else(|| sha256_hex(format!("termal-turn-grant:{grant_id}").as_bytes()));
    let mut last_reported = record
        .engram
        .active_turn_start_basis
        .as_ref()
        .map(|basis| basis.source_revision.clone());
    let mut reported_check = false;
    // Environment records before this index were observed before the last
    // change reported, so a later check may not cite them: its evidence
    // must come at or after the change it answers.
    let mut environment_floor = 0;
    for resolved in checks {
        let EngramResolvedCheck {
            check,
            end,
            outcome: check_outcome,
            basis,
            toolchain,
            ..
        } = resolved;
        let check_id = format!("{session_id}:{grant_id}:{}:{}", check.sequence, check.key);
        if mutation_granted && last_reported.as_deref() != Some(basis.source_revision.as_str()) {
            report.observations.push(EngramExecutionObservationInput {
                observation_id: sha256_hex(format!("termal-turn-change:{check_id}").as_bytes()),
                action_fingerprint: intent_fingerprint.clone(),
                effect: EngramEffect::MutateLocal,
                outcome: EngramExecutionOutcome::Succeeded,
                source_changed: true,
                source_basis: Some(basis.clone()),
                observed_at: Some(check.started_at.clone()),
            });
            environment_floor = report.environment_evidence.len();
        }
        last_reported = Some(basis.source_revision.clone());
        reported_check = true;
        let producer_id = sha256_hex(format!("termal-turn-check:{check_id}").as_bytes());
        report.observations.push(EngramExecutionObservationInput {
            observation_id: producer_id.clone(),
            action_fingerprint: engram_check_fingerprint(&check.command),
            effect: EngramEffect::Observe,
            outcome: check_outcome,
            source_changed: false,
            source_basis: Some(basis.clone()),
            observed_at: Some(end.completed_at.clone()),
        });
        let environment = toolchain
            .and_then(|toolchain| {
                engram_environment_components(
                    &toolchain,
                    check.sandbox.as_deref(),
                    &basis.workspace_id,
                )
            })
            .and_then(|components| {
                let environment_fingerprint = engram_environment_fingerprint(&components);
                report
                    .environment_evidence
                    .iter()
                    .enumerate()
                    .skip(environment_floor)
                    .find(|(_, existing)| {
                        existing.environment_fingerprint == environment_fingerprint
                            && existing.source_basis == basis
                    })
                    .map(|(index, _)| index)
                    .or_else(|| {
                        (report.environment_evidence.len() < ENGRAM_TURN_ENVIRONMENT_LIMIT).then(
                            || {
                                report
                                    .environment_evidence
                                    .push(EngramEnvironmentEvidenceInput {
                                        source_basis: basis.clone(),
                                        environment_fingerprint,
                                        components: Some(components),
                                        observed_at: end.completed_at.clone(),
                                    });
                                report.environment_evidence.len() - 1
                            },
                        )
                    })
            });
        report
            .verification_evidence
            .push(EngramVerificationEvidenceInput {
                producer_observation: EngramObservationReference::ObservationId {
                    observation_id: producer_id,
                },
                check_kind: EngramVerificationKind::Test,
                environment: environment.map(|index| EngramEnvironmentReference::Index { index }),
                summary: Some(engram_check_summary(
                    &check.command,
                    end.exit,
                    &end.result_lines,
                )),
                refs: engram_check_refs(&check.command, end.exit),
            });
    }
    let own_observation = |reported_revision: Option<String>| {
        engram_turn_execution_observation(
            record,
            session_id,
            grant_id,
            outcome,
            mutation_granted,
            end_basis.clone(),
            reported_revision,
        )
    };
    // Only under a grant that mediates local mutation did the checks report
    // the changes before them; otherwise the turn's own observation is
    // judged against its begin-time basis, which withholds it when the
    // source changed, as without checks.
    let reported_revision = (mutation_granted && reported_check)
        .then_some(last_reported)
        .flatten();
    let judged_alone = reported_revision.is_none();
    let own = own_observation(reported_revision);
    report.observations.extend(own.clone());
    // Without its own observation (withheld), the turn has nothing to fall
    // back to. One judged against the begin-time basis already is the
    // fallback's, judged (and any withholding logged) once.
    let fallback = reported_check
        .then(|| EngramTurnReport {
            observations: if judged_alone {
                own
            } else {
                own_observation(None)
            }
            .into_iter()
            .collect(),
            ..EngramTurnReport::default()
        })
        .filter(|fallback| !fallback.is_empty());
    (report, fallback)
}
