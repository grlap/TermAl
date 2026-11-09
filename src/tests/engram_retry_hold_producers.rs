//! Ordinary mailbox, pause, Stop and explicit receipt Resume controls.
use super::*;
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;
use crate::tests::mailbox_acknowledged_wake::send;
use crate::tests::prompt_delivery::{DeliveryWriter, HeldFence};

fn with_mailbox(state: AppState) -> AppState {
    let path = resolve_coordination_persistence_path(state.persistence_path.as_ref());
    AppState {
        mailbox_store: Arc::new(MailboxStore::open(&path).unwrap()),
        ..state
    }
}

fn assert_plain_pause(turn: &OrdinaryRoot) {
    assert_eq!(
        turn.state.mailbox_queued_wake_disposition(&turn.session),
        "heldBehindPausedQueue"
    );
    turn.record(|record| {
        assert!(record.orchestrator_auto_dispatch_blocked);
        let full = serde_json::to_value(AppState::wire_session_from_record(
            &turn.state.server_instance_id,
            record,
        ))
        .unwrap();
        let summary = serde_json::to_value(AppState::wire_session_summary_from_record(
            &turn.state.server_instance_id,
            record,
        ))
        .unwrap();
        assert!(full["queueHold"].is_null());
        assert!(summary["queueHold"].is_null());
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn an_ordinary_mailbox_wake_queued_behind_a_turn_delivers_once_without_resume() {
    let mut turn = OrdinaryRoot::new("ordinary-queued-mailbox");
    turn.state = with_mailbox(turn.state.clone());
    let sender = test_session_id(&turn.state, Agent::Claude);
    let dispatch = turn.prepare("first ordinary turn");
    let token = dispatch.runtime_token().clone();
    deliver_turn_dispatch(&turn.state, dispatch)
        .into_public_result()
        .unwrap();
    turn.prompt();
    let queued = send(&turn.state, &sender, &turn.session, "ordinary-followup");
    assert_eq!(queued.notification_disposition, "queuedBehindActiveTurn");
    let expected = turn.record(|record| {
        assert_eq!(record.queued_prompts[0].source, QueuedPromptSource::Mailbox);
        record.queued_prompts[0].pending_prompt.text.clone()
    });
    turn.assert_no_prompt();
    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.session, &token)
        .unwrap();
    let command = turn.prompt();
    assert!(command.prompt.contains(&expected));
    turn.record(|record| {
        assert!(record.queued_prompts.is_empty());
        assert!(!record.orchestrator_auto_dispatch_blocked);
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn an_operator_pause_keeps_mailbox_wakes_queued_without_an_automatic_retry() {
    for code in [
        "deadline_exceeded",
        "begin_preparation_pending",
        "control_unavailable",
    ] {
        let mut turn = OrdinaryRoot::new("paused-ordinary-mailbox");
        turn.state = with_mailbox(turn.state.clone());
        let sender = test_session_id(&turn.state, Agent::Claude);
        let history = json!({"code":code,"dueAt":"2026-10-20T10:00:00Z","attempts":8});
        turn.edit(|record| {
            record.set_auto_dispatch_blocked(true);
            record
                .engram
                .historical_control
                .insert("engramAdmissionRetry".into(), history.clone());
        });
        let queued = send(&turn.state, &sender, &turn.session, "paused-followup");
        assert_eq!(queued.notification_disposition, "heldBehindPausedQueue");
        let head = turn.record(|record| serde_json::to_value(&record.queued_prompts[0]).unwrap());
        turn.state.dispatch_orphaned_workflow_prompts();
        turn.state.dispatch_orphaned_workflow_prompts();
        turn.record(|record| {
            assert_eq!(
                serde_json::to_value(&record.queued_prompts[0]).unwrap(),
                head
            );
            assert_eq!(
                record.engram.historical_control["engramAdmissionRetry"],
                history
            );
        });
        assert_plain_pause(&turn);
    }
}

#[test]
fn a_public_stop_keeps_an_ordinary_queued_head_paused() {
    let turn = OrdinaryRoot::new("ordinary-stop-queued-head");
    let dispatch = turn.prepare("active ordinary turn before Stop");
    deliver_turn_dispatch(&turn.state, dispatch)
        .into_public_result()
        .unwrap();
    turn.prompt();
    let id = turn.enqueue(
        "ordinary queued content before Stop",
        QueuedPromptSource::Mailbox,
    );
    turn.state.request_stop_session(&turn.session).unwrap();
    let settled = phase_sync::PollGuard::new();
    while turn.record(|record| record.runtime_stop_in_progress) {
        settled.wait("the real Stop owner settles");
    }
    turn.state.dispatch_orphaned_workflow_prompts();
    turn.record(|record| {
        assert_eq!(record.queued_prompts[0].pending_prompt.id, id);
        assert_eq!(
            record.queued_prompts[0].pending_prompt.text,
            "ordinary queued content before Stop"
        );
    });
    assert_plain_pause(&turn);
}

#[test]
fn a_settings_authority_change_does_not_bypass_an_ordinary_pause() {
    let turn = OrdinaryRoot::new("paused-settings-authority-change");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let id = turn.enqueue("exact paused content", QueuedPromptSource::User);
    let mut settings = {
        let mut inner = turn.state.inner.lock().unwrap();
        let project = inner
            .projects
            .iter_mut()
            .find(|entry| entry.id == turn.project)
            .unwrap();
        let settings = project.engram.as_mut().unwrap();
        settings.binary_path = Some(
            real_engram_control_fixture_path()
                .to_string_lossy()
                .into_owned(),
        );
        settings.clone()
    };
    fs::write(turn.root.join(".engram-project"), "fixture-ok\n").unwrap();
    let home = turn.root.join("changed-authority-home");
    fs::create_dir_all(&home).unwrap();
    settings.home = Some(home.to_string_lossy().into_owned());
    turn.state
        .update_project_engram_settings(&turn.project, settings)
        .unwrap();
    turn.state.dispatch_orphaned_workflow_prompts();
    turn.record(|record| assert_eq!(record.queued_prompts[0].pending_prompt.id, id));
    assert_plain_pause(&turn);
}

#[test]
fn cancel_retires_only_the_selected_paused_mailbox_head() {
    let mut turn = OrdinaryRoot::new("cancel-paused-mailbox");
    turn.state = with_mailbox(turn.state.clone());
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let sender = test_session_id(&turn.state, Agent::Claude);
    send(&turn.state, &sender, &turn.session, "first-paused-wake");
    let first = turn.record(|record| record.queued_prompts[0].pending_prompt.id.clone());
    let successor = turn.enqueue("independent paused user content", QueuedPromptSource::User);
    let next = turn.record(|record| {
        let queued = record
            .queued_prompts
            .iter()
            .find(|queued| queued.pending_prompt.id == successor)
            .unwrap();
        serde_json::to_value(&queued.pending_prompt).unwrap()
    });
    turn.state
        .cancel_queued_prompt(&turn.session, &first)
        .unwrap();
    turn.record(|record| {
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0].pending_prompt).unwrap(),
            next
        );
    });
    assert_plain_pause(&turn);
}

#[test]
fn a_real_receipted_head_stays_paused_until_explicit_resume_then_sends_once() {
    let mut turn = OrdinaryRoot::new("paused-real-receipt");
    let writer = DeliveryWriter::attach(&mut turn.state);
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let id = turn.enqueue_durable("exact receipted ordinary content", true);
    assert!(
        turn.state
            .request_queued_prompt_durability(&turn.session, &id)
    );
    turn.assert_no_prompt();
    writer.take(HeldFence::Durability).finish(Ok(()));
    let acknowledged = phase_sync::PollGuard::new();
    while !turn.record(|record| {
        record
            .queued_prompt_durability_holds
            .get(&id)
            .is_some_and(|hold| hold.acknowledged)
    }) {
        acknowledged.wait("the content fence acknowledgement settles");
    }
    turn.record(|record| {
        assert_eq!(
            record.prompt_deliveries[&id].disposition,
            PromptDeliveryDisposition::Pending
        );
        assert!(record.prompt_deliveries[&id].turn_generation.is_none());
    });
    turn.state.dispatch_orphaned_workflow_prompts();
    assert_plain_pause(&turn);
    turn.state.resume_session_queue(&turn.session).unwrap();
    let write_ahead = writer.take(HeldFence::WriteAhead);
    turn.record(|record| {
        let delivery = &record.prompt_deliveries[&id];
        assert_eq!(delivery.disposition, PromptDeliveryDisposition::HandingOff);
        assert!(!delivery.write_ahead_acknowledged);
        assert!(delivery.turn_generation.is_none());
        assert!(record.orchestrator_auto_dispatch_blocked);
    });
    turn.assert_no_prompt();
    write_ahead.finish(Ok(()));
    let command = turn.prompt();
    assert!(command.prompt.contains("exact receipted ordinary content"));
    assert_eq!(command.delivery_receipt.as_ref().unwrap().prompt_id, id);
    turn.record(|record| {
        assert!(!record.orchestrator_auto_dispatch_blocked);
        assert_eq!(
            record.prompt_deliveries[&id].turn_generation,
            Some(command.active_turn_generation)
        );
        assert!(record.queued_prompts.is_empty());
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
    drop(writer);
}
