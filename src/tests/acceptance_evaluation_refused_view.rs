// Requester visibility for confirmed refused judgments. Extends the refresh
// suite; transport alone is scripted. This does not evaluate live work or
// substitute a retained refusal for an Engram evaluation receipt.
use super::*;

const CONTEXT: &str = "1111aaaa1111aaaa1111aaaa1111aaaa";

fn context_refusal() -> EngramCliOutput {
    // Producer: Engram's acceptance_evaluation/citations.rs NotOnRun branch.
    // A non-holder observation is context and is absent from run evidence,
    // even when the verdict citing it is insufficient-evidence rather than pass.
    cli_output(
        false,
        "",
        &refusal_envelope(
            "acceptance_evaluation_refused",
            &format!("criterion 1 cites {CONTEXT}, which is not evidence on this run"),
        ),
    )
}

fn context_judgment(id: &str, ordinal: u8) -> SubmitAcceptanceEvaluationRequest {
    let mut request = keyed_submission(id, ordinal);
    request.verdicts[0] = verdict(1, "insufficient-evidence", &"Ż".repeat(2_000), &[CONTEXT]);
    request.verdicts[1] = verdict(
        2,
        "fail",
        "The remaining behavior was not established.",
        &["bbbbbbbb2222"],
    );
    request
}

fn requester_target(state: &AppState, parent: &str, id: &str) -> Value {
    let status = serde_json::to_value(state.get_delegation(parent, id).unwrap()).unwrap();
    let target = status["delegation"]["acceptanceEvaluation"].clone();
    let listed = serde_json::to_value(state.list_delegations(parent).unwrap()).unwrap();
    assert_eq!(listed["delegations"][0]["acceptanceEvaluation"], target);
    for private in ["\"args\"", "--actor-id", "--actor-context", "sourcePayload"] {
        assert!(
            !status.to_string().contains(private),
            "private argument escaped: {private}"
        );
    }
    target
}

fn reload(state: &AppState) -> AppState {
    state.shutdown_persist_blocking();
    AppState::new_with_paths(
        state.default_workdir.clone(),
        state.persistence_path.as_ref().clone(),
        state.orchestrator_templates_path.as_ref().clone(),
    )
    .unwrap()
}

#[test]
fn refused_context_judgment_is_visible_after_reload_and_an_explicit_later_attempt() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    super::super::super::delegation_support::install_delegation_codex_runtime(
        &state,
        "refused-view-runtime",
    );
    let (id, child) =
        install_evaluator_delegation(&state, &parent, Some(&project), &root.to_string_lossy(), 2);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&id).unwrap();
        inner.delegations[index].agent = Agent::Codex;
        inner.session_mut(&child).unwrap().session.agent = Agent::Codex;
    }
    enable_attempt_history(&state, &id, &parent, &child);

    let mut shown = show_receipt(Some("independent_session"));
    shown["notes"][0]["locator"] = json!(CONTEXT);
    shown["notes"][0]["non_holder"] = json!(true);
    let task = parse_acceptance_evaluation_task(shown, full_receipt()).unwrap();
    let brief =
        build_acceptance_evaluator_prompt(&task, &root.to_string_lossy(), 64 * 1024).unwrap();
    assert!(brief.contains(&format!("{CONTEXT} (")));
    assert!(brief.contains("[context only: the tracker refuses this as a citation]"));

    let (calls, run) = scripted_runner(vec![Ok(context_refusal())]);
    let refused = state
        .submit_acceptance_evaluation_with_runner(&child, context_judgment(&id, 1), run)
        .unwrap_err();
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert!(!refused.message.contains("submit once more"));
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert!(
        calls.lock().unwrap()[0]
            .iter()
            .any(|arg| arg == &format!("1={CONTEXT}"))
    );
    assert!(outcome_of(&state, &id).is_none());
    let target = requester_target(&state, &parent, &id);
    assert!(target.get("submission").is_none());
    let entry = &target["attemptHistory"]["refusedSubmissions"][0];
    assert_eq!(entry["state"], "refused");
    assert_eq!(entry["recorded"], false);
    assert_eq!(
        entry["attemptKey"],
        acceptance_evaluation_ordinal_key(&id, 1)
    );
    assert_eq!(entry["ordinal"], 1);
    assert_eq!(entry["acceptanceBasis"], 7);
    assert_eq!(entry["evidenceBasis"], 42);
    assert_eq!(entry["code"], "acceptance_evaluation_refused");
    assert_eq!(
        entry["message"],
        format!("criterion 1 cites {CONTEXT}, which is not evidence on this run")
    );
    assert_eq!(
        entry["verdicts"],
        json!([
            {"criterion":1,"verdict":"insufficient_evidence","basis":"judgment","rationale":"Ż".repeat(2_000),"evidence":[CONTEXT]},
            {"criterion":2,"verdict":"fail","basis":"judgment","rationale":"The remaining behavior was not established.","evidence":["bbbbbbbb2222"]}
        ])
    );
    let saved = entry.clone();
    state
        .submit_acceptance_evaluation_with_runner(
            &child,
            context_judgment(&id, 1),
            runner_must_not_run,
        )
        .unwrap_err();

    super::super::super::delegation_support::finish_delegation_child_with_assistant_text(
        &state,
        &child,
        "## Result\n\nStatus: completed\n\nSummary:\nThe tracker refused my submission.",
    );
    let result = serde_json::to_value(state.get_delegation_result(&parent, &id).unwrap()).unwrap();
    assert_eq!(
        result["acceptanceEvaluation"]["attemptHistory"]["refusedSubmissions"][0],
        saved
    );
    let disk = load_state(state.persistence_path.as_ref())
        .unwrap()
        .unwrap();
    let durable = disk.delegations.iter().find(|d| d.id == id).unwrap();
    assert_eq!(
        serde_json::to_value(durable).unwrap()["acceptanceEvaluation"]["attemptHistory"]["refusedSubmissions"]
            [0],
        saved
    );

    // Only an explicit request creates the later host brief and key. Keep the
    // child busy so the real follow-up owner queues it without a provider.
    state
        .inner
        .lock()
        .unwrap()
        .session_mut(&child)
        .unwrap()
        .session
        .status = SessionStatus::Active;
    let mut request = evaluation_request(None);
    request.reuse_delegation_id = Some(id.clone());
    let response = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            request,
            fresh_reader(Arc::default(), 43),
        )
        .unwrap();
    let AcceptanceEvaluationRequestResponse::Spawned { reused, notice, .. } = response else {
        panic!("independent child required");
    };
    assert!(reused, "{notice:?}");
    let later = requester_target(&state, &parent, &id);
    assert_eq!(
        later["attemptKey"],
        acceptance_evaluation_ordinal_key(&id, 2)
    );
    assert_eq!(later["evidenceBasis"], 43);
    assert_eq!(
        later["attemptHistory"]["refusedSubmissions"],
        json!([saved])
    );
    let restarted = reload(&state);
    assert_eq!(
        requester_target(&restarted, &parent, &id)["attemptHistory"]["refusedSubmissions"],
        later["attemptHistory"]["refusedSubmissions"]
    );
    restarted.shutdown_persist_blocking();
}

#[test]
fn citation_retry_keeps_the_first_refused_judgment_separate_from_the_recorded_repair() {
    // A requester read refreshes lifecycle. Use the real spawn path with the
    // existing mock runtime so this is still the evaluator's active turn.
    let (state, parent, id, child) = selected_refresh_fixture();
    assert_eq!(
        state
            .get_delegation(&parent, &id)
            .unwrap()
            .delegation
            .status,
        DelegationStatus::Running
    );
    let alternatives = json!({"error":{"code":"acceptance_evaluation_refused", "message":"citation is not admitted",
        "details":{"cause":{"criterion":1,"citable_records":{"records":["cccccccc3333"],"more":false}}}, "next":[], "reminders":[]}});
    let (_, run) = scripted_runner(vec![Ok(cli_output(false, "", &alternatives.to_string()))]);
    let offer = state
        .submit_acceptance_evaluation_with_runner(&child, context_judgment(&id, 1), run)
        .unwrap_err();
    assert!(
        offer
            .message
            .contains("submit once more under the same attemptKey")
    );
    let first =
        requester_target(&state, &parent, &id)["attemptHistory"]["refusedSubmissions"].clone();
    assert_eq!(
        state
            .get_delegation(&parent, &id)
            .unwrap()
            .delegation
            .status,
        DelegationStatus::Running
    );
    assert_eq!(first[0]["verdicts"][0]["evidence"], json!([CONTEXT]));
    let disk = load_state(state.persistence_path.as_ref())
        .unwrap()
        .unwrap();
    let durable = disk.delegations.iter().find(|d| d.id == id).unwrap();
    assert_eq!(
        serde_json::to_value(durable).unwrap()["acceptanceEvaluation"]["attemptHistory"]["refusedSubmissions"],
        first
    );

    let invalid = state
        .submit_acceptance_evaluation_with_runner(
            &child,
            context_judgment(&id, 1),
            runner_must_not_run,
        )
        .unwrap_err();
    assert_eq!(invalid.status, StatusCode::BAD_REQUEST);
    assert!(invalid.message.contains("only the records Engram named"));
    let mut repair = context_judgment(&id, 1);
    repair.verdicts[0].evidence = vec!["cccccccc3333".to_owned()];
    repair.verdicts[0].verdict = "pass".to_owned();
    let (_, run) = scripted_runner(vec![Ok(cli_output(true, &evaluate_receipt(false), ""))]);
    state
        .submit_acceptance_evaluation_with_runner(&child, repair, run)
        .unwrap();
    let target = requester_target(&state, &parent, &id);
    assert_eq!(target["submission"]["state"], "recorded");
    assert_eq!(target["attemptHistory"]["refusedSubmissions"], first);
    assert_eq!(
        target["attemptHistory"]["refusedSubmissions"][0]["recorded"],
        false
    );
    assert!(outcome_of(&state, &id).is_some());
    super::super::super::delegation_support::finish_delegation_child_with_assistant_text(
        &state,
        &child,
        "## Result\n\nStatus: completed\n\nSummary:\nThe repaired evaluation was recorded.",
    );
    let restarted = reload(&state);
    assert_eq!(
        requester_target(&restarted, &parent, &id)["attemptHistory"]["refusedSubmissions"],
        first
    );
    restarted.shutdown_persist_blocking();
}

#[test]
fn unknown_send_followed_by_refusal_creates_no_confirmed_refused_submission() {
    let (state, parent, id, child) = evaluator_fixture();
    enable_attempt_history(&state, &id, &parent, &child);
    let (calls, run) = scripted_runner(vec![response_lost(), Ok(context_refusal())]);
    let unknown = state
        .submit_acceptance_evaluation_with_runner(&child, context_judgment(&id, 1), run)
        .unwrap_err();
    assert!(unknown.message.contains("unknown outcome"));
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], calls[1]);
    let target = requester_target(&state, &parent, &id);
    assert_eq!(target["submission"]["state"], "unconfirmed");
    assert!(target["attemptHistory"].get("refusedSubmissions").is_none());
    assert!(target["attemptHistory"].get("refusal").is_none());
    let restarted = reload(&state);
    let loaded = requester_target(&restarted, &parent, &id);
    assert_eq!(loaded["submission"]["state"], "unconfirmed");
    assert!(loaded["attemptHistory"].get("refusedSubmissions").is_none());
    restarted.shutdown_persist_blocking();
}

#[test]
fn old_refusal_is_explicitly_unavailable_without_reading_or_backfilling_private_argv() {
    let (state, parent, id, child) = evaluator_fixture();
    enable_attempt_history(&state, &id, &parent, &child);
    let (_, run) = scripted_runner(vec![Ok(context_refusal())]);
    state
        .submit_acceptance_evaluation_with_runner(&child, context_judgment(&id, 1), run)
        .unwrap_err();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&id).unwrap();
        let history = inner.delegations[index]
            .acceptance_evaluation
            .as_mut()
            .unwrap()
            .attempt_history
            .as_mut()
            .unwrap();
        history.refused_submissions.clear();
        history.refusal.as_mut().unwrap().original.args = vec!["private-not-a-command".to_owned()];
        inner.mark_delegation_mutated(index);
        state.commit_locked(&mut inner).unwrap();
    }
    let disk = load_state(state.persistence_path.as_ref())
        .unwrap()
        .unwrap();
    let old = disk.delegations.iter().find(|d| d.id == id).unwrap();
    let encoded = serde_json::to_value(old).unwrap();
    assert!(
        encoded["acceptanceEvaluation"]["attemptHistory"]
            .get("refusedSubmissions")
            .is_none()
    );
    let restarted = reload(&state);
    let target = requester_target(&restarted, &parent, &id);
    let entry = &target["attemptHistory"]["refusedSubmissions"][0];
    assert_eq!(entry["state"], "unavailable");
    assert_eq!(entry["recorded"], false);
    assert!(entry.get("verdicts").is_none());
    assert!(!target.to_string().contains("private-not-a-command"));
    assert!(
        current_target(&restarted, &id)
            .attempt_history
            .unwrap()
            .refused_submissions
            .is_empty(),
        "the view must not backfill the stored history"
    );
    restarted.shutdown_persist_blocking();
}

#[test]
fn a_second_refusal_keeps_both_submitted_judgments_and_rejects_a_third_send() {
    let (state, parent, id, child) = evaluator_fixture();
    enable_attempt_history(&state, &id, &parent, &child);
    let alternatives = json!({"error":{"code":"acceptance_evaluation_refused", "message":"first refusal",
        "details":{"cause":{"criterion":1,"citable_records":{"records":["cccccccc3333"],"more":false}}}}});
    let (_, run) = scripted_runner(vec![
        Ok(cli_output(false, "", &alternatives.to_string())),
        Ok(context_refusal()),
    ]);
    state
        .submit_acceptance_evaluation_with_runner(&child, context_judgment(&id, 1), &run)
        .unwrap_err();
    let mut repair = context_judgment(&id, 1);
    repair.verdicts[0].evidence = vec!["cccccccc3333".to_owned()];
    repair.verdicts[0].rationale = "I inspected the named alternative.".to_owned();
    state
        .submit_acceptance_evaluation_with_runner(&child, repair.clone(), run)
        .unwrap_err();
    let target = requester_target(&state, &parent, &id);
    let entries = target["attemptHistory"]["refusedSubmissions"]
        .as_array()
        .unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["verdicts"][0]["evidence"], json!([CONTEXT]));
    assert_eq!(
        entries[1]["verdicts"][0]["evidence"],
        json!(["cccccccc3333"])
    );
    assert_eq!(
        entries[1]["verdicts"][0]["rationale"],
        "I inspected the named alternative."
    );
    state
        .submit_acceptance_evaluation_with_runner(&child, repair, runner_must_not_run)
        .unwrap_err();
    let restarted = reload(&state);
    assert_eq!(
        requester_target(&restarted, &parent, &id)["attemptHistory"]["refusedSubmissions"],
        json!(entries)
    );
    restarted.shutdown_persist_blocking();
}

#[test]
fn persisted_refused_payloads_have_a_complete_bounded_read_contract() {
    let (state, parent, id, child) = evaluator_fixture();
    enable_attempt_history(&state, &id, &parent, &child);
    let (_, run) = scripted_runner(vec![Ok(context_refusal())]);
    state
        .submit_acceptance_evaluation_with_runner(&child, context_judgment(&id, 1), run)
        .unwrap_err();
    let target = current_target(&state, &id);
    let mut encoded = serde_json::to_value(&target).unwrap();
    let entry = encoded["attemptHistory"]["refusedSubmissions"][0].clone();
    encoded["attemptHistory"]["refusedSubmissions"] = json!(vec![entry.clone(); 7]);
    assert!(
        serde_json::from_value::<DelegationAcceptanceEvaluation>(encoded).is_err(),
        "an oversized history must not load as a complete view"
    );
    let mut encoded = serde_json::to_value(target).unwrap();
    encoded["attemptHistory"]["refusedSubmissions"][0]["verdicts"][0]["rationale"] =
        json!("x".repeat(2_001));
    assert!(
        serde_json::from_value::<DelegationAcceptanceEvaluation>(encoded).is_err(),
        "the existing rationale bound applies on reload too"
    );
}
