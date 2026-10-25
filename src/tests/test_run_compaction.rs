// Test-only child of src/tests/test_run_registration.rs.
// Owns resolved-detail compaction, lossless identity and unresolved-backlog
// discriminators through real persistence/API fixtures; does not mint credit.
use super::*;

fn fill_safe(fixture: &RunFixture, session: &str, count: usize) {
    for number in 0..count {
        let key = format!("compact-native-{number}");
        fixture.state.note_engram_command_started(
            session,
            &EngramObservationProvenance::Ambient,
            &key,
            Some("cargo test"),
            Some(&fixture.root.to_string_lossy()),
        );
        fixture.state.observe_test_command_end(
            session,
            &EngramObservationProvenance::Ambient,
            &key,
            Some(EngramCommandExit::Code(0)),
        );
    }
}

fn registered_alias(fixture: &RunFixture, session: &str, key: &str) -> (String, String, PathBuf) {
    let command = "node scripts/test-launcher.mjs full";
    fixture.state.note_engram_command_started(
        session,
        &EngramObservationProvenance::Ambient,
        key,
        Some(command),
        Some(&fixture.root.to_string_lossy()),
    );
    let observed = fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .values()
        .find(|row| row.tool_identity == sha256_hex(key.as_bytes()))
        .unwrap()
        .execution_id
        .clone();
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
        .register_test_run(session, registration_request(&run_id, &directory))
        .unwrap();
    fixture.state.note_engram_command_finished_with_capture(
        session,
        &EngramObservationProvenance::Ambient,
        key,
        command,
        &format!("RUN {}", test_run_display_path(&directory)),
        Some(EngramCommandExit::Code(0)),
        false,
    );
    fixture.refresh(&[]);
    assert_eq!(
        fixture.summary(&run_id).registration.superseded_run_ids,
        [observed.clone()]
    );
    (run_id, observed, directory)
}

fn reload(fixture: &RunFixture) {
    *fixture.state.inner.lock().unwrap() = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn merged_resolved_detail_is_bounded_with_counts_lookup_and_registration_identity() {
    let fixture = RunFixture::new("compaction-mixed-history");
    let session = running_owner(&fixture, "compaction-mixed-runtime");
    let (first, alias, directory) = registered_alias(&fixture, &session, "old-registered-one");
    registered_alias(&fixture, &session, "old-registered-two");
    fill_safe(&fixture, &session, TEST_RUN_TERMINAL_RUNS_PER_PROJECT + 1);
    let unknown = "test-unresolved-after-cap";
    write_run(
        &fixture.runs_dir(),
        unknown,
        full_request(&fixture.root, &test_run_timestamp()),
        Some(passed_results()),
    );
    fixture.refresh(&[]);
    let runs = fixture.summaries();
    assert_eq!(runs.len(), TEST_RUN_TERMINAL_RUNS_PER_PROJECT + 1);
    assert_eq!(
        fixture
            .summary(unknown)
            .registration
            .restart_would_void_evidence,
        None
    );
    {
        let inner = fixture.state.inner.lock().unwrap();
        let compacted = &inner.test_run_compacted[&first];
        assert!(compacted.compacted);
        assert_eq!(compacted.observations[0].execution_id, alias);
        assert!(test_run_final_unbound_valid(&compacted.observations[0]));
        assert!(compacted.summary.stages.is_empty());
        assert!(compacted.summary.command.is_none());
        assert!(!inner.test_run_registrations.contains_key(&first));
        assert!(!inner.test_run_observations.contains_key(&alias));
        let mut all: Vec<_> = runs
            .iter()
            .filter(|run| run.run_id != unknown)
            .cloned()
            .collect();
        all.extend(
            inner
                .test_run_compacted
                .values()
                .filter(|row| row.compacted)
                .map(|row| row.summary.clone()),
        );
        all.sort_by(|a, b| {
            b.ended_at
                .cmp(&a.ended_at)
                .then_with(|| a.run_id.cmp(&b.run_id))
        });
        let expected: HashSet<_> = all
            .into_iter()
            .take(TEST_RUN_TERMINAL_RUNS_PER_PROJECT)
            .map(|run| run.run_id)
            .collect();
        assert_eq!(
            expected,
            runs.iter()
                .filter(|run| run.run_id != unknown)
                .map(|run| run.run_id.clone())
                .collect()
        );
    }
    assert!(
        directory.join("results.json").is_file(),
        "compaction does not delete artifacts"
    );
    let app = app_router(fixture.state.clone());
    let (_, list): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .uri("/api/test-runs")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(list["detailHistoryLimit"], 50);
    assert_eq!(list["compactedCount"], 3);
    assert_eq!(list["omittedCount"], 3);
    assert_eq!(list["unresolvedBacklog"], 1);
    for id in [&first, &alias] {
        let (status, detail): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .uri(format!("/api/test-runs/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["run"]["historyCompacted"], true);
        assert!(
            detail["detail"]["limitations"]
                .as_str()
                .unwrap()
                .contains("Compacted retained identity")
        );
    }
    let (status, _): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .uri("/api/test-runs/never-known")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    fixture
        .state
        .register_test_run(&session, registration_request(&first, &directory))
        .unwrap();
    let original = fs::read(directory.join("request.json")).unwrap();
    let mut changed: Value = serde_json::from_slice(&original).unwrap();
    changed["detached"] = json!(true);
    fs::write(directory.join("request.json"), changed.to_string()).unwrap();
    assert!(
        fixture
            .state
            .register_test_run(&session, registration_request(&first, &directory))
            .is_err()
    );
    fixture.refresh(&[]);
    assert_eq!(
        fixture
            .summary(&first)
            .registration
            .restart_would_void_evidence,
        None
    );
    fs::write(directory.join("request.json"), original).unwrap();
    fixture.refresh(&[]);
    reload(&fixture);
    fixture.refresh(&[]);
    assert!(
        fixture.summaries().iter().all(|run| run.run_id != first),
        "rescan cannot resurrect resolved compact detail"
    );
    fixture
        .state
        .register_test_run(&session, registration_request(&first, &directory))
        .unwrap();
    assert!(
        !fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_runs
            .parsed
            .contains_key(&directory),
        "old artifacts are not fully reparsed into the index"
    );
}

#[test]
fn artifact_loss_retains_unresolved_identity_and_does_not_clear_risk() {
    for registered in [false, true] {
        for terminal in [false, true] {
            let fixture = RunFixture::new("compaction-unresolved-artifact-loss");
            let session = running_owner(&fixture, "artifact-loss-runtime");
            let run_id = "test-unresolved-artifact-loss";
            let mut request = full_request(&fixture.root, &test_run_timestamp());
            request["owner"] = json!(session);
            let directory = write_run(
                &fixture.runs_dir(),
                run_id,
                request,
                Some(if terminal {
                    passed_results()
                } else {
                    running_results(Some(7))
                }),
            );
            if registered {
                fixture
                    .state
                    .register_test_run(&session, registration_request(run_id, &directory))
                    .unwrap();
            }
            fixture.refresh(&[7]);
            if !registered && !terminal {
                let before = fixture.state.inner.lock().unwrap().test_run_compacted[run_id].clone();
                let mut progress = running_results(Some(7));
                progress["runId"] = json!(run_id);
                progress["stages"][0]["code"] = json!(0);
                fs::write(directory.join("results.json"), progress.to_string()).unwrap();
                fixture.refresh(&[7]);
                assert_eq!(
                    fixture.state.inner.lock().unwrap().test_run_compacted[run_id],
                    before,
                    "mutable stage progress does not rewrite retained identity"
                );
            }
            let mut client = fixture.summaries();
            let mut stream = fixture.state.subscribe_stream_events();
            let revision = fixture.state.full_snapshot().revision;
            fs::remove_file(directory.join("request.json")).unwrap();
            fs::remove_file(directory.join("results.json")).unwrap();
            fs::remove_dir(directory).unwrap();
            fixture.refresh(&[]);
            let retained = fixture.summary(run_id);
            assert_eq!(retained.registration.restart_would_void_evidence, None);
            assert_eq!(retained.registration.artifact_unavailable, Some(true));
            if !registered {
                assert_eq!(retained.registration.history_compacted, Some(false));
                let inner = fixture.state.inner.lock().unwrap();
                assert!(inner.test_run_compacted[run_id].summary.stages.is_empty());
                assert!(inner.test_run_compacted[run_id].summary.command.is_none());
            }
            if !terminal {
                assert_eq!(retained.state, TestRunState::Unknown);
                assert!(retained.interrupted);
            }
            let mut revisions = Vec::new();
            while let Ok(event) = stream.try_recv() {
                let SseStreamEvent::Delta(payload) = event else {
                    panic!("artifact loss must reach the live client without a snapshot reload");
                };
                match serde_json::from_str::<DeltaEvent>(&payload).unwrap() {
                    DeltaEvent::TestRunChanged { revision, run } => {
                        revisions.push(revision);
                        client.retain(|old| old.run_id != run.run_id);
                        client.push(run);
                    }
                    _ => panic!("retained unresolved artifact must never be removed"),
                }
            }
            assert_eq!(revisions, [revision + 1]);
            assert_eq!(client, fixture.summaries(), "SSE and API populations agree");
            assert_eq!(client[0].registration.restart_would_void_evidence, None);
            fixture.refresh(&[]);
            assert!(matches!(
                stream.try_recv(),
                Err(broadcast::error::TryRecvError::Empty)
            ));
            reload(&fixture);
            fixture.refresh(&[]);
            assert_eq!(
                fixture
                    .summary(run_id)
                    .registration
                    .restart_would_void_evidence,
                None
            );
            if !terminal {
                assert_eq!(fixture.summary(run_id).state, TestRunState::Unknown);
                assert!(fixture.summary(run_id).interrupted);
            }
        }
    }
}

#[test]
fn an_exact_detached_alias_never_inherits_a_final_unbound_proof() {
    let fixture = RunFixture::new("compaction-detached-alias");
    let session = running_owner(&fixture, "compaction-detached-runtime");
    let key = "detached-alias";
    let command = "node scripts/test-launcher.mjs full --detach";
    fixture.state.note_engram_command_started(
        &session,
        &EngramObservationProvenance::Ambient,
        key,
        Some(command),
        Some(&fixture.root.to_string_lossy()),
    );
    let observed = fixture.summaries()[0].run_id.clone();
    let run_id = format!("test-{}", Uuid::new_v4());
    let mut request = full_request(
        &fs::canonicalize(&fixture.root).unwrap(),
        &test_run_timestamp(),
    );
    request["owner"] = json!(session);
    request["detached"] = json!(true);
    let directory = write_run(
        &fixture.runs_dir(),
        &run_id,
        request,
        Some(running_results(Some(7))),
    );
    fixture
        .state
        .register_test_run(&session, registration_request(&run_id, &directory))
        .unwrap();
    fixture.state.note_engram_command_finished_with_capture(
        &session,
        &EngramObservationProvenance::Ambient,
        key,
        command,
        &format!("STARTED {}", test_run_display_path(&directory)),
        Some(EngramCommandExit::Code(0)),
        false,
    );
    let mut results = passed_results();
    results["runId"] = json!(run_id);
    results["ended"] = json!(test_run_timestamp());
    fs::write(directory.join("results.json"), results.to_string()).unwrap();
    fixture.refresh(&[]);
    let run = fixture.summary(&run_id);
    assert_eq!(run.state, TestRunState::Passed);
    assert_eq!(run.registration.superseded_run_ids, [observed.clone()]);
    assert_eq!(run.registration.restart_would_void_evidence, None);
    fill_safe(&fixture, &session, TEST_RUN_TERMINAL_RUNS_PER_PROJECT + 1);
    let current = fixture.summary(&run_id);
    {
        let inner = fixture.state.inner.lock().unwrap();
        let row = &inner.test_run_observations[&observed];
        assert!(row.launch_only && row.final_unbound_proof.is_none());
        assert!(!test_run_compaction_eligible(&inner, &current));
        assert!(inner.test_run_registrations.contains_key(&run_id));
        assert!(inner.test_run_compacted.get(&run_id).is_none());
        assert_eq!(test_run_observation_risk(&inner, row, true), None);
        assert_eq!(
            test_run_observation_risk(&inner, &test_run_minimal_observation(row.clone()), true),
            None
        );
    }
    reload(&fixture);
    fixture.refresh(&[]);
    let run = fixture.summary(&run_id);
    assert_eq!(run.state, TestRunState::Passed);
    assert_eq!(run.registration.restart_would_void_evidence, None);
    assert_eq!(run.registration.superseded_run_ids, [observed]);
}

#[test]
fn an_unsafe_alias_preserves_the_original_registration_and_every_association() {
    let fixture = RunFixture::new("compaction-unsafe-alias");
    let session = running_owner(&fixture, "compaction-unsafe-runtime");
    let (id, alias, _) = registered_alias(&fixture, &session, "unsafe-registered");
    let (evidence, evidence_id, _, _, _) = evidence_fixture("compaction-unsafe-source");
    let association = evidence.state.inner.lock().unwrap().test_run_observations[&evidence_id]
        .evidence[0]
        .clone();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let row = inner.test_run_observations.get_mut(&alias).unwrap();
        row.work_binding = Some(association.binding.clone());
        row.validated_unbound = false;
        row.final_unbound_proof = None;
        row.evidence.push(association.clone());
        let row = row.clone();
        fixture
            .state
            .persist_test_command_observation_locked(&mut inner, row);
    }
    fill_safe(&fixture, &session, TEST_RUN_TERMINAL_RUNS_PER_PROJECT + 1);
    let summary = fixture.summary(&id);
    assert_eq!(summary.registration.restart_would_void_evidence, Some(true));
    assert_eq!(summary.registration.superseded_run_ids, [alias.clone()]);
    let inner = fixture.state.inner.lock().unwrap();
    assert!(inner.test_run_registrations.contains_key(&id));
    assert_eq!(inner.test_run_observations[&alias].evidence, [association]);
    assert!(
        !inner
            .test_run_compacted
            .get(&id)
            .is_some_and(|row| row.compacted)
    );
}

#[test]
fn bound_compact_proof_needs_current_canonical_revalidation_after_boot() {
    let (fixture, id, store, binding, report) = evidence_fixture("compaction-bound-revalidation");
    fixture
        .state
        .settle_test_run_verification_receipt(&report, &exact_receipt(), &store, &binding);
    let read = serde_json::from_value(completed_lifecycle(&fixture, &store, &binding)).unwrap();
    fixture
        .state
        .revalidate_test_run_lifecycle(&store, &binding, &read);
    let session = fixture.state.inner.lock().unwrap().test_run_observations[&id]
        .summary
        .owner_session_id
        .clone()
        .unwrap();
    fill_safe(&fixture, &session, TEST_RUN_TERMINAL_RUNS_PER_PROJECT + 1);
    {
        let inner = fixture.state.inner.lock().unwrap();
        let proof = &inner.test_run_compacted[&id].observations[0].evidence[0];
        assert_eq!(proof.binding, binding);
        assert_eq!(proof.verification_record_id, Some("e".repeat(64)));
        assert!(inner.test_run_compacted[&id].compacted);
    }
    reload(&fixture);
    assert_eq!(
        fixture
            .summary(&id)
            .registration
            .restart_would_void_evidence,
        None
    );
    assert_eq!(fixture.summary(&id).state, TestRunState::Passed);
    fixture
        .state
        .revalidate_test_run_lifecycle(&store, &binding, &read);
    assert!(!fixture.summaries().iter().any(|run| run.run_id == id));
    let inner = fixture.state.inner.lock().unwrap();
    assert_eq!(
        test_run_compacted_summary(&inner, &inner.test_run_compacted[&id])
            .registration
            .restart_would_void_evidence,
        Some(false)
    );
}

/// A revalidation that makes an old run eligible selects nothing under its
/// own hold; the existing idle compaction step compacts it at the new cut.
#[test]
fn revalidated_eligible_run_is_compacted_by_the_idle_step_not_inline() {
    let (fixture, id, store, binding, report) = evidence_fixture("compaction-revalidation-deferred");
    fixture
        .state
        .settle_test_run_verification_receipt(&report, &exact_receipt(), &store, &binding);
    let session = fixture.state.inner.lock().unwrap().test_run_observations[&id]
        .summary
        .owner_session_id
        .clone()
        .unwrap();
    fill_safe(&fixture, &session, TEST_RUN_TERMINAL_RUNS_PER_PROJECT + 1);
    let compacted = |fixture: &RunFixture| {
        fixture.state.inner.lock().unwrap().test_run_compacted.get(&id).is_some_and(|row| row.compacted)
    };
    // Its lifecycle is not yet completed, so the run keeps its detail.
    fixture.state.step_test_run_compaction();
    assert!(!compacted(&fixture), "an unrevalidated run is not eligible");
    let read = serde_json::from_value(completed_lifecycle(&fixture, &store, &binding)).unwrap();
    fixture
        .state
        .revalidate_test_run_lifecycle(&store, &binding, &read);
    {
        let inner = fixture.state.inner.lock().unwrap();
        assert!(inner.test_run_observations.contains_key(&id), "the revalidating hold selects nothing");
        assert_eq!(inner.test_runs.compaction_examined_revision, None, "the next idle step must select");
    }
    assert!(!compacted(&fixture));
    fixture.state.step_test_run_compaction();
    assert!(compacted(&fixture), "the idle step compacts the now-eligible run");
    let inner = fixture.state.inner.lock().unwrap();
    let proof = &inner.test_run_compacted[&id].observations[0].evidence[0];
    assert_eq!(proof.binding, binding);
    assert_eq!(proof.verification_record_id, Some("e".repeat(64)));
}

#[test]
fn failed_compaction_persistence_retains_all_preexisting_detail_and_identity() {
    let mut fixture = RunFixture::new("compaction-persistence-failure");
    let session = running_owner(&fixture, "compaction-failure-runtime");
    fill_safe(&fixture, &session, TEST_RUN_TERMINAL_RUNS_PER_PROJECT + 2);
    // Restore the already sealed execution/proof records as full in-memory
    // rows to exercise the failed compaction boundary without minting proof.
    let ids = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let compacted = std::mem::take(&mut inner.test_run_compacted);
        for row in compacted.into_values() {
            for observation in row.observations {
                inner
                    .test_runs
                    .observations_durable
                    .insert(observation.execution_id.clone());
                inner
                    .test_run_observations
                    .insert(observation.execution_id.clone(), observation);
            }
        }
        inner.test_runs.compactions_durable.clear();
        // Manual restoration changes selection inputs without a commit.
        // Force the worker to attempt the failing write at this boundary.
        inner.test_runs.compaction_examined_revision = None;
        inner
            .test_run_observations
            .keys()
            .cloned()
            .collect::<HashSet<_>>()
    };
    let invalid = fixture.root.join("compaction-state-directory");
    fs::create_dir(&invalid).unwrap();
    fixture.state.persistence_path = Arc::new(invalid);
    fixture.state.step_test_run_compaction();
    let inner = fixture.state.inner.lock().unwrap();
    assert_eq!(
        inner.test_runs.compaction_examined_revision,
        None,
        "failed persistence must leave compaction eligible to retry"
    );
    assert_eq!(
        inner
            .test_run_observations
            .keys()
            .cloned()
            .collect::<HashSet<_>>(),
        ids
    );
    assert!(inner.test_run_compacted.is_empty());
    assert_eq!(
        test_run_summaries_locked(&inner).len(),
        TEST_RUN_TERMINAL_RUNS_PER_PROJECT + 2,
        "failed persistence keeps detail rather than pretending it was safely omitted"
    );
}
