//! Permission-envelope fixture follows kimi-code's pinned interaction-bridge
//! and question mapper; see the tracker for source revision and provenance.
use super::*;

fn captured_question() -> Value {
    serde_json::from_str(include_str!("../fixtures/kimi-question-permission.json")).unwrap()
}

fn question_card(state: &AppState, id: &str) -> (String, Vec<UserInputQuestion>) {
    session(state, id)
        .messages
        .into_iter()
        .find_map(|message| match message {
            Message::UserInputRequest {
                id,
                questions,
                state,
                declinable,
                ..
            } => {
                assert_eq!(state, InteractionRequestState::Pending);
                assert!(declinable);
                Some((id, questions))
            }
            _ => None,
        })
        .expect("Kimi choices must create a structured question, not an Approve card")
}

#[test]
fn kimi_question_returns_the_non_first_option_id() {
    let (state, id, rx, runtime) = running_kimi("auto-approve");
    deliver(&state, &id, &runtime, captured_question(), RUNTIME);
    assert_eq!(
        answered(&rx),
        None,
        "questions must await an explicit choice"
    );
    let (message_id, questions) = question_card(&state, &id);
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].question, "Which color should I use?");
    assert!(!questions[0].is_other);
    let options = serde_json::to_value(&questions[0].options).unwrap();
    assert_eq!(options[0]["label"], "Red");
    assert_eq!(options[1]["label"], "Blue");
    assert_eq!(options[1]["id"], "q0_opt_1");
    assert!(
        state
            .update_approval(&id, &message_id, ApprovalDecision::Accepted)
            .is_err()
    );
    assert_eq!(
        answered(&rx),
        None,
        "generic Approve must never pick a question answer"
    );
    state
        .submit_user_input(
            &id,
            &message_id,
            BTreeMap::from([(questions[0].id.clone(), vec!["q0_opt_1".to_owned()])]),
            false,
        )
        .unwrap();
    assert_eq!(
        answered(&rx),
        Some(json!({"outcome":"selected","optionId":"q0_opt_1"}))
    );
    assert!(
        state
            .submit_user_input(&id, &message_id, BTreeMap::new(), true)
            .is_err()
    );
    assert_eq!(
        answered(&rx),
        None,
        "a resolved question is delivered only once"
    );
}

#[test]
fn kimi_question_skip_returns_the_reject_option() {
    let (state, id, rx, runtime) = running_kimi("ask");
    deliver(&state, &id, &runtime, captured_question(), RUNTIME);
    let (message_id, _) = question_card(&state, &id);
    state
        .submit_user_input(&id, &message_id, BTreeMap::new(), true)
        .unwrap();
    assert_eq!(
        answered(&rx),
        Some(json!({"outcome":"selected","optionId":"q0_skip"}))
    );
    assert!(session(&state, &id).messages.iter().any(|message| matches!(
        message,
        Message::UserInputRequest {
            state: InteractionRequestState::Declined,
            ..
        }
    )));
}

#[test]
fn kimi_single_answer_question_and_ambiguous_other_title_stay_choices() {
    for single_answer in [false, true] {
        let (state, id, rx, runtime) = running_kimi("ask");
        let mut request = captured_question();
        if single_answer {
            request["params"]["options"]
                .as_array_mut()
                .unwrap()
                .remove(0);
        } else {
            request["params"]["toolCall"]["title"] = json!("ChooseColor");
        }
        deliver(&state, &id, &runtime, request, RUNTIME);
        let (message_id, questions) = question_card(&state, &id);
        state
            .submit_user_input(
                &id,
                &message_id,
                BTreeMap::from([(questions[0].id.clone(), vec!["q0_opt_1".to_owned()])]),
                false,
            )
            .unwrap();
        assert_eq!(
            answered(&rx),
            Some(json!({"outcome":"selected","optionId":"q0_opt_1"}))
        );
    }
}

#[test]
fn kimi_ordinary_single_allow_once_permission_keeps_approve() {
    let (state, id, rx, runtime) = running_kimi("ask");
    deliver(
        &state,
        &id,
        &runtime,
        permission("Bash", tool_options()),
        RUNTIME,
    );
    let message_id = session(&state, &id)
        .messages
        .into_iter()
        .find_map(|message| match message {
            Message::Approval { id, .. } => Some(id),
            _ => None,
        })
        .expect("ordinary tool permission should retain its approval card");
    state
        .update_approval(&id, &message_id, ApprovalDecision::Accepted)
        .unwrap();
    assert_eq!(
        answered(&rx),
        Some(json!({"outcome":"selected","optionId":"approve_once"}))
    );
}

#[test]
fn kimi_question_validates_opaque_answers_and_keeps_the_live_claim_on_failure() {
    let (state, id, rx, runtime) = running_kimi("ask");
    deliver(&state, &id, &runtime, captured_question(), RUNTIME);
    let (message_id, questions) = question_card(&state, &id);
    for values in [
        vec!["Blue"],
        vec!["not-offered"],
        vec!["q0_skip"],
        vec!["q0_opt_0", "q0_opt_1"],
        vec![],
    ] {
        let answers = BTreeMap::from([(
            questions[0].id.clone(),
            values.into_iter().map(str::to_owned).collect(),
        )]);
        assert!(
            state
                .submit_user_input(&id, &message_id, answers, false)
                .is_err()
        );
        assert_eq!(answered(&rx), None);
        assert_eq!(question_card(&state, &id).0, message_id);
    }
    drop(rx);
    assert!(
        state
            .submit_user_input(
                &id,
                &message_id,
                BTreeMap::from([(questions[0].id.clone(), vec!["q0_opt_1".to_owned()])]),
                false
            )
            .is_err()
    );
    assert_eq!(question_card(&state, &id).0, message_id);
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
    assert!(record.pending_acp_approvals.contains_key(&message_id));
    assert_eq!(record.pending_acp_approval_order.front(), Some(&message_id));
}

#[test]
fn kimi_question_equal_labels_round_trip_distinct_ids_and_request_id() {
    let (state, id, rx, runtime) = running_kimi("ask");
    let mut request = captured_question();
    request["params"]["options"][1]["name"] = json!("Red");
    request["params"]["options"][1]["optionId"] = json!("opaque:second answer");
    deliver(&state, &id, &runtime, request, RUNTIME);
    let (message_id, questions) = question_card(&state, &id);
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
        let digest = project_digest_session_from_record(record);
        assert!(digest.pending_approval_message_id.is_none());
        assert_eq!(
            digest.pending_interaction_message_id.as_deref(),
            Some(message_id.as_str())
        );
    }
    state
        .submit_user_input(
            &id,
            &message_id,
            BTreeMap::from([(
                questions[0].id.clone(),
                vec!["opaque:second answer".to_owned()],
            )]),
            false,
        )
        .unwrap();
    let response = rx
        .try_iter()
        .find_map(|command| match command {
            AcpRuntimeCommand::JsonRpcMessage(value) => Some(value),
            _ => None,
        })
        .unwrap();
    assert_eq!(response["id"], 42);
    assert_eq!(
        response["result"]["outcome"],
        json!({"outcome":"selected","optionId":"opaque:second answer"})
    );
}

#[test]
fn kimi_question_without_reject_dismisses_as_cancelled() {
    let (state, id, rx, runtime) = running_kimi("ask");
    let mut request = captured_question();
    request["params"]["options"].as_array_mut().unwrap().pop();
    deliver(&state, &id, &runtime, request, RUNTIME);
    let (message_id, _) = question_card(&state, &id);
    state
        .submit_user_input(&id, &message_id, BTreeMap::new(), true)
        .unwrap();
    assert_eq!(answered(&rx), Some(json!({"outcome":"cancelled"})));
}

#[test]
fn kimi_malformed_choices_never_fall_back_to_generic_approve() {
    for duplicate_id in [false, true] {
        let (state, id, rx, runtime) = running_kimi("ask");
        let mut request = captured_question();
        if duplicate_id {
            request["params"]["options"][1]["optionId"] = json!("q0_skip");
        } else {
            request["params"]["options"][1]["name"] = Value::Null;
        }
        deliver(&state, &id, &runtime, request, RUNTIME);
        assert_eq!(answered(&rx), Some(json!({"outcome":"cancelled"})));
        assert_eq!(manual_cards(&state, &id), 0);
    }
}

#[test]
fn kimi_question_cannot_be_answered_after_stop_or_pending_request_cleanup() {
    let (state, id, rx, runtime) = running_kimi("ask");
    deliver(&state, &id, &runtime, captured_question(), RUNTIME);
    let (message_id, _) = question_card(&state, &id);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        inner.sessions[index].runtime_stop_in_progress = true;
    }
    assert!(
        state
            .submit_user_input(&id, &message_id, BTreeMap::new(), true)
            .is_err()
    );
    assert_eq!(answered(&rx), None);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        clear_all_pending_requests(&mut inner.sessions[index]);
        inner.sessions[index].runtime_stop_in_progress = false;
    }
    assert!(
        state
            .submit_user_input(&id, &message_id, BTreeMap::new(), true)
            .is_err()
    );
    assert_eq!(answered(&rx), None);
}
