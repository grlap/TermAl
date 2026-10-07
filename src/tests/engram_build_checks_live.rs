//! Real Cargo outcomes through the host checkpoint and a pinned disposable
//! Engram store. Does not complete an operator's task or exercise live homes.
use super::*;

fn execute_build_check(fixture: &CompletionFixture, command: &str) {
    let session = &fixture.live.session_id;
    let mut recorder = SessionRecorder::new(fixture.live.state.clone(), session.clone());
    recorder
        .command_started_in(
            "build-admission",
            command,
            Some(command),
            Some(&fixture.root.to_string_lossy()),
        )
        .unwrap();
    fixture.wait_for_check_capture(session);
    let words = engram_shell_words(command).unwrap();
    let output = Command::new(&words[0])
        .args(&words[1..])
        .current_dir(&fixture.root)
        .env("CARGO_TARGET_DIR", fixture.root.join("target"))
        .output()
        .expect("the actual Cargo command launches");
    let code = output.status.code().expect("actual native Cargo exit") as i64;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    recorder
        .command_completed_with_exit(
            "build-admission",
            command,
            &text,
            if code == 0 {
                CommandStatus::Success
            } else {
                CommandStatus::Error
            },
            EngramCommandExit::Code(code),
        )
        .unwrap();
    fixture.wait_for_check_capture(session);
    finish_live_root(&fixture.live.state, session);
}

fn build_evaluation_args(work: &str, shown: &Value, id: &str, revision: &str) -> Vec<String> {
    vec![
        "evaluate".into(),
        work.into(),
        "--mode".into(),
        "same-session".into(),
        "--acceptance-basis".into(),
        shown["acceptance_basis"].as_i64().unwrap().to_string(),
        "--evidence-basis".into(),
        shown["evidence_basis"].as_i64().unwrap().to_string(),
        "--verdict".into(),
        "1=pass:observed".into(),
        "--rationale".into(),
        "1=The actual host-observed command supplies this criterion's evidence".into(),
        "--evidence".into(),
        format!("1={id}"),
        "--source-fingerprint".into(),
        revision.into(),
        "--attempt".into(),
        format!("build-admission-{id}"),
        "--json".into(),
    ]
}

fn rejected_build_evaluation(
    fixture: &CompletionFixture,
    args: &[String],
    citation: &str,
    mismatch: &str,
) {
    let session = &fixture.live.session_id;
    let (actor, context) = {
        let inner = fixture.live.state.inner.lock().unwrap();
        engram_runtime_actor_identity(
            &inner.preferences.engram.developer_name,
            &inner.sessions[inner.find_session_index(session).unwrap()],
        )
    };
    let mut command = Command::new(live_engram_binary());
    command
        .arg("--project-file")
        .arg(fixture.root.join(".engram-project"))
        .arg("--home")
        .arg(&fixture.home)
        .args(["work", "--actor-id", &actor, "--session-id", session]);
    if let Some(context) = context {
        command.args(["--actor-context", &context]);
    }
    let output = command.args(args).output().unwrap();
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.status.success(),
        "must refuse this citation: {text}"
    );
    let refusal: Value = serde_json::from_slice(&output.stderr)
        .or_else(|_| serde_json::from_slice(&output.stdout))
        .unwrap_or_else(|error| panic!("structured refusal required: {error}: {text}"));
    assert_eq!(
        refusal["error"]["code"], "acceptance_evaluation_refused",
        "{text}"
    );
    let cause = &refusal["error"]["details"]["cause"];
    assert_eq!(cause["kind"], "citation", "{text}");
    assert_eq!(cause["mismatch"], mismatch, "{text}");
    assert_eq!(cause["criterion"].as_u64(), Some(1), "{text}");
    assert_eq!(cause["requirement"]["check_kind"], "build", "{text}");
    assert_eq!(cause["citation"], citation, "{text}");
}

fn newest_build_record(
    fixture: &CompletionFixture,
    work: &str,
    kind: &str,
    result: &str,
) -> String {
    let shown = fixture.work(
        &fixture.live.session_id,
        &["show", work, "--notes", "--json"],
    );
    let mut records = Vec::new();
    objects_with_key(&shown, "verification", &mut records);
    let rows = records
        .into_iter()
        .filter(|row| row["verification"]["check_kind"] == kind)
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1, "one store-minted {kind}: {shown:#}");
    assert_eq!(rows[0]["verification"]["result"], result, "{shown:#}");
    rows[0]["locator"].as_str().unwrap().to_owned()
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn engram_build_live_producer_admits_actual_build_and_refuses_failed_or_test_only_citations() {
    for fails in [false, true] {
        let fixture = completion_fixture();
        let session = &fixture.live.session_id;
        let _cleanup = LiveRootCleanup {
            transport: fixture.live.transport.clone(),
            session_id: session.clone(),
        };
        if fails {
            fs::write(
                fixture.root.join("src/lib.rs"),
                "compile_error!(\"deliberate failed build fixture\");\n",
            )
            .unwrap();
        }
        let added = fixture.work(
            session,
            &[
                "add",
                "Observe a real Build",
                "--accept",
                "The host records a passing Build on this source",
                "--bind",
                "1=build",
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
            .ensure_engram_session_bound_off_lock(session)
            .unwrap()
            .unwrap();
        let named = fixture
            .live
            .state
            .name_engram_source_root(
                session,
                EngramSourceRootRequest {
                    work: work.into(),
                    path: Some(Some(fixture.root.to_string_lossy().into_owned())),
                },
            )
            .unwrap();
        fixture.work(
            session,
            &[
                "note",
                work,
                "Run the claimed Build in the named source",
                "--status",
                "--json",
            ],
        );
        let revision = named.source_revision.unwrap();
        fixture.begin(session, "Run the actual Cargo build.");
        execute_build_check(&fixture, "cargo build --offline");
        let checkpoints = fixture.live.transport.requests_for("turn_checkpoint");
        let checkpoint = checkpoints.last().unwrap();
        let rows = checkpoint["verification_evidence"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "one command, one kind: {checkpoint:#}");
        assert_eq!(rows[0]["check_kind"], "build");
        assert!(
            rows[0]["environment"]["index"].as_u64().is_some(),
            "{checkpoint:#}"
        );
        let id = newest_build_record(
            &fixture,
            work,
            "build",
            if fails { "failed" } else { "passed" },
        );
        let stored = fixture.object(&id);
        assert_eq!(
            stored["source_basis"]["source_revision"], revision,
            "{stored:#}"
        );
        assert_eq!(
            stored["source_basis"]["source_root_generation"], named.generation,
            "{stored:#}"
        );
        assert_eq!(stored["check_kind"], "build", "{stored:#}");
        let shown = fixture.work(session, &["show", work, "--json"]);
        let args = build_evaluation_args(work, &shown, &id, &revision);
        if fails {
            rejected_build_evaluation(&fixture, &args, &id, "passed_verification_required");
        } else {
            let receipt = fixture.work(
                session,
                &args.iter().map(String::as_str).collect::<Vec<_>>(),
            );
            let evaluation = &receipt["evaluation"];
            assert_eq!(evaluation["passed"].as_u64(), Some(1), "{receipt:#}");
            assert_eq!(
                evaluation["verdicts_total"].as_u64(),
                Some(1),
                "{receipt:#}"
            );
            assert_eq!(
                evaluation["verdicts_omitted"].as_u64(),
                Some(0),
                "{receipt:#}"
            );
            assert_eq!(evaluation["source_fingerprint"], revision, "{receipt:#}");
            let verdicts = evaluation["verdicts"].as_array().unwrap();
            assert_eq!(verdicts.len(), 1, "{receipt:#}");
            assert_eq!(verdicts[0]["position"].as_u64(), Some(1), "{receipt:#}");
            assert_eq!(verdicts[0]["basis"], "observed", "{receipt:#}");
            assert_eq!(verdicts[0]["verdict"], "pass", "{receipt:#}");
            assert_eq!(verdicts[0]["citations"].as_u64(), Some(1), "{receipt:#}");
            // Same source and run, but a passed Test cannot supply a Build
            // citation. Use an actual test subprocess, not invented evidence.
            fixture.begin(session, "Run a Test as the wrong-kind control.");
            execute_build_check(&fixture, "cargo test --offline");
            let test = newest_build_record(&fixture, work, "test", "passed");
            let shown = fixture.work(session, &["show", work, "--json"]);
            rejected_build_evaluation(
                &fixture,
                &build_evaluation_args(work, &shown, &test, &revision),
                &test,
                "bound_verification_mismatch",
            );
        }
        assert_eq!(
            content_revision(&fixture.root).unwrap().1,
            revision,
            "the actual command did not mutate its named source"
        );
    }
}
