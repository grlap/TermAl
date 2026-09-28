// The one-call form, `pushd "DIR" && TEST`: the one line whose test TermAl
// credits to the directory the line names rather than to where it presumes
// the shell is, since a runtime that reports no directory (Claude) may
// return its shell to the project between calls. Owns the form's grammar
// (`engram_one_call_prefix`, `engram_one_call_reads`), which sessions are
// told it up front (`engram_one_call_offered_up_front`), whether its end's
// exit is its test's own (`engram_check_exit_is_the_tests`), which decides
// the outcome its end may claim (`engram_check_command_outcome`) and what
// its evidence says of that exit (`engram_check_summary` and
// `engram_check_refs` in `engram_check_recognition.rs` ask it), the near
// misses of a test run after another command on its line, after the form's
// `pushd` with something the form does not take, or in the form but for its
// DIR, written in Git Bash's spelling of a drive (`engram_embedded_test`,
// `EngramNearMiss`, `engram_one_call_git_bash_dir`), and which host line a
// recognised test that gets no check leaves for the agent, with which remedy
// (`engram_uncredited_test_line`). Does not own the recognition of a plain
// test line or the wording of a check's evidence
// (`engram_check_recognition.rs`), where a check is credited
// (`engram_check_worktree_from` in `engram_check_paths.rs`), the wording of
// the host lines (`engram_source_roots.rs`), or the check lifecycle
// (`engram_turn_checks.rs`). New module: the choice of host line for a test
// that gets no check, formerly inline in `note_engram_command_started`
// (`engram_turn_checks.rs`), now lives here with the form's new grammar,
// outcome rule and near misses, which would otherwise have grown
// `engram_check_recognition.rs`.

/// At most this many bytes of a test's own command are named in a remedy
/// (`engram_one_call_template`); a longer one is named `<test>`, so an
/// agent's long command line does not swell its next prompt.
const ENGRAM_ONE_CALL_REMEDY_TEST_MAX_BYTES: usize = 256;

/// The directory and the test of the one-call form, `pushd "DIR" && TEST`,
/// matched exactly rather than read as a shell reads a line, because this is
/// one form TermAl can model completely in every shell that runs it:
/// - a bare line, with no wrapper to start another shell;
/// - `pushd` spelled exactly so, in lower case: bash, PowerShell 7 and cmd
///   all run it as a change of the shell's own directory, cmd's across drives
///   too. Bash's builtins are case-sensitive, so `PUSHD` there is a search of
///   the path, and a program that only looks like the builtin
///   (`/usr/bin/pushd`, `pushd.exe`) cannot change its parent's directory;
/// - one double-quoted DIR, a native absolute path made only of ASCII
///   letters, digits, spaces and `_ . - /`, with `:` only after the drive
///   letter and `\` only on Windows (a `:` further on names an alternate data
///   stream there, and PowerShell on other systems reads `x:` as a drive and
///   `\` as a separator where the file system does not), with no `.` or `..`
///   step, no doubled or trailing backslash and no network prefix, so that no
///   shell expands or escapes anything in it and every one reads the same
///   directory;
/// - then `&&`, so the test runs only once the change succeeded (`;` would
///   run it where the shell was after a failed one).
///
/// Anything else, `cd DIR && TEST` included, is not this form.
fn engram_one_call_prefix(line: &str) -> Option<(String, &str)> {
    let rest = line.strip_prefix("pushd ")?.trim_start_matches(' ');
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    let (directory, rest) = (&rest[..end], &rest[end + 1..]);
    let test = rest
        .trim_start_matches(' ')
        .strip_prefix("&&")?
        .trim_start_matches(' ');
    let plain = |(index, character): (usize, char)| {
        character.is_ascii_alphanumeric()
            || " _.-/".contains(character)
            || (cfg!(windows) && (character == '\\' || (character == ':' && index == 1)))
    };
    let canonical = !directory.is_empty()
        && directory.char_indices().all(plain)
        && directory
            .split(['\\', '/'])
            .all(|step| step != "." && step != "..")
        && !directory.contains(r"\\")
        && !directory.ends_with('\\')
        && FsPath::new(directory).is_absolute()
        && !engram_network_path(directory);
    (canonical && !test.is_empty()).then(|| (directory.to_owned(), test))
}

/// Whether the one-call form can name `root` (as the host lines show it) as
/// its DIR, so that a remedy may offer the form for a test there: one path
/// outside the form's grammar (`José`, `(x)`, `$`) cannot be named at all.
fn engram_one_call_reads(root: &str) -> bool {
    engram_one_call_prefix(&format!("pushd \"{root}\" && cargo test")).is_some()
}

/// Whether a session of `agent` is told the one-call form when it is bound
/// to a named root, before any test of it goes without credit
/// (`engram_source_root_line`). Only Claude's runtime reports no directory
/// for a command and runs the line as the agent wrote it: its shell alone may
/// be back in the project between calls, and it alone surely runs the bare
/// form. Codex reports its directory and wraps each line in a shell of its
/// own, which the form cannot be; an ACP runtime may report one or not, call
/// by call. Those are offered the form only once a test of theirs went
/// without credit in a shape the form carries (`engram_one_call_template`).
/// TermAl records a Claude command only from its `Bash` tool (claude.rs),
/// which runs the line in bash (Git Bash on Windows), so the form offered up
/// front never meets Windows PowerShell 5.1, which cannot read `&&`; were it
/// to, each such test would end unknown, never failed
/// (`engram_check_command_outcome`). That holds only because of what TermAl
/// records: a test Claude runs through its `PowerShell` tool is not recorded
/// at all, so it gets no check and no host line, in whatever shape.
fn engram_one_call_offered_up_front(agent: Agent) -> bool {
    agent == Agent::Claude
}

/// Whether `exit`, how `check`'s line ended, is its test's own. Always for a
/// plain line. A one-call line exits non-zero also when its `pushd`, or the
/// shell's reading of the line, failed and the test never ran (Windows
/// PowerShell cannot parse `&&`; a shell may lack `pushd`), so its non-zero
/// exit is the test's only when the test's runner stated its result in the
/// check's output (`result_lines`, `engram_is_result_line` for the check's
/// own program): it ran, so its `pushd` succeeded, and `pushd && TEST` exits
/// as its last command did. A size inventory is the test's output, not the
/// runner's result line, and does not count. Its success needs both to have
/// run, so it is the test's own.
fn engram_check_exit_is_the_tests(
    check: &EngramCheckCommand,
    exit: EngramCommandExit,
    result_lines: &[String],
) -> bool {
    match exit {
        EngramCommandExit::Code(code) if code != 0 && check.directory.is_some() => result_lines
            .iter()
            .any(|line| engram_is_result_line(&check.program, line.trim())),
        _ => true,
    }
}

/// `engram_check_outcome` for `check`, which ended `exit` with its runner's
/// `result_lines`: an exit that is not its test's own
/// (`engram_check_exit_is_the_tests`) is unknown.
fn engram_check_command_outcome(
    check: &EngramCheckCommand,
    exit: EngramCommandExit,
    result_lines: &[String],
) -> Option<EngramExecutionOutcome> {
    let exit = if engram_check_exit_is_the_tests(check, exit, result_lines) {
        exit
    } else {
        EngramCommandExit::Unknown
    };
    engram_check_outcome(exit, check.simple)
}

/// The recognised test a line runs after another command, `A && TEST` or
/// `A; TEST` (in a wrapper's script too): the near miss of the one-call form
/// (`cd "DIR" && cargo test`), whose test runs somewhere TermAl cannot place.
/// `None` for a line with no such test. Asked only of a line that is no check
/// itself (its caller has just recognised none in it, so the line is not read
/// twice on the runtime's event reader): a one-call line would otherwise
/// yield its own test. Its agent is told why, where it would otherwise get no
/// check and no word. A test inside a wrapper's script keeps the wrapper's
/// dialect: its runtime wraps what it runs, so the one-call form, a bare
/// line, is not one it could run (`engram_one_call_template`). A best effort
/// for a hint, never for credit: the line is split only at unquoted `&&` and
/// `;`, so a test after `||` or on a later line of a script still goes
/// without a word, and a backslash escape (`\"`) is not read, so a quote it
/// escapes may end a quoted string early.
fn engram_embedded_test(command: &str) -> Option<EngramCheckCommand> {
    let (script, dialect) = engram_unwrap_shell_command(command.trim());
    let mut segments = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut chars = script.char_indices().peekable();
    while let Some((index, character)) = chars.next() {
        match (quote, character) {
            (None, '\'' | '"') => quote = Some(character),
            (Some(open), close) if open == close => quote = None,
            (None, ';') => {
                segments.push(&script[start..index]);
                start = index + 1;
            }
            (None, '&') if chars.peek().is_some_and(|(_, next)| *next == '&') => {
                chars.next();
                segments.push(&script[start..index]);
                start = index + 2;
            }
            _ => {}
        }
    }
    segments.push(&script[start..]);
    segments
        .into_iter()
        .skip(1)
        .find_map(engram_plain_check_command)
        .map(|check| match dialect {
            EngramShellDialect::Unknown => check,
            wrapper => EngramCheckCommand {
                dialect: wrapper,
                ..check
            },
        })
}

/// Where on its line a recognised test that is no check ran
/// (`engram_embedded_test`), when not alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngramNearMiss {
    /// After another command (`cd "DIR" && TEST`, `A; TEST`).
    AfterCommand,
    /// After the one-call form's `pushd "DIR" &&`, but with something the
    /// form does not take: a pipe, redirection or list after the test, or a
    /// shell of its own around it (`pushd "DIR" && cargo test 2>&1 | tail`).
    AfterPushd,
    /// After what would be the one-call form's `pushd "DIR" &&` on Windows,
    /// but with DIR in Git Bash's spelling of a drive (`/c/…`), which the form
    /// does not take: PowerShell would read it as `\c\…` on the current drive.
    /// The spelling Claude's Bash tool (Git Bash) favours. `test_alone`:
    /// whether the line, its DIR written as Windows names it, would be the
    /// form itself, or would still hold more than its one test alone.
    GitBashDir { test_alone: bool },
}

impl EngramNearMiss {
    /// How the test of `command`, a line that is no check itself, sits on it.
    fn of(command: &str) -> Self {
        let command = command.trim();
        if engram_one_call_prefix(command).is_some() {
            Self::AfterPushd
        } else if let Some(test_alone) = engram_one_call_git_bash_dir(command) {
            Self::GitBashDir { test_alone }
        } else {
            Self::AfterCommand
        }
    }
}

/// For `line`, which would start as the one-call form on Windows were its
/// DIR, written in Git Bash's spelling of a drive (`/c/…`,
/// `engram_is_msys_drive_path`), written as Windows names it (`C:/…`,
/// `engram_msys_drive_path`): whether the line so written would be the form
/// itself, its test alone. `None` for any other line, and always on another
/// system, where `/c/…` is a path of its own.
fn engram_one_call_git_bash_dir(line: &str) -> Option<bool> {
    let rest = line.strip_prefix("pushd ")?.trim_start_matches(' ');
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    let (directory, after) = (&rest[..end], &rest[end + 1..]);
    if !engram_is_msys_drive_path(directory) {
        return None;
    }
    let windows = engram_msys_drive_path(directory);
    let rewritten = format!("pushd \"{windows}\"{after}");
    engram_one_call_prefix(&rewritten)?;
    Some(engram_check_command(&rewritten).is_some())
}

/// The test to name in the one-call form's remedy for `check`, whose turn is
/// measured in `measured_in`: its own command, or `<test>` when that is
/// longer than a host line should carry. `None` for a test the form could not
/// carry (one run inside a wrapper or in a compound line, whose runtime could
/// not run a bare `pushd` line) or for a worktree whose path the form cannot
/// read. The directory is always left to the agent (`pushd "DIR" && TEST`,
/// with DIR the absolute directory in the worktree where the test runs):
/// TermAl names no directory it would have to guess, since a test meant for
/// one inside the worktree would run somewhere else.
fn engram_one_call_template(check: &EngramCheckCommand, measured_in: &FsPath) -> Option<String> {
    let root = engram_source_root_display(&measured_in.to_string_lossy());
    let carried = check.simple
        && check.dialect == EngramShellDialect::Unknown
        && engram_one_call_reads(&root);
    carried.then(|| {
        if check.normalized.len() <= ENGRAM_ONE_CALL_REMEDY_TEST_MAX_BYTES {
            check.normalized.clone()
        } else {
            "<test>".to_owned()
        }
    })
}

/// The host line for a recognised test of a mediated turn that gets no
/// check, told once before the agent's next prompt, finished or not, with a
/// remedy that counts: the test ran after another command on its line, or
/// after the form's `pushd` with something the form does not take
/// (`near_miss`, `engram_embedded_test`); it started in another worktree
/// than the one the turn is measured in; or TermAl cannot confirm it did not,
/// because a shell that reports no directory may be back in the session's
/// workdir (`places`, as its target was judged), or cannot be placed. `None`
/// when none of these holds: a one-call line whose directory does not exist
/// never ran its test, and one inside the worktree got no check for another
/// reason (an argument that leads out) that is not its line's.
/// `reports_directory` says whether the runtime reported where the command
/// runs, one of the facts its remedy follows from (`EngramRemedyFacts`). A
/// network path is never resolved here, on the runtime's event reader, since
/// that can block for a network timeout (`engram_network_path`). Resolves on
/// the file system, so never under the state lock.
fn engram_uncredited_test_line(
    check: &EngramCheckCommand,
    near_miss: Option<EngramNearMiss>,
    workdir: &str,
    places: Option<&[Option<String>]>,
    credit_root: Option<(&FsPath, &str)>,
    child: bool,
    reports_directory: bool,
) -> Option<String> {
    if engram_network_path(workdir) {
        return None;
    }
    let measured_in = credit_root.map_or_else(
        || engram_worktree_root_path(FsPath::new(workdir)),
        |(root, _)| root.to_path_buf(),
    );
    let measured_key = engram_exact_path_key(&measured_in);
    let measured_text = measured_in.to_string_lossy();
    let named = credit_root.is_some();
    // The form's own line but for its test, or for its DIR's spelling, is
    // told the form, with the test left to the agent where it is not alone:
    // TermAl cannot rebuild a bare test from a piped or wrapped one.
    let form_but_for_one_part = matches!(
        near_miss,
        Some(EngramNearMiss::AfterPushd | EngramNearMiss::GitBashDir { .. })
    );
    let template = engram_one_call_template(check, &measured_in).or_else(|| {
        (form_but_for_one_part
            && engram_one_call_reads(&engram_source_root_display(&measured_text)))
        .then(|| "<test>".to_owned())
    });
    let template = template.as_deref();
    let facts = EngramRemedyFacts {
        template,
        reports_directory,
        // Without a named root the worktree measured is the workdir's own.
        workdir_inside: !named
            || engram_exact_path_key(&engram_worktree_root_path(FsPath::new(workdir)))
                == measured_key,
    };
    if let Some(near_miss) = near_miss {
        let reason = match near_miss {
            EngramNearMiss::AfterCommand => EngramUnconfirmedReason::InsideLine,
            EngramNearMiss::AfterPushd => EngramUnconfirmedReason::OneCallTestNotAlone,
            EngramNearMiss::GitBashDir { test_alone } => {
                EngramUnconfirmedReason::OneCallGitBashDir { test_alone }
            }
        };
        return Some(engram_source_root_unconfirmed_line(
            &measured_text,
            reason,
            facts,
        ));
    }
    // A one-call line started in its own directory, if its `pushd` ran at
    // all: one whose directory does not resolve never ran its test.
    if let Some(directory) = check.directory.as_deref() {
        let directory = engram_resolve_shell_move(None, directory)?;
        if engram_network_path(&directory) {
            return None;
        }
        let ran_in = engram_worktree_root_path(FsPath::new(&directory));
        return (engram_exact_path_key(&ran_in) != measured_key).then(|| {
            engram_source_root_withheld_line(&ran_in, &measured_text, named, child, template)
        });
    }
    let Some(places) = places else {
        return Some(engram_source_root_unconfirmed_line(
            &measured_text,
            EngramUnconfirmedReason::ShellUnknown,
            facts,
        ));
    };
    let started_in = places
        .iter()
        .map(|place| match place {
            Some(place) => FsPath::new(workdir).join(place),
            None => PathBuf::from(workdir),
        })
        .collect::<Vec<_>>();
    if started_in
        .iter()
        .any(|place| engram_network_path(&place.to_string_lossy()))
    {
        return None;
    }
    let roots = started_in
        .iter()
        .map(|place| engram_worktree_root_path(place))
        .collect::<Vec<_>>();
    // The presumed place of the shell is the last: when it lies outside, the
    // test started elsewhere; when only an earlier one does (the workdir),
    // TermAl cannot confirm where it ran.
    let last = roots.last()?;
    if engram_exact_path_key(last) != measured_key {
        return Some(engram_source_root_withheld_line(
            last,
            &measured_text,
            named,
            child,
            template,
        ));
    }
    roots
        .iter()
        .zip(&started_in)
        .find(|(root, _)| engram_exact_path_key(root) != measured_key)
        .map(|(_, place)| {
            engram_source_root_unconfirmed_line(
                &measured_text,
                EngramUnconfirmedReason::ShellMayReturn(place),
                facts,
            )
        })
}
