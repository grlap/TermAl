// Background and detached test-launcher gates, carried past their turn.
//
// Owns: telling a launcher full gate whose command result marks only its
// launch (a runtime's background mode, or `--detach`) from one that ended;
// keeping it on its session as a carried check past the turn that launched
// it; fencing it against every write the host observes in its worktree until
// it settles, with what wrote; finding the run's directory and keeping the
// digest of its terminal record from the first read that found it terminal;
// and, at the holder's next checkpoint on the same claim and root
// generation, crediting it as a resolved check for the report or refusing it
// with the reason told to the holder, and merging the credited ones into that
// checkpoint's checks (`engram_credit_carried_checks`). Also the marker that
// survives a host restart, so the holder learns that the run lost its credit,
// and the merging of the lines about checks' credit that wait for a prompt
// (`engram_merge_credit_lines`).
//
// Does not own: the in-turn check lifecycle and its overlap rules
// (engram_turn_checks.rs), the report's layout (`engram_turn_report`), the
// launcher (scripts/test-launcher.mjs), or the run index and its cards
// (test_runs.rs), whose tick only calls `poll_engram_carried_runs`.
//
// New file: nothing was moved into it.

/// At most this many carried checks per session; a new launch past it drops
/// the oldest, whose holder is told.
const ENGRAM_CARRIED_CHECK_LIMIT: usize = 4;

/// A carried check whose run never settles in this long is dropped, and its
/// holder told: its launch failed, or its run was removed.
const ENGRAM_CARRIED_CHECK_TTL_SECONDS: i64 = 6 * 60 * 60;

/// At most this many distinct directories of one watcher batch are placed in
/// their worktrees for the carried fence; a batch touching more fences every
/// carried check, since one of them may be its worktree.
const ENGRAM_CARRIED_WATCH_DIRECTORY_LIMIT: usize = 256;

/// The largest launcher record read, so a malformed or hostile file cannot
/// exhaust memory. A real terminal record of a full gate is a few KiB.
const ENGRAM_CARRIED_RECORD_MAX_BYTES: u64 = 1024 * 1024;

/// A carried launch whose run directory has not appeared this long after the
/// launch never started a run (a launcher argument error, a Node failure
/// before the run was created): it is refused as not found.
const ENGRAM_CARRIED_RUN_SEARCH_SECONDS: i64 = 10 * 60;

/// At most this many run directories that carried checks of a session
/// settled or dropped are kept (`carried_consumed_runs`), so a later launch
/// never takes a run an earlier one already used.
const ENGRAM_CARRIED_CONSUMED_RUN_LIMIT: usize = 16;

/// A run whose request started this long before the host stamped the
/// launch may still be the launch's: the host stamps a launch when it has
/// taken the command's start under its lock, which can lag the launcher's
/// own start by a contended lock or a runtime that reports late. The owner
/// match and the fence keep another run out of that window.
const ENGRAM_CARRIED_LAUNCH_SLACK_SECONDS: i64 = 10;

/// How every host line about a check's credit begins: a check withheld, a
/// background gate refused, dropped or not carried, or one lost to a restart.
/// Such lines merge with each other
/// (`EngramSessionState::set_pending_source_root_line`) rather than pushing
/// a bind or name line out of the pending lines.
const ENGRAM_CHECK_CREDIT_LINE_PREFIX: &str = "[TermAl] Check credit:";

/// The longest merged line about checks' credit. The merge keeps the newest
/// and cuts from the front; every such line is logged as it is set, so what
/// it cuts is still in the host log.
const ENGRAM_CHECK_CREDIT_LINE_MAX_BYTES: usize = 1600;

/// A launcher full gate whose result marked only its launch, kept past the
/// turn that launched it until the run settles.
#[derive(Clone, Debug)]
struct EngramCarriedCheck {
    /// The check as its launch recorded it: grant, command, root, start time
    /// and start source basis. It has no end.
    check: EngramTurnCheck,
    /// The claimed work the launching turn was bound to.
    work_id: String,
    claim_id: String,
    /// The generation of the named source root the check ran in.
    root_generation: u64,
    /// The run's directory, once found (`engram_find_carried_run`).
    run_directory: Option<PathBuf>,
    /// The SHA-256 of the run's terminal record, from the first read that
    /// found it terminal; settlement refuses a record that differs.
    terminal_digest: Option<String>,
    /// The run has no terminal record and the process responsible for it is
    /// provably gone (`engram_carried_run_launcher_gone`): it will never end.
    launcher_gone: bool,
    /// No run directory appeared within `ENGRAM_CARRIED_RUN_SEARCH_SECONDS`
    /// of the launch: the launch started no run.
    run_missing: bool,
    /// Two reads that each found the run terminal saw different records
    /// (polls run on the run index's tick and before checkpoints, and read
    /// off the lock): the first terminal read cannot be told, so the check is
    /// refused.
    terminal_conflict: bool,
    /// Another launch of the session in the same worktree had no run matched
    /// to it when this one was carried (a launch fenced before a poll found
    /// its run, one that left unmatched, or a compound launch that was
    /// dropped): a run found now could be either's, so this one is refused.
    ambiguous: bool,
    /// Run directories already read that can never be this launch's run (of
    /// another root or mode, or started before it), so later searches skip
    /// them rather than read every run again on every tick.
    ruled_out_runs: std::collections::BTreeSet<PathBuf>,
    /// What wrote into its worktree while it was carried, if anything did;
    /// such a check is refused and its holder told.
    fence: Option<String>,
}

/// What survives a host restart of a carried check: enough to tell its
/// holder that the run lost its credit.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramCarriedLaunchMarker {
    root: String,
    launched_at: String,
    fingerprint: String,
}

impl EngramCarriedCheck {
    fn marker(&self) -> EngramCarriedLaunchMarker {
        EngramCarriedLaunchMarker {
            root: self.check.target.root.to_string_lossy().into_owned(),
            launched_at: self.check.started_at.clone(),
            fingerprint: engram_check_fingerprint(&self.check.command),
        }
    }

    /// The line its holder gets when the check is refused, naming the gate by
    /// fingerprint and root, never by its command line.
    fn refusal_line(&self, why: &str) -> String {
        format!(
            "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} the background full gate launched at {} in {} \
             (check {}) earned no credit: {why}. Run the gate again.",
            self.check.started_at,
            engram_source_root_display(&self.check.target.root.to_string_lossy()),
            engram_check_fingerprint(&self.check.command),
        )
    }
}

/// The line the holder of a check the host carried when it restarted gets.
fn engram_carried_lost_to_restart_line(marker: &EngramCarriedLaunchMarker) -> String {
    format!(
        "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} the host restarted while the background full gate \
         launched at {} in {} (check {}) was running; it earned no credit, since the host \
         could not watch its worktree while it was down. Run the gate again.",
        marker.launched_at,
        engram_source_root_display(&marker.root),
        marker.fingerprint,
    )
}

/// The words of `check`'s test, when it is the test launcher.
fn engram_launcher_words(check: &EngramCheckCommand) -> Option<Vec<String>> {
    if check.program != "node" {
        return None;
    }
    let words = engram_shell_words(&check.normalized)?;
    words
        .get(1)
        .is_some_and(|script| script.replace('\\', "/").ends_with("test-launcher.mjs"))
        .then_some(words)
}

/// What becomes of a check whose command just ended
/// (`note_engram_command_finished`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngramLaunchDisposition {
    /// Not a gate's launch: the check ends as any check does.
    Ordinary,
    /// A full gate's launch the host carries until its run settles.
    Carry,
    /// A full gate's launch that can earn no credit and must not be recorded
    /// as a test result: the check is dropped and its holder told why.
    Drop(&'static str),
}

/// What becomes of `check`, which ended `exit`. A full gate whose result
/// marks only its launch (the runtime's background mode, or `--detach`) is
/// carried when its line is the gate alone and, detached, its launch
/// succeeded. A launch on a line that runs anything more is dropped: what the
/// rest wrote in that same call never passes the fence. A detached launch
/// that failed started no run, and is dropped rather than recorded as a
/// failed test. Any other check ends as usual.
/// Why a launch on a line that runs more than the gate is dropped.
const ENGRAM_LAUNCH_DROP_COMPOUND: &str = concat!(
    "its line runs more than the gate, and what the rest of it wrote is outside ",
    "the fence; launch the gate alone on its line, two minutes or more after this launch"
);

/// Why a detached launch that failed is dropped.
const ENGRAM_LAUNCH_DROP_FAILED: &str = "its detached launch failed, so no run started";

/// Why an ambiguous carried check is refused (`EngramCarriedCheck::ambiguous`).
const ENGRAM_CARRIED_AMBIGUOUS: &str = concat!(
    "another background gate launched in the same worktree less than two minutes before it ",
    "had no run matched to it, so the host cannot tell their runs apart; launch the gate ",
    "again two minutes or more after the last launch there"
);

/// How long after a launch with no run matched to it a later launch in the
/// same worktree is refused as ambiguous. Run matching takes only runs that
/// started at most `ENGRAM_CARRIED_LAUNCH_SLACK_SECONDS` before a launch, so
/// an earlier launch's run can be confused with a later launch only when it
/// started that late; this bounds the launcher's start lag, generously. The
/// refusal lines name this wait ("two minutes").
const ENGRAM_CARRIED_AMBIGUITY_SECONDS: i64 = 120;

/// At most this many unmatched launches are kept per session
/// (`carried_unmatched_launches`).
const ENGRAM_CARRIED_UNMATCHED_LAUNCH_LIMIT: usize = 16;

fn engram_launch_disposition(
    check: &EngramCheckCommand,
    exit: EngramCommandExit,
) -> EngramLaunchDisposition {
    let Some(words) = engram_launcher_words(check) else {
        return EngramLaunchDisposition::Ordinary;
    };
    // A word ends at a shell operator a compound line may join to it
    // (`full;`, `--detach&&`), so such a launch is still seen as the gate's.
    let bare = |word: &String| {
        word.split([';', '&', '|', '<', '>', '(', ')'])
            .next()
            .unwrap_or("")
            .to_owned()
    };
    if words.get(2).map(bare).as_deref() != Some("full") {
        return EngramLaunchDisposition::Ordinary;
    }
    let detached = words.iter().any(|word| bare(word) == "--detach");
    let background = exit == EngramCommandExit::NotFinished;
    if !detached && !background {
        return EngramLaunchDisposition::Ordinary;
    }
    if !check.simple {
        return EngramLaunchDisposition::Drop(ENGRAM_LAUNCH_DROP_COMPOUND);
    }
    let launched = matches!(
        exit,
        EngramCommandExit::NotFinished
            | EngramCommandExit::Code(0)
            | EngramCommandExit::ReportedSuccess
    );
    if !launched {
        return EngramLaunchDisposition::Drop(ENGRAM_LAUNCH_DROP_FAILED);
    }
    EngramLaunchDisposition::Carry
}

/// The line the holder of a dropped launch gets (`EngramLaunchDisposition::Drop`).
fn engram_dropped_launch_line(check: &EngramCheckCommand, why: &str) -> String {
    format!(
        "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} a background full gate (check {}) earned no credit \
         and was not recorded: {why}.",
        engram_check_fingerprint(check)
    )
}

/// Fences the carried checks of `record` for one of its own commands, which
/// may write in `worktrees`, unless the command only reads. Only a Claude
/// session's lines, which its Bash tool runs, are read as Bash
/// (`engram_command_reads_only`); another runtime's line may run under
/// PowerShell or a wrapper whose reading differs, so it always fences, as
/// does a command whose line TermAl was not told. The cause names the command
/// by its program and a fingerprint of its line, never the line itself, which
/// can carry a secret.
fn engram_fence_carried_for_own_command(
    record: &mut SessionRecord,
    worktrees: &[Option<String>],
    ran: Option<&str>,
) {
    if record.engram.carried_checks.is_empty() {
        return;
    }
    let reads_only = record.session.agent == Agent::Claude
        && ran.is_some_and(|ran| engram_command_reads_only(ran, &record.session.workdir));
    if reads_only {
        return;
    }
    let cause = match ran {
        Some(ran) => engram_own_command_cause(ran),
        None => "this session ran a command there whose line TermAl was not told".to_owned(),
    };
    engram_fence_carried_checks(record, worktrees, &cause);
}

/// How a fence names the holder's command `ran` (`engram_command_name`).
fn engram_own_command_cause(ran: &str) -> String {
    let (command, line) = engram_command_name(ran);
    format!("this session ran {command} there {line}")
}

/// How a fence names the command line `ran`, as "a `PROGRAM` command" and
/// "(line DIGEST)", the digest after whatever says where it ran: its program
/// (the first word after a one-call form's `pushd "DIR" &&`) and the start of
/// the SHA-256 of its whole line, never the line itself, which can carry a
/// secret. Returned in two parts so a caller can put the place between them.
fn engram_command_name(ran: &str) -> (String, String) {
    let ran = ran.trim();
    let line = engram_one_call_prefix(ran).map_or(ran, |(_, rest)| rest.trim());
    // Leading `NAME=value` assignments (and `env`) are skipped: their value
    // can be a secret.
    let program = engram_shell_words(line)
        .and_then(|words| {
            words
                .into_iter()
                .find(|word| !word.contains('=') && word != "env")
                .map(|word| engram_program_name(&word))
        })
        .filter(|program| !program.is_empty())
        .unwrap_or_else(|| "unparsed".to_owned());
    (
        format!("a `{}` command", engram_truncate_utf8(&program, 40)),
        format!("(line {})", &sha256_hex(ran.as_bytes())[..12]),
    )
}

/// What a session that may write just did, for the carried checks of the
/// other sessions (`engram_fence_carried_for_other_session`). A carried check
/// stays open for the length of a full gate, so only an act that may have
/// written fences it; an ordinary check of a turn, open for one command,
/// keeps the wider rule (`engram_mark_checks_overlapped_by`).
#[derive(Clone, Copy, Debug)]
enum EngramWriterAct<'a> {
    /// It started a turn, or its turn's named source root became known, or a
    /// command whose start it reported ended: nothing here says it wrote, so
    /// no carried check is fenced. What it then runs or edits is reported as
    /// it happens, and a change to a watched file fences on its own
    /// (`note_engram_workspace_file_changes`).
    Presence,
    /// It reported a command that may write in `worktrees`
    /// (`engram_command_worktrees`, as placed by
    /// `engram_place_command_worktrees`: `unplaced` when TermAl could not
    /// name where it ran, so it counts where its session works; `None` only
    /// for a session whose own workdir was never resolved), with its line
    /// `ran` when TermAl was told it.
    Command {
        worktrees: &'a [Option<String>],
        ran: Option<&'a str>,
        unplaced: bool,
    },
    /// A command of its ended whose start TermAl was never told: it may have
    /// written wherever the session writes (`engram_writer_worktrees`).
    UnreportedCommand,
    /// It reported a file edit, which names no path: it may have written
    /// wherever the session writes.
    Edit,
}

/// Fences the carried checks of every session but the one at `writer` for
/// what that session did (`act`), where it may have written under them:
/// - a command fences the carried checks in the worktrees it may write in
///   (one TermAl cannot place counts where its session works,
///   `engram_place_command_worktrees`), unless it is a Claude session's line
///   that only reads (`engram_session_command_reads_only`);
/// - a file edit, and a command whose start was never reported, fence those
///   in `writer_worktrees`, the worktrees the session writes in;
/// - its presence alone fences none.
/// The cause names the session and what it did, a command by its program and
/// the digest of its line (`engram_command_name`), and says whether it was
/// placed in the check's worktree or could not be placed and may have run
/// there.
fn engram_fence_carried_for_other_session(
    inner: &mut StateInner,
    writer: usize,
    writer_worktrees: &[Option<String>],
    act: EngramWriterAct<'_>,
) {
    // Most commands run while no other session carries a gate that a write
    // could still reach: nothing is read or formatted for them.
    let open = |carried: &EngramCarriedCheck| {
        carried.fence.is_none() && carried.terminal_digest.is_none()
    };
    if matches!(act, EngramWriterAct::Presence)
        || !inner.sessions.iter().enumerate().any(|(other, record)| {
            other != writer && record.engram.carried_checks.iter().any(open)
        })
    {
        return;
    }
    let record = &inner.sessions[writer];
    let who = format!("session {} ({})", record.session.name, record.session.id);
    let (worktrees, there, unplaced) = match act {
        EngramWriterAct::Presence => return,
        EngramWriterAct::Command {
            worktrees,
            ran,
            unplaced,
        } => {
            if engram_session_command_reads_only(record, ran) {
                return;
            }
            let (command, line) = match ran {
                Some(ran) => engram_command_name(ran),
                None => (
                    "a command".to_owned(),
                    "(its line TermAl was not told)".to_owned(),
                ),
            };
            let nowhere = format!("{who} ran {command} that TermAl could not place {line}");
            (
                worktrees,
                if unplaced {
                    format!(
                        "{who} ran {command} that TermAl could not place and that may have run \
                         there {line}"
                    )
                } else {
                    format!("{who} ran {command} there {line}")
                },
                nowhere,
            )
        }
        EngramWriterAct::UnreportedCommand => (
            writer_worktrees,
            format!("{who} ran a command there whose start TermAl was not told"),
            format!("{who} ran a command that TermAl could not place, whose start it was not told"),
        ),
        EngramWriterAct::Edit => (
            writer_worktrees,
            format!("{who} reported a file edit there"),
            format!("{who} reported a file edit that TermAl could not place"),
        ),
    };
    for (other, record) in inner.sessions.iter_mut().enumerate() {
        if other == writer {
            continue;
        }
        for carried in &mut record.engram.carried_checks {
            if !open(carried) {
                continue;
            }
            let root = engram_path_key(&carried.check.target.root);
            let named = worktrees
                .iter()
                .any(|worktree| worktree.as_deref() == Some(root.as_str()));
            if named {
                carried.fence = Some(there.clone());
            } else if worktrees.iter().any(Option::is_none) {
                carried.fence = Some(unplaced.clone());
            }
        }
    }
}

/// Whether the Bash line `ran`, started from `workdir`, only reads: the
/// launcher's `summary` of a run, or what the read-only reviewer policy
/// allows (`claude_bash_command_is_read_only`), which fails closed. The
/// one-call form's `pushd "DIR" &&` is read as running the rest in `DIR`.
fn engram_command_reads_only(ran: &str, workdir: &str) -> bool {
    let (cwd, line) = match engram_one_call_prefix(ran.trim()) {
        Some((directory, rest)) => (directory, rest.trim().to_owned()),
        None => (workdir.to_owned(), ran.trim().to_owned()),
    };
    engram_is_launcher_summary(&line, &cwd) || claude_bash_command_is_read_only(&line, &cwd)
}

/// Whether `line`, run from `cwd`, is `node scripts/test-launcher.mjs summary
/// RUN_DIRECTORY` and nothing else: the launcher's summary only reads a run's
/// records. The script must be the repository's launcher as run from its root:
/// `scripts/test-launcher.mjs` relative to `cwd`, or that same path written
/// absolute. Judged on the paths as written, without touching the file system.
fn engram_is_launcher_summary(line: &str, cwd: &str) -> bool {
    if line.contains(['|', '&', ';', '<', '>', '`', '\n', '\r']) || line.contains("$(") {
        return false;
    }
    let Some(words) = engram_shell_words(line) else {
        return false;
    };
    let [node, script, mode, _] = words.as_slice() else {
        return false;
    };
    if engram_program_name(node) != "node" || mode != "summary" {
        return false;
    }
    let script = script.replace('\\', "/");
    let relative = script.strip_prefix("./").unwrap_or(&script);
    relative == "scripts/test-launcher.mjs"
        || (FsPath::new(&script).is_absolute()
            && engram_path_key(FsPath::new(&script))
                == engram_path_key(&FsPath::new(cwd).join("scripts").join("test-launcher.mjs")))
}

/// Where the launcher keeps the runs of the worktree at `root`: its Git
/// directory's `review-runs` (`git rev-parse --git-path review-runs`). A
/// linked worktree's `.git` is a file naming its Git directory.
fn engram_git_run_directory(root: &FsPath) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    let metadata = fs::metadata(&dot_git).ok()?;
    if metadata.is_dir() {
        return Some(dot_git.join("review-runs"));
    }
    if metadata.len() > 4096 {
        return None;
    }
    let text = fs::read_to_string(&dot_git).ok()?;
    let git_dir = text.lines().next()?.strip_prefix("gitdir:")?.trim();
    let git_dir = PathBuf::from(git_dir);
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        root.join(git_dir)
    };
    Some(git_dir.join("review-runs"))
}

/// Reads a launcher record, bounded (`ENGRAM_CARRIED_RECORD_MAX_BYTES`).
fn engram_read_launcher_record(path: &FsPath) -> Option<Vec<u8>> {
    use std::io::Read;
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(ENGRAM_CARRIED_RECORD_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= ENGRAM_CARRIED_RECORD_MAX_BYTES).then_some(bytes)
}

/// Parses an RFC 3339 time as the launcher and the host write it.
fn engram_parse_time(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.with_timezone(&chrono::Utc))
}

/// The run of the gate session `session_id` launched at `launched_at` in the
/// worktree at `root`: among the full-gate runs whose request names that
/// root (both canonical, so a launch through a link or an alias of the
/// directory still finds its run), that started at or after the launch (less
/// `ENGRAM_CARRIED_LAUNCH_SLACK_SECONDS`) and, when the request names its
/// owner (the launching session's `TERMAL_SESSION_ID`), are owned by that
/// session, the earliest not in `claimed` (runs other carried checks of the
/// session took, or runs settled or dropped launches used) or `ruled_out`. A
/// run that started before the launch, within the slack, is taken only when
/// it had not ended by the launch: one that had is an earlier run. Also
/// returns the runs read that can never be this launch's, and, for a run
/// found whose record this search already read as terminal, that record's
/// digest: the first terminal read, which the caller pins.
fn engram_find_carried_run(
    session_id: &str,
    root: &FsPath,
    launched_at: &str,
    claimed: &[PathBuf],
    ruled_out: &std::collections::BTreeSet<PathBuf>,
) -> (Option<PathBuf>, Vec<PathBuf>, Option<String>) {
    let mut newly_ruled_out = Vec::new();
    let (Some(stamped), Some(runs)) = (
        engram_parse_time(launched_at),
        engram_git_run_directory(root),
    ) else {
        return (None, newly_ruled_out, None);
    };
    let Ok(entries) = fs::read_dir(&runs) else {
        return (None, newly_ruled_out, None);
    };
    let root_key = engram_path_key(root);
    let mut best: Option<(chrono::DateTime<chrono::Utc>, PathBuf, Option<String>)> = None;
    for entry in entries.flatten() {
        let directory = entry.path();
        let named_as_run = directory
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("test-"));
        if !named_as_run || claimed.contains(&directory) || ruled_out.contains(&directory) {
            continue;
        }
        let Some(request) = engram_read_launcher_record(&directory.join("request.json"))
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        else {
            continue;
        };
        let same_root = request
            .get("root")
            .and_then(Value::as_str)
            .is_some_and(|run_root| {
                // The launcher writes the directory as given, not resolved.
                // A network path is keyed as written: resolving it can block
                // for a network timeout while the poll lock is held.
                let resolved = if engram_network_path(run_root) {
                    PathBuf::from(run_root)
                } else {
                    fs::canonicalize(run_root).unwrap_or_else(|_| PathBuf::from(run_root))
                };
                engram_path_key(&resolved) == root_key
            });
        let full = request.get("full").and_then(Value::as_bool) == Some(true);
        // Another session's run in the same worktree is never this one's.
        let owned = request
            .get("owner")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|owner| !owner.is_empty())
            .is_none_or(|owner| owner == session_id);
        let Some(started) = request
            .get("started")
            .and_then(Value::as_str)
            .and_then(engram_parse_time)
        else {
            continue;
        };
        let launched = stamped - chrono::Duration::seconds(ENGRAM_CARRIED_LAUNCH_SLACK_SECONDS);
        // A run that started in the slack before the stamp but had already
        // ended by it belongs to an earlier launch.
        // What this read found, when it found the record terminal, is the
        // first terminal read: it is returned to be pinned, never read again
        // and replaced.
        let in_slack = started < stamped;
        let record_bytes = in_slack
            .then(|| engram_read_launcher_record(&directory.join("results.json")))
            .flatten();
        let ended_before = record_bytes
            .as_deref()
            .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
            .and_then(|record| {
                record
                    .get("ended")
                    .and_then(Value::as_str)
                    .and_then(engram_parse_time)
            })
            .is_some_and(|ended| ended < stamped);
        let first_terminal = record_bytes.as_deref().and_then(engram_terminal_digest_of);
        if !(same_root && full && owned && started >= launched) || ended_before {
            // A request once written names its root, mode, owner and start
            // for good.
            newly_ruled_out.push(directory);
            continue;
        }
        if best.as_ref().is_none_or(|(at, _, _)| started < *at) {
            best = Some((started, directory, first_terminal));
        }
    }
    match best {
        Some((_, directory, first_terminal)) => (Some(directory), newly_ruled_out, first_terminal),
        None => (None, newly_ruled_out, None),
    }
}

/// The digest of the run's terminal record at `directory`, or `None` while
/// the run is still going (or has no readable record). A record is terminal
/// once it has `ended`, or a state other than `running`, whatever that state
/// is: a run stopped or interrupted has ended too, however a launcher settled
/// it, and is then refused
/// (`engram_read_carried_run`) rather than carried until it expires.
fn engram_terminal_record_digest(directory: &FsPath) -> Option<String> {
    let bytes = engram_read_launcher_record(&directory.join("results.json"))?;
    engram_terminal_digest_of(&bytes)
}

/// The digest of `bytes`, a run's `results.json`, when it is a terminal
/// record (`engram_terminal_record_digest`).
fn engram_terminal_digest_of(bytes: &[u8]) -> Option<String> {
    let record: Value = serde_json::from_slice(bytes).ok()?;
    let ended = record.get("ended").and_then(Value::as_str).is_some();
    let settled = record
        .get("state")
        .and_then(Value::as_str)
        .is_some_and(|state| state != "running");
    (ended || settled).then(|| sha256_hex(bytes))
}

/// The process responsible for the run at `directory`: `results.json`'s pid,
/// else `request.json`'s `creatorPid`, with when that record was written.
fn engram_carried_run_responsible_pid(
    directory: &FsPath,
) -> Option<(u32, Option<std::time::SystemTime>)> {
    let recorded = |name: &str, key: &str| {
        let path = directory.join(name);
        let pid = engram_read_launcher_record(&path)
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|record| record.get(key).and_then(Value::as_u64))
            .and_then(|pid| u32::try_from(pid).ok())?;
        let written = fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok();
        Some((pid, written))
    };
    recorded("results.json", "pid").or_else(|| recorded("request.json", "creatorPid"))
}

/// At most this many responsible processes are checked in one read of a
/// run: a detached run's creator hands over to its worker once.
const ENGRAM_CARRIED_LIVENESS_CHECKS_MAX: usize = 3;

/// What one read of the run at `directory` finds: the digest of its terminal
/// record (`engram_terminal_record_digest`), or none and whether the process
/// responsible for it is provably gone, by the run index's own test
/// (`test_run_process_writer_may_be_alive`), which counts only proof.
fn engram_carried_run_progress(directory: &FsPath) -> (Option<String>, bool) {
    engram_carried_run_progress_with(directory, test_run_process_writer_may_be_alive)
}

/// As `engram_carried_run_progress`, judging liveness by `may_be_alive`. The
/// launcher may write its terminal record and exit between a read that found
/// none and the check that finds its process gone, so the record is read
/// again after that check, and only a run still not terminal whose responsible
/// process is the one checked is gone. A read that names another responsible
/// process (a detached run's creator handing over to its worker) was made
/// before that process's check, so it is judged the same way again. A run
/// with no recorded pid is never judged gone.
fn engram_carried_run_progress_with(
    directory: &FsPath,
    may_be_alive: impl Fn(u32, Option<std::time::SystemTime>) -> bool,
) -> (Option<String>, bool) {
    if let Some(digest) = engram_terminal_record_digest(directory) {
        return (Some(digest), false);
    }
    let mut responsible = engram_carried_run_responsible_pid(directory);
    for _ in 0..ENGRAM_CARRIED_LIVENESS_CHECKS_MAX {
        let Some((pid, written)) = responsible else {
            return (None, false);
        };
        if may_be_alive(pid, written) {
            return (None, false);
        }
        if let Some(digest) = engram_terminal_record_digest(directory) {
            return (Some(digest), false);
        }
        responsible = engram_carried_run_responsible_pid(directory);
        if responsible.map(|(after, _)| after) == Some(pid) {
            return (None, true);
        }
    }
    (None, false)
}

/// Why a check whose run two terminal reads saw differently is refused.
const ENGRAM_CARRIED_TERMINAL_CONFLICT: &str = concat!(
    "two reads that found its run terminal saw different records, so the host ",
    "cannot tell which it read first"
);

/// Why a run that ended neither passed nor failed is refused.
const ENGRAM_CARRIED_RUN_NEITHER: &str = concat!(
    "its run ended neither passed nor failed (it was stopped or interrupted), ",
    "so it is neither credited nor recorded"
);

/// Why a passed run with no test stage is refused
/// (`engram_launcher_test_stages`).
const ENGRAM_CARRIED_RUN_NO_TEST_STAGE: &str = concat!(
    "its run requested no test stage: its request record marks no stage `kind` `test`, ",
    "or, giving no kinds, names neither `rust-tests` nor `vitest`"
);

/// What a settled run says, when the host may credit it.
#[derive(Debug, PartialEq, Eq)]
struct EngramCarriedRunVerdict {
    passed: bool,
    exit_code: i64,
    ended: String,
    run_id: String,
    fingerprint: String,
    stages: Vec<String>,
    /// Each stage of the terminal record in the launcher's own line,
    /// `NAME: STATE exit=CODE`, which the host reads as the launcher's
    /// result lines (`engram_is_result_line`), so the run's exit is its
    /// stages' own even for the one-call form.
    stage_lines: Vec<String>,
}

/// Reads the run's terminal record at `directory`, which must have the
/// digest the host first read (`digest`), and returns what it says, or why it
/// cannot be credited. A passed run needs every stage its request lists
/// passed with code 0 and no error, among them a test stage by its request
/// record (`engram_launcher_test_stages`), as a foreground gate needs; a
/// failed one is recorded failed, whichever stage failed. Either way the
/// expected, before and after input fingerprints must agree, and so must the
/// request's.
fn engram_read_carried_run(
    directory: &FsPath,
    digest: &str,
) -> Result<EngramCarriedRunVerdict, &'static str> {
    let bytes = engram_read_launcher_record(&directory.join("results.json"))
        .ok_or("its terminal record could not be read")?;
    if sha256_hex(&bytes) != digest {
        return Err("its terminal record changed after the host first read it as terminal");
    }
    let record: Value =
        serde_json::from_slice(&bytes).map_err(|_| "its terminal record is not valid JSON")?;
    // An interrupted run is neither a pass nor a failure, whatever state it
    // was settled with (the launcher's `recover` writes failed and
    // interrupted), and neither is a run stopped in any other state.
    let interrupted = record.get("interrupted").and_then(Value::as_bool) == Some(true);
    let settled = matches!(
        record.get("state").and_then(Value::as_str),
        Some("passed" | "failed")
    );
    if interrupted || !settled {
        return Err(ENGRAM_CARRIED_RUN_NEITHER);
    }
    let request: Value = engram_read_launcher_record(&directory.join("request.json"))
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or("its request record could not be read")?;
    let text = |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    let run_id = text(&record, "runId").ok_or("its terminal record names no run")?;
    let named_directory = directory.file_name().and_then(|name| name.to_str());
    if named_directory != Some(run_id.as_str())
        || text(&request, "runId").as_deref() != Some(&run_id)
    {
        return Err("its records do not name the run directory they are in");
    }
    let expected = text(&record, "expectedFingerprint")
        .ok_or("its terminal record has no input fingerprint")?;
    if text(&record, "before").is_none() || text(&record, "after").is_none() {
        return Err(concat!(
            "its terminal record has no input fingerprint from before or after its stages ",
            "(the run ended before the launcher measured its input), so it is neither ",
            "credited nor recorded"
        ));
    }
    let consistent = text(&request, "expectedFingerprint").as_deref() == Some(expected.as_str())
        && text(&record, "before").as_deref() == Some(expected.as_str())
        && text(&record, "after").as_deref() == Some(expected.as_str());
    if !consistent {
        return Err("its input fingerprint after the run differs from the one before");
    }
    let ended = text(&record, "ended").ok_or("its terminal record has no end time")?;
    let exit_code = record
        .get("exitCode")
        .and_then(Value::as_i64)
        .ok_or("its terminal record has no exit code")?;
    let requested = request
        .get("stages")
        .and_then(Value::as_array)
        .map(|stages| {
            stages
                .iter()
                .filter_map(|stage| stage.get("name").and_then(Value::as_str).map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let stages = record
        .get("stages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let stage_passed = |name: &str| {
        stages.iter().any(|stage| {
            stage.get("name").and_then(Value::as_str) == Some(name)
                && stage.get("state").and_then(Value::as_str) == Some("passed")
                && stage.get("code").and_then(Value::as_i64) == Some(0)
                && stage.get("error").is_none_or(Value::is_null)
        })
    };
    let passed = match text(&record, "state").as_deref() {
        Some("passed") => {
            if requested.is_empty()
                || exit_code != 0
                || !requested.iter().all(|name| stage_passed(name))
            {
                return Err(
                    "its terminal record says passed, but not every stage it requested passed with code 0",
                );
            }
            if engram_launcher_test_stages(&request).is_empty() {
                return Err(ENGRAM_CARRIED_RUN_NO_TEST_STAGE);
            }
            true
        }
        Some("failed") => false,
        _ => return Err(ENGRAM_CARRIED_RUN_NEITHER),
    };
    let stage_lines = stages
        .iter()
        .filter_map(|stage| {
            let name = stage.get("name").and_then(Value::as_str)?;
            let state = stage.get("state").and_then(Value::as_str)?;
            let code = stage
                .get("code")
                .and_then(Value::as_i64)
                .map(|code| format!(" exit={code}"))
                .unwrap_or_default();
            Some(format!("{name}: {state}{code}"))
        })
        .collect();
    Ok(EngramCarriedRunVerdict {
        passed,
        exit_code,
        ended,
        run_id,
        fingerprint: expected,
        stages: requested,
        stage_lines,
    })
}

/// The result lines a settled run's verification carries: the verdict, each
/// stage's line, the run directory and the input fingerprint.
fn engram_carried_result_lines(
    verdict: &EngramCarriedRunVerdict,
    directory: &FsPath,
) -> Vec<String> {
    std::iter::once(format!(
        "full gate {} {}: {} stage{} ({})",
        verdict.run_id,
        if verdict.passed { "passed" } else { "failed" },
        verdict.stages.len(),
        if verdict.stages.len() == 1 { "" } else { "s" },
        verdict.stages.join(", ")
    ))
    .chain(verdict.stage_lines.iter().cloned())
    .chain([
        format!("run directory {}", directory.display()),
        format!("input fingerprint {}", verdict.fingerprint),
    ])
    .collect()
}

/// Settles `carried`, whose run the host has read as terminal: the record
/// must still be the one first read, it must say what the host may credit,
/// and a fresh source basis must equal the launch's. Waits for the snapshots
/// until `deadline`. Returns the resolved check for the report, `None` when
/// a snapshot was not ready in time (the check stays carried, fenced and
/// pinned, for the next checkpoint), or why it is refused.
fn engram_settle_carried_check(
    carried: &EngramCarriedCheck,
    workers: &Arc<std::sync::atomic::AtomicUsize>,
    deadline: std::time::Instant,
) -> Result<Option<EngramResolvedCheck>, String> {
    if carried.ambiguous {
        return Err(ENGRAM_CARRIED_AMBIGUOUS.to_owned());
    }
    if carried.terminal_conflict {
        return Err(ENGRAM_CARRIED_TERMINAL_CONFLICT.to_owned());
    }
    if carried.terminal_digest.is_none() && carried.run_directory.is_none() && carried.run_missing {
        return Err(
            "no run of it appeared within ten minutes of its launch, so it started none".to_owned(),
        );
    }
    if carried.terminal_digest.is_none() && carried.launcher_gone {
        return Err(concat!(
            "its launcher ended without recording a terminal result; settle the run with ",
            "the launcher's `recover` and run the gate again"
        )
        .to_owned());
    }
    let (Some(directory), Some(digest)) = (&carried.run_directory, &carried.terminal_digest) else {
        return Err("its run was not found".to_owned());
    };
    let verdict = engram_read_carried_run(directory, digest).map_err(str::to_owned)?;
    // A snapshot that finished without a basis failed, and never will give
    // one; only one not ready in time is tried again at the next checkpoint.
    let start = match carried.check.start_basis.wait_until(deadline) {
        Some(Some(start)) => start,
        Some(None) => {
            return Err("the host could not take its source snapshot at the launch".to_owned());
        }
        None => return Ok(None),
    };
    let end_basis = engram_spawn_basis_capture(carried.check.target.basis_place(), workers);
    let finish = match end_basis.wait_until(deadline) {
        Some(Some(finish)) => finish,
        Some(None) => {
            return Err("the host could not take its source snapshot at settlement".to_owned());
        }
        None => return Ok(None),
    };
    if start != finish {
        return Err(
            "the host's source basis at settlement differs from its basis at the launch".to_owned(),
        );
    }
    let basis = start;
    let mut check = carried.check.clone();
    let end = EngramTurnCheckEnd {
        completed_at: verdict.ended.clone(),
        exit: EngramCommandExit::Code(verdict.exit_code),
        result_lines: engram_carried_result_lines(&verdict, directory),
        showed_passing_tests: verdict.passed,
        end_basis,
    };
    check.end = Some(end.clone());
    Ok(Some(EngramResolvedCheck {
        toolchain: check.toolchain.wait_until(deadline).flatten(),
        check,
        end,
        outcome: if verdict.passed {
            EngramExecutionOutcome::Succeeded
        } else {
            EngramExecutionOutcome::Failed
        },
        basis,
        ran_successfully: verdict.passed,
    }))
}

/// A carried check's outcome at a checkpoint, by the grant and sequence that
/// name it (`engram_settle_carried_check`).
type EngramCarriedSettlement = (String, usize, Result<Option<EngramResolvedCheck>, String>);

/// Settles each of `carried` off the lock, within `deadline`.
fn engram_settle_carried_checks(
    carried: &[EngramCarriedCheck],
    workers: &Arc<std::sync::atomic::AtomicUsize>,
    deadline: std::time::Instant,
) -> Vec<EngramCarriedSettlement> {
    carried
        .iter()
        .map(|carried| {
            (
                carried.check.grant_id.clone(),
                carried.check.sequence,
                engram_settle_carried_check(carried, workers, deadline),
            )
        })
        .collect()
}

/// Why `launched`, whose command only launched its run, cannot be carried in
/// the session of `record`, as the line its holder gets: the session is a
/// delegated one, its turn is not measured in a named source root, it ran
/// outside that root, or another command or writer was active in its worktree
/// as it launched. `None` when it can be carried. A line that tells the
/// holder to launch again names the ambiguity window's wait
/// (`ENGRAM_CARRIED_AMBIGUITY_SECONDS`), since this launch is recorded as
/// unmatched.
fn engram_carry_refusal(record: &SessionRecord, launched: &EngramTurnCheck) -> Option<String> {
    let engram = &record.engram;
    let fingerprint = engram_check_fingerprint(&launched.command);
    // A delegated session is measured in its workdir and names no root, so a
    // background gate is never carried for it.
    if record.session.parent_delegation_id.is_some() {
        return Some(format!(
            "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} a background full gate (check {fingerprint}) will \
             earn no credit: a delegated session's turns are measured in its workdir, where a \
             background gate is not carried. Ask the root session that holds the claim to run \
             the gate in its named source root."
        ));
    }
    let Some(turn_root) = engram
        .active_turn_source_root
        .as_ref()
        .filter(|_| engram.work_binding.is_some())
    else {
        return Some(format!(
            "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} a background full gate (check {fingerprint}) will \
             earn no credit: its turn is not measured in a named source root. Name the worktree \
             with termal_name_source_root, then launch it in a later turn, two minutes or more \
             after this launch."
        ));
    };
    if engram_path_key(FsPath::new(&turn_root.root)) != engram_path_key(&launched.target.root) {
        return Some(format!(
            "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} a background full gate (check {fingerprint}) will \
             earn no credit: it ran outside the named source root this turn is measured in."
        ));
    }
    if launched.overlapped {
        return Some(format!(
            "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} a background full gate (check {fingerprint}) will \
             earn no credit: another command or another writable session was active in its \
             worktree when it launched. Launch it again on its own, two minutes or more after \
             this launch."
        ));
    }
    None
}

/// Keeps `launched`, whose command only launched its run, as a carried check
/// of `record`, when its turn is bound to claimed work and measured in the
/// named root it ran in. Past the limit the oldest carried check is dropped
/// and its holder told. Returns the line for the holder when it is not
/// carried.
fn engram_carry_check(record: &mut SessionRecord, launched: EngramTurnCheck) -> Option<String> {
    if let Some(line) = engram_carry_refusal(record, &launched) {
        // The launch may still have started a run, which a later launch in
        // the same worktree must not take as its own.
        engram_note_unmatched_launch(record, &launched.target.root, &launched.started_at);
        return Some(line);
    }
    let engram = &mut record.engram;
    let (Some(binding), Some(turn_root)) = (&engram.work_binding, &engram.active_turn_source_root)
    else {
        return None;
    };
    let root_key = engram_path_key(&launched.target.root);
    let launched_at = engram_parse_time(&launched.started_at);
    let recent = |at: &str| match (engram_parse_time(at), launched_at) {
        (Some(at), Some(launched)) => {
            (launched - at).num_seconds() <= ENGRAM_CARRIED_AMBIGUITY_SECONDS
        }
        _ => true,
    };
    let ambiguous = engram.carried_checks.iter().any(|carried| {
        carried.run_directory.is_none()
            && engram_path_key(&carried.check.target.root) == root_key
            && recent(&carried.check.started_at)
    }) || engram
        .carried_unmatched_launches
        .iter()
        .any(|(root, at)| *root == root_key && recent(at));
    // A change the watcher saw while the gate was being launched fences it.
    let fence = launched
        .watcher_fence
        .as_ref()
        .map(|cause| format!("{cause} (while it was being launched)"));
    let carried = EngramCarriedCheck {
        work_id: binding.work_id.clone(),
        claim_id: binding.claim_id.clone(),
        root_generation: turn_root.generation,
        check: launched,
        run_directory: None,
        terminal_digest: None,
        launcher_gone: false,
        run_missing: false,
        terminal_conflict: false,
        ambiguous,
        ruled_out_runs: std::collections::BTreeSet::new(),
        fence,
    };
    let mut told = None;
    if engram.carried_checks.len() >= ENGRAM_CARRIED_CHECK_LIMIT {
        let dropped = engram.carried_checks.remove(0);
        told = Some(dropped.refusal_line("a newer background gate took its place"));
        engram.carried_checks.push(carried);
        match dropped.run_directory {
            Some(directory) => engram_note_consumed_runs(record, vec![directory]),
            None => engram_note_unmatched_launch(
                record,
                &dropped.check.target.root,
                &dropped.check.started_at,
            ),
        }
        return told;
    }
    engram.carried_checks.push(carried);
    told
}

/// Keeps the run directories `used` by carried checks of `record` that
/// settled or were dropped, the newest `ENGRAM_CARRIED_CONSUMED_RUN_LIMIT`,
/// so no later launch takes one of them.
fn engram_note_consumed_runs(record: &mut SessionRecord, used: Vec<PathBuf>) {
    let consumed = &mut record.engram.carried_consumed_runs;
    for directory in used {
        if !consumed.contains(&directory) {
            consumed.push(directory);
        }
    }
    let excess = consumed
        .len()
        .saturating_sub(ENGRAM_CARRIED_CONSUMED_RUN_LIMIT);
    consumed.drain(..excess);
}

/// Keeps a background full-gate launch of `record` in the worktree at `root`,
/// launched at `launched_at`, that left without a run found for it, so a
/// later launch there within the run search time is refused as ambiguous
/// rather than taking that run (`engram_carry_check`).
fn engram_note_unmatched_launch(record: &mut SessionRecord, root: &FsPath, launched_at: &str) {
    let unmatched = &mut record.engram.carried_unmatched_launches;
    unmatched.push((engram_path_key(root), launched_at.to_owned()));
    let excess = unmatched
        .len()
        .saturating_sub(ENGRAM_CARRIED_UNMATCHED_LAUNCH_LIMIT);
    unmatched.drain(..excess);
}

/// Whether `command` is the test launcher's full gate, whose launch may be
/// carried (`engram_launch_disposition`).
fn engram_check_is_launcher_full(command: &EngramCheckCommand) -> bool {
    engram_launcher_words(command).is_some_and(|words| {
        words.get(2).is_some_and(|mode| {
            mode.split([';', '&', '|', '<', '>', '(', ')']).next() == Some("full")
        })
    })
}

/// The lines telling the holder in `record` that each of its carried checks
/// earned no credit because `why`, for a reset of its Engram state that
/// drops them (a project's Engram settings changed, or the project was
/// removed). The caller logs them and sets them again after the reset.
fn engram_carried_checks_reset_lines(record: &SessionRecord, why: &str) -> Vec<String> {
    record
        .engram
        .carried_checks
        .iter()
        .map(|carried| carried.refusal_line(why))
        .collect()
}

/// Fences every carried check of `record` whose worktree `worktrees` may
/// hold (`engram_worktrees_may_hold`), with `cause`: a write the host
/// observed may have reached what the run tested. A check whose run the host
/// already read as terminal is left alone: a write after the run ended cannot
/// reach what it tested, and one still there at settlement changes the
/// source basis, which settlement compares.
fn engram_fence_carried_checks(
    record: &mut SessionRecord,
    worktrees: &[Option<String>],
    cause: &str,
) {
    engram_fence_carried_checks_in(record, worktrees, cause, false);
}

/// As `engram_fence_carried_checks`, fencing a check read as terminal too
/// when `even_after_end`: for a write reported late, which may have landed
/// while the run was going (the workspace watcher's).
fn engram_fence_carried_checks_in(
    record: &mut SessionRecord,
    worktrees: &[Option<String>],
    cause: &str,
    even_after_end: bool,
) {
    for carried in &mut record.engram.carried_checks {
        if carried.fence.is_none()
            && (even_after_end || carried.terminal_digest.is_none())
            && engram_worktrees_may_hold(worktrees, &engram_path_key(&carried.check.target.root))
        {
            carried.fence = Some(cause.to_owned());
        }
    }
}

/// Fences every carried check of `record` still running for a file edit it
/// reported, which names no path.
fn engram_fence_carried_checks_for_edit(record: &mut SessionRecord) {
    for carried in &mut record.engram.carried_checks {
        if carried.fence.is_none() && carried.terminal_digest.is_none() {
            carried.fence = Some("this session reported a file edit".to_owned());
        }
    }
}

/// Whether `carried` is past `ENGRAM_CARRIED_CHECK_TTL_SECONDS` at `now`
/// (or its launch time cannot be read).
fn engram_carried_check_expired(
    carried: &EngramCarriedCheck,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    engram_parse_time(&carried.check.started_at)
        .is_none_or(|launched| (now - launched).num_seconds() > ENGRAM_CARRIED_CHECK_TTL_SECONDS)
}

/// The carried checks of `record` to settle at the checkpoint of its current
/// turn: bound to the same claim, measured in the same named-root
/// generation, read as terminal and not fenced. They are settled off the
/// lock (`engram_settle_carried_check`).
fn engram_carried_checks_to_settle(record: &SessionRecord) -> Vec<EngramCarriedCheck> {
    let engram = &record.engram;
    let (Some(binding), Some(turn_root)) = (&engram.work_binding, &engram.active_turn_source_root)
    else {
        return Vec::new();
    };
    let now = chrono::Utc::now();
    engram
        .carried_checks
        .iter()
        .filter(|carried| {
            !engram_carried_check_expired(carried, now)
                && carried.claim_id == binding.claim_id
                && carried.root_generation == turn_root.generation
                && (carried.terminal_digest.is_some()
                    || carried.launcher_gone
                    || carried.run_missing
                    || carried.ambiguous)
                && carried.fence.is_none()
        })
        .cloned()
        .collect()
}

/// Under the lock at the checkpoint of `record`'s turn: takes the outcomes
/// of the carried checks settled off the lock (`settled`, by grant and
/// sequence), keeping a credit only while the live record is still carried
/// and unfenced, and keeping carried one whose snapshots were not ready in
/// time; and drops every carried check that can no longer be
/// credited: fenced, older than `ENGRAM_CARRIED_CHECK_TTL_SECONDS` at `now`,
/// or whose claim no longer names the same root generation (`generations`,
/// aligned with the carried checks: the generation the host now records for
/// each one's claim, `None` when it records none). Returns the credited
/// checks for the report, in the order they launched, and the lines for the
/// holder.
fn engram_take_settled_carried_checks(
    record: &mut SessionRecord,
    mut settled: Vec<EngramCarriedSettlement>,
    generations: &[Option<u64>],
    now: chrono::DateTime<chrono::Utc>,
) -> (Vec<EngramResolvedCheck>, Vec<String>) {
    let mut credited = Vec::new();
    let mut lines = Vec::new();
    let carried_checks = std::mem::take(&mut record.engram.carried_checks);
    let mut left = Vec::new();
    let mut unmatched = Vec::new();
    for (position, carried) in carried_checks.into_iter().enumerate() {
        // Every check that leaves below leaves its run used, or, with none
        // found, its launch unmatched.
        left.extend(carried.run_directory.clone());
        if carried.run_directory.is_none() {
            unmatched.push((
                carried.check.target.root.clone(),
                carried.check.started_at.clone(),
            ));
        }
        let outcome = settled
            .iter()
            .position(|(grant_id, sequence, _)| {
                *grant_id == carried.check.grant_id && *sequence == carried.check.sequence
            })
            .map(|found| settled.swap_remove(found).2);
        if let Some(cause) = &carried.fence {
            lines.push(carried.refusal_line(&format!(
                "the host saw something that may have written in its worktree before it \
                 settled: {cause}"
            )));
            continue;
        }
        if generations.get(position).copied().flatten() != Some(carried.root_generation) {
            lines.push(carried.refusal_line(
                "its claim was released, or its source root renamed or cleared, before it settled",
            ));
            continue;
        }
        // A poll that read off the lock may have found the conflict after
        // this checkpoint copied the check to settle it.
        if carried.terminal_conflict {
            lines.push(carried.refusal_line(ENGRAM_CARRIED_TERMINAL_CONFLICT));
            continue;
        }
        // Six hours after its launch a gate is refused, whatever a
        // settlement made of it.
        if engram_carried_check_expired(&carried, now) {
            lines.push(carried.refusal_line(if carried.terminal_digest.is_some() {
                concat!(
                    "it was not settled within six hours of its launch (a background gate is ",
                    "settled only at a checkpoint on the same claim and source root)"
                )
            } else {
                "its run did not end within six hours of its launch"
            }));
            continue;
        }
        match outcome {
            Some(Ok(Some(resolved))) => {
                credited.push(resolved);
                continue;
            }
            Some(Err(why)) => {
                lines.push(carried.refusal_line(&why));
                continue;
            }
            Some(Ok(None)) | None => {}
        }
        if let Some(directory) = &carried.run_directory {
            left.retain(|used| used != directory);
        } else {
            unmatched.retain(|(root, at)| {
                *root != carried.check.target.root || *at != carried.check.started_at
            });
        }
        record.engram.carried_checks.push(carried);
    }
    engram_note_consumed_runs(record, left);
    for (root, at) in unmatched {
        engram_note_unmatched_launch(record, &root, &at);
    }
    credited.sort_by(|left, right| left.check.started_at.cmp(&right.check.started_at));
    (credited, lines)
}

/// Under the lock at the checkpoint of the session at `index` (`session_id`):
/// takes its settled carried checks (`engram_take_settled_carried_checks`),
/// with the generation the host now records for each one's claim; tells the
/// holder, and logs, why any was refused or dropped; and puts the credited
/// ones first in `resolved`, since they launched before this turn's checks.
/// Returns how many it put there, for `engram_trim_turn_checks`.
fn engram_credit_carried_checks(
    inner: &mut StateInner,
    index: usize,
    session_id: &str,
    settled: Vec<EngramCarriedSettlement>,
    resolved: &mut Vec<EngramResolvedCheck>,
) -> usize {
    let store = engram_project_for_session_locked(inner, session_id)
        .and_then(|project| project.engram.as_ref())
        .and_then(|settings| settings.authority_store_key.clone());
    let generations = inner.sessions[index]
        .engram
        .carried_checks
        .iter()
        .map(|carried| {
            store.as_ref().and_then(|store| {
                engram_work_source_root_for_claim(
                    &inner.engram_work_source_roots,
                    store,
                    &carried.work_id,
                    &carried.claim_id,
                )
                .map(|entry| entry.generation)
            })
        })
        .collect::<Vec<_>>();
    let record = inner
        .session_mut_by_index(index)
        .expect("session index should be valid");
    let (credited, lines) =
        engram_take_settled_carried_checks(record, settled, &generations, chrono::Utc::now());
    for line in lines {
        eprintln!("engram> session={session_id} {line}");
        record.engram.set_pending_source_root_line(line);
    }
    let carried_count = credited.len();
    resolved.splice(0..0, credited);
    carried_count
}

/// Trims this turn's checks in `resolved`, after its first `carried_count`
/// credited carried ones, so the report stays within
/// `ENGRAM_TURN_CHECK_LIMIT` where it can. The turn's own checks are already
/// held to that limit as they start (`note_engram_command_started`), so this
/// trims only for credited carried gates; it runs after the checks the host
/// cannot judge were withheld, so a withheld check makes room for one.
fn engram_trim_turn_checks(resolved: &mut Vec<EngramResolvedCheck>, carried_count: usize) {
    let carried_count = carried_count.min(resolved.len());
    let excess = resolved
        .len()
        .saturating_sub(ENGRAM_TURN_CHECK_LIMIT)
        .min(resolved.len() - carried_count);
    resolved.drain(carried_count..carried_count + excess);
}

/// `later`, a line about a check's credit, merged into `earlier`, one still
/// waiting for a prompt, as one line within
/// `ENGRAM_CHECK_CREDIT_LINE_MAX_BYTES`. The newest is kept: past the bound,
/// the front of what came before is cut, and the line says so.
fn engram_merge_credit_lines(earlier: &str, later: &str) -> String {
    let later = later
        .strip_prefix(ENGRAM_CHECK_CREDIT_LINE_PREFIX)
        .unwrap_or(later)
        .trim_start();
    let merged = format!("{earlier} Also: {later}");
    if merged.len() <= ENGRAM_CHECK_CREDIT_LINE_MAX_BYTES {
        return merged;
    }
    let cut =
        format!("{ENGRAM_CHECK_CREDIT_LINE_PREFIX} (earlier lines cut; the host log has them) ");
    let body = merged
        .strip_prefix(ENGRAM_CHECK_CREDIT_LINE_PREFIX)
        .unwrap_or(&merged);
    let budget = ENGRAM_CHECK_CREDIT_LINE_MAX_BYTES.saturating_sub(cut.len());
    let mut start = body.len().saturating_sub(budget);
    while !body.is_char_boundary(start) {
        start += 1;
    }
    format!("{cut}{}", body[start..].trim_start())
}

/// Whether a poll still reads the run of `carried` at `now`: its terminal
/// record is not yet read, and nothing already settled that it can never be
/// credited (a fence, a launcher gone, a run never found, its six hours
/// over, or its launch ambiguous), so the next checkpoint refuses it
/// without another read, and it finds no run.
fn engram_carried_check_polls(
    carried: &EngramCarriedCheck,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    carried.terminal_digest.is_none()
        && carried.fence.is_none()
        && !carried.ambiguous
        && !carried.launcher_gone
        && !carried.run_missing
        && engram_parse_time(&carried.check.started_at).is_some_and(|launched| {
            (now - launched).num_seconds() <= ENGRAM_CARRIED_CHECK_TTL_SECONDS
        })
}

/// Takes into `carried` what one poll read of its run (off the lock): the run
/// directory `found` (none yet, and `missing` when the search time is over),
/// the digest of its terminal record, whether its launcher is gone, and the
/// runs the search ruled out for good. The first terminal read is kept; a
/// later one that read another record marks a conflict, which refuses the
/// check (`engram_settle_carried_check`): polls read off the lock, so the one
/// that stores first need not be the one that read first.
fn engram_note_carried_run_read(
    carried: &mut EngramCarriedCheck,
    found: Option<PathBuf>,
    digest: Option<String>,
    launcher_gone: bool,
    missing: bool,
    ruled_out: Vec<PathBuf>,
) {
    carried.ruled_out_runs.extend(ruled_out);
    let Some(directory) = found else {
        carried.run_missing = missing;
        return;
    };
    if carried.run_directory.is_none() {
        carried.run_directory = Some(directory.clone());
    }
    if carried.run_directory.as_ref() != Some(&directory) {
        return;
    }
    match (&carried.terminal_digest, digest) {
        (None, digest) => {
            carried.launcher_gone = launcher_gone && digest.is_none();
            carried.terminal_digest = digest;
        }
        (Some(first), Some(read)) if *first != read => carried.terminal_conflict = true,
        (Some(_), _) => {}
    }
}

/// The scratch directory CLAUDE.md and AGENTS.md send every agent's scratch
/// files, throwaway stores, test homes and logs to, at a worktree's root.
/// The exemption assumes what this repository's .gitignore makes true: it is
/// git-ignored and nothing under it is tracked, so no change under it reaches
/// the launcher's input fingerprint or the host's source basis. Like the
/// watcher's own ignored names (target, dist, node_modules), it is not
/// checked per repository.
const ENGRAM_WORKTREE_SCRATCH_DIRECTORY: &str = ".tmp";

/// Whether a changed entry named `name` in the directory with exact key
/// `directory_key` lies under the scratch directory at the root of its own
/// worktree (exact key `worktree`), or is that directory itself. A worktree
/// nested under another checkout's scratch directory is its own worktree, so
/// a change in it is judged against its own root, not the enclosing one.
/// Both keys are exact (`engram_exact_path_key`) and the name is compared
/// exactly: an exemption must never widen by case folding, since on a
/// case-sensitive volume `.TMP` is another directory, which Git does not
/// ignore and which may hold source.
fn engram_is_worktree_scratch(worktree: &str, directory_key: &str, name: Option<&str>) -> bool {
    let worktree = worktree.trim_end_matches('/');
    let scratch = format!("{worktree}/{ENGRAM_WORKTREE_SCRATCH_DIRECTORY}");
    directory_key == scratch
        || directory_key.starts_with(&format!("{scratch}/"))
        || (directory_key == worktree && name == Some(ENGRAM_WORKTREE_SCRATCH_DIRECTORY))
}

impl AppState {
    /// The workspace watcher saw the files in `changes` change (its ignored
    /// directories, such as `.git`, `target` and `node_modules`, already left
    /// out): a carried check whose worktree holds one of them is fenced, with
    /// the path. A change under the `.tmp/` of its own worktree is left out
    /// too (`engram_is_worktree_scratch`). The worktrees are resolved off the
    /// state lock, once per distinct directory, and only while some carried
    /// check is unfenced.
    fn note_engram_workspace_file_changes(&self, changes: &[WorkspaceFileChangeEvent]) {
        if changes.is_empty() {
            return;
        }
        let open = self
            .inner
            .lock()
            .expect("state mutex poisoned")
            .sessions
            .iter()
            .any(|record| {
                record
                    .engram
                    .carried_checks
                    .iter()
                    .any(|carried| carried.fence.is_none())
                    || record.engram.active_turn_checks.iter().any(|check| {
                        check.end.is_none()
                            && check.watcher_fence.is_none()
                            && engram_check_is_launcher_full(&check.command)
                    })
            });
        if !open {
            return;
        }
        let mut directories: Vec<PathBuf> = Vec::new();
        for change in changes {
            let path = FsPath::new(&change.path);
            let directory = path.parent().unwrap_or(path).to_path_buf();
            if !directories.contains(&directory) {
                directories.push(directory);
            }
        }
        let placed = if directories.len() > ENGRAM_CARRIED_WATCH_DIRECTORY_LIMIT {
            // Too many places to resolve: any carried check may be touched.
            vec![(None, format!("{} files changed at once", changes.len()))]
        } else {
            // Each directory once: its worktree key for routing the fence.
            let resolved: Vec<(PathBuf, String)> = directories
                .into_iter()
                .map(|directory| {
                    let worktree = engram_worktree_root(&directory);
                    (directory, worktree)
                })
                .collect();
            // For the scratch test, the exact keys of a directory's worktree
            // root and of itself, both resolved alike through their longest
            // existing part, so a directory already gone when its event is
            // handled still reads in its resolved spelling. They are resolved
            // only for a path that names the scratch directory at all, and
            // once per directory: every other path cannot be scratch, and
            // resolving it would read the file system for each event of a
            // build. A network path is not resolved, as its worktree key is
            // not (`engram_worktree_root`), and so is never scratch.
            let mut exact: Vec<(PathBuf, String, String)> = Vec::new();
            let mut placed: Vec<(Option<String>, String)> = Vec::new();
            for change in changes {
                let path = FsPath::new(&change.path);
                let directory = path.parent().unwrap_or(path);
                let Some((_, worktree)) = resolved.iter().find(|(known, _)| known == directory)
                else {
                    continue;
                };
                let names_scratch = path.components().any(|component| {
                    component.as_os_str() == std::ffi::OsStr::new(ENGRAM_WORKTREE_SCRATCH_DIRECTORY)
                });
                if names_scratch && !engram_network_path(&change.path) {
                    if !exact.iter().any(|(known, _, _)| known == directory) {
                        let canonical = engram_canonical_path(directory);
                        exact.push((
                            directory.to_path_buf(),
                            engram_exact_path_key(&engram_worktree_root_path(&canonical)),
                            engram_exact_path_key(&canonical),
                        ));
                    }
                    let name = path.file_name().map(|name| name.to_string_lossy());
                    if exact.iter().any(|(known, root_exact, key)| {
                        known == directory
                            && engram_is_worktree_scratch(root_exact, key, name.as_deref())
                    }) {
                        continue;
                    }
                }
                if !placed
                    .iter()
                    .any(|(known, _)| known.as_deref() == Some(worktree.as_str()))
                {
                    placed.push((Some(worktree.clone()), change.path.clone()));
                }
            }
            placed
        };
        if placed.is_empty() {
            return;
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        for record in inner.sessions.iter_mut() {
            for (worktree, path) in &placed {
                // The watcher reports late, so its change may have landed
                // while the run was going, even when the run has ended since.
                let cause = format!("a file in its worktree changed: {path}");
                engram_fence_carried_checks_in(
                    record,
                    std::slice::from_ref(worktree),
                    &cause,
                    true,
                );
                // A full gate still being launched carries the change into its
                // fence once carried.
                for check in &mut record.engram.active_turn_checks {
                    if check.end.is_none()
                        && check.watcher_fence.is_none()
                        && engram_check_is_launcher_full(&check.command)
                        && engram_worktrees_may_hold(
                            std::slice::from_ref(worktree),
                            &engram_path_key(&check.target.root),
                        )
                    {
                        check.watcher_fence = Some(cause.clone());
                    }
                }
            }
        }
    }

    /// Finds the run of every carried check not yet read as terminal, and
    /// keeps the digest of its terminal record from the first read that finds
    /// it terminal. Disk reads happen off the state lock. Called on the run
    /// index's tick and before each checkpoint settles carried checks.
    fn poll_engram_carried_runs(&self) {
        // One poll at a time: a read made off the state lock is stored
        // before any other poll reads, and so before a checkpoint that polls
        // first can settle the check it read.
        let _polling = self.lock_engram_carried_poll();
        struct Pending {
            session_id: String,
            grant_id: String,
            sequence: usize,
            root: PathBuf,
            launched_at: String,
            directory: Option<PathBuf>,
            claimed: Vec<PathBuf>,
            ruled_out: std::collections::BTreeSet<PathBuf>,
        }
        let now = chrono::Utc::now();
        let pending: Vec<Pending> = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .sessions
                .iter()
                .flat_map(|record| {
                    // A run a check found stays its own, fenced or not; a
                    // check that can no longer be credited finds no new one
                    // (`engram_carried_check_polls`).
                    let claimed = record
                        .engram
                        .carried_checks
                        .iter()
                        .filter_map(|carried| carried.run_directory.clone())
                        .chain(record.engram.carried_consumed_runs.iter().cloned())
                        .collect::<Vec<_>>();
                    record
                        .engram
                        .carried_checks
                        .iter()
                        .filter(move |carried| engram_carried_check_polls(carried, now))
                        .map(move |carried| Pending {
                            session_id: record.session.id.clone(),
                            grant_id: carried.check.grant_id.clone(),
                            sequence: carried.check.sequence,
                            root: carried.check.target.root.clone(),
                            launched_at: carried.check.started_at.clone(),
                            directory: carried.run_directory.clone(),
                            claimed: claimed.clone(),
                            ruled_out: carried.ruled_out_runs.clone(),
                        })
                })
                .collect()
        };
        for pending in pending {
            let (found, ruled_out, first_terminal) = match &pending.directory {
                Some(directory) => (Some(directory.clone()), Vec::new(), None),
                None => engram_find_carried_run(
                    &pending.session_id,
                    &pending.root,
                    &pending.launched_at,
                    &pending.claimed,
                    &pending.ruled_out,
                ),
            };
            let missing = found.is_none()
                && engram_parse_time(&pending.launched_at).is_some_and(|launched| {
                    (chrono::Utc::now() - launched).num_seconds()
                        > ENGRAM_CARRIED_RUN_SEARCH_SECONDS
                });
            let (digest, launcher_gone) = match first_terminal {
                Some(first) => (Some(first), false),
                None => found
                    .as_deref()
                    .map_or((None, false), engram_carried_run_progress),
            };
            #[cfg(test)]
            wait_at_test_engram_carried_poll_pause(self);
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(&pending.session_id) else {
                continue;
            };
            // The run's place, digest and search state live in memory only,
            // so taking them does not mark the session for persistence.
            let record = &mut inner.sessions[index];
            let Some(carried) = record.engram.carried_checks.iter_mut().find(|carried| {
                carried.check.grant_id == pending.grant_id
                    && carried.check.sequence == pending.sequence
            }) else {
                continue;
            };
            engram_note_carried_run_read(carried, found, digest, launcher_gone, missing, ruled_out);
        }
    }

    /// Whether `session_id` carries a check a poll still reads
    /// (`engram_carried_check_polls`).
    fn engram_session_has_pollable_carried_checks(&self, session_id: &str) -> bool {
        let now = chrono::Utc::now();
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner.find_session_index(session_id).is_some_and(|index| {
            inner.sessions[index]
                .engram
                .carried_checks
                .iter()
                .any(|carried| engram_carried_check_polls(carried, now))
        })
    }

    /// Takes the lock that serializes carried-run polls
    /// (`poll_engram_carried_runs`).
    fn lock_engram_carried_poll(&self) -> std::sync::MutexGuard<'_, ()> {
        match self.engram_carried_poll_lock.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::WouldBlock) => {
                #[cfg(test)]
                signal_test_engram_carried_poll_waiting(self);
                self.engram_carried_poll_lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
            }
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        }
    }
}

/// Test hooks of the carried-run poll, keyed by the state they belong to.
#[cfg(test)]
#[derive(Default)]
struct TestEngramCarriedPollHooks {
    /// The next poll pauses after its reads, before it stores them: it sends
    /// on the first sender and waits on the receiver.
    pause: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
    /// The next poll that finds another poll holding the lock says so.
    waiting: Option<std::sync::mpsc::Sender<()>>,
}

#[cfg(test)]
static TEST_ENGRAM_CARRIED_POLL_HOOKS: std::sync::LazyLock<
    std::sync::Mutex<HashMap<usize, TestEngramCarriedPollHooks>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

#[cfg(test)]
fn test_engram_carried_poll_key(state: &AppState) -> usize {
    Arc::as_ptr(&state.inner) as usize
}

/// Makes the next carried-run poll of `state` pause after its reads: the
/// first receiver hears when it paused, and sending on the sender releases it.
#[cfg(test)]
fn install_test_engram_carried_poll_pause(
    state: &AppState,
) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
    let (paused_tx, paused_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    TEST_ENGRAM_CARRIED_POLL_HOOKS
        .lock()
        .expect("test poll hook mutex poisoned")
        .entry(test_engram_carried_poll_key(state))
        .or_default()
        .pause = Some((paused_tx, release_rx));
    (paused_rx, release_tx)
}

/// The receiver hears when the next carried-run poll of `state` finds the
/// poll lock held.
#[cfg(test)]
fn install_test_engram_carried_poll_waiting(state: &AppState) -> std::sync::mpsc::Receiver<()> {
    let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
    TEST_ENGRAM_CARRIED_POLL_HOOKS
        .lock()
        .expect("test poll hook mutex poisoned")
        .entry(test_engram_carried_poll_key(state))
        .or_default()
        .waiting = Some(waiting_tx);
    waiting_rx
}

#[cfg(test)]
fn wait_at_test_engram_carried_poll_pause(state: &AppState) {
    let pause = TEST_ENGRAM_CARRIED_POLL_HOOKS
        .lock()
        .expect("test poll hook mutex poisoned")
        .get_mut(&test_engram_carried_poll_key(state))
        .and_then(|hooks| hooks.pause.take());
    if let Some((paused_tx, release_rx)) = pause
        && paused_tx.send(()).is_ok()
    {
        let _ = release_rx.recv();
    }
}

#[cfg(test)]
fn signal_test_engram_carried_poll_waiting(state: &AppState) {
    let waiting = TEST_ENGRAM_CARRIED_POLL_HOOKS
        .lock()
        .expect("test poll hook mutex poisoned")
        .get_mut(&test_engram_carried_poll_key(state))
        .and_then(|hooks| hooks.waiting.take());
    if let Some(waiting) = waiting {
        let _ = waiting.send(());
    }
}
