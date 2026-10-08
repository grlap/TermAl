// Declared test commands: a command a repository declares in
// termal-tests.toml, run exactly as declared, is a test check judged from the
// result artifact it writes (docs/features/declared-test-commands.md).
//
// Owns: end-to-end turn tests of the declared route through the real
// command-start, completion and checkpoint path, the guards that the built-in
// recognition is unchanged beside it, and the unit tests of the declaration
// (`engram_declared_tests.rs`), the artifact rules and the TRX reading
// (`engram_trx_artifact.rs`), on real captured TRX files
// (fixtures/declared-trx).
// Does not own: built-in recognition and outcome rules, whose tests stay in
// engram_turn_checks.rs.
use super::*;

/// The declared .NET line of the documentation's example, for a project at
/// the repository root.
const DECLARED_TEST: &str =
    "dotnet test --logger \"trx;LogFileName=results.trx\" --results-directory .termal-results";

/// The result file `DECLARED_TEST` writes, relative to the repository root.
const DECLARED_ARTIFACT: &str = ".termal-results/results.trx";

/// Real TRX files `dotnet test --logger trx` wrote (fixtures/declared-trx).
const PASSING_TRX: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/tests/fixtures/declared-trx/pass.trx"
));
const FAILING_TRX: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/tests/fixtures/declared-trx/fail.trx"
));
const ZERO_TRX: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/tests/fixtures/declared-trx/zero.trx"
));
const MIXED_TRX: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/tests/fixtures/declared-trx/mixed.trx"
));

/// Commits a termal-tests.toml declaring `command` with `artifact`, and a
/// .gitignore for the result directory, so neither is a change the test
/// makes to its own source.
fn declare(turn: &CheckedTurn, command: &str, artifact: &str) {
    let declaration = format!(
        "[[test]]\ncommand = {}\ncwd = \".\"\nartifact = {}\n",
        toml_string(command),
        toml_string(artifact)
    );
    fs::write(turn.root.join("termal-tests.toml"), declaration).expect("declaration writes");
    fs::write(turn.root.join(".gitignore"), ".termal-results/\n").expect("ignore file writes");
    run_git_test_command(&turn.root, &["add", "termal-tests.toml", ".gitignore"]);
    run_git_test_command(&turn.root, &["commit", "--quiet", "-m", "declare tests"]);
}

/// `value` as a TOML basic string.
fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Writes the result file the declared command produces, as the command
/// would while it runs.
fn write_artifact(turn: &CheckedTurn, content: &[u8]) {
    let path = turn.root.join(DECLARED_ARTIFACT);
    fs::create_dir_all(path.parent().expect("artifact has a directory"))
        .expect("result directory creates");
    fs::write(path, content).expect("artifact writes");
}

/// The outcome of the observation the first verification record was minted
/// from: the check's own, not the turn's change observation beside it.
fn producer_outcome(checkpoint: &Value) -> Value {
    let id = &checkpoint["verification_evidence"][0]["producer_observation"]["observation_id"];
    observations(checkpoint)
        .into_iter()
        .find(|observation| observation["observation_id"] == *id)
        .map(|observation| observation["outcome"].clone())
        .unwrap_or_else(|| panic!("no producer observation: {checkpoint:#}"))
}

fn refs(evidence: &Value) -> Vec<String> {
    evidence["refs"]
        .as_array()
        .expect("refs")
        .iter()
        .map(|reference| reference.as_str().expect("a ref").to_owned())
        .collect()
}

#[test]
fn a_declared_command_with_a_passing_trx_is_reported_as_test_evidence() {
    let turn = CheckedTurn::start("declared-pass", true);
    declare(&turn, DECLARED_TEST, DECLARED_ARTIFACT);
    turn.run("check-1", DECLARED_TEST, EngramCommandExit::Code(0), || {
        write_artifact(&turn, PASSING_TRX);
    });
    let checkpoint = turn.finish();

    assert_eq!(producer_outcome(&checkpoint), "succeeded", "{checkpoint:#}");
    let evidence = &checkpoint["verification_evidence"][0];
    assert_eq!(evidence["check_kind"], "test", "{checkpoint:#}");
    assert!(
        refs(evidence)
            .iter()
            .any(|reference| reference == "kind:declared"),
        "{evidence:#}"
    );
}

#[test]
fn a_declared_run_whose_trx_records_a_failure_is_reported_failed_even_at_exit_0() {
    let turn = CheckedTurn::start("declared-fail", true);
    declare(&turn, DECLARED_TEST, DECLARED_ARTIFACT);
    turn.run("check-1", DECLARED_TEST, EngramCommandExit::Code(0), || {
        write_artifact(&turn, FAILING_TRX);
    });
    let checkpoint = turn.finish();

    assert_eq!(producer_outcome(&checkpoint), "failed", "{checkpoint:#}");
    assert_eq!(checkpoint["verification_evidence"][0]["check_kind"], "test");
}

#[test]
fn a_declared_run_that_writes_no_artifact_is_withheld_as_unknown_with_its_reason() {
    let turn = CheckedTurn::start("declared-missing", true);
    declare(&turn, DECLARED_TEST, DECLARED_ARTIFACT);
    turn.run("check-1", DECLARED_TEST, EngramCommandExit::Code(0), || {});
    let checkpoint = turn.finish();

    assert!(
        checkpoint.get("verification_evidence").is_none(),
        "{checkpoint:#}"
    );
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the holder should be told");
    assert!(
        line.contains("Declared test command earned no credit and was not recorded: UNKNOWN")
            && line.contains("missing"),
        "{line}"
    );
}

#[test]
fn a_built_in_test_command_keeps_its_own_recognition_when_it_is_also_declared() {
    let turn = CheckedTurn::start("declared-built-in", true);
    declare(&turn, SIZE_TEST, DECLARED_ARTIFACT);
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
    let checkpoint = turn.finish();

    let evidence = &checkpoint["verification_evidence"][0];
    assert_eq!(
        refs(evidence),
        vec![format!("command:{SIZE_TEST}"), "exit:0".to_owned()],
        "built-in recognition wins and is unchanged: {checkpoint:#}"
    );
}

#[test]
fn a_command_that_differs_from_its_declaration_is_not_a_check() {
    let turn = CheckedTurn::start("declared-differs", true);
    declare(&turn, DECLARED_TEST, DECLARED_ARTIFACT);
    turn.run("check-1", "dotnet test", EngramCommandExit::Code(0), || {
        write_artifact(&turn, PASSING_TRX);
    });
    let checkpoint = turn.finish();

    assert!(
        checkpoint.get("verification_evidence").is_none(),
        "{checkpoint:#}"
    );
}

#[test]
fn a_disabled_declaration_is_told_in_a_host_line_and_recognises_nothing() {
    let turn = CheckedTurn::start("declared-disabled", true);
    declare(&turn, "dotnet test --no-build", DECLARED_ARTIFACT);
    turn.run(
        "check-1",
        "dotnet test --no-build",
        EngramCommandExit::Code(0),
        || {
            write_artifact(&turn, PASSING_TRX);
        },
    );
    let checkpoint = turn.finish();

    assert!(
        checkpoint.get("verification_evidence").is_none(),
        "{checkpoint:#}"
    );
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the holder should be told");
    assert!(
        line.contains("termal-tests.toml in")
            && line.contains("is disabled")
            && line.contains("--no-build"),
        "{line}"
    );
}

#[test]
fn a_declared_run_that_exits_non_zero_with_no_artifact_is_reported_failed() {
    let turn = CheckedTurn::start("declared-exit-1", true);
    declare(&turn, DECLARED_TEST, DECLARED_ARTIFACT);
    turn.run("check-1", DECLARED_TEST, EngramCommandExit::Code(1), || {});
    let checkpoint = turn.finish();

    assert_eq!(producer_outcome(&checkpoint), "failed", "{checkpoint:#}");
    let refs = refs(&checkpoint["verification_evidence"][0]);
    assert!(refs.contains(&"exit:1".to_owned()) && refs.contains(&"kind:declared".to_owned()));
}

#[test]
fn a_passing_declared_record_carries_the_declaration_artifact_and_counters() {
    let turn = CheckedTurn::start("declared-record", true);
    declare(&turn, DECLARED_TEST, DECLARED_ARTIFACT);
    turn.run("check-1", DECLARED_TEST, EngramCommandExit::Code(0), || {
        write_artifact(&turn, PASSING_TRX);
    });
    let checkpoint = turn.finish();

    let evidence = &checkpoint["verification_evidence"][0];
    let refs = refs(evidence);
    let has = |prefix: &str| refs.iter().any(|reference| reference.starts_with(prefix));
    for prefix in [
        "declared-cwd:.",
        "declaration-sha256:",
        "declaration-bytes:",
        "artifact:.termal-results/results.trx",
        "artifact-sha256:",
        "artifact-bytes:",
        "artifact-modified:",
        "trx-outcome:Completed",
        "trx-counters:total=3,executed=3,passed=3,failed=0",
    ] {
        assert!(has(prefix), "{prefix} missing: {evidence:#}");
    }
    assert!(
        evidence["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("PASS") && summary.contains("passed=3")),
        "{evidence:#}"
    );
}

// Unit tests of the declaration, the artifact rules and the TRX reading.

/// A Git repository whose `.termal-results/` is ignored, as the docs'
/// example sets it up.
fn declared_root(prefix: &str) -> TestTempRoot {
    let root = TestTempRoot::create(prefix);
    run_git_test_command(&root, &["init", "--quiet"]);
    run_git_test_command(&root, &["config", "user.email", "termal-tests@example.com"]);
    run_git_test_command(&root, &["config", "user.name", "TermAl tests"]);
    fs::write(root.join(".gitignore"), ".termal-results/\n").expect("ignore file writes");
    run_git_test_command(&root, &["add", ".gitignore"]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "ignore results"]);
    root
}

fn load(root: &FsPath, declaration: &str) -> EngramDeclaration {
    fs::write(root.join(ENGRAM_DECLARATION_FILE), declaration).expect("declaration writes");
    engram_load_declaration(&engram_worktree_root_path(root))
}

fn disabled_reason(declaration: &str) -> String {
    let root = declared_root("declared-load");
    match load(&root, declaration) {
        EngramDeclaration::Disabled { reason, .. } => reason,
        other => panic!("expected a disabled declaration, got {other:?}"),
    }
}

fn entry(command: &str, cwd: Option<&str>, artifact: &str) -> String {
    let mut entry = format!("[[test]]\ncommand = {}\n", toml_string(command));
    if let Some(cwd) = cwd {
        entry.push_str(&format!("cwd = {}\n", toml_string(cwd)));
    }
    entry.push_str(&format!("artifact = {}\n", toml_string(artifact)));
    entry
}

fn target_at(root: &FsPath, directory: &FsPath) -> EngramCheckTarget {
    EngramCheckTarget {
        root: engram_worktree_root_path(root),
        directory: directory.to_path_buf(),
        common_dir_key: None,
    }
}

fn bind(root: &FsPath, line: &str) -> Option<EngramDeclaredBinding> {
    let words = engram_declared_words(line).expect("a readable line");
    engram_declared_bind(&words, &target_at(root, root)).0
}

#[test]
fn a_quoted_separator_is_argument_text_on_the_declared_route() {
    assert_eq!(
        engram_declared_words(DECLARED_TEST).expect("readable"),
        vec![
            "dotnet",
            "test",
            "--logger",
            "trx;LogFileName=results.trx",
            "--results-directory",
            ".termal-results",
        ]
    );
    assert_eq!(
        engram_declared_words("run 'a;b' \"x y\"").expect("readable"),
        vec!["run", "a;b", "x y"]
    );
}

#[test]
fn shell_syntax_is_refused_on_the_declared_route() {
    for line in [
        "dotnet test; rm x",
        "dotnet test | tee log",
        "dotnet test && echo",
        "dotnet test > log",
        "dotnet test < input",
        "dotnet test $(pwd)",
        "dotnet test `pwd`",
        "dotnet test $FILTER",
        "dotnet test \"$FILTER\"",
        "dotnet test \"a\\\"b\"",
        "dotnet test tests\\Orders",
        "dotnet test 'it''s'",
        "dotnet test \"unbalanced",
        "dotnet test *.csproj",
        "dotnet test # comment",
        "dotnet test ~/x",
        "dotnet test @args",
        "dotnet test (x)",
        "dotnet test %SUITE%",
        "dotnet test\nrm x",
    ] {
        assert!(
            engram_declared_words(line).is_err(),
            "{line:?} should be refused"
        );
    }
}

#[test]
fn a_declaration_entry_missing_command_or_artifact_disables_the_whole_file() {
    let good = entry(DECLARED_TEST, None, DECLARED_ARTIFACT);
    let reason = disabled_reason(&format!(
        "{good}[[test]]\nartifact = \"{DECLARED_ARTIFACT}\"\n"
    ));
    assert!(reason.contains("missing `command`"), "{reason}");
    let reason = disabled_reason(&format!("{good}[[test]]\ncommand = \"dotnet test\"\n"));
    assert!(reason.contains("missing `artifact`"), "{reason}");
}

#[test]
fn a_declaration_entry_without_cwd_runs_at_the_root() {
    let root = declared_root("declared-cwd-default");
    match load(&root, &entry(DECLARED_TEST, None, DECLARED_ARTIFACT)) {
        EngramDeclaration::Loaded { entries, .. } => assert_eq!(entries[0].cwd, "."),
        other => panic!("expected a loaded declaration, got {other:?}"),
    }
    assert!(bind(&root, DECLARED_TEST).is_some());
}

#[test]
fn a_declaration_that_breaks_a_load_rule_is_disabled_with_its_reason() {
    let artifact = DECLARED_ARTIFACT;
    for (declaration, expected) in [
        ("[[test]\ncommand = ".to_owned(), "does not parse"),
        (
            format!("{}color = \"red\"\n", entry(DECLARED_TEST, None, artifact)),
            "does not parse",
        ),
        (
            entry("dotnet test --no-build", None, artifact),
            "--no-build",
        ),
        (
            entry("dotnet test --list-tests", None, artifact),
            "--no-build",
        ),
        (
            entry("dotnet test bin/Orders.Tests.dll", None, artifact),
            "--no-build",
        ),
        (entry("dotnet test x | tee", None, artifact), "shell syntax"),
        (
            entry(DECLARED_TEST, None, "../outside.trx"),
            "`artifact` does not resolve inside",
        ),
        (
            entry(DECLARED_TEST, Some("missing-directory"), artifact),
            "`cwd` does not resolve inside",
        ),
        (
            entry(DECLARED_TEST, Some(".."), artifact),
            "`cwd` does not resolve inside",
        ),
        (
            entry(DECLARED_TEST, None, "C:/outside.trx"),
            "`artifact` does not resolve inside",
        ),
        (
            entry(DECLARED_TEST, None, "/outside.trx"),
            "`artifact` does not resolve inside",
        ),
        (
            entry(DECLARED_TEST, None, artifact).repeat(ENGRAM_DECLARATION_MAX_ENTRIES + 1),
            "entry cap",
        ),
        (
            format!(
                "# {}\n{}",
                "x".repeat(ENGRAM_DECLARATION_MAX_BYTES),
                entry(DECLARED_TEST, None, artifact)
            ),
            "size cap",
        ),
    ] {
        let reason = disabled_reason(&declaration);
        assert!(reason.contains(expected), "{declaration:.120}: {reason}");
    }
}

#[test]
fn a_command_matches_exactly_one_entry_in_its_cwd_or_is_no_check() {
    let root = declared_root("declared-match");
    fs::create_dir_all(root.join("tests")).expect("subdirectory");
    load(&root, &entry(DECLARED_TEST, None, DECLARED_ARTIFACT));
    assert!(
        bind(&root, DECLARED_TEST).is_some(),
        "the declared line matches"
    );
    let requoted =
        "dotnet test --logger 'trx;LogFileName=results.trx' --results-directory .termal-results";
    assert!(
        bind(&root, requoted).is_some(),
        "quoting differs, words are equal"
    );
    assert!(
        bind(&root, "dotnet test").is_none(),
        "another line is no check"
    );
    let words = engram_declared_words(DECLARED_TEST).expect("readable");
    assert!(
        engram_declared_bind(&words, &target_at(&root, &root.join("tests")))
            .0
            .is_none(),
        "another directory is no check"
    );
    // Two entries matching the same run is no check.
    load(
        &root,
        &entry(DECLARED_TEST, None, DECLARED_ARTIFACT).repeat(2),
    );
    assert!(bind(&root, DECLARED_TEST).is_none());
    // An entry declaring a built-in test never applies.
    load(&root, &entry(SIZE_TEST, None, DECLARED_ARTIFACT));
    assert!(bind(&root, SIZE_TEST).is_none());
}

/// Binds `DECLARED_TEST` at a fresh declared root, with the artifact as
/// `before` left it, and judges the end after `during` ran, at exit `exit`.
fn judge(
    prefix: &str,
    before: impl FnOnce(&FsPath),
    during: impl FnOnce(&FsPath),
    exit: EngramCommandExit,
) -> EngramDeclaredResult {
    let root = declared_root(prefix);
    load(&root, &entry(DECLARED_TEST, None, DECLARED_ARTIFACT));
    run_git_test_command(&root, &["add", ENGRAM_DECLARATION_FILE]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "declare"]);
    before(&root);
    let binding = bind(&root, DECLARED_TEST).expect("the declared line binds");
    during(&root);
    engram_declared_result(&binding, exit)
}

fn put(root: &FsPath, content: &[u8]) {
    let path = root.join(DECLARED_ARTIFACT);
    fs::create_dir_all(path.parent().expect("a directory")).expect("result directory");
    fs::write(path, content).expect("artifact writes");
}

fn unknown_reason(result: &EngramDeclaredResult) -> String {
    match &result.verdict {
        EngramTrxVerdict::Unknown(reason) => reason.clone(),
        other => panic!("expected UNKNOWN, got {other:?}"),
    }
}

#[test]
fn an_artifact_written_during_the_run_passes_with_its_facts() {
    let result = judge(
        "declared-fresh",
        |_| {},
        |root| put(root, PASSING_TRX),
        EngramCommandExit::Code(0),
    );
    assert_eq!(result.verdict, EngramTrxVerdict::Pass);
    let facts = result.artifact.expect("facts");
    assert_eq!(facts.path, DECLARED_ARTIFACT);
    assert_eq!(facts.size, PASSING_TRX.len() as u64);
    assert_eq!(facts.sha256, sha256_hex(PASSING_TRX));
}

#[test]
fn an_artifact_replaced_with_new_bytes_during_the_run_passes() {
    let result = judge(
        "declared-replaced",
        |root| put(root, MIXED_TRX),
        |root| put(root, PASSING_TRX),
        EngramCommandExit::Code(0),
    );
    assert_eq!(result.verdict, EngramTrxVerdict::Pass);
}

#[test]
fn each_artifact_rule_failure_is_unknown_with_its_observed_reason() {
    let exit = EngramCommandExit::Code(0);
    let missing = judge("declared-missing-unit", |_| {}, |_| {}, exit);
    assert!(unknown_reason(&missing).contains("missing"));

    let unchanged = judge(
        "declared-unchanged",
        |root| put(root, PASSING_TRX),
        |_| {},
        exit,
    );
    assert!(unknown_reason(&unchanged).contains("unchanged"));

    let stale = judge(
        "declared-stale",
        |_| {},
        |root| {
            put(root, PASSING_TRX);
            fs::File::options()
                .write(true)
                .open(root.join(DECLARED_ARTIFACT))
                .expect("artifact opens")
                .set_modified(std::time::SystemTime::now() - Duration::from_secs(3600))
                .expect("modification time sets");
        },
        exit,
    );
    assert!(unknown_reason(&stale).contains("before the command started"));

    let large = judge(
        "declared-large",
        |_| {},
        |root| put(root, &vec![b' '; ENGRAM_ARTIFACT_MAX_BYTES + 1]),
        exit,
    );
    assert!(unknown_reason(&large).contains("too large"));

    let directory = judge(
        "declared-directory",
        |_| {},
        |root| fs::create_dir_all(root.join(DECLARED_ARTIFACT)).expect("directory"),
        exit,
    );
    assert!(unknown_reason(&directory).contains("not a regular file"));

    let start_failed = judge(
        "declared-start-failed",
        |root| fs::create_dir_all(root.join(DECLARED_ARTIFACT)).expect("directory"),
        |root| {
            fs::remove_dir(root.join(DECLARED_ARTIFACT)).expect("directory removes");
            put(root, PASSING_TRX);
        },
        exit,
    );
    assert!(unknown_reason(&start_failed).contains("start inspection failed"));
}

#[test]
fn an_artifact_that_is_not_ignored_or_is_tracked_is_unknown() {
    let exit = EngramCommandExit::Code(0);
    let not_ignored = judge(
        "declared-not-ignored",
        |root| {
            fs::write(root.join(".gitignore"), "").expect("ignore file clears");
            run_git_test_command(root, &["commit", "--quiet", "-am", "unignore"]);
        },
        |root| put(root, PASSING_TRX),
        exit,
    );
    assert!(unknown_reason(&not_ignored).contains("not ignored"));

    let tracked = judge(
        "declared-tracked",
        |root| {
            put(root, MIXED_TRX);
            run_git_test_command(root, &["add", "--force", DECLARED_ARTIFACT]);
            run_git_test_command(root, &["commit", "--quiet", "-m", "track the result"]);
        },
        |root| put(root, PASSING_TRX),
        exit,
    );
    assert!(unknown_reason(&tracked).contains("tracked"));
}

#[test]
fn a_changed_declaration_is_unknown_before_the_exit_status_decides() {
    let changed = |exit| {
        judge(
            "declared-changed",
            |_| {},
            |root| {
                put(root, PASSING_TRX);
                let path = root.join(ENGRAM_DECLARATION_FILE);
                let mut text = fs::read_to_string(&path).expect("declaration reads");
                text.push_str("# edited\n");
                fs::write(path, text).expect("declaration writes");
            },
            exit,
        )
    };
    for exit in [EngramCommandExit::Code(0), EngramCommandExit::Code(1)] {
        assert!(unknown_reason(&changed(exit)).contains("declaration changed"));
    }
}

#[test]
fn a_non_zero_exit_is_fail_whatever_the_artifact_says() {
    let missing = judge(
        "declared-fail-missing",
        |_| {},
        |_| {},
        EngramCommandExit::Code(1),
    );
    assert!(matches!(missing.verdict, EngramTrxVerdict::Fail(_)));
    let passing = judge(
        "declared-fail-passing",
        |_| {},
        |root| put(root, PASSING_TRX),
        EngramCommandExit::Code(2),
    );
    assert!(matches!(passing.verdict, EngramTrxVerdict::Fail(_)));
    assert!(passing.counters.is_some(), "the facts are still recorded");
    let unknown_exit = judge(
        "declared-no-exit",
        |_| {},
        |root| put(root, PASSING_TRX),
        EngramCommandExit::Unknown,
    );
    assert!(unknown_reason(&unknown_exit).contains("without an exit status"));
}

// The TRX reading, on real captured files and edits of them.

fn verdict(bytes: &[u8]) -> EngramTrxVerdict {
    engram_read_trx(bytes).verdict
}

fn edited(bytes: &[u8], from: &str, to: &str) -> Vec<u8> {
    let text = String::from_utf8(bytes.to_vec()).expect("fixture is UTF-8");
    assert!(text.contains(from), "{from} not in the fixture");
    text.replacen(from, to, 1).into_bytes()
}

const ALL_PASSED: &str = "total=\"3\" executed=\"3\" passed=\"3\" failed=\"0\" error=\"0\" \
     timeout=\"0\" aborted=\"0\" inconclusive=\"0\" passedButRunAborted=\"0\" notRunnable=\"0\" \
     notExecuted=\"0\" disconnected=\"0\" warning=\"0\" completed=\"0\" inProgress=\"0\" \
     pending=\"0\"";

fn summary_document(inside_root: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<TestRun id=\"x\">{inside_root}</TestRun>\n"
    )
}

#[test]
fn real_captured_trx_files_read_as_their_runs_ended() {
    assert_eq!(verdict(PASSING_TRX), EngramTrxVerdict::Pass);
    assert!(matches!(verdict(FAILING_TRX), EngramTrxVerdict::Fail(_)));
    assert!(
        matches!(verdict(ZERO_TRX), EngramTrxVerdict::Unknown(reason) if reason == "no tests executed")
    );
    // A skipped test is counted in total only: the counters are consistent.
    assert_eq!(verdict(MIXED_TRX), EngramTrxVerdict::Pass);
    let counters = engram_read_trx(MIXED_TRX).counters.expect("counters");
    assert_eq!(
        (
            counters.get("total"),
            counters.get("executed"),
            counters.get("notExecuted")
        ),
        (3, 2, 0)
    );
}

#[test]
fn a_run_outcome_of_failure_is_fail_even_with_passing_counters() {
    for outcome in [
        "Failed",
        "Error",
        "Aborted",
        "Timeout",
        "PassedButRunAborted",
    ] {
        let bytes = edited(
            PASSING_TRX,
            "<ResultSummary outcome=\"Completed\">",
            &format!("<ResultSummary outcome=\"{outcome}\">"),
        );
        assert!(
            matches!(verdict(&bytes), EngramTrxVerdict::Fail(_)),
            "{outcome}"
        );
    }
    for counter in ["error", "timeout", "aborted", "passedButRunAborted"] {
        let bytes = edited(
            PASSING_TRX,
            &format!(" {counter}=\"0\""),
            &format!(" {counter}=\"1\""),
        );
        assert!(
            matches!(verdict(&bytes), EngramTrxVerdict::Fail(_)),
            "{counter}"
        );
    }
}

#[test]
fn an_incomplete_or_ambiguous_trx_is_unknown_never_pass() {
    let text = String::from_utf8(PASSING_TRX.to_vec()).expect("UTF-8");
    let counters_end = text.find("<Counters").expect("counters");
    let counters_end = counters_end + text[counters_end..].find("/>").expect("closes") + 2;
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (
            PASSING_TRX[..counters_end].to_vec(),
            "truncated after Counters",
        ),
        (
            PASSING_TRX[..PASSING_TRX.len() - 20].to_vec(),
            "truncated before the root closes",
        ),
        (
            edited(
                PASSING_TRX,
                "<TestRun ",
                "<!DOCTYPE TestRun [<!ENTITY x \"y\">]>\n<TestRun ",
            ),
            "a DTD",
        ),
        (format!("{text}<TestRun />").into_bytes(), "a second root"),
        (
            edited(
                PASSING_TRX,
                "<ResultSummary outcome=\"Completed\">",
                "<ResultSummary outcome=\"Completed\"><Counters total=\"1\" />",
            ),
            "two Counters",
        ),
        (
            edited(PASSING_TRX, " pending=\"0\"", ""),
            "a missing counter",
        ),
        (
            edited(PASSING_TRX, " passed=\"3\"", " passed=\"three\""),
            "a non-numeric counter",
        ),
        (
            edited(PASSING_TRX, " passed=\"3\"", " passed=\"2\""),
            "inconsistent counters",
        ),
        (
            edited(PASSING_TRX, " total=\"3\"", " total=\"2\""),
            "executed beyond total",
        ),
        (
            edited(PASSING_TRX, " warning=\"0\"", " warning=\"1\""),
            "an uninterpreted counter",
        ),
        (
            edited(PASSING_TRX, " pending=\"0\"", " pending=\"0\" novel=\"1\""),
            "an unknown counter",
        ),
        (
            edited(
                PASSING_TRX,
                "<ResultSummary outcome=\"Completed\">",
                "<ResultSummary outcome=\"Warning\">",
            ),
            "an unknown run outcome",
        ),
        (
            edited(PASSING_TRX, "encoding=\"utf-8\"", "encoding=\"utf-16\""),
            "another encoding",
        ),
        (
            b"<TestRun><Results /></TestRun>".to_vec(),
            "no ResultSummary",
        ),
        (
            b"<?xml version=\"1.0\"?><testsuites tests=\"1\" />".to_vec(),
            "not a TRX document",
        ),
    ];
    for (bytes, why) in cases {
        assert!(
            matches!(verdict(&bytes), EngramTrxVerdict::Unknown(_)),
            "{why}: {:?}",
            verdict(&bytes)
        );
    }
}

#[test]
fn only_the_direct_result_summary_counts_not_text_that_looks_like_one() {
    let summary =
        format!("<ResultSummary outcome=\"Completed\"><Counters {ALL_PASSED} /></ResultSummary>");
    assert_eq!(
        verdict(summary_document(&summary).as_bytes()),
        EngramTrxVerdict::Pass
    );
    for inside_root in [
        format!("<!-- {summary} -->"),
        format!("<Results>{summary}</Results>"),
        format!("<Output><![CDATA[{summary}]]></Output>"),
    ] {
        assert!(
            matches!(
                verdict(summary_document(&inside_root).as_bytes()),
                EngramTrxVerdict::Unknown(_)
            ),
            "{inside_root}"
        );
    }
}

#[test]
fn a_run_level_error_keeps_a_completed_run_from_pass() {
    let bytes = edited(
        PASSING_TRX,
        "<Counters ",
        "<RunInfos><RunInfo computerName=\"MACHINE\" outcome=\"Error\" timestamp=\"t\">\
         <Text>attachment failed</Text></RunInfo></RunInfos>\n    <Counters ",
    );
    assert!(
        matches!(verdict(&bytes), EngramTrxVerdict::Unknown(reason) if reason.contains("run-level error"))
    );
}

/// Judges a passing artifact as if its command started `past_slack` later
/// than the artifact's own modification time plus the clock slack: the start
/// is set from the time the file system recorded, so the edge is exact.
fn judge_against_start(prefix: &str, past_slack: Duration) -> EngramDeclaredResult {
    let root = declared_root(prefix);
    load(&root, &entry(DECLARED_TEST, None, DECLARED_ARTIFACT));
    run_git_test_command(&root, &["add", ENGRAM_DECLARATION_FILE]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "declare"]);
    let mut binding = bind(&root, DECLARED_TEST).expect("the declared line binds");
    put(&root, PASSING_TRX);
    let modified = fs::metadata(root.join(DECLARED_ARTIFACT))
        .expect("artifact metadata")
        .modified()
        .expect("modification time");
    binding.started = modified + ENGRAM_ARTIFACT_CLOCK_SLACK + past_slack;
    engram_declared_result(&binding, EngramCommandExit::Code(0))
}

#[test]
fn an_artifact_the_clock_slack_older_than_the_start_counts_and_one_tick_more_is_stale() {
    assert_eq!(
        judge_against_start("declared-slack-edge", Duration::ZERO).verdict,
        EngramTrxVerdict::Pass
    );
    // The smallest step the platform's clock keeps: 100 ns on Windows.
    let tick = Duration::from_nanos(if cfg!(windows) { 100 } else { 1 });
    let stale = judge_against_start("declared-slack-past", tick);
    assert!(unknown_reason(&stale).contains("before the command started"));
}

#[test]
fn a_completed_run_that_executed_nothing_is_unknown_never_pass() {
    let nothing = ALL_PASSED.replace("\"3\"", "\"0\"");
    let document = summary_document(&format!(
        "<ResultSummary outcome=\"Completed\"><Counters {nothing} /></ResultSummary>"
    ));
    assert!(
        matches!(verdict(document.as_bytes()), EngramTrxVerdict::Unknown(reason) if reason == "no tests executed")
    );
}

// Freeze-2 review findings: each control below failed on freeze 2.

#[test]
fn malformed_xml_anywhere_in_the_trx_is_unknown_never_pass() {
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (
            edited(
                PASSING_TRX,
                "<ResultSummary outcome=\"Completed\">",
                "<ResultSummary outcome=\"Completed\" outcome=\"Failed\">",
            ),
            "a duplicate attribute on the summary",
        ),
        (
            edited(PASSING_TRX, "<TestRun ", "<TestRun novalue "),
            "an attribute without a value on the root",
        ),
        (
            edited(
                PASSING_TRX,
                "<UnitTestResult ",
                "<UnitTestResult outcome=\"Passed\" ",
            ),
            "a duplicate attribute on a result",
        ),
        (
            edited(PASSING_TRX, "<Results>", "<Results><!-- a -- b -->"),
            "a comment holding --",
        ),
        (
            edited(
                PASSING_TRX,
                "<Results>",
                "<Results><Note>&undeclared;</Note>",
            ),
            "an undeclared entity",
        ),
    ];
    for (bytes, why) in cases {
        assert!(
            matches!(verdict(&bytes), EngramTrxVerdict::Unknown(_)),
            "{why}: {:?}",
            verdict(&bytes)
        );
    }
}

#[test]
fn a_declared_check_survives_a_description_that_repeats_its_command() {
    // ACP runtimes describe a running command again (its raw input) before
    // they report its end.
    let turn = CheckedTurn::start("declared-described", true);
    declare(&turn, DECLARED_TEST, DECLARED_ARTIFACT);
    let mut recorder = turn.recorder();
    recorder
        .command_started("check-1", DECLARED_TEST)
        .expect("the start should record");
    turn.wait_for_snapshots();
    write_artifact(&turn, PASSING_TRX);
    recorder
        .command_described("check-1", Some(DECLARED_TEST), None)
        .expect("the description should record");
    recorder
        .command_completed_with_exit(
            "check-1",
            DECLARED_TEST,
            "",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the end should record");
    turn.wait_for_snapshots();
    let checkpoint = turn.finish();

    assert_eq!(producer_outcome(&checkpoint), "succeeded", "{checkpoint:#}");
}

/// Removes the directory link `link` (`link_directory`), not its target.
fn unlink_directory(link: &FsPath) {
    #[cfg(windows)]
    fs::remove_dir(link).expect("the junction should be removed");
    #[cfg(unix)]
    fs::remove_file(link).expect("the symbolic link should be removed");
}

#[test]
fn an_artifact_reached_through_a_retargeted_link_is_unknown() {
    let result = judge(
        "declared-retarget",
        |root| {
            fs::create_dir_all(root.join("out-a")).expect("first target");
            fs::create_dir_all(root.join("out-b")).expect("second target");
            fs::write(
                root.join(".gitignore"),
                ".termal-results/\n.termal-results\nout-a/\nout-b/\n",
            )
            .expect("ignore file writes");
            run_git_test_command(root, &["commit", "--quiet", "-am", "ignore outputs"]);
            link_directory(&root.join(".termal-results"), &root.join("out-a"));
        },
        |root| {
            unlink_directory(&root.join(".termal-results"));
            link_directory(&root.join(".termal-results"), &root.join("out-b"));
            put(root, PASSING_TRX);
        },
        EngramCommandExit::Code(0),
    );
    assert!(unknown_reason(&result).contains("canonical path"));
}

#[test]
fn a_check_stays_open_to_writes_until_its_artifact_is_read() {
    let turn = CheckedTurn::start("declared-read-pending", true);
    let settled = Arc::new(EngramBasisCapture::default());
    settled.finish(None);
    let reading: Arc<EngramCapture<EngramDeclaredResult>> = Arc::new(EngramCapture {
        result: Mutex::new(None),
        ready: std::sync::Condvar::new(),
        ready_tick: std::sync::atomic::AtomicU64::new(0),
    });
    let mut check = turn.finished_check(0, settled);
    check.ended_at = Some(EngramHost::interference_tick());
    check.end.as_mut().expect("an end").declared = Some(reading.clone());
    assert!(check.open_to_writes(), "the artifact is not read yet");
    assert_eq!(check.interference_end(), None);
    turn.record_mut(|record| record.engram.active_turn_checks = vec![check]);

    // A command started while the artifact is still unread may rewrite it.
    turn.recorder()
        .command_started("later", "git checkout -- README.md")
        .expect("the start should record");
    assert!(
        turn.record(|record| record.engram.active_turn_checks[0].overlapped),
        "a write before the artifact read overlaps the check"
    );

    reading.finish(EngramDeclaredResult {
        verdict: EngramTrxVerdict::Pass,
        artifact: None,
        outcome: None,
        counters: None,
    });
    let check = turn.record(|record| record.engram.active_turn_checks[0].clone());
    assert!(!check.open_to_writes());
    assert!(
        check.interference_end() >= reading.ready_tick(),
        "the read closes the interval"
    );
}

#[test]
fn the_cmd_dialect_and_ambiguous_characters_are_refused_on_the_declared_route() {
    assert!(engram_declared_candidate(&format!("cmd /c {DECLARED_TEST}")).is_none());
    assert!(
        engram_declared_candidate("cmd /c \"node scripts/project-tests.mjs 'x& echo ok'\"")
            .is_none()
    );
    for line in [
        "dotnet test \"a\u{201C}; rm x; \u{201D}b\"",
        "dotnet test \u{2018}x\u{2019}",
        "dotnet\u{00A0}test",
        "dotnet\u{2003}test",
    ] {
        assert!(
            engram_declared_words(line).is_err(),
            "{line:?} should be refused"
        );
    }
    // Ordinary separators still split words.
    assert_eq!(
        engram_declared_words("dotnet \ttest").expect("readable"),
        vec!["dotnet", "test"]
    );
}

#[test]
fn a_declared_command_wrapped_in_cmd_is_no_check_end_to_end() {
    let turn = CheckedTurn::start("declared-cmd", true);
    declare(&turn, DECLARED_TEST, DECLARED_ARTIFACT);
    let wrapped = format!("cmd /c {DECLARED_TEST}");
    turn.run("check-1", &wrapped, EngramCommandExit::Code(0), || {
        write_artifact(&turn, PASSING_TRX);
    });
    let checkpoint = turn.finish();

    assert!(
        checkpoint.get("verification_evidence").is_none(),
        "{checkpoint:#}"
    );
}

#[test]
fn a_passing_summary_names_inconclusive_tests() {
    let counters = "total=\"14\" executed=\"14\" passed=\"12\" failed=\"0\" error=\"0\" \
                    timeout=\"0\" aborted=\"0\" inconclusive=\"2\" passedButRunAborted=\"0\" \
                    notRunnable=\"0\" notExecuted=\"0\" disconnected=\"0\" warning=\"0\" \
                    completed=\"0\" inProgress=\"0\" pending=\"0\"";
    let document = summary_document(&format!(
        "<ResultSummary outcome=\"Completed\"><Counters {counters} /></ResultSummary>"
    ));
    let reading = engram_read_trx(document.as_bytes());
    assert_eq!(reading.verdict, EngramTrxVerdict::Pass);
    let result = EngramDeclaredResult {
        verdict: reading.verdict,
        artifact: None,
        outcome: reading.outcome,
        counters: reading.counters,
    };
    let (command, _) = engram_declared_candidate(DECLARED_TEST).expect("a candidate");
    let summary = engram_declared_summary(&command, None, EngramCommandExit::Code(0), &result);
    assert!(
        summary.contains("PASS: 12 passed, 2 inconclusive"),
        "{summary}"
    );
}

#[test]
fn a_disabled_declaration_is_told_once_per_content() {
    let turn = CheckedTurn::start("declared-disabled-once", true);
    declare(&turn, "dotnet test --no-build", DECLARED_ARTIFACT);
    let pending = || turn.record(|record| record.engram.pending_source_root_line.clone());
    let clear = || turn.record_mut(|record| record.engram.pending_source_root_line = None);

    turn.run(
        "first",
        "dotnet test --no-build",
        EngramCommandExit::Code(0),
        || {},
    );
    assert!(pending().is_some_and(|line| line.contains("is disabled")));
    clear();
    turn.run(
        "second",
        "dotnet test --no-build",
        EngramCommandExit::Code(0),
        || {},
    );
    assert!(
        !pending().is_some_and(|line| line.contains("is disabled")),
        "the same content is told once"
    );
    declare(&turn, "dotnet test --list-tests", DECLARED_ARTIFACT);
    turn.run(
        "third",
        "dotnet test --list-tests",
        EngramCommandExit::Code(0),
        || {},
    );
    assert!(
        pending().is_some_and(|line| line.contains("is disabled")),
        "new content is told again"
    );
}

#[test]
fn a_declared_run_whose_declaration_changed_is_withheld_even_at_a_non_zero_exit() {
    let turn = CheckedTurn::start("declared-changed-exit-1", true);
    // An ignored declaration, so editing it mid-run moves no source basis
    // and only the declared route can see the change.
    fs::write(
        turn.root.join(".gitignore"),
        ".termal-results/\ntermal-tests.toml\n",
    )
    .expect("ignore file writes");
    run_git_test_command(&turn.root, &["add", ".gitignore"]);
    run_git_test_command(&turn.root, &["commit", "--quiet", "-m", "ignore"]);
    let declaration = entry(DECLARED_TEST, None, DECLARED_ARTIFACT);
    fs::write(turn.root.join(ENGRAM_DECLARATION_FILE), &declaration).expect("declaration");
    turn.run("check-1", DECLARED_TEST, EngramCommandExit::Code(1), || {
        write_artifact(&turn, FAILING_TRX);
        fs::write(
            turn.root.join(ENGRAM_DECLARATION_FILE),
            format!("{declaration}# edited\n"),
        )
        .expect("declaration changes");
    });
    let checkpoint = turn.finish();

    assert!(
        checkpoint.get("verification_evidence").is_none(),
        "{checkpoint:#}"
    );
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the holder should be told");
    assert!(
        line.contains("earned no credit and was not recorded: UNKNOWN")
            && line.contains("declaration changed"),
        "{line}"
    );
}

// Freeze-3 review findings (pair 2): each control below failed on freeze 3.

#[test]
fn xml_characters_and_names_outside_the_xml_productions_are_unknown() {
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (
            edited(PASSING_TRX, "<Results>", "<Results><Note>&#1;</Note>"),
            "a reference to a control character",
        ),
        (
            edited(PASSING_TRX, "<Results>", "<Results><Note>&#xFFFF;</Note>"),
            "a reference to U+FFFF",
        ),
        (
            edited(PASSING_TRX, "<Results>", "<Results><Note>\u{1}</Note>"),
            "a raw control character in text",
        ),
        (
            edited(PASSING_TRX, "<Results>", "<Results><Note a=\"\u{1}\" />"),
            "a raw control character in an attribute value",
        ),
        (
            edited(PASSING_TRX, "<Results>", "<Results><1Note />"),
            "an element name starting with a digit",
        ),
        (
            edited(
                PASSING_TRX,
                "<UnitTestResult ",
                "<UnitTestResult 1bad=\"x\" ",
            ),
            "an attribute name starting with a digit",
        ),
    ];
    for (bytes, why) in cases {
        assert!(
            matches!(verdict(&bytes), EngramTrxVerdict::Unknown(_)),
            "{why}: {:?}",
            verdict(&bytes)
        );
    }
    // Characters and names the productions allow still read.
    let allowed = edited(
        PASSING_TRX,
        "<Results>",
        "<Results><t:Note_1 x.y-z=\"&#x9;&#xD;&#x10000;\u{e9}\">\u{e9}&#x10FFFF;</t:Note_1>",
    );
    assert_eq!(verdict(&allowed), EngramTrxVerdict::Pass);
}

#[test]
fn the_declared_reader_takes_only_allow_listed_whole_tokens() {
    for line in [
        // A quoted part joined to an unquoted one is two PowerShell arguments.
        "node scripts/project-tests.mjs \"--suite=\"all",
        "node scripts/project-tests.mjs --suite=\"all\"",
        "node scripts/project-tests.mjs \"a\"'b'",
        // An unquoted comma is a PowerShell array.
        "node scripts/project-tests.mjs a,b",
        // An assignment before the command is an environment variable in bash.
        "SUITE=all node scripts/project-tests.mjs",
        // PowerShell's stop-parsing token and splatting.
        "node scripts/project-tests.mjs --% x",
        "node scripts/project-tests.mjs @args",
        // An empty argument: PowerShell may drop it.
        "node scripts/project-tests.mjs \"\"",
        // Characters outside the allow-list, quoted or not.
        "node scripts/project-tests.mjs caf\u{e9}",
        "node scripts/project-tests.mjs \"caf\u{e9}\"",
        "node scripts/project-tests.mjs ~x",
        "node scripts/project-tests.mjs a~b",
    ] {
        assert!(
            engram_declared_words(line).is_err(),
            "{line:?} should be refused"
        );
    }
    // The docs' examples, and lines like them, still read.
    for (line, words) in [
        (
            "dotnet test tests/Orders.Tests --logger \"trx;LogFileName=results.trx\" \
             --results-directory tests/Orders.Tests/.termal-results",
            vec![
                "dotnet",
                "test",
                "tests/Orders.Tests",
                "--logger",
                "trx;LogFileName=results.trx",
                "--results-directory",
                "tests/Orders.Tests/.termal-results",
            ],
        ),
        (
            "node scripts/project-tests.mjs",
            vec!["node", "scripts/project-tests.mjs"],
        ),
        (
            "dotnet test --filter 'Category=Fast' --configuration:Release v1.2+build",
            vec![
                "dotnet",
                "test",
                "--filter",
                "Category=Fast",
                "--configuration:Release",
                "v1.2+build",
            ],
        ),
        (
            "dotnet test --filter \"FullyQualifiedName=Orders.Tests\" C:/src/a@b",
            vec![
                "dotnet",
                "test",
                "--filter",
                "FullyQualifiedName=Orders.Tests",
                "C:/src/a@b",
            ],
        ),
    ] {
        assert_eq!(engram_declared_words(line).expect(line), words, "{line}");
    }
}

#[test]
fn a_powershell_wrapper_counts_only_when_its_script_is_one_word() {
    let multi_word = format!("pwsh -Command {DECLARED_TEST}");
    assert!(
        engram_declared_candidate(&multi_word).is_none(),
        "the outer shell strips the inner quotes before pwsh reads the line again"
    );
    let declared = engram_declared_words(DECLARED_TEST).expect("readable");
    for wrapped in [
        format!("pwsh -Command '{DECLARED_TEST}'"),
        format!("bash -lc '{DECLARED_TEST}'"),
    ] {
        let (_, words) = engram_declared_candidate(&wrapped).expect("a one-word script");
        assert_eq!(words, declared, "{wrapped}");
    }
}

fn passing_summary(command_line: &str, trx: &[u8]) -> String {
    let reading = engram_read_trx(trx);
    assert_eq!(reading.verdict, EngramTrxVerdict::Pass);
    let result = EngramDeclaredResult {
        verdict: reading.verdict,
        artifact: None,
        outcome: reading.outcome,
        counters: reading.counters,
    };
    let (command, _) = engram_declared_candidate(command_line).expect("a candidate");
    engram_declared_summary(&command, None, EngramCommandExit::Code(0), &result)
}

#[test]
fn a_passing_summary_names_skipped_tests() {
    let summary = passing_summary(DECLARED_TEST, MIXED_TRX);
    assert!(summary.contains("PASS: 2 passed, 1 skipped"), "{summary}");
}

#[test]
fn a_long_command_never_cuts_the_verdict_from_the_summary() {
    let long = format!("{DECLARED_TEST} --filter {}", "a".repeat(6000));
    let summary = passing_summary(&long, MIXED_TRX);
    assert!(
        summary.len() <= ENGRAM_CHECK_SUMMARY_MAX_BYTES,
        "{}",
        summary.len()
    );
    assert!(summary.contains("PASS: 2 passed, 1 skipped"), "{summary}");
    assert!(
        summary.contains("counters total=3,executed=2,passed=2"),
        "{summary}"
    );
}

// Freeze-4 review findings (pair 3): each control below failed on freeze 4.

#[test]
fn a_wrapper_counts_only_with_one_single_quoted_script_under_bash_or_pwsh() {
    for wrapped in [
        // The outer double quotes expand $SUITE before bash runs.
        "bash -lc \"node scripts/project-tests.mjs 'suite=x'\"".to_owned(),
        // An unquoted one-word script is still read by the outer shell.
        "bash -lc node".to_owned(),
        // Windows PowerShell 5.1 passes native arguments in its legacy way.
        format!("powershell -Command '{DECLARED_TEST}'"),
    ] {
        assert!(
            engram_declared_candidate(&wrapped).is_none(),
            "{wrapped} should be refused"
        );
    }
    let declared = engram_declared_words(DECLARED_TEST).expect("readable");
    for wrapped in [
        format!("pwsh -Command '{DECLARED_TEST}'"),
        format!("bash -lc '{DECLARED_TEST}'"),
        format!("sh -c '{DECLARED_TEST}'"),
        format!("zsh -c '{DECLARED_TEST}'"),
    ] {
        let (_, words) = engram_declared_candidate(&wrapped).expect("a single-quoted script");
        assert_eq!(words, declared, "{wrapped}");
    }
}

#[test]
fn quoted_words_take_only_the_allow_list_plus_space_semicolon_and_equals() {
    for line in [
        "dotnet test 'x\"y'",
        "dotnet test 'a \" --no-build \" b'",
        "dotnet test 'a\\b'",
        "dotnet test 'a&b'",
        "dotnet test 'a|b'",
        "dotnet test 'a<b'",
        "dotnet test 'a>b'",
        "dotnet test 'a^b'",
        "dotnet test 'a~b'",
        "dotnet test 'a,b'",
        "dotnet test \"a%b\"",
        // `%` is no longer allowed unquoted either: a batch-file target expands it.
        "dotnet test 50%",
        // zsh expands an unquoted word starting with `=` to a command path.
        "dotnet test =dotnet",
    ] {
        assert!(
            engram_declared_words(line).is_err(),
            "{line:?} should be refused"
        );
    }
    assert_eq!(
        engram_declared_words("dotnet test --filter 'TestCategory=Fast' --logger \"trx;a=b c\"")
            .expect("readable"),
        vec![
            "dotnet",
            "test",
            "--filter",
            "TestCategory=Fast",
            "--logger",
            "trx;a=b c"
        ]
    );
}

#[test]
fn cmd_is_never_a_declared_program_at_any_wrapper_level() {
    for line in [
        format!("cmd /r {DECLARED_TEST}"),
        format!("cmd /d/c {DECLARED_TEST}"),
        format!("CMD.EXE /q /c {DECLARED_TEST}"),
        format!("bash -lc 'cmd /r {DECLARED_TEST}'"),
    ] {
        assert!(engram_declared_candidate(&line).is_none(), "{line}");
    }
}

#[test]
fn forbidden_words_are_caught_in_their_other_spellings() {
    let artifact = DECLARED_ARTIFACT;
    for command in [
        "dotnet test --no-build:true",
        "dotnet test --no-build=true",
        "dotnet test --list-tests:true",
        "dotnet test -t",
        "dotnet test -p:VSTestNoBuild=true",
        "dotnet test /p:VSTestListTests=true",
        "dotnet test bin/Orders.Tests.dll.",
    ] {
        let reason = disabled_reason(&entry(command, None, artifact));
        assert!(reason.contains("--no-build"), "{command}: {reason}");
    }
    // `-t` belongs to dotnet test: another program may use it freely.
    let root = declared_root("declared-other-t");
    assert!(matches!(
        load(
            &root,
            &entry("node scripts/project-tests.mjs -t", None, artifact)
        ),
        EngramDeclaration::Loaded { .. }
    ));
}

#[test]
fn the_one_call_form_runs_a_declared_command_end_to_end() {
    let turn = CheckedTurn::start("declared-one-call", true);
    declare(&turn, DECLARED_TEST, DECLARED_ARTIFACT);
    let line = format!("pushd \"{}\" && {DECLARED_TEST}", turn.root.display());
    let (command, words) = engram_declared_candidate(&line).expect("a one-call candidate");
    assert_eq!(
        command.directory.as_deref(),
        Some(&*turn.root.to_string_lossy())
    );
    assert_eq!(
        words,
        engram_declared_words(DECLARED_TEST).expect("readable")
    );
    turn.run("check-1", &line, EngramCommandExit::Code(0), || {
        write_artifact(&turn, PASSING_TRX);
    });
    let checkpoint = turn.finish();

    assert_eq!(producer_outcome(&checkpoint), "succeeded", "{checkpoint:#}");
}

#[test]
fn the_five_stated_well_formedness_checks_hold() {
    let text = String::from_utf8(PASSING_TRX.to_vec()).expect("UTF-8");
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (
            edited(
                PASSING_TRX,
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
                "<?xml encoding=\"utf-8\"?>",
            ),
            "an XML declaration without a version",
        ),
        (
            edited(PASSING_TRX, "<Results>", "<Results><?1bad?>"),
            "a processing-instruction target that is not a Name",
        ),
        (
            edited(PASSING_TRX, "<Results>", "<Results><?XmL data?>"),
            "the reserved processing-instruction target xml",
        ),
        (
            edited(PASSING_TRX, "<Results>", "<Results><??>"),
            "an empty processing-instruction target",
        ),
        (
            edited(PASSING_TRX, "<Results>", "<Results><Note>]]></Note>"),
            "]]> in character data",
        ),
        (
            edited(PASSING_TRX, " executed=\"3\"", "executed=\"3\""),
            "no whitespace between two attributes",
        ),
        (
            format!("{}\u{A0}", text.trim_end()).into_bytes(),
            "non-XML whitespace after the root",
        ),
    ];
    for (bytes, why) in cases {
        assert!(
            matches!(verdict(&bytes), EngramTrxVerdict::Unknown(_)),
            "{why}: {:?}",
            verdict(&bytes)
        );
    }
    // Their legitimate neighbours still read.
    for allowed in [
        edited(
            PASSING_TRX,
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
            "<?xml version = '1.0' standalone=\"yes\" ?>",
        ),
        edited(
            PASSING_TRX,
            "<Results>",
            "<Results><?xml-stylesheet href=\"a\"?>",
        ),
        format!("{}\r\n\t ", text.trim_end()).into_bytes(),
    ] {
        assert_eq!(verdict(&allowed), EngramTrxVerdict::Pass);
    }
}

// Freeze-5 review findings (pair 4): each control below failed on freeze 5.

#[test]
fn only_exact_wrapper_shapes_count() {
    for wrapped in [
        // bash runs w.sh, with `-c …` as its arguments.
        format!("bash .tmp/w.sh -c '{DECLARED_TEST}'"),
        // Everything after -File goes to the script.
        format!("pwsh -File w.ps1 -Command '{DECLARED_TEST}'"),
        // The outer `;` runs cp; `true` ignores its script.
        format!(
            "bash --version ; cp src/tests/fixtures/declared-trx/pass.trx \
             .termal-results/results.trx ; true -c '{DECLARED_TEST}'"
        ),
        // A switch outside the fixed set.
        format!("pwsh -ExecutionPolicy Bypass -Command '{DECLARED_TEST}'"),
        // Two flags before the script.
        format!("bash -e -c '{DECLARED_TEST}'"),
        // A separator bash does not split on.
        format!("bash\u{a0}-lc '{DECLARED_TEST}'"),
    ] {
        assert!(
            engram_declared_candidate(&wrapped).is_none(),
            "{wrapped} should be refused"
        );
    }
    let declared = engram_declared_words(DECLARED_TEST).expect("readable");
    for wrapped in [
        format!("bash -c '{DECLARED_TEST}'"),
        format!("pwsh -c '{DECLARED_TEST}'"),
        format!("pwsh -NoProfile -NonInteractive -NoLogo -Command '{DECLARED_TEST}'"),
        format!("pwsh -noprofile -command '{DECLARED_TEST}'"),
    ] {
        let (_, words) = engram_declared_candidate(&wrapped).expect("an exact wrapper shape");
        assert_eq!(words, declared, "{wrapped}");
    }
}

#[test]
fn the_program_word_must_be_unquoted() {
    let quoted = DECLARED_TEST.replacen("dotnet", "\"dotnet\"", 1);
    assert!(engram_declared_words(&quoted).is_err(), "{quoted}");
    let single = DECLARED_TEST.replacen("dotnet", "'dotnet'", 1);
    assert!(engram_declared_words(&single).is_err(), "{single}");
    let wrapped = format!("bash -lc '{quoted}'");
    assert!(engram_declared_candidate(&wrapped).is_none(), "{wrapped}");
}

#[test]
fn a_bang_is_not_taken_inside_quotes_in_this_build() {
    for line in [
        "dotnet test --filter 'TestCategory!=Slow'",
        "dotnet test --filter \"TestCategory!=Slow\"",
    ] {
        assert!(
            engram_declared_words(line).is_err(),
            "{line:?} should be refused"
        );
    }
}

#[test]
fn an_unquoted_option_holding_a_dot_is_refused() {
    // pwsh splits an unquoted native argument that starts with `-` and holds
    // a `.` into two.
    for line in ["java -Dfoo.bar=x Main", "dotnet test -f:net10.0"] {
        assert!(
            engram_declared_words(line).is_err(),
            "{line:?} should be refused"
        );
    }
    // Quoted, or not starting with `-`, it reads.
    assert_eq!(
        engram_declared_words("dotnet test '-f:net10.0' tests/Orders.Tests/.termal-results")
            .expect("readable"),
        vec![
            "dotnet",
            "test",
            "-f:net10.0",
            "tests/Orders.Tests/.termal-results"
        ]
    );
}

#[test]
fn the_remaining_stated_attribute_and_declaration_checks_fail_closed() {
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (
            edited(PASSING_TRX, "<Results>", "<Results><Note a=\"x<y\" />"),
            "`<` in an attribute value",
        ),
        (
            edited(PASSING_TRX, "<Results>", "<Results><Note a=b />"),
            "an unquoted attribute value",
        ),
        (
            edited(
                PASSING_TRX,
                "<Results>",
                "<Results><Note a=\"&undeclared;\" />",
            ),
            "an undeclared reference in an attribute value",
        ),
        (
            format!(
                " {}",
                String::from_utf8(PASSING_TRX[3..].to_vec()).expect("UTF-8")
            )
            .into_bytes(),
            "an XML declaration after leading whitespace",
        ),
        (
            format!(
                "<!-- c -->{}",
                String::from_utf8(PASSING_TRX[3..].to_vec()).expect("UTF-8")
            )
            .into_bytes(),
            "an XML declaration after a comment",
        ),
    ];
    for (bytes, why) in cases {
        assert!(
            matches!(verdict(&bytes), EngramTrxVerdict::Unknown(_)),
            "{why}: {:?}",
            verdict(&bytes)
        );
    }
}

/// Phoenix's real .NET test line, as its maintainers run it, in the
/// forward-slash form a declaration uses.
const PHOENIX_TEST: &str = "dotnet test ./tests/CodeNav.GitTests/CodeNav.GitTests.csproj -c Release \
     --no-restore --nologo --logger \"trx;LogFileName=CodeNav.GitTests.trx\" \
     --results-directory ./artifacts/termal-runner-validation/run1";

#[test]
fn a_real_dotnet_line_in_forward_slash_form_is_declarable() {
    assert_eq!(
        engram_declared_words(PHOENIX_TEST).expect("readable"),
        vec![
            "dotnet",
            "test",
            "./tests/CodeNav.GitTests/CodeNav.GitTests.csproj",
            "-c",
            "Release",
            "--no-restore",
            "--nologo",
            "--logger",
            "trx;LogFileName=CodeNav.GitTests.trx",
            "--results-directory",
            "./artifacts/termal-runner-validation/run1",
        ]
    );
    let root = declared_root("declared-phoenix");
    fs::create_dir_all(root.join("tests/CodeNav.GitTests")).expect("project directory");
    let artifact = "artifacts/termal-runner-validation/run1/CodeNav.GitTests.trx";
    assert!(matches!(
        load(&root, &entry(PHOENIX_TEST, None, artifact)),
        EngramDeclaration::Loaded { .. }
    ));
    assert!(
        bind(&root, PHOENIX_TEST).is_some(),
        "the declared line binds"
    );
}

#[test]
fn a_backslash_path_is_refused_with_its_own_reason() {
    let backslashed =
        PHOENIX_TEST.replace("./tests/CodeNav.GitTests/", ".\\tests\\CodeNav.GitTests\\");
    let reason = engram_declared_words(&backslashed).expect_err("a backslash path is refused");
    assert!(reason.contains("use forward slashes"), "{reason}");
    let quoted = "dotnet test '.\\tests\\CodeNav.GitTests'";
    let reason = engram_declared_words(quoted).expect_err("quoted, too");
    assert!(reason.contains("use forward slashes"), "{reason}");
}

// Freeze-6 review finding (pair 5): this control failed on freeze 6.

#[test]
fn a_wrapper_program_word_is_read_by_the_allow_list_before_its_name() {
    for wrapped in [
        // bash redirects the `true` builtin; dotnet never runs.
        format!("true>.tmp/bash -c '{DECLARED_TEST}'"),
        // The `;` runs `a`, then bash.
        format!("a;bash -c '{DECLARED_TEST}'"),
        // A substitution in the program word.
        format!("$(x)bash -c '{DECLARED_TEST}'"),
        format!("pwsh;x -Command '{DECLARED_TEST}'"),
    ] {
        assert!(
            engram_declared_candidate(&wrapped).is_none(),
            "{wrapped} should be refused"
        );
    }
    // A plain path to the shell still reads.
    let declared = engram_declared_words(DECLARED_TEST).expect("readable");
    let (_, words) = engram_declared_candidate(&format!("/usr/bin/bash -lc '{DECLARED_TEST}'"))
        .expect("a plain program path");
    assert_eq!(words, declared);
}

// Freeze-7 review findings (pair 6).

/// Program-word shapes a shell would not run as the program they name, and
/// plain ones it would. The bare and the wrapped route must read each the
/// same way, through the one program-word rule.
#[test]
fn one_program_word_rule_reads_the_bare_and_the_wrapped_route_alike() {
    let hostile = [
        "X=/usr/bin/bash",
        "SHELL=bash",
        "=bash",
        "@pwsh",
        "true>.tmp/bash",
        "a;bash",
        "$(x)bash",
        "`x`bash",
        "\"bash\"",
        "'bash'",
        "ba\u{a0}sh",
        "ba\\sh",
        "-x.bash",
    ];
    let plain = ["bash", "/usr/bin/bash", "C:/tools/bash", "pwsh"];
    for (program, expected) in hostile
        .iter()
        .map(|program| (*program, false))
        .chain(plain.iter().map(|program| (*program, true)))
    {
        let bare = engram_declared_words(&format!("{program} --version")).is_ok();
        let wrapped = engram_declared_candidate(&format!("{program} -c '{DECLARED_TEST}'"))
            .is_some_and(|(command, _)| command.program == "dotnet");
        assert_eq!(bare, expected, "bare route, program {program:?}");
        assert_eq!(wrapped, expected, "wrapped route, program {program:?}");
    }
}

#[test]
fn a_declared_unknown_is_withheld_as_declared_even_when_its_output_was_cut() {
    let mut check = finished_check(0, engram_ready_basis_capture());
    check.runtime_output_cut = true;
    let mut end = check.end.clone().expect("a finished check");
    end.declared = Some(EngramCapture::settled(EngramDeclaredResult {
        verdict: EngramTrxVerdict::Unknown("the artifact is missing at the end".to_owned()),
        artifact: None,
        outcome: None,
        counters: None,
    }));
    let mut checks = vec![EngramResolvedCheck {
        check,
        end,
        outcome: EngramExecutionOutcome::Unknown,
        basis: EngramExecutionSourceBasis {
            workspace_id: "C:/w".to_owned(),
            source_revision: "revision".to_owned(),
            source_root_generation: None,
            source_root_state: None,
        },
        toolchain: None,
        ran_successfully: true,
    }];
    let withheld = engram_withhold_unjudged_successes(&mut checks);
    assert_eq!(withheld[0].reason, EngramWithheldReason::Declared);
    assert!(
        engram_withheld_check_line(&withheld[0]).contains("missing at the end"),
        "{}",
        engram_withheld_check_line(&withheld[0])
    );
}

#[test]
fn a_wrapped_declared_line_runs_end_to_end() {
    let turn = CheckedTurn::start("declared-wrapped", true);
    declare(&turn, DECLARED_TEST, DECLARED_ARTIFACT);
    let wrapped = format!("bash -c '{DECLARED_TEST}'");
    turn.run("check-1", &wrapped, EngramCommandExit::Code(0), || {
        write_artifact(&turn, PASSING_TRX);
    });
    let checkpoint = turn.finish();

    assert_eq!(producer_outcome(&checkpoint), "succeeded", "{checkpoint:#}");
    assert!(refs(&checkpoint["verification_evidence"][0]).contains(&"kind:declared".to_owned()));
}

// Freeze-8 review findings (pair 7): each control below failed on freeze 8.

#[test]
fn every_wrapper_word_is_read_as_written() {
    for wrapped in [
        // pwsh splits a quoted piece and what follows it into two arguments.
        format!("bash '-'c '{DECLARED_TEST}'"),
        format!("bash \"-\"c '{DECLARED_TEST}'"),
        format!("bash '-l'c '{DECLARED_TEST}'"),
        format!("bash \"-lc\" '{DECLARED_TEST}'"),
        format!("pwsh '-'Command '{DECLARED_TEST}'"),
        format!("pwsh \"-No\"Profile -Command '{DECLARED_TEST}'"),
        format!("pwsh -NoProfile '-c' '{DECLARED_TEST}'"),
        // A shim runs under cmd; a relative shell may be anything.
        format!("bash.cmd -c '{DECLARED_TEST}'"),
        format!("pwsh.bat -c '{DECLARED_TEST}'"),
        format!("pwsh.ps1 -c '{DECLARED_TEST}'"),
        format!(".tmp/bash -c '{DECLARED_TEST}'"),
        format!("tools/bash -lc '{DECLARED_TEST}'"),
    ] {
        assert!(
            engram_declared_candidate(&wrapped).is_none(),
            "{wrapped} should be refused"
        );
    }
    let declared = engram_declared_words(DECLARED_TEST).expect("readable");
    for wrapped in [
        format!("bash -lc '{DECLARED_TEST}'"),
        format!("/usr/bin/bash -c '{DECLARED_TEST}'"),
        format!("C:/tools/bash.exe -c '{DECLARED_TEST}'"),
        format!("pwsh -NoLogo -Command '{DECLARED_TEST}'"),
        format!("pwsh -noprofile -c '{DECLARED_TEST}'"),
    ] {
        let (command, words) =
            engram_declared_candidate(&wrapped).expect("an exact wrapper as written");
        assert_eq!(words, declared, "{wrapped}");
        assert_eq!(command.program, "dotnet");
    }
}

#[test]
fn whitespace_outside_ascii_space_and_tab_is_refused_at_either_end_on_every_route() {
    let one_call = |test: &str| format!("pushd \"C:/src/orders\" && {test}");
    for space in ['\u{a0}', '\u{2003}', '\u{3000}', '\u{2028}', '\u{85}'] {
        for line in [
            format!("{DECLARED_TEST}{space}"),
            format!("{space}{DECLARED_TEST}"),
            format!("bash -c '{DECLARED_TEST}'{space}"),
            format!("{space}bash -c '{DECLARED_TEST}'"),
            one_call(&format!("{DECLARED_TEST}{space}")),
        ] {
            assert!(
                engram_declared_candidate(&line).is_none(),
                "{line:?} should be refused"
            );
        }
    }
    // ASCII spaces and tabs at the ends still read.
    for line in [
        format!("  {DECLARED_TEST}\t"),
        format!("{DECLARED_TEST}\t "),
        format!("bash -c '{DECLARED_TEST}' "),
    ] {
        assert!(engram_declared_candidate(&line).is_some(), "{line:?}");
    }
}

#[test]
fn carriage_returns_are_refused_on_the_bare_declared_route() {
    for ending in ["\r", "\r\n"] {
        let line = format!("node scripts/project-tests.mjs --suite Fast{ending}");
        assert!(engram_declared_candidate(&line).is_none(), "{line:?}");
    }
}

#[test]
fn carriage_returns_are_refused_on_the_wrapper_declared_route() {
    for ending in ["\r", "\r\n"] {
        let line = format!("bash -c 'node scripts/project-tests.mjs --suite Fast'{ending}");
        assert!(engram_declared_candidate(&line).is_none(), "{line:?}");
    }
}

#[test]
fn carriage_returns_are_refused_on_the_one_call_declared_route() {
    let root = declared_root("declared-one-call-carriage-return");
    let control = format!(
        "pushd \"{}\" && node scripts/project-tests.mjs --suite Fast",
        root.display()
    );
    assert!(engram_one_call_prefix(&control).is_some(), "{control:?}");
    assert!(engram_declared_candidate(&control).is_some(), "{control:?}");
    for ending in ["\r", "\r\n"] {
        let line = format!("{control}{ending}");
        assert!(engram_declared_candidate(&line).is_none(), "{line:?}");
    }
}

#[test]
fn unsupported_raw_bytes_are_refused_at_every_position_on_every_declared_route() {
    let root = declared_root("declared-raw-byte-positions");
    let one_call = format!("pushd \"{}\" && {DECLARED_TEST}", root.display());
    assert!(engram_one_call_prefix(&one_call).is_some(), "{one_call:?}");
    let routes = [
        DECLARED_TEST.to_owned(),
        format!("bash -c '{DECLARED_TEST}'"),
        format!("pwsh -NoProfile -Command '{DECLARED_TEST}'"),
        one_call,
    ];
    let refused: Vec<char> = (0u8..=31)
        .filter(|byte| *byte != b'\t')
        .chain(std::iter::once(127))
        .map(char::from)
        .chain([
            '\u{a0}', '\u{2003}', '\u{3000}', '\u{2028}', '\u{85}', '\u{feff}',
        ])
        .collect();
    let declared = engram_declared_words(DECLARED_TEST).expect("readable");
    for route in routes {
        for padding in ["", " ", "\t", " \t "] {
            let control = format!("{padding}{route}{padding}");
            let (_, words) = engram_declared_candidate(&control).expect("space/tab control");
            assert_eq!(words, declared, "{control:?}");
        }
        for &character in &refused {
            for position in 0..=route.len() {
                let mut line = route.clone();
                line.insert(position, character);
                assert!(
                    engram_declared_candidate(&line).is_none(),
                    "{character:?} at byte {position}: {line:?}"
                );
            }
        }
    }
}
