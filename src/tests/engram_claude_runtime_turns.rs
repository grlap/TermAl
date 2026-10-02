// Owns the tests of turns Claude Code starts by itself
// (src/claude_runtime_turns.rs, src/claude_turn_ownership.rs) at the host's
// real choke points: an idle session adopts such a turn (Active, a new turn
// generation, prompts queued behind it, an immediate notice); a turn the
// runtime starts beside a granted turn neither ends that turn nor reports
// into its grant; and every result finalizes only the turn that owns it.
// Frames are fed exactly as the Claude stdout reader feeds them. Also owns
// the continuity tests (src/engram_turn_continuity.rs): a change between two
// turns kept as drift beside the next turn's own measured begin.
// Does not own the frame-ownership rules themselves
// (src/tests/claude_turn_ownership.rs) or the in-turn check tests
// (src/tests/engram_turn_checks.rs, whose `CheckedTurn` fixture this child
// module uses). New module.
use super::*;

fn task_notification() -> Value {
    json!({"type": "system", "subtype": "task_notification", "task_id": "b1",
        "tool_use_id": "toolu-b1", "status": "completed"})
}

fn init() -> Value {
    json!({"type": "system", "subtype": "init", "session_id": "probe"})
}

fn assistant_text(text: &str) -> Value {
    json!({"type": "assistant", "message": {"content": [{"type": "text", "text": text}]}})
}

fn echo(prompt: &ClaudePromptCommand) -> Value {
    json!({"type": "user", "message": {"role": "user",
        "content": claude_prompt_content(prompt)}, "isReplay": true})
}

fn result(is_error: bool) -> Value {
    json!({"type": "result", "subtype": if is_error { "error" } else { "success" },
        "is_error": is_error, "result": "done"})
}

/// Feeds one frame as the Claude stdout reader does.
fn feed(
    state: &AppState,
    session_id: &str,
    token: &RuntimeToken,
    ownership: &ClaudeTurnOwnership,
    frame: Value,
) {
    if frame.get("type").and_then(Value::as_str) == Some("result") {
        let owner = lock_claude_turn_ownership(ownership).close_for_result(&frame);
        let error = frame.get("is_error").and_then(Value::as_bool) == Some(true);
        state
            .finish_claude_result(session_id, token, owner, error.then_some("the turn failed"))
            .expect("the result should be routed");
    } else {
        state.observe_claude_frame_ownership(session_id, token, ownership, &frame);
    }
}

fn host_prompt(text: &str, turn_generation: u64) -> ClaudePromptCommand {
    ClaudePromptCommand {
        attachments: Vec::new(),
        replay_generation: format!("replay-{turn_generation}"),
        text: text.to_owned(),
        turn_generation,
    }
}

fn system_notices(record: &SessionRecord) -> Vec<String> {
    record
        .session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Text {
                author: Author::System,
                text,
                ..
            } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn checkpoint_requests(turn: &CheckedTurn) -> Vec<Value> {
    turn.transport
        .requests()
        .into_iter()
        .filter(|request| request.request["operation"] == "turn_checkpoint")
        .map(|request| request.request)
        .collect()
}

#[test]
fn a_runtime_started_turn_beside_a_granted_turn_never_ends_it_or_reports_into_its_grant() {
    // The probed race: TermAl's prompt is written while a background-task
    // notice starts a turn of the runtime's own. The notice turn opens first;
    // the granted turn waits for its prompt's own echo.
    let turn = CheckedTurn::start("runtime-beside-granted", true);
    let token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    let generation = turn.record(|record| record.active_turn_generation);
    let ownership = new_claude_turn_ownership();
    let prompt = host_prompt("Run the checks.", generation);
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&prompt));

    for frame in [
        task_notification(),
        init(),
        assistant_text("the background run finished"),
    ] {
        feed(&turn.state, &turn.session_id, &token, &ownership, frame);
    }
    turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Active);
        assert_eq!(
            record.active_turn_generation, generation,
            "a busy session's turn is not replaced"
        );
        assert_eq!(
            record
                .unmediated_claude_turn
                .as_ref()
                .map(|turn| turn.adopted_generation),
            Some(None)
        );
        let notices = system_notices(record);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(
            notices[0].contains("after a background-task notice")
                && notices[0].contains("TermAl did not mediate it")
                && notices[0].contains("not part of it"),
            "{}",
            notices[0]
        );
    });

    // A test the runtime runs in its own turn reaches no grant; it is kept on
    // the session as unassigned, under the notice that announced the turn.
    turn.run("runtime-test", SIZE_TEST, EngramCommandExit::Code(0), || {});
    assert!(
        turn.record(|record| record.engram.active_turn_checks.is_empty()),
        "a runtime-started turn's test is not reported into the granted turn"
    );
    turn.record(|record| {
        let notice_id = record
            .unmediated_claude_turn
            .as_ref()
            .map(|turn| turn.notice_message_id.clone())
            .expect("the turn is open");
        let kept = record
            .unassigned_claude_observations
            .iter()
            .filter(|observation| observation.notice_message_id == notice_id)
            .map(|observation| observation.what.clone())
            .collect::<Vec<_>>();
        assert!(
            kept.iter()
                .any(|what| what.contains("started") && what.contains(SIZE_TEST))
                && kept.iter().any(|what| what.contains("finished")),
            "{kept:?}"
        );
    });

    // The prompt's echo inside the runtime's turn moves nothing, and the
    // runtime turn's failing result neither ends nor fails the granted turn.
    feed(
        &turn.state,
        &turn.session_id,
        &token,
        &ownership,
        echo(&prompt),
    );
    feed(
        &turn.state,
        &turn.session_id,
        &token,
        &ownership,
        result(true),
    );
    turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Active);
        assert_eq!(record.active_turn_generation, generation);
        assert_eq!(record.unmediated_claude_turn, None);
        assert_eq!(
            record.engram.active_grant_id.as_deref(),
            Some(CHECK_GRANT),
            "the granted turn still holds its grant"
        );
    });
    assert!(
        checkpoint_requests(&turn).is_empty(),
        "nothing was checkpointed by the runtime's result"
    );

    // The granted turn opens on its own echo; its test is reported, and its
    // result closes it with only its own evidence.
    feed(&turn.state, &turn.session_id, &token, &ownership, init());
    feed(
        &turn.state,
        &turn.session_id,
        &token,
        &ownership,
        echo(&prompt),
    );
    turn.run("host-test", SIZE_TEST, EngramCommandExit::Code(0), || {});
    assert_eq!(
        turn.record(|record| record.engram.active_turn_checks.len()),
        1
    );
    feed(
        &turn.state,
        &turn.session_id,
        &token,
        &ownership,
        result(false),
    );
    assert_eq!(
        turn.record(|record| record.session.status),
        SessionStatus::Idle
    );
    let checkpoints = checkpoint_requests(&turn);
    assert_eq!(checkpoints.len(), 1);
    assert_eq!(
        checkpoints[0]["verification_evidence"]
            .as_array()
            .map(Vec::len),
        Some(1),
        "{:#}",
        checkpoints[0]
    );
}

#[test]
fn an_edit_by_a_turn_beside_a_granted_turn_leaves_the_grants_source_report_withheld_as_uncertain() {
    // The granted turn's begin basis was taken before the notice turn ran.
    // Nothing proves where that turn ended and the granted prompt's own turn
    // began, so the edit is neither the grant's change nor provably not: the
    // begin is kept as measured, and the grant reports no source observation,
    // neither a clean no-change turn nor the whole difference as its own.
    let turn = CheckedTurn::start("runtime-beside-edit", true);
    let token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    let generation = turn.record(|record| record.active_turn_generation);
    let begun = turn.revision();
    assert_eq!(
        turn.record(|record| record
            .engram
            .active_turn_start_basis
            .as_ref()
            .map(|basis| basis.source_revision.clone())),
        Some(begun.clone())
    );
    let ownership = new_claude_turn_ownership();
    let prompt = host_prompt("Run the checks.", generation);
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&prompt));

    for frame in [task_notification(), init(), assistant_text("editing")] {
        feed(&turn.state, &turn.session_id, &token, &ownership, frame);
    }
    turn.change_readme("edited by the turn no prompt owns\n");
    assert_ne!(turn.revision(), begun);
    feed(
        &turn.state,
        &turn.session_id,
        &token,
        &ownership,
        result(false),
    );
    turn.record(|record| {
        assert_eq!(
            record
                .engram
                .active_turn_start_basis
                .as_ref()
                .map(|basis| basis.source_revision.clone()),
            Some(begun.clone()),
            "the grant's measured begin is kept"
        );
        assert_eq!(
            record.engram.active_turn_mixed_attribution.as_deref(),
            Some(CHECK_GRANT)
        );
        assert!(
            record
                .unassigned_claude_observations
                .iter()
                .any(|observation| observation.what.contains(CHECK_GRANT)
                    && observation.what.contains("withheld as uncertain")),
            "{:?}",
            record.unassigned_claude_observations
        );
    });

    feed(&turn.state, &turn.session_id, &token, &ownership, init());
    feed(
        &turn.state,
        &turn.session_id,
        &token,
        &ownership,
        echo(&prompt),
    );
    feed(
        &turn.state,
        &turn.session_id,
        &token,
        &ownership,
        result(false),
    );
    let checkpoints = checkpoint_requests(&turn);
    assert_eq!(checkpoints.len(), 1, "the grant is still closed");
    assert!(
        checkpoints[0]
            .get("observations")
            .and_then(Value::as_array)
            .is_none_or(|observations| observations.is_empty()),
        "no source observation, clean or attributed, is reported for the mixed grant: {:#}",
        checkpoints[0]
    );
}

#[test]
fn an_adopted_runtime_started_turn_reports_nothing_and_never_closes_a_leftover_grant() {
    let turn = CheckedTurn::start("runtime-adopted", true);
    turn.finish();
    // A grant left open on the session (a degraded earlier checkpoint) must
    // not be closed as if the runtime's own turn had run under it.
    turn.record_mut(|record| record.engram.active_grant_id = Some("leftover-grant".to_owned()));
    let token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    let before = turn.record(|record| record.active_turn_generation);
    let ownership = new_claude_turn_ownership();

    for frame in [task_notification(), init(), assistant_text("noticed")] {
        feed(&turn.state, &turn.session_id, &token, &ownership, frame);
    }
    turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Active);
        assert_eq!(record.active_turn_generation, before + 1);
        assert_eq!(
            record
                .unmediated_claude_turn
                .as_ref()
                .map(|turn| turn.adopted_generation),
            Some(Some(before + 1))
        );
        let notices = system_notices(record);
        assert!(
            notices
                .last()
                .is_some_and(|notice| notice.contains("Prompts sent meanwhile wait")
                    && !notice.contains("not part of it")),
            "{notices:?}"
        );
    });
    {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        for token in [Some(&token), None] {
            assert!(
                AppState::engram_checkpoint_grant_locked(
                    &inner,
                    &turn.session_id,
                    EngramCheckpointPurpose::TurnTerminal,
                    token,
                    Some(before + 1),
                    None
                )
                .is_none(),
                "no terminal path of the adopted turn, with a token or the stop fence, \
                 checkpoints a grant (token: {})",
                token.is_some()
            );
        }
        for purpose in [
            EngramCheckpointPurpose::Settlement,
            EngramCheckpointPurpose::Teardown,
        ] {
            assert!(
                AppState::engram_checkpoint_grant_locked(
                    &inner,
                    &turn.session_id,
                    purpose,
                    None,
                    None,
                    None
                )
                .is_some_and(|(_, grant_id, _, resolved)| grant_id == "leftover-grant"
                    && resolved == EngramCheckpointPurpose::Settlement),
                "an independent settlement can still close the leftover grant, as a \
                 settlement ({purpose:?})"
            );
        }
    }
    turn.run("runtime-test", SIZE_TEST, EngramCommandExit::Code(0), || {});
    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));

    feed(
        &turn.state,
        &turn.session_id,
        &token,
        &ownership,
        result(false),
    );
    turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Idle);
        assert_eq!(record.unmediated_claude_turn, None);
        assert_eq!(
            record.engram.active_grant_id.as_deref(),
            Some("leftover-grant"),
            "the leftover grant was not checkpointed by the runtime's turn"
        );
    });
    assert_eq!(
        checkpoint_requests(&turn).len(),
        1,
        "only the dispatched turn was ever checkpointed"
    );
}

const CONTINUITY_NEXT_GRANT: &str = "turn-continuity-next-grant";

/// A claimed root whose first turn is begun and whose second Engram will
/// grant, begin and checkpoint.
fn two_turn_root(label: &str) -> CheckedTurn {
    CheckedTurn::start_with_turns(
        label,
        true,
        None,
        vec![
            checkpoint_reply(CHECK_GRANT),
            grant_reply(CONTINUITY_NEXT_GRANT),
            begin_reply(CONTINUITY_NEXT_GRANT),
            checkpoint_reply(CONTINUITY_NEXT_GRANT),
        ],
        2,
    )
}

fn begin_next_turn(turn: &CheckedTurn) {
    let dispatch = match send(&turn.state, &turn.session_id, "The next turn.") {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("an idle root should dispatch"),
    };
    deliver_turn_dispatch(&turn.state, dispatch).expect("the next turn should be delivered");
}

fn continuity(turn: &CheckedTurn) -> Option<(String, EngramTurnContinuity)> {
    turn.record(|record| record.engram.active_turn_continuity.clone())
}

#[test]
fn a_change_between_two_turns_is_kept_as_drift_apart_from_the_next_turns_own_begin() {
    let turn = two_turn_root("continuity-drift");
    assert_eq!(
        continuity(&turn),
        Some((
            CHECK_GRANT.to_owned(),
            EngramTurnContinuity::NotCompared(EngramContinuityGap::NoPreviousCheckpoint)
        )),
        "the first turn has nothing to compare with, and says so"
    );
    let left = turn.revision();
    turn.finish();
    let anchor = turn
        .record(|record| record.engram.continuity_anchor.clone())
        .expect("the acknowledged checkpoint leaves an anchor");
    assert_eq!(anchor.grant_id, CHECK_GRANT);
    assert_eq!(anchor.source_revision, left);

    // Something no turn reports writes between the turns: another session,
    // a turn the runtime started by itself, the naming turn.
    turn.change_readme("written between the turns\n");
    let changed = turn.revision();
    assert_ne!(changed, left);
    begin_next_turn(&turn);

    let (grant_id, recorded) = continuity(&turn).expect("the next turn's continuity is kept");
    assert_eq!(grant_id, CONTINUITY_NEXT_GRANT);
    let EngramTurnContinuity::Drifted {
        previous_grant_id,
        workspace_id,
        previous_revision,
        begin_revision,
        ..
    } = recorded
    else {
        panic!("expected drift, got {recorded:?}");
    };
    assert_eq!(previous_grant_id, CHECK_GRANT);
    assert_eq!(workspace_id, anchor.workspace_id);
    assert_eq!(previous_revision, left);
    assert_eq!(begin_revision, changed);
    assert_eq!(
        turn.record(|record| record
            .engram
            .active_turn_start_basis
            .as_ref()
            .map(|basis| basis.source_revision.clone())),
        Some(changed.clone()),
        "the next turn's begin stays the measured one, not the previous checkpoint's"
    );

    // The next turn edits nothing: it reports no change of its own, and the
    // drift stays recorded beside that report.
    let runtime_token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.session_id, &runtime_token)
        .expect("the next turn should complete");
    let checkpoints = checkpoint_requests(&turn);
    assert_eq!(checkpoints.len(), 2);
    assert_eq!(
        checkpoints[1]["observations"][0]["source_changed"], false,
        "{:#}",
        checkpoints[1]
    );
    assert_eq!(
        turn.record(|record| record
            .engram
            .continuity_anchor
            .as_ref()
            .map(|anchor| (anchor.grant_id.clone(), anchor.source_revision.clone()))),
        Some((CONTINUITY_NEXT_GRANT.to_owned(), changed)),
        "the anchor moves only with an acknowledged checkpoint"
    );
}

#[test]
fn an_unchanged_workspace_between_two_turns_shows_no_drift() {
    let turn = two_turn_root("continuity-unchanged");
    turn.finish();
    begin_next_turn(&turn);
    assert_eq!(
        continuity(&turn),
        Some((
            CONTINUITY_NEXT_GRANT.to_owned(),
            EngramTurnContinuity::Unchanged {
                previous_grant_id: CHECK_GRANT.to_owned()
            }
        ))
    );
}

#[test]
fn continuity_is_compared_only_within_one_binding_and_workspace() {
    let binding = test_control_work_binding("continuity-binding", 1);
    let anchor = EngramContinuityAnchor {
        grant_id: "previous".to_owned(),
        workspace_id: "C:/work/root".to_owned(),
        source_revision: "content-v1:aaa".to_owned(),
        observed_at: None,
        work_id: binding.work_id.clone(),
        run_id: binding.run_id.clone(),
        claim_id: binding.claim_id.clone(),
    };
    let begin = |workspace: &str, revision: &str| EngramExecutionSourceBasis {
        workspace_id: workspace.to_owned(),
        source_revision: revision.to_owned(),
    };
    let other_run = EngramControlWorkBinding {
        run_id: format!("{}-next", binding.run_id),
        ..binding.clone()
    };
    assert_eq!(
        engram_turn_continuity(
            Some(&anchor),
            &other_run,
            Some(&begin("C:/work/root", "x")),
            "t"
        ),
        EngramTurnContinuity::NotCompared(EngramContinuityGap::BindingChanged)
    );
    assert_eq!(
        engram_turn_continuity(
            Some(&anchor),
            &binding,
            Some(&begin("C:/work/other", "x")),
            "t"
        ),
        EngramTurnContinuity::NotCompared(EngramContinuityGap::WorkspaceChanged)
    );
    assert_eq!(
        engram_turn_continuity(Some(&anchor), &binding, None, "t"),
        EngramTurnContinuity::NotCompared(EngramContinuityGap::BeginUnmeasured)
    );
    assert_eq!(
        engram_turn_continuity(None, &binding, Some(&begin("C:/work/root", "x")), "t"),
        EngramTurnContinuity::NotCompared(EngramContinuityGap::NoPreviousCheckpoint)
    );
}

/// An idle Claude session with a fake persistent runtime, no Engram.
fn idle_claude_session(
    label: &str,
) -> (
    AppState,
    String,
    RuntimeToken,
    mpsc::Receiver<ClaudeRuntimeCommand>,
) {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (handle, runtime_rx) = test_claude_runtime_handle(label);
    let token = RuntimeToken::Claude(handle.runtime_id.clone());
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(handle);
        inner.sessions[index].session.status = SessionStatus::Idle;
    }
    (state, session_id, token, runtime_rx)
}

fn session_record<T>(
    state: &AppState,
    session_id: &str,
    read: impl FnOnce(&SessionRecord) -> T,
) -> T {
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(session_id)
        .expect("session should exist");
    read(&inner.sessions[index])
}

fn send(state: &AppState, session_id: &str, text: &str) -> DispatchTurnResult {
    state
        .dispatch_turn(
            session_id,
            SendMessageRequest {
                text: text.to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .expect("the prompt should be accepted")
}

#[test]
fn prompts_sent_during_an_adopted_runtime_started_turn_queue_behind_it() {
    let (state, session_id, token, runtime_rx) = idle_claude_session("runtime-turn-queue");
    let ownership = new_claude_turn_ownership();
    for frame in [task_notification(), init(), assistant_text("noticed")] {
        feed(&state, &session_id, &token, &ownership, frame);
    }
    let adopted = session_record(&state, &session_id, |record| {
        assert_eq!(record.session.status, SessionStatus::Active);
        record.active_turn_generation
    });
    assert!(matches!(
        send(&state, &session_id, "sent while the runtime's turn runs"),
        DispatchTurnResult::Queued
    ));
    assert!(
        runtime_rx.try_recv().is_err(),
        "nothing is written to the runtime while its own turn runs"
    );

    feed(&state, &session_id, &token, &ownership, result(false));
    let ClaudeRuntimeCommand::Prompt(prompt) = receive(
        &runtime_rx,
        "the queued prompt is dispatched after the runtime's turn",
    ) else {
        panic!("expected the queued prompt");
    };
    assert!(prompt.text.contains("sent while the runtime's turn runs"));
    assert!(
        prompt.turn_generation > adopted,
        "the queued prompt runs under a turn of its own"
    );
    session_record(&state, &session_id, |record| {
        assert_eq!(record.session.status, SessionStatus::Active);
        assert_eq!(record.active_turn_generation, prompt.turn_generation);
        assert_eq!(
            record.unmediated_claude_turn, None,
            "the runtime's turn is over"
        );
    });
}

#[test]
fn a_turn_with_no_echo_while_a_prompt_waits_is_surfaced_and_finalizes_nothing() {
    let (state, session_id, token, _runtime_rx) = idle_claude_session("runtime-turn-unassigned");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).expect("session");
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].active_turn_generation = 7;
    }
    let ownership = new_claude_turn_ownership();
    let prompt = host_prompt("Do the work.", 7);
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&prompt));

    for frame in [
        init(),
        assistant_text("an answer nothing ties to the prompt"),
    ] {
        feed(&state, &session_id, &token, &ownership, frame);
    }
    session_record(&state, &session_id, |record| {
        assert_eq!(record.session.status, SessionStatus::Active);
        assert_eq!(record.active_turn_generation, 7, "nothing is adopted");
        assert!(unmediated_claude_turn_hides_observations(record));
        let notices = system_notices(record);
        assert!(
            notices
                .last()
                .is_some_and(|notice| notice.contains("could not tie")
                    && notice.contains("finalizes no turn")),
            "{notices:?}"
        );
    });

    feed(&state, &session_id, &token, &ownership, result(false));
    session_record(&state, &session_id, |record| {
        assert_eq!(
            record.session.status,
            SessionStatus::Active,
            "the waiting prompt's turn is not finished by the unassigned one's end"
        );
        assert_eq!(record.active_turn_generation, 7);
        assert_eq!(
            record.unmediated_claude_turn, None,
            "the unassigned segment itself has ended"
        );
    });
    assert_eq!(
        lock_claude_turn_ownership(&ownership).outstanding.len(),
        1,
        "the prompt still waits for its own echo"
    );
}

#[test]
fn a_result_finalizes_only_the_turn_that_owns_it() {
    let (state, session_id, token, _runtime_rx) = idle_claude_session("runtime-turn-results");

    // An error result no turn owns leaves an idle session idle.
    state
        .finish_claude_result(
            &session_id,
            &token,
            ClaudeResultOwner::Uncorrelated {
                prompt_waiting: false,
            },
            Some("stray failure"),
        )
        .expect("an uncorrelated result is accepted");
    assert_eq!(
        session_record(&state, &session_id, |record| record.session.status),
        SessionStatus::Idle
    );
    assert!(session_record(&state, &session_id, system_notices).is_empty());

    // A dispatched prompt's result finalizes its own generation, not a
    // successor's.
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).expect("session");
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].active_turn_generation = 5;
    }
    let stale = claude_host_prompt_owner(&host_prompt("earlier", 4));
    state
        .finish_claude_result(&session_id, &token, ClaudeResultOwner::Host(stale), None)
        .expect("a stale result is accepted");
    assert_eq!(
        session_record(&state, &session_id, |record| record.session.status),
        SessionStatus::Active,
        "a result of generation 4 does not finish generation 5"
    );
    let stale_error = claude_host_prompt_owner(&host_prompt("earlier", 4));
    state
        .finish_claude_result(
            &session_id,
            &token,
            ClaudeResultOwner::Host(stale_error),
            Some("late failure"),
        )
        .expect("a stale error result is accepted");
    assert_eq!(
        session_record(&state, &session_id, |record| record.session.status),
        SessionStatus::Active,
        "nor does its error fail it"
    );
    // A result with no turn open while a prompt waits finalizes nothing and
    // says so, so the waiting session is visible to its user.
    state
        .finish_claude_result(
            &session_id,
            &token,
            ClaudeResultOwner::Uncorrelated {
                prompt_waiting: true,
            },
            None,
        )
        .expect("an uncorrelated result is accepted");
    session_record(&state, &session_id, |record| {
        assert_eq!(record.session.status, SessionStatus::Active);
        assert_eq!(record.active_turn_generation, 5);
        let notices = system_notices(record);
        assert!(
            notices
                .last()
                .is_some_and(|notice| notice.contains("could not tie")
                    && notice.contains("nothing was finalized")),
            "{notices:?}"
        );
    });
    let current = claude_host_prompt_owner(&host_prompt("current", 5));
    state
        .finish_claude_result(&session_id, &token, ClaudeResultOwner::Host(current), None)
        .expect("the current result finishes");
    assert_eq!(
        session_record(&state, &session_id, |record| record.session.status),
        SessionStatus::Idle
    );

    // A runtime-started turn that was not adopted finalizes nothing, error
    // or not.
    state
        .finish_claude_result(
            &session_id,
            &token,
            ClaudeResultOwner::Runtime(ClaudeRuntimeTurnOwner {
                cause: ClaudeUnownedCause::TaskNotice,
                adopted_generation: None,
            }),
            Some("the runtime's own turn failed"),
        )
        .expect("an unadopted runtime result is accepted");
    assert_eq!(
        session_record(&state, &session_id, |record| record.session.status),
        SessionStatus::Idle
    );
}

// A runtime that reports message lifecycles (`msg_lifecycle_v1`): the prompt's
// uuid opens its turn and its named result settles it.

fn capable_init() -> Value {
    json!({"type": "system", "subtype": "init", "capabilities": ["msg_lifecycle_v1"]})
}

fn lifecycle(prompt: &ClaudePromptCommand, state: &str) -> Value {
    json!({"type": "command_lifecycle", "command_uuid": prompt.replay_generation,
        "state": state})
}

fn named_result(prompt: &ClaudePromptCommand) -> Value {
    json!({"type": "result", "subtype": "success", "is_error": false, "result": "done",
        "user_message_uuid": prompt.replay_generation,
        "user_message_uuids": [prompt.replay_generation]})
}

#[test]
fn a_native_slash_command_finishes_its_turn_through_its_lifecycle_identity() {
    // /context is answered with no echo; its started and its named result
    // settle the session's turn, so the session is not left busy.
    let (state, session_id, token, runtime_rx) = idle_claude_session("runtime-turn-native-slash");
    let dispatch = match send(&state, &session_id, "/context") {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("an idle session should dispatch"),
    };
    deliver_turn_dispatch(&state, dispatch).expect("the prompt should be delivered");
    let ClaudeRuntimeCommand::Prompt(prompt) = receive(&runtime_rx, "the command is written")
    else {
        panic!("expected the prompt");
    };
    let ownership = new_claude_turn_ownership();
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&prompt));
    for frame in [
        lifecycle(&prompt, "queued"),
        lifecycle(&prompt, "started"),
        capable_init(),
        assistant_text("## Context Usage"),
        named_result(&prompt),
        lifecycle(&prompt, "completed"),
    ] {
        feed(&state, &session_id, &token, &ownership, frame);
    }
    session_record(&state, &session_id, |record| {
        assert_eq!(record.session.status, SessionStatus::Idle);
        assert_eq!(record.unmediated_claude_turn, None);
        assert!(system_notices(record).is_empty(), "nothing was unowned");
    });
}

#[test]
fn a_prompt_taken_up_inside_an_unowned_turn_is_settled_with_its_grant_marked_mixed() {
    let turn = CheckedTurn::start("runtime-turn-joined", true);
    let token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    let generation = turn.record(|record| record.active_turn_generation);
    let ownership = new_claude_turn_ownership();
    let prompt = host_prompt("Run the checks.", generation);
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&prompt));

    for frame in [
        task_notification(),
        capable_init(),
        assistant_text("working on the notice"),
        lifecycle(&prompt, "queued"),
        lifecycle(&prompt, "started"),
    ] {
        feed(&turn.state, &turn.session_id, &token, &ownership, frame);
    }
    turn.record(|record| {
        assert_eq!(
            record.engram.active_turn_mixed_attribution.as_deref(),
            Some(CHECK_GRANT),
            "taking the prompt up inside the unowned turn makes its grant mixed"
        );
        assert!(
            unmediated_claude_turn_hides_observations(record),
            "what follows stays unassigned until the result"
        );
    });
    feed(
        &turn.state,
        &turn.session_id,
        &token,
        &ownership,
        named_result(&prompt),
    );
    turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Idle);
        assert_eq!(record.unmediated_claude_turn, None);
    });
    let checkpoints = checkpoint_requests(&turn);
    assert_eq!(checkpoints.len(), 1, "the named prompt's grant is closed");
    assert!(
        checkpoints[0]
            .get("observations")
            .and_then(Value::as_array)
            .is_none_or(|observations| observations.is_empty()),
        "its own source report is withheld as mixed: {:#}",
        checkpoints[0]
    );
}

#[test]
fn a_prompt_that_takes_up_an_unattributed_prefix_is_settled_once_with_its_grant_marked_mixed() {
    let turn = CheckedTurn::start("runtime-turn-prefix", true);
    let token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    let generation = turn.record(|record| record.active_turn_generation);
    let ownership = new_claude_turn_ownership();
    let prompt = host_prompt("Run the checks.", generation);
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&prompt));

    // Output naming an identity TermAl never wrote, with no turn open: a
    // prefix nothing can be tied to. The waiting prompt's own start then
    // takes it up.
    for frame in [
        capable_init(),
        json!({"type": "assistant", "user_message_uuid": "99999999-0000-4000-8000-00000000000a",
            "message": {"content": [{"type": "text", "text": "unattributed work"}]}}),
        lifecycle(&prompt, "started"),
    ] {
        feed(&turn.state, &turn.session_id, &token, &ownership, frame);
    }
    turn.record(|record| {
        assert_eq!(
            record.engram.active_turn_mixed_attribution.as_deref(),
            Some(CHECK_GRANT),
            "the prompt's grant is marked mixed: the prefix is nobody's"
        );
        assert!(
            unmediated_claude_turn_hides_observations(record),
            "what follows stays unassigned until the prompt's result"
        );
        assert_eq!(record.session.status, SessionStatus::Active);
    });
    feed(
        &turn.state,
        &turn.session_id,
        &token,
        &ownership,
        named_result(&prompt),
    );
    turn.record(|record| {
        assert_eq!(
            record.session.status,
            SessionStatus::Idle,
            "the prompt settled"
        );
        assert_eq!(
            record.unmediated_claude_turn, None,
            "the prefix's marker is closed"
        );
    });
    let checkpoints = checkpoint_requests(&turn);
    assert_eq!(checkpoints.len(), 1, "the prompt's grant is closed once");
    assert!(
        checkpoints[0]
            .get("observations")
            .and_then(Value::as_array)
            .is_none_or(|observations| observations.is_empty()),
        "its own source report is withheld as mixed: {:#}",
        checkpoints[0]
    );
}

#[test]
fn a_result_without_identity_on_a_capable_runtime_finalizes_nothing_and_says_so() {
    let (state, session_id, token, _runtime_rx) = idle_claude_session("runtime-turn-unresolved");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).expect("session");
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].active_turn_generation = 12;
    }
    let ownership = new_claude_turn_ownership();
    let prompt = host_prompt("Do it.", 12);
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&prompt));
    for frame in [
        lifecycle(&prompt, "started"),
        capable_init(),
        assistant_text("done"),
        result(false),
    ] {
        feed(&state, &session_id, &token, &ownership, frame);
    }
    session_record(&state, &session_id, |record| {
        assert_eq!(record.session.status, SessionStatus::Active);
        assert_eq!(record.active_turn_generation, 12);
        let notices = system_notices(record);
        assert!(
            notices
                .last()
                .is_some_and(|notice| notice.contains("could not resolve")
                    && notice.contains("stop the session")),
            "{notices:?}"
        );
    });
}

#[test]
fn a_grant_closed_before_the_unowned_result_still_withholds_its_source_report() {
    // The grant is marked mixed as the unowned turn opens beside it, so a
    // close that comes first (a stop, a runtime exit, a reset) is covered too.
    let turn = CheckedTurn::start("runtime-turn-early-close", true);
    let token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    let ownership = new_claude_turn_ownership();
    for frame in [task_notification(), init(), assistant_text("editing")] {
        feed(&turn.state, &turn.session_id, &token, &ownership, frame);
    }
    assert_eq!(
        turn.record(|record| record.engram.active_turn_mixed_attribution.clone()),
        Some(CHECK_GRANT.to_owned())
    );
    turn.change_readme("edited while the grant was still open\n");
    let checkpoint = turn.finish();
    assert!(
        checkpoint
            .get("observations")
            .and_then(Value::as_array)
            .is_none_or(|observations| observations.is_empty()),
        "{checkpoint:#}"
    );
}

#[test]
fn a_replaced_runtimes_late_result_clears_and_marks_nothing() {
    let turn = CheckedTurn::start("runtime-turn-replaced", true);
    let current = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    let ownership = new_claude_turn_ownership();
    for frame in [task_notification(), init(), assistant_text("unowned")] {
        feed(&turn.state, &turn.session_id, &current, &ownership, frame);
    }
    let marker = turn.record(|record| record.unmediated_claude_turn.clone());
    assert!(marker.is_some());
    turn.record_mut(|record| record.engram.active_turn_mixed_attribution = None);
    let notices_before = turn.record(|record| system_notices(record).len());

    let stale = RuntimeToken::Claude("a-replaced-runtime".to_owned());
    for owner in [
        ClaudeResultOwner::Runtime(ClaudeRuntimeTurnOwner {
            cause: ClaudeUnownedCause::TaskNotice,
            adopted_generation: None,
        }),
        ClaudeResultOwner::Unresolved {
            ended_unowned: true,
            prompt_waiting: true,
            adopted_interval: None,
        },
        // An adopted interval of the replaced runtime, at the session's
        // current generation even: it ends nothing of the successor.
        ClaudeResultOwner::Unresolved {
            ended_unowned: true,
            prompt_waiting: false,
            adopted_interval: Some(ClaudeRuntimeTurnOwner {
                cause: ClaudeUnownedCause::TaskNotice,
                adopted_generation: Some(turn.record(|record| record.active_turn_generation)),
            }),
        },
    ] {
        turn.state
            .finish_claude_result(&turn.session_id, &stale, owner, None)
            .expect("a stale result is accepted");
    }
    turn.record(|record| {
        assert_eq!(
            record.unmediated_claude_turn, marker,
            "the marker is the current runtime's"
        );
        assert_eq!(record.engram.active_turn_mixed_attribution, None);
        assert_eq!(system_notices(record).len(), notices_before);
        assert_eq!(
            record.session.status,
            SessionStatus::Active,
            "the current runtime's turn runs on"
        );
    });
}

// Claude work that outlives its frame, through the same reader, likewise a
// child module.
#[path = "engram_claude_outstanding_work.rs"]
mod outstanding_work;

/// One Claude runtime's stdout reader on a `CheckedTurn`'s session: frames go
/// through the production frame application.
struct ClaudeReaderOn {
    context: ClaudeRuntimeContext,
    frames: ClaudeReaderFrames,
    recorder: SessionRecorder,
    _input_rx: std::sync::mpsc::Receiver<ClaudeRuntimeCommand>,
}

impl ClaudeReaderOn {
    fn new(turn: &CheckedTurn, ownership: ClaudeTurnOwnership) -> Self {
        let token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
        let (input_tx, input_rx) = std::sync::mpsc::channel();
        let context = ClaudeRuntimeContext::new(
            turn.state.clone(),
            turn.session_id.clone(),
            token,
            ownership,
            Arc::new(Mutex::new(None)),
            turn.record(|record| record.session.workdir.clone()),
        );
        let frames = ClaudeReaderFrames::new(&context, input_tx, None);
        let recorder = SessionRecorder::new(turn.state.clone(), turn.session_id.clone());
        Self {
            context,
            frames,
            recorder,
            _input_rx: input_rx,
        }
    }

    fn feed(&mut self, frame: Value) {
        let outcome =
            apply_claude_frame(&self.context, &mut self.frames, &mut self.recorder, &frame);
        assert_eq!(outcome.next, ClaudeFrameApplied::Continue, "{frame}");
    }
}

fn root_task(id: &str) -> Value {
    json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": id,
        "name": "Task", "input": {"description": "background work", "prompt": "work",
        "run_in_background": true}}]}})
}

fn nested_bash(id: &str, parent: &str) -> Value {
    json!({"type": "assistant", "parent_tool_use_id": parent, "message": {"content": [{
        "type": "tool_use", "id": id, "name": "Bash", "input": {"command": SIZE_TEST}}]}})
}

/// Claude Code's terminal notice for the background tool use `tool_use_id`.
fn task_ended(tool_use_id: &str) -> Value {
    json!({"type": "system", "subtype": "task_notification", "task_id": "b-task",
        "tool_use_id": tool_use_id, "status": "completed"})
}

fn nested_bash_result(id: &str, parent: &str) -> Value {
    json!({"type": "user", "parent_tool_use_id": parent, "message": {"role": "user",
        "content": [{"type": "tool_result", "tool_use_id": id,
            "content": "running 3 tests\ntest result: ok. 3 passed"}]},
        "tool_use_result": {"stdout": "running 3 tests\ntest result: ok. 3 passed",
            "stderr": "", "interrupted": false}})
}

#[test]
fn a_background_subagents_late_work_is_never_credited_to_the_next_prompts_grant() {
    let turn = two_turn_root("nested-origin");
    let ownership = new_claude_turn_ownership();
    let mut reader = ClaudeReaderOn::new(&turn, ownership.clone());
    let first_generation = turn.record(|record| record.active_turn_generation);
    let first = host_prompt("Launch the background work.", first_generation);
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&first));

    // Prompt A launches a background task, whose subagent starts a test
    // while A's own turn is current. A subagent's work is credited to no
    // grant, even its own attempt's, and the background task excludes the
    // grant that launched it (`claude_outstanding_work.rs`).
    reader.feed(capable_init());
    reader.feed(lifecycle(&first, "started"));
    reader.feed(root_task("toolu-background"));
    reader.feed(nested_bash("started-under-a", "toolu-background"));
    turn.wait_for_snapshots();
    turn.record(|record| {
        assert!(
            record.engram.active_turn_checks.is_empty(),
            "a subagent's test starts no check, even under its own attempt"
        );
        assert_eq!(
            record.engram.active_turn_mixed_attribution.as_deref(),
            Some(CHECK_GRANT),
            "the grant that launched the background task is mixed"
        );
    });
    reader.feed(named_result(&first));
    assert_eq!(
        turn.record(|record| record.session.status),
        SessionStatus::Idle
    );

    // Prompt B begins with a grant of its own.
    begin_next_turn(&turn);
    let second_generation = turn.record(|record| record.active_turn_generation);
    let second = host_prompt("The next prompt.", second_generation);
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&second));
    reader.feed(lifecycle(&second, "started"));
    assert_eq!(
        turn.record(|record| record.engram.active_grant_id.clone())
            .as_deref(),
        Some(CONTINUITY_NEXT_GRANT)
    );

    // A's subagent keeps working: the command A started ends, another test
    // starts and ends, and a file is written, all during B.
    reader.feed(nested_bash_result("started-under-a", "toolu-background"));
    reader.feed(nested_bash("late-under-b", "toolu-background"));
    reader.feed(nested_bash_result("late-under-b", "toolu-background"));
    reader.feed(
        json!({"type": "assistant", "parent_tool_use_id": "toolu-background",
        "message": {"content": [{"type": "tool_use", "id": "late-write", "name": "Write",
            "input": {"file_path": "late.txt", "content": "late"}}]}}),
    );
    reader.feed(
        json!({"type": "user", "parent_tool_use_id": "toolu-background",
        "message": {"role": "user", "content": [{"type": "tool_result",
            "tool_use_id": "late-write", "content": "File created"}]},
        "tool_use_result": {"type": "create", "filePath": "late.txt", "content": "late"}}),
    );
    turn.record(|record| {
        assert!(
            record.engram.active_turn_checks.is_empty(),
            "no check of B's grant came from A's subagent"
        );
        assert!(record.engram.running_command_keys.is_empty());
        assert_eq!(
            record.engram.active_turn_mixed_attribution.as_deref(),
            Some(CONTINUITY_NEXT_GRANT),
            "B's grant is marked mixed: foreign work fell into its interval"
        );
        assert!(
            record
                .unassigned_claude_observations
                .iter()
                .any(|kept| kept.what.contains("late-under-b") && kept.what.contains("started")),
            "the foreign work is kept as unassigned"
        );
        assert!(
            record.session.messages.iter().any(|message| matches!(
                message,
                Message::Command { command, .. } if command == SIZE_TEST
            )),
            "the subagent's command stays visible"
        );
        assert!(record.session.messages.iter().any(
            |message| matches!(message, Message::Diff { file_path, .. } if file_path == "late.txt")
        ));
        assert_eq!(
            record.unmediated_claude_turn, None,
            "no root turn was opened"
        );
        assert_eq!(record.session.status, SessionStatus::Active);
    });

    // A's background task reports its end. From then on nothing restricts
    // the session: a test B runs at top level keeps its credit, though B's
    // grant stays mixed, and B's result ends B.
    reader.feed(task_ended("toolu-background"));
    turn.record(|record| assert!(!record.claude_outstanding.any()));
    reader.feed(
        json!({"type": "assistant", "message": {"content": [{"type": "tool_use",
            "id": "own-test", "name": "Bash", "input": {"command": SIZE_TEST}}]}}),
    );
    turn.wait_for_snapshots();
    turn.record(|record| {
        assert_eq!(record.engram.active_turn_checks.len(), 1);
        assert!(!record.engram.active_turn_checks[0].overlapped);
    });
    reader.feed(
        json!({"type": "user", "message": {"role": "user", "content": [{
            "type": "tool_result", "tool_use_id": "own-test",
            "content": "running 3 tests\ntest result: ok. 3 passed"}]},
        "tool_use_result": {"stdout": "running 3 tests\ntest result: ok. 3 passed",
            "stderr": "", "interrupted": false}}),
    );
    reader.feed(named_result(&second));
    assert_eq!(
        turn.record(|record| record.session.status),
        SessionStatus::Idle
    );
    let checkpoints = checkpoint_requests(&turn);
    assert_eq!(checkpoints.len(), 2);
    assert_eq!(
        checkpoints[1]["verification_evidence"]
            .as_array()
            .map(Vec::len),
        Some(1),
        "only B's own top-level test is B's evidence: {:#}",
        checkpoints[1]
    );
}
