//! Ordinary Stop, mailbox, provider and persistence boundaries after gate removal.
//! Historical operation values stay data; no recovery protocol is executed.
use super::super::uncertain_begin::OrdinaryRoot;
use super::retained_disposition::{unknown_retained_entry, write_retained_state};
use super::*;

fn retained_mailbox_request(target: &str, key: &str, topic: &str) -> SendMailboxMessageRequest {
    SendMailboxMessageRequest {
        target_session_id: target.into(),
        message: format!("durable body for {topic}"),
        idempotency_key: key.into(),
        topic: Some(topic.into()),
        state_stamp: None,
        class: Some("routine".into()),
    }
}

fn change_evaluator_defaults(state: &AppState, session: &str) {
    let project = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(session).unwrap()]
            .session
            .project_id
            .clone()
            .unwrap()
    };
    state
        .update_acceptance_defaults(
            &project,
            AcceptanceEvaluatorDefaults {
                default_mode: Some(AcceptanceEvaluationMode::IndependentSession),
                evaluator_agent: Some(Agent::Claude),
                evaluator_model: Some("changed-default".into()),
            },
        )
        .unwrap();
}

#[test]
fn stop_keeps_exact_queued_content_and_prevents_the_old_provider_handoff() {
    let turn = OrdinaryRoot::new("stop-queued-content");
    let dispatch = turn.prepare("unsent original active turn");
    let queued = turn.enqueue("oldest queued user", QueuedPromptSource::User);
    let gate = install_test_stop_fence_gate(&turn.state, &turn.session);
    turn.state.request_stop_session(&turn.session).unwrap();
    gate.wait_until_claimed();
    let outcome = deliver_turn_dispatch(&turn.state, dispatch);
    let before = turn
        .record(|record| serde_json::to_value(&record.queued_prompts[0].pending_prompt).unwrap());
    // Always release the real Stop worker before any assertions.
    gate.release();
    let stopped = phase_sync::PollGuard::new();
    while turn.record(|record| record.runtime_stop_in_progress) {
        stopped.wait("ordinary Stop completes");
    }
    assert!(matches!(outcome, TurnDispatchDeliveryOutcome::Scheduled));
    turn.assert_no_prompt();
    turn.state
        .queue_mailbox_wakeups_for_session_outcome(
            &turn.session,
            vec![MailboxUnreadWakeup {
                mailbox_id: "later-mailbox".into(),
                message_id: "mail-1".into(),
                sequence: 1,
                unread_count: 1,
                sender_session_id: "sender".into(),
                sender_name: "Sender".into(),
                topic: None,
            }],
            MailboxWakeupRecovery::NeverWoken,
            true,
        )
        .unwrap();
    turn.enqueue("new user", QueuedPromptSource::User);
    turn.edit(|record| {
        prioritize_user_queued_prompts(record);
        assert_eq!(record.queued_prompts[0].pending_prompt.id, queued);
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0].pending_prompt).unwrap(),
            before
        );
        assert_eq!(record.queued_prompts.len(), 3);
        assert!(record.orchestrator_auto_dispatch_blocked);
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn historical_mailbox_wake_is_immutable_while_a_newer_wake_queues_separately() {
    let mut turn = OrdinaryRoot::new("mailbox-immutable-history");
    let coordination_path =
        resolve_coordination_persistence_path(turn.state.persistence_path.as_ref());
    turn.state.mailbox_store = Arc::new(MailboxStore::open(&coordination_path).unwrap());
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let sender = test_session_id(&turn.state, Agent::Claude);
    let first = turn
        .state
        .append_mailbox_message_and_notify(
            &sender,
            retained_mailbox_request(&turn.session, "first-immutable", "first"),
        )
        .unwrap();
    let id = turn.record(|record| record.queued_prompts[0].pending_prompt.id.clone());
    let original = unknown_retained_entry(&turn, &id, "unknown_mailbox_handoff");
    let second = turn
        .state
        .append_mailbox_message_and_notify(
            &sender,
            retained_mailbox_request(&turn.session, "second-immutable", "second"),
        )
        .unwrap();
    turn.record(|record| {
        assert_eq!(record.queued_prompts.len(), 2);
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            original
        );
        let sequences = record
            .queued_prompts
            .iter()
            .map(|entry| {
                entry
                    .pending_prompt
                    .source
                    .as_ref()
                    .unwrap()
                    .mailbox
                    .as_ref()
                    .unwrap()
                    .sequence
            })
            .collect::<Vec<_>>();
        assert_eq!(sequences, [first.sequence, second.sequence]);
    });
    turn.state.resume_session_queue(&turn.session).unwrap();
    let command = turn.prompt();
    assert!(command.prompt.contains("second"));
    turn.record(|record| {
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            original
        );
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn stopped_mailbox_content_and_its_successor_are_not_merged_or_replayed() {
    let turn = OrdinaryRoot::new("stopped-mailbox-successor");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    for sequence in [1, 2] {
        turn.state
            .queue_mailbox_wakeups_for_session_outcome(
                &turn.session,
                vec![MailboxUnreadWakeup {
                    mailbox_id: "same-mailbox".into(),
                    message_id: format!("mail-{sequence}"),
                    sequence,
                    unread_count: sequence,
                    sender_session_id: "sender".into(),
                    sender_name: "Sender".into(),
                    topic: Some(format!("topic-{sequence}")),
                }],
                MailboxWakeupRecovery::NeverWoken,
                true,
            )
            .unwrap();
        if sequence == 1 {
            let id = turn.record(|record| record.queued_prompts[0].pending_prompt.id.clone());
            unknown_retained_entry(&turn, &id, "stopped_before_known_handoff");
        }
    }
    let (id, original, successor) = turn.record(|record| {
        assert_eq!(record.queued_prompts.len(), 2);
        (
            record.queued_prompts[0].pending_prompt.id.clone(),
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            record.queued_prompts[1].pending_prompt.clone(),
        )
    });
    turn.state.cancel_queued_prompt(&turn.session, &id).unwrap();
    turn.assert_no_prompt();
    turn.state.resume_session_queue(&turn.session).unwrap();
    let command = turn.prompt();
    assert!(command.prompt.contains("topic-2"));
    assert!(!command.prompt.contains("topic-1"));
    assert_eq!(original["pending_prompt"]["id"], id);
    turn.record(|record| {
        assert!(record.queued_prompts.is_empty());
        assert_eq!(
            record
                .session
                .messages
                .iter()
                .filter(|message| message.id() == successor.id)
                .count(),
            1
        );
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

// This is the existing handoff seam with provider channels, not a live vendor
// process. It tests the actual runtime/turn guard before the channel enqueue.
#[test]
fn claude_and_acp_stale_handoff_cannot_send_or_rewrite_the_current_owner() {
    for acp in [false, true] {
        for supersession in ["generation", "runtime", "idle"] {
            let turn = OrdinaryRoot::new("provider-handoff-supersession");
            let prepared = turn.prepare("old prepared owner");
            let generation = prepared.active_turn_generation();
            let token = prepared.runtime_token().clone();
            drop(prepared);
            turn.edit(|record| match supersession {
                "generation" => record.active_turn_generation += 1,
                "runtime" => record.runtime = SessionRuntime::None,
                _ => record.session.status = SessionStatus::Idle,
            });
            let before = turn.saved();
            let (claude_tx, claude_rx) = mpsc::channel();
            let (acp_tx, acp_rx) = mpsc::channel();
            let lifecycle = Arc::new((Mutex::new(false), Condvar::new()));
            let dispatch = if acp {
                TurnDispatch::PersistentAcp {
                    human_prompt_id: None,
                    federation_handoff: None,
                    opening_prompt: None,
                    active_turn_generation: generation,
                    runtime_token: token,
                    session_id: turn.session.clone(),
                    mailbox_notification: None,
                    sender: acp_tx,
                    turn_lifecycle: lifecycle.clone(),
                    command: AcpPromptCommand {
                        cwd: String::new(),
                        cursor_mode: None,
                        model: String::new(),
                        opencode_effort: None,
                        opencode_mode: None,
                        prompt: "must not deliver".into(),
                        resume_session_id: None,
                    },
                }
            } else {
                TurnDispatch::PersistentClaude {
                    human_prompt_id: None,
                    federation_handoff: None,
                    opening_prompt: None,
                    active_turn_generation: generation,
                    runtime_token: token,
                    session_id: turn.session.clone(),
                    mailbox_notification: None,
                    sender: claude_tx,
                    command: ClaudePromptCommand {
                        attachments: Vec::new(),
                        replay_generation: "test".into(),
                        text: "must not deliver".into(),
                        turn_generation: 1,
                    },
                }
            };
            assert!(matches!(
                handoff_prepared_turn_dispatch(&turn.state, dispatch).unwrap(),
                HandoffPreparedTurnDispatchOutcome::Superseded
            ));
            assert!(claude_rx.try_recv().is_err());
            assert!(acp_rx.try_recv().is_err());
            assert!(!*lifecycle.0.lock().unwrap());
            assert_eq!(turn.saved(), before);
            turn.assert_no_prompt();
            turn.assert_no_control();
        }
    }
}

#[test]
fn loading_historical_begun_state_never_checkpoints_or_delivers_it() {
    let turn = OrdinaryRoot::new("passive-begun-reload");
    let id = turn.enqueue("historical begun prompt", QueuedPromptSource::User);
    let original = unknown_retained_entry(&turn, &id, "remote_turn_open_begun");
    let operation = super::super::turn_checks::checkpoint_result_recovery::passive_checkpoint(
        &turn.root,
        &turn.session,
    );
    {
        let mut inner = turn.state.inner.lock().unwrap();
        super::super::turn_checks::checkpoint_result_recovery::install_historical_checkpoint(
            &mut inner,
            &turn.session,
            &operation,
        );
        turn.state.commit_locked(&mut inner).unwrap();
    }
    let loaded = write_retained_state(&turn);
    let record = &loaded.sessions[loaded.find_session_index(&turn.session).unwrap()];
    assert_eq!(
        serde_json::to_value(&record.queued_prompts[0]).unwrap(),
        original
    );
    assert_eq!(
        record.queued_prompts[0].engram_evaluate.as_ref().unwrap()["begun_grant_id"],
        "historical-grant"
    );
    assert!(ordinary_queue_position(record).is_none());
    assert!(record.prompt_deliveries.is_empty());
    assert_eq!(
        serde_json::to_value(&loaded.engram_session_obligations.checkpoint_operations[0]).unwrap(),
        serde_json::to_value(&operation).unwrap()
    );
    assert!(matches!(
        loaded.engram_session_obligations.checkpoint_operations[0].attempts[0].result,
        Some(EngramCheckpointAttemptResult::Unknown { .. })
    ));
    assert!(loaded.test_run_observations.is_empty());
    turn.state.resume_session_queue(&turn.session).unwrap();
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn evaluator_defaults_preserve_unknown_bind_and_evaluate_history_without_replay() {
    for bind in [false, true] {
        let turn = OrdinaryRoot::new("evaluator-defaults-history");
        let id = turn.enqueue("unchanged unknown input", QueuedPromptSource::User);
        unknown_retained_entry(&turn, &id, "unknown_remote_answer");
        turn.edit(|record| {
            if bind {
                let entry = &mut record.queued_prompts[0];
                entry.engram_bind = entry.engram_evaluate.take();
            }
        });
        let before = turn.record(|record| serde_json::to_value(&record.queued_prompts).unwrap());
        change_evaluator_defaults(&turn.state, &turn.session);
        turn.state.resume_session_queue(&turn.session).unwrap();
        turn.assert_no_prompt();
        assert_eq!(
            turn.record(|record| serde_json::to_value(&record.queued_prompts).unwrap()),
            before
        );
        turn.assert_no_control();
    }
}

#[test]
fn evaluator_defaults_saved_after_preparation_preserve_ordinary_delivery_owner() {
    let turn = OrdinaryRoot::new("evaluator-defaults-prepared");
    let dispatch = turn.prepare("ordinary owner across evaluator save");
    let generation = dispatch.active_turn_generation();
    let token = dispatch.runtime_token().clone();
    change_evaluator_defaults(&turn.state, &turn.session);
    assert!(matches!(
        deliver_turn_dispatch(&turn.state, dispatch),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    assert!(
        turn.prompt()
            .prompt
            .contains("ordinary owner across evaluator save")
    );
    turn.record(|record| {
        assert!(record.runtime.matches_runtime_token(&token));
        assert_eq!(record.active_turn_generation, generation);
        assert_eq!(record.session.status, SessionStatus::Active);
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_completion_drains_fifo_and_explicit_pause_keeps_the_next_head() {
    let turn = OrdinaryRoot::new("ordinary-drain-fifo");
    let first = turn.prepare("first");
    let token = first.runtime_token().clone();
    let generation = first.active_turn_generation();
    deliver_turn_dispatch(&turn.state, first)
        .into_public_result()
        .unwrap();
    assert!(turn.prompt().prompt.ends_with("first"));
    let second = turn.enqueue("second", QueuedPromptSource::User);
    let third = turn.enqueue("third", QueuedPromptSource::User);
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    turn.state
        .finish_turn_ok_if_runtime_and_generation_match(&turn.session, &token, generation)
        .unwrap();
    turn.assert_no_prompt();
    turn.record(|record| {
        assert_eq!(
            record
                .queued_prompts
                .iter()
                .map(|entry| entry.pending_prompt.id.clone())
                .collect::<Vec<_>>(),
            [second, third]
        )
    });
    turn.state.resume_session_queue(&turn.session).unwrap();
    let command = turn.prompt();
    assert!(command.prompt.ends_with("second"));
    turn.state
        .finish_turn_ok_if_runtime_and_generation_match(
            &turn.session,
            &token,
            command.active_turn_generation,
        )
        .unwrap();
    assert!(turn.prompt().prompt.ends_with("third"));
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn stale_prepared_turn_cannot_clear_successor_bookkeeping_or_passive_history() {
    let turn = OrdinaryRoot::new("stale-turn-bookkeeping");
    let stale = turn.prepare("stale prepared turn");
    turn.edit(|record| {
        record.active_turn_generation += 1;
        record.session.status = SessionStatus::Active;
        record.active_turn_start_message_count = Some(999);
        record.active_turn_mailbox_notification = Some(MailboxNotificationDelivery {
            mailbox_id: "successor-mailbox".into(),
            session_id: record.session.id.clone(),
            through_sequence: 18,
        });
        record
            .active_turn_file_changes
            .insert("successor.rs".into(), WorkspaceFileChangeKind::Created);
    });
    let id = turn.enqueue("successor historical content", QueuedPromptSource::User);
    unknown_retained_entry(&turn, &id, "unresolved_successor");
    let before = turn.saved();
    assert!(matches!(
        deliver_turn_dispatch(&turn.state, stale),
        TurnDispatchDeliveryOutcome::Superseded
    ));
    assert_eq!(turn.saved(), before);
    turn.record(|record| {
        assert_eq!(record.active_turn_start_message_count, Some(999));
        assert_eq!(
            record
                .active_turn_mailbox_notification
                .as_ref()
                .unwrap()
                .through_sequence,
            18
        );
        assert!(record.active_turn_file_changes.contains_key("successor.rs"));
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn restored_unknown_history_does_not_block_distinct_ordinary_input_or_gain_credit() {
    let turn = OrdinaryRoot::new("unknown-restart-fresh-work");
    let id = turn.enqueue("unproven old input", QueuedPromptSource::User);
    let original = unknown_retained_entry(&turn, &id, "lost_evaluation");
    let saved = turn.saved();
    let mut restored = serde_json::from_value::<PersistedSessionRecord>(saved)
        .unwrap()
        .into_record()
        .unwrap();
    assert!(matches!(restored.runtime, SessionRuntime::None));
    assert_eq!(
        serde_json::to_value(&restored.queued_prompts[0]).unwrap(),
        original
    );
    assert!(ordinary_queue_position(&restored).is_none());
    // Reattach only the disposable provider wiring; the queue comes from the
    // actual persisted-record decoder, not from the pre-reload live record.
    restored.runtime = turn.record(|record| record.runtime.clone());
    {
        let mut inner = turn.state.inner.lock().unwrap();
        *inner.session_mut(&turn.session).unwrap() = restored;
    }
    turn.state.resume_session_queue(&turn.session).unwrap();
    turn.assert_no_prompt();
    let dispatch = turn.prepare("distinct new ordinary input");
    let token = dispatch.runtime_token().clone();
    let generation = dispatch.active_turn_generation();
    deliver_turn_dispatch(&turn.state, dispatch)
        .into_public_result()
        .unwrap();
    let command = turn.prompt();
    assert!(command.prompt.ends_with("distinct new ordinary input"));
    assert!(!command.prompt.contains("unproven old input"));
    assert!(command.delivery_receipt.is_none());
    turn.state
        .finish_turn_ok_if_runtime_and_generation_match(&turn.session, &token, generation)
        .unwrap();
    turn.record(|record| {
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            original
        );
        assert!(record.prompt_deliveries.is_empty());
    });
    turn.state.cancel_queued_prompt(&turn.session, &id).unwrap();
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn removed_control_setting_cannot_reactivate_replay_on_either_boolean_value() {
    for enabled in [false, true] {
        let turn = OrdinaryRoot::new("removed-control-setting");
        let id = turn.enqueue("unknown old bind or evaluate", QueuedPromptSource::User);
        let original = unknown_retained_entry(&turn, &id, "lost_rebind");
        let settings = turn
            .state
            .inner
            .lock()
            .unwrap()
            .find_project(&turn.project)
            .unwrap()
            .engram
            .clone()
            .unwrap();
        let mut wire = serde_json::to_value(settings).unwrap();
        wire["turnGatedControl"] = json!(enabled);
        let decoded: EngramProjectSettings = serde_json::from_value(wire).unwrap();
        assert!(
            serde_json::to_value(&decoded)
                .unwrap()
                .get("turnGatedControl")
                .is_none()
        );
        {
            let mut inner = turn.state.inner.lock().unwrap();
            inner
                .projects
                .iter_mut()
                .find(|project| project.id == turn.project)
                .unwrap()
                .engram = Some(decoded);
            turn.state.commit_locked(&mut inner).unwrap();
        }
        turn.state.resume_session_queue(&turn.session).unwrap();
        turn.assert_no_prompt();
        turn.record(|record| {
            assert_eq!(
                serde_json::to_value(&record.queued_prompts[0]).unwrap(),
                original
            )
        });
        turn.assert_no_control();
    }
}

#[test]
fn exact_human_selection_of_unknown_history_refuses_without_sending_a_successor() {
    for problem in ["begun", "uncertain", "different_settings"] {
        let turn = OrdinaryRoot::new("selected-unknown-no-substitution");
        let id = turn.enqueue(
            "exact selected unknown content 🦀",
            QueuedPromptSource::User,
        );
        let original = unknown_retained_entry(&turn, &id, problem);
        let successor = turn.enqueue("unselected eligible successor", QueuedPromptSource::User);
        let selection = turn.record(|record| HumanPromptSelection {
            prompt: projected_pending_prompts(record)[0].clone(),
            session_mutation_stamp: record.mutation_stamp,
        });
        let error = turn
            .state
            .resume_selected_human_prompt(&turn.session, selection)
            .err()
            .expect("unknown delivery cannot authorize replay or replacement");
        assert_eq!(error.status, StatusCode::CONFLICT);
        turn.record(|record| {
            assert_eq!(record.queued_prompts.len(), 2);
            assert_eq!(
                serde_json::to_value(&record.queued_prompts[0]).unwrap(),
                original
            );
            assert_eq!(record.queued_prompts[1].pending_prompt.id, successor);
            assert!(
                record.prompt_deliveries.is_empty(),
                "selection creates no synthetic delivery proof"
            );
        });
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

#[test]
fn provider_handoff_queue_removal_is_saved_without_later_user_activity() {
    let turn = OrdinaryRoot::new("ordinary-handoff-removal");
    let id = turn.enqueue("exact queued handoff", QueuedPromptSource::User);
    let started = turn
        .state
        .start_next_queued_turn_off_lock(&turn.session, false, false)
        .unwrap()
        .unwrap();
    deliver_turn_dispatch(&turn.state, started.dispatch)
        .into_public_result()
        .unwrap();
    assert!(turn.prompt().prompt.ends_with("exact queued handoff"));
    let stamp = turn.record(|record| {
        assert!(record.queued_prompts.is_empty());
        record.mutation_stamp
    });
    let loaded = write_retained_state(&turn);
    let saved = &loaded.sessions[loaded.find_session_index(&turn.session).unwrap()];
    assert!(saved.queued_prompts.is_empty());
    let hydrated = turn.state.get_session(&turn.session).unwrap().session;
    assert!(hydrated.pending_prompts.is_empty());
    assert_eq!(hydrated.session_mutation_stamp, Some(stamp));
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    let count: usize = connection
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id=?1 AND message_id=?2",
            rusqlite::params![turn.session, id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn stale_queue_callback_cannot_interrupt_or_clear_a_replacement_head() {
    let turn = OrdinaryRoot::new("stale-queue-callback");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let original = turn.enqueue("old head", QueuedPromptSource::User);
    let owner = turn.record(|record| QueuedTurnOwner::capture(record).unwrap());
    turn.state
        .cancel_queued_prompt(&turn.session, &original)
        .unwrap();
    let id = turn.enqueue("replacement", QueuedPromptSource::User);
    let before = unknown_retained_entry(&turn, &id, "replacement_unknown");
    turn.state
        .resume_local_session_queue(&turn.session, Some(owner))
        .unwrap();
    turn.record(|record| {
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            before
        );
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(!record.session.preview.contains("interrupted/unknown"));
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn canceled_queue_head_cannot_promote_or_send_the_exposed_successor() {
    let turn = OrdinaryRoot::new("canceled-head-no-promotion");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let canceled = turn.enqueue("cancel me", QueuedPromptSource::User);
    let owner = turn.record(|record| QueuedTurnOwner::capture(record).unwrap());
    let successor = turn.enqueue("new owner", QueuedPromptSource::User);
    turn.state
        .cancel_queued_prompt(&turn.session, &canceled)
        .unwrap();
    let before = turn.saved();
    turn.state
        .resume_local_session_queue(&turn.session, Some(owner))
        .unwrap();
    assert_eq!(turn.saved(), before);
    turn.record(|record| assert_eq!(record.queued_prompts[0].pending_prompt.id, successor));
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn idle_unknown_history_never_turns_internal_or_repeated_stop_into_false_success() {
    let turn = OrdinaryRoot::new("idle-history-stop");
    let id = turn.enqueue("unknown prior provider delivery", QueuedPromptSource::User);
    let original = unknown_retained_entry(&turn, &id, "lost_reply");
    assert_eq!(
        turn.state
            .stop_session_with_options(&turn.session, StopSessionOptions::default())
            .err()
            .expect("idle internal Stop conflicts")
            .status,
        StatusCode::CONFLICT
    );
    for _ in 0..2 {
        assert_eq!(
            turn.state
                .request_stop_session(&turn.session)
                .err()
                .expect("historical authorization is not an active runtime turn")
                .status,
            StatusCode::CONFLICT
        );
    }
    turn.record(|record| {
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            original
        )
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn selected_frozen_mailbox_history_never_resumes_the_newer_wake_instead() {
    let turn = OrdinaryRoot::new("selected-mailbox-no-replacement");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    turn.state
        .queue_mailbox_wakeups_for_session_outcome(
            &turn.session,
            vec![MailboxUnreadWakeup {
                mailbox_id: "selected-mailbox".into(),
                message_id: "first".into(),
                sequence: 7,
                unread_count: 1,
                sender_session_id: "sender".into(),
                sender_name: "Sender".into(),
                topic: Some("first".into()),
            }],
            MailboxWakeupRecovery::NeverWoken,
            true,
        )
        .unwrap();
    let id = turn.record(|record| record.queued_prompts[0].pending_prompt.id.clone());
    let original = unknown_retained_entry(&turn, &id, "unknown_first_sequence");
    turn.state
        .queue_mailbox_wakeups_for_session_outcome(
            &turn.session,
            vec![MailboxUnreadWakeup {
                mailbox_id: "selected-mailbox".into(),
                message_id: "second".into(),
                sequence: 8,
                unread_count: 2,
                sender_session_id: "sender".into(),
                sender_name: "Sender".into(),
                topic: Some("second".into()),
            }],
            MailboxWakeupRecovery::NeverWoken,
            true,
        )
        .unwrap();
    let choice = turn.record(|record| HumanPromptSelection {
        prompt: projected_pending_prompts(record)[0].clone(),
        session_mutation_stamp: record.mutation_stamp,
    });
    assert_eq!(
        turn.state
            .resume_selected_human_prompt(&turn.session, choice)
            .err()
            .unwrap()
            .status,
        StatusCode::CONFLICT
    );
    turn.record(|record| {
        assert_eq!(record.queued_prompts.len(), 2);
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            original
        );
        assert_eq!(
            record.queued_prompts[1]
                .pending_prompt
                .source
                .as_ref()
                .unwrap()
                .mailbox
                .as_ref()
                .unwrap()
                .sequence,
            8
        );
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn delegation_cancel_retires_an_idle_child_without_executing_historical_authorization() {
    let turn = OrdinaryRoot::new("idle-child-cancel-history");
    let created = turn
        .state
        .create_read_only_delegation(
            &turn.session,
            CreateDelegationRequest {
                prompt: "setup".into(),
                title: None,
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .unwrap();
    receive_synchronous_engram_prompt(&turn.state, &turn.receiver, "genuine child setup").unwrap();
    {
        let mut inner = turn.state.inner.lock().unwrap();
        let id = inner.next_message_id();
        let record = inner
            .session_mut(&created.delegation.child_session_id)
            .unwrap();
        record.session.status = SessionStatus::Idle;
        queue_prompt_on_record(
            record,
            PendingPrompt {
                id,
                timestamp: stamp_now(),
                text: "historical child content".into(),
                expanded_text: None,
                source: None,
                attachments: Vec::new(),
                engram_interrupted: false,
                is_engram_retained: false,
            },
            Vec::new(),
        );
        record.queued_prompts[0].engram_evaluate = Some(json!({
            "request":{"operation":"turn_evaluate","idempotency_key":"historical-child"}
        }));
        record.queued_prompts[0].engram_ordinary_handoff = true;
        sync_pending_prompts(record);
        turn.state.commit_locked(&mut inner).unwrap();
    }
    turn.state
        .cancel_delegation(&turn.session, &created.delegation.id)
        .unwrap();
    {
        let inner = turn.state.inner.lock().unwrap();
        let record = &inner.sessions[inner
            .find_session_index(&created.delegation.child_session_id)
            .unwrap()];
        assert!(record.queued_prompts.is_empty());
        assert!(matches!(record.runtime, SessionRuntime::None));
        assert!(!record.session.preview.contains("Prompt retained"));
    }
    turn.assert_no_prompt();
    turn.assert_no_control();
}

// Port of this file's old manual persistence driver: it now drives only the
// existing ordinary content/WAL fences. The real worker is joined before its
// sender is replaced. No fake acknowledgement or parallel writer is created.
fn take_real_ordinary_write_ahead(
    turn: &OrdinaryRoot,
    rx: &mpsc::Receiver<PersistRequest>,
    cache: &mut SqlitePersistConnectionCache,
) -> (Box<PersistFence>, bool) {
    let mut batch = PersistFenceBatch::default();
    loop {
        let request = phase_sync::receive(rx, "ordinary content or WAL persistence request");
        match request {
            PersistRequest::Fence(fence)
                if matches!(
                    &fence.target,
                    PersistFenceTarget::PromptDeliveryWriteAhead { .. }
                ) =>
            {
                let delta = collect_persist_delta_from_shared_state(&turn.state.inner, 0);
                persist_delta_via_cache(cache, turn.state.persistence_path.as_path(), &delta)
                    .unwrap();
                let connection = cache
                    .connection_for(turn.state.persistence_path.as_path())
                    .unwrap();
                let durable = fence.target.is_already_durable(connection).unwrap();
                return (fence, durable);
            }
            request => {
                batch.accept(request);
            }
        }
        let delta = collect_persist_delta_from_shared_state(&turn.state.inner, 0);
        persist_delta_with_fences(
            cache,
            turn.state.persistence_path.as_path(),
            &delta,
            &mut batch,
        )
        .unwrap();
    }
}

#[test]
fn failed_write_ahead_keeps_exact_receipted_content_and_never_hands_off() {
    let mut turn = OrdinaryRoot::new("failed-real-write-ahead");
    turn.state.shutdown_persist_blocking();
    let (tx, rx) = mpsc::channel();
    turn.state.persist_tx = tx;
    let id = turn.enqueue_durable("exact WAL content — źródło 🦀", true);
    assert!(
        turn.state
            .request_queued_prompt_durability(&turn.session, &id)
    );
    let mut cache = SqlitePersistConnectionCache::new();
    let (fence, durable) = take_real_ordinary_write_ahead(&turn, &rx, &mut cache);
    let (nonce, no_handoff_before_ack) = turn.record(|record| {
        (
            record.prompt_deliveries[&id].write_ahead.clone(),
            record.queued_prompts[0].promoted_message_index.is_none()
                && record.prompt_deliveries[&id].turn_generation.is_none(),
        )
    });
    let provider_was_empty = !turn
        .receiver
        .try_iter()
        .any(|command| matches!(command, CodexRuntimeCommand::Prompt { .. }));
    // Resolve the held boundary before making assertions.
    fence.finish(Err(PersistFenceError::WriteFailed(
        "injected WAL acknowledgement failure".into(),
    )));
    let settled = phase_sync::PollGuard::new();
    while !turn.record(|record| {
        record.prompt_deliveries[&id].disposition == PromptDeliveryDisposition::Pending
    }) {
        settled.wait("failed ordinary write-ahead returns to pending");
    }
    turn.state
        .settle_prompt_delivery_write_ahead(&turn.session, &id, &nonce, Ok(()));
    let delta = collect_persist_delta_from_shared_state(&turn.state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        turn.state.persistence_path.as_path(),
        &delta,
        &mut PersistFenceBatch::default(),
    )
    .unwrap();
    assert!(
        durable,
        "the real writer proved this exact WAL before its held acknowledgement"
    );
    assert!(no_handoff_before_ack && provider_was_empty);
    let loaded = load_state(turn.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.session).unwrap()];
    assert_eq!(
        record.prompt_deliveries[&id].disposition,
        PromptDeliveryDisposition::Pending
    );
    assert_ne!(
        record.prompt_deliveries[&id].write_ahead, nonce,
        "failure supersedes the stale ACK"
    );
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(record.queued_prompts[0].pending_prompt.id, id);
    assert_eq!(
        record.queued_prompts[0].pending_prompt.text,
        "exact WAL content — źródło 🦀"
    );
    assert!(record.session.prompt_history.is_empty());
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn real_content_and_write_ahead_cuts_precede_handoff_but_are_not_provider_acceptance() {
    let mut turn = OrdinaryRoot::new("real-wal-before-provider");
    turn.state.shutdown_persist_blocking();
    let (tx, rx) = mpsc::channel();
    turn.state.persist_tx = tx;
    let id = turn.enqueue_durable("receipted ordinary handoff", true);
    assert!(
        turn.state
            .request_queued_prompt_durability(&turn.session, &id)
    );
    let mut cache = SqlitePersistConnectionCache::new();
    let (fence, durable) = take_real_ordinary_write_ahead(&turn, &rx, &mut cache);
    let before = turn.record(|record| {
        (
            record.prompt_deliveries[&id].disposition,
            record.prompt_deliveries[&id].turn_generation,
            record.queued_prompts[0].promoted_message_index,
        )
    });
    let provider_was_empty = !turn
        .receiver
        .try_iter()
        .any(|command| matches!(command, CodexRuntimeCommand::Prompt { .. }));
    // The real SQLite proof releases this exact nonce, once.
    fence.finish(Ok(()));
    let command = turn.prompt();
    let delta = collect_persist_delta_from_shared_state(&turn.state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        turn.state.persistence_path.as_path(),
        &delta,
        &mut PersistFenceBatch::default(),
    )
    .unwrap();
    assert!(durable && provider_was_empty);
    assert_eq!(before, (PromptDeliveryDisposition::HandingOff, None, None));
    assert!(command.prompt.ends_with("receipted ordinary handoff"));
    assert_eq!(command.delivery_receipt.as_ref().unwrap().prompt_id, id);
    turn.record(|record| {
        assert!(record.queued_prompts.is_empty());
        assert_eq!(
            record.prompt_deliveries[&id].disposition,
            PromptDeliveryDisposition::HandingOff
        );
        assert_eq!(
            record.prompt_deliveries[&id].turn_generation,
            Some(command.active_turn_generation)
        );
    });
    let loaded = load_state(turn.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.session).unwrap()];
    assert!(record.queued_prompts.is_empty());
    assert_eq!(
        record.prompt_deliveries[&id].disposition,
        PromptDeliveryDisposition::Ambiguous,
        "channel enqueue and WAL ACK are not provider acceptance after restart"
    );
    assert!(record.prompt_deliveries[&id].resume.is_none());
    turn.assert_no_prompt();
    turn.assert_no_control();
}
