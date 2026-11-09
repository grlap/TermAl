//! Acknowledged mid-turn mail still owes one ordinary continuation.
//! Uses the actual mailbox receipt and turn-completion owners; observations
//! neither authorize nor suppress that continuation.
use super::super::super::mailbox_acknowledged_wake::{
    assert_continuation_text, read_and_acknowledge, send,
};
use super::*;
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;

#[tokio::test]
async fn ordinary_root_acknowledged_mid_turn_mail_starts_one_continuation_without_admission() {
    let mut turn = OrdinaryRoot::new("ordinary-acknowledged-mid-turn-mail");
    let path = resolve_coordination_persistence_path(turn.state.persistence_path.as_ref());
    turn.state.mailbox_store = Arc::new(MailboxStore::open(&path).unwrap());
    let sender = test_session_id(&turn.state, Agent::Claude);
    deliver_turn_dispatch(&turn.state, turn.prepare("First ordinary turn.")).unwrap();
    assert!(turn.prompt().prompt.contains("First ordinary turn."));

    let receipt = send(&turn.state, &sender, &turn.session, "ordinary-mid-turn");
    assert_eq!(receipt.notification_disposition, "queuedBehindActiveTurn");
    read_and_acknowledge(&turn.state, &turn.session, &receipt.mailbox_id).await;
    let token = turn.record(|record| record.runtime.runtime_token().unwrap());
    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.session, &token)
        .unwrap();
    let command = turn.prompt();
    assert_continuation_text(&command.prompt, &receipt.mailbox_id, receipt.sequence);
    turn.assert_no_prompt();
    turn.assert_no_control();

    // The fulfilled boundary cannot produce another wake on the next finish.
    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.session, &token)
        .unwrap();
    turn.assert_no_prompt();
    turn.record(|record| {
        assert!(record.queued_prompts.is_empty());
        assert_eq!(record.session.status, SessionStatus::Idle);
    });
    turn.assert_no_control();
}
