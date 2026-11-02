//! Ordinary delegation delivery, paused/unknown holds and durable wait fan-in.
//! Uses the existing runtime and wait owners; no control evaluation is emulated.
use super::*;
use super::super::uncertain_begin::OrdinaryRoot;
use crate::tests::delegation_support::{
    boot_on_delegation_root, finish_delegation_child_with_assistant_text,
    install_delegation_codex_runtime, mark_delegation_as_unstructured_explorer,
    temp_delegation_state_paths, test_app_state_with_drained_delegation_codex_runtime,
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


// Synthetic ordinary paused-child specimens. They model current queue/receipt
// uncertainty; they do not recreate a retired control admission or producer.
fn hold_child_without_refresh(state: &AppState, child: &str, unknown: bool) {
    let mut inner = state.inner.lock().unwrap();
    let id = inner.next_message_id();
    let record = inner.session_mut(child).unwrap();
    queue_prompt_on_record_with_source(record, PendingPrompt {
        engram_interrupted: false, is_engram_retained: false, attachments: Vec::new(),
        id, timestamp: stamp_now(), text: "paused ordinary child prompt".into(),
        expanded_text: None, source: None,
    }, Vec::new(), QueuedPromptSource::Orchestrator);
    if unknown {
        let head = record.queued_prompts.front_mut().unwrap();
        head.engram_ordinary_handoff = true;
    }
    record.session.status = SessionStatus::Idle;
    record.session.live_activity = None;
    record.session.preview = "Ordinary queue paused by the operator.".into();
    record.set_auto_dispatch_blocked(true);
    sync_pending_prompts(record);
    state.commit_locked(&mut inner).unwrap();
}

fn hold_child(state: &AppState, child: &str, unknown: bool) {
    hold_child_without_refresh(state, child, unknown);
    state.sync_delegation_attempt_for_child_session(child);
}

fn release_child(state: &AppState, child: &str) {
    {
        let mut inner = state.inner.lock().unwrap();
        let record = inner.session_mut(child).unwrap();
        record.queued_prompts.clear();
        record.set_auto_dispatch_blocked(false);
        record.session.status = SessionStatus::Active;
        sync_pending_prompts(record);
        state.commit_locked(&mut inner).unwrap();
    }
    state.sync_delegation_attempt_for_child_session(child);
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


#[test]
fn ordinary_first_turn_is_durable_and_reports_its_actual_delivery() {
    let turn = OrdinaryRoot::new("ordinary-delegation-first");
    let created = turn.state.create_read_only_delegation(&turn.session,
        explorer_request("ordinary child prompt", "Ordinary child")).unwrap();
    assert_eq!(created.first_turn.as_ref().unwrap().state, DelegationTurnDeliveryState::Delivered);
    assert_eq!(delegation_turn_response_status(&created), StatusCode::CREATED);
    let child = &created.delegation.child_session_id;
    assert!(matches!(receive_synchronous_engram_prompt(&turn.state, &turn.receiver, "child prompt").unwrap(),
        CodexRuntimeCommand::Prompt { session_id, .. } if &session_id == child));
    assert_eq!(status_json(&turn.state, &turn.session, &created.delegation.id)["status"], "running");
    assert!(created.delegation.attempt.hold.is_none());
    let inner = turn.state.inner.lock().unwrap();
    assert_eq!(inner.delegations.len(), 1);
    persist_locked_state_to_sqlite(&turn.state.persistence_path, &inner).unwrap();
    drop(inner);
    let disk = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    assert!(PersistFenceTarget::Delegation(Box::new(
        stored_delegation(&turn.state, &created.delegation.id))).is_already_durable(&disk).unwrap());
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn a_creation_that_cannot_be_saved_names_its_delegation_and_starts_no_turn() {
    let mut turn = OrdinaryRoot::new("ordinary-unsaved-delegation");
    let state = &mut turn.state;
    let parent = turn.session.clone();
    state.shutdown_persist_blocking();
    let failing_path = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("creation-commit-is-directory");
    fs::create_dir_all(&failing_path).unwrap();
    let original_path = state.persistence_path.clone();
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
    state.persistence_path = original_path;
    turn.assert_no_prompt();
    fs::remove_dir(failing_path).unwrap();
}

#[test]
fn ordinary_follow_up_never_reuses_the_previous_result_and_completes_its_own_wait() {
    let turn = OrdinaryRoot::new("ordinary-delegation-followup");
    let created = turn.state.create_read_only_delegation(&turn.session, explorer_request("initial", "Follow up")).unwrap();
    let delegation = created.delegation.id;
    let child = created.delegation.child_session_id;
    mark_delegation_as_unstructured_explorer(&turn.state, &delegation);
    receive_synchronous_engram_prompt(&turn.state, &turn.receiver, "initial child").unwrap();
    finish_child_turn(&turn.state, &child, "## Result\nStatus: completed\n\nSummary:\ninitial completed");
    let followed = turn.state.followup_delegation(&turn.session, &delegation, "follow-up prompt".into()).unwrap();
    assert_eq!(followed.turn.as_ref().unwrap().state, DelegationTurnDeliveryState::Delivered);
    receive_synchronous_engram_prompt(&turn.state, &turn.receiver, "followup child").unwrap();
    let current = status_json(&turn.state, &turn.session, &delegation);
    assert_eq!(current["status"], "running");
    assert!(current.get("result").is_none() && current.get("completedAt").is_none());
    assert!(turn.state.get_delegation_result(&turn.session, &delegation).is_err());
    let wait = wait_for(&turn.state, &turn.session, &[&delegation], DelegationWaitMode::All);
    assert!(!wait.resume_prompt_queued);
    finish_child_turn(&turn.state, &child, "## Result\nStatus: completed\n\nSummary:\nfollow-up completed");
    let notifications = wait_notifications(&turn.state, &turn.session, &wait.wait.id);
    assert_eq!(notifications.len(), 1);
    let text = notifications.values().next().unwrap();
    assert!(text.contains("follow-up completed") && !text.contains("initial completed"));
    let continuation = turn.prompt();
    assert!(continuation.prompt.contains(&wait.wait.id));
    assert!(continuation.prompt.contains("follow-up completed"));
    assert!(!continuation.prompt.contains("initial completed"));
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn held_children_wake_waits_once_with_attention_and_never_count_as_terminal() {
    let state = test_app_state_with_drained_delegation_codex_runtime("held-wait");
    let (parent, held, held_child, done, done_child) = two_explorers(&state);
    let all = wait_for(&state, &parent, &[&held, &done], DelegationWaitMode::All);
    assert!(!all.resume_prompt_queued);

    hold_child(&state, &held_child, false);
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
    assert!(text.contains("Reason: stopped"));
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
    hold_child(&state, &held_child, false);
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
    let (temp_root, project_root, persistence_path, templates_path) = temp_delegation_state_paths();
    let boot = || {
        boot_on_delegation_root(&temp_root, ||
            AppState::new_with_paths(
                project_root.to_string_lossy().into_owned(),
                persistence_path.clone(),
                templates_path.clone(),
            )
            .expect("state should boot")
        )
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
        hold_child_without_refresh(&state, &created.delegation.child_session_id, false);
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
fn ordinary_unknown_hold_is_cancel_only_and_cancellation_clears_its_presentation() {
    let state = test_app_state_with_drained_delegation_codex_runtime("ordinary-unknown-child");
    let (parent, held, child, running, _) = two_explorers(&state);
    assert_eq!(state.resume_delegation(&parent, &running).err().unwrap().status, StatusCode::CONFLICT);
    hold_child(&state, &child, true);
    let hold = stored_delegation(&state, &held).attempt.hold.unwrap();
    assert_eq!(hold.reason, DelegationHoldReason::DeliveryUnknown);
    assert_eq!(hold.actions, vec![DelegationHoldAction::Cancel]);
    assert!(!hold.retry_eligible);
    assert_eq!(state.resume_delegation(&parent, &held).err().unwrap().status, StatusCode::CONFLICT);
    let canceled = state.cancel_delegation(&parent, &held).unwrap();
    assert_eq!(canceled.delegation.status, DelegationStatus::Canceled);
    assert!(canceled.delegation.attempt.hold.is_none());
    assert!(state.list_delegations(&parent).unwrap().delegations.into_iter()
        .find(|record| record.id == held).unwrap().hold.is_none());
}

#[test]
fn ordinary_paused_child_resumes_once_and_wait_does_not_wake_while_it_runs() {
    let turn = OrdinaryRoot::new("ordinary-child-resume");
    let created = turn.state.create_read_only_delegation(&turn.session, explorer_request("initial", "Resume")).unwrap();
    receive_synchronous_engram_prompt(&turn.state, &turn.receiver, "initial child").unwrap();
    let child = created.delegation.child_session_id;
    let delegation = created.delegation.id;
    hold_child(&turn.state, &child, false);
    assert_eq!(status_json(&turn.state, &turn.session, &delegation)["status"], "held");
    let resumed = turn.state.resume_delegation(&turn.session, &delegation).unwrap();
    assert_eq!(resumed.turn.as_ref().unwrap().state, DelegationTurnDeliveryState::Delivered);
    receive_synchronous_engram_prompt(&turn.state, &turn.receiver, "resumed child").unwrap();
    assert_eq!(status_json(&turn.state, &turn.session, &delegation)["status"], "running");
    let wait = wait_for(&turn.state, &turn.session, &[&delegation], DelegationWaitMode::Any);
    assert!(!wait.resume_prompt_queued);
    assert!(wait_notifications(&turn.state, &turn.session, &wait.wait.id).is_empty());
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_fast_discovery_resume_reports_scheduled_before_any_provider_handoff() {
    let turn = OrdinaryRoot::new("ordinary-child-fast");
    let created = turn.state.create_read_only_delegation(&turn.session, explorer_request("initial", "Fast")).unwrap();
    receive_synchronous_engram_prompt(&turn.state, &turn.receiver, "initial child").unwrap();
    let child = created.delegation.child_session_id;
    let delegation = created.delegation.id;
    hold_child(&turn.state, &child, false);
    {
        let mut inner = turn.state.inner.lock().unwrap();
        let record = inner.session_mut(&child).unwrap();
        record.session.model = "catalog-model".into();
        record.session.codex_fast_mode = true;
        record.session.model_options.clear();
    }
    let resumed = turn.state.resume_delegation(&turn.session, &delegation).unwrap();
    assert_eq!(resumed.turn.as_ref().unwrap().state, DelegationTurnDeliveryState::Scheduled);
    let CodexRuntimeCommand::RefreshModelList { response_tx } =
        phase_sync::receive(&turn.receiver, "ordinary Fast discovery") else { panic!("expected discovery"); };
    response_tx.send(Err("forced catalog failure".into())).unwrap();
    let settled = phase_sync::PollGuard::new();
    while {
        let inner = turn.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&child).unwrap()].session.status == SessionStatus::Active
    } {
        settled.wait("the failed Fast worker reaches its terminal state");
    }
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn a_bound_resume_never_admits_a_successor_exposed_after_its_check() {
    let state = test_app_state_with_drained_delegation_codex_runtime("held-bound-resume");
    state.select_test_scripted_engram_budget_clock();
    let (parent, held, held_child, _, _) = two_explorers(&state);
    hold_child(&state, &held_child, false);
    let (owner, held_prompt) = {
        let inner = state.inner.lock().unwrap();
        let child = &inner.sessions[inner.find_session_index(&held_child).unwrap()];
        (
            QueuedTurnOwner::capture(child).expect("the held head"),
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
    let successor = {
        let mut inner = state.inner.lock().unwrap();
        let message_id = inner.next_message_id();
        let index = inner.find_session_index(&held_child).unwrap();
        queue_prompt_on_record_with_source(
            inner.session_mut_by_index(index).unwrap(),
            PendingPrompt {
                engram_interrupted: false,
                is_engram_retained: false,
                attachments: Vec::new(),
                id: message_id.clone(),
                timestamp: stamp_now(),
                text: "successor".to_owned(),
                expanded_text: None,
                source: None,
            },
            Vec::new(),
            QueuedPromptSource::User,
        );
        state.commit_locked(&mut inner).unwrap();
        message_id
    };
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
    assert_eq!(child.queued_prompts[0].pending_prompt.id, successor);
    assert_eq!(child.queued_prompts[0].pending_prompt.text, "successor");
    assert!(child.orchestrator_auto_dispatch_blocked);
    assert!(!child.session.messages.iter().any(|message| message.id() == successor));
    drop(inner);
    // A new public Resume captures the current hold; it does not reuse the
    // canceled head's owner from the earlier private call.
    let resumed = state.resume_delegation(&parent, &held).unwrap();
    assert_eq!(resumed.turn.as_ref().unwrap().state, DelegationTurnDeliveryState::Delivered);
    {
        let inner = state.inner.lock().unwrap();
        let child = &inner.sessions[inner.find_session_index(&held_child).unwrap()];
        assert!(child.queued_prompts.is_empty());
        assert_eq!(child.session.status, SessionStatus::Active);
        assert_eq!(child.session.messages.iter().filter(|message| message.id() == successor).count(), 1);
    }
    assert!(state.resume_delegation(&parent, &held).is_err(), "the active successor cannot be resumed twice");
}

/// Review regression (a wait judged on a stale stored hold): while the
/// child's admission owns the retained prompt, before any refresh clears
/// the stored hold, a new wait is not woken for attention.

#[test]
fn ordinary_wait_ignores_a_stale_hold_after_the_child_has_resumed() {
    let state = test_app_state_with_drained_delegation_codex_runtime("ordinary-stale-hold");
    let (parent, held, child, _, _) = two_explorers(&state);
    hold_child(&state, &child, false);
    assert!(stored_delegation(&state, &held).attempt.hold.is_some());
    // Current active state, before off-lock presentation refresh.
    {
        let mut inner = state.inner.lock().unwrap();
        inner.session_mut(&child).unwrap().session.status = SessionStatus::Active;
    }
    let wait = wait_for(&state, &parent, &[&held], DelegationWaitMode::Any);
    assert!(!wait.resume_prompt_queued);
    assert!(wait_notifications(&state, &parent, &wait.wait.id).is_empty());
    assert_eq!(state.inner.lock().unwrap().delegation_waits.len(), 1);
}

#[test]
fn a_failed_hold_refresh_commit_is_retried_and_notifies_once() {
    let mut state = test_app_state_with_drained_delegation_codex_runtime("held-commit-fail");
    let (parent, held, held_child, _, _) = two_explorers(&state);
    let wait = wait_for(&state, &parent, &[&held], DelegationWaitMode::Any);
    assert!(!wait.resume_prompt_queued);
    hold_child_without_refresh(&state, &held_child, false);

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
    fs::remove_dir(failing_path).unwrap();
}

/// A public Stop of a held child waiting for its automatic retry reads as
/// stopped, cancel only, and the stopped head survives a save and load.

#[test]
fn ordinary_public_stop_claim_cannot_be_bypassed_by_child_resume_and_survives_load() {
    let turn = OrdinaryRoot::new("ordinary-child-stop");
    let created = turn.state.create_read_only_delegation(&turn.session, explorer_request("initial", "Stop")).unwrap();
    receive_synchronous_engram_prompt(&turn.state, &turn.receiver, "initial child").unwrap();
    let child = created.delegation.child_session_id;
    hold_child(&turn.state, &child, false);
    {
        let mut inner = turn.state.inner.lock().unwrap();
        let record = inner.session_mut(&child).unwrap();
        record.queued_prompts.front_mut().unwrap().source = QueuedPromptSource::User;
        record.session.status = SessionStatus::Active;
        turn.state.commit_locked(&mut inner).unwrap();
    }
    let (_, claim) = turn.state.begin_requested_stop_session(&child, &StopSessionOptions::default()).unwrap();
    assert!(claim.is_some());
    assert!(turn.state.resume_session_queue(&child).is_err());
    {
        let inner = turn.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&child).unwrap()];
        assert!(record.runtime_stop_in_progress);
        assert!(!record.queued_prompts.is_empty());
        persist_locked_state_to_sqlite(&turn.state.persistence_path, &inner).unwrap();
    }
    let loaded = load_state(&turn.state.persistence_path).unwrap().unwrap();
    let record = loaded.sessions.iter().find(|record| record.session.id == child).unwrap();
    assert!(!record.queued_prompts.is_empty());
    assert!(record.orchestrator_auto_dispatch_blocked);
    turn.assert_no_prompt();
    turn.assert_no_control();
}
