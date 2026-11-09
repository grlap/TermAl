use super::*;
use crate::tests::engram_declared_source_fixture::NativeSourceOwner;

#[test]
fn native_source_wire_is_closed_and_cannot_decode_as_a_legacy_commit_record() {
    let source = json!({"commit":COMMIT,"tree":COMMIT,"submodules":[],"changes":[{
        "path":"local.txt","status":"??","origin":null,"blob":COMMIT,"mode":"100644"
    }]});
    let input = json!({"kind":"source_id","source_id":source});
    let post_run = json!({"source_id":source});
    let parsed: EngramVerificationRecordInput = serde_json::from_value(input.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), input);
    let parsed: EngramVerificationRecordPostRun = serde_json::from_value(post_run.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), post_run);
    for field in ["commit", "clean_before", "unknown"] {
        let mut mixed = input.clone();
        mixed[field] = json!(true);
        assert!(
            serde_json::from_value::<EngramVerificationRecordInput>(mixed).is_err(),
            "{field}"
        );
        let mut mixed = post_run.clone();
        mixed[field] = json!(true);
        assert!(
            serde_json::from_value::<EngramVerificationRecordPostRun>(mixed).is_err(),
            "{field}"
        );
    }
    for invalid in [Value::Null, json!({"commit":COMMIT}), json!({"changes":[]})] {
        assert!(
            serde_json::from_value::<EngramVerificationRecordInput>(
                json!({"kind":"source_id","source_id":invalid})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<EngramVerificationRecordPostRun>(json!({"source_id":invalid}))
                .is_err()
        );
    }
    let mut missing_nullable = input.clone();
    missing_nullable["source_id"]["changes"][0]
        .as_object_mut()
        .unwrap()
        .remove("origin");
    assert!(serde_json::from_value::<EngramVerificationRecordInput>(missing_nullable).is_err());
    let historical = json!({"outcome":"clean","clean_before":true,"clean_after":true});
    let parsed: EngramVerificationRecordPostRun =
        serde_json::from_value(historical.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), historical);
}

const PASS: &str = include_str!("fixtures/declared-trx/pass.trx");
const FAIL: &str = include_str!("fixtures/declared-trx/fail.trx");
const ZERO: &str = include_str!("fixtures/declared-trx/zero.trx");

fn native_request(root: &FsPath) -> Value {
    let mut request = commit_request(root);
    request["full"] = json!(false);
    request["stages"] = json!([{ "name": "focused", "command": "dotnet", "args": ["test"] }]);
    request
}

fn native_results() -> Value {
    let mut results = commit_results();
    results["state"] = json!("passed");
    results["exitCode"] = json!(0);
    results["stages"] = json!([stage("focused", "passed", 0, &["dotnet", "test"], 1)]);
    results
}

fn native_fixture(label: &str) -> RunFixture {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = RunFixture::new(label);
    fs::write(fixture.root.join(".gitignore"), "/.tmp/\n").unwrap();
    fs::write(
        fixture.root.join("termal-tests.toml"),
        "[[test]]\ncommand = \"dotnet test\"\ncwd = \".\"\nartifact = \".tmp/results.trx\"\n",
    )
    .unwrap();
    run_git_test_command(
        &fixture.root,
        &["add", "--", ".gitignore", "termal-tests.toml"],
    );
    run_git_test_command(
        &fixture.root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "--no-gpg-sign",
            "-m",
            "native declaration fixture",
        ],
    );
    fixture
}

#[test]
fn generic_launcher_registration_cannot_adopt_a_declaration_or_later_artifact() {
    let fixture = native_fixture("native-declared-launcher-refused");
    let request = native_request(&fixture.root);
    let binding = test_run_declared_binding(&request, &fixture.root).unwrap();
    let mut run = record_run(
        &fixture,
        "test-native-declared",
        request.clone(),
        native_results(),
    );
    fs::create_dir_all(fixture.root.join(".tmp")).unwrap();
    fs::write(fixture.root.join(".tmp/results.trx"), PASS).unwrap();
    // Even an old persisted generic binding cannot opt into native credit.
    run.registration.declared = Some(binding);
    assert!(
        engram_verification_record_body(&run.registration, &request, &native_results()).is_err()
    );
    assert_eq!(
        engram_verification_record_registration_unclassified_stages(
            &run.registration,
            &request,
            &native_results()
        ),
        vec!["focused"]
    );
    let mut spoof = request.clone();
    spoof["stages"][0]["kind"] = json!("test");
    assert!(engram_verification_record_body(&run.registration, &spoof, &native_results()).is_err());
}

#[test]
fn declared_end_artifact_unknown_or_failure_never_inherits_a_process_pass() {
    for (label, artifact, expected) in [
        (
            "missing",
            None,
            EngramVerificationRecordCheckOutcome::Indeterminate,
        ),
        (
            "malformed",
            Some("<broken>"),
            EngramVerificationRecordCheckOutcome::Indeterminate,
        ),
        (
            "zero",
            Some(ZERO),
            EngramVerificationRecordCheckOutcome::Indeterminate,
        ),
        (
            "failure",
            Some(FAIL),
            EngramVerificationRecordCheckOutcome::Failed,
        ),
        (
            "pass",
            Some(PASS),
            EngramVerificationRecordCheckOutcome::Passed,
        ),
    ] {
        let fixture = native_fixture(&format!("native-declared-{label}"));
        let request = native_request(&fixture.root);
        let binding = test_run_declared_binding(&request, &fixture.root).unwrap();
        fs::create_dir_all(fixture.root.join(".tmp")).unwrap();
        if let Some(artifact) = artifact {
            fs::write(fixture.root.join(".tmp/results.trx"), artifact).unwrap();
        }
        let check = engram_verification_record_declared_check(
            &native_results()["stages"][0],
            &engram_verification_record_planned_argv(&request),
            &binding,
            EngramCommandExit::ReportedSuccess,
        )
        .unwrap();
        assert_eq!(check.outcome, expected, "{label}");
        if label == "pass" {
            assert!(
                check
                    .refs
                    .iter()
                    .any(|value| value.starts_with("artifact-sha256:"))
            );
            let mut substituted = native_results()["stages"][0].clone();
            substituted["command"] = json!(["dotnet", "test", "another.csproj"]);
            assert!(
                engram_verification_record_declared_check(
                    &substituted,
                    &engram_verification_record_planned_argv(&request),
                    &binding,
                    EngramCommandExit::ReportedSuccess
                )
                .is_err()
            );
        }
    }
}

#[test]
fn declared_snapshot_refuses_stale_artifact_or_changed_declaration() {
    for changed_declaration in [false, true] {
        let fixture = native_fixture("native-declared-stale");
        let request = native_request(&fixture.root);
        fs::create_dir_all(fixture.root.join(".tmp")).unwrap();
        fs::write(fixture.root.join(".tmp/results.trx"), PASS).unwrap();
        let binding = test_run_declared_binding(&request, &fixture.root).unwrap();
        if changed_declaration {
            fs::write(
                fixture.root.join("termal-tests.toml"),
                "[[test]]\ncommand = \"dotnet other\"\nartifact = \".tmp/results.trx\"\n",
            )
            .unwrap();
        }
        let check = engram_verification_record_declared_check(
            &native_results()["stages"][0],
            &engram_verification_record_planned_argv(&request),
            &binding,
            EngramCommandExit::Code(0),
        )
        .unwrap();
        assert_eq!(
            check.outcome,
            EngramVerificationRecordCheckOutcome::Indeterminate
        );
        assert!(check.summary.as_ref().unwrap().contains("UNKNOWN"));
    }
}

#[test]
fn flutter_json_reporter_counts_use_the_same_end_artifact_reader() {
    let fixture = native_fixture("native-declared-flutter");
    fs::write(fixture.root.join("termal-tests.toml"),
        "[[test]]\ncommand = \"flutter test --file-reporter json:.tmp/results.json\"\nartifact = \".tmp/results.json\"\n").unwrap();
    let mut request = native_request(&fixture.root);
    request["stages"][0] = json!({"name":"focused", "command":"flutter",
        "args":["test", "--file-reporter", "json:.tmp/results.json"]});
    let binding = test_run_declared_binding(&request, &fixture.root).unwrap();
    fs::create_dir_all(fixture.root.join(".tmp")).unwrap();
    fs::write(
        fixture.root.join(".tmp/results.json"),
        include_bytes!("fixtures/declared-dart/flutter-pass.jsonl"),
    )
    .unwrap();
    let mut results = native_results();
    results["stages"][0]["command"] = json!([
        "flutter",
        "test",
        "--file-reporter",
        "json:.tmp/results.json"
    ]);
    let planned = engram_verification_record_planned_argv(&request);
    let check = engram_verification_record_declared_check(
        &results["stages"][0],
        &planned,
        &binding,
        EngramCommandExit::Code(0),
    )
    .unwrap();
    assert_eq!(check.outcome, EngramVerificationRecordCheckOutcome::Passed);
    assert_eq!(check.counts.as_ref().unwrap()["passed"], 1);
    assert!(
        check
            .refs
            .iter()
            .any(|value| value.starts_with("dart-json-outcome:"))
    );
    results["stages"][0]["state"] = json!("failed");
    let failed = engram_verification_record_declared_check(
        &results["stages"][0],
        &planned,
        &binding,
        EngramCommandExit::Code(1),
    )
    .unwrap();
    assert_eq!(
        failed.outcome,
        EngramVerificationRecordCheckOutcome::Passed,
        "the frozen artifact supplies the native verdict independently of the process report"
    );
}

#[test]
fn observed_declared_source_retains_local_changes_and_requires_a_committed_declaration_path() {
    let fixture = native_fixture("native-declared-source");
    let binding = test_run_declared_binding(&native_request(&fixture.root), &fixture.root).unwrap();
    let owner = NativeSourceOwner::new(&fixture.root);
    let capture = || owner.source(&binding, std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT);
    let before = capture().unwrap();
    assert_eq!(before.source_id.commit.len(), 40);
    fs::write(fixture.root.join("untracked.txt"), "dirt").unwrap();
    owner.record_file_change("untracked.txt", "??");
    let dirty = capture().unwrap();
    assert_eq!(
        dirty.source_id,
        owner.identity(),
        "the full owner delta is retained without a clean filter"
    );
    assert_ne!(dirty.source_id, before.source_id);
    fs::remove_file(fixture.root.join("untracked.txt")).unwrap();
    let mut restored = owner.identity();
    restored.changes.clear();
    owner.replace(restored);
    run_git_test_command(
        &fixture.root,
        &["rm", "--cached", "--", "termal-tests.toml"],
    );
    fs::write(
        fixture.root.join(".gitignore"),
        "/.tmp/\n/termal-tests.toml\n",
    )
    .unwrap();
    run_git_test_command(&fixture.root, &["add", "--", ".gitignore"]);
    run_git_test_command(
        &fixture.root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "--no-gpg-sign",
            "-m",
            "ignored uncommitted declaration fixture",
        ],
    );
    owner.refresh_commit();
    assert!(
        capture().is_err(),
        "an ignored declaration is not a committed declaration"
    );
}

#[test]
fn native_committed_declaration_membership_does_not_require_blob_byte_equality() {
    let fixture = native_fixture("native-declared-crlf");
    fs::write(
        fixture.root.join(".gitattributes"),
        "termal-tests.toml text eol=crlf\n",
    )
    .unwrap();
    run_git_test_command(
        &fixture.root,
        &["add", "--", ".gitattributes", "termal-tests.toml"],
    );
    run_git_test_command(
        &fixture.root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "--no-gpg-sign",
            "-m",
            "declaration line endings",
        ],
    );
    // A matching cached stat can skip conversion even with --force. Remove
    // only this disposable fixture file so Git must create its CRLF checkout.
    fs::remove_file(fixture.root.join("termal-tests.toml")).unwrap();
    run_git_test_command(
        &fixture.root,
        &[
            "checkout-index",
            "--force",
            "--index",
            "--",
            "termal-tests.toml",
        ],
    );
    let bytes = fs::read(fixture.root.join("termal-tests.toml")).unwrap();
    if !bytes.windows(2).any(|pair| pair == b"\r\n") {
        fixture
            .state
            .test_temp_root
            .as_ref()
            .unwrap()
            .retain_for_diagnosis();
        eprintln!("CRLF fixture kept: {}", fixture.root.display());
        eprintln!(
            "attributes: {:?}",
            fs::read_to_string(fixture.root.join(".gitattributes"))
        );
        eprintln!(
            "resolved attributes: {}",
            run_git_test_command_output(
                &fixture.root,
                &["check-attr", "--all", "--", "termal-tests.toml"]
            )
        );
        eprintln!(
            "index/worktree eol: {}",
            run_git_test_command_output(
                &fixture.root,
                &["ls-files", "--eol", "--", "termal-tests.toml"]
            )
        );
    }
    assert!(
        bytes.windows(2).any(|pair| pair == b"\r\n"),
        "Git actually checked out CRLF"
    );
    let owner = NativeSourceOwner::new(&fixture.root);
    let snapshot = owner.snapshot(std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT, None);
    assert!(
        snapshot.is_ok(),
        "committed CRLF declaration path: {snapshot:?}"
    );
}

#[test]
fn native_source_retains_child_changes_in_the_full_owner_object() {
    let fixture = native_fixture("native-declared-submodule");
    let donor = fixture.root.join(".tmp/donor");
    fs::create_dir_all(&donor).unwrap();
    run_git_test_command(&donor, &["init", "--quiet"]);
    run_git_test_command(&donor, &["config", "core.autocrlf", "false"]);
    fs::write(donor.join("input.txt"), "pinned\n").unwrap();
    run_git_test_command(&donor, &["add", "--", "input.txt"]);
    run_git_test_command(
        &donor,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "--no-gpg-sign",
            "-m",
            "child fixture",
        ],
    );
    let donor_path = identity_git_path(&fs::canonicalize(&donor).unwrap()).unwrap();
    run_git_test_command(
        &fixture.root,
        &[
            "-c",
            "core.autocrlf=false",
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--quiet",
            "--",
            &donor_path,
            "child",
        ],
    );
    let child = fixture.root.join("child");
    run_git_test_command(&child, &["config", "core.autocrlf", "false"]);
    run_git_test_command(&fixture.root, &["add", "--", ".gitmodules", "child"]);
    run_git_test_command(
        &fixture.root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "--no-gpg-sign",
            "-m",
            "pinned child fixture",
        ],
    );
    let binding = test_run_declared_binding(&native_request(&fixture.root), &fixture.root).unwrap();
    let owner = NativeSourceOwner::new(&fixture.root);
    let mut identity = owner.identity();
    identity.submodules = vec![EngramSourceIdSubmodule {
        path: "child".into(),
        commit: run_git_test_command_output(&child, &["rev-parse", "HEAD"]),
        changes: Vec::new(),
    }];
    owner.replace(identity);
    let capture = || owner.source(&binding, std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT);
    let before = capture().unwrap();
    assert_eq!(capture().unwrap().source_id, before.source_id);
    fs::write(child.join("input.txt"), "dirty after\n").unwrap();
    let mut changed = owner.identity();
    changed.submodules[0].changes = vec![EngramSourceIdChange {
        path: "input.txt".into(),
        status: " M".into(),
        origin: None,
        blob: Some(run_git_test_command_output(
            &child,
            &["hash-object", "--", "input.txt"],
        )),
        mode: Some(EngramSourceIdMode::Regular),
    }];
    owner.replace(changed);
    let after = capture().unwrap();
    assert_eq!(after.source_id, owner.identity());
    assert_ne!(
        after.source_id, before.source_id,
        "same root commit does not hide a changed child object"
    );
}

#[test]
fn native_opening_keeps_the_pre_prompt_artifact_image_and_exact_command() {
    let fixture = native_fixture("native-opening-image");
    let timings = Arc::new(SourceReadTimings::default());
    let owner = NativeSourceOwner::new(&fixture.root);
    let started = std::time::Instant::now();
    let captured = owner.snapshot(started + REVIEW_FREEZE_TIMEOUT, Some(timings.clone()));
    let summary = timings.summary();
    eprintln!(
        "scripted-owner native S0 assembly: success={} elapsed_ms={} stages={summary}",
        captured.is_ok(),
        started.elapsed().as_millis()
    );
    let snapshot = captured.unwrap();
    assert!(
        summary.contains("opening_preimages:")
            && summary.contains("source_owner_capture:")
            && summary.contains("git_cat_file:")
            && summary.contains("run_directory:"),
        "{summary}"
    );
    assert!(
        !summary.contains("native-opening-image"),
        "timings expose stages, not repository paths"
    );
    assert_eq!(snapshot.source.source_id.commit.len(), 40);
    fs::create_dir_all(fixture.root.join(".tmp")).unwrap();
    fs::write(fixture.root.join(".tmp/results.trx"), PASS).unwrap();
    let (_, binding) = snapshot
        .match_command("dotnet test", &fixture.root)
        .unwrap();
    assert_eq!(
        binding.artifact_start,
        EngramArtifactStart::Absent,
        "matching cannot replace the true pre-prompt artifact image with a mid-run read"
    );
    assert!(matches!(
        engram_declared_result(&binding, EngramCommandExit::Code(0)).verdict,
        EngramTrxVerdict::Pass
    ));
    assert!(
        snapshot
            .match_command("dotnet test other.csproj", &fixture.root)
            .is_none()
    );
    assert!(
        snapshot
            .match_command("dotnet test", &fixture.root.join(".tmp"))
            .is_none()
    );
}

#[test]
fn native_first_tool_rule_never_recovers_after_an_earlier_tool() {
    let mut first = DeclaredRunFirstTool::default();
    first.observe(Some("command-1"), true);
    first.observe(Some("command-1"), true);
    assert!(
        first.accepts("command-1"),
        "duplicate frames keep the same original command"
    );
    first.observe(Some("read-file"), false);
    first.observe(Some("command-1"), true);
    assert!(!first.accepts("command-1"));
    for earlier in [Some("read"), Some("edit"), Some("mcp"), None] {
        let mut first = DeclaredRunFirstTool::default();
        first.observe(earlier, false);
        first.observe(Some("test"), true);
        assert!(
            !first.accepts("test"),
            "every earlier or unidentified tool withholds credit"
        );
    }
}

#[test]
fn both_native_provider_parsers_report_all_tool_kinds_for_the_first_action_rule() {
    for kind in [
        "fileChange",
        "mcpToolCall",
        "dynamicToolCall",
        "unknownFutureTool",
    ] {
        let mut recorder = TestRecorder::default();
        handle_codex_app_server_item_started(&json!({"id":"earlier", "type":kind}), &mut recorder)
            .unwrap();
        handle_codex_app_server_item_started(
            &json!({"id":"test", "type":"commandExecution",
            "command":"dotnet test", "cwd":"."}),
            &mut recorder,
        )
        .unwrap();
        let mut first = DeclaredRunFirstTool::default();
        for (key, command) in recorder.native_tool_activities {
            first.observe(key.as_deref(), command);
        }
        assert!(
            !first.accepts("test"),
            "Codex {kind} is earlier tool activity"
        );
    }
    for name in ["Read", "Edit", "mcp__server__read", "unknownFutureTool"] {
        let mut recorder = TestRecorder::default();
        let mut state = ClaudeTurnState::default();
        register_claude_tool_use(
            &json!({"id":"earlier", "name":name,"input":{}}),
            &mut state,
            &mut recorder,
        )
        .unwrap();
        register_claude_tool_use(
            &json!({"id":"test", "name":"Bash",
            "input":{"command":"dotnet test"}}),
            &mut state,
            &mut recorder,
        )
        .unwrap();
        let mut first = DeclaredRunFirstTool::default();
        for (key, command) in recorder.native_tool_activities {
            first.observe(key.as_deref(), command);
        }
        assert!(
            !first.accepts("test"),
            "Claude {name} is earlier tool activity"
        );
    }
    let mut recorder = TestRecorder::default();
    register_claude_tool_use(
        &json!({"name":"Read"}),
        &mut ClaudeTurnState::default(),
        &mut recorder,
    )
    .unwrap();
    assert_eq!(
        recorder.native_tool_activities,
        vec![(None, false)],
        "unidentified activity fails closed"
    );
}

#[test]
fn codex_native_command_and_claude_one_call_select_the_same_opening_entry() {
    let fixture = native_fixture("native-provider-command");
    let owner = NativeSourceOwner::new(&fixture.root);
    let snapshot = owner
        .snapshot(std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT, None)
        .unwrap();
    let mut codex = TestRecorder::default();
    handle_codex_app_server_item_started(
        &json!({"id":"test", "type":"commandExecution",
        "command":"dotnet test", "cwd":test_run_display_path(&fixture.root)}),
        &mut codex,
    )
    .unwrap();
    assert_eq!(
        codex.native_tool_activities,
        vec![(Some("test".into()), true)]
    );
    let codex_binding = snapshot
        .match_command("dotnet test", &fixture.root)
        .unwrap()
        .1;
    let directory = identity_git_path(&fs::canonicalize(&fixture.root).unwrap())
        .unwrap()
        .replace('\\', "/");
    let one_call = format!("pushd \"{directory}\" && dotnet test");
    let mut claude = TestRecorder::default();
    register_claude_tool_use(
        &json!({"id":"test", "name":"Bash", "input":{"command":one_call}}),
        &mut ClaudeTurnState::default(),
        &mut claude,
    )
    .unwrap();
    assert_eq!(
        claude.native_tool_activities,
        vec![(Some("test".into()), true)]
    );
    assert_eq!(
        snapshot.match_command(&one_call, &fixture.root).unwrap().1,
        codex_binding
    );
}
