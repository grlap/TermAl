// Claude turn ownership: which turn a frame on a Claude runtime's stdout
// belongs to.
//
// Owns: the state that one Claude runtime's prompt writer and its stdout
// reader share to decide, from the runtime's own frames, whether an open turn
// answers a prompt TermAl wrote (and which one) or was started by Claude Code
// itself (after a background-task notice, for example), and which turn a
// `result` ends.
//
// Does not own: session status, turn generations, Engram grants and
// checkpoints (`turn_lifecycle.rs`, `claude_runtime_turns.rs`,
// `engram_host_adapter.rs`), or how frames are rendered (`claude.rs`). The
// writer and reader wiring lives in `claude_spawn.rs`.
//
// New file; nothing was split out of another.
//
// The contract, from live Claude Code 2.1.285 stream-json captures with
// TermAl's flags (`--input-format stream-json --replay-user-messages`):
// - TermAl writes each attempt of a prompt with a top-level `uuid` of its
//   own: the prompt's replay generation for its first attempt, a fresh one
//   for each automatic retry. A runtime that advertises `msg_lifecycle_v1` in
//   `init` answers with `command_lifecycle` frames naming that uuid
//   (`queued`, `started`, `completed`), and the attempt's `result` (and some
//   of its assistant frames) name it in `user_message_uuid` /
//   `user_message_uuids`. That is the identity of the attempt's turn, native
//   slash commands included (/context, /cost and a resumed /compact are
//   answered without an echo but with their uuid). `started` can come before
//   `init`. An attempt that has ended is retired: a late frame naming it
//   alone (a duplicate `result`, a late `started`) moves no turn.
// - A turn Claude Code starts by itself (after a background-task notice) has
//   no `started` of TermAl's and its `result` names no uuid of TermAl's.
// - A prompt written while such a turn runs can be taken up inside it: its
//   `started` arrives mid-turn and the turn's `result` names it. Activity
//   before that point belongs to no prompt.
// - A runtime without the capability is read by the earlier, echo-based rules:
//   a turn whose first top-level `user` frame is the exact echo of one
//   waiting prompt is that prompt's; anything else is unowned or unassigned.
// - Missing, unknown, contradictory or plural identities that name more than
//   one prompt leave a turn unresolved: its result finalizes nothing. The
//   attempts that took part in it are retired with it all the same. A prefix
//   of such frames that no attempt or unowned turn took part in may still be
//   taken up by a waiting prompt's own `started` or result, as mixed.

/// A prompt TermAl wrote to the runtime whose own turn has not ended.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ClaudeHostPromptOwner {
    /// The session turn generation the prompt was dispatched under.
    turn_generation: u64,
    /// The replay generation of the written prompt (`ClaudePromptCommand`):
    /// one immutable identity for the logical prompt, retries included.
    replay_generation: String,
    /// The `uuid` this attempt of the prompt is written with, which the
    /// runtime's lifecycle frames and result name: the replay generation for
    /// the first attempt, a fresh one for each retry, so a late frame of an
    /// earlier attempt never names the next one.
    attempt_uuid: String,
    /// 0 for the first attempt, then one more for each automatic retry.
    attempt: u32,
    /// The exact `message.content` written, which a runtime without the
    /// lifecycle capability echoes back.
    content: Value,
}

/// Why a turn belongs to no prompt TermAl wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClaudeUnownedCause {
    /// Claude Code started it after a background-task notice.
    TaskNotice,
    /// Claude Code started it while no prompt of TermAl's was waiting.
    NoPrompt,
    /// A prompt was waiting but the turn cannot be tied to it. It is
    /// unassigned: never adopted, never credited, and it finalizes no other
    /// turn.
    Unassigned,
}

/// A turn that belongs to no prompt TermAl wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ClaudeRuntimeTurnOwner {
    cause: ClaudeUnownedCause,
    /// The session turn generation the host adopted it under; `None` while it
    /// is not adopted (not yet, the session was busy with another turn, or it
    /// is unassigned).
    adopted_generation: Option<u64>,
}

/// Whether the runtime reports message lifecycle identities.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ClaudeLifecycleSupport {
    /// Not yet said: no `init` and no lifecycle frame seen.
    #[default]
    Unknown,
    /// `init` advertised `msg_lifecycle_v1`, or a lifecycle frame arrived.
    Supported,
    /// `init` did not advertise it.
    Absent,
}

/// The turn a runtime is running now.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ClaudeOpenTurn {
    Host(ClaudeHostPromptOwner),
    /// A turn no prompt owned took up a waiting prompt part-way (its
    /// `started` arrived inside the turn).
    HostAfterUnowned {
        owner: ClaudeHostPromptOwner,
        unowned: ClaudeRuntimeTurnOwner,
    },
    Runtime(ClaudeRuntimeTurnOwner),
    /// A turn a prompt or an unowned turn took part in, whose identities then
    /// contradicted each other: nothing it does or ends is anyone's.
    /// `unowned` is set when it began as a turn no prompt owned.
    /// `participants` are the attempts (their uuids) positively observed in
    /// it: the prompt whose turn it was before it became unresolved, and any
    /// waiting prompt whose `started` arrived inside it. An attempt a frame
    /// merely names is not a participant. Its result retires every
    /// participant, so their late frames move nothing; that ends no prompt.
    /// It always has an owner or a participant; without either it is an
    /// `UnresolvedPrefix`.
    Unresolved {
        unowned: Option<ClaudeRuntimeTurnOwner>,
        participants: Vec<String>,
    },
    /// Top-level frames nothing could be tied to (an unknown, plural or
    /// malformed identity) while no turn was open: no prompt and no unowned
    /// turn took part. A waiting prompt's own `started`, or a result naming
    /// that prompt alone, takes it up as a turn no prompt owned would be
    /// taken up (`observe_lifecycle`, `close_open_for_result`).
    UnresolvedPrefix,
}

/// What closing the open turn for a `result` decided, from that one turn
/// and that one result: the turn it ended (`disposition`) and the attempts
/// that ended with it (`retired`), whether or not the result settles any.
/// `opened_by_result` says the result alone opened the attempt it settles
/// (no turn was open): only such an attempt may be read as having done
/// nothing before its result.
struct ClaudeTurnClose {
    disposition: ClaudeResultOwner,
    retired: Vec<String>,
    opened_by_result: bool,
}

/// What one frame did to the runtime's turns.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ClaudeFrameOwnership {
    /// The frame belongs to the open turn or to no turn.
    Unchanged,
    /// The frame opened the turn of a prompt TermAl wrote.
    OpenedHost(ClaudeHostPromptOwner),
    /// The frame opened a turn that belongs to no prompt TermAl wrote.
    OpenedRuntime { cause: ClaudeUnownedCause },
    /// The open unowned turn took up this waiting prompt.
    JoinedHost(ClaudeHostPromptOwner),
    /// The frame's identities contradict the open turn (or name nobody known)
    /// and the turn is unresolved from now on.
    BecameUnresolved,
}

/// The turn a `result` ended.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ClaudeResultOwner {
    Host(ClaudeHostPromptOwner),
    /// A turn that began with no owner and took up this prompt part-way: the
    /// prompt is settled, and its grant's attribution is mixed.
    HostAfterUnowned(ClaudeHostPromptOwner),
    Runtime(ClaudeRuntimeTurnOwner),
    /// No turn was open and the result names no prompt: it finalizes nothing.
    /// `prompt_waiting` says a prompt of TermAl's still waits for its turn.
    Uncorrelated { prompt_waiting: bool },
    /// The result names only attempts that have already ended: a late
    /// duplicate. It ends no turn and finalizes nothing.
    Stale,
    /// The result's identities are missing, unknown, contradictory or name
    /// more than one prompt: it finalizes no prompt's turn and no grant.
    /// `ended_unowned` says the turn began with no owner. `adopted_interval`
    /// is the retained owner of an interval the host adopted for a turn
    /// Claude Code started by itself, set only when that interval may be
    /// ended by this result: it kept its adopted generation, no prompt's
    /// attempt was part of it, and no prompt waits. The result then ends
    /// that observed interval; it never says the unknown identities belong
    /// to a prompt of TermAl's.
    Unresolved {
        ended_unowned: bool,
        prompt_waiting: bool,
        adopted_interval: Option<ClaudeRuntimeTurnOwner>,
    },
}

/// An automatic retry the reader scheduled for one attempt of a prompt. The
/// writer may write it only while it is still the one pending
/// (`claude_frame_router.rs`).
#[derive(Clone, Debug, PartialEq, Eq)]
struct ClaudeRetryTicket {
    replay_generation: String,
    /// The session turn generation the prompt was dispatched under.
    turn_generation: u64,
    /// The attempt the retry writes: 1 for the first retry.
    attempt: u32,
}

/// At most this many ended attempts are remembered as retired.
const CLAUDE_RETIRED_ATTEMPT_LIMIT: usize = 32;

#[derive(Debug, Default)]
struct ClaudeTurnOwnershipState {
    /// Prompts written (or being written), oldest first, whose turn has not
    /// opened yet.
    outstanding: VecDeque<ClaudeHostPromptOwner>,
    open: Option<ClaudeOpenTurn>,
    notice_since_result: bool,
    lifecycle: ClaudeLifecycleSupport,
    /// The uuids of attempts that have ended, newest last.
    retired: VecDeque<String>,
    /// The automatic retry scheduled and not yet written, if any.
    pending_retry: Option<ClaudeRetryTicket>,
}

type ClaudeTurnOwnership = Arc<Mutex<ClaudeTurnOwnershipState>>;

fn new_claude_turn_ownership() -> ClaudeTurnOwnership {
    Arc::new(Mutex::new(ClaudeTurnOwnershipState::default()))
}

fn lock_claude_turn_ownership(
    ownership: &ClaudeTurnOwnership,
) -> std::sync::MutexGuard<'_, ClaudeTurnOwnershipState> {
    ownership
        .lock()
        .expect("Claude turn ownership mutex poisoned")
}

/// The exact `message.content` TermAl writes for a prompt.
fn claude_prompt_content(prompt: &ClaudePromptCommand) -> Value {
    let mut content = Vec::new();
    if !prompt.text.trim().is_empty() {
        content.push(json!({
            "type": "text",
            "text": prompt.text.as_str(),
        }));
    }
    for attachment in &prompt.attachments {
        content.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": attachment.metadata.media_type.as_str(),
                "data": attachment.data.as_str(),
            }
        }));
    }
    Value::Array(content)
}

/// The owner of a prompt's first attempt, written with its replay generation
/// as its uuid.
fn claude_host_prompt_owner(prompt: &ClaudePromptCommand) -> ClaudeHostPromptOwner {
    claude_host_prompt_attempt_owner(prompt, 0, prompt.replay_generation.clone())
}

/// The owner of attempt `attempt` of a prompt, written with `attempt_uuid`.
fn claude_host_prompt_attempt_owner(
    prompt: &ClaudePromptCommand,
    attempt: u32,
    attempt_uuid: String,
) -> ClaudeHostPromptOwner {
    ClaudeHostPromptOwner {
        turn_generation: prompt.turn_generation,
        replay_generation: prompt.replay_generation.clone(),
        attempt_uuid,
        attempt,
        content: claude_prompt_content(prompt),
    }
}

/// Whether a `user` frame carries Claude Code's background-task notice.
fn claude_user_frame_is_task_notice(message: &Value) -> bool {
    const NOTICE: &str = "<task-notification>";
    match message.pointer("/message/content") {
        Some(Value::String(text)) => text.trim_start().starts_with(NOTICE),
        Some(Value::Array(blocks)) => blocks.first().is_some_and(|block| {
            block.get("type").and_then(Value::as_str) == Some("text")
                && block
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.trim_start().starts_with(NOTICE))
        }),
        _ => false,
    }
}

/// Whether a frame is turn output: the frames that open a turn when none is
/// open. Init, status, hook and task bookkeeping frames, telemetry and
/// `result` are not.
fn claude_frame_is_turn_output(message: &Value) -> bool {
    matches!(
        message.get("type").and_then(Value::as_str),
        Some("assistant" | "stream_event" | "control_request")
    )
}

/// Whether a frame belongs to the top-level conversation: a subagent's frames
/// carry the tool use they belong to, and neither open nor end a turn.
fn claude_frame_is_top_level(message: &Value) -> bool {
    message
        .get("parent_tool_use_id")
        .is_none_or(Value::is_null)
}

/// The host message identities a frame names (`user_message_uuid` and
/// `user_message_uuids`), deduplicated. `Err` when they are malformed or the
/// singular one is missing from the plural list.
fn claude_frame_message_uuids(message: &Value) -> std::result::Result<Vec<String>, ()> {
    let mut uuids = Vec::new();
    match message.get("user_message_uuids") {
        None | Some(Value::Null) => {}
        Some(Value::Array(values)) => {
            for value in values {
                let uuid = value.as_str().ok_or(())?;
                if !uuids.iter().any(|known: &String| known == uuid) {
                    uuids.push(uuid.to_owned());
                }
            }
        }
        Some(_) => return Err(()),
    }
    match message.get("user_message_uuid") {
        None | Some(Value::Null) => {}
        Some(Value::String(uuid)) => {
            if message.get("user_message_uuids").is_some_and(|plural| !plural.is_null()) {
                if !uuids.iter().any(|known| known == uuid) {
                    return Err(());
                }
            } else {
                uuids.push(uuid.clone());
            }
        }
        Some(_) => return Err(()),
    }
    Ok(uuids)
}

impl ClaudeTurnOwnershipState {
    /// The writer is about to write `owner`'s attempt. A retry writes the
    /// same prompt again under a fresh uuid: its reservation replaces any
    /// still waiting for that prompt, so one logical prompt is reserved once.
    /// A prompt's first attempt supersedes any retry still pending.
    fn reserve(&mut self, owner: ClaudeHostPromptOwner) {
        self.outstanding
            .retain(|waiting| waiting.replay_generation != owner.replay_generation);
        if owner.attempt == 0 {
            self.pending_retry = None;
        }
        self.outstanding.push_back(owner);
    }

    /// The write of the attempt with `attempt_uuid` failed: it has no turn.
    fn release(&mut self, attempt_uuid: &str) {
        if let Some(position) = self
            .outstanding
            .iter()
            .rposition(|owner| owner.attempt_uuid == attempt_uuid)
        {
            self.outstanding.remove(position);
        }
    }

    /// Whether a turn is open.
    fn turn_is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Whether a written prompt still waits for its turn.
    fn prompt_is_waiting(&self) -> bool {
        !self.outstanding.is_empty()
    }

    /// Whether the runtime reports message lifecycles (`msg_lifecycle_v1`).
    fn reports_lifecycles(&self) -> bool {
        self.lifecycle == ClaudeLifecycleSupport::Supported
    }

    /// Whether `message` is a `user` frame whose content is exactly that of
    /// a prompt still waiting for its turn. It proves nothing about
    /// ownership (`observe` decides that); it only says the frame is that
    /// prompt's echo and not some other user content.
    fn is_exact_echo_of_waiting_prompt(&self, message: &Value) -> bool {
        message.get("type").and_then(Value::as_str) == Some("user")
            && message.pointer("/message/content").is_some_and(|content| {
                self.outstanding.iter().any(|owner| &owner.content == content)
            })
    }

    /// The attempt `attempt_uuid` ended: a late frame naming it alone moves
    /// no turn.
    fn retire_attempt(&mut self, attempt_uuid: &str) {
        if self.is_retired(attempt_uuid) {
            return;
        }
        if self.retired.len() >= CLAUDE_RETIRED_ATTEMPT_LIMIT {
            self.retired.pop_front();
        }
        self.retired.push_back(attempt_uuid.to_owned());
    }

    fn is_retired(&self, uuid: &str) -> bool {
        self.retired.iter().any(|retired| retired == uuid)
    }

    /// Whether `frame` names at least one attempt, and only attempts that
    /// have ended.
    fn names_only_retired(&self, frame: &Value) -> bool {
        claude_frame_message_uuids(frame)
            .is_ok_and(|uuids| !uuids.is_empty() && uuids.iter().all(|uuid| self.is_retired(uuid)))
    }

    /// The reader scheduled `ticket`: the writer may write it while it stays
    /// the one pending.
    fn schedule_retry(&mut self, ticket: ClaudeRetryTicket) {
        self.pending_retry = Some(ticket);
    }

    fn retry_is_pending(&self, ticket: &ClaudeRetryTicket) -> bool {
        self.pending_retry.as_ref() == Some(ticket)
    }

    /// Takes `ticket` if it is still the retry pending.
    fn take_pending_retry(&mut self, ticket: &ClaudeRetryTicket) -> bool {
        let pending = self.retry_is_pending(ticket);
        if pending {
            self.pending_retry = None;
        }
        pending
    }

    fn take_outstanding(&mut self, uuid: &str) -> Option<ClaudeHostPromptOwner> {
        let position = self
            .outstanding
            .iter()
            .position(|owner| owner.attempt_uuid == uuid)?;
        self.outstanding.remove(position)
    }

    fn open_turn(&mut self, turn: ClaudeOpenTurn, ownership: ClaudeFrameOwnership) -> ClaudeFrameOwnership {
        self.open = Some(turn);
        ownership
    }

    fn open_runtime(&mut self, cause: ClaudeUnownedCause) -> ClaudeFrameOwnership {
        self.open_turn(
            ClaudeOpenTurn::Runtime(ClaudeRuntimeTurnOwner {
                cause,
                adopted_generation: None,
            }),
            ClaudeFrameOwnership::OpenedRuntime { cause },
        )
    }

    /// The open turn's identities contradict a frame: it is unresolved now.
    /// The attempts that took part in it stay known (`participants`).
    fn unresolve(&mut self) -> ClaudeFrameOwnership {
        self.open = Some(match self.open.take() {
            Some(ClaudeOpenTurn::Runtime(unowned)) => ClaudeOpenTurn::Unresolved {
                unowned: Some(unowned),
                participants: Vec::new(),
            },
            Some(ClaudeOpenTurn::HostAfterUnowned { owner, unowned }) => {
                ClaudeOpenTurn::Unresolved {
                    unowned: Some(unowned),
                    participants: vec![owner.attempt_uuid],
                }
            }
            Some(ClaudeOpenTurn::Host(owner)) => ClaudeOpenTurn::Unresolved {
                unowned: None,
                participants: vec![owner.attempt_uuid],
            },
            Some(unresolved @ ClaudeOpenTurn::Unresolved { .. }) => unresolved,
            Some(ClaudeOpenTurn::UnresolvedPrefix) | None => ClaudeOpenTurn::UnresolvedPrefix,
        });
        ClaudeFrameOwnership::BecameUnresolved
    }

    /// The prompt attempt whose own turn is open now, if any: a turn taken up
    /// part-way, an unowned or an unresolved turn is none.
    fn open_host_owner(&self) -> Option<&ClaudeHostPromptOwner> {
        match &self.open {
            Some(ClaudeOpenTurn::Host(owner)) => Some(owner),
            _ => None,
        }
    }

    /// The waiting prompt `owner` started inside the open unresolved turn:
    /// it takes part in it and waits no longer.
    fn join_unresolved(&mut self, owner: ClaudeHostPromptOwner) {
        if let Some(ClaudeOpenTurn::Unresolved { participants, .. }) = &mut self.open {
            participants.push(owner.attempt_uuid);
        }
    }

    /// The retained owner of an unresolved interval that a top-level result
    /// may end (`ClaudeResultOwner::Unresolved::adopted_interval`): one the
    /// host adopted (with its generation) for a turn Claude Code started by
    /// itself, with no prompt's attempt part of it and none waiting.
    fn adopted_interval(
        &self,
        unowned: Option<ClaudeRuntimeTurnOwner>,
        participants: &[String],
    ) -> Option<ClaudeRuntimeTurnOwner> {
        unowned.filter(|owner| {
            owner.adopted_generation.is_some()
                && owner.cause != ClaudeUnownedCause::Unassigned
                && participants.is_empty()
                && self.outstanding.is_empty()
        })
    }

    /// The stand-in owner of an unresolved prefix a waiting prompt took up:
    /// what ran before the prompt started is unassigned, and was never
    /// adopted.
    fn unattributed_prefix_owner() -> ClaudeRuntimeTurnOwner {
        ClaudeRuntimeTurnOwner {
            cause: ClaudeUnownedCause::Unassigned,
            adopted_generation: None,
        }
    }

    /// Records what `message` does to the runtime's turns. `result` frames
    /// are closed by `close_for_result`, not here.
    fn observe(&mut self, message: &Value) -> ClaudeFrameOwnership {
        // A subagent's frames (lifecycle, system and output alike) carry the
        // tool use they belong to; they neither open nor end nor contradict
        // the root turn.
        if !claude_frame_is_top_level(message) {
            return ClaudeFrameOwnership::Unchanged;
        }
        let frame_type = message.get("type").and_then(Value::as_str);
        match frame_type {
            Some("command_lifecycle") => {
                self.lifecycle = ClaudeLifecycleSupport::Supported;
                return self.observe_lifecycle(message);
            }
            Some("system") => {
                match message.get("subtype").and_then(Value::as_str) {
                    Some("task_notification") if self.open.is_none() => {
                        self.notice_since_result = true;
                    }
                    Some("init") => {
                        let advertised = message
                            .get("capabilities")
                            .and_then(Value::as_array)
                            .is_some_and(|capabilities| {
                                capabilities
                                    .iter()
                                    .any(|capability| capability == "msg_lifecycle_v1")
                            });
                        if advertised {
                            self.lifecycle = ClaudeLifecycleSupport::Supported;
                        } else if self.lifecycle == ClaudeLifecycleSupport::Unknown {
                            self.lifecycle = ClaudeLifecycleSupport::Absent;
                        }
                    }
                    _ => {}
                }
                return ClaudeFrameOwnership::Unchanged;
            }
            _ => {}
        }
        if frame_type == Some("user") {
            if self.open.is_some() {
                // A mid-turn echo, a tool result or a replayed command frame
                // moves nothing: the lifecycle frames name owners.
                return ClaudeFrameOwnership::Unchanged;
            }
            if self.lifecycle != ClaudeLifecycleSupport::Supported {
                // A runtime without lifecycle identities (or one that has not
                // said yet: every capture announces it before any echo) is
                // read by its exact opening echo.
                if let Some(content) = message.pointer("/message/content") {
                    let matching = self
                        .outstanding
                        .iter()
                        .enumerate()
                        .filter(|(_, owner)| &owner.content == content)
                        .map(|(position, _)| position)
                        .collect::<Vec<_>>();
                    match matching.as_slice() {
                        [position] => {
                            let owner = self
                                .outstanding
                                .remove(*position)
                                .expect("matched outstanding prompt should exist");
                            return self.open_turn(
                                ClaudeOpenTurn::Host(owner.clone()),
                                ClaudeFrameOwnership::OpenedHost(owner),
                            );
                        }
                        // Identical text is no unique owner: the turn stays
                        // unassigned and both prompts keep waiting.
                        [_, _, ..] => return self.open_runtime(ClaudeUnownedCause::Unassigned),
                        [] => {}
                    }
                }
            }
            if claude_user_frame_is_task_notice(message) {
                return self.open_runtime(ClaudeUnownedCause::TaskNotice);
            }
            return ClaudeFrameOwnership::Unchanged;
        }
        if !claude_frame_is_turn_output(message) {
            return ClaudeFrameOwnership::Unchanged;
        }
        // Late output of an attempt that has ended belongs to no turn open
        // now; the router feeds it nowhere (`claude_frame_router.rs`).
        if self.names_only_retired(message) {
            return ClaudeFrameOwnership::Unchanged;
        }
        let Ok(uuids) = claude_frame_message_uuids(message) else {
            return self.unresolve();
        };
        match &self.open {
            Some(ClaudeOpenTurn::Host(owner))
            | Some(ClaudeOpenTurn::HostAfterUnowned { owner, .. }) => {
                if uuids.is_empty() || uuids == [owner.attempt_uuid.clone()] {
                    ClaudeFrameOwnership::Unchanged
                } else {
                    self.unresolve()
                }
            }
            Some(ClaudeOpenTurn::Runtime(_)) => {
                if uuids.is_empty() {
                    ClaudeFrameOwnership::Unchanged
                } else {
                    self.unresolve()
                }
            }
            Some(ClaudeOpenTurn::Unresolved { .. } | ClaudeOpenTurn::UnresolvedPrefix) => {
                ClaudeFrameOwnership::Unchanged
            }
            None => match uuids.as_slice() {
                // Turn output that names one waiting prompt is its turn.
                [uuid] => match self.take_outstanding(uuid) {
                    Some(owner) => self.open_turn(
                        ClaudeOpenTurn::Host(owner.clone()),
                        ClaudeFrameOwnership::OpenedHost(owner),
                    ),
                    None => self.unresolve(),
                },
                [_, _, ..] => self.unresolve(),
                [] => {
                    let cause = if self.notice_since_result {
                        ClaudeUnownedCause::TaskNotice
                    } else if self.outstanding.is_empty() {
                        ClaudeUnownedCause::NoPrompt
                    } else {
                        // Turn output with nothing tying it to the waiting
                        // prompt: it may answer it, but nothing proves it.
                        ClaudeUnownedCause::Unassigned
                    };
                    self.open_runtime(cause)
                }
            },
        }
    }

    /// A `command_lifecycle` frame: `started` names the prompt whose turn it
    /// is. `queued` proves only receipt, and `completed` or `cancelled` (an
    /// interrupted prompt, after its result) is bookkeeping; none moves a
    /// turn.
    fn observe_lifecycle(&mut self, message: &Value) -> ClaudeFrameOwnership {
        if message.get("state").and_then(Value::as_str) != Some("started") {
            return ClaudeFrameOwnership::Unchanged;
        }
        let Some(uuid) = message.get("command_uuid").and_then(Value::as_str) else {
            return self.unresolve();
        };
        // A late `started` of an attempt that has ended moves nothing.
        if self.is_retired(uuid) {
            return ClaudeFrameOwnership::Unchanged;
        }
        match self.open.clone() {
            None => match self.take_outstanding(uuid) {
                Some(owner) => self.open_turn(
                    ClaudeOpenTurn::Host(owner.clone()),
                    ClaudeFrameOwnership::OpenedHost(owner),
                ),
                None => self.unresolve(),
            },
            Some(ClaudeOpenTurn::Host(owner))
            | Some(ClaudeOpenTurn::HostAfterUnowned { owner, .. })
                if owner.attempt_uuid == uuid =>
            {
                ClaudeFrameOwnership::Unchanged
            }
            Some(ClaudeOpenTurn::Runtime(unowned)) if unowned.adopted_generation.is_none() => {
                match self.take_outstanding(uuid) {
                    Some(owner) => self.open_turn(
                        ClaudeOpenTurn::HostAfterUnowned {
                            owner: owner.clone(),
                            unowned,
                        },
                        ClaudeFrameOwnership::JoinedHost(owner),
                    ),
                    None => self.unresolve(),
                }
            }
            // A prefix nothing could be tied to: the waiting prompt's own
            // start takes it up as a turn no prompt owned would be taken up.
            // What ran before stays unassigned, the prompt's grant is marked
            // mixed, the replay barrier stays, and the prompt is never
            // retried; its own result still settles it.
            Some(ClaudeOpenTurn::UnresolvedPrefix) => match self.take_outstanding(uuid) {
                Some(owner) => self.open_turn(
                    ClaudeOpenTurn::HostAfterUnowned {
                        owner: owner.clone(),
                        unowned: Self::unattributed_prefix_owner(),
                    },
                    ClaudeFrameOwnership::JoinedHost(owner),
                ),
                None => ClaudeFrameOwnership::Unchanged,
            },
            // An unresolved turn a prompt or an unowned turn took part in: a
            // prompt that starts inside it takes part in it too, and nothing
            // is taken up.
            Some(ClaudeOpenTurn::Unresolved { .. }) => {
                if let Some(owner) = self.take_outstanding(uuid) {
                    self.join_unresolved(owner);
                }
                ClaudeFrameOwnership::Unchanged
            }
            // Another prompt started inside a prompt's turn, or inside a turn
            // the host adopted: two owners for one turn, and both took part.
            Some(_) => {
                let started = self.take_outstanding(uuid);
                let observed = self.unresolve();
                if let Some(owner) = started {
                    self.join_unresolved(owner);
                }
                observed
            }
        }
    }

    /// The host adopted the open runtime-started turn under `generation`.
    fn note_runtime_adoption(&mut self, generation: Option<u64>) {
        if let Some(ClaudeOpenTurn::Runtime(owner)) = &mut self.open {
            owner.adopted_generation = generation;
        }
    }

    /// A top-level `result` arrived: it ends the open turn and only that one,
    /// and it settles a prompt only when its identities name that prompt
    /// alone. A subagent's result (`claude_frame_is_top_level`) is not passed
    /// here: the reader closes no turn for it. Captured error results name
    /// their prompt as success results do (`error_max_turns`, and
    /// `error_during_execution` after an interrupt).
    #[cfg(test)]
    fn close_for_result(&mut self, result: &Value) -> ClaudeResultOwner {
        self.close_for_result_with_opening(result).0
    }

    /// `close_for_result`, also saying whether the result alone opened the
    /// attempt it settles (`ClaudeTurnClose::opened_by_result`).
    fn close_for_result_with_opening(&mut self, result: &Value) -> (ClaudeResultOwner, bool) {
        // A late result of an attempt that has ended (a duplicate terminal)
        // ends nothing, the turn open now included.
        if self.names_only_retired(result) {
            return (ClaudeResultOwner::Stale, false);
        }
        // The one close transition: what the turn's end means and which
        // attempts ended with it, decided together; retirement happens here
        // only.
        let closed = self.close_open_for_result(result);
        for attempt in &closed.retired {
            self.retire_attempt(attempt);
        }
        (closed.disposition, closed.opened_by_result)
    }

    /// Closes the open turn for `result`: from that one captured turn and
    /// that one result, the turn it ended and the attempts that ended with
    /// it. An attempt whose turn closes unresolved ends all the same, so its
    /// late frames move nothing; that settles no prompt. A prompt the result
    /// merely names, but which never took part, keeps waiting.
    fn close_open_for_result(&mut self, result: &Value) -> ClaudeTurnClose {
        self.notice_since_result = false;
        let supported = self.lifecycle == ClaudeLifecycleSupport::Supported;
        let uuids = claude_frame_message_uuids(result);
        let names_only = |owner: &ClaudeHostPromptOwner| {
            uuids
                .as_ref()
                .is_ok_and(|uuids| uuids == &[owner.attempt_uuid.clone()])
        };
        let open = self.open.take();
        let unresolved = |state: &Self, ended_unowned: bool| ClaudeResultOwner::Unresolved {
            ended_unowned,
            prompt_waiting: !state.outstanding.is_empty(),
            adopted_interval: None,
        };
        let unresolved_interval =
            |state: &Self, unowned: Option<ClaudeRuntimeTurnOwner>, participants: &[String]| {
                ClaudeResultOwner::Unresolved {
                    ended_unowned: unowned.is_some(),
                    prompt_waiting: !state.outstanding.is_empty(),
                    adopted_interval: state.adopted_interval(unowned, participants),
                }
            };
        let close = |disposition: ClaudeResultOwner, retired: Vec<String>| ClaudeTurnClose {
            disposition,
            retired,
            opened_by_result: false,
        };
        match open {
            Some(ClaudeOpenTurn::Host(owner)) => {
                let attempt = vec![owner.attempt_uuid.clone()];
                // A runtime without lifecycle identities names nobody in its
                // results; its turn was tied by the echo.
                if !supported || names_only(&owner) {
                    close(ClaudeResultOwner::Host(owner), attempt)
                } else {
                    close(unresolved(self, false), attempt)
                }
            }
            Some(ClaudeOpenTurn::HostAfterUnowned { owner, .. }) => {
                let attempt = vec![owner.attempt_uuid.clone()];
                if names_only(&owner) {
                    close(ClaudeResultOwner::HostAfterUnowned(owner), attempt)
                } else {
                    close(unresolved(self, true), attempt)
                }
            }
            Some(ClaudeOpenTurn::Runtime(owner)) => match &uuids {
                Ok(uuids) if uuids.is_empty() => {
                    close(ClaudeResultOwner::Runtime(owner), Vec::new())
                }
                _ => close(unresolved_interval(self, Some(owner), &[]), Vec::new()),
            },
            Some(ClaudeOpenTurn::Unresolved {
                unowned,
                participants,
            }) => close(
                unresolved_interval(self, unowned, &participants),
                participants,
            ),
            // A prefix nothing could be tied to, ended by a result that names
            // one waiting prompt alone: that prompt's turn took the prefix
            // up. It is settled as a turn no prompt owned that took up a
            // prompt is: mixed, never retried. Any other result ends only the
            // prefix.
            Some(ClaudeOpenTurn::UnresolvedPrefix) => match uuids.as_deref() {
                Ok([uuid]) => match self.take_outstanding(uuid) {
                    Some(owner) => {
                        let attempt = vec![owner.attempt_uuid.clone()];
                        close(ClaudeResultOwner::HostAfterUnowned(owner), attempt)
                    }
                    None => close(unresolved(self, false), Vec::new()),
                },
                _ => close(unresolved(self, false), Vec::new()),
            },
            None => match uuids.as_deref() {
                Ok([]) => close(
                    ClaudeResultOwner::Uncorrelated {
                        prompt_waiting: !self.outstanding.is_empty(),
                    },
                    Vec::new(),
                ),
                // A result that names one waiting prompt alone settles it;
                // that result alone opened the attempt.
                Ok([uuid]) => match self.take_outstanding(uuid) {
                    Some(owner) => {
                        let attempt = vec![owner.attempt_uuid.clone()];
                        ClaudeTurnClose {
                            opened_by_result: true,
                            ..close(ClaudeResultOwner::Host(owner), attempt)
                        }
                    }
                    None => close(unresolved(self, false), Vec::new()),
                },
                _ => close(unresolved(self, false), Vec::new()),
            },
        }
    }
}
