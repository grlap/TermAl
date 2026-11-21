//! Parent-packet discovery supplies immutable selections without widening reads,
//! and the parent packet publishes a completed or failed reviewer's terminal
//! current attempt once, under the canonical parent id and the owner guard.
//! Uses the existing archive and terminal-review fixtures; no provider or live
//! operator store is read.
use super::*;

#[test]
fn parent_compact_packet_supplies_each_retained_identity_for_an_independent_reader() {
    let (state, parent, delegation, child, reader, first_id) = access_fixture();
    let first_output = "Original validated prose. λ\n".repeat(70);
    let repaired_output = "A repaired publication in the first attempt.";
    finish_delegation_child_with_assistant_text(&state, &child, repaired_output);
    let (_, repaired_id) = state
        .retain_review_attempt(&parent, &delegation, 1)
        .unwrap();
    assert_ne!(first_id, repaired_id);

    install_delegation_codex_runtime(&state, "review-discovery-second-attempt");
    state
        .followup_delegation(&parent, &delegation, "Review again.".into())
        .unwrap();
    state
        .submit_delegation_review_result(&child, structured_review_request())
        .unwrap();
    let second_output = "The second attempt's original output.";
    finish_delegation_child_with_assistant_text(&state, &child, second_output);
    state.get_delegation_result(&parent, &delegation).unwrap();
    let (_, second_id) = state
        .retain_review_attempt(&parent, &delegation, 2)
        .unwrap();

    let packet =
        serde_json::to_value(state.get_delegation_result(&parent, &delegation).unwrap()).unwrap();
    let identities = packet["retainedReviewAttempts"]
        .as_array()
        .expect("the parent packet must expose its retained review identities");
    assert_eq!(identities.len(), 3);
    let expected = BTreeMap::from([
        (first_id, (1, first_output.as_str())),
        (repaired_id, (1, repaired_output)),
        (second_id, (2, second_output)),
    ]);
    let before = changes(&state);
    let mut observed = BTreeSet::new();
    for identity in identities {
        let selected: DelegationResultSelection = serde_json::from_value(identity.clone()).unwrap();
        let attempt = selected.submission_attempt.expect("published attempt");
        let id = selected.result_id.as_deref().expect("immutable result id");
        let (expected_attempt, expected_output) = expected.get(id).expect("known publication");
        assert_eq!(attempt, *expected_attempt);
        assert!(observed.insert(id.to_owned()), "no duplicate identity");

        let read = state
            .get_delegation_result_selected(&reader, &delegation, &selected)
            .unwrap();
        assert_eq!(read.submission_attempt, Some(attempt));
        assert_eq!(read.result_id.as_deref(), Some(id));
        let output = state
            .get_delegation_result_output_selected(&reader, &delegation, 0, 8192, &selected)
            .unwrap();
        assert_eq!(output.output, *expected_output);
        assert!(output.complete);
        assert!(!output.summary_fallback);

        let missing_id = DelegationResultSelection {
            submission_attempt: Some(attempt),
            result_id: None,
        };
        let error = state
            .get_delegation_result_selected(&reader, &delegation, &missing_id)
            .err()
            .expect("discovery must not bypass exact-identity access");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.starts_with("exact_identity_required"));
        assert!(error.message.contains("termal_get_session_result"));
        assert!(error.message.contains("retainedReviewAttempts"));
        assert!(error.message.contains("parent"));
    }
    assert_eq!(observed.len(), expected.len());
    assert_eq!(changes(&state), before, "exact reads and refusals are pure");
}

#[test]
fn unavailable_discovery_does_not_hide_the_parents_current_result() {
    let (state, parent, delegation, child, _, _) = access_fixture();
    let original = state
        .get_delegation_result(&parent, &delegation)
        .unwrap()
        .result;
    let rows = state
        .review_custody_rows_read_only(Some(&child), &delegation)
        .unwrap();
    state
        .mailbox_store
        .connection()
        .unwrap()
        .execute(
            "UPDATE mailbox_messages SET body=?1 WHERE id=?2",
            rusqlite::params!["damaged custody", rows[0].message_id],
        )
        .unwrap();

    let packet = state.get_delegation_result(&parent, &delegation).unwrap();
    assert_eq!(packet.result, original);
    let packet = serde_json::to_value(packet).unwrap();
    assert!(packet.get("retainedReviewAttempts").is_none());
    assert!(
        packet["retainedReviewAttemptsError"]
            .as_str()
            .unwrap()
            .contains("corrupt")
    );
}

#[test]
fn unpublished_custody_is_not_offered_as_an_exact_review_identity() {
    let (state, parent, delegation, child, reader, id) = access_fixture();
    let rows = state
        .review_custody_rows_read_only(Some(&child), &delegation)
        .unwrap();
    let key = state
        .mailbox_store
        .review_attempt_auth_key(false)
        .unwrap()
        .unwrap();
    let mut slot: ReviewCustodySlot = decode_review_value(&rows[0].body, &key).unwrap();
    slot.publication = None;
    state
        .mailbox_store
        .connection()
        .unwrap()
        .execute(
            "UPDATE mailbox_messages SET body=?1 WHERE id=?2",
            rusqlite::params![
                encode_review_value(&slot, &key).unwrap(),
                rows[0].message_id
            ],
        )
        .unwrap();

    let packet =
        serde_json::to_value(state.get_delegation_result(&parent, &delegation).unwrap()).unwrap();
    assert!(packet.get("retainedReviewAttempts").is_none());
    assert!(packet.get("retainedReviewAttemptsError").is_none());
    let error = state
        .get_delegation_result_selected(&reader, &delegation, &selection(&id))
        .err()
        .expect("a provisional candidate remains unavailable");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.starts_with("review_result_pending"));
}

fn unselected() -> DelegationResultSelection {
    DelegationResultSelection {
        submission_attempt: None,
        result_id: None,
    }
}

#[test]
fn parent_packet_lists_a_terminal_attempt_that_was_never_retained() {
    let output = "A completed review that nothing retained yet.";
    let (state, parent, delegation, _) = terminal_review(output);
    let internal =
        serde_json::to_value(state.get_delegation_result(&parent, &delegation).unwrap()).unwrap();
    assert!(internal.get("retainedReviewAttempts").is_none());
    assert!(internal.get("retainedReviewAttemptsError").is_none());

    let packet = serde_json::to_value(
        state
            .get_delegation_result_selected(&parent, &delegation, &unselected())
            .unwrap(),
    )
    .unwrap();
    let identities = packet["retainedReviewAttempts"]
        .as_array()
        .expect("the parent packet must list the terminal attempt without an explicit read");
    assert_eq!(identities.len(), 1);
    assert!(packet.get("retainedReviewAttemptsError").is_none());
    let selected: DelegationResultSelection =
        serde_json::from_value(identities[0].clone()).unwrap();
    assert_eq!(selected.submission_attempt, Some(1));
    let page = state
        .get_delegation_result_output_selected(&parent, &delegation, 0, 8192, &selected)
        .unwrap();
    assert_eq!(page.output, output);

    let before = changes(&state);
    let again = serde_json::to_value(
        state
            .get_delegation_result_selected(&parent, &delegation, &unselected())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        again["retainedReviewAttempts"],
        packet["retainedReviewAttempts"]
    );
    assert_eq!(
        changes(&state),
        before,
        "a confirmed terminal attempt is listed without another custody write"
    );
}

#[test]
fn parent_packet_reports_unreadable_custody_of_its_terminal_attempt() {
    let (state, parent, delegation, child) =
        terminal_review("A completed review whose custody is later damaged.");
    state
        .get_delegation_result_selected(&parent, &delegation, &unselected())
        .unwrap();
    let rows = state
        .review_custody_rows_read_only(Some(&child), &delegation)
        .unwrap();
    assert!(
        !rows.is_empty(),
        "the parent packet published the terminal attempt"
    );
    state
        .mailbox_store
        .connection()
        .unwrap()
        .execute(
            "UPDATE mailbox_messages SET body=?1 WHERE id=?2",
            rusqlite::params!["damaged custody", rows[0].message_id],
        )
        .unwrap();
    let original = state
        .get_delegation_result(&parent, &delegation)
        .unwrap()
        .result;

    let packet = state
        .get_delegation_result_selected(&parent, &delegation, &unselected())
        .unwrap();
    assert_eq!(packet.result, original);
    let packet = serde_json::to_value(packet).unwrap();
    assert!(packet.get("retainedReviewAttempts").is_none());
    assert!(
        packet["retainedReviewAttemptsError"]
            .as_str()
            .unwrap()
            .contains("corrupt")
    );
}

#[test]
fn parent_packet_lists_an_attempt_confirmed_before_its_owner_guard_without_a_second_version() {
    let (state, parent, delegation, _) = terminal_review("The original terminal cut.");
    let racing = state.clone();
    let (racing_parent, racing_delegation) = (parent.clone(), delegation.clone());
    let racing_id = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let recorded_id = racing_id.clone();
    TEST_REVIEW_PACKET_BEFORE_OWNER_GUARD.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            // Another read confirms the attempt after the packet's unguarded
            // check, then the terminal cut changes before the packet's guard.
            let (_, id) = racing
                .retain_review_attempt(&racing_parent, &racing_delegation, 1)
                .unwrap();
            *recorded_id.lock().unwrap() = Some(id);
            let mut inner = racing.inner.lock().unwrap();
            let index = inner.find_delegation_index(&racing_delegation).unwrap();
            inner.delegations[index].result.as_mut().unwrap().summary =
                "A later, different terminal cut".to_owned();
        }));
    });

    let packet = serde_json::to_value(
        state
            .get_delegation_result_selected(&parent, &delegation, &unselected())
            .unwrap(),
    )
    .unwrap();
    assert!(packet.get("retainedReviewAttemptsError").is_none());
    let identities = packet["retainedReviewAttempts"]
        .as_array()
        .expect("the attempt confirmed by the other read is listed");
    assert_eq!(
        identities.len(),
        1,
        "the packet must not publish a second version of an already confirmed attempt"
    );
    let selected: DelegationResultSelection =
        serde_json::from_value(identities[0].clone()).unwrap();
    assert_eq!(selected.submission_attempt, Some(1));
    let racing_id = racing_id
        .lock()
        .unwrap()
        .clone()
        .expect("the racing read ran between the packet's check and its guard");
    assert_eq!(
        selected.result_id.as_deref(),
        Some(racing_id.as_str()),
        "the listed identity is the racing read's publication"
    );
}

#[test]
fn parent_packet_with_a_padded_parent_id_keeps_custody_under_the_canonical_parent() {
    let (state, parent, delegation, _) =
        terminal_review("A completed review read through a padded parent id.");
    let padded = format!("  {parent} ");
    let packet = serde_json::to_value(
        state
            .get_delegation_result_selected(&padded, &delegation, &unselected())
            .unwrap(),
    )
    .unwrap();
    assert!(packet.get("retainedReviewAttemptsError").is_none());
    let identities = packet["retainedReviewAttempts"]
        .as_array()
        .expect("the padded parent's packet lists the terminal attempt");
    assert_eq!(identities.len(), 1);

    let canonical = serde_json::to_value(
        state
            .get_delegation_result_selected(&parent, &delegation, &unselected())
            .unwrap(),
    )
    .unwrap();
    assert!(
        canonical.get("retainedReviewAttemptsError").is_none(),
        "custody written for the padded read validates under the canonical parent"
    );
    assert_eq!(
        canonical["retainedReviewAttempts"],
        packet["retainedReviewAttempts"]
    );
    let selected: DelegationResultSelection =
        serde_json::from_value(identities[0].clone()).unwrap();
    let (_, retained) = state
        .retain_review_attempt(&parent, &delegation, 1)
        .unwrap();
    assert_eq!(
        selected.result_id.as_deref(),
        Some(retained.as_str()),
        "a later canonical retention finds the same publication"
    );

    let before = changes(&state);
    let repeated = serde_json::to_value(
        state
            .get_delegation_result_selected(&padded, &delegation, &unselected())
            .unwrap(),
    )
    .unwrap();
    assert!(
        repeated.get("retainedReviewAttemptsError").is_none(),
        "a repeated padded read does not report the canonical custody as corrupt"
    );
    assert_eq!(
        repeated["retainedReviewAttempts"],
        packet["retainedReviewAttempts"]
    );
    assert_eq!(
        changes(&state),
        before,
        "a repeated padded read makes no custody write"
    );
}

#[test]
fn parent_packet_reports_a_failed_terminal_publication_and_still_serves_its_result() {
    let (state, parent, delegation, _) = terminal_review("A completed review whose cut moves.");
    let served = state
        .get_delegation_result(&parent, &delegation)
        .unwrap()
        .result;
    let changed = state.clone();
    let changing = delegation.clone();
    TEST_REVIEW_ATTEMPT_AFTER_APPEND.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let mut inner = changed.inner.lock().unwrap();
            let index = inner.find_delegation_index(&changing).unwrap();
            inner.delegations[index].result.as_mut().unwrap().summary =
                "A concurrent different terminal cut".to_owned();
            false
        }));
    });

    let packet = state
        .get_delegation_result_selected(&parent, &delegation, &unselected())
        .unwrap();
    assert_eq!(packet.result, served);
    let packet = serde_json::to_value(packet).unwrap();
    assert!(packet.get("retainedReviewAttempts").is_none());
    assert!(
        packet["retainedReviewAttemptsError"]
            .as_str()
            .unwrap()
            .contains("terminal cut changed")
    );
}

#[test]
fn explicit_reads_with_a_padded_parent_id_resolve_the_canonical_parent() {
    let output = "A completed review selected through a padded parent id.";
    let (state, parent, delegation, _) = terminal_review(output);
    let padded = format!("  {parent} ");
    let attempt_only = DelegationResultSelection {
        submission_attempt: Some(1),
        result_id: None,
    };
    let explicit = state
        .get_delegation_result_selected(&padded, &delegation, &attempt_only)
        .unwrap();
    let id = explicit
        .result_id
        .clone()
        .expect("the explicit read publishes the attempt");

    let canonical = serde_json::to_value(
        state
            .get_delegation_result_selected(&parent, &delegation, &unselected())
            .unwrap(),
    )
    .unwrap();
    assert!(
        canonical.get("retainedReviewAttemptsError").is_none(),
        "custody written by the padded explicit read validates under the canonical parent"
    );
    let identities = canonical["retainedReviewAttempts"].as_array().unwrap();
    assert_eq!(identities.len(), 1);
    assert_eq!(identities[0]["resultId"], json!(id));
    let (_, retained) = state
        .retain_review_attempt(&parent, &delegation, 1)
        .unwrap();
    assert_eq!(retained, id, "a later canonical retention finds it");

    // The result-id branch serves the padded id as the parent, not as a
    // root: the fixture's parent belongs to no project, so a root read
    // would be refused.
    let exact = state
        .get_delegation_result_selected(&padded, &delegation, &selection(&id))
        .unwrap();
    assert_eq!(exact.result_id.as_deref(), Some(id.as_str()));
    let page = state
        .get_delegation_result_output_selected(&padded, &delegation, 0, 8192, &selection(&id))
        .unwrap();
    assert_eq!(page.output, output);

    install_delegation_codex_runtime(&state, "review-padded-parent-followup");
    state
        .followup_delegation(&parent, &delegation, "Review again.".into())
        .expect("a follow-up retains the cut under the canonical parent");
}

#[test]
fn parent_packet_publishes_a_failed_terminal_attempt() {
    let (state, parent, delegation, _) = terminal_review("A reviewer that ended as failed.");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&delegation).unwrap();
        let record = &mut inner.delegations[index];
        record.status = DelegationStatus::Failed;
        record.result.as_mut().unwrap().status = DelegationStatus::Failed;
        state.commit_locked(&mut inner).unwrap();
    }

    let packet = state
        .get_delegation_result_selected(&parent, &delegation, &unselected())
        .unwrap();
    assert_eq!(packet.result.status, DelegationStatus::Failed);
    let packet = serde_json::to_value(packet).unwrap();
    assert!(packet.get("retainedReviewAttemptsError").is_none());
    let identities = packet["retainedReviewAttempts"]
        .as_array()
        .expect("a failed terminal attempt is published too");
    assert_eq!(identities.len(), 1);
    assert_eq!(identities[0]["submissionAttempt"], json!(1));
}

#[test]
fn parent_packet_does_not_publish_a_non_current_attempt() {
    let (state, parent, delegation, child) = terminal_review("The first attempt.");
    install_delegation_codex_runtime(&state, "review-packet-non-current");
    state
        .followup_delegation(&parent, &delegation, "Review again.".into())
        .unwrap();
    state
        .submit_delegation_review_result(&child, structured_review_request())
        .unwrap();
    finish_delegation_child_with_assistant_text(&state, &child, "The second attempt.");
    let mut packet = state.get_delegation_result(&parent, &delegation).unwrap();
    assert_eq!(packet.submission_attempt, Some(2));
    let first_retained = state
        .read_retained_review_attempt(&parent, &delegation, 1)
        .unwrap()
        .map(|(_, id)| id);

    // The packet names an attempt that is no longer the current one.
    packet.submission_attempt = Some(1);
    packet
        .retained_review_attempts
        .retain(|identity| identity.submission_attempt != 1);
    let before = changes(&state);
    state.publish_terminal_review_for_packet(&parent, &delegation, &mut packet);
    assert_eq!(
        changes(&state),
        before,
        "no custody write for a non-current attempt"
    );
    assert!(packet.retained_review_attempts_error.is_none());
    assert!(
        packet
            .retained_review_attempts
            .iter()
            .all(|identity| identity.submission_attempt != 1)
    );
    assert_eq!(
        state
            .read_retained_review_attempt(&parent, &delegation, 1)
            .unwrap()
            .map(|(_, id)| id),
        first_retained
    );
}

#[test]
fn parent_packet_publishes_nothing_while_review_storage_is_disabled() {
    let (mut state, parent, delegation, child) =
        terminal_review("A completed review read while storage is disabled.");
    // The packet is read while storage works, so discovery reports no error
    // and only the publication's own storage check stands in its way.
    let mut packet = state.get_delegation_result(&parent, &delegation).unwrap();
    assert!(packet.retained_review_attempts_error.is_none());
    assert!(packet.retained_review_attempts.is_empty());
    let enabled = state.mailbox_store.clone();
    state.mailbox_store = Arc::new(MailboxStore::disabled_for_tests());
    state.publish_terminal_review_for_packet(&parent, &delegation, &mut packet);
    assert!(
        packet.retained_review_attempts_error.is_none(),
        "a disabled store is not attempted"
    );
    assert!(packet.retained_review_attempts.is_empty());
    assert!(
        state
            .inner
            .lock()
            .unwrap()
            .review_packet_retention_failures
            .is_empty(),
        "a disabled store is not remembered as a failed retention"
    );

    state.mailbox_store = enabled;
    assert!(
        state
            .review_custody_rows_read_only(Some(&child), &delegation)
            .unwrap()
            .is_empty(),
        "the disabled publication captured nothing"
    );
    state.publish_terminal_review_for_packet(&parent, &delegation, &mut packet);
    assert!(packet.retained_review_attempts_error.is_none());
    assert_eq!(packet.retained_review_attempts.len(), 1);
}

#[test]
fn a_packet_failure_seen_after_its_delegation_was_removed_is_not_remembered() {
    let (state, parent, delegation, _) = terminal_review("A review removed while captured.");
    let removing = state.clone();
    let removed = delegation.clone();
    TEST_REVIEW_ATTEMPT_AFTER_APPEND.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let mut inner = removing.inner.lock().unwrap();
            let index = inner.find_delegation_index(&removed).unwrap();
            inner.remove_delegation_at(index);
            false
        }));
    });

    let packet = state
        .get_delegation_result_selected(&parent, &delegation, &unselected())
        .unwrap();
    assert!(
        packet.retained_review_attempts_error.is_some(),
        "the capture of a removed delegation fails"
    );
    assert!(
        state
            .inner
            .lock()
            .unwrap()
            .review_packet_retention_failures
            .is_empty(),
        "a removed delegation keeps no remembered failure"
    );
}

/// Two overlapping packet reads of one attempt. The first waits before its
/// owner guard while the record changes and a second read fails and
/// remembers that failure. The first read then fails for its older record;
/// it must leave the newer failure in place, so a third read serves it
/// without running retention again.
#[test]
fn an_older_overlapping_failure_keeps_the_newer_remembered_failure() {
    let (state, parent, delegation, _) = terminal_review("A completed review whose cut moves.");
    let newer_completion = "2026-11-21T00:30:00Z";
    let during = state.clone();
    let (during_parent, during_delegation) = (parent.clone(), delegation.clone());
    TEST_REVIEW_PACKET_BEFORE_OWNER_GUARD.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            {
                let mut inner = during.inner.lock().unwrap();
                let index = inner.find_delegation_index(&during_delegation).unwrap();
                inner.delegations[index].completed_at = Some(newer_completion.to_owned());
            }
            fail_next_capture(&during, &during_delegation, "The newer read's cut");
            let newer = during
                .get_delegation_result_selected(&during_parent, &during_delegation, &unselected())
                .unwrap();
            assert!(
                newer.retained_review_attempts_error.is_some(),
                "the newer read fails"
            );
            // The older read's own capture fails as well.
            fail_next_capture(&during, &during_delegation, "The older read's cut");
        }));
    });

    let older = state
        .get_delegation_result_selected(&parent, &delegation, &unselected())
        .unwrap();
    assert!(
        older.retained_review_attempts_error.is_some(),
        "the older read fails"
    );
    {
        let inner = state.inner.lock().unwrap();
        let failures = &inner.review_packet_retention_failures;
        assert_eq!(failures.len(), 1, "the newer failure is still remembered");
        assert!(
            failures
                .values()
                .all(|failure| failure.record.contains(newer_completion)),
            "the remembered failure is the newer record's"
        );
    }

    let count = count_packet_retentions();
    let before = changes(&state);
    let third = state
        .get_delegation_result_selected(&parent, &delegation, &unselected())
        .unwrap();
    assert!(third.retained_review_attempts_error.is_some());
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "no retention runs for the unchanged newer record"
    );
    assert_eq!(
        changes(&state),
        before,
        "no capture for the unchanged record"
    );
}

#[test]
fn a_published_attempt_clears_its_remembered_packet_failure() {
    let (state, parent, delegation, _) = terminal_review("A completed review whose cut moves.");
    fail_next_capture(&state, &delegation, "A concurrent cut");
    let failed = state
        .get_delegation_result_selected(&parent, &delegation, &unselected())
        .unwrap();
    assert!(failed.retained_review_attempts_error.is_some());
    let remembered = |state: &AppState| {
        state
            .inner
            .lock()
            .unwrap()
            .review_packet_retention_failures
            .len()
    };
    assert_eq!(remembered(&state), 1);

    // An explicit attempt-only read publishes the attempt at once.
    let attempt_only = DelegationResultSelection {
        submission_attempt: Some(1),
        result_id: None,
    };
    state
        .get_delegation_result_selected(&parent, &delegation, &attempt_only)
        .unwrap();
    let packet = state
        .get_delegation_result_selected(&parent, &delegation, &unselected())
        .unwrap();
    assert!(packet.retained_review_attempts_error.is_none());
    assert_eq!(packet.retained_review_attempts.len(), 1);
    assert_eq!(
        remembered(&state),
        0,
        "a published attempt keeps no remembered failure"
    );
}

/// Installs a hook on the parent packet's retention path and returns how
/// many times that path has run since.
fn count_packet_retentions() -> Arc<std::sync::atomic::AtomicUsize> {
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = count.clone();
    TEST_REVIEW_PACKET_BEFORE_OWNER_GUARD.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
    });
    count
}

/// Makes the next capture fail: the terminal cut changes after its append.
fn fail_next_capture(state: &AppState, delegation: &str, summary: &str) {
    let changed = state.clone();
    let (changing, summary) = (delegation.to_owned(), summary.to_owned());
    TEST_REVIEW_ATTEMPT_AFTER_APPEND.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let mut inner = changed.inner.lock().unwrap();
            let index = inner.find_delegation_index(&changing).unwrap();
            inner.delegations[index].result.as_mut().unwrap().summary = summary;
            false
        }));
    });
}

#[test]
fn parent_packet_retries_a_failed_retention_once_per_record_change() {
    let (state, parent, delegation, _) = terminal_review("A completed review whose cut moves.");
    let packet_error = |state: &AppState| {
        state
            .get_delegation_result_selected(&parent, &delegation, &unselected())
            .unwrap()
            .retained_review_attempts_error
    };
    let remembered = |state: &AppState| {
        state
            .inner
            .lock()
            .unwrap()
            .review_packet_retention_failures
            .len()
    };
    let change_record = |state: &AppState, completed_at: &str| {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&delegation).unwrap();
        inner.delegations[index].completed_at = Some(completed_at.to_owned());
    };

    fail_next_capture(&state, &delegation, "A first concurrent cut");
    let first = packet_error(&state).expect("the first retention fails");
    assert!(first.contains("terminal cut changed"));
    assert_eq!(remembered(&state), 1);

    // Unchanged record: the remembered failure is served, nothing runs.
    let count = count_packet_retentions();
    let before = changes(&state);
    for _ in 0..3 {
        assert_eq!(packet_error(&state).as_deref(), Some(first.as_str()));
    }
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        changes(&state),
        before,
        "no capture while the record is unchanged"
    );

    // A changed record is retried once; a failure is remembered again.
    change_record(&state, "2026-11-20T18:00:00Z");
    fail_next_capture(&state, &delegation, "A second concurrent cut");
    assert!(
        packet_error(&state)
            .unwrap()
            .contains("terminal cut changed")
    );
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    let count = count_packet_retentions();
    let before = changes(&state);
    assert!(packet_error(&state).is_some());
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(changes(&state), before);
    assert_eq!(remembered(&state), 1);

    // The next change removes the entry; its retry now succeeds.
    change_record(&state, "2026-11-20T18:05:00Z");
    let packet = state
        .get_delegation_result_selected(&parent, &delegation, &unselected())
        .unwrap();
    assert!(packet.retained_review_attempts_error.is_none());
    assert_eq!(packet.retained_review_attempts.len(), 1);
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        remembered(&state),
        0,
        "a changed record identity removes the entry"
    );
}
