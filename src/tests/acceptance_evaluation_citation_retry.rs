// Tests for the one re-submission an off-source refusal naming citable
// alternatives allows an acceptance evaluator.
// Owns: reading the alternatives from the tracker's refusal, the retry the
// host offers and keeps, the local refusal of a re-submission citing anything
// else for the named criteria, and that every other refusal, and a refusal of
// the re-submission, still ends the attempt.
// Does not own: the other submission outcomes (receipts, unknown writes, locked
// stores), which stay in acceptance_evaluation.rs, or the typed resubmit and
// its refreshed brief (acceptance_evaluation_refresh.rs).
// A new module declared from acceptance_evaluation.rs; it reuses that
// module's evaluator fixture and scripted runner.
use super::*;

/// The record the evaluator first cites: it ran in the producer's worktree.
const PEER: &str = "1111aaaa1111aaaa1111aaaa1111aaaa";
/// The record the tracker names instead: it ran in the judged worktree.
const CURRENT: &str = "2222bbbb2222bbbb2222bbbb2222bbbb";

const OFF_SOURCE: &str = "criterion 2 is bound to test verification, and 1111aaaa1111aaaa1111aaaa1111aaaa ran in workspace C:\\repo\\wt-producer, not the workspace C:\\repo\\wt-merger this evaluation judged; run the check on the current source, then evaluate again citing it";

/// The tracker's refusal with `cause` in its details, the shape Engram gives
/// an off-source refusal: the criterion it concerns and, when any exists, the
/// records citable for it instead. `None` gives the refusal as it is given
/// today, without a cause.
fn refusal_with(code: &str, cause: Option<Value>) -> String {
    let mut details = json!({"reason": OFF_SOURCE, "remedy": "run the required check"});
    if let Some(cause) = cause {
        details["cause"] = cause;
    }
    json!({"error": {"code": code, "details": details, "message": OFF_SOURCE,
        "next": [], "reminders": [OFF_SOURCE]}})
    .to_string()
}

fn refused_with(
    code: &str,
    cause: Option<Value>,
) -> std::result::Result<EngramCliOutput, EngramTransportError> {
    Ok(cli_output(false, "", &refusal_with(code, cause)))
}

/// A cause concerning criterion 2 with `records` citable for it.
fn cause_naming(records: Value) -> Option<Value> {
    Some(json!({"criterion": 2, "citable_records": {"records": records, "more": false}}))
}

fn current_named() -> Option<Value> {
    cause_naming(json!([CURRENT]))
}

/// An evaluator of two criteria, the second bound to a `test` check.
fn bound_evaluator() -> (AppState, String, String) {
    let (state, _, delegation, child) = evaluator_fixture();
    update_evaluation_target(&state, &delegation, |target| {
        target.bindings = vec![AcceptanceCriterionBinding {
            criterion: 2,
            check_kind: "test".to_owned(),
            fingerprint: None,
        }];
    });
    (state, delegation, child)
}

/// Both criteria pass; the bound one cites `cited`.
fn citing(cited: &str) -> SubmitAcceptanceEvaluationRequest {
    let mut bound = verdict(2, "pass", "The fail-first test passed.", &[cited]);
    bound.basis = Some("observed".to_owned());
    submission(vec![
        verdict(1, "pass", "Read the diff; the route exists.", &["aaaaaaaa1111"]),
        bound,
    ])
}

fn history_of(state: &AppState, delegation: &str) -> AcceptanceEvaluationAttemptHistory {
    let inner = state.inner.lock().unwrap();
    let index = inner.find_delegation_index(delegation).unwrap();
    inner.delegations[index]
        .acceptance_evaluation
        .as_ref()
        .unwrap()
        .attempt_history
        .clone()
        .unwrap()
}

#[test]
fn an_off_source_refusal_naming_alternatives_allows_one_resubmission_citing_only_them() {
    let (state, delegation, child) = bound_evaluator();
    let (_, run) = scripted_runner(vec![refused_with(
        "acceptance_evaluation_refused",
        current_named(),
    )]);
    let offer = state
        .submit_acceptance_evaluation_with_runner(&child, citing(PEER), run)
        .unwrap_err();
    assert_eq!(offer.status, StatusCode::CONFLICT, "{}", offer.message);
    assert!(
        offer.message.contains(OFF_SOURCE)
            && offer
                .message
                .contains(&format!("It names records it admits instead (criterion 2: {CURRENT})"))
            && offer.message.contains("You may submit once more under the same attemptKey"),
        "{}",
        offer.message
    );
    assert_eq!(submission_of(&state, &delegation), AcceptanceEvaluationSubmission::None);
    let history = history_of(&state, &delegation);
    assert_eq!(history.refusal, None, "the refusal is not definitive");
    assert_eq!(
        history.citation_retry.as_ref().map(|retry| retry.alternatives.clone()),
        Some(vec![AcceptanceCitableAlternatives {
            criterion: 2,
            records: vec![CURRENT.to_owned()],
        }])
    );

    // A re-submission citing anything else for criterion 2 never reaches the
    // tracker.
    let refused = state
        .submit_acceptance_evaluation_with_runner(&child, citing(PEER), runner_must_not_run)
        .unwrap_err();
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.message);
    assert!(
        refused.message.contains(&format!(
            "criterion 2: this re-submission may cite for it only the records Engram named ({CURRENT}); {PEER} is not one of them"
        )),
        "{}",
        refused.message
    );

    // Citing the named record, it is sent and recorded.
    let (calls, run) = scripted_runner(vec![Ok(cli_output(true, &evaluate_receipt(false), ""))]);
    state
        .submit_acceptance_evaluation_with_runner(&child, citing(CURRENT), run)
        .unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].iter().any(|arg| arg.contains(CURRENT)), "{:?}", calls[0]);
    assert!(!calls[0].iter().any(|arg| arg.contains(PEER)), "{:?}", calls[0]);
    assert!(outcome_of(&state, &delegation).is_some());
}

#[test]
fn a_refusal_of_the_resubmission_is_final() {
    let (state, delegation, child) = bound_evaluator();
    let (_, run) = scripted_runner(vec![refused_with(
        "acceptance_evaluation_refused",
        current_named(),
    )]);
    state
        .submit_acceptance_evaluation_with_runner(&child, citing(PEER), run)
        .unwrap_err();
    // Refused again, alternatives and all: the attempt's one re-submission
    // was used.
    let (calls, run) = scripted_runner(vec![refused_with(
        "acceptance_evaluation_refused",
        current_named(),
    )]);
    let final_refusal = state
        .submit_acceptance_evaluation_with_runner(&child, citing(CURRENT), run)
        .unwrap_err();
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert!(
        final_refusal
            .message
            .starts_with("Engram refused the evaluation; nothing was recorded:")
            && final_refusal.message.contains("Finish and report the refusal")
            && !final_refusal.message.contains("submit once more"),
        "{}",
        final_refusal.message
    );
    assert!(history_of(&state, &delegation).refusal.is_some());
    let again = state
        .submit_acceptance_evaluation_with_runner(&child, citing(CURRENT), runner_must_not_run)
        .unwrap_err();
    assert!(
        again.message.contains("this attempt was definitively refused"),
        "{}",
        again.message
    );
}

#[test]
fn any_other_refusal_still_ends_the_attempt() {
    for (code, cause) in [
        // The refusal as Engram gives it today, without a cause.
        ("acceptance_evaluation_refused", None),
        // A cause without citable records: none exists, so it stays final.
        ("acceptance_evaluation_refused", Some(json!({"criterion": 2}))),
        // An empty or unusable list names none.
        ("acceptance_evaluation_refused", cause_naming(json!([]))),
        ("acceptance_evaluation_refused", cause_naming(json!(["NOT-HEX"]))),
        // Alternatives on any other refusal are not read.
        ("work_revision_conflict", current_named()),
    ] {
        let (state, delegation, child) = bound_evaluator();
        let (_, run) = scripted_runner(vec![refused_with(code, cause.clone())]);
        let refused = state
            .submit_acceptance_evaluation_with_runner(&child, citing(PEER), run)
            .unwrap_err();
        assert!(
            refused.message.contains("Finish and report the refusal")
                && !refused.message.contains("submit once more"),
            "{code} {cause:?}: {}",
            refused.message
        );
        let history = history_of(&state, &delegation);
        assert!(history.refusal.is_some() && history.citation_retry.is_none(), "{code}");
        let again = state
            .submit_acceptance_evaluation_with_runner(&child, citing(CURRENT), runner_must_not_run)
            .unwrap_err();
        assert!(
            again.message.contains("this attempt was definitively refused"),
            "{code}: {}",
            again.message
        );
    }
}

#[test]
fn citable_alternatives_keep_citable_records_once_for_the_cause_criterion() {
    let parse = |cause: Value| {
        let envelope: Value =
            serde_json::from_str(&refusal_with("acceptance_evaluation_refused", Some(cause))).unwrap();
        acceptance_citable_alternatives(&envelope)
    };
    assert_eq!(
        parse(json!({"criterion": 2, "citable_records": {
            "records": [CURRENT, CURRENT, "NOT-HEX", 7, PEER], "more": true}})),
        vec![AcceptanceCitableAlternatives {
            criterion: 2,
            records: vec![CURRENT.to_owned(), PEER.to_owned()],
        }]
    );
    for cause in [
        json!({"criterion": 0, "citable_records": {"records": [CURRENT]}}),
        json!({"criterion": "2", "citable_records": {"records": [CURRENT]}}),
        json!({"citable_records": {"records": [CURRENT]}}),
        json!({"criterion": 2, "citable_records": [CURRENT]}),
        json!({"criterion": 2, "citable_records": {"records": []}}),
        json!("criterion 2"),
    ] {
        assert!(parse(cause.clone()).is_empty(), "{cause}");
    }
    assert!(acceptance_citable_alternatives(&json!({"error": {"details": null}})).is_empty());
}

#[test]
fn a_retry_record_persisted_before_this_field_reads_as_none() {
    let history: AcceptanceEvaluationAttemptHistory = serde_json::from_value(json!({
        "schemaVersion": 1, "parentSessionId": "p", "childSessionId": "c", "cwd": "/w",
        "identity": serde_json::to_value(AcceptanceEvidenceIdentity::default()).unwrap(),
        "ordinal": 1
    }))
    .unwrap();
    assert_eq!(history.citation_retry, None);
    let written = serde_json::to_value(&history).unwrap();
    assert!(written.get("citationRetry").is_none(), "{written}");
}
