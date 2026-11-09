// Retained-history, scan-bound and between-read selection regressions.
use super::*;

fn passed_baseline() -> (AppState, String, String, String, TestRunRegistration) {
    let (state, parent, _store, named_root) = named_root_parent();
    let head = commit_named_root(&named_root);
    register_committed_run(
        &state,
        &parent,
        &named_root,
        committed_results("passed", input_record(&head, "clean")),
    );
    state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .get_mut(COMMITTED_RUN_ID)
        .unwrap()
        .registered_at = "2026-10-30T09:59:58.000Z".to_owned();
    let registration = current_registration(&state);
    host_observed(&state, &registration, &head, acknowledged("settled"));
    assert_eq!(
        decide(&state, &parent),
        Ok(AcceptanceCommittedInputDecision::Admitted(
            AcceptanceCommittedInputAdmission {
                run_id: COMMITTED_RUN_ID.to_owned(),
                commit: head.clone()
            }
        )),
        "the older PASS admits before the new boundary exists"
    );
    let acknowledged = current_registration(&state);
    assert!(
        acknowledged.verification_record.is_some(),
        "the returned baseline includes the host's receipt"
    );
    (state, parent, named_root, head, acknowledged)
}

fn pending_results(run_id: &str, head: &str) -> Value {
    let mut results = committed_results("passed", input_record(head, "clean"));
    results["runId"] = json!(run_id);
    results["state"] = json!("running");
    results["ended"] = Value::Null;
    results["stages"] = json!([]);
    results["input"]["after"] = Value::Null;
    results["input"]["outcome"] = Value::Null;
    results["input"]["eligible"] = Value::Null;
    results
}

fn register_newer(
    state: &AppState,
    parent: &str,
    root: &str,
    results: Value,
) -> TestRunRegistration {
    let id = results["runId"].as_str().unwrap().to_owned();
    register_committed_run_as(state, parent, root, &id, results);
    state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .get_mut(&id)
        .unwrap()
        .registered_at = "2026-10-30T09:59:59.000Z".to_owned();
    state.inner.lock().unwrap().test_run_registrations[&id].clone()
}

#[test]
fn pending_results_are_observed_again_before_an_older_pass_is_selected() {
    let mut outcomes = Vec::new();
    for case in ["failed", "ineligible", "unacknowledged", "missing"] {
        let (state, parent, root, head, _) = passed_baseline();
        let transport = install_sighting_transport(&state, &root, 1, present_sighting());
        let newer = register_newer(
            &state,
            &parent,
            &root,
            pending_results("test-pending", &head),
        );
        let path = PathBuf::from(&newer.run_directory).join("results.json");
        let expected = match case {
            "missing" => AcceptanceCommittedInputRefusal::ScanIncomplete,
            "unacknowledged" => AcceptanceCommittedInputRefusal::RecordNotAcknowledged {
                run_id: newer.run_id.clone(),
                commit: head.clone(),
                state: "the host has received no receipt for it".to_owned(),
            },
            _ => AcceptanceCommittedInputRefusal::NoEligibleRun {
                commit: head.clone(),
            },
        };
        TEST_ACCEPTANCE_COMMITTED_INPUT_BEFORE_SELECTION.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                if case == "missing" {
                    fs::remove_file(path).unwrap();
                } else {
                    let mut results = committed_results(
                        if case == "failed" { "failed" } else { "passed" },
                        input_record(&head, "clean"),
                    );
                    results["runId"] = json!("test-pending");
                    results["input"]["eligible"] = json!(case == "unacknowledged");
                    fs::write(path, results.to_string()).unwrap();
                }
            }));
        });
        let decision = decide(&state, &parent);
        assert_eq!(sighting_reads(&transport), 0);
        outcomes.push((case, decision, Err(expected)));
    }
    // Execute every interleaving before reporting a mismatch.
    for (case, actual, expected) in &outcomes {
        eprintln!("pending transition {case}: {actual:?}");
        assert_eq!(actual, expected, "{case}; all outcomes: {outcomes:?}");
    }
}

#[test]
fn a_running_result_before_input_capture_does_not_supersede_a_terminal_pass() {
    let (state, parent, root, head, older) = passed_baseline();
    let transport = install_sighting_transport(&state, &root, 1, present_sighting());
    let mut running = pending_results("test-before-input", &head);
    running.as_object_mut().unwrap().remove("input");
    // The launcher's initial file lists planned stages as unrun before it
    // registers, then captures and writes input.
    running["stages"] = json!([{ "name": "rust-tests", "state": "unrun" }]);
    let newer = register_newer(&state, &parent, &root, running.clone());
    assert_eq!(
        decide(&state, &parent),
        Ok(AcceptanceCommittedInputDecision::Admitted(
            AcceptanceCommittedInputAdmission {
                run_id: older.run_id,
                commit: head
            }
        ))
    );
    for invalid in [Value::Null, json!({"kind":"commit", "cleanBefore":"bad"})] {
        running["input"] = invalid;
        fs::write(
            PathBuf::from(&newer.run_directory).join("results.json"),
            running.to_string(),
        )
        .unwrap();
        assert_eq!(
            decide(&state, &parent),
            Err(AcceptanceCommittedInputRefusal::ScanIncomplete),
            "a present but incoherent input is never the initial pending window"
        );
    }
    assert_eq!(sighting_reads(&transport), 0);
}

fn retain_registration(state: &AppState, newer: &TestRunRegistration) {
    state.refresh_test_runs();
    {
        let mut inner = state.inner.lock().unwrap();
        let entry = inner
            .test_runs
            .find(&newer.run_id)
            .expect("the artifact is indexed");
        // As the compactor does, copy the current host row, including its
        // acknowledgement; a pre-ack caller snapshot is not the survivor.
        let mut retained = inner.test_run_registrations[&newer.run_id].clone();
        assert_eq!(retained.verification_record, newer.verification_record);
        retained.stage_names.clear();
        let row = TestRunCompacted {
            compacted: true,
            request_identity: entry.disk.request_identity.clone(),
            request_stamp: entry.disk.request_stamp,
            results_stamp: entry.disk.results_stamp,
            summary: test_run_minimal_summary(entry.summary.clone()),
            registration: Some(retained),
            observations: Vec::new(),
            reason: "reader fixture of installed retained history".to_owned(),
            artifact_conflict: false,
            artifact_unavailable: false,
        };
        // Model the installed compaction snapshot, not the eligibility/cap
        // algorithm: the real compactor retains this row and its artifacts.
        inner.test_run_registrations.remove(&newer.run_id);
        inner.test_runs.registered.remove(&newer.run_id);
        inner
            .test_runs
            .entries
            .retain(|entry| entry.summary.run_id != newer.run_id);
        inner
            .test_runs
            .compactions_durable
            .insert(newer.run_id.clone());
        inner
            .test_run_compacted
            .insert(newer.run_id.clone(), row.clone());
        assert!(test_run_compact_is_installed(&*inner, &row));
    }
}

#[test]
fn a_compacted_newer_failure_still_supersedes_an_older_pass() {
    let (state, parent, root, head, older) = passed_baseline();
    let task = task_under(current_claim(1, older.work_binding.clone()));
    let deadline = || std::time::Instant::now() + Duration::from_secs(60);
    assert_eq!(
        decide_task(&state, &parent, &task, deadline()),
        Ok(AcceptanceCommittedInputDecision::Admitted(
            AcceptanceCommittedInputAdmission {
                run_id: older.run_id,
                commit: head.clone(),
            }
        )),
        "the supplied live work-run binding admits before a newer run exists"
    );
    let transport = install_sighting_transport(&state, &root, 1, present_sighting());
    let mut results = committed_results("failed", input_record(&head, "clean"));
    results["runId"] = json!("test-compacted-failure");
    results["input"]["eligible"] = json!(false);
    let newer = register_newer(&state, &parent, &root, results);
    assert_eq!(
        decide(&state, &parent),
        Err(AcceptanceCommittedInputRefusal::NoEligibleRun {
            commit: head.clone()
        }),
        "the active newer failure already refuses"
    );
    retain_registration(&state, &newer);
    assert!(
        PathBuf::from(&newer.run_directory)
            .join("results.json")
            .is_file()
    );
    assert_eq!(
        decide(&state, &parent),
        Err(AcceptanceCommittedInputRefusal::NoEligibleRun {
            commit: head.clone()
        }),
        "retention must not hide the newer failure"
    );
    let path = PathBuf::from(&newer.run_directory).join("results.json");
    fs::remove_file(path).unwrap();
    assert_eq!(
        decide(&state, &parent),
        Err(AcceptanceCommittedInputRefusal::ScanIncomplete),
        "an unreadable retained newer result cannot fall back to the older PASS"
    );
    state
        .inner
        .lock()
        .unwrap()
        .test_run_compacted
        .update(&newer.run_id, |row| {
            row.registration
                .as_mut()
                .unwrap()
                .work_binding
                .as_mut()
                .unwrap()
                .run_id = "another-work-run".to_owned();
        })
        .unwrap();
    assert_eq!(
        decide_task(&state, &parent, &task, deadline()),
        Ok(AcceptanceCommittedInputDecision::Admitted(
            AcceptanceCommittedInputAdmission {
                run_id: COMMITTED_RUN_ID.to_owned(),
                commit: head
            }
        )),
        "the live binding excludes retained rows of another work run"
    );
    assert_eq!(sighting_reads(&transport), 0);
}

#[test]
fn an_acknowledged_pass_survives_installed_compaction_before_or_during_the_read() {
    for during in [false, true] {
        let (state, parent, root, head, registration) = passed_baseline();
        let transport = install_sighting_transport(&state, &root, 1, present_sighting());
        if during {
            let state = state.clone();
            let registration = registration.clone();
            TEST_ACCEPTANCE_COMMITTED_INPUT_BEFORE_RECHECK.with(|slot| {
                *slot.borrow_mut() =
                    Some(Box::new(move || retain_registration(&state, &registration)));
            });
        } else {
            retain_registration(&state, &registration);
        }
        assert_eq!(
            decide(&state, &parent),
            Ok(AcceptanceCommittedInputDecision::Admitted(
                AcceptanceCommittedInputAdmission {
                    run_id: registration.run_id.clone(),
                    commit: head
                }
            )),
            "during={during}: the retained original binding and receipt still decide"
        );
        state
            .inner
            .lock()
            .unwrap()
            .test_run_compacted
            .remove(&registration.run_id);
        assert_eq!(
            decide(&state, &parent),
            Ok(AcceptanceCommittedInputDecision::Legacy),
            "during={during}: absent retained registration supplies no authority"
        );
        assert_eq!(sighting_reads(&transport), 0);
    }
}

// Matching registration clones have independent coherent result files, so
// Unknown cannot mask the reader's bounded-scan decision.
fn add_scan_rows(
    state: &AppState,
    prototype: &TestRunRegistration,
    head: &str,
    count: usize,
    started: bool,
) {
    for index in 0..count {
        let mut row = prototype.clone();
        row.run_id = format!("test-scan-{index:02}");
        row.run_directory = PathBuf::from(&prototype.run_directory)
            .with_file_name(&row.run_id)
            .to_string_lossy()
            .into_owned();
        row.registered_at = format!("2026-10-30T09:59:59.{index:03}Z");
        row.verification_record = None;
        let mut results = if started {
            committed_results("failed", input_record(&"f".repeat(40), "clean"))
        } else {
            never_started_results("refused dirty before the run", head)
        };
        results["runId"] = json!(row.run_id);
        fs::create_dir_all(&row.run_directory).unwrap();
        fs::write(
            PathBuf::from(&row.run_directory).join("results.json"),
            results.to_string(),
        )
        .unwrap();
        let mut inner = state.inner.lock().unwrap();
        inner.test_runs.registered.insert(row.run_id.clone());
        inner.test_run_registrations.insert(row.run_id.clone(), row);
    }
}

#[test]
fn a_truncated_scan_with_started_nonmatching_runs_is_incomplete() {
    let (state, parent, root, head, older) = passed_baseline();
    let transport = install_sighting_transport(&state, &root, 1, present_sighting());
    add_scan_rows(
        &state,
        &older,
        &head,
        ACCEPTANCE_COMMITTED_INPUT_CANDIDATES_MAX,
        true,
    );
    take_results_reads();
    assert_eq!(
        decide(&state, &parent),
        Err(AcceptanceCommittedInputRefusal::ScanIncomplete)
    );
    assert_eq!(
        take_results_reads(),
        ACCEPTANCE_COMMITTED_INPUT_CANDIDATES_MAX * 2
    );
    assert_eq!(sighting_reads(&transport), 0);
}

#[test]
fn exactly_the_bound_of_coherent_never_started_runs_allows_legacy() {
    let (state, parent, root, head, prototype) = passed_baseline();
    let transport = install_sighting_transport(&state, &root, 1, present_sighting());
    state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .remove(&prototype.run_id);
    add_scan_rows(
        &state,
        &prototype,
        &head,
        ACCEPTANCE_COMMITTED_INPUT_CANDIDATES_MAX,
        false,
    );
    take_results_reads();
    assert_eq!(
        decide(&state, &parent),
        Ok(AcceptanceCommittedInputDecision::Legacy)
    );
    assert_eq!(
        take_results_reads(),
        ACCEPTANCE_COMMITTED_INPUT_CANDIDATES_MAX
    );
    assert_eq!(sighting_reads(&transport), 0);
}
