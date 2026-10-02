// Which turn a Claude stdout frame belongs to, replayed from live Claude Code
// 2.1.285 stream-json captures taken with TermAl's flags. The frames are the
// captured ones, trimmed to the fields the ownership rules read.

use super::*;

fn host_prompt(text: &str, turn_generation: u64) -> ClaudePromptCommand {
    ClaudePromptCommand {
        attachments: Vec::new(),
        replay_generation: format!("replay-{turn_generation}"),
        text: text.to_owned(),
        turn_generation,
    }
}

fn init() -> Value {
    json!({"type": "system", "subtype": "init", "session_id": "probe"})
}

fn status_requesting() -> Value {
    json!({"type": "system", "subtype": "status", "status": "requesting"})
}

fn rate_limit() -> Value {
    json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed"}})
}

fn echo(prompt: &ClaudePromptCommand) -> Value {
    json!({
        "type": "user",
        "message": {"role": "user", "content": claude_prompt_content(prompt)},
        "isReplay": true,
    })
}

fn message_start() -> Value {
    json!({"type": "stream_event", "event": {"type": "message_start"}})
}

fn assistant_text(text: &str) -> Value {
    json!({"type": "assistant", "message": {"content": [{"type": "text", "text": text}]}})
}

fn assistant_tool_use(command: &str, background: bool) -> Value {
    json!({"type": "assistant", "message": {"content": [{
        "type": "tool_use",
        "id": format!("toolu-{command}"),
        "name": "Bash",
        "input": {"command": command, "run_in_background": background},
    }]}})
}

fn tool_result(command: &str) -> Value {
    json!({"type": "user", "message": {"role": "user", "content": [{
        "tool_use_id": format!("toolu-{command}"),
        "type": "tool_result",
        "content": "Command running in background with ID: b1.",
    }]}})
}

fn background_tasks_changed() -> Value {
    json!({"type": "system", "subtype": "background_tasks_changed", "tasks": []})
}

fn task_started(task_id: &str) -> Value {
    json!({"type": "system", "subtype": "task_started", "task_id": task_id})
}

fn task_updated(task_id: &str) -> Value {
    json!({"type": "system", "subtype": "task_updated", "task_id": task_id,
        "patch": {"status": "completed"}})
}

fn task_notification(task_id: &str) -> Value {
    json!({"type": "system", "subtype": "task_notification", "task_id": task_id,
        "tool_use_id": format!("toolu-{task_id}"), "status": "completed"})
}

fn notice_echo(task_id: &str) -> Value {
    json!({"type": "user", "message": {"role": "user", "content": format!(
        "<task-notification>\n<task-id>{task_id}</task-id>\n<status>completed</status>\n</task-notification>"
    )}, "isReplay": true})
}

/// Feeds frames through the production frame router, as the reader does.
/// Returns what each opening frame did and the turn each result ended (a
/// late duplicate ends none, and is reported as `Stale`).
fn replay(
    state: &mut ClaudeTurnOwnershipState,
    frames: Vec<Value>,
) -> (Vec<ClaudeFrameOwnership>, Vec<ClaudeResultOwner>) {
    let ownership: ClaudeTurnOwnership = Arc::new(Mutex::new(std::mem::take(state)));
    let mut router = ClaudeFrameRouter::new(ownership.clone(), Arc::new(Mutex::new(None)), "probe");
    let mut opened = Vec::new();
    let mut closed = Vec::new();
    for frame in frames {
        let top_level_result = frame.get("type").and_then(Value::as_str) == Some("result")
            && claude_frame_is_top_level(&frame);
        let plan = router.route(&frame, true);
        match plan.terminal {
            Some(owner) => closed.push(owner),
            None if top_level_result => closed.push(ClaudeResultOwner::Stale),
            None => {}
        }
        if plan.ownership != ClaudeFrameOwnership::Unchanged {
            opened.push(plan.ownership);
        }
    }
    *state = std::mem::take(&mut *lock_claude_turn_ownership(&ownership));
    (opened, closed)
}

fn result() -> Value {
    json!({"type": "result", "subtype": "success", "is_error": false})
}

#[test]
fn a_background_notice_after_a_dispatched_turn_opens_a_runtime_started_turn() {
    // Probe 1: the prompt launches a background Bash task and ends; the
    // task's completion notice then starts a turn nobody prompted.
    let mut state = ClaudeTurnOwnershipState::default();
    let first = host_prompt("Run sleep 8 in the background, then say launched.", 4);
    state.reserve(claude_host_prompt_owner(&first));
    let (opened, closed) = replay(
        &mut state,
        vec![
            init(),
            status_requesting(),
            rate_limit(),
            echo(&first),
            message_start(),
            assistant_tool_use("sleep 8 && echo done", true),
            background_tasks_changed(),
            task_started("b92ruf5ai"),
            tool_result("sleep 8 && echo done"),
            status_requesting(),
            assistant_text("launched"),
            result(),
            background_tasks_changed(),
            task_updated("b92ruf5ai"),
            task_notification("b92ruf5ai"),
            init(),
            status_requesting(),
            message_start(),
            assistant_text("finished"),
            result(),
        ],
    );
    assert_eq!(
        opened,
        vec![
            ClaudeFrameOwnership::OpenedHost(claude_host_prompt_owner(&first)),
            ClaudeFrameOwnership::OpenedRuntime {
                cause: ClaudeUnownedCause::TaskNotice
            },
        ]
    );
    assert_eq!(
        closed,
        vec![
            ClaudeResultOwner::Host(claude_host_prompt_owner(&first)),
            ClaudeResultOwner::Runtime(ClaudeRuntimeTurnOwner {
                cause: ClaudeUnownedCause::TaskNotice,
                adopted_generation: None,
            }),
        ]
    );
    assert!(state.outstanding.is_empty());
}

#[test]
fn a_notice_during_a_dispatched_turn_folds_into_it_and_opens_nothing() {
    // Probe 2: a background task and a foreground call both finish while the
    // dispatched turn runs. Both emit task notifications (a foreground call
    // too), and the background notice is echoed inside the same turn. The
    // next prompt then opens its own turn.
    let mut state = ClaudeTurnOwnershipState::default();
    let first = host_prompt("Background sleep 4, then foreground sleep 12.", 7);
    let second = host_prompt("Reply with only the word two.", 8);
    state.reserve(claude_host_prompt_owner(&first));
    let (opened, closed) = replay(
        &mut state,
        vec![
            init(),
            status_requesting(),
            echo(&first),
            assistant_tool_use("sleep 4 && echo bg", true),
            assistant_tool_use("sleep 12", false),
            task_started("bzx3xpanq"),
            tool_result("sleep 4 && echo bg"),
            task_started("bxw4y3eyq"),
            task_updated("bzx3xpanq"),
            task_notification("bzx3xpanq"),
            task_notification("bxw4y3eyq"),
            tool_result("sleep 12"),
            notice_echo("bzx3xpanq"),
            status_requesting(),
            assistant_text("noticed"),
            result(),
        ],
    );
    assert_eq!(
        opened,
        vec![ClaudeFrameOwnership::OpenedHost(claude_host_prompt_owner(
            &first
        ))]
    );
    assert_eq!(
        closed,
        vec![ClaudeResultOwner::Host(claude_host_prompt_owner(&first))]
    );
    assert!(
        !state.notice_since_result,
        "a notice inside a turn does not mark the next one"
    );

    state.reserve(claude_host_prompt_owner(&second));
    let (opened, closed) = replay(
        &mut state,
        vec![
            init(),
            status_requesting(),
            echo(&second),
            assistant_text("two"),
            result(),
        ],
    );
    assert_eq!(
        opened,
        vec![ClaudeFrameOwnership::OpenedHost(claude_host_prompt_owner(
            &second
        ))]
    );
    assert_eq!(
        closed,
        vec![ClaudeResultOwner::Host(claude_host_prompt_owner(&second))]
    );
}

#[test]
fn a_prompt_written_during_a_runtime_started_turn_owns_only_its_own_turn() {
    // Probe 3, with the prompt reserved before the runtime-started turn's
    // first frame is read (the write races the turn's start): the notice turn
    // still owns its result, the prompt's mid-turn echo transfers nothing, and
    // the prompt's own turn opens on its second echo.
    let mut state = ClaudeTurnOwnershipState::default();
    let prompt = host_prompt("HOSTPROMPT: reply with only the word two.", 12);
    state.notice_since_result = false;
    let (opened, _) = replay(
        &mut state,
        vec![
            background_tasks_changed(),
            task_updated("bb3r9si3c"),
            task_notification("bb3r9si3c"),
            init(),
            status_requesting(),
        ],
    );
    assert!(
        opened.is_empty(),
        "bookkeeping, init and status open nothing"
    );
    state.reserve(claude_host_prompt_owner(&prompt));
    let (opened, closed) = replay(
        &mut state,
        vec![message_start(), assistant_tool_use("sleep 6", false)],
    );
    assert_eq!(
        opened,
        vec![ClaudeFrameOwnership::OpenedRuntime {
            cause: ClaudeUnownedCause::TaskNotice
        }]
    );
    assert!(closed.is_empty());
    let (opened, closed) = replay(
        &mut state,
        vec![
            task_started("b3lvma3cm"),
            task_notification("b3lvma3cm"),
            tool_result("sleep 6"),
            echo(&prompt),
            status_requesting(),
            assistant_text("finished"),
            result(),
        ],
    );
    assert!(opened.is_empty(), "the mid-turn echo opens nothing");
    assert_eq!(
        closed,
        vec![ClaudeResultOwner::Runtime(ClaudeRuntimeTurnOwner {
            cause: ClaudeUnownedCause::TaskNotice,
            adopted_generation: None,
        })],
        "the runtime-started turn's result never ends the waiting prompt's turn"
    );
    assert_eq!(
        state.outstanding.len(),
        1,
        "the prompt still waits for its turn"
    );

    let (opened, closed) = replay(
        &mut state,
        vec![
            init(),
            status_requesting(),
            echo(&prompt),
            assistant_text("two"),
            result(),
        ],
    );
    assert_eq!(
        opened,
        vec![ClaudeFrameOwnership::OpenedHost(claude_host_prompt_owner(
            &prompt
        ))]
    );
    assert_eq!(
        closed,
        vec![ClaudeResultOwner::Host(claude_host_prompt_owner(&prompt))]
    );
    assert!(state.outstanding.is_empty());
}

#[test]
fn a_result_after_a_notice_with_no_turn_output_names_no_turn() {
    let mut state = ClaudeTurnOwnershipState::default();
    let (opened, closed) = replay(&mut state, vec![task_notification("b1"), init(), result()]);
    assert!(opened.is_empty());
    assert_eq!(
        closed,
        vec![ClaudeResultOwner::Uncorrelated {
            prompt_waiting: false
        }]
    );
}

#[test]
fn a_notice_echoed_first_after_init_opens_a_runtime_started_turn() {
    let mut state = ClaudeTurnOwnershipState::default();
    let (opened, _) = replay(&mut state, vec![init(), notice_echo("b2")]);
    assert_eq!(
        opened,
        vec![ClaudeFrameOwnership::OpenedRuntime {
            cause: ClaudeUnownedCause::TaskNotice
        }]
    );
}

#[test]
fn turn_output_with_no_echo_while_a_prompt_waits_is_unassigned_not_credited_to_it() {
    // It may answer the waiting prompt, but nothing proves it: the turn is
    // unassigned and the prompt keeps waiting for its own echo.
    let mut state = ClaudeTurnOwnershipState::default();
    let prompt = host_prompt("no echo", 3);
    state.reserve(claude_host_prompt_owner(&prompt));
    let (opened, closed) = replay(&mut state, vec![init(), assistant_text("ok"), result()]);
    assert_eq!(
        opened,
        vec![ClaudeFrameOwnership::OpenedRuntime {
            cause: ClaudeUnownedCause::Unassigned
        }]
    );
    assert_eq!(
        closed,
        vec![ClaudeResultOwner::Runtime(ClaudeRuntimeTurnOwner {
            cause: ClaudeUnownedCause::Unassigned,
            adopted_generation: None,
        })]
    );
    assert_eq!(state.outstanding.len(), 1, "the prompt still waits");
}

#[test]
fn turn_output_with_no_prompt_waiting_and_no_notice_is_started_by_the_runtime() {
    let mut state = ClaudeTurnOwnershipState::default();
    let (opened, _) = replay(&mut state, vec![init(), message_start()]);
    assert_eq!(
        opened,
        vec![ClaudeFrameOwnership::OpenedRuntime {
            cause: ClaudeUnownedCause::NoPrompt
        }]
    );
}

#[test]
fn an_echo_less_answer_to_a_waiting_slash_command_stays_unassigned_and_finalizes_nothing() {
    // A live `/context` capture: init, then the answer, then result, with no
    // echo of the written command. The same frames could come from a turn the
    // runtime started for a cron or a monitor while the command waits, so
    // they prove no owner: the turn is unassigned, its result names no prompt,
    // and the command keeps waiting.
    let slash = host_prompt("/context", 11);
    let mut state = ClaudeTurnOwnershipState::default();
    state.reserve(claude_host_prompt_owner(&slash));
    let (opened, closed) = replay(
        &mut state,
        vec![init(), assistant_text("## Context Usage"), result()],
    );
    assert_eq!(
        opened,
        vec![ClaudeFrameOwnership::OpenedRuntime {
            cause: ClaudeUnownedCause::Unassigned
        }]
    );
    assert_eq!(
        closed,
        vec![ClaudeResultOwner::Runtime(ClaudeRuntimeTurnOwner {
            cause: ClaudeUnownedCause::Unassigned,
            adopted_generation: None,
        })]
    );
    assert_eq!(state.outstanding.len(), 1, "the command still waits");

    // After a background-task notice the same output is the runtime's own.
    let mut state = ClaudeTurnOwnershipState::default();
    state.reserve(claude_host_prompt_owner(&slash));
    let (opened, _) = replay(
        &mut state,
        vec![task_notification("b1"), init(), assistant_text("noticed")],
    );
    assert_eq!(
        opened,
        vec![ClaudeFrameOwnership::OpenedRuntime {
            cause: ClaudeUnownedCause::TaskNotice
        }]
    );

    // With another prompt waiting too, nothing proves which it answers.
    let mut state = ClaudeTurnOwnershipState::default();
    state.reserve(claude_host_prompt_owner(&host_prompt("a plain prompt", 10)));
    state.reserve(claude_host_prompt_owner(&slash));
    let (opened, _) = replay(&mut state, vec![init(), assistant_text("answer")]);
    assert_eq!(
        opened,
        vec![ClaudeFrameOwnership::OpenedRuntime {
            cause: ClaudeUnownedCause::Unassigned
        }]
    );
}

#[test]
fn an_echo_matching_two_waiting_prompts_leaves_the_turn_unassigned() {
    // Identical text is no unique owner.
    let mut state = ClaudeTurnOwnershipState::default();
    let first = host_prompt("same text", 3);
    let second = ClaudePromptCommand {
        replay_generation: "replay-other".to_owned(),
        ..host_prompt("same text", 4)
    };
    state.reserve(claude_host_prompt_owner(&first));
    state.reserve(claude_host_prompt_owner(&second));
    let (opened, closed) = replay(&mut state, vec![init(), echo(&first), result()]);
    assert_eq!(
        opened,
        vec![ClaudeFrameOwnership::OpenedRuntime {
            cause: ClaudeUnownedCause::Unassigned
        }]
    );
    assert!(matches!(closed.as_slice(), [ClaudeResultOwner::Runtime(_)]));
    assert_eq!(state.outstanding.len(), 2, "neither prompt is consumed");
}

#[test]
fn only_a_top_level_echo_opens_a_prompts_turn() {
    let mut state = ClaudeTurnOwnershipState::default();
    let prompt = host_prompt("delegate this", 6);
    state.reserve(claude_host_prompt_owner(&prompt));
    let mut nested = echo(&prompt);
    nested["parent_tool_use_id"] = json!("toolu-task");
    assert_eq!(state.observe(&init()), ClaudeFrameOwnership::Unchanged);
    assert_eq!(state.observe(&nested), ClaudeFrameOwnership::Unchanged);
    assert_eq!(
        state.observe(&echo(&prompt)),
        ClaudeFrameOwnership::OpenedHost(claude_host_prompt_owner(&prompt))
    );
}

#[test]
fn a_retry_reservation_replaces_the_waiting_one_so_the_retried_echo_owns_its_turn() {
    // A transient-error result can arrive before the prompt's echo opened its
    // turn; the retry then writes the same prompt again.
    let ownership = new_claude_turn_ownership();
    let prompt = host_prompt("retry before the echo", 8);
    let replay_prompt = Arc::new(Mutex::new(None));
    let mut writer = Vec::new();
    write_claude_runtime_command(
        &mut writer,
        &replay_prompt,
        &ownership,
        ClaudeRuntimeCommand::Prompt(prompt.clone()),
    )
    .expect("prompt should be written");
    write_claude_runtime_command(
        &mut writer,
        &replay_prompt,
        &ownership,
        ClaudeRuntimeCommand::RetryLastPrompt {
            ticket: ClaudeRetryTicket {
                replay_generation: prompt.replay_generation.clone(),
                turn_generation: 8,
                attempt: 1,
            },
            retry_detail: "Retrying.".to_owned(),
        },
    )
    .expect("retry should be written");
    let mut state = lock_claude_turn_ownership(&ownership);
    assert_eq!(state.outstanding.len(), 1, "one reservation per prompt");
    let retried = state.outstanding[0].clone();
    assert_eq!(retried.attempt, 1);
    assert_eq!(retried.replay_generation, prompt.replay_generation);
    assert_ne!(retried.attempt_uuid, prompt.replay_generation);
    assert_eq!(state.observe(&init()), ClaudeFrameOwnership::Unchanged);
    assert_eq!(
        state.observe(&echo(&prompt)),
        ClaudeFrameOwnership::OpenedHost(retried)
    );
}

#[test]
fn only_a_result_of_the_prompts_own_turn_acts_on_its_replay_state() {
    let ownership = new_claude_turn_ownership();
    let prompt = host_prompt("waiting", 9);
    let replay_prompt = Arc::new(Mutex::new(Some(prompt.clone())));
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&prompt));
    let mut router = ClaudeFrameRouter::new(ownership.clone(), replay_prompt.clone(), "s");
    let error_result = json!({"type": "result", "is_error": true, "api_error_status": 529});

    // Bookkeeping while the prompt waits neither releases nor bars it.
    let plan = router.route(&init(), true);
    assert_eq!(plan.replay, ClaudeReplayStep::Retain);

    // A turn no prompt owns, and its transient error result, leave it alone.
    router.route(&task_notification("b1"), true);
    let plan = router.route(&assistant_text("noticed"), true);
    assert_eq!(
        plan.ownership,
        ClaudeFrameOwnership::OpenedRuntime {
            cause: ClaudeUnownedCause::TaskNotice
        }
    );
    // The runtime's turn is prepared like any opening (a clean parser) but
    // opens no host attempt and takes no barrier of the waiting prompt.
    assert_eq!(
        plan.parser,
        ClaudeParserStep::Root {
            open: true,
            carry_barrier: false
        }
    );
    assert_eq!(
        router.parser_attempt, None,
        "the runtime's turn opens no host attempt"
    );
    let plan = router.route(&error_result, true);
    assert!(plan.retry.is_none());
    assert_eq!(plan.replay, ClaudeReplayStep::Retain);
    assert!(matches!(plan.terminal, Some(ClaudeResultOwner::Runtime(_))));
    assert_eq!(
        claude_replay_generation(&replay_prompt).as_deref(),
        Some(prompt.replay_generation.as_str())
    );

    // The transient error result of its own attempt retries it.
    router.route(&init(), true);
    let plan = router.route(&echo(&prompt), true);
    assert_eq!(
        plan.parser,
        ClaudeParserStep::Root {
            open: true,
            carry_barrier: false
        }
    );
    let plan = router.route(&error_result, true);
    assert_eq!(plan.parser, ClaudeParserStep::DiscardForRetry);
    assert_eq!(plan.terminal, None);
    assert_eq!(
        plan.retry.map(|retry| retry.ticket),
        Some(ClaudeRetryTicket {
            replay_generation: prompt.replay_generation.clone(),
            turn_generation: 9,
            attempt: 1,
        })
    );
}

#[test]
fn a_result_with_no_turn_open_names_no_waiting_prompt() {
    let mut state = ClaudeTurnOwnershipState::default();
    let prompt = host_prompt("waiting", 2);
    state.reserve(claude_host_prompt_owner(&prompt));
    let (_, closed) = replay(&mut state, vec![init(), result()]);
    assert_eq!(
        closed,
        vec![ClaudeResultOwner::Uncorrelated {
            prompt_waiting: true
        }]
    );
    assert_eq!(state.outstanding.len(), 1, "the prompt still waits");
}

#[test]
fn a_failed_write_releases_its_reservation_and_a_retry_reserves_again() {
    let mut state = ClaudeTurnOwnershipState::default();
    let prompt = host_prompt("retry me", 5);
    state.reserve(claude_host_prompt_owner(&prompt));
    state.release(&prompt.replay_generation);
    assert!(state.outstanding.is_empty());

    state.reserve(claude_host_prompt_owner(&prompt));
    let (_, closed) = replay(
        &mut state,
        vec![
            init(),
            echo(&prompt),
            json!({"type": "result", "is_error": true}),
        ],
    );
    assert_eq!(
        closed,
        vec![ClaudeResultOwner::Host(claude_host_prompt_owner(&prompt))]
    );
    state.reserve(claude_host_prompt_owner(&prompt));
    let (opened, closed) = replay(&mut state, vec![init(), echo(&prompt), result()]);
    assert_eq!(
        opened,
        vec![ClaudeFrameOwnership::OpenedHost(claude_host_prompt_owner(
            &prompt
        ))]
    );
    assert_eq!(
        closed,
        vec![ClaudeResultOwner::Host(claude_host_prompt_owner(&prompt))]
    );
}

#[test]
fn the_writer_reserves_the_exact_content_it_writes() {
    let prompt = ClaudePromptCommand {
        attachments: vec![PromptImageAttachment {
            data: "encoded".to_owned(),
            metadata: MessageImageAttachment {
                byte_size: 7,
                file_name: "a.png".to_owned(),
                media_type: "image/png".to_owned(),
            },
        }],
        replay_generation: "exact".to_owned(),
        text: "Line one.\n\nLine two.".to_owned(),
        turn_generation: 9,
    };
    let ownership = new_claude_turn_ownership();
    let mut writer = Vec::new();
    write_claude_runtime_command(
        &mut writer,
        &Arc::new(Mutex::new(None)),
        &ownership,
        ClaudeRuntimeCommand::Prompt(prompt.clone()),
    )
    .expect("prompt should be written");
    let written: Value = serde_json::from_slice(writer.trim_ascii_end()).expect("one NDJSON line");
    let reserved = lock_claude_turn_ownership(&ownership)
        .outstanding
        .front()
        .cloned()
        .expect("the owner is reserved");
    assert_eq!(reserved.content, written["message"]["content"]);
    assert_eq!(reserved.turn_generation, 9);
    assert_eq!(
        lock_claude_turn_ownership(&ownership).observe(&json!({
            "type": "user",
            "message": written["message"].clone(),
            "isReplay": true,
        })),
        ClaudeFrameOwnership::OpenedHost(reserved)
    );
    assert_eq!(
        written["uuid"], "exact",
        "the prompt is written with its replay generation as its uuid"
    );
}

// A runtime that reports message lifecycles (`msg_lifecycle_v1`): frames from
// live captures where TermAl wrote each prompt with a uuid
// (.tmp/claude-probe/probe9..11).

fn lifecycle(prompt: &ClaudePromptCommand, state: &str) -> Value {
    json!({"type": "command_lifecycle", "command_uuid": prompt.replay_generation,
        "state": state})
}

fn capable_init() -> Value {
    json!({"type": "system", "subtype": "init",
        "capabilities": ["interrupt_receipt_v1", "msg_lifecycle_v1"]})
}

fn named(mut frame: Value, prompts: &[&ClaudePromptCommand]) -> Value {
    let uuids = prompts
        .iter()
        .map(|prompt| prompt.replay_generation.clone())
        .collect::<Vec<_>>();
    if let Some(first) = uuids.first() {
        frame["user_message_uuid"] = json!(first);
    }
    frame["user_message_uuids"] = json!(uuids);
    frame
}

fn command_replay(prompt: &ClaudePromptCommand, name: &str) -> Value {
    json!({"type": "user", "uuid": prompt.replay_generation, "isReplay": true,
        "parent_tool_use_id": null, "message": {"role": "user",
        "content": format!("<command-name>{name}</command-name>")}})
}

fn lifecycle_prompt(text: &str, turn_generation: u64, uuid: &str) -> ClaudePromptCommand {
    ClaudePromptCommand {
        replay_generation: uuid.to_owned(),
        ..host_prompt(text, turn_generation)
    }
}

#[test]
fn a_native_command_is_owned_by_its_lifecycle_started_and_settled_by_its_result_identity() {
    // /context: started arrives before init, the answer carries no uuid, the
    // result names the command, and completed after it moves nothing.
    let context = lifecycle_prompt("/context", 21, "cccccccc-0000-4000-8000-000000000003");
    let mut state = ClaudeTurnOwnershipState::default();
    state.reserve(claude_host_prompt_owner(&context));
    let (opened, closed) = replay(
        &mut state,
        vec![
            lifecycle(&context, "queued"),
            lifecycle(&context, "started"),
            capable_init(),
            assistant_text("## Context Usage"),
            command_replay(&context, "/context"),
            named(result(), &[&context]),
            lifecycle(&context, "completed"),
        ],
    );
    assert_eq!(
        opened,
        vec![ClaudeFrameOwnership::OpenedHost(claude_host_prompt_owner(
            &context
        ))]
    );
    assert_eq!(
        closed,
        vec![ClaudeResultOwner::Host(claude_host_prompt_owner(&context))]
    );
    assert!(state.outstanding.is_empty() && state.open.is_none());

    // A resumed /compact: compaction status frames, an init after them, a
    // summary and the command's own replay, all inside the one owned turn.
    let compact = lifecycle_prompt("/compact", 22, "dddddddd-0000-4000-8000-000000000004");
    state.reserve(claude_host_prompt_owner(&compact));
    let (opened, closed) = replay(
        &mut state,
        vec![
            lifecycle(&compact, "queued"),
            lifecycle(&compact, "started"),
            json!({"type": "system", "subtype": "status", "status": "compacting"}),
            rate_limit(),
            json!({"type": "system", "subtype": "status", "status": null,
                "compact_result": "success"}),
            capable_init(),
            json!({"type": "system", "subtype": "compact_boundary"}),
            json!({"type": "user", "uuid": "summary", "isReplay": false,
                "message": {"role": "user", "content": "This session is being continued"}}),
            command_replay(&compact, "/compact"),
            named(result(), &[&compact]),
            lifecycle(&compact, "completed"),
        ],
    );
    assert_eq!(
        opened,
        vec![ClaudeFrameOwnership::OpenedHost(claude_host_prompt_owner(
            &compact
        ))]
    );
    assert_eq!(
        closed,
        vec![ClaudeResultOwner::Host(claude_host_prompt_owner(&compact))]
    );
}

#[test]
fn a_prompt_taken_up_inside_a_runtime_started_turn_is_settled_by_its_named_result() {
    // probe11 race: a notice turn is running when the prompt is written; the
    // runtime takes it up mid-turn (started inside the turn) and the one
    // result names it. There is no separate turn for it.
    let first = lifecycle_prompt(
        "background sleep",
        30,
        "aaaaaaaa-0000-4000-8000-000000000001",
    );
    let second = lifecycle_prompt("HOSTPROMPT", 31, "bbbbbbbb-0000-4000-8000-000000000002");
    let mut state = ClaudeTurnOwnershipState::default();
    state.reserve(claude_host_prompt_owner(&first));
    let (_, closed) = replay(
        &mut state,
        vec![
            lifecycle(&first, "queued"),
            lifecycle(&first, "started"),
            capable_init(),
            echo(&first),
            named(message_start(), &[&first]),
            assistant_tool_use("sleep 5 && echo done", true),
            tool_result("sleep 5 && echo done"),
            assistant_text("launched"),
            named(result(), &[&first]),
            lifecycle(&first, "completed"),
        ],
    );
    assert_eq!(
        closed,
        vec![ClaudeResultOwner::Host(claude_host_prompt_owner(&first))]
    );

    let (opened, _) = replay(
        &mut state,
        vec![
            task_notification("b4gz16mxt"),
            capable_init(),
            message_start(),
            assistant_tool_use("sleep 6", false),
        ],
    );
    assert_eq!(
        opened,
        vec![ClaudeFrameOwnership::OpenedRuntime {
            cause: ClaudeUnownedCause::TaskNotice
        }]
    );
    state.reserve(claude_host_prompt_owner(&second));
    let (opened, closed) = replay(
        &mut state,
        vec![
            lifecycle(&second, "queued"),
            tool_result("sleep 6"),
            echo(&second),
            lifecycle(&second, "started"),
            named(message_start(), &[&second]),
            assistant_text("finished"),
            lifecycle(&second, "completed"),
            named(result(), &[&second]),
        ],
    );
    assert_eq!(
        opened,
        vec![ClaudeFrameOwnership::JoinedHost(claude_host_prompt_owner(
            &second
        ))]
    );
    assert_eq!(
        closed,
        vec![ClaudeResultOwner::HostAfterUnowned(
            claude_host_prompt_owner(&second)
        )]
    );
}

#[test]
fn a_result_naming_no_host_identity_never_consumes_a_waiting_prompt() {
    // probe10: the notice turn's result names no uuid of TermAl's while a
    // prompt waits queued; the prompt keeps waiting for its own started.
    let waiting = lifecycle_prompt(
        "queued meanwhile",
        40,
        "eeeeeeee-0000-4000-8000-000000000005",
    );
    let mut state = ClaudeTurnOwnershipState::default();
    state.observe(&capable_init());
    state.reserve(claude_host_prompt_owner(&waiting));
    let (opened, closed) = replay(
        &mut state,
        vec![
            task_notification("b1"),
            lifecycle(&waiting, "queued"),
            capable_init(),
            assistant_text("finished"),
            result(),
        ],
    );
    assert_eq!(
        opened,
        vec![ClaudeFrameOwnership::OpenedRuntime {
            cause: ClaudeUnownedCause::TaskNotice
        }]
    );
    assert!(matches!(
        closed.as_slice(),
        [ClaudeResultOwner::Runtime(ClaudeRuntimeTurnOwner {
            cause: ClaudeUnownedCause::TaskNotice,
            ..
        })]
    ));
    assert_eq!(state.outstanding.len(), 1, "the prompt still waits");
}

#[test]
fn duplicate_and_late_lifecycle_frames_move_nothing() {
    let prompt = lifecycle_prompt("plain", 50, "ffffffff-0000-4000-8000-000000000006");
    let mut state = ClaudeTurnOwnershipState::default();
    state.reserve(claude_host_prompt_owner(&prompt));
    let (opened, closed) = replay(
        &mut state,
        vec![
            lifecycle(&prompt, "started"),
            lifecycle(&prompt, "started"),
            capable_init(),
            assistant_text("ok"),
            named(result(), &[&prompt, &prompt]),
            lifecycle(&prompt, "completed"),
            lifecycle(&prompt, "completed"),
        ],
    );
    assert_eq!(opened.len(), 1, "one opening: {opened:?}");
    assert_eq!(
        closed,
        vec![ClaudeResultOwner::Host(claude_host_prompt_owner(&prompt))],
        "a repeated identity is one owner, and completed settles nothing again"
    );
    assert!(state.open.is_none());
}

#[test]
fn identities_naming_two_prompts_or_none_on_a_capable_runtime_leave_the_turn_unresolved() {
    let one = lifecycle_prompt("one", 60, "11111111-0000-4000-8000-000000000007");
    let two = lifecycle_prompt("two", 61, "22222222-0000-4000-8000-000000000008");

    // A result naming two prompts settles neither.
    let mut state = ClaudeTurnOwnershipState::default();
    state.reserve(claude_host_prompt_owner(&one));
    state.reserve(claude_host_prompt_owner(&two));
    let (_, closed) = replay(
        &mut state,
        vec![
            lifecycle(&one, "started"),
            capable_init(),
            assistant_text("both?"),
            named(result(), &[&one, &two]),
        ],
    );
    assert_eq!(
        closed,
        vec![ClaudeResultOwner::Unresolved {
            ended_unowned: false,
            prompt_waiting: true,
            adopted_interval: None,
        }]
    );

    // A second prompt started inside the first one's turn makes it unresolved.
    let mut state = ClaudeTurnOwnershipState::default();
    state.reserve(claude_host_prompt_owner(&one));
    state.reserve(claude_host_prompt_owner(&two));
    let (opened, closed) = replay(
        &mut state,
        vec![
            lifecycle(&one, "started"),
            capable_init(),
            lifecycle(&two, "started"),
            named(result(), &[&two]),
        ],
    );
    assert_eq!(
        opened,
        vec![
            ClaudeFrameOwnership::OpenedHost(claude_host_prompt_owner(&one)),
            ClaudeFrameOwnership::BecameUnresolved,
        ]
    );
    assert!(matches!(
        closed.as_slice(),
        [ClaudeResultOwner::Unresolved { .. }]
    ));

    // On a capable runtime, a prompt's result that names nobody is unresolved,
    // and so is one whose singular identity is missing from its plural list.
    for bad in [
        result(),
        json!({"type": "result", "user_message_uuid": one.replay_generation,
            "user_message_uuids": [two.replay_generation]}),
    ] {
        let mut state = ClaudeTurnOwnershipState::default();
        state.reserve(claude_host_prompt_owner(&one));
        let (_, closed) = replay(
            &mut state,
            vec![lifecycle(&one, "started"), capable_init(), bad],
        );
        assert!(
            matches!(closed.as_slice(), [ClaudeResultOwner::Unresolved { .. }]),
            "{closed:?}"
        );
    }

    // A started for a uuid TermAl never wrote names nobody known.
    let mut state = ClaudeTurnOwnershipState::default();
    state.observe(&capable_init());
    assert_eq!(
        state.observe(&json!({"type": "command_lifecycle",
            "command_uuid": "99999999-0000-4000-8000-000000000009", "state": "started"})),
        ClaudeFrameOwnership::BecameUnresolved
    );
}

#[test]
fn a_prompt_started_inside_an_adopted_runtime_turn_is_unresolved() {
    let prompt = lifecycle_prompt("late", 70, "33333333-0000-4000-8000-00000000000a");
    let mut state = ClaudeTurnOwnershipState::default();
    state.observe(&capable_init());
    state.observe(&task_notification("b1"));
    state.observe(&assistant_text("noticed"));
    state.note_runtime_adoption(Some(9));
    state.reserve(claude_host_prompt_owner(&prompt));
    assert_eq!(
        state.observe(&lifecycle(&prompt, "started")),
        ClaudeFrameOwnership::BecameUnresolved
    );
}

#[test]
fn on_a_capable_runtime_an_echo_does_not_own_a_turn_and_subagent_frames_open_nothing() {
    let prompt = lifecycle_prompt("echoed", 80, "44444444-0000-4000-8000-00000000000b");
    let mut state = ClaudeTurnOwnershipState::default();
    state.observe(&capable_init());
    state.reserve(claude_host_prompt_owner(&prompt));
    assert_eq!(
        state.observe(&echo(&prompt)),
        ClaudeFrameOwnership::Unchanged
    );
    let mut subagent = assistant_text("from a subagent");
    subagent["parent_tool_use_id"] = json!("toolu-task");
    assert_eq!(state.observe(&subagent), ClaudeFrameOwnership::Unchanged);
    assert_eq!(
        state.observe(&lifecycle(&prompt, "started")),
        ClaudeFrameOwnership::OpenedHost(claude_host_prompt_owner(&prompt))
    );
}

#[test]
fn a_retried_prompt_is_written_under_a_fresh_uuid_owned_by_its_own_started() {
    let ownership = new_claude_turn_ownership();
    let prompt = lifecycle_prompt("retry me", 90, "55555555-0000-4000-8000-00000000000c");
    let replay_prompt = Arc::new(Mutex::new(None));
    let mut writer = Vec::new();
    write_claude_runtime_command(
        &mut writer,
        &replay_prompt,
        &ownership,
        ClaudeRuntimeCommand::Prompt(prompt.clone()),
    )
    .expect("prompt should be written");
    {
        let mut state = lock_claude_turn_ownership(&ownership);
        state.observe(&lifecycle(&prompt, "started"));
        let closed = state.close_for_result(&named(
            json!({"type": "result", "is_error": true, "api_error_status": 529}),
            &[&prompt],
        ));
        assert_eq!(
            closed,
            ClaudeResultOwner::Host(claude_host_prompt_owner(&prompt))
        );
    }
    write_claude_runtime_command(
        &mut writer,
        &replay_prompt,
        &ownership,
        ClaudeRuntimeCommand::RetryLastPrompt {
            ticket: ClaudeRetryTicket {
                replay_generation: prompt.replay_generation.clone(),
                turn_generation: 90,
                attempt: 1,
            },
            retry_detail: "Retrying.".to_owned(),
        },
    )
    .expect("retry should be written");
    let lines = String::from_utf8(writer).expect("UTF-8");
    let written = lines
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("NDJSON"))
        .collect::<Vec<_>>();
    assert_eq!(written.len(), 2);
    assert_eq!(written[0]["uuid"], json!(prompt.replay_generation));
    assert_ne!(
        written[1]["uuid"], written[0]["uuid"],
        "each attempt has its own uuid"
    );
    assert_eq!(
        written[1]["message"], written[0]["message"],
        "the retry writes the exact prompt again"
    );
    let retry_uuid = written[1]["uuid"].as_str().expect("uuid").to_owned();
    let mut state = lock_claude_turn_ownership(&ownership);
    assert_eq!(state.outstanding.len(), 1);
    assert_eq!(
        state.observe(&lifecycle(&prompt, "started")),
        ClaudeFrameOwnership::Unchanged,
        "a late started of the failed attempt opens nothing"
    );
    let retried = claude_host_prompt_attempt_owner(&prompt, 1, retry_uuid.clone());
    assert_eq!(
        state.observe(
            &json!({"type": "command_lifecycle", "command_uuid": retry_uuid,
            "state": "started"})
        ),
        ClaudeFrameOwnership::OpenedHost(retried)
    );
}

#[test]
fn a_subagents_lifecycle_and_result_frames_move_no_root_turn() {
    let prompt = lifecycle_prompt("root", 100, "66666666-0000-4000-8000-00000000000d");
    let ownership = new_claude_turn_ownership();
    let replay_prompt = Arc::new(Mutex::new(Some(prompt.clone())));
    let mut state = lock_claude_turn_ownership(&ownership);
    state.observe(&capable_init());
    state.reserve(claude_host_prompt_owner(&prompt));
    let mut nested_started = lifecycle(&prompt, "started");
    nested_started["parent_tool_use_id"] = json!("toolu-task");
    assert_eq!(
        state.observe(&nested_started),
        ClaudeFrameOwnership::Unchanged
    );
    assert_eq!(
        state.observe(&lifecycle(&prompt, "started")),
        ClaudeFrameOwnership::OpenedHost(claude_host_prompt_owner(&prompt))
    );
    drop(state);
    let mut nested_result = named(
        json!({"type": "result", "is_error": true, "api_error_status": 529}),
        &[&prompt],
    );
    nested_result["parent_tool_use_id"] = json!("toolu-task");
    let mut router = ClaudeFrameRouter::new(ownership.clone(), replay_prompt.clone(), "s");
    let plan = router.route(&nested_result, true);
    assert_eq!(plan.scope, ClaudeFrameScope::Nested);
    assert_eq!(
        plan.parser,
        ClaudeParserStep::Skip,
        "it resets no root parser state"
    );
    assert_eq!(plan.replay, ClaudeReplayStep::Retain);
    assert!(
        plan.retry.is_none() && plan.terminal.is_none(),
        "a subagent's result never retries, releases or ends the root prompt"
    );
    assert!(lock_claude_turn_ownership(&ownership).turn_is_open());
}

#[test]
fn captured_error_results_name_their_prompt_and_settle_it() {
    // probe12: --max-turns 1 ends in error_max_turns, and an interrupt in
    // error_during_execution followed by lifecycle `cancelled`; both results
    // name the prompt.
    for (subtype, after) in [
        ("error_max_turns", "completed"),
        ("error_during_execution", "cancelled"),
    ] {
        let prompt = lifecycle_prompt("a long run", 110, "eeeeeeee-1111-4000-8000-0000000000ee");
        let mut state = ClaudeTurnOwnershipState::default();
        state.reserve(claude_host_prompt_owner(&prompt));
        let (_, closed) = replay(
            &mut state,
            vec![
                lifecycle(&prompt, "queued"),
                lifecycle(&prompt, "started"),
                capable_init(),
                assistant_tool_use("sleep 20 && echo done", false),
                named(
                    json!({"type": "result", "subtype": subtype, "is_error": true}),
                    &[&prompt],
                ),
                lifecycle(&prompt, after),
            ],
        );
        assert_eq!(
            closed,
            vec![ClaudeResultOwner::Host(claude_host_prompt_owner(&prompt))],
            "{subtype}"
        );
        assert!(state.open.is_none());
    }
}
