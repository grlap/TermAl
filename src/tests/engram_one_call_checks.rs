// Owns the tests of the one-call form, `pushd "DIR" && TEST`
// (src/engram_one_call.rs): which lines are that form and which are not,
// including every shape a review found that could credit a test to a
// directory it did not run in; the check it starts, credited to DIR wherever
// the shell was, and what it does to the presumed shell and the worktrees it
// may write in; the outcome of its end; and the host lines a test that gets
// no check leaves for the agent (`engram_uncredited_test_line`), naming the
// form with its directory left to the agent where the form could carry the
// test, and a fallback where it could not, a lost shell found again first;
// and which sessions the named-root line tells the form up front. Does not
// own the recognition of a
// plain test line or the rest of check crediting
// (src/tests/engram_turn_checks.rs, whose `CheckedTurn` fixture this child
// module uses). New module, split from the one-call tests that would
// otherwise have grown src/tests/engram_turn_checks.rs.
use super::*;

/// A native absolute directory for recognition alone, which never touches
/// the file system: the form's grammar needs no real path.
fn grammar_dir() -> &'static str {
    if cfg!(windows) {
        r"C:\one call\root"
    } else {
        "/one call/root"
    }
}

/// Runs `command` to the end `exit`, as a shell that reports no directory
/// (Claude) runs it, waiting for the check's snapshots at its start and end.
fn run_without_directory(turn: &CheckedTurn, key: &str, command: &str, exit: EngramCommandExit) {
    run_with_output(
        turn,
        key,
        command,
        "running 1 test\ntest result: ok. 1 passed",
        exit,
    );
}

/// `run_without_directory` with the command's `output`.
fn run_with_output(
    turn: &CheckedTurn,
    key: &str,
    command: &str,
    output: &str,
    exit: EngramCommandExit,
) {
    let mut recorder = turn.recorder();
    recorder
        .command_started(key, command)
        .expect("the start should record");
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit(key, command, output, CommandStatus::Success, exit)
        .expect("the end should record");
    turn.wait_for_snapshots();
}

/// Starts `command` without ending it, as a shell that reports no directory
/// runs it, once its opening snapshot is taken.
fn start_without_directory(turn: &CheckedTurn, key: &str, command: &str) {
    turn.recorder()
        .command_started(key, command)
        .expect("the start should record");
    turn.wait_for_snapshots();
}

/// Ends the started `command` with exit 0.
fn end_command(turn: &CheckedTurn, key: &str, command: &str) {
    turn.recorder()
        .command_completed_with_exit(
            key,
            command,
            "",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the end should record");
}

/// The key and target of each check the turn has kept.
fn kept_checks(turn: &CheckedTurn) -> Vec<(String, EngramCheckTarget)> {
    turn.record(|record| {
        record
            .engram
            .active_turn_checks
            .iter()
            .map(|check| (check.key.clone(), check.target.clone()))
            .collect()
    })
}

/// The host line waiting for the agent's next prompt, cleared for the next
/// step of a test.
fn take_pending_line(turn: &CheckedTurn) -> Option<String> {
    let line = turn.record(|record| record.engram.pending_source_root_line.clone());
    turn.record_mut(|record| record.engram.pending_source_root_line = None);
    line
}

/// The three kinds of test directory whose host lines differ, as a suffix of
/// a fixture's label, which becomes part of its project's path: one the form
/// can name; one it cannot (`(x)`) but a `cd` into it TermAl follows; and one
/// TermAl follows no `cd` into (`$x`, which a shell would expand). Every test
/// whose lines depend on the path runs once per kind, and each branch asserts
/// the exact text it expects, with no skip and no silent pass. The `(x)` and
/// `$x` branches run on any host. The plain kind can only add to the host's
/// temp path, so it takes the branch the form can name only where that path
/// allows it: on a host whose temp path the form cannot name, that branch
/// (a one-call line credited end to end) runs once `TERMAL_TEST_USER_TEMP`
/// points at a plain path (docs/test.md), and a test on it is not required,
/// since a precondition on the host would make the suite a lottery.
const PATH_KINDS: [&str; 3] = ["", " (x)", " $x"];

/// How the host lines name `worktree` (canonical, without the verbatim
/// prefix), and whether the one-call form can name it at all: a test
/// directory with a character outside the form's plain set (`José`, `(x)`)
/// cannot be, and there each test checks the fallback instead of the form.
fn root_as_named(worktree: &FsPath) -> (String, bool) {
    let root = engram_source_root_display(
        &fs::canonicalize(worktree)
            .expect("the worktree canonicalizes")
            .to_string_lossy(),
    );
    let readable = engram_one_call_reads(&root);
    (root, readable)
}

/// Whether TermAl follows a `cd` into `root` (`engram_shell_move`).
fn cd_followed(root: &str) -> bool {
    matches!(
        engram_shell_move(&format!("cd \"{root}\"")),
        EngramShellMove::To(_)
    )
}

#[test]
fn the_unreadable_path_kinds_reach_their_branches_on_any_host() {
    // Whatever the host's temp path, `(x)` puts a project where the form
    // names nothing, and `$x` where TermAl follows no `cd`: the default gate
    // takes those branches everywhere. The plain kind takes the branch the
    // form can name only where the host's path allows it, so it asserts
    // nothing here (`PATH_KINDS`).
    for kind in PATH_KINDS {
        let turn = CheckedTurn::start(&format!("path-kind{kind}"), false);
        let (root, readable) = root_as_named(&turn.root);
        match kind {
            "" => {}
            " (x)" => assert!(!readable, "{root}"),
            _ => assert!(!readable && !cd_followed(&root), "{root}"),
        }
    }
}

/// The remedy a session with the named root `root` and its workdir outside
/// it is given for a test run after another command on its line, where the
/// form cannot carry the test.
fn near_miss_fallback(root: &str) -> String {
    format!("If it was meant to count here, run it from a session whose workdir is in {root}.")
}

/// The remedy, after `lead`, for a runtime that reports its directory.
fn directory_remedy(lead: &str, worktree: &str) -> String {
    format!(
        "{lead} it as a line of its own, with its working directory set to the directory in \
         {worktree} where it runs."
    )
}

/// The remedy, after `lead`, for a session whose workdir is in `worktree`
/// and whose runtime reports no directory, where the form cannot carry the
/// test: its shell brought where the test runs with a `cd` TermAl follows,
/// or a new session where it follows none into that path.
fn shell_remedy(lead: &str, worktree: &str) -> String {
    if cd_followed(worktree) {
        cd_recovery(lead, worktree)
    } else {
        new_session_remedy(lead, worktree)
    }
}

/// The remedy, after `lead`, where TermAl follows no `cd` into `worktree`.
fn new_session_remedy(lead: &str, worktree: &str) -> String {
    format!(
        "{lead} it from a new session whose workdir is in {worktree}, since TermAl cannot follow \
         a directory change into that path."
    )
}

/// The directory TermAl presumes `turn`'s shell in, as a canonical key.
fn shell_key(turn: &CheckedTurn) -> Option<String> {
    turn.record(|record| {
        record
            .engram
            .shell_directory
            .as_ref()
            .and_then(|shell| shell.directory.clone())
    })
    .map(|directory| engram_exact_path_key(FsPath::new(&directory)))
}

/// The one-call form of `test` as the host lines name it, with the
/// directory left to the agent.
fn form_in(root: &str, test: &str) -> String {
    format!("`pushd \"DIR\" && {test}`, with DIR the absolute directory in {root} where it runs")
}

/// The canonical key of `worktree`, as checks are credited.
fn root_key(worktree: &FsPath) -> String {
    engram_exact_path_key(&fs::canonicalize(worktree).expect("the worktree canonicalizes"))
}

#[test]
fn the_one_call_form_is_recognised_as_its_test() {
    let dir = grammar_dir();
    let sub = format!("{dir}/sub-dir_1.2");
    for (command, directory, normalized) in [
        (
            format!("pushd \"{dir}\" && cargo test --lib"),
            dir,
            "cargo test --lib",
        ),
        (format!("pushd \"{dir}\"&&npm  test"), dir, "npm test"),
        (
            format!("pushd  \"{sub}\"  &&  pytest -q"),
            sub.as_str(),
            "pytest -q",
        ),
    ] {
        let check = engram_check_command(&command)
            .unwrap_or_else(|| panic!("`{command}` is the one-call form"));
        assert_eq!(check.directory.as_deref(), Some(directory), "{command}");
        assert_eq!(check.normalized, normalized, "{command}");
        assert_eq!(check.dialect, EngramShellDialect::Unknown, "{command}");
        assert!(check.simple, "{command}");
        assert_eq!(
            engram_check_fingerprint(&check),
            engram_check_fingerprint(&engram_check_command(normalized).expect("a test")),
            "the fingerprint is the test's own, the one an author binds: {command}"
        );
    }
}

#[test]
fn every_other_shape_is_no_check() {
    let dir = grammar_dir();
    let bare = if cfg!(windows) { "C:/named" } else { "/named" };
    let mut not_checks = vec![
        // An operator glued to a closing quote belongs to the outer shell:
        // the change dies with the wrapper and the test runs where that
        // shell is.
        format!("bash -lc \"cd {bare}\"&&pytest"),
        format!("bash -lc \"pushd {bare}\"&&pytest"),
        // A program that only looks like the builtin cannot change its
        // parent's directory; bash's builtins are case-sensitive, so
        // `PUSHD` there is a search of the path.
        format!("/usr/bin/pushd \"{dir}\" && cargo test"),
        format!("pushd.exe \"{dir}\" && cargo test"),
        format!("PUSHD \"{dir}\" && cargo test"),
        format!("Pushd \"{dir}\" && cargo test"),
        format!("/usr/bin/cd \"{dir}\" && cargo test"),
        format!("cd.exe \"{dir}\" && cargo test"),
        // An escape cmd would evaluate names another directory.
        format!("pushd \"{dir}^B\" && cargo test"),
        // cmd's `cd` to a junction on another drive stays on this one.
        r#"cd "D:\alias" && pytest"#.to_owned(),
        // Every wrapper: its script runs in another shell.
        format!("bash -lc 'pushd \"{dir}\" && cargo test'"),
        format!("pwsh -Command \"pushd '{dir}' && cargo test\""),
        format!("cmd /c \"pushd {bare} && cargo test\""),
        format!("powershell -Command \"pushd '{dir}' && cargo test\""),
        // Every other directory change.
        format!("cd \"{dir}\" && cargo test"),
        format!("chdir \"{dir}\" && cargo test"),
        format!("Set-Location \"{dir}\" && cargo test"),
        format!("sl \"{dir}\" && cargo test"),
        format!("Push-Location \"{dir}\" && cargo test"),
        // A character some shell expands or escapes.
        format!("pushd \"{dir}$x\" && cargo test"),
        format!("pushd \"{dir}%x%\" && cargo test"),
        format!("pushd \"{dir}!x\" && cargo test"),
        format!("pushd \"{dir}`x\" && cargo test"),
        format!("pushd \"{dir}(x)\" && cargo test"),
        format!("pushd \"{dir}&x\" && cargo test"),
        format!("pushd \"{dir}~1\" && cargo test"),
        // Only one double-quoted directory.
        format!("pushd {bare} && cargo test"),
        format!("pushd '{dir}' && cargo test"),
        format!("pushd\t\"{dir}\" && cargo test"),
        // Only `&&`: `;` runs the test after a failed change, `&` apart
        // from it, `||` only after a failed one.
        format!("pushd \"{dir}\"; cargo test"),
        format!("pushd \"{dir}\" & cargo test"),
        format!("pushd \"{dir}\" || cargo test"),
        // A step or a relative path depends on how each shell takes it.
        format!("pushd \"{dir}/../x\" && cargo test"),
        format!("pushd \"{dir}/./x\" && cargo test"),
        "pushd \"crate\" && cargo test".to_owned(),
        // One simple test after it, on its own.
        format!("pushd \"{dir}\" && cargo test | tail -3"),
        format!("pushd \"{dir}\" && cargo test && git clean -fdx"),
        format!("pushd \"{dir}\" && cargo build"),
        format!("pushd \"{dir}\" &&"),
        format!("pushd \"{dir}\" && pushd \"{dir}\" && cargo test"),
        format!("pushd \"{dir}\" && bash -lc 'cargo test'"),
    ];
    if cfg!(windows) {
        not_checks.extend([
            // Bash reads `\"` inside double quotes as a quote, and `\\` as
            // one backslash, where cmd and PowerShell do not.
            r#"pushd "C:\one call\" && cargo test"#.to_owned(),
            r#"pushd "C:\\one call" && cargo test"#.to_owned(),
            // A network path is never resolved.
            r#"pushd "\\server\share" && cargo test"#.to_owned(),
            r#"pushd "//server/share" && cargo test"#.to_owned(),
            // Git Bash's drive spelling is no native path.
            r#"pushd "/c/one call" && cargo test"#.to_owned(),
            // A `:` after the drive's names an alternate data stream.
            r#"pushd "C:\one call\a:b" && cargo test"#.to_owned(),
            r#"pushd "C::\one call" && cargo test"#.to_owned(),
        ]);
    } else {
        // PowerShell on this system reads `\` as a separator, the file
        // system as part of a name, and `x:` as a drive.
        not_checks.push(r#"pushd "/one call/a\b" && cargo test"#.to_owned());
        not_checks.push(r#"pushd "/one call/a:b" && cargo test"#.to_owned());
    }
    for command in not_checks {
        assert_eq!(
            engram_check_command(&command),
            None,
            "`{command}` is no check"
        );
    }
}

#[test]
fn deeply_nested_one_call_prefixes_are_refused_without_recursing() {
    // Each prefix is read once and the rest as a plain line, so a runtime
    // line of any depth cannot exhaust the reader's stack. The recursive
    // version of the recogniser overflowed its stack here.
    let prefix = format!("pushd \"{}\" && ", grammar_dir());
    let line = format!("{}cargo test", prefix.repeat(20_000));
    assert_eq!(engram_check_command(&line), None);
    assert_eq!(
        engram_embedded_test(&line).map(|check| check.normalized),
        Some("cargo test".to_owned()),
        "its agent is told why its test gets no credit"
    );
}

#[test]
fn a_one_call_line_ends_as_its_test_does_unless_a_non_zero_exit_hides_which_failed() {
    // `A && B` exits non-zero also when A, or the shell's reading of the
    // line, failed and B never ran, unless B's runner stated its result;
    // success needs both to have run.
    let one_call = engram_check_command(&format!("pushd \"{}\" && cargo test", grammar_dir()))
        .expect("the one-call form");
    let plain = engram_check_command("cargo test").expect("a test");
    let stated = ["test result: FAILED. 2 passed; 1 failed".to_owned()];
    // A size inventory is the test's output, not its runner's result line.
    let inventory = ["src/main.rs: 120 physical lines (limit 400)".to_owned()];
    for (check, exit, lines, outcome) in [
        (
            &one_call,
            EngramCommandExit::Code(1),
            &[][..],
            EngramExecutionOutcome::Unknown,
        ),
        (
            &one_call,
            EngramCommandExit::Code(1),
            &inventory[..],
            EngramExecutionOutcome::Unknown,
        ),
        (
            &one_call,
            EngramCommandExit::Code(1),
            &stated[..],
            EngramExecutionOutcome::Failed,
        ),
        (
            &one_call,
            EngramCommandExit::Code(0),
            &[][..],
            EngramExecutionOutcome::Succeeded,
        ),
        (
            &one_call,
            EngramCommandExit::ReportedSuccess,
            &[][..],
            EngramExecutionOutcome::Succeeded,
        ),
        (
            &plain,
            EngramCommandExit::Code(1),
            &[][..],
            EngramExecutionOutcome::Failed,
        ),
    ] {
        assert_eq!(
            engram_check_command_outcome(check, exit, lines),
            Some(outcome),
            "{} {exit:?} {lines:?}",
            check.normalized
        );
    }
}

#[test]
fn a_test_run_after_another_command_on_its_line_is_a_near_miss() {
    let dir = grammar_dir();
    for (command, test, dialect) in [
        (
            format!("cd \"{dir}\" && cargo test"),
            "cargo test",
            EngramShellDialect::Unknown,
        ),
        (
            "cargo build; cargo test --lib".to_owned(),
            "cargo test --lib",
            EngramShellDialect::Unknown,
        ),
        // A test inside a wrapper keeps its wrapper's dialect: its runtime
        // wraps what it runs, and the form is a bare line.
        (
            "bash -lc 'cd x && npm test'".to_owned(),
            "npm test",
            EngramShellDialect::Bash,
        ),
        (
            format!("pwsh -Command \"pushd '{dir}' && cargo test\""),
            "cargo test",
            EngramShellDialect::PowerShell,
        ),
    ] {
        let check = engram_embedded_test(&command)
            .unwrap_or_else(|| panic!("`{command}` runs a test after another command"));
        assert_eq!(check.normalized, test, "{command}");
        assert_eq!(check.dialect, dialect, "{command}");
    }
    // After the form's `pushd` is its own near miss, but only on a bare line:
    // a wrapper's script is another shell's.
    for (command, near_miss) in [
        (
            format!("pushd \"{dir}\" && cargo test 2>&1 | tail -40"),
            EngramNearMiss::AfterPushd,
        ),
        (
            format!("pushd \"{dir}\" && bash -lc 'cargo test'"),
            EngramNearMiss::AfterPushd,
        ),
        (
            format!("cd \"{dir}\" && cargo test"),
            EngramNearMiss::AfterCommand,
        ),
        (
            format!("bash -lc 'pushd \"{dir}\" && cargo test | tail'"),
            EngramNearMiss::AfterCommand,
        ),
    ] {
        assert!(engram_embedded_test(&command).is_some(), "{command}");
        assert_eq!(EngramNearMiss::of(&command), near_miss, "{command}");
    }
    // On Windows, Git Bash's spelling of a drive in DIR is its own near miss,
    // whatever follows the test, unless the path would not be the form's
    // even in Windows spelling; elsewhere `/c/…` is a path of its own, and
    // the line is the form itself.
    let git_bash = "pushd \"/c/one call/root\" && cargo test";
    if cfg!(windows) {
        for (command, near_miss) in [
            (
                git_bash.to_owned(),
                EngramNearMiss::GitBashDir { test_alone: true },
            ),
            // Its test not alone either: both are told.
            (
                "pushd \"/c/one call/root\" && cargo test | tail -3".to_owned(),
                EngramNearMiss::GitBashDir { test_alone: false },
            ),
            (
                "pushd \"/c/one call/r$x\" && cargo test".to_owned(),
                EngramNearMiss::AfterCommand,
            ),
        ] {
            assert_eq!(engram_check_command(&command), None, "{command}");
            assert!(engram_embedded_test(&command).is_some(), "{command}");
            assert_eq!(EngramNearMiss::of(&command), near_miss, "{command}");
        }
    } else {
        assert_eq!(engram_one_call_git_bash_dir(git_bash), None);
        assert_eq!(
            engram_check_command(git_bash).and_then(|check| check.directory),
            Some("/c/one call/root".to_owned())
        );
    }
    // No test after another command. A check line, the one-call form
    // included, is never asked: its caller recognised it (a credited one-call
    // line leaves no word, as the crediting tests below check).
    for command in [
        "cargo test",
        "echo \"a && cargo test\"",
        "cargo build && echo done",
    ] {
        assert_eq!(engram_embedded_test(command), None, "{command}");
    }
}

#[test]
fn the_remedy_names_the_test_unless_it_is_too_long_for_a_host_line() {
    let dir = FsPath::new(grammar_dir());
    let short = engram_check_command("cargo test --lib").expect("a test");
    assert_eq!(
        engram_one_call_template(&short, dir).as_deref(),
        Some("cargo test --lib")
    );
    let long = engram_check_command(&format!("cargo test {}", "x".repeat(300))).expect("a test");
    assert_eq!(
        engram_one_call_template(&long, dir).as_deref(),
        Some("<test>"),
        "an agent's long command line does not swell its next prompt"
    );
    // Nothing for a test the form could not carry.
    let wrapped = engram_check_command("bash -lc 'cargo test'").expect("a test");
    assert_eq!(engram_one_call_template(&wrapped, dir), None);
    let piped = engram_check_command("cargo test | tail -3").expect("a test");
    assert_eq!(engram_one_call_template(&piped, dir), None);
}

/// Every host line about a test that got no check, as its builders give it,
/// across each reason, remedy and kind of session.
fn every_uncredited_test_line() -> Vec<String> {
    let dir = grammar_dir();
    let elsewhere = FsPath::new(if cfg!(windows) {
        r"C:\elsewhere"
    } else {
        "/elsewhere"
    });
    let mut lines = Vec::new();
    for template in [None, Some("cargo test")] {
        for (named, child) in [(false, false), (true, false), (false, true)] {
            lines.push(engram_source_root_withheld_line(
                elsewhere, dir, named, child, template,
            ));
        }
        for facts in every_remedy_facts(template) {
            for reason in [
                EngramUnconfirmedReason::ShellMayReturn(elsewhere),
                EngramUnconfirmedReason::ShellUnknown,
                EngramUnconfirmedReason::InsideLine,
                EngramUnconfirmedReason::OneCallTestNotAlone,
                EngramUnconfirmedReason::OneCallGitBashDir { test_alone: true },
                EngramUnconfirmedReason::OneCallGitBashDir { test_alone: false },
            ] {
                lines.push(engram_source_root_unconfirmed_line(dir, reason, facts));
            }
        }
    }
    lines
}

/// Each combination of the facts a remedy follows from, with `template`.
fn every_remedy_facts(template: Option<&str>) -> Vec<EngramRemedyFacts<'_>> {
    [(false, false), (false, true), (true, false), (true, true)]
        .into_iter()
        .map(|(reports_directory, workdir_inside)| EngramRemedyFacts {
            template,
            reports_directory,
            workdir_inside,
        })
        .collect()
}

// The remedy table: one row per remedy, each with its test. The facts are
// the form can carry the test (F1), the runtime reports its directory (F2),
// the session's workdir is in the worktree (F3), TermAl follows a `cd` into
// the worktree (F4).
//
// | F1 | F2 | F3 | F4 | remedy                                   | test                                      |
// |----|----|----|----|------------------------------------------|-------------------------------------------|
// | y  |    |    |    | the form, `pushd "DIR" && TEST`          | the_form_is_the_remedy_wherever_...       |
// | n  | y  |    |    | a line of its own, working directory set | a_runtime_that_reports_its_directory_...  |
// | n  | n  | y  | y  | `cd "DIR"` in a call of its own, then it | a_session_whose_workdir_is_in_the_root_...|
// | n  | n  | y  | n  | a new session in the worktree            | a_shell_in_a_path_no_cd_can_follow_...    |
// | n  | n  | n  |    | a session whose workdir is in it         | a_session_whose_workdir_is_outside_...    |

#[test]
fn the_form_is_the_remedy_wherever_it_can_carry_the_test() {
    // It counts wherever the shell is and whatever the runtime reports.
    let dir = grammar_dir();
    for facts in every_remedy_facts(Some("cargo test")) {
        for (reason, lead) in [
            (EngramUnconfirmedReason::ShellUnknown, "Run"),
            (
                EngramUnconfirmedReason::InsideLine,
                "If it was meant to count here, run",
            ),
        ] {
            let line = engram_source_root_unconfirmed_line(dir, reason, facts);
            assert!(
                line.ends_with(&format!(
                    "{lead} it in one call as {}{ENGRAM_ONE_CALL_DIR_SPELLING} and nothing piped, \
                     redirected or chained after the test.",
                    form_in(dir, "cargo test")
                )),
                "{facts:?}: {line}"
            );
        }
    }
}

#[test]
fn only_a_line_about_a_test_that_got_no_check_replaces_its_kind() {
    // Which pending line replaces which is read from how it begins, so every
    // line about a test without credit must begin so, and no other line may.
    for line in every_uncredited_test_line() {
        assert!(
            line.starts_with(ENGRAM_UNCREDITED_TEST_LINE_PREFIX),
            "{line}"
        );
    }
    let entry = EngramWorkSourceRoot {
        store: EngramAuthorityStoreKey {
            database_path: PathBuf::from("C:/store/engram.db"),
            project_id: "github.com/example/project".to_owned(),
        },
        work_id: "work-a".to_owned(),
        short_ref: "w-a".to_owned(),
        claim_id: "claim-a".to_owned(),
        claim_fence: 1,
        root: grammar_dir().to_owned(),
        common_dir_key: "c:/p/.git".to_owned(),
        named_by_session: "session-1".to_owned(),
        named_at: "2026-09-27T00:00:00.000Z".to_owned(),
        generation: 1,
    };
    for one_call in [false, true] {
        for entry in [Some(&entry), None] {
            let line = engram_source_root_line(entry, "work a", grammar_dir(), one_call);
            assert!(
                !line.starts_with(ENGRAM_UNCREDITED_TEST_LINE_PREFIX),
                "{line}"
            );
        }
    }
}

#[test]
fn a_run_of_uncredited_test_lines_never_evicts_a_bind_line() {
    let mut engram = EngramSessionState::default();
    let bind = engram_source_root_line(None, "work a", grammar_dir(), false);
    engram.set_pending_source_root_line(bind.clone());
    let uncredited = every_uncredited_test_line();
    for line in &uncredited {
        engram.set_pending_source_root_line(line.clone());
    }
    let pending = engram
        .pending_source_root_line
        .as_deref()
        .map(|pending| pending.lines().map(str::to_owned).collect::<Vec<_>>())
        .unwrap_or_default();
    assert_eq!(
        pending,
        [bind, uncredited.last().expect("a line").clone()],
        "the latest uncredited-test line replaces the earlier ones"
    );
}

#[test]
fn a_test_after_a_cd_in_an_earlier_call_gets_no_credit_and_the_agent_is_told_the_one_call_form() {
    // Claude reports no directory and may return its shell to the project
    // between calls. A session rooted in the main checkout that moved its
    // shell into the named root with an earlier call cannot show its test ran
    // there: no credit, and it is told so, with the form that counts. Before,
    // it got neither a check nor a word.
    for kind in PATH_KINDS {
        let turn = CheckedTurn::start(&format!("cd-then-test{kind}"), true);
        let worktree = name_turn_source_root(&turn);
        let (root, readable) = root_as_named(&worktree);
        let workdir = turn.record(|record| record.session.workdir.clone());
        // The session's workdir, the main checkout, lies outside the root.
        let remedy = if readable {
            form_in(&root, "cargo test")
        } else {
            format!("it from a session whose workdir is in {root}")
        };
        run_without_directory(
            &turn,
            "enter",
            &format!("cd \"{root}\""),
            EngramCommandExit::Code(0),
        );
        let followed = cd_followed(&root);
        if followed {
            assert_eq!(
                shell_key(&turn),
                Some(root_key(&worktree)),
                "{kind}: TermAl follows the shell into the named root"
            );
        } else {
            assert_eq!(shell_key(&turn), None, "{kind}: TermAl follows no cd here");
        }
        start_without_directory(&turn, "after-cd", "cargo test");
        assert!(kept_checks(&turn).is_empty(), "{:?}", kept_checks(&turn));
        let line = take_pending_line(&turn).expect("the agent is told why the test gets no credit");
        assert!(line.contains("cannot confirm it ran in"), "{line}");
        let reason = if followed {
            format!("may return to {}", engram_source_root_display(&workdir))
        } else {
            "cannot tell where its shell is".to_owned()
        };
        assert!(line.contains(&reason), "{line}");
        assert!(line.contains(&remedy), "{line}");

        // A shell TermAl lost cannot be placed at all; the same remedy counts.
        end_command(&turn, "after-cd", "cargo test");
        run_without_directory(&turn, "lose", "cd $OTHER", EngramCommandExit::Code(0));
        start_without_directory(&turn, "lost", "cargo test");
        assert!(kept_checks(&turn).is_empty(), "{:?}", kept_checks(&turn));
        let line = take_pending_line(&turn).expect("the agent is told why the test gets no credit");
        assert!(line.contains("cannot tell where its shell is"), "{line}");
        assert!(line.contains(&remedy), "{line}");
    }
}

#[test]
fn a_one_call_line_into_the_named_root_is_credited_there_wherever_the_shell_was() {
    // The form names its directory in the line, so its test ran there
    // whether or not the shell keeps a `cd`, even one TermAl lost; the shell
    // is presumed in DIR afterwards.
    for kind in PATH_KINDS {
        let turn = CheckedTurn::start(&format!("one-call-credit{kind}"), true);
        let worktree = name_turn_source_root(&turn);
        let (root, readable) = root_as_named(&worktree);
        run_without_directory(&turn, "lose", "cd $OTHER", EngramCommandExit::Code(0));
        let command = format!("pushd \"{root}\" && cargo test --lib");
        run_without_directory(&turn, "exited", &command, EngramCommandExit::Code(0));
        run_without_directory(
            &turn,
            "reported",
            &command,
            EngramCommandExit::ReportedSuccess,
        );
        let checks = kept_checks(&turn);

        if !readable {
            // No form can name this test directory: the line is a test run
            // after another command on its line, no check, and the agent is
            // told why, with the fallback for a named root outside its
            // workdir.
            assert!(checks.is_empty(), "{checks:?}");
            let line =
                take_pending_line(&turn).expect("the agent is told why the test gets no credit");
            assert!(
                line.contains("ran after another command on its line"),
                "{line}"
            );
            assert!(line.contains(&near_miss_fallback(&root)), "{line}");
            continue;
        }
        assert_eq!(
            checks
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>(),
            ["exited", "reported"]
        );
        for (key, target) in &checks {
            assert_eq!(
                engram_exact_path_key(&target.root),
                root_key(&worktree),
                "{key}"
            );
        }
        let bound =
            engram_check_fingerprint(&engram_check_command("cargo test --lib").expect("a test"));
        assert!(
            turn.record(|record| {
                record
                    .engram
                    .active_turn_checks
                    .iter()
                    .all(|check| engram_check_fingerprint(&check.command) == bound)
            }),
            "the check fingerprint is the bound test's, not the whole line's"
        );
        assert_eq!(
            take_pending_line(&turn),
            None,
            "a credited test needs no word"
        );
        assert_eq!(
            shell_key(&turn),
            Some(root_key(&worktree)),
            "the shell is presumed in DIR after the line"
        );
    }
}

#[test]
fn a_one_call_line_writes_where_its_directory_is() {
    // A one-call line started with its shell presumed elsewhere may write in
    // DIR's worktree: the worktrees a command may write in, which overlap
    // marking reads, include it.
    for kind in PATH_KINDS {
        let turn = CheckedTurn::start(&format!("one-call-writer{kind}"), true);
        let worktree = name_turn_source_root(&turn);
        // The worktrees follow the line's change of directory, whether or
        // not the form can read DIR: into DIR's worktree where TermAl follows
        // a `cd` into it, and anywhere (`None`) where it cannot, which covers
        // DIR's.
        let (root, _) = root_as_named(&worktree);
        let line = format!("pushd \"{root}\" && cargo test");
        let workdir = turn.root.to_string_lossy().into_owned();
        let worktrees = engram_command_worktrees(&workdir, Some(&[None][..]), Some(&line));
        let root_worktree = engram_worktree_root(&fs::canonicalize(&worktree).expect("canonical"));
        if cd_followed(&root) {
            assert!(
                worktrees.contains(&Some(root_worktree)),
                "{worktrees:?} includes DIR's worktree"
            );
        } else {
            assert!(worktrees.contains(&None), "{worktrees:?} includes anywhere");
        }
        assert!(
            worktrees.contains(&Some(engram_worktree_root(&turn.root))),
            "{worktrees:?} still includes the worktree the shell was presumed in"
        );
    }
}

#[test]
fn a_test_in_another_shape_of_line_is_no_check_and_the_agent_is_told_why() {
    for kind in PATH_KINDS {
        let turn = CheckedTurn::start(&format!("other-shapes{kind}"), true);
        let worktree = name_turn_source_root(&turn);
        let (root, readable) = root_as_named(&worktree);
        let fallback = format!("it from a session whose workdir is in {root}");
        // (key, line, whether the form may be named: only for a bare line).
        for (key, command, bare) in [
            ("cd", format!("cd \"{root}\" && cargo test"), true),
            (
                "bash",
                format!("bash -lc 'pushd \"{root}\" && cargo test'"),
                false,
            ),
            (
                "pwsh",
                format!("pwsh -Command \"pushd '{root}' && cargo test\""),
                false,
            ),
        ] {
            start_without_directory(&turn, key, &command);
            assert!(
                kept_checks(&turn).is_empty(),
                "{key}: {:?}",
                kept_checks(&turn)
            );
            let line = take_pending_line(&turn)
                .unwrap_or_else(|| panic!("{key}: the agent is told the test gets no credit"));
            assert!(
                line.contains("ran after another command on its line"),
                "{key}: {line}"
            );
            // It may have been meant for another project: the remedy is
            // offered for the case it was meant here.
            assert!(
                line.contains("If it was meant to count here, run"),
                "{key}: {line}"
            );
            let remedy = if bare && readable {
                form_in(&root, "cargo test")
            } else {
                fallback.clone()
            };
            assert!(line.contains(&remedy), "{key}: {line}");
        }
        // A runtime that reports its directory and wraps what it runs is
        // told to run the test where it should run, never a bare form it
        // could not run.
        turn.recorder()
            .command_started_in(
                "wrapped-cwd",
                "bash -lc 'cd sub && cargo test'",
                Some("bash -lc 'cd sub && cargo test'"),
                Some(&root),
            )
            .expect("the start should record");
        turn.wait_for_snapshots();
        let line = take_pending_line(&turn).expect("the agent is told the test gets no credit");
        assert!(
            line.contains(&directory_remedy(
                "If it was meant to count here, run",
                &root
            )),
            "{line}"
        );
        assert!(!line.contains("pushd"), "{line}");
    }
}

#[test]
fn a_one_call_line_elsewhere_or_to_a_missing_directory_gets_no_credit() {
    for kind in PATH_KINDS {
        let turn = CheckedTurn::start(&format!("one-call-elsewhere{kind}"), true);
        let worktree = name_turn_source_root(&turn);
        let (root, root_readable) = root_as_named(&worktree);
        let (main, main_readable) = root_as_named(&turn.root);

        // The main checkout is outside the named root: no check, and the
        // remedy names the form in the root, with its directory left to the
        // agent.
        start_without_directory(
            &turn,
            "elsewhere",
            &format!("pushd \"{main}\" && cargo test"),
        );
        assert!(kept_checks(&turn).is_empty(), "{:?}", kept_checks(&turn));
        let line = take_pending_line(&turn).expect("the agent is told why the test gets no credit");
        match (main_readable, root_readable) {
            (true, true) => {
                assert!(
                    line.contains(&format!("started in {main}, outside")),
                    "{line}"
                );
                assert!(line.contains(&form_in(&root, "cargo test")), "{line}");
            }
            (true, false) => {
                assert!(
                    line.contains(&format!("started in {main}, outside")),
                    "{line}"
                );
                assert!(
                    line.contains("run the tests in that root, or name the worktree you work in"),
                    "{line}"
                );
            }
            // A line the form cannot read is a test after another command.
            (false, _) => {
                assert!(
                    line.contains("ran after another command on its line"),
                    "{line}"
                );
                let remedy = if root_readable {
                    form_in(&root, "cargo test")
                } else {
                    near_miss_fallback(&root)
                };
                assert!(line.contains(&remedy), "{line}");
            }
        }
        end_command(
            &turn,
            "elsewhere",
            &format!("pushd \"{main}\" && cargo test"),
        );

        // A directory that does not exist: the `pushd` fails first, the test
        // never runs, and there is nothing to tell.
        start_without_directory(
            &turn,
            "missing",
            &format!("pushd \"{root}/missing\" && cargo test"),
        );
        assert!(kept_checks(&turn).is_empty(), "{:?}", kept_checks(&turn));
        let line = take_pending_line(&turn);
        if root_readable {
            assert_eq!(line, None);
        } else {
            let line = line.expect("the agent is told why the test gets no credit");
            assert!(
                line.contains("ran after another command on its line"),
                "{line}"
            );
            assert!(line.contains(&near_miss_fallback(&root)), "{line}");
        }
    }
}

#[test]
fn a_one_call_test_whose_argument_leads_out_gets_no_check_and_no_line() {
    // In the root, but testing a manifest outside it: no check, as for any
    // test, and nothing about its line to tell.
    for kind in PATH_KINDS {
        let turn = CheckedTurn::start(&format!("one-call-leads-out{kind}"), true);
        let worktree = name_turn_source_root(&turn);
        let (root, readable) = root_as_named(&worktree);
        start_without_directory(
            &turn,
            "leads-out",
            &format!("pushd \"{root}\" && cargo test --manifest-path ../../elsewhere/Cargo.toml"),
        );
        assert!(kept_checks(&turn).is_empty(), "{:?}", kept_checks(&turn));
        let line = take_pending_line(&turn);
        if readable {
            assert_eq!(line, None);
        } else {
            let line = line.expect("the agent is told why the test gets no credit");
            assert!(
                line.contains("ran after another command on its line"),
                "{line}"
            );
            assert!(line.contains(&near_miss_fallback(&root)), "{line}");
        }
    }
}

#[test]
fn the_remedy_never_names_a_directory_it_would_have_to_guess() {
    // A test meant for `ui` inside the root is never told to run from the
    // root itself, where it would test something else, or nothing: the
    // directory is left to the agent.
    for kind in PATH_KINDS {
        let turn = CheckedTurn::start(&format!("one-call-subdirectory{kind}"), true);
        let worktree = name_turn_source_root(&turn);
        fs::create_dir_all(worktree.join("ui")).expect("the subdirectory should exist");
        let (ui, _) = root_as_named(&worktree.join("ui"));
        let (root, readable) = root_as_named(&worktree);
        // The session's workdir, the main checkout, lies outside the root.
        let expected = if readable {
            form_in(&root, "npm test")
        } else {
            format!("it from a session whose workdir is in {root}")
        };
        for (key, before, test) in [
            // Its line changed to the root first, then to `ui`, before the
            // test.
            (
                "inside-line",
                None,
                format!("cd \"{root}\" && cd ui && npm test"),
            ),
            // A `cd` in an earlier call left the shell in `ui`, or lost it.
            (
                "after-cd",
                Some(format!("cd \"{ui}\"")),
                "npm test".to_owned(),
            ),
        ] {
            if let Some(before) = before {
                run_without_directory(&turn, "enter", &before, EngramCommandExit::Code(0));
            }
            start_without_directory(&turn, key, &test);
            let line =
                take_pending_line(&turn).expect("the agent is told why the test gets no credit");
            assert!(line.contains(&expected), "{key}: {line}");
            assert!(
                !line.contains(&format!("pushd \"{root}\" &&")),
                "{key}: {line}"
            );
            assert!(
                !line.contains(&format!("pushd \"{ui}\" &&")),
                "{key}: {line}"
            );
            end_command(&turn, key, &test);
        }
    }
}

#[test]
fn without_a_named_root_the_remedy_names_a_directory_in_the_sessions_worktree() {
    // The measured worktree is the session's; its tests run from the workdir
    // inside it (`ui`), which the agent names.
    for kind in PATH_KINDS {
        let turn = CheckedTurn::start_with(
            &format!("workdir-remedy{kind}"),
            true,
            Some("ui"),
            vec![checkpoint_reply(CHECK_GRANT)],
        );
        let workdir = turn.record(|record| record.session.workdir.clone());
        let (worktree, readable) = root_as_named(&turn.root);
        // The line's own `cd` moves the shell only if it succeeds: split into
        // a call of its own, it keeps the directory the test was meant for.
        let lead = "If it was meant to count here, run";
        let remedy = if readable {
            form_in(&worktree, "npm test")
        } else {
            shell_remedy(lead, &worktree)
        };
        let inside_line = format!("cd \"{workdir}\" && npm test");
        start_without_directory(&turn, "inside-line", &inside_line);
        let line = take_pending_line(&turn).expect("the agent is told why the test gets no credit");
        assert!(line.contains(lead), "{line}");
        assert!(line.contains(&remedy), "{line}");
        end_command(&turn, "inside-line", &inside_line);

        // A lost shell: the same remedy brings it back first.
        let remedy = if readable {
            form_in(&worktree, "npm test")
        } else {
            shell_remedy("Run", &worktree)
        };
        run_without_directory(&turn, "lose", "cd $OTHER", EngramCommandExit::Code(0));
        start_without_directory(&turn, "lost", "npm test");
        let line = take_pending_line(&turn).expect("the agent is told why the test gets no credit");
        assert!(line.contains(&remedy), "{line}");
    }
}

/// The remedy, after `lead`, a session whose workdir is in `worktree` is
/// given where the form cannot carry the test and TermAl follows a `cd` into
/// the worktree.
fn cd_recovery(lead: &str, worktree: &str) -> String {
    format!(
        "{lead} `cd \"DIR\"` in a call of its own, with DIR the absolute directory in \
         {worktree} where it runs, then run it as a line of its own."
    )
}

#[test]
fn a_session_whose_workdir_is_in_the_root_brings_its_shell_back_with_a_cd() {
    // Without the form (a pipe keeps it out on any path), a session that
    // reports no directory and whose workdir is in the worktree is told to
    // bring its shell where the test runs in a call of its own, whether
    // TermAl places the shell there, lost it, or it ran the test after
    // another command; followed, the test counts. Where TermAl follows no
    // `cd` into the path, it is told a new session instead. This holds with a
    // named root the workdir lies in, too.
    for (kind, named) in PATH_KINDS
        .into_iter()
        .flat_map(|kind| [(kind, false), (kind, true)])
    {
        let turn = CheckedTurn::start_with(
            &format!("shell-back{kind}"),
            true,
            Some("ui"),
            vec![checkpoint_reply(CHECK_GRANT)],
        );
        let worktree_path = if named {
            // A session working in its named root.
            let root = name_turn_source_root(&turn);
            let workdir = root.to_string_lossy().into_owned();
            turn.record_mut(|record| record.session.workdir = workdir);
            root
        } else {
            turn.root.clone()
        };
        let workdir = turn.record(|record| record.session.workdir.clone());
        let (worktree, _) = root_as_named(&worktree_path);
        let test = "cargo test | tail -3";
        let line_with_test = format!("echo ready && {test}");
        let if_meant = "If it was meant to count here, run";

        start_without_directory(&turn, "placed", &line_with_test);
        let line = take_pending_line(&turn).expect("the agent is told why the test gets no credit");
        assert!(
            line.contains("ran after another command on its line"),
            "{kind} {named}: {line}"
        );
        assert!(
            line.ends_with(&shell_remedy(if_meant, &worktree)),
            "{kind} {named}: {line}"
        );
        end_command(&turn, "placed", &line_with_test);

        run_without_directory(&turn, "lose", "cd $OTHER", EngramCommandExit::Code(0));
        start_without_directory(&turn, "lost", test);
        let line = take_pending_line(&turn).expect("the agent is told why the test gets no credit");
        assert!(
            line.contains("cannot tell where its shell is"),
            "{kind} {named}: {line}"
        );
        assert!(
            line.ends_with(&shell_remedy("Run", &worktree)),
            "{kind} {named}: {line}"
        );
        end_command(&turn, "lost", test);
        start_without_directory(&turn, "lost-inside-line", &line_with_test);
        let line = take_pending_line(&turn).expect("the agent is told why the test gets no credit");
        assert!(
            line.ends_with(&shell_remedy(if_meant, &worktree)),
            "{kind} {named}: {line}"
        );
        end_command(&turn, "lost-inside-line", &line_with_test);

        if !cd_followed(&worktree) {
            continue;
        }
        run_without_directory(
            &turn,
            "back",
            &format!("cd \"{workdir}\""),
            EngramCommandExit::Code(0),
        );
        start_without_directory(&turn, "found", test);
        assert!(
            kept_checks(&turn).iter().any(|(key, _)| key == "found"),
            "{kind} {named}: {:?}",
            kept_checks(&turn)
        );
        assert_eq!(
            take_pending_line(&turn),
            None,
            "a credited test needs no word"
        );
    }
}

#[test]
fn a_shell_in_a_path_no_cd_can_follow_is_left_to_a_new_session() {
    // TermAl follows no `cd` into a path a shell would expand, so only a new
    // session starts with a shell it places there.
    let workdir = if cfg!(windows) {
        r"C:\ch$ild"
    } else {
        "/ch$ild"
    };
    let facts = EngramRemedyFacts {
        template: None,
        reports_directory: false,
        workdir_inside: true,
    };
    for (reason, lead) in [
        (EngramUnconfirmedReason::ShellUnknown, "Run"),
        (
            EngramUnconfirmedReason::InsideLine,
            "If it was meant to count here, run",
        ),
    ] {
        let line = engram_source_root_unconfirmed_line(workdir, reason, facts);
        assert!(line.ends_with(&new_session_remedy(lead, workdir)), "{line}");
    }
}

#[test]
fn a_session_whose_workdir_is_outside_the_named_root_is_told_to_use_one_inside() {
    // A shell that reports no directory counts in a named root only when the
    // session's workdir lies in it too; without the form (a pipe keeps it
    // out on any path), a session whose workdir is outside is told so,
    // whether TermAl followed its `cd` into the root or not.
    for kind in PATH_KINDS {
        let turn = CheckedTurn::start(&format!("outside-root{kind}"), true);
        let worktree = name_turn_source_root(&turn);
        let (root, _) = root_as_named(&worktree);
        run_without_directory(
            &turn,
            "enter",
            &format!("cd \"{root}\""),
            EngramCommandExit::Code(0),
        );
        start_without_directory(&turn, "piped", "cargo test | tail -3");
        assert!(kept_checks(&turn).is_empty(), "{:?}", kept_checks(&turn));
        let line = take_pending_line(&turn).expect("the agent is told why the test gets no credit");
        assert!(
            line.ends_with(&format!(
                "Run it from a session whose workdir is in {root}."
            )),
            "{kind}: {line}"
        );
    }
}

#[test]
fn a_runtime_that_reports_its_directory_is_told_to_set_it_where_the_test_runs() {
    // A runtime that reports its directory starts each command where it
    // says, so a `cd` in a call of its own would not carry over: a test it
    // ran after another command, in a shell of its own, is told to run as a
    // line of its own with its working directory there; so run, it counts.
    for kind in PATH_KINDS {
        let turn = CheckedTurn::start_with(
            &format!("reported-directory{kind}"),
            true,
            Some("ui"),
            vec![checkpoint_reply(CHECK_GRANT)],
        );
        let workdir = turn.record(|record| record.session.workdir.clone());
        let (worktree, _) = root_as_named(&turn.root);
        fs::create_dir_all(turn.root.join("crate")).expect("the subdirectory should exist");
        let (inside, _) = root_as_named(&turn.root.join("crate"));
        let wrapped = "bash -lc 'cd ../crate && cargo test'";
        turn.recorder()
            .command_started_in("wrapped", wrapped, Some(wrapped), Some(&workdir))
            .expect("the start should record");
        turn.wait_for_snapshots();
        let line = take_pending_line(&turn).expect("the agent is told why the test gets no credit");
        assert!(
            line.ends_with(&directory_remedy(
                "If it was meant to count here, run",
                &worktree
            )),
            "{kind}: {line}"
        );
        assert!(!line.contains("pushd"), "{kind}: {line}");

        let alone = "bash -lc 'cargo test'";
        turn.recorder()
            .command_started_in("alone", alone, Some(alone), Some(&inside))
            .expect("the start should record");
        turn.wait_for_snapshots();
        assert!(
            kept_checks(&turn).iter().any(|(key, _)| key == "alone"),
            "{kind}: {:?}",
            kept_checks(&turn)
        );
        assert_eq!(
            take_pending_line(&turn),
            None,
            "a credited test needs no word"
        );
    }
}

#[test]
fn without_a_named_root_a_shell_moved_inside_its_worktree_needs_no_word() {
    // Without a named root the workdir lies in the worktree measured, so a
    // shell TermAl follows anywhere in it leaves no doubt where a test ran: a
    // shell that may return to the workdir matters only with a named root.
    // Where TermAl follows no `cd` into the path, the shell is lost instead.
    for kind in PATH_KINDS {
        let turn = CheckedTurn::start_with(
            &format!("unnamed-shell-inside{kind}"),
            true,
            Some("ui"),
            vec![checkpoint_reply(CHECK_GRANT)],
        );
        fs::create_dir_all(turn.root.join("crate")).expect("the subdirectory should exist");
        let (inside, _) = root_as_named(&turn.root.join("crate"));
        let (worktree, readable) = root_as_named(&turn.root);
        run_without_directory(
            &turn,
            "enter",
            &format!("cd \"{inside}\""),
            EngramCommandExit::Code(0),
        );
        start_without_directory(&turn, "inside", "cargo test");
        if cd_followed(&inside) {
            assert_eq!(
                kept_checks(&turn)
                    .iter()
                    .map(|(key, _)| key.as_str())
                    .collect::<Vec<_>>(),
                ["inside"]
            );
            assert_eq!(
                take_pending_line(&turn),
                None,
                "a credited test needs no word"
            );
            continue;
        }
        assert!(kept_checks(&turn).is_empty(), "{:?}", kept_checks(&turn));
        let line = take_pending_line(&turn).expect("the agent is told why the test gets no credit");
        assert!(line.contains("cannot tell where its shell is"), "{line}");
        let remedy = if readable {
            form_in(&worktree, "cargo test")
        } else {
            new_session_remedy("Run", &worktree)
        };
        assert!(line.contains(&remedy), "{line}");
    }
}

#[test]
fn a_delegated_session_whose_test_started_elsewhere_is_told_to_run_its_tests_in_its_workdir() {
    // A delegated session names no root, so it is never told to name one or
    // given a form for another worktree.
    let workdir = if cfg!(windows) { r"C:\child" } else { "/child" };
    let line = engram_source_root_withheld_line(
        FsPath::new(grammar_dir()),
        workdir,
        false,
        true,
        Some("cargo test"),
    );
    assert!(
        line.ends_with(
            "and gets no credit: run the tests in your workdir, where your turns are measured."
        ),
        "{line}"
    );
    assert!(!line.contains("pushd"), "{line}");
}

#[test]
fn a_one_call_check_is_credited_to_its_own_directory_on_any_path() {
    // The crediting does not depend on the form's characters, so it is
    // checked on every kind of test directory.
    for kind in PATH_KINDS {
        let turn = CheckedTurn::start(&format!("one-call-any-path{kind}"), true);
        let worktree = name_turn_source_root(&turn);
        let named = turn
            .record(|record| record.engram.active_turn_source_root.clone())
            .expect("the turn has a named root");
        let credit_root = Some((FsPath::new(&named.root), named.common_dir_key.as_str()));
        let check = |directory: &FsPath| EngramCheckCommand {
            directory: Some(
                fs::canonicalize(directory)
                    .expect("the directory canonicalizes")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ..engram_check_command("cargo test --lib").expect("a test")
        };
        let target = engram_check_worktree_from(&check(&worktree), &turn.root, None, credit_root)
            .expect("a check is credited to its own directory");
        assert_eq!(engram_exact_path_key(&target.root), root_key(&worktree));
        assert_eq!(
            engram_check_worktree_from(&check(&turn.root), &turn.root, None, credit_root),
            None,
            "its own directory outside the root is no check of it"
        );
    }
}

#[test]
fn a_one_call_test_whose_runner_stated_no_result_is_reported_as_unknown() {
    // Its exit does not say whether the `pushd` or the test failed: nothing
    // in its output shows its runner ran.
    for kind in PATH_KINDS {
        let turn = CheckedTurn::start(&format!("one-call-fails{kind}"), true);
        let (root, readable) = root_as_named(&turn.root);
        let command = format!("pushd \"{root}\" && cargo test");
        if !readable {
            // No form can name this test directory: no check at all.
            assert_eq!(engram_check_command(&command), None);
            continue;
        }
        run_with_output(
            &turn,
            "check-1",
            &command,
            "The system cannot find the path specified.",
            EngramCommandExit::Code(1),
        );
        let checkpoint = turn.finish();

        let observations = observations(&checkpoint);
        assert_eq!(observations[0]["outcome"], "unknown", "{checkpoint:#}");
        // Nor do its summary and refs pin the exit on the test.
        let evidence = &checkpoint["verification_evidence"][0];
        assert_eq!(
            evidence["summary"],
            format!(
                "`{command}` exited 1, which does not say whether its `pushd` or its test failed"
            ),
            "{checkpoint:#}"
        );
        assert_eq!(evidence["refs"], serde_json::json!(["command:cargo test"]));
    }
}

#[test]
fn a_one_call_test_whose_runner_stated_its_result_fails_as_its_own() {
    // Its runner ran, so its `pushd` succeeded and the line's exit is the
    // test's: a failure is recorded as one, with its exit.
    for kind in PATH_KINDS {
        let turn = CheckedTurn::start(&format!("one-call-runner-failed{kind}"), true);
        let (root, readable) = root_as_named(&turn.root);
        let command = format!("pushd \"{root}\" && cargo test");
        if !readable {
            // No form can name this test directory: no check at all.
            assert_eq!(engram_check_command(&command), None);
            continue;
        }
        run_with_output(
            &turn,
            "check-1",
            &command,
            "running 3 tests\ntest result: FAILED. 2 passed; 1 failed",
            EngramCommandExit::Code(101),
        );
        let checkpoint = turn.finish();

        let observations = observations(&checkpoint);
        assert_eq!(observations[0]["outcome"], "failed", "{checkpoint:#}");
        let evidence = &checkpoint["verification_evidence"][0];
        assert_eq!(
            evidence["summary"],
            format!("`{command}` exited 101\ntest result: FAILED. 2 passed; 1 failed"),
            "{checkpoint:#}"
        );
        assert_eq!(
            evidence["refs"],
            serde_json::json!(["command:cargo test", "exit:101"])
        );
    }
}

#[test]
fn a_one_call_lines_result_lines_reach_its_outcome_and_evidence_on_any_host() {
    // The two tests above reach a one-call line's end only where the host's
    // temp path is one the form can name. A finished check's record needs no
    // path, so here the runner's result lines are carried from the check's
    // end to its outcome, summary and refs on every host.
    let turn = CheckedTurn::start("one-call-result-lines", true);
    let line = format!("pushd \"{}\" && cargo test", grammar_dir());
    for (stated, outcome, summary, refs) in [
        (
            vec!["test result: FAILED. 2 passed; 1 failed".to_owned()],
            EngramExecutionOutcome::Failed,
            format!("`{line}` exited 101\ntest result: FAILED. 2 passed; 1 failed"),
            vec!["command:cargo test", "exit:101"],
        ),
        (
            Vec::new(),
            EngramExecutionOutcome::Unknown,
            format!(
                "`{line}` exited 101, which does not say whether its `pushd` or its test failed"
            ),
            vec!["command:cargo test"],
        ),
    ] {
        let mut check = finished_check(0, engram_ready_basis_capture());
        check.command = engram_check_command(&line).expect("the one-call form");
        let end = check.end.as_mut().expect("a finished check");
        end.exit = EngramCommandExit::Code(101);
        end.result_lines = stated;
        let resolved = engram_resolve_turn_checks(
            &turn.session_id,
            CHECK_GRANT,
            vec![check],
            std::time::Instant::now() + DEADLOCK_GUARD,
        );
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].outcome, outcome);
        let (report, _) = turn.record(|record| {
            engram_turn_report(
                record,
                &turn.session_id,
                CHECK_GRANT,
                EngramExecutionOutcome::Succeeded,
                false,
                None,
                resolved,
            )
        });
        assert_eq!(report.observations[0].outcome, outcome);
        let evidence = &report.verification_evidence[0];
        assert_eq!(evidence.summary.as_deref(), Some(summary.as_str()));
        assert_eq!(evidence.refs, refs);
    }
}

#[test]
fn a_one_call_lines_evidence_names_the_line_and_never_pins_a_non_zero_exit_on_its_test() {
    let line = format!("pushd \"{}\" && cargo test", grammar_dir());
    let one_call = engram_check_command(&line).expect("the one-call form");
    assert_eq!(
        engram_check_summary(&one_call, EngramCommandExit::Code(0), &[]),
        format!("`{line}` exited 0")
    );
    assert_eq!(
        engram_check_refs(&one_call, EngramCommandExit::Code(0), &[]),
        ["command:cargo test", "exit:0"]
    );
    assert_eq!(
        engram_check_summary(&one_call, EngramCommandExit::Code(1), &[]),
        format!("`{line}` exited 1, which does not say whether its `pushd` or its test failed")
    );
    assert_eq!(
        engram_check_refs(&one_call, EngramCommandExit::Code(1), &[]),
        ["command:cargo test"]
    );
    // Unless its runner stated its result, which it did only once it ran.
    let stated = ["test result: FAILED. 0 passed; 1 failed".to_owned()];
    assert_eq!(
        engram_check_summary(&one_call, EngramCommandExit::Code(1), &stated),
        format!("`{line}` exited 1\ntest result: FAILED. 0 passed; 1 failed")
    );
    assert_eq!(
        engram_check_refs(&one_call, EngramCommandExit::Code(1), &stated),
        ["command:cargo test", "exit:1"]
    );
    // A directory too long for the bound is shortened from its start, so the
    // test is always named.
    let long_line = format!(
        "pushd \"{}/{}\" && cargo test",
        grammar_dir(),
        "d".repeat(1100)
    );
    let long = engram_check_command(&long_line).expect("the one-call form");
    let summary = engram_check_summary(&long, EngramCommandExit::Code(1), &[]);
    let named = summary.split('`').nth(1).expect("the line is named");
    assert!(named.starts_with("pushd \"…ddd"), "{summary}");
    assert!(named.ends_with("ddd\" && cargo test"), "{summary}");
    assert!(named.len() <= ENGRAM_CHECK_REF_MAX_BYTES, "{}", named.len());
    // A test near the bound itself still leaves the line within it, its
    // directory reduced to the ellipsis.
    let near_line = format!(
        "pushd \"{}\" && cargo test {}",
        grammar_dir(),
        "t".repeat(ENGRAM_CHECK_REF_MAX_BYTES - "cargo test ".len() - 5)
    );
    let near = engram_check_command(&near_line).expect("the one-call form");
    let summary = engram_check_summary(&near, EngramCommandExit::Code(0), &[]);
    let named = summary.split('`').nth(1).expect("the line is named");
    assert!(
        named.starts_with("pushd \"…\" && cargo test ttt"),
        "{named}"
    );
    assert!(named.len() <= ENGRAM_CHECK_REF_MAX_BYTES, "{}", named.len());
    // A plain test's exit is its own.
    let plain = engram_check_command("cargo test").expect("a test");
    assert_eq!(
        engram_check_refs(&plain, EngramCommandExit::Code(1), &[]),
        ["command:cargo test", "exit:1"]
    );
}

#[test]
fn a_text_kept_by_its_end_stays_within_its_bound_at_a_character_boundary() {
    // (text, bound, kept): an ellipsis marks a cut start, which never splits
    // a character; with no room for the ellipsis nothing is kept.
    for (text, max, kept) in [
        ("abc", 5, "abc"),
        ("abcde", 5, "abcde"),
        ("abcdef", 5, "…ef"),
        ("abcdef", 3, "…"),
        ("abcdef", 2, ""),
        ("aééé", 7, "aééé"),
        ("aééé", 6, "…é"),
        ("aééé", 5, "…é"),
    ] {
        let result = engram_keep_end_utf8(text, max);
        assert_eq!(result, kept, "{text} within {max}");
        assert!(result.len() <= max, "{text} within {max}: {result}");
    }
}

#[test]
fn only_a_runners_own_result_line_makes_a_one_call_failure_the_tests() {
    // Whatever a shell prints when its `pushd`, or its reading of the line,
    // fails is no runner's result line, for any runner, so the exit stays
    // unknown; a runner's own result line makes it the test's failure.
    let dir = grammar_dir();
    let shell_failures = [
        "bash: pushd: /c/missing: No such file or directory",
        "The system cannot find the path specified.",
        "At line:1 char:17\n+ pushd \"C:\\missing\" && cargo test\n+                 ~~\nThe token '&&' is \
         not a valid statement separator in this version.",
    ];
    for (test, runner_failed) in [
        ("cargo test", "test result: FAILED. 2 passed; 1 failed"),
        ("go test ./...", "FAIL\texample.com/pkg\t0.012s"),
        ("pytest -q", "1 failed, 2 passed in 0.52s"),
        ("npm test", "Tests  1 failed | 2 passed (3)"),
        (
            "node scripts/test-launcher.mjs full",
            "rust-tests: failed exit=101",
        ),
    ] {
        let check = engram_check_command(&format!("pushd \"{dir}\" && {test}"))
            .unwrap_or_else(|| panic!("`{test}` in the one-call form is a check"));
        let outcome = |output: &str| {
            let lines = engram_check_result_lines(&check.program, output);
            engram_check_command_outcome(&check, EngramCommandExit::Code(1), &lines)
        };
        for output in shell_failures {
            assert_eq!(
                outcome(output),
                Some(EngramExecutionOutcome::Unknown),
                "{test}: {output}"
            );
        }
        assert_eq!(
            outcome(runner_failed),
            Some(EngramExecutionOutcome::Failed),
            "{test}: {runner_failed}"
        );
    }
}

/// The reason a line in the one-call form but for its Git Bash spelling of a
/// drive in DIR is given.
const GIT_BASH_DIR_REASON: &str = "its line is the one-call form but for its DIR, written in Git \
     Bash's spelling of a drive (/c/…), which the form does not take, since not every shell \
     reads it as that drive (PowerShell reads it as \\c\\… on the current drive)";

#[test]
fn a_git_bash_dir_line_is_told_each_part_the_form_does_not_take() {
    let dir = grammar_dir();
    let facts = EngramRemedyFacts {
        template: Some("<test>"),
        reports_directory: false,
        workdir_inside: false,
    };
    let alone = engram_source_root_unconfirmed_line(
        dir,
        EngramUnconfirmedReason::OneCallGitBashDir { test_alone: true },
        facts,
    );
    assert!(
        alone.contains(&format!("because {GIT_BASH_DIR_REASON}.")),
        "{alone}"
    );
    let not_alone = engram_source_root_unconfirmed_line(
        dir,
        EngramUnconfirmedReason::OneCallGitBashDir { test_alone: false },
        facts,
    );
    assert!(
        not_alone.contains(
            "because its line starts as the one-call form, but with its DIR in Git Bash's \
             spelling of a drive (/c/…), which not every shell reads as that drive (PowerShell \
             reads it as \\c\\… on the current drive), and with more than its one test alone \
             after its `pushd`: the form takes neither."
        ),
        "{not_alone}"
    );
    for line in [alone, not_alone] {
        assert!(
            line.contains("If it was meant to count here, run it in one call as"),
            "{line}"
        );
    }
}

#[test]
fn a_one_call_line_with_a_git_bash_dir_is_told_to_write_it_as_windows_names_it() {
    // Claude's Bash tool is Git Bash on Windows, whose own spelling of a drive
    // (`/c/…`) the form does not take: the agent is told so, with the form
    // and DIR in Windows spelling, which then counts. Elsewhere `/c/…` is a
    // path of its own.
    for kind in PATH_KINDS {
        let turn = CheckedTurn::start(&format!("one-call-git-bash{kind}"), true);
        let worktree = name_turn_source_root(&turn);
        let (root, readable) = root_as_named(&worktree);
        if !cfg!(windows) {
            // No drive to spell: the root's own `/…` path is the form's DIR
            // where the form can read it, never a Git Bash near miss.
            let line_with_test = format!("pushd \"{root}\" && cargo test");
            assert_eq!(
                engram_one_call_git_bash_dir(&line_with_test),
                None,
                "{kind}"
            );
            assert_eq!(
                engram_check_command(&line_with_test).and_then(|check| check.directory),
                readable.then(|| root.clone()),
                "{kind}"
            );
            continue;
        }
        let git_bash_root = format!(
            "/{}{}",
            root[..1].to_ascii_lowercase(),
            root[2..].replace('\\', "/")
        );
        let line_with_test = format!("pushd \"{git_bash_root}\" && cargo test");
        start_without_directory(&turn, "git-bash", &line_with_test);
        assert!(kept_checks(&turn).is_empty(), "{:?}", kept_checks(&turn));
        let line = take_pending_line(&turn).expect("the agent is told why the test gets no credit");
        end_command(&turn, "git-bash", &line_with_test);
        if !readable {
            // Not the form even in Windows spelling: a test after another
            // command on its line.
            assert!(
                line.contains("ran after another command on its line"),
                "{kind}: {line}"
            );
            assert!(line.contains(&near_miss_fallback(&root)), "{kind}: {line}");
            continue;
        }
        assert!(
            line.contains(&format!("because {GIT_BASH_DIR_REASON}.")),
            "{kind}: {line}"
        );
        assert!(
            line.ends_with(&format!(
                "If it was meant to count here, run it in one call as \
                 {}{ENGRAM_ONE_CALL_DIR_SPELLING} and nothing piped, redirected or chained after \
                 the test.",
                form_in(&root, "cargo test")
            )),
            "{kind}: {line}"
        );

        let windows = format!("pushd \"{root}\" && cargo test");
        start_without_directory(&turn, "windows", &windows);
        assert!(
            kept_checks(&turn).iter().any(|(key, _)| key == "windows"),
            "{kind}: {:?}",
            kept_checks(&turn)
        );
        assert_eq!(
            take_pending_line(&turn),
            None,
            "a credited test needs no word"
        );
    }
}

#[test]
fn a_one_call_line_with_something_after_its_test_is_told_to_run_the_test_alone() {
    // A pipe hides the test's exit status, so its check could never pass: the
    // form takes the test alone, and the agent is told so, with the form,
    // rather than sent to another session. So is one with a command between
    // its `pushd` and its test.
    let reason = "its line starts as the one-call form, but the form takes only one test after \
                  its `pushd`, alone, with nothing before or after it and no shell of its own.";
    for kind in PATH_KINDS {
        let turn = CheckedTurn::start(&format!("one-call-piped{kind}"), true);
        let worktree = name_turn_source_root(&turn);
        let (root, readable) = root_as_named(&worktree);
        // (key, line, the test its remedy names: `<test>` where TermAl cannot
        // rebuild the test alone from the line).
        for (key, line_with_test, named_test) in [
            (
                "piped",
                format!("pushd \"{root}\" && cargo test 2>&1 | tail -40"),
                "<test>",
            ),
            (
                "before",
                format!("pushd \"{root}\" && cd ui && npm test"),
                "npm test",
            ),
        ] {
            start_without_directory(&turn, key, &line_with_test);
            assert!(kept_checks(&turn).is_empty(), "{:?}", kept_checks(&turn));
            let line =
                take_pending_line(&turn).expect("the agent is told why the test gets no credit");
            end_command(&turn, key, &line_with_test);
            if !readable {
                // No form: a test after another command on its line.
                assert!(
                    line.contains("ran after another command on its line"),
                    "{key}: {line}"
                );
                assert!(line.contains(&near_miss_fallback(&root)), "{key}: {line}");
                continue;
            }
            assert!(line.contains(reason), "{key}: {line}");
            assert!(
                line.ends_with(&format!(
                    "If it was meant to count here, run it in one call as \
                     {}{ENGRAM_ONE_CALL_DIR_SPELLING} and nothing piped, redirected or chained \
                     after the test.",
                    form_in(&root, named_test)
                )),
                "{key}: {line}"
            );
        }
        if !readable {
            continue;
        }

        let alone = format!("pushd \"{root}\" && cargo test");
        start_without_directory(&turn, "alone", &alone);
        assert!(
            kept_checks(&turn).iter().any(|(key, _)| key == "alone"),
            "{:?}",
            kept_checks(&turn)
        );
        assert_eq!(
            take_pending_line(&turn),
            None,
            "a credited test needs no word"
        );
    }
}

#[test]
fn a_runtime_that_reports_its_directory_is_credited_to_a_one_call_lines_directory() {
    // The line names where its test runs, so the directory the runtime
    // reported for it (the main checkout) does not decide.
    for kind in PATH_KINDS {
        let turn = CheckedTurn::start(&format!("one-call-reported-cwd{kind}"), true);
        let worktree = name_turn_source_root(&turn);
        let (root, readable) = root_as_named(&worktree);
        let (main, _) = root_as_named(&turn.root);
        let command = format!("pushd \"{root}\" && cargo test");
        turn.recorder()
            .command_started_in("reported", &command, Some(&command), Some(&main))
            .expect("the start should record");
        turn.wait_for_snapshots();
        let checks = kept_checks(&turn);
        if !readable {
            // No form: a test after another command on its line, from a
            // runtime that reports its directory.
            assert!(checks.is_empty(), "{checks:?}");
            let line =
                take_pending_line(&turn).expect("the agent is told why the test gets no credit");
            assert!(
                line.contains("ran after another command on its line"),
                "{line}"
            );
            assert!(
                line.ends_with(&directory_remedy(
                    "If it was meant to count here, run",
                    &root
                )),
                "{line}"
            );
            continue;
        }
        assert_eq!(
            checks
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>(),
            ["reported"]
        );
        assert_eq!(
            engram_exact_path_key(&checks[0].1.root),
            root_key(&worktree)
        );
        assert_eq!(
            take_pending_line(&turn),
            None,
            "a credited test needs no word"
        );
    }
}

#[test]
fn a_repeated_report_of_a_one_call_line_keeps_its_check_until_it_names_another_directory() {
    for kind in PATH_KINDS {
        let turn = CheckedTurn::start(&format!("one-call-repeated{kind}"), true);
        let worktree = name_turn_source_root(&turn);
        let (root, readable) = root_as_named(&worktree);
        let (main, _) = root_as_named(&turn.root);
        let command = format!("pushd \"{root}\" && cargo test");
        start_without_directory(&turn, "repeated", &command);
        if !readable {
            // No form: a test after another command on its line.
            assert!(kept_checks(&turn).is_empty(), "{:?}", kept_checks(&turn));
            let line =
                take_pending_line(&turn).expect("the agent is told why the test gets no credit");
            assert!(
                line.contains("ran after another command on its line"),
                "{line}"
            );
            assert!(line.contains(&near_miss_fallback(&root)), "{line}");
            continue;
        }
        let keys = |turn: &CheckedTurn| {
            kept_checks(turn)
                .into_iter()
                .map(|(key, _)| key)
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(&turn), ["repeated"]);
        // The same line reported again is the same test in the same place.
        start_without_directory(&turn, "repeated", &command);
        assert_eq!(keys(&turn), ["repeated"]);
        // Reported with another directory, the check no longer says where
        // its test ran.
        start_without_directory(
            &turn,
            "repeated",
            &format!("pushd \"{main}\" && cargo test"),
        );
        assert!(keys(&turn).is_empty(), "{:?}", keys(&turn));
    }
}

#[test]
fn the_named_root_line_names_the_form_only_to_claude_and_for_a_root_the_form_reads() {
    let entry = |root: &str| EngramWorkSourceRoot {
        store: EngramAuthorityStoreKey {
            database_path: PathBuf::from("C:/store/engram.db"),
            project_id: "github.com/example/project".to_owned(),
        },
        work_id: "work-a".to_owned(),
        short_ref: "w-a".to_owned(),
        claim_id: "claim-a".to_owned(),
        claim_fence: 1,
        root: root.to_owned(),
        common_dir_key: "c:/p/.git".to_owned(),
        named_by_session: "session-1".to_owned(),
        named_at: "2026-09-27T00:00:00.000Z".to_owned(),
        generation: 1,
    };
    let (plain, expanded) = if cfg!(windows) {
        (r"C:\p\wt", r"C:\p\w$t")
    } else {
        ("/p/wt", "/p/w$t")
    };
    let line = engram_source_root_line(Some(&entry(plain)), "work a", "C:/p", true);
    assert!(
        line.contains(&format!(
            "Tests count for it only when they run there: start each in one call as \
             `pushd \"DIR\" && <test>`, with DIR the absolute directory in it where the test \
             runs{ENGRAM_ONE_CALL_DIR_SPELLING} and nothing piped"
        )),
        "{line}"
    );
    // On Windows it says how DIR is written, since Claude's Bash tool (Git
    // Bash) favours a spelling the form does not take.
    assert_eq!(
        line.contains("not Git Bash's /c/…"),
        cfg!(windows),
        "{line}"
    );
    // Not to a session that is told the form only once a test goes without
    // credit, nor for a root the form cannot name.
    for (root, one_call) in [(plain, false), (expanded, true)] {
        let line = engram_source_root_line(Some(&entry(root)), "work a", "C:/p", one_call);
        assert!(
            !line.contains("pushd"),
            "no form the agent could not use: {line}"
        );
        assert!(
            line.ends_with("Tests count for it only when they run there."),
            "{line}"
        );
    }
    // Only Claude's runtime reports no directory and runs the bare line.
    for agent in [
        Agent::Codex,
        Agent::Claude,
        Agent::Cursor,
        Agent::Gemini,
        Agent::OpenCode,
        Agent::Kimi,
    ] {
        assert_eq!(
            engram_one_call_offered_up_front(agent),
            agent == Agent::Claude,
            "{agent:?}"
        );
    }
}
