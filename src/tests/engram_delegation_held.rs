// Owns the tests of held and not-yet-started delegation attempts
// (src/delegation_attempt_state.rs): a spawn or follow-up whose turn is held
// returns its durable delegation with the hold instead of an error, every
// projection reports `held` or `queued` instead of `running`, a hold wakes a
// parent's wait once as an attention notification without counting as
// terminal, a safe resume releases it, and holds survive a restart. Does not
// own the retained prompt states themselves (src/tests/engram_post_receipt.rs,
// src/tests/engram_retained_disposition.rs, src/tests/engram_abort_retry.rs)
// or ordinary wait fan-in (src/tests/delegation_wait.rs). New module, a child
// of the root dispatch tests whose fixtures it uses.
use super::abort_retry::{ScriptedPersister, await_settlement_acknowledgement, prompts_received};
use super::*;
use crate::tests::delegation_support::{
    finish_delegation_child_with_assistant_text, install_delegation_codex_runtime,
    mark_delegation_as_unstructured_explorer, temp_delegation_state_paths,
    test_app_state_with_drained_delegation_codex_runtime,
};

fn explorer_request(prompt: &str, title: &str) -> CreateDelegationRequest {
    CreateDelegationRequest {
        prompt: prompt.to_owned(),
        title: Some(title.to_owned()),
        cwd: None,
        agent: Some(Agent::Codex),
        model: None,
        mode: Some(DelegationMode::Explorer),
        write_policy: Some(DelegationWritePolicy::ReadOnly),
    }
}

fn status_json(state: &AppState, parent: &str, delegation: &str) -> Value {
    serde_json::to_value(state.get_delegation(parent, delegation).unwrap()).unwrap()["delegation"]
        .clone()
}

fn stored_delegation(state: &AppState, delegation: &str) -> DelegationRecord {
    let inner = state.inner.lock().unwrap();
    inner.delegations[inner.find_delegation_index(delegation).unwrap()].clone()
}

/// The parent prompts that name `wait_id`, by message id: a promoted prompt
/// can sit both in the queue and in the transcript.
fn wait_notifications(state: &AppState, parent: &str, wait_id: &str) -> BTreeMap<String, String> {
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(parent).unwrap()];
    record
        .queued_prompts
        .iter()
        .map(|queued| {
            (
                queued.pending_prompt.id.clone(),
                queued.pending_prompt.text.clone(),
            )
        })
        .chain(
            record
                .session
                .messages
                .iter()
                .filter_map(|message| match message {
                    Message::Text {
                        author: Author::You,
                        id,
                        text,
                        ..
                    } => Some((id.clone(), text.clone())),
                    _ => None,
                }),
        )
        .filter(|(_, text)| text.contains(wait_id))
        .collect()
}

fn delta_events(receiver: &mut broadcast::Receiver<String>) -> Vec<Value> {
    let mut events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push(serde_json::from_str(&event).unwrap());
    }
    events
}

/// Puts `child` into the shape a held delegation child has: idle, its queue
/// paused behind an Engram-retained prompt (`waiting` for a parked admission,
/// `interrupted` for an interrupted one). Does not refresh the delegation.
fn hold_child_without_refresh(state: &AppState, child: &str, waiting: bool, interrupted: bool) {
    let mut inner = state.inner.lock().unwrap();
    let message_id = inner.next_message_id();
    let index = inner.find_session_index(child).unwrap();
    let record = inner.session_mut_by_index(index).unwrap();
    queue_prompt_on_record_with_source(
        record,
        PendingPrompt {
            engram_interrupted: false,
            is_engram_retained: false,
            attachments: Vec::new(),
            id: message_id,
            timestamp: stamp_now(),
            text: "retained child prompt".to_owned(),
            expanded_text: None,
            source: None,
        },
        Vec::new(),
        QueuedPromptSource::Orchestrator,
    );
    let head = record.queued_prompts.front_mut().unwrap();
    head.engram_waiting = waiting;
    head.engram_interrupted = interrupted;
    record.session.status = SessionStatus::Idle;
    record.session.live_activity = None;
    record.session.preview =
        "Engram: Waiting/Unknown. Original prompt retained; resume to retry or cancel.".to_owned();
    record.set_auto_dispatch_blocked(true);
    sync_pending_prompts(record);
    state.commit_locked(&mut inner).unwrap();
}

fn hold_child(state: &AppState, child: &str, waiting: bool, interrupted: bool) {
    hold_child_without_refresh(state, child, waiting, interrupted);
    state.sync_delegation_attempt_for_child_session(child);
}

/// Releases a manually held child back to an executing turn.
fn release_child(state: &AppState, child: &str) {
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(child).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.queued_prompts.clear();
        record.set_auto_dispatch_blocked(false);
        record.session.status = SessionStatus::Active;
        sync_pending_prompts(record);
        state.commit_locked(&mut inner).unwrap();
    }
    state.sync_delegation_attempt_for_child_session(child);
}

/// Criteria 1, 2 and 4: a spawn whose first delivery is withheld after its
/// grant began, for a slow admission fence, answers with its durable
/// delegation, held for an unconfirmed settlement (202, `firstTurn` held, no
/// error). Once the settlement is acknowledged the hold reads as a scheduled
/// retry; the retry delivers the same prompt in the same child of the same
/// delegation, which then reports running and nothing else.
#[test]
fn a_held_first_turn_returns_its_durable_delegation_and_retries_in_the_same_child() {
    let (mut state, parent, receiver, _) = root_fixture([]);
    let persister = ScriptedPersister::install(&mut state);
    let (begin, begin_gate) = gated_engram_step("turn_begin", begin_reply("held-grant-1"));
    let transport = GatedEngramControlTransport::new([
        immediate_engram_step("session_bind", bind_reply("held-parent")),
        immediate_engram_step("session_bind", bind_reply("held-child")),
        immediate_engram_step("turn_evaluate", grant_reply("held-grant-1")),
        begin,
        immediate_engram_step("turn_checkpoint", checkpoint_reply("held-grant-1")),
        immediate_engram_step("session_status", status_reply("ready")),
        immediate_engram_step("session_bind", bind_reply("held-child-2")),
        immediate_engram_step("turn_evaluate", grant_reply("held-grant-2")),
        immediate_engram_step("turn_begin", begin_reply("held-grant-2")),
    ]);
    state.install_control_test_transport(transport.clone());

    let response = std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            state.create_read_only_delegation(&parent, explorer_request("held spawn", "Held"))
        });
        begin_gate.wait();
        persister.fail_next(1);
        begin_gate.release();
        worker.join().unwrap()
    })
    .expect("a held first turn is not an error");
    assert!(receiver.try_recv().is_err(), "nothing reached the provider");
    assert_eq!(
        delegation_turn_response_status(&response),
        StatusCode::ACCEPTED
    );
    let delegation = response.delegation.id.clone();
    let child = response.delegation.child_session_id.clone();
    let wire = serde_json::to_value(&response).unwrap();
    assert_eq!(wire["delegation"]["status"], "held");
    assert_eq!(wire["firstTurn"]["state"], "held");
    assert_eq!(wire["firstTurn"]["hold"]["reason"], "persistenceUnknown");
    assert!(
        wire["firstTurn"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("persistence is unknown"))
    );
    assert_eq!(wire["delegation"]["hold"]["actions"], json!(["cancel"]));
    assert_eq!(wire["delegation"]["hold"]["retryEligible"], false);
    assert!(wire["delegation"].get("completedAt").is_none());
    assert!(wire["delegation"].get("result").is_none());
    {
        let inner = state.inner.lock().unwrap();
        assert_eq!(inner.delegations.len(), 1, "one delegation, no duplicate");
        assert_eq!(inner.delegations[0].id, delegation);
    }
    let first = status_json(&state, &parent, &delegation);
    assert_eq!(first["status"], "held");
    let held_since = first["hold"]["heldSince"].as_str().unwrap().to_owned();
    assert!(chrono::DateTime::parse_from_rfc3339(&held_since).is_ok());
    assert!(
        chrono::DateTime::parse_from_rfc3339(first["hold"]["lastActivityAt"].as_str().unwrap())
            .is_ok()
    );
    let first_generation = first["hold"]["generation"].as_u64().unwrap();
    // A status read moves nothing.
    assert_eq!(
        status_json(&state, &parent, &delegation)["hold"],
        first["hold"]
    );

    // The acknowledgement turns the hold into a scheduled retry: a new hold
    // generation, the same held-since time, resume now offered.
    let mut events = state.subscribe_delta_events();
    await_settlement_acknowledgement(&state, &child);
    state.engram_abort_retry_tick(chrono::Utc::now());
    let scheduled = status_json(&state, &parent, &delegation);
    assert_eq!(scheduled["status"], "held");
    assert_eq!(scheduled["hold"]["reason"], "retryScheduled");
    assert_eq!(scheduled["hold"]["heldSince"], held_since.as_str());
    assert_eq!(
        scheduled["hold"]["generation"].as_u64().unwrap(),
        first_generation + 1
    );
    assert_eq!(scheduled["hold"]["retryEligible"], true);
    assert_eq!(scheduled["hold"]["actions"], json!(["resume", "cancel"]));
    let due_at = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&child).unwrap()]
            .engram
            .abort_retry
            .as_ref()
            .unwrap()
            .due_at
            .clone()
    };
    assert_eq!(scheduled["hold"]["nextRetryAt"], due_at.as_str());
    // The live update carries the changed hold itself, not only the status.
    let updates = delta_events(&mut events)
        .into_iter()
        .filter(|event| {
            event["type"] == "delegationUpdated" && event["delegationId"] == delegation.as_str()
        })
        .collect::<Vec<_>>();
    let changed = updates.last().expect("the hold change is announced");
    assert_eq!(changed["status"], "held");
    assert_eq!(changed["hold"]["reason"], "retryScheduled");
    assert_eq!(changed["hold"]["generation"], first_generation + 1);
    assert_eq!(changed["hold"]["actions"], json!(["resume", "cancel"]));
    let listed = state.list_delegations(&parent).unwrap();
    assert_eq!(listed.delegations.len(), 1);
    assert_eq!(listed.delegations[0].status, DelegationStatus::Held);
    assert_eq!(
        listed.delegations[0].hold.as_ref().unwrap().reason,
        DelegationHoldReason::RetryScheduled
    );

    // Due: the same prompt is admitted afresh in the same child, once.
    let mut events = state.subscribe_delta_events();
    state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(10));
    assert_eq!(prompts_received(&receiver), 1);
    let released = delta_events(&mut events)
        .into_iter()
        .filter(|event| {
            event["type"] == "delegationUpdated" && event["delegationId"] == delegation.as_str()
        })
        .collect::<Vec<_>>();
    let last = released.last().expect("the release is announced");
    assert_eq!(last["status"], "running");
    assert!(last["hold"].is_null(), "a released hold is cleared: {last}");
    assert!(
        released.iter().all(|event| event["status"] != "held"),
        "the retried admission is never reported held: {released:?}"
    );
    let running = status_json(&state, &parent, &delegation);
    assert_eq!(running["status"], "running");
    assert!(running.get("hold").is_none());
    assert_eq!(running["childSessionId"], child.as_str());
    let listed = state.list_delegations(&parent).unwrap();
    assert_eq!(listed.delegations.len(), 1);
    assert_eq!(listed.delegations[0].id, delegation);
    assert_eq!(listed.delegations[0].status, DelegationStatus::Running);
    persister.stop(&state);
}

/// Criterion 1, the other side: a delegation that cannot be saved at all is
/// an error, not a hold. It names the delegation and child it leaves in
/// memory, says its durability is unknown, and starts no turn.
#[test]
fn a_creation_that_cannot_be_saved_names_its_delegation_and_starts_no_turn() {
    let (mut state, parent, receiver, _) = root_fixture([]);
    state.shutdown_persist_blocking();
    let failing_path = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("creation-commit-is-directory");
    fs::create_dir_all(&failing_path).unwrap();
    state.persistence_path = Arc::new(failing_path.clone());

    let error = match state.create_read_only_delegation(&parent, explorer_request("unsaved", "U")) {
        Ok(_) => panic!("an unsaved creation must not be reported as created"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!error.message.contains("persistence is unknown"));
    let inner = state.inner.lock().unwrap();
    assert_eq!(inner.delegations.len(), 1);
    let delegation = &inner.delegations[0];
    assert!(error.message.contains(&delegation.id), "{}", error.message);
    assert!(
        error.message.contains(&delegation.child_session_id),
        "{}",
        error.message
    );
    assert!(error.message.contains("termal_list_delegations"));
    assert!(error.message.contains("first turn was not started"));
    let child = &inner.sessions[inner
        .find_session_index(&delegation.child_session_id)
        .unwrap()];
    assert!(child.queued_prompts.is_empty());
    assert!(child.session.messages.is_empty());
    drop(inner);
    assert!(receiver.try_recv().is_err());
    fs::remove_dir_all(failing_path).unwrap();
}

/// Criteria 2 and 4: a follow-up whose admission Engram deferred is held,
/// not running, and the previous attempt's result never completes it. A safe
/// resume releases it to running; a wait registered after that completes on
/// the follow-up's own result, not on the earlier one.
#[test]
fn a_deferred_follow_up_is_held_ignores_the_previous_result_and_resumes_to_its_own_result() {
    let (state, parent, receiver, _) = root_fixture([
        bind_reply("parent"),
        bind_reply("child"),
        grant_reply("first"),
        begin_reply("first"),
        checkpoint_reply("first"),
        defer_reply("busy"),
        grant_reply("retry"),
        begin_reply("retry"),
    ]);
    let created = state
        .create_read_only_delegation(&parent, explorer_request("initial", "Follow-up hold"))
        .unwrap();
    assert_eq!(
        created.first_turn.as_ref().unwrap().state,
        DelegationTurnDeliveryState::Delivered
    );
    let delegation = created.delegation.id;
    let child = created.delegation.child_session_id;
    assert!(matches!(
        receive_synchronous_engram_prompt(&state, &receiver, "initial prompt").unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    finish_child_turn(
        &state,
        &child,
        "## Result\nStatus: completed\n\nSummary:\ninitial completed",
    );
    assert_eq!(
        stored_delegation(&state, &delegation).status,
        DelegationStatus::Completed
    );

    let followed = state
        .followup_delegation(&parent, &delegation, "follow-up prompt".into())
        .expect("a deferred follow-up is not an error");
    let turn = followed.turn.clone().unwrap();
    assert_eq!(turn.state, DelegationTurnDeliveryState::Held);
    let hold = turn.hold.unwrap();
    assert_eq!(hold.reason, DelegationHoldReason::AdmissionDeferred);
    assert!(hold.retry_eligible);
    assert_eq!(
        hold.actions,
        vec![DelegationHoldAction::Resume, DelegationHoldAction::Cancel]
    );
    for _ in 0..2 {
        let wire = status_json(&state, &parent, &delegation);
        assert_eq!(wire["status"], "held");
        assert!(wire.get("result").is_none(), "no earlier result");
        assert!(wire.get("completedAt").is_none());
        assert!(state.get_delegation_result(&parent, &delegation).is_err());
        state.refresh_delegation_for_child_session(&child).unwrap();
    }
    assert!(receiver.try_recv().is_err());

    let resumed = state.resume_delegation(&parent, &delegation).unwrap();
    assert_eq!(
        resumed.turn.as_ref().unwrap().state,
        DelegationTurnDeliveryState::Delivered
    );
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let wire = status_json(&state, &parent, &delegation);
    assert_eq!(wire["status"], "running");
    assert!(wire.get("hold").is_none());

    let wait = state
        .create_delegation_wait(
            &parent,
            CreateDelegationWaitRequest {
                delegation_ids: vec![delegation.clone()],
                mode: DelegationWaitMode::All,
                title: Some("Follow-up result".to_owned()),
            },
        )
        .unwrap();
    assert!(
        !wait.resume_prompt_queued,
        "a running child does not wake it"
    );
    finish_child_turn(
        &state,
        &child,
        "## Result\nStatus: completed\n\nSummary:\nfollow-up completed",
    );
    let notifications = wait_notifications(&state, &parent, &wait.wait.id);
    assert_eq!(notifications.len(), 1);
    let text = notifications.values().next().unwrap();
    assert!(text.contains("Wait outcome: `completed`"));
    assert!(text.contains("follow-up completed"));
    assert!(!text.contains("initial completed"));
}

fn finish_child_turn(state: &AppState, child: &str, text: &str) {
    let token = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(child).unwrap()]
            .runtime
            .runtime_token()
            .unwrap()
    };
    finish_delegation_child_with_assistant_text(state, child, text);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(child).unwrap();
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    state
        .finish_turn_ok_if_runtime_matches(child, &token)
        .unwrap();
    state.refresh_delegation_for_child_session(child).unwrap();
}

/// Criterion 2: a turn whose Engram admission is still running is queued,
/// not executing, and reports running only once its provider has it.
#[test]
fn an_admitting_first_turn_reports_queued_until_its_provider_runs() {
    let (state, parent, receiver, _) = root_fixture([]);
    let (evaluate, evaluate_gate) =
        gated_engram_step("turn_evaluate", grant_reply("admitting-grant"));
    state.install_control_test_transport(GatedEngramControlTransport::new([
        immediate_engram_step("session_bind", bind_reply("admitting-parent")),
        immediate_engram_step("session_bind", bind_reply("admitting-child")),
        evaluate,
        immediate_engram_step("turn_begin", begin_reply("admitting-grant")),
    ]));
    // Read while the admission waits, and assert only after releasing it, so
    // a failed assertion cannot leave the admitting worker parked.
    let (response, during, listed, delivered_early) = std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            state.create_read_only_delegation(&parent, explorer_request("admitting", "Admitting"))
        });
        evaluate_gate.wait();
        let delegation = state.inner.lock().unwrap().delegations[0].id.clone();
        let during = state
            .get_delegation(&parent, &delegation)
            .map(|response| serde_json::to_value(response).unwrap()["delegation"].clone());
        let listed = state
            .list_delegations(&parent)
            .map(|list| list.delegations[0].status);
        let delivered_early = receiver.try_recv().is_ok();
        evaluate_gate.release();
        (worker.join().unwrap(), during, listed, delivered_early)
    });
    let response = response.unwrap();
    let during = during.unwrap();
    assert_eq!(during["status"], "queued", "admitting is not executing");
    assert!(during.get("hold").is_none());
    assert_eq!(listed.unwrap(), DelegationStatus::Queued);
    assert!(!delivered_early);
    assert_eq!(
        response.first_turn.as_ref().unwrap().state,
        DelegationTurnDeliveryState::Delivered
    );
    assert_eq!(
        delegation_turn_response_status(&response),
        StatusCode::CREATED
    );
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert_eq!(
        status_json(&state, &parent, &response.delegation.id)["status"],
        "running"
    );
}

fn two_explorers(state: &AppState) -> (String, String, String, String, String) {
    let parent = test_session_id(state, Agent::Codex);
    let first = state
        .create_read_only_delegation(&parent, explorer_request("first child", "Held child"))
        .unwrap();
    let second = state
        .create_read_only_delegation(&parent, explorer_request("second child", "Done child"))
        .unwrap();
    mark_delegation_as_unstructured_explorer(state, &first.delegation.id);
    mark_delegation_as_unstructured_explorer(state, &second.delegation.id);
    (
        parent,
        first.delegation.id,
        first.delegation.child_session_id,
        second.delegation.id,
        second.delegation.child_session_id,
    )
}

fn wait_for(
    state: &AppState,
    parent: &str,
    ids: &[&str],
    mode: DelegationWaitMode,
) -> DelegationWaitResponse {
    state
        .create_delegation_wait(
            parent,
            CreateDelegationWaitRequest {
                delegation_ids: ids.iter().map(|id| (*id).to_owned()).collect(),
                mode,
                title: None,
            },
        )
        .unwrap()
}

/// Criterion 3: an all-mode wait over a held child and a running sibling
/// wakes once both need attention, as one durable notification that says
/// attention is required, names the hold and its actions, and carries the
/// sibling's result. Duplicate refreshes add nothing; the held child stays
/// non-terminal with no result; an any-mode wait wakes on the hold alone; a
/// new wait over the unchanged hold reports it once and nothing re-arms; a
/// hold that cleared before a wait was judged does not wake it.
#[test]
fn held_children_wake_waits_once_with_attention_and_never_count_as_terminal() {
    let state = test_app_state_with_drained_delegation_codex_runtime("held-wait");
    let (parent, held, held_child, done, done_child) = two_explorers(&state);
    let all = wait_for(&state, &parent, &[&held, &done], DelegationWaitMode::All);
    assert!(!all.resume_prompt_queued);

    hold_child(&state, &held_child, true, false);
    assert_eq!(status_json(&state, &parent, &held)["status"], "held");
    assert!(
        wait_notifications(&state, &parent, &all.wait.id).is_empty(),
        "the running sibling keeps an all-mode wait"
    );

    let mut events = state.subscribe_delta_events();
    finish_delegation_child_with_assistant_text(
        &state,
        &done_child,
        "## Result\n\nStatus: completed\n\nSummary:\nThe sibling is clean.",
    );
    state
        .refresh_delegation_for_child_session(&done_child)
        .unwrap();
    let notifications = wait_notifications(&state, &parent, &all.wait.id);
    assert_eq!(notifications.len(), 1, "one notification");
    let text = notifications.values().next().unwrap();
    assert!(text.contains("Wait outcome: `attentionRequired`"));
    assert!(text.contains("Status: held (not completed)"));
    assert!(text.contains("Reason: admissionDeferred"));
    assert!(text.contains("Supported actions: resume, cancel"));
    assert!(text.contains("Retry eligible: true"));
    assert!(text.contains("The sibling is clean."));
    assert!(delta_events(&mut events).iter().any(|event| {
        event["type"] == "delegationWaitConsumed"
            && event["waitId"] == all.wait.id.as_str()
            && event["reason"] == "attentionRequired"
    }));
    let held_record = stored_delegation(&state, &held);
    assert!(held_record.completed_at.is_none());
    assert!(held_record.result.is_none());
    assert!(!delegation_is_terminal(public_delegation_status(
        &held_record
    )));
    assert!(state.get_delegation_result(&parent, &held).is_err());

    // Duplicate callbacks and polls deliver nothing more.
    state.sync_delegation_attempt_for_child_session(&held_child);
    state.sync_delegation_attempt_for_child_session(&held_child);
    let _ = state.get_delegation(&parent, &held).unwrap();
    let _ = state.get_delegation(&parent, &held).unwrap();
    state
        .refresh_delegation_for_child_session(&held_child)
        .unwrap();
    assert_eq!(wait_notifications(&state, &parent, &all.wait.id).len(), 1);

    // An any-mode wait over the held child alone reports it once, at once;
    // another new wait reports the unchanged hold once too, and refreshes
    // re-arm neither.
    let any = wait_for(&state, &parent, &[&held], DelegationWaitMode::Any);
    assert!(any.resume_prompt_queued);
    let again = wait_for(&state, &parent, &[&held], DelegationWaitMode::Any);
    assert!(again.resume_prompt_queued);
    state.sync_delegation_attempt_for_child_session(&held_child);
    let _ = state.get_delegation(&parent, &held).unwrap();
    for wait in [&any, &again] {
        let notifications = wait_notifications(&state, &parent, &wait.wait.id);
        assert_eq!(notifications.len(), 1);
        assert!(
            notifications
                .values()
                .all(|text| text.contains("Wait outcome: `attentionRequired`"))
        );
    }
    assert!(state.inner.lock().unwrap().delegation_waits.is_empty());

    // A hold that cleared before the wait is judged does not wake it.
    release_child(&state, &held_child);
    assert_eq!(status_json(&state, &parent, &held)["status"], "running");
    let after = wait_for(&state, &parent, &[&held], DelegationWaitMode::Any);
    assert!(!after.resume_prompt_queued);
    assert!(wait_notifications(&state, &parent, &after.wait.id).is_empty());
    assert_eq!(state.inner.lock().unwrap().delegation_waits.len(), 1);
}

/// Criterion 3, a paused parent: the attention notification is queued and
/// waits behind the pause rather than starting a turn.
#[test]
fn a_paused_parent_keeps_the_attention_notification_queued() {
    let state = test_app_state_with_drained_delegation_codex_runtime("held-wait-paused");
    let (parent, held, held_child, _, _) = two_explorers(&state);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&parent).unwrap();
        inner.sessions[index].set_auto_dispatch_blocked(true);
        state.commit_locked(&mut inner).unwrap();
    }
    hold_child(&state, &held_child, true, false);
    let wait = wait_for(&state, &parent, &[&held], DelegationWaitMode::All);
    assert!(wait.resume_prompt_queued);
    assert!(!wait.resume_dispatch_requested);
    let inner = state.inner.lock().unwrap();
    let parent_record = &inner.sessions[inner.find_session_index(&parent).unwrap()];
    assert!(parent_record.queued_prompts.iter().any(|queued| {
        queued
            .pending_prompt
            .text
            .contains("Wait outcome: `attentionRequired`")
    }));
}

/// Criteria 2 and 3 across a restart: a child held while its wait was
/// pending, before any refresh announced it, is read as held at boot; the
/// wait then wakes once, and booting again neither repeats it nor changes
/// the hold that was saved.
#[test]
fn a_hold_and_its_wait_survive_a_restart_and_wake_once() {
    let (_temp_root, project_root, persistence_path, templates_path) =
        temp_delegation_state_paths();
    let boot = || {
        AppState::new_with_paths(
            project_root.to_string_lossy().into_owned(),
            persistence_path.clone(),
            templates_path.clone(),
        )
        .expect("state should boot")
    };
    let (parent, delegation, wait_id) = {
        let state = boot();
        install_delegation_codex_runtime(&state, "held-restart-runtime");
        let parent = test_session_id(&state, Agent::Codex);
        let created = state
            .create_read_only_delegation(&parent, explorer_request("restart", "Restart hold"))
            .unwrap();
        mark_delegation_as_unstructured_explorer(&state, &created.delegation.id);
        let wait = wait_for(
            &state,
            &parent,
            &[&created.delegation.id],
            DelegationWaitMode::All,
        );
        {
            // Keep the recovered notification queued: no runtime after boot.
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&parent).unwrap();
            inner.sessions[index].set_auto_dispatch_blocked(true);
            state.commit_locked(&mut inner).unwrap();
        }
        hold_child_without_refresh(&state, &created.delegation.child_session_id, true, false);
        state.shutdown_persist_blocking();
        (parent, created.delegation.id, wait.wait.id)
    };

    let hold = {
        let restarted = boot();
        let record = stored_delegation(&restarted, &delegation);
        assert_eq!(record.status, DelegationStatus::Running);
        assert_eq!(public_delegation_status(&record), DelegationStatus::Held);
        assert_eq!(wait_notifications(&restarted, &parent, &wait_id).len(), 1);
        assert!(
            wait_notifications(&restarted, &parent, &wait_id)
                .values()
                .all(|text| text.contains("Wait outcome: `attentionRequired`"))
        );
        restarted.reconcile_delegation_waits_after_boot().unwrap();
        assert_eq!(wait_notifications(&restarted, &parent, &wait_id).len(), 1);
        restarted.shutdown_persist_blocking();
        record.attempt.hold.unwrap()
    };

    let again = boot();
    let record = stored_delegation(&again, &delegation);
    assert_eq!(public_delegation_status(&record), DelegationStatus::Held);
    let reloaded = record.attempt.hold.unwrap();
    assert_eq!(reloaded.held_since, hold.held_since);
    assert_eq!(reloaded.prompt_id, hold.prompt_id);
    assert_eq!(wait_notifications(&again, &parent, &wait_id).len(), 1);
    // Join the third boot's persist worker, which keeps the database open,
    // before the guarded root is removed.
    again.shutdown_persist_blocking();
}

/// Criterion 4: a hold that cannot be resumed safely (an interrupted
/// authorization, a stop) offers only cancellation, refuses Resume, and its
/// cancellation is a real one that clears the hold. A delegation that is not
/// held has nothing to resume.
#[test]
fn an_unsafe_hold_offers_only_cancellation_and_refuses_resume() {
    let state = test_app_state_with_drained_delegation_codex_runtime("held-unsafe");
    let (parent, held, held_child, running, _) = two_explorers(&state);
    let refused = state
        .resume_delegation(&parent, &running)
        .err()
        .expect("nothing to resume");
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert!(refused.message.contains("not held"));

    hold_child(&state, &held_child, false, true);
    let hold = stored_delegation(&state, &held).attempt.hold.unwrap();
    assert_eq!(hold.reason, DelegationHoldReason::DeliveryUnknown);
    assert_eq!(hold.actions, vec![DelegationHoldAction::Cancel]);
    assert!(!hold.retry_eligible);
    let refused = state
        .resume_delegation(&parent, &held)
        .err()
        .expect("an unsafe hold refuses resume");
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert!(refused.message.contains("does not offer resume"));
    assert!(refused.message.contains("deliveryUnknown"));

    // The same head stopped by the user reads as stopped, still cancel-only.
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&held_child).unwrap();
        let head = inner.sessions[index].queued_prompts[0]
            .pending_prompt
            .id
            .clone();
        inner.sessions[index].engram.stopped_prompt_id = Some(head);
        state.commit_locked(&mut inner).unwrap();
    }
    state.sync_delegation_attempt_for_child_session(&held_child);
    let stopped = stored_delegation(&state, &held).attempt.hold.unwrap();
    assert_eq!(stopped.reason, DelegationHoldReason::Stopped);
    assert_eq!(stopped.generation, hold.generation + 1);
    assert_eq!(stopped.held_since, hold.held_since);
    assert!(state.resume_delegation(&parent, &held).is_err());

    let canceled = state.cancel_delegation(&parent, &held).unwrap();
    assert_eq!(canceled.delegation.status, DelegationStatus::Canceled);
    assert!(canceled.delegation.attempt.hold.is_none());
    let summary = state
        .list_delegations(&parent)
        .unwrap()
        .delegations
        .into_iter()
        .find(|summary| summary.id == held)
        .unwrap();
    assert_eq!(summary.status, DelegationStatus::Canceled);
    assert!(summary.hold.is_none());
}

/// Review regression (an in-flight resume read as held): while a resumed
/// admission of a deferred first turn waits on its evaluation, with the
/// paused latch and the retained head still in place, the delegation is
/// queued, not held, and a wait judged then is not woken for attention. It
/// runs once the provider has it.
#[test]
fn a_resumed_admission_in_flight_is_queued_and_wakes_no_wait() {
    let (state, parent, receiver, _) = root_fixture([]);
    let (evaluate, evaluate_gate) = gated_engram_step("turn_evaluate", grant_reply("resumed"));
    state.install_control_test_transport(GatedEngramControlTransport::new([
        immediate_engram_step("session_bind", bind_reply("resume-parent")),
        immediate_engram_step("session_bind", bind_reply("resume-child")),
        immediate_engram_step("turn_evaluate", defer_reply("busy")),
        evaluate,
        immediate_engram_step("turn_begin", begin_reply("resumed")),
    ]));
    let created = state
        .create_read_only_delegation(&parent, explorer_request("deferred", "Resumed"))
        .unwrap();
    assert_eq!(
        created.first_turn.as_ref().unwrap().state,
        DelegationTurnDeliveryState::Held
    );
    let delegation = created.delegation.id;
    assert_eq!(status_json(&state, &parent, &delegation)["status"], "held");

    // Read and wait while the resumed admission is in flight; assert after
    // releasing it, so a failed assertion cannot park the worker.
    let (resumed, during, wait, delivered_early) = std::thread::scope(|scope| {
        let worker = scope.spawn(|| state.resume_delegation(&parent, &delegation));
        evaluate_gate.wait();
        let during = state
            .get_delegation(&parent, &delegation)
            .map(|response| serde_json::to_value(response).unwrap()["delegation"].clone());
        let wait = state.create_delegation_wait(
            &parent,
            CreateDelegationWaitRequest {
                delegation_ids: vec![delegation.clone()],
                mode: DelegationWaitMode::Any,
                title: None,
            },
        );
        let delivered_early = receiver.try_recv().is_ok();
        evaluate_gate.release();
        (worker.join().unwrap(), during, wait, delivered_early)
    });
    let during = during.unwrap();
    assert_eq!(
        during["status"], "queued",
        "an admitting resume is not held"
    );
    assert!(during.get("hold").is_none());
    let wait = wait.unwrap();
    assert!(
        !wait.resume_prompt_queued,
        "no attention wake while it admits"
    );
    assert!(!delivered_early);
    let resumed = resumed.unwrap();
    assert_eq!(
        resumed.turn.as_ref().unwrap().state,
        DelegationTurnDeliveryState::Delivered
    );
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert_eq!(
        status_json(&state, &parent, &delegation)["status"],
        "running"
    );
    assert!(wait_notifications(&state, &parent, &wait.wait.id).is_empty());
}

/// Review regression (a delivered follow-up answered with its admission
/// snapshot): a follow-up whose Engram admission delivers answers with the
/// delegation as it stands after delivery, running, as a status read does.
#[test]
fn a_delivered_follow_up_answers_with_its_current_running_status() {
    let (state, parent, receiver, _) = root_fixture([
        bind_reply("parent"),
        bind_reply("child"),
        grant_reply("first"),
        begin_reply("first"),
        checkpoint_reply("first"),
        grant_reply("second"),
        begin_reply("second"),
    ]);
    let created = state
        .create_read_only_delegation(&parent, explorer_request("initial", "Delivered follow-up"))
        .unwrap();
    let delegation = created.delegation.id;
    let child = created.delegation.child_session_id;
    assert!(matches!(
        receive_synchronous_engram_prompt(&state, &receiver, "initial prompt").unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    finish_child_turn(
        &state,
        &child,
        "## Result\nStatus: completed\n\nSummary:\ndone",
    );

    let followed = state
        .followup_delegation(&parent, &delegation, "second prompt".into())
        .unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert_eq!(
        followed.turn.as_ref().unwrap().state,
        DelegationTurnDeliveryState::Delivered
    );
    let wire = serde_json::to_value(&followed).unwrap();
    assert_eq!(wire["delegation"]["status"], "running");
    assert!(wire["delegation"].get("pendingStart").is_none());
    assert_eq!(
        wire["delegation"]["status"],
        status_json(&state, &parent, &delegation)["status"]
    );
}

/// Review regression (a resume reported delivered by inference): a resume
/// whose turn goes through Codex Fast discovery is scheduled when the call
/// returns; the worker hands it over later, or fails. The response reports
/// that outcome, not one read off the child's status.
#[test]
fn a_resume_through_fast_discovery_reports_scheduled() {
    let (state, parent, receiver, _) = root_fixture([
        bind_reply("fast-parent"),
        bind_reply("fast-child"),
        defer_reply("busy"),
        grant_reply("fast-grant"),
        begin_reply("fast-grant"),
        checkpoint_reply("fast-grant"),
    ]);
    let created = state
        .create_read_only_delegation(&parent, explorer_request("deferred", "Fast resume"))
        .unwrap();
    let delegation = created.delegation.id;
    let child = created.delegation.child_session_id;
    assert_eq!(status_json(&state, &parent, &delegation)["status"], "held");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        let record = &mut inner.sessions[index];
        record.session.model = "catalog-model".into();
        record.session.codex_fast_mode = true;
        record.session.model_options.clear();
    }

    let resumed = state.resume_delegation(&parent, &delegation).unwrap();
    assert_eq!(
        resumed.turn.as_ref().unwrap().state,
        DelegationTurnDeliveryState::Scheduled,
        "a Fast-discovery turn is not delivered until its worker hands it over"
    );
    // Let the worker finish: the discovery fails, and nothing was delivered.
    let CodexRuntimeCommand::RefreshModelList { response_tx } =
        phase_sync::receive(&receiver, "resumed Fast discovery request")
    else {
        panic!("Fast discovery must precede provider delivery")
    };
    response_tx
        .send(Err("forced catalog failure".to_owned()))
        .unwrap();
    assert!(
        receiver.recv_timeout(Duration::from_millis(200)).is_err(),
        "a failed discovery delivers nothing"
    );
}

/// Review regression (a resume not bound to the prompt it checked): the
/// bound drain admits only the retained head captured at the check. A
/// successor exposed by a cancellation in between is never promoted; nothing
/// starts and the successor stays queued.
#[test]
fn a_bound_resume_never_admits_a_successor_exposed_after_its_check() {
    let state = test_app_state_with_drained_delegation_codex_runtime("held-bound-resume");
    let (parent, held, held_child, _, _) = two_explorers(&state);
    hold_child(&state, &held_child, true, false);
    let (owner, held_prompt) = {
        let inner = state.inner.lock().unwrap();
        let child = &inner.sessions[inner.find_session_index(&held_child).unwrap()];
        (
            EngramQueuedAdmissionOwner::capture(child).expect("the held head"),
            child.queued_prompts[0].pending_prompt.id.clone(),
        )
    };
    assert_eq!(
        stored_delegation(&state, &held)
            .attempt
            .hold
            .unwrap()
            .prompt_id,
        held_prompt
    );
    // A successor queued behind the held head, then the held head canceled.
    {
        let mut inner = state.inner.lock().unwrap();
        let message_id = inner.next_message_id();
        let index = inner.find_session_index(&held_child).unwrap();
        queue_prompt_on_record_with_source(
            inner.session_mut_by_index(index).unwrap(),
            PendingPrompt {
                engram_interrupted: false,
                is_engram_retained: false,
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                text: "successor".to_owned(),
                expanded_text: None,
                source: None,
            },
            Vec::new(),
            QueuedPromptSource::User,
        );
        state.commit_locked(&mut inner).unwrap();
    }
    state
        .cancel_queued_prompt(&held_child, &held_prompt)
        .unwrap();

    let resumed = state
        .resume_local_session_queue(&held_child, Some(owner))
        .unwrap();
    assert!(resumed.is_none(), "nothing is promoted for a changed head");
    let inner = state.inner.lock().unwrap();
    let child = &inner.sessions[inner.find_session_index(&held_child).unwrap()];
    assert_eq!(child.queued_prompts.len(), 1);
    assert_eq!(child.queued_prompts[0].pending_prompt.text, "successor");
    drop(inner);
    // Through the route, a hold whose prompt is gone refuses resume.
    assert!(state.resume_delegation(&parent, &held).is_err());
}

/// Review regression (a wait judged on a stale stored hold): while the
/// child's admission owns the retained prompt, before any refresh clears
/// the stored hold, a new wait is not woken for attention.
#[test]
fn a_wait_ignores_a_stored_hold_the_child_no_longer_has() {
    let state = test_app_state_with_drained_delegation_codex_runtime("held-stale-wait");
    let (parent, held, held_child, _, _) = two_explorers(&state);
    hold_child(&state, &held_child, true, false);
    assert!(stored_delegation(&state, &held).attempt.hold.is_some());
    {
        // The admission installed its ownership; the off-lock refresh has
        // not run yet.
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&held_child).unwrap();
        inner.sessions[index].engram.admission_in_progress =
            Some(Arc::new(std::sync::atomic::AtomicBool::new(false)));
    }
    let wait = wait_for(&state, &parent, &[&held], DelegationWaitMode::Any);
    assert!(
        !wait.resume_prompt_queued,
        "an admitting child needs no attention"
    );
    assert!(wait_notifications(&state, &parent, &wait.wait.id).is_empty());
    assert_eq!(state.inner.lock().unwrap().delegation_waits.len(), 1);
}

/// Review regression (a failed hold-refresh commit stranding the wake): when
/// the commit fails, the wait and the parent's queue are restored while the
/// hold stays the in-memory truth, and a later refresh judges the wait again
/// although the attempt is unchanged, so the parent is notified exactly once.
#[test]
fn a_failed_hold_refresh_commit_is_retried_and_notifies_once() {
    let mut state = test_app_state_with_drained_delegation_codex_runtime("held-commit-fail");
    let (parent, held, held_child, _, _) = two_explorers(&state);
    let wait = wait_for(&state, &parent, &[&held], DelegationWaitMode::Any);
    assert!(!wait.resume_prompt_queued);
    hold_child_without_refresh(&state, &held_child, true, false);

    state.shutdown_persist_blocking();
    let original_path = state.persistence_path.clone();
    let failing_path = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("hold-refresh-commit-is-directory");
    fs::create_dir_all(&failing_path).unwrap();
    state.persistence_path = Arc::new(failing_path.clone());
    state.sync_delegation_attempt_for_child_session(&held_child);
    {
        let inner = state.inner.lock().unwrap();
        assert_eq!(inner.delegation_waits.len(), 1, "the wait is restored");
        let record = &inner.delegations[inner.find_delegation_index(&held).unwrap()];
        // Memory keeps what the child is; only the wake's transaction is
        // restored for a retry.
        assert!(
            record.attempt.hold.is_some(),
            "the hold stays the in-memory truth"
        );
    }
    assert!(wait_notifications(&state, &parent, &wait.wait.id).is_empty());

    state.persistence_path = original_path;
    state.sync_delegation_attempt_for_child_session(&held_child);
    assert!(state.inner.lock().unwrap().delegation_waits.is_empty());
    assert_eq!(wait_notifications(&state, &parent, &wait.wait.id).len(), 1);
    state.sync_delegation_attempt_for_child_session(&held_child);
    assert_eq!(wait_notifications(&state, &parent, &wait.wait.id).len(), 1);
    fs::remove_dir_all(failing_path).unwrap();
}

/// A public Stop of a held child waiting for its automatic retry reads as
/// stopped, cancel only, and the stopped head survives a save and load.
#[test]
fn a_public_stop_of_a_retrying_held_child_reads_stopped_and_persists() {
    let (mut state, parent, _receiver, _) = root_fixture([]);
    let persister = ScriptedPersister::install(&mut state);
    let (begin, begin_gate) = gated_engram_step("turn_begin", begin_reply("stop-grant"));
    state.install_control_test_transport(GatedEngramControlTransport::new([
        immediate_engram_step("session_bind", bind_reply("stop-parent")),
        immediate_engram_step("session_bind", bind_reply("stop-child")),
        immediate_engram_step("turn_evaluate", grant_reply("stop-grant")),
        begin,
        immediate_engram_step("turn_checkpoint", checkpoint_reply("stop-grant")),
    ]));
    let response = std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            state.create_read_only_delegation(&parent, explorer_request("stopped", "Stopped"))
        });
        begin_gate.wait();
        persister.fail_next(1);
        begin_gate.release();
        worker.join().unwrap()
    })
    .unwrap();
    let delegation = response.delegation.id;
    let child = response.delegation.child_session_id;
    await_settlement_acknowledgement(&state, &child);
    state.engram_abort_retry_tick(chrono::Utc::now());
    assert_eq!(
        stored_delegation(&state, &delegation)
            .attempt
            .hold
            .unwrap()
            .reason,
        DelegationHoldReason::RetryScheduled
    );

    state.request_stop_session(&child).unwrap();
    let hold = stored_delegation(&state, &delegation).attempt.hold.unwrap();
    assert_eq!(hold.reason, DelegationHoldReason::Stopped);
    assert_eq!(hold.actions, vec![DelegationHoldAction::Cancel]);
    assert_eq!(
        status_json(&state, &parent, &delegation)["hold"]["reason"],
        "stopped"
    );

    let saved = {
        let inner = state.inner.lock().unwrap();
        serde_json::to_string(&PersistedSessionRecord::from_record(
            &inner.sessions[inner.find_session_index(&child).unwrap()],
        ))
        .unwrap()
    };
    let loaded = serde_json::from_str::<PersistedSessionRecord>(&saved)
        .unwrap()
        .into_record()
        .unwrap();
    assert_eq!(
        loaded.engram.stopped_prompt_id.as_deref(),
        Some(hold.prompt_id.as_str())
    );
    assert_eq!(
        delegation_child_hold(&loaded).map(|held| held.reason),
        Some(DelegationHoldReason::Stopped)
    );
    persister.stop(&state);
}
