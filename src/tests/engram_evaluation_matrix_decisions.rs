//! Decision-time controls independent of closing reports. Every run and
//! evaluation below uses the ordinary registered/HTTP/CLI production paths.
use super::*;

macro_rules! live_row {
    ($name:ident, $body:block) => {
        #[test]
        #[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
        fn $name() $body
    };
}

fn changed_after_evaluation(commit_change: bool) {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("Check and evaluate; the next edit emits no closing report.");
    let check = matrix.check(false);
    matrix.close();
    matrix.settle();
    let (evaluation_id, original_evaluation) = matrix.evaluate(&check, || {});
    let originals = matrix.stored_objects();
    let reports = matrix
        .fixture
        .live
        .transport
        .requests_for("execution_observe");
    matrix.edit("real change after the recorded independent evaluation\n");
    if commit_change {
        matrix.commit();
    }
    // No holder turn is opened or closed after this edit. Even the old
    // declared fingerprint must not override Engram's own current-root read.
    matrix.refuse(&check, || {});
    matrix.refuse_done(&check, true);
    assert_eq!(
        matrix
            .fixture
            .live
            .transport
            .requests_for("execution_observe"),
        reports
    );
    assert_eq!(matrix.fixture.object(&evaluation_id), original_evaluation);
    matrix.assert_objects_unchanged(originals);
}

live_row!(dirty_edit_without_turn_report_before_done_is_refused, {
    changed_after_evaluation(false);
});

live_row!(new_commit_without_turn_report_before_done_is_refused, {
    changed_after_evaluation(true);
});

live_row!(
    dirty_run_cannot_be_promoted_by_cleaning_the_root_afterward,
    {
        let matrix = Matrix::new(Citation::UnboundNote, false);
        matrix.begin("A dirty run remains ineligible after the root is restored.");
        let original = fs::read(matrix.fixture.root.join("README.md")).unwrap();
        matrix.edit("dirty input at launch\n");
        let (events, mut dirty) = matrix.launch(false);
        assert_ne!(dirty.exit, 0);
        assert_ne!(dirty.results["input"]["eligible"], true);
        matrix.receive_check(events, &mut dirty, true);
        assert!(dirty.records.is_empty());
        let original_result = fs::read(dirty.directory.join("results.json")).unwrap();
        fs::write(matrix.fixture.root.join("README.md"), original).unwrap();
        assert!(
            run_git_test_command_output(&matrix.fixture.root, &["status", "--porcelain"])
                .is_empty()
        );
        // The note is a real unbound citation, not an invented verification for
        // the failed preflight. No passing run exists to support this judgment.
        matrix.refuse(&dirty, || {});
        let SourceRevision::Taken { revision, .. } =
            source_revision(&matrix.fixture.root, true).unwrap()
        else {
            panic!("the dirty run's root has really been restored to a clean commit")
        };
        let output = matrix.done_output_revision(&revision);
        matrix.assert_completion_refusal(&output, false);
        assert_eq!(
            fs::read(dirty.directory.join("results.json")).unwrap(),
            original_result
        );
        assert!(
            matrix
                .fixture
                .live
                .transport
                .requests_for("verification_record")
                .is_empty()
        );
        matrix.close();
        matrix.settle();
    }
);

fn install_failure_switch(matrix: &Matrix) {
    // This ignored runtime input changes the actual test outcome while its
    // tracked code and commit remain the same. Neither result is fabricated.
    fs::write(
        matrix.fixture.root.join("src/lib.rs"),
        r#"#[test]
fn checked_content() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(!root.join("target/matrix-fail").exists(), "real later failure");
    assert!(!std::fs::read_to_string(root.join("README.md")).unwrap().trim().is_empty());
}
"#,
    )
    .unwrap();
    fs::write(
        matrix.fixture.root.join("launcher-check.test.mjs"),
        r#"import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import test from "node:test";
test("checked content", () => {
  assert.equal(existsSync(new URL("./target/matrix-fail", import.meta.url)), false);
  assert.match(readFileSync(new URL("./README.md", import.meta.url), "utf8"), /\S/);
});
"#,
    )
    .unwrap();
    run_git_test_command(
        &matrix.fixture.root,
        &["add", "--", "src/lib.rs", "launcher-check.test.mjs"],
    );
    run_git_test_command(
        &matrix.fixture.root,
        &["commit", "--quiet", "-m", "fixture native failure switch"],
    );
}

fn assert_clean_failed_check(matrix: &Matrix, passed: &ActualCheck, failed: &ActualCheck) {
    assert_ne!(
        failed.exit, 0,
        "the native test must really fail: {}",
        failed.output
    );
    assert_eq!(failed.results["state"], "failed");
    assert_ne!(failed.results["interrupted"], true);
    let input = TestRunCommittedInput::read(&failed.results).unwrap();
    assert!(input.clean_before);
    assert_eq!(input.outcome, Some(TestRunCommittedInputOutcome::Clean));
    // Eligibility grants passed credit. A clean native FAIL must remain
    // ineligible while its real failed verification supersedes the old PASS.
    assert_eq!(input.eligible, Some(false));
    assert_eq!(
        passed.results["input"]["commit"],
        failed.results["input"]["commit"]
    );
    let after = input.after.as_ref().unwrap();
    assert!(after.clean);
    assert_eq!(Some(after.commit.as_str()), input.commit.as_deref());
    assert!(
        run_git_test_command_output(&matrix.fixture.root, &["status", "--porcelain"]).is_empty()
    );
    assert!(
        !failed.records.is_empty(),
        "the newer failure is durably registered"
    );
    for record in &failed.records {
        assert_eq!(matrix.fixture.object(record)["result"], "failed");
    }
}

fn pass_then_fail() -> (Matrix, ActualCheck, ActualCheck) {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    install_failure_switch(&matrix);
    matrix.begin("Run the same required check twice at the same clean commit.");
    let passed = matrix.check(false);
    fs::create_dir_all(matrix.fixture.root.join("target")).unwrap();
    fs::write(matrix.fixture.root.join("target/matrix-fail"), "fail").unwrap();
    let (events, mut failed) = matrix.launch(false);
    assert_ne!(
        failed.exit, 0,
        "the native test must really fail: {}",
        failed.output
    );
    matrix.receive_check(events, &mut failed, true);
    assert_clean_failed_check(&matrix, &passed, &failed);
    (matrix, passed, failed)
}

live_row!(
    newer_failed_check_at_same_commit_blocks_early_selection_of_old_pass,
    {
        let (matrix, passed, _failed) = pass_then_fail();
        let originals = matrix.stored_objects();
        let old_result = fs::read(passed.directory.join("results.json")).unwrap();
        match matrix.request(&passed) {
            Err(body) => {
                let text = body.to_string().to_ascii_lowercase();
                assert!(
                    text.contains("eligible") || text.contains("fail"),
                    "refuse the newer failed check, not unrelated setup: {body:#}"
                );
            }
            Ok(_) => {
                panic!("early selection must not skip the newer failure and reuse an old PASS")
            }
        }
        assert_eq!(
            fs::read(passed.directory.join("results.json")).unwrap(),
            old_result
        );
        matrix.assert_objects_unchanged(originals);
        matrix.close();
        matrix.settle();
    }
);

live_row!(
    newer_failed_check_at_same_commit_blocks_done_after_recorded_evaluation,
    {
        let matrix = Matrix::new(Citation::BoundVerification, false);
        install_failure_switch(&matrix);
        matrix.begin("An evaluation of PASS cannot hide the same check's later FAIL.");
        let passed = matrix.check(false);
        matrix.close();
        matrix.settle();
        let (_evaluation_id, _evaluation) = matrix.evaluate(&passed, || {});
        let originals = matrix.stored_objects();
        matrix.begin("The same native check fails on the same tracked input.");
        fs::write(matrix.fixture.root.join("target/matrix-fail"), "fail").unwrap();
        let (events, mut failed) = matrix.launch(false);
        assert_ne!(failed.exit, 0);
        matrix.receive_check(events, &mut failed, true);
        assert_clean_failed_check(&matrix, &passed, &failed);
        let output = matrix.done_output(&passed);
        assert!(
            !output.status.success(),
            "newer FAIL must block done: {output:?}"
        );
        let body: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(body["operation"], "done");
        let text = body.to_string();
        if text.contains("result_not_passed") {
            assert!(
                failed.records.iter().any(|record| text.contains(record)),
                "the actual later failed verification must decide: {body:#}"
            );
        } else {
            // A newly registered failure can first stale the older
            // evaluation. That typed refusal also blocks its use at done.
            matrix.assert_completion_refusal(&output, true);
        }
        let core = matrix
            .fixture
            .work(&matrix.holder, &["core", "inspect", &matrix.work, "--json"]);
        assert!(core["run"]["completion_seal"].is_null(), "{core:#}");
        matrix.assert_objects_unchanged(originals);
        matrix.close();
        matrix.settle();
    }
);

fn uncertain_opening(later_edit: bool) {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    let retained = {
        let mut inner = matrix.fixture.live.state.inner.lock().unwrap();
        let index = inner.find_session_index(&matrix.holder).unwrap();
        let retained = inner.sessions[index].engram.named_root.clone().unwrap();
        let Some(EngramNamedRootState::Bound { generation, .. }) =
            inner.sessions[index].engram.named_root.as_mut()
        else {
            panic!("the fixture starts with a real named root")
        };
        *generation += 1;
        retained
    };
    matrix.begin("A native check in a turn with a genuinely unconfirmed opening.");
    assert!(matrix.fixture.record(&matrix.holder, |row| matches!(
        row.engram.observed_opening().unwrap().provenance,
        EngramRootCapture::Unconfirmed
    )));
    {
        let mut inner = matrix.fixture.live.state.inner.lock().unwrap();
        let index = inner.find_session_index(&matrix.holder).unwrap();
        inner.sessions[index].engram.named_root = Some(retained);
    }
    let check = matrix.check(false);
    assert!(
        matrix.fixture.record(&matrix.holder, |row| matches!(
            row.engram.observed_opening().unwrap().provenance,
            EngramRootCapture::Unconfirmed
        )),
        "native registration does not upgrade the captured opening"
    );
    matrix.close();
    matrix.settle();
    let originals = matrix.stored_objects();
    if later_edit {
        matrix.edit("a real retained change after the uncertain-opening check\n");
        matrix.commit();
    }
    matrix.begin("A later ordinary close must not decide registered-check currency.");
    matrix.close();
    matrix.settle();
    if later_edit {
        matrix.refuse(&check, || {});
        matrix.refuse_done(&check, false);
    } else {
        matrix.accept(&check, || {});
    }
    matrix.assert_objects_unchanged(originals);
}

live_row!(
    unconfirmed_opening_does_not_veto_a_current_registered_pass,
    {
        uncertain_opening(false);
    }
);

live_row!(
    unconfirmed_opening_cannot_cover_a_real_new_commit_after_check,
    {
        uncertain_opening(true);
    }
);

fn settle_claim_transition(
    matrix: &Matrix,
    original: &EngramControlWorkBinding,
) -> Vec<EngramRuntimeClosingJob> {
    // A changed claim fence can retire an old closing scope. Retain that
    // diagnostic instead of demanding a newly recorded report for the check.
    matrix
        .fixture
        .live
        .state
        .start_runtime_observation_worker(&matrix.holder);
    let guard = phase_sync::PollGuard::new();
    loop {
        let settled = matrix.fixture.record(&matrix.holder, |record| {
            record
                .engram
                .unadmitted_closing_jobs
                .iter()
                .all(|job| job.terminal_failure.is_some())
                && record
                    .engram
                    .unadmitted_reports
                    .iter()
                    .all(|report| report.is_settled())
                && !record.engram.unadmitted_worker_running
        });
        if settled {
            break;
        }
        guard.wait("claim transition reaches recorded reports or terminal custody");
    }
    matrix.fixture.record(&matrix.holder, |record| {
        for job in &record.engram.unadmitted_closing_jobs {
            assert_eq!(job.scope.binding.as_ref(), Some(original));
            assert!(
                job.terminal_failure
                    .as_ref()
                    .unwrap()
                    .contains("scope changed")
            );
        }
        for report in &record.engram.unadmitted_reports {
            for envelope in &report.envelopes {
                assert!(matches!(
                    envelope.delivery,
                    EngramRuntimeObservationDelivery::Recorded { .. }
                ));
            }
        }
        record.engram.unadmitted_closing_jobs.clone()
    })
}

live_row!(
    released_claim_requires_new_named_generation_and_fresh_check,
    {
        let matrix = Matrix::new(Citation::BoundVerification, false);
        matrix.begin("Check before explicitly releasing this claim.");
        let check = matrix.check(false);
        matrix.close();
        matrix.settle();
        let original = matrix.fixture.claim_binding(&matrix.holder);
        let root = matrix.fixture.canonical_read(&matrix.holder, &original);
        let EngramNamedRootState::Bound {
            workspace_id,
            generation,
            ..
        } = root.named_root
        else {
            panic!("the check has a canonical named generation")
        };
        let originals = matrix.stored_objects();
        matrix.fixture.work(
            &matrix.holder,
            &[
                "update",
                &matrix.work,
                "--release",
                "--reason",
                "Release checked work",
                "--json",
            ],
        );
        matrix.fixture.work(
            &matrix.holder,
            &[
                "claim",
                &matrix.work,
                "--recover",
                "Recover explicitly released work",
                "--json",
            ],
        );
        let recovered = matrix.fixture.claim_binding(&matrix.holder);
        assert_eq!(recovered.run_id, original.run_id);
        assert!(recovered.claim_fence > original.claim_fence);
        assert!(
            matches!(matrix.fixture.canonical_read(&matrix.holder, &recovered).named_root,
        EngramNamedRootState::UnboundByRelease { last_generation, .. } if last_generation == generation)
        );
        matrix.begin("A released generation cannot reuse its old check.");
        matrix.close();
        let retained_jobs = settle_claim_transition(&matrix, &original);
        matrix.refuse(&check, || {});

        matrix
            .fixture
            .live
            .state
            .name_engram_source_root(
                &matrix.holder,
                EngramSourceRootRequest {
                    work: matrix.work.clone(),
                    path: Some(Some(matrix.fixture.root.to_string_lossy().into_owned())),
                },
            )
            .unwrap();
        let named = matrix.fixture.canonical_read(&matrix.holder, &recovered);
        assert!(
            matches!(named.named_root, EngramNamedRootState::Bound { workspace_id: ref next_root, generation: next, .. }
        if next_root == &workspace_id && next > generation)
        );
        // Equal commit bytes do not transfer a check into a different generation.
        matrix.assert_checked_source(&check);
        matrix.refuse(&check, || {});
        matrix.assert_objects_unchanged(originals.clone());
        matrix.begin("Check the same commit in its newly named generation.");
        let fresh = matrix.check(false);
        assert_eq!(
            check.results["input"]["commit"],
            fresh.results["input"]["commit"]
        );
        assert_ne!(check.records, fresh.records);
        matrix.close();
        assert_eq!(settle_claim_transition(&matrix, &original), retained_jobs);
        matrix.accept(&fresh, || {});
        matrix.assert_objects_unchanged(originals);
    }
);

live_row!(
    handoff_return_same_generation_can_reuse_original_registered_check,
    {
        let matrix = Matrix::new(Citation::BoundVerification, false);
        matrix.begin("Check once before handing off unchanged work.");
        let check = matrix.check(false);
        matrix.close();
        matrix.settle();
        let original = matrix.fixture.claim_binding(&matrix.holder);
        let root = matrix
            .fixture
            .canonical_read(&matrix.holder, &original)
            .named_root;
        assert!(matches!(root, EngramNamedRootState::Bound { .. }));
        let originals = matrix.stored_objects();
        let peer = create_test_project_session(
            &matrix.fixture.live.state,
            Agent::Codex,
            &matrix.fixture.project_id,
            &matrix.fixture.root,
        );
        let _peer = LiveRootCleanup {
            transport: matrix.fixture.live.transport.clone(),
            session_id: peer.clone(),
        };
        for (from, to) in [(&matrix.holder, &peer), (&peer, &matrix.holder)] {
            matrix.fixture.work(
                from,
                &[
                    "handoff",
                    &matrix.work,
                    "--to",
                    to,
                    "--summary",
                    "Return the unchanged checked input without releasing its generation",
                    "--json",
                ],
            );
            matrix
                .fixture
                .work(to, &["handoff", &matrix.work, "--accept", "--json"]);
        }
        let returned = matrix.fixture.claim_binding(&matrix.holder);
        assert_eq!(returned.run_id, original.run_id);
        assert_eq!(returned.claim_id, original.claim_id);
        assert!(returned.claim_fence > original.claim_fence);
        // Read the producer's actual generation; no local root metadata is copied.
        assert_eq!(
            matrix
                .fixture
                .canonical_read(&matrix.holder, &returned)
                .named_root,
            root
        );
        matrix.begin("Continue after handoff without renaming or checking again.");
        matrix.close();
        let retained_jobs = settle_claim_transition(&matrix, &original);
        matrix.accept(&check, || {});
        assert_eq!(
            matrix.fixture.record(&matrix.holder, |record| record
                .engram
                .unadmitted_closing_jobs
                .clone()),
            retained_jobs
        );
        matrix.assert_objects_unchanged(originals);
    }
);

live_row!(
    root_only_measured_edit_then_revert_accepts_without_erasing_history,
    {
        let matrix = Matrix::new(Citation::BoundVerification, false);
        matrix.begin("Check before a root-only observation of a real edit.");
        let check = matrix.check(false);
        matrix.close();
        matrix.settle();
        let binding = matrix.fixture.claim_binding(&matrix.holder);
        let root = matrix.fixture.canonical_read(&matrix.holder, &binding);
        let EngramNamedRootState::Bound {
            workspace_id,
            generation,
            ..
        } = root.named_root.clone()
        else {
            panic!("the producer retains the real named root")
        };
        let measure = || EngramMeasuredSighting {
            source_basis: EngramExecutionSourceBasis {
                workspace_id: workspace_id.clone(),
                source_revision: content_revision(&matrix.fixture.root).unwrap().1,
                source_root_generation: Some(generation),
                source_root_state: Some(EngramSourceRootState::Named),
            },
            observed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        };
        let baseline = measure();
        let original = fs::read(matrix.fixture.root.join("README.md")).unwrap();
        matrix.edit("real root-only interval edit\n");
        let target = matrix.fixture.target(&matrix.holder);
        let policy = target
            .adapter
            .read_observation_policy(
                &target.connection,
                &matrix.fixture.store(),
                target.settings.call_timeout(),
            )
            .unwrap();
        let facts = EngramMeasuredSourceFacts {
            id: Uuid::new_v4().to_string(),
            binding,
            baseline: Some(baseline),
            sighting: measure(),
            root_basis: json!({"capture_run_cut":root.read_cut.position,
            "latest_event":root.latest_event.as_ref().map(|event| &event.event),
            "state":root.named_root}),
        };
        let observation = EngramInterTurnObservation::from_source_change(&facts, policy).unwrap();
        let request = EngramControlRequest::ExecutionObserve {
            observation: observation.clone(),
        };
        let result = parse_engram_result(
            target
                .adapter
                .request(&target.connection, &request, target.settings.call_timeout())
                .unwrap(),
        )
        .unwrap();
        let receipt = EngramSourceObservationReceipt::from_accounted_result(
            result,
            &observation,
            &matrix.holder,
        )
        .unwrap();
        assert!(matches!(
            receipt.accounting,
            EngramObservationAccounting::SourceChange { .. }
        ));
        let stored = matrix.fixture.object(&receipt.observation);
        fs::write(matrix.fixture.root.join("README.md"), original).unwrap();
        matrix.accept(&check, || {});
        assert_eq!(matrix.fixture.object(&receipt.observation), stored);
    }
);
