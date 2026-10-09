//! Crash drill: sessions the store holds mid-turn when the process dies.
//!
//! The persist worker writes a session's status as it changes, so a crash,
//! kill or power loss can leave rows stored as `Active` or `Approval` with no
//! process behind them. These tests write such a store directly, with no
//! shutdown step, and boot it the way a restart after a crash does. Boot must
//! end every interrupted turn: the sessions come back as errors with no
//! runtime, a pending request is marked interrupted, each gets one restart
//! notice, a queued prompt is kept but not sent, and neither session stays
//! busy, so a later prompt or mailbox wake is not queued behind a turn that no
//! longer exists.

use super::*;

const RESTART_NOTICE_PREFIX: &str = "TermAl restarted";

fn crash_drill_queued_prompt(prompt_id: &str) -> QueuedPromptRecord {
    QueuedPromptRecord {
        engram_waiting: false,
        promoted_message_index: None,
        promotion_disposition_known: true,
        engram_bind: None,
        engram_evaluate: None,
        engram_interrupted: false,
        source: QueuedPromptSource::User,
        attachments: Vec::new(),
        pending_prompt: PendingPrompt {
            engram_interrupted: false,
            is_engram_retained: false,
            attachments: Vec::new(),
            id: prompt_id.to_owned(),
            timestamp: stamp_now(),
            text: "queued before the crash".to_owned(),
            expanded_text: None,
            source: None,
        },
    }
}

fn crash_drill_pending_approval(message_id: String) -> Message {
    Message::Approval {
        id: message_id,
        timestamp: stamp_now(),
        author: Author::Assistant,
        title: "Claude needs approval".to_owned(),
        command: "Edit src/main.rs".to_owned(),
        command_language: None,
        detail: "Allow editing src/main.rs?".to_owned(),
        decision: ApprovalDecision::Pending,
        supported_decisions: None,
    }
}

fn restart_notice_count(record: &SessionRecord) -> usize {
    record
        .session
        .messages
        .iter()
        .filter(|message| {
            matches!(
                message,
                Message::Text { author: Author::Assistant, text, .. }
                    if text.starts_with(RESTART_NOTICE_PREFIX)
            )
        })
        .count()
}

/// Mirrors the part of `dispatch_turn`'s busy check that an interrupted turn
/// can leave behind: a running status or a stop in progress. Its other
/// reasons to queue (a project reset fence, prompts already queued, a blocked
/// automatic prompt) do not come from the interrupted turn, so they are not
/// what this drill is about.
fn counts_as_busy(record: &SessionRecord) -> bool {
    matches!(
        record.session.status,
        SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
    ) || record.runtime_stop_in_progress
}

#[test]
fn crash_restart_ends_turns_stored_mid_flight_without_dispatching_their_queue() {
    let root = TestTempRoot::create("termal-crash-drill");
    let workdir = root.path().to_string_lossy().into_owned();
    let persistence_path = root.path().join("termal.sqlite");
    let templates_path = root.path().join("orchestrators.json");

    // The store as the persist worker left it when the process died: one
    // session streaming a turn, one waiting on an approval with a prompt
    // queued behind it.
    let mut inner = StateInner::new();
    let active_id = inner
        .create_session(Agent::Claude, None, workdir.clone(), None, None)
        .session
        .id
        .clone();
    let approval_id = inner
        .create_session(Agent::Claude, None, workdir.clone(), None, None)
        .session
        .id
        .clone();
    let active_index = inner
        .find_session_index(&active_id)
        .expect("active session should exist");
    inner.sessions[active_index].session.status = SessionStatus::Active;
    let approval_message_id = inner.next_message_id();
    let approval_index = inner
        .find_session_index(&approval_id)
        .expect("approval session should exist");
    {
        let record = inner
            .session_mut_by_index(approval_index)
            .expect("approval session index should be valid");
        push_message_on_record(
            record,
            crash_drill_pending_approval(approval_message_id.clone()),
        );
        record.session.status = SessionStatus::Approval;
        record
            .queued_prompts
            .push_back(crash_drill_queued_prompt("queued-before-crash"));
        sync_pending_prompts(record);
    }
    persist_state(&persistence_path, &inner).expect("the mid-turn store should persist");
    drop(inner);

    let state = AppState::new_with_paths(workdir, persistence_path, templates_path)
        .expect("a crash restart should boot the mid-turn store");
    // Join the background workers before asserting, so a failed assertion
    // cannot leave them running with the store open. The booted state stays
    // readable afterwards.
    state.shutdown_persist_blocking();

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        for session_id in [&active_id, &approval_id] {
            let index = inner
                .find_session_index(session_id)
                .expect("the session should survive the restart");
            let record = &inner.sessions[index];
            assert_eq!(
                record.session.status,
                SessionStatus::Error,
                "{session_id} must not load as a running turn"
            );
            assert!(
                matches!(record.runtime, SessionRuntime::None),
                "{session_id} must have no runtime after the restart"
            );
            assert_eq!(
                restart_notice_count(record),
                1,
                "{session_id} must carry exactly one restart notice"
            );
            assert!(
                !counts_as_busy(record),
                "{session_id} must not count as busy, or a later prompt or mailbox wake would queue behind the interrupted turn"
            );
        }

        let approval = &inner.sessions[inner
            .find_session_index(&approval_id)
            .expect("approval session should survive the restart")];
        let decision = approval
            .session
            .messages
            .iter()
            .find_map(|message| match message {
                Message::Approval { id, decision, .. } if *id == approval_message_id => {
                    Some(decision.clone())
                }
                _ => None,
            })
            .expect("the approval request should still be in the transcript");
        assert_eq!(decision, ApprovalDecision::Interrupted);
        let queued = approval
            .queued_prompts
            .iter()
            .map(|prompt| prompt.pending_prompt.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            queued,
            vec!["queued-before-crash"],
            "the queued prompt must be kept, not dispatched or dropped"
        );
    }
}
