//! A real failed formatter stage reaches the pinned disposable Engram as Lint.
use super::*;

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn full_launcher_formatter_failure_is_stored_as_lint_without_test_credit() {
    let fixture = completion_fixture();
    install_launcher(&fixture);
    let session = &fixture.live.session_id;
    let service = LoopbackRegistration::start(&fixture.live.state);
    fs::create_dir_all(&service.temporary).unwrap();
    // Reuse the existing locked UI setup fixture. No UI stage will run: the
    // actual formatter must stop this full invocation at its first stage.
    let setup = Command::new(if cfg!(windows) { "node.exe" } else { "node" })
        .current_dir(&fixture.root)
        .args([
            "--input-type=module",
            "-e",
            r#"import {readFileSync, appendFileSync} from 'node:fs';
import {writeUiBootstrapFixture} from './scripts/test-ui-bootstrap-fixtures.mjs';
const ignore = readFileSync('.gitignore', 'utf8');
const env = writeUiBootstrapFixture(process.cwd(), process.argv[1]);
appendFileSync('.gitignore', ignore.replace('Cargo.lock\n', ''));
console.log(JSON.stringify(env));"#,
        ])
        .arg(&service.temporary)
        .output()
        .expect("existing UI setup fixture starts");
    assert!(setup.status.success(), "UI fixture setup failed");
    let ui_environment: BTreeMap<String, String> = serde_json::from_slice(&setup.stdout).unwrap();
    fs::copy(
        FsPath::new(env!("CARGO_MANIFEST_DIR")).join("ui/test-categories.ts"),
        fixture.root.join("ui/test-categories.ts"),
    )
    .unwrap();
    let source = "pub fn answer()->u32{42}\n";
    fs::write(fixture.root.join("src/lib.rs"), source).unwrap();
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(&fixture.root)
        .output()
        .expect("fixture lockfile preparation starts");
    assert!(lock.status.success(), "fixture lockfile preparation failed");
    run_git_test_command(
        &fixture.root,
        &[
            "add",
            "--",
            ".gitignore",
            "Cargo.lock",
            "src/lib.rs",
            "ui/package.json",
            "ui/package-lock.json",
            "ui/test-categories.ts",
        ],
    );
    run_git_test_command(
        &fixture.root,
        &[
            "commit",
            "--quiet",
            "-m",
            "unformatted full-launcher fixture",
        ],
    );
    let added = fixture.work(
        session,
        &[
            "add",
            "Receive an actual formatter failure",
            "--accept",
            "The real formatter is recorded as Lint",
            "--json",
        ],
    );
    let work = added["work"]["short_ref"]
        .as_str()
        .or_else(|| added["short_ref"].as_str())
        .unwrap();
    fixture.work(session, &["claim", work, "--json"]);
    fixture
        .live
        .state
        .name_engram_source_root(
            session,
            EngramSourceRootRequest {
                work: work.to_owned(),
                path: Some(Some(fixture.root.to_string_lossy().into_owned())),
            },
        )
        .unwrap();
    fixture.begin(session, "Run the real full launcher on unformatted Rust.");
    let before = fixture.source_id_now(session, &fixture.root);
    let mut command = environment::isolated_full_launcher(&fixture.root, session, &service);
    for (key, value) in &ui_environment {
        command.env(key, value);
    }
    let output = command.output().expect("actual full launcher starts");
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(1), "{text}");
    let directory = text
        .lines()
        .find_map(|line| line.strip_prefix("RUN "))
        .map(PathBuf::from)
        .expect("actual full launcher prints its original run locator");
    let request: Value =
        serde_json::from_slice(&fs::read(directory.join("request.json")).unwrap()).unwrap();
    let results: Value =
        serde_json::from_slice(&fs::read(directory.join("results.json")).unwrap()).unwrap();
    assert_eq!(request["full"], true);
    assert_eq!(request["input"], "commit");
    assert_eq!(request["owner"].as_str(), Some(session.as_str()));
    assert_eq!(results["input"]["cleanBefore"], true, "{text}");
    assert_eq!(results["input"]["outcome"], "clean", "{text}");
    assert_eq!(results["input"]["after"]["clean"], true, "{text}");
    assert_eq!(results["input"]["eligible"], false);
    assert_eq!(results["state"], "failed");
    let stages = results["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 6);
    assert_eq!(stages[0]["name"], "fmt");
    assert_eq!(stages[0]["state"], "failed", "{text}");
    assert!(stages[0]["code"].as_i64().is_some_and(|code| code != 0));
    assert!(stages[1..].iter().all(|stage| stage["state"] == "unrun"));
    assert_eq!(
        fs::read_to_string(fixture.root.join("src/lib.rs")).unwrap(),
        source
    );
    assert!(fixture.source_id_now(session, &fixture.root) == before);
    let run_id = request["runId"].as_str().unwrap();
    let registration = fixture
        .live
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations[run_id]
        .clone();
    let EngramVerificationRecordPrepared::Ready(frozen) =
        engram_verification_record_prepare(&registration)
    else {
        panic!("the actual failed full run must prepare a verification record");
    };
    assert_eq!(frozen.body.checks.len(), 6);
    assert_eq!(frozen.record["checks"][0]["check_kind"], "lint");
    let fmt_argv = engram_shell_words(&frozen.body.checks[0].command).unwrap();
    assert_eq!(fmt_argv.len(), 3);
    assert_eq!(engram_program_name(&fmt_argv[0]), "cargo");
    assert_eq!(fmt_argv[1], "fmt");
    assert_eq!(fmt_argv[2], "--check");
    assert!(engram_verification_record_producers(run_id, &frozen.body).is_empty());
    let (body, receipt) = fixture.record_registered_check(registration.clone(), frozen);
    assert_eq!(receipt.verifications[0].stage, "fmt");
    let stored = fixture.object(&receipt.verifications[0].record_id);
    assert_eq!(stored["check_kind"], "lint");
    assert_eq!(stored["result"], "failed");
    assert_eq!(
        fixture
            .live
            .state
            .complete_engram_verification_record(&registration, &body, &receipt),
        EngramVerificationRecordSettlement::Settled
    );
    eprintln!(
        "PINNED_FMT_LINT {}",
        json!({"run_id":run_id, "directory":directory, "check_kind":stored["check_kind"],
            "result":stored["result"], "record_id":receipt.verifications[0].record_id,
            "records":receipt.verifications.len(), "stages":stages.len(), "settled":true})
    );
    finish_live_root(&fixture.live.state, session);
    fixture.wait_for_reports(session);
    assert_no_live_turn_gate(&fixture.live.transport);
}
