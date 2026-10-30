// Owns: the regression for recovering an original closing checkpoint after
// the work authority it was granted under moved. A turn's closing checkpoint
// reply is lost; then the session's bound claim is released, or replaced by a
// different claim and fence. The producer settles the original against its
// stored control-session binding, so the exact retained request must still be
// sent once, and only a checkpointed receipt retires the duty. A refused,
// busy or expired replay keeps the duty and its exclusion.
// Does not own: the recovery schedule (engram_checkpoint_result_recovery_
// progress.rs) or waking a queued head after settlement.
// New file; a child of engram_checkpoint_result_recovery.rs, whose
// lost-checkpoint fixture it uses.

use super::*;

/// The binding as the session's held claims read after the original's own
/// claim moved: released with nothing bound, or a different claim and fence.
fn moved_bindings(original: &EngramCheckpointOperation) -> [Option<EngramControlWorkBinding>; 2] {
    let mut reclaimed = original
        .original
        .work_binding
        .clone()
        .expect("a claimed turn is bound");
    reclaimed.claim_id = "claim-held-by-another-session".to_owned();
    reclaimed.claim_fence += 1;
    [None, Some(reclaimed)]
}

/// Every later work-binding read returns `moved` instead of the original.
fn move_authority(turn: &CheckedTurn, moved: &Option<EngramControlWorkBinding>) {
    let mut reads = turn.transport.work_bindings.lock().unwrap();
    reads.clear();
    reads.extend((0..8).map(|_| Ok(moved.clone())));
}

/// The production tick: its first pass schedules the existing delay and the
/// pass after it runs the due recovery.
fn tick_through_first_recovery(turn: &CheckedTurn) {
    let now = chrono::Utc::now();
    turn.state.engram_abort_retry_tick(now);
    turn.state.engram_abort_retry_tick(now + chrono::Duration::seconds(3));
}

#[test]
fn an_original_checkpoint_whose_claim_moved_is_replayed_exactly_and_settles() {
    for (case, label) in [(0, "moved-released"), (1, "moved-reclaimed")] {
        let (turn, original) = lost_checkpoint(label, vec![checkpoint_reply(CHECK_GRANT)]);
        move_authority(&turn, &moved_bindings(&original)[case]);
        tick_through_first_recovery(&turn);
        assert_eq!(
            checkpoints(&turn).len(),
            2,
            "{label}: the exact original must be sent once; a moved claim must not withhold it"
        );
        assert_recovered(&turn, &original);
    }
}

#[test]
fn an_original_checkpoint_whose_claim_moved_keeps_its_duty_when_the_replay_is_not_accepted() {
    for (label, reply) in [
        ("moved-refused", checkpoint_refusal_reply("grant_scope_mismatch")),
        ("moved-busy", remote_error_reply("control_store_busy")),
        ("moved-expired", remote_error_reply("request_expired")),
    ] {
        let (turn, original) = lost_checkpoint(label, vec![reply]);
        move_authority(&turn, &moved_bindings(&original)[1]);
        let queued = turn
            .state
            .dispatch_turn(
                &turn.session_id,
                SendMessageRequest {
                    text: "An ordinary prompt behind the checkpoint.".to_owned(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
            .expect("the send is accepted");
        assert!(matches!(queued, DispatchTurnResult::Queued), "{label}: the prompt queues");
        tick_through_first_recovery(&turn);
        let sent = checkpoints(&turn);
        assert_eq!(sent.len(), 2, "{label}: the exact original is replayed once");
        assert_eq!(sent[0], sent[1], "{label}: the replay is the exact original request");
        let inner = turn.state.inner.lock().unwrap();
        assert!(
            !inner.engram_confirmed_containment_ids.contains(&original.original.id),
            "{label}: no receipt settles the duty"
        );
        assert!(
            engram_session_obligation_locked(&inner, &turn.session_id).is_some(),
            "{label}: the original duty stays and keeps excluding admission"
        );
        let canonical = inner
            .engram_session_obligations
            .checkpoint_operations
            .iter()
            .find(|operation| operation.original.id == original.original.id)
            .expect("the original duty is retained");
        assert!(
            !matches!(canonical.progress, EngramExistingCheckpointProgress::Accepted { .. }),
            "{label}: nothing is recorded as accepted"
        );
        assert!(canonical.has_uncertain_checkpoint_history(), "{label}: the earlier Unknown is not certified");
        let record = &inner.sessions[inner.find_session_index(&turn.session_id).unwrap()];
        assert_eq!(record.queued_prompts.len(), 1, "{label}: the queued prompt is not admitted");
        drop(inner);
        assert!(turn._runtime_rx.try_recv().is_err(), "{label}: nothing reached the provider");
    }
}
