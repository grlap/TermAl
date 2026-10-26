// Automatic recovery of a retained Engram prompt whose prepared begin was not
// durably acknowledged before any begin was sent, as happened under build
// load. Nothing can have begun. Two outcomes are told apart:
// - The attempt's wait for the acknowledgement ran out. That proves nothing
//   about the write, so the exact prepared begin (key, grant, delivery tokens)
//   is kept as pending. The head parks for an admission retry under
//   `ENGRAM_BEGIN_PREPARATION_PENDING_CODE`, which sends no evaluate: it
//   acknowledges that same content again, then sends that same begin.
// - The write failed. The prepared begin is withdrawn and the head is settled
//   as an abort retry under its own reason (`EngramAbortReason::BeginPreparation`).
//   It is admitted afresh: the rebind that expires the issued grant goes first,
//   then a fresh evaluate, with the queued prompt itself kept.
// Both run on the shared schedule (`engram_retry_schedule.rs`) inside one
// overall budget (`ENGRAM_ADMISSION_BUDGET_SECONDS`, `EngramBeginPreparationLineage`).
// The budget starts when the head is accepted for control admission and is
// saved with the head. Each dispatch attempt is clipped to what remains of
// it. Once it is spent, no automatic send starts: the tick ends the recovery,
// whichever retry then holds the head, and the head keeps the explicit
// Resume-or-Cancel hold.
// Owns: the classification of the fence's outcome for a prepared begin
// (`EngramAdmissionDurabilityFault`, `EngramBeginPreparationFailure`); the
// settlement after such a failure; the budget, its pending marker and its
// enforcement; the boot pass that re-admits a saved hold still inside its
// budget; and the release of such a hold for an explicit Resume.
// Does not own: the fence itself (`confirm_engram_admission_durable` in
// `engram_queued_admission.rs`); the abort record, its step, Stop and boot
// rebuild (`engram_abort_retry.rs`); a begin sent with an unknown outcome,
// which is replayed exactly (`engram_begin_replay.rs`).
// A stopped persist writer is not transient and keeps its hold.
// New module: before it, every such failure waited for an explicit Resume.

/// The overall budget of one admitted request's automatic recovery, counted
/// from when its queue head was accepted for control admission, before any
/// control, persistence or capacity wait: past it, no automatic send starts
/// and the head waits for Resume or Cancel. Each control call and dispatch
/// attempt inside it stays bounded by `ENGRAM_DISPATCH_BUDGET_MS` and is
/// clipped to what is left of it. Its start and deadline are saved with the
/// head, so a restart only gets the time that remains.
const ENGRAM_ADMISSION_BUDGET_SECONDS: i64 = 240;

/// The card code of a begin withheld because its prepared record was not
/// durably acknowledged.
const ENGRAM_BEGIN_PREPARATION_CODE: &str = "begin_preparation_unacknowledged";

/// The card code of a begin withheld while its prepared record's durable
/// acknowledgement is still pending: the attempt's wait ran out, which proves
/// nothing about the write. The exact prepared begin is kept, and the head's
/// admission retry acknowledges that same content again before sending it.
const ENGRAM_BEGIN_PREPARATION_PENDING_CODE: &str = "begin_preparation_pending";

/// What kind of failure the admission durability fence met.
#[derive(Clone, Debug, PartialEq, Eq)]
enum EngramAdmissionDurabilityFault {
    /// The writer acknowledged the content, but only after the operation
    /// budget: the content is durable.
    Late,
    /// The fence itself failed or did not finish.
    Fence(PersistFenceError),
    /// The session or the queue owner changed, or the synchronous fallback
    /// failed: nothing transient to wait out.
    Local,
}

impl EngramAdmissionDurabilityFault {
    /// Whether waiting and trying again can succeed: a missed deadline or a
    /// failed write. A stopped or shut-down writer, a superseded fence and a
    /// changed owner cannot be waited out by the same process.
    fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Fence(PersistFenceError::Deadline | PersistFenceError::WriteFailed(_))
        )
    }
}

/// What a begin about to be prepared is, which decides what its fence's
/// failure means for its prepared record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngramBeginPreparationKind {
    /// A first begin for the grant just evaluated: never sent.
    First,
    /// The head's pending prepared begin (`engram_begin_preparation_pending`):
    /// never sent.
    Pending,
    /// A begin-unknown replay or a reconstruction: possibly applied before.
    Recovery,
}

/// Why a prepared begin was not durably acknowledged, so not sent.
#[derive(Debug)]
struct EngramBeginPreparationFailure {
    error: EngramTransportError,
    /// The failure can be waited out: the head is re-admitted automatically.
    transient: bool,
    /// The prepared begin is kept: its acknowledgement is still pending (a
    /// wait that ran out, or a failed re-acknowledgement of a pending one),
    /// so the same content is acknowledged again before the begin is sent.
    pending: bool,
    /// The prepared record was replayed and may have been sent before (it is
    /// not this host's pending begin): it is kept, never withdrawn, and its
    /// grant is never expired by an orphan rebind.
    possibly_sent: bool,
}

impl EngramBeginPreparationFailure {
    fn not_transient(error: EngramTransportError) -> Self {
        Self {
            error,
            transient: false,
            pending: false,
            possibly_sent: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramAdmissionWindow {
    admitted_at: String,
    deadline: String,
}

/// Read-only view of saved timing. No deadline enforcement reads this view.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PromptAdmissionBudgetView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    original_admitted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    original_deadline: Option<String>,
    current_admitted_at: String,
    current_deadline: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    renewed_at: Option<String>,
    attempts: u32,
    ended: bool,
}

impl From<&EngramBeginPreparationLineage> for PromptAdmissionBudgetView {
    fn from(lineage: &EngramBeginPreparationLineage) -> Self {
        Self {
            original_admitted_at: lineage.original_window.as_ref().map(|w| w.admitted_at.clone()),
            original_deadline: lineage.original_window.as_ref().map(|w| w.deadline.clone()),
            current_admitted_at: lineage.admitted_at.clone(),
            current_deadline: lineage.deadline.clone(),
            renewed_at: lineage.renewed_at.clone(),
            attempts: lineage.attempts,
            ended: lineage.ended,
        }
    }
}

/// The overall automatic-recovery budget of one admitted request for a queue
/// head (`ENGRAM_ADMISSION_BUDGET_SECONDS`). It starts when the head is
/// accepted for control admission and stays with the head whichever retry
/// holds it meanwhile (this module's abort record, or a bind or parked-
/// admission retry), and it is saved with the session, so neither a change
/// of retry kind nor a restart starts it again. It is enforced for every
/// automatic recovery of the head (bind, admission, abort, the exact replay
/// of a begin possibly sent), whether or not a prepared begin of the head
/// was ever withheld (`held_since`, which marks prepared-begin recovery). A
/// delivered head or another head drops it. An exhausted recovery, a Stop, an operator's
/// pause or control disabled ends it (`ended`) but keeps it, so nothing that
/// follows starts another one; only an explicit accepted Resume replaces it
/// with one new window (`renew_engram_admission_budget_window`).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramBeginPreparationLineage {
    /// Observation only. Legacy rows cannot tell whether Resume already
    /// replaced a window, so absence must not invent original timing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    original_window: Option<EngramAdmissionWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    renewed_at: Option<String>,
    /// The queue head it bounds; any other head drops it.
    prompt_id: String,
    /// When the head was accepted for control admission (RFC 3339).
    admitted_at: String,
    /// `admitted_at` plus the budget (RFC 3339): no automatic send starts
    /// at or after it.
    deadline: String,
    /// When a prepared begin of the head was first withheld (RFC 3339): the
    /// head entered prepared-begin recovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    held_since: Option<String>,
    /// Automatic re-admission attempts started for the head so far.
    attempts: u32,
    /// The grant of the head's prepared begin whose durable acknowledgement
    /// is pending, in this host: that begin was never sent. Cleared before
    /// the begin is sent, so a saved one proves nothing after a restart,
    /// which reads the prepared begin as possibly sent (`engram_begin_boot.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_grant_id: Option<String>,
    /// The window ended before the head was delivered: an exhausted
    /// recovery, a Stop, an operator's pause or control disabled. Its start,
    /// deadline and first hold are kept as they were, so no later acceptance,
    /// boot or re-enablement reads the head as having no budget and starts
    /// another; only an explicit accepted Resume opens a new window.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    ended: bool,
    /// The grant of a begin Engram applied whose matching receipt this host
    /// first observed at or after the deadline: the actual outcome, kept with
    /// the hold. The receipt does not revive automatic advancement: the head
    /// holds, and Resume replays that same begin
    /// (`engram_begin_receipt_after_deadline`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    applied_after_deadline: Option<String>,
}

impl EngramBeginPreparationLineage {
    /// A budget for `prompt_id` accepted at `now`.
    fn accepted(prompt_id: String, now: chrono::DateTime<chrono::Utc>) -> Self {
        Self {
            original_window: Some(EngramAdmissionWindow {
                admitted_at: now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                deadline: (now + chrono::Duration::seconds(ENGRAM_ADMISSION_BUDGET_SECONDS))
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            }),
            renewed_at: None,
            prompt_id,
            admitted_at: now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            deadline: (now + chrono::Duration::seconds(ENGRAM_ADMISSION_BUDGET_SECONDS))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            held_since: None,
            attempts: 0,
            pending_grant_id: None,
            ended: false,
            applied_after_deadline: None,
        }
    }

    /// The one new window an explicit accepted Resume opens for the same
    /// head at `now`: a full budget from `now`, its attempts counted afresh.
    /// The head stays in prepared-begin recovery (its first hold is kept, so
    /// the new window is enforced), and a pending prepared begin stays
    /// pending for its exact begin.
    fn renewed_at(&self, now: chrono::DateTime<chrono::Utc>) -> Self {
        Self {
            original_window: self.original_window.clone(),
            renewed_at: Some(now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
            held_since: self.held_since.clone(),
            pending_grant_id: self.pending_grant_id.clone(),
            ..Self::accepted(self.prompt_id.clone(), now)
        }
    }

    /// The saved deadline; `None` when unreadable.
    fn deadline_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        chrono::DateTime::parse_from_rfc3339(&self.deadline)
            .ok()
            .map(|deadline| deadline.with_timezone(&chrono::Utc))
    }

    /// Whether the budget is spent at `now`. An unreadable deadline counts
    /// as spent, so a damaged record never retries forever.
    fn exhausted_at(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        self.deadline_at().is_none_or(|deadline| now >= deadline)
    }

    /// Whether the head entered prepared-begin recovery: a prepared begin of
    /// it was withheld. The budget itself binds every head that has one.
    fn entered_prepared_begin_recovery(&self) -> bool {
        self.held_since.is_some()
    }
}

/// Starts, under the lock, the budget of `owner`'s head as it is accepted for
/// control admission, unless that head already has one: a retry, a fence
/// wait or a repeated Resume keeps the running budget. Returns whether it
/// started one.
fn accept_engram_admission_budget(
    record: &mut SessionRecord,
    owner: &EngramQueuedAdmissionOwner,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    if !owner.matches(record) || engram_begin_preparation_lineage(record).is_some() {
        return false;
    }
    record.engram.begin_preparation_lineage = Some(EngramBeginPreparationLineage::accepted(
        owner.prompt_id.clone(),
        now,
    ));
    true
}

/// The budget of `record`'s head while it is enforced: the head has one, and
/// no begin OF THAT HEAD is applied. A grant left open by an earlier turn
/// (its closing checkpoint failed, and it is settled separately) is not the
/// head's applied begin and exempts nothing. A begin possibly
/// sent (an uncertain grant, a restart's reading of a saved prepared begin
/// included) is not an applied one: its exact replay (`engram_begin_replay.rs`)
/// runs inside what is left of the budget, and at the deadline it ends in the
/// explicit hold, its prepared begin and grant kept, never withdrawn and
/// never replaced by a fresh grant. Only an applied begin (an active grant)
/// ends the budget: it was the last automatic send, and the steps that
/// complete its turn (the delivery fence, source observation) keep their own
/// per-attempt bounds, neither clipped nor ended by it.
fn engram_admission_budget_enforced(
    record: &SessionRecord,
) -> Option<&EngramBeginPreparationLineage> {
    if engram_head_begin_applied(record) {
        return None;
    }
    engram_begin_preparation_lineage(record)
}

/// Whether `record`'s active grant is its queue head's own applied begin: the
/// grant the head's retained evaluate recorded as begun when Engram applied
/// that begin. Any other active grant, such as a previous turn's grant whose
/// closing checkpoint failed, is not.
fn engram_head_begin_applied(record: &SessionRecord) -> bool {
    let Some(active) = record.engram.active_grant_id.as_deref() else {
        return false;
    };
    record
        .queued_prompts
        .front()
        .and_then(|head| head.engram_evaluate.as_ref())
        .and_then(|evaluate| evaluate.begun_grant_id.as_deref())
        == Some(active)
}

/// Whether `record`'s head's admission window is spent at `now`: its saved
/// window has ended (exhaustion, a Stop, an operator's pause or control
/// disabled) or its saved deadline is at or before `now`. False for a head
/// with no enforced budget (none, another head's, or an applied begin's).
/// The park asks it first: a spent window schedules no further automatic
/// attempt (`end_engram_spent_admission_window`).
fn engram_admission_window_spent(record: &SessionRecord, now: chrono::DateTime<chrono::Utc>) -> bool {
    engram_admission_budget_enforced(record)
        .is_some_and(|lineage| lineage.ended || lineage.exhausted_at(now))
}

/// What is known of the last invocation when a spent window ends the
/// automatic recovery, for the hold's card and preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngramSpentInvocation {
    /// The producer certified that this invocation committed nothing
    /// (`control_request_expired`, `control_store_busy`); earlier attempts
    /// with the same key may have.
    CertifiedNotApplied,
    /// The invocation's outcome is unknown.
    Unknown,
    /// A local preparation cause: nothing was sent.
    NotSent,
    /// Engram applied the begin, but its receipt was first observed after the
    /// deadline: the hold's own text says so.
    Applied,
}

impl EngramSpentInvocation {
    fn of_cause(cause: &str) -> Self {
        if cause == ENGRAM_BEGIN_APPLIED_AFTER_DEADLINE_CODE {
            Self::Applied
        } else if cause == ENGRAM_BEGIN_PREPARATION_CODE
            || cause == ENGRAM_BEGIN_PREPARATION_PENDING_CODE
        {
            Self::NotSent
        } else if EngramInvocationApplication::from_remote_code(Some(cause))
            == EngramInvocationApplication::CertifiedNotApplied
        {
            Self::CertifiedNotApplied
        } else {
            Self::Unknown
        }
    }

    /// The sentence the exhausted hold adds about the last invocation.
    fn sentence(self) -> &'static str {
        match self {
            Self::CertifiedNotApplied => {
                " This invocation committed no changes (earlier attempts with the same key may have)."
            }
            Self::Unknown => " This invocation's outcome is unknown.",
            Self::NotSent | Self::Applied => "",
        }
    }
}

/// The facts of a window that ended the automatic recovery, for the card the
/// park writes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramSpentWindow {
    /// Automatic attempts started in the window.
    attempts: u32,
    /// The newest dispatch card's code, else the class of the hold.
    last_cause: String,
    invocation: EngramSpentInvocation,
    /// The window's saved start (RFC 3339).
    admitted_at: String,
    /// The window's saved deadline (RFC 3339).
    deadline: String,
}

/// Ends the automatic recovery of `record`'s head whose window is spent
/// (`engram_admission_window_spent`) in the Resume-or-Cancel exhaustion hold,
/// as the tick does (`end_engram_begin_preparation_retry`), and returns its
/// facts. It sends and saves nothing; the caller commits.
fn end_engram_spent_admission_window(record: &mut SessionRecord) -> EngramSpentWindow {
    let (attempts, admitted_at, deadline) = engram_begin_preparation_lineage(record).map_or_else(
        || (0, String::new(), String::new()),
        |lineage| (lineage.attempts, lineage.admitted_at.clone(), lineage.deadline.clone()),
    );
    let last_cause = engram_begin_preparation_last_cause(record);
    let preview = engram_begin_preparation_exhausted_preview(record, attempts);
    end_engram_begin_preparation_retry(record, preview);
    EngramSpentWindow {
        attempts,
        invocation: EngramSpentInvocation::of_cause(&last_cause),
        last_cause,
        admitted_at,
        deadline,
    }
}

/// Whether a matching applied-begin receipt is late for the applied-begin
/// exemption: the head's enforced admission budget ends at `deadline` (on
/// the operation budget clock), and the host first observed the complete
/// response at `observed` on that clock, at or after it. Receipt time is that
/// observation, not later local application or provider delivery: a receipt
/// observed strictly before the deadline ends the budget even when the steps
/// after it cross the deadline. An unobserved receipt is never assumed
/// timely. Without an enforced budget nothing is late.
fn engram_begin_receipt_after_deadline(
    deadline: Option<std::time::Instant>,
    observed: Option<std::time::Instant>,
) -> bool {
    match (deadline, observed) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(deadline), Some(observed)) => observed >= deadline,
    }
}

/// The card code of a begin Engram applied whose matching receipt was first
/// observed at or after the overall deadline: held, with no automatic
/// delivery, and that same begin replayed on Resume.
const ENGRAM_BEGIN_APPLIED_AFTER_DEADLINE_CODE: &str = "begin_applied_after_deadline";

/// The monotonic instant of `record`'s enforced budget deadline on `clock`,
/// for clipping a dispatch attempt: the time left before the saved deadline,
/// from now. `None` when no budget is enforced for the head.
fn engram_admission_budget_deadline(
    record: &SessionRecord,
    clock: &EngramBudgetClock,
) -> Option<std::time::Instant> {
    engram_admission_budget_deadline_at(record, clock, clock.wall_now())
}

/// `engram_admission_budget_deadline`, with the wall-clock `now` the saved
/// deadline is measured from.
fn engram_admission_budget_deadline_at(
    record: &SessionRecord,
    clock: &EngramBudgetClock,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<std::time::Instant> {
    let lineage = engram_admission_budget_enforced(record)?;
    let left = lineage
        .deadline_at()
        .map(|deadline| deadline - now)
        .and_then(|left| left.to_std().ok())
        .unwrap_or(Duration::ZERO);
    Some(clock.now() + left)
}

/// What a retry preview adds for a head in prepared-begin recovery: when
/// automatic recovery ends, after which only Resume or Cancel moves it.
fn engram_admission_budget_preview_suffix(record: &SessionRecord) -> String {
    engram_admission_budget_enforced(record)
        .filter(|lineage| lineage.entered_prepared_begin_recovery())
        .map_or_else(String::new, |lineage| {
            format!(" Automatic recovery ends at {}.", lineage.deadline)
        })
}

/// The pending prepared begin of `record`'s head: never sent in this host,
/// its acknowledgement still to be proven, while no grant is active or
/// possibly begun.
fn engram_begin_preparation_pending(record: &SessionRecord) -> Option<&EngramPreparedBegin> {
    let pending = engram_begin_preparation_lineage(record)?
        .pending_grant_id
        .as_deref()?;
    if record.engram.active_grant_id.is_some() || record.engram.uncertain_grant_id.is_some() {
        return None;
    }
    record
        .queued_prompts
        .front()?
        .engram_evaluate
        .as_ref()
        .filter(|evaluate| evaluate.begun_grant_id.is_none())?
        .prepared_begin
        .as_ref()
        .filter(|prepared| {
            prepared.grant_id == pending && prepared.phase == EngramPreparedBeginPhase::Prepared
        })
}

/// Whether control disabled or an operator's pause ends a begin-preparation
/// re-admission: it keeps the hold every park showed.
fn engram_begin_preparation_retry_held(record: &SessionRecord) -> bool {
    record.engram.disabled_reason.is_some() || record.engram.operator_paused
}

/// The last cause of `record`'s head's automatic recovery: the code of its
/// newest dispatch card (`engram_newest_card_code`), or, without one, the
/// class of its hold.
fn engram_begin_preparation_last_cause(record: &SessionRecord) -> String {
    let fallback = if record.engram.uncertain_grant_id.is_some() {
        ENGRAM_BEGIN_REPLAY_CODE
    } else if engram_begin_preparation_pending(record).is_some() {
        ENGRAM_BEGIN_PREPARATION_PENDING_CODE
    } else {
        ENGRAM_BEGIN_PREPARATION_CODE
    };
    engram_newest_card_code(record).unwrap_or_else(|| fallback.to_owned())
}

/// The preview of `record`'s head whose automatic re-admission is exhausted:
/// it keeps the explicit hold, which Resume or Cancel ends, and names the
/// attempts and the last cause (`engram_begin_preparation_last_cause`). It
/// says what is known of the last invocation: that it committed nothing only
/// for a producer certificate, that its outcome is unknown for any other
/// control outcome, and nothing for a local preparation cause, which sent
/// nothing. When a begin may have been sent (an uncertain grant) or this
/// host's pending begin is kept, that same begin is kept and Resume sends it,
/// never a fresh grant.
fn engram_begin_preparation_exhausted_preview(record: &SessionRecord, attempts: u32) -> String {
    let possibly_sent = record.engram.uncertain_grant_id.is_some();
    let pending = engram_begin_preparation_pending(record).is_some();
    let cause = engram_begin_preparation_last_cause(record);
    let invocation = EngramSpentInvocation::of_cause(&cause).sentence();
    let applied_late = possibly_sent
        && engram_begin_preparation_lineage(record).is_some_and(|lineage| {
            lineage.applied_after_deadline.is_some()
                && lineage.applied_after_deadline == record.engram.uncertain_grant_id
        });
    let kept = if applied_late {
        "Engram applied the begin, but its receipt was first observed after the deadline; it \
         is kept, and Resume replays that same begin to continue, or cancel."
    } else if possibly_sent {
        "The begin may have been sent and is kept; resume to replay that same begin or cancel."
    } else if pending {
        "The prepared begin was never sent and is kept; resume to send that same begin or \
         cancel."
    } else {
        "Original prompt retained; resume to retry or cancel."
    };
    format!(
        "Engram: automatic re-admission exhausted after {attempts} attempts; last cause \
         {cause}.{invocation} {kept}"
    )
}

/// The code `record`'s newest dispatch card names: its refusal code, or its
/// published cause's original code.
fn engram_newest_card_code(record: &SessionRecord) -> Option<String> {
    record
        .session
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::EngramControl { card, .. } => Some(card),
            _ => None,
        })
        .and_then(|card| {
            card.refusal_code.clone().or_else(|| {
                card.causal_failure
                    .as_ref()
                    .and_then(|cause| cause.original_code.clone())
            })
        })
}

/// The lineage of `record`'s current queue head, if it has one.
fn engram_begin_preparation_lineage(
    record: &SessionRecord,
) -> Option<&EngramBeginPreparationLineage> {
    let head = record.queued_prompts.front()?;
    record
        .engram
        .begin_preparation_lineage
        .as_ref()
        .filter(|lineage| lineage.prompt_id == head.pending_prompt.id)
}

/// Drops `record`'s lineage when it names `prompt_id` (the head just
/// delivered).
fn clear_engram_begin_preparation_lineage_for(record: &mut SessionRecord, prompt_id: Option<&str>) {
    if record
        .engram
        .begin_preparation_lineage
        .as_ref()
        .is_some_and(|lineage| Some(lineage.prompt_id.as_str()) == prompt_id)
    {
        record.engram.begin_preparation_lineage = None;
    }
}

/// Ends the automatic re-admission of `record`'s head with no new send: any
/// retry record holding the head is dropped, and the head takes the resumable
/// explicit hold a park with no wire intent has (waiting, not interrupted,
/// the queue paused), which Resume moves on and Cancel removes. Whatever
/// prepared begin the head keeps stays as it is: this host's pending begin
/// keeps its marker, grant, key and delivery tokens (its write may still be
/// unresolved, so nothing proves it absent), and a begin possibly sent keeps
/// its uncertain grant. Resume then sends that same begin, never a fresh
/// grant. The lineage's window ends but is kept
/// (`end_engram_admission_budget_window`): only the explicit Resume opens a
/// new one.
fn end_engram_begin_preparation_retry(record: &mut SessionRecord, preview: String) {
    record.engram.abort_retry = None;
    record.engram.abort_retry_fence = None;
    clear_engram_admission_retry(record);
    clear_engram_bind_retry(record);
    end_engram_admission_budget_window(record);
    if let Some(head) = record.queued_prompts.front_mut() {
        head.engram_interrupted = false;
        head.engram_waiting = true;
    }
    record.set_auto_dispatch_blocked(true);
    record.session.preview = preview;
    sync_pending_prompts(record);
}

/// Ends the window of `record`'s head without dropping it: a Stop, an
/// operator's pause, control disabled or exhaustion stops automatic
/// advancement, and the window's start, deadline and first hold stay as they
/// were until an explicit accepted Resume replaces them.
fn end_engram_admission_budget_window(record: &mut SessionRecord) {
    let Some(head) = record.queued_prompts.front() else {
        return;
    };
    if let Some(lineage) = record
        .engram
        .begin_preparation_lineage
        .as_mut()
        .filter(|lineage| lineage.prompt_id == head.pending_prompt.id)
    {
        lineage.ended = true;
    }
}

/// What a project's Engram settings change keeps of `record`'s budget as it
/// rebuilds the session's Engram state: a head in prepared-begin recovery
/// keeps its window, ended, so the change grants it no new one and only an
/// explicit accepted Resume opens one. Its pending marker is dropped: a
/// prepared begin evaluated under the old settings is no longer this host's
/// pending begin to send, so Resume says it may have been sent and holds it
/// for reconciliation or Cancel, never sends it under the new settings. A
/// window already ended (exhausted, stopped, paused) is kept ended too, so
/// the change revives no automatic recovery. Any other running budget is
/// dropped with the rest of that state, as before.
fn take_engram_admission_budget_for_settings_reset(
    record: &mut SessionRecord,
) -> Option<EngramBeginPreparationLineage> {
    if engram_begin_preparation_lineage(record)
        .is_some_and(EngramBeginPreparationLineage::entered_prepared_begin_recovery)
    {
        end_engram_admission_budget_window(record);
    }
    record
        .engram
        .begin_preparation_lineage
        .take()
        .filter(|lineage| lineage.ended)
        .map(|lineage| EngramBeginPreparationLineage {
            pending_grant_id: None,
            ..lineage
        })
}

/// The tick's bound on a head with an enforced budget, before the step of
/// whichever retry holds it: at or past the saved deadline, with no attempt
/// in flight, the recovery is exhausted and ends (`Dropped`), so no new
/// automatic send starts even when an attempt, an acknowledgement or a
/// capacity slot is due. That holds for this host's pending begin and for the
/// exact replay of a begin possibly sent too: each ends in the explicit hold
/// with its prepared begin and grant kept. A budget naming another head is
/// dropped. An operator's pause or
/// control disabled ends the window but keeps it, and the retry's own step
/// then ends the retry. `None` lets the retry's own step run.
fn engram_begin_preparation_lineage_step(
    record: &mut SessionRecord,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<EngramAbortRetryStep> {
    record.engram.begin_preparation_lineage.as_ref()?;
    let Some(lineage) = engram_begin_preparation_lineage(record).cloned() else {
        record.engram.begin_preparation_lineage = None;
        return None;
    };
    let running = engram_retry_attempt_in_flight(record)
        || matches!(
            record.session.status,
            SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
        );
    if running || engram_admission_budget_enforced(record).is_none() {
        return None;
    }
    if engram_begin_preparation_retry_held(record) {
        end_engram_admission_budget_window(record);
        return None;
    }
    // A begin applied but first observed at or after the deadline spent the
    // budget on the operation clock, whatever the wall clock reads now.
    if !lineage.exhausted_at(now) && lineage.applied_after_deadline.is_none() {
        return None;
    }
    let preview = engram_begin_preparation_exhausted_preview(record, lineage.attempts);
    end_engram_begin_preparation_retry(record, preview);
    Some(EngramAbortRetryStep::Dropped)
}

/// An explicit accepted Resume of a head whose window ended (a Stop, an
/// operator's pause, control disabled, exhaustion) or ran out opens one new
/// window from `now` (`renew_engram_admission_budget_window`). A window still
/// running, or one whose attempt is in flight, is kept: a repeated Resume
/// neither resets its deadline nor starts a second flight. Returns whether
/// it renewed one.
fn renew_engram_admission_budget_for_resume(
    record: &mut SessionRecord,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let running = engram_retry_attempt_in_flight(record)
        || matches!(
            record.session.status,
            SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
        );
    let Some(lineage) = engram_begin_preparation_lineage(record) else {
        return false;
    };
    if running || !(lineage.ended || lineage.exhausted_at(now)) {
        return false;
    }
    renew_engram_admission_budget_window(record, now)
}

/// Replaces the window of `record`'s head with the one new window an
/// explicit accepted Resume opens (`EngramBeginPreparationLineage::renewed_at`),
/// in the same write that releases the head. Returns whether the head had
/// one.
fn renew_engram_admission_budget_window(
    record: &mut SessionRecord,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let Some(renewed) =
        engram_begin_preparation_lineage(record).map(|lineage| lineage.renewed_at(now))
    else {
        return false;
    };
    record.engram.begin_preparation_lineage = Some(renewed);
    true
}

/// Counts an automatic attempt the tick starts for `record`'s head against
/// its lineage. Returns whether it counted one.
fn count_engram_begin_preparation_attempt(record: &mut SessionRecord) -> bool {
    let Some(head) = record.queued_prompts.front() else {
        return false;
    };
    match record.engram.begin_preparation_lineage.as_mut() {
        Some(lineage) if lineage.prompt_id == head.pending_prompt.id => {
            lineage.attempts = lineage.attempts.saturating_add(1);
            true
        }
        _ => false,
    }
}

/// Whether a saved `record` keeps the hold a prepared begin's failure left
/// when the host that met it could not wait it out (a stopped writer): its
/// newest dispatch card names that failure, the head is still in that
/// interrupted hold with its retained evaluate and no prepared or begun
/// grant, no grant is active or possibly begun, nothing else holds or
/// schedules the head, and the head's lineage dates the hold. A hold saved
/// without a lineage (by an earlier build) cannot be dated, so it keeps its
/// hold. Nothing was sent, so after a restart such a head is admitted afresh,
/// once the new host's settlement is acknowledged.
fn engram_begin_preparation_boot_hold(record: &SessionRecord) -> bool {
    let engram = &record.engram;
    engram_begin_preparation_held_head(record)
        && engram.disabled_reason.is_none()
        && !engram.operator_paused
        && !record.runtime_stop_in_progress
        && engram_begin_preparation_lineage(record)
            .is_some_and(|lineage| lineage.entered_prepared_begin_recovery() && !lineage.ended)
}

/// Whether `record`'s head is in the interrupted hold a prepared begin's
/// failure left, with nothing sent: the newest dispatch card names that
/// failure, the head keeps its retained evaluate with no prepared or begun
/// grant, no grant is active or possibly begun, and no retry record holds or
/// schedules the head.
fn engram_begin_preparation_held_head(record: &SessionRecord) -> bool {
    let engram = &record.engram;
    let unscheduled = engram.abort_retry.is_none()
        && engram.admission_retry.is_none()
        && engram.bind_retry.is_none();
    let ungranted = engram.active_grant_id.is_none() && engram.uncertain_grant_id.is_none();
    // The park's interrupted hold: waiting and interrupted.
    let head_held = record.queued_prompts.front().is_some_and(|head| {
        head.engram_interrupted
            && head.promotion_disposition_known
            && head.engram_evaluate.as_ref().is_some_and(|evaluate| {
                evaluate.prepared_begin.is_none() && evaluate.begun_grant_id.is_none()
            })
    });
    unscheduled && ungranted && head_held && engram_begin_preparation_newest_cause(record)
}

/// What an explicit Resume finds on `record`'s head after a prepared begin
/// was withheld, nothing being scheduled for it any more (a park left it
/// interrupted, or a Stop ended its automatic recovery).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngramBeginPreparationResume {
    /// Not such a hold: the ordinary Resume applies.
    NotHeld,
    /// Nothing was sent and no prepared begin is kept: its retained evaluate
    /// holds no prepared begin, or there is none at all after a failed
    /// write's settlement. Resume releases it for a fresh admission.
    Releasable,
    /// This host's pending prepared begin (never sent), its card's cause
    /// whatever it is: Resume releases it for that exact begin, which the
    /// drain acknowledges again and replays (a durably acknowledged prepared
    /// begin is sent as that exact begin, never re-admitted afresh), so its
    /// grant is kept.
    ReleasablePending,
    /// A replay was held at its local acknowledgement fence. The uncertain
    /// grant still owns the exact Prepared request; Resume may release only
    /// that replay, after checking its original authority.
    ReconcilePrepared,
    /// Settings interrupted a retained bind/evaluate. Keep its original
    /// authority and recover it exactly, including the cold status check;
    /// interruption alone is not evidence that a grant was never begun.
    ReconcileRetained,
    /// A prepared begin with no pending marker of this host, as a restart
    /// leaves one, whatever its card's cause: it may have been sent, so it
    /// stays held and Resume says why.
    PossiblySent,
}

/// `EngramBeginPreparationResume` for `record`: no retry record holds or
/// schedules the head, no grant is active or possibly begun, and the head is
/// interrupted.
fn engram_begin_preparation_resume(record: &SessionRecord) -> EngramBeginPreparationResume {
    let engram = &record.engram;
    let unscheduled = engram.abort_retry.is_none()
        && engram.admission_retry.is_none()
        && engram.bind_retry.is_none();
    let ungranted = engram.active_grant_id.is_none() && engram.uncertain_grant_id.is_none();
    let Some(head) = record.queued_prompts.front() else {
        return EngramBeginPreparationResume::NotHeld;
    };
    if head.engram_ordinary_handoff {
        return EngramBeginPreparationResume::NotHeld;
    }
    if !(unscheduled && head.engram_interrupted && head.promotion_disposition_known) {
        return EngramBeginPreparationResume::NotHeld;
    }
    if engram_begin_replay_pending(record).is_some()
        && head.engram_evaluate.as_ref().is_some_and(|evaluate| evaluate.begun_grant_id.is_none())
        && engram_begin_preparation_newest_cause(record)
        && engram.disabled_reason.is_none()
        && !engram_retry_attempt_in_flight(record)
        && !record.runtime_stop_in_progress
        && !matches!(record.session.status,
            SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping)
    {
        return EngramBeginPreparationResume::ReconcilePrepared;
    }
    if !ungranted {
        return EngramBeginPreparationResume::NotHeld;
    }
    match head.engram_evaluate.as_ref() {
        Some(evaluate) if evaluate.begun_grant_id.is_some() => {
            EngramBeginPreparationResume::NotHeld
        }
        // A kept prepared begin decides by itself, whatever the card says.
        Some(evaluate) if evaluate.prepared_begin.is_some() => {
            if engram_begin_preparation_pending(record).is_some() {
                EngramBeginPreparationResume::ReleasablePending
            } else {
                EngramBeginPreparationResume::PossiblySent
            }
        }
        // Without one, only a card naming a withheld prepared begin makes it
        // this module's never-sent hold.
        _ if engram_begin_preparation_newest_cause(record) => {
            EngramBeginPreparationResume::Releasable
        }
        // This session records Begin ownership on the head before sending.
        // Thus no head intent proves no Begin was sent only for this format;
        // legacy sessions without begins_recorded retain the conservative hold.
        _ if !head.has_engram_intent()
            && engram.begins_recorded
            && !engram_retry_attempt_in_flight(record)
            && !record.runtime_stop_in_progress
            && !matches!(
                record.session.status,
                SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
            ) =>
        {
            EngramBeginPreparationResume::Releasable
        }
        // A settings reset clears transport failure state. An unresolved
        // protocol/store fault on this live binding is not a retry grant.
        _ if head.has_engram_intent()
            && engram.consecutive_transport_failures == 0
            && engram.disabled_reason.is_none()
            && !engram_retry_attempt_in_flight(record)
            && !record.runtime_stop_in_progress
            && !matches!(
                record.session.status,
                SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
            ) =>
        {
            EngramBeginPreparationResume::ReconcileRetained
        }
        _ => EngramBeginPreparationResume::NotHeld,
    }
}

/// Whether `record`'s newest dispatch card names a withheld prepared begin.
fn engram_begin_preparation_newest_cause(record: &SessionRecord) -> bool {
    record
        .session
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::EngramControl { card, .. } => {
                Some(card.causal_failure.as_ref().is_some_and(|cause| {
                    cause.operation == "turn_begin"
                        && cause.original_code.as_deref() == Some(ENGRAM_BEGIN_PREPARATION_CODE)
                }))
            }
            _ => None,
        })
        .unwrap_or(false)
}

impl AppState {
    fn refuse_engram_queue_resume_locked(
        &self,
        inner: &mut StateInner,
        index: usize,
        reason: &str,
    ) -> ApiError {
        if inner.sessions[index].session.preview == reason {
            return ApiError::conflict(reason);
        }
        let previous_preview = std::mem::replace(
            &mut inner.sessions[index].session.preview,
            reason.to_owned(),
        );
        inner.stamp_session_at_index(index);
        if let Err(error) = self.commit_locked(inner) {
            inner.sessions[index].session.preview = previous_preview;
            // Refusing delivery is independent of saving its explanatory preview.
            // A broken store must not turn a safety refusal into a server error.
            return ApiError::conflict(format!(
                "{reason}; recovery status could not be saved: {error:#}"
            ));
        }
        ApiError::conflict(reason)
    }

    /// Records, under the lock, that `owner`'s head had a prepared begin
    /// withheld: its first such hold, from which its admission budget is
    /// enforced (a head that already has one keeps it), and the grant of a
    /// prepared begin kept pending, if any (`pending_grant_id`; `None` clears
    /// it). A head accepted before this build kept no budget: one starts now.
    /// It is saved with the dispatch card that names the failure.
    fn note_engram_begin_preparation_hold(
        &self,
        session_id: &str,
        owner: &EngramQueuedAdmissionOwner,
        pending_grant_id: Option<String>,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if !owner.matches(record) {
            return;
        }
        let now = inner.engram_budget_clock_snapshot().wall_now();
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        accept_engram_admission_budget(record, owner, now);
        if let Some(lineage) = record.engram.begin_preparation_lineage.as_mut() {
            if lineage.held_since.is_none() {
                lineage.held_since =
                    Some(now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
            }
            lineage.pending_grant_id = pending_grant_id;
        }
    }

    /// Records, under the lock, that Engram applied `grant_id`'s begin for
    /// `owner`'s head but the host first observed the matching receipt at or
    /// after the head's deadline: the actual outcome, kept on the hold, and
    /// saved with the dispatch card that names it. The tick then ends the
    /// recovery in the explicit hold.
    fn note_engram_begin_applied_after_deadline(
        &self,
        session_id: &str,
        owner: &EngramQueuedAdmissionOwner,
        grant_id: &str,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if owner.matches(record)
            && let Some(lineage) = record.engram.begin_preparation_lineage.as_mut()
            && lineage.prompt_id == owner.prompt_id
        {
            lineage.applied_after_deadline = Some(grant_id.to_owned());
        }
    }

    /// Under the lock, the head's prepared begin is about to be sent: it is
    /// no longer pending.
    fn clear_engram_begin_preparation_pending(
        &self,
        session_id: &str,
        owner: &EngramQueuedAdmissionOwner,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if owner.matches(record)
            && let Some(lineage) = record.engram.begin_preparation_lineage.as_mut()
        {
            lineage.pending_grant_id = None;
        }
    }

    /// Whether `owner`'s head is in prepared-begin recovery: its admission
    /// budget is enforced and it entered that recovery
    /// (`EngramBeginPreparationLineage::entered_prepared_begin_recovery`). The
    /// budget alone is enforced for every head and says nothing of the
    /// recovery path a head takes.
    fn engram_head_in_prepared_begin_recovery_for(
        &self,
        session_id: &str,
        owner: &EngramQueuedAdmissionOwner,
    ) -> bool {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner.find_session_index(session_id).is_some_and(|index| {
            let record = &inner.sessions[index];
            owner.matches(record)
                && engram_admission_budget_enforced(record)
                    .is_some_and(EngramBeginPreparationLineage::entered_prepared_begin_recovery)
        })
    }

    /// The pending prepared begin of `session_id`'s head, while `owner`
    /// still owns it (`engram_begin_preparation_pending`).
    fn engram_begin_preparation_pending_for(
        &self,
        session_id: &str,
        owner: Option<&EngramQueuedAdmissionOwner>,
    ) -> Option<EngramPreparedBegin> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner.find_session_index(session_id)?];
        if !owner.is_some_and(|owner| owner.matches(record)) {
            return None;
        }
        engram_begin_preparation_pending(record).cloned()
    }

    /// Under boot preparation's lock: settles every saved head that keeps a
    /// prepared begin's hold (`engram_begin_preparation_boot_hold`) for an
    /// automatic fresh admission under the authority its admission would run
    /// under now, its window counted from its saved first hold. A hold whose
    /// window has passed, or a session with no such authority, keeps its
    /// hold. The settlement is not yet acknowledged, so the head stays held
    /// until the new host's writer acknowledges it. Returns whether a record
    /// changed.
    fn readmit_engram_begin_preparation_holds_on_boot_locked(
        &self,
        inner: &mut StateInner,
    ) -> bool {
        let mut wall_now = None;
        let mut changed = false;
        for index in 0..inner.sessions.len() {
            if !engram_begin_preparation_boot_hold(&inner.sessions[index]) {
                continue;
            }
            // The wall time the saved deadline is measured on, read only
            // once a hold is found (a fixture chooses its clock first).
            let now = *wall_now.get_or_insert_with(|| inner.engram_budget_clock_snapshot().wall_now());
            // Only the time left before the saved deadline: the downtime
            // counted, and a restart never grants another budget.
            let exhausted = engram_begin_preparation_lineage(&inner.sessions[index])
                .is_none_or(|lineage| lineage.exhausted_at(now));
            if exhausted {
                continue;
            }
            let session_id = inner.sessions[index].session.id.clone();
            let Some(target) =
                Self::engram_binding_target_for_session_shape_locked(inner, &session_id, true)
                    .ok()
                    .flatten()
            else {
                continue;
            };
            // Only under the connection and admission settings the retained
            // evaluate was issued under, as the begin boot plan requires: a
            // changed connection's rebind could not expire the old grant.
            let same_authority = inner.sessions[index]
                .queued_prompts
                .front()
                .and_then(|head| head.engram_evaluate.as_ref())
                .is_some_and(|evaluate| {
                    evaluate.connection == target.connection
                        && evaluate.settings.same_admission_settings(&target.settings)
                });
            if !same_authority {
                continue;
            }
            let authority = engram_abort_authority(&target);
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            changed |= settle_engram_abort_before_handoff_locked(
                record,
                EngramAbortReason::BeginPreparation,
                None,
                &authority,
                None,
                now,
            );
        }
        changed
    }

    /// Settles `session_id`'s queue head after its prepared begin was not
    /// durably acknowledged and nothing was sent, for an automatic fresh
    /// admission. Only the exact owner whose begin was withheld settles it,
    /// with no grant begun or possibly begun. Its bound is the head's
    /// lineage, not the abort record's own times. Returns whether it settled.
    fn settle_engram_begin_preparation_failure(
        &self,
        session_id: &str,
        owner: &EngramQueuedAdmissionOwner,
        target: &EngramBindingTarget,
    ) -> bool {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return false;
        };
        let record = &inner.sessions[index];
        let settles = owner.matches(record)
            && record.engram.active_grant_id.is_none()
            && record.engram.uncertain_grant_id.is_none()
            && !engram_begin_preparation_retry_held(record)
            && record.queued_prompts.front().is_some_and(|head| {
                head.engram_evaluate.as_ref().is_some_and(|evaluate| {
                    evaluate.begun_grant_id.is_none() && evaluate.prepared_begin.is_none()
                })
            });
        settles
            && self.settle_engram_abort_before_handoff(
                &mut inner,
                index,
                EngramAbortReason::BeginPreparation,
                None,
                &engram_abort_authority(target),
                None,
            )
    }

    /// Before an explicit Resume drains the queue: releases a begin-
    /// preparation hold so the Resume admits the head afresh instead of
    /// finding it interrupted and doing nothing. Nothing was sent, so the
    /// stale retained evaluate is dropped, the session moves to a new
    /// dispatch generation (a new bind, which expires the issued grant), and
    /// the head takes the resumable park with no wire intent that the drain
    /// admits. When it cannot be released it says why, and the hold is left
    /// unchanged: control disabled, an automatic re-admission whose
    /// settlement is not yet saved, or a save that fails. Any other head,
    /// or a superseded `owner`, is left to the ordinary Resume. An accepted
    /// Resume that releases a hold, or finds the head's window ended or run
    /// out, opens one new window; a rejected one keeps the old window.
    ///
    /// Returns the head's owner as re-captured after a release, which moved
    /// it to a new dispatch generation: a caller that resumes for an owner
    /// dispatches with that one, or the drain would find its owner stale.
    fn release_engram_begin_preparation_hold_for_resume(
        &self,
        session_id: &str,
        owner: Option<&EngramQueuedAdmissionOwner>,
    ) -> std::result::Result<Option<EngramQueuedAdmissionOwner>, ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_visible_session_index(session_id) else {
            return Ok(None);
        };
        // A new window starts on the wall time its deadline is measured on.
        let wall_now = inner.engram_budget_clock_snapshot().wall_now();
        if owner.is_some_and(|owner| !owner.matches(&inner.sessions[index])) {
            return Ok(None);
        }
        let gated = Self::engram_session_requires_dispatch_card_locked(&inner, session_id);
        if inner.sessions[index].queued_prompts.front()
            .is_some_and(|head| head.engram_ordinary_handoff)
        {
            return Err(self.refuse_engram_queue_resume_locked(&mut inner, index,
                "The retained ordinary prompt may already have reached its provider; inspect the transcript and cancel it before sending a replacement"));
        }
        if !gated
            && inner.sessions[index]
                .queued_prompts
                .front()
                .is_some_and(QueuedPromptRecord::is_engram_retained)
        {
            return Err(self.refuse_engram_queue_resume_locked(&mut inner, index,
                "Engram control is off for this project; enable it to reconcile the retained prompt, or cancel it"));
        }
        let resume = engram_begin_preparation_resume(&inner.sessions[index]);
        let reconcile = resume == EngramBeginPreparationResume::ReconcileRetained;
        // The original store and settings own a retained request. Never turn
        // an explicit Resume into authorization on a different authority.
        if (reconcile || resume == EngramBeginPreparationResume::ReconcilePrepared) && gated {
            let target = Self::engram_binding_target_for_session_shape_locked(
                &inner, session_id, true,
            )
            .map_err(|error| ApiError::conflict(format!("Engram recovery unavailable: {error}")))?
            .ok_or_else(|| {
                ApiError::conflict(
                    "Engram recovery has no binding; restore the original settings before resuming",
                )
            })?;
            let head = inner.sessions[index]
                .queued_prompts
                .front()
                .expect("retained head");
            let matches = head.engram_bind.as_ref().is_none_or(|bind| {
                bind.connection == target.connection
                    && bind.settings.same_admission_settings(&target.settings)
            }) && head.engram_evaluate.as_ref().is_none_or(|evaluate| {
                evaluate.connection == target.connection
                    && evaluate.settings.same_admission_settings(&target.settings)
            });
            if !matches {
                return Err(self.refuse_engram_queue_resume_locked(&mut inner, index,
                    "Engram retained authorization belongs to different settings; restore the original settings to reconcile this prompt, or cancel it"));
            }
        }
        let record = &mut inner.sessions[index];
        if owner.is_some_and(|owner| !owner.matches(record)) {
            return Ok(None);
        }
        if let Some(retry) = record.engram.abort_retry.as_ref()
            && retry.reason == EngramAbortReason::BeginPreparation
            && !(retry.acknowledged && record.engram.abort_retry_acknowledged)
        {
            return Err(ApiError::conflict(
                "Engram: this prompt's automatic re-admission is waiting for its settlement to \
                 be saved locally; it starts by itself once saved. Nothing was sent.",
            ));
        }
        match engram_begin_preparation_resume(record) {
            EngramBeginPreparationResume::NotHeld => {
                if gated
                    && engram_queue_head_refused_as_interrupted(record)
                    && !record.engram.recovered_admission
                {
                    return Err(self.refuse_engram_queue_resume_locked(&mut inner, index,
                        "Engram retained prompt cannot resume until its admission state is reconciled; the prompt remains queued",
                    ));
                }
                // Accepted for the ordinary Resume, which admits a head that
                // is not interrupted while control is enabled: an ended or
                // spent window is replaced by one new window, stamped so the
                // session's next save carries it. A Resume refused here
                // (control disabled, an interrupted head) keeps the old
                // window; one the drain refuses later has already opened the
                // new window, which governs only an explicit Resume's flight.
                let admits = record.engram.disabled_reason.is_none()
                    && record
                        .queued_prompts
                        .front()
                        .is_some_and(|head| !head.engram_interrupted);
                if admits && renew_engram_admission_budget_for_resume(record, wall_now) {
                    inner.stamp_session_at_index(index);
                }
                return Ok(None);
            }
            EngramBeginPreparationResume::PossiblySent => {
                return Err(ApiError::conflict(
                    "Engram: this prompt's prepared begin is no longer this host's pending \
                     begin (after a restart or a change of the project's Engram settings) and \
                     may have been sent; it stays held for reconciliation. Cancel it before \
                     continuing.",
                ));
            }
            EngramBeginPreparationResume::Releasable
            | EngramBeginPreparationResume::ReleasablePending
            | EngramBeginPreparationResume::ReconcilePrepared
            | EngramBeginPreparationResume::ReconcileRetained => {}
        }
        let keep_pending = matches!(engram_begin_preparation_resume(record),
            EngramBeginPreparationResume::ReleasablePending
                | EngramBeginPreparationResume::ReconcilePrepared);
        if let Some(code) = record.engram.disabled_reason.as_deref() {
            return Err(ApiError::conflict(format!(
                "Engram control is disabled for this session after `{code}`; the prompt stays \
                 held. Nothing was sent. Cancel it before continuing."
            )));
        }
        if keep_pending || reconcile {
            // Preserve the exact pending begin or retained bind/evaluate.
            // Reconciliation checks remote status before proceeding; a
            // rebind here could expire the grant being recovered. Open one
            // explicit Resume window in the same saved transition.
            let head = record.queued_prompts.front_mut().expect("a held head");
            let saved = (
                head.engram_interrupted,
                head.engram_waiting,
                record.engram.rebind_required,
                record.engram.begin_preparation_lineage.clone(),
                record.engram.recovered_admission,
                record.engram.routing_token.clone(),
            );
            head.engram_interrupted = false;
            head.engram_waiting = true;
            record.engram.rebind_required = false;
            if reconcile {
                record.engram.recovered_admission = true;
                if let Some(evaluate) = &head.engram_evaluate
                    && let EngramControlRequest::TurnEvaluate { routing_token, .. } =
                        &evaluate.request
                {
                    // The reset may have lost the session mirror. Recovery
                    // verifies this exact token before any new begin; the
                    // subsequent delivery preparation needs the same mirror.
                    record.engram.routing_token = Some(routing_token.clone());
                }
            }
            renew_engram_admission_budget_window(record, wall_now);
            sync_pending_prompts(record);
            inner.stamp_session_at_index(index);
            if let Err(error) = self.commit_locked(&mut inner) {
                let record = &mut inner.sessions[index];
                let (interrupted, waiting, rebind, lineage, recovered, routing) = saved;
                if let Some(head) = record.queued_prompts.front_mut() {
                    head.engram_interrupted = interrupted;
                    head.engram_waiting = waiting;
                }
                record.engram.rebind_required = rebind;
                record.engram.begin_preparation_lineage = lineage;
                record.engram.recovered_admission = recovered;
                record.engram.routing_token = routing;
                sync_pending_prompts(record);
                return Err(ApiError::internal(format!(
                    "Engram: the held prompt could not be released, because its state could \
                     not be saved locally; it stays held. Nothing was sent. {error:#}"
                )));
            }
            return Ok(EngramQueuedAdmissionOwner::capture(&inner.sessions[index]));
        }
        let head = record.queued_prompts.front_mut().expect("a held head");
        let saved = (
            head.engram_evaluate.take(),
            head.engram_bind.take(),
            head.engram_interrupted,
            head.engram_waiting,
            record.engram.dispatch_generation,
            record.engram.rebind_required,
            record.engram.begin_preparation_lineage.clone(),
        );
        head.engram_interrupted = false;
        head.engram_waiting = true;
        record.engram.dispatch_generation = record.engram.dispatch_generation.saturating_add(1);
        record.engram.rebind_required = true;
        // This accepted Resume opens one new window for the head, in the
        // same write; a head saved without one starts it at its admission.
        renew_engram_admission_budget_window(record, wall_now);
        sync_pending_prompts(record);
        inner.stamp_session_at_index(index);
        if let Err(error) = self.commit_locked(&mut inner) {
            let record = &mut inner.sessions[index];
            let (evaluate, bind, interrupted, waiting, generation, rebind, lineage) = saved;
            if let Some(head) = record.queued_prompts.front_mut() {
                head.engram_evaluate = evaluate;
                head.engram_bind = bind;
                head.engram_interrupted = interrupted;
                head.engram_waiting = waiting;
            }
            record.engram.dispatch_generation = generation;
            record.engram.rebind_required = rebind;
            record.engram.begin_preparation_lineage = lineage;
            sync_pending_prompts(record);
            return Err(ApiError::internal(format!(
                "Engram: the held prompt could not be released, because its state could not be \
                 saved locally; it stays held. Nothing was sent. {error:#}"
            )));
        }
        Ok(EngramQueuedAdmissionOwner::capture(&inner.sessions[index]))
    }
}
