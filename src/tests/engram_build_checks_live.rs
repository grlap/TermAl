//! Real native Build/Test records through registered verification and a pinned disposable store.
use super::*;

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
        if fails {
            fs::write(fixture.root.join("src/lib.rs"), "compile_error!(\"deliberate failed build fixture\");\n").unwrap();
            run_git_test_command(&fixture.root, &["add", "src/lib.rs"]);
            run_git_test_command(&fixture.root, &["commit", "--quiet", "-m", "deliberate failed build input"]);
        }
        let added = fixture.work(session, &["add", "Observe a real Build", "--accept",
            "The host records a passing Build on this source", "--bind", "1=build", "--json"]);
        let work = added["work"]["short_ref"].as_str().or_else(|| added["short_ref"].as_str()).unwrap();
        fixture.work(session, &["claim", work, "--json"]);
        let named = fixture.live.state.name_engram_source_root(session, EngramSourceRootRequest {
            work: work.to_owned(), path: Some(Some(fixture.root.to_string_lossy().into_owned())),
        }).unwrap();
        fixture.work(session, &["note", work, "Run the claimed Build in the named source", "--status", "--json"]);
        let source = content_revision(&fixture.root).unwrap().1;
        fixture.begin(session, "Run the actual Cargo build.");
        let (body, receipt) = fixture.registered_check(session, &fixture.root, "cargo build --offline");
        let id = &receipt.verifications[0].record_id;
        let stored = fixture.object(id);
        assert_eq!(stored["check_kind"], "build", "{stored:#}");
        assert_eq!(stored["source_basis"]["source_root_generation"], named.generation, "{stored:#}");
        assert_eq!(stored["source_basis"], serde_json::to_value(engram_verification_record_basis(&body).unwrap()).unwrap());
        assert_eq!(newest_build_record(&fixture, work, "build", if fails { "failed" } else { "passed" }), *id);
        let EngramVerificationRecordInput::Commit { commit: revision } = &body.input else { panic!("the registered native witness requires committed input"); };
        let shown = fixture.work(session, &["show", work, "--json"]);
        let args = build_evaluation_args(work, &shown, id, revision);
        if fails {
            rejected_build_evaluation(&fixture, &args, id, "passed_verification_required");
        } else {
            let evaluated = fixture.work(session, &args.iter().map(String::as_str).collect::<Vec<_>>());
            assert_eq!(evaluated["evaluation"]["passed"], 1, "{evaluated:#}");
            assert_eq!(evaluated["evaluation"]["verdicts_total"], 1);
            assert_eq!(evaluated["evaluation"]["verdicts_omitted"], 0);
            assert_eq!(evaluated["evaluation"]["source_fingerprint"], *revision);
            let verdicts = evaluated["evaluation"]["verdicts"].as_array().unwrap();
            assert_eq!(verdicts.len(), 1);
            assert_eq!(verdicts[0]["position"], 1);
            assert_eq!(verdicts[0]["basis"], "observed");
            assert_eq!(verdicts[0]["verdict"], "pass");
            assert_eq!(verdicts[0]["citations"], 1);
            finish_live_root(&fixture.live.state, session);
            fixture.wait_for_reports(session);
            fixture.begin(session, "Run a real Test as the wrong-kind control.");
            let (_, receipt) = fixture.registered_check(session, &fixture.root, "cargo test --offline");
            let test = newest_build_record(&fixture, work, "test", "passed");
            assert_eq!(test, receipt.verifications[0].record_id);
            let shown = fixture.work(session, &["show", work, "--json"]);
            rejected_build_evaluation(&fixture, &build_evaluation_args(work, &shown, &test, revision),
                &test, "bound_verification_mismatch");
        }
        finish_live_root(&fixture.live.state, session);
        fixture.wait_for_reports(session);
        assert_eq!(content_revision(&fixture.root).unwrap().1, source, "the actual command did not mutate its named source");
        assert_no_live_turn_gate(&fixture.live.transport);
    }
}
