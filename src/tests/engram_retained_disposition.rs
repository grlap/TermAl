//! Ordinary queue custody beside passive historical operation data.
//! Real local runtimes and SQLite writes; no admission or replay executor.
use super::super::uncertain_begin::OrdinaryRoot;
use super::*;
use crate::tests::prompt_delivery::{DeliveryWriter, HeldFence};

pub(super) fn unknown_retained_entry(turn: &OrdinaryRoot, id: &str, code: &str) -> Value {
    turn.edit(|record| {
        let queued = record
            .queued_prompts
            .iter_mut()
            .find(|queued| queued.pending_prompt.id == id)
            .unwrap();
        queued.engram_evaluate = Some(json!({
            "request": { "operation": "turn_evaluate", "idempotency_key": "historical-key",
                "intent_fingerprint": "unknown-before-reload" },
            "begun_grant_id": "historical-grant", "unknown_code": code
        }));
        queued.engram_ordinary_handoff = true;
        queued.engram_interrupted = true;
        sync_pending_prompts(record);
        serde_json::to_value(&record.queued_prompts[0]).unwrap()
    })
}

// Join the fixture's real worker before taking over its SQLite connection.
// This writes through the existing collector/writer, without inventing an ACK.
pub(super) fn write_retained_state(turn: &OrdinaryRoot) -> StateInner {
    turn.state.shutdown_persist_blocking();
    let delta = collect_persist_delta_from_shared_state(&turn.state.inner, 0);
    persist_delta_with_fences(
        &mut SqlitePersistConnectionCache::new(),
        turn.state.persistence_path.as_path(),
        &delta,
        &mut PersistFenceBatch::default(),
    )
    .unwrap();
    load_state(turn.state.persistence_path.as_path())
        .unwrap()
        .unwrap()
}

#[test]
fn cancel_supersedes_exact_ordinary_resume_owner_and_old_wakes() {
    let turn = OrdinaryRoot::new("cancel-exact-resume");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let canceled = turn.enqueue("canceled explicit Resume", QueuedPromptSource::User);
    let owner = turn.record(|record| QueuedTurnOwner::capture(record).unwrap());
    let successor = turn.enqueue("successor needs its own Resume", QueuedPromptSource::User);
    turn.state
        .cancel_queued_prompt(&turn.session, &canceled)
        .unwrap();
    let before = turn.saved();
    for _ in 0..2 {
        turn.state
            .resume_local_session_queue(&turn.session, Some(owner.clone()))
            .unwrap();
    }
    assert_eq!(
        turn.saved(),
        before,
        "an old drain cannot consume or unpause its successor"
    );
    turn.assert_no_prompt();
    turn.record(|record| {
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.queued_prompts[0].pending_prompt.id, successor);
    });
    turn.state.resume_session_queue(&turn.session).unwrap();
    assert!(
        turn.prompt()
            .prompt
            .contains("successor needs its own Resume")
    );
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn stale_resume_cannot_rewrite_successors_passive_operation() {
    let turn = OrdinaryRoot::new("stale-retained-successor");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let original = turn.enqueue("original", QueuedPromptSource::User);
    let owner = turn.record(|record| QueuedTurnOwner::capture(record).unwrap());
    turn.state
        .cancel_queued_prompt(&turn.session, &original)
        .unwrap();
    let successor = turn.enqueue("exact successor", QueuedPromptSource::User);
    let before = unknown_retained_entry(&turn, &successor, "unknown_bind_reply");
    turn.state
        .resume_local_session_queue(&turn.session, Some(owner))
        .unwrap();
    turn.record(|record| {
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            before
        );
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(QueuedTurnOwner::capture(record).is_none());
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn restart_keeps_unknown_remote_answer_as_data_without_replay() {
    let turn = OrdinaryRoot::new("unknown-answer-reload");
    let id = turn.enqueue("Unicode original — żółć 🦀", QueuedPromptSource::User);
    let original = unknown_retained_entry(&turn, &id, "cached_defer_reply_lost");
    let loaded = write_retained_state(&turn);
    let record = &loaded.sessions[loaded.find_session_index(&turn.session).unwrap()];
    assert_eq!(
        serde_json::to_value(&record.queued_prompts[0]).unwrap(),
        original
    );
    assert!(ordinary_queue_position(record).is_none());
    assert!(
        record.prompt_deliveries.is_empty(),
        "old remote history is not a provider receipt"
    );
    assert!(matches!(record.runtime, SessionRuntime::None));
    turn.state.resume_session_queue(&turn.session).unwrap();
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_promotion_is_visible_and_durable_once_without_defer_transitions() {
    let turn = OrdinaryRoot::new("visible-ordinary-promotion");
    let id = turn.enqueue(
        "Orchestrator root content",
        QueuedPromptSource::Orchestrator,
    );
    let mut deltas = turn.state.subscribe_delta_events();
    let started = turn
        .state
        .start_next_queued_turn_off_lock(&turn.session, false, false)
        .unwrap()
        .unwrap();
    assert!(matches!(
        deliver_turn_dispatch(&turn.state, started.dispatch),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    assert!(turn.prompt().prompt.contains("Orchestrator root content"));
    turn.assert_no_prompt();
    let mut emitted = Vec::new();
    let published = phase_sync::PollGuard::new();
    loop {
        emitted.extend(
            std::iter::from_fn(|| deltas.try_recv().ok())
                .filter_map(|payload| serde_json::from_str::<Value>(&payload).ok()),
        );
        if emitted
            .iter()
            .any(|event| event["sessionId"] == turn.session && event["type"] == "messageCreated")
        {
            break;
        }
        published.wait("ordinary promotion message publication");
    }
    assert!(
        emitted
            .iter()
            .any(|event| event["sessionId"] == turn.session && event["type"] == "messageCreated"),
        "ordinary promotion must publish its message"
    );
    let loaded = write_retained_state(&turn);
    let record = &loaded.sessions[loaded.find_session_index(&turn.session).unwrap()];
    assert!(record.queued_prompts.is_empty());
    assert_eq!(record.session.prompt_history, ["Orchestrator root content"]);
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    let count: usize = connection
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id=?1 AND message_id=?2",
            rusqlite::params![turn.session, id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    turn.assert_no_control();
}

#[test]
fn failed_cancel_write_does_not_claim_durable_loss_of_unknown_content() {
    let mut turn = OrdinaryRoot::new("failed-cancel-write");
    let id = turn.enqueue("last durable unknown input", QueuedPromptSource::User);
    let original = unknown_retained_entry(&turn, &id, "authorization_unknown");
    write_retained_state(&turn);
    let original_path = turn.state.persistence_path.clone();
    let failed_path = original_path.with_extension("cancel-is-directory");
    fs::create_dir_all(&failed_path).unwrap();
    turn.state.persistence_path = Arc::new(failed_path);
    let result = turn.state.cancel_queued_prompt(&turn.session, &id);
    // Restore the file path before asserting, so teardown owns a valid store.
    turn.state.persistence_path = original_path;
    assert!(result.is_err(), "an unwritten cancel cannot report success");
    let loaded = load_state(turn.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.session).unwrap()];
    assert_eq!(
        serde_json::to_value(&record.queued_prompts[0]).unwrap(),
        original
    );
    assert!(ordinary_queue_position(record).is_none());
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn disabled_integration_preserves_unknown_history_and_allows_distinct_ordinary_input() {
    for disable_host in [false, true] {
        let turn = OrdinaryRoot::new("disabled-history");
        let id = turn.enqueue("possibly sent old prompt", QueuedPromptSource::User);
        let original = unknown_retained_entry(&turn, &id, "unknown_begun");
        let operation = super::super::turn_checks::checkpoint_result_recovery::passive_checkpoint(
            &turn.root,
            &turn.session,
        );
        let original_operation = serde_json::to_value(&operation).unwrap();
        {
            let mut inner = turn.state.inner.lock().unwrap();
            super::super::turn_checks::checkpoint_result_recovery::install_historical_checkpoint(
                &mut inner,
                &turn.session,
                &operation,
            );
            turn.state.commit_locked(&mut inner).unwrap();
        }
        if disable_host {
            let mut settings = turn
                .state
                .inner
                .lock()
                .unwrap()
                .find_project(&turn.project)
                .unwrap()
                .engram
                .clone()
                .unwrap();
            settings.enabled = false;
            turn.state
                .update_project_engram_settings(&turn.project, settings)
                .unwrap();
        }
        turn.state.resume_session_queue(&turn.session).unwrap();
        turn.assert_no_prompt();
        let dispatch = turn.prepare("distinct successor after disable");
        let token = dispatch.runtime_token().clone();
        let generation = dispatch.active_turn_generation();
        assert!(matches!(
            deliver_turn_dispatch(&turn.state, dispatch),
            TurnDispatchDeliveryOutcome::Delivered
        ));
        let command = turn.prompt();
        assert!(command.prompt.contains("distinct successor after disable"));
        assert!(!command.prompt.contains("possibly sent old prompt"));
        turn.state
            .finish_turn_ok_if_runtime_and_generation_match(&turn.session, &token, generation)
            .unwrap();
        turn.assert_no_prompt();
        turn.record(|record| {
            assert_eq!(record.queued_prompts.len(), 1);
            assert_eq!(
                serde_json::to_value(&record.queued_prompts[0]).unwrap(),
                original
            );
            assert!(record.prompt_deliveries.is_empty());
        });
        turn.state.cancel_queued_prompt(&turn.session, &id).unwrap();
        let loaded = write_retained_state(&turn);
        assert!(
            loaded.sessions[loaded.find_session_index(&turn.session).unwrap()]
                .queued_prompts
                .is_empty()
        );
        assert_eq!(
            serde_json::to_value(&loaded.engram_session_obligations.checkpoint_operations[0])
                .unwrap(),
            original_operation,
            "Cancel removes content, never rewrites independent historical evidence"
        );
        assert!(matches!(
            loaded.engram_session_obligations.checkpoint_operations[0].attempts[0].result,
            Some(EngramCheckpointAttemptResult::Unknown { .. })
        ));
        assert!(loaded.test_run_observations.is_empty());
        turn.assert_no_control();
    }
}

#[test]
fn unknown_historical_faults_stay_cancelable_in_roots_and_real_children() {
    for child in [false, true] {
        for code in [
            "authorization_unknown",
            "control_binding_unavailable",
            "unknown_control_schema",
            "store_corrupt",
            "future_unknown_code",
        ] {
            let turn = OrdinaryRoot::new("historical-fault-cancel");
            let session = if child {
                let created = turn
                    .state
                    .create_read_only_delegation(
                        &turn.session,
                        CreateDelegationRequest {
                            prompt: "initial child prompt".into(),
                            title: None,
                            cwd: None,
                            agent: Some(Agent::Codex),
                            model: None,
                            mode: Some(DelegationMode::Explorer),
                            write_policy: Some(DelegationWritePolicy::ReadOnly),
                        },
                    )
                    .unwrap();
                receive_synchronous_engram_prompt(
                    &turn.state,
                    &turn.receiver,
                    "actual initial child handoff",
                )
                .unwrap();
                created.delegation.child_session_id
            } else {
                turn.session.clone()
            };
            let id = {
                let mut inner = turn.state.inner.lock().unwrap();
                let id = inner.next_message_id();
                let record = inner.session_mut(&session).unwrap();
                queue_prompt_on_record(
                    record,
                    PendingPrompt {
                        id: id.clone(),
                        timestamp: stamp_now(),
                        text: format!("unknown payload {code}"),
                        expanded_text: None,
                        source: None,
                        attachments: Vec::new(),
                        engram_interrupted: false,
                        is_engram_retained: false,
                    },
                    Vec::new(),
                );
                let queued = record.queued_prompts.back_mut().unwrap();
                queued.engram_evaluate =
                    Some(json!({"unknown_code":code,"request":{"operation":"turn_evaluate"}}));
                queued.engram_ordinary_handoff = true;
                sync_pending_prompts(record);
                turn.state.commit_locked(&mut inner).unwrap();
                id
            };
            let before = {
                let inner = turn.state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                serde_json::to_value(&record.queued_prompts).unwrap()
            };
            turn.state
                .refresh_delegation_for_child_session(&session)
                .unwrap();
            {
                let inner = turn.state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                assert_eq!(
                    serde_json::to_value(&record.queued_prompts).unwrap(),
                    before
                );
            }
            turn.state.cancel_queued_prompt(&session, &id).unwrap();
            {
                let inner = turn.state.inner.lock().unwrap();
                assert!(
                    inner.sessions[inner.find_session_index(&session).unwrap()]
                        .queued_prompts
                        .is_empty()
                );
            }
            turn.assert_no_prompt();
            turn.assert_no_control();
        }
    }
}

#[test]
fn trimmed_promoted_history_keeps_its_sql_position_without_reinsertion() {
    let turn = OrdinaryRoot::new("trim-promoted-history");
    let id = turn.enqueue("already promoted user content", QueuedPromptSource::User);
    turn.edit(|record| {
        let prompt = record.queued_prompts[0].pending_prompt.clone();
        let position = session_message_count(record);
        push_message_on_record(
            record,
            Message::Text {
                id: prompt.id,
                timestamp: prompt.timestamp,
                author: Author::You,
                text: prompt.text,
                attachments: Vec::new(),
                expanded_text: None,
                source: None,
            },
        );
        record.queued_prompts[0].promoted_message_index = Some(usize::try_from(position).unwrap());
        record
            .session
            .prompt_history
            .push("already promoted user content".into());
    });
    write_retained_state(&turn);
    turn.edit(|record| {
        for sequence in 0..80 {
            push_message_on_record(
                record,
                Message::Text {
                    id: format!("trim-filler-{sequence}"),
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: "unrelated history".into(),
                    attachments: Vec::new(),
                    expanded_text: None,
                    source: None,
                },
            );
        }
    });
    write_retained_state(&turn);
    let position = turn.record(|record| record.queued_prompts[0].promoted_message_index.unwrap());
    turn.edit(|record| {
        trim_retained_session_messages(record, SESSION_IN_MEMORY_MESSAGE_LIMIT);
        assert!(cached_message_index_on_record(record, &id).is_none());
    });
    let history = turn.record(|record| record.session.prompt_history.clone());
    turn.state.resume_session_queue(&turn.session).unwrap();
    turn.assert_no_prompt();
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    let row: (usize, usize) = connection
        .query_row(
            "SELECT COUNT(*),MIN(position) FROM messages WHERE session_id=?1 AND message_id=?2",
            rusqlite::params![turn.session, id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(row, (1, position));
    turn.record(|record| {
        assert_eq!(record.session.prompt_history, history);
        assert!(cached_message_index_on_record(record, &id).is_none());
        assert!(ordinary_queue_position(record).is_none());
    });
    turn.assert_no_control();
}

#[test]
fn trimmed_unpromoted_known_content_can_dispatch_once_after_reload() {
    let turn = OrdinaryRoot::new("trim-unpromoted-content");
    turn.edit(|record| {
        for sequence in 0..80 {
            push_message_on_record(
                record,
                Message::Text {
                    id: format!("unpromoted-filler-{sequence}"),
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: "unrelated history".into(),
                    attachments: Vec::new(),
                    expanded_text: None,
                    source: None,
                },
            );
        }
    });
    let id = turn.enqueue(
        "exact unpromoted restart content 🦀",
        QueuedPromptSource::User,
    );
    turn.edit(|record| {
        record.queued_prompts[0].engram_bind = Some(json!({"request":{
            "operation":"session_bind","idempotency_key":"historical-unsent"}}));
    });
    let loaded = write_retained_state(&turn);
    let mut saved = loaded.sessions[loaded.find_session_index(&turn.session).unwrap()].clone();
    assert!(saved.message_start_index > 0);
    assert!(saved.queued_prompts[0].promotion_disposition_known);
    assert_eq!(saved.queued_prompts[0].promoted_message_index, None);
    assert_eq!(ordinary_queue_position(&saved), Some(0));
    saved.runtime = turn.record(|record| record.runtime.clone());
    saved.session.status = SessionStatus::Idle;
    {
        let mut inner = turn.state.inner.lock().unwrap();
        *inner.session_mut(&turn.session).unwrap() = saved;
    }
    turn.state.resume_session_queue(&turn.session).unwrap();
    assert!(
        turn.prompt()
            .prompt
            .contains("exact unpromoted restart content 🦀")
    );
    turn.assert_no_prompt();
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    let count: usize = connection
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id=?1 AND message_id=?2",
            rusqlite::params![turn.session, id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    turn.assert_no_control();
}

#[test]
fn ordinary_promotion_write_failure_keeps_exact_payload_for_a_later_resume() {
    let turn = OrdinaryRoot::new("promotion-own-row-failure");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let id = turn.enqueue("exact rollback prompt", QueuedPromptSource::Orchestrator);
    turn.edit(|record| {
        let head = &mut record.queued_prompts[0];
        let metadata = MessageImageAttachment {
            byte_size: 1,
            file_name: "exact.png".into(),
            media_type: "image/png".into(),
        };
        head.pending_prompt.expanded_text = Some("expanded exact rollback prompt".into());
        head.pending_prompt.source = Some(MessageSource::peer(
            "peer-session".into(),
            "Peer agent".into(),
        ));
        head.pending_prompt.attachments = vec![metadata.clone()];
        head.attachments = vec![PromptImageAttachment {
            data: "AA==".into(),
            metadata,
        }];
        head.engram_evaluate =
            Some(json!({"request":{"operation":"turn_evaluate","idempotency_key":"old-raw-key"}}));
        sync_pending_prompts(record);
    });
    let successor = turn.enqueue("successor stays queued", QueuedPromptSource::Orchestrator);
    write_retained_state(&turn);
    let historical = turn.record(|record| record.queued_prompts[0].clone());
    let before = turn.record(|record| {
        (
            serde_json::to_value(&record.queued_prompts).unwrap(),
            record.session.prompt_history.clone(),
            session_message_count(record),
            record.active_turn_generation,
        )
    });
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    connection.execute_batch(&format!(
        "CREATE TRIGGER reject_ordinary_promotion BEFORE INSERT ON messages
         WHEN NEW.message_id='{}' BEGIN SELECT RAISE(ABORT,'injected-promotion-commit-failure'); END;",
        id.replace('\'', "''"))).unwrap();
    let result = turn
        .state
        .start_next_queued_turn_off_lock(&turn.session, true, false);
    assert!(result.err().is_some_and(|error| format!("{error:#}").contains("injected-promotion-commit-failure")));
    turn.assert_no_prompt();
    turn.record(|record| {
        assert_eq!(
            serde_json::to_value(&record.queued_prompts).unwrap(),
            before.0
        );
        assert_eq!(record.session.prompt_history, before.1);
        assert_eq!(session_message_count(record), before.2);
        assert_eq!(record.active_turn_generation, before.3);
        assert_eq!(record.session.status, SessionStatus::Idle);
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(cached_message_index_on_record(record, &id).is_none());
    });
    connection
        .execute_batch("DROP TRIGGER reject_ordinary_promotion;")
        .unwrap();
    drop(connection);
    turn.state.resume_session_queue(&turn.session).unwrap();
    let command = turn.prompt();
    assert!(command.prompt.contains("expanded exact rollback prompt"));
    assert!(command.prompt.contains("Peer agent") && command.prompt.contains("peer-session"));
    assert_eq!(command.attachments.len(), 1);
    turn.assert_no_prompt();
    turn.record(|record| {
        assert_eq!(record.queued_prompts.len(), 2);
        let retained = &record.queued_prompts[0];
        assert_eq!(retained.pending_prompt.id, id);
        assert!(!ordinary_queue_entry_is_runnable(record, retained));
        let mut expected = historical.clone();
        expected.engram_ordinary_handoff = true;
        expected.promoted_message_index = Some(
            record.message_start_index
                + record
                    .session
                    .messages
                    .iter()
                    .position(|message| message.id() == id)
                    .unwrap(),
        );
        assert_eq!(
            serde_json::to_value(retained).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        assert_eq!(ordinary_queue_position(record), Some(1));
        assert_eq!(record.queued_prompts[1].pending_prompt.id, successor);
        assert_eq!(
            record
                .session
                .messages
                .iter()
                .filter(|message| message.id() == id)
                .count(),
            1
        );
    });
    let token = turn.record(|record| record.runtime.runtime_token().unwrap());
    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.session, &token)
        .unwrap();
    let next = turn.prompt();
    assert!(next.prompt.contains("successor stays queued"));
    assert!(!next.prompt.contains("expanded exact rollback prompt"));
    turn.assert_no_prompt();
    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.session, &token)
        .unwrap();
    turn.record(|record| {
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.queued_prompts[0].pending_prompt.id, id);
        assert!(ordinary_queue_position(record).is_none());
        assert_eq!(
            record
                .session
                .messages
                .iter()
                .filter(|message| message.id() == id)
                .count(),
            1
        );
        assert_eq!(
            record
                .session
                .messages
                .iter()
                .filter(|message| message.id() == successor)
                .count(),
            1
        );
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn real_explorer_initial_delivery_does_not_become_a_restart_admission() {
    let turn = OrdinaryRoot::new("explorer-restart-custody");
    let created = turn
        .state
        .create_read_only_delegation(
            &turn.session,
            CreateDelegationRequest {
                prompt: "initial Explorer content across restart".into(),
                title: Some("restart explorer".into()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Explorer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .unwrap();
    let delivered =
        receive_synchronous_engram_prompt(&turn.state, &turn.receiver, "initial Explorer").unwrap();
    assert!(matches!(delivered, CodexRuntimeCommand::Prompt {command,..}
        if command.prompt.contains("initial Explorer content across restart")));
    turn.assert_no_prompt();
    let loaded = write_retained_state(&turn);
    let child = &loaded.sessions[loaded
        .find_session_index(&created.delegation.child_session_id)
        .unwrap()];
    assert!(child.queued_prompts.is_empty());
    assert!(
        child.prompt_deliveries.is_empty(),
        "ordinary Explorer setup invents no receipt"
    );
    assert!(matches!(child.runtime, SessionRuntime::None));
    assert_eq!(
        child
            .session
            .prompt_history
            .iter()
            .filter(|text| text.contains("initial Explorer content across restart"))
            .count(),
        1
    );
    turn.assert_no_control();
}

#[test]
fn receipted_handoff_without_provider_ack_is_ambiguous_after_restart() {
    let mut turn = OrdinaryRoot::new("receipted-reload-unknown");
    let writer = DeliveryWriter::attach(&mut turn.state);
    let id = turn.enqueue_durable("receipted prompt without provider ACK", true);
    assert!(
        turn.state
            .request_queued_prompt_durability(&turn.session, &id)
    );
    turn.assert_no_prompt();
    writer.take(HeldFence::Durability).finish(Ok(()));
    let write_ahead = writer.take(HeldFence::WriteAhead);
    turn.record(|record| {
        assert_eq!(
            record.prompt_deliveries[&id].disposition,
            PromptDeliveryDisposition::HandingOff
        );
        assert!(!record.prompt_deliveries[&id].write_ahead_acknowledged);
        assert!(record.prompt_deliveries[&id].turn_generation.is_none());
    });
    turn.assert_no_prompt();
    write_ahead.finish(Ok(()));
    let command = turn.prompt();
    assert!(
        command
            .prompt
            .contains("receipted prompt without provider ACK")
    );
    assert!(command.delivery_receipt.is_some());
    let loaded = write_retained_state(&turn);
    let record = &loaded.sessions[loaded.find_session_index(&turn.session).unwrap()];
    assert_eq!(
        record.prompt_deliveries[&id].disposition,
        PromptDeliveryDisposition::Ambiguous
    );
    assert!(record.prompt_deliveries[&id].resume.is_none());
    assert!(record.queued_prompts.is_empty());
    turn.assert_no_prompt();
    turn.assert_no_control();
    drop(writer);
}

#[test]
fn writer_backed_ordinary_queue_restart_adds_no_control_barrier() {
    let turn = OrdinaryRoot::new("ordinary-restart-queue");
    let id = turn.enqueue("ordinary queued prompt", QueuedPromptSource::User);
    let loaded = write_retained_state(&turn);
    let record = &loaded.sessions[loaded.find_session_index(&turn.session).unwrap()];
    assert!(!record.orchestrator_auto_dispatch_blocked);
    assert!(!record.session.queue_paused);
    assert_eq!(record.queued_prompts[0].pending_prompt.id, id);
    assert_eq!(
        record.queued_prompts[0].pending_prompt.text,
        "ordinary queued prompt"
    );
    assert!(record.queued_prompts[0].engram_evaluate.is_none());
    assert!(record.queued_prompts[0].engram_bind.is_none());
    assert_eq!(ordinary_queue_position(record), Some(0));
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn trimmed_unknown_promotion_stays_unknown_and_never_replays() {
    let turn = OrdinaryRoot::new("trimmed-unknown-promotion");
    turn.edit(|record| {
        for sequence in 0..80 {
            push_message_on_record(
                record,
                Message::Text {
                    id: format!("unknown-filler-{sequence}"),
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: "historical transcript".into(),
                    attachments: Vec::new(),
                    expanded_text: None,
                    source: None,
                },
            );
        }
    });
    let id = turn.enqueue("unknown disposition — źródło 🦀", QueuedPromptSource::User);
    write_retained_state(&turn);
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    let encoded = persisted_session_json(&turn.state, &turn.session);
    let mut saved: Value = serde_json::from_str(&encoded).unwrap();
    saved["queuedPrompts"][0]
        .as_object_mut()
        .unwrap()
        .remove("promotion_disposition_known");
    connection
        .execute(
            "UPDATE sessions SET value_json=?1 WHERE id=?2",
            rusqlite::params![saved.to_string(), turn.session],
        )
        .unwrap();
    let loaded = load_state(turn.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.session).unwrap()];
    assert!(record.message_start_index > 0);
    assert!(cached_message_index_on_record(record, &id).is_none());
    assert!(!record.queued_prompts[0].promotion_disposition_known);
    assert!(record.queued_prompts[0].engram_ordinary_handoff);
    assert_eq!(record.queued_prompts[0].pending_prompt.id, id);
    assert_eq!(
        record.queued_prompts[0].pending_prompt.text,
        "unknown disposition — źródło 🦀"
    );
    assert!(ordinary_queue_position(record).is_none());
    assert!(record.prompt_deliveries.is_empty());
    turn.assert_no_prompt();
    turn.assert_no_control();
}
