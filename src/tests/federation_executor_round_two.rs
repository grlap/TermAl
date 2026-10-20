// Production handoff/recovery regressions. SQL history and reload conversion
// below are fixture boundaries, not authenticated history or OS-crash proof.
use super::*;

pub(super) fn idle_channel(fixture: &Fixture, label: &str) -> mpsc::Receiver<ClaudeRuntimeCommand> {
    let (runtime, input) = test_claude_runtime_handle(label);
    let mut inner = fixture.satellite.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.native).unwrap();
    let record = &mut inner.sessions[index];
    record.session.agent = Agent::Claude;
    record.session.claude_approval_mode = Some(default_claude_approval_mode());
    record.session.claude_effort = Some(default_claude_effort());
    record.session.approval_policy = None;
    record.session.reasoning_effort = None;
    record.session.sandbox_mode = None;
    record.session.codex_thread_state = None;
    record.runtime = SessionRuntime::Claude(runtime);
    record.session.status = SessionStatus::Idle;
    record.orchestrator_auto_dispatch_blocked = false;
    fixture.satellite.commit_locked(&mut inner).unwrap();
    input
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_two_accepted_receipt_cannot_intercept_a_reused_turn_after_reload() {
    let fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let (token, sender, input) = prepare_channel(&fixture, "before-reload", 1);
    handoff_prepared_turn_dispatch(&fixture.satellite,
        channel_dispatch(&fixture, &token, &sender, 1)).unwrap();
    assert!(matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))));
    fixture.satellite.note_prompt_delivery_outcome(&fixture.native, Some(1), PromptDeliveryOutcome::Accepted);
    let (runtime, ordinary) = test_claude_runtime_handle("ordinary-after-reload");
    let token = RuntimeToken::Claude(runtime.runtime_id.clone());
    let sender = runtime.input_tx.clone();
    let reloaded = {
        let inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        PersistedSessionRecord::from_record(&inner.sessions[index])
    }.into_record().unwrap();
    assert_eq!(reloaded.active_turn_generation, 0);
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        inner.sessions[index] = reloaded;
        let record = &mut inner.sessions[index];
        // The first ordinary turn reuses the persisted receipt's generation.
        record.active_turn_generation += 1;
        record.session.status = SessionStatus::Active;
        record.runtime = SessionRuntime::Claude(runtime);
    }
    let result = handoff_prepared_turn_dispatch(&fixture.satellite,
        channel_dispatch(&fixture, &token, &sender, 1));
    assert!(result.is_ok(), "a settled remote receipt must not reject an ordinary turn: {result:?}");
    assert!(matches!(ordinary.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_two_prepare_failure_terminalizes_the_exact_started_turn() {
    let fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let (token, sender, input) = prepare_channel(&fixture, "missing-journal", 1);
    // Inject missing durable association at the storage seam, before enqueue.
    fixture.satellite.mailbox_store.connection().unwrap().execute(
        "DELETE FROM federation_executor_operations", []).unwrap();
    let result = handoff_prepared_turn_dispatch(&fixture.satellite,
        channel_dispatch(&fixture, &token, &sender, 1));
    assert!(result.is_err());
    assert!(!matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))));
    let status = {
        let inner = fixture.satellite.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&fixture.native).unwrap()].session.status
    };
    assert_eq!(status, SessionStatus::Error,
        "a prepare refusal must not strand an Active turn with nothing delivered");
}

fn seed_settled_prefix(fixture: &Fixture) {
    let connection = fixture.central.mailbox_store.connection().unwrap();
    let id = fixture.request["wake"]["messageId"].as_str().unwrap();
    let (target, binding): (String, String) = connection.query_row(
        "SELECT target_session_id,target_binding_json FROM federation_mailbox_wake_obligations WHERE message_id=?1",
        [id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    connection.execute("DELETE FROM federation_mailbox_wake_obligations WHERE message_id=?1", [id]).unwrap();
    connection.execute(
        "WITH RECURSIVE rows(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM rows WHERE n<1050)
         INSERT INTO mailbox_messages(id,mailbox_id,sequence,sender_session_id,sender_name,
          target_session_id,target_name,created_at,class,topic,state_stamp,body,idempotency_key,
          unread_depth_at_append,notification_disposition,dispatch_outcome)
         SELECT 'history-'||n,m.mailbox_id,m.sequence+n,m.sender_session_id,m.sender_name,
          m.target_session_id,m.target_name,m.created_at,m.class,m.topic,NULL,m.body,
          'history-key-'||n,1,m.notification_disposition,NULL
         FROM mailbox_messages m CROSS JOIN rows WHERE m.id=?1", [id]).unwrap();
    connection.execute(
        "INSERT INTO federation_mailbox_wake_obligations(message_id,target_session_id,target_binding_json,state)
         SELECT id,?1,?2,'settled' FROM mailbox_messages WHERE id LIKE 'history-%'",
        rusqlite::params![target,binding]).unwrap();
    connection.execute("INSERT INTO federation_mailbox_wake_obligations VALUES(?1,?2,?3,'pending')",
        rusqlite::params![id,target,binding]).unwrap();
    *fixture.central.mailbox_store.executor_reservations.lock().unwrap() = Default::default();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_two_central_drain_continues_a_settled_history_prefix() {
    let fixture = Fixture::new();
    seed_settled_prefix(&fixture);
    let (stop, server) = attach_loopback(&fixture).await;
    fixture.central.drain_federation_executor_wakes("", 16).await;
    let pending: u64 = fixture.central.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM federation_mailbox_wake_obligations WHERE state='pending'",
        [], |row| row.get(0)).unwrap();
    let _ = stop.send(());
    server.await.unwrap();
    assert_eq!(pending, 0, "bounded scan progress must chain without waiting another supervisor tick");
}

#[test]
fn federation_m4_round_two_satellite_recovery_reaches_work_after_settled_history() {
    let fixture = Fixture::new();
    let operation = fixture.operation();
    fixture.satellite.mailbox_store.admit_federation_executor_operation(&operation, || Ok(())).unwrap();
    fixture.satellite.mailbox_store.connection().unwrap().execute(
        "WITH RECURSIVE rows(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM rows WHERE n<150)
         INSERT INTO federation_executor_operations(central_host_id,operation_key,central_epoch,
          target_host_id,enrollment_generation,session_id,creation_id,placement_generation,command_kind,
          payload_digest,payload,payload_bytes,stage,effect_state,queue_entry_id,delivery_attempt_id,
          processed_evidence_kind,processed_evidence)
         SELECT central_host_id,'a-history-'||n,central_epoch,target_host_id,enrollment_generation,
          session_id,creation_id,placement_generation,command_kind,payload_digest,payload,payload_bytes,
          'processed','not_started','retained-'||n,'retained-'||n,'mailbox_ack','already-acknowledged'
         FROM federation_executor_operations CROSS JOIN rows WHERE operation_key=?1",
        [&operation.operation_key]).unwrap();
    FederationExecutorRecovery::default().recover(&fixture.satellite);
    assert!(fixture.queue(&fixture.satellite).iter().any(|entry|
        entry.pending_prompt.id == operation.queue_identity()),
        "a new received operation cannot wait a periodic tick per settled page");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_two_live_activation_survives_a_transient_queue_evidence_gap() {
    let mut fixture = Fixture::new();
    let operation = fixture.operation();
    let input = idle_channel(&fixture, "live-recovery");
    fixture.satellite.mailbox_store.admit_federation_executor_operation(&operation, || Ok(())).unwrap();
    // Same received->live queue seam as the authenticated route. This injected
    // return is not an OS/storage crash or a claimed HTTP response observation.
    FEDERATION_EXECUTOR_TEST_CUT.with(|slot| *slot.borrow_mut() = Some("afterQueueAssociationCommit"));
    assert!(fixture.satellite.queue_federation_executor_operation(&operation, true).is_err());
    assert!(FEDERATION_EXECUTOR_TEST_CUT.with(|slot| slot.borrow().is_none()), "the intended association cut was consumed");
    assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)));
    // The base fixture has no persist worker. Observe the real write-ahead
    // fence, flush the production row, then release its exact acknowledgement.
    let writer = prompt_delivery::DeliveryWriter::attach(&mut fixture.satellite);
    FederationExecutorRecovery::default().recover(&fixture.satellite);
    let fence = writer.take(prompt_delivery::HeldFence::WriteAhead);
    {
        let inner = fixture.satellite.inner.lock().unwrap();
        let persisted = PersistedState::from_inner(&inner);
        persist_state_from_persisted(fixture.satellite.persistence_path.as_path(), &persisted).unwrap();
    }
    let connection = rusqlite::Connection::open(fixture.satellite.persistence_path.as_path()).unwrap();
    assert!(fence.target.is_already_durable(&connection).unwrap());
    drop(connection);
    fence.finish(Ok(()));
    assert!(matches!(phase_sync::receive(&input, "recovered live wake handoff"), ClaudeRuntimeCommand::Prompt(_)),
        "same-boot recovery must retain the accepted live activation without a second request");
    assert!(!matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))));
    let journal = fixture.inbox().unwrap();
    assert_eq!(journal.queue_entry_id.as_deref(), Some(operation.queue_identity().as_str()));
    assert_eq!(journal.effect_state, "unknown");
    assert!(journal.runtime_ack.is_none());
}

fn committed_ack(fixture: &Fixture) -> MailboxSummary {
    let actor = FederationMailboxActor { binding: fixture.target.clone(), name: "Satellite".into() };
    let mailbox = fixture.request["wake"]["mailboxId"].as_str().unwrap().to_owned();
    let request = |operation| FederationMailboxRequest { protocol_version: 1, schema_version: "1".into(),
        central: fixture.target.central.clone(), actor: Some(actor.clone()), operation };
    let lease = fixture.outbound.current_lease().unwrap();
    let page = fixture.central.execute_federation_mailbox(request(FederationMailboxOperation::Read {
        mailbox_id: mailbox.clone(), after_sequence: None, limit: 100, issue_receipt: true,
    }), Some(lease.clone())).unwrap();
    let page: MailboxReadResponse = serde_json::from_value(page.value).unwrap();
    let ack = fixture.central.execute_federation_mailbox(request(FederationMailboxOperation::Acknowledge {
        mailbox_id: mailbox, receipt: page.receipt.unwrap(),
    }), Some(lease)).unwrap();
    serde_json::from_value(ack.value).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_two_delayed_committed_ack_isolates_a_retired_row_without_adopting_lease() {
    let fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let summary = committed_ack(&fixture);
    let old = fixture.satellite.inner.lock().unwrap().federation.peers[&fixture.target.central.host_id].clone();
    fixture.satellite.enroll_federation_peer(old).unwrap();
    assert!(fixture.lease.with_current(&fixture.satellite, |_, _| Ok(())).is_err());
    let result = fixture.satellite.retain_federation_executor_mailbox_ack(&fixture.target, &summary, &fixture.lease);
    assert!(result.is_ok(), "a permanently retired row must not undo the canonical committed ACK: {result:?}");
    assert!(fixture.inbox().unwrap().processed_evidence.is_none());
    assert!(fixture.lease.with_current(&fixture.satellite, |_, _| Ok(())).is_err(),
        "skipping a retired row never revives its captured old lease");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_two_ack_skips_a_different_binding_before_consulting_the_lease() {
    let fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let mut unrelated = fixture.target.clone();
    unrelated.identity.creation_id = Uuid::new_v4().to_string();
    let mut summary = committed_ack(&fixture);
    for participant in &mut summary.participants {
        if participant.session_id == fixture.target.participant_key() {
            participant.session_id = unrelated.participant_key();
        }
    }
    fixture.channel.invalidate();
    // Helper-contract control only: no claim that this mismatching binding
    // was authenticated, placed or transported under the now-retired lease.
    assert!(fixture.satellite.retain_federation_executor_mailbox_ack(
        &unrelated, &summary, &fixture.lease).is_ok());
    assert!(fixture.inbox().unwrap().processed_evidence.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_two_ack_storage_error_stays_visible_and_non_stale() {
    let fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let summary = committed_ack(&fixture);
    fixture.satellite.mailbox_store.connection().unwrap().execute_batch(
        "DROP TABLE federation_executor_operations").unwrap();
    let error = fixture.satellite.retain_federation_executor_mailbox_ack(
        &fixture.target, &summary, &fixture.lease).unwrap_err();
    assert_ne!(error.status, StatusCode::CONFLICT, "storage errors cannot be silently excluded as stale authority");
}

#[test]
fn federation_m4_round_two_recovery_without_live_intent_keeps_loaded_work_dormant() {
    let fixture = Fixture::new();
    let operation = fixture.operation();
    let input = idle_channel(&fixture, "dormant-recovery");
    fixture.satellite.mailbox_store.admit_federation_executor_operation(&operation, || Ok(())).unwrap();
    let reloaded = {
        let inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        PersistedSessionRecord::from_record(&inner.sessions[index])
    }.into_record().unwrap();
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        inner.sessions[index] = reloaded;
    }
    FederationExecutorRecovery::default().recover(&fixture.satellite);
    assert!(fixture.queue(&fixture.satellite).iter().any(|entry|
        entry.pending_prompt.id == operation.queue_identity()));
    assert!(!matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))),
        "recovery without a same-boot live intent only materializes the queue");
}

#[test]
fn federation_m4_round_two_pause_cancels_retained_live_activation() {
    let fixture = Fixture::new();
    let operation = fixture.operation();
    let input = idle_channel(&fixture, "paused-live-recovery");
    fixture.satellite.mailbox_store.admit_federation_executor_operation(&operation, || Ok(())).unwrap();
    FEDERATION_EXECUTOR_TEST_CUT.with(|slot| *slot.borrow_mut() = Some("afterQueueAssociationCommit"));
    assert!(fixture.satellite.queue_federation_executor_operation(&operation, true).is_err());
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        inner.sessions[index].set_auto_dispatch_blocked(true);
        // Even lifting the pause cannot revive its older live activation.
        inner.sessions[index].set_auto_dispatch_blocked(false);
        fixture.satellite.commit_locked(&mut inner).unwrap();
    }
    FederationExecutorRecovery::default().recover(&fixture.satellite);
    assert!(!matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))));
    assert_eq!(fixture.queue(&fixture.satellite).len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_two_stale_nonce_cannot_reject_a_successor_handoff() {
    let fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let (token, sender, input) = prepare_channel(&fixture, "nonce-successor", 1);
    let old = channel_dispatch(&fixture, &token, &sender, 1);
    let fresh = Uuid::new_v4().to_string();
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        inner.sessions[index].prompt_deliveries.get_mut(&fixture.operation().queue_identity()).unwrap()
            .write_ahead = fresh.clone();
        fixture.satellite.commit_locked(&mut inner).unwrap();
    }
    assert!(matches!(handoff_prepared_turn_dispatch(&fixture.satellite, old).unwrap(),
        HandoffPreparedTurnDispatchOutcome::Superseded));
    assert!(!matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))));
    let (status, nonce) = {
        let inner = fixture.satellite.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
        (record.session.status, record.prompt_deliveries[&fixture.operation().queue_identity()].write_ahead.clone())
    };
    assert_eq!(status, SessionStatus::Active);
    assert_eq!(nonce, fresh);
}
