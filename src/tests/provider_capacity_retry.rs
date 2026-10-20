//! Capacity recovery resumes queued work as a new turn, preserving prior work.

use super::*;

#[path = "provider_capacity_terminal_ownership.rs"]
mod terminal_ownership;

struct CapacityFixture {
    state: AppState,
    session_id: String,
    runtime: SharedCodexRuntime,
    input_rx: mpsc::Receiver<CodexRuntimeCommand>,
    failed_generation: u64,
    sender_id: String,
}

impl CapacityFixture {
    fn started_mailbox_turn() -> Self {
        Self::mailbox_turn(true)
    }

    fn mailbox_turn(started: bool) -> Self {
        let (state, sender_id, session_id) = mailboxes::mailbox_test_state();
        let (runtime, input_rx, process) = test_shared_codex_runtime("capacity-recovery");
        {
            let mut inner = state.inner.lock().unwrap();
            let codex_index = inner.find_session_index(&sender_id).unwrap();
            let codex_session = inner.sessions[codex_index].session.clone();
            let index = inner.find_session_index(&session_id).unwrap();
            let record = &mut inner.sessions[index];
            // Reuse the valid Codex wire shape, including agent-specific
            // settings, while keeping this target's identity and busy state.
            record.session = codex_session;
            record.session.id = session_id.clone();
            record.session.name = "Capacity root".to_owned();
            record.session.status = SessionStatus::Active;
            record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                runtime_id: runtime.runtime_id.clone(),
                input_tx: runtime.input_tx.clone(),
                process,
                shared_session: Some(SharedCodexSessionHandle {
                    runtime: runtime.clone(),
                    session_id: session_id.clone(),
                }),
            });
        }
        state
            .append_mailbox_message_and_notify(
                &sender_id,
                SendMailboxMessageRequest {
                    target_session_id: session_id.clone(),
                    message: "Continue the queued task after checking its current state."
                        .to_owned(),
                    idempotency_key: "capacity-fixture-wake".to_owned(),
                    topic: None,
                    state_stamp: None,
                    class: Some("routine".to_owned()),
                },
            )
            .unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session_id).unwrap();
            inner.sessions[index].session.status = SessionStatus::Idle;
        }
        let dispatch = state
            .dispatch_next_queued_turn(&session_id, false)
            .unwrap()
            .unwrap();
        deliver_turn_dispatch(&state, dispatch).expect("fixture runtime accepts the mailbox turn");
        let failed_generation = match recv_within_guard(&input_rx, "initial capacity fixture turn")
        {
            Ok(CodexRuntimeCommand::Prompt { command, .. }) => command.active_turn_generation,
            _ => panic!("expected the initial prompt"),
        };
        runtime.sessions.lock().unwrap().insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("capacity-thread".to_owned()),
                active_turn_generation: Some(failed_generation),
                ..SharedCodexSessionState::default()
            },
        );
        runtime
            .thread_sessions
            .lock()
            .unwrap()
            .insert("capacity-thread".to_owned(), session_id.clone());
        let fixture = Self {
            state,
            session_id,
            runtime,
            input_rx,
            failed_generation,
            sender_id,
        };
        if !started {
            fixture
                .runtime
                .sessions
                .lock()
                .unwrap()
                .get_mut(&fixture.session_id)
                .unwrap()
                .turn_id = Some("capacity-turn".to_owned());
            return fixture;
        }
        fixture.event(json!({
            "method": "turn/started",
            "params": { "threadId": "capacity-thread", "turn": { "id": "capacity-turn" } }
        }));
        fixture.event(json!({
            "method": "item/completed",
            "params": {
                "threadId": "capacity-thread", "turnId": "capacity-turn",
                "item": { "id": "prior-answer", "type": "agentMessage", "text": "Prior work is complete." }
            }
        }));
        fixture.event(json!({
            "method": "item/completed",
            "params": {
                "threadId": "capacity-thread", "turnId": "capacity-turn",
                "item": {
                    "id": "prior-tool", "type": "commandExecution", "command": "fixture-check",
                    "aggregatedOutput": "prior tool result", "status": "completed", "exitCode": 0
                }
            }
        }));
        fixture.assert_prior_work();
        fixture
    }

    fn event(&self, message: Value) {
        handle_shared_codex_app_server_message(
            &message,
            &self.state,
            &self.runtime.runtime_id,
            &Arc::new(Mutex::new(HashMap::new())),
            &self.runtime.sessions,
            &self.runtime.thread_sessions,
            &self.runtime.input_tx,
        )
        .expect("fixture event should be handled");
    }

    fn capacity_failure(&self) {
        let turn_id = self.runtime.sessions.lock().unwrap()[&self.session_id]
            .turn_id
            .clone()
            .unwrap();
        // ErrorNotification from the installed app-server schema: willRetry
        // belongs to params; the enum is structured error.codexErrorInfo.
        self.event(json!({
            "method": "error",
            "params": {
                "threadId": "capacity-thread", "turnId": turn_id, "willRetry": false,
                "error": { "message": "Selected model is at capacity.", "codexErrorInfo": "serverOverloaded" }
            }
        }));
    }

    fn due(&self) -> chrono::DateTime<chrono::Utc> {
        let inner = self.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&self.session_id).unwrap()];
        chrono::DateTime::parse_from_rfc3339(
            record
                .provider_capacity_retry
                .as_ref()
                .unwrap()
                .due_at
                .as_ref()
                .unwrap(),
        )
        .unwrap()
        .with_timezone(&chrono::Utc)
    }

    fn fire(&self) {
        let due = self.due();
        // One tick charges and saves the attempt; the next may Resume once
        // that content is acknowledged. Neither tick waits real minutes.
        self.state.engram_abort_retry_tick(due);
        self.state.engram_abort_retry_tick(due);
        self.state.engram_abort_retry_tick(due);
    }

    fn start_recovery(&self) -> CodexPromptCommand {
        let command = match recv_within_guard(&self.input_rx, "automatic capacity Resume") {
            Ok(CodexRuntimeCommand::Prompt { command, .. }) => command,
            _ => panic!("expected a recovered prompt"),
        };
        {
            let mut sessions = self.runtime.sessions.lock().unwrap();
            let session = sessions.get_mut(&self.session_id).unwrap();
            session.active_turn_generation = Some(command.active_turn_generation);
        }
        self.event(json!({ "method": "turn/started", "params": {
            "threadId": "capacity-thread", "turn": { "id": format!("recovery-{}", command.active_turn_generation) }
        }}));
        assert!(matches!(
            self.input_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        command
    }

    fn assert_manual(&self) {
        let inner = self.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&self.session_id).unwrap()];
        assert!(record.provider_capacity_retry.is_none());
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(matches!(
            self.input_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    fn assert_prior_work(&self) {
        let snapshot = self.state.full_snapshot();
        let session = snapshot
            .sessions
            .iter()
            .find(|s| s.id == self.session_id)
            .unwrap();
        assert!(session.messages.iter().any(
            |message| matches!(message, Message::Text { author: Author::Assistant, text, .. }
                if text == "Prior work is complete.")
        ));
        assert!(session.messages.iter().any(|message|
            matches!(message, Message::Command { command, output, status: CommandStatus::Success, .. }
                if command == "fixture-check" && output == "prior tool result")));
    }
}

#[test]
fn typed_capacity_after_started_turn_waits_for_automatic_resume() {
    let fixture = CapacityFixture::started_mailbox_turn();
    fixture.capacity_failure();
    fixture.assert_prior_work();
    assert!(matches!(
        fixture.input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let snapshot = fixture.state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|s| s.id == fixture.session_id)
        .unwrap();
    // Fail-first witness uses only the existing public snapshot: the baseline
    // leaves Error/queuePaused here instead of scheduling capacity recovery.
    assert!(
        session
            .preview
            .starts_with("Waiting: provider at capacity, retry 1/8 at "),
        "typed terminal capacity must schedule recovery; actual preview: {}",
        session.preview
    );
    assert_ne!(session.status, SessionStatus::Error);
    assert!(session.provider_capacity_waiting);
}

#[test]
fn manual_resume_promotes_current_mailbox_head_as_one_new_turn() {
    let fixture = CapacityFixture::started_mailbox_turn();
    fixture.capacity_failure();
    let queued_text = {
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert_eq!(record.queued_prompts.len(), 1);
        record
            .queued_prompts
            .front()
            .unwrap()
            .pending_prompt
            .text
            .clone()
    };
    fixture
        .state
        .resume_session_queue(&fixture.session_id)
        .unwrap();
    let (session_id, command) = match recv_within_guard(&fixture.input_rx, "manual capacity Resume")
    {
        Ok(CodexRuntimeCommand::Prompt {
            session_id,
            command,
        }) => (session_id, command),
        _ => panic!("expected the resumed queue head"),
    };
    assert_eq!(session_id, fixture.session_id);
    assert_eq!(command.prompt, queued_text);
    assert!(command.active_turn_generation > fixture.failed_generation);
    assert!(matches!(
        fixture.input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    fixture.assert_prior_work();
}

#[test]
fn duplicate_capacity_terminals_preserve_the_same_schedule() {
    for error_first in [false, true] {
        let fixture = CapacityFixture::started_mailbox_turn();
        let completion = json!({ "method": "turn/completed", "params": {
            "threadId": "capacity-thread", "turn": { "id": "capacity-turn", "error": {
                "message": "Selected model is at capacity.", "codexErrorInfo": "serverOverloaded"
            }}
        }});
        if error_first {
            fixture.capacity_failure();
        } else {
            fixture.event(completion.clone());
        }
        let before = {
            let inner = fixture.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
            (
                serde_json::to_value(record.provider_capacity_retry.as_ref().unwrap()).unwrap(),
                record.session.preview.clone(),
            )
        };
        fixture.event(completion.clone());
        fixture.event(completion);
        fixture.event(json!({ "method": "error", "params": {
            "threadId": "capacity-thread", "turnId": "capacity-turn", "willRetry": false,
            "error": { "message": "Selected model is at capacity.", "codexErrorInfo": "serverOverloaded" }
        }}));
        {
            let inner = fixture.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
            assert_eq!(
                serde_json::to_value(record.provider_capacity_retry.as_ref().unwrap()).unwrap(),
                before.0
            );
            assert_eq!(record.session.preview, before.1);
            assert!(record.session.provider_capacity_waiting);
        }
        fixture.fire();
        fixture.start_recovery();
    }
}

#[test]
fn an_unbound_capacity_error_cannot_terminalize_an_unarmed_turn() {
    let fixture = CapacityFixture::mailbox_turn(false);
    fixture
        .runtime
        .sessions
        .lock()
        .unwrap()
        .get_mut(&fixture.session_id)
        .unwrap()
        .turn_id = None;
    fixture.event(json!({ "method": "error", "params": {
        "threadId": "capacity-thread", "turnId": "capacity-turn", "willRetry": false,
        "error": { "message": "Selected model is at capacity.", "codexErrorInfo": "serverOverloaded" }
    }}));
    let snapshot = fixture.state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == fixture.session_id)
        .unwrap();
    assert_eq!(session.status, SessionStatus::Active);
    assert!(!session.preview.contains("Selected model is at capacity"));
    // Neither a handshake nor an established turn associates this event.
    let inner = fixture.state.inner.lock().unwrap();
    assert!(
        inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()]
            .provider_capacity_retry
            .is_none()
    );
    assert!(fixture.input_rx.try_recv().is_err());
}

#[test]
fn a_delayed_receipt_ack_cannot_outlive_capacity_cancellation_or_settings() {
    use crate::tests::prompt_delivery::{DeliveryWriter, HeldFence};
    for change in ["control", "tail_cancel", "settings"] {
        let mut fixture = CapacityFixture::started_mailbox_turn();
        fixture.capacity_failure();
        let writer = DeliveryWriter::attach(&mut fixture.state);
        let (head_id, tail_id) = {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.session_id).unwrap();
            let record = &mut inner.sessions[index];
            let id = record
                .queued_prompts
                .front()
                .unwrap()
                .pending_prompt
                .id
                .clone();
            let mut tail = record.queued_prompts.front().unwrap().clone();
            tail.pending_prompt.id = Uuid::new_v4().to_string();
            tail.source = QueuedPromptSource::User;
            let tail_id = tail.pending_prompt.id.clone();
            record.queued_prompts.push_back(tail);
            let sequence = next_prompt_delivery_sequence(record);
            record.prompt_deliveries.insert(
                id.clone(),
                PromptDelivery {
                    federation_operation: None,
                    federation_handoff_started: false,
                    disposition: PromptDeliveryDisposition::Pending,
                    attempt: 0,
                    write_ahead: Uuid::new_v4().to_string(),
                    sequence,
                    message_id: None,
                    turn_generation: None,
                    attempt_key: None,
                    reason: None,
                    entry: None,
                    write_ahead_acknowledged: false,
                    resume: None,
                    runtime_write: PromptRuntimeWrite::Untracked,
                },
            );
            sync_prompt_delivery_view(record);
            fixture.state.commit_locked(&mut inner).unwrap();
            (id, tail_id)
        };
        let (nonce, owner) = {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.session_id).unwrap();
            let record = &mut inner.sessions[index];
            let retry = record.provider_capacity_retry.as_mut().unwrap();
            retry.recovering = true;
            retry.dispatched = true;
            retry.attempts = 1;
            retry.due_at = None;
            let nonce = retry.nonce.clone();
            record.session.status = SessionStatus::Error;
            record.session.provider_capacity_waiting = false;
            let mut owner = EngramQueuedAdmissionOwner::capture(record).unwrap();
            owner.capacity_nonce = Some(nonce.clone());
            (nonce, owner)
        };
        assert!(matches!(
            fixture
                .state
                .resume_local_session_queue_inner(&fixture.session_id, Some(owner), Some(&nonce),)
                .unwrap(),
            Some(TurnDispatchDeliveryOutcome::Scheduled)
        ));
        let fence = writer.take(HeldFence::WriteAhead);
        let write_ahead = {
            let inner = fixture.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
            let receipt = &record.prompt_deliveries[&head_id];
            assert!(receipt.resume.is_some());
            receipt.write_ahead.clone()
        };
        match change {
            "tail_cancel" => {
                fixture
                    .state
                    .cancel_queued_prompt(&fixture.session_id, &tail_id)
                    .unwrap();
            }
            "settings" => {
                let mut inner = fixture.state.inner.lock().unwrap();
                let index = inner.find_session_index(&fixture.session_id).unwrap();
                inner.sessions[index].session.model = "another-model".to_owned();
                drop(inner);
                fixture
                    .state
                    .provider_capacity_retry_tick(chrono::Utc::now());
            }
            _ => {}
        }
        // Deliver the exact delayed ACK through its production settlement seam
        // synchronously, so assertions observe its dispatch decision.
        fixture.state.settle_prompt_delivery_write_ahead(
            &fixture.session_id,
            &head_id,
            &write_ahead,
            Ok(()),
        );
        if change == "control" {
            fixture.start_recovery();
        } else {
            fixture.assert_manual();
            let inner = fixture.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
            assert_eq!(
                record.queued_prompts.front().unwrap().pending_prompt.id,
                head_id
            );
            assert_eq!(
                record.prompt_deliveries[&head_id].disposition,
                PromptDeliveryDisposition::Pending
            );
        }
        drop(fence);
        drop(writer);
    }
}

#[test]
fn automatic_resume_matches_manual_head_once_and_success_clears_budget() {
    let fixture = CapacityFixture::started_mailbox_turn();
    fixture.capacity_failure();
    let queued = {
        let inner = fixture.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()]
            .queued_prompts
            .front()
            .unwrap()
            .pending_prompt
            .text
            .clone()
    };
    fixture
        .state
        .engram_abort_retry_tick(fixture.due() - chrono::Duration::milliseconds(1));
    assert!(matches!(
        fixture.input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    fixture.fire();
    let command = fixture.start_recovery();
    assert_eq!(command.prompt, queued);
    assert!(command.active_turn_generation > fixture.failed_generation);
    fixture.assert_prior_work();
    fixture.event(json!({ "method": "turn/completed", "params": {
        "threadId": "capacity-thread", "turn": { "id": format!("recovery-{}", command.active_turn_generation), "error": null }
    }}));
    let inner = fixture.state.inner.lock().unwrap();
    assert!(
        inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()]
            .provider_capacity_retry
            .is_none()
    );
}

#[test]
fn minute_schedule_and_eight_attempt_budget_never_allow_a_ninth() {
    for attempt in 0..8 {
        let delay = provider_capacity_delay("cadence", attempt).num_milliseconds();
        let base = PROVIDER_CAPACITY_DELAYS_SECONDS[attempt] as i64 * 1000;
        assert!((base * 80 / 100..=base * 120 / 100).contains(&delay));
        assert!(delay >= 60_000);
    }
    assert_eq!(ENGRAM_ABORT_RETRY_DELAYS_SECONDS, [2, 5, 10, 20, 30, 60]);
    let fixture = CapacityFixture::started_mailbox_turn();
    fixture.capacity_failure();
    for expected_attempt in 1..=8 {
        fixture.fire();
        fixture.start_recovery();
        fixture.capacity_failure();
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
        assert_eq!(
            record.provider_capacity_retry.as_ref().unwrap().attempts,
            expected_attempt
        );
    }
    fixture
        .state
        .engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::hours(3));
    let inner = fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert_eq!(record.session.status, SessionStatus::Error);
    assert_eq!(
        record.session.preview,
        "Paused: provider at capacity, retries exhausted (8 attempts, last error serverOverloaded); Resume to retry"
    );
    assert!(
        record
            .provider_capacity_retry
            .as_ref()
            .unwrap()
            .due_at
            .is_none()
    );
    assert!(matches!(
        fixture.input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn empty_queue_only_unpauses_and_preserves_manual_error_preview() {
    let fixture = CapacityFixture::started_mailbox_turn();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session_id).unwrap();
        inner.sessions[index].active_turn_mailbox_notification = None;
    }
    fixture.capacity_failure();
    fixture.fire();
    let inner = fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
    assert!(!record.orchestrator_auto_dispatch_blocked);
    assert!(record.provider_capacity_retry.is_none());
    assert_eq!(record.session.status, SessionStatus::Error);
    assert_eq!(record.session.preview, "Selected model is at capacity.");
    assert!(!record.session.provider_capacity_waiting);
    assert!(matches!(
        fixture.input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn untyped_prose_and_quota_errors_stay_manual() {
    for code in [
        None,
        Some("usageLimitExceeded"),
        Some("rateLimitExceeded"),
        Some("flexUnavailable"),
    ] {
        let fixture = CapacityFixture::started_mailbox_turn();
        fixture.event(json!({"method": "error", "params": {
            "threadId": "capacity-thread", "turnId": "capacity-turn", "willRetry": false,
            "error": { "message": "Selected model is at capacity.", "codexErrorInfo": code }
        }}));
        fixture.assert_manual();
    }
}

#[test]
fn provider_owned_retry_and_stale_turn_or_generation_never_start_capacity_recovery() {
    for boundary in [
        "provider-retry",
        "stale-turn",
        "stale-generation",
        "stale-runtime",
    ] {
        let fixture = CapacityFixture::started_mailbox_turn();
        if boundary == "stale-generation" {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.session_id).unwrap();
            inner.sessions[index].active_turn_generation += 1;
        }
        if boundary == "stale-runtime" {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.session_id).unwrap();
            inner.sessions[index].runtime = SessionRuntime::None;
        }
        fixture.event(json!({ "method": "error", "params": {
            "threadId": "capacity-thread", "turnId": if boundary == "stale-turn" { "other-turn" } else { "capacity-turn" },
            "willRetry": boundary == "provider-retry",
            "error": { "message": "capacity", "codexErrorInfo": "serverOverloaded" }
        }}));
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
        assert!(record.provider_capacity_retry.is_none());
        assert_eq!(record.session.status, SessionStatus::Active);
        assert!(matches!(
            fixture.input_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }
}

#[test]
fn unknown_delivery_and_remote_compaction_keep_manual_recovery() {
    for boundary in [
        "unknown-begin",
        "not-started",
        "compaction-start",
        "compaction-end",
    ] {
        let fixture = CapacityFixture::mailbox_turn(boundary != "not-started");
        match boundary {
            "unknown-begin" => {
                let mut inner = fixture.state.inner.lock().unwrap();
                let index = inner.find_session_index(&fixture.session_id).unwrap();
                inner.sessions[index].engram.uncertain_grant_id = Some("unknown-delivery".to_owned());
            }
            "not-started" => {}
            _ => fixture.event(json!({ "method": if boundary == "compaction-start" { "item/started" } else { "item/completed" }, "params": {
                "threadId": "capacity-thread", "turnId": "capacity-turn", "item": { "id": "compact", "type": "contextCompaction" }
            }})),
        }
        fixture.capacity_failure();
        fixture.assert_manual();
    }
}

#[test]
fn manual_resume_stop_cancel_and_changed_owner_fence_schedule() {
    for boundary in ["resume", "stop", "cancel", "pause", "runtime", "settings"] {
        let fixture = CapacityFixture::started_mailbox_turn();
        fixture.capacity_failure();
        let due = fixture.due();
        match boundary {
            "resume" => {
                fixture
                    .state
                    .resume_session_queue(&fixture.session_id)
                    .unwrap();
                fixture.start_recovery();
            }
            "stop" => {
                fixture
                    .state
                    .begin_requested_stop_session(
                        &fixture.session_id,
                        &StopSessionOptions::default(),
                    )
                    .unwrap();
            }
            "cancel" => {
                let prompt_id = {
                    let inner = fixture.state.inner.lock().unwrap();
                    inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()]
                        .queued_prompts
                        .front()
                        .unwrap()
                        .pending_prompt
                        .id
                        .clone()
                };
                fixture
                    .state
                    .cancel_queued_prompt(&fixture.session_id, &prompt_id)
                    .unwrap();
            }
            _ => {
                let mut inner = fixture.state.inner.lock().unwrap();
                let index = inner.find_session_index(&fixture.session_id).unwrap();
                match boundary {
                    "pause" => inner.sessions[index].set_auto_dispatch_blocked(true),
                    "runtime" => inner.sessions[index].runtime = SessionRuntime::None,
                    _ => inner.sessions[index].session.model = "changed-model".to_owned(),
                }
            }
        }
        fixture.state.engram_abort_retry_tick(due);
        let inner = fixture.state.inner.lock().unwrap();
        assert!(
            inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()]
                .provider_capacity_retry
                .is_none()
        );
        assert!(matches!(
            fixture.input_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }
}

#[test]
fn capacity_mailbox_disposition_shared_cap_and_pending_ack_hold() {
    let fixture = CapacityFixture::started_mailbox_turn();
    fixture.capacity_failure();
    let previous_retry = {
        let inner = fixture.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()]
            .provider_capacity_retry
            .clone()
            .unwrap()
    };
    assert_eq!(
        fixture
            .state
            .mailbox_queued_wake_disposition(&fixture.session_id),
        "waitingForProviderCapacity"
    );
    let receipt = fixture
        .state
        .append_mailbox_message_and_notify(
            &fixture.sender_id,
            SendMailboxMessageRequest {
                target_session_id: fixture.session_id.clone(),
                message: "New wake during capacity wait".to_owned(),
                idempotency_key: "during-capacity-wait".to_owned(),
                topic: None,
                state_stamp: None,
                class: Some("routine".to_owned()),
            },
        )
        .unwrap();
    assert_eq!(
        receipt.notification_disposition,
        "waitingForProviderCapacity"
    );
    {
        let inner = fixture.state.inner.lock().unwrap();
        let retry = inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()]
            .provider_capacity_retry
            .as_ref()
            .unwrap();
        assert_eq!(retry.nonce, previous_retry.nonce);
        assert_eq!(retry.due_at, previous_retry.due_at);
        assert_eq!(retry.attempts, 0);
        assert!(matches!(
            fixture.input_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }
    let slots = (0..ENGRAM_RETRY_MAX_IN_FLIGHT)
        .map(|_| fixture.state.engram_retry_slots.try_acquire().unwrap())
        .collect::<Vec<_>>();
    let due = fixture.due();
    fixture.state.engram_abort_retry_tick(due);
    {
        let inner = fixture.state.inner.lock().unwrap();
        assert_eq!(
            inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()]
                .provider_capacity_retry
                .as_ref()
                .unwrap()
                .attempts,
            0
        );
    }
    drop(slots);
    fixture.fire();
    fixture.start_recovery();

    let mut pending = CapacityFixture::started_mailbox_turn();
    let (persist_tx, _persist_rx) = mpsc::channel();
    pending.state.persist_tx = persist_tx;
    pending.capacity_failure();
    pending.fire();
    let inner = pending.state.inner.lock().unwrap();
    assert_eq!(
        inner.sessions[inner.find_session_index(&pending.session_id).unwrap()]
            .provider_capacity_retry
            .as_ref()
            .unwrap()
            .attempts,
        0
    );
    assert!(matches!(
        pending.input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn stopped_capacity_owner_cannot_dispatch_after_off_lock_resume() {
    let fixture = CapacityFixture::started_mailbox_turn();
    fixture.capacity_failure();
    fixture.state.engram_abort_retry_tick(fixture.due());
    let (nonce, owner) = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session_id).unwrap();
        let record = &mut inner.sessions[index];
        let retry = record.provider_capacity_retry.as_mut().unwrap();
        retry.dispatched = true;
        let nonce = retry.nonce.clone();
        record.session.status = SessionStatus::Error;
        record.session.preview = retry.original_preview.clone();
        let mut owner = EngramQueuedAdmissionOwner::capture(record).unwrap();
        owner.capacity_nonce = Some(nonce.clone());
        (nonce, owner)
    };
    fixture
        .state
        .begin_requested_stop_session(&fixture.session_id, &StopSessionOptions::default())
        .unwrap();
    assert!(matches!(
        fixture
            .state
            .resume_local_session_queue_inner(&fixture.session_id, Some(owner), Some(&nonce))
            .unwrap(),
        Some(TurnDispatchDeliveryOutcome::Superseded)
    ));
    let inner = fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(record.provider_capacity_retry.is_none());
    assert!(matches!(
        fixture.input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn terminal_turn_error_schedules_and_restart_leaves_manual_hold() {
    let fixture = CapacityFixture::started_mailbox_turn();
    fixture.event(json!({ "method": "turn/completed", "params": {
        "threadId": "capacity-thread", "turn": { "id": "capacity-turn",
            "error": { "message": "capacity", "codexErrorInfo": "serverOverloaded" }
        }
    }}));
    let persisted = {
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
        assert!(record.provider_capacity_retry.is_some());
        PersistedSessionRecord::from_record(record)
    };
    let restored = persisted.into_record().unwrap();
    assert!(restored.provider_capacity_retry.is_none());
    assert!(restored.orchestrator_auto_dispatch_blocked);
    assert_eq!(restored.session.status, SessionStatus::Error);
    assert!(
        !restored
            .session
            .preview
            .starts_with("Waiting: provider at capacity,")
    );
}

#[test]
fn peer_wake_preserves_capacity_schedule_and_manual_prompt_cancels_it() {
    let fixture = CapacityFixture::started_mailbox_turn();
    fixture.capacity_failure();
    let due = fixture.due();
    let peer = SendMessageRequest {
        text: "Peer wake during capacity wait".to_owned(),
        expanded_text: None,
        attachments: Vec::new(),
        source_session_id: Some(fixture.sender_id.clone()),
        source_mailbox: None,
    };
    assert!(matches!(
        fixture
            .state
            .dispatch_turn(&fixture.session_id, peer)
            .unwrap(),
        DispatchTurnResult::Queued
    ));
    assert_eq!(fixture.due(), due);
    assert!(matches!(
        fixture.input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    fixture
        .state
        .dispatch_turn(
            &fixture.session_id,
            SendMessageRequest {
                text: "Manual replacement".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .unwrap();
    let inner = fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
    assert!(record.provider_capacity_retry.is_none());
    assert!(!record.session.provider_capacity_waiting);
}

#[test]
fn peer_without_capacity_hold_keeps_existing_manual_priority() {
    let fixture = CapacityFixture::started_mailbox_turn();
    fixture.event(json!({ "method": "error", "params": {
        "threadId": "capacity-thread", "turnId": "capacity-turn", "willRetry": false,
        "error": { "message": "ordinary failure" }
    }}));
    fixture.assert_manual();
    let result = fixture
        .state
        .dispatch_turn(
            &fixture.session_id,
            SendMessageRequest {
                text: "Existing peer priority".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: Some(fixture.sender_id.clone()),
                source_mailbox: None,
            },
        )
        .unwrap();
    let dispatch = match result {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        _ => panic!("peer without capacity hold should retain today's immediate priority"),
    };
    deliver_turn_dispatch(&fixture.state, dispatch).expect("peer dispatch accepted");
    let command = match recv_within_guard(&fixture.input_rx, "ordinary peer priority") {
        Ok(CodexRuntimeCommand::Prompt { command, .. }) => command,
        _ => panic!("expected the ordinary peer prompt"),
    };
    assert!(command.prompt.contains("Existing peer priority"));
    assert!(command.active_turn_generation > fixture.failed_generation);
    assert!(matches!(
        fixture.input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}
