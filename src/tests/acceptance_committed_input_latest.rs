// Selection regressions for the early committed-input reader, using its
// existing registration, receipt and public-request fixtures.
use super::*;

#[test]
fn the_newest_terminal_result_at_the_checked_commit_decides() {
    for case in [
        "newer failure",
        "newer pass",
        "newer unacknowledged pass",
        "newer ineligible pass",
        "newer failure on another commit",
        "newer failure of another work run",
        "newer failure under another root generation",
        "unfinished newer run",
        "newer missing results",
        "newer malformed results",
        "newer oversized results",
        "newer incoherent input",
        "newer wrong run id",
        "newer results disappear during selection",
        "newer run id changes during selection",
        "older missing results",
        "newer run before any stage",
    ] {
        let (state, parent, _store, named_root) = named_root_parent();
        let head = commit_named_root(&named_root);
        let transport =
            install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
        let older = register_committed_run(
            &state,
            &parent,
            &named_root,
            committed_results("passed", input_record(&head, "clean")),
        );
        // Establish fixture order before the receipt freezes this metadata.
        state
            .inner
            .lock()
            .unwrap()
            .test_run_registrations
            .get_mut(&older.run_id)
            .unwrap()
            .registered_at = "2026-10-30T09:59:58.000Z".to_owned();
        let older = current_registration(&state);
        host_observed(&state, &older, &head, acknowledged("settled"));
        assert_eq!(
            decide(&state, &parent),
            Ok(AcceptanceCommittedInputDecision::Admitted(
                AcceptanceCommittedInputAdmission {
                    native_source: None,
                    run_id: COMMITTED_RUN_ID.to_owned(),
                    commit: head.clone(),
                }
            )),
            "{case}: the older receipt must admit before a newer result exists"
        );

        let newer_id = "test-newer-terminal-result";
        let newer_commit = if case == "newer failure on another commit" {
            "f".repeat(40)
        } else {
            head.clone()
        };
        let mut results = committed_results(
            if matches!(
                case,
                "newer pass"
                    | "newer unacknowledged pass"
                    | "newer ineligible pass"
                    | "older missing results"
            ) {
                "passed"
            } else {
                "failed"
            },
            input_record(&newer_commit, "clean"),
        );
        results["input"]["eligible"] = json!(matches!(
            case,
            "newer pass" | "newer unacknowledged pass" | "older missing results"
        ));
        results["runId"] = json!(newer_id);
        if matches!(case, "unfinished newer run" | "newer run before any stage") {
            results["state"] = json!("running");
            results["ended"] = Value::Null;
            results["stages"][0]["state"] = json!("running");
            results["stages"][0]["ended"] = Value::Null;
            if case == "newer run before any stage" {
                results["stages"][0]["started"] = Value::Null;
                results["input"]["after"] = Value::Null;
                results["input"]["outcome"] = Value::Null;
                results["input"]["eligible"] = Value::Null;
            }
        }
        register_committed_run_as(&state, &parent, &named_root, newer_id, results);
        {
            let mut inner = state.inner.lock().unwrap();
            let mut newer = inner.test_run_registrations.get_mut(newer_id).unwrap();
            newer.registered_at = "2026-10-30T09:59:59.000Z".to_owned();
            if case == "newer failure of another work run" {
                newer.work_binding.as_mut().unwrap().run_id = "another-work-run".to_owned();
            }
            if case == "newer failure under another root generation" {
                newer.named_root.as_mut().unwrap().generation += 1;
            }
        }
        if matches!(case, "newer pass" | "older missing results") {
            let newer = state.inner.lock().unwrap().test_run_registrations[newer_id].clone();
            host_observed(&state, &newer, &head, acknowledged("received"));
        }
        let newer_results = PathBuf::from(
            &state.inner.lock().unwrap().test_run_registrations[newer_id].run_directory,
        )
        .join("results.json");
        match case {
            "newer missing results" => fs::remove_file(&newer_results).unwrap(),
            "newer malformed results" => fs::write(&newer_results, "{").unwrap(),
            "newer oversized results" => {
                fs::write(&newer_results, vec![b' '; TEST_RUN_JSON_MAX_BYTES + 1]).unwrap();
            }
            "older missing results" => {
                fs::remove_file(PathBuf::from(&older.run_directory).join("results.json")).unwrap();
            }
            "newer results disappear during selection"
            | "newer run id changes during selection" => {
                TEST_ACCEPTANCE_COMMITTED_INPUT_BEFORE_SELECTION.with(|slot| {
                    *slot.borrow_mut() = Some(Box::new(move || {
                        if case == "newer results disappear during selection" {
                            fs::remove_file(&newer_results).unwrap();
                        } else {
                            let mut value: Value =
                                serde_json::from_slice(&fs::read(&newer_results).unwrap()).unwrap();
                            value["runId"] = json!("changed-between-reads");
                            fs::write(&newer_results, value.to_string()).unwrap();
                        }
                    }));
                });
            }
            "newer incoherent input" | "newer wrong run id" => {
                let mut value: Value =
                    serde_json::from_slice(&fs::read(&newer_results).unwrap()).unwrap();
                if case == "newer incoherent input" {
                    value["input"]["after"]["commit"] = json!("a".repeat(40));
                } else {
                    value["runId"] = json!("not-the-registered-run");
                }
                fs::write(&newer_results, value.to_string()).unwrap();
            }
            _ => {}
        }
        let task = task_under(current_claim(1, older.work_binding.clone()));
        let decision = decide_task(
            &state,
            &parent,
            &task,
            std::time::Instant::now() + Duration::from_secs(60),
        );
        if matches!(
            case,
            "newer missing results"
                | "newer malformed results"
                | "newer oversized results"
                | "newer incoherent input"
                | "newer wrong run id"
                | "newer results disappear during selection"
                | "newer run id changes during selection"
        ) {
            assert_refused_without_a_sighting(
                &transport,
                decision,
                "committed_input_scan_incomplete",
                case,
            );
        } else if matches!(case, "newer failure" | "newer ineligible pass") {
            assert_refused_without_a_sighting(
                &transport,
                decision,
                "committed_input_no_eligible_run",
                case,
            );
        } else if case == "newer unacknowledged pass" {
            assert_refused_without_a_sighting(
                &transport,
                decision,
                "verification_record_not_acknowledged",
                case,
            );
        } else {
            assert_eq!(
                decision,
                Ok(AcceptanceCommittedInputDecision::Admitted(
                    AcceptanceCommittedInputAdmission {
                        native_source: None,
                        run_id: if matches!(case, "newer pass" | "older missing results") {
                            newer_id
                        } else {
                            COMMITTED_RUN_ID
                        }
                        .to_owned(),
                        commit: head,
                    }
                )),
                "{case}"
            );
            assert_eq!(sighting_reads(&transport), 0, "{case}");
        }
    }
}

#[test]
fn recovery_on_the_same_work_run_keeps_the_original_registered_evidence() {
    let (state, parent, _store, named_root) = named_root_parent();
    let head = commit_named_root(&named_root);
    let transport = install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
    let registration = register_committed_run(
        &state,
        &parent,
        &named_root,
        committed_results("passed", input_record(&head, "clean")),
    );
    host_observed(&state, &registration, &head, acknowledged("settled"));
    assert_eq!(
        decide(&state, &parent),
        Ok(AcceptanceCommittedInputDecision::Admitted(
            AcceptanceCommittedInputAdmission {
                native_source: None,
                run_id: COMMITTED_RUN_ID.to_owned(),
                commit: head.clone(),
            }
        )),
        "the original receipt must admit before recovery"
    );
    let original = registration.work_binding.clone().unwrap();
    let mut recovered = original.clone();
    recovered.claim_fence += 1;
    recovered.work_revision += 1;
    let task = task_under(current_claim(
        recovered.claim_fence,
        Some(recovered.clone()),
    ));
    assert_eq!(
        decide_task(
            &state,
            &parent,
            &task,
            std::time::Instant::now() + Duration::from_secs(60)
        ),
        Ok(AcceptanceCommittedInputDecision::Admitted(
            AcceptanceCommittedInputAdmission {
                native_source: None,
                run_id: COMMITTED_RUN_ID.to_owned(),
                commit: head.clone(),
            }
        )),
        "same work run, original registered binding, recovered claim fence"
    );
    let reads = independent_reader();
    let recovered_for_reader = recovered.clone();
    let response = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(Some(Agent::Codex)),
            move |connection, args, timeout| {
                if args.iter().any(|arg| arg == "held") {
                    Ok(
                        json!({"items":[{"work_id":SIGHTED_WORK_ID,"short_ref":"w-task",
                    "claim_id":"claim-task","claim_fence":recovered_for_reader.claim_fence,
                    "control_binding":recovered_for_reader}],"omitted":0}),
                    )
                } else {
                    reads(connection, args, timeout)
                }
            },
        )
        .unwrap_or_else(|error| {
            panic!(
                "recovery must not read a legacy sighting: {}",
                error.message
            )
        });
    assert!(matches!(
        response,
        AcceptanceEvaluationRequestResponse::Spawned { .. }
    ));
    assert_eq!(sighting_reads(&transport), 0);
    assert_eq!(
        current_registration(&state).work_binding.as_ref(),
        Some(&original),
        "the registered evidence must never be relabelled to the new fence"
    );

    for field in ["work", "run", "root execution", "claim"] {
        let mut other = recovered.clone();
        match field {
            "work" => other.work_id = "another-work".to_owned(),
            "run" => other.run_id = "another-run".to_owned(),
            "root execution" => other.root_execution_id = "another-root".to_owned(),
            "claim" => other.claim_id = "another-claim".to_owned(),
            _ => unreachable!(),
        }
        assert_eq!(
            decide_task(
                &state,
                &parent,
                &task_under(current_claim(other.claim_fence, Some(other))),
                std::time::Instant::now() + Duration::from_secs(60)
            ),
            Ok(AcceptanceCommittedInputDecision::Legacy),
            "a different {field} cannot inherit the registration"
        );
    }
    for case in [
        "regressed fence",
        "regressed revision",
        "live/current fence disagreement",
    ] {
        let mut other = recovered.clone();
        match case {
            "regressed fence" => other.claim_fence = original.claim_fence - 1,
            "regressed revision" => other.work_revision = original.work_revision - 1,
            _ => {}
        }
        let current_fence =
            other.claim_fence + i64::from(case == "live/current fence disagreement");
        assert_eq!(
            decide_task(
                &state,
                &parent,
                &task_under(current_claim(current_fence, Some(other))),
                std::time::Instant::now() + Duration::from_secs(60)
            ),
            Ok(AcceptanceCommittedInputDecision::Legacy),
            "{case}",
        );
    }
}

#[test]
fn a_new_naming_generation_does_not_reuse_the_original_evidence() {
    let (state, parent, store, named_root) = named_root_parent();
    let head = commit_named_root(&named_root);
    let transport = install_sighting_transport(&state, &named_root, 2, json!({"state":"absent"}));
    let registration = register_committed_run(
        &state,
        &parent,
        &named_root,
        committed_results("passed", input_record(&head, "clean")),
    );
    host_observed(&state, &registration, &head, acknowledged("settled"));
    let original = current_registration(&state);
    let mut recovered = original.work_binding.clone().unwrap();
    recovered.claim_fence += 1;
    recovered.work_revision += 1;
    let task = task_under(current_claim(
        recovered.claim_fence,
        Some(recovered.clone()),
    ));
    let expected = Ok(AcceptanceCommittedInputDecision::Admitted(
        AcceptanceCommittedInputAdmission {
            native_source: None,
            run_id: COMMITTED_RUN_ID.to_owned(),
            commit: head,
        },
    ));
    let deadline = || std::time::Instant::now() + Duration::from_secs(60);
    assert_eq!(
        decide_task(&state, &parent, &task, deadline()),
        expected,
        "the old naming and receipt admit before the current name changes"
    );
    let original_entry = state.inner.lock().unwrap().engram_work_source_roots[0].clone();
    {
        let mut inner = state.inner.lock().unwrap();
        let entry = &mut inner.engram_work_source_roots[0];
        entry.generation += 1;
        entry.claim_fence = recovered.claim_fence;
    };
    let rebound = assert_refused_without_a_sighting(
        &transport,
        decide_task(&state, &parent, &task, deadline()),
        "registered_check_root_rebound",
        "an explicit release and new naming generation needs its own check",
    );
    let conflict = rebound.into_api_error("w-task");
    assert!(conflict.message.contains("generation 1") && conflict.message.contains("generation 2"));
    assert!(
        conflict.message.contains("new registered check")
            && conflict.message.contains("current root")
    );
    assert_eq!(current_registration(&state).named_root, original.named_root);
    assert_eq!(
        current_registration(&state).work_binding,
        original.work_binding
    );
    assert_eq!(sighting_reads(&transport), 0);

    // Restore the admitting generation so each independent pin decides alone.
    *state.inner.lock().unwrap().engram_work_source_roots = vec![original_entry.clone()];
    assert_eq!(decide_task(&state, &parent, &task, deadline()), expected);
    for case in ["registration store", "registration workspace"] {
        {
            let mut inner = state.inner.lock().unwrap();
            let mut row = inner
                .test_run_registrations
                .get_mut(COMMITTED_RUN_ID)
                .unwrap();
            if case == "registration store" {
                row.authority_store
                    .as_mut()
                    .unwrap()
                    .project_id
                    .push_str("-other");
            } else {
                row.named_root
                    .as_mut()
                    .unwrap()
                    .workspace_id
                    .push_str("-other");
            }
        }
        let decision = decide_task(&state, &parent, &task, deadline());
        if case == "registration store" {
            assert_eq!(
                decision,
                Ok(AcceptanceCommittedInputDecision::Legacy),
                "{case}"
            );
        } else {
            let rebound = assert_refused_without_a_sighting(
                &transport,
                decision,
                "registered_check_root_rebound",
                case,
            );
            assert!(rebound.into_api_error("w-task").message.contains("-other"));
        }
        *state
            .inner
            .lock()
            .unwrap()
            .test_run_registrations
            .get_mut(COMMITTED_RUN_ID)
            .unwrap() = original.clone();
        assert_eq!(
            decide_task(&state, &parent, &task, deadline()),
            expected,
            "restoring only {case} restores admission"
        );
    }
    let request_root = AcceptanceEvaluationSourceRoot::from_entry(&original_entry);
    let reader = acceptance_evaluation_host_target_locked(&state.inner.lock().unwrap(), &parent)
        .unwrap()
        .connection;
    for case in ["another workspace", "another store", "ended name"] {
        let mut entry = original_entry.clone();
        match case {
            "another workspace" => entry.root.push_str("-other"),
            "another store" => entry.store.project_id.push_str("-other"),
            "ended name" => {}
            _ => unreachable!(),
        }
        *state.inner.lock().unwrap().engram_work_source_roots = if case == "ended name" {
            Vec::new()
        } else {
            vec![entry]
        };
        assert_eq!(
            state.acceptance_committed_input_decision(
                &parent,
                &reader,
                &store,
                &task,
                Some(&request_root),
                deadline(),
                &std::time::Instant::now,
            ),
            Ok(AcceptanceCommittedInputDecision::Legacy),
            "{case}: the original receipt cannot supply a current named root",
        );
    }
}
