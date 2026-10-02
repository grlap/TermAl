// Claude work that outlives its frame: who produced a recorder observation,
// what the session's grant may take from it, and the work a Claude runtime
// has started that may still run and write without any later frame.
//
// Owns: the provenance a Claude frame's observations carry to the Engram
// sink (the runtime and the prompt turn the frame belongs to), the one
// disposition the sink gives an observation under the state lock (credited
// to the session's grant, kept for no grant, or a replaced runtime's), the
// exclusion of the live grant from work its root turn cannot be credited
// with (marked mixed, its open checks fenced), and the runtime-scoped record
// of outstanding work: shell commands, subagent work and background
// launches, registered when a frame starts them, with where each may write,
// and retired only on a correlated end from the runtime that started them (a
// foreground tool result, a background task's terminal notification).
// Nothing else releases it: not a Stop, a runtime replacement or exit, a
// parent's end, an abandoned call, or the session's deletion, which moves it
// to the host. Subagent work is never credited to a grant. While background,
// subagent or orphaned work is outstanding, the session's overlapping grants
// are mixed and its checks fenced, other sessions' checks are fenced where
// that work may write, and each says why: a transcript notice, a line for the
// agent's next prompt, the cause on each refused check, and the requester's
// evaluation notice.
//
// Does not own: which turn a frame belongs to (`claude_turn_ownership.rs`)
// or which root tool use launched a subagent (`claude_frame_router.rs`), the
// handlers that act on an observation (`engram_turn_checks.rs`), the grant's
// begin and its report (`engram_host_adapter.rs`), or turns no prompt owns
// (`claude_runtime_turns.rs`).
//
// New file; nothing was split out of another. It replaces the per-frame
// foreign scope that `claude_runtime_turns.rs` held.

/// At most this many outstanding work entries are kept per session; past
/// that, the oldest detail is dropped and the work is kept as unknown, which
/// no later frame can release.
const CLAUDE_OUTSTANDING_WORK_LIMIT: usize = 256;

/// The turn a Claude frame's work belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ClaudeWorkOrigin {
    /// The prompt attempt dispatched under this session turn generation.
    Attempt { turn_generation: u64 },
    /// No prompt attempt: a turn no prompt owned, an unresolved turn, no turn,
    /// or a subagent whose task the open attempt did not launch.
    Unattributed,
}

/// Who produced a Claude recorder observation: the runtime that read the
/// frame and the turn the frame belongs to. Fixed before the frame's parser
/// or control step runs, and checked again where the observation is applied.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ClaudeObservationProvenance {
    token: RuntimeToken,
    origin: ClaudeWorkOrigin,
}

/// The provenance a recorder gives its observations. `Ambient` is that of a
/// runtime with no turn ownership of its own (Codex, ACP) and of a test
/// driving a recorder directly; the Claude reader gives every frame `Claude`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum EngramObservationProvenance {
    #[default]
    Ambient,
    Claude(ClaudeObservationProvenance),
}

/// What the sink does with one observation, decided under the state lock in
/// the section that applies it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClaudeObservationDisposition {
    /// The session's own turn: attributed to its grant as ever.
    Current,
    /// A turn no prompt owns is open on the session and hides what the
    /// recorder sees (`claude_runtime_turns.rs`): kept as unassigned and
    /// attributed to no grant. That turn's own transitions mark the grant it
    /// overlaps.
    Unowned,
    /// Work of another turn on the same runtime: kept as unassigned, the
    /// live grant excluded from it, nothing attributed.
    Foreign,
    /// A replaced runtime's: nothing attributed and nothing kept on the
    /// session. Only what proves new activity (a command starting) excludes
    /// the live grant; a buffered result or edit report of the old runtime
    /// does not show that the work ran during the live grant.
    Replaced,
}

/// The disposition of an observation of `provenance` on `record`, now.
fn claude_observation_disposition(
    record: &SessionRecord,
    provenance: &EngramObservationProvenance,
) -> ClaudeObservationDisposition {
    let provenance = match provenance {
        EngramObservationProvenance::Ambient => {
            return if unmediated_claude_turn_hides_observations(record) {
                ClaudeObservationDisposition::Unowned
            } else {
                ClaudeObservationDisposition::Current
            };
        }
        EngramObservationProvenance::Claude(provenance) => provenance,
    };
    if !record.runtime.matches_runtime_token(&provenance.token) {
        return ClaudeObservationDisposition::Replaced;
    }
    match provenance.origin {
        ClaudeWorkOrigin::Attempt { turn_generation }
            if turn_generation != record.active_turn_generation =>
        {
            ClaudeObservationDisposition::Foreign
        }
        _ if unmediated_claude_turn_hides_observations(record) => {
            ClaudeObservationDisposition::Unowned
        }
        ClaudeWorkOrigin::Attempt { .. } => ClaudeObservationDisposition::Current,
        ClaudeWorkOrigin::Unattributed => ClaudeObservationDisposition::Foreign,
    }
}

impl ClaudeObservationDisposition {
    /// Whether the live grant must be excluded from the work: it is another
    /// turn's or a replaced runtime's.
    fn excludes_live_grant(self) -> bool {
        matches!(self, Self::Foreign | Self::Replaced)
    }
}

/// Why a check or an evaluation is refused credit for outstanding Claude
/// work: told from the same retained facts that fenced it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ClaudeHazardCause {
    /// The session's own Claude work with no verified completion: the
    /// session's provenance is unresolved, whatever workspace it measures.
    OwnSession,
    /// Another session's Claude work that may write in the workspace.
    OtherSession { session_id: String, name: String },
    /// Work a deleted session left with no verified completion.
    DeletedSession { session_id: String },
}

impl ClaudeHazardCause {
    fn describe(&self) -> String {
        match self {
            Self::OwnSession => "this session's own Claude work (a background or subagent task, \
                 or a command a stopped or replaced runtime started) has no verified completion, \
                 so its later work cannot be told apart from it in any workspace"
                .to_owned(),
            Self::OtherSession { session_id, name } => format!(
                "Claude work of session {name} ({session_id}) may still be writing in this \
                 workspace, with no verified completion"
            ),
            Self::DeletedSession { session_id } => format!(
                "Claude work a deleted session ({session_id}) left may still be writing in this \
                 workspace, with no verified completion"
            ),
        }
    }
}

/// The notice a session gives while its evidence is restricted by Claude
/// work it cannot see end.
fn claude_evidence_restricted_notice(record: &SessionRecord) -> String {
    let work = &record.claude_outstanding;
    let current = record.runtime.runtime_token();
    let hazards = work.hazard_count(current.as_ref(), record.active_turn_generation);
    let orphaned = work.unknown
        || work
            .entries
            .iter()
            .any(|entry| Some(&entry.token) != current.as_ref());
    let what = match (hazards, work.unknown) {
        (0, false) => "Claude work of another turn or runtime ran in this grant's interval, with \
                       no verified completion"
            .to_owned(),
        (count, unknown) => format!(
            "{count} piece{} of background, subagent or orphaned Claude work{} may still be \
             running, with no verified completion",
            if count == 1 { "" } else { "s" },
            if unknown {
                " (and more TermAl could not keep track of)"
            } else {
                ""
            }
        ),
    };
    let orphan = if orphaned {
        " Some of it belongs to a runtime that was stopped or replaced, which can never report \
         its end."
    } else {
        ""
    };
    format!(
        "Evidence restricted for this session (workdir {}): {what}.{orphan} Prompts keep \
         working, but while it lasts this session's tests may pass without earning verification \
         credit, in any workspace, and an acceptance result that needs that credit cannot pass \
         on those runs; a grant that overlaps it keeps its own source report withheld. Other \
         sessions are refused credit only for tests in the workspaces this work may write in. \
         Stopping the session, replacing its runtime, deleting it or starting a fresh session in \
         the same workspace does not show that this work finished. Eligibility returns when \
         Claude reports the work ended. TermAl has no reset for this.",
        record.session.workdir
    )
}

/// The notice a session gives when no Claude work restricts its evidence
/// any more.
fn claude_evidence_released_notice(record: &SessionRecord) -> String {
    format!(
        "Evidence restriction lifted for this session (workdir {}): the Claude work that \
         restricted it has reported its end. Tests run from now on can earn verification credit \
         again; a grant that overlapped that work keeps its source report withheld, and a check \
         refused then stays refused.",
        record.session.workdir
    )
}

/// Excludes the session's live grant from work that is not its own: the
/// grant is marked mixed, so its own source report is withheld as uncertain,
/// and its checks still open to writes are fenced, with the session's own
/// unresolved work as the stored cause. Called under the state lock, before
/// or in the same section as the work's effects. The mark is sticky for the
/// grant; nothing clears it but the next grant. A newly effective exclusion
/// makes the session's notice due. `self_gate` names the call of a
/// recognised simple full gate whose own promotion is the exclusion: its own
/// check is not fenced by it (`claude_session_work_restricts_except`).
fn claude_exclude_live_grant(record: &mut SessionRecord, self_gate: Option<&str>) {
    for check in &mut record.engram.active_turn_checks {
        if check.open_to_writes() && Some(check.key.as_str()) != self_gate {
            check.overlapped = true;
            check
                .fenced_by_outstanding
                .get_or_insert(ClaudeHazardCause::OwnSession);
        }
    }
    let Some(grant_id) = record.engram.active_grant_id.clone() else {
        return;
    };
    if record.engram.active_turn_mixed_attribution.as_deref() == Some(grant_id.as_str()) {
        return;
    }
    record.engram.active_turn_mixed_attribution = Some(grant_id.clone());
    record.claude_outstanding.notice_due = true;
    push_unassigned_claude_observation(
        record,
        format!(
            "Claude work grant {grant_id}'s own root turn cannot be credited with ran in its \
             measurement interval; what changed may be that work's, so the grant's own source \
             report is withheld as uncertain and its open checks are fenced"
        ),
    );
}

/// Marks the grant `grant_id`, beginning now, mixed when Claude work may
/// still be outstanding on `record`: its interval overlaps that work from
/// its first instant. Called under the state lock, in the section that
/// begins the grant.
fn claude_begin_grant_beside_outstanding_work(record: &mut SessionRecord, grant_id: &str) {
    if !record.claude_outstanding.any() {
        return;
    }
    record.engram.active_turn_mixed_attribution = Some(grant_id.to_owned());
    record.claude_outstanding.notice_due = true;
    push_unassigned_claude_observation(
        record,
        format!(
            "grant {grant_id} began while Claude work an earlier turn or runtime started may \
             still run; what changes during it may be that work's, so the grant's own source \
             report is withheld as uncertain"
        ),
    );
}

/// Applies the part of an observation the sink gives a non-current
/// disposition: kept as unassigned when it is the session runtime's, and the
/// live grant excluded from another turn's or a subagent's work, and from a
/// replaced runtime's work only when it shows new activity (a command
/// starting). Under the state lock, in the section that applies the
/// observation.
fn claude_exclude_observation(
    record: &mut SessionRecord,
    disposition: ClaudeObservationDisposition,
    observation: &EngramRecorderObservation<'_>,
) {
    match disposition {
        ClaudeObservationDisposition::Current => {}
        ClaudeObservationDisposition::Unowned => {
            push_unassigned_claude_observation(
                record,
                unassigned_claude_observation_text(observation),
            );
        }
        ClaudeObservationDisposition::Foreign => {
            push_unassigned_claude_observation(
                record,
                unassigned_claude_observation_text(observation),
            );
            claude_exclude_live_grant(record, None);
        }
        ClaudeObservationDisposition::Replaced => {
            if matches!(
                observation,
                EngramRecorderObservation::CommandStarted { .. }
                    | EngramRecorderObservation::CommandDescribed { .. }
            ) {
                claude_exclude_live_grant(record, None);
            }
        }
    }
}

/// Adds the worktrees in `more` to `locations`, keeping each once; a
/// worktree TermAl could not name (`None`) stands for any.
fn claude_merge_locations(locations: &mut Vec<Option<String>>, more: &[Option<String>]) {
    for location in more {
        if !locations.contains(location) {
            locations.push(location.clone());
        }
    }
}

/// One piece of outstanding work: a tool call a Claude runtime started that
/// may still run and write, until a correlated end from that runtime arrives.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ClaudeOutstandingEntry {
    /// The runtime that started it.
    token: RuntimeToken,
    /// Its tool-use id.
    key: String,
    /// The turn that started it.
    origin: ClaudeWorkOrigin,
    /// Launched, or later moved, to run in the background: its tool result
    /// marks only the launch, and its end is a terminal task notification
    /// naming it.
    background: bool,
    /// A subagent started it.
    nested: bool,
    /// It is a top-level call of a recognised simple full gate
    /// (`engram_is_simple_full_launcher`): it does not fence the check and the
    /// carried run it launched itself, and nothing else.
    self_gate: bool,
    /// Where it may write: the worktrees known when it was registered (the
    /// session's workdir worktree, the grant's named root) and every place
    /// its command was later placed. Only grows while the work is
    /// outstanding; `None` is a worktree TermAl could not name, so any.
    locations: Vec<Option<String>>,
}

impl ClaudeOutstandingEntry {
    /// Whether it restricts the session: background or subagent work, or
    /// work of another runtime or another turn. A root foreground command of
    /// the session's current runtime and turn is not: it is that turn's own,
    /// and the turn's running commands already fence its checks; it is kept
    /// so it survives a Stop or a runtime replacement as an orphan.
    fn is_hazard(&self, current: Option<&RuntimeToken>, turn_generation: u64) -> bool {
        self.background
            || self.nested
            || Some(&self.token) != current
            || self.origin != (ClaudeWorkOrigin::Attempt { turn_generation })
    }
}

/// The outstanding work of a session's Claude runtimes, and the notices the
/// session owes about it.
#[derive(Clone, Debug, Default)]
struct ClaudeOutstandingWork {
    entries: VecDeque<ClaudeOutstandingEntry>,
    /// Work past the bound was dropped: it is kept as unknown, and nothing
    /// releases it.
    unknown: bool,
    /// Where the dropped work may write, kept when its detail was dropped.
    unknown_locations: Vec<Option<String>>,
    /// A restriction newly took effect and the session has not said so yet.
    notice_due: bool,
    /// The session said its evidence is restricted, and has not yet said it
    /// was lifted.
    restricting: bool,
}

impl ClaudeOutstandingWork {
    fn register(&mut self, entry: ClaudeOutstandingEntry) {
        if let Some(known) = self
            .entries
            .iter_mut()
            .find(|known| known.token == entry.token && known.key == entry.key)
        {
            claude_merge_locations(&mut known.locations, &entry.locations);
            return;
        }
        if self.entries.len() >= CLAUDE_OUTSTANDING_WORK_LIMIT
            && let Some(dropped) = self.entries.pop_front()
        {
            self.unknown = true;
            claude_merge_locations(&mut self.unknown_locations, &dropped.locations);
        }
        self.entries.push_back(entry);
    }

    /// The command `key` was placed in `worktrees`: they join where its
    /// entry may write, if it is still outstanding.
    fn place(&mut self, key: &str, worktrees: &[Option<String>]) {
        for entry in self.entries.iter_mut().filter(|entry| entry.key == key) {
            claude_merge_locations(&mut entry.locations, worktrees);
        }
    }

    /// The tool call `key` of the runtime `token` returned its result. A
    /// result that says the call now runs in the background moves it there
    /// first; then a foreground call ended, and a background one only
    /// started. Work a subagent of that call started ends only with its own
    /// result.
    fn tool_result(&mut self, token: &RuntimeToken, key: &str, moved_to_background: bool) {
        if moved_to_background {
            for entry in self
                .entries
                .iter_mut()
                .filter(|entry| entry.token == *token && entry.key == key)
            {
                entry.background = true;
            }
        }
        self.entries
            .retain(|entry| entry.token != *token || entry.key != key || entry.background);
    }

    /// The background work `key` of the runtime `token` reached a terminal
    /// status. Work its subagent started ends only with its own end.
    fn task_ended(&mut self, token: &RuntimeToken, key: &str) {
        self.entries
            .retain(|entry| entry.token != *token || entry.key != key);
    }

    fn holds(&self, key: &str) -> bool {
        self.entries.iter().any(|entry| entry.key == key)
    }

    /// Whether any work may still be running, known or unknown. A grant
    /// beginning now started none of it.
    fn any(&self) -> bool {
        self.unknown || !self.entries.is_empty()
    }

    fn hazard_count(&self, current: Option<&RuntimeToken>, turn_generation: u64) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.is_hazard(current, turn_generation))
            .count()
    }

    /// Whether any work restricts the session's turn `turn_generation` of
    /// the runtime `current` (`ClaudeOutstandingEntry::is_hazard`), or
    /// unknown work does; the recognised simple full gate whose exact call is
    /// `self_gate` (of `current`, at top level) does not count.
    fn restricts_except(
        &self,
        current: Option<&RuntimeToken>,
        turn_generation: u64,
        self_gate: Option<&str>,
    ) -> bool {
        self.unknown
            || self.entries.iter().any(|entry| {
                entry.is_hazard(current, turn_generation)
                    && !(entry.self_gate
                        && !entry.nested
                        && Some(entry.key.as_str()) == self_gate
                        && Some(&entry.token) == current)
            })
    }

    /// Whether any of it, known or unknown, may write in the worktree with
    /// key `root`.
    fn may_write_in(&self, root: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| EngramHost::worktrees_may_hold(&entry.locations, root))
            || (self.unknown && EngramHost::worktrees_may_hold(&self.unknown_locations, root))
    }

    /// Takes in `other`'s work (a removed session's), keeping what each may
    /// write in; taking the same work twice adds nothing.
    fn absorb(&mut self, other: &ClaudeOutstandingWork) {
        for entry in &other.entries {
            self.register(entry.clone());
        }
        if other.unknown {
            self.unknown = true;
            claude_merge_locations(&mut self.unknown_locations, &other.unknown_locations);
        }
    }
}

/// Whether Claude work may restrict `record`'s current turn now
/// (`ClaudeOutstandingWork::restricts`).
fn claude_session_work_restricts(record: &SessionRecord) -> bool {
    claude_session_work_restricts_except(record, None)
}

/// As `claude_session_work_restricts`, for the check the call `self_gate`
/// starts when it is a recognised simple full gate: its own outstanding call
/// does not fence it, every other hazard does.
fn claude_session_work_restricts_except(record: &SessionRecord, self_gate: Option<&str>) -> bool {
    record.claude_outstanding.restricts_except(
        record.runtime.runtime_token().as_ref(),
        record.active_turn_generation,
        self_gate,
    )
}

/// Where the outstanding call `key` of `record` may write, and whether it is
/// a recognised simple full gate, when it restricts the session: what an
/// extension of its scope must fence (`engram_fence_checks_for_claude_work`).
fn claude_hazard_scope(record: &SessionRecord, key: &str) -> Option<(Vec<Option<String>>, bool)> {
    let current = record.runtime.runtime_token();
    record
        .claude_outstanding
        .entries
        .iter()
        .find(|entry| {
            entry.key == key && entry.is_hazard(current.as_ref(), record.active_turn_generation)
        })
        .map(|entry| (entry.locations.clone(), entry.self_gate))
}

/// Outstanding Claude work a removed session left, kept on the host so its
/// interference outlives the session record. Process-local: a host restart
/// forgets it, which is a limitation, not a proof that the work ended.
#[derive(Clone, Debug)]
struct ClaudeOrphanedWork {
    session_id: String,
    work: ClaudeOutstandingWork,
}

impl StateInner {
    /// Keeps `work`, the outstanding Claude work of the session
    /// `session_id` being removed, on the host. Called under the state lock
    /// in the section that removes the session, so no interference query
    /// sees a gap; keeping the same work twice adds nothing.
    fn orphan_claude_work(&mut self, session_id: &str, work: &ClaudeOutstandingWork) {
        if !work.any() {
            return;
        }
        match self
            .claude_orphaned_work
            .iter_mut()
            .find(|orphan| orphan.session_id == session_id)
        {
            Some(orphan) => orphan.work.absorb(work),
            None => self.claude_orphaned_work.push(ClaudeOrphanedWork {
                session_id: session_id.to_owned(),
                work: work.clone(),
            }),
        }
    }

    /// Why the worktree with key `root` is restricted for checks of the
    /// session at `index` by Claude work of any other session, live or
    /// removed, that may write there.
    fn claude_work_restricting_worktree(
        &self,
        index: Option<usize>,
        root: &str,
    ) -> Option<ClaudeHazardCause> {
        let other = self
            .sessions
            .iter()
            .enumerate()
            .find(|(other, record)| {
                Some(*other) != index
                    && record.claude_outstanding.may_write_in(root)
                    && EngramHost::session_may_write(self, *other)
            })
            .map(|(_, record)| ClaudeHazardCause::OtherSession {
                session_id: record.session.id.clone(),
                name: record.session.name.clone(),
            });
        other.or_else(|| {
            self.claude_orphaned_work
                .iter()
                .find(|orphan| orphan.work.may_write_in(root))
                .map(|orphan| ClaudeHazardCause::DeletedSession {
                    session_id: orphan.session_id.clone(),
                })
        })
    }
}

/// What one Claude frame does to the outstanding work: tool calls it starts,
/// tool results it returns, background tasks it reports ended.
#[derive(Debug, Default, PartialEq, Eq)]
struct ClaudeFrameWork {
    /// The frame starts a tool call, registered or not: new activity.
    starts_work: bool,
    /// Tool calls to register: key, background, recognised simple full gate.
    started: Vec<(String, bool, bool)>,
    /// Tool results, by tool-use id, with whether the result says the call
    /// now runs in the background.
    results: Vec<(String, bool)>,
    /// Background tasks that reached a terminal status, by tool-use id.
    ended: Vec<String>,
}

impl ClaudeFrameWork {
    /// Whether the frame proves tool activity: a tool call or its result.
    fn proves_activity(&self) -> bool {
        self.starts_work || !self.results.is_empty()
    }
}

/// A task notification's status that ends the task.
fn claude_task_status_is_terminal(status: &str) -> bool {
    matches!(
        status,
        "completed" | "failed" | "killed" | "stopped" | "cancelled" | "canceled" | "error"
    )
}

/// Whether a tool call may go on running past its turn, or past its
/// runtime: a shell command (its process may outlive both), anything
/// launched in the background, and a subagent's own subagent launch. A
/// top-level subagent launch in the foreground holds its turn open, and the
/// commands its subagent runs are registered themselves.
fn claude_tool_call_may_outlive_its_turn(
    name: Option<&str>,
    nested: bool,
    background: bool,
) -> bool {
    background || name == Some("Bash") || (nested && matches!(name, Some("Task" | "Agent")))
}

/// What `message` does to the outstanding work. A Bash result whose
/// `tool_use_result.backgroundTaskId` names a task says the command runs on
/// in the background (launched there, moved there by a timeout, the user or
/// a turn abort): Claude Code 2.1.285 reports its end as a task notification
/// naming the call.
fn claude_frame_work(message: &Value, nested: bool) -> ClaudeFrameWork {
    let mut work = ClaudeFrameWork::default();
    match message.get("type").and_then(Value::as_str) {
        Some("assistant") => {
            for content in message
                .pointer("/message/content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if content.get("type").and_then(Value::as_str) != Some("tool_use") {
                    continue;
                }
                work.starts_work = true;
                let Some(key) = content.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let background = content
                    .pointer("/input/run_in_background")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let name = content.get("name").and_then(Value::as_str);
                if claude_tool_call_may_outlive_its_turn(name, nested, background) {
                    let self_gate = !nested
                        && name == Some("Bash")
                        && content
                            .pointer("/input/command")
                            .and_then(Value::as_str)
                            .is_some_and(EngramHost::is_simple_full_launcher_line);
                    work.started.push((key.to_owned(), background, self_gate));
                }
            }
        }
        Some("user") => {
            let moved_to_background = message
                .pointer("/tool_use_result/backgroundTaskId")
                .and_then(Value::as_str)
                .is_some_and(|task| !task.is_empty());
            for content in message
                .pointer("/message/content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if content.get("type").and_then(Value::as_str) != Some("tool_result") {
                    continue;
                }
                if let Some(key) = content.get("tool_use_id").and_then(Value::as_str) {
                    work.results.push((key.to_owned(), moved_to_background));
                }
            }
        }
        Some("system")
            if message.get("subtype").and_then(Value::as_str) == Some("task_notification") =>
        {
            let terminal = message
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(claude_task_status_is_terminal);
            if terminal && let Some(key) = message.get("tool_use_id").and_then(Value::as_str) {
                work.ended.push(key.to_owned());
            }
        }
        _ => {}
    }
    work
}

impl AppState {
    /// Admits one Claude frame before its parser or control step runs: the
    /// work it starts is registered, by the runtime and turn its provenance
    /// names, with where it may write as known now; and the live grant is
    /// excluded first when the frame proves activity of another turn or a
    /// subagent, starts background work, or (for a replaced runtime) starts
    /// anything, so no report built from here on is clean of it. One lock
    /// section.
    fn admit_claude_frame(
        &self,
        session_id: &str,
        provenance: &ClaudeObservationProvenance,
        nested: bool,
        work: &ClaudeFrameWork,
    ) {
        if !work.proves_activity() {
            return;
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let current = inner.sessions[index].runtime.runtime_token();
        let turn_generation = inner.sessions[index].active_turn_generation;
        let disposition = claude_observation_disposition(
            &inner.sessions[index],
            &EngramObservationProvenance::Claude(provenance.clone()),
        );
        // Work that restricts the session from now on, newly registered or
        // promoted: what is already running where it may write is fenced.
        let mut hazards: Vec<(String, Vec<Option<String>>, bool)> = Vec::new();
        let mut promoted_self_gate = None;
        let mut promoted = false;
        {
            let record = &mut inner.sessions[index];
            let locations = EngramHost::session_write_locations(record);
            for (key, background, self_gate) in &work.started {
                let entry = ClaudeOutstandingEntry {
                    token: provenance.token.clone(),
                    key: key.clone(),
                    origin: provenance.origin.clone(),
                    background: *background,
                    nested,
                    self_gate: *self_gate,
                    locations: locations.clone(),
                };
                if entry.is_hazard(current.as_ref(), turn_generation) {
                    hazards.push((key.clone(), entry.locations.clone(), *self_gate));
                }
                record.claude_outstanding.register(entry);
            }
            // A result that says its call runs on in the background moves it
            // there now, before the parser can end its check.
            for (key, moved_to_background) in &work.results {
                if !moved_to_background {
                    continue;
                }
                if let Some(entry) = record
                    .claude_outstanding
                    .entries
                    .iter_mut()
                    .find(|entry| entry.token == provenance.token && entry.key == *key)
                {
                    entry.background = true;
                    promoted = true;
                    if entry.self_gate {
                        promoted_self_gate = Some(key.clone());
                    }
                    hazards.push((key.clone(), entry.locations.clone(), entry.self_gate));
                }
            }
            let starts_background = work.started.iter().any(|(_, background, _)| *background);
            let excludes = match disposition {
                ClaudeObservationDisposition::Foreign => true,
                ClaudeObservationDisposition::Replaced => work.starts_work,
                ClaudeObservationDisposition::Current => starts_background || promoted,
                ClaudeObservationDisposition::Unowned => false,
            };
            if excludes {
                claude_exclude_live_grant(record, promoted_self_gate.as_deref());
            }
        }
        for (key, locations, self_gate) in &hazards {
            EngramHost::fence_checks_for_claude_work(&mut inner, index, key, locations, *self_gate);
        }
    }

    /// Settles one Claude frame after its effects were applied: a result
    /// that moves its call to the background does so first, then the tool
    /// results and the terminal task notifications retire what they prove
    /// ended. Only the session's current runtime settles anything: a replaced
    /// runtime's late frame retires no entry. One lock section.
    fn settle_claude_frame(&self, session_id: &str, token: &RuntimeToken, work: &ClaudeFrameWork) {
        if work.results.is_empty() && work.ended.is_empty() {
            return;
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let record = &mut inner.sessions[index];
        if !record.runtime.matches_runtime_token(token) {
            return;
        }
        let outstanding = &mut record.claude_outstanding;
        for (key, moved_to_background) in &work.results {
            outstanding.tool_result(token, key, *moved_to_background);
        }
        for key in &work.ended {
            outstanding.task_ended(token, key);
        }
    }

    /// Gives the notices the session owes about its evidence: a transcript
    /// notice and a line for the agent's next prompt when a restriction newly
    /// took effect and was not yet told, and the same when it lifts because
    /// no work restricts the session any more. Each is told once; the earlier
    /// ones stay in the transcript. Only for the session's current runtime
    /// `token`.
    fn give_claude_evidence_notices(&self, session_id: &str, token: &RuntimeToken) {
        let notice = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            let record = &mut inner.sessions[index];
            if !record.runtime.matches_runtime_token(token) {
                return;
            }
            let restricted = record.claude_outstanding.restricting;
            let due = std::mem::take(&mut record.claude_outstanding.notice_due);
            let notice = if !restricted && due && claude_session_work_restricts(record) {
                record.claude_outstanding.restricting = true;
                Some(claude_evidence_restricted_notice(record))
            } else if restricted && !claude_session_work_restricts(record) {
                record.claude_outstanding.restricting = false;
                Some(claude_evidence_released_notice(record))
            } else {
                None
            };
            if let Some(notice) = &notice {
                record.engram.set_pending_source_root_line(format!(
                    "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} {notice}"
                ));
                eprintln!("engram> session={session_id} {notice}");
            }
            notice
        };
        if let Some(notice) = notice
            && let Err(err) = self.push_message(
                session_id,
                Message::Text {
                    attachments: Vec::new(),
                    id: self.allocate_message_id(),
                    timestamp: stamp_now(),
                    author: Author::System,
                    text: notice,
                    expanded_text: None,
                    source: None,
                },
            )
        {
            eprintln!(
                "runtime state warning> failed to record the evidence notice of session \
                 `{session_id}`: {err:#}"
            );
        }
    }

    /// The evidence restriction an acceptance evaluation requested by
    /// `session_id` and measured at `place` is under, for the requester's
    /// notice: its own unresolved Claude work, or another session's (live
    /// or deleted) that may write in the measured workspace. Tests run while
    /// it lasts earn no credit, so an evaluation that needs that credit
    /// cannot pass on them.
    fn claude_evidence_restriction_notice(
        &self,
        session_id: &str,
        place: &EngramBasisPlace,
    ) -> Option<String> {
        // The measured worktree, resolved off the lock.
        let root = match place {
            EngramBasisPlace::Workdir(workdir) => engram_worktree_root(FsPath::new(workdir)),
            EngramBasisPlace::Named { root, .. } => engram_path_key(FsPath::new(root)),
        };
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(session_id);
        if let Some(index) = index
            && claude_session_work_restricts(&inner.sessions[index])
        {
            return Some(claude_evidence_restricted_notice(&inner.sessions[index]));
        }
        inner
            .claude_work_restricting_worktree(index, &root)
            .map(|cause| {
                format!(
                    "Evidence restricted in {root}: {}. Tests run there while it lasts may pass \
                     without earning verification credit, so an acceptance result that needs \
                     that credit cannot pass on those runs. Stopping or deleting that session, or \
                     starting a fresh one in the same workspace, does not show that the work \
                     finished; TermAl has no reset for this.",
                    cause.describe()
                )
            })
    }
}
