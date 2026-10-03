//! Real producer accounting separates a between-turn write from a quiet turn.
use super::*;

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn between_turn_observation_makes_the_earlier_check_stale() {
    for named in [true, false] {
        let fixture = completion_fixture();
        let session = &fixture.live.session_id;
        let added = fixture.work(session, &[
            "add", "Check source observation freshness", "--accept",
            "A test passes at the current source", "--bind", "1=test", "--json",
        ]);
        let work = added["work"]["short_ref"].as_str()
            .or_else(|| added["short_ref"].as_str()).unwrap().to_owned();
        fixture.work(session, &["claim", &work, "--json"]);
        fixture.live.state.ensure_engram_session_bound_off_lock(session).unwrap().unwrap();
        let generation = if named {
            Some(fixture.live.state.name_engram_source_root(session, EngramSourceRootRequest {
                work: work.clone(), path: Some(Some(fixture.root.to_string_lossy().into_owned())),
            }).unwrap().generation as i64)
        } else { None };

        fixture.begin(session, "Check the first measured revision.");
        let first_basis = fixture.record(session, |record|
            record.engram.active_turn_start_basis.clone().unwrap());
        assert_eq!(first_basis.source_root_generation, generation);
        let cwd = fs::canonicalize(&fixture.root).unwrap().to_string_lossy().into_owned();
        let mut recorder = SessionRecorder::new(fixture.live.state.clone(), session.clone());
        recorder.command_started_in("first-check", "cargo test --offline",
            Some("cargo test --offline"), Some(&cwd)).unwrap();
        fixture.wait_for_check_capture(session);
        let output = Command::new("cargo").args(["test", "--offline"])
            .current_dir(&fixture.root).output().unwrap();
        let text = format!("{}\n{}", String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success() && text.contains("1 passed"), "{text}");
        recorder.command_completed_with_exit("first-check", "cargo test --offline", &text,
            CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
        fixture.wait_for_check_capture(session);
        finish_live_root(&fixture.live.state, session);
        let shown = fixture.work(session, &["show", &work, "--notes", "--json"]);
        let mut checks = Vec::new();
        objects_with_key(&shown, "verification", &mut checks);
        assert_eq!(checks.len(), 1, "{shown:#}");
        let verification = checks[0]["locator"].as_str().unwrap().to_owned();
        let original_check = fixture.object(&verification);
        assert_eq!(original_check["source_basis"], serde_json::to_value(&first_basis).unwrap(),
            "the stored producer verification retains its complete basis: {original_check:#}");
        assert_eq!(checks[0]["verification"]["result"], "passed");
        let before_count = fixture.live.transport.requests_for("execution_observe").len();

        // No recorder or watcher reports this write. Only the next real opening
        // measurement can account for the interval.
        fs::write(fixture.root.join("README.md"), "changed between mediated turns\n").unwrap();
        let changed_revision = content_revision(&fixture.root).unwrap().1;
        assert_ne!(changed_revision, first_basis.source_revision);
        fixture.begin(session, "Inspect quietly without changing source or running a check.");
        let requests = fixture.live.transport.requests_for("execution_observe");
        assert_eq!(requests.len(), before_count + 1);
        let request = requests.last().unwrap();
        let change = &request["occurrence"]["source_change"];
        assert_eq!(request["occurrence"]["kind"], "inter_turn_change");
        assert_eq!(request["causality"]["kind"], "unknown");
        assert_eq!(request["policy_basis"]["mode"], "account_if_eligible");
        assert_eq!(change["detection"], "content_comparison");
        assert_eq!(change["baseline"]["source_revision"], first_basis.source_revision);
        assert_eq!(change["sighting"]["source_basis"]["source_revision"], changed_revision);
        let receipt = {
            let inner = fixture.live.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(session).unwrap()];
            assert!(matches!(record.engram.source_opening_disposition,
                EngramSourceOpeningDisposition::Accounted { .. }));
            let intent = inner.engram_source_sightings.iter().flat_map(|owner| &owner.observations)
                .find(|intent| intent.session_id == *session
                    && intent.sighting.basis.source_revision == changed_revision).unwrap();
            let EngramSourceObservationPhase::Recorded { receipt, .. } = &intent.phase
                else { panic!("provider delivered only after durable accounting"); };
            receipt.clone()
        };
        assert_eq!(receipt["accounting"]["kind"], "source_change");
        let stored_observation = fixture.object(receipt["observation"].as_str().unwrap());
        assert_eq!(stored_observation["occurrence"]["source_change"], *change);
        assert_eq!(stored_observation["causality"]["kind"], "unknown");
        assert_eq!(stored_observation["accounting"]["kind"], "source_change");
        finish_live_root(&fixture.live.state, session);
        assert_eq!(content_revision(&fixture.root).unwrap().1, changed_revision);
        let checkpoints = fixture.live.transport.requests_for("turn_checkpoint");
        let quiet = checkpoints.last().unwrap();
        assert!(quiet["observations"].as_array().unwrap().iter()
            .all(|observation| observation["source_changed"] == false), "{quiet:#}");
        assert!(quiet["verification_evidence"].as_array().is_none_or(Vec::is_empty), "{quiet:#}");
        assert_eq!(fixture.object(&verification), original_check,
            "a later observation does not rewrite the old verification");
        // A verification detail reconstructs its historical record cut. The
        // current evaluation consumer must refuse to cite that old check as
        // evidence for the newly observed revision.
        let current = fixture.work(session, &["show", &work, "--json"]);
        let acceptance = current["acceptance_basis"].as_i64().unwrap().to_string();
        let evidence = current["evidence_basis"].as_i64().unwrap().to_string();
        let citation = format!("1={verification}");
        let refused = refused_evaluation(&fixture, session, &[
            "evaluate", &work, "--mode", "same-session", "--acceptance-basis", &acceptance,
            "--evidence-basis", &evidence, "--verdict", "1=pass:observed", "--rationale",
            "1=Attempt to cite the earlier passed check at the current revision", "--evidence",
            &citation, "--source-fingerprint", &changed_revision, "--attempt", "old-check", "--json",
        ]);
        assert_eq!(refused["error"]["code"], "acceptance_evaluation_refused", "{refused:#}");
        let cause = &refused["error"]["details"]["cause"];
        assert_eq!(cause["kind"], "citation", "{refused:#}");
        assert_eq!(cause["mismatch"], "wrong_source", "{refused:#}");
        assert_eq!(cause["citation"], verification, "{refused:#}");
        assert_eq!(cause["checked_revision"], first_basis.source_revision, "{refused:#}");
        assert_eq!(cause["judged_revision"], changed_revision, "{refused:#}");
        assert_eq!(cause["remedy"], "run_current_check_and_evaluate", "{refused:#}");
    }
}

fn refused_evaluation(fixture: &CompletionFixture, session: &str, args: &[&str]) -> Value {
    let (actor, context) = {
        let inner = fixture.live.state.inner.lock().unwrap();
        engram_runtime_actor_identity(&inner.preferences.engram.developer_name,
            &inner.sessions[inner.find_session_index(session).unwrap()])
    };
    let mut command = Command::new(live_engram_binary());
    command.arg("--project-file").arg(fixture.root.join(".engram-project"))
        .arg("--home").arg(&fixture.home)
        .args(["work", "--actor-id", &actor, "--session-id", session]);
    if let Some(context) = context.as_deref() { command.args(["--actor-context", context]); }
    let output = command.args(args).output().unwrap();
    assert_eq!(output.status.code(), Some(1), "a stale citation must be refused: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stderr).unwrap_or_else(|error|
        panic!("typed evaluation refusal: {error}: {}", String::from_utf8_lossy(&output.stderr)))
}
