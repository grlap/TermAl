// Each schedule has its own test/result and disposable producer store. A RED
// in one row must not hide whether the other rows reached the real boundary.
use super::*;

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn delayed_predecessor_three_turn_check_attribution() {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    let turn_ref = || {
        matrix.fixture.record(&matrix.holder, |row| {
            row.engram
                .unadmitted_active_turn
                .as_ref()
                .unwrap()
                .host_turn_ref
                .clone()
        })
    };
    matrix.begin("A closes, but its real source-sighting RPC cannot reach the producer yet.");
    let a = turn_ref();
    let gate = matrix.transport.arm();
    matrix.close();
    gate.entered();
    matrix.begin("B finishes a real registered check on unchanged X, then closes.");
    let b = turn_ref();
    let check = matrix.check(false);
    let original = fs::read(check.directory.join("results.json")).unwrap();
    matrix.close();
    assert!(
        matrix.fixture.record(&matrix.holder, |row| row
            .engram
            .unadmitted_closing_jobs
            .iter()
            .any(|job| job.trace.host_turn_ref == b)),
        "B's closing custody exists while the same worker is blocked delivering A"
    );
    matrix.begin("C edits and commits Y before A is released and B can be measured.");
    let c = turn_ref();
    matrix.edit("real later-turn C edit after B's finished check\n");
    matrix.commit();
    gate.release();
    matrix.settle();
    matrix.close();
    matrix.settle();
    // Record actual attribution, including any mistaken B change/C quiet
    // classification. The diagnostic must not choose a source timestamp or
    // relabel the producer's result to match the intended interpretation.
    let observations = matrix.fixture.work(
        &matrix.holder,
        &["show", &matrix.work, "--observations", "--json"],
    );
    eprintln!(
        "MATRIX_THREE_TURN_ATTRIBUTION {:#}",
        json!({
            "turnRefs":{"A":a,"B":b,"C":c},
            "checkRun":check.run_id,
            "checkState":check.results["state"],
            "requestCount":matrix.fixture.live.transport.requests_for("execution_observe").len(),
            "observationsRead":!observations.is_null()
        })
    );
    matrix.refuse(&check, || {});
    let current = matrix.source_id_now();
    assert!(current.changes.is_empty(), "the probe measures committed Y");
    assert_ne!(
        current.commit,
        check.results["input"]["commit"].as_str().unwrap()
    );
    let attempted = matrix.done_output_source(&current);
    eprintln!(
        "MATRIX_THREE_TURN_DONE status={} stdout_bytes={} stderr_bytes={}",
        attempted.status,
        attempted.stdout.len(),
        attempted.stderr.len()
    );
    assert!(
        !attempted.status.success(),
        "C's real unchecked edit cannot complete on B's check"
    );
    matrix.assert_completion_refusal(&attempted, false);
    let core = matrix
        .fixture
        .work(&matrix.holder, &["core", "inspect", &matrix.work, "--json"]);
    assert_ne!(core["run"]["state"], "completed", "<core withheld>");
    assert!(
        core["run"]["completion_seal"].is_null(),
        "no seal on the unchecked input: <core withheld>"
    );
    assert!(fs::read(check.directory.join("results.json")).unwrap() == original,
        "the complete original result is unchanged");
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn check_a_edit_commit_check_b_in_one_turn_accepts_last_input() {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("Run A, edit and commit, then run B without ending this turn.");
    let first = matrix.check(false);
    let original_results = fs::read(first.directory.join("results.json")).unwrap();
    let original_records: Vec<_> = first
        .records
        .iter()
        .map(|id| matrix.fixture.object(id))
        .collect();
    matrix.edit("intended second input in the same turn\n");
    matrix.commit();
    let last = matrix.check(false);
    assert_ne!(
        first.results["input"]["commit"],
        last.results["input"]["commit"]
    );
    matrix.close();
    matrix.settle();
    matrix.accept(&last, || {});
    assert!(fs::read(first.directory.join("results.json")).unwrap() == original_results,
        "later checks preserve every original result byte");
    for (id, original) in first.records.iter().zip(original_records) {
        assert!(
            matrix.fixture.object(id) == original,
            "later checks never rewrite original records"
        );
    }
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn check_a_edit_check_b_then_real_edit_stays_refused() {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("Run A and B, then keep a real edit after B.");
    let first = matrix.check(false);
    matrix.edit("input B\n");
    matrix.commit();
    let last = matrix.check(false);
    assert_ne!(
        first.results["input"]["commit"],
        last.results["input"]["commit"]
    );
    matrix.edit("real change after the final check\n");
    matrix.close();
    matrix.settle();
    matrix.refuse(&last, || {});
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn late_close_a_after_successor_edit_commit_check_b_accepts_b() {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("A checks its input and closes with its source sighting held at the RPC.");
    let first = matrix.check(false);
    let original_results = fs::read(first.directory.join("results.json")).unwrap();
    let gate = matrix.transport.arm();
    matrix.close();
    gate.entered();
    matrix.begin("B edits, commits and checks before A's immutable sighting is delivered.");
    matrix.edit("new intended successor input\n");
    matrix.commit();
    let last = matrix.check(false);
    assert_ne!(
        first.results["input"]["commit"],
        last.results["input"]["commit"]
    );
    matrix.close();
    matrix.accept_after_done(&last, || {}, || gate.release());
    assert!(fs::read(first.directory.join("results.json")).unwrap() == original_results,
        "the complete earlier result is unchanged");
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn late_close_a_cannot_cover_real_edit_after_successor_check_b() {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("A checks and its closing sighting is delayed.");
    let first = matrix.check(false);
    let gate = matrix.transport.arm();
    matrix.close();
    gate.entered();
    matrix.begin("B edits, commits and checks while A's sighting waits.");
    matrix.edit("input checked by B\n");
    matrix.commit();
    let last = matrix.check(false);
    assert_ne!(
        first.results["input"]["commit"],
        last.results["input"]["commit"]
    );
    matrix.edit("real unchecked change after B\n");
    matrix.close();
    matrix.refuse(&last, || {});
    gate.release();
    matrix.settle();
}

// This row intentionally reaches done even when the host already refuses the
// request. A producer-only judgment can be recorded while completion is still
// pending; neither case may seal an input changed after the actual check.
#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn note_citing_judgment_after_measured_change_cannot_seal() {
    let matrix = Matrix::new(Citation::UnboundNote, false);
    matrix.begin("Run an actual clean registered check.");
    let check = matrix.check(false);
    matrix.close();
    matrix.settle();
    matrix.begin("Measure and keep a real source change before the evaluator's evidence cut.");
    matrix.edit("real intended change with no later check\n");
    matrix.commit();
    matrix.close();
    matrix.settle();
    let evaluation_recorded = match matrix.request(&check) {
        Err(body) => {
            Matrix::assert_source_refusal(&body);
            eprintln!("MATRIX_CHANGED_NOTE_PENDING_AT_HOST <body withheld>");
            false
        }
        Ok((child, _cleanup)) => {
            let (status, body) = matrix.submit(&child, &check);
            eprintln!(
                "MATRIX_CHANGED_NOTE_EVALUATION status={} body=<body withheld>",
                status.as_u16()
            );
            if !status.is_success() {
                Matrix::assert_source_refusal(&body);
            }
            status.is_success()
        }
    };
    let current = matrix.source_id_now();
    assert!(current.changes.is_empty(), "negative completion measures the clean new input");
    let output = matrix.done_output_source(&current);
    eprintln!(
        "MATRIX_CHANGED_NOTE_DONE exit={:?} <body withheld>",
        output.status.code()
    );
    let core = matrix
        .fixture
        .work(&matrix.holder, &["core", "inspect", &matrix.work, "--json"]);
    assert!(
        core["run"]["completion_seal"].is_null(),
        "REAL HOLE: a note-citing judgment sealed unchecked changed input: <core withheld>; <body withheld>"
    );
    assert_eq!(
        core["run"]["state"], "active",
        "evaluation stays pending completion: <core withheld>"
    );
    matrix.assert_completion_refusal(&output, evaluation_recorded);
}

macro_rules! live_row {
    ($name:ident, $body:block) => {
        #[test]
        #[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
        fn $name() $body
    };
}

live_row!(first_named_close_bound_check_accepts_and_seals, {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("Run the first check after naming, without a setup close.");
    let check = matrix.check(false);
    matrix.close();
    matrix.settle();
    matrix.accept(&check, || {});
});

live_row!(first_named_close_unbound_verification_accepts_and_seals, {
    let matrix = Matrix::new(Citation::UnboundVerification, false);
    matrix.begin("A correct check also counts on an unbound criterion.");
    let check = matrix.check(false);
    matrix.close();
    matrix.settle();
    matrix.accept(&check, || {});
});

live_row!(first_named_close_unbound_note_accepts_checked_commit, {
    let matrix = Matrix::new(Citation::UnboundNote, false);
    matrix.begin("The evaluator cites a holder note, with a real Git check in the cut.");
    let check = matrix.check(false);
    matrix.close();
    matrix.settle();
    matrix.accept(&check, || {});
});

live_row!(codex_one_call_check_first_close_accepts, {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("Codex reports a plain one-call line with its absolute directory.");
    let check = matrix.check(true);
    assert!(
        !matrix.fixture.record(&matrix.holder, |row| row
            .engram
            .active_turn_checks
            .is_empty()),
        "the production parser recognizes this explicit one-call directory"
    );
    matrix.close();
    matrix.settle();
    matrix.accept(&check, || {});
});

live_row!(
    codex_wrapped_one_call_without_reported_cwd_first_close_accepts,
    {
        let matrix = Matrix::new(Citation::BoundVerification, false);
        matrix.begin(
            "The runtime reports the retained PowerShell wrapper shape without inventing a cwd.",
        );
        let mut events =
            matrix
                .provider
                .events(&matrix.fixture, &matrix.holder, "wrapped-check", false);
        events.powershell_one_call_without_reported_cwd();
        events.start();
        let output = isolated_launcher(&matrix.fixture.root, &matrix.holder, &matrix.service)
            .output()
            .expect("actual registered native check runs");
        let mut check = matrix.actual_check_output(output);
        assert_eq!(check.exit, 0, "{}", check.output);
        matrix.receive_unplaced_check(events, &mut check);
        let received = matrix.persisted_verification(&check);
        assert!(
            matrix.fixture.record(&matrix.holder, |row| row
                .engram
                .active_turn_checks
                .is_empty()),
            "the wrapped runtime command really supplies no placed ordinary check"
        );
        matrix.close();
        matrix.settle();
        matrix.accept(&check, || {});
        assert_eq!(matrix.persisted_verification(&check), received);
    }
);

live_row!(
    registered_check_without_runtime_command_first_close_accepts,
    {
        let matrix = Matrix::new(Citation::BoundVerification, false);
        matrix.begin("The registered launcher runs without a placeable runtime command event.");
        // Registration and execution remain real. This row explicitly models
        // absent provider command telemetry, not an invented wrapper refusal.
        let output = isolated_launcher(&matrix.fixture.root, &matrix.holder, &matrix.service)
            .output()
            .expect("actual launcher runs without runtime command telemetry");
        let mut check = matrix.actual_check_output(output);
        assert_eq!(check.exit, 0, "{}", check.output);
        matrix.read_check_receipt(&mut check, true);
        let received = matrix.persisted_verification(&check);
        assert!(
            matrix.fixture.record(&matrix.holder, |row| row
                .engram
                .active_turn_checks
                .is_empty()),
            "no runtime command/check was injected for the actual registered run"
        );
        matrix.close();
        matrix.settle();
        matrix.accept(&check, || {});
        assert_eq!(matrix.persisted_verification(&check), received);
    }
);

live_row!(claude_one_call_check_first_close_accepts, {
    let matrix = Matrix::new(Citation::BoundVerification, true);
    matrix.begin("Claude reports its normal one-call Bash pushd line.");
    let check = matrix.check(false);
    matrix.close();
    matrix.settle();
    matrix.accept(&check, || {});
});

live_row!(closing_sighting_before_successor_turn_accepts, {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("Check A, then close A with its real sighting gated.");
    let check = matrix.check(false);
    let gate = matrix.transport.arm();
    matrix.close();
    let request = gate.entered();
    assert_eq!(request["occurrence"]["kind"], "source_sighting");
    gate.release();
    matrix.settle();
    matrix.begin("B starts after A's receipt; no source changed.");
    matrix.close();
    matrix.settle();
    matrix.accept(&check, || {});
});

live_row!(closing_sighting_after_successor_turn_start_accepts, {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("Check A, then hold A's closing sighting at the RPC.");
    let check = matrix.check(false);
    let gate = matrix.transport.arm();
    matrix.close();
    gate.entered();
    matrix.begin("B starts before A's sighting reaches Engram, without a change.");
    matrix.close();
    matrix.accept_after_done(&check, || {}, || gate.release());
});

live_row!(closing_sighting_after_successor_turn_close_accepts, {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("Check A; retain its immutable closing RPC.");
    let check = matrix.check(false);
    let gate = matrix.transport.arm();
    matrix.close();
    gate.entered();
    matrix.begin("B starts and closes unchanged before A's sighting is delivered.");
    matrix.close();
    matrix.accept_after_done(&check, || {}, || gate.release());
});

live_row!(ordinary_mail_wake_overlaps_previous_close_and_accepts, {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("A performs the check while an ordinary message arrives.");
    let check = matrix.check(false);
    let gate = matrix.transport.arm();
    matrix.mail_wake();
    matrix.close();
    gate.entered();
    matrix.provider.receive(&matrix.fixture, &matrix.holder);
    assert!(matrix.fixture.record(&matrix.holder, |row| {
        row.engram.unadmitted_active_turn.is_some()
    }));
    matrix.close();
    matrix.accept_after_done(&check, || {}, || gate.release());
});

live_row!(
    unchanged_holder_close_between_evidence_basis_and_submission_accepts,
    {
        let matrix = Matrix::new(Citation::UnboundNote, false);
        matrix.begin("Check then begin an independent evaluation.");
        let check = matrix.check(false);
        matrix.close();
        matrix.settle();
        let gate = matrix.transport.arm();
        matrix.accept_after_done(
            &check,
            || {
                matrix.begin(
                    "Ordinary holder work closes after the evaluator read its evidence basis.",
                );
                matrix.close();
                gate.entered();
            },
            || gate.release(),
        );
    }
);

live_row!(recovered_claim_and_renamed_root_accepts, {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    let original = matrix.fixture.claim_binding(&matrix.holder);
    matrix.fixture.work(
        &matrix.holder,
        &[
            "update",
            &matrix.work,
            "--release",
            "--reason",
            "Recover ordinary work",
            "--json",
        ],
    );
    matrix.fixture.work(
        &matrix.holder,
        &[
            "claim",
            &matrix.work,
            "--recover",
            "Recover the same disposable run",
            "--json",
        ],
    );
    let recovered = matrix.fixture.claim_binding(&matrix.holder);
    assert_eq!(original.run_id, recovered.run_id);
    assert!(recovered.claim_fence > original.claim_fence);
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
    matrix.begin("A recovered holder checks its freshly named root.");
    let check = matrix.check(false);
    matrix.close();
    matrix.settle();
    matrix.accept(&check, || {});
});

live_row!(ordinary_edit_commit_check_further_turn_evaluate_done, {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("Edit, commit and run the real check in this one ordinary turn.");
    matrix.edit("intended edited content\n");
    matrix.commit();
    let check = matrix.check(false);
    matrix.close();
    matrix.settle();
    matrix.begin("A further ordinary turn makes no source change.");
    matrix.close();
    matrix.settle();
    matrix.accept(&check, || {
        matrix.begin("Another ordinary holder turn while its independent evaluator works.");
        matrix.close();
        matrix.settle();
    });
});

live_row!(failed_check_fix_commit_recheck_same_turn_accepts, {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("Fail a real check, fix it, commit and check again without an empty turn.");
    // Both native runners read these tracked fixtures; neither result is
    // synthesized. The launcher receives the actual failing runner output.
    matrix.edit("");
    fs::write(
        matrix.fixture.root.join("src/lib.rs"),
        "#[test]\nfn checked_content() { panic!(\"fixture red\"); }\n",
    )
    .unwrap();
    matrix.commit();
    let (events, mut failed) = matrix.launch(false);
    assert_ne!(failed.exit, 0, "the first real test must fail");
    matrix.receive_check(events, &mut failed, true);
    let original_results = fs::read(failed.directory.join("results.json")).unwrap();
    matrix.edit("fixed intended content\n");
    fs::write(
        matrix.fixture.root.join("src/lib.rs"),
        "#[test]\nfn checked_content() { assert_eq!(2 + 2, 4); }\n",
    )
    .unwrap();
    matrix.commit();
    let passed = matrix.check(false);
    assert_ne!(failed.run_id, passed.run_id);
    matrix.close();
    matrix.settle();
    matrix.accept(&passed, || {});
    assert!(
        fs::read(failed.directory.join("results.json")).unwrap() == original_results,
        "fix and later check leave the original failing evidence alone"
    );
});

live_row!(real_edit_after_check_in_same_turn_is_refused, {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("Check, then make a real source edit.");
    let check = matrix.check(false);
    matrix.edit("real change after check\n");
    matrix.close();
    matrix.settle();
    matrix.refuse(&check, || {});
});

live_row!(real_edit_after_check_in_later_turn_is_refused, {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("Check the original source.");
    let check = matrix.check(false);
    matrix.close();
    matrix.settle();
    matrix.begin("A later ordinary turn changes real source.");
    matrix.edit("later real source change\n");
    matrix.close();
    matrix.settle();
    matrix.refuse(&check, || {});
});

live_row!(real_edit_during_independent_evaluator_is_refused, {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("Check before the evaluator begins.");
    let check = matrix.check(false);
    matrix.close();
    matrix.settle();
    let (child, _cleanup) = matrix.request(&check).unwrap_or_else(|_| {
        panic!("this row must create the evaluator BEFORE the edit: <details withheld>")
    });
    let reports = matrix
        .fixture
        .live
        .transport
        .requests_for("execution_observe");
    matrix.edit("changed during evaluator with no new turn report\n");
    let (status, body) = matrix.submit(&child, &check);
    assert!(
        !status.is_success(),
        "post-request edit must fail submission: <body withheld>"
    );
    Matrix::assert_source_refusal(&body);
    assert!(
        matrix
            .fixture
            .live
            .transport
            .requests_for("execution_observe") == reports,
        "the submission cannot rely on a new turn report"
    );
});

live_row!(cached_original_completion_after_real_edit_is_refused, {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("A real check finishes, but its original provider completion is delayed.");
    let (events, mut check) = matrix.launch(false);
    assert_eq!(check.exit, 0);
    let original = fs::read(check.directory.join("results.json")).unwrap();
    matrix.edit("source changed before cached completion is delivered\n");
    matrix.receive_check(events, &mut check, true);
    matrix.close();
    matrix.settle();
    matrix.refuse(&check, || {});
    assert!(fs::read(check.directory.join("results.json")).unwrap() == original,
        "the complete cached result is unchanged");
});

live_row!(check_on_another_commit_is_refused, {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("Check the first commit, then create a different clean commit.");
    let check = matrix.check(false);
    run_git_test_command(
        &matrix.fixture.root,
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "different fixture commit",
        ],
    );
    assert_ne!(
        run_git_test_command_output(&matrix.fixture.root, &["rev-parse", "HEAD"]).trim(),
        check.results["input"]["commit"].as_str().unwrap()
    );
    matrix.close();
    matrix.settle();
    matrix.refuse(&check, || {});
});

live_row!(dirty_input_check_is_refused_without_a_passed_record, {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("The launcher must refuse a dirty checkout before running its test.");
    matrix.edit("uncommitted tracked input\n");
    let (events, mut check) = matrix.launch(false);
    assert_ne!(
        check.exit, 0,
        "dirty input must not execute as an eligible passing run"
    );
    assert_ne!(
        check.results["input"]["eligible"], true,
        "dirty input must not be eligible"
    );
    matrix.receive_check(events, &mut check, true);
    assert!(check.records.is_empty());
    matrix.close();
    matrix.settle();
    assert!(
        matrix
            .fixture
            .live
            .transport
            .requests_for("verification_record")
            .is_empty(),
        "the host does not send invented passed evidence for a dirty check"
    );
});

live_row!(
    asserted_note_cannot_borrow_registered_git_provenance_for_bound_pass,
    {
        let mut matrix = Matrix::new(Citation::BoundVerification, false);
        matrix
            .begin("A real registered Git check does not turn an asserted note into verification.");
        let check = matrix.check(false);
        matrix.close();
        matrix.settle();
        let git_text = format!(
            "asserted git-commit:{}; this prose is not registered verification",
            check.results["input"]["commit"].as_str().unwrap()
        );
        let noted = matrix
            .fixture
            .work(&matrix.holder, &["note", &matrix.work, &git_text, "--json"]);
        matrix.note = noted["evidence"].as_str().unwrap().to_owned();
        // Change only which producer-issued citation the evaluator submits. The
        // real task remains test-bound, and the real registered record remains.
        matrix.citation = Citation::UnboundNote;
        let (child, _cleanup) = matrix.request(&check).unwrap_or_else(|_| {
            panic!("the evaluation request must reach the actual citation judgment: <details withheld>")
        });
        let (status, body) = matrix.submit(&child, &check);
        assert!(
            !status.is_success(),
            "asserted prose cannot borrow Git provenance: <body withheld>"
        );
        let text = body.to_string().to_ascii_lowercase();
        assert!(
            text.contains("verification") || text.contains("bound") || text.contains("observed"),
            "must be the actual evidence refusal, not a fixture error: <body withheld>"
        );
    }
);

live_row!(unmeasured_edit_reverted_to_identical_input_accepts, {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("The final checked input is restored before the next measurement.");
    let (events, mut check) = matrix.launch(false);
    let original = fs::read(matrix.fixture.root.join("README.md")).unwrap();
    matrix.edit("temporary interval content, never measured\n");
    fs::write(matrix.fixture.root.join("README.md"), &original).unwrap();
    matrix.receive_check(events, &mut check, true);
    matrix.close();
    matrix.settle();
    matrix.accept(&check, || {});
});

live_row!(measured_edit_then_revert_to_checked_commit_accepts, {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    matrix.begin("Check before a real change is measured.");
    let check = matrix.check(false);
    matrix.close();
    matrix.settle();
    let original = fs::read(matrix.fixture.root.join("README.md")).unwrap();
    matrix.begin("Measure an actual changed endpoint.");
    matrix.edit("measured changed content\n");
    let changed_revision = content_revision(&matrix.fixture.root).unwrap().1;
    matrix.close();
    matrix.settle();
    let measured = matrix.fixture.work(
        &matrix.holder,
        &["show", &matrix.work, "--observations", "--json"],
    );
    assert!(
        measured.to_string().contains("source_change"),
        "the intermediate change really reached the producer: <measured withheld>"
    );
    let originals = matrix.stored_objects();
    assert!(
        originals.iter().any(
            |(_, object)| object["accounting"]["kind"] == "source_change"
                && object.to_string().contains(&changed_revision)
        ),
        "the stored history must contain the actual changed endpoint"
    );
    matrix.begin("Restore original content but do not check again.");
    fs::write(matrix.fixture.root.join("README.md"), original).unwrap();
    matrix.close();
    matrix.settle();
    matrix.accept(&check, || {});
    matrix.assert_objects_unchanged(originals);
});
