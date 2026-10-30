use super::delegation_support::{
    finish_delegation_child_with_assistant_text, install_delegation_codex_runtime,
    install_required_review_delegation, structured_review_request,
};
use super::mailboxes::mailbox_test_state;
use super::*;

pub(super) fn terminal_review(output: &str) -> (AppState, String, String, String) {
    let (state, _, parent) = mailbox_test_state();
    let (delegation, child) = install_required_review_delegation(&state, &parent);
    state
        .submit_delegation_review_result(&child, structured_review_request())
        .unwrap();
    finish_delegation_child_with_assistant_text(&state, &child, output);
    state.get_delegation_result(&parent, &delegation).unwrap();
    (state, parent, delegation, child)
}

fn selected(attempt: u32, id: Option<&str>) -> DelegationResultSelection {
    DelegationResultSelection {
        submission_attempt: Some(attempt),
        result_id: id.map(str::to_owned),
    }
}

fn archived_review_recut_runtime(
    state: &AppState,
    child: &str,
) -> mpsc::Receiver<CodexRuntimeCommand> {
    let (runtime, input_rx, _) = test_shared_codex_runtime("review-restore-recut");
    install_single_wire_codex_fixture(state, Some(runtime));
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(child).unwrap();
    let record = &mut inner.sessions[index];
    record.external_session_id = Some("review-restore-thread".to_owned());
    set_record_codex_thread_state(record, CodexThreadState::Archived);
    let release = Arc::new(CodexDelegationRelease {
        terminal_release: true,
        ..Default::default()
    });
    release.finish(CodexReleaseOutcome::Archived);
    record.codex_delegation_release = Some(release);
    input_rx
}

fn answer_review_restore(
    input_rx: mpsc::Receiver<CodexRuntimeCommand>,
    next: Option<&'static str>,
) -> std::thread::JoinHandle<mpsc::Receiver<CodexRuntimeCommand>> {
    std::thread::spawn(move || {
        let CodexRuntimeCommand::JsonRpcRequest {
            method,
            response_tx,
            ..
        } = phase_sync::receive(&input_rx, "review recut restore")
        else {
            panic!("expected unarchive");
        };
        assert_eq!(method, "thread/unarchive");
        response_tx.send(Ok(json!({}))).unwrap();
        if let Some(next) = next {
            match phase_sync::receive(&input_rx, "review recut outcome") {
                CodexRuntimeCommand::Prompt { .. } => assert_eq!(next, "prompt"),
                CodexRuntimeCommand::JsonRpcRequest {
                    method,
                    response_tx,
                    ..
                } => {
                    assert_eq!(next, "archive");
                    assert_eq!(method, "thread/archive");
                    response_tx.send(Ok(json!({}))).unwrap();
                }
                _ => panic!("unexpected review recut command"),
            }
        }
        input_rx
    })
}

fn reserved_review_slot_body(state: &AppState, child: &str, token: &str) -> String {
    state
        .mailbox_store
        .read_idempotent_message(child, token)
        .unwrap()
        .unwrap()
        .body
}

fn reject_review_recut_back_to_original(
    state: &AppState,
    parent: &str,
    delegation: &str,
    child: &str,
) {
    finish_delegation_child_with_assistant_text(state, child, "Rejected recut prose.");
    let changed = state.clone();
    let child = child.to_owned();
    TEST_REVIEW_ATTEMPT_AFTER_APPEND.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            finish_delegation_child_with_assistant_text(
                &changed,
                &child,
                "Confirmed original prose.",
            );
            false
        }));
    });
    let error = state.retain_review_attempt(parent, delegation, 1).err().unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("terminal cut changed"));
}

fn reopen_review_test_state(state: &AppState) -> AppState {
    let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    AppState {
        inner: Arc::new(StateMutex::new(loaded)),
        mailbox_store: Arc::new(
            MailboxStore::open(&resolve_coordination_persistence_path(
                state.persistence_path.as_ref(),
            ))
            .unwrap(),
        ),
        ..state.clone()
    }
}

#[test]
fn obsolete_pending_recut_is_neutralized_without_losing_the_next_capture_or_confirmed_prose() {
    for restart in [false, true] {
        let (state, parent, delegation, child) = terminal_review("Confirmed original prose.");
        let (original, original_id) = state.retain_review_attempt(&parent, &delegation, 1).unwrap();
        let token = state.reserve_next_review_capture(&original).unwrap();
        let reserved = reserved_review_slot_body(&state, &child, &token);
        reject_review_recut_back_to_original(&state, &parent, &delegation, &child);
        let mut rows = state.review_custody_rows(&child, &delegation).unwrap();
        assert_eq!(rows.len(), 3);
        let saved = rows.pop().unwrap();
        let key = state.mailbox_store.review_attempt_auth_key(false).unwrap().unwrap();
        let pending: ReviewCustodySlot = decode_review_value(&saved.body, &key).unwrap();
        assert!(pending.candidate.is_some());
        assert!(pending.publication.is_none());
        let pending_id = review_candidate_id(pending.candidate.as_ref().unwrap()).unwrap();
        let mut stale_confirmed = pending.clone();
        stale_confirmed.publication = Some(ReviewPublicationProof {
            result_id: pending_id.clone(),
            predecessor_result_id: Some(original_id.clone()),
            ordinal: 2,
        });
        let state = if restart { reopen_review_test_state(&state) } else { state };
        assert_eq!(state.retain_review_attempt(&parent, &delegation, 1).unwrap().1, original_id);
        let after = state.review_custody_rows(&child, &delegation).unwrap();
        assert_eq!(after.len(), 3);
        let cleared: ReviewCustodySlot = decode_review_value(&after[2].body, &key).unwrap();
        assert_eq!(cleared.slot, pending.slot);
        assert_eq!(cleared.submission_attempt, 1);
        assert!(cleared.candidate.is_none());
        assert!(cleared.publication.is_none());
        assert_eq!(reserved_review_slot_body(&state, &child, &token), reserved);
        {
            let _owner = state.mailbox_store.delegation_review_recovery_guard.lock().unwrap();
            let error = state.mailbox_store.replace_review_custody_slot(
                &saved, &pending, &stale_confirmed, &original.write_policy, &key,
                Some(&original_id),
            ).err().unwrap();
            assert_eq!(error.status, StatusCode::CONFLICT);
            assert!(error.message.contains("pending candidate changed"));
        }
        install_delegation_codex_runtime(&state, "neutralized-review-next-capture");
        state.followup_delegation(&parent, &delegation, "Continue review.".to_owned()).unwrap();
        state.submit_delegation_review_result(&child, structured_review_request()).unwrap();
        finish_delegation_child_with_assistant_text(&state, &child, "Second attempt prose.");
        state.get_delegation_result(&parent, &delegation).unwrap();
        let (second, _) = state.retain_review_attempt(&parent, &delegation, 2).unwrap();
        assert_eq!(second.raw_output.as_deref(), Some("Second attempt prose."));
        let used = reserved_review_slot_body(&state, &child, &token);
        let used: ReviewCustodySlot = decode_review_value(&used, &key).unwrap();
        assert_eq!(used.submission_attempt, 2);
        assert!(used.publication.is_some());
        assert_eq!(state.review_custody_rows(&child, &delegation).unwrap().len(), 3);
        let old = state.get_delegation_result_output_selected(
            &parent, &delegation, 0, 256, &selected(1, Some(&original_id)),
        ).unwrap();
        assert_eq!(old.output, "Confirmed original prose.");
        let error = state.get_delegation_result_selected(
            &parent, &delegation, &selected(1, Some(&pending_id)),
        ).err().unwrap();
        assert_eq!(error.status, StatusCode::CONFLICT);
    }
}

#[test]
fn pending_recut_after_custody_adoption_cannot_block_a_new_attempt_after_restart() {
    let (state, parent, delegation, child) = terminal_review("Confirmed original prose.");
    let (original, original_id) = state.retain_review_attempt(&parent, &delegation, 1).unwrap();
    let token = state.reserve_next_review_capture(&original).unwrap();
    let reserved = reserved_review_slot_body(&state, &child, &token);
    let racing = state.clone();
    let p = parent.clone();
    let d = delegation.clone();
    let c = child.clone();
    TEST_REVIEW_FOLLOWUP_AFTER_RECUT_ADOPTION.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            reject_review_recut_back_to_original(&racing, &p, &d, &c);
        }));
    });
    install_delegation_codex_runtime(&state, "late-obsolete-review-recut");
    state.followup_delegation(&parent, &delegation, "Continue review.".to_owned()).unwrap();
    assert_eq!(reserved_review_slot_body(&state, &child, &token), reserved);
    let rows = state.review_custody_rows(&child, &delegation).unwrap();
    let key = state.mailbox_store.review_attempt_auth_key(false).unwrap().unwrap();
    let pending: ReviewCustodySlot = decode_review_value(&rows[2].body, &key).unwrap();
    assert!(pending.candidate.is_some());
    assert!(pending.publication.is_none());
    assert_eq!(pending.submission_attempt, 1);
    state.submit_delegation_review_result(&child, structured_review_request()).unwrap();
    finish_delegation_child_with_assistant_text(&state, &child, "Second attempt after late recut.");
    state.get_delegation_result(&parent, &delegation).unwrap();
    let restarted = reopen_review_test_state(&state);
    let (second, _) = restarted.retain_review_attempt(&parent, &delegation, 2).unwrap();
    assert_eq!(second.raw_output.as_deref(), Some("Second attempt after late recut."));
    let rows = restarted.review_custody_rows(&child, &delegation).unwrap();
    assert_eq!(rows.len(), 3);
    let cleared: ReviewCustodySlot = decode_review_value(&rows[2].body, &key).unwrap();
    assert!(cleared.candidate.is_none());
    assert_eq!(cleared.submission_attempt, 1);
    let protected: ReviewCustodySlot = decode_review_value(
        &reserved_review_slot_body(&restarted, &child, &token), &key,
    ).unwrap();
    assert_eq!(protected.submission_attempt, 2);
    assert!(protected.publication.is_some());
    assert_eq!(restarted.get_delegation_result_output_selected(
        &parent, &delegation, 0, 256, &selected(1, Some(&original_id)),
    ).unwrap().output, "Confirmed original prose.");
}

#[test]
fn failed_pending_neutralization_refuses_rearm_and_keeps_confirmed_reads_and_capacity() {
    let (state, parent, delegation, child) = terminal_review("Confirmed original prose.");
    let (original, original_id) = state.retain_review_attempt(&parent, &delegation, 1).unwrap();
    let token = state.reserve_next_review_capture(&original).unwrap();
    let reserved = reserved_review_slot_body(&state, &child, &token);
    reject_review_recut_back_to_original(&state, &parent, &delegation, &child);
    state.mailbox_store.connection().unwrap().execute_batch(
        "CREATE TEMP TRIGGER reject_pending_neutralization BEFORE UPDATE OF body ON mailbox_messages
         WHEN NEW.topic = 'delegation-review-terminal/v1'
           AND json_extract(json_extract(OLD.body,'$.payload'),'$.candidate') IS NOT NULL
           AND json_extract(json_extract(NEW.body,'$.payload'),'$.candidate') IS NULL
         BEGIN SELECT RAISE(ABORT,'pending neutralization failed'); END;",
    ).unwrap();
    let error = state.followup_delegation(&parent, &delegation, "Do not admit.".to_owned()).err().unwrap();
    assert!(error.message.contains("pending neutralization failed"), "{}", error.message);
    let selected_error = state
        .get_delegation_result_selected(&parent, &delegation, &selected(1, None))
        .err()
        .expect("explicit selection must propagate retention failure despite an older publication");
    assert!(selected_error.message.contains("pending neutralization failed"));
    let inner = state.inner.lock().unwrap();
    let record = &inner.delegations[inner.find_delegation_index(&delegation).unwrap()];
    assert_eq!(record.review_result_submission_attempt, 1);
    assert_eq!(record.status, DelegationStatus::Completed);
    assert!(!inner.delegation_followup_admissions.contains_key(&delegation));
    drop(inner);
    assert_eq!(state.get_delegation_result_output_selected(
        &parent, &delegation, 0, 256, &selected(1, Some(&original_id)),
    ).unwrap().output, "Confirmed original prose.");
    assert_eq!(reserved_review_slot_body(&state, &child, &token), reserved);
    assert_eq!(state.review_custody_rows(&child, &delegation).unwrap().len(), 3);
    state.mailbox_store.connection().unwrap().execute_batch("DROP TRIGGER reject_pending_neutralization").unwrap();
    assert_eq!(state.retain_review_attempt(&parent, &delegation, 1).unwrap().1, original_id);
}

#[test]
fn pending_neutralization_lost_reply_uses_exact_readback_and_refuses_a_replaced_body() {
    for replace_after_commit in [false, true] {
        let (state, parent, delegation, child) = terminal_review("Confirmed original prose.");
        let (original, original_id) = state.retain_review_attempt(&parent, &delegation, 1).unwrap();
        let token = state.reserve_next_review_capture(&original).unwrap();
        let reserved = reserved_review_slot_body(&state, &child, &token);
        reject_review_recut_back_to_original(&state, &parent, &delegation, &child);
        let saved = state.review_custody_rows(&child, &delegation).unwrap().pop().unwrap();
        let key = state.mailbox_store.review_attempt_auth_key(false).unwrap().unwrap();
        let pending: ReviewCustodySlot = decode_review_value(&saved.body, &key).unwrap();
        let racing = state.clone();
        let head = original_id.clone();
        let policy = original.write_policy.clone();
        let observed = Arc::new(AtomicBool::new(false));
        let reached = observed.clone();
        TEST_REVIEW_CUSTODY_AFTER_NEUTRALIZE_COMMIT.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                reached.store(true, Ordering::SeqCst);
                if replace_after_commit {
                    let current = racing.mailbox_store.read_idempotent_message(
                        &pending.child_session_id,
                        &review_attempt_key(&pending.delegation_id, pending.slot),
                    ).unwrap().unwrap();
                    let cleared: ReviewCustodySlot = decode_review_value(&current.body, &key).unwrap();
                    assert!(cleared.candidate.is_none());
                    assert!(cleared.publication.is_none());
                    // A real competing transaction replaces the committed body
                    // before the lost-reply path can read it back.
                    racing.mailbox_store.replace_review_custody_slot(
                        &current, &cleared, &pending, &policy, &key, Some(&head),
                    ).unwrap();
                }
                true
            }));
        });
        install_delegation_codex_runtime(&state, "neutralization-lost-reply");
        let followed = state.followup_delegation(&parent, &delegation, "Continue only with proven custody.".to_owned());
        assert!(observed.load(Ordering::SeqCst), "real neutralization commit hook must run");
        if replace_after_commit {
            assert!(followed.err().unwrap().message.contains("lost reply after the real custody commit"));
            let inner = state.inner.lock().unwrap();
            assert_eq!(inner.delegations[inner.find_delegation_index(&delegation).unwrap()]
                .review_result_submission_attempt, 1);
            assert!(!inner.delegation_followup_admissions.contains_key(&delegation));
        } else {
            assert_eq!(followed.unwrap().delegation.review_result_submission_attempt, 2);
        }
        assert_eq!(reserved_review_slot_body(&state, &child, &token), reserved);
        assert_eq!(state.review_custody_rows(&child, &delegation).unwrap().len(), 3);
        assert_eq!(state.get_delegation_result_output_selected(
            &parent, &delegation, 0, 256, &selected(1, Some(&original_id)),
        ).unwrap().output, "Confirmed original prose.");
    }
}

#[test]
fn future_or_unauthenticated_pending_state_is_not_neutralized_or_admitted() {
    for corrupt in [false, true] {
        let (state, parent, delegation, child) = terminal_review("Confirmed original prose.");
        let (original, original_id) = state.retain_review_attempt(&parent, &delegation, 1).unwrap();
        let token = state.reserve_next_review_capture(&original).unwrap();
        let saved = state.mailbox_store.read_idempotent_message(&child, &token).unwrap().unwrap();
        let key = state.mailbox_store.review_attempt_auth_key(false).unwrap().unwrap();
        let old: ReviewCustodySlot = decode_review_value(&saved.body, &key).unwrap();
        let mut future = original.clone();
        future.submission_attempt = 2;
        let mut pending = old.clone();
        pending.candidate = Some(serde_json::from_str(&encode_review_value(&future, &key).unwrap()).unwrap());
        let body = {
            let _owner = state.mailbox_store.delegation_review_recovery_guard.lock().unwrap();
            state.mailbox_store.replace_review_custody_slot(
                &saved, &old, &pending, &original.write_policy, &key, Some(&original_id),
            ).unwrap()
        };
        let body = if corrupt {
            let mut signed: SignedReviewAttempt = serde_json::from_str(&body).unwrap();
            signed.authentication_tag = "0".repeat(64);
            let body = serde_json::to_string(&signed).unwrap();
            state.mailbox_store.connection().unwrap().execute(
                "UPDATE mailbox_messages SET body=?2 WHERE id=?1",
                rusqlite::params![saved.message_id, body],
            ).unwrap();
            body
        } else { body };
        let error = state.followup_delegation(&parent, &delegation, "Refuse unexplained state.".to_owned()).err().unwrap();
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(error.message.contains(if corrupt { "unproven host provenance" } else { "future review attempt" }), "{}", error.message);
        assert_eq!(reserved_review_slot_body(&state, &child, &token), body);
        let inner = state.inner.lock().unwrap();
        assert_eq!(inner.delegations[inner.find_delegation_index(&delegation).unwrap()]
            .review_result_submission_attempt, 1);
        assert!(!inner.delegation_followup_admissions.contains_key(&delegation));
        drop(inner);
        assert_eq!(state.get_delegation_result_output_selected(
            &parent, &delegation, 0, 256, &selected(1, Some(&original_id)),
        ).unwrap().output, "Confirmed original prose.");
    }
}

#[test]
fn actual_restore_recuts_raw_custody_without_replacing_the_old_id_or_next_capture_slot() {
    let (state, parent, delegation, child) = terminal_review("Original reviewer prose.");
    let (original, old_id) = state
        .retain_review_attempt(&parent, &delegation, 1)
        .unwrap();
    let token = state.reserve_next_review_capture(&original).unwrap();
    let reserved = reserved_review_slot_body(&state, &child, &token);
    let driver = answer_review_restore(
        archived_review_recut_runtime(&state, &child),
        Some("prompt"),
    );
    let followed = state
        .followup_delegation(&parent, &delegation, "Continue after restore.".to_owned())
        .unwrap();
    let input_rx = driver.join().unwrap();
    assert!(input_rx.try_recv().is_err());
    assert_eq!(followed.delegation.review_result_submission_attempt, 2);
    let (refreshed, refreshed_id) = state
        .read_retained_review_attempt(&parent, &delegation, 1)
        .unwrap()
        .unwrap();
    assert_ne!(refreshed_id, old_id);
    assert_eq!(refreshed.result, original.result);
    let latest = state
        .get_delegation_result_selected(&parent, &delegation, &selected(1, None))
        .unwrap();
    assert_eq!(latest.result_id.as_deref(), Some(refreshed_id.as_str()));
    let latest_output = state
        .get_delegation_result_output_selected(
            &parent,
            &delegation,
            0,
            8192,
            &selected(1, latest.result_id.as_deref()),
        )
        .unwrap();
    assert!(latest_output.complete);
    assert_eq!(Some(latest_output.output), refreshed.raw_output);
    assert!(
        refreshed
            .raw_output
            .as_deref()
            .unwrap()
            .contains("Restored the archived Codex thread")
    );
    let (old, _) = state
        .read_review_publication(&parent, &delegation, 1, Some(&old_id))
        .unwrap()
        .unwrap();
    assert_eq!(old.raw_output, original.raw_output);
    assert_eq!(old.result, original.result);
    assert_eq!(reserved_review_slot_body(&state, &child, &token), reserved);
    assert_eq!(
        state
            .review_custody_rows(&child, &delegation)
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn canceled_or_replaced_reservation_cannot_adopt_a_completed_restore_recut() {
    for replaced in [false, true] {
        let (state, parent, delegation, child) =
            terminal_review("Reservation-owned reviewer prose.");
        let (original, old_id) = state
            .retain_review_attempt(&parent, &delegation, 1)
            .unwrap();
        let token = state.reserve_next_review_capture(&original).unwrap();
        let reserved = reserved_review_slot_body(&state, &child, &token);
        let observed = state.clone();
        let d = delegation.clone();
        TEST_REVIEW_FOLLOWUP_AFTER_RECUT.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                let mut inner = observed.inner.lock().unwrap();
                let current = inner.delegation_followup_admissions.get(&d).unwrap();
                if replaced {
                    let mut successor =
                        FollowupAdmissionReservation::new(current.last_user_prompt_id.clone());
                    successor.review_custody = current.review_custody.clone();
                    successor.review_capture_reservation =
                        current.review_capture_reservation.clone();
                    successor.review_custody_required = true;
                    assert_ne!(successor.reservation_id, current.reservation_id);
                    inner.delegation_followup_admissions.insert(d, successor);
                } else {
                    inner
                        .delegation_followup_admissions
                        .get_mut(&d)
                        .unwrap()
                        .canceled = true;
                }
            }))
        });
        let driver = answer_review_restore(
            archived_review_recut_runtime(&state, &child),
            if replaced { None } else { Some("archive") },
        );
        let error = state
            .followup_delegation(
                &parent,
                &delegation,
                "Do not admit a stale recut.".to_owned(),
            )
            .err()
            .unwrap();
        let input_rx = driver.join().unwrap();
        assert!(input_rx.try_recv().is_err());
        if !replaced {
            assert_eq!(
                state.wait_for_codex_child_release(&child).unwrap(),
                Some(CodexReleaseOutcome::Archived)
            );
        }
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(
            error
                .message
                .contains("caller reservation changed during recapture"),
            "{}",
            error.message
        );
        let inner = state.inner.lock().unwrap();
        let record = &inner.delegations[inner.find_delegation_index(&delegation).unwrap()];
        assert_eq!(record.result.as_ref(), Some(&original.result));
        assert_eq!(record.review_result_submission_attempt, 1);
        assert!(
            inner.sessions[inner.find_session_index(&child).unwrap()]
                .queued_prompts
                .is_empty()
        );
        if replaced {
            let successor = inner
                .delegation_followup_admissions
                .get(&delegation)
                .expect("old caller must not release its successor");
            assert_eq!(
                successor.review_custody.as_ref().unwrap().raw_output,
                original.raw_output
            );
            assert_eq!(
                successor.review_capture_reservation.as_deref(),
                Some(token.as_str())
            );
        } else {
            assert!(
                !inner
                    .delegation_followup_admissions
                    .contains_key(&delegation)
            );
        }
        drop(inner);
        assert_eq!(reserved_review_slot_body(&state, &child, &token), reserved);
        assert!(
            state
                .read_review_publication(&parent, &delegation, 1, Some(&old_id))
                .unwrap()
                .is_some()
        );
        if replaced {
            let (reservation_id, previous) = {
                let inner = state.inner.lock().unwrap();
                let child_record = &inner.sessions[inner.find_session_index(&child).unwrap()];
                assert!(
                    child_record
                        .codex_delegation_release
                        .as_ref()
                        .unwrap()
                        .needs_followup_compensation()
                );
                (
                    inner.delegation_followup_admissions[&delegation].reservation_id,
                    inner.delegations[inner.find_delegation_index(&delegation).unwrap()].clone(),
                )
            };
            let mut successor = DelegationFollowupAdmission {
                state: state.clone(),
                reservation_id,
                previous,
                restore_candidate: true,
                released: false,
            };
            let archive = std::thread::spawn(move || {
                let CodexRuntimeCommand::JsonRpcRequest {
                    method,
                    response_tx,
                    ..
                } = phase_sync::receive(&input_rx, "successor-owned archive compensation")
                else {
                    panic!("expected archive");
                };
                assert_eq!(method, "thread/archive");
                response_tx.send(Ok(json!({}))).unwrap();
                input_rx
            });
            assert!(successor.rollback_before_prompt().unwrap());
            successor.release().unwrap();
            let _input_rx = archive.join().unwrap();
            assert_eq!(
                state.wait_for_codex_child_release(&child).unwrap(),
                Some(CodexReleaseOutcome::Archived)
            );
            assert!(
                !state
                    .inner
                    .lock()
                    .unwrap()
                    .delegation_followup_admissions
                    .contains_key(&delegation)
            );
        }
    }
}

#[test]
fn drift_after_restore_recut_refuses_before_prompt_and_rearchives_the_child() {
    let (state, parent, delegation, child) = terminal_review("Before restore drift.");
    let observed = state.clone();
    let c = child.clone();
    TEST_REVIEW_FOLLOWUP_AFTER_RECUT_ADOPTION.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            finish_delegation_child_with_assistant_text(
                &observed,
                &c,
                "New raw cut after recapture.",
            );
        }))
    });
    let driver = answer_review_restore(
        archived_review_recut_runtime(&state, &child),
        Some("archive"),
    );
    let error = state
        .followup_delegation(
            &parent,
            &delegation,
            "Must not cross later drift.".to_owned(),
        )
        .err()
        .unwrap();
    let input_rx = driver.join().unwrap();
    assert!(input_rx.try_recv().is_err());
    assert_eq!(
        state.wait_for_codex_child_release(&child).unwrap(),
        Some(CodexReleaseOutcome::Archived)
    );
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error
            .message
            .contains("custody changed before prompt admission")
    );
    let inner = state.inner.lock().unwrap();
    let record = &inner.delegations[inner.find_delegation_index(&delegation).unwrap()];
    assert_eq!(record.review_result_submission_attempt, 1);
    let child = &inner.sessions[inner.find_session_index(&child).unwrap()];
    assert!(child.queued_prompts.is_empty());
    assert!(record_has_archived_codex_thread(child));
    assert!(
        !child
            .session
            .messages
            .iter()
            .any(|message| message.user_prompt_text() == Some("Must not cross later drift."))
    );
}

#[test]
fn restore_recut_capacity_and_unconfirmed_publication_preserve_reserved_capture_and_compensate() {
    for capacity in [false, true] {
        let (state, parent, delegation, child) = terminal_review("Before failed restore recut.");
        let (original, old_id) = state
            .retain_review_attempt(&parent, &delegation, 1)
            .unwrap();
        let token = state.reserve_next_review_capture(&original).unwrap();
        let reserved = reserved_review_slot_body(&state, &child, &token);
        if capacity {
            for version in 0..(MAX_RETAINED_REVIEW_ATTEMPTS - 2) {
                finish_delegation_child_with_assistant_text(
                    &state,
                    &child,
                    &format!("Retained recut {version}."),
                );
                state
                    .retain_review_attempt(&parent, &delegation, 1)
                    .unwrap();
            }
            assert_eq!(
                state
                    .review_custody_rows(&child, &delegation)
                    .unwrap()
                    .len(),
                64
            );
        } else {
            state.mailbox_store.connection().unwrap().execute_batch(
                "CREATE TEMP TRIGGER reject_restore_publication BEFORE UPDATE OF body ON mailbox_messages
                 WHEN NEW.topic='delegation-review-terminal/v1'
                   AND json_extract(json_extract(NEW.body,'$.payload'),'$.publication') IS NOT NULL
                 BEGIN SELECT RAISE(ABORT,'restore publication failed'); END;"
            ).unwrap();
        }
        let driver = answer_review_restore(
            archived_review_recut_runtime(&state, &child),
            Some("archive"),
        );
        let error = state
            .followup_delegation(&parent, &delegation, "Refused recapture.".to_owned())
            .err()
            .unwrap();
        let input_rx = driver.join().unwrap();
        assert!(input_rx.try_recv().is_err());
        assert_eq!(
            state.wait_for_codex_child_release(&child).unwrap(),
            Some(CodexReleaseOutcome::Archived)
        );
        assert!(
            error.message.contains(if capacity {
                "capacity is exhausted"
            } else {
                "restore publication failed"
            }),
            "{}",
            error.message
        );
        let inner = state.inner.lock().unwrap();
        assert_eq!(
            inner.delegations[inner.find_delegation_index(&delegation).unwrap()]
                .review_result_submission_attempt,
            1
        );
        let child_record = &inner.sessions[inner.find_session_index(&child).unwrap()];
        assert!(child_record.queued_prompts.is_empty());
        assert!(record_has_archived_codex_thread(child_record));
        assert!(
            !inner
                .delegation_followup_admissions
                .contains_key(&delegation)
        );
        drop(inner);
        assert_eq!(reserved_review_slot_body(&state, &child, &token), reserved);
        assert!(
            state
                .read_review_publication(&parent, &delegation, 1, Some(&old_id))
                .unwrap()
                .is_some()
        );
        if !capacity {
            assert_eq!(
                state
                    .read_retained_review_attempt(&parent, &delegation, 1)
                    .unwrap()
                    .unwrap()
                    .1,
                old_id
            );
        }
    }
}

#[test]
fn final_rearm_still_requires_the_exact_raw_cut_after_a_confirmed_recapture() {
    let (state, parent, delegation, child) = terminal_review("The confirmed admission cut.");
    let (snapshot, old_id) = state
        .retain_review_attempt(&parent, &delegation, 1)
        .unwrap();
    let token = state.reserve_next_review_capture(&snapshot).unwrap();
    let previous = {
        let mut inner = state.inner.lock().unwrap();
        let mut reservation = FollowupAdmissionReservation::new(
            delegation_last_user_prompt_id_locked(&inner, &child),
        );
        reservation.review_custody = Some(snapshot);
        reservation.review_capture_reservation = Some(token);
        reservation.review_custody_required = true;
        inner
            .delegation_followup_admissions
            .insert(delegation.clone(), reservation);
        inner.delegations[inner.find_delegation_index(&delegation).unwrap()].clone()
    };
    finish_delegation_child_with_assistant_text(&state, &child, "Later unmatched raw text.");
    let mut inner = state.inner.lock().unwrap();
    let error = state
        .commit_followup_prompt_locked(&mut inner, Some(&previous), "unadmitted-prompt", true)
        .err()
        .unwrap();
    assert!(
        error
            .to_string()
            .contains("custody or reserved capture changed before final rearm")
    );
    assert_eq!(
        inner.delegations[inner.find_delegation_index(&delegation).unwrap()],
        previous
    );
    drop(inner);
    assert!(
        state
            .read_review_publication(&parent, &delegation, 1, Some(&old_id))
            .unwrap()
            .is_some()
    );
}

#[test]
fn followup_and_real_restart_preserve_the_original_packet_and_pinned_utf8_pages() {
    let prose = format!("Original review: {}", "żółć".repeat(300));
    let (state, parent, delegation, child) = terminal_review(&prose);
    let first = state
        .get_delegation_result_selected(&parent, &delegation, &selected(1, None))
        .unwrap();
    let result_id = first.result_id.clone().unwrap();
    let first_page = state
        .get_delegation_result_output_selected(
            &parent,
            &delegation,
            0,
            256,
            &selected(1, Some(&result_id)),
        )
        .unwrap();
    assert!(!first_page.complete);
    install_delegation_codex_runtime(&state, "retained-review-followup");
    state
        .followup_delegation(&parent, &delegation, "Check the correction.".to_owned())
        .unwrap();
    let mut request = structured_review_request();
    request.summary = "Second attempt is clean.".to_owned();
    request.findings.clear();
    state
        .submit_delegation_review_result(&child, request)
        .unwrap();
    finish_delegation_child_with_assistant_text(&state, &child, "Second attempt raw prose.");
    assert_eq!(
        state
            .get_delegation_result(&parent, &delegation)
            .unwrap()
            .result
            .summary,
        "Second attempt is clean."
    );
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let rebooted = AppState {
        inner: Arc::new(StateMutex::new(loaded)),
        mailbox_store: Arc::new(
            MailboxStore::open(&resolve_coordination_persistence_path(
                state.persistence_path.as_ref(),
            ))
            .unwrap(),
        ),
        ..state.clone()
    };
    let before = {
        let inner = rebooted.inner.lock().unwrap();
        (
            inner.revision,
            serde_json::to_value(&*inner.delegations).unwrap(),
            serde_json::to_value(&*inner.delegation_waits).unwrap(),
        )
    };
    let retained = rebooted
        .get_delegation_result_selected(&parent, &delegation, &selected(1, Some(&result_id)))
        .unwrap();
    assert_eq!(retained.result, first.result);
    assert_eq!(retained.result_id.as_deref(), Some(result_id.as_str()));
    assert_eq!(
        retained.review_attempt_provenance.unwrap().submission,
        "validated"
    );
    let mut output = first_page.output;
    let mut offset = first_page.next_offset_bytes.unwrap();
    loop {
        let page = rebooted
            .get_delegation_result_output_selected(
                &parent,
                &delegation,
                offset,
                256,
                &selected(1, Some(&result_id)),
            )
            .unwrap();
        assert_eq!(page.result_id.as_deref(), Some(result_id.as_str()));
        output.push_str(&page.output);
        if page.complete {
            break;
        }
        offset = page.next_offset_bytes.unwrap();
    }
    assert_eq!(output.as_bytes(), prose.as_bytes());
    let inner = rebooted.inner.lock().unwrap();
    assert_eq!(inner.revision, before.0);
    assert_eq!(serde_json::to_value(&*inner.delegations).unwrap(), before.1);
    assert_eq!(
        serde_json::to_value(&*inner.delegation_waits).unwrap(),
        before.2
    );
}

#[test]
fn retention_is_idempotent_and_rejects_wrong_parent_attempt_and_result_identity() {
    let (state, parent, delegation, _) = terminal_review("Original prose.");
    let first = state
        .retain_review_attempt(&parent, &delegation, 1)
        .unwrap();
    let second = state
        .retain_review_attempt(&parent, &delegation, 1)
        .unwrap();
    assert_eq!(first.1, second.1);
    assert_eq!(
        serde_json::to_value(first.0).unwrap(),
        serde_json::to_value(second.0).unwrap()
    );
    let unauthorized = state
        .get_delegation_result_selected("foreign-parent", &delegation, &selected(1, None))
        .err()
        .unwrap();
    assert_eq!(unauthorized.status, StatusCode::NOT_FOUND);
    assert!(
        state
            .get_delegation_result_selected(&parent, &delegation, &selected(2, None))
            .is_err()
    );
    assert!(
        state
            .get_delegation_result_output_selected(
                &parent,
                &delegation,
                0,
                256,
                &selected(1, Some("wrong-id"))
            )
            .is_err()
    );
}

#[test]
fn failed_snapshot_write_refuses_followup_without_clearing_the_terminal_result() {
    let (state, parent, delegation, _) = terminal_review("Do not lose this prose.");
    state.mailbox_store.connection().unwrap().execute_batch(
        "CREATE TEMP TRIGGER reject_review_snapshot BEFORE INSERT ON mailbox_messages
         WHEN NEW.topic='delegation-review-terminal/v1' BEGIN SELECT RAISE(ABORT,'snapshot write failed'); END;"
    ).unwrap();
    let before = state
        .get_delegation_result(&parent, &delegation)
        .unwrap()
        .result;
    let error = state
        .followup_delegation(&parent, &delegation, "Follow up.".to_owned())
        .err()
        .unwrap();
    assert!(
        error.message.contains("snapshot write failed"),
        "{}",
        error.message
    );
    assert_eq!(
        state
            .get_delegation_result(&parent, &delegation)
            .unwrap()
            .result,
        before
    );
    let inner = state.inner.lock().unwrap();
    let d = &inner.delegations[inner.find_delegation_index(&delegation).unwrap()];
    assert_eq!(d.review_result_submission_attempt, 1);
    assert_eq!(d.status, DelegationStatus::Completed);
    assert!(
        !inner
            .delegation_followup_admissions
            .contains_key(&delegation)
    );
}

#[test]
fn oversized_complete_prose_and_attempt_exhaustion_never_truncate_or_rearm() {
    let prose = "x".repeat(MAX_MAILBOX_BODY_BYTES);
    let (state, parent, delegation, _) = terminal_review(&prose);
    let error = state
        .followup_delegation(&parent, &delegation, "Follow up.".to_owned())
        .err()
        .unwrap();
    assert!(error.message.contains("256 KiB"));
    assert_eq!(
        state
            .get_delegation_result_output(&parent, &delegation, 0, 8192)
            .unwrap()
            .total_bytes,
        prose.len()
    );
    {
        let mut inner = state.inner.lock().unwrap();
        let i = inner.find_delegation_index(&delegation).unwrap();
        inner.delegations[i].review_result_submission_attempt = MAX_RETAINED_REVIEW_ATTEMPTS;
        state.commit_locked(&mut inner).unwrap();
    }
    let error = state
        .followup_delegation(&parent, &delegation, "Beyond capacity.".to_owned())
        .err()
        .unwrap();
    assert!(error.message.contains("capacity is exhausted"));
    let inner = state.inner.lock().unwrap();
    let d = &inner.delegations[inner.find_delegation_index(&delegation).unwrap()];
    assert_eq!(
        d.review_result_submission_attempt,
        MAX_RETAINED_REVIEW_ATTEMPTS
    );
    assert!(d.result.is_some());
}

#[test]
fn lost_append_reply_resolves_the_same_durable_key_and_terminal_cut() {
    let (state, parent, delegation, _) = terminal_review("A committed snapshot.");
    let seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = seen.clone();
    TEST_REVIEW_CUSTODY_AFTER_CONFIRM_COMMIT.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            observed.store(true, Ordering::SeqCst);
            true
        }))
    });
    let first = state
        .retain_review_attempt(&parent, &delegation, 1)
        .unwrap();
    let again = state
        .retain_review_attempt(&parent, &delegation, 1)
        .unwrap();
    assert_eq!(first.1, again.1);
    assert_eq!(first.0.raw_output, again.0.raw_output);
    assert!(
        seen.load(Ordering::SeqCst),
        "the real post-commit lost-reply cut must run"
    );
}

#[test]
fn a_changed_terminal_cut_after_real_append_cannot_authorize_rearm() {
    let (state, parent, delegation, _) = terminal_review("Original cut.");
    let changed = state.clone();
    let id = delegation.clone();
    TEST_REVIEW_ATTEMPT_AFTER_APPEND.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let mut inner = changed.inner.lock().unwrap();
            let i = inner.find_delegation_index(&id).unwrap();
            inner.delegations[i].result.as_mut().unwrap().summary =
                "A concurrent different terminal cut".to_owned();
            false
        }))
    });
    let error = state
        .followup_delegation(&parent, &delegation, "Follow up.".to_owned())
        .err()
        .unwrap();
    assert!(error.message.contains("terminal cut changed"));
    let inner = state.inner.lock().unwrap();
    assert_eq!(
        inner.delegations[inner.find_delegation_index(&delegation).unwrap()]
            .review_result_submission_attempt,
        1
    );
    assert!(
        !inner
            .delegation_followup_admissions
            .contains_key(&delegation)
    );
    drop(inner);
    assert!(
        state
            .read_retained_review_attempt(&parent, &delegation, 1)
            .unwrap()
            .is_none(),
        "a failed candidate is never reader-authoritative"
    );
    let retried = state
        .get_delegation_result_selected(&parent, &delegation, &selected(1, None))
        .unwrap();
    assert_eq!(
        retried.result.summary,
        "A concurrent different terminal cut"
    );
    let rows = state
        .review_custody_rows(&retried.result.child_session_id, &delegation)
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "retry replaces the one unpublished candidate without a tombstone"
    );
}

#[test]
fn missing_or_corrupt_owner_key_over_history_is_never_regenerated() {
    for corruption in [None, Some("not-a-valid-key")] {
        let (state, parent, delegation, _) = terminal_review("Retained under original key.");
        state
            .retain_review_attempt(&parent, &delegation, 1)
            .unwrap();
        {
            let connection = state.mailbox_store.connection().unwrap();
            match corruption {
                None => {
                    connection
                        .execute("DELETE FROM meta WHERE key=?1", [REVIEW_ATTEMPT_AUTH_KEY])
                        .unwrap();
                }
                Some(value) => {
                    connection
                        .execute(
                            "UPDATE meta SET value=?2 WHERE key=?1",
                            rusqlite::params![REVIEW_ATTEMPT_AUTH_KEY, value],
                        )
                        .unwrap();
                }
            }
        }
        assert!(
            state
                .read_retained_review_attempt(&parent, &delegation, 1)
                .is_err()
        );
        assert!(state.mailbox_store.review_attempt_auth_key(true).is_err());
        let key: Option<String> = state
            .mailbox_store
            .connection()
            .unwrap()
            .query_row(
                "SELECT value FROM meta WHERE key=?1",
                [REVIEW_ATTEMPT_AUTH_KEY],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert!(key.as_deref() == corruption);
    }
}

#[test]
fn independent_store_connections_initialize_one_key_and_old_open_preserves_opaque_custody() {
    let (state, parent, delegation, _) = terminal_review("Opaque old-reader custody.");
    let path = resolve_coordination_persistence_path(state.persistence_path.as_ref());
    let a = MailboxStore::open(&path).unwrap();
    let b = MailboxStore::open(&path).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let barrier_a = barrier.clone();
    let handle = std::thread::spawn(move || {
        barrier_a.wait();
        a.review_attempt_auth_key(true).unwrap().unwrap()
    });
    barrier.wait();
    let key_b = b.review_attempt_auth_key(true).unwrap().unwrap();
    let key_a = handle.join().unwrap();
    assert!(key_a.as_str() == key_b.as_str());
    let first = state
        .retain_review_attempt(&parent, &delegation, 1)
        .unwrap();
    drop(b);
    // These schema validation/open/departure owners are byte-unchanged from
    // the previous reader. They treat the added meta/message values as opaque.
    let old_reader = MailboxStore::open(&path).unwrap();
    old_reader.mark_session_left(&parent).unwrap();
    assert!(
        old_reader
            .review_attempt_auth_key(false)
            .unwrap()
            .unwrap()
            .as_str()
            == key_a.as_str()
    );
    assert_eq!(
        state
            .read_retained_review_attempt(&parent, &delegation, 1)
            .unwrap()
            .unwrap()
            .1,
        first.1
    );
}

#[test]
fn legacy_missing_provenance_and_prose_are_reported_separately_without_summary_substitution() {
    let (state, parent, delegation, child) = terminal_review("Original prose.");
    {
        let connection = state.mailbox_store.connection().unwrap();
        connection
            .execute(
                "DELETE FROM mailbox_messages WHERE sender_session_id=?1 AND idempotency_key=?2",
                rusqlite::params![
                    child,
                    delegation_review_result_idempotency_key(&delegation, 1)
                ],
            )
            .unwrap();
    }
    {
        let mut inner = state.inner.lock().unwrap();
        let i = inner.find_session_index(&child).unwrap();
        inner.sessions[i].session.messages.clear();
        state.commit_locked(&mut inner).unwrap();
    }
    let packet = state
        .get_delegation_result_selected(&parent, &delegation, &selected(1, None))
        .unwrap();
    let provenance = packet.review_attempt_provenance.unwrap();
    assert_eq!(provenance.submission, "missing");
    assert!(!provenance.raw_output_available);
    let error = state
        .get_delegation_result_output_selected(&parent, &delegation, 0, 256, &selected(1, None))
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert!(error.message.contains("no summary fallback"));
}

#[test]
fn a_legal_large_producer_envelope_cannot_be_silently_reduced_to_fit_retention() {
    let (state, _, parent) = mailbox_test_state();
    let (delegation, child) = install_required_review_delegation(&state, &parent);
    let mut request = structured_review_request();
    request.notes = vec!["n".repeat(15_000); 12];
    state
        .submit_delegation_review_result(&child, request)
        .unwrap();
    finish_delegation_child_with_assistant_text(&state, &child, "Small raw prose.");
    let original = state
        .get_delegation_result(&parent, &delegation)
        .unwrap()
        .result;
    let error = state
        .followup_delegation(&parent, &delegation, "Follow up.".to_owned())
        .err()
        .unwrap();
    assert!(error.message.contains("256 KiB"));
    assert_eq!(
        state
            .get_delegation_result(&parent, &delegation)
            .unwrap()
            .result,
        original
    );
    assert!(
        state
            .read_retained_review_attempt(&parent, &delegation, 1)
            .unwrap()
            .is_none()
    );
}

#[test]
fn concurrent_retention_retries_publish_one_identical_snapshot_without_dispatch_deadlock() {
    let (state, parent, delegation, _) = terminal_review("Concurrent immutable prose.");
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let worker = state.clone();
    let p = parent.clone();
    let d = delegation.clone();
    let start = barrier.clone();
    let handle = std::thread::spawn(move || {
        start.wait();
        worker.retain_review_attempt(&p, &d, 1).unwrap().1
    });
    barrier.wait();
    let id = state
        .retain_review_attempt(&parent, &delegation, 1)
        .unwrap()
        .1;
    assert_eq!(handle.join().unwrap(), id);
    let count: u64 = state
        .mailbox_store
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM mailbox_messages WHERE topic=?1",
            [REVIEW_ATTEMPT_TOPIC],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn generic_root_child_and_producer_routes_cannot_forge_or_replace_a_host_snapshot() {
    let (state, parent, delegation, child) = terminal_review("Host-owned prose.");
    let (_, id) = state
        .retain_review_attempt(&parent, &delegation, 1)
        .unwrap();
    let original = state
        .mailbox_store
        .read_delegation_review_result_for_recovery(&child, &review_attempt_key(&delegation, 1))
        .unwrap()
        .unwrap();
    let send = |target: &str| SendMailboxMessageRequest {
        target_session_id: target.to_owned(),
        message: original.body.clone(),
        idempotency_key: review_attempt_key(&delegation, 1),
        topic: Some(REVIEW_ATTEMPT_TOPIC.to_owned()),
        state_stamp: Some(format!("{delegation}:1")),
        class: None,
    };
    assert!(
        state
            .append_mailbox_message_and_notify(&child, send(&parent))
            .is_err()
    );
    let other_root = test_session_id(&state, Agent::Codex);
    let root_error = state
        .append_mailbox_message_and_notify(&other_root, send(&parent))
        .err()
        .expect("routine root must not write host custody topics");
    assert_eq!(root_error.status, StatusCode::BAD_REQUEST);
    assert!(
        root_error
            .message
            .contains("reserved for internal result custody")
    );
    let producer = state
        .submit_delegation_review_result(&child, structured_review_request())
        .unwrap();
    assert_ne!(producer.message_id, original.message_id);
    assert_eq!(
        state
            .read_retained_review_attempt(&parent, &delegation, 1)
            .unwrap()
            .unwrap()
            .1,
        id
    );
}

#[test]
fn unsigned_altered_and_replayed_host_envelopes_are_explicitly_rejected() {
    for damage in ["unsigned", "altered", "replayed"] {
        let (state, parent, delegation, child) = terminal_review("Authentic raw prose.");
        let (_, id) = state
            .retain_review_attempt(&parent, &delegation, 1)
            .unwrap();
        let stored = state
            .mailbox_store
            .read_delegation_review_result_for_recovery(&child, &review_attempt_key(&delegation, 1))
            .unwrap()
            .unwrap();
        if damage == "replayed" {
            {
                let mut inner = state.inner.lock().unwrap();
                let i = inner.find_delegation_index(&delegation).unwrap();
                rearm_terminal_delegation_for_followup_locked(&mut inner, i).unwrap();
            }
            state
                .mailbox_store
                .append_delegation_review_result(&MailboxAppendInput {
                    sender_session_id: child.clone(),
                    sender_name: child.clone(),
                    target_session_id: parent.clone(),
                    target_name: parent.clone(),
                    body: stored.body,
                    idempotency_key: review_attempt_key(&delegation, 2),
                    topic: Some(REVIEW_ATTEMPT_TOPIC.to_owned()),
                    state_stamp: Some(format!("{delegation}:2")),
                })
                .unwrap();
            let error = state
                .read_retained_review_attempt(&parent, &delegation, 2)
                .err()
                .unwrap();
            assert_eq!(error.status, StatusCode::CONFLICT);
            assert!(
                error
                    .message
                    .contains("corrupt or has unproven host provenance")
            );
            assert_eq!(
                state
                    .read_review_publication(&parent, &delegation, 1, Some(&id))
                    .unwrap()
                    .unwrap()
                    .1,
                id
            );
        } else {
            let body = if damage == "unsigned" {
                "{}".to_owned()
            } else {
                stored
                    .body
                    .replace("Authentic raw prose.", "Fabricated prose.")
            };
            assert_ne!(body, stored.body);
            state
                .mailbox_store
                .connection()
                .unwrap()
                .execute(
                    "UPDATE mailbox_messages SET body=?2 WHERE id=?1",
                    rusqlite::params![stored.message_id, body],
                )
                .unwrap();
            let error = state
                .get_delegation_result_selected(&parent, &delegation, &selected(1, None))
                .err()
                .unwrap();
            assert_eq!(error.status, StatusCode::CONFLICT);
            assert!(
                error
                    .message
                    .contains("corrupt or has unproven host provenance")
            );
        }
    }
}

#[test]
fn old_coordination_schema_migration_preserves_the_opaque_key_and_exact_attempt_body() {
    let (state, parent, delegation, child) = terminal_review("Migration preserves exact prose.");
    let (_, original_id) = state
        .retain_review_attempt(&parent, &delegation, 1)
        .unwrap();
    let source_path = resolve_coordination_persistence_path(state.persistence_path.as_ref());
    let old_path = state
        .persistence_path
        .parent()
        .unwrap()
        .join("old-review-coordination.sqlite");
    {
        let connection = rusqlite::Connection::open(&old_path).unwrap();
        connection
            .execute_batch(COORDINATION_SCHEMA_V1_SQL)
            .unwrap();
        connection
            .execute(
                "INSERT INTO meta(key,value) VALUES('coordination_schema_version','1')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "ATTACH DATABASE ?1 AS original",
                [source_path.to_string_lossy().as_ref()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO meta SELECT key,value FROM original.meta WHERE key=?1",
                [REVIEW_ATTEMPT_AUTH_KEY],
            )
            .unwrap();
        connection
            .execute_batch(
                "INSERT INTO mailboxes SELECT * FROM original.mailboxes;
            INSERT INTO mailbox_participants SELECT * FROM original.mailbox_participants;
            INSERT INTO mailbox_messages SELECT * FROM original.mailbox_messages;",
            )
            .unwrap();
    }
    let migrated = AppState {
        mailbox_store: Arc::new(MailboxStore::open(&old_path).unwrap()),
        ..state.clone()
    };
    assert_eq!(
        migrated
            .read_retained_review_attempt(&parent, &delegation, 1)
            .unwrap()
            .unwrap()
            .1,
        original_id
    );
    let before = state
        .mailbox_store
        .read_delegation_review_result_for_recovery(&child, &review_attempt_key(&delegation, 1))
        .unwrap()
        .unwrap();
    let after = migrated
        .mailbox_store
        .read_delegation_review_result_for_recovery(&child, &review_attempt_key(&delegation, 1))
        .unwrap()
        .unwrap();
    assert_eq!(before.body.as_bytes(), after.body.as_bytes());
}

#[test]
fn a_preforged_snapshot_is_not_signed_or_adopted_during_followup() {
    let (state, parent, delegation, child) = terminal_review("Still the original terminal result.");
    state
        .mailbox_store
        .append_delegation_review_result(&MailboxAppendInput {
            sender_session_id: child.clone(),
            sender_name: child,
            target_session_id: parent.clone(),
            target_name: parent.clone(),
            body: "{}".to_owned(),
            idempotency_key: review_attempt_key(&delegation, 1),
            topic: Some(REVIEW_ATTEMPT_TOPIC.to_owned()),
            state_stamp: Some(format!("{delegation}:1")),
        })
        .unwrap();
    let before = state
        .get_delegation_result(&parent, &delegation)
        .unwrap()
        .result;
    let error = state
        .followup_delegation(&parent, &delegation, "Follow up.".to_owned())
        .err()
        .unwrap();
    assert!(
        error
            .message
            .contains("key is missing over retained history"),
        "{}",
        error.message
    );
    assert!(
        state
            .mailbox_store
            .review_attempt_auth_key(false)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        state
            .get_delegation_result(&parent, &delegation)
            .unwrap()
            .result,
        before
    );
}

#[test]
fn http_output_query_parses_attempt_and_page_as_typed_numeric_selectors() {
    let uri = "/result/output?submissionAttempt=3&resultId=review-result-abc&offsetBytes=256&limitBytes=8192"
        .parse::<axum::http::Uri>().unwrap();
    let Query(query) = Query::<DelegationResultOutputQuery>::try_from_uri(&uri).unwrap();
    assert_eq!(query.submission_attempt, Some(3));
    assert_eq!(query.result_id.as_deref(), Some("review-result-abc"));
    assert_eq!(query.offset_bytes, 256);
    assert_eq!(query.limit_bytes, 8192);
}

#[test]
fn retained_snapshots_are_hidden_non_waking_control_rows_with_preserved_participants() {
    let (state, parent, delegation, child) = terminal_review("Private retained reviewer prose.");
    let store = &state.mailbox_store;
    let submission = store
        .read_delegation_review_result_for_recovery(
            &child,
            &delegation_review_result_idempotency_key(&delegation, 1),
        )
        .unwrap()
        .unwrap();
    let participants =
        mailbox_participants(&store.connection().unwrap(), &submission.mailbox_id).unwrap();
    state
        .retain_review_attempt(&parent, &delegation, 1)
        .unwrap();
    let retained = store
        .read_delegation_review_result_for_recovery(&child, &review_attempt_key(&delegation, 1))
        .unwrap()
        .unwrap();
    assert!(store.list_for_session(&parent).unwrap().is_empty());
    let hidden = store
        .read_range(&parent, &retained.mailbox_id, Some(0), 20)
        .unwrap();
    assert!(hidden.messages.is_empty());
    assert!(hidden.receipt.is_none());
    assert!(store.read_message(&parent, &retained.message_id).is_err());
    assert!(
        store
            .unread_wakeup_for_mailbox(&parent, &retained.mailbox_id)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .unread_wakeup_for_mailbox_through(&parent, &retained.mailbox_id, 100)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .mailbox_wake_boundary(&parent, &retained.mailbox_id, &retained.message_id)
            .unwrap()
            .is_none()
    );
    for recovery in [
        MailboxWakeupRecovery::NeverWoken,
        MailboxWakeupRecovery::AllUnreadAfterBoot,
    ] {
        assert!(
            store
                .wakeups_for_session(&parent, recovery)
                .unwrap()
                .is_empty()
        );
    }
    let after = mailbox_participants(&store.connection().unwrap(), &retained.mailbox_id).unwrap();
    assert_eq!(
        participants
            .iter()
            .map(|p| (&p.session_id, &p.display_name))
            .collect::<Vec<_>>(),
        after
            .iter()
            .map(|p| (&p.session_id, &p.display_name))
            .collect::<Vec<_>>()
    );
    let ordinary = store
        .append(&MailboxAppendInput {
            sender_session_id: child.clone(),
            sender_name: submission.sender_name,
            target_session_id: parent.clone(),
            target_name: submission.target_name,
            body: "Visible coordination after hidden custody.".to_owned(),
            idempotency_key: "visible-after-retained-cut".to_owned(),
            topic: None,
            state_stamp: None,
        })
        .unwrap()
        .receipt;
    let page = store
        .read_range(&parent, &ordinary.mailbox_id, Some(0), 20)
        .unwrap();
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].id, ordinary.message_id);
    assert_eq!(ordinary.unread_depth, 1);
    assert_eq!(store.list_for_session(&parent).unwrap()[0].unread_count, 1);
    store
        .acknowledge_page(
            &parent,
            &ordinary.mailbox_id,
            page.receipt.as_deref().unwrap(),
        )
        .unwrap();
    assert_eq!(store.list_for_session(&parent).unwrap()[0].unread_count, 0);
}

#[test]
fn public_validation_reserves_the_entire_review_control_namespace_before_key_creation() {
    let (state, parent, delegation, child) =
        terminal_review("No forged history may be introduced.");
    for (topic, key) in [
        (Some(REVIEW_ATTEMPT_TOPIC), "unrelated-key"),
        (Some("delegation-review-terminal/next"), "unrelated-key"),
        (None, "delegation-review-terminal-v1-spoof"),
        (None, "delegation-review-result-v1-spoof"),
    ] {
        let input = MailboxAppendInput {
            sender_session_id: "other-root".to_owned(),
            sender_name: "Other root".to_owned(),
            target_session_id: parent.clone(),
            target_name: "Parent".to_owned(),
            body: "Unsigned fake.".to_owned(),
            idempotency_key: key.to_owned(),
            topic: topic.map(str::to_owned),
            state_stamp: None,
        };
        let error = state
            .mailbox_store
            .append(&input)
            .err()
            .expect("reserved namespace must be refused");
        assert!(
            error
                .to_string()
                .contains("reserved for internal result custody")
        );
        assert!(
            state
                .mailbox_store
                .read_idempotent_message("other-root", key)
                .unwrap()
                .is_none()
        );
    }
    assert!(
        state
            .mailbox_store
            .review_attempt_auth_key(false)
            .unwrap()
            .is_none()
    );
    state
        .retain_review_attempt(&parent, &delegation, 1)
        .unwrap();
    assert!(
        state
            .mailbox_store
            .review_attempt_auth_key(false)
            .unwrap()
            .is_some()
    );
    assert!(
        state
            .read_retained_review_attempt(&parent, &delegation, 1)
            .unwrap()
            .is_some()
    );
    assert_ne!(parent, child);
}

#[test]
fn completion_between_followup_observation_and_reservation_never_bypasses_custody() {
    for attempt in [1, MAX_RETAINED_REVIEW_ATTEMPTS] {
        let (state, _, parent) = mailbox_test_state();
        let (delegation, child) = install_required_review_delegation(&state, &parent);
        let (runtime, _input_rx) = test_codex_runtime_handle("completion-before-reservation");
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_delegation_index(&delegation).unwrap();
            inner.delegations[index].review_result_submission_attempt = attempt;
            let index = inner.find_session_index(&child).unwrap();
            inner.sessions[index].session.status = SessionStatus::Active;
            inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
            state.commit_locked(&mut inner).unwrap();
        }
        assert_eq!(
            state
                .get_delegation(&parent, &delegation)
                .unwrap()
                .delegation
                .status,
            DelegationStatus::Running
        );
        let captured = state.clone();
        let captured_child = child.clone();
        TEST_REVIEW_BEFORE_FOLLOWUP_RESERVATION.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                captured
                    .submit_delegation_review_result(&captured_child, structured_review_request())
                    .unwrap();
                finish_delegation_child_with_assistant_text(
                    &captured,
                    &captured_child,
                    "Finished inside the admission race.",
                );
            }));
        });
        let error = state
            .followup_delegation(&parent, &delegation, "Follow up.".to_owned())
            .err()
            .expect("a raced terminal cut must not be rearmed without custody");
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(
            error.message.contains("became terminal before reservation"),
            "{}",
            error.message
        );
        let inner = state.inner.lock().unwrap();
        let record = &inner.delegations[inner.find_delegation_index(&delegation).unwrap()];
        assert_eq!(record.status, DelegationStatus::Completed);
        assert_eq!(record.review_result_submission_attempt, attempt);
        assert!(record.result.is_some());
        assert!(
            !inner
                .delegation_followup_admissions
                .contains_key(&delegation)
        );
        drop(inner);
        assert!(
            state
                .read_retained_review_attempt(&parent, &delegation, attempt)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn unconfirmed_candidates_are_invisible_to_an_independent_reader_and_restart_revalidates_them() {
    let (state, parent, delegation, child) = terminal_review("An unpublished original cut.");
    let observed = state.clone();
    let p = parent.clone();
    let d = delegation.clone();
    let c = child.clone();
    let rejected_id = Arc::new(std::sync::Mutex::new(None));
    let captured_id = rejected_id.clone();
    TEST_REVIEW_ATTEMPT_AFTER_APPEND.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let independent = AppState {
                mailbox_store: Arc::new(
                    MailboxStore::open(&resolve_coordination_persistence_path(
                        observed.persistence_path.as_ref(),
                    ))
                    .unwrap(),
                ),
                ..observed.clone()
            };
            assert!(
                independent
                    .read_retained_review_attempt(&p, &d, 1)
                    .unwrap()
                    .is_none()
            );
            let rows = independent.review_custody_rows(&c, &d).unwrap();
            let key = independent
                .mailbox_store
                .review_attempt_auth_key(false)
                .unwrap()
                .unwrap();
            let slot: ReviewCustodySlot = decode_review_value(&rows[0].body, &key).unwrap();
            assert!(slot.publication.is_none());
            *captured_id.lock().unwrap() =
                Some(review_candidate_id(slot.candidate.as_ref().unwrap()).unwrap());
            finish_delegation_child_with_assistant_text(&observed, &c, "A changed raw-only cut.");
            false
        }))
    });
    let error = state
        .retain_review_attempt(&parent, &delegation, 1)
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("terminal cut changed"));
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let restarted = AppState {
        inner: Arc::new(StateMutex::new(loaded)),
        mailbox_store: Arc::new(
            MailboxStore::open(&resolve_coordination_persistence_path(
                state.persistence_path.as_ref(),
            ))
            .unwrap(),
        ),
        ..state.clone()
    };
    assert!(
        restarted
            .read_retained_review_attempt(&parent, &delegation, 1)
            .unwrap()
            .is_none()
    );
    let pending_id = rejected_id.lock().unwrap().clone().unwrap();
    let error = restarted
        .get_delegation_result_selected(&parent, &delegation, &selected(1, Some(&pending_id)))
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    let (_, published) = restarted
        .retain_review_attempt(&parent, &delegation, 1)
        .unwrap();
    assert_ne!(published, pending_id);
    let output = restarted
        .get_delegation_result_output_selected(
            &parent,
            &delegation,
            0,
            256,
            &selected(1, Some(&published)),
        )
        .unwrap();
    assert_eq!(output.output, "A changed raw-only cut.");
    assert_eq!(
        restarted
            .review_custody_rows(&child, &delegation)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn same_attempt_repairs_publish_new_ids_without_changing_old_packets_or_pages() {
    let (state, parent, delegation, child) = terminal_review("Original published prose.");
    let first = state
        .get_delegation_result_selected(&parent, &delegation, &selected(1, None))
        .unwrap();
    let first_id = first.result_id.clone().unwrap();
    let first_page = state
        .get_delegation_result_output_selected(
            &parent,
            &delegation,
            0,
            256,
            &selected(1, Some(&first_id)),
        )
        .unwrap();
    finish_delegation_child_with_assistant_text(
        &state,
        &child,
        "Repaired prose in the same attempt.",
    );
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&delegation).unwrap();
        inner.delegations[index].result.as_mut().unwrap().summary =
            "Repaired terminal packet".to_owned();
        state.commit_locked(&mut inner).unwrap();
    }
    let repaired = state
        .get_delegation_result_selected(&parent, &delegation, &selected(1, None))
        .unwrap();
    let repaired_id = repaired.result_id.clone().unwrap();
    assert_ne!(repaired_id, first_id);
    assert_eq!(repaired.result.summary, "Repaired terminal packet");
    let old = state
        .get_delegation_result_selected(&parent, &delegation, &selected(1, Some(&first_id)))
        .unwrap();
    assert_eq!(old.result, first.result);
    let old_page = state
        .get_delegation_result_output_selected(
            &parent,
            &delegation,
            0,
            256,
            &selected(1, Some(&first_id)),
        )
        .unwrap();
    assert_eq!(old_page.output, first_page.output);
    assert_eq!(old_page.total_bytes, first_page.total_bytes);
    assert_eq!(old_page.result_id, first_page.result_id);
    state
        .commit_locked(&mut state.inner.lock().unwrap())
        .unwrap();
    assert_eq!(
        state
            .get_delegation_result_selected(&parent, &delegation, &selected(1, None))
            .unwrap()
            .result_id,
        Some(repaired_id)
    );
    assert_eq!(
        state
            .review_custody_rows(&child, &delegation)
            .unwrap()
            .len(),
        2,
        "a revision-only change is not another semantic version"
    );
}

#[test]
fn explicit_latest_selection_keeps_current_cut_custody_separate_after_a_confirmed_reversion() {
    let (state, parent, delegation, child) = terminal_review("Original A prose.");
    let original = state
        .get_delegation_result_selected(&parent, &delegation, &selected(1, None))
        .unwrap();
    let original_id = original.result_id.clone().unwrap();
    let original_page = state
        .get_delegation_result_output_selected(
            &parent, &delegation, 0, 256, &selected(1, Some(&original_id)),
        )
        .unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&delegation).unwrap();
        inner.delegations[index].result.as_mut().unwrap().summary = "Confirmed C packet".to_owned();
        state.commit_locked(&mut inner).unwrap();
    }
    finish_delegation_child_with_assistant_text(&state, &child, "Confirmed C prose.");
    let confirmed = state
        .get_delegation_result_selected(&parent, &delegation, &selected(1, None))
        .unwrap();
    let confirmed_id = confirmed.result_id.clone().unwrap();
    assert_ne!(confirmed_id, original_id);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&delegation).unwrap();
        inner.delegations[index].result = Some(original.result.clone());
        state.commit_locked(&mut inner).unwrap();
    }
    finish_delegation_child_with_assistant_text(&state, &child, "Original A prose.");
    let (custody, custody_id) = state.retain_review_attempt(&parent, &delegation, 1).unwrap();
    assert_eq!(custody_id, original_id);
    assert_eq!(custody.result, original.result);
    assert_eq!(custody.raw_output.as_deref(), Some("Original A prose."));
    assert_eq!(state.get_delegation_result(&parent, &delegation).unwrap().result, original.result);
    let latest = state
        .get_delegation_result_selected(&parent, &delegation, &selected(1, None))
        .unwrap();
    assert_eq!(latest.result, confirmed.result);
    assert_eq!(latest.result_id.as_deref(), Some(confirmed_id.as_str()));
    let latest_page = state
        .get_delegation_result_output_selected(&parent, &delegation, 0, 256, &selected(1, None))
        .unwrap();
    assert_eq!(latest_page.result_id.as_deref(), Some(confirmed_id.as_str()));
    assert_eq!(latest_page.output, "Confirmed C prose.");
    assert_eq!(state.review_custody_rows(&child, &delegation).unwrap().len(), 2);
    let token = state.reserve_next_review_capture(&custody).unwrap();
    let reserved = reserved_review_slot_body(&state, &child, &token);
    let inspected = Arc::new(AtomicBool::new(false));
    let observed = inspected.clone();
    let checking = state.clone();
    let d = delegation.clone();
    let expected = original.result.clone();
    let expected_token = token.clone();
    TEST_REVIEW_FOLLOWUP_AFTER_RECUT_ADOPTION.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let inner = checking.inner.lock().unwrap();
            let admission = &inner.delegation_followup_admissions[&d];
            let actual = admission.review_custody.as_ref().unwrap();
            assert_eq!(actual.result, expected);
            assert_eq!(actual.raw_output.as_deref(), Some("Original A prose."));
            assert_eq!(admission.review_capture_reservation.as_deref(), Some(expected_token.as_str()));
            observed.store(true, Ordering::SeqCst);
        }));
    });
    install_delegation_codex_runtime(&state, "latest-selection-current-custody");
    let followed = state.followup_delegation(&parent, &delegation, "Continue with actual A custody.".to_owned()).unwrap();
    assert_eq!(followed.delegation.review_result_submission_attempt, 2);
    assert!(inspected.load(Ordering::SeqCst));
    assert_eq!(reserved_review_slot_body(&state, &child, &token), reserved);
    state.submit_delegation_review_result(&child, structured_review_request()).unwrap();
    finish_delegation_child_with_assistant_text(&state, &child, "New attempt D prose.");
    state.get_delegation_result(&parent, &delegation).unwrap();
    let second = state.get_delegation_result_selected(&parent, &delegation, &selected(2, None)).unwrap();
    assert_ne!(second.result_id.as_deref(), Some(confirmed_id.as_str()));
    let historical = state.get_delegation_result_selected(&parent, &delegation, &selected(1, None)).unwrap();
    assert_eq!(historical.result_id.as_deref(), Some(confirmed_id.as_str()));
    assert_eq!(historical.result, confirmed.result);
    let historical_page = state.get_delegation_result_output_selected(
        &parent, &delegation, 0, 256, &selected(1, Some(&confirmed_id)),
    ).unwrap();
    assert_eq!(historical_page.output, latest_page.output);
    let pinned_original = state.get_delegation_result_selected(
        &parent, &delegation, &selected(1, Some(&original_id)),
    ).unwrap();
    assert_eq!(pinned_original.result, original.result);
    let pinned_page = state.get_delegation_result_output_selected(
        &parent, &delegation, 0, 256, &selected(1, Some(&original_id)),
    ).unwrap();
    assert_eq!(pinned_page.output, original_page.output);
    assert_eq!(pinned_page.result_id, original_page.result_id);
    assert_eq!(pinned_page.total_bytes, original_page.total_bytes);
    assert_eq!(state.review_custody_rows(&child, &delegation).unwrap().len(), 3);
}

#[test]
fn legacy_zero_refuses_capture_and_rearm_honestly_without_renumbering_or_hiding_default_evidence() {
    let (state, parent, delegation, _) =
        terminal_review("Legacy current evidence remains available.");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&delegation).unwrap();
        inner.delegations[index].review_result_submission_attempt = 0;
        state.commit_locked(&mut inner).unwrap();
    }
    let default = state.get_delegation_result(&parent, &delegation).unwrap();
    for error in [
        state
            .get_delegation_result_selected(&parent, &delegation, &selected(0, None))
            .err()
            .unwrap(),
        state
            .followup_delegation(
                &parent,
                &delegation,
                "Do not silently discard legacy evidence.".to_owned(),
            )
            .err()
            .unwrap(),
    ] {
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(
            error
                .message
                .contains("legacy review attempt identity is unavailable")
        );
    }
    let after = state.get_delegation_result(&parent, &delegation).unwrap();
    assert_eq!(after.submission_attempt, Some(0));
    assert_eq!(after.result, default.result);
}

#[test]
fn recut_capacity_cannot_consume_the_reserved_first_capture_of_the_next_admitted_attempt() {
    let (state, parent, delegation, child) = terminal_review("Original capacity witness.");
    let (first, first_id) = state
        .retain_review_attempt(&parent, &delegation, 1)
        .unwrap();
    let reservation = state.reserve_next_review_capture(&first).unwrap();
    let mut latest_prose = String::new();
    for version in 0..(MAX_RETAINED_REVIEW_ATTEMPTS - 2) {
        latest_prose = format!("Distinct semantic repair {version}.");
        finish_delegation_child_with_assistant_text(&state, &child, &latest_prose);
        state
            .retain_review_attempt(&parent, &delegation, 1)
            .unwrap();
    }
    assert_eq!(
        state
            .review_custody_rows(&child, &delegation)
            .unwrap()
            .len(),
        64
    );
    finish_delegation_child_with_assistant_text(
        &state,
        &child,
        "A repair beyond the charged capacity.",
    );
    let error = state
        .retain_review_attempt(&parent, &delegation, 1)
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("capacity is exhausted"));
    assert_eq!(
        state
            .get_delegation_result_selected(&parent, &delegation, &selected(1, Some(&first_id)))
            .unwrap()
            .result_id,
        Some(first_id)
    );
    finish_delegation_child_with_assistant_text(&state, &child, &latest_prose);
    let (current, _) = state
        .retain_review_attempt(&parent, &delegation, 1)
        .unwrap();
    assert_eq!(
        state.reserve_next_review_capture(&current).unwrap(),
        reservation
    );
    install_delegation_codex_runtime(&state, "reserved-next-review-capacity");
    state
        .followup_delegation(
            &parent,
            &delegation,
            "The next terminal cut already owns a slot.".to_owned(),
        )
        .unwrap();
    state
        .submit_delegation_review_result(&child, structured_review_request())
        .unwrap();
    finish_delegation_child_with_assistant_text(&state, &child, "The reserved second attempt.");
    state.get_delegation_result(&parent, &delegation).unwrap();
    let (second, _) = state
        .retain_review_attempt(&parent, &delegation, 2)
        .unwrap();
    assert_eq!(
        second.raw_output.as_deref(),
        Some("The reserved second attempt.")
    );
    assert_eq!(
        state
            .review_custody_rows(&child, &delegation)
            .unwrap()
            .len(),
        64
    );
    assert!(
        state
            .reserve_next_review_capture(&second)
            .err()
            .unwrap()
            .message
            .contains("capacity is exhausted")
    );
}

#[test]
fn a_replaced_candidate_and_advanced_predecessor_refuse_the_old_finalizer_transaction() {
    let (state, parent, delegation, child) = terminal_review("First confirmed cut.");
    let (_, first_id) = state
        .retain_review_attempt(&parent, &delegation, 1)
        .unwrap();
    finish_delegation_child_with_assistant_text(
        &state,
        &child,
        "Candidate awaiting its publication transaction.",
    );
    let racing = state.clone();
    let p = parent.clone();
    let d = delegation.clone();
    let c = child.clone();
    let winner = Arc::new(std::sync::Mutex::new(None));
    let saved_winner = winner.clone();
    TEST_REVIEW_CUSTODY_AFTER_CUT_VALIDATION.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let independent = AppState {
                mailbox_store: Arc::new(
                    MailboxStore::open(&resolve_coordination_persistence_path(
                        racing.persistence_path.as_ref(),
                    ))
                    .unwrap(),
                ),
                ..racing.clone()
            };
            finish_delegation_child_with_assistant_text(
                &independent,
                &c,
                "A different finalizer publishes first.",
            );
            *saved_winner.lock().unwrap() =
                Some(independent.retain_review_attempt(&p, &d, 1).unwrap().1);
        }))
    });
    let error = state
        .retain_review_attempt(&parent, &delegation, 1)
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error.message.contains("publication predecessor changed"),
        "{}",
        error.message
    );
    let winner = winner.lock().unwrap().clone().unwrap();
    assert_eq!(
        state
            .get_delegation_result_selected(&parent, &delegation, &selected(1, None))
            .unwrap()
            .result_id,
        Some(winner.clone())
    );
    assert_eq!(
        state
            .get_delegation_result_output_selected(
                &parent,
                &delegation,
                0,
                256,
                &selected(1, Some(&winner))
            )
            .unwrap()
            .output,
        "A different finalizer publishes first."
    );
    assert_eq!(
        state
            .get_delegation_result_output_selected(
                &parent,
                &delegation,
                0,
                256,
                &selected(1, Some(&first_id))
            )
            .unwrap()
            .output,
        "First confirmed cut."
    );
    assert_eq!(
        state
            .review_custody_rows(&child, &delegation)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn current_capture_recovers_the_durable_submission_before_freezing_failed_primary_projection() {
    let (state, parent, delegation, child) =
        terminal_review("Durably submitted result before capture.");
    let original = state
        .get_delegation_result(&parent, &delegation)
        .unwrap()
        .result;
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&delegation).unwrap();
        let record = &mut inner.delegations[index];
        record.status = DelegationStatus::Failed;
        record.result.as_mut().unwrap().status = DelegationStatus::Failed;
        record.result.as_mut().unwrap().summary =
            "Submission projection was lost before restart".to_owned();
        record.submitted_review_result = None;
        record.review_result_schema_version = None;
        record.review_result_recovery_probe_attempt = None;
        state.commit_locked(&mut inner).unwrap();
    }
    let selected = state
        .get_delegation_result_selected(&parent, &delegation, &selected(1, None))
        .unwrap();
    assert_eq!(selected.result.status, DelegationStatus::Completed);
    assert_eq!(selected.result.summary, original.summary);
    assert_eq!(
        selected.review_attempt_provenance.unwrap().submission,
        "validated"
    );
    let inner = state.inner.lock().unwrap();
    let record = &inner.delegations[inner.find_delegation_index(&delegation).unwrap()];
    assert_eq!(
        record.review_result_schema_version,
        Some(DELEGATION_REVIEW_RESULT_SCHEMA_VERSION)
    );
    assert_eq!(record.result.as_ref(), Some(&original));
    assert!(record.submitted_review_result.is_none());
    assert_eq!(record.child_session_id, child);
    drop(inner);
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = loaded
        .delegations
        .iter()
        .find(|record| record.id == delegation)
        .unwrap();
    assert_eq!(record.result.as_ref(), Some(&original));
    assert_eq!(
        record.review_result_schema_version,
        Some(DELEGATION_REVIEW_RESULT_SCHEMA_VERSION)
    );
}

#[tokio::test]
async fn federated_mailbox_reads_receipts_and_routine_sends_isolate_review_custody() {
    let state = super::federation_mailboxes::central_store();
    let (sender, a) = super::federation_mailboxes::local_participant(&state, "Reviewer transport");
    let (reader, b) = super::federation_mailboxes::local_participant(&state, "Review parent");
    let hidden = state
        .mailbox_store
        .append_delegation_review_result(&MailboxAppendInput {
            sender_session_id: a.session_id.clone(),
            sender_name: a.name.clone(),
            target_session_id: b.session_id.clone(),
            target_name: b.name.clone(),
            body: "Private host custody envelope.".to_owned(),
            idempotency_key: "delegation-review-terminal-v1-domain-filter".to_owned(),
            topic: Some(REVIEW_ATTEMPT_TOPIC.to_owned()),
            state_stamp: None,
        })
        .unwrap()
        .receipt;
    state
        .mailbox_store
        .record_initial_dispatch_outcome(&hidden.message_id, "durableButNotWoken")
        .unwrap();
    let page: MailboxReadResponse = state
        .federation_mailbox_operation(
            &reader,
            FederationMailboxOperation::Read {
                mailbox_id: hidden.mailbox_id.clone(),
                after_sequence: Some(0),
                limit: 20,
                issue_receipt: true,
            },
        )
        .await
        .unwrap();
    assert!(page.messages.is_empty());
    assert!(page.receipt.is_none());
    let exact: Result<MailboxMessage, ApiError> = state
        .federation_mailbox_operation(
            &reader,
            FederationMailboxOperation::ReadMessage {
                message_id: hidden.message_id.clone(),
            },
        )
        .await;
    assert_eq!(exact.err().unwrap().status, StatusCode::NOT_FOUND);
    let forged: Result<MailboxAppendReceipt, ApiError> = state
        .federation_mailbox_operation(
            &sender,
            FederationMailboxOperation::Send {
                target: b.session_id.clone(),
                message: "Forged control row".to_owned(),
                key: "routine-reserved-topic".to_owned(),
                topic: Some(REVIEW_ATTEMPT_TOPIC.to_owned()),
                state_stamp: None,
            },
        )
        .await;
    let error = forged.err().unwrap();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("reserved"));
    let visible = state
        .mailbox_store
        .append(&MailboxAppendInput {
            sender_session_id: a.session_id,
            sender_name: a.name,
            target_session_id: b.session_id,
            target_name: b.name,
            body: "Ordinary domain coordination.".to_owned(),
            idempotency_key: "ordinary-after-custody".to_owned(),
            topic: None,
            state_stamp: None,
        })
        .unwrap()
        .receipt;
    state
        .mailbox_store
        .record_initial_dispatch_outcome(&visible.message_id, "durableButNotWoken")
        .unwrap();
    let page: MailboxReadResponse = state
        .federation_mailbox_operation(
            &reader,
            FederationMailboxOperation::Read {
                mailbox_id: hidden.mailbox_id.clone(),
                after_sequence: Some(0),
                limit: 20,
                issue_receipt: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].id, visible.message_id);
    let acknowledged: MailboxSummary = state
        .federation_mailbox_operation(
            &reader,
            FederationMailboxOperation::Acknowledge {
                mailbox_id: hidden.mailbox_id,
                receipt: page.receipt.unwrap(),
            },
        )
        .await
        .unwrap();
    assert_eq!(acknowledged.unread_count, 0);
}

#[test]
fn corrupt_submission_provenance_stays_explicit_on_its_original_published_identity() {
    let (state, parent, delegation, child) =
        terminal_review("Raw evidence survives a corrupt envelope.");
    let envelope = state
        .mailbox_store
        .read_delegation_review_result_for_recovery(
            &child,
            &delegation_review_result_idempotency_key(&delegation, 1),
        )
        .unwrap()
        .unwrap();
    state
        .mailbox_store
        .connection()
        .unwrap()
        .execute(
            "UPDATE mailbox_messages SET body='not a submission envelope' WHERE id=?1",
            [&envelope.message_id],
        )
        .unwrap();
    let corrupt = state
        .get_delegation_result_selected(&parent, &delegation, &selected(1, None))
        .unwrap();
    let corrupt_id = corrupt.result_id.unwrap();
    assert_eq!(
        corrupt.review_attempt_provenance.unwrap().submission,
        "corrupt"
    );
    state
        .mailbox_store
        .connection()
        .unwrap()
        .execute(
            "UPDATE mailbox_messages SET body=?2 WHERE id=?1",
            rusqlite::params![envelope.message_id, envelope.body],
        )
        .unwrap();
    let repaired = state
        .get_delegation_result_selected(&parent, &delegation, &selected(1, None))
        .unwrap();
    assert_ne!(repaired.result_id.as_deref(), Some(corrupt_id.as_str()));
    assert_eq!(
        repaired.review_attempt_provenance.unwrap().submission,
        "validated"
    );
    let pinned = state
        .get_delegation_result_selected(&parent, &delegation, &selected(1, Some(&corrupt_id)))
        .unwrap();
    assert_eq!(
        pinned.review_attempt_provenance.unwrap().submission,
        "corrupt"
    );
    assert_eq!(
        state
            .get_delegation_result_output_selected(
                &parent,
                &delegation,
                0,
                256,
                &selected(1, Some(&corrupt_id))
            )
            .unwrap()
            .output,
        "Raw evidence survives a corrupt envelope."
    );
}
