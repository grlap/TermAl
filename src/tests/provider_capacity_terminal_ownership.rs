//! Retired capacity terminals cannot settle a later unresolved request.
use super::*;
use crate::tests::prompt_delivery::{DeliveryWriter, HeldFence};

fn old_completion() -> Value {
    json!({ "method": "turn/completed", "params": {
        "threadId": "capacity-thread", "turn": { "id": "capacity-turn", "error": {
            "message": "Selected model is at capacity.", "codexErrorInfo": "serverOverloaded"
        }}
    }})
}

#[test]
fn a_stale_shared_generation_cannot_start_or_terminalize_the_current_turn() {
    let fixture = CapacityFixture::started_mailbox_turn();
    let (cancel_tx, cancel_rx) = mpsc::channel();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session_id).unwrap();
        inner.sessions[index].active_turn_generation = fixture.failed_generation + 1;
    }
    {
        let mut sessions = fixture.runtime.sessions.lock().unwrap();
        let shared = sessions.get_mut(&fixture.session_id).unwrap();
        shared.pending_turn_start_request_id = Some("old-request".to_owned());
        shared.turn_started_watchdog_cancel_tx = Some(cancel_tx);
    }
    let before = fixture.state.full_snapshot();
    fixture.event(json!({"method":"turn/started", "params":{
        "threadId":"capacity-thread", "turn":{"id":"capacity-turn"}}}));
    fixture.event(old_completion());
    let after = fixture.state.full_snapshot();
    assert_eq!(
        serde_json::to_value(&before.sessions).unwrap(),
        serde_json::to_value(&after.sessions).unwrap()
    );
    let sessions = fixture.runtime.sessions.lock().unwrap();
    let shared = &sessions[&fixture.session_id];
    assert_eq!(
        shared.pending_turn_start_request_id.as_deref(),
        Some("old-request")
    );
    assert!(shared.turn_started_watchdog_cancel_tx.is_some());
    assert!(shared.retired_turn_ids.is_empty());
    assert!(matches!(
        cancel_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(fixture.input_rx.try_recv().is_err());
}

#[test]
fn an_unusable_matching_reply_surfaces_a_hold_without_applying_a_terminal_candidate() {
    let fixture = CapacityFixture::mailbox_turn(false);
    let (_request, response) = arm_real_request(&fixture);
    fixture.event(
        json!({"method":"turn/started", "params":{"threadId":"capacity-thread", "turn":{}}}),
    );
    let candidate = json!({"method":"turn/completed", "params":{
        "threadId":"capacity-thread", "turn":{"id":"unproven-B", "error":null}}});
    fixture.event(candidate.clone());
    let mut events = fixture
        .state
        .state_broadcast_senders
        .state_events
        .subscribe();
    response.send(Ok(json!({"turn":{}}))).unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(crate::tests::phase_sync::DEADLOCK_GUARD, async {
                loop {
                    let held = {
                        let inner = fixture.state.inner.lock().unwrap();
                        inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()]
                            .codex_terminal_identity_unresolved
                    };
                    if held {
                        break;
                    }
                    events
                        .recv()
                        .await
                        .expect("unresolved identity publication");
                }
            })
            .await
            .expect("unusable reply surfaces manual hold");
        });
    let before = fixture.state.full_snapshot();
    fixture.event(candidate);
    let after = fixture.state.full_snapshot();
    assert_eq!(
        serde_json::to_value(&before.sessions).unwrap(),
        serde_json::to_value(&after.sessions).unwrap()
    );
    let session = after
        .sessions
        .iter()
        .find(|session| session.id == fixture.session_id)
        .unwrap();
    assert_eq!(session.status, SessionStatus::Active);
    assert!(session.queue_paused);
    assert!(session.preview.contains("terminal identity unresolved"));
    assert!(fixture.input_rx.try_recv().is_err());
}

#[test]
fn an_explicit_terminal_after_real_idless_reconciliation_is_a_visible_manual_hold() {
    let fixture = CapacityFixture::mailbox_turn(false);
    {
        // The state a successful id-less response reconciliation establishes.
        let mut sessions = fixture.runtime.sessions.lock().unwrap();
        let shared = sessions.get_mut(&fixture.session_id).unwrap();
        shared.turn_id = None;
        shared.turn_started = true;
        assert_eq!(
            shared.active_turn_generation,
            Some(fixture.failed_generation)
        );
        assert!(shared.pending_turn_start_request_id.is_none());
    }
    fixture.event(old_completion());
    let inner = fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
    assert!(record.codex_terminal_identity_unresolved);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(
        record
            .session
            .preview
            .contains("terminal identity unresolved")
    );
    assert!(record.provider_capacity_retry.is_none());
    assert!(fixture.input_rx.try_recv().is_err());
}

#[test]
fn a_valid_explicit_start_applies_its_buffered_terminal_once() {
    let fixture = CapacityFixture::mailbox_turn(false);
    let (_request, _response) = arm_real_request(&fixture);
    let terminal = json!({"method":"turn/completed", "params":{
        "threadId":"capacity-thread", "turn":{"id":"actual-B", "error":null}}});
    fixture.event(terminal.clone());
    fixture.event(terminal.clone());
    assert_eq!(
        fixture.runtime.sessions.lock().unwrap()[&fixture.session_id]
            .deferred_terminals
            .len(),
        1
    );
    fixture.event(json!({"method":"turn/started", "params":{
        "threadId":"capacity-thread", "turn":{"id":"actual-B"}}}));
    let snapshot = fixture.state.full_snapshot();
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|s| s.id == fixture.session_id)
            .unwrap()
            .status,
        SessionStatus::Idle
    );
    fixture.event(terminal);
    assert!(
        fixture.runtime.sessions.lock().unwrap()[&fixture.session_id]
            .deferred_terminals
            .is_empty()
    );
    assert!(fixture.input_rx.try_recv().is_err());
}

#[test]
fn conflicting_or_excessive_unbound_terminals_hold_without_settling_the_request() {
    for mode in ["conflict", "count", "bytes"] {
        let fixture = CapacityFixture::mailbox_turn(false);
        let (request, _response) = arm_real_request(&fixture);
        let count = if mode == "count" {
            CODEX_DEFERRED_TERMINAL_LIMIT + 1
        } else {
            2
        };
        for index in 0..count {
            let id = if mode == "count" {
                format!("candidate-{index}")
            } else {
                "candidate".to_owned()
            };
            let detail = if mode == "bytes" {
                "x".repeat(CODEX_DEFERRED_TERMINAL_BYTES)
            } else {
                format!("error-{index}")
            };
            fixture.event(json!({"method":"turn/completed", "params":{
                "threadId":"capacity-thread", "turn":{"id":id, "error":{"message":detail}}}}));
        }
        let sessions = fixture.runtime.sessions.lock().unwrap();
        let shared = &sessions[&fixture.session_id];
        assert!(shared.terminal_identity_unresolved);
        assert!(shared.deferred_terminals.is_empty());
        assert_eq!(
            shared.pending_turn_start_request_id.as_deref(),
            Some(request.as_str())
        );
        drop(sessions);
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
        assert!(record.codex_terminal_identity_unresolved);
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(
            record
                .session
                .preview
                .contains("terminal identity unresolved")
        );
        assert!(record.provider_capacity_retry.is_none());
        drop(inner);
        assert!(
            fixture
                .state
                .resume_local_session_queue(&fixture.session_id, None)
                .is_err()
        );
        assert!(fixture.input_rx.try_recv().is_err());
    }
}

fn arm_real_request(
    fixture: &CapacityFixture,
) -> (
    String,
    mpsc::Sender<std::result::Result<Value, CodexResponseError>>,
) {
    let pending: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut bytes = Vec::new();
    handle_shared_codex_start_turn(
        &mut bytes,
        &pending,
        &fixture.state,
        &fixture.runtime.runtime_id,
        &fixture.runtime.sessions,
        &fixture.runtime.thread_sessions,
        None,
        &fixture.session_id,
        "capacity-thread",
        None,
        CodexPromptCommand {
            active_turn_generation: fixture.failed_generation,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: fixture
                .state
                .full_snapshot()
                .sessions
                .iter()
                .find(|session| session.id == fixture.session_id)
                .unwrap()
                .workdir
                .clone(),
            model: "gpt-5.4".to_owned(),
            prompt: "owned request".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
            delivery_receipt: None,
        },
    )
    .unwrap();
    assert_eq!(
        bytes.iter().filter(|byte| **byte == b'\n').count(),
        1,
        "one provider request"
    );
    take_pending_codex_request(&pending)
}

fn await_idle(fixture: &CapacityFixture, events: &mut tokio::sync::broadcast::Receiver<String>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(crate::tests::phase_sync::DEADLOCK_GUARD, async {
                loop {
                    let idle = {
                        let inner = fixture.state.inner.lock().unwrap();
                        inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()]
                            .session
                            .status
                            == SessionStatus::Idle
                    };
                    if idle {
                        break;
                    }
                    events.recv().await.expect("terminal state publication");
                }
            })
            .await
            .expect("the matching response applies the retained terminal");
        });
}

#[test]
fn an_idless_starts_only_completion_waits_for_its_matching_response() {
    let fixture = CapacityFixture::mailbox_turn(false);
    let (request, response) = arm_real_request(&fixture);
    fixture.event(
        json!({"method":"turn/started", "params":{"threadId":"capacity-thread", "turn":{}}}),
    );
    let completion = json!({"method":"turn/completed", "params":{"threadId":"capacity-thread", "turn":{"id":"actual-B", "error":null}}});
    fixture.event(completion.clone());
    {
        let inner = fixture.state.inner.lock().unwrap();
        assert_eq!(
            inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()]
                .session
                .status,
            SessionStatus::Active,
            "an unbound terminal cannot complete the request"
        );
    }
    assert_eq!(
        fixture.runtime.sessions.lock().unwrap()[&fixture.session_id]
            .pending_turn_start_request_id
            .as_deref(),
        Some(request.as_str())
    );
    let mut events = fixture
        .state
        .state_broadcast_senders
        .state_events
        .subscribe();
    response
        .send(Ok(json!({"turn":{"id":"actual-B"}})))
        .unwrap();
    await_idle(&fixture, &mut events);
    fixture.event(completion);
    assert!(
        fixture.input_rx.try_recv().is_err(),
        "deferred notifications never resend provider work"
    );
}

#[test]
fn a_provisional_item_id_cannot_prove_its_own_terminal() {
    let fixture = CapacityFixture::mailbox_turn(false);
    let (request, response) = arm_real_request(&fixture);
    fixture.event(
        json!({"method":"turn/started", "params":{"threadId":"capacity-thread", "turn":{}}}),
    );
    fixture.event(json!({"method":"item/completed", "params":{
        "threadId":"capacity-thread", "turnId":"never-recorded-stale-id",
        "item":{"type":"agentMessage", "id":"provisional-item", "text":"candidate routing id"}
    }}));
    fixture.event(
        json!({"method":"turn/started", "params":{"threadId":"capacity-thread", "turn":{}}}),
    );
    fixture.event(json!({"method":"turn/completed", "params":{"threadId":"capacity-thread", "turn":{
        "id":"never-recorded-stale-id", "error":{"message":"stale capacity", "codexErrorInfo":"serverOverloaded"}
    }}}));
    {
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
        assert_eq!(record.session.status, SessionStatus::Active);
        assert!(record.provider_capacity_retry.is_none());
    }
    assert_eq!(
        fixture.runtime.sessions.lock().unwrap()[&fixture.session_id]
            .pending_turn_start_request_id
            .as_deref(),
        Some(request.as_str())
    );
    fixture.event(json!({"method":"turn/completed", "params":{"threadId":"capacity-thread", "turn":{"id":"actual-B", "error":null}}}));
    let mut events = fixture
        .state
        .state_broadcast_senders
        .state_events
        .subscribe();
    response
        .send(Ok(json!({"turn":{"id":"actual-B"}})))
        .unwrap();
    await_idle(&fixture, &mut events);
    let inner = fixture.state.inner.lock().unwrap();
    assert!(
        inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()]
            .provider_capacity_retry
            .is_none()
    );
    assert!(fixture.input_rx.try_recv().is_err());
}

#[test]
fn an_old_capacity_terminal_cannot_settle_a_new_armed_receipted_request() {
    let mut fixture = CapacityFixture::started_mailbox_turn();
    fixture.capacity_failure();
    let writer = DeliveryWriter::attach(&mut fixture.state);
    let (head_id, nonce, owner) = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session_id).unwrap();
        let record = &mut inner.sessions[index];
        let head_id = record
            .queued_prompts
            .front()
            .unwrap()
            .pending_prompt
            .id
            .clone();
        let sequence = next_prompt_delivery_sequence(record);
        record.prompt_deliveries.insert(
            head_id.clone(),
            PromptDelivery {
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
        let retry = record.provider_capacity_retry.as_mut().unwrap();
        retry.recovering = true;
        retry.dispatched = true;
        retry.attempts = 1;
        retry.due_at = None;
        let nonce = retry.nonce.clone();
        record.session.status = SessionStatus::Error;
        record.session.provider_capacity_waiting = false;
        sync_prompt_delivery_view(record);
        let mut owner = EngramQueuedAdmissionOwner::capture(record).unwrap();
        owner.capacity_nonce = Some(nonce.clone());
        (head_id, nonce, owner)
    };
    assert!(matches!(
        fixture
            .state
            .resume_local_session_queue_inner(&fixture.session_id, Some(owner), Some(&nonce),)
            .unwrap(),
        Some(TurnDispatchDeliveryOutcome::Scheduled)
    ));
    writer.take(HeldFence::WriteAhead).finish(Ok(()));
    let generation = match recv_within_guard(&fixture.input_rx, "receipted recovery promoted") {
        Ok(CodexRuntimeCommand::Prompt { command, .. }) => command.active_turn_generation,
        _ => panic!("expected the recovery prompt"),
    };
    let (watchdog_tx, watchdog_rx) = mpsc::channel();
    {
        let mut sessions = fixture.runtime.sessions.lock().unwrap();
        let shared = sessions.get_mut(&fixture.session_id).unwrap();
        // The writer's real arming seam, before response or turn/started
        // supplies the new turn id (codex.rs).
        clear_shared_codex_turn_session_state(shared);
        shared.pending_turn_start_request_id = Some("armed-recovery-request".to_owned());
        shared.active_turn_generation = Some(generation);
        shared.turn_started_watchdog_cancel_tx = Some(watchdog_tx);
    }
    fixture.event(old_completion());
    fixture.event(json!({ "method": "error", "params": {
        "threadId": "capacity-thread", "turnId": "capacity-turn", "willRetry": false,
        "error": { "message": "old capacity failure", "codexErrorInfo": "serverOverloaded" }
    }}));
    {
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
        assert_eq!(
            record.session.status,
            SessionStatus::Active,
            "old A must not terminate B"
        );
        assert_eq!(record.active_turn_generation, generation);
        assert_eq!(
            record.prompt_deliveries[&head_id].disposition,
            PromptDeliveryDisposition::HandingOff,
            "A proves nothing about B's delivery"
        );
        assert_eq!(
            record.provider_capacity_retry.as_ref().unwrap().nonce,
            nonce
        );
    }
    {
        let sessions = fixture.runtime.sessions.lock().unwrap();
        let shared = &sessions[&fixture.session_id];
        assert_eq!(
            shared.pending_turn_start_request_id.as_deref(),
            Some("armed-recovery-request")
        );
        assert!(shared.turn_started_watchdog_cancel_tx.is_some());
        assert!(!shared.turn_started);
    }
    assert!(matches!(
        watchdog_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    // Genuine B events must still settle the receipt and advance its budget.
    fixture.event(json!({ "method": "turn/started", "params": {
        "threadId": "capacity-thread", "turn": { "id": "recovery-turn-B" }
    }}));
    fixture.capacity_failure();
    let inner = fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()];
    assert_eq!(
        record.prompt_deliveries[&head_id].disposition,
        PromptDeliveryDisposition::Accepted
    );
    assert_eq!(record.provider_capacity_retry.as_ref().unwrap().attempts, 1);
    assert!(record.session.provider_capacity_waiting);
    assert!(fixture.input_rx.try_recv().is_err());
    drop(inner);
    drop(writer);
}

#[test]
fn retired_capacity_terminals_stay_retired_after_operator_actions() {
    for action in ["stop", "cancel", "empty_resume"] {
        let fixture = CapacityFixture::started_mailbox_turn();
        fixture.capacity_failure();
        if action == "stop" {
            fixture
                .state
                .begin_requested_stop_session(&fixture.session_id, &StopSessionOptions::default())
                .unwrap();
        } else {
            let head = {
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
                .cancel_queued_prompt(&fixture.session_id, &head)
                .unwrap();
            if action == "empty_resume" {
                fixture
                    .state
                    .resume_session_queue(&fixture.session_id)
                    .unwrap();
            }
        }
        let before = fixture.state.full_snapshot();
        fixture.event(old_completion());
        fixture.event(old_completion());
        let after = fixture.state.full_snapshot();
        let previous = before
            .sessions
            .iter()
            .find(|session| session.id == fixture.session_id)
            .unwrap();
        let current = after
            .sessions
            .iter()
            .find(|session| session.id == fixture.session_id)
            .unwrap();
        assert_eq!(current.status, previous.status, "{action}");
        assert_eq!(current.preview, previous.preview, "{action}");
        assert_eq!(
            current.messages.len(),
            previous.messages.len(),
            "no duplicate terminal message: {action}"
        );
        let inner = fixture.state.inner.lock().unwrap();
        assert!(
            inner.sessions[inner.find_session_index(&fixture.session_id).unwrap()]
                .provider_capacity_retry
                .is_none(),
            "no fresh budget after {action}"
        );
        assert!(fixture.input_rx.try_recv().is_err());
    }
}
