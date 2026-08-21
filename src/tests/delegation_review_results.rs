//! Structured delegated-review result protocol coverage.
//!
//! These tests keep prompt injection, typed submission, durable recovery,
//! lifecycle promotion, and fail-closed behavior together at the protocol
//! boundary rather than coupling them to generic mailbox or delegation tests.

use super::delegation_support::{
    finish_delegation_child_with_assistant_text, install_delegation_codex_runtime,
    test_app_state_with_drained_delegation_codex_runtime,
};
use super::mailboxes::mailbox_test_state;
use super::*;

fn structured_review_test_app_state() -> AppState {
    test_app_state_with_drained_delegation_codex_runtime("structured-review-test-runtime")
}

#[test]
fn reviewer_delegation_prompt_injects_termal_owned_result_protocol() {
    let record = DelegationRecord {
        id: "delegation-marker-test".to_owned(),
        parent_session_id: "session-parent".to_owned(),
        child_session_id: "session-child".to_owned(),
        mode: DelegationMode::Reviewer,
        status: DelegationStatus::Running,
        title: "Marker Test".to_owned(),
        prompt: "Run this repository's own review command.".to_owned(),
        cwd: "/tmp/termal-marker-test".to_owned(),
        agent: Agent::Codex,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: Some(stamp_now()),
        completed_at: None,
        result: None,
        submitted_review_result: None,
        post_submission_transport_error: None,
        review_result_recovery_probe_attempt: None,
        review_result_recovery_error: None,
        review_result_schema_version: None,
        review_result_required: true,
        review_result_submission_attempt: 1,
        result_parser_version: 0,
    };
    let prompt = build_delegation_prompt(&record);

    assert!(
        prompt.contains(DELEGATED_CHILD_SESSION_MARKER),
        "delegation runtime prompt must expose the delegated-session marker"
    );
    assert!(
        prompt.contains("TERMAL_STRUCTURED_REVIEW_RESULT_V1"),
        "TermAl must inject the structured result protocol independently of repository commands"
    );
    assert!(
        prompt.contains("TermAl control plane, not a workspace mutation"),
        "the built-in protocol must distinguish control-plane delivery from workspace writes"
    );
    assert!(
        prompt.contains("termal_submit_review_result"),
        "TermAl must tell every reviewer child how to submit the typed result"
    );
}

#[test]
fn non_reviewer_delegation_prompt_does_not_inject_review_result_protocol() {
    let record = DelegationRecord {
        id: "delegation-explorer-prompt".to_owned(),
        parent_session_id: "session-parent".to_owned(),
        child_session_id: "session-child".to_owned(),
        mode: DelegationMode::Explorer,
        status: DelegationStatus::Running,
        title: "Explorer".to_owned(),
        prompt: "Inspect one subsystem.".to_owned(),
        cwd: "/tmp/termal-explorer".to_owned(),
        agent: Agent::Codex,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: Some(stamp_now()),
        completed_at: None,
        result: None,
        submitted_review_result: None,
        post_submission_transport_error: None,
        review_result_recovery_probe_attempt: None,
        review_result_recovery_error: None,
        review_result_schema_version: None,
        review_result_required: false,
        review_result_submission_attempt: 0,
        result_parser_version: 0,
    };

    let prompt = build_delegation_prompt(&record);
    assert!(!prompt.contains("TERMAL_STRUCTURED_REVIEW_RESULT_V1"));
    assert!(!prompt.contains("termal_submit_review_result"));
}

#[test]
fn reviewer_spawn_requires_structured_result_without_repository_prompt_marker() {
    let state = structured_review_test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Use the repository's review workflow.".to_owned(),
                title: Some("Repository-owned review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("reviewer delegation should be created");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == created.delegation.id)
        .expect("delegation should exist");
    assert!(delegation.review_result_required);
    assert_eq!(delegation.review_result_submission_attempt, 1);
    drop(inner);
    assert!(state.delegation_control_plane_capability_allowed(
        &created.delegation.child_session_id,
        DelegationControlPlaneCapability::SubmitReviewResult,
    ));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn codex_reviewer_auto_accepts_only_authorized_review_result_control_plane_request() {
    let state = structured_review_test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review the current changes.".to_owned(),
                title: Some("Control-plane review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("reviewer delegation should be created");
    let request = json!({
        "id": "review-submit-approval",
        "params": {
            "threadId": "thread-reviewer",
            "turnId": "turn-reviewer",
            "serverName": TERMAL_DELEGATION_MCP_SERVER_NAME,
            "mode": "form",
            "message": "Approval copy is not part of the authorization contract.",
            "requestedSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            },
            "_meta": {
                "codex_approval_kind": "mcp_tool_call",
                "tool_description": TERMAL_SUBMIT_REVIEW_RESULT_TOOL_DESCRIPTION,
                "tool_params": { "schemaVersion": 1 }
            }
        }
    });
    let (input_tx, input_rx) = mpsc::channel();

    assert!(
        try_auto_respond_delegation_control_plane_request(
            "mcpServer/elicitation/request",
            &request,
            &state,
            &created.delegation.child_session_id,
            &input_tx,
        )
        .expect("authorized control-plane request should be handled")
    );
    let response = input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("Codex should receive an automatic control-plane response");
    assert!(matches!(
        response,
        CodexRuntimeCommand::JsonRpcResponse {
            response: CodexJsonRpcResponseCommand {
                request_id,
                payload: CodexJsonRpcResponsePayload::Result(result),
            }
        } if request_id == json!("review-submit-approval")
            && result == json!({ "action": "accept", "content": {} })
    ));

    let mut unrelated = request;
    unrelated["params"]["_meta"]["tool_description"] = json!("Different tool");
    assert!(
        !try_auto_respond_delegation_control_plane_request(
            "mcpServer/elicitation/request",
            &unrelated,
            &state,
            &created.delegation.child_session_id,
            &input_tx,
        )
        .expect("unrelated MCP request should remain interactive")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

fn install_required_review_delegation(
    state: &AppState,
    parent_session_id: &str,
) -> (String, String) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let delegation_id = inner.next_delegation_id();
    let child = inner.create_session(
        Agent::Codex,
        Some("Structured reviewer".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let child_session_id = child.session.id.clone();
    let child_index = inner
        .find_session_index(&child_session_id)
        .expect("review child should exist");
    inner.sessions[child_index].session.parent_delegation_id = Some(delegation_id.clone());
    inner.delegations.push(DelegationRecord {
        id: delegation_id.clone(),
        parent_session_id: parent_session_id.to_owned(),
        child_session_id: child_session_id.clone(),
        mode: DelegationMode::Reviewer,
        status: DelegationStatus::Running,
        title: "Structured review".to_owned(),
        prompt: "Use the repository's review workflow.".to_owned(),
        cwd: "/tmp".to_owned(),
        agent: Agent::Codex,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: Some(stamp_now()),
        completed_at: None,
        result: None,
        submitted_review_result: None,
        post_submission_transport_error: None,
        review_result_recovery_probe_attempt: None,
        review_result_recovery_error: None,
        review_result_schema_version: None,
        review_result_required: true,
        review_result_submission_attempt: 1,
        result_parser_version: 0,
    });
    state.commit_locked(&mut inner).unwrap();
    (delegation_id, child_session_id)
}

fn structured_review_request() -> SubmitDelegationReviewResultRequest {
    SubmitDelegationReviewResultRequest {
        schema_version: DELEGATION_REVIEW_RESULT_SCHEMA_VERSION,
        status: DelegationStatus::Completed,
        summary: "One medium issue found.".to_owned(),
        findings: vec![SubmitDelegationReviewFinding {
            severity: "Medium".to_owned(),
            file: Some("src/example.rs".to_owned()),
            line: Some(42),
            message: "The exact structured finding survives regardless of reviewer prose."
                .to_owned(),
        }],
        commands_run: vec![SubmitDelegationReviewCommand {
            command: "git status --short".to_owned(),
            status: DelegationReviewCommandStatus::Success,
        }],
        files_inspected: vec!["src/example.rs".to_owned()],
        notes: vec!["Review lenses ran inline.".to_owned()],
        suggested_tracker_updates: vec![
            "Proposal only: bug, priority 2 — preserve the exact finding.".to_owned(),
        ],
    }
}

#[test]
fn structured_review_result_uses_durable_mailbox_and_bypasses_prose_parser() {
    let (state, _root_sender_id, parent_session_id) = mailbox_test_state();
    let (delegation_id, child_session_id) =
        install_required_review_delegation(&state, &parent_session_id);

    let first = state
        .submit_delegation_review_result(&child_session_id, structured_review_request())
        .expect("structured result should be accepted");
    assert!(!first.duplicate);
    assert_eq!(first.notification_disposition, "durableButNotWoken");
    let duplicate = state
        .submit_delegation_review_result(&child_session_id, structured_review_request())
        .expect("an exact retry should be idempotent");
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.message_id, first.message_id);

    let stored = state
        .mailbox_store
        .read_delegation_review_result_for_recovery(
            &child_session_id,
            &delegation_review_result_idempotency_key(&delegation_id, 1),
        )
        .expect("recovery lookup should succeed")
        .expect("the durable result envelope should remain available to recovery");
    assert_eq!(
        stored.topic.as_deref(),
        Some(DELEGATION_REVIEW_RESULT_TOPIC)
    );
    assert_eq!(
        stored.state_stamp.as_deref(),
        Some(format!("{delegation_id}:1").as_str())
    );
    let envelope: DelegationReviewMailboxResult =
        serde_json::from_str(&stored.body).expect("mailbox body should be strict JSON");
    assert_eq!(envelope.kind, DELEGATION_REVIEW_RESULT_KIND);
    assert_eq!(envelope.submission_attempt, 1);
    assert_eq!(envelope.findings.len(), 1);
    assert!(
        state
            .mailbox_store
            .wakeups_for_session(
                &parent_session_id,
                MailboxWakeupRecovery::AllUnreadAfterBoot,
            )
            .expect("boot recovery query should succeed")
            .is_empty(),
        "structured review transport must stay non-waking after restart"
    );

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let parent = inner
            .sessions
            .iter()
            .find(|record| record.session.id == parent_session_id)
            .expect("parent should exist");
        assert!(
            parent.queued_prompts.is_empty(),
            "review-result transport must not wake the parent before fan-in"
        );
    }

    finish_delegation_child_with_assistant_text(
        &state,
        &child_session_id,
        "## Result\nStatus: completed\nSummary: One high issue exists.\nFindings:\n- None",
    );
    let result = state
        .get_delegation_result(&parent_session_id, &delegation_id)
        .expect("completed structured review should have a result")
        .result;
    assert_eq!(result.status, DelegationStatus::Completed);
    assert_eq!(result.summary, "One medium issue found.");
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].severity, "Medium");
    assert!(
        result
            .notes
            .iter()
            .any(|note| note == "Inspected src/example.rs")
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .delegations
        .iter()
        .find(|record| record.id == delegation_id)
        .expect("delegation should remain persisted");
    assert_eq!(
        record.review_result_schema_version,
        Some(DELEGATION_REVIEW_RESULT_SCHEMA_VERSION)
    );
    drop(inner);

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_delegation_index(&delegation_id)
            .expect("delegation should exist");
        rearm_terminal_delegation_for_followup_locked(&mut inner, index)
            .expect("completed review should rearm");
        state.commit_locked(&mut inner).unwrap();
    }
    let mut followup_request = structured_review_request();
    followup_request.summary = "Follow-up review is clean.".to_owned();
    followup_request.findings.clear();
    let followup = state
        .submit_delegation_review_result(&child_session_id, followup_request)
        .expect("a rearmed review should receive a new idempotency scope");
    assert!(!followup.duplicate);
    assert_eq!(followup.sequence, first.sequence + 1);
    let followup_message = state
        .mailbox_store
        .read_delegation_review_result_for_recovery(
            &child_session_id,
            &delegation_review_result_idempotency_key(&delegation_id, 2),
        )
        .expect("follow-up recovery lookup should succeed")
        .expect("follow-up result envelope should be readable by recovery");
    assert_eq!(followup_message.message_id, followup.message_id);
    assert_eq!(
        followup_message.state_stamp.as_deref(),
        Some(format!("{delegation_id}:2").as_str())
    );
    finish_delegation_child_with_assistant_text(
        &state,
        &child_session_id,
        "## Result\nStatus: completed\nSummary: One high issue exists.\nFindings:\n- None",
    );
    let followup_result = state
        .get_delegation_result(&parent_session_id, &delegation_id)
        .expect("clean structured follow-up should complete")
        .result;
    assert_eq!(followup_result.summary, "Follow-up review is clean.");
    assert!(
        followup_result.findings.is_empty(),
        "an explicit empty structured list is the only clean authority"
    );
}

#[test]
fn structured_review_submission_rejects_a_rearmed_attempt_before_recording() {
    let (state, _root_sender_id, parent_session_id) = mailbox_test_state();
    let (_delegation_id, child_session_id) =
        install_required_review_delegation(&state, &parent_session_id);
    let submission = state
        .validate_delegation_review_submission(&child_session_id, structured_review_request())
        .expect("initial attempt should validate");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_delegation_index_by_child_session_id(&child_session_id)
            .expect("delegation should exist");
        inner.delegations[index].review_result_submission_attempt += 1;
        state.commit_locked(&mut inner).unwrap();
    }

    let error = state
        .record_delegation_review_submission(&submission)
        .expect_err("an earlier attempt must not attach to a rearmed delegation");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("submit a fresh structured result"));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .delegations
        .iter()
        .find(|record| record.child_session_id == child_session_id)
        .expect("delegation should remain available");
    assert!(record.submitted_review_result.is_none());
}

#[test]
fn concurrent_different_review_submissions_promote_the_mailbox_winner() {
    let (state, _root_sender_id, parent_session_id) = mailbox_test_state();
    let (delegation_id, child_session_id) =
        install_required_review_delegation(&state, &parent_session_id);

    // Validate both requests before either one reaches durable storage. This
    // pins the narrow race where two callers hold valid snapshots for the
    // same delegation attempt.
    let first_submission = state
        .validate_delegation_review_submission(&child_session_id, structured_review_request())
        .expect("first submission should validate");
    let mut different_request = structured_review_request();
    different_request.summary = "A different low issue found.".to_owned();
    different_request.findings[0].severity = "Low".to_owned();
    different_request.findings[0].message =
        "Only the payload stored in the mailbox may reach delegation state.".to_owned();
    let second_submission = state
        .validate_delegation_review_submission(&child_session_id, different_request)
        .expect("second submission should validate against the same attempt");

    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut handles = Vec::new();
    for submission in [first_submission, second_submission] {
        let state = state.clone();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            state.persist_validated_delegation_review_submission(&submission)
        }));
    }
    barrier.wait();

    let mut accepted = None;
    let mut rejected = None;
    for handle in handles {
        match handle.join().expect("submission thread should join") {
            Ok(receipt) => {
                assert!(accepted.replace(receipt).is_none(), "only one payload wins");
            }
            Err(error) => {
                assert!(rejected.replace(error).is_none(), "only one payload loses");
            }
        }
    }
    let accepted = accepted.expect("one payload should commit");
    let rejected = rejected.expect("the differing payload should conflict");
    assert_eq!(rejected.status, StatusCode::CONFLICT);
    assert!(rejected.message.contains("different mailbox message"));

    let stored_message = state
        .mailbox_store
        .read_delegation_review_result_for_recovery(
            &child_session_id,
            &delegation_review_result_idempotency_key(&delegation_id, 1),
        )
        .expect("recovery lookup should succeed")
        .expect("the winning mailbox envelope should be readable by recovery");
    assert_eq!(stored_message.message_id, accepted.message_id);
    let stored_envelope: DelegationReviewMailboxResult =
        serde_json::from_str(&stored_message.body).expect("stored envelope should be valid JSON");
    let expected_result = delegation_result_from_review_envelope(&stored_envelope);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .delegations
        .iter()
        .find(|record| record.child_session_id == child_session_id)
        .expect("delegation should remain available");
    assert_eq!(
        record.submitted_review_result.as_ref(),
        Some(&expected_result)
    );
}

#[test]
fn durable_review_envelope_repairs_terminal_observation_before_state_recording() {
    let (state, _root_sender_id, parent_session_id) = mailbox_test_state();
    let (delegation_id, child_session_id) =
        install_required_review_delegation(&state, &parent_session_id);
    let submission = state
        .validate_delegation_review_submission(&child_session_id, structured_review_request())
        .expect("review submission should validate");
    let input = MailboxAppendInput {
        sender_session_id: submission.child_session_id.clone(),
        sender_name: submission.sender_name.clone(),
        target_session_id: submission.parent_session_id.clone(),
        target_name: submission.target_name.clone(),
        body: serde_json::to_string(&submission.envelope).expect("review envelope should encode"),
        idempotency_key: delegation_review_result_idempotency_key(
            &submission.delegation_id,
            submission.submission_attempt,
        ),
        topic: Some(DELEGATION_REVIEW_RESULT_TOPIC.to_owned()),
        state_stamp: Some(format!(
            "{}:{}",
            submission.delegation_id, submission.submission_attempt
        )),
    };
    let appended = state
        .mailbox_store
        .append(&input)
        .expect("mailbox append should commit before the simulated interruption");
    state
        .mailbox_store
        .record_initial_dispatch_outcome(&appended.receipt.message_id, "durableButNotWoken")
        .expect("non-waking review envelope should finalize");

    finish_delegation_child_with_assistant_text(
        &state,
        &child_session_id,
        "Reviewer prose is not the authoritative result.",
    );
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_delegation_index(&delegation_id)
            .expect("delegation should exist");
        let delta = refresh_delegation_from_child_locked(&mut inner, index)
            .expect("terminal observation should initially fail closed");
        assert!(matches!(delta, DelegationLifecycleDelta::Failed { .. }));
        state.commit_locked(&mut inner).unwrap();
    }

    let result = state
        .get_delegation_result(&parent_session_id, &delegation_id)
        .expect("status read should recover and promote the durable envelope")
        .result;
    assert_eq!(result.status, DelegationStatus::Completed);
    assert_eq!(result.summary, "One medium issue found.");
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].severity, "Medium");
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .delegations
        .iter()
        .find(|record| record.id == delegation_id)
        .expect("delegation should remain available");
    assert_eq!(
        record.review_result_schema_version,
        Some(DELEGATION_REVIEW_RESULT_SCHEMA_VERSION)
    );
}

#[test]
fn invalid_durable_review_envelope_is_quarantined_without_bricking_lifecycle_apis() {
    let (state, _root_sender_id, parent_session_id) = mailbox_test_state();
    let (delegation_id, child_session_id) =
        install_required_review_delegation(&state, &parent_session_id);
    let submission = state
        .validate_delegation_review_submission(&child_session_id, structured_review_request())
        .expect("review submission metadata should validate");
    let input = MailboxAppendInput {
        sender_session_id: submission.child_session_id.clone(),
        sender_name: submission.sender_name.clone(),
        target_session_id: submission.parent_session_id.clone(),
        target_name: submission.target_name.clone(),
        body: "{not-valid-json".to_owned(),
        idempotency_key: delegation_review_result_idempotency_key(
            &submission.delegation_id,
            submission.submission_attempt,
        ),
        topic: Some(DELEGATION_REVIEW_RESULT_TOPIC.to_owned()),
        state_stamp: Some(format!(
            "{}:{}",
            submission.delegation_id, submission.submission_attempt
        )),
    };
    state
        .mailbox_store
        .append_delegation_review_result(&input)
        .expect("corrupt fixture should model an already-durable artifact");
    finish_delegation_child_with_assistant_text(
        &state,
        &child_session_id,
        "Reviewer prose remains available for output paging.",
    );

    let status = state
        .get_delegation(&parent_session_id, &delegation_id)
        .expect("status must survive a corrupt durable envelope");
    assert_eq!(status.delegation.status, DelegationStatus::Failed);
    assert!(
        status
            .delegation
            .review_result_recovery_error
            .as_deref()
            .is_some_and(|reason| reason.contains("JSON is invalid"))
    );
    assert_eq!(
        status.delegation.review_result_recovery_probe_attempt,
        Some(1)
    );

    let result = state
        .get_delegation_result(&parent_session_id, &delegation_id)
        .expect("result must remain readable after quarantine")
        .result;
    assert_eq!(result.status, DelegationStatus::Failed);
    assert!(
        result
            .notes
            .iter()
            .any(|note| note.contains("quarantined during recovery"))
    );
    state
        .get_delegation_result_output(
            &parent_session_id,
            &delegation_id,
            0,
            MIN_DELEGATION_RESULT_OUTPUT_PAGE_BYTES,
        )
        .expect("full output must remain pageable after quarantine");
    state
        .cancel_delegation(&parent_session_id, &delegation_id)
        .expect("cancel must remain idempotently readable for the failed delegation");

    install_delegation_codex_runtime(&state, "quarantined-review-followup");
    let followup = state
        .followup_delegation(
            &parent_session_id,
            &delegation_id,
            "Review the corrected attempt.".to_owned(),
        )
        .expect("follow-up must rearm instead of being blocked by the quarantined artifact");
    assert_eq!(followup.delegation.status, DelegationStatus::Running);
    assert_eq!(followup.delegation.review_result_submission_attempt, 2);
    assert_eq!(
        followup.delegation.review_result_recovery_probe_attempt,
        None
    );
    assert_eq!(followup.delegation.review_result_recovery_error, None);

    state
        .submit_delegation_review_result(&child_session_id, structured_review_request())
        .expect("the rearmed attempt should accept a valid structured result");
    finish_delegation_child_with_assistant_text(
        &state,
        &child_session_id,
        "The corrected attempt completed.",
    );
    let recovered = state
        .get_delegation_result(&parent_session_id, &delegation_id)
        .expect("the next attempt's valid envelope should promote normally")
        .result;
    assert_eq!(recovered.status, DelegationStatus::Completed);
    assert_eq!(recovered.summary, "One medium issue found.");
}

#[test]
fn mismatched_durable_review_envelope_is_quarantined_without_an_api_error() {
    let (state, _root_sender_id, parent_session_id) = mailbox_test_state();
    let (delegation_id, child_session_id) =
        install_required_review_delegation(&state, &parent_session_id);
    let submission = state
        .validate_delegation_review_submission(&child_session_id, structured_review_request())
        .expect("review submission metadata should validate");
    let input = MailboxAppendInput {
        sender_session_id: submission.child_session_id.clone(),
        sender_name: submission.sender_name.clone(),
        target_session_id: submission.parent_session_id.clone(),
        target_name: submission.target_name.clone(),
        body: serde_json::to_string(&submission.envelope).expect("envelope should encode"),
        idempotency_key: delegation_review_result_idempotency_key(
            &submission.delegation_id,
            submission.submission_attempt,
        ),
        topic: Some(DELEGATION_REVIEW_RESULT_TOPIC.to_owned()),
        state_stamp: Some("wrong-attempt-stamp".to_owned()),
    };
    state
        .mailbox_store
        .append_delegation_review_result(&input)
        .expect("mismatched fixture should model an already-durable artifact");
    finish_delegation_child_with_assistant_text(
        &state,
        &child_session_id,
        "Reviewer prose is not authoritative.",
    );

    let status = state
        .get_delegation(&parent_session_id, &delegation_id)
        .expect("metadata mismatch must not escape as an API error");
    assert_eq!(status.delegation.status, DelegationStatus::Failed);
    assert!(
        status
            .delegation
            .review_result_recovery_error
            .as_deref()
            .is_some_and(|reason| reason.contains("metadata does not match"))
    );
}

#[test]
fn durable_review_recovery_propagates_primary_state_persistence_failures() {
    let (mut state, _root_sender_id, parent_session_id) = mailbox_test_state();
    let (delegation_id, child_session_id) =
        install_required_review_delegation(&state, &parent_session_id);
    let submission = state
        .validate_delegation_review_submission(&child_session_id, structured_review_request())
        .expect("review submission metadata should validate");
    let input = MailboxAppendInput {
        sender_session_id: submission.child_session_id.clone(),
        sender_name: submission.sender_name.clone(),
        target_session_id: submission.parent_session_id.clone(),
        target_name: submission.target_name.clone(),
        body: serde_json::to_string(&submission.envelope).expect("envelope should encode"),
        idempotency_key: delegation_review_result_idempotency_key(
            &submission.delegation_id,
            submission.submission_attempt,
        ),
        topic: Some(DELEGATION_REVIEW_RESULT_TOPIC.to_owned()),
        state_stamp: Some(format!(
            "{}:{}",
            submission.delegation_id, submission.submission_attempt
        )),
    };
    state
        .mailbox_store
        .append_delegation_review_result(&input)
        .expect("fixture should leave the validated envelope durable but unprojected");
    finish_delegation_child_with_assistant_text(
        &state,
        &child_session_id,
        "Reviewer prose is not authoritative.",
    );

    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-review-recovery-persist-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");
    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());

    let error = match state.get_delegation_result(&parent_session_id, &delegation_id) {
        Ok(_) => panic!("a genuine primary-state projection failure must block lifecycle refresh"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        error
            .message
            .contains("failed to persist structured delegation review result")
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .delegations
        .iter()
        .find(|record| record.id == delegation_id)
        .expect("delegation should remain available");
    assert_eq!(record.status, DelegationStatus::Running);
    assert!(
        record.submitted_review_result.is_some(),
        "the accepted envelope may stay provisional, but fan-in must not advance"
    );
    drop(inner);
    let _ = fs::remove_dir_all(failing_persistence_path);
}

#[tokio::test]
async fn structured_review_envelopes_stay_out_of_routine_mailbox_surfaces() {
    let (state, _root_sender_id, parent_session_id) = mailbox_test_state();
    let (_delegation_id, child_session_id) =
        install_required_review_delegation(&state, &parent_session_id);
    let submission = state
        .validate_delegation_review_submission(&child_session_id, structured_review_request())
        .expect("review submission metadata should validate");
    let receipt = state
        .persist_validated_delegation_review_submission(&submission)
        .expect("structured review result should persist");
    let idempotency_key = delegation_review_result_idempotency_key(
        &submission.delegation_id,
        submission.submission_attempt,
    );
    assert!(
        state
            .mailbox_store
            .read_delegation_review_result_for_recovery(&child_session_id, &idempotency_key)
            .expect("the dedicated recovery read should succeed")
            .is_some(),
        "presentation filtering must not hide the durable row from recovery"
    );
    {
        let connection = state
            .mailbox_store
            .connection()
            .expect("mailbox connection should be available");
        let participants = mailbox_participants(&connection, &receipt.mailbox_id)
            .expect("mailbox participants should decode");
        assert_eq!(
            participants
                .iter()
                .find(|participant| participant.session_id == parent_session_id)
                .expect("parent should participate")
                .processed_through,
            0,
            "control-plane filtering must not advance the parent's cursor"
        );
    }

    let Json(summaries) = list_mailboxes(AxumPath(parent_session_id.clone()), State(state.clone()))
        .await
        .expect("parent mailbox listing should succeed");
    assert!(
        summaries
            .iter()
            .all(|summary| summary.id != receipt.mailbox_id),
        "a control-plane-only mailbox must not appear as routine unread work"
    );
    let Json(messages) = read_mailbox(
        AxumPath((parent_session_id.clone(), receipt.mailbox_id.clone())),
        State(state.clone()),
        Json(ReadMailboxRequest {
            after_sequence: 0,
            limit: 20,
        }),
    )
    .await
    .expect("routine mailbox read should succeed");
    assert!(messages.is_empty());
    assert!(
        state
            .mailbox_store
            .unread_wakeup_for_mailbox(&parent_session_id, &receipt.mailbox_id)
            .expect("wakeup lookup should succeed")
            .is_none()
    );
    assert!(
        state
            .mailbox_store
            .unread_wakeups_for_session(&parent_session_id)
            .expect("session wakeup lookup should succeed")
            .is_empty()
    );

    let ordinary = state
        .mailbox_store
        .append(&MailboxAppendInput {
            sender_session_id: child_session_id,
            sender_name: submission.sender_name,
            target_session_id: parent_session_id.clone(),
            target_name: submission.target_name,
            body: "Visible coordination update.".to_owned(),
            idempotency_key: "visible-after-review-result".to_owned(),
            topic: Some("coordination".to_owned()),
            state_stamp: None,
        })
        .expect("ordinary message should share the mailbox safely")
        .receipt;
    assert_eq!(ordinary.mailbox_id, receipt.mailbox_id);
    state
        .mailbox_store
        .append(&MailboxAppendInput {
            sender_session_id: submission.child_session_id,
            sender_name: "Structured reviewer".to_owned(),
            target_session_id: parent_session_id.clone(),
            target_name: "Fable".to_owned(),
            body: "Later technical envelope.".to_owned(),
            idempotency_key: "later-review-control-envelope".to_owned(),
            topic: Some(DELEGATION_REVIEW_RESULT_TOPIC.to_owned()),
            state_stamp: None,
        })
        .expect("a later control-plane row should append without becoming visible");

    let Json(summaries) = list_mailboxes(AxumPath(parent_session_id.clone()), State(state.clone()))
        .await
        .expect("parent mailbox listing should include ordinary content");
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].unread_count, 1);
    assert_eq!(summaries[0].latest_sequence, ordinary.sequence);
    assert_eq!(
        summaries[0].latest_message_preview.as_deref(),
        Some("Visible coordination update.")
    );
    let wakeups = state
        .mailbox_store
        .unread_wakeups_for_session(&parent_session_id)
        .expect("session wakeup lookup should succeed");
    assert_eq!(wakeups.len(), 1);
    assert_eq!(wakeups[0].sequence, ordinary.sequence);
    assert_eq!(wakeups[0].unread_count, 1);
    let Json(messages) = read_mailbox(
        AxumPath((parent_session_id, receipt.mailbox_id)),
        State(state),
        Json(ReadMailboxRequest {
            after_sequence: 0,
            limit: 20,
        }),
    )
    .await
    .expect("routine mailbox read should include ordinary content");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].body, "Visible coordination update.");
}

#[test]
fn stale_recovery_probe_cannot_cross_a_submission_attempt_boundary() {
    let (state, _root_sender_id, parent_session_id) = mailbox_test_state();
    let (delegation_id, child_session_id) =
        install_required_review_delegation(&state, &parent_session_id);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_delegation_index(&delegation_id)
            .expect("delegation should exist");
        inner.delegations[index].review_result_submission_attempt = 2;
        inner.mark_delegation_mutated(index);
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .record_delegation_review_recovery_probe(
            &child_session_id,
            &delegation_id,
            &parent_session_id,
            1,
            None,
        )
        .expect("a stale probe should be dropped without error");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .delegations
        .iter()
        .find(|record| record.id == delegation_id)
        .expect("delegation should remain available");
    assert_eq!(record.review_result_recovery_probe_attempt, None);
}

#[test]
fn structured_review_result_enforces_aggregate_mailbox_envelope_limit() {
    let (state, _root_sender_id, parent_session_id) = mailbox_test_state();
    let (_delegation_id, child_session_id) =
        install_required_review_delegation(&state, &parent_session_id);

    let mut accepted = structured_review_request();
    accepted.notes = vec!["a".repeat(MAX_DELEGATION_REVIEW_RESULT_TEXT_CHARS); 15];
    state
        .validate_delegation_review_submission(&child_session_id, accepted)
        .expect("a review envelope below the aggregate mailbox cap should validate");

    let mut rejected = structured_review_request();
    rejected.notes = vec!["b".repeat(MAX_DELEGATION_REVIEW_RESULT_TEXT_CHARS); 16];
    let error = state
        .validate_delegation_review_submission(&child_session_id, rejected)
        .expect_err("a review envelope above the aggregate mailbox cap must fail early");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("aggregate envelope limit"));
    assert!(error.message.contains(&MAX_MAILBOX_BODY_BYTES.to_string()));
}

#[test]
fn required_review_result_fails_closed_when_submission_is_missing() {
    let (state, _root_sender_id, parent_session_id) = mailbox_test_state();
    let (delegation_id, child_session_id) =
        install_required_review_delegation(&state, &parent_session_id);
    finish_delegation_child_with_assistant_text(
        &state,
        &child_session_id,
        "## Result\nStatus: completed\nSummary: Two high issues found.\nFindings:\n- None",
    );

    let result = state
        .get_delegation_result(&parent_session_id, &delegation_id)
        .expect("missing structured result should become terminal and explicit")
        .result;
    assert_eq!(result.status, DelegationStatus::Failed);
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].severity, "Unavailable");
    assert!(
        result.findings[0]
            .message
            .contains("unavailable, not empty")
    );
}

#[test]
fn removed_review_child_does_not_hide_the_persisted_failed_result() {
    let (state, _root_sender_id, parent_session_id) = mailbox_test_state();
    let (delegation_id, child_session_id) =
        install_required_review_delegation(&state, &parent_session_id);
    finish_delegation_child_with_assistant_text(
        &state,
        &child_session_id,
        "Reviewer prose is not an authoritative structured result.",
    );

    let initial = state
        .get_delegation_result(&parent_session_id, &delegation_id)
        .expect("missing structured result should fail closed")
        .result;
    assert_eq!(initial.status, DelegationStatus::Failed);
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .delegations
            .iter()
            .find(|record| record.id == delegation_id)
            .expect("delegation should remain available");
        assert_eq!(
            record.review_result_recovery_probe_attempt,
            Some(record.review_result_submission_attempt),
            "a terminal review without an envelope should cache one probe on its record"
        );
    }
    state
        .kill_session(&child_session_id)
        .expect("review child removal should succeed");

    let status = state
        .get_delegation(&parent_session_id, &delegation_id)
        .expect("missing child must not turn persisted delegation status into a 404");
    assert_eq!(status.delegation.status, DelegationStatus::Failed);
    let recovered = state
        .get_delegation_result(&parent_session_id, &delegation_id)
        .expect("missing child must not hide the persisted failed result")
        .result;
    assert_eq!(recovered, initial);
}

#[test]
fn completed_structured_review_survives_later_child_runtime_failure() {
    let (state, _root_sender_id, parent_session_id) = mailbox_test_state();
    let (delegation_id, child_session_id) =
        install_required_review_delegation(&state, &parent_session_id);
    let submission = state
        .validate_delegation_review_submission(&child_session_id, structured_review_request())
        .expect("completed structured result should validate");
    let expected = delegation_result_from_review_envelope(&submission.envelope);
    state
        .persist_validated_delegation_review_submission(&submission)
        .expect("completed structured result should be stored provisionally");
    let wait = state
        .create_delegation_wait(
            &parent_session_id,
            CreateDelegationWaitRequest {
                delegation_ids: vec![delegation_id.clone()],
                mode: DelegationWaitMode::All,
                title: Some("Structured transport failure fan-in".to_owned()),
            },
        )
        .expect("running delegation wait should be accepted");
    assert!(!wait.resume_prompt_queued);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&child_session_id)
            .expect("review child should exist");
        let child = inner
            .session_mut_by_index(child_index)
            .expect("review child index should be valid");
        child.session.status = SessionStatus::Error;
        child.session.preview = "runtime exited after the review result was accepted".to_owned();
        state.commit_locked(&mut inner).unwrap();
    }

    let result = state
        .get_delegation_result(&parent_session_id, &delegation_id)
        .expect("accepted structured result should survive the later runtime failure")
        .result;
    assert_eq!(result, expected, "the submitted envelope must stay exact");
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .delegations
        .iter()
        .find(|record| record.id == delegation_id)
        .expect("delegation should remain available");
    assert_eq!(record.status, DelegationStatus::Completed);
    assert!(
        record
            .post_submission_transport_error
            .as_deref()
            .is_some_and(|detail| detail.contains("runtime exited after"))
    );
    assert!(inner.delegation_waits.is_empty());
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should remain available");
    assert_eq!(parent.queued_prompts.len(), 1);
    let resume_prompt = &parent.queued_prompts[0].pending_prompt.text;
    assert!(resume_prompt.contains("Structured transport failure fan-in"));
    assert!(resume_prompt.contains("One medium issue found."));
}

#[test]
fn completed_structured_review_survives_idle_child_without_final_prose() {
    let (state, _root_sender_id, parent_session_id) = mailbox_test_state();
    let (delegation_id, child_session_id) =
        install_required_review_delegation(&state, &parent_session_id);
    let submission = state
        .validate_delegation_review_submission(&child_session_id, structured_review_request())
        .expect("completed structured result should validate");
    let expected = delegation_result_from_review_envelope(&submission.envelope);
    state
        .persist_validated_delegation_review_submission(&submission)
        .expect("completed structured result should be stored provisionally");

    let result = state
        .get_delegation_result(&parent_session_id, &delegation_id)
        .expect("accepted structured result should not depend on final prose")
        .result;
    assert_eq!(result, expected);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .delegations
        .iter()
        .find(|record| record.id == delegation_id)
        .expect("delegation should remain available");
    assert!(
        record
            .post_submission_transport_error
            .as_deref()
            .is_some_and(|detail| detail.contains("idle without a final assistant packet"))
    );
}

#[test]
fn completed_structured_review_survives_child_session_removal() {
    let (state, _root_sender_id, parent_session_id) = mailbox_test_state();
    let (delegation_id, child_session_id) =
        install_required_review_delegation(&state, &parent_session_id);
    let submission = state
        .validate_delegation_review_submission(&child_session_id, structured_review_request())
        .expect("completed structured result should validate");
    let expected = delegation_result_from_review_envelope(&submission.envelope);
    state
        .persist_validated_delegation_review_submission(&submission)
        .expect("completed structured result should be stored provisionally");

    state
        .kill_session(&child_session_id)
        .expect("review child removal should succeed");
    let result = state
        .get_delegation_result(&parent_session_id, &delegation_id)
        .expect("accepted structured result should survive child removal")
        .result;
    assert_eq!(result, expected);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .delegations
        .iter()
        .find(|record| record.id == delegation_id)
        .expect("delegation should remain available");
    assert_eq!(record.status, DelegationStatus::Completed);
    assert!(
        record
            .post_submission_transport_error
            .as_deref()
            .is_some_and(|detail| detail.contains("session was removed"))
    );
}

#[test]
fn structured_review_output_paging_ignores_legacy_parser_version_stamps() {
    let (state, _root_sender_id, parent_session_id) = mailbox_test_state();
    let (delegation_id, child_session_id) =
        install_required_review_delegation(&state, &parent_session_id);
    state
        .submit_delegation_review_result(&child_session_id, structured_review_request())
        .expect("structured result should be accepted");
    finish_delegation_child_with_assistant_text(
        &state,
        &child_session_id,
        &"structured full output ".repeat(300),
    );
    state
        .refresh_delegation_for_child_session(&child_session_id)
        .expect("structured review should complete");

    let revision = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_delegation_index(&delegation_id)
            .expect("delegation should exist");
        inner.delegations[index].result_parser_version =
            DELEGATION_RESULT_PARSER_VERSION.saturating_sub(1);
        inner.mark_delegation_mutated(index);
        state.commit_locked(&mut inner).unwrap()
    };

    let page = state
        .get_delegation_result_output(
            &parent_session_id,
            &delegation_id,
            0,
            MIN_DELEGATION_RESULT_OUTPUT_PAGE_BYTES,
        )
        .expect("structured full output should page without parser repair");
    assert_eq!(page.revision, revision);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .delegations
        .iter()
        .find(|record| record.id == delegation_id)
        .expect("delegation should remain available");
    assert_eq!(
        record.result_parser_version,
        DELEGATION_RESULT_PARSER_VERSION.saturating_sub(1)
    );
}

#[test]
fn structured_failed_review_preserves_typed_blocker() {
    let (state, _root_sender_id, parent_session_id) = mailbox_test_state();
    let (delegation_id, child_session_id) =
        install_required_review_delegation(&state, &parent_session_id);
    let mut request = structured_review_request();
    request.status = DelegationStatus::Failed;
    request.summary = "Required repository inspection was unavailable.".to_owned();
    request.findings.clear();
    request.notes = vec!["The local file tool could not be started.".to_owned()];
    state
        .submit_delegation_review_result(&child_session_id, request)
        .expect("typed failed review should be accepted");
    finish_delegation_child_with_assistant_text(
        &state,
        &child_session_id,
        "## Result\nStatus: failed\nSummary: Unparseable human wording.",
    );

    let result = state
        .get_delegation_result(&parent_session_id, &delegation_id)
        .expect("typed failed review should be terminal")
        .result;
    assert_eq!(result.status, DelegationStatus::Failed);
    assert_eq!(
        result.summary,
        "Required repository inspection was unavailable."
    );
    assert_eq!(
        result.notes,
        vec![
            "The local file tool could not be started.".to_owned(),
            "Inspected src/example.rs".to_owned(),
            "Suggested tracker update: Proposal only: bug, priority 2 — preserve the exact finding."
                .to_owned(),
        ]
    );
}

#[test]
fn failed_structured_review_survives_later_child_runtime_failure() {
    let (state, _root_sender_id, parent_session_id) = mailbox_test_state();
    let (delegation_id, child_session_id) =
        install_required_review_delegation(&state, &parent_session_id);
    let mut request = structured_review_request();
    request.status = DelegationStatus::Failed;
    request.summary = "Review could not inspect the required source.".to_owned();
    request.findings.clear();
    let submission = state
        .validate_delegation_review_submission(&child_session_id, request)
        .expect("failed structured result should validate");
    let expected = delegation_result_from_review_envelope(&submission.envelope);
    state
        .persist_validated_delegation_review_submission(&submission)
        .expect("failed structured result should be stored provisionally");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&child_session_id)
            .expect("review child should exist");
        let child = inner
            .session_mut_by_index(child_index)
            .expect("review child index should be valid");
        child.session.status = SessionStatus::Error;
        child.session.preview = "runtime also failed after submission".to_owned();
        state.commit_locked(&mut inner).unwrap();
    }

    let result = state
        .get_delegation_result(&parent_session_id, &delegation_id)
        .expect("accepted failed result should remain authoritative")
        .result;
    assert_eq!(result, expected);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .delegations
        .iter()
        .find(|record| record.id == delegation_id)
        .expect("delegation should remain available");
    assert_eq!(record.status, DelegationStatus::Failed);
    assert!(
        record
            .post_submission_transport_error
            .as_deref()
            .is_some_and(|detail| detail.contains("runtime also failed"))
    );
}

#[test]
fn cancellation_retains_an_accepted_structured_result_without_promoting_it() {
    let (state, _root_sender_id, parent_session_id) = mailbox_test_state();
    let (delegation_id, child_session_id) =
        install_required_review_delegation(&state, &parent_session_id);
    let submission = state
        .validate_delegation_review_submission(&child_session_id, structured_review_request())
        .expect("completed structured result should validate");
    let expected = delegation_result_from_review_envelope(&submission.envelope);
    state
        .persist_validated_delegation_review_submission(&submission)
        .expect("completed structured result should be stored provisionally");

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let delegation_index = inner
        .find_delegation_index(&delegation_id)
        .expect("delegation should exist");
    let delta = mark_delegation_canceled_locked(
        &mut inner,
        delegation_index,
        Some("Canceled by the coordinator.".to_owned()),
    )
    .expect("running delegation should become canceled");
    assert!(matches!(delta, DelegationLifecycleDelta::Canceled { .. }));
    let record = inner
        .delegations
        .get(delegation_index)
        .expect("delegation should remain available");
    assert_eq!(record.status, DelegationStatus::Canceled);
    assert_eq!(record.submitted_review_result.as_ref(), Some(&expected));
    assert_eq!(
        record.result.as_ref().map(|result| result.status),
        Some(DelegationStatus::Canceled)
    );
}

#[test]
fn structured_review_request_rejects_unknown_nested_fields() {
    let error = serde_json::from_value::<SubmitDelegationReviewResultRequest>(json!({
        "schemaVersion": 1,
        "status": "completed",
        "summary": "Clean review.",
        "findings": [{
            "severity": "Low",
            "message": "Example",
            "unexpected": true
        }],
        "commandsRun": [],
        "filesInspected": [],
        "notes": [],
        "suggestedTrackerUpdates": []
    }))
    .expect_err("nested result objects must reject unknown fields");
    assert!(error.to_string().contains("unknown field `unexpected`"));
}

#[test]
fn structured_review_request_rejects_unknown_command_status() {
    let error = serde_json::from_value::<SubmitDelegationReviewResultRequest>(json!({
        "schemaVersion": 1,
        "status": "completed",
        "summary": "Review completed.",
        "findings": [],
        "commandsRun": [{
            "command": "git status --short",
            "status": "maybe"
        }],
        "filesInspected": [],
        "notes": [],
        "suggestedTrackerUpdates": []
    }))
    .expect_err("command result status must use the protocol vocabulary");
    assert!(error.to_string().contains("unknown variant `maybe`"));
}

#[test]
fn structured_review_result_rejects_root_and_nonreviewer_callers() {
    let (state, root_session_id, parent_session_id) = mailbox_test_state();
    let root_error = state
        .submit_delegation_review_result(&root_session_id, structured_review_request())
        .expect_err("root sessions must not submit child review results");
    assert_eq!(root_error.status, StatusCode::BAD_REQUEST);

    let (_delegation_id, child_session_id) =
        install_required_review_delegation(&state, &parent_session_id);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_delegation_index_by_child_session_id(&child_session_id)
            .expect("delegation should exist");
        inner.delegations[index].mode = DelegationMode::Explorer;
        state.commit_locked(&mut inner).unwrap();
    }
    let explorer_error = state
        .submit_delegation_review_result(&child_session_id, structured_review_request())
        .expect_err("explorer children must not submit review results");
    assert_eq!(explorer_error.status, StatusCode::BAD_REQUEST);
}
