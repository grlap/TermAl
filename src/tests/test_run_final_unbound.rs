// Test-only child of src/tests/test_run_registration.rs.
// Owns final host no-binding proof and boot revalidation witnesses, using
// the parent's real persistence/provider fixtures; does not mint credit.
use super::*;

fn start(fixture: &RunFixture, session: &str, command: &str) -> String {
    fixture.state.note_engram_command_started(
        session,
        &EngramObservationProvenance::Ambient,
        "final-tool",
        Some(command),
        Some(&fixture.root.to_string_lossy()),
    );
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .values()
        .next()
        .unwrap()
        .execution_id
        .clone()
}

fn finish(fixture: &RunFixture, session: &str) {
    fixture.state.observe_test_command_end(
        session,
        &EngramObservationProvenance::Ambient,
        "final-tool",
        Some(EngramCommandExit::Code(0)),
    );
}

fn reload(fixture: &RunFixture) {
    let restored = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    *fixture.state.inner.lock().unwrap() = restored;
}

#[test]
fn final_no_binding_proof_survives_boot_and_historical_monitoring_gaps() {
    let fixture = RunFixture::new("final-unbound-boot");
    let session = running_owner(&fixture, "final-unbound-runtime");
    let id = start(&fixture, &session, "cargo test");
    finish(&fixture, &session);
    let original = fixture.summary(&id);
    assert_eq!(
        original.registration.restart_would_void_evidence,
        Some(false)
    );
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let row = inner.test_run_observations.get_mut(&id).unwrap();
        assert!(test_run_final_unbound_valid(row));
        row.monitoring_interrupted_at = Some(test_run_timestamp());
        let row = row.clone();
        fixture
            .state
            .persist_test_command_observation_locked(&mut inner, row);
    }
    reload(&fixture);
    let restored = fixture.summary(&id);
    assert_eq!(restored.state, TestRunState::Passed);
    assert_eq!(restored.ended_at, original.ended_at);
    assert_eq!(
        restored.registration.restart_would_void_evidence,
        Some(false)
    );
    assert!(
        restored.registration.monitoring_interrupted_at.is_some(),
        "retain actual historical provenance"
    );
    assert!(restored.registration.verification_record_ids.is_empty());
    let mut inner = fixture.state.inner.lock().unwrap();
    inner
        .test_run_observations
        .get_mut(&id)
        .unwrap()
        .summary
        .exit_code = Some(1);
    drop(inner);
    assert_eq!(
        fixture
            .summary(&id)
            .registration
            .restart_would_void_evidence,
        None,
        "contradictory terminal facts cannot reuse the exact final proof"
    );
}

#[test]
fn a_start_snapshot_cannot_finalize_after_binding_or_reporting_changes() {
    for change in ["binding", "grant", "report", "generation"] {
        let fixture = RunFixture::new(&format!("final-unbound-{change}"));
        let session = running_owner(&fixture, "final-unbound-context-runtime");
        let id = start(&fixture, &session, "cargo test");
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            match change {
                "binding" => {
                    record.engram.work_binding = Some(EngramControlWorkBinding {
                        root_execution_id: "new-root".to_owned(),
                        work_id: "new-work".to_owned(),
                        run_id: "new-run".to_owned(),
                        work_revision: 1,
                        claim_id: "new-claim".to_owned(),
                        claim_fence: 1,
                    })
                }
                "grant" => record.engram.active_grant_id = Some("pending-grant".to_owned()),
                "report" => {
                    record.engram.active_turn_report =
                        Some(("pending-grant".to_owned(), EngramTurnReport::default()))
                }
                "generation" => record.active_turn_generation += 1,
                _ => unreachable!(),
            }
        }
        finish(&fixture, &session);
        assert_eq!(
            fixture.summary(&id).state,
            TestRunState::Passed,
            "native execution fact stays truthful"
        );
        assert_eq!(
            fixture
                .summary(&id)
                .registration
                .restart_would_void_evidence,
            None,
            "{change}"
        );
        assert!(
            fixture.state.inner.lock().unwrap().test_run_observations[&id]
                .final_unbound_proof
                .is_none()
        );
        reload(&fixture);
        assert_eq!(
            fixture.summary(&id).state,
            TestRunState::Passed,
            "boot does not erase terminal execution"
        );
        assert_eq!(
            fixture
                .summary(&id)
                .registration
                .restart_would_void_evidence,
            None,
            "{change}"
        );
    }
}

#[test]
fn a_running_unbound_execution_at_boot_remains_interrupted_and_unknown() {
    let fixture = RunFixture::new("final-unbound-running");
    let session = running_owner(&fixture, "final-unbound-running-runtime");
    let id = start(&fixture, &session, "cargo test");
    reload(&fixture);
    let summary = fixture.summary(&id);
    assert_eq!(summary.state, TestRunState::Unknown);
    assert!(summary.interrupted);
    assert!(summary.ended_at.is_none());
    assert!(summary.registration.monitoring_interrupted_at.is_some());
    assert_eq!(summary.registration.restart_would_void_evidence, None);
}

#[test]
fn terminal_artifacts_and_registration_start_snapshots_cannot_clear_unknown_binding() {
    let fixture = RunFixture::new("final-unbound-artifact-only");
    let session = owner(&fixture);
    let run_id = "test-final-artifact-only";
    let directory = registered_run(&fixture, &session, run_id);
    fixture
        .state
        .register_test_run(&session, registration_request(run_id, &directory))
        .unwrap();
    let mut results = passed_results();
    results["runId"] = json!(run_id);
    fs::write(directory.join("results.json"), results.to_string()).unwrap();
    fixture.refresh(&[]);
    assert_eq!(fixture.summary(run_id).state, TestRunState::Passed);
    assert_eq!(
        fixture
            .summary(run_id)
            .registration
            .restart_would_void_evidence,
        None,
        "terminal artifact plus a start snapshot is not final host reporting proof"
    );
    reload(&fixture);
    fixture.refresh(&[]);
    assert_eq!(
        fixture
            .summary(run_id)
            .registration
            .restart_would_void_evidence,
        None
    );
}

#[test]
fn exact_registered_alias_reuses_final_host_proof_after_boot_and_artifact_loss() {
    let fixture = RunFixture::new("final-unbound-registered-alias");
    let session = running_owner(&fixture, "final-unbound-alias-runtime");
    let command = "node scripts/test-launcher.mjs full";
    let observed_id = start(&fixture, &session, command);
    let run_id = format!("test-{}", Uuid::new_v4());
    let mut request = full_request(
        &fs::canonicalize(&fixture.root).unwrap(),
        &test_run_timestamp(),
    );
    request["owner"] = json!(session);
    let mut results = passed_results();
    results["ended"] = json!(test_run_timestamp());
    let directory = write_run(&fixture.runs_dir(), &run_id, request, Some(results));
    fixture
        .state
        .register_test_run(&session, registration_request(&run_id, &directory))
        .unwrap();
    fixture.state.note_engram_command_finished_with_capture(
        &session,
        &EngramObservationProvenance::Ambient,
        "final-tool",
        command,
        &format!("RUN {}", test_run_display_path(&directory)),
        Some(EngramCommandExit::Code(0)),
        false,
    );
    fixture.refresh(&[]);
    let summary = fixture.summary(&run_id);
    assert_eq!(summary.registration.superseded_run_ids, [observed_id]);
    assert_eq!(
        summary.registration.restart_would_void_evidence,
        Some(false)
    );
    reload(&fixture);
    fixture.refresh(&[]);
    let summary = fixture.summary(&run_id);
    assert_eq!(summary.state, TestRunState::Passed);
    assert_eq!(
        summary.registration.restart_would_void_evidence,
        Some(false)
    );
    assert!(summary.registration.monitoring_interrupted_at.is_none());
    fs::remove_file(directory.join("request.json")).unwrap();
    fs::remove_file(directory.join("results.json")).unwrap();
    fs::remove_dir(&directory).unwrap();
    fixture.refresh(&[]);
    let retained = fixture.summary(&run_id);
    assert_eq!(
        retained.state,
        TestRunState::Passed,
        "retain exact durable terminal provenance"
    );
    assert_eq!(
        retained.registration.restart_would_void_evidence,
        Some(false)
    );
    assert!(
        retained.stages.is_empty(),
        "missing artifacts do not invent stage detail"
    );
}
