// Owns the operator-run sub-agent acceptance checks against a pinned real
// Engram store. Requests create actual evaluator delegations; submissions use
// the production CLI runner. Only the model/provider runtime is simulated.
// Does not own policy selection fixtures or the shared completion fixture.
use super::*;
use std::sync::atomic::AtomicUsize;

fn configure_sub_agent(fixture: &CompletionFixture, modes: &str) {
    fixture.command(&fixture.live.session_id, &[
        "control-policy", "set-acceptance-evaluation", "--modes", modes,
        "--mechanical-basis", "asserted", "--authorized-by",
        "disposable-sub-agent-test", "--idempotency-key", "sub-agent-policy",
    ], false);
    fixture.live.state.update_acceptance_defaults(&fixture.project_id,
        AcceptanceEvaluatorDefaults {
            default_mode: Some(AcceptanceEvaluationMode::SameSession),
            ..Default::default()
        },
    ).unwrap();
}

fn another_session(fixture: &CompletionFixture) -> String {
    create_test_project_session(
        &fixture.live.state, Agent::Codex, &fixture.project_id, &fixture.root,
    )
}

fn handoff(fixture: &CompletionFixture, work: &str, from: &str, to: &str) {
    fixture.work(from, &["handoff", work, "--to", to, "--summary",
        "Continue the disposable acceptance witness", "--json"]);
    fixture.work(to, &["handoff", work, "--accept", "--json"]);
    let core = fixture.work(to, &["core", "inspect", work, "--json"]);
    assert_eq!(core["run"]["executor"], to, "actual handoff moves executor: {core:#}");
    let held = fixture.work(to, &["core", "held", "--json"]);
    assert!(held["items"].as_array().unwrap().iter()
        .any(|row| row["short_ref"] == work), "recipient really holds the claim: {held:#}");
    let outgoing = fixture.work(from, &["core", "held", "--json"]);
    assert!(!outgoing["items"].as_array().unwrap().iter()
        .any(|row| row["short_ref"] == work), "outgoing parent no longer holds: {outgoing:#}");
}

fn add_claimed_work(fixture: &CompletionFixture, former: &str, pin: bool) -> String {
    let mut args = vec!["add", "Record the evaluator's distinct child identity", "--accept",
        "The evaluator has its own session and attested parent", "--json"];
    if pin { args.extend(["--evaluation-mode", "sub-agent"]); }
    let added = fixture.work(former, &args);
    let work = added["work"]["short_ref"].as_str()
        .or_else(|| added["short_ref"].as_str()).unwrap().to_owned();
    fixture.work(former, &["claim", &work, "--json"]);
    handoff(fixture, &work, former, &fixture.live.session_id);
    fixture.live.state.ensure_engram_session_bound_off_lock(&fixture.live.session_id)
        .expect("actual parent claim binds").expect("control enabled");
    work
}

fn record_evidence(fixture: &CompletionFixture, work: &str, former: &str) -> String {
    // The pass must cite a real holder note captured BEFORE the request's
    // evidence basis. Do not invent a locator or bypass host verdict checks.
    let body = format!(
        "Canonical handoff from {former} to {} was checked: the requester now holds and executes this run. The live witness will inspect the actual evaluator delegation and stored evaluation to verify its distinct child identity and immutable parent/execution pair.",
        fixture.live.session_id,
    );
    let noted = fixture.work(&fixture.live.session_id,
        &["note", work, &body, "--json"]);
    noted["evidence"].as_str().expect("producer-issued note locator").to_owned()
}

fn spawn_child(
    fixture: &CompletionFixture, work: &str, evidence: &str,
) -> (DelegationRecord, LiveRootCleanup) {
    let response = fixture.live.state.request_acceptance_evaluation(
        &fixture.live.session_id,
        RequestAcceptanceEvaluationRequest {
            work_ref: work.to_owned(), agent: Some(Agent::Codex), model: None,
            criterion_evidence: vec![AcceptanceCriterionEvidenceRequest {
                criterion: 1, locators: vec![evidence.to_owned()],
            }],
        },
    ).unwrap_or_else(|error| panic!("real host request must spawn: {}", error.message));
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, mode, .. } = response else {
        panic!("sub_agent never falls back to a same-session brief")
    };
    let record = delegation.delegation;
    // Delegation binding starts a persistent producer connection for the
    // actual child. Its guard must close before the fixture deletes the home,
    // including on an assertion panic; the parent/reader guards do not own it.
    let cleanup = LiveRootCleanup {
        transport: fixture.live.transport.clone(),
        session_id: record.child_session_id.clone(),
    };
    assert_eq!(mode, AcceptanceEvaluationMode::SubAgent);
    assert!(fixture.live.transport.inner.processes.lock().unwrap()
        .contains_key(&record.child_session_id), "the witness owns a live child connection");
    assert_eq!(record.write_policy, DelegationWritePolicy::ReadOnly);
    assert_eq!(record.parent_session_id, fixture.live.session_id);
    assert_ne!(record.child_session_id, fixture.live.session_id);
    let target = record.acceptance_evaluation.as_ref().unwrap();
    assert_eq!(target.parent_session.as_deref(), Some(fixture.live.session_id.as_str()));
    assert_eq!(target.execution_identity.as_deref(), Some(record.id.as_str()));
    (record, cleanup)
}

fn verdicts(evidence: &str) -> SubmitAcceptanceEvaluationRequest {
    SubmitAcceptanceEvaluationRequest {
        schema_version: ACCEPTANCE_EVALUATION_SUBMISSION_SCHEMA_VERSION,
        verdicts: vec![SubmitAcceptanceEvaluationVerdict {
            criterion: 1, verdict: "pass".to_owned(), basis: Some("judgment".to_owned()),
            rationale: "The actual child is distinct from the parent and the former holder; host attestation is captured on its delegation.".to_owned(),
            evidence: vec![evidence.to_owned()],
        }],
    }
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn live_sub_agent_host_submission_is_accepted_under_each_required_policy() {
    for (modes, pin) in [
        ("sub-agent", false), ("sub-agent,same-session", false),
        ("sub-agent,same-session", true),
    ] {
        let fixture = completion_fixture();
        let _reader = LiveRootCleanup {
            transport: fixture.live.transport.clone(),
            session_id: WORK_HOST_READER_SESSION_ID.to_owned(),
        };
        configure_sub_agent(&fixture, modes);
        let former = another_session(&fixture);
        let work = add_claimed_work(&fixture, &former, pin);
        let evidence = record_evidence(&fixture, &work, &former);
        let (child, child_cleanup) = spawn_child(&fixture, &work, &evidence);
        assert_ne!(child.child_session_id, former);
        let response = fixture.live.state.submit_acceptance_evaluation(
            &child.child_session_id, verdicts(&evidence),
        ).unwrap_or_else(|error| panic!("real producer accepts sub_agent: {}", error.message));
        assert_eq!(response.mode, AcceptanceEvaluationMode::SubAgent);
        assert_eq!(response.receipt["evaluation"]["mode"], "sub_agent");
        assert_eq!(response.receipt["evaluation"]["passed"], 1);
        let hash = response.receipt["evaluation"]["hash"].as_str().unwrap();
        let evaluation = fixture.object(hash);
        assert_eq!(evaluation["mode"], "sub_agent");
        assert_eq!(evaluation["evaluator"]["session_id"], child.child_session_id);
        assert_eq!(evaluation["parent_session"], fixture.live.session_id);
        assert_eq!(evaluation["execution_identity"], child.id);
        for affiliated in [&former, &fixture.live.session_id] {
            assert_ne!(evaluation["evaluator"]["session_id"], affiliated.as_str());
        }
        let core = fixture.work(&fixture.live.session_id, &["core", "inspect", &work, "--json"]);
        assert_eq!(core["run"]["executor"], fixture.live.session_id);
        let stored = fixture.live.state.inner.lock().unwrap().delegations.iter()
            .find(|record| record.id == child.id).unwrap().acceptance_evaluation.clone().unwrap();
        assert!(matches!(stored.submission, AcceptanceEvaluationSubmission::Recorded { .. }));
        eprintln!("real host sub_agent accepted modes={modes} pin={pin} evaluator={} parent={} former={} hash={hash}",
            child.child_session_id, fixture.live.session_id, former);
        drop(child_cleanup);
        assert!(!fixture.live.transport.inner.processes.lock().unwrap()
            .contains_key(&child.child_session_id), "normal teardown retires the child connection");
    }
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn live_sub_agent_parent_loses_both_routes_after_spawn_and_host_surfaces_final_refusal() {
    let fixture = completion_fixture();
    let _reader = LiveRootCleanup {
        transport: fixture.live.transport.clone(),
        session_id: WORK_HOST_READER_SESSION_ID.to_owned(),
    };
    configure_sub_agent(&fixture, "sub-agent");
    let former = another_session(&fixture);
    let work = add_claimed_work(&fixture, &former, false);
    let evidence = record_evidence(&fixture, &work, &former);
    let (child, child_cleanup) = spawn_child(&fixture, &work, &evidence);
    // Preflight was eligible; a real producer handoff afterwards removes BOTH
    // routes. Releasing only the claim would leave the parent as executor.
    let successor = another_session(&fixture);
    handoff(&fixture, &work, &fixture.live.session_id, &successor);
    let calls = AtomicUsize::new(0);
    let refusal = Mutex::new(None);
    let error = fixture.live.state.submit_acceptance_evaluation_with_runner(
        &child.child_session_id, verdicts(&evidence), |connection, args, timeout| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let result = run_acceptance_evaluation_submit(connection, args, timeout);
            if let Ok(output) = &result {
                *refusal.lock().unwrap() = serde_json::from_slice::<Value>(&output.stderr).ok();
            }
            result
        },
    ).expect_err("the real producer, not parent preflight, must refuse");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "no automatic retry");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("Engram refused") && !error.message.contains("unknown"), "{error:?}");
    let refusal = refusal.lock().unwrap().clone().expect("real Engram error envelope");
    // The pin's public envelope, not the current producer's internal enum
    // spelling. This must be the parent-standing refusal, not CLI usage or a
    // host preflight refusal; the two real eligibility routes were removed.
    assert!(refusal["error"]["code"].as_str().is_some_and(|code| !code.is_empty()), "{refusal:#}");
    assert!(refusal["error"]["message"].as_str().is_some_and(|message|
        message.contains("sub_agent parent session must hold or execute the run")), "{refusal:#}");
    let stored = fixture.live.state.inner.lock().unwrap().delegations.iter()
        .find(|record| record.id == child.id).unwrap().acceptance_evaluation.clone().unwrap();
    assert!(matches!(stored.submission, AcceptanceEvaluationSubmission::None));
    assert_eq!(stored.parent_session.as_deref(), Some(fixture.live.session_id.as_str()));
    eprintln!("real host sub_agent final refusal after actual handoff: {refusal:#}");
    drop(child_cleanup);
    assert!(!fixture.live.transport.inner.processes.lock().unwrap()
        .contains_key(&child.child_session_id), "normal teardown retires the child connection");
}
