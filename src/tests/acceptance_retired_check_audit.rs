use super::*;

// Companion controls leave the original six RED regression bodies unchanged.
fn old_root_fixture(outcome: &str) -> (AppState, String, TestRunRegistration, String) {
    let (state, parent, store, old) = named_root_parent();
    let head = commit_named_root(&old);
    let registration = register_committed_run(
        &state,
        &parent,
        &old,
        committed_results(outcome, input_record(&head, "clean")),
    );
    host_observed(&state, &registration, &head, acknowledged("settled"));
    let replacement = worktree(&old).parent().unwrap().join("audit-replacement");
    run_git_test_command(
        &worktree(&old),
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            replacement.to_str().unwrap(),
            &head,
        ],
    );
    let current = engram_exact_path_key(&replacement);
    state.inner.lock().unwrap().engram_work_source_roots[0].root = current.clone();
    rename_same_workspace(&state, &parent, &store);
    acceptance_owner_reports(&state, &parent, &current, Vec::new(), Vec::new());
    let mut answer = sighting_reply(&current, 2, present_sighting());
    answer["current_binding"] = json!("fixture-renamed-event");
    answer["root"]["binding_event"] = json!("fixture-renamed-event");
    answer["root"]["binding_position"] = json!(2);
    install_sighting_answer(&state, Ok(answer));
    (state, parent, registration, current)
}

fn exclude_old_check() -> RequestAcceptanceEvaluationRequest {
    serde_json::from_value(json!({"workRef":"w-task", "agent":"Codex",
        "retiredCheckDispositions":[{"runId":COMMITTED_RUN_ID,"reason":"Retired different worktree."}]})).unwrap()
}

#[test]
fn audit_keeps_failed_original_and_receipt_disagreement_without_creating_a_pass() {
    let (state, parent, _, _) = old_root_fixture("failed");
    let ordinary = independent_reader();
    let saved: Arc<Mutex<Option<Value>>> = Arc::default();
    let capture = saved.clone();
    let response = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            exclude_old_check(),
            move |connection, args, timeout| {
                if let Some(position) = args.iter().position(|arg| arg == "note") {
                    let note: Value = serde_json::from_str(&args[position + 2]).unwrap();
                    assert_eq!(note["originalVerification"]["run"]["outcome"], "failed");
                    assert_eq!(note["artifactOutcome"], "failed");
                    assert_eq!(note["author"]["sessionId"], connection.session_id);
                    assert_eq!(note["author"]["actorId"], connection.actor_id);
                    assert_eq!(
                        note["originalAcknowledgement"]["receipt"]["run_outcome"],
                        "passed"
                    );
                    assert_eq!(
                        note["receiptValidationError"],
                        "the receipt states another run outcome"
                    );
                    *capture.lock().unwrap() = Some(note);
                    Ok(
                        json!({"operation":"note","evidence":"dddddddddddddddddddddddddddddddd",
                "work":{"short_ref":"w-task","revision":7,"lifecycle":"open"}}),
                    )
                } else {
                    ordinary(connection, args, timeout)
                }
            },
        )
        .unwrap_or_else(|e| panic!("{}", e.message));
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response else {
        panic!("independent evaluator required")
    };
    let note = saved.lock().unwrap().clone().unwrap();
    let record = &delegation.delegation;
    assert!(
        record
            .prompt
            .contains(&serde_json::to_string(&note["originalVerification"]).unwrap())
    );
    assert_eq!(
        record
            .acceptance_evaluation
            .as_ref()
            .unwrap()
            .source_root
            .as_ref()
            .unwrap()
            .root,
        note["currentRoot"]["root"].as_str().unwrap()
    );
    assert_eq!(
        current_registration(&state).verification_record,
        Some(
            serde_json::from_value::<TestRunVerificationAck>(
                note["originalAcknowledgement"].clone()
            )
            .unwrap()
        )
    );
}

#[test]
fn naming_change_after_the_disposition_write_still_prevents_a_spawn() {
    let (state, parent, _, _) = old_root_fixture("passed");
    let ordinary = independent_reader();
    let changed = state.clone();
    let result = state.request_acceptance_evaluation_with_runner(
        &parent,
        exclude_old_check(),
        move |connection, args, timeout| {
            if args.iter().any(|a| a == "note") {
                changed.inner.lock().unwrap().engram_work_source_roots[0].generation += 1;
                Ok(
                    json!({"operation":"note","evidence":"dddddddddddddddddddddddddddddddd",
                "work":{"short_ref":"w-task","revision":7,"lifecycle":"open"}}),
                )
            } else {
                ordinary(connection, args, timeout)
            }
        },
    );
    let error = refusal(&state, result, "source changed after disposition");
    assert!(error.message.contains("disposition"), "{}", error.message);
}
