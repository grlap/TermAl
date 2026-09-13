//! Follow-up admission, rollback, queue and compensation regressions, including
//! scenarios moved from delegation_review_results.rs. Shared fixtures live in
//! delegation_support.rs; review submission protocol tests remain separate.

use super::delegation_support::finish_delegation_child_with_assistant_text;
use super::delegation_support::test_app_state_with_delegation_codex_runtime;
use super::delegation_support::{install_required_review_delegation, structured_review_request};
use super::engram_host_adapter::enable_test_project_engram;
use super::mailboxes::mailbox_test_state;
use super::*;

#[test]
fn manual_unarchive_is_not_owned_by_a_later_canceled_or_rejected_followup() {
    for path in ["queue cancel", "parent cancel", "late rejection"] {
        let (state, _, parent) = mailbox_test_state();
        let (delegation, child) = install_required_review_delegation(&state, &parent);
        state
            .submit_delegation_review_result(&child, structured_review_request())
            .unwrap();
        finish_delegation_child_with_assistant_text(&state, &child, "Previous review.");
        state.refresh_delegation_for_child_session(&child).unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&child).unwrap();
            let record = &mut inner.sessions[index];
            record.external_session_id = Some("manually-restored-thread".to_owned());
            set_record_codex_thread_state(record, CodexThreadState::Archived);
            let release = Arc::new(CodexDelegationRelease {
                terminal_release: true,
                ..Default::default()
            });
            release.finish(CodexReleaseOutcome::Archived);
            record.codex_delegation_release = Some(release);
        }
        let (runtime, input_rx, _) = test_shared_codex_runtime("manual-unarchive-owner");
        *state.shared_codex_runtime.lock().unwrap() = Some(runtime);
        let worker = std::thread::spawn(move || {
            let CodexRuntimeCommand::JsonRpcRequest {
                method,
                response_tx,
                ..
            } = phase_sync::receive(&input_rx, "manual unarchive")
            else {
                panic!("expected RPC")
            };
            assert_eq!(method, "thread/unarchive");
            response_tx.send(Ok(json!({}))).unwrap();
            input_rx
        });
        state.unarchive_codex_thread(&child).unwrap();
        let input_rx = worker.join().unwrap();
        if path == "late rejection" {
            let previous = {
                let mut inner = state.inner.lock().unwrap();
                let watermark = delegation_last_user_prompt_id_locked(&inner, &child);
                inner.delegation_followup_admissions.insert(
                    delegation.clone(),
                    FollowupAdmissionReservation::new(watermark),
                );
                inner.delegations[inner.find_delegation_index(&delegation).unwrap()].clone()
            };
            let mut admission = DelegationFollowupAdmission {
                state: state.clone(),
                previous: previous.clone(),
                restore_candidate: true,
                released: false,
            };
            // Public cancellation during off-lock preparation rejects final
            // admission while preserving the existing terminal result.
            state.cancel_delegation(&parent, &delegation).unwrap();
            let error = state
                .dispatch_turn_with_followup(
                    &child,
                    SendMessageRequest {
                        text: "Rejected follow-up".to_owned(),
                        expanded_text: None,
                        attachments: vec![],
                        source_session_id: None,
                        source_mailbox: None,
                    },
                    Some(&previous),
                )
                .err()
                .expect("canceled reservation must reject admission");
            assert_eq!(error.status, StatusCode::CONFLICT);
            assert!(!admission.rollback_before_prompt().unwrap());
            admission.release().unwrap();
        } else {
            {
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&child).unwrap();
                inner.sessions[index].engram.project_reset_in_progress = true;
            }
            let queued = state
                .followup_delegation(&parent, &delegation, "Canceled follow-up".to_owned())
                .unwrap();
            if path == "parent cancel" {
                state.cancel_delegation(&parent, &delegation).unwrap();
            } else {
                state
                    .cancel_queued_prompt(
                        &child,
                        queued
                            .delegation
                            .queued_followup_prompt_id
                            .as_deref()
                            .unwrap(),
                    )
                    .unwrap();
            }
        }
        state.get_delegation(&parent, &delegation).unwrap();
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&child).unwrap()];
        assert_eq!(
            record.session.codex_thread_state,
            Some(CodexThreadState::Active),
            "{path}"
        );
        let release = record.codex_delegation_release.as_ref().unwrap();
        assert_eq!(
            *release.outcome.lock().unwrap(),
            Some(CodexReleaseOutcome::Restored)
        );
        assert!(!release.needs_followup_compensation());
        assert!(
            input_rx.try_recv().is_err(),
            "manual Unarchive must not enqueue an archive: {path}"
        );
    }
}

#[test]
fn delayed_followup_direct_promotion_commit_failure_settles_without_delivery() {
    for blocked in [false, true] {
        let (state, _, parent) = mailbox_test_state();
        let (delegation, child) = install_required_review_delegation(&state, &parent);
        state
            .submit_delegation_review_result(&child, structured_review_request())
            .unwrap();
        finish_delegation_child_with_assistant_text(&state, &child, "Previous review.");
        state.refresh_delegation_for_child_session(&child).unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&child).unwrap();
            inner.sessions[index].engram.project_reset_in_progress = true;
        }
        let admitted = state
            .followup_delegation(&parent, &delegation, "Undelivered follow-up".to_owned())
            .unwrap();
        let prompt_id = admitted.delegation.queued_followup_prompt_id.unwrap();
        let wait = state
            .create_delegation_wait(
                &parent,
                CreateDelegationWaitRequest {
                    delegation_ids: vec![delegation.clone()],
                    mode: DelegationWaitMode::All,
                    title: None,
                },
            )
            .unwrap();
        let (runtime, input_rx, _) = test_shared_codex_runtime("delayed-promotion-commit");
        *state.shared_codex_runtime.lock().unwrap() = Some(runtime);
        {
            let mut inner = state.inner.lock().unwrap();
            assert!(!inner
                .delegation_followup_admissions
                .contains_key(&delegation));
            let index = inner.find_session_index(&child).unwrap();
            inner.sessions[index].engram.project_reset_in_progress = false;
            inner.sessions[index].set_auto_dispatch_blocked(blocked);
            state.commit_locked(&mut inner).unwrap();
        }
        // A real SQLite error at the second commit: queue metadata is valid,
        // but inserting the promoted prompt's transcript boundary is rejected.
        let connection = rusqlite::Connection::open(&*state.persistence_path).unwrap();
        connection
            .execute_batch(&format!(
                "CREATE TRIGGER reject_followup_promotion BEFORE INSERT ON messages
             WHEN NEW.message_id = '{}'
             BEGIN SELECT RAISE(ABORT, 'injected-promotion-commit-failure'); END;",
                prompt_id.replace('\'', "''"),
            ))
            .unwrap();
        let queue_commit_revision = state.inner.lock().unwrap().revision + 1;
        let mut snapshots = state.subscribe_events();
        let error = state
            .dispatch_turn(
                &child,
                SendMessageRequest {
                    text: "Additional user prompt".to_owned(),
                    expanded_text: None,
                    attachments: vec![],
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
            .err()
            .expect("promotion commit must fail");
        assert!(
            error.message.contains("injected-promotion-commit-failure"),
            "{}",
            error.message
        );
        let first: Value = serde_json::from_str(&snapshots.try_recv().unwrap()).unwrap();
        let queued_child = first["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|session| session["id"] == child)
            .unwrap();
        assert_eq!(
            first["revision"], queue_commit_revision,
            "the additional-message queue commit published before promotion failed"
        );
        assert_eq!(queued_child["status"], "idle");
        let inner = state.inner.lock().unwrap();
        let failed = &inner.delegations[inner.find_delegation_index(&delegation).unwrap()];
        assert_eq!(failed.status, DelegationStatus::Failed);
        let record = &inner.sessions[inner.find_session_index(&child).unwrap()];
        assert_eq!(record.session.status, SessionStatus::Error);
        assert!(matches!(record.runtime, SessionRuntime::None));
        assert!(record.queued_prompts.is_empty());
        assert!(!record
            .session
            .messages
            .iter()
            .any(|message| message.id() == prompt_id));
        assert!(!inner
            .delegation_waits
            .iter()
            .any(|item| item.id == wait.wait.id));
        assert_eq!(
            inner.sessions[inner.find_session_index(&parent).unwrap()]
                .queued_prompts
                .iter()
                .filter(|entry| entry.source == QueuedPromptSource::Orchestrator)
                .count(),
            1
        );
        drop(inner);
        while let Ok(command) = input_rx.try_recv() {
            assert!(
                !matches!(command, CodexRuntimeCommand::Prompt { .. }),
                "no prompt was delivered"
            );
        }
        let disk = load_state(&state.persistence_path).unwrap().unwrap();
        assert_eq!(
            disk.delegations
                .iter()
                .find(|row| row.id == delegation)
                .unwrap()
                .status,
            DelegationStatus::Failed
        );
    }
}

#[test]
fn canceled_restored_followup_keeps_cleanup_and_manual_archive_retries_same_attempt() {
    for cancel_parent in [false, true] {
        let (state, _, parent) = mailbox_test_state();
        let (delegation, child) = install_required_review_delegation(&state, &parent);
        state
            .submit_delegation_review_result(&child, structured_review_request())
            .unwrap();
        finish_delegation_child_with_assistant_text(&state, &child, "Previous review.");
        state.refresh_delegation_for_child_session(&child).unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&child).unwrap();
            let record = &mut inner.sessions[index];
            record.engram.project_reset_in_progress = true;
            record.external_session_id = Some("canceled-restored-thread".to_owned());
            set_record_codex_thread_state(record, CodexThreadState::Archived);
            let release = Arc::new(CodexDelegationRelease {
                terminal_release: true,
                ..Default::default()
            });
            release.finish(CodexReleaseOutcome::Archived);
            record.codex_delegation_release = Some(release);
        }
        let (runtime, input_rx, _) = test_shared_codex_runtime("restore-then-cancel");
        *state.shared_codex_runtime.lock().unwrap() = Some(runtime);
        let restore = std::thread::spawn(move || {
            let CodexRuntimeCommand::JsonRpcRequest {
                method,
                response_tx,
                ..
            } = phase_sync::receive(&input_rx, "restore canceled follow-up")
            else {
                panic!("expected RPC")
            };
            assert_eq!(method, "thread/unarchive");
            response_tx.send(Ok(json!({}))).unwrap();
            input_rx
        });
        let admitted = state
            .followup_delegation(
                &parent,
                &delegation,
                "Cancel this queued follow-up".to_owned(),
            )
            .unwrap();
        let _old_receiver = restore.join().unwrap();
        *state.shared_codex_runtime.lock().unwrap() = None;
        if cancel_parent {
            state.cancel_delegation(&parent, &delegation).unwrap();
        } else {
            state
                .cancel_queued_prompt(
                    &child,
                    admitted
                        .delegation
                        .queued_followup_prompt_id
                        .as_deref()
                        .unwrap(),
                )
                .unwrap();
        }
        let canceled = {
            let inner = state.inner.lock().unwrap();
            let canceled =
                inner.delegations[inner.find_delegation_index(&delegation).unwrap()].clone();
            assert_eq!(canceled.status, DelegationStatus::Canceled);
            let record = &inner.sessions[inner.find_session_index(&child).unwrap()];
            assert!(matches!(record.runtime, SessionRuntime::None));
            assert!(record.queued_prompts.is_empty());
            let release = record.codex_delegation_release.as_ref().unwrap();
            assert!(release
                .compensation_pending
                .load(std::sync::atomic::Ordering::Acquire));
            assert_eq!(
                release.undurable_terminal.lock().unwrap().as_ref(),
                Some(&canceled)
            );
            canceled
        };
        let (runtime, input_rx, _) = test_shared_codex_runtime("canceled-manual-archive-retry");
        *state.shared_codex_runtime.lock().unwrap() = Some(runtime);
        let archive = std::thread::spawn(move || {
            let CodexRuntimeCommand::JsonRpcRequest {
                method,
                response_tx,
                ..
            } = phase_sync::receive(&input_rx, "manual archive canceled attempt")
            else {
                panic!("expected RPC")
            };
            assert_eq!(method, "thread/archive");
            response_tx.send(Ok(json!({}))).unwrap();
            input_rx
        });
        state.archive_codex_thread(&child).unwrap();
        let input_rx = archive.join().unwrap();
        assert_eq!(
            state
                .get_delegation(&parent, &delegation)
                .unwrap()
                .delegation,
            canceled
        );
        assert!(state
            .followup_delegation(&parent, &delegation, "Must remain canceled".to_owned())
            .is_err());
        assert!(input_rx.try_recv().is_err());
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&child).unwrap()];
        assert!(record_has_archived_codex_thread(record));
        assert!(record
            .codex_delegation_release
            .as_ref()
            .unwrap()
            .undurable_terminal
            .lock()
            .unwrap()
            .is_none());
    }
}

#[test]
fn followup_admission_commit_failure_settles_undelivered_attempt_and_releases_runtime() {
    for queued in [false, true] {
        let (state, _, parent) = mailbox_test_state();
        let (delegation, child) = install_required_review_delegation(&state, &parent);
        state
            .submit_delegation_review_result(&child, structured_review_request())
            .unwrap();
        finish_delegation_child_with_assistant_text(&state, &child, "Old structured review.");
        state.refresh_delegation_for_child_session(&child).unwrap();
        let (runtime, input_rx, _) = test_shared_codex_runtime("failed-admission-commit");
        *state.shared_codex_runtime.lock().unwrap() = Some(runtime);
        let previous = {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&child).unwrap();
            inner.sessions[index].engram.project_reset_in_progress = queued;
            let previous =
                inner.delegations[inner.find_delegation_index(&delegation).unwrap()].clone();
            let watermark = delegation_last_user_prompt_id_locked(&inner, &child);
            inner.delegation_followup_admissions.insert(
                delegation.clone(),
                FollowupAdmissionReservation::new(watermark),
            );
            previous
        };
        let mut admission = DelegationFollowupAdmission {
            state: state.clone(),
            previous: previous.clone(),
            restore_candidate: false,
            released: false,
        };
        let failed_path =
            test_temp_dir().join(format!("followup-admission-commit-{}", Uuid::new_v4()));
        fs::create_dir_all(&failed_path).unwrap();
        let mut failing = state.clone();
        failing.persistence_path = Arc::new(failed_path.clone());
        let error = failing
            .dispatch_turn_with_followup(
                &child,
                SendMessageRequest {
                    text: "Never delivered".to_owned(),
                    expanded_text: None,
                    attachments: vec![],
                    source_session_id: None,
                    source_mailbox: None,
                },
                Some(&previous),
            )
            .err()
            .expect("admission commit must fail");
        assert!(
            error.message.contains("failed to persist session state"),
            "{}",
            error.message
        );
        if queued {
            {
                let inner = state.inner.lock().unwrap();
                assert_eq!(
                    inner.delegations[inner.find_delegation_index(&delegation).unwrap()],
                    previous
                );
                assert!(inner.sessions[inner.find_session_index(&child).unwrap()]
                    .queued_prompts
                    .is_empty());
            }
            admission.release().unwrap();
            state
                .commit_locked(&mut state.inner.lock().unwrap())
                .unwrap();
            let disk = load_state(&state.persistence_path).unwrap().unwrap();
            assert_eq!(
                disk.delegations
                    .iter()
                    .find(|record| record.id == delegation)
                    .unwrap(),
                &previous
            );
            assert!(input_rx.try_recv().is_err());
            remove_test_directory(failed_path);
            continue;
        }
        {
            let inner = state.inner.lock().unwrap();
            let failed = &inner.delegations[inner.find_delegation_index(&delegation).unwrap()];
            assert_eq!(failed.status, DelegationStatus::Failed);
            assert!(failed
                .result
                .as_ref()
                .unwrap()
                .summary
                .contains("admission could not be persisted"));
            let record = &inner.sessions[inner.find_session_index(&child).unwrap()];
            assert_eq!(record.session.status, SessionStatus::Error);
            assert!(matches!(record.runtime, SessionRuntime::None));
            assert!(record.queued_prompts.is_empty());
            assert!(inner.delegation_followup_admissions[&delegation]
                .failure
                .is_some());
        }
        // Cleanup also runs if storage is still unavailable at release.
        admission.state.persistence_path = Arc::new(failed_path.clone());
        assert!(admission.release().is_err());
        state
            .commit_locked(&mut state.inner.lock().unwrap())
            .unwrap();
        while let Ok(command) = input_rx.try_recv() {
            assert!(!matches!(command, CodexRuntimeCommand::Prompt { .. }));
        }
        let disk = load_state(&state.persistence_path).unwrap().unwrap();
        assert_eq!(
            disk.delegations
                .iter()
                .find(|row| row.id == delegation)
                .unwrap()
                .status,
            DelegationStatus::Failed
        );
        assert!(!state
            .inner
            .lock()
            .unwrap()
            .delegation_followup_admissions
            .contains_key(&delegation));
        remove_test_directory(failed_path);
    }
}

#[test]
fn canceled_queued_followup_never_reuses_old_result_or_discards_successor() {
    for reviewer in [false, true] {
        for successor_source in [
            None,
            Some(QueuedPromptSource::User),
            Some(QueuedPromptSource::Mailbox),
            Some(QueuedPromptSource::Orchestrator),
        ] {
            let successor = successor_source == Some(QueuedPromptSource::User);
            let (state, _, parent) = mailbox_test_state();
            let (delegation, child) = install_required_review_delegation(&state, &parent);
            if reviewer {
                state
                    .submit_delegation_review_result(&child, structured_review_request())
                    .unwrap();
            } else {
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_delegation_index(&delegation).unwrap();
                inner.delegations[index].mode = DelegationMode::Explorer;
            }
            finish_delegation_child_with_assistant_text(
                &state,
                &child,
                "## Result\nStatus: completed\n\nSummary:\nOld result.",
            );
            state.refresh_delegation_for_child_session(&child).unwrap();
            {
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&child).unwrap();
                inner.sessions[index].engram.project_reset_in_progress = true;
            }
            let admitted = state
                .followup_delegation(&parent, &delegation, "Cancel me".to_owned())
                .unwrap();
            let prompt = admitted.delegation.queued_followup_prompt_id.unwrap();
            let next = if let Some(successor_source) = successor_source {
                let mut inner = state.inner.lock().unwrap();
                let next = inner.next_message_id();
                let index = inner.find_session_index(&child).unwrap();
                queue_prompt_on_record_with_source(
                    &mut inner.sessions[index],
                    PendingPrompt {
                        id: next.clone(),
                        timestamp: stamp_now(),
                        text: "Successor".to_owned(),
                        expanded_text: None,
                        attachments: vec![],
                        source: None,
                    },
                    vec![],
                    successor_source,
                );
                Some(next)
            } else {
                None
            };
            {
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&parent).unwrap();
                inner.sessions[index].session.status = SessionStatus::Active;
            }
            let wait = state
                .create_delegation_wait(
                    &parent,
                    CreateDelegationWaitRequest {
                        delegation_ids: vec![delegation.clone()],
                        mode: if reviewer {
                            DelegationWaitMode::All
                        } else {
                            DelegationWaitMode::Any
                        },
                        title: None,
                    },
                )
                .unwrap();
            let mut events = state.subscribe_delta_events();
            state.cancel_queued_prompt(&child, &prompt).unwrap();
            // Inspect before any status/result poll can refresh the waits.
            {
                let inner = state.inner.lock().unwrap();
                assert_eq!(
                    inner
                        .delegation_waits
                        .iter()
                        .any(|item| item.id == wait.wait.id),
                    successor
                );
                let parent_record = &inner.sessions[inner.find_session_index(&parent).unwrap()];
                assert_eq!(parent_record.queued_prompts.len(), usize::from(!successor));
                if !successor {
                    assert!(matches!(
                        inner.sessions[inner.find_session_index(&child).unwrap()].runtime,
                        SessionRuntime::None
                    ));
                }
            }
            let mut canceled = 0;
            while let Ok(event) = events.try_recv() {
                let event: Value = serde_json::from_str(&event).unwrap();
                if event["type"] == "delegationCanceled" && event["delegationId"] == delegation {
                    canceled += 1;
                }
            }
            assert_eq!(canceled, usize::from(!successor));
            for _ in 0..2 {
                let status = state
                    .get_delegation(&parent, &delegation)
                    .unwrap()
                    .delegation;
                assert_eq!(
                    status.status,
                    if successor {
                        DelegationStatus::Running
                    } else {
                        DelegationStatus::Canceled
                    }
                );
                if successor {
                    assert!(state.get_delegation_result(&parent, &delegation).is_err());
                    assert_eq!(status.queued_followup_prompt_id, next);
                    let inner = state.inner.lock().unwrap();
                    let record = &inner.sessions[inner.find_session_index(&child).unwrap()];
                    assert_eq!(record.queued_prompts.len(), 1);
                    assert_eq!(
                        record.queued_prompts[0].pending_prompt.id,
                        next.as_ref().unwrap().as_str()
                    );
                } else {
                    assert!(status
                        .result
                        .unwrap()
                        .summary
                        .contains("canceled before it started"));
                }
            }
        }
    }
}

#[test]
fn delayed_queued_followup_start_failure_settles_after_reservation_release() {
    for reviewer in [false, true] {
        let (state, _, parent) = mailbox_test_state();
        let (delegation, child) = install_required_review_delegation(&state, &parent);
        if reviewer {
            state
                .submit_delegation_review_result(&child, structured_review_request())
                .unwrap();
        } else {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_delegation_index(&delegation).unwrap();
            inner.delegations[index].mode = DelegationMode::Explorer;
        }
        finish_delegation_child_with_assistant_text(
            &state,
            &child,
            "## Result\nStatus: completed\n\nSummary:\nOld result.",
        );
        state.refresh_delegation_for_child_session(&child).unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&child).unwrap();
            inner.sessions[index].engram.project_reset_in_progress = true;
            let index = inner.find_session_index(&parent).unwrap();
            inner.sessions[index].session.status = SessionStatus::Active;
        }
        let admitted = state
            .followup_delegation(&parent, &delegation, "Delayed work".to_owned())
            .unwrap();
        assert_eq!(admitted.delegation.status, DelegationStatus::Running);
        let wait = state
            .create_delegation_wait(
                &parent,
                CreateDelegationWaitRequest {
                    delegation_ids: vec![delegation.clone()],
                    mode: if reviewer {
                        DelegationWaitMode::All
                    } else {
                        DelegationWaitMode::Any
                    },
                    title: None,
                },
            )
            .unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            assert!(!inner
                .delegation_followup_admissions
                .contains_key(&delegation));
            let index = inner.find_session_index(&child).unwrap();
            inner.sessions[index].engram.project_reset_in_progress = false;
        }
        // No runtime transport is installed and spawning is disabled by this
        // fixture. Failure happens only after the original Queued response.
        let error = state
            .start_next_queued_turn_off_lock(&child, true, false)
            .err()
            .expect("delayed spawn fails");
        assert!(format!("{error:#}").contains("spawn"), "{error:#}");
        let inner = state.inner.lock().unwrap();
        let finished = &inner.delegations[inner.find_delegation_index(&delegation).unwrap()];
        assert_eq!(finished.status, DelegationStatus::Failed);
        assert!(finished.result.as_ref().unwrap().summary.contains("spawn"));
        assert!(finished.queued_followup_prompt_id.is_none());
        let record = &inner.sessions[inner.find_session_index(&child).unwrap()];
        assert!(record.queued_prompts.is_empty());
        assert!(matches!(record.runtime, SessionRuntime::None));
        assert!(!inner
            .delegation_waits
            .iter()
            .any(|item| item.id == wait.wait.id));
        assert_eq!(
            inner.sessions[inner.find_session_index(&parent).unwrap()]
                .queued_prompts
                .len(),
            1
        );
    }
}

#[test]
fn followup_admission_uses_allocated_prompt_id_after_queue_prioritization() {
    let (state, _, parent) = mailbox_test_state();
    let (delegation, child) = install_required_review_delegation(&state, &parent);
    state
        .submit_delegation_review_result(&child, structured_review_request())
        .unwrap();
    finish_delegation_child_with_assistant_text(&state, &child, "Previous review.");
    state.refresh_delegation_for_child_session(&child).unwrap();
    let mut inner = state.inner.lock().unwrap();
    let previous = inner.delegations[inner.find_delegation_index(&delegation).unwrap()].clone();
    let watermark = delegation_last_user_prompt_id_locked(&inner, &child);
    inner.delegation_followup_admissions.insert(
        delegation.clone(),
        FollowupAdmissionReservation::new(watermark),
    );
    let mailbox_id = inner.next_message_id();
    let followup_id = inner.next_message_id();
    let index = inner.find_session_index(&child).unwrap();
    // Deliberately exercise a mixed queue even though normal terminal cleanup
    // empties it today. Prioritization must not turn the tail into our identity.
    for (id, source) in [
        (mailbox_id.clone(), QueuedPromptSource::Mailbox),
        (followup_id.clone(), QueuedPromptSource::User),
    ] {
        queue_prompt_on_record_with_source(
            &mut inner.sessions[index],
            PendingPrompt {
                id,
                timestamp: stamp_now(),
                text: "Queued work".to_owned(),
                expanded_text: None,
                attachments: vec![],
                source: None,
            },
            vec![],
            source,
        );
    }
    prioritize_user_queued_prompts(&mut inner.sessions[index]);
    assert_eq!(
        inner.sessions[index]
            .queued_prompts
            .back()
            .unwrap()
            .pending_prompt
            .id,
        mailbox_id
    );
    state
        .commit_followup_prompt_locked(&mut inner, Some(&previous), &followup_id, true)
        .unwrap();
    assert_eq!(
        inner.delegations[inner.find_delegation_index(&delegation).unwrap()]
            .queued_followup_prompt_id
            .as_ref(),
        Some(&followup_id)
    );
    drop(inner);
    state
        .release_delegation_followup_reservation(&delegation)
        .unwrap();
}

#[test]
fn queued_followup_survives_project_reset_polling_and_dispatches_once() {
    for (reviewer, failed) in [(false, true), (true, true), (false, false), (true, false)] {
        let (state, _, parent) = mailbox_test_state();
        let (delegation, child) = install_required_review_delegation(&state, &parent);
        if reviewer && !failed {
            state
                .submit_delegation_review_result(&child, structured_review_request())
                .unwrap();
        } else if !reviewer {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_delegation_index(&delegation).unwrap();
            inner.delegations[index].mode = DelegationMode::Explorer;
        }
        finish_delegation_child_with_assistant_text(
            &state,
            &child,
            "## Result\nStatus: completed\n\nSummary:\nOld result.",
        );
        if failed {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_delegation_index(&delegation).unwrap();
            mark_delegation_failed_locked(&mut inner, index, "Previous child failed.");
            state.commit_locked(&mut inner).unwrap();
            assert_eq!(
                inner.sessions[inner.find_session_index(&child).unwrap()]
                    .session
                    .status,
                SessionStatus::Error
            );
        } else {
            state.refresh_delegation_for_child_session(&child).unwrap();
        }
        let (runtime, input_rx, _) = test_shared_codex_runtime("queued-followup");
        *state.shared_codex_runtime.lock().unwrap() = Some(runtime);
        let before_attempt = {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&child).unwrap();
            inner.sessions[index].engram.project_reset_in_progress = true;
            inner.delegations[inner.find_delegation_index(&delegation).unwrap()]
                .review_result_submission_attempt
        };
        let mut events = state.subscribe_delta_events();
        let response = state
            .followup_delegation(&parent, &delegation, "New follow-up".to_owned())
            .unwrap();
        assert_eq!(response.delegation.status, DelegationStatus::Running);
        let persisted: DelegationRecord =
            serde_json::from_value(serde_json::to_value(&response.delegation).unwrap()).unwrap();
        assert_eq!(
            persisted, response.delegation,
            "queued admission identity survives storage encoding"
        );
        assert!(persisted.queued_followup_prompt_id.is_some());
        assert_eq!(
            response.delegation.review_result_submission_attempt,
            before_attempt + u32::from(reviewer)
        );
        for _ in 0..3 {
            assert_eq!(
                state
                    .get_delegation(&parent, &delegation)
                    .unwrap()
                    .delegation
                    .status,
                DelegationStatus::Running
            );
            assert!(state.get_delegation_result(&parent, &delegation).is_err());
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&child).unwrap()];
            if reviewer {
                assert_eq!(
                    inner.delegations[inner.find_delegation_index(&delegation).unwrap()]
                        .review_result_recovery_probe_attempt,
                    None,
                    "polling a queued attempt must not spend its durable recovery probe"
                );
            }
            assert_eq!(record.queued_prompts.len(), 1);
            assert_eq!(
                response.delegation.queued_followup_prompt_id.as_deref(),
                Some(record.queued_prompts[0].pending_prompt.id.as_str())
            );
            assert_eq!(
                record.queued_prompts[0].pending_prompt.text,
                "New follow-up"
            );
            assert!(!inner
                .delegation_followup_admissions
                .contains_key(&delegation));
        }
        assert!(state
            .start_next_queued_turn_off_lock(&child, true, false)
            .unwrap()
            .is_none());
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&child).unwrap();
            inner.sessions[index].engram.project_reset_in_progress = false;
        }
        let started = state
            .start_next_queued_turn_off_lock(&child, true, false)
            .unwrap()
            .unwrap();
        deliver_turn_dispatch(&state, started.dispatch).unwrap();
        assert!(matches!(
            phase_sync::receive(&input_rx, "queued follow-up delivery"),
            CodexRuntimeCommand::Prompt { .. }
        ));
        assert!(input_rx.try_recv().is_err());
        assert!(state
            .start_next_queued_turn_off_lock(&child, true, false)
            .unwrap()
            .is_none());
        let mut updates = 0;
        while let Ok(event) = events.try_recv() {
            let event: Value = serde_json::from_str(&event).unwrap();
            if event["type"] == "delegationUpdated" && event["delegationId"] == delegation {
                updates += 1;
            }
        }
        assert_eq!(
            updates, 1,
            "queue admission and later promotion must re-arm once"
        );
        // A queue left over after an actually started turn must still be
        // discarded on completion. It is not the first-prompt admission fence.
        {
            let mut inner = state.inner.lock().unwrap();
            let queued_id = inner.next_message_id();
            let index = inner.find_session_index(&child).unwrap();
            queue_prompt_on_record_with_source(
                &mut inner.sessions[index],
                PendingPrompt {
                    id: queued_id,
                    timestamp: stamp_now(),
                    text: "Do not run after terminal result".to_owned(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source: None,
                },
                Vec::new(),
                QueuedPromptSource::User,
            );
        }
        if reviewer {
            // Simulate loss of the primary-row write after the mailbox commit.
            // Recovery must still probe this attempt after earlier queue polls.
            let submission = state
                .validate_delegation_review_submission(&child, structured_review_request())
                .unwrap();
            let appended = state
                .mailbox_store
                .append(&MailboxAppendInput {
                    sender_session_id: child.clone(),
                    sender_name: submission.sender_name.clone(),
                    target_session_id: parent.clone(),
                    target_name: submission.target_name.clone(),
                    body: serde_json::to_string(&submission.envelope).unwrap(),
                    idempotency_key: delegation_review_result_idempotency_key(
                        &delegation,
                        submission.submission_attempt,
                    ),
                    topic: Some(DELEGATION_REVIEW_RESULT_TOPIC.to_owned()),
                    state_stamp: Some(format!("{}:{}", delegation, submission.submission_attempt)),
                })
                .unwrap();
            state
                .mailbox_store
                .record_initial_dispatch_outcome(&appended.receipt.message_id, "durableButNotWoken")
                .unwrap();
        }
        finish_delegation_child_with_assistant_text(
            &state,
            &child,
            "## Result\nStatus: completed\n\nSummary:\nNew result.",
        );
        state.refresh_delegation_for_child_session(&child).unwrap();
        assert_eq!(
            state
                .get_delegation(&parent, &delegation)
                .unwrap()
                .delegation
                .status,
            DelegationStatus::Completed
        );
        let inner = state.inner.lock().unwrap();
        let finished = &inner.delegations[inner.find_delegation_index(&delegation).unwrap()];
        assert!(
            finished.queued_followup_prompt_id.is_none(),
            "terminal attempts clear their queue fence"
        );
        if reviewer {
            assert_eq!(
                finished.result.as_ref().unwrap().summary,
                "One medium issue found."
            );
            assert_eq!(
                finished.review_result_schema_version,
                Some(DELEGATION_REVIEW_RESULT_SCHEMA_VERSION)
            );
        }
        assert!(inner.sessions[inner.find_session_index(&child).unwrap()]
            .queued_prompts
            .is_empty());
    }
}

#[test]
fn followup_reservation_defers_all_wait_until_admission_or_rejection() {
    for succeeds in [false, true] {
        let (state, _, parent) = mailbox_test_state();
        let (a, child_a) = install_required_review_delegation(&state, &parent);
        let (b, child_b) = install_required_review_delegation(&state, &parent);
        state
            .submit_delegation_review_result(&child_a, structured_review_request())
            .unwrap();
        finish_delegation_child_with_assistant_text(&state, &child_a, "Old A review.");
        state
            .refresh_delegation_for_child_session(&child_a)
            .unwrap();
        let (runtime, input_rx, _) = test_shared_codex_runtime("followup-wait");
        *state.shared_codex_runtime.lock().unwrap() = Some(runtime);
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&parent).unwrap();
            inner.sessions[index].session.status = SessionStatus::Active;
            let index = inner.find_session_index(&child_a).unwrap();
            inner.sessions[index].external_session_id = Some("wait-thread".to_owned());
            set_record_codex_thread_state(&mut inner.sessions[index], CodexThreadState::Archived);
            let release = Arc::new(CodexDelegationRelease {
                terminal_release: true,
                ..Default::default()
            });
            release.finish(CodexReleaseOutcome::Archived);
            inner.sessions[index].codex_delegation_release = Some(release);
        }
        let wait = state
            .create_delegation_wait(
                &parent,
                CreateDelegationWaitRequest {
                    delegation_ids: vec![a.clone(), b.clone()],
                    mode: DelegationWaitMode::All,
                    title: None,
                },
            )
            .unwrap();
        let following = state.clone();
        let following_parent = parent.clone();
        let following_a = a.clone();
        let worker = std::thread::spawn(move || {
            following.followup_delegation(&following_parent, &following_a, "Follow up A".to_owned())
        });
        let CodexRuntimeCommand::JsonRpcRequest {
            method,
            response_tx,
            ..
        } = phase_sync::receive(&input_rx, "A restore parked while B completes")
        else {
            panic!("expected restore RPC")
        };
        assert_eq!(method, "thread/unarchive");
        state
            .submit_delegation_review_result(&child_b, structured_review_request())
            .unwrap();
        finish_delegation_child_with_assistant_text(&state, &child_b, "B review done.");
        state
            .refresh_delegation_for_child_session(&child_b)
            .unwrap();
        {
            let inner = state.inner.lock().unwrap();
            assert!(
                inner
                    .delegation_waits
                    .iter()
                    .any(|item| item.id == wait.wait.id),
                "A's reservation must keep All pending"
            );
            assert!(inner.sessions[inner.find_session_index(&parent).unwrap()]
                .queued_prompts
                .is_empty());
        }
        response_tx
            .send(if succeeds {
                Ok(json!({}))
            } else {
                Err(CodexResponseError::JsonRpc("restore rejected".to_owned()))
            })
            .unwrap();
        let result = worker.join().unwrap();
        if succeeds {
            result.unwrap();
            assert!(matches!(
                phase_sync::receive(&input_rx, "A follow-up prompt"),
                CodexRuntimeCommand::Prompt { .. }
            ));
            {
                let inner = state.inner.lock().unwrap();
                assert!(inner
                    .delegation_waits
                    .iter()
                    .any(|item| item.id == wait.wait.id));
                assert!(!inner.delegation_followup_admissions.contains_key(&a));
            }
            // Avoid testing external archive here; the wait concerns the review result.
            {
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&child_a).unwrap();
                inner.sessions[index].external_session_id = None;
            }
            state
                .submit_delegation_review_result(&child_a, structured_review_request())
                .unwrap();
            finish_delegation_child_with_assistant_text(&state, &child_a, "New A review done.");
            state
                .refresh_delegation_for_child_session(&child_a)
                .unwrap();
        } else {
            assert!(result.is_err());
        }
        let inner = state.inner.lock().unwrap();
        assert!(!inner
            .delegation_waits
            .iter()
            .any(|item| item.id == wait.wait.id));
        assert_eq!(
            inner.sessions[inner.find_session_index(&parent).unwrap()]
                .queued_prompts
                .len(),
            1,
            "exactly one resume after the new attempt finishes or admission is rejected"
        );
    }
}

#[test]
fn released_followup_guard_cannot_remove_the_next_reservation() {
    let (state, _, parent) = mailbox_test_state();
    let (id, child) = install_required_review_delegation(&state, &parent);
    let previous = {
        let mut inner = state.inner.lock().unwrap();
        let watermark = delegation_last_user_prompt_id_locked(&inner, &child);
        inner
            .delegation_followup_admissions
            .insert(id.clone(), FollowupAdmissionReservation::new(watermark));
        inner.delegations[inner.find_delegation_index(&id).unwrap()].clone()
    };
    let mut guard = DelegationFollowupAdmission {
        state: state.clone(),
        previous,
        restore_candidate: false,
        released: false,
    };
    guard.release().unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        inner
            .delegation_followup_admissions
            .insert(id.clone(), FollowupAdmissionReservation::new(None));
    }
    drop(guard);
    assert!(state
        .inner
        .lock()
        .unwrap()
        .delegation_followup_admissions
        .contains_key(&id));
}

#[test]
fn rejected_followup_preserves_completed_structured_review() {
    for rejection in ["attachment", "empty", "boot", "reserved"] {
        let (state, _, parent) = mailbox_test_state();
        let (delegation, child) = install_required_review_delegation(&state, &parent);
        state
            .submit_delegation_review_result(&child, structured_review_request())
            .unwrap();
        finish_delegation_child_with_assistant_text(&state, &child, "Finished review.");
        state.refresh_delegation_for_child_session(&child).unwrap();
        let before = {
            let inner = state.inner.lock().unwrap();
            inner.delegations[inner.find_delegation_index(&delegation).unwrap()].clone()
        };
        assert_eq!(
            before.review_result_schema_version,
            Some(DELEGATION_REVIEW_RESULT_SCHEMA_VERSION)
        );
        assert_eq!(
            before.result.as_ref().unwrap().summary,
            "One medium issue found."
        );
        if rejection == "boot" {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&child).unwrap();
            inner.sessions[index].engram_boot_recovery_pending = true;
        }
        if rejection == "reserved" {
            let mut inner = state.inner.lock().unwrap();
            let watermark = delegation_last_user_prompt_id_locked(&inner, &child);
            inner.delegation_followup_admissions.insert(
                delegation.clone(),
                FollowupAdmissionReservation::new(watermark),
            );
        }
        let result = state.followup_delegation_request(
            &parent,
            &delegation,
            SendMessageRequest {
                text: if rejection == "empty" {
                    " "
                } else {
                    "Continue"
                }
                .to_owned(),
                expanded_text: None,
                source_session_id: None,
                source_mailbox: None,
                attachments: if rejection == "attachment" {
                    vec![SendMessageAttachmentRequest {
                        media_type: "application/pdf".to_owned(),
                        data: "eA==".to_owned(),
                        file_name: None,
                    }]
                } else {
                    vec![]
                },
            },
        );
        assert!(result.is_err(), "fixture must reject {rejection}");
        if rejection == "reserved" {
            let error = result.err().expect("reserved follow-up must be rejected");
            assert_eq!(error.status, StatusCode::CONFLICT);
            assert_eq!(
                error.message,
                "delegation follow-up admission is already in progress"
            );
        }
        let after = {
            let inner = state.inner.lock().unwrap();
            inner.delegations[inner.find_delegation_index(&delegation).unwrap()].clone()
        };
        assert_eq!(
            after, before,
            "rejected {rejection} must preserve the result, schema and submission attempt"
        );
    }
}

#[test]
fn followup_runtime_start_failure_never_clears_the_previous_review_or_card() {
    let (state, _, parent) = mailbox_test_state();
    let (delegation, child) = install_required_review_delegation(&state, &parent);
    state
        .submit_delegation_review_result(&child, structured_review_request())
        .unwrap();
    finish_delegation_child_with_assistant_text(&state, &child, "Finished review.");
    state.refresh_delegation_for_child_session(&child).unwrap();
    let (previous, parent_before, read_only_before, generation_before) = {
        let mut inner = state.inner.lock().unwrap();
        let previous = inner.delegations[inner.find_delegation_index(&delegation).unwrap()].clone();
        add_parent_delegation_card_locked(&mut inner, &previous);
        update_parent_delegation_card_locked(
            &mut inner,
            &previous,
            ParallelAgentStatus::Completed,
            "Exact prior card detail, independent of the result summary".to_owned(),
        );
        state.commit_locked(&mut inner).unwrap();
        let parent_before = serde_json::to_value(
            &inner.sessions[inner.find_session_index(&parent).unwrap()]
                .session
                .messages,
        )
        .unwrap();
        let generation =
            inner.sessions[inner.find_session_index(&child).unwrap()].active_turn_generation;
        let watermark = delegation_last_user_prompt_id_locked(&inner, &child);
        inner.delegation_followup_admissions.insert(
            delegation.clone(),
            FollowupAdmissionReservation::new(watermark),
        );
        (
            previous,
            parent_before,
            inner.running_read_only_delegations.clone(),
            generation,
        )
    };
    // No shared fake runtime: the lightweight AppState rejects runtime creation
    // inside start_turn_on_record, after validation but before the user message.
    assert!(!state.agent_runtime_spawning_enabled);
    *state.shared_codex_runtime.lock().unwrap() = None;
    let mut events = state.subscribe_delta_events();
    let error = state
        .dispatch_turn_with_followup(
            &child,
            SendMessageRequest {
                text: "Continue".to_owned(),
                expanded_text: None,
                source_session_id: None,
                source_mailbox: None,
                attachments: vec![],
            },
            Some(&previous),
        )
        .err()
        .expect("runtime start must fail");
    assert!(
        error
            .message
            .contains("failed to start persistent Codex session"),
        "{}",
        error.message
    );
    assert!(error.message.contains("agent runtime spawning is disabled"));
    {
        let mut inner = state.inner.lock().unwrap();
        assert!(
            inner.sessions[inner.find_session_index(&child).unwrap()].active_turn_generation
                > generation_before,
            "fixture must reach runtime start, not an earlier admission gate"
        );
        assert_eq!(
            inner.delegations[inner.find_delegation_index(&delegation).unwrap()],
            previous
        );
        assert_eq!(
            serde_json::to_value(
                &inner.sessions[inner.find_session_index(&parent).unwrap()]
                    .session
                    .messages
            )
            .unwrap(),
            parent_before
        );
        assert_eq!(inner.running_read_only_delegations, read_only_before);
        assert!(inner
            .delegation_followup_admissions
            .contains_key(&delegation));
        // Observe the exact gap before the caller can perform compensation or
        // release its reservation. Another commit cannot persist a cleared result.
        state.commit_locked(&mut inner).unwrap();
        inner.delegation_followup_admissions.remove(&delegation);
    }
    while let Ok(event) = events.try_recv() {
        let delta: serde_json::Value = serde_json::from_str(&event).unwrap();
        if delta["type"] == "delegationUpdated" && delta["delegationId"] == delegation {
            assert_ne!(delta["status"], "running");
        }
    }
    let disk = load_state(&state.persistence_path).unwrap().unwrap();
    assert_eq!(
        disk.delegations
            .iter()
            .find(|row| row.id == delegation)
            .unwrap(),
        &previous
    );
}

#[test]
fn followup_admission_publishes_its_card_revision_before_a_later_parent_mutation() {
    let (state, _, parent) = mailbox_test_state();
    let (delegation, child) = install_required_review_delegation(&state, &parent);
    state
        .submit_delegation_review_result(&child, structured_review_request())
        .unwrap();
    finish_delegation_child_with_assistant_text(&state, &child, "Finished review.");
    state.refresh_delegation_for_child_session(&child).unwrap();
    let (runtime, _input_rx, _) = test_shared_codex_runtime("followup-revision");
    *state.shared_codex_runtime.lock().unwrap() = Some(runtime);
    let previous = {
        let mut inner = state.inner.lock().unwrap();
        let previous = inner.delegations[inner.find_delegation_index(&delegation).unwrap()].clone();
        add_parent_delegation_card_locked(&mut inner, &previous);
        let watermark = delegation_last_user_prompt_id_locked(&inner, &child);
        inner.delegation_followup_admissions.insert(
            delegation.clone(),
            FollowupAdmissionReservation::new(watermark),
        );
        previous
    };
    let mut events = state.subscribe_delta_events();
    let dispatch = state
        .dispatch_turn_with_followup(
            &child,
            SendMessageRequest {
                text: "Continue".to_owned(),
                expanded_text: None,
                source_session_id: None,
                source_mailbox: None,
                attachments: vec![],
            },
            Some(&previous),
        )
        .unwrap();
    assert!(matches!(dispatch, DispatchTurnResult::Dispatched(_)));
    let (admission_revision, newer_revision) = {
        let mut inner = state.inner.lock().unwrap();
        let admission_revision = inner.revision;
        update_parent_delegation_card_locked(
            &mut inner,
            &previous,
            ParallelAgentStatus::Running,
            "Newer parent card detail".to_owned(),
        );
        let newer_revision = state.commit_locked(&mut inner).unwrap();
        let captured = inner.delegation_followup_admissions[&delegation]
            .admitted
            .as_ref()
            .unwrap();
        assert_eq!(captured.0, admission_revision);
        assert_eq!(captured.1.status, DelegationStatus::Running);
        // Even deletion after admission cannot turn an accepted request into a
        // response-time 404: the reservation owns its committed response.
        inner.delegations.retain(|row| row.id != delegation);
        (admission_revision, newer_revision)
    };
    assert!(newer_revision > admission_revision);
    let mut admission = DelegationFollowupAdmission {
        state: state.clone(),
        previous,
        restore_candidate: false,
        released: false,
    };
    let response = admission.admitted_response().unwrap();
    assert_eq!(response.revision, admission_revision);
    assert_eq!(response.delegation.id, delegation);
    admission.release().unwrap();
    let mut saw_admission_card = false;
    while let Ok(event) = events.try_recv() {
        let delta: serde_json::Value = serde_json::from_str(&event).unwrap();
        if delta["type"] == "parallelAgentsUpdate" {
            saw_admission_card = true;
            assert_eq!(delta["revision"], admission_revision);
        }
    }
    assert!(
        saw_admission_card,
        "dispatch itself must publish the admitted card, not its later caller"
    );
}

#[test]
fn failed_followup_restore_preserves_structured_review_and_polling_cannot_finish_it() {
    for failure in ["transport", "timeout", "rejection", "success"] {
        let (state, _, parent) = mailbox_test_state();
        let (delegation, child) = install_required_review_delegation(&state, &parent);
        state
            .submit_delegation_review_result(&child, structured_review_request())
            .unwrap();
        finish_delegation_child_with_assistant_text(&state, &child, "Finished review.");
        state.refresh_delegation_for_child_session(&child).unwrap();
        let (runtime, input_rx, _) = test_shared_codex_runtime("failed-followup-restore");
        *state.shared_codex_runtime.lock().unwrap() = Some(runtime);
        let before = {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&child).unwrap();
            inner.sessions[index].external_session_id = Some("review-thread".to_owned());
            set_record_codex_thread_state(&mut inner.sessions[index], CodexThreadState::Archived);
            let release = Arc::new(CodexDelegationRelease {
                terminal_release: true,
                ..Default::default()
            });
            release.finish(CodexReleaseOutcome::Archived);
            inner.sessions[index].codex_delegation_release = Some(release);
            inner.delegations[inner.find_delegation_index(&delegation).unwrap()].clone()
        };
        let following = state.clone();
        let following_parent = parent.clone();
        let following_delegation = delegation.clone();
        let followup = std::thread::spawn(move || {
            following.followup_delegation(
                &following_parent,
                &following_delegation,
                "Continue".to_owned(),
            )
        });
        let CodexRuntimeCommand::JsonRpcRequest {
            method,
            response_tx,
            ..
        } = phase_sync::receive(&input_rx, "parked follow-up restore")
        else {
            panic!("expected RPC")
        };
        assert_eq!(method, "thread/unarchive");
        // Observe both public read paths while restoration is still blocked.
        let observed = state
            .get_delegation(&parent, &delegation)
            .unwrap()
            .delegation;
        let observed_result = state.get_delegation_result(&parent, &delegation);
        let duplicate = state
            .followup_delegation(&parent, &delegation, "Duplicate".to_owned())
            .err()
            .expect("parked restoration must reject duplicate");
        assert_eq!(duplicate.status, StatusCode::CONFLICT);
        assert_eq!(
            duplicate.message,
            "Codex child recovery is already in progress; retry the prompt"
        );
        response_tx
            .send(if failure == "success" {
                Ok(json!({}))
            } else {
                Err(match failure {
                    "transport" => {
                        CodexResponseError::Transport("restore transport failed".to_owned())
                    }
                    "timeout" => CodexResponseError::Timeout("restore timed out".to_owned()),
                    _ => CodexResponseError::JsonRpc("restore rejected".to_owned()),
                })
            })
            .unwrap();
        let result = followup.join().unwrap();
        let after = state
            .get_delegation(&parent, &delegation)
            .unwrap()
            .delegation;
        if failure == "success" {
            result.expect("restored follow-up must be admitted after polling");
            assert!(matches!(
                phase_sync::receive(&input_rx, "follow-up prompt"),
                CodexRuntimeCommand::Prompt { .. }
            ));
            assert_eq!(after.status, DelegationStatus::Running);
            assert_eq!(
                after.review_result_submission_attempt,
                before.review_result_submission_attempt + 1
            );
            assert!(after.result.is_none());
        } else {
            assert!(result.is_err());
            assert_eq!(
                after, before,
                "failed {failure} restore must preserve the exact prior review"
            );
        }
        assert_eq!(
            observed, before,
            "polling during restore must retain the prior terminal attempt"
        );
        assert_eq!(observed_result.unwrap().result, before.result.unwrap());
    }
}

#[test]
fn followup_engram_queue_start_failure_settles_instead_of_stranding_running() {
    let (state, runtime_rx) =
        test_app_state_with_delegation_codex_runtime("followup-engram-start-failure");
    let root = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("followup-engram-start-failure");
    fs::create_dir_all(&root).unwrap();
    let project = create_test_project(&state, &root, "Follow-up start failure");
    let parent = create_test_project_session(&state, Agent::Codex, &project, &root);
    let created = state
        .create_read_only_delegation(
            &parent,
            CreateDelegationRequest {
                prompt: "Initial attempt".to_owned(),
                title: None,
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Explorer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .unwrap();
    assert!(matches!(
        runtime_rx.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let child = created.delegation.child_session_id;
    super::delegation_support::finish_delegation_child_with_assistant_text(
        &state,
        &child,
        "## Result\nStatus: completed\n\nSummary:\nOld result.",
    );
    state.refresh_delegation_for_child_session(&child).unwrap();
    enable_test_project_engram(&state, &project, &root);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        // A disabled control adapter still uses the dispatch-card queue path,
        // recording the fallback without requiring an external Engram process.
        inner.sessions[index].engram.disabled_reason = Some("fixture unavailable".to_owned());
        assert!(AppState::engram_child_requires_dispatch_card_locked(
            &inner, &child
        ));
    }
    assert!(!state.agent_runtime_spawning_enabled);
    *state.shared_codex_runtime.lock().unwrap() = None;
    let error = state
        .followup_delegation(&parent, &created.delegation.id, "Cannot start".to_owned())
        .err()
        .expect("runtime spawn must fail after queue admission");
    assert!(
        error.message.contains("agent runtime spawning is disabled"),
        "{}",
        error.message
    );
    let failed = state
        .get_delegation(&parent, &created.delegation.id)
        .unwrap()
        .delegation;
    assert_eq!(failed.status, DelegationStatus::Failed);
    assert!(failed
        .result
        .unwrap()
        .summary
        .contains("agent runtime spawning is disabled"));
    let inner = state.inner.lock().unwrap();
    assert!(inner.sessions[inner.find_session_index(&child).unwrap()]
        .queued_prompts
        .is_empty());
    assert!(!inner
        .delegation_followup_admissions
        .contains_key(&created.delegation.id));
}

#[test]
fn followup_wait_commit_failure_retains_idle_parent_wake_for_exactly_once_retry() {
    for terminal in [
        DelegationStatus::Completed,
        DelegationStatus::Failed,
        DelegationStatus::Canceled,
    ] {
        let (state, parent, _) = mailbox_test_state();
        let (delegation, child) = install_required_review_delegation(&state, &parent);
        let wait = state
            .create_delegation_wait(
                &parent,
                CreateDelegationWaitRequest {
                    delegation_ids: vec![delegation.clone()],
                    mode: DelegationWaitMode::All,
                    title: None,
                },
            )
            .unwrap();
        state
            .submit_delegation_review_result(&child, structured_review_request())
            .unwrap();
        finish_delegation_child_with_assistant_text(&state, &child, "Old review.");
        {
            let mut inner = state.inner.lock().unwrap();
            let watermark = delegation_last_user_prompt_id_locked(&inner, &child);
            inner.delegation_followup_admissions.insert(
                delegation.clone(),
                FollowupAdmissionReservation::new(watermark),
            );
        }
        state.refresh_delegation_for_child_session(&child).unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_delegation_index(&delegation).unwrap();
            match terminal {
                DelegationStatus::Failed => {
                    mark_delegation_failed_locked(&mut inner, index, "Delayed start failed.");
                }
                DelegationStatus::Canceled => {
                    mark_delegation_canceled_locked(
                        &mut inner,
                        index,
                        Some("Canceled before start.".to_owned()),
                    );
                }
                _ => {}
            }
            state.commit_locked(&mut inner).unwrap();
        }
        let failed_path = test_temp_dir().join(format!("followup-wait-commit-{}", Uuid::new_v4()));
        fs::create_dir_all(&failed_path).unwrap();
        let mut failing = state.clone();
        failing.persistence_path = Arc::new(failed_path.clone());
        assert!(failing
            .release_delegation_followup_reservation(&delegation)
            .is_err());
        {
            let inner = state.inner.lock().unwrap();
            assert!(inner
                .delegation_waits
                .iter()
                .any(|item| item.id == wait.wait.id));
            assert!(inner.sessions[inner.find_session_index(&parent).unwrap()]
                .queued_prompts
                .is_empty());
        }
        let (runtime, input_rx, _) = test_shared_codex_runtime("retry-wait-parent");
        *state.shared_codex_runtime.lock().unwrap() = Some(runtime);
        state.get_delegation(&parent, &delegation).unwrap();
        assert!(matches!(
            phase_sync::receive(&input_rx, "retried parent wake"),
            CodexRuntimeCommand::Prompt { .. }
        ));
        state.get_delegation(&parent, &delegation).unwrap();
        assert!(input_rx.try_recv().is_err());
        let inner = state.inner.lock().unwrap();
        assert!(!inner
            .delegation_waits
            .iter()
            .any(|item| item.id == wait.wait.id));
        drop(inner);
        remove_test_directory(failed_path);
    }
}

#[test]
fn restored_queued_followup_failure_keeps_compensation_without_runtime_on_all_dispatch_paths() {
    for direct in [false, true] {
        let (state, _, parent) = mailbox_test_state();
        let (delegation, child) = install_required_review_delegation(&state, &parent);
        state
            .submit_delegation_review_result(&child, structured_review_request())
            .unwrap();
        finish_delegation_child_with_assistant_text(&state, &child, "Previous review.");
        state.refresh_delegation_for_child_session(&child).unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&child).unwrap();
            inner.sessions[index].engram.project_reset_in_progress = true;
            inner.sessions[index].external_session_id = Some("restored-delayed-thread".to_owned());
            set_record_codex_thread_state(&mut inner.sessions[index], CodexThreadState::Archived);
            let release = Arc::new(CodexDelegationRelease {
                terminal_release: true,
                ..Default::default()
            });
            release.finish(CodexReleaseOutcome::Archived);
            inner.sessions[index].codex_delegation_release = Some(release);
        }
        let (runtime, input_rx, _) = test_shared_codex_runtime("restore-before-queue");
        *state.shared_codex_runtime.lock().unwrap() = Some(runtime);
        let worker = std::thread::spawn(move || {
            let CodexRuntimeCommand::JsonRpcRequest {
                method,
                response_tx,
                ..
            } = phase_sync::receive(&input_rx, "unarchive before queue")
            else {
                panic!("expected RPC");
            };
            assert_eq!(method, "thread/unarchive");
            response_tx.send(Ok(json!({}))).unwrap();
            input_rx
        });
        state
            .followup_delegation(&parent, &delegation, "Queued follow-up".to_owned())
            .unwrap();
        let _old_receiver = worker.join().unwrap();
        *state.shared_codex_runtime.lock().unwrap() = None;
        {
            let mut inner = state.inner.lock().unwrap();
            assert!(!inner
                .delegation_followup_admissions
                .contains_key(&delegation));
            let index = inner.find_session_index(&child).unwrap();
            inner.sessions[index].engram.project_reset_in_progress = false;
        }
        let error = if direct {
            state
                .dispatch_turn(
                    &child,
                    SendMessageRequest {
                        text: "Additional user prompt".to_owned(),
                        expanded_text: None,
                        attachments: vec![],
                        source_session_id: None,
                        source_mailbox: None,
                    },
                )
                .err()
                .expect("direct queue start fails")
                .message
        } else {
            format!(
                "{:#}",
                state
                    .start_next_queued_turn_off_lock(&child, true, false)
                    .err()
                    .expect("delayed start fails")
            )
        };
        assert!(error.contains("spawn"), "{error}");
        {
            let inner = state.inner.lock().unwrap();
            assert_eq!(
                inner.delegations[inner.find_delegation_index(&delegation).unwrap()].status,
                DelegationStatus::Failed
            );
            let record = &inner.sessions[inner.find_session_index(&child).unwrap()];
            let release = record.codex_delegation_release.as_ref().unwrap();
            assert!(release
                .compensation_pending
                .load(std::sync::atomic::Ordering::Acquire));
            assert!(release.undurable_terminal.lock().unwrap().is_some());
            assert!(record.queued_prompts.is_empty());
        }
        // Leave the lazy slot empty across retries. This fixture forbids
        // process creation, so reaching its spawning guard (500) proves that
        // retry attempts bootstrap rather than returning the old permanent
        // "runtime unavailable" 409 before ever invoking the lazy accessor.
        for _ in 0..2 {
            let error = state
                .followup_delegation(&parent, &delegation, "Retry with empty slot".to_owned())
                .err()
                .expect("fixture forbids spawning the missing runtime");
            assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
            assert!(
                error
                    .message
                    .contains("failed to start Codex runtime for archive compensation"),
                "{}",
                error.message
            );
            assert!(
                error.message.contains("agent runtime spawning is disabled"),
                "{}",
                error.message
            );
            assert!(state.shared_codex_runtime.lock().unwrap().is_none());
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&child).unwrap()];
            assert!(record
                .codex_delegation_release
                .as_ref()
                .unwrap()
                .needs_followup_compensation());
            assert!(!inner
                .delegation_followup_admissions
                .contains_key(&delegation));
        }
        let (runtime, input_rx, _) = test_shared_codex_runtime("compensation-retry");
        *state.shared_codex_runtime.lock().unwrap() = Some(runtime);
        let worker = std::thread::spawn(move || {
            let CodexRuntimeCommand::JsonRpcRequest {
                method,
                response_tx,
                ..
            } = phase_sync::receive(&input_rx, "archive retry")
            else {
                panic!("expected RPC");
            };
            assert_eq!(method, "thread/archive");
            response_tx.send(Ok(json!({}))).unwrap();
        });
        state.prepare_codex_child_rearm(&child).unwrap();
        worker.join().unwrap();
        let inner = state.inner.lock().unwrap();
        assert!(record_has_archived_codex_thread(
            &inner.sessions[inner.find_session_index(&child).unwrap()]
        ));
    }
}

#[test]
fn queued_followup_marker_survives_sqlite_load_without_reservation() {
    let (state, _, parent) = mailbox_test_state();
    let (delegation, child) = install_required_review_delegation(&state, &parent);
    state
        .submit_delegation_review_result(&child, structured_review_request())
        .unwrap();
    finish_delegation_child_with_assistant_text(&state, &child, "Previous review.");
    state.refresh_delegation_for_child_session(&child).unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        inner.sessions[index].engram.project_reset_in_progress = true;
    }
    let admitted = state
        .followup_delegation(&parent, &delegation, "Survive restart".to_owned())
        .unwrap();
    let mut loaded = load_state(&state.persistence_path).unwrap().unwrap();
    assert!(loaded.delegation_followup_admissions.is_empty());
    let index = loaded.find_delegation_index(&delegation).unwrap();
    assert_eq!(
        loaded.delegations[index].queued_followup_prompt_id,
        admitted.delegation.queued_followup_prompt_id
    );
    assert!(delegation_followup_awaits_first_turn(
        &loaded,
        &loaded.delegations[index]
    ));
    assert!(refresh_delegation_from_child_locked(&mut loaded, index).is_none());
    assert_eq!(loaded.delegations[index].status, DelegationStatus::Running);
}

#[test]
fn followup_archive_compensation_rejects_attached_runtime_or_remaining_queue() {
    for attached in [false, true] {
        let (state, _, parent) = mailbox_test_state();
        let (delegation, child) = install_required_review_delegation(&state, &parent);
        state
            .submit_delegation_review_result(&child, structured_review_request())
            .unwrap();
        finish_delegation_child_with_assistant_text(&state, &child, "Previous result.");
        state.refresh_delegation_for_child_session(&child).unwrap();
        let (runtime, input_rx, _) = test_shared_codex_runtime("must-not-archive-busy");
        *state.shared_codex_runtime.lock().unwrap() = Some(runtime);
        let (attached_runtime, _attached_rx) = test_codex_runtime_handle("attached-followup");
        let release = Arc::new(CodexDelegationRelease {
            terminal_release: true,
            ..Default::default()
        });
        release.finish(CodexReleaseOutcome::Restored);
        {
            let mut inner = state.inner.lock().unwrap();
            let id = inner.next_message_id();
            let index = inner.find_session_index(&child).unwrap();
            let record = &mut inner.sessions[index];
            record.external_session_id = Some("busy-thread".to_owned());
            record.codex_delegation_release = Some(release.clone());
            if attached {
                record.runtime = SessionRuntime::Codex(attached_runtime);
            } else {
                queue_prompt_on_record_with_source(
                    record,
                    PendingPrompt {
                        id,
                        timestamp: stamp_now(),
                        text: "Retain this prompt".to_owned(),
                        expanded_text: None,
                        attachments: vec![],
                        source: None,
                    },
                    vec![],
                    QueuedPromptSource::User,
                );
            }
        }
        let error = state
            .rearchive_undispatched_codex_child(&child)
            .err()
            .expect("busy child is rejected");
        assert!(error.message.contains("no longer detached and terminal"));
        assert!(input_rx.try_recv().is_err());
        let inner = state.inner.lock().unwrap();
        assert_eq!(
            inner.delegations[inner.find_delegation_index(&delegation).unwrap()].status,
            DelegationStatus::Completed
        );
        let record = &inner.sessions[inner.find_session_index(&child).unwrap()];
        assert!(Arc::ptr_eq(
            record.codex_delegation_release.as_ref().unwrap(),
            &release
        ));
        assert_eq!(record.queued_prompts.len(), usize::from(!attached));
    }
}
