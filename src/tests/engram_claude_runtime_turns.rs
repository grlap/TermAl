// Owns the tests of turns Claude Code starts by itself
// (src/claude_runtime_turns.rs, src/claude_turn_ownership.rs) at the host's
// real choke points: an idle session adopts such a turn (Active, a new turn
// generation, prompts queued behind it, an immediate notice); a turn the
// runtime starts beside an ordinary turn neither ends that turn nor borrows
// its observation identity; every result finalizes only the turn that owns it.
// Frames are fed exactly as the Claude stdout reader feeds them. Also owns
// historical endpoint comparison without grant or checkpoint continuity.
// Does not own the frame-ownership rules themselves
// (src/tests/claude_turn_ownership.rs) or the in-turn check tests
// (src/tests/engram_turn_checks.rs, whose `EOCheckedTurn` fixture this child
// module uses). New module.
use super::*;

#[path = "engram_unadmitted_runtime_reporting.rs"]
mod unadmitted_runtime_reporting;

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

fn observation_requests(turn: &EOCheckedTurn) -> Vec<Value> {
    turn.transport
        .requests()
        .into_iter()
        .filter(|request| request.request["operation"] == "execution_observe"
            && request.request["occurrence"]["kind"] == "unadmitted_turn")
        .map(|request| request.request)
        .collect()
}

#[test]
fn a_runtime_started_turn_beside_an_ordinary_turn_never_ends_or_borrows_it() {
    // The probed race: TermAl's prompt is written while a background-task
    // notice starts a turn of the runtime's own. The notice turn opens first;
    // the ordinary prompt still waits for its own echo.
    let turn = EOCheckedTurn::start("runtime-beside-ordinary", true);
    let turn_ref = turn.record(|record| record.engram.unadmitted_active_turn.as_ref().unwrap().host_turn_ref.clone());
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
                && notices[0].contains("TermAl did not prompt it")
                && notices[0].contains("no automatic observation report")
                && notices[0].contains("no verification credit")
                && notices[0].contains("not part of it"),
            "{}",
            notices[0]
        );
    });

    // A test in the foreign interval is kept on
    // the session as unassigned, under the notice that announced the turn.
    // The Claude reader tags work while no host prompt's turn is open as
    // unattributed; the fixture's Codex runtime would accept ambient work.
    let mut runtime_recorder = turn.recorder();
    runtime_recorder.set_observation_provenance(EngramObservationProvenance::Claude(
        ClaudeObservationProvenance { token: token.clone(), origin: ClaudeWorkOrigin::Unattributed },
    ));
    runtime_recorder.command_started("runtime-test", SIZE_TEST).unwrap();
    turn.wait_for_snapshots();
    runtime_recorder.command_completed_with_exit("runtime-test", SIZE_TEST,
        "running 3 tests\ntest result: ok. 3 passed", CommandStatus::Success,
        EngramCommandExit::Code(0)).unwrap();
    turn.wait_for_snapshots();
    assert!(
        turn.record(|record| record.engram.active_turn_checks.is_empty()),
        "a foreign test cannot become the waiting prompt's check"
    );
    assert!(
        turn.record(|record| record.engram.unadmitted_active_turn.as_ref().unwrap().commands.is_empty()),
        "nor a fact of the waiting prompt's ordinary trace"
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
    // runtime turn's failing result neither ends nor fails the waiting turn.
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
            record.engram.unadmitted_active_turn.as_ref().map(|trace| trace.host_turn_ref.as_str()),
            Some(turn_ref.as_str()),
            "the original ordinary turn retains its trace"
        );
    });
    assert!(
        observation_requests(&turn).is_empty(),
        "the foreign runtime result closed no ordinary trace"
    );

    // The ordinary turn opens on its own echo; its test is reported, and its
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
    let observed = turn.observation_for(&turn_ref);
    assert_ordinary_observation_has_no_native_credit(&turn, &observed);
    assert_eq!(
        observed["occurrence"]["observed_checks"]
            .as_array()
            .map(Vec::len),
        Some(1),
        "{observed:#}"
    );
}

#[test]
fn an_edit_by_a_foreign_turn_leaves_the_ordinary_source_report_withheld_as_uncertain() {
    // Nothing separates the notice turn's edits from this prompt's interval.
    // Keep the original trace and withhold source attribution while its
    // current owner can still complete normally.
    let turn = EOCheckedTurn::start("runtime-beside-edit", true);
    let token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    let generation = turn.record(|record| record.active_turn_generation);
    let begun = turn.revision();
    let turn_ref = turn.record(|record| record.engram.unadmitted_active_turn.as_ref().unwrap().host_turn_ref.clone());
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
                .unadmitted_active_turn
                .as_ref()
                .map(|trace| trace.host_turn_ref.clone()),
            Some(turn_ref.clone()),
            "the original trace is kept"
        );
        assert!(record.engram.unadmitted_active_turn.as_ref().unwrap().source_attribution_uncertain);
        assert!(
            record
                .unassigned_claude_observations
                .iter()
                .any(|observation| observation.what.contains("source changes are not attributed")),
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
    let observed = turn.observation_for(&turn_ref);
    assert!(observed["occurrence"]["source_change"].is_null(), "{observed:#}");
    assert_ordinary_observation_has_no_native_credit(&turn, &observed);
}

#[test]
fn an_adopted_runtime_started_turn_preserves_passive_grant_history_without_using_it() {
    let turn = EOCheckedTurn::start("runtime-adopted", true);
    turn.finish();
    turn.record_mut(|record| {
        record.engram.historical_control.insert("engramOpenGrantId".into(), json!("leftover-grant"));
    });
    let token = turn.record(|record| record.runtime.runtime_token().unwrap());
    let before = turn.record(|record| record.active_turn_generation);
    let ownership = new_claude_turn_ownership();
    for frame in [task_notification(), init(), assistant_text("noticed")] {
        feed(&turn.state, &turn.session_id, &token, &ownership, frame);
    }
    let adopted_ref = turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Active);
        assert_eq!(record.active_turn_generation, before + 1);
        assert_eq!(record.unmediated_claude_turn.as_ref().map(|turn| turn.adopted_generation), Some(Some(before + 1)));
        let notices = system_notices(record);
        assert!(notices.last().is_some_and(|notice| notice.contains("Prompts sent meanwhile wait")
            && notice.contains("the host attempts to report")
            && notice.contains("unknown causality")
            && notice.contains("do not earn verification credit")
            && !notice.contains("not part of it")), "{notices:?}");
        record.engram.unadmitted_active_turn.as_ref().unwrap().host_turn_ref.clone()
    });
    turn.run("runtime-test", SIZE_TEST, EngramCommandExit::Code(0), || {});
    feed(&turn.state, &turn.session_id, &token, &ownership, result(false));
    turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Idle);
        assert_eq!(record.unmediated_claude_turn, None);
        assert_eq!(record.engram.historical_control.get("engramOpenGrantId"), Some(&json!("leftover-grant")));
    });
    let observed = turn.observation_for(&adopted_ref);
    assert!(!observed.to_string().contains("leftover-grant"));
    assert_eq!(observed["occurrence"]["observed_checks"].as_array().unwrap().len(), 1);
    assert_ordinary_observation_has_no_native_credit(&turn, &observed);
}

#[test]
fn a_bound_runtime_started_turn_with_observability_off_promises_no_automatic_report() {
    let turn = EOCheckedTurn::idle("runtime-bound-observability-off", true);
    turn.set_observability(false);
    let (handle, _claude_rx) = test_claude_runtime_handle("runtime-bound-observability-off");
    let token = RuntimeToken::Claude(handle.runtime_id.clone());
    turn.record_mut(|record| {
        record.session.agent = Agent::Claude;
        record.runtime = SessionRuntime::Claude(handle);
        record.session.status = SessionStatus::Idle;
    });
    let ownership = new_claude_turn_ownership();
    for frame in [task_notification(), init(), assistant_text("noticed with observability off")] {
        feed(&turn.state, &turn.session_id, &token, &ownership, frame);
    }
    turn.record(|record| {
        assert!(record.engram.work_binding.is_some(), "a claim is not an adopted reporting trace");
        assert!(record.adopted_claude_turn_generation.is_some());
        assert!(record.engram.unadmitted_active_turn.is_none());
        let notices = system_notices(record);
        let notice = notices.last().unwrap();
        assert!(notice.contains("TermAl did not prompt it")
            && notice.contains("no automatic observation report")
            && notice.contains("kept unassigned")
            && notice.contains("no verification credit"), "{notice}");
        assert!(!notice.contains("the host attempts to report"), "{notice}");
    });
    feed(&turn.state, &turn.session_id, &token, &ownership, result(false));
    turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Idle);
        assert!(record.engram.unadmitted_active_turn.is_none());
        assert!(record.engram.unadmitted_closing_jobs.is_empty());
        assert!(record.engram.unadmitted_reports.is_empty());
    });
    assert!(observation_requests(&turn).is_empty());
    assert_ordinary_turn_has_no_native_requests(&turn);
}

/// The first ordinary prompt is active; a successor needs no protocol script.
fn two_turn_root(label: &str) -> EOCheckedTurn {
    EOCheckedTurn::start(label, true)
}

fn begin_next_turn(turn: &EOCheckedTurn) {
    turn.begin("The next turn.");
}

fn endpoint_comparison() -> EngramRuntimeClosingCapture {
    let binding = test_control_work_binding("endpoint-comparison", 1);
    EngramRuntimeClosingCapture {
        registered_check_baselines: Vec::new(),
        baseline: Some(EngramRuntimeCheckpointBasis {
            binding: binding.clone(),
            endpoint: EngramMeasuredBaseline {
                workspace_id: "workspace-original".into(),
                source_revision: "content-v1:before".into(),
                observed_at: "2026-10-18T10:00:00Z".into(),
            },
            source_root_generation: None,
            source_root_state: None,
        }),
        binding,
        root_basis: EngramObservationRootBasis {
            capture_run_cut: 1, latest_event: None,
            state: EngramObservationRootState::None {},
        },
        sighting: EngramMeasuredSighting {
            source_basis: EngramExecutionSourceBasis {
                workspace_id: "workspace-original".into(),
                source_revision: "content-v1:after".into(),
                source_root_generation: None, source_root_state: None,
            },
            observed_at: "2026-10-18T10:02:00Z".into(),
        },
        policy: EngramObservationPolicyBasis::AccountIfEligible {
            project_policy_epoch: 1, policy: "a".repeat(32), obligation_rule_set: "b".repeat(32),
        },
    }
}

#[test]
fn an_ordinary_source_comparison_keeps_the_original_endpoint_and_new_measurement() {
    let capture = endpoint_comparison();
    let before = capture.baseline.as_ref().unwrap().clone();
    let change = capture.source_change().unwrap();
    let EngramObservedSourceChange::ContentComparison { workspace_id, baseline, sighting } = change else {
        panic!("same-owner endpoint comparison should preserve both measured cuts");
    };
    assert_eq!(workspace_id, "workspace-original");
    assert_eq!(baseline, before.endpoint);
    assert_eq!(sighting, capture.sighting);
    assert_eq!(capture.baseline.as_ref(), Some(&before), "comparison never advances historical state");
}

#[test]
fn an_equal_source_endpoint_does_not_manufacture_an_edit() {
    let mut capture = endpoint_comparison();
    capture.sighting.source_basis.source_revision = capture.baseline.as_ref().unwrap().endpoint.source_revision.clone();
    assert!(capture.source_change().is_none());
}

#[test]
fn an_ordinary_source_comparison_never_borrows_another_bindings_or_workspaces_endpoint() {
    for mismatch in ["work", "run", "claim", "fence", "workspace", "missing"] {
        let mut capture = endpoint_comparison();
        let before = capture.baseline.as_mut().unwrap();
        match mismatch {
            "work" => before.binding.work_id.push_str("-other"),
            "run" => before.binding.run_id.push_str("-other"),
            "claim" => before.binding.claim_id.push_str("-other"),
            "fence" => before.binding.claim_fence += 1,
            "workspace" => before.endpoint.workspace_id.push_str("-other"),
            "missing" => capture.baseline = None,
            _ => unreachable!(),
        }
        let Some(EngramObservedSourceChange::AssumedMissingBaseline { workspace_id, sighting }) = capture.source_change() else {
            panic!("{mismatch}: an incompatible endpoint cannot become this observation's baseline");
        };
        assert_eq!(workspace_id, "workspace-original");
        assert_eq!(sighting, capture.sighting);
    }
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
fn a_prompt_taken_up_inside_an_unowned_turn_keeps_source_attribution_uncertain() {
    let turn = EOCheckedTurn::start("runtime-turn-joined", true);
    let turn_ref = turn.record(|record| record.engram.unadmitted_active_turn.as_ref().unwrap().host_turn_ref.clone());
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
        assert!(record.engram.unadmitted_active_turn.as_ref().unwrap().source_attribution_uncertain,
            "taking the prompt up inside an unowned interval makes its source attribution uncertain");
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
    let observed = turn.observation_for(&turn_ref);
    assert!(observed["occurrence"]["source_change"].is_null(), "{observed:#}");
    assert_eq!(observation_requests(&turn).iter().filter(|request|
        request["occurrence"]["host_turn_ref"] == turn_ref).count(), 1);
    assert_ordinary_observation_has_no_native_credit(&turn, &observed);
}

#[test]
fn a_prompt_that_takes_up_an_unattributed_prefix_settles_once_with_uncertain_sources() {
    let turn = EOCheckedTurn::start("runtime-turn-prefix", true);
    let turn_ref = turn.record(|record| record.engram.unadmitted_active_turn.as_ref().unwrap().host_turn_ref.clone());
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
        assert!(record.engram.unadmitted_active_turn.as_ref().unwrap().source_attribution_uncertain,
            "the unattributed prefix cannot become the prompt's source change");
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
    let observed = turn.observation_for(&turn_ref);
    assert!(observed["occurrence"]["source_change"].is_null(), "{observed:#}");
    assert_eq!(observation_requests(&turn).iter().filter(|request|
        request["occurrence"]["host_turn_ref"] == turn_ref).count(), 1);
    assert_ordinary_observation_has_no_native_credit(&turn, &observed);
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
fn an_ordinary_turn_closed_before_the_unowned_result_still_withholds_its_source_report() {
    // Source attribution becomes uncertain as the unowned turn opens, so a
    // close that comes first (a stop, a runtime exit, a reset) is covered too.
    let turn = EOCheckedTurn::start("runtime-turn-early-close", true);
    let token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    let ownership = new_claude_turn_ownership();
    for frame in [task_notification(), init(), assistant_text("editing")] {
        feed(&turn.state, &turn.session_id, &token, &ownership, frame);
    }
    assert!(turn.record(|record| record.engram.unadmitted_active_turn.as_ref().unwrap().source_attribution_uncertain));
    turn.change_readme("edited during the unowned interval\n");
    let observed = turn.finish();
    assert!(observed["occurrence"]["source_change"].is_null(), "{observed:#}");
    assert_ordinary_observation_has_no_native_credit(&turn, &observed);
}

#[test]
fn a_replaced_runtimes_late_result_clears_and_marks_nothing() {
    let turn = EOCheckedTurn::start("runtime-turn-replaced", true);
    let current = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    let ownership = new_claude_turn_ownership();
    for frame in [task_notification(), init(), assistant_text("unowned")] {
        feed(&turn.state, &turn.session_id, &current, &ownership, frame);
    }
    let marker = turn.record(|record| record.unmediated_claude_turn.clone());
    assert!(marker.is_some());
    turn.record_mut(|record| record.engram.unadmitted_active_turn.as_mut().unwrap().source_attribution_uncertain = false);
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
        assert!(!record.engram.unadmitted_active_turn.as_ref().unwrap().source_attribution_uncertain);
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

/// One Claude runtime's stdout reader on an ordinary fixture session: frames go
/// through the production frame application.
struct ClaudeReaderOn {
    context: ClaudeRuntimeContext,
    frames: ClaudeReaderFrames,
    recorder: SessionRecorder,
    _input_rx: std::sync::mpsc::Receiver<ClaudeRuntimeCommand>,
}

impl ClaudeReaderOn {
    fn new(turn: &EOCheckedTurn, ownership: ClaudeTurnOwnership) -> Self {
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
fn a_background_subagents_late_work_never_becomes_the_next_prompts_check_or_source_change() {
    let turn = two_turn_root("nested-origin");
    let ownership = new_claude_turn_ownership();
    let mut reader = ClaudeReaderOn::new(&turn, ownership.clone());
    let first_generation = turn.record(|record| record.active_turn_generation);
    let first_ref = turn.record(|record| record.engram.unadmitted_active_turn.as_ref().unwrap().host_turn_ref.clone());
    let first = host_prompt("Launch the background work.", first_generation);
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&first));

    // Prompt A launches a background task, whose subagent starts a test
    // while A's own turn is current. Those foreign check facts cannot become
    // root checks; overlapping work makes source attribution uncertain.
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
        assert!(record.engram.unadmitted_active_turn.as_ref().unwrap().source_attribution_uncertain);
    });
    reader.feed(named_result(&first));
    assert_eq!(
        turn.record(|record| record.session.status),
        SessionStatus::Idle
    );

    let first_observed = turn.observation_for(&first_ref);
    assert!(first_observed["occurrence"]["source_change"].is_null());
    // Prompt B begins with a distinct ordinary trace.
    begin_next_turn(&turn);
    let second_generation = turn.record(|record| record.active_turn_generation);
    let second = host_prompt("The next prompt.", second_generation);
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&second));
    reader.feed(lifecycle(&second, "started"));
    let second_ref = turn.record(|record| record.engram.unadmitted_active_turn.as_ref().unwrap().host_turn_ref.clone());
    assert_ne!(first_ref, second_ref);

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
            "no check of B came from A's subagent"
        );
        assert!(record.engram.running_command_keys.is_empty());
        assert!(record.engram.unadmitted_active_turn.as_ref().unwrap().source_attribution_uncertain);
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
    // the session: a top-level test becomes B's own uncredited fact. Source
    // attribution stays uncertain, and B's result ends B.
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
    let observed = turn.observation_for(&second_ref);
    assert!(observed["occurrence"]["source_change"].is_null());
    assert_eq!(
        observed["occurrence"]["observed_checks"]
            .as_array()
            .map(Vec::len),
        Some(1),
        "only B's own top-level test is B's observed fact: {observed:#}"
    );
    assert_eq!(observed["occurrence"]["observed_checks"][0]["host_check_id"],
        unadmitted_host_check_id(&second_ref, "own-test"));
    assert_ordinary_observation_has_no_native_credit(&turn, &observed);
}
