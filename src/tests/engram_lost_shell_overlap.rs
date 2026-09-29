// Owns the tests of where a command of a shell TermAl lost may write, for
// overlap marking (src/engram_write_places.rs, and the lost-shell state of
// `EngramShellDirectory` in src/engram_check_paths.rs): which of a line's
// directory changes name a place TermAl can be sure of (an absolute literal
// target) and which may lead anywhere, the places a lost shell is kept among
// until TermAl follows it again, and that a lost shell leaves an open check
// in a worktree its changes cannot reach alone while any other change still
// marks it. Does not own following a shell or crediting a test
// (src/tests/engram_turn_checks.rs and its one-call child). New module, a
// child of src/tests/engram_turn_checks.rs, whose `CheckedTurn` fixture it
// uses.
use super::*;

/// `path` as a shell line names it: forward slashes, so the same line reads
/// alike in every shell.
fn line_path(path: &FsPath) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// The directory `path` resolves to, as TermAl records a resolved move.
fn resolved(path: &FsPath) -> String {
    fs::canonicalize(path)
        .expect("the directory resolves")
        .to_string_lossy()
        .into_owned()
}

/// `places`, sorted as a lost shell keeps them (`EngramShellDirectory::lose`:
/// by `engram_exact_path_key`, so a spelled workdir and resolved places sort
/// the same whatever their prefixes).
fn sorted(mut places: Vec<String>) -> Vec<String> {
    places.sort_by_cached_key(|place| engram_exact_path_key(FsPath::new(place)));
    places
}

/// A session of `turn`'s project working in `workdir`, and a recorder for
/// it. The recorder's `command_started` reports no directory, which is the
/// shape of a Claude command whatever the session's agent.
fn writer_in(turn: &CheckedTurn, workdir: &FsPath) -> (String, SessionRecorder) {
    let project_id = turn.record(|record| {
        record
            .session
            .project_id
            .clone()
            .expect("the root belongs to a project")
    });
    let session = create_test_project_session(&turn.state, Agent::Codex, &project_id, workdir);
    let recorder = SessionRecorder::new(turn.state.clone(), session.clone());
    (session, recorder)
}

/// Starts and ends `command` in `recorder`'s session, ending as `status`
/// with `exit`.
fn run_ending(
    recorder: &mut SessionRecorder,
    key: &str,
    command: &str,
    status: CommandStatus,
    exit: EngramCommandExit,
) {
    recorder
        .command_started(key, command)
        .expect("the start should record");
    recorder
        .command_completed_with_exit(key, command, "", status, exit)
        .expect("the end should record");
}

/// Starts and ends `command` in `recorder`'s session, successfully.
fn run_command(recorder: &mut SessionRecorder, key: &str, command: &str) {
    run_ending(
        recorder,
        key,
        command,
        CommandStatus::Success,
        EngramCommandExit::Code(0),
    );
}

/// Where TermAl presumes the shell of `session` to be.
fn shell_of(turn: &CheckedTurn, session: &str) -> EngramShellDirectory {
    let inner = turn.state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(session).expect("the writer");
    inner.sessions[index]
        .engram
        .shell_directory
        .clone()
        .expect("the writer's shell")
}

/// Where a next command, `key`, of `session` may write
/// (`engram_command_write_places`).
fn write_places_of(turn: &CheckedTurn, session: &str, key: &str) -> (Vec<Option<String>>, bool) {
    let inner = turn.state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(session).expect("the writer");
    let places = engram_command_write_places(&inner.sessions[index], key, None);
    (places.directories, places.anywhere)
}

/// The places the running command `key` of `session` may write in, as
/// recorded when it started or was last described.
fn running_worktrees(turn: &CheckedTurn, session: &str, key: &str) -> Vec<Option<String>> {
    let inner = turn.state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(session).expect("the writer");
    inner.sessions[index]
        .engram
        .running_command_worktrees
        .iter()
        .find(|(running, _)| running == key)
        .map(|(_, worktrees)| worktrees.clone())
        .expect("the command runs")
}

/// Gives `turn` one check, finished but with its closing snapshot still
/// pending, so it stays open to writes, credited to `turn`'s worktree;
/// returns the pending capture, to finish at the end.
fn open_check(turn: &CheckedTurn) -> Arc<EngramBasisCapture> {
    let pending = Arc::new(EngramBasisCapture::default());
    turn.record_mut(|record| {
        record.engram.active_turn_checks = vec![turn.finished_check(0, pending.clone())];
    });
    pending
}

fn overlapped(turn: &CheckedTurn) -> bool {
    turn.record(|record| record.engram.active_turn_checks[0].overlapped)
}

fn move_of(targets: &[String]) -> EngramLineChanges {
    EngramLineChanges {
        targets: targets.to_vec(),
        hidden: false,
    }
}

#[test]
fn a_lost_line_is_bounded_only_by_absolute_literal_targets() {
    // Git Bash's `/c/…` spelling is absolute on every system.
    let literal = |targets: &[&str]| EngramLineChanges {
        targets: targets.iter().map(|target| (*target).to_owned()).collect(),
        hidden: false,
    };
    // Only a line TermAl cannot follow as one move (`engram_shell_move`) is
    // read for its changes; each line below is one.
    let lost = |line: &str| {
        assert_eq!(engram_shell_move(line), EngramShellMove::Lost, "{line}");
        engram_line_changes(line)
    };
    // The shapes a Claude session in another repository used: a pipe after
    // the form, a change after another command, more than one change.
    assert_eq!(
        lost("pushd \"/c/work/repo/.worktrees/x\" && cargo test 2>&1 | tail -5"),
        literal(&["/c/work/repo/.worktrees/x"])
    );
    assert_eq!(
        lost("git status; cd /c/work/sub"),
        literal(&["/c/work/sub"])
    );
    assert_eq!(
        lost("cd /c/a && git status | tail -1; cd /c/b"),
        literal(&["/c/a", "/c/b"])
    );
    // Commands that only read may run before the last change.
    assert_eq!(
        lost("echo hi && cd /c/a && cargo test 2>&1 | tail -3"),
        literal(&["/c/a"])
    );
    assert_eq!(
        lost("ls /c/a; cd /c/a; git status | head"),
        literal(&["/c/a"])
    );
    // A word that only looks like a definition or an in-shell script, as an
    // argument, runs nothing.
    assert_eq!(lost("echo foo.ps1; cd /c/a"), literal(&["/c/a"]));
    assert_eq!(lost("ls call; cd /c/a"), literal(&["/c/a"]));
    assert_eq!(lost("echo FOO=1.ps1; cd /c/a"), literal(&["/c/a"]));
    assert_eq!(lost("cat cd.log; cd /c/a"), literal(&["/c/a"]));
    // A script behind a wrapper runs in another process, which cannot move
    // this shell, even where a flag's value stands in command position.
    assert_eq!(
        lost("git status; cd /c/a; sudo -u root ./setup.ps1"),
        literal(&["/c/a"])
    );
    assert_eq!(lost("ls cd..\\x; cd /c/a"), literal(&["/c/a"]));
    assert_eq!(lost("cat cd\\readme.md; cd /c/a"), literal(&["/c/a"]));
    // A substitution inside single quotes is text, not a command.
    assert_eq!(lost("echo '$(ln x y)'; cd /c/a; ls"), literal(&["/c/a"]));
    // Any other change may lead anywhere: a target TermAl cannot read, a
    // relative one (a subshell may leave the shell elsewhere to take it from,
    // a link may make its `..` land elsewhere), one given more than its
    // target, one bash reads otherwise, and every change on a line that may
    // repeat it, run another shell or script, or change the paths it names.
    for line in [
        "cd \"$OTHER\" && ls",
        "cd; ls",
        "cd -; ls",
        "popd; ls",
        "eval \"$X\"; ls",
        "git status; cd sub",
        "(cd /c/a); cd b",
        "cd /c/repos/a/link && cd .. && git checkout -- README.md",
        "cd /d C:\\other && git checkout .",
        "cd old new; ls",
        "cd /c/lin\\ked; ls",
        "for i in 1 2; do cd /c/a; done",
        "f() { cd /c/a; }; f; f",
        "1..2 | ForEach-Object { Set-Location /c/a }",
        "1..2 | % { cd /c/a }",
        "git status; cd /c/x; bash -lc 'cd ../y && git checkout .'",
        "git status; cd /c/x && source env.sh",
        "ln -sfn /c/check /c/cur && cd /c/cur; ls",
        "rm -rf /c/sub/.git && cd /c/sub; ls",
        "git worktree add /c/w && cd /c/w; ls",
        "if true; then ln -sfn /c/checked /c/other/link; fi; cd /c/other/link; ls",
        "sudo ln -sfn /c/a /c/cur && cd /c/cur; ls",
        "git status; cd /c/x && git checkout main",
        "git status; cd /c/x && git -C /c/y status",
        "if true; then . env.sh; fi; cd /c/a; ls",
        "alias g='cd /c/b'; cd /c/a | cat",
        "./relink.sh && cd /c/cur; ls",
        "python -c \"import os\" && cd /c/cur; ls",
        "cp -a /c/x /c/y && cd /c/y; ls",
        "npm run setup && cd /c/w; ls",
        "echo \"$(ln -sfn /c/checked /c/other/link)\"; cd /c/other/link; ls",
        "echo `ln -sfn /c/a /c/b`; cd /c/b; ls",
        "diff <(ls /c/a) <(ls /c/b); cd /c/b; ls",
        "git status; cd /c/w/sub; rd /s /q .git",
        "git status; cd /c/w/sub; find . -name .git -delete",
        "git status; cd /c/w; unlink cur",
        "echo start # '\ncd /c/checked\n# '\n(cd /c/other)",
        "echo \\' $(ln -sfn /c/a /c/cur) \\'; cd /c/cur; ls",
        "git status; cd /c/a; ./setup.ps1",
        "git status; cd /c/a; call setup.bat",
        "git status; cd /c/a; iex $script",
        "git status; cd /c/a; trap 'cd /c/b' DEBUG",
        // An assignment before a command does not hide what it runs.
        "git status; cd /c/a; FOO=1 . ./env.sh",
        "git status; cd /c/a; env FOO=1 ./setup.ps1",
        // PowerShell's and cmd's shortcuts name no target word.
        "cd..",
        "git status; cd.. /c/x",
        "git status; cd /c/a; cd\\windows",
        "git status; cd /c/a; cd~",
        "git status; cd /c/a; time cd..",
        "git status; cd /c/a; D:",
        "git status; cd /c/a 2>&1",
        "git status; cd C:/a; chdir..",
        "git status; cd /c/a; cd=..",
        "git status; cd /c/a; cd,..",
        // A reading program named by a path or with an extension may be
        // any program.
        "./ls; cd /c/a",
        "./cat.exe x; cd /c/a",
        "node_modules/.bin/cat x; cd /c/a",
        // A flag after a keyword or wrapper does not hide what it runs.
        "git status; cd /c/a; time -p . ./env.sh",
        "git status; cd /c/a; command -p . ./env.sh",
    ] {
        assert!(lost(line).hidden, "{line}");
    }
    // A command that only reads may run before the last change; eval is
    // not one, though it may change directory.
    let words = |line: &[&str]| {
        line.iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>()
    };
    assert!(engram_command_only_reads(&words(&["git", "status"])));
    assert!(engram_command_only_reads(&words(&["then", "cd", "/c/a"])));
    assert!(!engram_command_only_reads(&words(&["eval", "cd /c/a"])));
    for program in ["./ls", "cat.exe", "/usr/bin/cat", "git.exe"] {
        assert!(
            !engram_command_only_reads(&words(&[program, "status"])),
            "{program}"
        );
    }
    assert!(engram_command_only_reads(&words(&["LS", "-la"])));
    assert!(!engram_command_only_reads(&words(&[
        "git", "checkout", "main"
    ])));
    // An assignment is a name, `=` and a value; a flag or a path is not one.
    for word in ["FOO=1", "_x=", "a1=b=c"] {
        assert!(engram_assignment_word(word), "{word}");
    }
    for word in [
        "1a=b", "--flag=x", "a-b=c", "=x", ".", "./x=y", "cd=..", "CD=x", "chdir=x",
    ] {
        assert!(!engram_assignment_word(word), "{word}");
    }
    // Command position skips keywords, wrappers, flags and assignments, but
    // not a flag's separate value.
    assert_eq!(
        engram_command_position(&words(&["time", "-p", ".", "env.sh"])),
        Some(2)
    );
    assert_eq!(engram_command_position(&words(&["FOO=1", "then"])), None);
    assert_eq!(
        engram_command_position(&words(&["sudo", "-u", "root", "./setup.ps1"])),
        Some(2)
    );
    // A `cd` written against its target changes directory with no target
    // word; a word that merely starts with `cd` does not.
    for word in [
        "cd..", "CD..", "cd\\", "cd~", "cd/d", "CD/D", "cd..\\x", "cd.\\x", "cd.", "D:", "z:",
        "chdir..", "CHDIR\\", "chdir\\x", "cd=..", "cd,..", "chdir=x",
    ] {
        assert!(engram_directory_shortcut(word), "{word}");
        assert!(!engram_change_names_target(word), "{word}");
    }
    for word in [
        "cd",
        "cd.log",
        "cdk",
        "cd.exe",
        "D:x",
        "d:\\",
        "1:",
        "cd/deploy.sh",
        "cd/",
        "cd./x",
        "chdirx",
        "chdir.log",
    ] {
        assert!(!engram_directory_shortcut(word), "{word}");
    }
    assert_eq!(engram_shell_move("cd.. C:/x"), EngramShellMove::Lost);
    assert_eq!(engram_shell_move("cat cd.log"), EngramShellMove::Stays);
    // A shortcut changes directory only where a shell runs it, in command
    // position; as an argument it is a path.
    for line in [
        "git add cd/deploy.yml",
        "cat cd\\readme.md",
        "ls cd..",
        "robocopy C: D:",
        // Bash runs a script under a `cd` directory; it moves no shell.
        "cd/deploy.sh",
    ] {
        assert_eq!(engram_shell_move(line), EngramShellMove::Stays, "{line}");
    }
    for line in [
        "D:",
        "time cd..",
        "FOO=1 cd..",
        "cd/d C:/x",
        "cd /c/a 2>&1",
        "cd=..",
        "chdir.. C:/x",
    ] {
        assert_eq!(engram_shell_move(line), EngramShellMove::Lost, "{line}");
    }
    // An unclosed quote names nothing TermAl can read.
    assert_eq!(
        lost("cd \"/c/x; ls"),
        EngramLineChanges {
            targets: Vec::new(),
            hidden: true,
        }
    );
}

#[test]
fn a_move_resolves_absolute_targets_once_and_relative_ones_from_every_start() {
    let turn = CheckedTurn::start("lost-move-resolution", true);
    for directory in ["sub", "inner", "sub/inner"] {
        fs::create_dir_all(turn.root.join(directory)).expect("fixture directories");
    }
    let root = turn.root.to_string_lossy().into_owned();
    let sub = turn.root.join("sub").to_string_lossy().into_owned();

    // An absolute target names one place from anywhere; one that does not
    // exist yet (the line may make it) may lead anywhere.
    let absolute =
        engram_resolve_lost_move(&[None], &move_of(&[line_path(&turn.root.join("sub"))]));
    assert_eq!(absolute.targets, [resolved(&turn.root.join("sub"))]);
    assert!(!absolute.computed);
    let missing =
        engram_resolve_lost_move(&[None], &move_of(&[line_path(&turn.root.join("made"))]));
    assert!(
        missing.computed && missing.targets.is_empty(),
        "{missing:?}"
    );

    // A lone relative `cd` a command's own write places follow resolves
    // from every place the command may start in; from one it does not
    // resolve from, or an unknown one, it may lead anywhere.
    let both = engram_resolve_lost_move(
        &[Some(root.clone()), Some(sub.clone())],
        &move_of(&["inner".to_owned()]),
    );
    assert_eq!(
        sorted(both.targets.clone()),
        sorted(vec![
            resolved(&turn.root.join("inner")),
            resolved(&turn.root.join("sub").join("inner")),
        ])
    );
    assert!(!both.computed);
    fs::remove_dir(turn.root.join("sub").join("inner")).expect("the nested directory goes");
    let partly = engram_resolve_lost_move(
        &[Some(root.clone()), Some(sub)],
        &move_of(&["inner".to_owned()]),
    );
    assert!(partly.computed, "{partly:?}");
    let unknown = engram_resolve_lost_move(&[None], &move_of(&["sub".to_owned()]));
    assert!(
        unknown.computed && unknown.targets.is_empty(),
        "{unknown:?}"
    );

    // Past the places a lost shell is kept among, it stops resolving and
    // may lead anywhere.
    let many = (0..=ENGRAM_LOST_PLACES_LIMIT)
        .map(|index| {
            let place = turn.root.join("many").join(format!("d{index}"));
            fs::create_dir_all(place.join("x")).expect("a place with a subdirectory");
            Some(place.to_string_lossy().into_owned())
        })
        .collect::<Vec<_>>();
    let capped = engram_resolve_lost_move(&many, &move_of(&["x".to_owned()]));
    assert!(capped.computed && capped.targets.is_empty(), "{capped:?}");
}

#[test]
fn a_lost_shell_is_kept_among_the_places_its_changes_named_until_it_is_followed_again() {
    let turn = CheckedTurn::start("lost-among", true);
    fs::create_dir_all(turn.root.join("sub")).expect("a subdirectory");
    let workdir = turn.root.to_string_lossy().into_owned();
    let root = resolved(&turn.root);
    let sub = resolved(&turn.root.join("sub"));
    let (session, mut recorder) = writer_in(&turn, &turn.root);

    // A change the runtime reports twice (pending, then running) is the
    // same change.
    let line = format!(
        "cd \"{}\" && git status | tail -1",
        line_path(&turn.root.join("sub"))
    );
    recorder
        .command_started("lose", &line)
        .expect("the start should record");
    run_command(&mut recorder, "lose", &line);
    let lost = shell_of(&turn, &session);
    assert_eq!(lost.directory, None, "a pipe: TermAl cannot follow it");
    assert_eq!(
        lost.lost_among,
        sorted(vec![workdir.clone(), sub.clone()]),
        "where it was and where it led"
    );
    assert!(!lost.lost_unbounded, "{lost:?}");
    let mut places = vec![None];
    places.extend(
        sorted(vec![workdir.clone(), sub.clone()])
            .into_iter()
            .map(Some),
    );
    assert_eq!(
        write_places_of(&turn, &session, "next"),
        (places, false),
        "a command of the lost shell writes where it may be"
    );

    // A lone relative change of the lost shell may lead anywhere; no place
    // can narrow that until TermAl follows the shell again.
    run_command(&mut recorder, "relative", "cd sub");
    let lost = shell_of(&turn, &session);
    assert!(
        lost.lost_unbounded && lost.lost_among.is_empty(),
        "{lost:?}"
    );
    assert_eq!(write_places_of(&turn, &session, "next"), (vec![None], true));

    // A line that starts with a change to an absolute directory and changes
    // directory nowhere else, outside a pipeline or group, is followed again
    // once it succeeds, and the lost state is forgotten.
    run_command(
        &mut recorder,
        "anchor",
        &format!("cd \"{}\" && git status", line_path(&turn.root.join("sub"))),
    );
    let followed = shell_of(&turn, &session);
    assert_eq!(followed.directory, Some(sub.clone()));
    assert!(
        followed.lost_among.is_empty() && !followed.lost_unbounded,
        "{followed:?}"
    );

    // A change that fails may have stopped before or after its `cd`: the
    // shell is lost between where it was and where the `cd` leads.
    run_ending(
        &mut recorder,
        "fails",
        &format!("cd \"{}\"", line_path(&turn.root)),
        CommandStatus::Error,
        EngramCommandExit::Code(1),
    );
    let lost = shell_of(&turn, &session);
    assert_eq!(lost.directory, None);
    assert_eq!(lost.lost_among, sorted(vec![root, sub]));
    assert!(!lost.lost_unbounded);
}

#[test]
fn a_lost_shell_among_too_many_places_or_none_may_be_anywhere() {
    let mut shell = EngramShellDirectory::at(None, "/w".to_owned());
    shell.lose(vec!["/w/a".to_owned(), "/w/b".to_owned()], false);
    assert_eq!(shell.lost_among, ["/w", "/w/a", "/w/b"]);
    assert!(!shell.lost_unbounded);
    // A place spelled resolved and as the workdir gave it is one place.
    let mut spelled = EngramShellDirectory::at(None, "C:/w".to_owned());
    spelled.lose(vec!["//?/C:/w".to_owned(), "C:/w/a".to_owned()], false);
    assert_eq!(spelled.lost_among, ["C:/w", "C:/w/a"]);
    shell.lose(
        (0..ENGRAM_LOST_PLACES_LIMIT)
            .map(|index| format!("/w/d{index}"))
            .collect(),
        false,
    );
    assert!(
        shell.lost_unbounded && shell.lost_among.is_empty(),
        "{shell:?}"
    );

    // A lost shell that names no place at all, which no loss leaves, writes
    // anywhere rather than only in the workdir.
    let turn = CheckedTurn::start("lost-nowhere", true);
    let (session, _) = writer_in(&turn, &turn.root);
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session).expect("the writer");
        let runtime = inner.sessions[index].runtime.runtime_token();
        inner.sessions[index].engram.shell_directory = Some(EngramShellDirectory {
            runtime,
            directory: None,
            pending: None,
            lost_among: Vec::new(),
            lost_unbounded: false,
        });
    }
    assert_eq!(write_places_of(&turn, &session, "next"), (vec![None], true));
}

#[test]
fn a_move_left_pending_by_another_command_bounds_the_shell_to_both_places() {
    // A command whose end is never reported leaves its move pending: later
    // commands may run where the shell was or where that move leads, never
    // anywhere.
    let turn = CheckedTurn::start("pending-bound", true);
    fs::create_dir_all(turn.root.join("sub")).expect("a subdirectory");
    let workdir = turn.root.to_string_lossy().into_owned();
    let sub = resolved(&turn.root.join("sub"));
    let (session, mut recorder) = writer_in(&turn, &turn.root);
    recorder
        .command_started(
            "stuck",
            &format!("cd \"{}\"", line_path(&turn.root.join("sub"))),
        )
        .expect("the start should record");
    assert_eq!(
        write_places_of(&turn, &session, "later"),
        (vec![None, Some(workdir), Some(sub)], false)
    );

    // A pending move TermAl could not resolve may lead anywhere.
    let unresolved = CheckedTurn::start("pending-unresolved", true);
    let unresolved_workdir = unresolved.root.to_string_lossy().into_owned();
    let (session, mut recorder) = writer_in(&unresolved, &unresolved.root);
    recorder
        .command_started(
            "stuck",
            &format!("cd \"{}\"", line_path(&unresolved.root.join("missing"))),
        )
        .expect("the start should record");
    assert_eq!(
        write_places_of(&unresolved, &session, "later"),
        (vec![None, Some(unresolved_workdir)], true)
    );
    // It ran: the shell moved where TermAl cannot name, and may be anywhere.
    recorder
        .command_completed_with_exit(
            "stuck",
            &format!("cd \"{}\"", line_path(&unresolved.root.join("missing"))),
            "",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the end should record");
    let settled = shell_of(&unresolved, &session);
    assert!(
        settled.directory.is_none()
            && settled.pending.is_none()
            && settled.lost_unbounded
            && settled.lost_among.is_empty(),
        "{settled:?}"
    );
    assert_eq!(
        write_places_of(&unresolved, &session, "later"),
        (vec![None], true)
    );

    // A line TermAl cannot follow, while that move is still pending, may
    // leave the shell where the move led: anywhere.
    let absorbed = CheckedTurn::start("pending-absorbed", true);
    let (session, mut recorder) = writer_in(&absorbed, &absorbed.root);
    recorder
        .command_started(
            "stuck",
            &format!("cd \"{}\"", line_path(&absorbed.root.join("missing"))),
        )
        .expect("the start should record");
    run_command(
        &mut recorder,
        "lose",
        &format!("git status; cd \"{}\"", line_path(&absorbed.root)),
    );
    let lost = shell_of(&absorbed, &session);
    assert!(
        lost.pending.is_none() && lost.lost_unbounded && lost.lost_among.is_empty(),
        "{lost:?}"
    );
    assert_eq!(
        write_places_of(&absorbed, &session, "later"),
        (vec![None], true)
    );
}

#[test]
fn a_move_read_from_a_place_the_shell_left_is_placed_only_when_absolute() {
    // A lone `cd` read while TermAl presumed one place, but applied after
    // another command's move became pending (or TermAl lost the shell), may
    // start from any place the shell may now be in: an absolute target still
    // names one place, a relative one may lead anywhere.
    let turn = CheckedTurn::start("second-move", true);
    fs::create_dir_all(turn.root.join("a")).expect("a directory");
    fs::create_dir_all(turn.root.join("b")).expect("another directory");
    let workdir = turn.root.to_string_lossy().into_owned();
    let a = resolved(&turn.root.join("a"));
    let b = resolved(&turn.root.join("b"));
    let (session, _) = writer_in(&turn, &turn.root);
    let apply = |pending: bool, lost: bool, to: String| {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session).expect("the writer");
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let runtime = record.runtime.runtime_token();
        let mut shell = EngramShellDirectory::at(runtime.clone(), workdir.clone());
        if pending {
            shell.pending = Some(("other".to_owned(), Some(a.clone())));
        }
        if lost {
            shell.lose(vec![a.clone()], false);
        }
        record.engram.shell_directory = Some(shell);
        // Read while the shell was presumed in the workdir.
        engram_note_shell_move(
            record,
            "mine",
            &runtime,
            EngramShellMove::To(to),
            (Some(workdir.as_str()), Some(b.clone())),
            None,
        );
        record
            .engram
            .shell_directory
            .clone()
            .expect("the writer's shell")
    };

    // Another command's move became pending in between.
    let absolute = apply(true, false, line_path(&turn.root.join("b")));
    assert_eq!(absolute.directory, None);
    assert_eq!(
        absolute.lost_among,
        sorted(vec![workdir.clone(), a.clone(), b.clone()])
    );
    assert!(!absolute.lost_unbounded, "{absolute:?}");
    let relative = apply(true, false, "b".to_owned());
    assert!(
        relative.lost_unbounded && relative.lost_among.is_empty(),
        "`b` may be taken from where the pending move led: {relative:?}"
    );

    // TermAl lost the shell in between: an absolute move is still pending
    // and follows the shell once it has run; a relative one is not taken as
    // leading from the place it was read from.
    let absolute = apply(false, true, line_path(&turn.root.join("b")));
    assert_eq!(absolute.pending, Some(("mine".to_owned(), Some(b.clone()))));
    let relative = apply(false, true, "b".to_owned());
    assert_eq!(relative.pending, None, "{relative:?}");
    assert!(relative.lost_unbounded, "{relative:?}");
}

#[test]
fn a_later_command_of_a_lost_shell_writes_wherever_its_earlier_lines_led() {
    // Two lost lines, then a plain command: it may write where the first
    // line led, though the second led elsewhere.
    let turn = CheckedTurn::start("lost-accumulates", true);
    let other = sibling_worktree(&turn, "lost-accumulates-other");
    fs::create_dir_all(other.join("crates")).expect("a directory in the other repository");
    let (_, mut recorder) = writer_in(&turn, &other);
    run_command(
        &mut recorder,
        "into-the-root",
        &format!("cd \"{}\" && git status | tail -1", line_path(&turn.root)),
    );
    run_command(
        &mut recorder,
        "back",
        &format!(
            "cd \"{}\" && git status | tail -1",
            line_path(&other.join("crates"))
        ),
    );
    let pending = open_check(&turn);
    run_command(&mut recorder, "plain", "cargo build");
    assert!(overlapped(&turn), "the shell may still be in the root");
    pending.finish(None);
}

#[test]
fn a_lost_shell_elsewhere_leaves_an_open_check_alone_until_its_changes_may_reach_it() {
    let turn = CheckedTurn::start("lost-elsewhere", true);
    let other = sibling_worktree(&turn, "lost-elsewhere-other");
    fs::create_dir_all(other.join("crates")).expect("a directory in the other repository");
    let pending = open_check(&turn);
    let (session, mut recorder) = writer_in(&turn, &other);

    // The shapes that marked every check on the host: a pipe after the
    // change, a change after another command, and a command of the shell
    // lost that way.
    for (key, line) in [
        (
            "pipe",
            format!(
                "pushd \"{}\" && cargo build 2>&1 | tail -3",
                line_path(&other.join("crates"))
            ),
        ),
        ("after", format!("git status; cd \"{}\"", line_path(&other))),
        ("in-the-lost-shell", "cargo build".to_owned()),
    ] {
        run_command(&mut recorder, key, &line);
        assert!(!overlapped(&turn), "{key}: {line}");
    }

    // A lost line that names the check's worktree marks it, by that target
    // alone, not as anywhere.
    let line = format!(
        "pushd \"{}\" && cargo build 2>&1 | tail -1",
        line_path(&turn.root)
    );
    recorder
        .command_started("into-this-one", &line)
        .expect("the start should record");
    let worktrees = running_worktrees(&turn, &session, "into-this-one");
    assert!(
        worktrees.contains(&Some(engram_worktree_root(&turn.root))) && !worktrees.contains(&None),
        "{worktrees:?}"
    );
    assert!(
        overlapped(&turn),
        "a literal target in the check's worktree"
    );
    pending.finish(None);
}

#[test]
fn a_change_termal_cannot_place_exactly_still_marks_every_open_check() {
    // A target TermAl cannot read, a relative one, one the line may make
    // first, and one on a line that loops may each lead into the check's
    // worktree, now and for the lost shell's later commands.
    let turn = CheckedTurn::start("lost-anywhere", true);
    let other = sibling_worktree(&turn, "lost-anywhere-other");
    fs::create_dir_all(other.join("sub")).expect("a directory in the other repository");
    for line in [
        "cd \"$OTHER\" && git checkout -- README.md".to_owned(),
        "(cd sub); cd sub; git checkout -- README.md".to_owned(),
        "git worktree add ../made && cd ../made && cargo test 2>&1 | tail -3".to_owned(),
        // A comment's quotes quote nothing: the `cd` into the root between
        // them runs, though the words TermAl reads show only the last one.
        format!(
            "echo start # '\ncd \"{}\"\n# '\n(cd \"{}\")",
            line_path(&turn.root),
            line_path(&other.join("sub"))
        ),
        format!(
            "for i in 1 2; do cd \"{}\"; done; git checkout -- README.md",
            line_path(&other.join("sub"))
        ),
    ] {
        let pending = open_check(&turn);
        let (session, mut recorder) = writer_in(&turn, &other);
        run_command(&mut recorder, "first", "git status");
        assert!(!overlapped(&turn), "before: {line}");
        recorder
            .command_started("moves", &line)
            .expect("the start should record");
        assert!(
            running_worktrees(&turn, &session, "moves").contains(&None),
            "{line}"
        );
        assert!(overlapped(&turn), "{line}");
        recorder
            .command_completed_with_exit(
                "moves",
                &line,
                "",
                CommandStatus::Success,
                EngramCommandExit::Code(0),
            )
            .expect("the end should record");
        turn.record_mut(|record| record.engram.active_turn_checks[0].overlapped = false);
        run_command(&mut recorder, "after", "git checkout -- README.md");
        assert!(overlapped(&turn), "the shell may still be anywhere: {line}");
        pending.finish(None);
    }
    // cmd's `cd /d DIR` names its target after a switch: the switch is no
    // target, and the change may lead anywhere.
    let workdir = other.to_string_lossy().into_owned();
    let line = format!("cmd /c \"cd /d {} && git checkout .\"", turn.root.display());
    assert!(
        engram_command_worktrees(
            &workdir,
            &EngramCommandPlaces::at(vec![None]),
            Some(&line),
            None
        )
        .contains(&None),
        "{line}"
    );
}

#[test]
fn a_command_reported_more_than_once_writes_where_it_first_did() {
    // A runtime reports one command more than once (pending, running, a
    // description, its end), and another command's reports may come in
    // between: each report of the first resolves its line the same way.
    let turn = CheckedTurn::start("lost-repeated", true);
    let other = sibling_worktree(&turn, "lost-repeated-other");
    fs::create_dir_all(other.join("sub")).expect("a directory in the other repository");
    let pending = open_check(&turn);
    let (session, mut recorder) = writer_in(&turn, &other);
    let first = format!(
        "cd \"{}\" && git status | tail -1",
        line_path(&other.join("sub"))
    );
    let second = format!("git status; cd \"{}\"", line_path(&other));
    recorder
        .command_started("first", &first)
        .expect("the start should record");
    recorder
        .command_started("second", &second)
        .expect("the other start should record");
    recorder
        .command_described("first", Some(&first), None)
        .expect("the description should record");
    recorder
        .command_started("first", &first)
        .expect("the repeated start should record");
    for key in ["first", "second"] {
        let worktrees = running_worktrees(&turn, &session, key);
        assert!(!worktrees.contains(&None), "{key}: {worktrees:?}");
    }
    assert!(!overlapped(&turn));
    for (key, line) in [("first", &first), ("second", &second)] {
        recorder
            .command_completed_with_exit(
                key,
                line,
                "",
                CommandStatus::Success,
                EngramCommandExit::Code(0),
            )
            .expect("the end should record");
    }
    run_command(&mut recorder, "after", "git status");
    assert!(!overlapped(&turn), "a later command of the shell");
    assert!(!shell_of(&turn, &session).lost_unbounded);
    pending.finish(None);
}

#[test]
fn a_relative_change_of_a_lost_shell_may_write_anywhere_as_it_starts() {
    // The lost shell may be in a place TermAl knows only as resolved, not
    // as the shell spells it: through a link, bash's `cd ..` may land in a
    // worktree none of the resolved places' parents is. The command itself
    // counts in every worktree as it starts, not only the shell after it.
    let turn = CheckedTurn::start("lost-relative-now", true);
    let other = sibling_worktree(&turn, "lost-relative-now-other");
    fs::create_dir_all(other.join("crates")).expect("a directory in the other repository");
    let (session, mut recorder) = writer_in(&turn, &other);
    run_command(
        &mut recorder,
        "lose",
        &format!(
            "cd \"{}\" && git status | tail -1",
            line_path(&other.join("crates"))
        ),
    );
    let pending = open_check(&turn);
    recorder
        .command_started("up", "cd .. && git checkout -- README.md")
        .expect("the start should record");
    let worktrees = running_worktrees(&turn, &session, "up");
    assert!(worktrees.contains(&None), "{worktrees:?}");
    assert!(overlapped(&turn), "marked as the command starts");
    pending.finish(None);
}

#[test]
fn a_lone_absolute_change_of_a_lost_shell_writes_where_it_leads_as_it_starts() {
    // TermAl cannot name where the lost shell is, but an absolute target
    // needs no start: the command counts where it leads, not anywhere.
    let turn = CheckedTurn::start("lost-absolute-now", true);
    let other = sibling_worktree(&turn, "lost-absolute-now-other");
    fs::create_dir_all(other.join("crates")).expect("a directory in the other repository");
    let (session, mut recorder) = writer_in(&turn, &other);
    run_command(
        &mut recorder,
        "lose",
        &format!(
            "cd \"{}\" && git status | tail -1",
            line_path(&other.join("crates"))
        ),
    );
    let pending = open_check(&turn);
    let line = format!("cd \"{}\" && git status", line_path(&turn.root));
    recorder
        .command_started("into-the-root", &line)
        .expect("the start should record");
    let worktrees = running_worktrees(&turn, &session, "into-the-root");
    assert!(
        worktrees.contains(&Some(engram_worktree_root(&turn.root))),
        "{worktrees:?}"
    );
    assert!(!worktrees.contains(&None), "bounded: {worktrees:?}");
    assert!(overlapped(&turn), "its target is the check's worktree");
    pending.finish(None);
}

#[test]
fn a_line_resolved_for_the_shell_it_loses_is_not_resolved_again() {
    // A report resolves a line TermAl cannot follow once: the worktrees its
    // command may write in take the changes as resolved for the shell it
    // loses (`engram_line_lost_move`).
    let turn = CheckedTurn::start("lost-resolved-once", true);
    let other = sibling_worktree(&turn, "lost-resolved-once-other");
    let workdir = other.to_string_lossy().into_owned();
    let in_workdir = EngramCommandPlaces::at(vec![None]);
    let made = line_path(&turn.root.join("made"));
    let line = format!("git status; cd \"{made}\"");
    // Resolved here, a target that does not exist yet may lead anywhere.
    assert!(engram_command_worktrees(&workdir, &in_workdir, Some(&line), None).contains(&None));
    // Resolved already, it leads where the loss says.
    let loss = EngramLostMove {
        targets: vec![resolved(&turn.root)],
        computed: false,
    };
    let worktrees = engram_command_worktrees(&workdir, &in_workdir, Some(&line), Some(&loss));
    assert!(
        worktrees.contains(&Some(engram_worktree_root(&turn.root))),
        "{worktrees:?}"
    );
    assert!(!worktrees.contains(&None), "{worktrees:?}");
    // A lone `cd` resolves from where the command starts, and a script a
    // shell wrapper runs is not the line the loss was resolved for.
    for line in [
        format!("cd \"{made}\""),
        format!("bash -lc 'git status; cd \"{made}\"'"),
    ] {
        assert!(
            engram_command_worktrees(&workdir, &in_workdir, Some(&line), Some(&loss))
                .contains(&None),
            "{line}"
        );
    }
}

#[test]
fn a_relative_change_resolves_from_each_start_only_while_the_shell_is_followed() {
    // A relative `cd` of a command whose shell TermAl follows resolves from
    // each place the command may start in; of a lost shell's command, whose
    // places TermAl knows only resolved, it may lead anywhere.
    let turn = CheckedTurn::start("places-shell-lost", true);
    fs::create_dir_all(turn.root.join("sub")).expect("a subdirectory");
    let workdir = turn.root.to_string_lossy().into_owned();
    let root = Some(engram_worktree_root(&turn.root));
    let followed = EngramCommandPlaces::at(vec![None]);
    let worktrees = engram_command_worktrees(&workdir, &followed, Some("cd sub"), None);
    assert_eq!(worktrees, [root.clone()]);
    let lost = EngramCommandPlaces {
        shell_lost: true,
        ..EngramCommandPlaces::at(vec![None])
    };
    let worktrees = engram_command_worktrees(&workdir, &lost, Some("cd sub"), None);
    assert!(
        worktrees.contains(&None) && worktrees.contains(&root),
        "{worktrees:?}"
    );
    // An absolute target still names its one place from anywhere.
    let line = format!("cd \"{}\"", line_path(&turn.root.join("sub")));
    let worktrees = engram_command_worktrees(&workdir, &lost, Some(&line), None);
    assert_eq!(worktrees, [root]);
}
