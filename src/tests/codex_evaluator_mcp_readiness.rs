// Evaluator readiness at the existing shared Codex transport seam. The real
// turn handoff must observe its exact-thread bridge before any model work.
// Reuses ReviewerFixture; no provider process or live tracker is contacted.
use super::controls::{archive_and_result, drive_ready, next_command, respond_start};
use super::*;
use crate::tests::acceptance_evaluation::{evaluation_target, install_store, spawn_request};

fn evaluator_fixture(label: &str) -> ReviewerFixture {
    let (state, project, parent, root) = crate::tests::work_visualizer::fixture();
    install_store(&state, &project, &root);
    let fixture = ReviewerFixture::with_state(state, parent, label);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&fixture.delegation).unwrap();
        let delegation = &mut inner.delegations[index];
        delegation.mode = DelegationMode::Evaluator;
        delegation.review_result_submission_attempt = 0;
        delegation.acceptance_evaluation = Some(evaluation_target(&fixture.delegation, 2));
        fixture.state.commit_locked(&mut inner).unwrap();
    }
    fixture
}

fn evaluator_ready_page() -> Value {
    json!({"data":[{"name":TERMAL_DELEGATION_MCP_SERVER_NAME,
        "runtimeStatus":"connected", "tools":{"namespaced-submit":{
            "name":TERMAL_SUBMIT_ACCEPTANCE_EVALUATION_TOOL_NAME}}}]})
}

fn recorded_submission() -> AcceptanceEvaluationSubmission {
    AcceptanceEvaluationSubmission::Recorded {
        receipt: AcceptanceEvaluationReceiptExtract {
            passed: Some(1),
            verdicts_total: Some(2),
            ..Default::default()
        },
        recorded_at: "2026-01-01T00:00:00Z".to_owned(),
    }
}

fn evaluator_parent_card(fixture: &ReviewerFixture) -> String {
    let inner = fixture.state.inner.lock().unwrap();
    let parent = &inner.sessions[inner.find_session_index(&fixture.parent).unwrap()];
    parent
        .session
        .messages
        .iter()
        .rev()
        .find_map(|message| {
            let Message::ParallelAgents { agents, .. } = message else {
                return None;
            };
            agents
                .iter()
                .find(|agent| agent.id == fixture.delegation)
                .and_then(|agent| agent.detail.clone())
        })
        .expect("evaluator parent card must exist")
}

fn written_requests(written: Vec<u8>) -> Vec<Value> {
    String::from_utf8(written)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect()
}

#[test]
fn evaluator_observes_exact_thread_bridge_before_model_work() {
    let fixture = evaluator_fixture("evaluator-start-status");
    let written = fixture.start();
    let requests = written_requests(written.clone());
    assert!(
        !requests
            .iter()
            .any(|request| request["method"] == "turn/start"),
        "an evaluator must not start model work before exact-thread bridge readiness"
    );
    let request = requests
        .first()
        .expect("evaluator startup must query its bridge");
    assert_eq!(request["method"], "mcpServerStatus/list");
    assert_eq!(request["params"]["threadId"], REVIEW_THREAD);
    assert_eq!(
        request["params"]["serverName"],
        TERMAL_DELEGATION_MCP_SERVER_NAME
    );
    assert_eq!(request["params"]["detail"], "toolsAndAuthOnly");
    respond_start(
        &fixture,
        &written,
        json!({"data":[{
        "name":TERMAL_DELEGATION_MCP_SERVER_NAME,"runtimeStatus":"starting","tools":{}}]}),
    );
    let CodexRuntimeCommand::JsonRpcRequest {
        method,
        params,
        timeout,
        response_tx,
    } = next_command(&fixture)
    else {
        panic!("starting evaluator must recheck before model work");
    };
    assert_eq!(method, "mcpServerStatus/list");
    assert_eq!(params["threadId"], REVIEW_THREAD);
    assert!(timeout <= CODEX_REVIEWER_MCP_BUDGET);
    response_tx.send(Ok(evaluator_ready_page())).unwrap();
    let wire = drive_ready(&fixture, next_command(&fixture));
    let requests = written_requests(wire);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["method"], "turn/start");
    assert_eq!(requests[0]["params"]["threadId"], REVIEW_THREAD);
}

#[test]
fn evaluator_missing_tool_and_timeout_fail_durably_before_model_work_and_allow_fresh_request() {
    for timeout in [false, true] {
        let fixture = evaluator_fixture(if timeout {
            "evaluator-timeout"
        } else {
            "evaluator-missing-submit"
        });
        let worker = if timeout {
            let mut scope = codex_reviewer_mcp_scope(
                &fixture.state,
                &fixture.child,
                REVIEW_THREAD,
                &fixture.runtime.runtime_id,
            )
            .unwrap();
            // Exercise the real deadline branch without waiting sixty seconds.
            scope.query_started -= CODEX_REVIEWER_MCP_BUDGET + Duration::from_secs(1);
            Some(super::terminal_controls::worker_with_scope(
                &fixture, true, scope,
            ))
        } else {
            let written = fixture.start();
            // The review tool is not the evaluator's submit capability.
            respond_start(&fixture, &written, super::controls::ready_page());
            None
        };
        let result = archive_and_result(&fixture);
        if let Some(worker) = worker {
            worker.join().unwrap();
        }
        let outcome = if timeout {
            CodexReviewerMcpOutcome::Timeout
        } else {
            CodexReviewerMcpOutcome::MissingTool
        };
        super::controls::assert_observation(&fixture, &result, "startup", outcome);
        assert!(
            result
                .summary
                .contains("Evaluator MCP bridge is unavailable")
        );
        let saved = load_state(fixture.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let delegation = saved
            .delegations
            .iter()
            .find(|d| d.id == fixture.delegation)
            .unwrap();
        assert!(
            delegation
                .acceptance_evaluation
                .as_ref()
                .unwrap()
                .submission
                .is_none()
        );
        assert!(
            matches!(
                spawn_request(&fixture.state, &fixture.parent),
                Ok(AcceptanceEvaluationRequestResponse::Spawned { .. })
            ),
            "a host-side readiness failure must permit a fresh evaluator request"
        );
    }
}

#[test]
fn evaluator_finishing_without_submission_reports_host_reason_not_child_verdict() {
    let fixture = evaluator_fixture("evaluator-no-submission");
    fixture.complete();
    let CodexRuntimeCommand::JsonRpcRequest {
        method,
        response_tx,
        ..
    } = next_command(&fixture)
    else {
        panic!("finish must observe bridge before archive");
    };
    assert_eq!(method, "mcpServerStatus/list");
    response_tx.send(Ok(evaluator_ready_page())).unwrap();
    let result = archive_and_result(&fixture);
    assert_eq!(result.status, DelegationStatus::Failed);
    assert!(result.summary.contains("no submission recorded"));
    assert_eq!(result.reviewer_mcp_observations[0].phase, "finish");
}

#[test]
fn evaluator_child_prose_is_not_a_verdict_and_open_writes_remain_uncertain() {
    for packet_status in ["completed", "failed"] {
        for submission in [
            AcceptanceEvaluationSubmission::None,
            AcceptanceEvaluationSubmission::Pending {
                payload_digest: "fixture-digest".into(),
                started_at: stamp_now(),
                original: Default::default(),
            },
            AcceptanceEvaluationSubmission::Unconfirmed {
                payload_digest: "fixture-digest".into(),
                reason: "fixture transport failure".into(),
                at: stamp_now(),
                original: Default::default(),
            },
        ] {
            let fixture = evaluator_fixture("evaluator-terminal-prose");
            {
                let mut inner = fixture.state.inner.lock().unwrap();
                let i = inner.find_delegation_index(&fixture.delegation).unwrap();
                inner.delegations[i]
                    .acceptance_evaluation
                    .as_mut()
                    .unwrap()
                    .submission = submission.clone();
            }
            crate::tests::delegation_support::finish_delegation_child_with_assistant_text(
                &fixture.state,
                &fixture.child,
                &format!(
                    "## Result\n\nStatus: {packet_status}\n\nSummary:\nALL CRITERIA PASS fixture claim"
                ),
            );
            let result = fixture
                .state
                .get_delegation_result(&fixture.parent, &fixture.delegation)
                .unwrap()
                .result;
            assert_eq!(result.status, DelegationStatus::Failed);
            assert!(!result.summary.contains("ALL CRITERIA PASS"));
            assert!(result.summary.contains(if submission.is_none() {
                "no submission recorded"
            } else {
                "unconfirmed submission outcome"
            }));
            let saved = load_state(fixture.state.persistence_path.as_path())
                .unwrap()
                .unwrap();
            let target = saved
                .delegations
                .iter()
                .find(|d| d.id == fixture.delegation)
                .unwrap()
                .acceptance_evaluation
                .as_ref()
                .unwrap();
            assert!(
                target.submission == submission,
                "terminal presentation must preserve the original write state"
            );
        }
    }
}

#[test]
fn evaluator_recorded_submission_skips_finish_query_and_remains_authoritative() {
    let fixture = evaluator_fixture("evaluator-recorded");
    let submission = AcceptanceEvaluationSubmission::Recorded {
        receipt: AcceptanceEvaluationReceiptExtract::default(),
        recorded_at: stamp_now(),
    };
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let i = inner.find_delegation_index(&fixture.delegation).unwrap();
        inner.delegations[i]
            .acceptance_evaluation
            .as_mut()
            .unwrap()
            .submission = submission.clone();
        fixture.state.commit_locked(&mut inner).unwrap();
    }
    fixture.complete();
    let result = archive_and_result(&fixture);
    assert_eq!(result.status, DelegationStatus::Completed);
    assert!(result.reviewer_mcp_observations.is_empty());
    let saved = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let target = saved
        .delegations
        .iter()
        .find(|d| d.id == fixture.delegation)
        .unwrap()
        .acceptance_evaluation
        .as_ref()
        .unwrap();
    assert!(
        target.submission == submission,
        "recorded receipt must remain unchanged"
    );
}

#[test]
fn evaluator_readiness_cannot_cross_evaluation_attempts() {
    let fixture = evaluator_fixture("evaluator-replaced-attempt");
    let written = fixture.start();
    respond_start(&fixture, &written, evaluator_ready_page());
    let continuation = next_command(&fixture);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let i = inner.find_delegation_index(&fixture.delegation).unwrap();
        inner.delegations[i]
            .acceptance_evaluation
            .as_mut()
            .unwrap()
            .attempt_key = "replacement-attempt".into();
    }
    let wire = drive_ready(&fixture, continuation);
    assert!(
        wire.is_empty(),
        "a previous attempt cannot start model work for its replacement"
    );
    assert!(fixture.input_rx.try_recv().is_err());
}

#[test]
fn evaluator_recorded_result_uses_receipt_not_contradictory_completed_packet() {
    let fixture = evaluator_fixture("evaluator-recorded-contradiction");
    let submission = recorded_submission();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let i = inner.find_delegation_index(&fixture.delegation).unwrap();
        inner.delegations[i]
            .acceptance_evaluation
            .as_mut()
            .unwrap()
            .submission = submission.clone();
        let delegation = inner.delegations[i].clone();
        add_parent_delegation_card_locked(&mut inner, &delegation).unwrap();
    }
    crate::tests::delegation_support::finish_delegation_child_with_assistant_text(
        &fixture.state,
        &fixture.child,
        "## Result\n\nStatus: completed\n\nSummary:\nALL CRITERIA PASS fixture claim\n\nFindings:\n- Note: child-only judgment",
    );
    let result = fixture
        .state
        .get_delegation_result(&fixture.parent, &fixture.delegation)
        .unwrap()
        .result;
    assert_eq!(result.status, DelegationStatus::Completed);
    assert!(!result.summary.contains("ALL CRITERIA PASS"));
    assert!(
        result.findings.is_empty(),
        "child findings cannot become tracker verdicts"
    );
    assert!(result.summary.contains("1 of 2 criteria passed"));
    assert!(result.summary.contains("recorded at 2026-01-01T00:00:00Z"));
    for refreshed in [false, true] {
        if refreshed {
            fixture
                .state
                .refresh_acceptance_evaluation_card(&fixture.child);
        }
        let detail = evaluator_parent_card(&fixture);
        assert_eq!(
            detail.matches("Acceptance evaluation of").count(),
            1,
            "the parent card must decorate the receipt once"
        );
        assert!(detail.contains("1 of 2 criteria passed"));
        assert!(!detail.contains("ALL CRITERIA PASS"));
    }
    let inner = fixture.state.inner.lock().unwrap();
    let d = &inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()];
    assert!(
        d.acceptance_evaluation.as_ref().unwrap().submission == submission,
        "presentation must not rewrite a recorded receipt"
    );
    let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
    assert!(child.session.messages.iter().any(|message| matches!(message,
            Message::Text { author: Author::Assistant, text, .. } if text.contains("ALL CRITERIA PASS"))),
            "original child prose remains diagnostic transcript output");
}

#[test]
fn evaluator_failed_packet_with_trailing_output_uses_host_submission_state() {
    for status in [SessionStatus::Idle, SessionStatus::Active] {
        for submission in [AcceptanceEvaluationSubmission::None, recorded_submission()] {
            let fixture = evaluator_fixture("evaluator-failed-trailing-text");
            {
                let mut inner = fixture.state.inner.lock().unwrap();
                let i = inner.find_delegation_index(&fixture.delegation).unwrap();
                inner.delegations[i]
                    .acceptance_evaluation
                    .as_mut()
                    .unwrap()
                    .submission = submission.clone();
            }
            for text in [
                "## Result\n\nStatus: failed\n\nSummary:\nALL CRITERIA PASS fixture claim",
                "Trailing assistant chunk after the result packet.",
            ] {
                crate::tests::delegation_support::finish_delegation_child_with_assistant_text(
                    &fixture.state,
                    &fixture.child,
                    text,
                );
            }
            {
                let mut inner = fixture.state.inner.lock().unwrap();
                let i = inner.find_session_index(&fixture.child).unwrap();
                inner.sessions[i].session.status = status;
                inner.sessions[i].runtime = SessionRuntime::None;
            }
            let result = fixture
                .state
                .get_delegation_result(&fixture.parent, &fixture.delegation)
                .unwrap()
                .result;
            assert!(!result.summary.contains("ALL CRITERIA PASS"));
            if submission.is_none() {
                assert_eq!(result.status, DelegationStatus::Failed);
                assert!(result.summary.contains("no submission recorded"));
            } else {
                assert_eq!(result.status, DelegationStatus::Completed);
                assert!(result.summary.contains("1 of 2 criteria passed"));
                assert!(result.findings.is_empty());
            }
            let inner = fixture.state.inner.lock().unwrap();
            let d = &inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()];
            assert!(
                d.acceptance_evaluation.as_ref().unwrap().submission == submission,
                "packet presentation must preserve submission state"
            );
        }
    }
}

#[test]
fn evaluator_transport_error_does_not_adopt_an_older_failed_packet() {
    let fixture = evaluator_fixture("evaluator-real-transport-error");
    for text in [
        "## Result\n\nStatus: failed\n\nSummary:\nALL CRITERIA PASS fixture claim",
        "Trailing assistant chunk after the result packet.",
    ] {
        crate::tests::delegation_support::finish_delegation_child_with_assistant_text(
            &fixture.state,
            &fixture.child,
            text,
        );
    }
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let i = inner.find_session_index(&fixture.child).unwrap();
        inner.sessions[i].session.status = SessionStatus::Error;
        inner.sessions[i].session.preview = "fixture transport disconnected".to_owned();
    }
    let result = fixture
        .state
        .get_delegation_result(&fixture.parent, &fixture.delegation)
        .unwrap()
        .result;
    assert_eq!(result.status, DelegationStatus::Failed);
    assert_eq!(result.summary, "fixture transport disconnected");
}

#[test]
fn evaluator_completion_waits_for_submission_guard_release() {
    let pending = AcceptanceEvaluationSubmission::Pending {
        payload_digest: "fixture-digest".into(),
        started_at: stamp_now(),
        original: Default::default(),
    };
    let unconfirmed = AcceptanceEvaluationSubmission::Unconfirmed {
        payload_digest: "fixture-digest".into(),
        reason: "fixture acknowledgement failure".into(),
        at: stamp_now(),
        original: Default::default(),
    };
    for intermediate in [pending.clone(), recorded_submission()] {
        for settled in [
            AcceptanceEvaluationSubmission::None,
            pending.clone(),
            unconfirmed.clone(),
            recorded_submission(),
        ] {
            let fixture = evaluator_fixture("evaluator-submission-finishes-after-child");
            let guard = {
                let mut inner = fixture.state.inner.lock().unwrap();
                let i = inner.find_delegation_index(&fixture.delegation).unwrap();
                inner.delegations[i]
                    .acceptance_evaluation
                    .as_mut()
                    .unwrap()
                    .submission = intermediate.clone();
                let delegation = inner.delegations[i].clone();
                add_parent_delegation_card_locked(&mut inner, &delegation).unwrap();
                AcceptanceEvaluationSubmissionInFlight::admit_locked(
                    &fixture.state,
                    &mut inner,
                    &fixture.delegation,
                )
                .unwrap()
            };
            crate::tests::delegation_support::finish_delegation_child_with_assistant_text(
                &fixture.state,
                &fixture.child,
                "## Result\n\nStatus: completed\n\nSummary:\nALL CRITERIA PASS fixture claim",
            );
            fixture
                .state
                .refresh_delegation_for_child_session(&fixture.child)
                .unwrap();
            {
                let inner = fixture.state.inner.lock().unwrap();
                let d =
                    &inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()];
                assert_eq!(d.status, DelegationStatus::Running);
                assert!(
                    d.result.is_none(),
                    "an in-flight write cannot freeze a terminal result"
                );
            }
            let detail = evaluator_parent_card(&fixture);
            assert!(detail.contains("submission in progress"));
            assert!(!detail.contains("criteria passed"));
            {
                let mut inner = fixture.state.inner.lock().unwrap();
                let i = inner.find_delegation_index(&fixture.delegation).unwrap();
                inner.delegations[i]
                    .acceptance_evaluation
                    .as_mut()
                    .unwrap()
                    .submission = settled.clone();
            }
            // This is the production submission owner's release, with no
            // explicit refresh afterwards: it must settle the ended child.
            drop(guard);
            let inner = fixture.state.inner.lock().unwrap();
            let d = &inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()];
            assert!(
                !inner
                    .acceptance_evaluation_submissions_in_flight
                    .contains_key(&fixture.delegation)
            );
            assert!(
                d.acceptance_evaluation.as_ref().unwrap().submission == settled,
                "presentation must preserve the settled write or receipt"
            );
            let result = d
                .result
                .as_ref()
                .expect("guard release must refresh the ended child");
            assert!(!result.summary.contains("ALL CRITERIA PASS"));
            if matches!(settled, AcceptanceEvaluationSubmission::Recorded { .. }) {
                assert_eq!(result.status, DelegationStatus::Completed);
                assert!(result.summary.contains("1 of 2 criteria passed"));
            } else {
                assert_eq!(result.status, DelegationStatus::Failed);
                assert!(result.summary.contains(if settled.is_none() {
                    "no submission recorded"
                } else {
                    "unconfirmed submission outcome"
                }));
            }
        }
    }
}
