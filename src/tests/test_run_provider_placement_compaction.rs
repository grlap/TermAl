// Provider-placement retention beside test_run_provider_placement.rs.
// Uses ordinary provider boundaries, history compaction and public API reads;
// no execution proof, compacted row or verification receipt is manufactured.
use super::*;

#[tokio::test]
async fn refused_provider_placement_stays_readable_in_compacted_history_and_detail_after_reload() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = RunFixture::new("provider-refusal-compaction");
    let session = running_owner(&fixture, "refusal-compaction-runtime");
    let outside = fixture.root.with_file_name("refusal-foreign-repository");
    fs::create_dir_all(&outside).unwrap();
    run_git_test_command(&outside, &["init", "--quiet"]);
    let provenance = {
        let inner = fixture.state.inner.lock().unwrap();
        assert!(
            inner
                .projects
                .iter()
                .find(|row| row.id == fixture.project_id)
                .unwrap()
                .engram
                .is_none(),
            "the ordinary producer runs with work tracking disabled"
        );
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        EngramObservationProvenance::Runtime {
            token: record.runtime.runtime_token().unwrap(),
            turn_generation: record.active_turn_generation,
        }
    };
    let mut originals = Vec::new();
    for (key, directory, refusal) in [
        (
            "refused",
            &outside,
            Some(TestRunPlacementRefusal::ForeignRepository),
        ),
        ("healthy", &fixture.root, None),
    ] {
        let cwd = test_run_display_path(&fs::canonicalize(directory).unwrap());
        fixture.state.engram_host().observe(
            &session,
            &provenance,
            EngramRecorderObservation::CommandStarted {
                key,
                ran: Some("cargo test"),
                cwd: Some(&cwd),
            },
        );
        fixture.state.engram_host().observe(
            &session,
            &provenance,
            EngramRecorderObservation::CommandFinished {
                key,
                command: "cargo test",
                reported_command: Some("cargo test"),
                output: "",
                exit: Some(EngramCommandExit::Code(0)),
                runtime_output_cut: false,
            },
        );
        let original = {
            let inner = fixture.state.inner.lock().unwrap();
            inner
                .test_run_observations
                .values()
                .find(|row| row.tool_identity == sha256_hex(key.as_bytes()))
                .unwrap()
                .clone()
        };
        assert_eq!(original.placement_refusal, refusal);
        assert!(
            test_run_final_unbound_valid(&original),
            "the real terminal boundary supplies its original no-binding proof"
        );
        assert!(original.artifact.is_none() && original.evidence.is_empty());
        assert!(
            original.work_binding.is_none()
                && original.source_root.is_none()
                && original.grant_identity.is_none()
        );
        let summary = fixture.summary(&original.execution_id);
        assert_eq!(summary.state, TestRunState::Passed);
        assert_eq!(
            summary.error.as_deref(),
            refusal.map(TestRunPlacementRefusal::message)
        );
        assert!(summary.registration.verification_record_ids.is_empty());
        originals.push((original, refusal));
    }

    // Exceed the actual project bound with genuine terminal provider history.
    // Do not insert compacted state or edit a terminal timestamp/proof.
    super::super::compaction::fill_unbound_history(
        &fixture,
        TEST_RUN_TERMINAL_RUNS_PER_PROJECT + 1,
    );
    fixture.refresh(&[]);
    for restored in [false, true] {
        if restored {
            *fixture.state.inner.lock().unwrap() =
                load_state(fixture.state.persistence_path.as_path())
                    .unwrap()
                    .unwrap();
        }
        {
            let inner = fixture.state.inner.lock().unwrap();
            assert!(inner.test_run_registrations.is_empty());
            for (original, refusal) in &originals {
                assert!(
                    !inner
                        .test_run_observations
                        .contains_key(&original.execution_id)
                );
                let retained = &inner.test_run_compacted[&original.execution_id];
                assert!(
                    retained.compacted,
                    "the public reads must exercise retained history"
                );
                assert_eq!(retained.observations.len(), 1);
                let row = &retained.observations[0];
                assert_eq!(row.execution_id, original.execution_id);
                assert_eq!(row.placement_refusal, *refusal);
                assert_eq!(row.final_unbound_proof, original.final_unbound_proof);
                assert!(test_run_final_unbound_valid(row));
                assert!(row.artifact.is_none() && row.evidence.is_empty());
                assert!(
                    row.work_binding.is_none()
                        && row.source_root.is_none()
                        && row.grant_identity.is_none()
                );
            }
        }
        let app = app_router(fixture.state.clone());
        let (status, history): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .uri("/api/test-runs")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        for (original, refusal) in &originals {
            let listed = history["runs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["runId"] == original.execution_id)
                .unwrap();
            assert_eq!(listed["historyCompacted"], true);
            assert_eq!(
                listed["error"],
                json!(refusal.map(TestRunPlacementRefusal::message)),
                "the deciding placement check and remedy survive compaction; reload={restored}"
            );
            let (status, detail): (StatusCode, Value) = request_json(
                &app,
                Request::builder()
                    .uri(format!("/api/test-runs/{}", original.execution_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(detail["source"], "retained");
            assert_eq!(detail["run"]["historyCompacted"], true);
            assert_eq!(detail["run"]["state"], "passed");
            assert_eq!(
                detail["run"]["error"],
                json!(refusal.map(TestRunPlacementRefusal::message)),
                "retained detail keeps the same bounded deciding check; reload={restored}"
            );
            assert!(
                detail["run"]["verificationRecordIds"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
    }
}
