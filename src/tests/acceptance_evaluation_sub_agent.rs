// Sub-agent request selection, holder/executor preflight and host-owned child
// identity. These tests use the parent request fixtures, not a live Engram
// process; they do not prove that Engram accepts a submitted evaluation.
use super::*;

#[derive(Clone, Copy)]
enum ExecutorRead {
    Absent,
    Null,
    Requester,
    Other,
    Malformed,
}

fn request_case(
    admitted: &[&str],
    preferred: Option<AcceptanceEvaluationMode>,
    pin: Option<&str>,
    held: bool,
) -> (
    AppState,
    String,
    Result<AcceptanceEvaluationRequestResponse, ApiError>,
) {
    request_case_with_executor(
        admitted,
        preferred,
        pin,
        held,
        (ExecutorRead::Absent, ExecutorRead::Absent),
    )
}

fn request_case_with_executor(
    admitted: &[&str],
    preferred: Option<AcceptanceEvaluationMode>,
    pin: Option<&str>,
    held: bool,
    executor: (ExecutorRead, ExecutorRead),
) -> (
    AppState,
    String,
    Result<AcceptanceEvaluationRequestResponse, ApiError>,
) {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    state
        .update_acceptance_defaults(
            &project,
            AcceptanceEvaluatorDefaults {
                default_mode: preferred,
                ..Default::default()
            },
        )
        .unwrap();
    crate::tests::delegation_support::install_delegation_codex_runtime(
        &state,
        "sub-agent-acceptance-runtime",
    );
    let reader = fixture_reader(
        Arc::default(),
        show_receipt(pin),
        Ok(policy_receipt(Some(admitted))),
    );
    let core_reads = AtomicUsize::new(0);
    let response = state.request_acceptance_evaluation_with_runner(
        &parent,
        evaluation_request(Some(Agent::Codex)),
        |connection, args, timeout| {
            if args.iter().any(|arg| arg == "inspect") {
                let mut core = evidence_selection::canonical_core_receipt();
                let read = core_reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                match if read == 0 { executor.0 } else { executor.1 } {
                    ExecutorRead::Absent => (),
                    ExecutorRead::Null => core["run"]["executor"] = Value::Null,
                    ExecutorRead::Requester => core["run"]["executor"] = json!(parent),
                    ExecutorRead::Other => core["run"]["executor"] = json!("session-other"),
                    ExecutorRead::Malformed => core["run"]["executor"] = json!(42),
                }
                Ok(core)
            } else if args.iter().any(|arg| arg == "held") {
                assert_eq!(connection.session_id, parent);
                assert_ne!(connection.session_id, WORK_HOST_READER_SESSION_ID);
                let items = if held {
                    vec![json!({
                        "work_id": evidence_selection::canonical_core_receipt()
                            ["status"]["work"]["work_id"],
                        "short_ref": "w-task",
                        "claim_id": "fixture-claim",
                        "claim_fence": 1
                    })]
                } else {
                    Vec::new()
                };
                Ok(json!({"items": items, "omitted": 0}))
            } else {
                reader(connection, args, timeout)
            }
        },
    );
    (state, parent, response)
}

fn assert_sub_agent_selected(
    admitted: &[&str],
    preferred: Option<AcceptanceEvaluationMode>,
    pin: Option<&str>,
) {
    let (state, parent, response) = request_case(admitted, preferred, pin, true);
    assert_sub_agent_child(state, parent, response);
}

fn assert_sub_agent_child(
    state: AppState,
    parent: String,
    response: Result<AcceptanceEvaluationRequestResponse, ApiError>,
) {
    let response = match response {
        Ok(response) => response,
        Err(error) => panic!("an eligible or unknown sub-agent parent must produce a child: {error:?}"),
    };
    let wire = serde_json::to_value(&response).unwrap();
    assert_eq!(wire["mode"], "sub_agent", "never fall back to same_session");
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response else {
        panic!("sub_agent must spawn an evaluator, not return an executor's brief")
    };
    let record = delegation.delegation;
    assert_eq!(record.mode, DelegationMode::Evaluator);
    assert_eq!(record.write_policy, DelegationWritePolicy::ReadOnly);
    assert_eq!(record.parent_session_id, parent);
    assert_ne!(record.child_session_id, parent);
    let target = serde_json::to_value(record.acceptance_evaluation.as_ref().unwrap()).unwrap();
    assert_eq!(target["mode"], "sub_agent");
    assert_eq!(target["parentSession"], parent);
    assert_eq!(target["executionIdentity"], record.id);
    let inner = state.inner.lock().unwrap();
    let stored = &inner.delegations[inner.find_delegation_index(&record.id).unwrap()];
    assert_eq!(stored.acceptance_evaluation, record.acceptance_evaluation);
    assert_eq!(
        inner.sessions[inner.find_session_index(&record.child_session_id).unwrap()]
            .session
            .parent_delegation_id
            .as_deref(),
        Some(record.id.as_str())
    );
}

#[test]
fn sub_agent_only_policy_produces_a_child_without_a_pin() {
    assert_sub_agent_selected(&["sub_agent"], None, None);
}

#[test]
fn sub_agent_only_policy_does_not_use_an_unadmitted_preference() {
    for preferred in [
        AcceptanceEvaluationMode::SameSession,
        AcceptanceEvaluationMode::IndependentSession,
    ] {
        assert_sub_agent_selected(&["sub_agent"], Some(preferred), None);
    }
}

#[test]
fn sub_agent_and_same_policy_produces_a_child_without_a_pin() {
    for admitted in [
        ["sub_agent", "same_session"],
        ["same_session", "sub_agent"],
    ] {
        assert_sub_agent_selected(&admitted, None, None);
    }
}

#[test]
fn sub_agent_and_same_policy_never_falls_back_to_saved_same_preference() {
    assert_sub_agent_selected(
        &["sub_agent", "same_session"],
        Some(AcceptanceEvaluationMode::SameSession),
        None,
    );
    assert_sub_agent_selected(
        &["sub-agent", "same-session"],
        Some(AcceptanceEvaluationMode::SameSession),
        None,
    );
}

#[test]
fn sub_agent_and_same_policy_ignores_unadmitted_independent_preference() {
    assert_sub_agent_selected(
        &["sub_agent", "same_session"],
        Some(AcceptanceEvaluationMode::IndependentSession),
        None,
    );
}

#[test]
fn sub_agent_pin_produces_a_child_even_when_other_modes_are_admitted() {
    assert_sub_agent_selected(
        &["independent_session", "sub_agent", "same_session"],
        Some(AcceptanceEvaluationMode::SameSession),
        Some("sub_agent"),
    );
}

#[test]
fn sub_agent_request_with_neither_holder_nor_executor_refuses_before_spawning() {
    for (admitted, pin) in [
        (vec!["sub_agent"], None),
        (vec!["sub_agent", "same_session"], None),
        (
            vec!["independent_session", "sub_agent"],
            Some("sub_agent"),
        ),
    ] {
        let (state, _, response) = request_case_with_executor(
            &admitted,
            None,
            pin,
            false,
            (ExecutorRead::Other, ExecutorRead::Other),
        );
        let error = response.err().expect("a proven ineligible parent cannot spawn a child");
        assert_eq!(error.status, StatusCode::CONFLICT, "{error:?}");
        assert!(error.message.contains("sub_agent"), "{error:?}");
        assert!(error.message.contains("holder") && error.message.contains("executor"), "{error:?}");
        assert!(state.inner.lock().unwrap().delegations.is_empty());
    }
}

#[test]
fn sub_agent_requester_executor_does_not_need_a_held_claim() {
    let (state, parent, response) = request_case_with_executor(
        &["sub_agent"],
        None,
        None,
        false,
        (ExecutorRead::Other, ExecutorRead::Requester),
    );
    assert_sub_agent_child(state, parent, response);
}

#[test]
fn sub_agent_holder_does_not_need_to_be_the_run_executor() {
    let (state, parent, response) = request_case_with_executor(
        &["sub_agent"],
        None,
        None,
        true,
        (ExecutorRead::Other, ExecutorRead::Other),
    );
    assert_sub_agent_child(state, parent, response);
}

#[test]
fn sub_agent_missing_or_null_executor_is_unknown_not_proven_ineligible() {
    for executor in [ExecutorRead::Absent, ExecutorRead::Null] {
        let (state, parent, response) = request_case_with_executor(
            &["sub_agent"],
            None,
            None,
            false,
            (executor, executor),
        );
        assert_sub_agent_child(state, parent, response);
    }
}

#[test]
fn sub_agent_preflight_reads_executor_from_the_validated_closing_core() {
    let (state, _, response) = request_case_with_executor(
        &["sub_agent"],
        None,
        None,
        false,
        (ExecutorRead::Requester, ExecutorRead::Other),
    );
    let error = response.err().expect("opening executor is not a closing eligibility proof");
    assert_eq!(error.status, StatusCode::CONFLICT, "{error:?}");
    assert!(error.message.contains("holder") && error.message.contains("executor"), "{error:?}");
    assert!(state.inner.lock().unwrap().delegations.is_empty());
}

#[test]
fn sub_agent_no_fallback_guard_preserves_other_preferences_and_task_pins() {
    for (admitted, pin) in [
        (vec!["independent_session", "sub_agent", "same_session"], None),
        (vec!["sub_agent", "same_session"], Some("same_session")),
        (vec!["same_session"], None),
    ] {
        let (state, _, response) = request_case(
            &admitted,
            Some(AcceptanceEvaluationMode::SameSession),
            pin,
            false,
        );
        let response = response.unwrap();
        assert_eq!(serde_json::to_value(response).unwrap()["mode"], "same_session");
        assert!(state.inner.lock().unwrap().delegations.is_empty());
    }
}

#[test]
fn sub_agent_admitted_preference_is_available() {
    assert_sub_agent_selected(
        &["independent_session", "sub_agent", "same_session"],
        Some(AcceptanceEvaluationMode::SubAgent),
        None,
    );
}

#[test]
fn sub_agent_auto_selection_covers_every_admitted_list() {
    // All eight subsets, including the explicit self-asserted/off policy.
    for (admitted, expected) in [
        (vec![], None),
        (vec!["same_session"], Some(AcceptanceEvaluationMode::SameSession)),
        (vec!["sub_agent"], Some(AcceptanceEvaluationMode::SubAgent)),
        (vec!["independent_session"], Some(AcceptanceEvaluationMode::IndependentSession)),
        (vec!["same_session", "sub_agent"], Some(AcceptanceEvaluationMode::SubAgent)),
        (vec!["same_session", "independent_session"], Some(AcceptanceEvaluationMode::IndependentSession)),
        (vec!["sub_agent", "independent_session"], Some(AcceptanceEvaluationMode::IndependentSession)),
        (vec!["same_session", "sub_agent", "independent_session"], Some(AcceptanceEvaluationMode::IndependentSession)),
    ] {
        let (state, _, response) = request_case(&admitted, None, None, true);
        if let Some(mode) = expected {
            assert_eq!(serde_json::to_value(response.unwrap()).unwrap()["mode"], mode.word());
        } else {
            assert_eq!(response.err().unwrap().status, StatusCode::CONFLICT);
            assert!(state.inner.lock().unwrap().delegations.is_empty());
        }
    }
}

#[test]
fn sub_agent_malformed_closing_executor_does_not_bypass_preflight() {
    let (state, _, response) = request_case_with_executor(
        &["sub_agent"], None, None, false,
        (ExecutorRead::Requester, ExecutorRead::Malformed),
    );
    let error = response.err().unwrap();
    assert_eq!(error.status, StatusCode::BAD_GATEWAY);
    assert!(error.message.contains("executor identity"));
    assert!(state.inner.lock().unwrap().delegations.is_empty());
}

fn submission_fixture() -> (AppState, String, String, String) {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let (delegation, child) = install_evaluator_delegation(
        &state, &parent, Some(&project), &root.to_string_lossy(), 2,
    );
    update_evaluation_target(&state, &delegation, |target| {
        target.mode = AcceptanceEvaluationMode::SubAgent;
        target.parent_session = Some(parent.clone());
        target.execution_identity = Some(delegation.clone());
    });
    (state, parent, delegation, child)
}

fn sub_agent_fixture_receipt(replayed: bool) -> String {
    let mut receipt: Value = serde_json::from_str(&evaluate_receipt(replayed)).unwrap();
    receipt["evaluation"]["mode"] = json!("sub_agent");
    receipt.to_string()
}

#[test]
fn sub_agent_submit_uses_child_identity_and_host_attested_parent_pair() {
    let (state, parent, delegation, child) = submission_fixture();
    let calls: RecordedEngramCalls = Arc::default();
    let seen = calls.clone();
    let response = state.submit_acceptance_evaluation_with_runner(
        &child, two_verdicts(), |connection, args, _| {
            seen.lock().unwrap().push((connection.clone(), args.to_vec()));
            Ok(cli_output(true, &sub_agent_fixture_receipt(false), ""))
        },
    ).unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let (connection, args) = &calls[0];
    let flag = |name| args.windows(2).find(|pair| pair[0] == name).unwrap()[1].clone();
    assert_ne!(child, parent);
    assert_eq!(connection.session_id, child);
    assert_eq!(flag("--session-id"), child);
    assert_eq!(flag("--mode"), "sub_agent");
    assert_eq!(flag("--parent-session"), parent);
    assert_eq!(flag("--execution-identity"), delegation);
    assert_eq!(flag("--attempt"), delegation);
    assert_eq!(response.mode, AcceptanceEvaluationMode::SubAgent);
    assert_eq!(outcome_of(&state, &delegation).unwrap().0.mode.as_deref(), Some("sub_agent"));
}

#[test]
fn sub_agent_missing_or_mismatched_attestation_never_reconstructed_or_sent() {
    for case in 0..5 {
        let (state, _, delegation, child) = submission_fixture();
        update_evaluation_target(&state, &delegation, |target| match case {
            0 => target.parent_session = None,
            1 => target.execution_identity = None,
            2 => target.parent_session = Some("another-parent".to_owned()),
            3 => target.execution_identity = Some("another-execution".to_owned()),
            _ => target.mode = AcceptanceEvaluationMode::IndependentSession,
        });
        let error = state.submit_acceptance_evaluation_with_runner(
            &child, two_verdicts(), |_, _, _| panic!("invalid attestation must send nothing"),
        ).unwrap_err();
        assert_eq!(error.status, StatusCode::CONFLICT, "{error:?}");
        assert!(error.message.contains("identity"));
        assert!(matches!(submission_of(&state, &delegation), AcceptanceEvaluationSubmission::None));
    }
}

#[test]
fn sub_agent_attestation_survives_persistence_and_legacy_load_is_unattested() {
    let (state, parent, delegation, child) = submission_fixture();
    let target = state.inner.lock().unwrap().delegations.iter()
        .find(|record| record.id == delegation).unwrap().acceptance_evaluation.clone().unwrap();
    let value = serde_json::to_value(&target).unwrap();
    let restored: DelegationAcceptanceEvaluation = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(restored, target);
    assert_eq!(restored.parent_session.as_deref(), Some(parent.as_str()));
    assert_eq!(restored.execution_identity.as_deref(), Some(delegation.as_str()));
    let mut legacy = value;
    legacy.as_object_mut().unwrap().remove("parentSession");
    legacy.as_object_mut().unwrap().remove("executionIdentity");
    let legacy: DelegationAcceptanceEvaluation = serde_json::from_value(legacy).unwrap();
    assert!(legacy.parent_session.is_none() && legacy.execution_identity.is_none());
    update_evaluation_target(&state, &delegation, |target| *target = legacy);
    let error = state.submit_acceptance_evaluation_with_runner(
        &child, two_verdicts(), |_, _, _| panic!("legacy target must send nothing"),
    ).unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
}

#[test]
fn sub_agent_definitive_producer_refusal_is_final_not_retried() {
    let (state, _, delegation, child) = submission_fixture();
    let (calls, run) = scripted_runner(vec![Ok(cli_output(false, "", &refusal_envelope(
        "SubAgentParentNotExecuting", "sub-agent parent is neither holder nor executor",
    )))]);
    let error = state.submit_acceptance_evaluation_with_runner(&child, two_verdicts(), run).unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("Engram refused") && error.message.contains("neither holder nor executor"));
    assert!(!error.message.contains("unknown"));
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert!(matches!(submission_of(&state, &delegation), AcceptanceEvaluationSubmission::None));
}

#[test]
fn sub_agent_uncertain_submission_replays_the_exact_identity_pair() {
    let (state, parent, delegation, child) = submission_fixture();
    let (calls, run) = scripted_runner(vec![
        response_lost(), Ok(cli_output(true, &sub_agent_fixture_receipt(true), "")),
    ]);
    state.submit_acceptance_evaluation_with_runner(&child, two_verdicts(), run).unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], calls[1]);
    assert!(calls[0].windows(2).any(|pair| pair == ["--parent-session", parent.as_str()]));
    assert!(calls[0].windows(2).any(|pair| pair == ["--execution-identity", delegation.as_str()]));
}
