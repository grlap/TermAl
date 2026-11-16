//! Ordinary permission regressions using the recovered Read content shape.
//! Session/RPC/tool IDs and the path are synthetic; options are reconstructed
//! from kimi-code f409caa21e71ce7beb158d29ffca1fed76216a64's ACP approval mapper.
//! The original card retained Content, not the complete wire envelope.
use super::*;

fn read_permission() -> Value {
    serde_json::from_str(include_str!("../fixtures/kimi-tool-permission.json")).unwrap()
}

fn assert_automatically_answered(request: Value) {
    let (state, id, rx, runtime) = running_kimi("auto-approve");
    let messages_before = session(&state, &id).messages.len();
    deliver(&state, &id, &runtime, request, RUNTIME);
    assert_eq!(
        answered(&rx),
        Some(json!({"outcome":"selected", "optionId":"approve_once"})),
        "an ordinary ask must receive its one-time answer without a user wait"
    );
    assert_eq!(answered(&rx), None, "only one answer is sent");
    assert_eq!(manual_cards(&state, &id), 0);
    let current = session(&state, &id);
    assert_eq!(current.status, SessionStatus::Active);
    assert_eq!(
        current.messages.len(),
        messages_before,
        "no approval card or notice"
    );
}

#[test]
fn kimi_auto_approve_answers_read_inside_git_worktrees_without_a_card() {
    assert_automatically_answered(read_permission());
}

#[test]
fn kimi_auto_approve_answers_an_unknown_ordinary_tool_without_a_card() {
    let mut request = read_permission();
    request["params"]["toolCall"]["title"] = json!("FutureWorkspaceOperation");
    request["params"]["toolCall"]["content"][0]["content"]["text"] =
        json!("Requesting approval to performing the requested workspace operation");
    assert_automatically_answered(request);
}

#[test]
fn kimi_auto_approve_uses_the_opaque_once_id_instead_of_position_or_label() {
    let (state, id, rx, runtime) = running_kimi("auto-approve");
    let mut request = read_permission();
    request["params"]["options"] = json!([
        {"optionId":"permanent","kind":"allow_always","name":"Allow once"},
        {"optionId":"opaque-once-42","kind":"allow_once","name":"Reject"},
        {"optionId":"approve_once","kind":"reject_once","name":"Allow once"}
    ]);
    deliver(&state, &id, &runtime, request, RUNTIME);
    assert_eq!(
        answered(&rx),
        Some(json!({"outcome":"selected","optionId":"opaque-once-42"}))
    );
    assert_eq!(answered(&rx), None);
    assert_eq!(manual_cards(&state, &id), 0);
    assert_eq!(session(&state, &id).status, SessionStatus::Active);
}

#[test]
fn kimi_manual_read_card_has_readable_content_and_keeps_exact_decisions() {
    for (decision, option_id) in [
        (ApprovalDecision::Accepted, "approve_once"),
        (ApprovalDecision::AcceptedForSession, "approve_always"),
        (ApprovalDecision::Rejected, "reject"),
    ] {
        let (state, id, rx, runtime) = running_kimi("ask");
        let request = read_permission();
        let action = request["params"]["toolCall"]["content"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .to_owned();
        deliver(&state, &id, &runtime, request, RUNTIME);
        assert_eq!(
            answered(&rx),
            None,
            "ask mode waits for its explicit decision"
        );
        let (message_id, detail) = session(&state, &id)
            .messages
            .into_iter()
            .find_map(|message| match message {
                Message::Approval { id, detail, .. } => Some((id, detail)),
                _ => None,
            })
            .expect("ask mode retains an approval card");
        assert!(
            detail.contains(&action),
            "the actual requested action is visible: {detail}"
        );
        assert!(
            !detail.contains("\"content\"") && !detail.contains("\"type\""),
            "the card renders content text, not serialized ACP objects: {detail}"
        );
        state.update_approval(&id, &message_id, decision).unwrap();
        assert_eq!(
            answered(&rx),
            Some(json!({"outcome":"selected", "optionId":option_id}))
        );
        assert_eq!(answered(&rx), None, "the user decision is delivered once");
    }
}

#[test]
fn kimi_read_only_refusal_is_answered_with_a_visible_bounded_reason() {
    let (state, id, rx, runtime) = read_only_kimi_fixture();
    let before = session(&state, &id).messages.len();
    let mut events = state.subscribe_delta_events();
    let stamp_before = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&id).unwrap()].mutation_stamp
    };
    deliver(
        &state,
        &id,
        &runtime,
        permission("Write", tool_options()),
        RUNTIME,
    );
    assert_eq!(
        answered(&rx),
        Some(json!({"outcome":"selected", "optionId":"reject"}))
    );
    assert_eq!(
        manual_cards(&state, &id),
        0,
        "a constraint is not an approval wait"
    );
    let current = session(&state, &id);
    let (reason_id, reason) = current
        .messages
        .iter()
        .skip(before)
        .find_map(|message| match message {
            Message::Text {
                id,
                author: Author::System,
                text,
                ..
            } if text.contains("read-only") && text.contains("Write") => Some((id, text)),
            _ => None,
        })
        .expect("the refusal reason must be visible on the session, not only stderr");
    assert!(
        reason.chars().count() <= 2_000,
        "constraint reasons stay bounded"
    );
    assert!(
        reason.contains("cannot")
            || reason.contains("refus")
            || reason.contains("denied")
            || reason.contains("not allowed"),
        "the reason explains the enforced constraint: {reason}"
    );
    let mut published = false;
    while let Ok(payload) = events.try_recv() {
        if let DeltaEvent::MessageCreated {
            session_id,
            message_id,
            message:
                Message::Text {
                    author: Author::System,
                    text,
                    ..
                },
            status,
            session_mutation_stamp: Some(stamp),
            ..
        } = serde_json::from_str(&payload).unwrap()
        {
            if message_id == *reason_id {
                assert_eq!(session_id, id);
                assert_eq!(text, *reason);
                assert_eq!(status, SessionStatus::Active);
                assert!(stamp > stamp_before);
                published = true;
            }
        }
    }
    assert!(
        published,
        "the visible reason must reach clients with the stamped message delta"
    );
}

#[test]
fn kimi_flat_plan_permission_remains_an_explicit_user_decision() {
    let (state, id, rx, runtime) = running_kimi("auto-approve");
    let mut request = permission("ExitPlanMode", tool_options());
    request["params"]["toolCall"]
        .as_object_mut()
        .unwrap()
        .remove("title");
    request["params"]["toolName"] = json!("ExitPlanMode");
    deliver(&state, &id, &runtime, request, RUNTIME);
    assert_eq!(answered(&rx), None);
    assert_eq!(manual_cards(&state, &id), 1);
    assert_eq!(session(&state, &id).status, SessionStatus::Approval);
}

#[test]
fn kimi_read_only_stale_or_stopping_request_cancels_without_a_new_diagnostic() {
    for stopping in [false, true] {
        let (state, id, rx, runtime) = read_only_kimi_fixture();
        let before = session(&state, &id).messages.len();
        if stopping {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&id).unwrap();
            inner.sessions[index].runtime_stop_in_progress = true;
        }
        deliver(
            &state,
            &id,
            &runtime,
            permission("Write", tool_options()),
            if stopping { RUNTIME } else { "retired-runtime" },
        );
        assert_eq!(answered(&rx), Some(json!({"outcome":"cancelled"})));
        assert_eq!(manual_cards(&state, &id), 0);
        assert_eq!(session(&state, &id).messages.len(), before);
    }
}

#[test]
fn acp_manual_diff_card_keeps_new_and_old_content_visible_for_large_files() {
    let (state, id, rx, runtime) = running_kimi("ask");
    let mut request = read_permission();
    request["params"]["toolCall"]["content"] = json!([{
        "type":"diff", "path":"large-file.txt",
        "oldText": format!("OLD_VISIBLE\n{}", "🦀".repeat(9_000)),
        "newText": format!("NEW_VISIBLE\n{}", "🪴".repeat(9_000))
    }]);
    deliver(&state, &id, &runtime, request, RUNTIME);
    assert_eq!(answered(&rx), None);
    let (message_id, detail) = session(&state, &id)
        .messages
        .into_iter()
        .find_map(|message| match message {
            Message::Approval { id, detail, .. } => Some((id, detail)),
            _ => None,
        })
        .unwrap();
    assert!(
        detail.contains("After:\nNEW_VISIBLE"),
        "the proposed write must be visible"
    );
    assert!(
        detail.contains("Before:\nOLD_VISIBLE"),
        "the prior content retains its own budget"
    );
    assert!(detail.contains("large-file.txt"));
    assert!(detail.chars().count() <= 8_002);
    assert!(detail.contains('…'), "truncation must remain visible");
    state
        .update_approval(&id, &message_id, ApprovalDecision::Accepted)
        .unwrap();
    assert_eq!(
        answered(&rx),
        Some(json!({"outcome":"selected","optionId":"approve_once"}))
    );
    assert_eq!(answered(&rx), None);
}

#[test]
fn acp_manual_card_renders_single_content_objects_and_preserves_rejection() {
    for content in [
        json!({"type":"text","text":"SINGLE_ACTION"}),
        json!({"type":"content","content":{"type":"text","text":"SINGLE_ACTION"}}),
    ] {
        let (state, id, rx, runtime) = running_kimi("ask");
        let mut request = read_permission();
        request["params"]["toolCall"]["content"] = content;
        deliver(&state, &id, &runtime, request, RUNTIME);
        assert_eq!(answered(&rx), None);
        let (message_id, detail) = session(&state, &id)
            .messages
            .into_iter()
            .find_map(|message| match message {
                Message::Approval { id, detail, .. } => Some((id, detail)),
                _ => None,
            })
            .unwrap();
        assert!(detail.contains("SINGLE_ACTION"));
        assert!(!detail.contains("\"type\""));
        state
            .update_approval(&id, &message_id, ApprovalDecision::Rejected)
            .unwrap();
        assert_eq!(
            answered(&rx),
            Some(json!({"outcome":"selected","optionId":"reject"}))
        );
        assert_eq!(answered(&rx), None);
    }
}

#[test]
fn kimi_read_only_missing_once_option_has_accurate_reason_and_one_delta() {
    let (state, id, rx, runtime) = read_only_kimi_fixture();
    let mut observations = KimiReadOnlyObservations::default();
    let _ = observations.observe(&json!({
        "sessionUpdate":"tool_call", "toolCallId":"c1", "title":"Bash",
        "content":[{"type":"content","content":{"type":"text", "text":"{\"command\":\"git status --short\"}"}}]
    }));
    let mut request = permission(
        "Bash",
        json!([
            {"optionId":"persistent","kind":"allow_always","name":"Always"},
            {"optionId":"reject","kind":"reject_once","name":"Reject"}
        ]),
    );
    request["params"]["toolCall"]["content"][0]["content"]["text"] =
        json!("Requesting approval to Running: git status --short");
    let mut events = state.subscribe_delta_events();
    assert!(
        state
            .answer_kimi_read_only_permission(
                &request,
                &id,
                &RuntimeToken::Acp(RUNTIME.to_owned()),
                &runtime.input_tx,
                &mut observations,
            )
            .unwrap()
    );
    assert_eq!(
        answered(&rx),
        Some(json!({"outcome":"selected","optionId":"reject"}))
    );
    assert_eq!(answered(&rx), None);
    assert_eq!(manual_cards(&state, &id), 0);
    let notices: Vec<_> = session(&state, &id)
        .messages
        .into_iter()
        .filter_map(|message| match message {
            Message::Text {
                id,
                author: Author::System,
                text,
                ..
            } => Some((id, text)),
            _ => None,
        })
        .collect();
    assert_eq!(notices.len(), 1);
    let (notice_id, text) = &notices[0];
    assert!(
        text.contains("requires a one-time grant") && text.contains("offers none"),
        "{text}"
    );
    assert!(text.contains("Retry with a one-time permission option"));
    assert!(!text.contains("choose a read-only operation"));
    assert!(text.chars().count() <= 2_000);
    let mut published = 0;
    while let Ok(payload) = events.try_recv() {
        if let DeltaEvent::MessageCreated {
            session_id,
            message_id,
            session_mutation_stamp: Some(_),
            ..
        } = serde_json::from_str(&payload).unwrap()
        {
            if message_id == *notice_id {
                assert_eq!(session_id, id);
                published += 1;
            }
        }
    }
    assert_eq!(published, 1);
}

#[test]
fn acp_manual_card_describes_media_resources_and_unknown_content_without_binary_data() {
    let (state, id, rx, runtime) = running_kimi("ask");
    let mut request = read_permission();
    request["params"]["toolCall"]["content"] = json!([
        {"type":"content","content":{"type":"image","mimeType":"image/png","data":"not-visible-image-bytes"}},
        {"type":"content","content":{"type":"audio","mimeType":"audio/wav","data":"not-visible-audio-bytes"}},
        {"type":"content","content":{"type":"resource","resource":{"uri":"file:///workspace/report.txt","text":"Report content","blob":"not-visible-resource-bytes"}}},
        {"type":"content","content":{"type":"resource_link","name":"Build report","uri":"file:///workspace/build.txt","description":"Read the build result"}},
        {"type":"terminal","terminalId":"terminal-7"},
        {"type":"future-content","privatePayload":"not-visible-unknown-bytes"}
    ]);
    deliver(&state, &id, &runtime, request, RUNTIME);
    assert_eq!(answered(&rx), None);
    let detail = session(&state, &id)
        .messages
        .into_iter()
        .find_map(|message| match message {
            Message::Approval { detail, .. } => Some(detail),
            _ => None,
        })
        .unwrap();
    for visible in [
        "image/png",
        "audio/wav",
        "file:///workspace/report.txt",
        "Report content",
        "Build report",
        "file:///workspace/build.txt",
        "Read the build result",
        "terminal-7",
        "future-content",
    ] {
        assert!(
            detail.contains(visible),
            "requested content remains understandable: {detail}"
        );
    }
    assert!(
        !detail.contains("not-visible-"),
        "binary and unsupported payloads are not serialized into the card"
    );
    assert!(!detail.contains("\"type\""));
}

#[test]
fn acp_manual_card_bounds_large_unicode_text_without_changing_the_decision() {
    let (state, id, rx, runtime) = running_kimi("ask");
    let mut request = read_permission();
    request["params"]["toolCall"]["content"][0]["content"]["text"] = json!("🦀".repeat(9_000));
    deliver(&state, &id, &runtime, request, RUNTIME);
    let (message_id, detail) = session(&state, &id)
        .messages
        .into_iter()
        .find_map(|message| match message {
            Message::Approval { id, detail, .. } => Some((id, detail)),
            _ => None,
        })
        .unwrap();
    assert!(detail.contains('🦀'));
    assert!(detail.chars().count() <= 8_002);
    assert!(
        detail.ends_with('…'),
        "the user can see that the preview is incomplete"
    );
    state
        .update_approval(&id, &message_id, ApprovalDecision::Rejected)
        .unwrap();
    assert_eq!(
        answered(&rx),
        Some(json!({"outcome":"selected","optionId":"reject"}))
    );
}

#[test]
fn acp_manual_cards_show_nested_text_for_all_acp_agents() {
    // The nested content comes from the recovered Kimi card. This exercises
    // the common ACP presentation contract, not a claimed wire capture from
    // each of the other providers.
    for (agent, acp_agent, settings) in [
        (
            Agent::Kimi,
            AcpAgent::Kimi,
            json!({"kimiApprovalMode":"ask"}),
        ),
        (Agent::Cursor, AcpAgent::Cursor, json!({"cursorMode":"ask"})),
        (
            Agent::Gemini,
            AcpAgent::Gemini,
            json!({"geminiApprovalMode":"default"}),
        ),
        (
            Agent::OpenCode,
            AcpAgent::OpenCode,
            json!({"opencodeApprovalMode":"ask"}),
        ),
    ] {
        let state = test_app_state();
        let mut create = json!({"agent":agent,"workdir":state.default_workdir});
        create
            .as_object_mut()
            .unwrap()
            .extend(settings.as_object().unwrap().clone());
        let id = state
            .create_session_with_agent_setup_validator(
                serde_json::from_value(create).unwrap(),
                |_, _| Ok(()),
            )
            .unwrap()
            .session_id;
        let (tx, rx) = mpsc::channel();
        let mut recorder = SessionRecorder::new(state.clone(), id.clone());
        let request = read_permission();
        let action = request["params"]["toolCall"]["content"][0]["content"]["text"]
            .as_str()
            .unwrap();
        handle_acp_request(&request, &state, &id, &tx, &mut recorder, acp_agent).unwrap();
        assert_eq!(answered(&rx), None);
        let detail = session(&state, &id)
            .messages
            .into_iter()
            .find_map(|message| match message {
                Message::Approval { detail, .. } => Some(detail),
                _ => None,
            })
            .expect("an ask-mode permission has a manual card");
        assert!(detail.contains(action), "{acp_agent:?}: {detail}");
        assert!(
            !detail.contains("\"content\"") && !detail.contains("\"type\""),
            "{acp_agent:?}: ACP wrappers must not appear as raw JSON: {detail}"
        );
    }
}
