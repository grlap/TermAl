//! Failure, restart, race and diagnostic controls for mailbox coverage.
//! Uses disposable fixture stores and deterministic off-lock interleavings.

use super::*;

#[tokio::test]
async fn mailbox_continuation_coverage_audit_is_durable_but_not_restart_proof() {
    let _temp = TestTempRootDirectoryScope::repository_local();
    let (state, sender, target, _receiver) = active_fixture();
    let message = send(&state, &sender, &target, "audit");
    let page = read_page(&state, &target, &message.mailbox_id, None).await;
    ack(&state, &target, &message.mailbox_id, &page["receipt"], true).await;
    ack(&state, &target, &message.mailbox_id, &page["receipt"], true).await;
    ack(
        &state,
        &target,
        &message.mailbox_id,
        &page["receipt"],
        false,
    )
    .await;
    {
        let connection = state.mailbox_store.connection().unwrap();
        let row: (String, String, u64, u64, String) = connection.query_row(
            "SELECT session_id, receipt, through_sequence, turn_generation, declared_at FROM mailbox_continuation_declarations",
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        ).unwrap();
        assert_eq!(row.0, target);
        assert_eq!(row.1, page["receipt"].as_str().unwrap());
        assert_eq!(row.2, message.sequence);
        assert_eq!(row.3, 1);
        assert!(!row.4.is_empty());
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM mailbox_continuation_declarations",
                    [],
                    |r| r.get::<_, u64>(0)
                )
                .unwrap(),
            1
        );
    }
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&target).unwrap();
    assert!(
        inner.sessions[index].mailbox_continuations.proofs[&message.mailbox_id].explicit,
        "a plain repeat does not revoke the existing declaration"
    );
    let restored = PersistedSessionRecord::from_record(&inner.sessions[index])
        .into_record()
        .unwrap();
    assert!(restored.mailbox_continuations.accepted.is_empty());
    assert!(restored.mailbox_continuations.reads.is_empty());
    assert!(restored.mailbox_continuations.proofs.is_empty());
    assert_eq!(
        restored.queued_prompts.len(),
        1,
        "boot retains the obligation despite the audit"
    );
    inner.sessions[index] = restored;
}

#[tokio::test]
async fn mailbox_continuation_coverage_partial_outside_and_numeric_reads_do_not_cover() {
    let _temp = TestTempRootDirectoryScope::repository_local();
    for kind in [
        "read-outside",
        "ack-outside",
        "explicit-outside",
        "numeric",
        "partial",
    ] {
        let (state, sender, target, receiver) = active_fixture();
        queue_user(&state, &target, "later uncertainty");
        let first = send(&state, &sender, &target, "uncertainty-first");
        let second = if kind == "partial" {
            Some(send(&state, &sender, &target, "uncertainty-second"))
        } else {
            None
        };
        finish_turn(&state, &target);
        assert_eq!(
            expect_prompt(&receiver, "later uncertainty turn"),
            "later uncertainty"
        );
        let set_status = |status| {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&target).unwrap();
            inner.sessions[index].session.status = status;
        };
        if kind == "read-outside" {
            set_status(SessionStatus::Idle);
        }
        let page = if kind == "partial" {
            post(
                &state,
                format!("/api/sessions/{target}/mailboxes/{}/read", first.mailbox_id),
                json!({"issueReceipt": true, "limit": 1}),
            )
            .await
        } else {
            read_page(&state, &target, &first.mailbox_id, None).await
        };
        set_status(SessionStatus::Active);
        if kind == "ack-outside" || kind == "explicit-outside" {
            set_status(SessionStatus::Idle);
        }
        if kind == "numeric" {
            post(
                &state,
                format!(
                    "/api/sessions/{target}/mailboxes/{}/acknowledge",
                    first.mailbox_id
                ),
                json!({"expectedProcessedThrough": 0, "processedThrough": first.sequence}),
            )
            .await;
        } else {
            post(
                &state,
                format!(
                    "/api/sessions/{target}/mailboxes/{}/acknowledge",
                    first.mailbox_id
                ),
                json!({"receipt": page["receipt"], "noContinuation": kind == "explicit-outside"}),
            )
            .await;
        }
        if kind == "explicit-outside" {
            assert_eq!(state.mailbox_store.connection().unwrap().query_row(
                "SELECT turn_generation FROM mailbox_continuation_declarations WHERE receipt=?1",
                [page["receipt"].as_str().unwrap()], |r| r.get::<_, u64>(0)).unwrap(), 0);
        }
        set_status(SessionStatus::Active);
        finish_turn(&state, &target);
        let prompt = expect_prompt(&receiver, "uncertain coverage preserves the wake");
        assert!(prompt.contains(&first.mailbox_id));
        if let Some(second) = second {
            assert!(prompt.contains(&format!("#{}", second.sequence)));
        }
    }
}

#[tokio::test]
async fn mailbox_continuation_coverage_durability_and_stopped_barriers_keep_proven_entries() {
    let _temp = TestTempRootDirectoryScope::repository_local();
    for kind in [
        "durability",
        "stopped",
        "paused",
        "invisible",
        "other-target",
        "review-result",
    ] {
        let (state, sender, target, receiver) = active_fixture();
        let message = send(&state, &sender, &target, "barrier");
        let page = read_page(&state, &target, &message.mailbox_id, None).await;
        ack(&state, &target, &message.mailbox_id, &page["receipt"], true).await;
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&target).unwrap();
            let record = &mut inner.sessions[index];
            match kind {
                "durability" => {
                    let queued = record.queued_prompts.front().unwrap();
                    record.queued_prompt_durability_holds.insert(
                        queued.pending_prompt.id.clone(),
                        QueuedPromptDurabilityHold {
                            nonce: "held-proof".into(),
                            content: queued_prompt_durability_content(queued),
                            failure: None,
                            acknowledged: false,
                        },
                    );
                }
                "stopped" => {
                    record.session.status = SessionStatus::Idle;
                    record.set_auto_dispatch_blocked(true);
                }
                "paused" => record.set_auto_dispatch_blocked(true),
                _ => {}
            }
        }
        if kind == "invisible" {
            state.mailbox_store.mark_session_left(&target).unwrap();
        }
        if kind == "other-target" {
            state
                .mailbox_store
                .connection()
                .unwrap()
                .execute(
                    "UPDATE mailbox_messages SET target_session_id=?1 WHERE id=?2",
                    rusqlite::params![sender, message.message_id],
                )
                .unwrap();
        }
        if kind == "review-result" {
            state
                .mailbox_store
                .connection()
                .unwrap()
                .execute(
                    "UPDATE mailbox_messages SET topic=?1 WHERE id=?2",
                    rusqlite::params![DELEGATION_REVIEW_RESULT_TOPIC, message.message_id],
                )
                .unwrap();
        }
        state
            .settle_covered_mailbox_continuations(&target, None)
            .unwrap();
        assert_eq!(
            queued_mailbox_wakes(&state, &target).len(),
            1,
            "{kind} cannot establish new coverage"
        );
        let inner = state.inner.lock().unwrap();
        assert_eq!(
            inner.sessions[inner.find_session_index(&target).unwrap()]
                .mailbox_continuations
                .removed,
            0
        );
        expect_no_turn(&receiver, "no barrier path starts a provider");
    }
}

#[tokio::test]
async fn mailbox_continuation_coverage_counter_is_once_and_unrelated_work_is_untouched() {
    let _temp = TestTempRootDirectoryScope::repository_local();
    let (state, sender, target, receiver) = active_fixture();
    queue_user(&state, &target, "unrelated user work");
    let message = send(&state, &sender, &target, "behind-other-work");
    let page = read_page(&state, &target, &message.mailbox_id, None).await;
    ack(&state, &target, &message.mailbox_id, &page["receipt"], true).await;
    for _ in 0..2 {
        state
            .settle_covered_mailbox_continuations(&target, None)
            .unwrap();
    }
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&target).unwrap()];
        assert_eq!(record.mailbox_continuations.removed, 1);
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(
            record.queued_prompts[0].pending_prompt.text,
            "unrelated user work"
        );
    }
    finish_turn(&state, &target);
    assert_eq!(
        expect_prompt(&receiver, "unrelated user work survives"),
        "unrelated user work"
    );
    finish_turn(&state, &target);
    expect_no_turn(&receiver, "coverage never touches a second obligation");
}

#[tokio::test]
async fn mailbox_continuation_coverage_audit_failure_rolls_back_the_ack_and_proof() {
    let _temp = TestTempRootDirectoryScope::repository_local();
    let (state, sender, target, receiver) = active_fixture();
    let message = send(&state, &sender, &target, "audit-write-failure");
    let page = read_page(&state, &target, &message.mailbox_id, None).await;
    state.mailbox_store.connection().unwrap().execute_batch(
        "CREATE TEMP TRIGGER refuse_declaration BEFORE INSERT ON mailbox_continuation_declarations
         BEGIN SELECT RAISE(ABORT, 'fixture audit write refused'); END;",
    ).unwrap();
    assert_eq!(
        post_status(
            &state,
            &target,
            &message.mailbox_id,
            json!({"receipt": page["receipt"], "noContinuation": true})
        )
        .await,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    {
        let connection = state.mailbox_store.connection().unwrap();
        assert_eq!(connection.query_row("SELECT processed_through FROM mailbox_participants WHERE session_id = ?1 AND mailbox_id = ?2",
            rusqlite::params![target, message.mailbox_id], |r| r.get::<_, u64>(0)).unwrap(), 0);
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM mailbox_continuation_declarations",
                    [],
                    |r| r.get::<_, u64>(0)
                )
                .unwrap(),
            0
        );
        connection
            .execute_batch("DROP TRIGGER refuse_declaration")
            .unwrap();
    }
    {
        let inner = state.inner.lock().unwrap();
        assert!(
            inner.sessions[inner.find_session_index(&target).unwrap()]
                .mailbox_continuations
                .proofs
                .is_empty()
        );
    }
    ack(
        &state,
        &target,
        &message.mailbox_id,
        &page["receipt"],
        false,
    )
    .await;
    finish_turn(&state, &target);
    expect_prompt(&receiver, "a failed audit cannot suppress the continuation");
}

#[tokio::test]
async fn mailbox_continuation_coverage_off_lock_races_keep_the_obligation() {
    let _temp = TestTempRootDirectoryScope::repository_local();
    for kind in [
        "append", "turn", "runtime", "head", "expanded", "retained", "paused", "proof",
    ] {
        let (state, sender, target, receiver) = active_fixture();
        let message = send(&state, &sender, &target, "off-lock-race");
        let page = read_page(&state, &target, &message.mailbox_id, None).await;
        ack(&state, &target, &message.mailbox_id, &page["receipt"], true).await;
        // Removing the runtime from session state must invalidate coverage,
        // while the receiver stays live so Empty still proves no handoff.
        let original_runtime = {
            let inner = state.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&target).unwrap()]
                .runtime
                .clone()
        };
        let mut calls = 0;
        state
            .settle_covered_mailbox_continuations_before_apply(&target, None, || {
                calls += 1;
                assert_eq!(calls, 1, "the stale observation must stop this application");
                if kind == "append" {
                    send(&state, &sender, &target, "arrived-after-zero-unread");
                    return;
                }
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&target).unwrap();
                let record = &mut inner.sessions[index];
                match kind {
                    "turn" => record.active_turn_generation += 1,
                    "runtime" => record.runtime = SessionRuntime::None,
                    "head" => record
                        .queued_prompts
                        .front_mut()
                        .unwrap()
                        .pending_prompt
                        .text
                        .push_str(" changed"),
                    "retained" => record.queued_prompts.front_mut().unwrap().engram_waiting = true,
                    "expanded" => {
                        record
                            .queued_prompts
                            .front_mut()
                            .unwrap()
                            .pending_prompt
                            .expanded_text = Some("new work".into())
                    }
                    "paused" => record.set_auto_dispatch_blocked(true),
                    "proof" => {
                        record.mailbox_continuations.proofs.clear();
                    }
                    _ => unreachable!(),
                }
            })
            .unwrap();
        assert_eq!(calls, 1, "{kind} reached the off-lock boundary");
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&target).unwrap()];
        assert_eq!(
            record.queued_prompts.len(),
            1,
            "{kind} retains the queued obligation"
        );
        assert_eq!(record.mailbox_continuations.removed, 0);
        if kind == "runtime" {
            assert!(record.runtime.runtime_token().is_none());
        }
        expect_no_turn(
            &receiver,
            &format!("stale {kind} observation does not hand off a turn"),
        );
        drop(inner);
        drop(original_runtime);
    }
}

#[tokio::test]
async fn mailbox_continuation_coverage_failed_queue_commit_restores_the_entry_and_counter() {
    let _temp = TestTempRootDirectoryScope::repository_local();
    let (mut state, sender, target, receiver) = active_fixture();
    let message = send(&state, &sender, &target, "queue-write-failure");
    let page = read_page(&state, &target, &message.mailbox_id, None).await;
    ack(&state, &target, &message.mailbox_id, &page["receipt"], true).await;
    let before = {
        let inner = state.inner.lock().unwrap();
        serde_json::to_value(
            &inner.sessions[inner.find_session_index(&target).unwrap()].queued_prompts,
        )
        .unwrap()
    };
    let failing_path = TestTempRoot::create("mailbox-coverage-persistence-directory");
    state.shutdown_persist_blocking();
    let original_path = state.persistence_path.clone();
    state.persistence_path = Arc::new(failing_path.path().to_path_buf());
    assert!(
        state
            .settle_covered_mailbox_continuations(&target, None)
            .is_err()
    );
    state.persistence_path = original_path;
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&target).unwrap()];
    assert_eq!(
        serde_json::to_value(&record.queued_prompts).unwrap(),
        before
    );
    assert_eq!(record.mailbox_continuations.removed, 0);
    assert!(
        record
            .mailbox_continuations
            .proofs
            .contains_key(&message.mailbox_id)
    );
    expect_no_turn(
        &receiver,
        "persistence failure preserves work without a handoff",
    );
}

#[test]
fn mailbox_continuation_coverage_diagnostic_is_bounded_and_names_the_exact_proof() {
    assert_eq!(
        mailbox_coverage_diagnostic("session-a", "mailbox-b", 42, false),
        "mailbox coverage> session=session-a mailbox=mailbox-b boundary=42 proof=later-turn"
    );
    let untrusted = format!("line\n\t\u{202e}{}", "x".repeat(2000));
    let line = mailbox_coverage_diagnostic(&untrusted, &untrusted, u64::MAX, true);
    assert!(line.chars().count() < 420);
    assert!(!line.chars().any(char::is_control));
    assert!(!line.contains('\u{202e}'));
    assert!(line.ends_with("boundary=18446744073709551615 proof=explicit"));
}
