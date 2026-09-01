// Claude Code CLI recorder and turn-state tests.
//
// The anthropic/claude-code CLI emits an NDJSON stream on stdout that TermAl
// parses via `handle_claude_stdout_message` in `src/runtime.rs`. Each line is
// an `assistant`, `user`, `stream_event`, or `result` envelope.
// `ClaudeTurnState` accumulates per-turn bookkeeping — pending tool uses
// keyed by `tool_use_id`, parallel sub-agents spawned via the `task` tool,
// the streamed assistant text buffer, and approval keys already seen — and
// is finalized by a `result` event or torn down when the runtime exits.
//
// Streamed text reconciliation is the trickiest seam: Claude emits a stream
// of `text_delta` chunks and then a final full-text payload inside an
// `assistant` frame after `message_stop`. `handle_claude_streamed_text` must
// append the missing suffix when the final is longer, skip the duplicate
// when the final matches, and REPLACE the bubble when the final diverges.
// Parallel agents (the `task` tool) spawn sub-recorders that fan progress
// into the parent transcript; their tool-use / tool-result / tool-error
// frames are folded into `ParallelAgentProgress` entries and recorded as
// subagent results. Transcript boundary: a `tool_use` arriving after
// streamed text ends must start a follow-up `Message`, not append to the
// closed text bubble. Production surfaces under test live in
// `src/runtime.rs`: `handle_claude_stdout_message`, `handle_claude_tool_use`,
// `handle_claude_tool_result`, the `handle_claude_task_tool_*` family,
// `handle_claude_streamed_text`, and `handle_claude_result`.

use super::*;

fn claude_permission_request(tool_name: &str, tool_input: Value) -> Value {
    json!({
        "type": "control_request",
        "request_id": "permission-request-1",
        "request": {
            "subtype": "can_use_tool",
            "tool_name": tool_name,
            "input": tool_input
        }
    })
}

fn claude_user_dialog_request() -> Value {
    let questions = json!([
        {
            "header": "Scope",
            "question": "Which scope should I use?",
            "multiSelect": false,
            "options": [
                {"label": "Focused", "description": "Only the changed module"},
                {"label": "Broad", "description": "The whole workspace"}
            ]
        },
        {
            "header": "Checks",
            "question": "Which checks should I run?",
            "multiSelect": true,
            "options": [
                {"label": "Tests", "description": "Run tests"},
                {"label": "Lint", "description": "Run lint"}
            ]
        }
    ]);
    json!({
        "type": "control_request",
        "request_id": "dialog-request-1",
        "request": {
            "subtype": "request_user_dialog",
            "dialog_kind": "permission_ask_user_question",
            "tool_use_id": "tool-use-1",
            "payload": {
                "requestId": "tool-use-1",
                "toolName": "AskUserQuestion",
                "permissionResult": {"behavior": "ask"},
                "questions": questions,
                "input": {"questions": questions}
            }
        }
    })
}

#[test]
fn claude_ask_user_question_dialog_is_classified_with_every_question() {
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &claude_user_dialog_request(),
        &mut turn_state,
        ClaudeApprovalMode::AutoApprove,
        false,
        "/tmp",
        false,
    )
    .expect("dialog should parse")
    .expect("dialog should be classified");

    let ClaudeControlRequestAction::QueueUserInput {
        title,
        detail,
        questions,
        request,
    } = action
    else {
        panic!("AskUserQuestion must wait for user input in every approval mode");
    };
    assert_eq!(title, "Claude needs your input");
    assert!(detail.contains("2 questions"));
    assert_eq!(request.request_id, "dialog-request-1");
    assert_eq!(questions.len(), 2);
    assert_eq!(questions[0].id, "claude-question-1");
    assert!(questions[0].is_other);
    assert!(!questions[0].multi_select);
    assert_eq!(questions[1].id, "claude-question-2");
    assert!(questions[1].multi_select);
    assert_eq!(request.questions, questions);
    assert_eq!(request.transport, ClaudeUserInputTransport::Dialog);
}

fn claude_ask_user_question_permission_request() -> Value {
    claude_permission_request(
        "AskUserQuestion",
        json!({
            "questions": [
                {
                    "header": "Scope",
                    "question": "Which scope should I use?",
                    "multiSelect": false,
                    "options": [
                        {"label": "Focused", "description": "Only the changed module"},
                        {"label": "Broad", "description": "The whole workspace"}
                    ]
                }
            ]
        }),
    )
}

#[test]
fn claude_ask_user_question_permission_waits_for_user_input_in_every_mode() {
    // Current Claude CLIs deliver AskUserQuestion as a can_use_tool
    // permission request and read the answers from the allow decision's
    // updatedInput.answers. Every attended approval mode — including Plan,
    // which denies ordinary tools — must queue the question card instead of
    // resolving the permission on its own; an instant allow makes the tool
    // return "The user did not answer the questions". Read-only reviewer
    // delegations are covered separately below.
    for approval_mode in [
        ClaudeApprovalMode::Ask,
        ClaudeApprovalMode::AutoApprove,
        ClaudeApprovalMode::Plan,
    ] {
        for delegation_control_plane_access in [false, true] {
            let mut turn_state = ClaudeTurnState::default();
            let action = classify_claude_control_request(
                &claude_ask_user_question_permission_request(),
                &mut turn_state,
                approval_mode,
                false,
                "/tmp",
                delegation_control_plane_access,
            )
            .expect("permission payload should parse")
            .expect("permission payload should be classified");

            let ClaudeControlRequestAction::QueueUserInput {
                title,
                detail,
                questions,
                request,
            } = action
            else {
                panic!(
                    "AskUserQuestion must wait for user input in mode {approval_mode:?} (delegation={delegation_control_plane_access})"
                );
            };
            assert_eq!(title, "Claude needs your input");
            assert_eq!(detail, "Answer Claude's question to continue.");
            assert_eq!(questions.len(), 1);
            assert_eq!(questions[0].question, "Which scope should I use?");
            assert!(questions[0].is_other);
            assert_eq!(request.request_id, "permission-request-1");
            assert_eq!(request.transport, ClaudeUserInputTransport::Permission);
            assert_eq!(
                request.input.pointer("/questions/0/question"),
                Some(&json!("Which scope should I use?"))
            );

            // The same request replayed within one turn is suppressed.
            let replay = classify_claude_control_request(
                &claude_ask_user_question_permission_request(),
                &mut turn_state,
                approval_mode,
                false,
                "/tmp",
                delegation_control_plane_access,
            )
            .expect("replay should parse");
            assert!(replay.is_none());
        }
    }
}

#[test]
fn claude_ask_user_question_keeps_immediate_denial_for_read_only_reviewers() {
    // Read-only reviewer delegations run unattended under a fan-in: parking
    // a question card would stall the review, so they keep a fail-closed
    // immediate denial whose message tells Claude to decide without asking.
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &claude_ask_user_question_permission_request(),
        &mut turn_state,
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        false,
        "/tmp",
        true,
    )
    .expect("permission payload should parse")
    .expect("permission payload should be classified");

    // The runtime still receives the immediate deny; the transcript
    // additionally gets a resolved audit card carrying the questions.
    let ClaudeControlRequestAction::RecordSelfResolvedQuestion {
        questions,
        response:
            ClaudeSelfResolvedQuestionResponse::PermissionDeny(ClaudePermissionDecision::Deny {
                request_id,
                message,
            }),
        ..
    } = action
    else {
        panic!("read-only reviewers must keep the immediate AskUserQuestion denial");
    };
    assert_eq!(questions.len(), 1);
    assert_eq!(request_id, "permission-request-1");
    assert!(message.contains("unattended"));
    assert!(message.contains("your own judgment"));
}

#[test]
fn claude_ask_user_question_attendedness_matrix_on_permission_transport() {
    // Attended (question card queued): a root AutoApprove session has a
    // person watching it, and an Ask-mode implementer child surfaces its
    // approvals and questions to a human on purpose. Unattended
    // (self-resolve): an AutoApprove delegation child runs headless under
    // the fan-in by the parent's choice, and a read-only reviewer always.
    for (approval_mode, delegation_child) in [
        (ClaudeApprovalMode::AutoApprove, false),
        (ClaudeApprovalMode::Plan, false),
        (ClaudeApprovalMode::Ask, true),
    ] {
        let mut turn_state = ClaudeTurnState::default();
        let action = classify_claude_control_request(
            &claude_ask_user_question_permission_request(),
            &mut turn_state,
            approval_mode,
            delegation_child,
            "/tmp",
            false,
        )
        .expect("permission payload should parse")
        .expect("permission payload should be classified");
        assert!(
            matches!(action, ClaudeControlRequestAction::QueueUserInput { .. }),
            "mode {approval_mode:?} child={delegation_child} must queue the question card"
        );
    }

    for (approval_mode, delegation_child) in [
        (ClaudeApprovalMode::AutoApprove, true),
        (ClaudeApprovalMode::Plan, true),
        (ClaudeApprovalMode::ReadOnlyAutoApprove, false),
    ] {
        let mut turn_state = ClaudeTurnState::default();
        let action = classify_claude_control_request(
            &claude_ask_user_question_permission_request(),
            &mut turn_state,
            approval_mode,
            delegation_child,
            "/tmp",
            false,
        )
        .expect("permission payload should parse")
        .expect("permission payload should be classified");
        // Self-resolution records an audit card with the parsed questions
        // and answers the runtime with the very deny a plain refusal sends.
        let ClaudeControlRequestAction::RecordSelfResolvedQuestion {
            title,
            detail,
            questions,
            response:
                ClaudeSelfResolvedQuestionResponse::PermissionDeny(ClaudePermissionDecision::Deny {
                    request_id,
                    message,
                }),
        } = action
        else {
            panic!("mode {approval_mode:?} child={delegation_child} must self-resolve");
        };
        assert_eq!(title, "Claude asked a question");
        assert!(detail.contains("without human input"));
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].question, "Which scope should I use?");
        assert_eq!(request_id, "permission-request-1");
        assert!(message.contains("unattended"));
        assert!(message.contains("your own judgment"));

        let replay = classify_claude_control_request(
            &claude_ask_user_question_permission_request(),
            &mut turn_state,
            approval_mode,
            delegation_child,
            "/tmp",
            false,
        )
        .expect("replay should parse");
        assert!(
            replay.is_none(),
            "the denial must deduplicate within a turn"
        );
    }
}

#[test]
fn unattended_ask_user_question_with_malformed_payload_records_an_error_and_denies() {
    // No parsed questions means no question card can be built. Preserve the
    // parser diagnostic as a transcript error before denying so the automatic
    // decision remains auditable without parking the agent.
    let message = claude_permission_request("AskUserQuestion", json!({ "metadata": {} }));
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &message,
        &mut turn_state,
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        false,
        "/tmp",
        false,
    )
    .expect("permission payload should parse")
    .expect("permission payload should be classified");
    let ClaudeControlRequestAction::RecordSelfResolvedQuestionError {
        detail,
        response:
            ClaudeSelfResolvedQuestionResponse::PermissionDeny(ClaudePermissionDecision::Deny {
                request_id,
                message,
            }),
    } = action
    else {
        panic!("a malformed unattended question must be diagnosed and denied");
    };
    assert!(detail.contains("has no `questions` array"));
    assert_eq!(request_id, "permission-request-1");
    assert!(message.contains("your own judgment"));
}

#[test]
fn unattended_ask_user_question_retries_are_bounded_per_turn() {
    let mut turn_state = ClaudeTurnState::default();
    for attempt in 1..=MAX_CLAUDE_UNATTENDED_QUESTIONS_PER_TURN {
        let mut request = claude_ask_user_question_permission_request();
        request["request_id"] = json!(format!("permission-request-{attempt}"));
        let action = classify_claude_control_request(
            &request,
            &mut turn_state,
            ClaudeApprovalMode::ReadOnlyAutoApprove,
            false,
            "/tmp",
            false,
        )
        .expect("an unattended retry within the cap should classify")
        .expect("an unattended retry within the cap should self-resolve");
        assert!(matches!(
            action,
            ClaudeControlRequestAction::RecordSelfResolvedQuestion { .. }
        ));
    }

    let mut request = claude_ask_user_question_permission_request();
    request["request_id"] = json!("permission-request-over-limit");
    let err = match classify_claude_control_request(
        &request,
        &mut turn_state,
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        false,
        "/tmp",
        false,
    ) {
        Err(err) => err,
        Ok(_) => panic!("the fourth unattended question must fail the turn"),
    };
    assert!(err.to_string().contains("more than 3 times"));
}

#[test]
fn claude_legacy_dialog_attendedness_matrix() {
    // The no-park invariant applies the same attendedness policy to the
    // legacy request_user_dialog channel: root AutoApprove and Ask-mode
    // implementer children queue the card; AutoApprove delegation children
    // and read-only reviewers get an immediate control error carrying the
    // self-decide wording (the dialog protocol has no deny decision),
    // deduplicated within the turn like the queued path.
    for (approval_mode, delegation_child) in [
        (ClaudeApprovalMode::AutoApprove, false),
        (ClaudeApprovalMode::Plan, false),
        (ClaudeApprovalMode::Ask, true),
    ] {
        let mut turn_state = ClaudeTurnState::default();
        let action = classify_claude_control_request(
            &claude_user_dialog_request(),
            &mut turn_state,
            approval_mode,
            delegation_child,
            "/tmp",
            false,
        )
        .expect("legacy dialog should parse")
        .expect("legacy dialog should be classified");
        assert!(
            matches!(action, ClaudeControlRequestAction::QueueUserInput { .. }),
            "mode {approval_mode:?} child={delegation_child} must queue the legacy dialog card"
        );
    }

    for approval_mode in [ClaudeApprovalMode::AutoApprove, ClaudeApprovalMode::Plan] {
        let mut turn_state = ClaudeTurnState::default();
        let child_action = classify_claude_control_request(
            &claude_user_dialog_request(),
            &mut turn_state,
            approval_mode,
            true,
            "/tmp",
            false,
        )
        .expect("legacy dialog should parse")
        .expect("legacy dialog should be classified");
        assert!(
            matches!(
                child_action,
                ClaudeControlRequestAction::RecordSelfResolvedQuestion {
                    response: ClaudeSelfResolvedQuestionResponse::DialogError(_),
                    ..
                }
            ),
            "a {approval_mode:?} delegation child must self-resolve the legacy dialog"
        );
    }

    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &claude_user_dialog_request(),
        &mut turn_state,
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        false,
        "/tmp",
        false,
    )
    .expect("legacy dialog should parse")
    .expect("legacy dialog should be classified");
    // The audit card carries the parsed questions; the control error the
    // runtime receives is unchanged from a plain refusal.
    let ClaudeControlRequestAction::RecordSelfResolvedQuestion {
        title,
        detail,
        questions,
        response: ClaudeSelfResolvedQuestionResponse::DialogError(response),
    } = action
    else {
        panic!("a read-only reviewer's legacy dialog must respond immediately");
    };
    assert_eq!(title, "Claude asked a question");
    assert!(detail.contains("without human input"));
    assert!(detail.contains("control error"));
    assert_eq!(questions.len(), 2);
    assert_eq!(response.request_id, "dialog-request-1");
    assert!(response.error.contains("unattended"));
    assert!(response.error.contains("your own judgment"));

    let replay = classify_claude_control_request(
        &claude_user_dialog_request(),
        &mut turn_state,
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        false,
        "/tmp",
        false,
    )
    .expect("replay should parse");
    assert!(
        replay.is_none(),
        "the denial must deduplicate within a turn"
    );
}

#[test]
fn namespaced_mcp_ask_user_question_takes_the_ordinary_permission_flow() {
    // The CLI namespaces MCP tools as mcp__<server>__<tool>; a leaf that
    // names itself AskUserQuestion must not reach the question card.
    let message = claude_permission_request(
        "mcp__helper__AskUserQuestion",
        json!({
            "questions": [
                {"header": "Scope", "question": "Which scope should I use?"}
            ]
        }),
    );
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &message,
        &mut turn_state,
        ClaudeApprovalMode::Ask,
        false,
        "/tmp",
        false,
    )
    .expect("permission payload should parse")
    .expect("permission payload should be classified");
    assert!(
        matches!(action, ClaudeControlRequestAction::QueueApproval { .. }),
        "a namespaced MCP AskUserQuestion must take the ordinary permission flow"
    );
}

#[test]
fn question_dedupe_keys_are_namespaced_per_transport() {
    // A legacy dialog and a permission request that share a request id
    // within one turn must not suppress each other: the dedupe namespaces
    // are per transport.
    let mut turn_state = ClaudeTurnState::default();
    let dialog_action = classify_claude_control_request(
        &claude_user_dialog_request(),
        &mut turn_state,
        ClaudeApprovalMode::Ask,
        false,
        "/tmp",
        false,
    )
    .expect("legacy dialog should parse");
    assert!(
        dialog_action.is_some(),
        "the legacy dialog must be classified"
    );

    let mut permission_message = claude_ask_user_question_permission_request();
    permission_message["request_id"] = json!("dialog-request-1");
    let permission_action = classify_claude_control_request(
        &permission_message,
        &mut turn_state,
        ClaudeApprovalMode::Ask,
        false,
        "/tmp",
        false,
    )
    .expect("permission payload should parse");
    assert!(
        matches!(
            permission_action,
            Some(ClaudeControlRequestAction::QueueUserInput { ref request, .. })
                if request.request_id == "dialog-request-1"
        ),
        "a colliding request id on the other transport must still be classified"
    );
}

#[test]
fn recorder_records_self_resolved_question_as_declined_without_a_claim() {
    // The audit card is a resolved, non-declinable Declined card: nothing
    // pending, no live claim, no Approval status — just the transcript
    // entry saying TermAl asked Claude to decide.
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut delta_rx = state.subscribe_delta_events();
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    recorder
        .push_claude_self_resolved_user_input(
            "Claude asked a question",
            "TermAl asked Claude to decide without human input.",
            vec![UserInputQuestion {
                header: "Scope".to_owned(),
                id: "claude-question-1".to_owned(),
                is_other: true,
                is_secret: false,
                multi_select: false,
                options: None,
                question: "Which scope should I use?".to_owned(),
            }],
        )
        .expect("self-resolved question should record");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert!(record.pending_claude_user_inputs.is_empty());
    assert!(!has_pending_requests(record));
    assert_ne!(record.session.status, SessionStatus::Approval);
    assert!(matches!(
        record.session.messages.last(),
        Some(Message::UserInputRequest {
            state: InteractionRequestState::Declined,
            declinable: false,
            submitted_answers: None,
            questions,
            detail,
            ..
        }) if questions.len() == 1 && detail.contains("without human input")
    ));
    drop(inner);
    assert!(
        delta_rx.try_recv().is_ok(),
        "the audit card must publish a transcript delta"
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn recorder_marks_user_input_declinable_by_pending_transport() {
    // Wiring pin: the recorded card's declinable flag must come from the
    // pending request's transport (permission = skippable, legacy dialog =
    // not), without hand-constructing the message.
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    for (transport, expected_declinable) in [
        (ClaudeUserInputTransport::Permission, true),
        (ClaudeUserInputTransport::Dialog, false),
    ] {
        let questions = vec![UserInputQuestion {
            header: "Scope".to_owned(),
            id: "claude-question-1".to_owned(),
            is_other: true,
            is_secret: false,
            multi_select: false,
            options: None,
            question: "Which scope should I use?".to_owned(),
        }];
        let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
        recorder
            .push_claude_user_input_request(
                "Claude needs your input",
                "Answer Claude's question to continue.",
                questions.clone(),
                ClaudePendingUserInput {
                    input: json!({ "questions": [{ "question": "Which scope should I use?" }] }),
                    questions,
                    request_id: format!("recorder-declinable-{expected_declinable}"),
                    transport,
                },
            )
            .expect("user input request should record");

        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == session_id)
            .expect("Claude session should exist");
        assert!(
            matches!(
                record.session.messages.last(),
                Some(Message::UserInputRequest { declinable, .. })
                    if *declinable == expected_declinable
            ),
            "transport {transport:?} must record declinable={expected_declinable}"
        );
    }
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn claude_ask_user_question_preserves_exact_question_and_option_text() {
    // The CLI keys the returned updatedInput.answers by the exact question
    // string and matches answer values against its exact option labels;
    // trimming either would orphan the answer on the CLI side.
    let message = claude_permission_request(
        "AskUserQuestion",
        json!({
            "questions": [
                {
                    "header": "Scope",
                    "question": "  Which scope should I use? \t",
                    "options": [
                        {"label": "  Focused \t", "description": "Only the changed module"}
                    ]
                }
            ]
        }),
    );
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &message,
        &mut turn_state,
        ClaudeApprovalMode::Ask,
        false,
        "/tmp",
        false,
    )
    .expect("permission payload should parse")
    .expect("permission payload should be classified");

    let ClaudeControlRequestAction::QueueUserInput {
        questions, request, ..
    } = action
    else {
        panic!("a well-formed question must queue the question card");
    };
    assert_eq!(questions[0].question, "  Which scope should I use? \t");
    assert_eq!(
        questions[0]
            .options
            .as_ref()
            .and_then(|options| options.first())
            .map(|option| option.label.as_str()),
        Some("  Focused \t")
    );

    let (updated_input, display_answers) = validate_claude_user_input_answers(
        &request,
        BTreeMap::from([(
            "claude-question-1".to_owned(),
            vec!["  Focused \t".to_owned()],
        )]),
    )
    .expect("the exact option label should remain a valid answer");
    assert_eq!(
        updated_input["answers"]["  Which scope should I use? \t"],
        "  Focused \t"
    );
    assert_eq!(
        display_answers["claude-question-1"],
        vec!["  Focused \t".to_owned()]
    );
}

#[test]
fn legacy_claude_question_dialog_retains_historical_text_trimming() {
    // Exact strings are required by the live permission channel, but the
    // compatibility-only dialog channel historically normalized both keys
    // and labels. Keep that unverified legacy contract stable.
    let questions = json!([
        {
            "header": "Scope",
            "question": "  Which scope should I use? \t",
            "options": [
                {"label": "  Focused \t", "description": "Only the changed module"}
            ]
        }
    ]);
    let mut message = claude_user_dialog_request();
    message["request"]["payload"]["questions"] = questions.clone();
    message["request"]["payload"]["input"]["questions"] = questions;

    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &message,
        &mut turn_state,
        ClaudeApprovalMode::Ask,
        false,
        "/tmp",
        false,
    )
    .expect("legacy dialog should parse")
    .expect("legacy dialog should be classified");
    let ClaudeControlRequestAction::QueueUserInput {
        questions, request, ..
    } = action
    else {
        panic!("the legacy dialog must queue structured input");
    };
    assert_eq!(questions[0].question, "Which scope should I use?");
    assert_eq!(
        questions[0]
            .options
            .as_ref()
            .and_then(|options| options.first())
            .map(|option| option.label.as_str()),
        Some("Focused")
    );

    let (updated_input, _) = validate_claude_user_input_answers(
        &request,
        BTreeMap::from([("claude-question-1".to_owned(), vec!["Focused".to_owned()])]),
    )
    .expect("the historically normalized dialog answer should validate");
    assert_eq!(
        updated_input["answers"]["Which scope should I use?"],
        "Focused"
    );
}

#[test]
fn claude_ask_user_question_rejects_present_invalid_optional_field_types() {
    for (field, invalid_value, expected_diagnostic) in [
        (
            "options",
            json!({"Focused": "Only this module"}),
            "is not an array",
        ),
        ("multiSelect", json!("yes"), "is not a boolean"),
    ] {
        let mut question = json!({
            "header": "Scope",
            "question": "Which scope should I use?"
        });
        question[field] = invalid_value;
        let message =
            claude_permission_request("AskUserQuestion", json!({"questions": [question]}));
        let mut turn_state = ClaudeTurnState::default();
        let action = classify_claude_control_request(
            &message,
            &mut turn_state,
            ClaudeApprovalMode::Ask,
            false,
            "/tmp",
            false,
        )
        .expect("permission payload should classify")
        .expect("malformed optional fields should use normal permission handling");
        let ClaudeControlRequestAction::QueueApproval { detail, .. } = action else {
            panic!("a present invalid {field} must fall back to permission handling");
        };
        assert!(detail.contains("AskUserQuestion payload rejected"));
        assert!(
            detail.contains(expected_diagnostic),
            "diagnostic for {field} was missing: {detail}"
        );
    }
}

#[test]
fn claude_ask_user_question_rejects_oversized_or_duplicate_option_lists() {
    let cases = [
        (
            (1..=5)
                .map(|index| json!({"label": format!("Option {index}")}))
                .collect::<Vec<_>>(),
            "expected at most 4",
        ),
        (
            vec![json!({"label": "Focused"}), json!({"label": "Focused"})],
            "duplicate option label",
        ),
    ];

    for (options, expected_diagnostic) in cases {
        let message = claude_permission_request(
            "AskUserQuestion",
            json!({
                "questions": [{
                    "header": "Scope",
                    "question": "Which scope should I use?",
                    "options": options
                }]
            }),
        );
        let mut turn_state = ClaudeTurnState::default();
        let action = classify_claude_control_request(
            &message,
            &mut turn_state,
            ClaudeApprovalMode::Ask,
            false,
            "/tmp",
            false,
        )
        .expect("permission payload should classify")
        .expect("invalid option lists should use normal permission handling");
        let ClaudeControlRequestAction::QueueApproval { detail, .. } = action else {
            panic!("an invalid option list must fall back to permission handling");
        };
        assert!(detail.contains("AskUserQuestion payload rejected"));
        assert!(
            detail.contains(expected_diagnostic),
            "expected `{expected_diagnostic}` in `{detail}`"
        );
    }
}

#[test]
fn claude_ask_user_question_with_malformed_questions_falls_back_to_permission_flow() {
    // Five questions exceed the tool contract; the payload is treated as an
    // ordinary permission request instead of failing the turn.
    let questions: Vec<Value> = (1..=5)
        .map(|index| {
            json!({
                "header": format!("Q{index}"),
                "question": format!("Question number {index}?"),
                "multiSelect": false,
                "options": []
            })
        })
        .collect();
    let message = claude_permission_request("AskUserQuestion", json!({ "questions": questions }));

    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &message,
        &mut turn_state,
        ClaudeApprovalMode::Ask,
        false,
        "/tmp",
        false,
    )
    .expect("permission payload should parse")
    .expect("permission payload should be classified");
    let ClaudeControlRequestAction::QueueApproval { detail, .. } = action else {
        panic!("a malformed question list must fall back to the approval flow");
    };
    assert!(
        detail.contains("AskUserQuestion payload rejected"),
        "the approval card must surface why no question card was shown: {detail}"
    );
    assert!(detail.contains("expected 1 to 4"));
    // The fallback wording must stay mode-neutral: depending on the approval
    // mode the ordinary flow shows a card, auto-allows, or denies.
    assert!(detail.contains("normal permission handling"));
    assert!(!detail.contains("asking for approval"));
}

#[test]
fn claude_ask_user_question_without_questions_array_is_diagnosed_in_fallback() {
    // A payload with no questions array (missing or mistyped) is diagnosed
    // like any other parse failure — the approval card explains why the
    // question card did not appear instead of silently degrading.
    for tool_input in [
        json!({ "metadata": {"source": "test"} }),
        json!({ "questions": "not-an-array" }),
    ] {
        let message = claude_permission_request("AskUserQuestion", tool_input);
        let mut turn_state = ClaudeTurnState::default();
        let action = classify_claude_control_request(
            &message,
            &mut turn_state,
            ClaudeApprovalMode::Ask,
            false,
            "/tmp",
            false,
        )
        .expect("permission payload should parse")
        .expect("permission payload should be classified");
        let ClaudeControlRequestAction::QueueApproval { detail, .. } = action else {
            panic!("a missing question list must fall back to the approval flow");
        };
        assert!(
            detail.contains("has no `questions` array"),
            "the approval card must diagnose the missing question list: {detail}"
        );
    }
}

#[test]
fn claude_user_input_response_routes_by_transport() {
    let response = ClaudeUserInputResponse {
        request_id: "permission-request-1".to_owned(),
        updated_input: json!({
            "questions": [],
            "answers": {"Which scope should I use?": "Focused"}
        }),
    };

    let command =
        claude_user_input_runtime_command(ClaudeUserInputTransport::Permission, response.clone());
    let ClaudeRuntimeCommand::PermissionResponse(ClaudePermissionDecision::Allow {
        request_id,
        updated_input,
    }) = command
    else {
        panic!("permission-transport answers must return through the allow decision");
    };
    assert_eq!(request_id, "permission-request-1");
    assert_eq!(
        updated_input.pointer("/answers/Which scope should I use?"),
        Some(&json!("Focused"))
    );

    let command = claude_user_input_runtime_command(ClaudeUserInputTransport::Dialog, response);
    assert!(matches!(
        command,
        ClaudeRuntimeCommand::UserInputResponse(_)
    ));
}

#[test]
fn claude_self_resolved_question_response_routes_by_transport() {
    let command = claude_self_resolved_question_runtime_command(
        ClaudeSelfResolvedQuestionResponse::PermissionDeny(ClaudePermissionDecision::Deny {
            request_id: "permission-request-1".to_owned(),
            message: "Decide without asking.".to_owned(),
        }),
    );
    let ClaudeRuntimeCommand::PermissionResponse(ClaudePermissionDecision::Deny {
        request_id,
        message,
    }) = command
    else {
        panic!("permission self-resolution must use the permission deny envelope");
    };
    assert_eq!(request_id, "permission-request-1");
    assert_eq!(message, "Decide without asking.");

    let command = claude_self_resolved_question_runtime_command(
        ClaudeSelfResolvedQuestionResponse::DialogError(ClaudeControlErrorResponse {
            error: "Decide without asking.".to_owned(),
            request_id: "dialog-request-1".to_owned(),
        }),
    );
    let ClaudeRuntimeCommand::ControlErrorResponse(ClaudeControlErrorResponse {
        error,
        request_id,
    }) = command
    else {
        panic!("legacy dialog self-resolution must use the control error envelope");
    };
    assert_eq!(request_id, "dialog-request-1");
    assert_eq!(error, "Decide without asking.");
}

#[test]
fn claude_initialize_declares_question_dialog_and_response_uses_completed_envelope() {
    let mut initialize = Vec::new();
    write_claude_initialize(&mut initialize).expect("initialize should serialize");
    let initialize: Value =
        serde_json::from_slice(initialize.trim_ascii_end()).expect("initialize should be JSON");
    assert_eq!(
        initialize.pointer("/request/supportedDialogKinds"),
        Some(&json!(["permission_ask_user_question"]))
    );

    let mut response = Vec::new();
    write_claude_user_input_response(
        &mut response,
        &ClaudeUserInputResponse {
            request_id: "dialog-request-1".to_owned(),
            updated_input: json!({
                "questions": [],
                "answers": {"Which scope should I use?": "Focused"}
            }),
        },
    )
    .expect("dialog response should serialize");
    let response: Value =
        serde_json::from_slice(response.trim_ascii_end()).expect("response should be JSON");
    assert_eq!(
        response,
        json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": "dialog-request-1",
                "response": {
                    "behavior": "completed",
                    "result": {
                        "behavior": "allow",
                        "updatedInput": {
                            "questions": [],
                            "answers": {"Which scope should I use?": "Focused"}
                        }
                    }
                }
            }
        })
    );
}

#[test]
fn malformed_claude_question_dialog_is_answered_with_a_control_error() {
    let mut request = claude_user_dialog_request();
    request["request"]["payload"]["questions"] = json!([{}, {}, {}, {}, {}]);
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &request,
        &mut turn_state,
        ClaudeApprovalMode::Ask,
        false,
        "/tmp",
        false,
    )
    .expect("malformed identified dialog should produce a response")
    .expect("malformed identified dialog should not be ignored");
    let ClaudeControlRequestAction::RespondError(response) = action else {
        panic!("malformed dialog should produce a protocol error response");
    };
    assert_eq!(response.request_id, "dialog-request-1");
    assert!(response.error.contains("expected 1 to 4"));

    let mut wire = Vec::new();
    write_claude_control_error_response(&mut wire, &response)
        .expect("control error should serialize");
    let wire: Value =
        serde_json::from_slice(wire.trim_ascii_end()).expect("response should be JSON");
    assert_eq!(
        wire,
        json!({
            "type": "control_response",
            "response": {
                "subtype": "error",
                "request_id": "dialog-request-1",
                "error": response.error,
            }
        })
    );
}

#[test]
fn malformed_unattended_claude_question_dialogs_are_deduped_and_bounded() {
    let mut turn_state = ClaudeTurnState::default();
    for attempt in 1..=MAX_CLAUDE_UNATTENDED_QUESTIONS_PER_TURN {
        let mut request = claude_user_dialog_request();
        request["request_id"] = json!(format!("dialog-request-{attempt}"));
        request["request"]["payload"]["questions"] = json!([{}, {}, {}, {}, {}]);
        let action = classify_claude_control_request(
            &request,
            &mut turn_state,
            ClaudeApprovalMode::ReadOnlyAutoApprove,
            false,
            "/tmp",
            false,
        )
        .expect("a malformed unattended dialog within the cap should classify")
        .expect("a malformed unattended dialog within the cap should self-resolve");
        let ClaudeControlRequestAction::RecordSelfResolvedQuestionError {
            detail,
            response:
                ClaudeSelfResolvedQuestionResponse::DialogError(ClaudeControlErrorResponse {
                    request_id,
                    error,
                }),
        } = action
        else {
            panic!("malformed unattended dialog should record an error and self-resolve");
        };
        assert!(detail.contains("expected 1 to 4"));
        assert_eq!(request_id, format!("dialog-request-{attempt}"));
        assert!(error.contains("your own judgment"));

        if attempt == 1 {
            let replay = classify_claude_control_request(
                &request,
                &mut turn_state,
                ClaudeApprovalMode::ReadOnlyAutoApprove,
                false,
                "/tmp",
                false,
            )
            .expect("a malformed dialog replay should classify");
            assert!(
                replay.is_none(),
                "a malformed dialog replay must deduplicate"
            );
        }
    }

    let mut request = claude_user_dialog_request();
    request["request_id"] = json!("dialog-request-over-limit");
    request["request"]["payload"]["questions"] = json!([{}, {}, {}, {}, {}]);
    let err = match classify_claude_control_request(
        &request,
        &mut turn_state,
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        false,
        "/tmp",
        false,
    ) {
        Err(err) => err,
        Ok(_) => panic!("the fourth malformed unattended dialog must fail the turn"),
    };
    assert!(err.to_string().contains("more than 3 times"));
}

#[test]
fn claude_cancel_request_clears_pending_permission_question_and_updates_its_card() {
    // control_cancel_request is keyed by request id, so a question that
    // arrived over the can_use_tool permission transport is cleared exactly
    // like a legacy dialog: pending claim dropped, card Canceled, delta
    // published.
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let message_id = "claude-canceled-permission-question".to_owned();
    let questions = vec![UserInputQuestion {
        header: "Scope".to_owned(),
        id: "claude-question-1".to_owned(),
        is_other: true,
        is_secret: false,
        multi_select: false,
        options: None,
        question: "Which scope should I use?".to_owned(),
    }];
    state
        .push_message(
            &session_id,
            Message::UserInputRequest {
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Claude needs your input".to_owned(),
                detail: "Answer Claude's question to continue.".to_owned(),
                questions: questions.clone(),
                state: InteractionRequestState::Pending,
                declinable: true,
                submitted_answers: None,
            },
        )
        .expect("user input request should be recorded");
    state
        .register_claude_pending_user_input(
            &session_id,
            message_id.clone(),
            ClaudePendingUserInput {
                transport: ClaudeUserInputTransport::Permission,
                input: json!({ "questions": [{ "question": "Which scope should I use?" }] }),
                questions,
                request_id: "permission-request-canceled-by-claude".to_owned(),
            },
        )
        .expect("pending Claude user input should be registered");
    let mut delta_rx = state.subscribe_delta_events();

    state
        .clear_claude_pending_interaction_by_request(
            &session_id,
            "permission-request-canceled-by-claude",
        )
        .expect("Claude cancellation should clear the permission question");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert!(record.pending_claude_user_inputs.is_empty());
    assert!(!has_pending_requests(record));
    assert!(matches!(
        record.session.messages.last(),
        Some(Message::UserInputRequest {
            state: InteractionRequestState::Canceled,
            submitted_answers: None,
            ..
        })
    ));
    drop(inner);

    let delta: DeltaEvent = serde_json::from_str(
        &delta_rx
            .try_recv()
            .expect("canceled permission question should publish its message update"),
    )
    .expect("canceled permission question delta should decode");
    assert!(matches!(delta, DeltaEvent::MessageUpdated { .. }));
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn claude_cancel_request_clears_pending_user_dialog_and_updates_its_card() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let message_id = "claude-canceled-dialog".to_owned();
    let questions = vec![UserInputQuestion {
        header: "Scope".to_owned(),
        id: "claude-question-1".to_owned(),
        is_other: true,
        is_secret: false,
        multi_select: false,
        options: None,
        question: "Which scope should I use?".to_owned(),
    }];
    state
        .push_message(
            &session_id,
            Message::UserInputRequest {
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Claude needs your input".to_owned(),
                detail: "Answer Claude's question to continue.".to_owned(),
                questions: questions.clone(),
                state: InteractionRequestState::Pending,
                declinable: false,
                submitted_answers: None,
            },
        )
        .expect("user input request should be recorded");
    state
        .register_claude_pending_user_input(
            &session_id,
            message_id.clone(),
            ClaudePendingUserInput {
                transport: ClaudeUserInputTransport::Dialog,
                input: json!({"questions": []}),
                questions,
                request_id: "dialog-request-canceled-by-claude".to_owned(),
            },
        )
        .expect("pending Claude user input should be registered");
    let mut delta_rx = state.subscribe_delta_events();

    state
        .clear_claude_pending_interaction_by_request(
            &session_id,
            "dialog-request-canceled-by-claude",
        )
        .expect("Claude cancellation should clear the dialog");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert!(record.pending_claude_user_inputs.is_empty());
    assert!(!has_pending_requests(record));
    assert!(matches!(
        record.session.messages.last(),
        Some(Message::UserInputRequest {
            state: InteractionRequestState::Canceled,
            submitted_answers: None,
            ..
        })
    ));
    drop(inner);

    let delta: DeltaEvent = serde_json::from_str(
        &delta_rx
            .try_recv()
            .expect("canceled dialog should publish its message update"),
    )
    .expect("canceled dialog delta should decode");
    assert!(matches!(
        delta,
        DeltaEvent::MessageUpdated {
            message_id: ref updated_message_id,
            message: Message::UserInputRequest {
                state: InteractionRequestState::Canceled,
                ..
            },
            ..
        } if updated_message_id == &message_id
    ));
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn claude_transient_api_retry_prefers_numeric_status_over_result_prose() {
    let overloaded = json!({
        "type": "result",
        "subtype": "success",
        "is_error": true,
        "api_error_status": 529,
        "result": "API Error: 529 Overloaded."
    });
    assert_eq!(claude_transient_api_status(&overloaded), Some(529));

    let fatal_with_misleading_text = json!({
        "type": "result",
        "subtype": "success",
        "is_error": true,
        "api_error_status": 401,
        "result": "API Error: 529 should not override the structural status"
    });
    assert_eq!(
        claude_transient_api_status(&fatal_with_misleading_text),
        None
    );
}

#[test]
fn claude_transient_api_retry_accepts_success_subtype_when_is_error_is_true() {
    // Claude Code 2.1.220 reports API failures with the counterintuitive
    // subtype `success`; `is_error` and `api_error_status` are authoritative.
    let overloaded = json!({
        "type": "result",
        "subtype": "success",
        "is_error": true,
        "api_error_status": 529,
        "result": "API Error: 529 Overloaded."
    });

    assert_eq!(claude_transient_api_status(&overloaded), Some(529));
}

#[test]
fn claude_transient_api_retry_has_bounded_exact_legacy_fallback() {
    for status in [429, 503, 529] {
        let message = json!({
            "type": "result",
            "subtype": "success",
            "is_error": true,
            "result": format!("API Error: {status} temporary failure")
        });
        assert_eq!(claude_transient_api_status(&message), Some(status));
    }

    for message in [
        json!({
            "type": "result",
            "subtype": "success",
            "is_error": true,
            "result": "The API is Overloaded; try later"
        }),
        json!({
            "type": "result",
            "subtype": "success",
            "is_error": true,
            "result": "API Error: 401 Unauthorized"
        }),
        json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": "API Error: 529 Overloaded"
        }),
        json!({
            "type": "assistant",
            "is_error": true,
            "result": "API Error: 529 Overloaded"
        }),
    ] {
        assert_eq!(claude_transient_api_status(&message), None);
    }
}

#[test]
fn claude_transient_api_retry_delay_is_session_stable_and_exponential() {
    let session_id = "session-claude-retry";
    let first = claude_transient_api_retry_delay(session_id, 1, 529);
    let second = claude_transient_api_retry_delay(session_id, 2, 529);

    assert_eq!(
        first,
        claude_transient_api_retry_delay(session_id, 1, 529),
        "one session must get a deterministic retry schedule"
    );
    assert!(
        (Duration::from_millis(150)..=Duration::from_millis(250)).contains(&first),
        "first retry should be the 200ms base scaled by 75%-125% jitter"
    );
    assert!(
        (Duration::from_millis(300)..=Duration::from_millis(500)).contains(&second),
        "second retry should double the base before jitter"
    );

    let rate_limited = claude_transient_api_retry_delay(session_id, 1, 429);
    assert!(
        (Duration::from_millis(750)..=Duration::from_millis(1250)).contains(&rate_limited),
        "429 should use the longer one-second capacity-window base"
    );
}

#[test]
fn claude_transient_api_retry_exhausts_after_five_total_attempts() {
    let overloaded = json!({
        "type": "result",
        "subtype": "success",
        "is_error": true,
        "api_error_status": 529,
        "result": "API Error: 529 Overloaded."
    });

    for prior_completed_attempts in 0..4 {
        let Some(ClaudeTransientApiResult::Retry {
            completed_attempts,
            status,
            ..
        }) = classify_claude_transient_api_result(
            &overloaded,
            "session-bounded-retry",
            prior_completed_attempts,
            true,
        )
        else {
            panic!(
                "attempt {} should schedule a replay",
                prior_completed_attempts + 1
            );
        };
        assert_eq!(completed_attempts, prior_completed_attempts + 1);
        assert_eq!(status, 529);
    }

    assert_eq!(
        classify_claude_transient_api_result(&overloaded, "session-bounded-retry", 4, true),
        Some(ClaudeTransientApiResult::Exhausted {
            completed_attempts: 5,
            status: 529,
        })
    );
}

#[test]
fn claude_transient_api_retry_fails_closed_after_partial_turn_output() {
    let overloaded = json!({
        "type": "result",
        "subtype": "success",
        "is_error": true,
        "api_error_status": 529,
        "result": "API Error: 529 Overloaded."
    });

    assert_eq!(
        classify_claude_transient_api_result(&overloaded, "session-partial-output", 0, false,),
        None,
        "a turn that emitted transcript or tool activity must not be replayed"
    );
}

#[test]
fn claude_tool_use_marks_transient_api_replay_unsafe() {
    let mut state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;
    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "id": "tool-before-overload",
                    "name": "WebSearch",
                    "input": {}
                }]
            }
        }),
        &mut session_id,
        &mut state,
        &mut recorder,
    )
    .expect("tool use should be recorded");

    assert!(
        state.replay_became_unsafe,
        "observing a tool use must suppress whole-prompt replay"
    );
}

#[test]
fn claude_unknown_protocol_events_mark_transient_replay_unsafe() {
    for event in [
        json!({
            "type": "system",
            "subtype": "hook_started",
            "hook_name": "UserPromptSubmit",
            "hook_event": "UserPromptSubmit"
        }),
        json!({
            "type": "stream_event",
            "event": { "type": "future_stream_event" }
        }),
        json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "future_content_block",
                    "payload": "unknown"
                }]
            }
        }),
        json!({ "future": "missing top-level type" }),
    ] {
        let mut state = ClaudeTurnState::default();
        let mut recorder = TestRecorder::default();
        let mut session_id = None;

        handle_claude_event(&event, &mut session_id, &mut state, &mut recorder)
            .expect("unknown event should be tolerated but fail replay closed");

        assert!(
            state.replay_became_unsafe,
            "unrecognized event must disable whole-prompt replay: {event}"
        );
    }
}

#[test]
fn claude_hook_lifecycle_events_are_safety_only_and_do_not_reach_the_transcript() {
    for subtype in ["hook_started", "hook_response"] {
        let mut state = ClaudeTurnState::default();
        let mut recorder = TestRecorder::default();
        let mut session_id = None;

        handle_claude_event(
            &json!({
                "type": "system",
                "subtype": subtype,
                "hook_name": "UserPromptSubmit",
                "hook_event": "UserPromptSubmit"
            }),
            &mut session_id,
            &mut state,
            &mut recorder,
        )
        .expect("hook lifecycle events should be consumed without recorder output");

        assert!(state.replay_became_unsafe);
        assert!(session_id.is_none());
        assert!(recorder.texts.is_empty());
        assert!(recorder.text_deltas.is_empty());
        assert!(recorder.thinking.is_empty());
        assert!(recorder.commands.is_empty());
        assert!(recorder.approvals.is_empty());
        assert!(recorder.diffs.is_empty());
        assert!(recorder.parallel_agents.is_empty());
        assert!(recorder.subagent_results.is_empty());
    }
}

#[test]
fn claude_live_frame_sequence_keeps_only_pre_effect_overloads_replayable() {
    let overloaded = json!({
        "type": "result",
        "subtype": "success",
        "is_error": true,
        "api_error_status": 529,
        "result": "API Error: 529 Overloaded."
    });
    let prompt_echo = json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{
                "type": "text",
                "text": "Review this change."
            }]
        }
    });

    let mut state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;
    let mut observed_generation = None;

    // Claude Code 2.1.220 emits process-scoped SessionStart hooks before the
    // first prompt. The new prompt generation resets any out-of-turn parser
    // state, while exact process-local hook/status frames remain effect-free.
    handle_claude_event(
        &json!({
            "type": "system",
            "subtype": "hook_started",
            "hook_event": "SessionStart"
        }),
        &mut session_id,
        &mut state,
        &mut recorder,
    )
    .expect("SessionStart hook should be consumed");
    handle_claude_event(
        &json!({
            "type": "system",
            "subtype": "future_post_turn_event"
        }),
        &mut session_id,
        &mut state,
        &mut recorder,
    )
    .expect("out-of-turn unknown event should fail closed");
    assert!(state.replay_became_unsafe);
    reset_claude_turn_state_for_replay_generation(
        &mut observed_generation,
        Some("live-generation-1"),
        &mut state,
        &mut recorder,
    )
    .expect("new generation should establish a clean turn boundary");
    assert!(!state.replay_became_unsafe);
    for event in [
        json!({
            "type": "system",
            "subtype": "status",
            "status": "requesting"
        }),
        prompt_echo.clone(),
        json!({
            "type": "rate_limit_event",
            "rate_limit_info": {
                "status": "allowed"
            }
        }),
    ] {
        handle_claude_event(&event, &mut session_id, &mut state, &mut recorder)
            .expect("known effect-free frame should be consumed");
    }
    assert!(!state.replay_became_unsafe);
    assert!(matches!(
        classify_claude_transient_api_result(
            &overloaded,
            "session-live-sequence",
            0,
            !state.replay_became_unsafe
        ),
        Some(ClaudeTransientApiResult::Retry { status: 529, .. })
    ));

    // Prompt hooks occur after the prompt boundary and may have side effects.
    handle_claude_event(
        &json!({
            "type": "system",
            "subtype": "hook_response",
            "hook_event": "UserPromptSubmit"
        }),
        &mut session_id,
        &mut state,
        &mut recorder,
    )
    .expect("prompt hook should be tolerated but fail replay closed");
    assert_eq!(
        classify_claude_transient_api_result(
            &overloaded,
            "session-live-sequence",
            0,
            !state.replay_became_unsafe
        ),
        None
    );

    reset_claude_turn_state_for_replay_generation(
        &mut observed_generation,
        Some("live-generation-2"),
        &mut state,
        &mut recorder,
    )
    .expect("next prompt generation should reset the prior turn");
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "partial assistant output"
                }
            }
        }),
        &mut session_id,
        &mut state,
        &mut recorder,
    )
    .expect("partial assistant output should be recorded");
    assert_eq!(
        classify_claude_transient_api_result(
            &overloaded,
            "session-live-sequence",
            0,
            !state.replay_became_unsafe
        ),
        None
    );
}

#[test]
fn claude_retry_replays_the_exact_last_written_prompt() {
    let prompt = ClaudePromptCommand {
        attachments: vec![PromptImageAttachment {
            data: "encoded-image".to_owned(),
            metadata: MessageImageAttachment {
                byte_size: 13,
                file_name: "retry.png".to_owned(),
                media_type: "image/png".to_owned(),
            },
        }],
        replay_generation: "retry-generation-1".to_owned(),
        text: "review this exact prompt".to_owned(),
    };
    let mut writer = Vec::new();
    let replay_prompt = Arc::new(Mutex::new(None));

    write_claude_runtime_command(
        &mut writer,
        &replay_prompt,
        ClaudeRuntimeCommand::Prompt(prompt),
    )
    .expect("initial prompt should be written");
    write_claude_runtime_command(
        &mut writer,
        &replay_prompt,
        ClaudeRuntimeCommand::RetryLastPrompt {
            replay_generation: "retry-generation-1".to_owned(),
            retry_detail: "Retrying Claude automatically.".to_owned(),
        },
    )
    .expect("retry should replay the saved prompt");

    let messages = String::from_utf8(writer)
        .expect("Claude NDJSON should be UTF-8")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("valid Claude NDJSON"))
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0], messages[1]);
}

#[test]
fn claude_stale_retry_generation_is_ignored_and_terminal_clear_releases_prompt() {
    let prompt = ClaudePromptCommand {
        attachments: vec![PromptImageAttachment {
            data: "encoded-image".to_owned(),
            metadata: MessageImageAttachment {
                byte_size: 13,
                file_name: "retry.png".to_owned(),
                media_type: "image/png".to_owned(),
            },
        }],
        replay_generation: "current-generation".to_owned(),
        text: "retain only until terminal result".to_owned(),
    };
    let mut writer = Vec::new();
    let replay_prompt = Arc::new(Mutex::new(None));
    write_claude_runtime_command(
        &mut writer,
        &replay_prompt,
        ClaudeRuntimeCommand::Prompt(prompt),
    )
    .expect("initial prompt should be written");
    let initial_wire_len = writer.len();

    write_claude_runtime_command(
        &mut writer,
        &replay_prompt,
        ClaudeRuntimeCommand::RetryLastPrompt {
            replay_generation: "stale-generation".to_owned(),
            retry_detail: "Stale retry.".to_owned(),
        },
    )
    .expect("stale retry should be ignored without killing the runtime");
    assert_eq!(writer.len(), initial_wire_len);
    assert!(!clear_claude_replay_prompt_if_matches(
        &replay_prompt,
        "stale-generation"
    ));
    assert_eq!(
        claude_replay_generation(&replay_prompt).as_deref(),
        Some("current-generation")
    );

    assert!(clear_claude_replay_prompt_if_matches(
        &replay_prompt,
        "current-generation"
    ));
    assert_eq!(claude_replay_generation(&replay_prompt), None);

    write_claude_runtime_command(
        &mut writer,
        &replay_prompt,
        ClaudeRuntimeCommand::RetryLastPrompt {
            replay_generation: "current-generation".to_owned(),
            retry_detail: "Retry after terminal cleanup.".to_owned(),
        },
    )
    .expect("retry after terminal cleanup should remain a safe no-op");
    assert_eq!(writer.len(), initial_wire_len);
}

// Pins read-only auto-approval as a filtered Claude permission mode, not a
// shortcut to full `AutoApprove`. Read-only Bash commands may proceed without
// surfacing an approval card so `/review-code` can finish unattended.
#[test]
fn claude_read_only_auto_approve_allows_read_only_bash_permission_request() {
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &claude_permission_request(
            "Bash",
            json!({
                "command": "git diff --cached -- src/delegations.rs | head -40"
            }),
        ),
        &mut turn_state,
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        false,
        "C:/reviewer-sandbox",
        false,
    )
    .unwrap()
    .expect("permission request should be classified");

    let ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow {
        request_id,
        updated_input,
    }) = action
    else {
        panic!("read-only bash permission should be auto-allowed");
    };

    assert_eq!(request_id, "permission-request-1");
    assert_eq!(
        updated_input.get("command").and_then(Value::as_str),
        Some("git diff --cached -- src/delegations.rs | head -40")
    );
}

#[test]
fn claude_reviewer_allows_authorized_delegation_control_plane_submission() {
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &claude_permission_request(
            "mcp__termal-delegation__termal_submit_review_result",
            json!({ "schemaVersion": 1, "status": "completed" }),
        ),
        &mut turn_state,
        ClaudeApprovalMode::Ask,
        false,
        "C:/reviewer-sandbox",
        true,
    )
    .unwrap()
    .expect("permission request should be classified");

    assert!(matches!(
        action,
        ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow { .. })
    ));
}

#[test]
fn claude_reviewer_denies_control_plane_submission_without_delegation_authority() {
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &claude_permission_request(
            "mcp__termal-delegation__termal_submit_review_result",
            json!({ "schemaVersion": 1, "status": "completed" }),
        ),
        &mut turn_state,
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        false,
        "C:/reviewer-sandbox",
        false,
    )
    .unwrap()
    .expect("permission request should be classified");

    assert!(matches!(
        action,
        ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Deny { .. })
    ));
}

#[test]
fn claude_reviewer_does_not_auto_approve_an_unscoped_colliding_tool_name() {
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &claude_permission_request(
            TERMAL_SUBMIT_REVIEW_RESULT_TOOL_NAME,
            json!({ "schemaVersion": 1, "status": "completed" }),
        ),
        &mut turn_state,
        ClaudeApprovalMode::Ask,
        false,
        "C:/reviewer-sandbox",
        true,
    )
    .unwrap()
    .expect("permission request should be classified");

    assert!(matches!(
        action,
        ClaudeControlRequestAction::QueueApproval { .. }
    ));
}

#[test]
fn claude_reviewer_does_not_auto_approve_a_qualified_foreign_server_alias() {
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &claude_permission_request(
            "mcp__termal_delegation__termal_submit_review_result",
            json!({ "schemaVersion": 1, "status": "completed" }),
        ),
        &mut turn_state,
        ClaudeApprovalMode::Ask,
        false,
        "C:/reviewer-sandbox",
        true,
    )
    .unwrap()
    .expect("permission request should be classified");

    assert!(matches!(
        action,
        ClaudeControlRequestAction::QueueApproval { .. }
    ));
}

#[test]
fn claude_read_only_auto_approve_allows_review_code_bash_shapes() {
    for command in [
        "git status",
        "git status --short",
        "git diff --cached -- src/delegations.rs",
        "git diff --name-only && git diff --cached --name-only",
        "git ls-files --others --exclude-standard",
        "git --no-pager log",
        "git --no-pager diff --cached",
        "git diff --stat && echo \"=== X ===\" && git diff --name-only",
        "git remote -v",
        "git describe --tags --always",
        "git blame -L 10,20 src/claude.rs",
        "git --no-pager shortlog -sn",
        "git -P log",
        "git --no-optional-locks status",
        "git branch",
        "git branch -a",
        "git branch -vv",
        "git branch --list",
        "git branch --sort=-committerdate",
        "git log --grep='fix(scope)' -n 5",
        "git diff --text",
        "git shortlog -sn HEAD",
        "git grep -n TODO",
        "git grep -e ReadOnly",
        "git grep -eReadOnly",
        "git grep --text pattern",
        "cd ui && cat package.json",
        "find .claude/reviewers -name \"*.md\" 2>/dev/null",
        "grep -n ReadOnlyAutoApprove src/claude.rs | head -20",
        "grep -n 'two words' docs/bugs.md",
        "sed -n 1,120p src/claude.rs",
        "sed -e 's/window/door/' src/main.rs",
        "sed -e 's/^/word /' src/main.rs",
        "grep -n 'a & b' docs/bugs.md",
        "cat docs/bugs.md | tail -40",
        "wc -l src/claude.rs",
    ] {
        let mut turn_state = ClaudeTurnState::default();
        let action = classify_claude_control_request(
            &claude_permission_request("Bash", json!({ "command": command })),
            &mut turn_state,
            ClaudeApprovalMode::ReadOnlyAutoApprove,
            false,
            "C:/reviewer-sandbox",
            false,
        )
        .unwrap()
        .expect("permission request should be classified");

        let ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow {
            request_id,
            updated_input,
        }) = action
        else {
            panic!("read-only review-code command should be auto-allowed: {command}");
        };

        assert_eq!(request_id, "permission-request-1");
        assert_eq!(
            updated_input.get("command").and_then(Value::as_str),
            Some(command)
        );
    }
}

// Exercises `claude_bash_command_is_read_only` (through the full permission classifier)
// for read-only git *content* commands reviewers depend on. This pins two formerly over-broad
// denials while keeping the write boundary intact:
//   * `cd <cwd> && git …` was rejected wholesale by the cd+git exec-sink guard, even
//     though a `cd` into the reviewer's OWN working directory is a no-op — byte-for-byte
//     identical to running the git command with no `cd`. Reviewers `cd` into the target
//     repo first, so this silently killed their entire git surface -> INCONCLUSIVE.
//   * hashers (`sha256sum`, `md5sum`, `cksum`, `git patch-id`) were absent from the
//     read-only allow-lists, so diff-fingerprinting was blocked.
// The security boundary is exact-cwd-only: a `cd` into a *different* repo, or into a
// subdir (which may carry a nested `.git`), still fails closed because that genuinely
// retargets git the way `-C` / `--git-dir` do — AND that detection is tokenized, so a
// quoted/escaped `'git'` / `"git"` / `g\it` cannot slip past the guard. `git
// hash-object` is deliberately excluded: it runs gitattributes clean filters, an exec sink.
fn claude_bash_is_read_only_for_test(command: &str, cwd: &str) -> bool {
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &claude_permission_request("Bash", json!({ "command": command })),
        &mut turn_state,
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        false,
        cwd,
        false,
    )
    .unwrap()
    .expect("permission request should be classified");
    matches!(
        action,
        ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow { .. })
    )
}

#[test]
fn claude_read_only_auto_approve_allows_literal_read_only_for_loop() {
    let cwd = "/Users/greg/GitHub/Personal/rincon-common";
    for command in [
        "for f in architecture core legal platform security testing; do echo \"=== $f ===\"; head -30 /Users/greg/GitHub/Personal/rincon-common/.claude/reviewers/$f.md; done",
        "for f in architecture core; do echo \"${f}\"; done",
        "for f in architecture; do echo \"it's $f\"; done",
        "for f in architecture; do echo '$f'; done",
        "for f in architecture; do cd .; git status; echo $f; done",
    ] {
        assert!(
            claude_bash_is_read_only_for_test(command, cwd),
            "a bounded loop over literal values should be approved when every expanded body command is read-only: {command}"
        );
    }

    let maximum_body = std::iter::repeat_n("echo $f", 16)
        .collect::<Vec<_>>()
        .join("; ");
    let command = format!("for f in architecture; do {maximum_body}; done");
    assert!(
        claude_bash_is_read_only_for_test(&command, cwd),
        "literal loops should accept the documented body-command limit"
    );
}

#[test]
fn claude_read_only_auto_approve_denies_unsafe_for_loop_shapes() {
    let cwd = "/Users/greg/GitHub/Personal/rincon-common";
    for command in [
        "for f in architecture core; do touch $f; done",
        "for f in architecture core; do echo $f > out.txt; done",
        "for f in architecture; do echo $(touch victim); done",
        "for f in architecture; do echo $other; done",
        "for f in 'architecture; touch victim'; do echo $f; done",
        "for f in --output=/tmp/out; do git diff $f; done",
        "for f in other; do cd $f; git status; done",
        "for f in architecture; do echo $f; done; touch victim",
        "for f in architecture; do for g in core; do echo $g; done; done",
        "for f in architecture; do echo ${f; done",
        "for f in architecture; do; echo $f; done",
    ] {
        assert!(
            !claude_bash_is_read_only_for_test(command, cwd),
            "unsafe or dynamic loop shape must fail closed: {command}"
        );
    }

    let too_many_values = (0..65)
        .map(|index| format!("value{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let command = format!("for f in {too_many_values}; do echo $f; done");
    assert!(
        !claude_bash_is_read_only_for_test(&command, cwd),
        "literal loops must retain an explicit expansion bound"
    );

    let too_many_body_commands = std::iter::repeat_n("echo $f", 17)
        .collect::<Vec<_>>()
        .join("; ");
    let command = format!("for f in architecture; do {too_many_body_commands}; done");
    assert!(
        !claude_bash_is_read_only_for_test(&command, cwd),
        "literal loops must retain an explicit body-command bound"
    );
}

#[test]
fn read_only_git_content_checker_allows_reviewer_git_and_hashers() {
    // The reviewer child's own working directory, pre-normalized exactly as the runtime
    // does before handing it to the classifier.
    let cwd = normalize_local_user_facing_path("C:/github/Personal/TermAl");
    let allowed = [
        "git diff",
        "git --no-pager diff",
        "git --no-pager diff --binary --no-ext-diff --no-textconv --no-color",
        "git --no-pager diff --binary --no-ext-diff --no-textconv --no-color | wc -c",
        "git --no-pager diff --binary --no-ext-diff --no-textconv --no-color | cat",
        "git diff | wc -c",
        "git diff | cat",
        // Hashers as pipe targets — diff fingerprinting.
        "git --no-pager diff --binary --no-ext-diff --no-textconv --no-color | sha256sum",
        "git diff | md5sum",
        "git diff | cksum",
        "git --no-pager diff --no-ext-diff --no-textconv --no-color | git patch-id --stable",
        // `cd` into the reviewer's OWN cwd is a no-op; git content stays read-only.
        "cd \"C:/github/Personal/TermAl\" && git rev-parse HEAD",
        "cd \"C:/github/Personal/TermAl\" && git --no-pager diff --no-ext-diff --no-textconv --no-color | wc -c",
        "cd \"C:/github/Personal/TermAl\" && git diff | cat",
        "cd \"C:/github/Personal/TermAl\" && git diff | sha256sum",
        // Tokenized detection routes even a quoted `git` through the cd-guard; into the
        // OWN cwd it still passes, so the guard must not over-deny same-cwd quoting.
        "cd \"C:/github/Personal/TermAl\" && 'git' status",
        // The `cd` HEAD is tokenized too, so a quoted `cd` into the OWN cwd is the same
        // no-op as an unquoted one and stays allowed.
        "'cd' \"C:/github/Personal/TermAl\" && git status",
        // `cd .` is always a no-op regardless of cwd.
        "cd . && git diff",
    ];
    for command in allowed {
        assert!(
            claude_bash_is_read_only_for_test(command, &cwd),
            "expected allowed (read-only): {command}"
        );
    }

    let denied = [
        // A subdir may carry a nested `.git`, so cd into it still retargets git.
        "cd \"C:/github/Personal/TermAl/ui\" && git diff",
        // A different repo entirely.
        "cd \"C:/other/repo\" && git diff",
        // Bare `cd` (-> $HOME) and `cd ~` are not the cwd.
        "cd && git diff",
        "cd ~ && git diff",
        // A quoted/escaped `git` after `cd <other repo>` must NOT slip past the cd-guard:
        // the tokenizer de-quotes it to a real git command, so detection must too.
        "cd \"C:/other/repo\" && 'git' status",
        "cd \"C:/other/repo\" && \"git\" status",
        "cd \"C:/other/repo\" && g\\it status",
        // The mirror image: a quoted/escaped `cd` retargeting git must ALSO be
        // caught. The cd-guard decides "is this a cd" from tokens for the same
        // reason — the tokenizer de-quotes `'cd'` into a real cd — and the approval pass
        // accepts a tokenized `cd <target>`, so raw-text detection here would skip the
        // cwd check and hand back a git retargeted at another repo.
        "'cd' \"C:/other/repo\" && git status",
        "\"cd\" \"C:/other/repo\" && git status",
        "c\\d \"C:/other/repo\" && git status",
        // `git hash-object` is fully denied (clean-filter exec sink), with or without `-w`.
        "git hash-object src/claude.rs",
        "git hash-object -w src/claude.rs",
        "git diff | git hash-object --stdin",
        // Genuine writes stay denied, unaffected by the cwd allowance.
        "git commit -m x",
        "echo mutated > README.md",
    ];
    for command in denied {
        assert!(
            !claude_bash_is_read_only_for_test(command, &cwd),
            "expected denied: {command}"
        );
    }
}

// The runtime cwd is stored with backslashes on Windows while the agent writes `cd`
// with forward slashes, so the same-folder allowance must compare paths
// separator-insensitively (and case-insensitively on Windows). A raw string `==`
// rejected the exact reviewer command `cd "C:/github/Personal/PhoenixCodeNav" && git ...`.
#[test]
fn read_only_checker_same_folder_cd_matches_across_separator() {
    let backslash_cwd = "C:\\github\\Personal\\PhoenixCodeNav";
    // Forward-slash `cd` into a backslash cwd — the exact denied shape from the field.
    assert!(
        claude_bash_is_read_only_for_test(
            "cd \"C:/github/Personal/PhoenixCodeNav\" && git rev-parse --show-toplevel",
            backslash_cwd,
        ),
        "forward-slash cd into a backslash cwd must be allowed"
    );
    assert!(
        claude_bash_is_read_only_for_test(
            "cd \"C:/github/Personal/PhoenixCodeNav\" && git --no-optional-locks status --short",
            backslash_cwd,
        ),
        "forward-slash cd + git status into a backslash cwd must be allowed"
    );
    // Reverse: backslash `cd` into a forward-slash cwd.
    assert!(
        claude_bash_is_read_only_for_test(
            "cd \"C:\\github\\Personal\\PhoenixCodeNav\" && git diff",
            "C:/github/Personal/PhoenixCodeNav",
        ),
        "backslash cd into a forward-slash cwd must be allowed"
    );
    // A different repo, and a subdirectory (may hold a nested .git), stay denied
    // regardless of separators.
    assert!(
        !claude_bash_is_read_only_for_test(
            "cd \"C:/github/Personal/OtherRepo\" && git diff",
            backslash_cwd,
        ),
        "a different repo must stay denied"
    );
    assert!(
        !claude_bash_is_read_only_for_test(
            "cd \"C:/github/Personal/PhoenixCodeNav/src\" && git diff",
            backslash_cwd,
        ),
        "a subdirectory must stay denied"
    );
}

// Windows filesystems are case-insensitive, so a case-only difference is the same dir.
#[test]
#[cfg(windows)]
fn read_only_checker_same_folder_cd_is_case_insensitive_on_windows() {
    assert!(
        claude_bash_is_read_only_for_test(
            "cd \"c:/GitHub/personal/PHOENIXCODENAV\" && git diff",
            "C:\\github\\Personal\\PhoenixCodeNav",
        ),
        "case-only difference must be allowed on Windows"
    );
}

// The Windows PowerShell tool is denied wholesale for read-only reviewers.
//
// It used to route through the Bash reader, which implements BASH grammar. Every
// PowerShell-specific construct that reader mis-modelled became a security defect:
// `(...)`/`@(...)` sub-expression evaluation (survived only because the
// tokenizer happens to fail closed on `(`), the `2>/dev/null` strip writing
// `<drive>\dev\null`, the `cd ` head reaching `continue` before the
// tokenizer and giving an arbitrary-path WRITE, and `\` de-escaped per
// bash so `g\it` read as `git` while PowerShell executes `.\g\it` FROM THE
// REVIEWED TREE — arbitrary code execution.
//
// So this pins the structural rule, not another denylist: a bash parser gates only
// bash. Each historical escape is listed as a case so that re-introducing a
// PowerShell arm without its own fail-closed checker fails here first. The Bash
// counterparts assert the shared reader was NOT collaterally tightened — bash
// genuinely does de-escape `g\it` to git and does treat /dev/null as the null
// device, and reviewers depend on those.
#[test]
fn read_only_powershell_tool_is_denied_wholesale() {
    let cwd = "C:\\github\\Personal\\TermAl";
    let allow = |tool: &str, command: &str| {
        let mut turn_state = ClaudeTurnState::default();
        let action = classify_claude_control_request(
            &claude_permission_request(tool, json!({ "command": command })),
            &mut turn_state,
            ClaudeApprovalMode::ReadOnlyAutoApprove,
            false,
            cwd,
            false,
        )
        .unwrap()
        .expect("permission request should be classified");
        matches!(
            action,
            ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow { .. })
        )
    };

    for command in [
        // Every historical bypass shape.
        "echo (Set-Content victim.txt data)",
        "echo @(Set-Content victim.txt data)",
        "cd (Set-Content victim.txt data)",
        "g\\it status",
        "git status 2>/dev/null",
        "git status 2> /dev/null",
        // ...and the innocuous reads the arm used to clear: denial is wholesale, so
        // nothing here is a judgement call the parser can get wrong.
        "git --no-optional-locks status --short",
        "git --no-pager diff --cached --name-only",
        "echo hello",
        "cd \"C:/github/Personal/TermAl\" && git status",
    ] {
        assert!(
            !allow("PowerShell", command),
            "PowerShell must be denied wholesale for read-only reviewers: {command}"
        );
    }

    // The Bash reader keeps its exact behaviour — this fix must not be collateral.
    assert!(allow("Bash", "git --no-optional-locks status --short"));
    assert!(
        allow("Bash", "g\\it status"),
        "bash really does de-escape g\\it to git; the shared reader must still match bash"
    );
    assert!(
        allow("Bash", "git status 2>/dev/null"),
        "/dev/null is the real null device under bash; the idiom must survive"
    );
}

// Pins read-only Claude reviewer delegations denying explicit file mutation
// tool requests. This closes the bug where read-only reviewers used full
// `AutoApprove` and could allow `Write`/`Edit` operations.
#[test]
fn claude_read_only_auto_approve_denies_write_permission_request() {
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &claude_permission_request(
            "Write",
            json!({
                "file_path": "src/main.rs",
                "content": "mutated"
            }),
        ),
        &mut turn_state,
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        false,
        "C:/reviewer-sandbox",
        false,
    )
    .unwrap()
    .expect("permission request should be classified");

    let ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Deny {
        request_id,
        message,
    }) = action
    else {
        panic!("write permission should be denied");
    };

    assert_eq!(request_id, "permission-request-1");
    assert!(message.contains("read-only"));
}

#[test]
fn claude_read_only_auto_approve_denies_unsafe_bash_permission_request() {
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &claude_permission_request(
            "Bash",
            json!({
                "command": "echo mutated > README.md"
            }),
        ),
        &mut turn_state,
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        false,
        "C:/reviewer-sandbox",
        false,
    )
    .unwrap()
    .expect("permission request should be classified");

    let ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Deny {
        request_id,
        message,
    }) = action
    else {
        panic!("unsafe bash permission should be denied");
    };

    assert_eq!(request_id, "permission-request-1");
    assert!(message.contains("read-only"));
}

#[test]
fn claude_read_only_auto_approve_denies_mutating_git_find_and_sed_shapes() {
    for command in [
        "git branch -D old-branch",
        "git branch -m old-name new-name",
        "git branch new-branch",
        "git branch -dfoo",
        "git branch -mNEW",
        "git branch -uorigin/main",
        "git -C /abs branch -uorigin/main",
        // Clustered short options: git parses this as `-q -u origin/main`.
        "git branch -quorigin/main",
        "git branch -qd old-branch",
        "git branch -f",
        // Unambiguous long-option abbreviations git expands to mutating forms.
        "git branch --uns",
        "git branch --edi",
        "git branch --set-up=origin/main",
        // Repository retargeting: a tracked `fixture.git/config` is committable,
        // so these let reviewed content supply `core.fsmonitor`/`diff.external`.
        "git --git-dir=fixture.git --work-tree=. status",
        "git --git-dir=/abs/.git --work-tree=/abs status",
        "git --namespace foo status",
        "git -C /abs diff",
        "git -C \"/path with space\" status",
        "git -C /abs remote -v",
        "git -C /abs ls-files --others --exclude-standard",
        "find . -execdir rm {} \\;",
        "find . -fls files.txt",
        "find . -fprint files.txt",
        "find . -ok rm {} \\;",
        "find . '-execdir' rm {} \\;",
        "sed --in-place s/a/b/ src/main.rs",
        "sed -i.bak s/a/b/ src/main.rs",
        "sed -e w/out.txt src/main.rs",
        "sed '-i.bak' s/a/b/ src/main.rs",
        "sed -e 'w out.txt' src/main.rs",
        "sed -f script.sed src/main.rs",
        "sed 'w/tmp/out' src/main.rs",
        "sed '1w/tmp/out' src/main.rs",
        "sed -n '/foo/w/tmp/out' src/main.rs",
        "sed -e 's/a/b/w out.txt' src/main.rs",
        "sed -e 'W out.txt' src/main.rs",
        "sed -e 'e date' src/main.rs",
        "git diff --output=out.patch",
        "git log --output out.log",
        "git show --output=out.patch HEAD",
        "git diff --ext-diff",
        "git diff --textconv",
        "git -C /abs diff --ext-diff",
        // Abbreviations git expands to the denied options above.
        "git diff --ext",
        "git diff --textc",
        "git diff --out=/tmp/x",
        "git log --outp out.log",
        "git grep --open pattern",
        "git grep -Ocat pattern",
        "git grep -O pattern",
        "git grep --open-files-in-pager pattern",
        // Clustered short options bundle `-O`: git parses `-nOcat` as
        // `-n --open-files-in-pager=cat`.
        "git grep -nOcat pattern",
        "git grep -inOcat pattern",
        "git grep --textconv pattern",
        "git grep --textc pattern",
        // Backslash escaping: bash strips the backslash, so these execute the
        // denied option even though the raw token does not match it literally.
        "git diff --out\\put=/tmp/x",
        "git grep --open\\-files-in-pager=cat pattern",
        // `shortlog --output <path>` writes/truncates an arbitrary file.
        "git shortlog --output=/tmp/x",
        "git shortlog --output /tmp/x HEAD",
        // Shell expansion / subshells the tokenizer cannot resolve rewrite the
        // argv before git runs it, so the literal text must fail closed.
        "git diff $'--outp\\x75t=out.patch'",
        "git diff ${OUT}",
        "git diff $(printf -- --output=x)",
        "git diff `printf x`",
        "git diff --outp{u,X}t=/tmp/x",
        "git diff <(printf x)",
        "git diff \"$OUT\"",
        // Unquoted globs expand before git runs; a tracked filename like
        // `--output=x` turns `git diff *` into a file write.
        "git diff *",
        "git diff *.rs",
        "git diff ?.rs",
        "git diff src/foo[12].rs",
        "git grep pattern *.ts",
        "git branch --set-upstream-to=origin/main",
        "git branch --unset-upstream",
        "git branch --edit-description",
        "git branch --create-reflog",
        "git -C /abs commit -m x",
        "git -C /abs push",
        "git -c a=b push",
        "git -c diff.external=/x diff",
        "git -c core.fsmonitor=/x status",
        "git -c core.pager=cat diff",
        "git -C /abs checkout .",
        "git -C /abs reset --hard",
        "git -C /abs add .",
        "git add .",
        "git -C /abs restore .",
        "git -C /abs merge main",
        "git -C /abs rebase main",
        "git -C /abs config user.email evil@example.com",
        "git stash",
        "git tag v1.0.0",
        "git switch -c topic",
        "git cherry-pick HEAD~1",
        "git revert HEAD",
        "git clean -fd",
        "git rm -r src",
        "git mv a b",
        "git am patch.mbox",
        "git remote add origin https://example.com/x.git",
        "git remote set-url origin git@example.com:x.git",
        "git remote remove origin",
        "git -C /abs remote prune origin",
        "git -C /abs diff --output=/tmp/x",
        "git --exec-path=/evil diff",
        "git --paginate log",
        "git -p log",
        "git -Z diff",
        "git -C /abs",
        "rg --pre 'cat' pattern src",
        "cat README.md & touch /tmp/termal-owned",
        // An escaped quote must not hide the `&` background separator: bash
        // reads `\"` as a literal, so the trailing `& touch ...` still runs.
        "echo \\\"& touch /tmp/termal-owned",
        // A stripped `2>/dev/null` must not make a real background `&` look
        // escaped: the separator scan runs on the original command.
        "echo first \\2>/dev/null& touch /tmp/termal-owned",
        // `cd` into a repo fixture retargets git the same way `-C`/`--git-dir`
        // do, so a directory change combined with git fails closed.
        "cd fixture.git && git status",
        "cd /tmp && git diff --stat",
        // `--help` / `-h` dispatch through `git help`'s configured viewer.
        "git blame --help",
        "git shortlog --help",
        "git diff --help",
        "git status -h",
    ] {
        let mut turn_state = ClaudeTurnState::default();
        let action = classify_claude_control_request(
            &claude_permission_request("Bash", json!({ "command": command })),
            &mut turn_state,
            ClaudeApprovalMode::ReadOnlyAutoApprove,
            false,
            "C:/reviewer-sandbox",
            false,
        )
        .unwrap()
        .expect("permission request should be classified");

        let ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Deny {
            request_id,
            message,
        }) = action
        else {
            panic!("mutating read-only-looking command should be denied: {command}");
        };

        assert_eq!(request_id, "permission-request-1");
        assert!(message.contains("read-only"));
    }
}

// Pins `clear_claude_turn_state` zeroing every field of `ClaudeTurnState` —
// approval keys, parallel agent group key and order, pending tools, the
// streamed text buffer, the `saw_text_delta` flag, and
// `permission_denied_this_turn`. Guards against leaking per-turn state
// (stale pending tools, phantom parallel agents, already-seen approvals)
// into the next Claude turn, which would corrupt the next transcript.
#[test]
fn clear_claude_turn_state_resets_all_fields() {
    let mut state = ClaudeTurnState {
        approval_keys_this_turn: HashSet::from(["approval-1".to_owned()]),
        unattended_questions_self_resolved_this_turn: 2,
        parallel_agent_group_key: Some("group-1".to_owned()),
        parallel_agent_order: vec!["agent-1".to_owned()],
        parallel_agents: HashMap::from([(
            "agent-1".to_owned(),
            ParallelAgentProgress {
                detail: Some("Working".to_owned()),
                id: "agent-1".to_owned(),
                source: ParallelAgentSource::Tool,
                status: ParallelAgentStatus::Running,
                title: "Agent 1".to_owned(),
            },
        )]),
        permission_denied_this_turn: true,
        pending_tools: HashMap::from([(
            "tool-1".to_owned(),
            ClaudeToolUse {
                command: Some("echo hi".to_owned()),
                description: Some("Shell".to_owned()),
                file_path: Some("README.md".to_owned()),
                name: "bash".to_owned(),
                subagent_type: Some("worker".to_owned()),
            },
        )]),
        replay_became_unsafe: true,
        streamed_assistant_text: "partial".to_owned(),
        saw_text_delta: true,
    };

    clear_claude_turn_state(&mut state);

    assert!(state.approval_keys_this_turn.is_empty());
    assert_eq!(state.unattended_questions_self_resolved_this_turn, 0);
    assert_eq!(state.parallel_agent_group_key, None);
    assert!(state.parallel_agent_order.is_empty());
    assert!(state.parallel_agents.is_empty());
    assert!(!state.permission_denied_this_turn);
    assert!(state.pending_tools.is_empty());
    assert!(!state.replay_became_unsafe);
    assert!(state.streamed_assistant_text.is_empty());
    assert!(!state.saw_text_delta);
}

// Pins `reset_claude_turn_state` as the softer variant used at end-of-turn:
// it runs the full `clear_claude_turn_state` field wipe plus finalizes any
// open streaming text bubble on the recorder and calls `reset_turn_state`.
// Guards against a result envelope leaving a half-streamed text bubble open
// or failing to notify the recorder that the turn has ended, which would
// leak partial text into the next turn's transcript.
#[test]
fn reset_claude_turn_state_clears_all_fields_and_finishes_streaming_text() {
    let mut state = ClaudeTurnState {
        approval_keys_this_turn: HashSet::from(["approval-1".to_owned()]),
        unattended_questions_self_resolved_this_turn: 2,
        parallel_agent_group_key: Some("group-1".to_owned()),
        parallel_agent_order: vec!["agent-1".to_owned()],
        parallel_agents: HashMap::from([(
            "agent-1".to_owned(),
            ParallelAgentProgress {
                detail: Some("Working".to_owned()),
                id: "agent-1".to_owned(),
                source: ParallelAgentSource::Tool,
                status: ParallelAgentStatus::Running,
                title: "Agent 1".to_owned(),
            },
        )]),
        permission_denied_this_turn: true,
        pending_tools: HashMap::from([(
            "tool-1".to_owned(),
            ClaudeToolUse {
                command: Some("echo hi".to_owned()),
                description: Some("Shell".to_owned()),
                file_path: Some("README.md".to_owned()),
                name: "bash".to_owned(),
                subagent_type: Some("worker".to_owned()),
            },
        )]),
        replay_became_unsafe: true,
        streamed_assistant_text: "partial".to_owned(),
        saw_text_delta: true,
    };
    let mut recorder = TestRecorder {
        streaming_text_delta_start: Some(2),
        streaming_text_active: true,
        ..TestRecorder::default()
    };

    reset_claude_turn_state(&mut state, &mut recorder).unwrap();

    assert!(state.approval_keys_this_turn.is_empty());
    assert_eq!(state.unattended_questions_self_resolved_this_turn, 0);
    assert_eq!(state.parallel_agent_group_key, None);
    assert!(state.parallel_agent_order.is_empty());
    assert!(state.parallel_agents.is_empty());
    assert!(!state.permission_denied_this_turn);
    assert!(state.pending_tools.is_empty());
    assert!(!state.replay_became_unsafe);
    assert!(state.streamed_assistant_text.is_empty());
    assert!(!state.saw_text_delta);
    assert_eq!(recorder.reset_turn_state_calls, 1);
    assert_eq!(recorder.finish_streaming_text_calls, 2);
    assert_eq!(recorder.streaming_text_delta_start, None);
    assert!(!recorder.streaming_text_active);
}

// Pins the complete AskUserQuestion denial result lifecycle: the assistant
// registers the tool use, TermAl deliberately returns an unattended deny,
// and Claude reports that deny as an error-shaped tool result. The resolved
// question card already owns that outcome, so the result must not add a
// duplicate transcript error; an unrelated AskUserQuestion failure must.
#[test]
fn claude_ask_user_question_denial_result_is_suppressed_but_unexpected_error_is_recorded() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "id": "question-tool-1",
                    "name": "AskUserQuestion",
                    "input": {
                        "questions": [{
                            "header": "Scope",
                            "question": "Which scope should I use?"
                        }]
                    }
                }]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let action = classify_claude_control_request(
        &claude_ask_user_question_permission_request(),
        &mut turn_state,
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        true,
        "/tmp",
        false,
    )
    .expect("permission payload should parse")
    .expect("permission payload should be classified");
    let ClaudeControlRequestAction::RecordSelfResolvedQuestion {
        response:
            ClaudeSelfResolvedQuestionResponse::PermissionDeny(ClaudePermissionDecision::Deny {
                message: denial_message,
                ..
            }),
        ..
    } = action
    else {
        panic!("the unattended question should produce a permission deny");
    };

    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "question-tool-1",
                    "is_error": true,
                    "content": denial_message
                }]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    assert!(recorder.errors.is_empty());
    assert!(!turn_state.pending_tools.contains_key("question-tool-1"));

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "id": "question-tool-skip",
                    "name": "AskUserQuestion",
                    "input": {"questions": []}
                }]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "question-tool-skip",
                    "is_error": true,
                    "content": CLAUDE_USER_DECLINED_QUESTION_MESSAGE
                }]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    assert!(recorder.errors.is_empty());

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "id": "question-tool-2",
                    "name": "AskUserQuestion",
                    "input": {"questions": []}
                }]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "question-tool-2",
                    "is_error": true,
                    "content": "AskUserQuestion failed to validate its schema."
                }]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    assert_eq!(
        recorder.errors,
        vec!["AskUserQuestion failed to validate its schema.".to_owned()]
    );

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "id": "question-tool-3",
                    "name": "AskUserQuestion",
                    "input": {"questions": []}
                }]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "question-tool-3",
                    "is_error": true,
                    "content": "AskUserQuestion failed while checking requested permissions."
                }]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    assert_eq!(
        recorder.errors,
        vec![
            "AskUserQuestion failed to validate its schema.".to_owned(),
            "AskUserQuestion failed while checking requested permissions.".to_owned(),
        ]
    );
}

// Pins `handle_claude_tool_use` fanning out two concurrent `task` tool_use
// frames into a pair of `ParallelAgentProgress` entries titled by
// `description`, both in `Initializing` status with detail "Initializing...".
// Guards against the `task` fan-out being lost, collapsed into a single
// agent, or recorded with the wrong status so the UI would show only one
// sub-agent instead of the full group running in parallel.
#[test]
fn claude_task_tool_use_updates_parallel_agent_progress() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {
                            "description": "Rust code review",
                            "subagent_type": "general-purpose"
                        }
                    },
                    {
                        "type": "tool_use",
                        "id": "task-2",
                        "name": "Task",
                        "input": {
                            "description": "Architecture code review",
                            "subagent_type": "general-purpose"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("parallel agents update should be recorded");
    assert_eq!(latest.len(), 2);
    assert_eq!(latest[0].title, "Rust code review");
    assert_eq!(latest[0].detail.as_deref(), Some("Initializing..."));
    assert_eq!(latest[0].status, ParallelAgentStatus::Initializing);
    assert_eq!(latest[1].title, "Architecture code review");
    assert_eq!(latest[1].status, ParallelAgentStatus::Initializing);
}

// Pins `handle_claude_task_tool_result` advancing an initializing
// `ParallelAgentProgress` to `Completed` with a single-line detail preview,
// and emitting a `push_subagent_result` carrying the full multi-line body.
// Guards against the parent transcript losing the sub-agent's return value
// or the progress card being stuck in `Initializing` after the task tool
// returns successfully.
#[test]
fn claude_task_tool_result_updates_parallel_agents_and_records_subagent_result() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {
                            "description": "Rust code review",
                            "subagent_type": "general-purpose"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let detail = "Reviewer found a batching bug in location smoothing.\nRead src/state.rs for the stale preview path.";
    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "task-1",
                        "content": detail
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("completed parallel agent update should be recorded");
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].title, "Rust code review");
    assert_eq!(latest[0].source, ParallelAgentSource::Tool);
    assert_eq!(latest[0].status, ParallelAgentStatus::Completed);
    assert_eq!(
        latest[0].detail.as_deref(),
        Some("Reviewer found a batching bug in location smoothing.")
    );
    assert_eq!(
        recorder.subagent_results,
        vec![("Rust code review".to_owned(), detail.to_owned())]
    );
}

// Pins Claude task-result updates reclaiming an existing progress row as
// tool-sourced. This is a release-mode guard: a source mismatch must not
// silently preserve a delegation-routable id on a Claude Task row.
#[test]
fn claude_task_tool_result_resets_existing_non_tool_progress_source() {
    let mut turn_state = ClaudeTurnState {
        parallel_agent_group_key: Some("group-1".to_owned()),
        parallel_agent_order: vec!["task-1".to_owned()],
        parallel_agents: HashMap::from([(
            "task-1".to_owned(),
            ParallelAgentProgress {
                detail: Some("Running".to_owned()),
                id: "task-1".to_owned(),
                source: ParallelAgentSource::Delegation,
                status: ParallelAgentStatus::Running,
                title: "Task agent".to_owned(),
            },
        )]),
        pending_tools: HashMap::from([(
            "task-1".to_owned(),
            ClaudeToolUse {
                command: None,
                description: Some("Rust code review".to_owned()),
                file_path: None,
                name: "Task".to_owned(),
                subagent_type: Some("general-purpose".to_owned()),
            },
        )]),
        ..ClaudeTurnState::default()
    };
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "task-1",
                        "content": "Reviewer finished."
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("parallel agent source repair should be recorded");
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].id, "task-1");
    assert_eq!(latest[0].source, ParallelAgentSource::Tool);
    assert_eq!(latest[0].status, ParallelAgentStatus::Completed);
    assert_eq!(
        turn_state
            .parallel_agents
            .get("task-1")
            .expect("task row should remain")
            .source,
        ParallelAgentSource::Tool,
    );
}

// Pins `handle_claude_task_tool_error` flipping the progress entry to
// `Error` with the first failure line as the preview detail, while handing
// the full multi-line payload (stack trace and all) to the recorder via
// `push_subagent_result`. Guards against failure diagnostics being
// truncated to the preview or dropped entirely, which would hide the real
// cause of the sub-agent failure from the user.
#[test]
fn claude_task_tool_error_records_full_failure_detail() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {
                            "description": "Rust code review",
                            "subagent_type": "general-purpose"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let detail = "Reviewer failed to parse the diff.\nStack trace line 1\nStack trace line 2";
    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "task-1",
                        "is_error": true,
                        "content": detail
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("errored parallel agent update should be recorded");
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].title, "Rust code review");
    assert_eq!(latest[0].status, ParallelAgentStatus::Error);
    assert_eq!(
        latest[0].detail.as_deref(),
        Some("Reviewer failed to parse the diff.")
    );
    assert_eq!(
        recorder.subagent_results,
        vec![("Rust code review".to_owned(), detail.to_owned())]
    );
}

// Pins `handle_claude_task_tool_error` substituting the literal "Task
// failed." string when the tool_result has `is_error: true` but an empty
// content body — used both for the progress detail and for
// `push_subagent_result`. Guards against empty-detail errors producing an
// empty subagent result bubble or a parallel agent card that shows no
// reason for the failure.
#[test]
fn claude_task_tool_error_without_detail_records_fallback_failure_message() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {
                            "description": "Rust code review",
                            "subagent_type": "general-purpose"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "task-1",
                        "is_error": true,
                        "content": ""
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("errored parallel agent update should be recorded");
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].status, ParallelAgentStatus::Error);
    assert_eq!(latest[0].detail.as_deref(), Some("Task failed."));
    assert_eq!(
        recorder.subagent_results,
        vec![("Rust code review".to_owned(), "Task failed.".to_owned())]
    );
}

// Pins `handle_claude_streamed_text` reconciling a short stream ("Hello")
// with a longer final assistant text ("Hello there.") arriving after
// `message_stop`, by appending the missing " there." suffix to the open
// bubble so the transcript ends up with the full final text in a single
// `Message::Text`. Guards against lost trailing words when Claude flushes
// the full payload only in the post-`message_stop` `assistant` envelope.
#[test]
fn claude_streamed_text_appends_missing_final_suffix_after_message_stop() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "Hello"
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "message_stop"
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": "Hello there."
                    }
                ]
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Hello there."
    ));
}

// Pins `handle_claude_streamed_text` recognizing that the final assistant
// text exactly matches the already-streamed buffer and skipping the append,
// so the transcript keeps a single `Message::Text` rather than duplicating
// the full line. Guards against doubled assistant text in the bubble when
// Claude's post-`message_stop` payload restates the complete streamed body
// verbatim.
#[test]
fn claude_streamed_text_skips_duplicate_final_text_after_message_stop() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "Hello there."
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "message_stop"
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": "Hello there."
                    }
                ]
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Hello there."
    ));
}

// Pins `handle_claude_streamed_text` calling `replace_streaming_text` when
// the final assistant body ("Final answer.") is not a prefix-extension of
// the streamed draft ("Draft answer."), so the bubble is rewritten in
// place to the authoritative final text. Guards against TermAl keeping a
// stale early draft (or concatenating draft+final) when Claude rewrites
// its own in-flight text.
#[test]
fn claude_streamed_text_replaces_divergent_final_text() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "Draft answer."
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": "Final answer."
                    }
                ]
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Final answer."
    ));
}

// Pins the transcript boundary: `handle_claude_tool_use` arriving after a
// streamed text bubble has ended must close the text `Message` and start a
// fresh `Message::Command`, then a subsequent stream delta opens yet
// another text bubble — yielding three distinct messages (text, command,
// text) in order. Guards against follow-up tool calls or post-tool text
// being appended to an already-closed text bubble.
#[test]
fn claude_tool_use_after_streamed_text_starts_followup_in_new_message() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "Hello"
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "message_stop"
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "bash-1",
                        "name": "Bash",
                        "input": {
                            "command": "pwd"
                        }
                    }
                ]
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "World"
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");

    assert_eq!(session.messages.len(), 3);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Hello"
    ));
    assert!(matches!(
        session.messages.get(1),
        Some(Message::Command {
            command,
            output,
            status,
            ..
        }) if command == "pwd" && output.is_empty() && *status == CommandStatus::Running
    ));
    assert!(matches!(
        session.messages.get(2),
        Some(Message::Text { text, .. }) if text == "World"
    ));
}

// Pins `handle_claude_result` draining `pending_tools` so that a
// tool_result envelope arriving after the turn's `result` is silently
// discarded rather than mutating a recorded command — the Running Bash
// command keeps its original empty output and `Running` status. Guards
// against stray late tool-result frames from Claude retroactively
// rewriting a completed turn's transcript.
#[test]
fn claude_result_clears_pending_tools_and_ignores_late_tool_results() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "bash-1",
                        "name": "Bash",
                        "input": {
                            "command": "pwd"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    assert!(turn_state.pending_tools.is_empty());

    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "bash-1",
                        "content": "/tmp/late"
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(
        recorder.commands,
        vec![("pwd".to_owned(), String::new(), CommandStatus::Running)]
    );
}

// Pins `handle_claude_result` resetting the recorder's command-id keying
// between turns so a second turn reusing the same `tool_use_id` ("bash-1")
// registers a fresh command rather than overwriting the prior turn's
// completed Bash message — both commands end up persisted with their own
// output and `Success` status. Guards against cross-turn id collisions
// merging two independent Bash invocations into one transcript entry.
#[test]
fn claude_result_resets_recorder_command_keys_between_turns() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    for (command, output) in [("pwd", "/tmp/one"), ("git status", "working tree clean")] {
        handle_claude_event(
            &json!({
                "type": "assistant",
                "message": {
                    "content": [
                        {
                            "type": "tool_use",
                            "id": "bash-1",
                            "name": "Bash",
                            "input": {
                                "command": command
                            }
                        }
                    ]
                }
            }),
            &mut external_session_id,
            &mut turn_state,
            &mut recorder,
        )
        .unwrap();
        handle_claude_event(
            &json!({
                "type": "user",
                "message": {
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "bash-1",
                            "content": output
                        }
                    ]
                }
            }),
            &mut external_session_id,
            &mut turn_state,
            &mut recorder,
        )
        .unwrap();
        handle_claude_event(
            &json!({
                "type": "result",
                "is_error": false
            }),
            &mut external_session_id,
            &mut turn_state,
            &mut recorder,
        )
        .unwrap();
    }

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");
    let commands = session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Command {
                command,
                output,
                status,
                ..
            } => Some((command.clone(), output.clone(), *status)),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        commands,
        vec![
            (
                "pwd".to_owned(),
                "/tmp/one".to_owned(),
                CommandStatus::Success
            ),
            (
                "git status".to_owned(),
                "working tree clean".to_owned(),
                CommandStatus::Success
            ),
        ]
    );
}
