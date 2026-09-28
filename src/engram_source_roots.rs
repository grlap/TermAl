// The work's source root: the worktree an agent names once for a claimed Engram
// item, which TermAl then measures instead of the session's workdir for every
// source basis of a turn on that claim, for the credit of its tests, and as the
// acceptance evaluator's cwd and declared fingerprint. Owns the persisted
// entries (`EngramWorkSourceRoot`), their validation (a registered worktree of
// the caller's repository inside its project folder), the basis taken on
// exactly the stored path and never on an ancestor's, the lookup by claim, the
// rules that end an entry, and the naming request behind the
// `termal_name_source_root` tool. Does not own when a basis is taken
// (`engram_turn_observations.rs`, `engram_turn_checks.rs`,
// `acceptance_evaluation_api.rs`), the held-claims read
// (`engram_held_claims.rs`) or the MCP tool definition (`delegation_mcp.rs`).
// New file.

/// At most this many named roots are kept host-wide. A full list refuses a
/// new name rather than evict one: an evicted entry would silently send its
/// work back to the session's workdir.
const ENGRAM_WORK_SOURCE_ROOT_LIMIT: usize = 64;

/// One work's source root, persisted so a restart keeps it. It applies to the
/// work's claim `claim_id` only, so a new claim on the same work does not
/// inherit an old tree; `claim_fence` is recorded, not matched, so a renewal
/// or a revision of the claim keeps the root.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramWorkSourceRoot {
    store: EngramAuthorityStoreKey,
    work_id: String,
    short_ref: String,
    claim_id: String,
    claim_fence: i64,
    /// The canonical path as `ReviewFreezeGit` returns it (on Windows with
    /// its verbatim prefix), which is also the basis's `workspace_id`.
    root: String,
    /// `engram_exact_path_key` of the repository's common Git directory,
    /// compared exactly: on a case-sensitive volume two repositories may
    /// differ only in case.
    common_dir_key: String,
    named_by_session: String,
    named_at: String,
    /// Unique host-wide to each new name (`StateInner::
    /// engram_source_root_generation`): naming the same root again under the
    /// same claim keeps it, while another root, another claim or a name after
    /// a clear gets a new one. A turn and an evaluation record the one they
    /// were admitted or requested under.
    generation: u64,
}

/// How long naming a source root may take in all, on the server: the held
/// reads, the full list's reclaim and the captures share it, and what has not
/// started when it runs out is skipped, conservatively. The MCP bridge waits
/// this long on top of its normal request timeout, so a name is never kept
/// after the tool call reported a failure.
const ENGRAM_SOURCE_ROOT_NAMING_BUDGET: Duration = Duration::from_secs(40);

/// The part of the naming budget the captures leave for the commit under the
/// lock, so a slow capture reports the new root unmeasured (or seals no
/// revision) rather than spending the budget and refusing a valid name.
const ENGRAM_SOURCE_ROOT_COMMIT_RESERVE: Duration = Duration::from_secs(2);

/// How long one capture of a naming call may take, with `left` of its budget
/// remaining: the freeze budget, stopping `ENGRAM_SOURCE_ROOT_COMMIT_RESERVE`
/// short of the end.
fn engram_source_root_capture_budget(left: Duration) -> Duration {
    REVIEW_FREEZE_TIMEOUT.min(left.saturating_sub(ENGRAM_SOURCE_ROOT_COMMIT_RESERVE))
}

#[cfg(test)]
thread_local! {
    /// Run once on this test thread right after a naming call's held-claims
    /// read (`AppState::name_engram_source_root`), so a test can land a name
    /// between that read and the call's commit.
    static TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// At most this many host lines about source roots wait for the agent's
/// next prompt; older ones give way.
const ENGRAM_SOURCE_ROOT_PENDING_LINES: usize = 4;

/// How every host line about a recognised test that got no check begins
/// (`engram_source_root_withheld_line`, `engram_source_root_unconfirmed_line`):
/// a new one replaces an earlier one still pending
/// (`EngramSessionState::set_pending_source_root_line`).
const ENGRAM_UNCREDITED_TEST_LINE_PREFIX: &str = "[TermAl] A recognised test";

impl EngramSessionState {
    /// Adds a host line for the agent's next prompt about where its turns on
    /// claimed work are measured. Lines not yet delivered are kept, so a
    /// later one (a withheld test) does not hide an earlier one (a bind); a
    /// line already pending is not repeated but moves to the newest place,
    /// so the last line always tells the latest state (name, clear, name
    /// again ends on the name); only the newest
    /// `ENGRAM_SOURCE_ROOT_PENDING_LINES` are kept. A line given again after
    /// a prompt carrying it was built is a new line: that prompt's
    /// acceptance does not take it, or an older line would end the next
    /// prompt's lines. The lines are kept one per text line, so a line break
    /// inside one (a path may hold one on Linux and macOS) becomes a space.
    /// A line about a test that got no check replaces an earlier one still
    /// pending, so a run of such tests never evicts a bind or name line.
    fn set_pending_source_root_line(&mut self, line: String) {
        let line = line.replace(['\r', '\n'], " ");
        let mut lines = self
            .pending_source_root_line
            .as_deref()
            .map(|pending| pending.lines().map(str::to_owned).collect::<Vec<_>>())
            .unwrap_or_default();
        let uncredited = line.starts_with(ENGRAM_UNCREDITED_TEST_LINE_PREFIX);
        lines.retain(|pending| {
            *pending != line
                && !(uncredited && pending.starts_with(ENGRAM_UNCREDITED_TEST_LINE_PREFIX))
        });
        if let Some((delivered, _)) = self.source_root_line_delivery.as_mut() {
            *delivered = delivered
                .lines()
                .filter(|delivered| *delivered != line)
                .collect::<Vec<_>>()
                .join("\n");
        }
        lines.push(line);
        let excess = lines.len().saturating_sub(ENGRAM_SOURCE_ROOT_PENDING_LINES);
        lines.drain(..excess);
        self.pending_source_root_line = Some(lines.join("\n"));
    }

    /// Drops the source-root lines waiting for the agent's next prompt, and
    /// any delivery of them in flight, once Engram disables the session: its
    /// turns are measured nowhere from then on, so the lines would tell of
    /// measurements that no longer happen. A project's settings change
    /// resets the whole record instead.
    fn drop_pending_source_root_lines(&mut self) {
        self.pending_source_root_line = None;
        self.source_root_line_delivery = None;
    }
}

/// At most this many held-claims reads of other sessions run at once when a
/// full list is reclaimed.
const ENGRAM_SOURCE_ROOT_RECLAIM_READERS: usize = 8;

/// Where a source basis is taken: the session's workdir, as before phase 2,
/// or a work's named root, on exactly that path.
#[derive(Clone, Debug, PartialEq, Eq)]
enum EngramBasisPlace {
    Workdir(String),
    Named {
        root: String,
        common_dir_key: String,
    },
}

/// The root a turn was admitted with, kept on the session record while that
/// turn runs. Runtime only: a turn does not survive a restart.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramTurnSourceRoot {
    root: String,
    common_dir_key: String,
    short_ref: String,
    claim_id: String,
    /// The root's revision when a rename or a clear during this turn sealed
    /// it. The close uses it only when the root is gone
    /// (`engram_named_root_absent`); it knows of no edit made after the seal.
    sealed_revision: Option<String>,
}

impl EngramTurnSourceRoot {
    fn from_entry(entry: &EngramWorkSourceRoot) -> Self {
        Self {
            root: entry.root.clone(),
            common_dir_key: entry.common_dir_key.clone(),
            short_ref: entry.short_ref.clone(),
            claim_id: entry.claim_id.clone(),
            sealed_revision: None,
        }
    }

    fn place(&self) -> EngramBasisPlace {
        EngramBasisPlace::Named {
            root: self.root.clone(),
            common_dir_key: self.common_dir_key.clone(),
        }
    }
}

/// The named source root an acceptance evaluation was requested on, recorded
/// on its target: the evaluator's cwd and fingerprint were taken there, and
/// its first submission is refused if the work's root has changed since
/// (another claim, another generation, another path).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceEvaluationSourceRoot {
    root: String,
    common_dir_key: String,
    work_id: String,
    claim_id: String,
    generation: u64,
}

impl AcceptanceEvaluationSourceRoot {
    fn from_entry(entry: &EngramWorkSourceRoot) -> Self {
        Self {
            root: entry.root.clone(),
            common_dir_key: entry.common_dir_key.clone(),
            work_id: entry.work_id.clone(),
            claim_id: entry.claim_id.clone(),
            generation: entry.generation,
        }
    }

    fn place(&self) -> EngramBasisPlace {
        EngramBasisPlace::Named {
            root: self.root.clone(),
            common_dir_key: self.common_dir_key.clone(),
        }
    }

    /// Whether the work's entry in `store` still names this root, under the
    /// same claim and generation.
    fn still_named(
        &self,
        entries: &[EngramWorkSourceRoot],
        store: &EngramAuthorityStoreKey,
    ) -> bool {
        engram_work_source_root_for_claim(entries, store, &self.work_id, &self.claim_id)
            .is_some_and(|entry| entry.generation == self.generation && entry.root == self.root)
    }
}

/// The claim the requesting session was bound to when an evaluation was
/// requested with no root named, recorded on its target: the claim, not the
/// session's binding at submission, decides whether a root was named since.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceEvaluationSourceClaim {
    work_id: String,
    claim_id: String,
}

impl AcceptanceEvaluationSourceClaim {
    fn from_binding(binding: &EngramControlWorkBinding) -> Self {
        Self {
            work_id: binding.work_id.clone(),
            claim_id: binding.claim_id.clone(),
        }
    }
}

/// Whether the root the evaluation `target` was requested on is no longer
/// the one its work is measured in: a recorded root the work no longer names
/// under its claim and generation, or, with none recorded, a root named since
/// for the claim it was requested under (`source_claim`) on the evaluated
/// work, which moved the work's measurements away from the tree evaluated.
fn acceptance_evaluation_root_changed_locked(
    inner: &StateInner,
    target: &DelegationAcceptanceEvaluation,
) -> bool {
    let Some(store) = &target.store else {
        return false;
    };
    match (&target.source_root, &target.source_claim) {
        (Some(source_root), _) => !source_root.still_named(&inner.engram_work_source_roots, store),
        (None, Some(claim)) => acceptance_evaluation_claim_named_since(
            &inner.engram_work_source_roots,
            store,
            claim,
            &target.work_ref,
        ),
        (None, None) => false,
    }
}

/// Whether a root is now named for `claim`, the claim an evaluation with no
/// root was requested under, on the evaluated work `work_ref`: the work's
/// measurements have moved away from the workdir it was evaluated in. The
/// recorded claim decides, whatever the requesting session is bound to now.
fn acceptance_evaluation_claim_named_since(
    entries: &[EngramWorkSourceRoot],
    store: &EngramAuthorityStoreKey,
    claim: &AcceptanceEvaluationSourceClaim,
    work_ref: &str,
) -> bool {
    engram_work_source_root_for_claim(entries, store, &claim.work_id, &claim.claim_id)
        .is_some_and(|entry| entry.short_ref == work_ref || entry.work_id == work_ref)
}

/// The refusal of an evaluation's first submission once the root it was
/// requested on (`None`: the session's workdir) is no longer the one its
/// work is measured in. The evaluator is told to treat it as final; the host
/// enforces that only for a recorded root, whose generation never returns
/// (with none recorded, a clear of the first name would admit a retry).
fn acceptance_evaluation_source_root_changed_error(
    source_root: Option<&AcceptanceEvaluationSourceRoot>,
) -> ApiError {
    let requested_on = match source_root {
        Some(source_root) => format!(
            "the evaluation was requested on {} (generation {}), which the work no longer names \
             under that claim",
            source_root.root, source_root.generation
        ),
        None => "the evaluation was requested on the session's workdir, and the work has since \
                 been given a named source root"
            .to_owned(),
    };
    ApiError::conflict(format!(
        "the work's source root changed while it was evaluated: {requested_on}, so these \
         verdicts may not describe its tree. Treat this as final, even if the root changes \
         back: do not submit again. Finish and report this; the parent must request a new \
         evaluation"
    ))
}

/// The source root of the work an acceptance evaluation of `work_ref` (with
/// `work_id` when the tracker's receipt carried it) judges, as the requesting
/// session is bound to it: the entry of that session's bound claim, when the
/// claim is on this work. A session bound to another claim, or to none,
/// measures its workdir.
fn engram_evaluation_source_root(
    entries: &[EngramWorkSourceRoot],
    store: &EngramAuthorityStoreKey,
    binding: Option<&EngramControlWorkBinding>,
    work_ref: &str,
    work_id: Option<&str>,
) -> Option<AcceptanceEvaluationSourceRoot> {
    let binding = binding?;
    engram_work_source_root_for_claim(entries, store, &binding.work_id, &binding.claim_id)
        .filter(|entry| {
            work_id.map_or(entry.short_ref == work_ref, |work_id| entry.work_id == work_id)
        })
        .map(AcceptanceEvaluationSourceRoot::from_entry)
}

/// The one host line an agent gets, before its next prompt, about where its
/// turns on claimed work are measured: at a newly bound claim, after it names
/// or clears a root, and once a recognised test started outside the turn's root
/// (`engram_source_root_withheld_line`). `work` names the work when no entry
/// does; `one_call` says whether the session is told the one-call form up
/// front (`engram_one_call_offered_up_front`).
fn engram_source_root_line(
    entry: Option<&EngramWorkSourceRoot>,
    work: &str,
    workdir: &str,
    one_call: bool,
) -> String {
    match entry {
        Some(entry) => {
            let root = engram_source_root_display(&entry.root);
            // The one-call form is named only to a session whose runtime
            // reports no directory and runs the bare line (`one_call`,
            // `engram_one_call_offered_up_front`): one that reports a
            // directory is credited where it says it runs, and one that
            // wraps its lines could not run the form. And only for a root
            // the form reads (`engram_one_call_reads`: one holding `$`, `(`
            // or a network prefix would not count).
            let one_call = (one_call && engram_one_call_reads(&root))
                .then(|| {
                    format!(
                        ": start each in one call as `pushd \"DIR\" && <test>`, with DIR the \
                         absolute directory in it where the test \
                         runs{ENGRAM_ONE_CALL_DIR_SPELLING} and nothing piped, redirected or \
                         chained after the test, because a directory changed in an earlier call \
                         may not hold"
                    )
                })
                .unwrap_or_default();
            format!(
                "[TermAl] Engram source basis for {}: its named source root {root}. Tests count \
                 for it only when they run there{one_call}.",
                entry.short_ref
            )
        }
        None => format!(
            "[TermAl] Engram source basis for {work}: this session's workdir {workdir} (no \
             worktree named). If you do this item's work in another worktree, name it once with \
             termal_name_source_root."
        ),
    }
}

/// The host line a bind leaves for the agent's next prompt when it binds a
/// claim the session was not bound to (`current`), with the entry of that
/// claim in `store` when there is one; `None` for a bind that keeps the
/// claim, or for a delegated session (`child`), which names no root and is
/// measured in its workdir. The binding carries no short reference, so
/// without an entry the line names the work by its id. `one_call` is
/// `engram_source_root_line`'s.
fn engram_bind_source_root_line(
    child: bool,
    entries: &[EngramWorkSourceRoot],
    store: Option<&EngramAuthorityStoreKey>,
    current: Option<&EngramControlWorkBinding>,
    bound: &EngramControlWorkBinding,
    workdir: &str,
    one_call: bool,
) -> Option<String> {
    if child || current.is_some_and(|current| current.claim_id == bound.claim_id) {
        return None;
    }
    let entry = store.and_then(|store| {
        engram_work_source_root_for_claim(entries, store, &bound.work_id, &bound.claim_id)
    });
    Some(engram_source_root_line(
        entry,
        &format!("work {}", bound.work_id),
        workdir,
        one_call,
    ))
}

/// Where a bound turn's source basis was taken: the work's named source root,
/// or, with no `root`, the session's workdir because no worktree is named.
/// Built from the root the turn was admitted with; a bound turn whose
/// begin-time capture never ran shows the workdir even if an entry exists.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramControlSourceRootCard {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    work_ref: Option<String>,
}

impl EngramControlSourceRootCard {
    fn for_turn(turn_root: Option<&EngramTurnSourceRoot>) -> Self {
        Self {
            root: turn_root.map(|turn_root| engram_source_root_display(&turn_root.root)),
            work_ref: turn_root.map(|turn_root| turn_root.short_ref.clone()),
        }
    }
}

/// The one-call form of `test` as a remedy clause, with the directory left to
/// the agent (`engram_one_call_template` in src/engram_one_call.rs). The test
/// stands alone: one with anything after it is no one-call line, and its exit
/// status could not say it passed. On Windows DIR is written as Windows names
/// it (`ENGRAM_ONE_CALL_DIR_SPELLING`).
fn engram_one_call_clause(test: &str, measured_in: &str) -> String {
    format!(
        "in one call as `pushd \"DIR\" && {test}`, with DIR the absolute directory in \
         {measured_in} where it runs{ENGRAM_ONE_CALL_DIR_SPELLING} and nothing piped, redirected \
         or chained after the test"
    )
}

/// How DIR is to be written in the one-call form, said wherever the form is
/// named: on Windows as Windows names it, since Git Bash's spelling of a drive
/// (`/c/…`), which Claude's Bash tool favours, is one the form does not take
/// (`engram_one_call_git_bash_dir`); nothing on other systems.
const ENGRAM_ONE_CALL_DIR_SPELLING: &str = if cfg!(windows) {
    ", written as a Windows path (C:\\… or C:/…, not Git Bash's /c/…),"
} else {
    ""
};

/// The host line once a recognised test of a turn started in `ran_in`,
/// outside the worktree the turn is measured in (`measured_in`), and so gets
/// no credit whether or not it finishes: that is when a missing name costs
/// something. `template` is the test to name in the one-call form, when the
/// form could carry it (`engram_one_call_template`). Left for a test that gets
/// no check by `engram_uncredited_test_line` in src/engram_one_call.rs.
fn engram_source_root_withheld_line(
    ran_in: &FsPath,
    measured_in: &str,
    named: bool,
    child: bool,
    template: Option<&str>,
) -> String {
    let measured_in = engram_source_root_display(measured_in);
    // A delegated session names no root, so it is told where to run them.
    let remedy = if child {
        "run the tests in your workdir, where your turns are measured".to_owned()
    } else if named {
        match template.map(|test| engram_one_call_clause(test, &measured_in)) {
            Some(clause) => format!(
                "run the tests in that root, {clause}, or name the worktree you work in with \
                 termal_name_source_root"
            ),
            None => "run the tests in that root, or name the worktree you work in with \
                     termal_name_source_root"
                .to_owned(),
        }
    } else {
        "name that worktree once with termal_name_source_root so its tests count".to_owned()
    };
    format!(
        "[TermAl] A recognised test started in {}, outside the worktree this turn is measured in \
         ({measured_in}), and gets no credit: {remedy}.",
        engram_source_root_display(&ran_in.to_string_lossy()),
    )
}

/// Why TermAl cannot confirm that a recognised test ran in the worktree its
/// turn is measured in (`engram_source_root_unconfirmed_line`).
enum EngramUnconfirmedReason<'place> {
    /// The runtime reports no directory, and its shell may be back in this
    /// place (the session's workdir) between calls.
    ShellMayReturn(&'place FsPath),
    /// TermAl cannot place the shell: a directory change it cannot follow,
    /// or another command still changing it.
    ShellUnknown,
    /// The test ran after another command on its line (`A && TEST`), which
    /// TermAl does not read as a shell would (`engram_embedded_test`).
    InsideLine,
    /// The line starts as the one-call form, but with more than one test
    /// alone after its `pushd`: something before or after the test, or a
    /// shell of its own around it.
    OneCallTestNotAlone,
    /// The line starts as the one-call form on Windows but writes its DIR in
    /// Git Bash's spelling of a drive (`/c/…`), which the form does not take
    /// (`engram_one_call_git_bash_dir`); `test_alone`: whether that is all,
    /// or the line also holds more than its one test alone after its `pushd`.
    OneCallGitBashDir { test_alone: bool },
}

/// What TermAl knows of a session and its test that decides which remedy
/// can make the test count (`engram_source_root_unconfirmed_line`); the
/// fourth fact, whether TermAl follows a `cd` into the worktree, it reads
/// from the worktree's path.
#[derive(Clone, Copy, Debug)]
struct EngramRemedyFacts<'test> {
    /// The test, when the one-call form can carry it
    /// (`engram_one_call_template`).
    template: Option<&'test str>,
    /// The runtime reported the directory this command runs in: it starts
    /// each command where it says, so a `cd` in a call of its own does not
    /// carry over.
    reports_directory: bool,
    /// The session's workdir lies in the worktree the turn is measured in, so
    /// a shell that reports no directory counts there once TermAl places it
    /// there too.
    workdir_inside: bool,
}

/// The host line once a recognised test of a turn gets no credit because
/// TermAl cannot confirm it ran in the worktree the turn is measured in
/// (`measured_in`), though its shell was presumed there (`reason`). The
/// remedy follows from four facts (`facts`), so that each is one the session
/// can carry out:
/// - the one-call form, when it can carry the test: it counts wherever the
///   shell is;
/// - else, for a runtime that reports its directory, the test as a line of
///   its own with its working directory where it should run;
/// - else, for a session whose workdir is in the worktree, a `cd` to the
///   directory where the test runs in a call of its own, which TermAl
///   follows, then the test as a line of its own; or a new session, where
///   TermAl follows no `cd` into the worktree's path;
/// - else a session whose workdir is in the worktree.
///
/// A test run after another command on its line may have been meant for
/// another project, so its remedy is offered only for the case it was meant
/// here. Which line a test that gets no check leaves, and with which
/// `reason`, is decided by `engram_uncredited_test_line` in
/// src/engram_one_call.rs.
fn engram_source_root_unconfirmed_line(
    measured_in: &str,
    reason: EngramUnconfirmedReason<'_>,
    facts: EngramRemedyFacts<'_>,
) -> String {
    let measured_in = engram_source_root_display(measured_in);
    let lead = if matches!(
        reason,
        EngramUnconfirmedReason::InsideLine
            | EngramUnconfirmedReason::OneCallTestNotAlone
            | EngramUnconfirmedReason::OneCallGitBashDir { .. }
    ) {
        "If it was meant to count here, run"
    } else {
        "Run"
    };
    let reason = match reason {
        EngramUnconfirmedReason::ShellMayReturn(place) => format!(
            "this runtime reports no directory and its shell may return to {} between calls",
            engram_source_root_display(&place.to_string_lossy())
        ),
        EngramUnconfirmedReason::ShellUnknown => "TermAl cannot tell where its shell is: a \
             directory change it cannot follow, or another command still changing it"
            .to_owned(),
        EngramUnconfirmedReason::InsideLine => {
            "it ran after another command on its line, which TermAl cannot place".to_owned()
        }
        EngramUnconfirmedReason::OneCallTestNotAlone => "its line starts as the one-call form, \
             but the form takes only one test after its `pushd`, alone, with nothing before or \
             after it and no shell of its own"
            .to_owned(),
        EngramUnconfirmedReason::OneCallGitBashDir { test_alone: true } => "its line is the \
             one-call form but for its DIR, written in Git Bash's spelling of a drive (/c/…), \
             which the form does not take, since not every shell reads it as that drive \
             (PowerShell reads it as \\c\\… on the current drive)"
            .to_owned(),
        EngramUnconfirmedReason::OneCallGitBashDir { test_alone: false } => "its line starts \
             as the one-call form, but with its DIR in Git Bash's spelling of a drive (/c/…), \
             which not every shell reads as that drive (PowerShell reads it as \\c\\… on the \
             current drive), and with more than its one test alone after its `pushd`: the form \
             takes neither"
            .to_owned(),
    };
    let remedy = if let Some(test) = facts.template {
        format!("{lead} it {}", engram_one_call_clause(test, &measured_in))
    } else if facts.reports_directory {
        format!(
            "{lead} it as a line of its own, with its working directory set to the directory in \
             {measured_in} where it runs"
        )
    } else if facts.workdir_inside {
        // The shell counts once TermAl places it where the test runs, which
        // a `cd` to an absolute directory it follows does
        // (`engram_shell_move`), whether TermAl lost the shell or not.
        if matches!(
            engram_shell_move(&format!("cd \"{measured_in}\"")),
            EngramShellMove::To(_)
        ) {
            format!(
                "{lead} `cd \"DIR\"` in a call of its own, with DIR the absolute directory in \
                 {measured_in} where it runs, then run it as a line of its own"
            )
        } else {
            format!(
                "{lead} it from a new session whose workdir is in {measured_in}, since TermAl \
                 cannot follow a directory change into that path"
            )
        }
    } else {
        format!("{lead} it from a session whose workdir is in {measured_in}")
    };
    format!(
        "[TermAl] A recognised test gets no credit: TermAl cannot confirm it ran in \
         {measured_in}, where this turn is measured, because {reason}. {remedy}."
    )
}

/// `root` as a delegation takes a cwd: the canonical path without the
/// Windows verbatim prefix `fs::canonicalize` adds.
fn engram_source_root_display(root: &str) -> String {
    match root.strip_prefix(r"\\?\UNC\") {
        Some(share) => format!(r"\\{share}"),
        None => root.strip_prefix(r"\\?\").unwrap_or(root).to_owned(),
    }
}

/// The common Git directory of the worktree whose root is exactly `root`,
/// canonical, read from its own `.git` entry only: a `.git` directory, or a
/// `.git` file naming a linked worktree's directory and its `commondir`. It
/// never looks at an ancestor, so a root whose `.git` is gone does not
/// resolve to the main checkout above it.
fn engram_source_root_common_dir(root: &FsPath) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    let metadata = fs::symlink_metadata(&dot_git).ok()?;
    if metadata.is_dir() {
        return fs::canonicalize(&dot_git).ok();
    }
    if !metadata.is_file() {
        return None;
    }
    let text = fs::read_to_string(&dot_git).ok()?;
    let own = root.join(text.trim().strip_prefix("gitdir:")?.trim());
    // A linked worktree's directory names the common directory; one without
    // `commondir` is not a linked worktree (a submodule's, say).
    let commondir = fs::read_to_string(own.join("commondir")).ok()?;
    fs::canonicalize(own.join(commondir.trim())).ok()
}

/// Whether the worktree at `root` is registered with the repository whose
/// common directory is `common`: it is the main worktree, whose `.git`
/// directory is `common`, or a linked one whose own directory is
/// `<common>/worktrees/<name>` and names `<root>/.git` back in its `gitdir`
/// file. A copied `.git` file, or one whose entry was pruned, fails.
fn engram_source_root_registered(root: &FsPath, common: &FsPath) -> bool {
    let dot_git = root.join(".git");
    let Ok(metadata) = fs::symlink_metadata(&dot_git) else {
        return false;
    };
    if metadata.is_dir() {
        return fs::canonicalize(&dot_git).is_ok_and(|dot_git| dot_git == common);
    }
    let Some(own) = fs::read_to_string(&dot_git)
        .ok()
        .and_then(|text| {
            text.trim()
                .strip_prefix("gitdir:")
                .map(|gitdir| root.join(gitdir.trim()))
        })
        .and_then(|own| fs::canonicalize(own).ok())
    else {
        return false;
    };
    let under_worktrees = own.parent().is_some_and(|parent| {
        parent.file_name().is_some_and(|name| name == "worktrees") && parent.parent() == Some(common)
    });
    if !under_worktrees {
        return false;
    }
    let Ok(back) = fs::read_to_string(own.join("gitdir")) else {
        return false;
    };
    let back = PathBuf::from(back.trim());
    let back = if back.is_absolute() {
        back
    } else {
        own.join(back)
    };
    matches!(
        (fs::canonicalize(back), fs::canonicalize(&dot_git)),
        (Ok(back), Ok(dot_git)) if back == dot_git
    )
}

/// Whether the named root is gone: its path no longer exists, or it no
/// longer holds its own `.git` entry, so it is no longer a worktree root.
/// Only then may a turn's close use a sealed revision. Only a lookup that
/// says so counts (`engram_lookup_says_missing`): a denied or failed one
/// leaves a root that may still exist, and changed, unmeasured.
fn engram_named_root_absent(root: &str) -> bool {
    let path = FsPath::new(root);
    match fs::symlink_metadata(path) {
        Err(error) => engram_lookup_says_missing(error.kind()),
        Ok(_) => fs::symlink_metadata(path.join(".git"))
            .is_err_and(|error| engram_lookup_says_missing(error.kind())),
    }
}

/// Whether a failed file-system lookup says the entry is not there, rather
/// than that it could not be looked at (denied, or an I/O failure).
fn engram_lookup_says_missing(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// The source basis at `place`.
fn engram_place_source_basis(place: &EngramBasisPlace) -> Option<EngramExecutionSourceBasis> {
    match place {
        EngramBasisPlace::Workdir(workdir) => engram_execution_source_basis(FsPath::new(workdir)),
        EngramBasisPlace::Named {
            root,
            common_dir_key,
        } => engram_named_root_source_basis(root, common_dir_key),
    }
}

/// The source basis of a named root, taken on exactly `root`: never through
/// the ancestor walk the workdir's basis uses (`engram_worktree_root_path`),
/// so a root that was removed, or lost its `.git`, has no basis rather than
/// the main checkout's. The path must still resolve to itself: one replaced
/// by a link (a symbolic link, or a Windows junction) to another tree of the
/// same repository has no basis, checked before that tree is read and again
/// on the root the capture took. The root must still belong to the repository
/// it was named in (`common_dir_key`), and `content_revision` refuses a path
/// that is not its own worktree's root.
fn engram_named_root_source_basis(
    root: &str,
    common_dir_key: &str,
) -> Option<EngramExecutionSourceBasis> {
    let path = FsPath::new(root);
    let resolves_to_itself =
        |resolved: &FsPath| engram_exact_path_key(resolved) == engram_exact_path_key(path);
    match fs::canonicalize(path) {
        Ok(resolved) if resolves_to_itself(&resolved) => {}
        Ok(resolved) => {
            eprintln!(
                "engram> no source basis for the named root {root}: it now leads to {}",
                resolved.display()
            );
            return None;
        }
        Err(_) => {
            eprintln!("engram> no source basis for the named root {root}: it is gone");
            return None;
        }
    }
    let Some(common) = engram_source_root_common_dir(path) else {
        eprintln!(
            "engram> no source basis for the named root {root}: it is gone or no longer a \
             worktree root"
        );
        return None;
    };
    if engram_exact_path_key(&common) != common_dir_key {
        eprintln!(
            "engram> no source basis for the named root {root}: it now belongs to another \
             repository"
        );
        return None;
    }
    match content_revision(path) {
        Ok((canonical, source_revision)) if resolves_to_itself(&canonical) => {
            engram_bounded_source_basis(path, &canonical, source_revision)
        }
        Ok((canonical, _)) => {
            eprintln!(
                "engram> no source basis for the named root {root}: it was measured as {}",
                canonical.display()
            );
            None
        }
        Err(error) => {
            eprintln!("engram> no source basis for the named root {root}: {error:#}");
            None
        }
    }
}

/// Checks that `path` may be a work's source root, for a session working in
/// `workdir` of the project at `project_root`, and returns its canonical root
/// and the key of its repository's common directory. Relative paths resolve
/// from `workdir`. It must be (a) the root of a worktree TermAl can measure
/// (a trusted Git, no configured filters), (b) of the same repository as the
/// workdir, (c) registered with that repository, and (d) inside the project
/// folder, since the acceptance evaluator runs there and a delegated
/// session's folder must lie inside its project. Runs Git: off the lock.
fn validate_engram_source_root(
    path: &str,
    workdir: &str,
    project_root: &str,
) -> std::result::Result<(String, String), String> {
    let network = || {
        format!(
            "`{path}` is on a network share: TermAl measures no source root there (resolving \
             it can block, and the acceptance evaluator cannot start in it); name a local \
             worktree"
        )
    };
    // The path as written first: on Unix a `\\server\share` spelling is a
    // relative name, which would otherwise be joined to the workdir.
    if engram_network_path(path) {
        return Err(network());
    }
    // Git Bash on Windows spells a drive the MSYS way (`/c/github/x`), as
    // its `pwd` prints it; read as written, that would be joined to the
    // workdir.
    let windows_path = engram_msys_drive_path(path);
    let requested = FsPath::new(windows_path.as_ref());
    // `C:wt` on Windows: relative to that drive's current folder, which is
    // the process's and not the session's, so it is not joined to the workdir.
    if !requested.has_root()
        && matches!(requested.components().next(), Some(std::path::Component::Prefix(_)))
    {
        return Err(format!(
            "`{path}` is relative to a drive's current folder; name the worktree by its full \
             path, or relative to this session's folder"
        ));
    }
    let requested = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        FsPath::new(workdir).join(requested)
    };
    if engram_network_path(&requested.to_string_lossy()) {
        return Err(network());
    }
    let project = fs::canonicalize(project_root)
        .map_err(|error| format!("the project folder {project_root} cannot be resolved: {error}"))?;
    let outside = |root: &FsPath| {
        format!(
            "`{}` lies outside the project folder {}: the acceptance evaluator runs in the \
             named root, and a delegated session's folder must lie inside its project; create \
             the worktree inside the project (for example under .worktrees/)",
            root.display(),
            project.display()
        )
    };
    // Outside the project before any Git runs there; the root Git resolves
    // is checked again below.
    if let Ok(resolved) = fs::canonicalize(&requested)
        && !resolved.starts_with(&project)
    {
        return Err(outside(&resolved));
    }
    let git = ReviewFreezeGit::new(&requested).map_err(|error| {
        format!("`{path}` is not the root of a worktree TermAl can measure: {error:#}")
    })?;
    let root = git.root.clone();
    // A mapped drive resolves to its share.
    if engram_network_path(&root.to_string_lossy()) {
        return Err(network());
    }
    let common = engram_source_root_common_dir(&root)
        .ok_or_else(|| {
            format!(
                "`{path}` has no Git directory TermAl can read as a worktree's: its `.git` must \
                 be a directory, or a linked worktree's file (the main checkout of a \
                 `--separate-git-dir` repository cannot be named; name a linked worktree of it)"
            )
        })?;
    let session_common = test_runs_git_common_dir(&engram_worktree_root_path(FsPath::new(workdir)))
        .and_then(|common| fs::canonicalize(common).ok())
        .ok_or_else(|| "this session's workdir is not in a Git repository".to_owned())?;
    if engram_exact_path_key(&common) != engram_exact_path_key(&session_common) {
        return Err(format!(
            "`{path}` belongs to another repository than this session's workdir"
        ));
    }
    if !engram_source_root_registered(&root, &common) {
        return Err(format!(
            "`{path}` is not a worktree registered with this repository (a copied or pruned \
             .git entry); create it with git worktree add"
        ));
    }
    if !root.starts_with(&project) {
        return Err(outside(&root));
    }
    Ok((root.to_string_lossy().into_owned(), engram_exact_path_key(&common)))
}

/// Why a path was not accepted as a source root.
#[derive(Debug, PartialEq, Eq)]
enum EngramSourceRootValidation {
    /// It is not one (`validate_engram_source_root`).
    Refused(String),
    /// Checking it took longer than naming may take.
    OutOfTime(String),
}

/// `validate_engram_source_root` on its own thread, waited for at most
/// `budget`: it resolves paths and reads Git metadata, which a stalled volume
/// can hold for longer than naming may take. Past the budget the name is
/// refused, and the thread is left to finish with its answer dropped.
fn engram_validate_source_root_within(
    live: &Arc<std::sync::atomic::AtomicUsize>,
    path: String,
    workdir: String,
    project_root: String,
    budget: Duration,
) -> std::result::Result<(String, String), EngramSourceRootValidation> {
    engram_run_source_root_validation_within(
        live,
        ENGRAM_SOURCE_ROOT_VALIDATION_LIMIT,
        budget,
        path.clone(),
        move || validate_engram_source_root(&path, &workdir, &project_root),
    )
}

/// At most this many path checks run at once, host-wide. A check that
/// outlasted its request keeps its place until its thread ends, so a stalled
/// volume cannot pile up threads call after call.
const ENGRAM_SOURCE_ROOT_VALIDATION_LIMIT: usize = 4;

/// Runs `validate` on its own thread and waits for it at most `budget`, if
/// fewer than `limit` such threads counted in `live` are still running.
fn engram_run_source_root_validation_within(
    live: &Arc<std::sync::atomic::AtomicUsize>,
    limit: usize,
    budget: Duration,
    path: String,
    validate: impl FnOnce() -> std::result::Result<(String, String), String> + Send + 'static,
) -> std::result::Result<(String, String), EngramSourceRootValidation> {
    use std::sync::atomic::Ordering;
    /// Gives the place back when the thread ends, on a panic too.
    struct Release(Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    if live
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |running| {
            (running < limit).then_some(running + 1)
        })
        .is_err()
    {
        return Err(EngramSourceRootValidation::OutOfTime(format!(
            "`{path}` cannot be checked now: earlier path checks are still running (a slow or \
             stalled volume?); name it again later"
        )));
    }
    let release = Release(live.clone());
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("engram-source-root-validate".to_owned())
        .spawn(move || {
            let _release = release;
            let _ = sender.send(validate());
        })
        .map_err(|error| {
            EngramSourceRootValidation::OutOfTime(format!(
                "`{path}` cannot be checked now: {error}; name it again"
            ))
        })?;
    match receiver.recv_timeout(budget) {
        Ok(validated) => validated.map_err(EngramSourceRootValidation::Refused),
        Err(_) => Err(EngramSourceRootValidation::OutOfTime(format!(
            "checking `{path}` took longer than naming may take, so nothing was named; name it \
             again"
        ))),
    }
}

/// The entry naming the source root of `work_id`'s claim `claim_id` in
/// `store`, if any. An entry for another claim of the same work does not
/// apply.
fn engram_work_source_root_for_claim<'a>(
    entries: &'a [EngramWorkSourceRoot],
    store: &EngramAuthorityStoreKey,
    work_id: &str,
    claim_id: &str,
) -> Option<&'a EngramWorkSourceRoot> {
    entries.iter().find(|entry| {
        &entry.store == store && entry.work_id == work_id && entry.claim_id == claim_id
    })
}

/// The entry of `work_id` in `store`, whatever its claim.
fn engram_work_source_root_for_work<'a>(
    entries: &'a [EngramWorkSourceRoot],
    store: &EngramAuthorityStoreKey,
    work_id: &str,
) -> Option<&'a EngramWorkSourceRoot> {
    entries
        .iter()
        .find(|entry| &entry.store == store && entry.work_id == work_id)
}

/// Replaces the entry of `work_id` in `store` with `entry`, or removes it when
/// `entry` is `None`. A new entry on a full list is refused, naming every
/// entry, so the caller can see what holds the list; nothing is evicted.
fn engram_set_work_source_root(
    entries: &mut Vec<EngramWorkSourceRoot>,
    store: &EngramAuthorityStoreKey,
    work_id: &str,
    entry: Option<EngramWorkSourceRoot>,
) -> std::result::Result<(), String> {
    let existing = entries
        .iter()
        .position(|entry| &entry.store == store && entry.work_id == work_id);
    match (existing, entry) {
        (Some(index), Some(entry)) => entries[index] = entry,
        (Some(index), None) => {
            entries.remove(index);
        }
        (None, Some(entry)) => {
            if entries.len() >= ENGRAM_WORK_SOURCE_ROOT_LIMIT {
                let held = entries
                    .iter()
                    .map(|entry| {
                        format!(
                            "{} (named by {} at {}, in store {})",
                            entry.short_ref,
                            entry.named_by_session,
                            entry.named_at,
                            entry.store.project_id
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(format!(
                    "TermAl keeps at most {ENGRAM_WORK_SOURCE_ROOT_LIMIT} named source roots and \
                     evicts none. An entry ends when a session holding its work's claim clears \
                     it, when the session that named it is removed, or, once its claim has ended, \
                     at that session's next naming call; entries of released claims of every \
                     live session were already ended before this refusal. An entry in a store \
                     its session's project no longer uses, or of a project with Engram turned \
                     off, cannot be read or cleared and stays until the session that named it \
                     is removed. Entries: {held}"
                ));
            }
            entries.push(entry);
        }
        (None, None) => {}
    }
    Ok(())
}

/// Ends the entries session `session_id` named in `store` whose claim a
/// complete held-claims read of that session no longer lists. A read that
/// left claims out (`omitted`) ends nothing: an unlisted claim may be among
/// them; nor does one with a row that names no claim (an Engram build that
/// lists no claim ids), since any entry's claim may be that row's. Only
/// entries in `known`, the list as it was before the read began, can end:
/// one named after the read began has a claim the read may not list yet.
/// Returns whether anything ended.
fn engram_end_released_work_source_roots(
    entries: &mut Vec<EngramWorkSourceRoot>,
    known: &[EngramWorkSourceRoot],
    session_id: &str,
    store: &EngramAuthorityStoreKey,
    held: &EngramHeldClaims,
) -> bool {
    if held.omitted > 0 || held.items.iter().any(|claim| claim.claim_id.is_empty()) {
        return false;
    }
    let before = entries.len();
    entries.retain(|entry| {
        entry.named_by_session != session_id
            || &entry.store != store
            || !known.contains(entry)
            || held
                .items
                .iter()
                .any(|claim| claim.claim_id == entry.claim_id)
    });
    entries.len() != before
}

/// Ends the entries whose naming session no longer exists, so no entry is
/// left that nobody may clear. Returns whether anything ended.
fn engram_end_orphaned_work_source_roots(
    entries: &mut Vec<EngramWorkSourceRoot>,
    session_exists: impl Fn(&str) -> bool,
) -> bool {
    let before = entries.len();
    entries.retain(|entry| session_exists(&entry.named_by_session));
    entries.len() != before
}

/// `termal_name_source_root`: names, or with no `path` clears, the source
/// root of a work the calling session holds a live claim on. `work` is the
/// work id or its short reference.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramSourceRootRequest {
    work: String,
    /// `None` when omitted, the only way to clear; `Some(None)` for an
    /// explicit null, which is refused rather than read as a clear.
    #[serde(default, deserialize_with = "engram_present_optional_string")]
    path: Option<Option<String>>,
}

/// A field that is present, null or not: `Some` of what it holds, so an
/// explicit null is told apart from an omitted field (`None`, by default).
fn engram_present_optional_string<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramSourceRootResponse {
    work_ref: String,
    work_id: String,
    /// The named root; absent after a clear.
    #[serde(skip_serializing_if = "Option::is_none")]
    root: Option<String>,
    /// The root's content revision now.
    #[serde(skip_serializing_if = "Option::is_none")]
    source_revision: Option<String>,
    /// Why the root has no revision now, when it has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    unmeasured: Option<String>,
    /// The generation of the new name; 0 after a clear.
    generation: u64,
    /// The previous root's revision, sealed for the caller's turn running in
    /// it under the same claim since before the call; absent when no such
    /// turn took it.
    #[serde(skip_serializing_if = "Option::is_none")]
    sealed: Option<EngramSealedSourceRoot>,
    notice: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramSealedSourceRoot {
    root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_revision: Option<String>,
}

const ENGRAM_SOURCE_ROOT_TAKES_EFFECT_NOTICE: &str = "The source root takes effect at your next \
     turn; this turn keeps the root it began with.";

/// Names, or clears, the source root of a work the session in the path holds
/// a live claim on (`AppState::name_engram_source_root`).
async fn name_engram_source_root(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<EngramSourceRootRequest>, JsonRejection>,
) -> Result<Json<EngramSourceRootResponse>, ApiError> {
    let Json(request) =
        request.map_err(|rejection| api_json_rejection("source root request", rejection))?;
    let response =
        run_blocking_api(move || state.name_engram_source_root(&session_id, request)).await?;
    Ok(Json(response))
}

impl AppState {
    /// One held-claims read under the own connection of every session other
    /// than `caller` that named an entry and still exists, with the store
    /// its project is bound to: what a full list needs to end the entries of
    /// claims that have ended. Sessions without an Engram target, and reads
    /// that fail, are left out, so they end nothing. Off the lock; at most one
    /// read per naming session, and only when the list is full. The reads run
    /// side by side (`ENGRAM_SOURCE_ROOT_RECLAIM_READERS`), each bounded by
    /// what is left before `deadline`; one not started by then is skipped and
    /// ends nothing. Each is a one-shot `engram work core held` process under
    /// that session's own identity, not a request on its control transport,
    /// so it does not touch the session's binding; a session in a turn is
    /// read as it stands. The MCP bridge waits for all of it:
    /// `tool_name_source_root` allows `ENGRAM_SOURCE_ROOT_NAMING_BUDGET` on top
    /// of its normal request timeout.
    fn engram_source_root_held_reads_of_other_sessions(
        &self,
        caller: &str,
        deadline: std::time::Instant,
    ) -> Vec<(String, EngramAuthorityStoreKey, EngramHeldClaims)> {
        let targets = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let mut sessions = inner
                .engram_work_source_roots
                .iter()
                .map(|entry| entry.named_by_session.clone())
                .filter(|named_by| named_by != caller)
                .collect::<Vec<_>>();
            sessions.sort();
            sessions.dedup();
            sessions
                .into_iter()
                .filter(|named_by| inner.find_session_index(named_by).is_some())
                .filter_map(|named_by| {
                    // A project with Engram control off is not read: its
                    // entries stay, as the full-list refusal says.
                    let target =
                        Self::engram_binding_target_for_session_shape_locked(&inner, &named_by, true)
                            .ok()
                            .flatten()?;
                    let store = target.settings.authority_store_key.clone()?;
                    Some((named_by, store, target))
                })
                .collect::<Vec<_>>()
        };
        let readers = targets.len().min(ENGRAM_SOURCE_ROOT_RECLAIM_READERS);
        let queue = Mutex::new(targets.into_iter());
        let reads = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..readers {
                scope.spawn(|| {
                    loop {
                        let Some((named_by, store, target)) =
                            queue.lock().expect("source root reclaim queue poisoned").next()
                        else {
                            break;
                        };
                        let left = deadline.saturating_duration_since(std::time::Instant::now());
                        if left.is_zero() {
                            continue;
                        }
                        if let Ok(held) = target.adapter.read_held_claims(
                            &target.connection,
                            ENGRAM_WORK_BINDING_COMMAND_TIMEOUT.min(left),
                        ) {
                            reads
                                .lock()
                                .expect("source root reclaim reads poisoned")
                                .push((named_by, store, held));
                        }
                    }
                });
            }
        });
        reads.into_inner().expect("source root reclaim reads poisoned")
    }

    /// Takes the lines the prompt of turn `active_turn_generation` carried
    /// out of the pending source-root lines once the runtime accepted it; a
    /// line set since that prompt was built, even one it carried, stays for
    /// the next prompt (`set_pending_source_root_line`).
    fn acknowledge_engram_source_root_line_delivery(
        &self,
        session_id: &str,
        active_turn_generation: u64,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let engram = &mut inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .engram;
        if engram
            .source_root_line_delivery
            .as_ref()
            .is_none_or(|(_, turn_generation)| *turn_generation != active_turn_generation)
        {
            return;
        }
        let Some((delivered, _)) = engram.source_root_line_delivery.take() else {
            return;
        };
        let delivered = delivered.lines().collect::<HashSet<_>>();
        engram.pending_source_root_line = engram
            .pending_source_root_line
            .as_deref()
            .map(|pending| {
                pending
                    .lines()
                    .filter(|line| !delivered.contains(line))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|left| !left.is_empty());
    }

    fn name_engram_source_root(
        &self,
        session_id: &str,
        request: EngramSourceRootRequest,
    ) -> std::result::Result<EngramSourceRootResponse, ApiError> {
        // Everything below shares one budget, within the bridge's wait.
        let deadline = std::time::Instant::now() + ENGRAM_SOURCE_ROOT_NAMING_BUDGET;
        let left = || deadline.saturating_duration_since(std::time::Instant::now());
        let work = request.work.trim().to_owned();
        if work.is_empty() || work.len() > 256 {
            return Err(ApiError::bad_request(
                "`work` must name the work by its id or short reference, in at most 256 bytes",
            ));
        }
        // Only an omitted `path` clears: a blank one is refused, not read as a
        // clear.
        let path = match request.path.as_ref().map(|path| path.as_deref().map(str::trim)) {
            None => None,
            Some(None) => {
                return Err(ApiError::bad_request(
                    "`path` is null; name a worktree, or omit `path` to clear the name",
                ));
            }
            Some(Some("")) => {
                return Err(ApiError::bad_request(
                    "`path` is blank; name a worktree, or omit `path` to clear the name",
                ));
            }
            Some(Some(path)) if path.chars().count() > MAX_DELEGATION_CWD_CHARS => {
                return Err(ApiError::bad_request(format!(
                    "`path` must be at most {MAX_DELEGATION_CWD_CHARS} characters"
                )));
            }
            Some(Some(path)) => Some(path.to_owned()),
        };
        // `known` is the list before any held-claims read begins: a read ends
        // only entries that existed when it began.
        let (target, workdir, project_id, project_root, known, validations_live, sealable) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            // A delegated session by its row or by its own link, as every
            // root-only route tells them.
            if Self::engram_session_has_child_binding_shape_locked(&inner, session_id) {
                return Err(ApiError::conflict(
                    "only a root session holding the work's claim names its source root",
                ));
            }
            let project = engram_project_for_session_locked(&inner, session_id).ok_or_else(|| {
                ApiError::conflict(
                    "this session has no project; Engram is configured per project",
                )
            })?;
            let project_root = project.root_path.clone();
            // A name counts only for turns Engram mediates, so a project or
            // session with Engram control off names nothing.
            let target = Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                .map_err(ApiError::conflict)?
                .ok_or_else(|| {
                    ApiError::conflict(
                        "Engram control is not enabled for this session or its project",
                    )
                })?;
            // The caller's turn a rename or a clear may seal: one admitted,
            // with its root kept, before this call read anything. A turn
            // admitted later measures what it finds; a finished one keeps its
            // root on the record until the next grant, but has nothing left
            // to seal.
            let engram = &inner.sessions[index].engram;
            let sealable = engram.active_grant_id.clone().zip(
                engram
                    .active_turn_source_root
                    .as_ref()
                    .map(|turn_root| (turn_root.root.clone(), turn_root.claim_id.clone())),
            );
            (
                target,
                inner.sessions[index].session.workdir.clone(),
                inner.sessions[index].session.project_id.clone(),
                project_root,
                inner.engram_work_source_roots.clone(),
                inner.engram_source_root_validations_live.clone(),
                sealable,
            )
        };
        let store = target.settings.authority_store_key.clone().ok_or_else(|| {
            ApiError::conflict(
                "this project's Engram store has no identity yet; a source root is named after \
                 the session has bound",
            )
        })?;
        let held = target
            .adapter
            .read_held_claims(
                &target.connection,
                ENGRAM_WORK_BINDING_COMMAND_TIMEOUT.min(left()),
            )
            .map_err(|error| ApiError::bad_gateway(format!("engram work core held: {error}")))?;
        #[cfg(test)]
        if let Some(meanwhile) = TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| hook.borrow_mut().take()) {
            meanwhile();
        }
        let claim = held
            .items
            .iter()
            .find(|claim| claim.work_id == work || claim.short_ref == work)
            .cloned()
            .ok_or_else(|| {
                let unlisted = if held.omitted > 0 {
                    format!(
                        " among the {} claims Engram listed ({} more were left out of its list)",
                        held.items.len(),
                        held.omitted
                    )
                } else {
                    String::new()
                };
                ApiError::conflict(format!(
                    "this session holds no live claim on `{work}`{unlisted}; only the agent \
                     holding the claim names its source root"
                ))
            })?;
        if claim.claim_id.is_empty() {
            return Err(ApiError::bad_gateway(
                "this Engram build lists no claim id in `work core held`; install a newer Engram",
            ));
        }
        // An evaluation finds its root by the work's short reference (the
        // show receipt carries no work id), so an entry without one would
        // leave the evaluator in the workdir while the turns use the root.
        if claim.short_ref.is_empty() {
            return Err(ApiError::bad_gateway(
                "Engram listed the claim without its work's short reference in `work core held`, \
                 which TermAl matches evaluations by; install a newer Engram",
            ));
        }
        let validated = match path {
            None => None,
            Some(path) => Some(
                engram_validate_source_root_within(
                    &validations_live,
                    path,
                    workdir.clone(),
                    project_root.clone(),
                    left(),
                )
                .map_err(|refusal| match refusal {
                    EngramSourceRootValidation::Refused(message) => ApiError::bad_request(message),
                    EngramSourceRootValidation::OutOfTime(message) => ApiError::from_status(
                        StatusCode::SERVICE_UNAVAILABLE,
                        message,
                    ),
                })?,
            ),
        };
        // The work's entry as it was before any read began: whatever named
        // the work since (a newer claim's holder, say) makes the name below
        // a conflict rather than be overwritten by what this call read.
        let old = engram_work_source_root_for_work(&known, &store, &claim.work_id).cloned();
        // Full once the caller's own entries of ended claims are gone, as
        // they are at the commit below: only a list still full then needs
        // the reads of other sessions.
        let list_full = {
            let mut after_own = known.clone();
            engram_end_released_work_source_roots(&mut after_own, &known, session_id, &store, &held);
            after_own.len() >= ENGRAM_WORK_SOURCE_ROOT_LIMIT
        };
        // A new name on a full list first ends the entries of released
        // claims of every other naming session that still exists, one
        // held-claims read under each one's own connection, so an entry of an
        // ended claim cannot hold a place nobody may clear. A read that
        // fails, runs out of the budget, or leaves claims out, ends nothing
        // for that session.
        let reclaimed = if list_full && old.is_none() && validated.is_some() {
            self.engram_source_root_held_reads_of_other_sessions(session_id, deadline)
        } else {
            Vec::new()
        };
        // A rename or a clear seals the old root for the caller's turn running
        // in it on that claim since before this call began (`sealable`;
        // sealed below, under the lock, if that turn still runs); with no
        // such turn nothing is captured. The captures are taken within what
        // is left of the budget, counted with the turns' capture threads; one
        // not taken seals no revision, or reports the new root unmeasured.
        let sealed = old
            .as_ref()
            .filter(|old| validated.as_ref().is_none_or(|(root, _)| *root != old.root))
            .filter(|old| {
                sealable.as_ref().is_some_and(|(_, (root, claim_id))| {
                    *root == old.root && *claim_id == old.claim_id
                })
            })
            .map(|old| EngramSealedSourceRoot {
                root: old.root.clone(),
                source_revision: self
                    .engram_source_basis_within(
                        &EngramBasisPlace::Named {
                            root: old.root.clone(),
                            common_dir_key: old.common_dir_key.clone(),
                        },
                        engram_source_root_capture_budget(left()),
                    )
                    .map(|basis| basis.source_revision),
            });
        let basis = validated.as_ref().and_then(|(root, common_dir_key)| {
            self.engram_source_basis_within(
                &EngramBasisPlace::Named {
                    root: root.clone(),
                    common_dir_key: common_dir_key.clone(),
                },
                engram_source_root_capture_budget(left()),
            )
        });
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        // Nothing is kept past the budget, so the bridge, which waits that
        // long and more, never reports a failure for a name the server kept.
        if left().is_zero() {
            return Err(ApiError::from_status(
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "naming took longer than its {} s budget, so nothing was named; name it again",
                    ENGRAM_SOURCE_ROOT_NAMING_BUDGET.as_secs()
                ),
            ));
        }
        // The session must still be where the name was checked: the same
        // project, folder and Engram store, with Engram control on. A project
        // removed or reconfigured meanwhile would leave a name nobody could
        // use or clear.
        let still_there = inner.sessions[index].session.workdir == workdir
            && inner.sessions[index].session.project_id == project_id
            && engram_project_for_session_locked(&inner, session_id)
                .is_some_and(|project| project.root_path == project_root)
            && Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                .ok()
                .flatten()
                .is_some_and(|current| current.settings.authority_store_key.as_ref() == Some(&store));
        if !still_there {
            return Err(ApiError::conflict(
                "this session's project, folder or Engram store changed while the root was being \
                 named, so nothing was named; name it again",
            ));
        }
        if engram_work_source_root_for_work(&inner.engram_work_source_roots, &store, &claim.work_id)
            != old.as_ref()
        {
            return Err(ApiError::conflict(
                "the work's source root changed while it was being named; name it again",
            ));
        }
        let previous = inner.engram_work_source_roots.clone();
        let previous_generation = inner.engram_source_root_generation;
        let previous_record = {
            let engram = &inner.sessions[index].engram;
            (
                engram.pending_source_root_line.clone(),
                engram.source_root_line_delivery.clone(),
                engram.active_turn_source_root.clone(),
            )
        };
        engram_end_released_work_source_roots(
            &mut inner.engram_work_source_roots,
            &known,
            session_id,
            &store,
            &held,
        );
        for (named_by, named_store, named_held) in &reclaimed {
            engram_end_released_work_source_roots(
                &mut inner.engram_work_source_roots,
                &known,
                named_by,
                named_store,
                named_held,
            );
        }
        // The same root named again under the same claim is the same name;
        // anything else is a new one, numbered above every name given before.
        let kept = old.as_ref().filter(|old| {
            old.claim_id == claim.claim_id
                && validated.as_ref().is_some_and(|(root, _)| *root == old.root)
        });
        let generation = match (&validated, kept) {
            (None, _) => 0,
            (Some(_), Some(kept)) => kept.generation,
            (Some(_), None) => {
                inner.engram_source_root_generation =
                    inner.engram_source_root_generation.saturating_add(1);
                inner.engram_source_root_generation
            }
        };
        let entry = validated
            .as_ref()
            .map(|(root, common_dir_key)| EngramWorkSourceRoot {
                store: store.clone(),
                work_id: claim.work_id.clone(),
                short_ref: claim.short_ref.clone(),
                claim_id: claim.claim_id.clone(),
                claim_fence: claim.claim_fence,
                root: root.clone(),
                common_dir_key: common_dir_key.clone(),
                named_by_session: session_id.to_owned(),
                named_at: kept.map_or_else(
                    || chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    |kept| kept.named_at.clone(),
                ),
                generation,
            });
        let line = engram_source_root_line(
            entry.as_ref(),
            &claim.short_ref,
            &workdir,
            engram_one_call_offered_up_front(inner.sessions[index].session.agent),
        );
        if let Err(error) = engram_set_work_source_root(
            &mut inner.engram_work_source_roots,
            &store,
            &claim.work_id,
            entry,
        ) {
            // Only the new name is refused: the entries of ended claims the
            // reclaim ended stay ended, as the refusal says, and are kept.
            inner.engram_source_root_generation = previous_generation;
            if inner.engram_work_source_roots != previous
                && self.commit_locked(&mut inner).is_err()
            {
                inner.engram_work_source_roots = previous;
            }
            return Err(ApiError::conflict(error));
        }
        // The agent's next turn is told where it is measured from then on.
        inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .engram
            .set_pending_source_root_line(line);
        // The seal is for the caller's turn measured in the old root under
        // the old entry's claim, and is reported only when that turn took it.
        // A turn of another claim in the same tree (two works may name one
        // worktree) is not sealed: its work still names the tree, so its
        // later edits there are its own and a seal would hide them. Nor is a
        // turn admitted after this call began, whose own start the capture
        // may predate: the grant must be the one `sealable` noted.
        let sealed = match (sealed, &old, &sealable) {
            (Some(sealed), Some(old), Some((grant_id, _))) => {
                let engram = &mut inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid")
                    .engram;
                let same_turn = engram.active_grant_id.as_ref() == Some(grant_id);
                engram
                    .active_turn_source_root
                    .as_mut()
                    .filter(|turn_root| {
                        same_turn
                            && turn_root.root == sealed.root
                            && turn_root.claim_id == old.claim_id
                    })
                    .map(|turn_root| {
                        turn_root.sealed_revision = sealed.source_revision.clone();
                        sealed
                    })
            }
            _ => None,
        };
        if let Err(error) = self.commit_locked(&mut inner) {
            // Nothing was kept, so the agent is told of nothing and the
            // running turn keeps its root unsealed.
            inner.engram_work_source_roots = previous;
            inner.engram_source_root_generation = previous_generation;
            let engram = &mut inner
                .session_mut_by_index(index)
                .expect("session index should be valid")
                .engram;
            (
                engram.pending_source_root_line,
                engram.source_root_line_delivery,
                engram.active_turn_source_root,
            ) = previous_record;
            return Err(ApiError::internal(format!(
                "failed to persist the source root: {error:#}"
            )));
        }
        let (root, source_revision, unmeasured) = match (&validated, basis) {
            (Some((root, _)), Some(basis)) => (Some(root.clone()), Some(basis.source_revision), None),
            (Some((root, _)), None) => (
                Some(root.clone()),
                None,
                Some(
                    "TermAl cannot take its content revision now; see the host log".to_owned(),
                ),
            ),
            (None, _) => (None, None, None),
        };
        let notice = match &root {
            Some(root) => format!(
                "`{}` now names {root} as its source root. {ENGRAM_SOURCE_ROOT_TAKES_EFFECT_NOTICE}",
                claim.short_ref
            ),
            None => format!(
                "`{}` has no named source root: its turns are measured in the session's workdir. \
                 {ENGRAM_SOURCE_ROOT_TAKES_EFFECT_NOTICE}",
                claim.short_ref
            ),
        };
        Ok(EngramSourceRootResponse {
            work_ref: claim.short_ref,
            work_id: claim.work_id,
            root,
            source_revision,
            unmeasured,
            generation,
            sealed,
            notice,
        })
    }
}
