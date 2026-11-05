//! The other project's maintained launcher in a disposable clone. Its source
//! commit is an explicit input, separate from the reviewed executable pin.
use super::*;

const NATIVE_TEST: &str = "matrix-engram-check.test.mjs";
const COMMAND: &str = "node scripts/test-launcher.mjs focused --input commit -- node --test matrix-engram-check.test.mjs";

fn clone_engram_fixture() -> CompletionFixture {
    let mut fixture = completion_fixture();
    let source = PathBuf::from(
        std::env::var_os("TERMAL_TEST_ENGRAM_LAUNCHER_ROOT")
            .expect("own-launcher row requires the explicit Engram source repository"),
    );
    let commit = std::env::var("TERMAL_TEST_ENGRAM_LAUNCHER_COMMIT")
        .expect("own-launcher row requires its exact Engram source commit");
    assert!(source.is_absolute() && source.is_dir());
    assert!(commit.len() == 40 && commit.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(
        run_git_test_command_output(&source, &["rev-parse", &commit]).trim(),
        commit
    );
    let clone = fixture.root.join(".w/e");
    fs::create_dir_all(clone.parent().unwrap()).unwrap();
    // A local shared clone only reads the source object database; no fetch,
    // configuration write or checkout happens in the source repository.
    run_git_test_command(
        &fixture.root,
        &[
            "clone",
            "--quiet",
            "--shared",
            "--no-checkout",
            source.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    run_git_test_command(&clone, &["checkout", "--quiet", "--detach", &commit]);
    assert_eq!(
        run_git_test_command_output(&clone, &["rev-parse", "HEAD"]).trim(),
        commit
    );
    assert!(run_git_test_command_output(&clone, &["status", "--porcelain"]).is_empty());
    assert!(clone.join("scripts/test-launcher.mjs").is_file());
    assert!(clone.join("scripts/test-run-registration.mjs").is_file());
    let launcher = fs::read(clone.join("scripts/test-launcher.mjs")).unwrap();
    let registration = fs::read(clone.join("scripts/test-run-registration.mjs")).unwrap();
    run_git_test_command(
        &clone,
        &["config", "user.email", "termal-tests@example.com"],
    );
    run_git_test_command(&clone, &["config", "user.name", "TermAl tests"]);
    // The clone keeps the actual Engram tree and scripts. Only its disposable
    // project binding and tiny native check are committed as fixture inputs.
    fs::copy(
        fixture.root.join(".engram-project"),
        clone.join(".engram-project"),
    )
    .unwrap();
    fs::write(clone.join(NATIVE_TEST), r#"import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
test("ordinary intended source", () => {
  assert.match(readFileSync(new URL("./README.md", import.meta.url), "utf8"), /matrix intended source/);
});
"#).unwrap();
    run_git_test_command(&clone, &["add", "--", ".engram-project", NATIVE_TEST]);
    run_git_test_command(
        &clone,
        &[
            "commit",
            "--quiet",
            "-m",
            "disposable project and native check",
        ],
    );
    assert_eq!(
        fs::read(clone.join("scripts/test-launcher.mjs")).unwrap(),
        launcher
    );
    assert_eq!(
        fs::read(clone.join("scripts/test-run-registration.mjs")).unwrap(),
        registration
    );
    fixture.root = fs::canonicalize(clone).unwrap();
    {
        let mut inner = fixture.live.state.inner.lock().unwrap();
        inner
            .projects
            .iter_mut()
            .find(|project| project.id == fixture.project_id)
            .unwrap()
            .root_path = fixture.root.to_string_lossy().into_owned();
        let index = inner.find_session_index(&fixture.live.session_id).unwrap();
        inner.session_mut_by_index(index).unwrap().session.workdir =
            fixture.root.to_string_lossy().into_owned();
        fixture.live.state.commit_locked(&mut inner).unwrap();
    }
    eprintln!(
        "MATRIX_ENGRAM_LAUNCHER_SOURCE {}",
        json!({
            "sourceCommit":commit,"sourceRepository":source,"clone":fixture.root,
            "fixtureCommit":run_git_test_command_output(&fixture.root, &["rev-parse", "HEAD"]).trim()
        })
    );
    fixture
}

#[test]
#[ignore = "requires reviewed Engram binary pin and explicit Engram launcher source root/commit"]
fn ordinary_engram_launcher_edit_commit_check_evaluate_done() {
    let matrix = Matrix::from_fixture(clone_engram_fixture(), Citation::BoundVerification, false);
    matrix.begin("Edit, commit and use Engram's own maintained launcher in this turn.");
    matrix.edit("matrix intended source\n");
    matrix.commit();
    let mut events =
        matrix
            .provider
            .events(&matrix.fixture, &matrix.holder, "engram-launcher", false);
    events.command_line(COMMAND);
    events.start();
    matrix.fixture.wait_for_check_capture(&matrix.holder);
    let output = environment::isolated_launcher_for(
        &matrix.fixture.root,
        &matrix.holder,
        &matrix.service,
        &["node", "--test", NATIVE_TEST],
    )
    .output()
    .unwrap();
    let mut check = matrix.actual_check_output(output);
    assert_eq!(check.exit, 0, "{}", check.output);
    matrix.receive_check(events, &mut check, true);
    matrix.close();
    matrix.settle();
    matrix.begin("A further ordinary holder turn on the checked source.");
    matrix.close();
    matrix.settle();
    matrix.accept(&check, || {});
}
