//! Ordinary bound-work completion without a required acceptance evaluation.
use super::*;

#[derive(Clone, Copy)]
enum CompletionInput {
    Checked,
    Dirty,
    NewCommit,
    OlderCheck,
    Reverted,
}

fn assert_no_evaluations(matrix: &Matrix) {
    let database = matrix.fixture.store().database_path;
    assert!(database.starts_with(&matrix.fixture.home));
    let connection =
        rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM objects WHERE object_kind = 'acceptance_evaluation'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 0,
        "plain completion must not manufacture an evaluation"
    );
}

fn bound_completion_without_evaluation(input: CompletionInput) {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.fixture.command(
        &matrix.fixture.live.session_id,
        &[
            "control-policy",
            "set-acceptance-evaluation",
            "--mechanical-basis",
            "observed",
            "--authorized-by",
            "disposable-bound-completion-test",
            "--idempotency-key",
            "matrix-evaluations-off",
        ],
        false,
    );
    let policy = matrix.fixture.command(
        &matrix.fixture.live.session_id,
        &["control-policy", "show"],
        false,
    );
    assert_eq!(policy["acceptance_evaluation"]["allowed_modes"], json!([]));
    matrix.begin("Run the real bound check with acceptance evaluations off.");
    let check = matrix.check(false);
    matrix.close();
    matrix.settle();
    assert_no_evaluations(&matrix);
    let originals = matrix.stored_objects();
    let checked_commit = check.results["input"]["commit"].as_str().unwrap();
    match input {
        CompletionInput::Checked => {}
        CompletionInput::Dirty => matrix.edit("unchecked dirty source\n"),
        CompletionInput::NewCommit => {
            run_git_test_command(
                &matrix.fixture.root,
                &[
                    "commit",
                    "--quiet",
                    "--allow-empty",
                    "-m",
                    "different clean input",
                ],
            );
            assert!(
                run_git_test_command_output(
                    &matrix.fixture.root,
                    &["diff", "--name-only", checked_commit, "HEAD"],
                )
                .is_empty(),
                "the different commit deliberately retains the checked tree"
            );
        }
        CompletionInput::OlderCheck => {
            matrix.edit("new committed source without a fresh check\n");
            matrix.commit();
            assert!(
                !run_git_test_command_output(
                    &matrix.fixture.root,
                    &["diff", "--name-only", checked_commit, "HEAD"],
                )
                .is_empty(),
                "only the original input was checked"
            );
        }
        CompletionInput::Reverted => {
            let original = fs::read(matrix.fixture.root.join("README.md")).unwrap();
            matrix.edit("temporary unchecked edit\n");
            fs::write(matrix.fixture.root.join("README.md"), original).unwrap();
        }
    }
    if matches!(input, CompletionInput::Checked | CompletionInput::Reverted) {
        matrix.assert_checked_source(&check);
        matrix.done();
    } else {
        if !matches!(input, CompletionInput::Dirty) {
            assert_ne!(
                run_git_test_command_output(&matrix.fixture.root, &["rev-parse", "HEAD"]).trim(),
                checked_commit,
            );
            assert!(
                run_git_test_command_output(&matrix.fixture.root, &["status", "--porcelain"])
                    .is_empty()
            );
        }
        let output = matrix.done_output(&check);
        eprintln!(
            "MATRIX_DONE_POLICY_OFF_REFUSAL exit={:?} stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(!output.status.success(), "an unchecked input must not seal");
        let body: Value = serde_json::from_slice(&output.stdout)
            .or_else(|_| serde_json::from_slice(&output.stderr))
            .unwrap();
        assert_eq!(body["error"]["code"], "work_completion_refused", "{body:#}");
        let text = body.to_string().to_ascii_lowercase();
        assert!(
            text.contains(if matches!(input, CompletionInput::Dirty) {
                "dirty"
            } else {
                "commit"
            }),
            "the refusal must name the actual input fault: {body:#}"
        );
        assert!(!text.contains("missing_acceptance_evaluation"), "{body:#}");
        let core = matrix
            .fixture
            .work(&matrix.holder, &["core", "inspect", &matrix.work, "--json"]);
        assert!(core["run"]["completion_seal"].is_null(), "{core:#}");
    }
    assert_no_evaluations(&matrix);
    matrix.assert_objects_unchanged(originals);
}

#[test]
#[ignore = "requires reviewed Engram binary pin"]
fn evaluations_off_clean_bound_check_seals() {
    bound_completion_without_evaluation(CompletionInput::Checked);
}

#[test]
#[ignore = "requires reviewed Engram binary pin"]
fn evaluations_off_dirty_root_is_refused_without_seal() {
    bound_completion_without_evaluation(CompletionInput::Dirty);
}

#[test]
#[ignore = "requires reviewed Engram binary pin"]
fn evaluations_off_new_commit_is_refused_without_seal() {
    bound_completion_without_evaluation(CompletionInput::NewCommit);
}

#[test]
#[ignore = "requires reviewed Engram binary pin"]
fn evaluations_off_older_check_is_refused_without_seal() {
    bound_completion_without_evaluation(CompletionInput::OlderCheck);
}

#[test]
#[ignore = "requires reviewed Engram binary pin"]
fn evaluations_off_reverted_edit_accepts_original_check() {
    bound_completion_without_evaluation(CompletionInput::Reverted);
}
