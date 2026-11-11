//! Real native Build/Test records through registered verification and a pinned disposable store.
use super::*;

fn build_evaluation_args(work: &str, shown: &Value, id: &str, source_file: &str) -> Vec<String> {
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
        "--source-id-file".into(),
        source_file.to_owned(),
        "--attempt".into(),
        format!("build-admission-{id}"),
        "--json".into(),
    ]
}

enum BuildEvaluationRefusal<'a> {
    NoPassedRegisteredCheck,
    CitationMismatch(&'a str),
}

fn rejected_build_evaluation(
    fixture: &CompletionFixture,
    args: &[String],
    citation: &str,
    expected: BuildEvaluationRefusal<'_>,
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
    assert!(
        !output.status.success(),
        "must refuse this citation (exit {:?}, stdout {} bytes, stderr {} bytes)",
        output.status.code(),
        output.stdout.len(),
        output.stderr.len()
    );
    let refusal: Value = serde_json::from_slice(&output.stderr)
        .or_else(|_| serde_json::from_slice(&output.stdout))
        .unwrap_or_else(|_| panic!("structured refusal required"));
    let mismatch = match expected {
        BuildEvaluationRefusal::NoPassedRegisteredCheck => {
            // Registered-input freshness is required before citation admission.
            assert_eq!(refusal["error"]["code"], "work_completion_refused");
            assert_eq!(
                refusal["error"]["details"]["reason"],
                "no registered check at the evaluated cut tested the declared commit; run a passing check on the intended current commit"
            );
            return;
        }
        BuildEvaluationRefusal::CitationMismatch(mismatch) => mismatch,
    };
    assert_eq!(
        refusal["error"]["code"], "acceptance_evaluation_refused",
        "<refusal details withheld>"
    );
    let cause = &refusal["error"]["details"]["cause"];
    assert_eq!(cause["kind"], "citation", "<refusal details withheld>");
    assert_eq!(cause["mismatch"], mismatch, "<refusal details withheld>");
    assert_eq!(
        cause["criterion"].as_u64(),
        Some(1),
        "<refusal details withheld>"
    );
    assert_eq!(
        cause["requirement"]["check_kind"], "build",
        "<refusal details withheld>"
    );
    assert_eq!(cause["citation"], citation, "<refusal details withheld>");
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
    assert_eq!(rows.len(), 1, "one store-minted {kind}: <shown withheld>");
    assert_eq!(
        rows[0]["verification"]["result"], result,
        "<shown withheld>"
    );
    rows[0]["locator"].as_str().unwrap().to_owned()
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn engram_build_live_producer_admits_actual_build_and_refuses_failed_or_test_only_citations() {
    for fails in [false, true] {
        let fixture = completion_fixture();
        let session = &fixture.live.session_id;
        if fails {
            fs::write(
                fixture.root.join("src/lib.rs"),
                "compile_error!(\"deliberate failed build fixture\");\n",
            )
            .unwrap();
            run_git_test_command(&fixture.root, &["add", "src/lib.rs"]);
            run_git_test_command(
                &fixture.root,
                &["commit", "--quiet", "-m", "deliberate failed build input"],
            );
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
        let named = fixture
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
        let source = fixture.source_id_now(session, &fixture.root);
        fixture.begin(session, "Run the actual Cargo build.");
        let (body, receipt) =
            fixture.registered_check(session, &fixture.root, "cargo build --offline");
        let id = &receipt.verifications[0].record_id;
        let stored = fixture.object(id);
        assert_eq!(stored["check_kind"], "build", "<stored withheld>");
        assert_eq!(
            stored["source_basis"]["source_root_generation"], named.generation,
            "<stored withheld>"
        );
        assert!(
            stored["source_basis"]
                == serde_json::to_value(engram_verification_record_basis(&body).unwrap()).unwrap(),
            "the Build record retains the complete registered source basis"
        );
        assert_eq!(
            newest_build_record(
                &fixture,
                work,
                "build",
                if fails { "failed" } else { "passed" }
            ),
            *id
        );
        let declaration = fixture.checked_source_file(&body);
        let source_file = declaration.path().to_str().unwrap();
        let shown = fixture.work(session, &["show", work, "--json"]);
        let args = build_evaluation_args(work, &shown, id, source_file);
        if fails {
            rejected_build_evaluation(
                &fixture,
                &args,
                id,
                BuildEvaluationRefusal::NoPassedRegisteredCheck,
            );
        } else {
            let evaluated = fixture.work(
                session,
                &args.iter().map(String::as_str).collect::<Vec<_>>(),
            );
            assert_eq!(evaluated["evaluation"]["passed"], 1, "<evaluated withheld>");
            assert_eq!(evaluated["evaluation"]["verdicts_total"], 1);
            assert_eq!(evaluated["evaluation"]["verdicts_omitted"], 0);
            fixture.assert_evaluation_source(&evaluated, &source);
            let verdicts = evaluated["evaluation"]["verdicts"].as_array().unwrap();
            assert_eq!(verdicts.len(), 1);
            assert_eq!(verdicts[0]["position"], 1);
            assert_eq!(verdicts[0]["basis"], "observed");
            assert_eq!(verdicts[0]["verdict"], "pass");
            assert_eq!(verdicts[0]["citations"], 1);
            finish_live_root(&fixture.live.state, session);
            fixture.wait_for_reports(session);
            fixture.begin(session, "Run a real Test as the wrong-kind control.");
            let (_, receipt) =
                fixture.registered_check(session, &fixture.root, "cargo test --offline");
            let test = newest_build_record(&fixture, work, "test", "passed");
            assert_eq!(test, receipt.verifications[0].record_id);
            let shown = fixture.work(session, &["show", work, "--json"]);
            rejected_build_evaluation(
                &fixture,
                &build_evaluation_args(work, &shown, &test, source_file),
                &test,
                BuildEvaluationRefusal::CitationMismatch("bound_verification_mismatch"),
            );
        }
        finish_live_root(&fixture.live.state, session);
        fixture.wait_for_reports(session);
        assert!(
            fixture.source_id_now(session, &fixture.root) == source,
            "the actual command did not mutate its named source"
        );
        assert!(
            fixture.object(id) == stored,
            "evaluation never rewrites the Build record"
        );
        assert_no_live_turn_gate(&fixture.live.transport);
    }
}
