//! Parent-packet discovery supplies immutable selections without widening reads.
//! Uses the existing archive fixture; no provider or live operator store is read.
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
