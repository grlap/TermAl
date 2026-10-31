//! Registration-only admission and delayed indexing through the existing wait owner.
//! All runs and durable state are disposable fixtures; no launcher or live store is used.
use super::*;

fn durable_run(fixture: &WaitFixture, run_id: &str) -> PathBuf {
    let directory = fixture.write(run_id, Some(running(std::process::id())));
    let path = directory.join("request.json");
    let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    request["owner"] = json!(fixture.session);
    request["creatorPid"] = json!(std::process::id());
    fs::write(path, request.to_string()).unwrap();
    fixture.state.register_test_run(&fixture.session, RegisterTestRunRequest {
        run_id: run_id.to_owned(),
        run_directory: test_run_display_path(&directory),
    }).unwrap();
    assert!(fixture.state.inner.lock().unwrap().test_runs.find(run_id).is_none());
    directory
}

fn finish(directory: &FsPath, run_id: &str, mut results: Value) {
    results["runId"] = json!(run_id);
    fs::write(directory.join("results.json"), results.to_string()).unwrap();
}

#[test]
fn registered_runs_wait_without_rescanning_then_deliver_any_or_all_once() {
    for mode in ["any", "all"] {
        let fixture = WaitFixture::new(&format!("registered-{mode}"));
        fixture.scan(&[]);
        let first = durable_run(&fixture, "test-first");
        let second = durable_run(&fixture, "test-second");
        // Registration precedes publication of results. Absence alone is
        // pending, unlike an existing unreadable results artifact.
        fs::remove_file(second.join("results.json")).unwrap();
        fixture.busy();
        let response = fixture.state.create_test_run_wait_with(
            &fixture.session,
            serde_json::from_value(json!({"runIds":["test-first", "test-second"], "mode":mode})).unwrap(),
            &|| panic!("a durable registration does not depend on a forced scan"),
        ).unwrap();
        assert!(!response.resume_prompt_queued);
        assert_eq!(fixture.pending_waits().len(), 1);
        assert!(response.wait.runs.iter().all(|label| label.registration_backed
            && label.request_identity.is_some() && label.locator_identity.is_some()));
        fixture.state.refresh_test_run_waits();
        assert!(fixture.queued_prompts().is_empty(), "index lag alone is pending");
        let restored = load_state(fixture.state.persistence_path.as_path()).unwrap().unwrap();
        assert_eq!(serde_json::to_value(&restored.test_run_waits).unwrap(),
            serde_json::to_value(fixture.pending_waits()).unwrap());

        finish(&first, "test-first", passed());
        fixture.scan(&[std::process::id()]);
        if mode == "all" {
            assert!(fixture.queued_prompts().is_empty());
            assert_eq!(fixture.pending_waits().len(), 1);
        } else {
            let prompts = fixture.queued_prompts();
            assert_eq!(prompts.len(), 1);
            assert!(prompts[0].contains("- `test-first`: PASS"));
            assert!(prompts[0].contains("- `test-second`: NOT SETTLED YET"));
        }
        finish(&second, "test-second", failed("fixture terminal failure"));
        fixture.scan(&[]);
        let prompts = fixture.queued_prompts();
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("- `test-first`: PASS"));
        if mode == "all" {
            assert!(prompts[0].contains("- `test-second`: FAIL"));
            assert!(prompts[0].contains("fixture terminal failure"));
        }
        assert!(fixture.pending_waits().is_empty());
        fixture.state.refresh_test_run_waits();
        assert_eq!(fixture.queued_prompts(), prompts, "one resume for the original wait");
    }
}

#[test]
fn registered_run_loss_or_replacement_settles_as_named_unknown_before_indexing() {
    for change in ["missing-request", "malformed-results", "malformed-results-index", "retired", "request", "locator", "index"] {
        let fixture = WaitFixture::new(&format!("registered-loss-{change}"));
        fixture.scan(&[]);
        let directory = durable_run(&fixture, "test-lost");
        fixture.busy();
        let response = fixture.register(&["test-lost"], "all").unwrap();
        assert!(!response.resume_prompt_queued);
        let expected = match change {
            "missing-request" => {
                fs::remove_file(directory.join("request.json")).unwrap();
                "registered artifacts unreadable"
            }
            "malformed-results" | "malformed-results-index" => {
                fs::write(directory.join("results.json"), "{malformed").unwrap();
                if change == "malformed-results-index" {
                    fixture.state.refresh_test_runs_with(&|_, _| false, &|_, _| {});
                }
                "registered artifacts unreadable"
            }
            "retired" => {
                let mut inner = fixture.state.inner.lock().unwrap();
                inner.test_run_registrations.remove("test-lost");
                inner.test_runs.registered.remove("test-lost");
                "registration retired"
            }
            "locator" => {
                fixture.state.inner.lock().unwrap().test_run_registrations
                    .get_mut("test-lost").unwrap().locator_identity = Some("replacement".into());
                "registered identity changed"
            }
            "request" | "index" => {
                let path = directory.join("request.json");
                let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                request["started"] = json!("2026-09-26T11:00:00.000Z");
                fs::write(path, request.to_string()).unwrap();
                if change == "index" {
                    finish(&directory, "test-lost", passed());
                    fixture.state.refresh_test_runs_with(&|_, _| false, &|_, _| {});
                }
                "registered identity changed"
            }
            _ => unreachable!(),
        };
        fixture.state.refresh_test_run_waits();
        if ["missing-request", "malformed-results", "malformed-results-index"].contains(&change) {
            assert!(fixture.queued_prompts().is_empty(), "one unreadable refresh gets grace: {change}");
            fixture.state.refresh_test_run_waits();
        }
        let prompts = fixture.queued_prompts();
        assert_eq!(prompts.len(), 1, "{change}");
        assert!(prompts[0].contains(&format!("UNKNOWN ({expected})")), "{}", prompts[0]);
        assert!(!prompts[0].contains("- Verdict: PASS"));
        assert!(fixture.pending_waits().is_empty());
        fixture.state.refresh_test_run_waits();
        assert_eq!(fixture.queued_prompts(), prompts);
    }
}

#[test]
fn registration_without_durable_identity_is_typed_and_foreign_projects_stay_refused() {
    for missing in ["durability", "locator", "request", "project", "foreign", "indexed-conflict"] {
        let fixture = WaitFixture::new(&format!("registered-refusal-{missing}"));
        fixture.scan(&[]);
        durable_run(&fixture, "test-incomplete");
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            if missing == "durability" {
                inner.test_runs.registered.remove("test-incomplete");
            } else if missing != "indexed-conflict" {
                let mut row = inner.test_run_registrations.get_mut("test-incomplete").unwrap();
                match missing {
                    "locator" => row.locator_identity = None,
                    "request" => row.request_identity.clear(),
                    "project" => row.project_id = None,
                    "foreign" => row.project_id = Some("another-project".into()),
                    _ => unreachable!(),
                }
            }
        }
        if missing == "indexed-conflict" {
            let path = fixture.runs_dir().join("test-incomplete").join("request.json");
            let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            request["started"] = json!("2026-09-26T11:00:00.000Z");
            fs::write(path, request.to_string()).unwrap();
            fixture.state.refresh_test_runs_with(&|_, _| true, &|_, _| {});
        }
        let error = fixture.register(&["test-incomplete"], "all").err().unwrap();
        if missing == "foreign" {
            assert_eq!(error.status, StatusCode::BAD_REQUEST);
            assert!(error.message.contains("not in the waiting session's project"));
        } else if missing == "indexed-conflict" {
            assert_eq!(error.status, StatusCode::BAD_REQUEST);
            assert!(error.message.contains("index conflicts with its durable registration"));
        } else {
            assert_eq!(error.status, StatusCode::CONFLICT);
            assert_eq!(error.kind, Some(ApiErrorKind::TestRunWaitRegistrationUnavailable));
            assert!(error.message.starts_with("test_run_wait_registration_unavailable:"));
        }
        assert!(fixture.pending_waits().is_empty());
    }
}

#[test]
fn stop_consumes_a_registration_only_wait_before_the_index_arrives() {
    let fixture = WaitFixture::new("registered-stop");
    fixture.scan(&[]);
    let directory = durable_run(&fixture, "test-stop");
    fixture.busy();
    fixture.register(&["test-stop"], "all").unwrap();
    fixture.state.stop_session(&fixture.session).unwrap();
    assert!(fixture.pending_waits().is_empty());
    finish(&directory, "test-stop", passed());
    fixture.scan(&[]);
    assert!(fixture.queued_prompts().iter().all(|prompt| !prompt.contains("test-stop")));
}

#[test]
fn registration_is_rechecked_after_off_lock_artifact_reads_are_picked() {
    let fixture = WaitFixture::new("registered-retire-race");
    fixture.scan(&[]);
    durable_run(&fixture, "test-retire");
    fixture.busy();
    fixture.register(&["test-retire"], "all").unwrap();
    fixture.state.refresh_test_run_waits_pausing(&|| {
        fixture.state.inner.lock().unwrap().test_run_registrations.remove("test-retire");
    });
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("UNKNOWN (registration retired)"));
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn registered_settlement_regression_preserves_published_results_loss() {
    let fixture = WaitFixture::new("registered-published-loss");
    fixture.scan(&[]);
    let directory = durable_run(&fixture, "test-published");
    fixture.busy();
    fixture.register(&["test-published"], "all").unwrap();
    // Admission's refresh has seen the published running results without indexing.
    fs::remove_file(directory.join("results.json")).unwrap();
    fixture.state.refresh_test_run_waits();
    assert!(fixture.queued_prompts().is_empty(), "first failed read gets grace");
    let restored = load_state(fixture.state.persistence_path.as_path()).unwrap().unwrap();
    assert_eq!(serde_json::to_value(&restored.test_run_waits).unwrap(),
        serde_json::to_value(fixture.pending_waits()).unwrap());
    fixture.state.inner.lock().unwrap().test_run_waits = restored.test_run_waits;
    // A first request-only scan must not erase the earlier publication/loss.
    fixture.scan(&[std::process::id()]);
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("UNKNOWN (registered artifacts unreadable)"));
    assert!(fixture.pending_waits().is_empty());
    fixture.state.refresh_test_run_waits();
    assert_eq!(fixture.queued_prompts(), prompts);
}

#[test]
fn registered_settlement_regression_preserves_wait_through_transient_reads() {
    for artifact in ["request.json", "results.json"] {
        let fixture = WaitFixture::new(&format!("registered-transient-{artifact}"));
        fixture.scan(&[]);
        let directory = durable_run(&fixture, "test-transient");
        fixture.busy();
        fixture.register(&["test-transient"], "all").unwrap();
        let path = directory.join(artifact);
        let good = fs::read(&path).unwrap();
        for _ in 0..2 {
            fs::write(&path, "{malformed").unwrap();
            fixture.state.refresh_test_run_waits();
            assert!(fixture.queued_prompts().is_empty(), "a single failure must not consume: {artifact}");
            assert_eq!(fixture.pending_waits().len(), 1);
            fs::write(&path, &good).unwrap();
            fixture.state.refresh_test_run_waits();
            assert!(fixture.queued_prompts().is_empty(), "a good read resets the failure streak");
        }
        finish(&directory, "test-transient", passed());
        fixture.scan(&[]);
        let prompts = fixture.queued_prompts();
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("- `test-transient`: PASS"));
        assert!(!prompts[0].contains("UNKNOWN"));
    }
}

#[test]
fn registered_settlement_regression_hands_healthy_indexing_to_existing_verdict_rules() {
    let fixture = WaitFixture::new("registered-index-handoff");
    fixture.scan(&[]);
    let directory = durable_run(&fixture, "test-indexed");
    fixture.busy();
    fixture.register(&["test-indexed"], "all").unwrap();
    fixture.scan(&[std::process::id()]);
    fixture.register(&["test-indexed"], "all").unwrap();
    // Reload the persisted wait progress; both admission kinds now use the
    // indexed policy even after its one-read cache grace is exhausted.
    let restored = load_state(fixture.state.persistence_path.as_path()).unwrap().unwrap();
    fixture.state.inner.lock().unwrap().test_run_waits = restored.test_run_waits;
    fs::write(directory.join("results.json"), "{malformed").unwrap();
    fixture.scan(&[std::process::id()]);
    fixture.scan(&[std::process::id()]);
    assert!(fixture.queued_prompts().is_empty(), "indexed unreadability is nonterminal for both waits");
    assert_eq!(fixture.pending_waits().len(), 2);
    finish(&directory, "test-indexed", passed());
    fixture.scan(&[]);
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2);
    assert!(prompts.iter().all(|prompt| prompt.contains("- `test-indexed`: PASS") && !prompt.contains("UNKNOWN")));
    assert!(fixture.pending_waits().is_empty());
    fixture.state.refresh_test_run_waits();
    assert_eq!(fixture.queued_prompts(), prompts);
}

#[test]
fn a_changed_native_target_with_identical_request_cannot_satisfy_a_registered_wait() {
    let fixture = WaitFixture::new("registered-native-target");
    fixture.scan(&[]);
    let directory = durable_run(&fixture, "test-native");
    fixture.busy();
    let response = fixture.register(&["test-native"], "all").unwrap();
    let original_request = fs::read(directory.join("request.json")).unwrap();
    let relocated = fixture.runs_dir().join("test-relocated");
    // Both paths are in this disposable fixture. A same-path recreation
    // would retain the canonical-path identity, so change the actual target.
    assert!(directory.starts_with(&fixture.root) && relocated.starts_with(&fixture.root));
    fs::rename(&directory, &relocated).unwrap();
    // Use native paths for Windows mklink, which parses forward slashes as switches.
    let native_parent = fs::canonicalize(directory.parent().unwrap()).unwrap();
    crate::tests::engram_child_temp::link_directory(
        &native_parent.join("test-native"), &native_parent.join("test-relocated"),
    );
    assert_eq!(fs::read(directory.join("request.json")).unwrap(), original_request);
    let replacement = TestRunDisk::read(&directory, None).unwrap();
    assert_eq!(replacement.request_identity, response.wait.runs[0].request_identity);
    assert_ne!(replacement.locator_identity, response.wait.runs[0].locator_identity);
    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("UNKNOWN (registered identity changed)"));
    assert!(fixture.pending_waits().is_empty());
    fixture.state.refresh_test_run_waits();
    assert_eq!(fixture.queued_prompts(), prompts);
    #[cfg(windows)]
    fs::remove_dir(&directory).unwrap();
    #[cfg(unix)]
    fs::remove_file(&directory).unwrap();
}

#[test]
fn an_off_lock_read_cannot_reapply_a_failure_streak_after_recovery() {
    let fixture = WaitFixture::new("registered-read-aba");
    fixture.scan(&[]);
    let directory = durable_run(&fixture, "test-read-race");
    fixture.busy();
    fixture.register(&["test-read-race"], "all").unwrap();
    let results = directory.join("results.json");
    let good = fs::read(&results).unwrap();
    fs::write(&results, "{malformed").unwrap();
    fixture.state.refresh_test_run_waits();
    assert!(fixture.queued_prompts().is_empty());
    fixture.state.refresh_test_run_waits_pausing(&|| {
        fs::write(&results, &good).unwrap();
        fixture.state.refresh_test_run_waits();
        fs::write(&results, "{malformed").unwrap();
        fixture.state.refresh_test_run_waits();
    });
    assert!(fixture.queued_prompts().is_empty(), "the intervening recovery broke the old streak");
    finish(&directory, "test-read-race", passed());
    fixture.scan(&[]);
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("- `test-read-race`: PASS"));
}

#[test]
fn registered_ordering_regression_success_fences_an_older_completed_failure() {
    let fixture = WaitFixture::new("registered-noop-success-fence");
    fixture.scan(&[]);
    let directory = durable_run(&fixture, "test-noop-fence");
    fixture.busy();
    fixture.register(&["test-noop-fence"], "all").unwrap();
    let results = directory.join("results.json");
    let good = fs::read(&results).unwrap();
    fs::write(&results, "{malformed").unwrap();
    let state = fixture.state.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
    let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(0);
    let older_refresh = std::thread::spawn(move || {
        state.refresh_test_run_waits_with_hooks(&|| {}, &|| {
            ready_tx.send(()).unwrap();
            resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        });
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // A's failure read is complete. B now reads a good file, without changing
    // the durable results-seen/zero-failure state, before A can lock again.
    fs::write(&results, &good).unwrap();
    let revision = fixture.state.inner.lock().unwrap().revision;
    fixture.state.refresh_test_run_waits();
    assert_eq!(fixture.state.inner.lock().unwrap().revision, revision);
    resume_tx.send(()).unwrap();
    older_refresh.join().unwrap();
    fs::write(&results, "{malformed").unwrap();
    fixture.state.refresh_test_run_waits();
    assert!(fixture.queued_prompts().is_empty(), "the good B read separated the two failed reads");
    assert_eq!(fixture.pending_waits().len(), 1);
    finish(&directory, "test-noop-fence", passed());
    fixture.scan(&[]);
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("- `test-noop-fence`: PASS"));
}

#[test]
fn registered_ordering_regression_checks_registration_before_first_index_handoff() {
    for change in ["retired", "replacement"] {
        let fixture = WaitFixture::new(&format!("registered-handoff-{change}"));
        fixture.scan(&[]);
        let directory = durable_run(&fixture, "test-first-index");
        fixture.busy();
        fixture.register(&["test-first-index"], "all").unwrap();
        fixture.state.refresh_test_run_waits_pausing(&|| {
            {
                let mut inner = fixture.state.inner.lock().unwrap();
                if change == "retired" {
                    inner.test_run_registrations.remove("test-first-index");
                    inner.test_runs.registered.remove("test-first-index");
                } else {
                    inner.test_run_registrations.get_mut("test-first-index").unwrap()
                        .locator_identity = Some("replacement".into());
                }
            }
            finish(&directory, "test-first-index", passed());
            fixture.state.refresh_test_runs_with(&|_, _| false, &|_, _| {});
        });
        let prompts = fixture.queued_prompts();
        assert_eq!(prompts.len(), 1);
        let reason = if change == "retired" { "registration retired" } else { "registered identity changed" };
        assert!(prompts[0].contains(&format!("UNKNOWN ({reason})")), "{}", prompts[0]);
        assert!(!prompts[0].contains("- `test-first-index`: PASS"));
        assert!(fixture.pending_waits().is_empty());
        fixture.state.refresh_test_run_waits();
        assert_eq!(fixture.queued_prompts(), prompts);
    }
}

#[test]
fn a_settled_registration_unknown_does_not_commit_on_each_all_wait_refresh() {
    let fixture = WaitFixture::new("registered-bounded-progress");
    fixture.scan(&[]);
    let unknown = durable_run(&fixture, "test-unknown");
    let running = durable_run(&fixture, "test-running");
    fixture.busy();
    fixture.register(&["test-unknown", "test-running"], "all").unwrap();
    fs::write(unknown.join("results.json"), "{malformed").unwrap();
    fixture.scan(&[std::process::id()]);
    fixture.state.refresh_test_run_waits();
    let revision = fixture.state.inner.lock().unwrap().revision;
    for _ in 0..3 {
        fixture.state.refresh_test_run_waits();
        assert_eq!(fixture.state.inner.lock().unwrap().revision, revision,
            "unchanged UNKNOWN plus running must not publish more snapshots");
    }
    assert!(fixture.queued_prompts().is_empty());
    finish(&running, "test-running", passed());
    fixture.scan(&[]);
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("UNKNOWN (registered artifacts unreadable)"));
    assert!(prompts[0].contains("- `test-running`: PASS"));
}

#[test]
fn registered_planning_regression_unknown_and_fail_resume_with_diagnostics_once() {
    for available in [true, false] {
        let fixture = WaitFixture::new(&format!("registered-unknown-fail-{available}"));
        fixture.scan(&[]);
        let unknown = durable_run(&fixture, "test-unknown");
        let failure = durable_run(&fixture, "test-failure");
        fixture.busy();
        fixture.register(&["test-unknown", "test-failure"], "all").unwrap();
        fs::write(unknown.join("results.json"), "{malformed").unwrap();
        finish(&failure, "test-failure", failed("fixture terminal failure"));
        // Publish both summaries without refreshing waits: the registration
        // observation below must still receive its first-read grace.
        fixture.state.refresh_test_runs_with(&|_, _| true, &|_, _| {});
        fixture.state.refresh_test_run_waits();
        assert!(fixture.queued_prompts().is_empty(), "planning cannot settle within grace");
        assert_eq!(fixture.pending_waits().len(), 1);
        fixture.state.refresh_test_run_waits_pausing(&|| {
            if !available {
                // The indexed FAIL still exists, but its bounded read now
                // answers Unavailable. That answer must not defer forever.
                fs::write(failure.join("results.json"), "{malformed").unwrap();
            }
        });
        let prompts = fixture.queued_prompts();
        assert_eq!(prompts.len(), 1, "UNKNOWN plus FAIL must resume after grace");
        assert!(prompts[0].contains("UNKNOWN (registered artifacts unreadable)"));
        assert!(prompts[0].contains("- `test-failure`: FAIL"));
        if available {
            assert!(prompts[0].contains("fixture terminal failure"));
        } else {
            assert!(prompts[0].contains("not available here"), "{}", prompts[0]);
        }
        assert!(fixture.pending_waits().is_empty());
        let revision = fixture.state.inner.lock().unwrap().revision;
        for _ in 0..3 {
            fixture.state.refresh_test_run_waits();
            assert_eq!(fixture.queued_prompts(), prompts, "exactly one resume");
            assert_eq!(fixture.state.inner.lock().unwrap().revision, revision);
        }
    }
}

#[test]
fn planning_keeps_handed_off_unreadable_runs_pending_with_a_failed_sibling() {
    for mode in ["all", "any"] {
        let fixture = WaitFixture::new(&format!("registered-indexed-fail-{mode}"));
        fixture.scan(&[]);
        let unreadable = durable_run(&fixture, "test-unreadable");
        let failure = durable_run(&fixture, "test-failure");
        fixture.busy();
        fixture.register(&["test-unreadable", "test-failure"], mode).unwrap();
        fixture.scan(&[std::process::id()]);
        assert!(fixture.pending_waits()[0].runs.iter()
            .all(|label| label.registration_progress.indexed));
        fs::write(unreadable.join("results.json"), "{malformed").unwrap();
        finish(&failure, "test-failure", failed("indexed sibling failure"));
        // Exhaust the index's grace before letting the wait observe FAIL.
        for _ in 0..2 {
            fixture.state.refresh_test_runs_with(&|_, _| true, &|_, _| {});
        }
        fixture.state.refresh_test_run_waits();
        if mode == "all" {
            assert!(fixture.queued_prompts().is_empty(), "no post-handoff UNKNOWN override");
            assert_eq!(fixture.pending_waits().len(), 1);
            finish(&unreadable, "test-unreadable", passed());
            fixture.scan(&[]);
        }
        let prompts = fixture.queued_prompts();
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("indexed sibling failure"));
        assert!(prompts[0].contains(if mode == "all" {
            "- `test-unreadable`: PASS"
        } else {
            "- `test-unreadable`: NOT SETTLED YET"
        }));
        assert!(fixture.pending_waits().is_empty());
        fixture.state.refresh_test_run_waits();
        assert_eq!(fixture.queued_prompts(), prompts);
    }
}

#[test]
fn deterministic_registration_artifact_loss_needs_no_grace_refresh() {
    for change in ["directory", "request.json", "results.json"] {
        let fixture = WaitFixture::new(&format!("registered-deterministic-{change}"));
        fixture.scan(&[]);
        let directory = durable_run(&fixture, "test-deterministic");
        fixture.busy();
        fixture.register(&["test-deterministic"], "all").unwrap();
        if change == "directory" {
            let relocated = fixture.runs_dir().join("test-displaced");
            assert!(directory.starts_with(&fixture.root) && relocated.starts_with(&fixture.root));
            fs::rename(&directory, relocated).unwrap();
            assert!(!directory.exists(), "the original directory is gone");
        } else {
            fs::write(directory.join(change), vec![b' '; TEST_RUN_JSON_MAX_BYTES + 1]).unwrap();
        }
        fixture.state.refresh_test_run_waits();
        let prompts = fixture.queued_prompts();
        assert_eq!(prompts.len(), 1, "{change} must settle on the first refresh");
        assert!(prompts[0].contains("UNKNOWN (registered artifacts unreadable)"));
        assert!(fixture.pending_waits().is_empty());
        fixture.state.refresh_test_run_waits();
        assert_eq!(fixture.queued_prompts(), prompts);
    }
}
