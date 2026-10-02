//! The acknowledged-continuation contract on an Engram-controlled root, the
//! configuration of every recorded incident: a message accepted mid-turn and
//! acknowledged in that turn makes the turn-end checkpoint say `continue`,
//! and the continuation wake passes through ordinary queued admission.
//!
//! Owns: that one end-to-end path through Engram admission. Does not own the
//! contract itself, which `tests/mailbox_acknowledged_wake.rs` pins without
//! Engram. Split out of `engram_root_dispatch.rs` to keep that file from
//! growing; it reuses that module's root fixture.

use super::super::super::mailbox_acknowledged_wake::{
    assert_continuation_text, read_and_acknowledge, send,
};
use super::*;

// On master the acknowledgement deleted the queued wake, the checkpoint said
// `wait` and no further operation reached Engram or the runtime.
#[tokio::test]
async fn engram_root_acknowledged_mid_turn_mail_starts_a_continuation_through_admission() {
    let (base, session, receiver, transport) = root_fixture([
        bind_reply("root-token"),
        grant_reply("first"),
        begin_reply("first"),
        checkpoint_reply("first"),
        grant_reply("continuation"),
        begin_reply("continuation"),
    ]);
    let coordination_path = resolve_coordination_persistence_path(base.persistence_path.as_ref());
    let state = AppState {
        mailbox_store: Arc::new(
            MailboxStore::open(&coordination_path).expect("mailbox test store should open"),
        ),
        ..base
    };
    let sender = test_session_id(&state, Agent::Claude);

    let first = root_dispatch(&state, &session, false);
    deliver_turn_dispatch(&state, first).unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));

    let receipt = send(&state, &sender, &session, "engram-mid-turn");
    assert_eq!(receipt.notification_disposition, "queuedBehindActiveTurn");
    read_and_acknowledge(&state, &session, &receipt.mailbox_id).await;

    let token = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .runtime
            .runtime_token()
            .unwrap()
    };
    state
        .finish_turn_ok_if_runtime_matches(&session, &token)
        .expect("the turn should finish and drain the continuation");
    let CodexRuntimeCommand::Prompt { command, .. } = receiver
        .try_recv()
        .expect("the continuation must reach the runtime")
    else {
        panic!("expected the continuation prompt");
    };
    assert_continuation_text(&command.prompt, &receipt.mailbox_id, receipt.sequence);

    let requests = transport.requests();
    let checkpoint = requests
        .iter()
        .find(|request| request.request["operation"] == "turn_checkpoint")
        .expect("the first turn should checkpoint");
    assert_eq!(
        checkpoint.request["next_intent"], "continue",
        "an owed wake makes the turn end say continue, not wait"
    );
    assert_eq!(
        operations(&transport),
        [
            "session_bind",
            "turn_evaluate",
            "turn_begin",
            "turn_checkpoint",
            "turn_evaluate",
            "turn_begin"
        ],
        "the continuation passes through ordinary admission exactly once"
    );
}
