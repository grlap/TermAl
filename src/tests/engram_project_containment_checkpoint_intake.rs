//! Current input and mailbox provenance are independent of passive checkpoints.
use super::*;

#[test]
fn ordinary_paused_intake_preserves_exact_text_and_passive_checkpoint() {
    let (turn, original) = passive_checkpoint_turn("checkpoint-ordinary-intake");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let first = turn.enqueue("Existing ordinary FIFO predecessor", QueuedPromptSource::User);
    let text = "New ordinary input\nżółw 🐢";
    // Manual Send releases the paused User FIFO, as in ordinary dispatch:
    // the existing predecessor starts and the new exact input stays behind it.
    let dispatch = match turn.state.dispatch_turn(&turn.session, SendMessageRequest {
        text: text.into(), expanded_text: None, attachments: Vec::new(),
        source_session_id: None, source_mailbox: None,
    }).unwrap() {
        DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        _ => panic!("manual Send must start the existing User predecessor"),
    };
    turn.record(|record| {
        assert!(!record.orchestrator_auto_dispatch_blocked);
        assert_eq!(record.queued_prompts.len(), 1);
        assert_ne!(record.queued_prompts[0].pending_prompt.id, first);
        assert_eq!(record.queued_prompts[0].pending_prompt.text, text);
    });
    turn.assert_no_prompt();
    assert_passive_checkpoint(&turn, &original);
    deliver_turn_dispatch(&turn.state, dispatch).unwrap();
    assert!(turn.prompt().prompt.contains("Existing ordinary FIFO predecessor"));
    turn.assert_no_prompt();
    let token = turn.record(|record| record.runtime.runtime_token().unwrap());
    turn.state.finish_turn_ok_if_runtime_matches(&turn.session, &token).unwrap();
    assert!(turn.prompt().prompt.contains(text));
    turn.record(|record| assert!(record.queued_prompts.is_empty()));
    turn.assert_no_prompt();
    assert_passive_checkpoint(&turn, &original);
}

#[test]
fn ordinary_resume_skips_unknown_original_handoff_and_delivers_only_new_input() {
    let (turn, original) = passive_checkpoint_turn("checkpoint-unknown-handoff");
    let old = turn.enqueue("Possibly sent original content", QueuedPromptSource::User);
    turn.edit(|record| {
        record.queued_prompts[0].engram_ordinary_handoff = true;
        record.queued_prompts[0].promotion_disposition_known = false;
        record.set_auto_dispatch_blocked(true);
    });
    let old_row = turn.record(|record| serde_json::to_value(&record.queued_prompts[0]).unwrap());
    turn.enqueue("Different fresh ordinary input", QueuedPromptSource::User);
    turn.state.resume_session_queue(&turn.session).unwrap();
    let delivered = turn.prompt();
    assert!(delivered.prompt.contains("Different fresh ordinary input"));
    assert!(!delivered.prompt.contains("Possibly sent original content"));
    turn.assert_no_prompt();
    turn.record(|record| {
        let retained = record.queued_prompts.iter().find(|row| row.pending_prompt.id == old).unwrap();
        assert_eq!(serde_json::to_value(retained).unwrap(), old_row);
    });
    assert_passive_checkpoint(&turn, &original);
}

#[test]
fn ordinary_mailbox_coalescing_keeps_provenance_order_pause_and_history_through_reload() {
    let (turn, original) = passive_checkpoint_turn("checkpoint-mailbox-provenance");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    for sequence in [9, 7] {
        assert!(matches!(turn.state.dispatch_turn(&turn.session, SendMessageRequest {
            text: format!("Mailbox wake {sequence}"), expanded_text: None, attachments: Vec::new(),
            source_session_id: Some(turn.session.clone()), source_mailbox: Some(MailboxMessageSource {
                mailbox_id: "checkpoint-mailbox".into(), message_id: format!("wake-{sequence}"),
                sequence, unread_count: sequence,
            }),
        }).unwrap(), DispatchTurnResult::Queued));
    }
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.session).unwrap()];
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert_eq!(record.queued_prompts.len(), 1);
    let row = &record.queued_prompts[0];
    assert_eq!(row.source, QueuedPromptSource::Mailbox);
    assert_eq!(row.pending_prompt.text, "Mailbox wake 9");
    let source = row.pending_prompt.source.as_ref().unwrap();
    assert_eq!(source.session_id.as_deref(), Some(turn.session.as_str()));
    assert_eq!(source.mailbox.as_ref().unwrap().sequence, 9);
    assert_passive_checkpoint(&turn, &original);
    turn.assert_no_prompt();
}
