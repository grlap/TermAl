//! A run wait that resumed on a registered run's verdict before the run's
//! Engram verification record settled resumes once more, naming that record.
//! The producer's look is an explicit step here: an ack the host records, or
//! what its latest look came to as the host keeps it in memory, then a wait
//! refresh. No timer, launcher, Engram or live store is used.
use super::*;

const RUN: &str = "test-recorded";
const OTHER: &str = "test-still-running";
const RECORD: &str = "record-0001";

/// A registered run the host sends a record for: bound through a named root
/// to a claim in an authority store.
fn recorded_run(fixture: &WaitFixture, run_id: &str) -> PathBuf {
    let directory = fixture.write(run_id, Some(running(std::process::id())));
    let path = directory.join("request.json");
    let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    request["owner"] = json!(fixture.session);
    request["creatorPid"] = json!(std::process::id());
    fs::write(path, request.to_string()).unwrap();
    fixture
        .state
        .register_test_run(
            &fixture.session,
            RegisterTestRunRequest {
                run_id: run_id.to_owned(),
                run_directory: test_run_display_path(&directory),
            },
        )
        .unwrap();
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .update(run_id, |row| {
            row.work_binding = Some(EngramControlWorkBinding {
                root_execution_id: "fixture-root-execution".into(),
                work_id: "fixture-work".into(),
                run_id: "fixture-run".into(),
                work_revision: 1,
                claim_id: "fixture-claim".into(),
                claim_fence: 1,
            });
            row.named_root = Some(TestRunNamedRoot {
                workspace_id: row.worktree.clone(),
                generation: 1,
            });
            row.authority_store = Some(EngramAuthorityStoreKey {
                database_path: PathBuf::from("fixture-store.db"),
                project_id: "fixture-project".into(),
            });
        })
        .unwrap();
    // Its request is committed input, as a launcher run's that sends a record
    // is. Registering one reads a live claim, so the row is bound above and
    // the request marked here, with the row's identity kept equal to it.
    let path = directory.join("request.json");
    let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    request["input"] = json!("commit");
    fs::write(&path, request.to_string()).unwrap();
    let identity = test_run_registration_identity(&request).unwrap();
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .update(run_id, |row| row.request_identity = identity)
        .unwrap();
    directory
}

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// The run fails with the committed-input record its verification record is
/// built from, as the launcher writes it: a failed run is never eligible.
fn fail(directory: &FsPath, run_id: &str) {
    let mut results = failed("fixture terminal failure");
    results["runId"] = json!(run_id);
    results["input"] = json!({"kind": "commit", "commit": COMMIT, "cleanBefore": true,
        "after": {"commit": COMMIT, "clean": true, "paths": []},
        "outcome": "clean", "eligible": false});
    fs::write(directory.join("results.json"), results.to_string()).unwrap();
}

/// The run fails with no committed-input record, as when the launcher's
/// input capture failed: no verification record can be built from it.
fn fail_without_input(directory: &FsPath, run_id: &str) {
    let mut results = failed("fixture terminal failure");
    results["runId"] = json!(run_id);
    fs::write(directory.join("results.json"), results.to_string()).unwrap();
}

fn registration(fixture: &WaitFixture, run_id: &str) -> TestRunRegistration {
    fixture.state.inner.lock().unwrap().test_run_registrations[run_id].clone()
}

fn failed_receipt(record_id: &str) -> EngramVerificationRecordReceipt {
    EngramVerificationRecordReceipt {
        operation: "fixture-operation".into(),
        producer: "fixture-producer".into(),
        verifications: vec![EngramVerificationRecordVerification {
            stage: "rust-tests".into(),
            record_id: record_id.into(),
        }],
        run_outcome: EngramVerificationRecordRunOutcome::Failed,
    }
}

fn settle(fixture: &WaitFixture, run_id: &str) {
    fixture.state.note_engram_verification_record_ack(
        &registration(fixture, run_id),
        TestRunVerificationAck::Settled {
            receipt: failed_receipt(RECORD),
            body_digest: "fixture-digest".into(),
        },
    );
}

/// The second resume's record lines read as single lines of prose: no run of
/// spaces and no stray backslash left inside a line, as a lost line
/// continuation in the source would leave.
fn assert_record_lines_read_cleanly(prompt: &str) {
    for line in prompt.lines().filter(|line| line.starts_with("- `")) {
        assert!(!line.contains("  ") && !line.contains('\\'), "{line:?}");
    }
}

/// What the producer's latest look came to, as the host keeps it in memory:
/// what a run wait decides from.
fn look(fixture: &WaitFixture, look: EngramVerificationRecordLook) {
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_record_pending
        .insert(RUN.into(), look);
}

/// The run fails and its wait resumes on that verdict, before any record.
fn resumed_on_failure(label: &str) -> (WaitFixture, PathBuf) {
    let fixture = WaitFixture::new(label);
    fixture.scan(&[]);
    let directory = recorded_run(&fixture, RUN);
    fixture.busy();
    fixture.register(&[RUN], "all").unwrap();
    fail(&directory, RUN);
    fixture.scan(&[]);
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains(&format!("- `{RUN}`: FAIL")),
        "{}",
        prompts[0]
    );
    assert!(
        prompts[0].contains("- Verification record: not sent yet; this wait resumes once more"),
        "{}",
        prompts[0]
    );
    assert_eq!(fixture.pending_waits().len(), 1, "kept for the record");
    (fixture, directory)
}

#[test]
fn a_wait_resumed_on_a_failed_run_resumes_again_naming_its_settled_record() {
    let (fixture, _) = resumed_on_failure("record-settled");
    let reloaded = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(
        reloaded.test_run_waits[0].runs[0]
            .registration_progress
            .record_follow_up,
        "the second resume survives a restart"
    );
    // A receipt that is not settled yet resumes nothing.
    fixture.state.note_engram_verification_record_ack(
        &registration(&fixture, RUN),
        TestRunVerificationAck::Received {
            receipt: failed_receipt(RECORD),
            body_digest: "fixture-digest".into(),
        },
    );
    fixture.state.refresh_test_run_waits();
    assert_eq!(fixture.queued_prompts().len(), 1);
    settle(&fixture, RUN);
    assert_eq!(
        fixture.queued_prompts().len(),
        1,
        "settlement itself resumes nothing; the next refresh does"
    );
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains(&format!(
            "- `{RUN}`: settled; Engram record(s) `{RECORD}`; run outcome failed"
        )),
        "{}",
        prompts[1]
    );
    assert!(fixture.pending_waits().is_empty());
    fixture.state.refresh_test_run_waits();
    assert_eq!(fixture.queued_prompts().len(), 2, "one second resume");
}

#[test]
fn a_wait_resumed_on_a_failed_run_resumes_again_naming_a_refused_record() {
    let (fixture, _) = resumed_on_failure("record-refused");
    fixture.state.note_engram_verification_record_ack(
        &registration(&fixture, RUN),
        TestRunVerificationAck::Rejected {
            detail: "fixture refusal".into(),
            producer_body_digest: None,
        },
    );
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains(&format!("- `{RUN}`: refused: fixture refusal")),
        "{}",
        prompts[1]
    );
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn a_record_left_pending_for_good_ends_the_wait_naming_why() {
    let (fixture, _) = resumed_on_failure("record-pending");
    // The producer's look leaves the record pending; this reason never
    // changes on its own.
    look(
        &fixture,
        EngramVerificationRecordLook::Pending(EngramVerificationRecordPending::BodyMismatch),
    );
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains(&format!(
            "- `{RUN}`: not settled; the host left it pending (body_mismatch)"
        )),
        "{}",
        prompts[1]
    );
    assert!(fixture.pending_waits().is_empty(), "the wait does not stay");
}

#[test]
fn a_marked_run_whose_request_no_longer_makes_a_record_ends_the_wait() {
    let (fixture, directory) = resumed_on_failure("record-request-rewritten");
    // Its files still make a record and no reason is left: the wait stays.
    fixture.state.refresh_test_run_waits();
    assert_eq!(fixture.queued_prompts().len(), 1);
    assert_eq!(fixture.pending_waits().len(), 1);
    // The request is rewritten to something no record is built from: the
    // producer's next look says so.
    fs::write(directory.join("request.json"), "{").unwrap();
    // The producer's next look, which the wait decides from.
    fixture
        .state
        .prepare_engram_verification_record(&registration(&fixture, RUN));
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains(&format!(
            "- `{RUN}`: not sent; the run's request or results no longer make a record"
        )),
        "{}",
        prompts[1]
    );
    assert_record_lines_read_cleanly(&prompts[1]);
    assert!(fixture.pending_waits().is_empty(), "the wait does not stay");
}

#[test]
fn a_marked_run_whose_directory_is_removed_ends_the_wait() {
    for held in [false, true] {
        let (fixture, directory) = resumed_on_failure(if held {
            "record-dir-removed-held"
        } else {
            "record-dir-removed"
        });
        if held {
            // Answered but not settled: nothing beside the run is left to
            // settle it from once the directory is gone.
            fixture.state.note_engram_verification_record_ack(
                &registration(&fixture, RUN),
                TestRunVerificationAck::Received {
                    receipt: failed_receipt(RECORD),
                    body_digest: "fixture-digest".into(),
                },
            );
        }
        fs::remove_dir_all(&directory).unwrap();
        // The producer's next look, which the wait decides from.
        fixture
            .state
            .prepare_engram_verification_record(&registration(&fixture, RUN));
        fixture.state.refresh_test_run_waits();
        let prompts = fixture.queued_prompts();
        assert_eq!(prompts.len(), 2, "held {held}: {prompts:?}");
        let line = prompts[1]
            .lines()
            .find(|line| line.starts_with(&format!("- `{RUN}`:")))
            .unwrap();
        if held {
            // Engram has the record, but its frozen body went with the
            // directory: its id is named with that reason, never "not sent".
            assert!(
                line.contains(&format!("`{RECORD}`"))
                    && line.contains("received, not yet settled")
                    && line.contains("(body_unreadable)")
                    && !line.contains("not sent"),
                "{line}"
            );
        } else {
            assert!(
                line.contains("not sent") && line.contains("no longer make a record"),
                "{line}"
            );
        }
        assert_record_lines_read_cleanly(&prompts[1]);
        assert!(fixture.pending_waits().is_empty(), "held {held}");
    }
}

#[test]
fn a_held_receipt_no_launch_observation_carries_ends_the_wait_naming_its_records() {
    let (fixture, _) = resumed_on_failure("record-unassociated");
    fixture.state.note_engram_verification_record_ack(
        &registration(&fixture, RUN),
        TestRunVerificationAck::Received {
            receipt: failed_receipt(RECORD),
            body_digest: "fixture-digest".into(),
        },
    );
    fixture.state.refresh_test_run_waits();
    assert_eq!(
        fixture.queued_prompts().len(),
        1,
        "a receipt still being settled is not an end"
    );
    // The producer's settlement found no launch observation carrying the
    // records (as `complete_engram_verification_record` keeps it).
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_unassociated_receipts
        .insert(RUN.into(), "fixture-digest".into());
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains(&format!(
            "- `{RUN}`: received, not yet settled; Engram record(s) `{RECORD}`; run outcome \
            failed; no launch observation of the run carries it"
        )),
        "{}",
        prompts[1]
    );
    assert_record_lines_read_cleanly(&prompts[1]);
    assert!(fixture.pending_waits().is_empty(), "the wait does not stay");
}

#[test]
fn a_newer_look_replaces_an_older_unassociated_mark() {
    let (fixture, _) = resumed_on_failure("record-mark-stale");
    fixture.state.note_engram_verification_record_ack(
        &registration(&fixture, RUN),
        TestRunVerificationAck::Received {
            receipt: failed_receipt(RECORD),
            body_digest: "fixture-digest".into(),
        },
    );
    // An earlier look's completion found no launch observation carrying the
    // records.
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_unassociated_receipts
        .insert(RUN.into(), "fixture-digest".into());
    // The producer's next look: the held receipt's frozen body is not beside
    // the run. That look, not the older mark, is what the wait names.
    assert!(matches!(
        fixture
            .state
            .prepare_engram_verification_record(&registration(&fixture, RUN)),
        EngramVerificationRecordPrepared::Pending(EngramVerificationRecordPending::BodyUnreadable)
    ));
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    let line = prompts[1]
        .lines()
        .find(|line| line.starts_with(&format!("- `{RUN}`:")))
        .unwrap();
    assert!(
        line.contains("(body_unreadable)")
            && line.contains(&format!("`{RECORD}`"))
            && !line.contains("no launch observation"),
        "{line}"
    );
    assert_record_lines_read_cleanly(&prompts[1]);
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn a_settled_copy_is_never_read_through_a_non_file_nor_retried_for_a_removed_directory() {
    let (fixture, directory) = resumed_on_failure("record-mirror-repair");
    settle(&fixture, RUN);
    let pending = || {
        fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_run_unmirrored_settlements
            .contains(RUN)
    };
    // Its copy could not be written at settlement.
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_unmirrored_settlements
        .insert(RUN.into());
    // Something other than a file stands at the state path: it is left
    // alone, never read, and the copy is tried again later.
    let state_file = directory.join(ENGRAM_VERIFICATION_RECORD_STATE_FILE);
    let _ = fs::remove_file(&state_file);
    fs::create_dir(&state_file).unwrap();
    fixture.state.engram_verification_records_tick();
    assert!(pending());
    // The run directory is gone: no summary can read a copy there, so it
    // is not tried again.
    fs::remove_dir_all(&directory).unwrap();
    fixture.state.engram_verification_records_tick();
    assert!(!pending());
}

#[test]
fn a_settled_record_is_named_while_another_run_of_the_wait_is_still_being_sent() {
    let fixture = WaitFixture::new("record-partial");
    fixture.scan(&[]);
    let first = recorded_run(&fixture, RUN);
    let second = recorded_run(&fixture, OTHER);
    fixture.busy();
    fixture.register(&[RUN, OTHER], "all").unwrap();
    fail(&first, RUN);
    fail(&second, OTHER);
    fixture.scan(&[]);
    assert_eq!(fixture.queued_prompts().len(), 1);
    // One record settles while the other's send is retried.
    settle(&fixture, RUN);
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_record_pending
        .insert(
            OTHER.into(),
            EngramVerificationRecordLook::Pending(
                EngramVerificationRecordPending::TransportFailed {
                    error_kind: "connection_reset".into(),
                },
            ),
        );
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains(&format!("- `{RUN}`: settled; Engram record(s) `{RECORD}`"))
            && !prompts[1].contains(&format!("- `{OTHER}`:")),
        "{}",
        prompts[1]
    );
    assert_eq!(
        fixture.pending_waits().len(),
        1,
        "kept for the record still being sent"
    );
    // The other record settles too: the wait names it and ends.
    settle(&fixture, OTHER);
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 3, "{prompts:?}");
    assert!(
        prompts[2].contains(&format!("- `{OTHER}`: settled"))
            && !prompts[2].contains(&format!("- `{RUN}`:")),
        "{}",
        prompts[2]
    );
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn a_send_in_flight_keeps_the_wait_although_the_run_directory_is_gone() {
    let (fixture, directory) = resumed_on_failure("record-send-in-flight");
    // The producer's look found nothing holding the record back and is
    // sending the frozen body it holds in memory.
    look(&fixture, EngramVerificationRecordLook::NoBlocker);
    fs::remove_dir_all(&directory).unwrap();
    fixture.state.refresh_test_run_waits();
    assert_eq!(
        fixture.queued_prompts().len(),
        1,
        "the wait reads no run file; the look still says no blocker"
    );
    // The send's answer lands: the record settled.
    settle(&fixture, RUN);
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains(&format!("- `{RUN}`: settled; Engram record(s) `{RECORD}`")),
        "{}",
        prompts[1]
    );
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn an_any_wait_names_a_record_already_settled_before_its_runs_verdict_is_indexed() {
    let fixture = WaitFixture::new("record-any-lag");
    fixture.scan(&[]);
    let settled_first = recorded_run(&fixture, RUN);
    let failing = recorded_run(&fixture, OTHER);
    fixture.busy();
    fixture.register(&[RUN, OTHER], "any").unwrap();
    // RUN's record settled from its files while the index still shows it
    // running; OTHER fails and satisfies the wait.
    settle(&fixture, RUN);
    let _ = settled_first;
    fail(&failing, OTHER);
    fixture.scan(&[std::process::id()]);
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains(&format!(
            "- Verification record: settled; Engram record(s) `{RECORD}`"
        )),
        "{}",
        prompts[0]
    );
}

#[test]
fn a_look_begun_before_the_run_was_marked_never_ends_its_wait() {
    for rewritten_after_mark in [false, true] {
        let fixture = WaitFixture::new(if rewritten_after_mark {
            "record-look-fence-rewritten"
        } else {
            "record-look-fence"
        });
        fixture.scan(&[]);
        let directory = recorded_run(&fixture, RUN);
        fixture.busy();
        fixture.register(&[RUN], "all").unwrap();
        // The producer looks while the run's terminal results are still
        // being published: it reads none, so its files make no record yet.
        fs::remove_file(directory.join("results.json")).unwrap();
        fixture
            .state
            .prepare_engram_verification_record(&registration(&fixture, RUN));
        assert!(matches!(
            fixture
                .state
                .inner
                .lock()
                .unwrap()
                .test_run_record_pending
                .get(RUN),
            Some(EngramVerificationRecordLook::NoRecord { .. })
        ));
        // The terminal results land: the wait resumes on the verdict and
        // marks the run for its record.
        fail(&directory, RUN);
        fixture.scan(&[]);
        let prompts = fixture.queued_prompts();
        assert_eq!(prompts.len(), 1, "{prompts:?}");
        assert!(
            prompts[0].contains("- Verification record: not sent yet; this wait resumes once more"),
            "{}",
            prompts[0]
        );
        // The look from before the mark ends nothing: the wait stays.
        fixture.state.refresh_test_run_waits();
        assert_eq!(fixture.queued_prompts().len(), 1);
        assert_eq!(fixture.pending_waits().len(), 1);
        if rewritten_after_mark {
            // A look begun after the mark that finds the files rewritten
            // still ends the wait.
            fs::write(directory.join("request.json"), "{").unwrap();
            fixture
                .state
                .prepare_engram_verification_record(&registration(&fixture, RUN));
            fixture.state.refresh_test_run_waits();
            let prompts = fixture.queued_prompts();
            assert_eq!(prompts.len(), 2, "{prompts:?}");
            assert!(
                prompts[1].contains(&format!(
                    "- `{RUN}`: not sent; the run's request or results no longer make a record"
                )),
                "{}",
                prompts[1]
            );
        } else {
            // The record is sent and settles: the wait names it.
            settle(&fixture, RUN);
            fixture.state.refresh_test_run_waits();
            let prompts = fixture.queued_prompts();
            assert_eq!(prompts.len(), 2, "{prompts:?}");
            assert!(
                prompts[1].contains(&format!("- `{RUN}`: settled; Engram record(s) `{RECORD}`"))
                    && !prompts[1].contains("not sent;"),
                "{}",
                prompts[1]
            );
        }
        assert_record_lines_read_cleanly(&fixture.queued_prompts()[1]);
        assert!(fixture.pending_waits().is_empty());
    }
}

#[test]
fn external_dart_wait_ends_when_terminal_launch_custody_never_arrives() {
    for input in ["clean", "dirty", "missing"] {
        external_dart_wait_reports_missing_custody(input);
    }
}

fn external_dart_wait_reports_missing_custody(input: &str) {
    let fixture = WaitFixture::new("external-dart-custody-wait");
    let directory = recorded_run(&fixture, RUN);
    let path = directory.join("request.json");
    let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    request["full"] = json!(false);
    request["detached"] = json!(false);
    request["stages"] = json!([{"name":"focused", "command":"dart", "args":["test"]}]);
    request["externalLauncher"] = json!({"script": test_run_display_path(&fixture.root.join("scripts/test-launcher.mjs")), "commit": COMMIT});
    fs::write(path, request.to_string()).unwrap();
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .update(RUN, |row| {
            row.request_identity = test_run_registration_identity(&request).unwrap();
            row.external_launcher = engram_external_launcher_request(&request).unwrap();
            row.preset = TestRunPreset::Focused;
            row.stage_names = vec!["focused".to_owned()];
        })
        .unwrap();
    fixture.scan(&[std::process::id()]);
    fixture.busy();
    fixture.register(&[RUN], "all").unwrap();
    fail(&directory, RUN);
    let results_path = directory.join("results.json");
    let mut results: Value = serde_json::from_slice(&fs::read(&results_path).unwrap()).unwrap();
    let now = test_run_timestamp();
    results["started"] = json!(now);
    results["ended"] = json!(now);
    let owned = fs::canonicalize(&directory)
        .unwrap()
        .join(ENGRAM_LAUNCHER_DART_REPORT);
    results["stages"] = json!([{"name":"focused", "state":"failed", "code":1,
        "cwd": request["root"], "started": now, "ended": now,
        "command":["dart", "test", "--file-reporter", format!("json:{}", test_run_display_path(&owned))]}]);
    if input == "missing" {
        results.as_object_mut().unwrap().remove("input");
    } else if input == "dirty" {
        results["input"]["outcome"] = json!("input_changed");
        results["input"]["after"]["clean"] = json!(false);
        results["input"]["after"]["paths"] = json!([{"status": " M", "path": "changed.txt"}]);
    }
    fs::write(results_path, results.to_string()).unwrap();
    let registered = registration(&fixture, RUN);
    assert!(matches!(
        fixture
            .state
            .prepare_engram_verification_record(&registered),
        EngramVerificationRecordPrepared::Pending(
            EngramVerificationRecordPending::ExternalLaunchCustodyMissing
        )
    ));
    fixture.scan(&[]);
    assert_eq!(fixture.queued_prompts().len(), 1);
    fixture.state.refresh_test_run_waits();
    assert_eq!(
        fixture.pending_waits().len(),
        1,
        "a look from before the terminal mark is insufficient"
    );
    fixture
        .state
        .prepare_engram_verification_record(&registered);
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains("external_launch_custody_missing"),
        "{prompts:?}"
    );
    assert!(fixture.pending_waits().is_empty());
    assert!(registration(&fixture, RUN).verification_record.is_none());
}

/// The run is indexed while it runs, then a wait is registered on it.
fn waited_while_running(label: &str) -> (WaitFixture, PathBuf) {
    let fixture = WaitFixture::new(label);
    fixture.scan(&[]);
    let directory = recorded_run(&fixture, RUN);
    fixture.scan(&[std::process::id()]);
    fixture.busy();
    fixture.register(&[RUN], "all").unwrap();
    assert!(fixture.queued_prompts().is_empty());
    (fixture, directory)
}

#[test]
fn a_final_record_resumes_a_wait_whose_runs_results_became_unreadable() {
    for refused in [false, true] {
        let (fixture, directory) = waited_while_running(if refused {
            "record-final-unreadable-refused"
        } else {
            "record-final-unreadable"
        });
        // The run fails and the producer builds its record from those
        // results; the record settles or is refused. Then the results become
        // unreadable before the index reads them.
        fail(&directory, RUN);
        if refused {
            fixture.state.note_engram_verification_record_ack(
                &registration(&fixture, RUN),
                TestRunVerificationAck::Rejected {
                    detail: "fixture refusal".into(),
                    producer_body_digest: None,
                },
            );
        } else {
            settle(&fixture, RUN);
        }
        fs::write(directory.join("results.json"), "{ not json").unwrap();
        fixture.scan(&[]);
        let prompts = fixture.queued_prompts();
        assert_eq!(prompts.len(), 1, "refused {refused}: {prompts:?}");
        let prompt = &prompts[0];
        if refused {
            assert!(
                prompt.contains("- Verdict: UNKNOWN")
                    && prompt.contains("- Verification record: refused: fixture refusal"),
                "{prompt}"
            );
        } else {
            assert!(
                prompt.contains("- Verdict: FAIL")
                    && prompt.contains(&format!(
                        "- Verification record: settled; Engram record(s) `{RECORD}`; run outcome failed"
                    ))
                    && prompt.contains("Diagnostics: not available here"),
                "{prompt}"
            );
        }
        assert!(!prompt.contains("not sent"), "{prompt}");
        assert!(fixture.pending_waits().is_empty(), "refused {refused}");
    }
}

#[test]
fn a_final_record_waits_for_a_run_the_index_still_reads_as_running() {
    let (fixture, directory) = waited_while_running("record-final-running");
    // The record settled while the index's last read still found the run
    // running: its next rescan reads the terminal results.
    settle(&fixture, RUN);
    fixture.state.refresh_test_run_waits();
    assert!(fixture.queued_prompts().is_empty());
    assert_eq!(fixture.pending_waits().len(), 1);
    fail(&directory, RUN);
    fixture.scan(&[]);
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("- Verdict: FAIL")
            && prompts[0].contains("fixture terminal failure")
            && prompts[0].contains(&format!(
                "- Verification record: settled; Engram record(s) `{RECORD}`"
            )),
        "{}",
        prompts[0]
    );
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn a_final_record_decides_for_a_run_the_index_keeps_reading_as_running() {
    let (fixture, _directory) = waited_while_running("record-final-stays-running");
    // The record settled, but the run's results on disk still read as
    // running and its process is alive: real rescans keep reading it so.
    settle(&fixture, RUN);
    fixture.scan(&[std::process::id()]);
    assert!(
        fixture.queued_prompts().is_empty(),
        "that rescan may have begun before the record settled"
    );
    fixture.scan(&[std::process::id()]);
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("- Verdict: FAIL")
            && prompts[0].contains(&format!(
                "- Verification record: settled; Engram record(s) `{RECORD}`; run outcome failed"
            ))
            && prompts[0].contains("Diagnostics: not available here")
            && !prompts[0].contains("Settle it"),
        "{}",
        prompts[0]
    );
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn a_record_in_flight_is_followed_for_a_run_whose_directory_was_removed() {
    for in_flight in [true, false] {
        let (fixture, directory) = waited_while_running(if in_flight {
            "record-in-flight-removed"
        } else {
            "record-none-removed"
        });
        fail(&directory, RUN);
        if in_flight {
            // The producer's look froze the record's body, as
            // prepare_engram_verification_record keeps it: its send is in
            // flight from memory.
            fixture
                .state
                .inner
                .lock()
                .unwrap()
                .test_run_record_bodies_built
                .insert(RUN.into());
        }
        // The run directory is removed before the index reads the verdict.
        fs::remove_dir_all(&directory).unwrap();
        fixture.scan(&[]);
        let prompts = fixture.queued_prompts();
        assert_eq!(prompts.len(), 1, "in flight {in_flight}: {prompts:?}");
        assert!(prompts[0].contains("- Verdict: UNKNOWN"), "{}", prompts[0]);
        if in_flight {
            assert!(
                prompts[0]
                    .contains("- Verification record: not sent yet; this wait resumes once more"),
                "{}",
                prompts[0]
            );
            assert_eq!(fixture.pending_waits().len(), 1, "kept for the record");
            // The send's answer lands: the record settled.
            settle(&fixture, RUN);
            fixture.state.refresh_test_run_waits();
            let prompts = fixture.queued_prompts();
            assert_eq!(prompts.len(), 2, "{prompts:?}");
            assert!(
                prompts[1].contains(&format!("- `{RUN}`: settled; Engram record(s) `{RECORD}`")),
                "{}",
                prompts[1]
            );
            assert_record_lines_read_cleanly(&prompts[1]);
        } else {
            // Nothing was built: no record is followed, as before.
            assert!(
                !prompts[0].contains("- Verification record:"),
                "{}",
                prompts[0]
            );
        }
        assert!(fixture.pending_waits().is_empty(), "in flight {in_flight}");
    }
}

#[test]
fn a_followed_run_whose_record_was_never_sent_ends_as_not_sent() {
    let (fixture, directory) = waited_while_running("record-built-never-sent");
    fail(&directory, RUN);
    // Its body was built, so a send may be in flight.
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_record_bodies_built
        .insert(RUN.into());
    fs::remove_dir_all(&directory).unwrap();
    fixture.scan(&[]);
    assert_eq!(fixture.queued_prompts().len(), 1);
    assert_eq!(fixture.pending_waits().len(), 1, "kept for the record");
    // No send happened: the producer's next look, begun after the mark,
    // finds the run's request gone.
    fixture
        .state
        .prepare_engram_verification_record(&registration(&fixture, RUN));
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains(&format!(
            "- `{RUN}`: not sent; the run's request or results no longer make a record"
        )),
        "{}",
        prompts[1]
    );
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn an_any_wait_never_follows_a_run_the_index_does_not_show_finished() {
    let fixture = WaitFixture::new("record-any-no-verdict");
    fixture.scan(&[]);
    recorded_run(&fixture, RUN);
    let failing = recorded_run(&fixture, OTHER);
    fixture.busy();
    fixture.register(&[RUN, OTHER], "any").unwrap();
    // RUN wrote its terminal results, the producer froze its body and its
    // next look is under way, while the index still reads RUN as running;
    // OTHER fails and satisfies the wait.
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner.test_run_record_bodies_built.insert(RUN.into());
        let begun = inner.test_run_look_sequence;
        inner
            .test_run_record_looks_in_flight
            .insert(RUN.into(), begun);
    }
    fail(&failing, OTHER);
    fixture.scan(&[std::process::id()]);
    // The resume comes at once: RUN's look is not waited for.
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains(&format!("- `{RUN}`: NOT SETTLED YET"))
            && prompts[0]
                .contains("- Verification record: not sent yet; this wait resumes once more"),
        "{}",
        prompts[0]
    );
    let wait = &fixture.pending_waits()[0];
    assert!(
        wait.runs
            .iter()
            .all(|label| label.registration_progress.record_follow_up == (label.run_id == OTHER)),
        "only the failed run is followed"
    );
    // Both records settle: the wait names OTHER's and ends; RUN's
    // settlement is not this wait's to tell.
    settle(&fixture, RUN);
    settle(&fixture, OTHER);
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains(&format!(
            "- `{OTHER}`: settled; Engram record(s) `{RECORD}`"
        )) && !prompts[1].contains(&format!("`{RUN}`")),
        "{}",
        prompts[1]
    );
    assert_record_lines_read_cleanly(&prompts[1]);
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn a_look_in_flight_defers_the_first_resume_until_it_lands() {
    let (fixture, directory) = waited_while_running("record-look-in-flight");
    fail(&directory, RUN);
    // The producer's look has begun and not yet published what it came to.
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_record_looks_in_flight
        .insert(RUN.into(), 0);
    fs::remove_dir_all(&directory).unwrap();
    fixture.scan(&[]);
    assert!(
        fixture.queued_prompts().is_empty(),
        "the first resume waits for the look"
    );
    assert_eq!(fixture.pending_waits().len(), 1);
    // The look lands with a frozen body: the run is followed for its record.
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner.test_run_record_looks_in_flight.remove(RUN);
        inner.test_run_record_bodies_built.insert(RUN.into());
    }
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("- Verification record: not sent yet; this wait resumes once more"),
        "{}",
        prompts[0]
    );
    assert_eq!(fixture.pending_waits().len(), 1, "kept for the record");
}

/// What the producer's looks of `run_id` are doing: the one in flight, if
/// any, and the latest that published.
fn looks(fixture: &WaitFixture, run_id: &str, in_flight: Option<u64>, published: Option<u64>) {
    let mut inner = fixture.state.inner.lock().unwrap();
    match in_flight {
        Some(begun) => inner
            .test_run_record_looks_in_flight
            .insert(run_id.into(), begun),
        None => inner.test_run_record_looks_in_flight.remove(run_id),
    };
    if let Some(begun) = published {
        inner
            .test_run_record_look_published
            .insert(run_id.into(), begun);
    }
}

/// The place the producer's next look takes.
fn next_look(fixture: &WaitFixture) -> u64 {
    fixture.state.inner.lock().unwrap().test_run_look_sequence
}

#[test]
fn a_first_resume_waits_for_one_look_only() {
    let (fixture, directory) = waited_while_running("record-one-look");
    fail(&directory, RUN);
    let first = next_look(&fixture);
    looks(&fixture, RUN, Some(first), None);
    fs::remove_dir_all(&directory).unwrap();
    fixture.scan(&[]);
    assert!(
        fixture.queued_prompts().is_empty(),
        "the first resume waits for the look in flight"
    );
    // That look publishes, finding nothing to follow, and a newer look is
    // already under way: the resume does not wait for that one too.
    looks(&fixture, RUN, Some(first + 1), Some(first));
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "one look only: {prompts:?}");
    assert!(
        prompts[0].contains("- Verdict: UNKNOWN") && !prompts[0].contains("- Verification record:"),
        "{}",
        prompts[0]
    );
    assert!(fixture.pending_waits().is_empty());
}

/// A wait admitted from the run's registration before the index lists the
/// run, on a run whose record body is built: its send may be in flight.
fn registered_with_a_body_built(label: &str) -> (WaitFixture, PathBuf) {
    let fixture = WaitFixture::new(label);
    fixture.scan(&[]);
    let directory = recorded_run(&fixture, RUN);
    fixture.busy();
    fixture.register(&[RUN], "all").unwrap();
    assert!(fixture.queued_prompts().is_empty());
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_record_bodies_built
        .insert(RUN.into());
    (fixture, directory)
}

#[test]
fn a_registered_run_whose_directory_was_removed_is_followed_for_its_record() {
    let (fixture, directory) = registered_with_a_body_built("record-registered-removed");
    fs::remove_dir_all(&directory).unwrap();
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("UNKNOWN (registered run directory removed)")
            && prompts[0]
                .contains("- Verification record: not sent yet; this wait resumes once more"),
        "{}",
        prompts[0]
    );
    assert_eq!(fixture.pending_waits().len(), 1, "kept for the record");
    settle(&fixture, RUN);
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains(&format!("- `{RUN}`: settled; Engram record(s) `{RECORD}`")),
        "{}",
        prompts[1]
    );
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn a_registered_run_whose_files_could_not_be_read_is_not_followed() {
    let (fixture, directory) = registered_with_a_body_built("record-registered-unreadable");
    // An oversized results file: the registered read decides at once.
    fs::write(
        directory.join("results.json"),
        vec![b' '; TEST_RUN_JSON_MAX_BYTES + 1],
    )
    .unwrap();
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("UNKNOWN (registered artifacts unreadable)")
            && !prompts[0].contains("- Verification record:"),
        "{}",
        prompts[0]
    );
    assert!(fixture.pending_waits().is_empty(), "not followed");
    // The record settles later: the run's owner is told on its own line
    // (engram_received_check_notice.rs); this wait asks no more.
    settle(&fixture, RUN);
    fixture.state.refresh_test_run_waits();
    assert_eq!(fixture.queued_prompts().len(), 1);
    assert!(matches!(
        registration(&fixture, RUN).verification_record,
        Some(TestRunVerificationAck::Settled { .. })
    ));
}

#[test]
fn an_indexed_run_dropped_for_unreadable_files_is_not_followed() {
    let (fixture, directory) = waited_while_running("record-dropped-unreadable");
    fail(&directory, RUN);
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_record_bodies_built
        .insert(RUN.into());
    // Its request cannot be read and the index drops it; the directory is
    // still there.
    fs::remove_file(directory.join("request.json")).unwrap();
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_runs
        .entries
        .clear();
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("UNKNOWN (registered artifacts unreadable)")
            && !prompts[0].contains("- Verification record:"),
        "{}",
        prompts[0]
    );
    assert!(fixture.pending_waits().is_empty(), "not followed");
}

#[test]
fn an_unreadable_input_never_holds_back_another_runs_settled_record() {
    let fixture = WaitFixture::new("record-input-unreadable");
    fixture.scan(&[]);
    let failing = recorded_run(&fixture, RUN);
    let passing = recorded_run(&fixture, OTHER);
    fail(&failing, RUN);
    settle(&fixture, RUN);
    let mut results = passed();
    results["runId"] = json!(OTHER);
    results["input"] = json!({"kind": "commit", "commit": COMMIT, "cleanBefore": true,
        "after": {"commit": COMMIT, "clean": true, "paths": []},
        "outcome": "clean", "eligible": false});
    fs::write(passing.join("results.json"), results.to_string()).unwrap();
    // The index reads both terminal results.
    fixture.scan(&[]);
    // OTHER's results then cannot be read, while the index keeps its
    // verdict: the file keeps its size and modification time.
    let path = passing.join("results.json");
    let metadata = fs::metadata(&path).unwrap();
    let modified = metadata.modified().unwrap();
    fs::write(&path, "x".repeat(metadata.len() as usize)).unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    fixture.busy();
    fixture.register(&[RUN, OTHER], "all").unwrap();
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains(&format!(
            "- Verification record: settled; Engram record(s) `{RECORD}`; run outcome failed"
        )) && prompts[0]
            .contains("- Verification record: not sent yet; this wait resumes once more"),
        "{}",
        prompts[0]
    );
    assert_record_lines_read_cleanly(&prompts[0]);
    let wait = &fixture.pending_waits()[0];
    assert!(
        wait.runs
            .iter()
            .all(|label| label.registration_progress.record_follow_up == (label.run_id == OTHER)),
        "the unreadable run is followed; the settled one is named"
    );
}

#[test]
fn a_reason_left_for_good_is_named_for_a_run_whose_directory_was_removed() {
    let (fixture, directory) = waited_while_running("record-persist-failed-removed");
    fail(&directory, RUN);
    // The producer read the terminal results, but the directory went before
    // it could write the frozen body: the look left the record pending.
    look(
        &fixture,
        EngramVerificationRecordLook::Pending(EngramVerificationRecordPending::PersistFailed),
    );
    fs::remove_dir_all(&directory).unwrap();
    fixture.scan(&[]);
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("- Verdict: UNKNOWN")
            && prompts[0].contains(
                "- Verification record: not settled; the host left it pending (persist_failed)"
            ),
        "{}",
        prompts[0]
    );
    assert_record_lines_read_cleanly(&prompts[0]);
    assert!(fixture.pending_waits().is_empty(), "the reason is final");
}

#[test]
fn a_marked_wait_does_not_end_on_a_reason_older_than_a_look_in_flight() {
    let (fixture, _) = resumed_on_failure("record-older-reason");
    // An earlier look left no_connection; the host has since reconnected and
    // a newer look is in flight.
    look(
        &fixture,
        EngramVerificationRecordLook::Pending(EngramVerificationRecordPending::NoConnection),
    );
    looks(&fixture, RUN, Some(100), Some(99));
    fixture.state.refresh_test_run_waits();
    assert_eq!(
        fixture.queued_prompts().len(),
        1,
        "the older reason waits for the look in flight"
    );
    // The newer look found the record ready and is sending it.
    look(&fixture, EngramVerificationRecordLook::NoBlocker);
    looks(&fixture, RUN, None, Some(100));
    fixture.state.refresh_test_run_waits();
    assert_eq!(fixture.queued_prompts().len(), 1);
    // Its send settles: the wait names the record.
    settle(&fixture, RUN);
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains(&format!("- `{RUN}`: settled; Engram record(s) `{RECORD}`")),
        "{}",
        prompts[1]
    );
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn a_wait_that_deferred_once_never_defers_again_for_a_later_look() {
    let (fixture, _) = resumed_on_failure("record-defers-once");
    // Look 100 is in flight: the marked wait defers for it.
    looks(&fixture, RUN, Some(100), Some(99));
    fixture.state.refresh_test_run_waits();
    assert_eq!(fixture.queued_prompts().len(), 1, "deferred for look 100");
    // Look 100 completes finding nothing that ends the wait: it keeps waiting.
    look(&fixture, EngramVerificationRecordLook::NoBlocker);
    looks(&fixture, RUN, None, Some(100));
    fixture.state.refresh_test_run_waits();
    assert_eq!(fixture.queued_prompts().len(), 1, "still being sent");
    // Later, the latest completed look left the record pending for a route
    // reason while look 101 is under way: the wait, which deferred once for
    // this run, ends at once naming that reason.
    look(
        &fixture,
        EngramVerificationRecordLook::Pending(EngramVerificationRecordPending::NoConnection),
    );
    looks(&fixture, RUN, Some(101), Some(100));
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains("not settled; the host left it pending (no_connection)"),
        "{}",
        prompts[1]
    );
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn a_first_resume_deferral_is_the_runs_one_deferral() {
    let (fixture, directory) = waited_while_running("record-first-resume-defers-once");
    fail(&directory, RUN);
    let first = next_look(&fixture);
    looks(&fixture, RUN, Some(first), None);
    fs::remove_dir_all(&directory).unwrap();
    fixture.scan(&[]);
    assert!(
        fixture.queued_prompts().is_empty(),
        "the first resume defers for the look"
    );
    // The look lands having built the record's body: the run is followed.
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_record_bodies_built
        .insert(RUN.into());
    looks(&fixture, RUN, None, Some(first));
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("- Verification record: not sent yet; this wait resumes once more"),
        "{}",
        prompts[0]
    );
    // A later look is under way when the latest completed one left the record
    // pending for a route reason: the follow-up does not defer again.
    look(
        &fixture,
        EngramVerificationRecordLook::Pending(EngramVerificationRecordPending::NoConnection),
    );
    looks(&fixture, RUN, Some(first + 1), Some(first));
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains("not settled; the host left it pending (no_connection)"),
        "{}",
        prompts[1]
    );
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn a_deferral_for_one_run_never_stops_a_wait_deferring_for_another() {
    let fixture = WaitFixture::new("record-defers-per-run");
    fixture.scan(&[]);
    let removed_first = recorded_run(&fixture, RUN);
    let removed_later = recorded_run(&fixture, OTHER);
    fixture.busy();
    fixture.register(&[RUN, OTHER], "any").unwrap();
    fixture.scan(&[std::process::id()]);
    // RUN's directory is removed while a look of it is under way: the first
    // resume defers for that look.
    let first = next_look(&fixture);
    looks(&fixture, RUN, Some(first), None);
    fs::remove_dir_all(&removed_first).unwrap();
    fixture.scan(&[std::process::id()]);
    assert!(
        fixture.queued_prompts().is_empty(),
        "deferred for RUN's look"
    );
    // That look completes having built nothing. Meanwhile OTHER's look is
    // preparing its record, and OTHER's directory is removed too.
    looks(&fixture, RUN, None, Some(first));
    looks(&fixture, OTHER, Some(first + 1), None);
    fs::remove_dir_all(&removed_later).unwrap();
    fixture.scan(&[]);
    assert!(
        fixture.queued_prompts().is_empty(),
        "the wait defers once for OTHER's look as well"
    );
    // OTHER's look lands with the record's body built: OTHER is followed.
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_record_bodies_built
        .insert(OTHER.into());
    looks(&fixture, OTHER, None, Some(first + 1));
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("- Verification record: not sent yet; this wait resumes once more"),
        "{}",
        prompts[0]
    );
    // Its record settles from memory: the wait names it.
    settle(&fixture, OTHER);
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains(&format!(
            "- `{OTHER}`: settled; Engram record(s) `{RECORD}`"
        )),
        "{}",
        prompts[1]
    );
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn an_index_entry_without_a_locator_is_not_taken_for_another_run() {
    let (fixture, _) = waited_while_running("record-entry-no-locator");
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_record_bodies_built
        .insert(RUN.into());
    // The rescan could not resolve the run's path just then: its entry
    // carries no locator. That is no evidence of another run.
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        for entry in inner.test_runs.entries.entries.iter_mut() {
            if entry.summary.run_id == RUN {
                let mut disk = (*entry.disk).clone();
                disk.locator_identity = None;
                entry.disk = std::sync::Arc::new(disk);
            }
        }
    }
    fixture.state.refresh_test_run_waits();
    assert!(
        fixture.queued_prompts().is_empty(),
        "neither gone nor resumed: {:?}",
        fixture.queued_prompts()
    );
    assert_eq!(
        fixture.pending_waits().len(),
        1,
        "retried at the next rescan"
    );
}

#[test]
fn a_final_record_decides_for_a_run_the_rescans_never_list() {
    let fixture = WaitFixture::new("record-final-never-listed");
    fixture.scan(&[]);
    let directory = recorded_run(&fixture, RUN);
    fixture.busy();
    fixture.register(&[RUN], "all").unwrap();
    fail(&directory, RUN);
    settle(&fixture, RUN);
    // A rescan that cannot list the run's directory completes without
    // indexing it.
    let rescan_without_listing = || {
        fixture.state.inner.lock().unwrap().test_run_scan_generation += 1;
        fixture.state.refresh_test_run_waits();
    };
    rescan_without_listing();
    assert!(
        fixture.queued_prompts().is_empty(),
        "that rescan may have begun before the record settled"
    );
    rescan_without_listing();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("- Verdict: FAIL")
            && prompts[0].contains(&format!(
                "- Verification record: settled; Engram record(s) `{RECORD}`; run outcome failed"
            ))
            && !prompts[0].contains("Settle it"),
        "{}",
        prompts[0]
    );
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn a_registration_under_a_reused_run_id_never_resumes_another_runs_wait() {
    for unreadable in [false, true] {
        let (fixture, directory) = waited_while_running(if unreadable {
            "record-reused-id-unreadable"
        } else {
            "record-reused-id"
        });
        // A registration under the same run id, for another request and
        // locator, takes the place of the one this wait's run was admitted
        // with; its record settles.
        fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_run_registrations
            .update(RUN, |row| {
                row.request_identity = "reused-run-id-request".into();
                row.locator_identity = Some("reused-run-id-locator".into());
            })
            .unwrap();
        fail(&directory, RUN);
        settle(&fixture, RUN);
        if unreadable {
            fs::write(directory.join("results.json"), "{ not json").unwrap();
        }
        fixture.scan(&[]);
        for _ in 0..2 {
            fixture.state.inner.lock().unwrap().test_run_scan_generation += 1;
            fixture.state.refresh_test_run_waits();
        }
        let prompts = fixture.queued_prompts();
        assert!(
            prompts.iter().all(
                |prompt| !prompt.contains(RECORD) && !prompt.contains("- Verification record:")
            ),
            "unreadable {unreadable}: {prompts:?}"
        );
        if unreadable {
            // The index has no verdict, and the other registration's record
            // gives none.
            assert!(prompts.is_empty(), "{prompts:?}");
            assert_eq!(fixture.pending_waits().len(), 1);
        }
    }
}

#[test]
fn a_reason_this_host_keeps_ends_the_wait_without_a_state_file() {
    let (fixture, directory) = resumed_on_failure("record-reason-kept");
    let remember = |reason: EngramVerificationRecordPending| {
        look(&fixture, EngramVerificationRecordLook::Pending(reason))
    };
    // A retried send is not an end, kept in memory or not.
    remember(EngramVerificationRecordPending::TransportFailed {
        error_kind: "connection_reset".into(),
    });
    fixture.state.refresh_test_run_waits();
    assert_eq!(fixture.queued_prompts().len(), 1);
    // The producer's look came to persist_failed in a run directory that
    // took no state file (as `prepare_engram_verification_record` keeps it).
    remember(EngramVerificationRecordPending::PersistFailed);
    assert!(engram_verification_record_progress(&directory).is_none());
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains(&format!(
            "- `{RUN}`: not settled; the host left it pending (persist_failed)"
        )),
        "{}",
        prompts[1]
    );
    assert!(fixture.pending_waits().is_empty(), "the wait does not stay");
}

#[test]
fn the_latest_look_overrides_an_older_state_file() {
    let (fixture, directory) = resumed_on_failure("record-reason-newer");
    // An earlier look left no_connection beside the run.
    engram_verification_record_set_progress(
        &directory,
        RUN,
        EngramVerificationRecordProgress::Pending {
            reason: EngramVerificationRecordPending::NoConnection,
        },
    );
    // The latest looks found no blocker (the record is being sent again),
    // then a retried send: neither ends the wait, whatever the file says.
    for latest in [
        EngramVerificationRecordLook::NoBlocker,
        EngramVerificationRecordLook::Pending(EngramVerificationRecordPending::TransportFailed {
            error_kind: "connection_reset".into(),
        }),
    ] {
        look(&fixture, latest);
        fixture.state.refresh_test_run_waits();
        assert_eq!(
            fixture.queued_prompts().len(),
            1,
            "{:?}",
            fixture.queued_prompts()
        );
        assert_eq!(fixture.pending_waits().len(), 1);
    }
    // The run's own files no longer making a record, as the producer's next
    // look finds, ends it.
    fs::write(directory.join("request.json"), "{").unwrap();
    // The producer's next look, which the wait decides from.
    fixture
        .state
        .prepare_engram_verification_record(&registration(&fixture, RUN));
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains(&format!(
            "- `{RUN}`: not sent; the run's request or results no longer make a record"
        )),
        "{}",
        prompts[1]
    );
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn a_restarted_host_decides_from_its_first_look_not_an_older_state_file() {
    let (fixture, directory) = resumed_on_failure("record-restart");
    // Answered before the restart, while the state file beside the run still
    // says no_connection from an older look.
    fixture.state.note_engram_verification_record_ack(
        &registration(&fixture, RUN),
        TestRunVerificationAck::Received {
            receipt: failed_receipt(RECORD),
            body_digest: "fixture-digest".into(),
        },
    );
    engram_verification_record_set_progress(
        &directory,
        RUN,
        EngramVerificationRecordProgress::Pending {
            reason: EngramVerificationRecordPending::NoConnection,
        },
    );
    // A restarted host keeps nothing in memory yet: the wait stays.
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_record_pending
        .clear();
    fixture.state.refresh_test_run_waits();
    assert_eq!(fixture.queued_prompts().len(), 1);
    assert_eq!(fixture.pending_waits().len(), 1);
    // The producer's first look: the held receipt's frozen body is not
    // beside the run.
    assert!(matches!(
        fixture
            .state
            .prepare_engram_verification_record(&registration(&fixture, RUN)),
        EngramVerificationRecordPrepared::Pending(EngramVerificationRecordPending::BodyUnreadable)
    ));
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    // Engram has the record: its id is named with the reason, and that this
    // host has not settled it.
    let line = prompts[1]
        .lines()
        .find(|line| line.starts_with(&format!("- `{RUN}`:")))
        .unwrap();
    assert!(
        line.contains(&format!("`{RECORD}`"))
            && line.contains("(body_unreadable)")
            && line.contains("received, not yet settled")
            && !line.contains("no_connection"),
        "{line}"
    );
    assert_record_lines_read_cleanly(&prompts[1]);
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn a_marked_run_whose_results_grow_past_the_bound_ends_the_wait() {
    let (fixture, directory) = resumed_on_failure("record-results-too-large");
    // Valid JSON, but past what the host reads of a run file.
    let padding = "x".repeat(TEST_RUN_JSON_MAX_BYTES);
    fs::write(
        directory.join("results.json"),
        format!("{{\"state\":\"failed\",\"padding\":\"{padding}\"}}"),
    )
    .unwrap();
    // The producer's next look, which the wait decides from.
    fixture
        .state
        .prepare_engram_verification_record(&registration(&fixture, RUN));
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains(&format!(
            "- `{RUN}`: not sent; the run's request or results no longer make a record"
        )),
        "{}",
        prompts[1]
    );
    assert!(fixture.pending_waits().is_empty(), "the wait does not stay");
}

#[test]
fn a_failed_call_keeps_the_wait_until_the_resent_record_settles() {
    let (fixture, _) = resumed_on_failure("record-transport");
    // The call fails without an answer; the producer sends the same record again.
    look(
        &fixture,
        EngramVerificationRecordLook::Pending(EngramVerificationRecordPending::TransportFailed {
            error_kind: "connection_reset".into(),
        }),
    );
    fixture.state.refresh_test_run_waits();
    assert_eq!(
        fixture.queued_prompts().len(),
        1,
        "a failed call is not an end"
    );
    assert_eq!(fixture.pending_waits().len(), 1);
    settle(&fixture, RUN);
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains(&format!(
            "- `{RUN}`: settled; Engram record(s) `{RECORD}`; run outcome failed"
        )),
        "{}",
        prompts[1]
    );
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn an_engram_deferral_keeps_the_wait_until_the_resent_record_settles() {
    let (fixture, _) = resumed_on_failure("record-deferred");
    // Engram answers busy; the producer sends the same record again later.
    look(
        &fixture,
        EngramVerificationRecordLook::Pending(EngramVerificationRecordPending::RemoteDeferred {
            code: "busy".into(),
        }),
    );
    fixture.state.refresh_test_run_waits();
    assert_eq!(
        fixture.queued_prompts().len(),
        1,
        "a deferral is not an end"
    );
    assert_eq!(fixture.pending_waits().len(), 1);
    settle(&fixture, RUN);
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains(&format!(
            "- `{RUN}`: settled; Engram record(s) `{RECORD}`; run outcome failed"
        )),
        "{}",
        prompts[1]
    );
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn a_failed_run_without_a_committed_input_record_is_not_followed_up() {
    let fixture = WaitFixture::new("record-no-input");
    fixture.scan(&[]);
    let directory = recorded_run(&fixture, RUN);
    fixture.busy();
    fixture.register(&[RUN], "all").unwrap();
    fail_without_input(&directory, RUN);
    fixture.scan(&[]);
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains(
            "- Verification record: none; the run's results carry no committed-input record"
        ),
        "{}",
        prompts[0]
    );
    assert!(
        fixture.pending_waits().is_empty(),
        "no record can come, so the wait does not stay"
    );
}

#[test]
fn an_any_wait_names_its_failed_runs_record_while_another_run_still_runs() {
    let fixture = WaitFixture::new("record-any");
    fixture.scan(&[]);
    let failing = recorded_run(&fixture, RUN);
    recorded_run(&fixture, OTHER);
    fixture.busy();
    fixture.register(&[RUN, OTHER], "any").unwrap();
    fail(&failing, RUN);
    fixture.scan(&[std::process::id()]);
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(prompts[0].contains(&format!("- `{OTHER}`: NOT SETTLED YET")));
    let wait = &fixture.pending_waits()[0];
    assert!(
        wait.runs
            .iter()
            .all(|label| label.registration_progress.record_follow_up == (label.run_id == RUN)),
        "only the run with a verdict waits on its record"
    );
    settle(&fixture, RUN);
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains(&format!("- `{RUN}`: settled; Engram record(s) `{RECORD}`"))
            && !prompts[1].contains(OTHER),
        "{}",
        prompts[1]
    );
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn a_record_settled_before_the_verdict_is_named_in_the_one_resume() {
    let fixture = WaitFixture::new("record-first");
    fixture.scan(&[]);
    let directory = recorded_run(&fixture, RUN);
    fixture.busy();
    fixture.register(&[RUN], "all").unwrap();
    settle(&fixture, RUN);
    fixture.state.refresh_test_run_waits();
    assert!(
        fixture.queued_prompts().is_empty(),
        "the run has no verdict yet"
    );
    fail(&directory, RUN);
    fixture.scan(&[]);
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains(&format!(
            "- Verification record: settled; Engram record(s) `{RECORD}`; run outcome failed"
        )),
        "{}",
        prompts[0]
    );
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn record_lines_survive_a_full_excerpt_budget() {
    let run_ids: Vec<String> = (0..16).map(|index| format!("test-{index:02}")).collect();
    let summaries: Vec<TestRunSummary> = run_ids.iter().map(|id| failed_summary(id)).collect();
    let failures: HashMap<String, TestRunCardFailureRead> = run_ids
        .iter()
        .map(|id| {
            (
                id.clone(),
                failure_read("x".repeat(TEST_RUN_WAIT_EXCERPT_MAX_BYTES), false),
            )
        })
        .collect();
    let records: HashMap<String, String> = run_ids
        .iter()
        .map(|id| {
            (
                id.clone(),
                format!("settled; Engram record(s) `record-{id}`; run outcome failed"),
            )
        })
        .collect();
    let wait = wait_on(&summaries);
    let refs: Vec<Option<&TestRunSummary>> = summaries.iter().map(Some).collect();
    let verdicts = test_run_wait_verdicts(&wait, &refs, |_| false);
    let prompt = build_test_run_wait_prompt(
        &wait,
        &refs,
        &verdicts,
        &failures,
        &HashMap::new(),
        &records,
    );
    assert!(prompt.len() <= TEST_RUN_WAIT_PROMPT_MAX_BYTES);
    for id in &run_ids {
        assert!(
            prompt.contains(&format!(
                "- Verification record: settled; Engram record(s) `record-{id}`"
            )),
            "{id}"
        );
    }
}

#[test]
fn admission_reserves_the_record_line_for_runs_that_send_a_record() {
    // The layout the unbound control in prompt_evidence admits at 600-byte
    // display paths: bound runs also carry a record line, which counts.
    let fixture = WaitFixture::new("record-admission");
    fixture.scan(&[]);
    let ids: Vec<String> = (0..16).map(|index| format!("test-bound-{index}")).collect();
    for id in &ids {
        let directory = recorded_run(&fixture, id);
        let mut results = passed();
        results["runId"] = json!(id);
        fs::write(directory.join("results.json"), results.to_string()).unwrap();
    }
    fixture.scan(&[]);
    fixture.busy();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let prefix = test_run_display_path(&fixture.root);
        for position in 0..inner.test_runs.entries.len() {
            inner.test_runs.entries.update(position, |entry| {
                entry.summary.run_dir = format!("{prefix}/{}", "x".repeat(600 - prefix.len() - 1));
            });
        }
    }
    let error = fixture
        .register(&ids.iter().map(String::as_str).collect::<Vec<_>>(), "all")
        .err()
        .expect("the record line counts at admission");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(
        error.message.contains("49152-byte limit"),
        "{}",
        error.message
    );
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn a_results_file_unreadable_at_the_first_resume_never_ends_the_wait() {
    let fixture = WaitFixture::new("record-input-unreadable");
    fixture.scan(&[]);
    let directory = recorded_run(&fixture, RUN);
    fixture.busy();
    fixture.register(&[RUN], "all").unwrap();
    fail(&directory, RUN);
    // The index takes the failed verdict; no wait is refreshed yet.
    fixture
        .state
        .refresh_test_runs_with(&|_, _| false, &|inner, event| {
            fixture.state.publish_delta_locked(inner, event.clone())
        });
    // The results file cannot be read just when the wait reads it.
    let results = directory.join("results.json");
    let readable = fs::read(&results).unwrap();
    fixture
        .state
        .refresh_test_run_waits_pausing(&|| fs::write(&results, b"{").unwrap());
    // The verdict is not held back by the unreadable input, and the run is
    // followed for its record, not dropped on that failed read.
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("- Verdict: FAIL")
            && prompts[0]
                .contains("- Verification record: not sent yet; this wait resumes once more"),
        "{}",
        prompts[0]
    );
    assert_eq!(
        fixture.pending_waits().len(),
        1,
        "an unreadable input does not end the wait"
    );
    // Still unreadable through the index's one-rescan grace and past it:
    // the wait stays, and says nothing more.
    fixture.scan(&[]);
    fixture.scan(&[]);
    assert_eq!(fixture.queued_prompts().len(), 1);
    assert_eq!(fixture.pending_waits().len(), 1);
    // Readable again: still followed until its record reaches an outcome.
    fs::write(&results, &readable).unwrap();
    fixture.scan(&[]);
    assert_eq!(fixture.queued_prompts().len(), 1);
    assert_eq!(fixture.pending_waits().len(), 1, "kept for the record");
    // The record settles: the wait names it.
    settle(&fixture, RUN);
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(
        prompts[1].contains(&format!("- `{RUN}`: settled; Engram record(s) `{RECORD}`")),
        "{}",
        prompts[1]
    );
    assert!(fixture.pending_waits().is_empty());
}
