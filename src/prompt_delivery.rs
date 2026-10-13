// The durable delivery receipt of a host-dispatched prompt. A producer that
// must know whether its prompt reached a runtime enqueues it receipted: held
// until its content is durable (queued_prompt_durability.rs), and with a
// delivery record whose disposition is pending, handing-off, accepted,
// rejected or ambiguous.
//
// Before the runtime is called, a write-ahead hand-off record (the attempt and
// a fresh write-ahead nonce) must be acknowledged durable by the persist
// writer; only then is the entry promoted and handed off. The runtime's first
// positive acknowledgement records acceptance; a hand-off that provably never
// reached the runtime records rejection; a hand-off whose outcome cannot be
// known records ambiguity. After a restart a hand-off without acceptance is
// ambiguous and never replayed. A failed or missed write-ahead returns the
// prompt to pending: nothing reached the runtime, since the hand-off waits for
// that acknowledgement.
//
// On a paused queue only an explicit Resume begins the head's write-ahead. It
// attaches a one-shot continuation to that attempt, bound to the head, its
// content, the attempt and the pause it crossed. Its acknowledgement then
// drains for that head alone, and the promotion's gate consumes the
// continuation, or drops it on any Stop, pause, Cancel or edit since.
//
// Owns the record, its view on the wire, the promotion gate, the write-ahead
// fence and its settlement, the explicit Resume's continuation, the outcome
// transitions, restart settlement and the re-queue of a rejected delivery.
// Does not own the queue or its hold
// (session_interaction.rs, queued_prompt_durability.rs), promotion and
// hand-off (turn_dispatch.rs, api.rs) or the runtimes' events (claude_*.rs,
// codex.rs, acp.rs), which report here. New module beside
// queued_prompt_durability.rs.

/// How long the writer has to prove a write-ahead hand-off record durable.
/// A fence that runs out returns the prompt to pending; nothing retries it by
/// itself, and the next ordinary dispatch of the queue starts a new attempt.
#[cfg_attr(not(test), allow(dead_code))]
const PROMPT_DELIVERY_WRITE_AHEAD_BUDGET: Duration = Duration::from_secs(30);

/// Settled receipts a session keeps; the oldest settled ones go first.
const PROMPT_DELIVERY_SETTLED_LIMIT: usize = 64;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum PromptDeliveryDisposition {
    /// Queued, and no hand-off has begun (or a write-ahead failed before it).
    Pending,
    /// A write-ahead hand-off record exists; no runtime acknowledgement yet.
    HandingOff,
    /// The runtime acknowledged the prompt.
    Accepted,
    /// The hand-off provably never reached the runtime.
    Rejected,
    /// The hand-off began and its outcome cannot be known.
    Ambiguous,
}

impl PromptDeliveryDisposition {
    fn is_settled(self) -> bool {
        matches!(self, Self::Accepted | Self::Rejected | Self::Ambiguous)
    }
}

/// The delivery record of one receipted prompt, persisted in its session row.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PromptDelivery {
    disposition: PromptDeliveryDisposition,
    /// Hand-off attempts begun so far.
    attempt: u32,
    /// Fresh for every write-ahead and every restore to pending, so a late
    /// acknowledgement of an earlier write-ahead settles nothing.
    write_ahead: String,
    /// The receipt's place among the session's, for pruning settled ones.
    sequence: u64,
    /// The transcript message the promoted prompt became.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
    /// The turn generation the prompt was promoted under: runtime events name
    /// their turn by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    turn_generation: Option<u64>,
    /// The runtime's identity for the attempt: for Claude the command uuid of
    /// its first write, otherwise the session, turn and attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attempt_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    /// The promoted entry, kept while a rejected delivery may be re-queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    entry: Option<QueuedPromptRecord>,
    /// Whether the write-ahead record was acknowledged durable. In memory
    /// only: after a restart no hand-off is assumed acknowledged.
    #[serde(skip)]
    write_ahead_acknowledged: bool,
    /// The explicit Resume of the paused queue that began this attempt, while
    /// it may still carry the attempt across that pause. In memory only: it
    /// is never a permission after a restart.
    #[serde(skip)]
    resume: Option<PromptDeliveryResume>,
    /// Whether the promoted prompt may have reached its runtime yet, for a
    /// runtime whose one prompt-carrying write the host can name (Codex's
    /// turn/start). In memory only: after a restart a handing-off attempt is
    /// ambiguous, whatever this said.
    #[serde(skip)]
    runtime_write: PromptRuntimeWrite,
}

/// Where the promoted prompt stands against its runtime's prompt-carrying
/// write. `Untracked` (Claude, ACP, and every receipt read back after a
/// restart) gives no proof either way. `Pending` is positive proof that the
/// prompt was never exposed to the runtime, so a failure settles it
/// rejected. `Begun` is set before that write's I/O starts, under the state
/// lock, and from then on the prompt may have escaped: a failure leaves it
/// ambiguous. It only moves forward, and only once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PromptRuntimeWrite {
    #[default]
    Untracked,
    Pending,
    Begun,
}

/// The exact receipted attempt a Codex prompt command carries, from its
/// promotion to its turn/start write: a command that carries none was never
/// receipted. A command that carries one is written only while that exact
/// attempt (prompt and write-ahead nonce, under its turn) is still handing
/// off unwritten; otherwise it fails closed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CodexPromptReceipt {
    prompt_id: String,
    write_ahead: String,
}

/// The one-shot continuation of an explicit Resume that began a receipted
/// head's write-ahead on a paused queue. It is bound to exactly that head
/// (its dispatch and turn generations and its content), that attempt (its
/// write-ahead nonce) and the pause it crossed (the stop generation and pause
/// epoch then): a Stop, a later pause, a Cancel, a replacement or a content
/// edit, or another attempt, ends it. Only the promotion it authorizes
/// consumes it.
#[derive(Clone, Debug)]
struct PromptDeliveryResume {
    dispatch_generation: u64,
    active_turn_generation: u64,
    intent: String,
    write_ahead: String,
    stop_generation: u64,
    pause_epoch: u64,
}

impl PromptDeliveryResume {
    /// The owner the continuation's drain is bound to: exactly the head it
    /// was captured for, never one captured afresh.
    fn owner(&self, prompt_id: &str) -> EngramQueuedAdmissionOwner {
        EngramQueuedAdmissionOwner {
            prompt_id: prompt_id.to_owned(),
            generation: self.dispatch_generation,
            active_turn_generation: self.active_turn_generation,
        }
    }
}

/// The content an explicit Resume is bound to: the prompt's own intent (its
/// text, expansion, attachments and source), not the Engram admission state
/// that the release and the admission change on the same head.
fn receipted_prompt_intent(head: &QueuedPromptRecord) -> String {
    engram_turn_intent_fingerprint(
        &head.pending_prompt.text,
        head.pending_prompt.expanded_text.as_deref(),
        &head.attachments,
        head.pending_prompt.source.as_ref(),
        head.source,
    )
}

/// Whether the explicit Resume that began the head `prompt_id`'s current
/// attempt still authorizes crossing the pause for it: the same head, content
/// and attempt, and no Stop or pause since.
fn prompt_delivery_resume_holds(record: &SessionRecord, prompt_id: &str) -> bool {
    let Some(head) = record.queued_prompts.front() else {
        return false;
    };
    let Some(delivery) = record.prompt_deliveries.get(prompt_id) else {
        return false;
    };
    let Some(resume) = delivery.resume.as_ref() else {
        return false;
    };
    head.pending_prompt.id == prompt_id
        && delivery.disposition == PromptDeliveryDisposition::HandingOff
        && delivery.turn_generation.is_none()
        && delivery.write_ahead == resume.write_ahead
        && record.engram.dispatch_generation == resume.dispatch_generation
        && record.active_turn_generation == resume.active_turn_generation
        && record.runtime_stop_generation == resume.stop_generation
        && record.auto_dispatch_pause_epoch == resume.pause_epoch
        && !record.runtime_stop_in_progress
        && receipted_prompt_intent(head) == resume.intent
}

/// What an explicit Resume accepted, under its first lock, of a receipted
/// head: exactly that head and content, and the Stop and pause it crossed.
/// Its write-ahead is begun after the Engram release (which may move the
/// head to a new dispatch generation) only while all of it still holds.
struct ReceiptedResumeIdentity {
    prompt_id: String,
    intent: String,
    stop_generation: u64,
    pause_epoch: u64,
}

/// The identity a Resume accepts for its receipted head, if the head keeps a
/// receipt and is the one this Resume may start (no owner, or a matching one).
fn receipted_resume_identity(
    record: &SessionRecord,
    owner: Option<&EngramQueuedAdmissionOwner>,
) -> Option<ReceiptedResumeIdentity> {
    if owner.is_some_and(|owner| !owner.matches(record)) {
        return None;
    }
    let head = record.queued_prompts.front()?;
    record
        .prompt_deliveries
        .contains_key(&head.pending_prompt.id)
        .then(|| ReceiptedResumeIdentity {
            prompt_id: head.pending_prompt.id.clone(),
            intent: receipted_prompt_intent(head),
            stop_generation: record.runtime_stop_generation,
            pause_epoch: record.auto_dispatch_pause_epoch,
        })
}

/// What an explicit Resume of a paused queue does with its head's receipt.
enum ReceiptedResume {
    /// The head keeps no receipt (or another rule decides it): the Resume
    /// goes on as for any head.
    NotReceipted,
    /// This Resume began the head's write-ahead, or one it began is still in
    /// flight: the head is sent once that record is durable, while this
    /// Resume still holds. The pause stays until then.
    Preparing,
    /// The head's hand-off is held by an attempt this Resume did not begin.
    Refused(String),
}

/// A delivery receipt as clients see it, on the session.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PromptDeliveryView {
    prompt_id: String,
    disposition: PromptDeliveryDisposition,
    attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

/// What a runtime event or a hand-off result says about a delivery.
#[derive(Clone, Debug)]
enum PromptDeliveryOutcome {
    Accepted,
    Rejected(String),
    Ambiguous(String),
}

fn next_prompt_delivery_sequence(record: &SessionRecord) -> u64 {
    record
        .prompt_deliveries
        .values()
        .map(|delivery| delivery.sequence)
        .max()
        .map_or(1, |sequence| sequence.saturating_add(1))
}

/// Enqueues a held orchestrator prompt that also keeps a delivery receipt,
/// and returns the hold's nonce. The caller commits, then asks for the
/// durability acknowledgement with `request_queued_prompt_durability`.
#[cfg_attr(not(test), allow(dead_code))]
fn queue_receipted_orchestrator_prompt_on_record(
    record: &mut SessionRecord,
    pending_prompt: PendingPrompt,
    attachments: Vec<PromptImageAttachment>,
) -> String {
    let prompt_id = pending_prompt.id.clone();
    let nonce = queue_durable_orchestrator_prompt_on_record(record, pending_prompt, attachments);
    let sequence = next_prompt_delivery_sequence(record);
    record.prompt_deliveries.insert(
        prompt_id,
        PromptDelivery {
            disposition: PromptDeliveryDisposition::Pending,
            attempt: 0,
            write_ahead: Uuid::new_v4().to_string(),
            sequence,
            message_id: None,
            turn_generation: None,
            attempt_key: None,
            reason: None,
            entry: None,
            write_ahead_acknowledged: false,
            resume: None,
            runtime_write: PromptRuntimeWrite::Untracked,
        },
    );
    sync_prompt_delivery_view(record);
    nonce
}

/// Refreshes the receipts clients see on the session.
fn sync_prompt_delivery_view(record: &mut SessionRecord) {
    let mut view = record
        .prompt_deliveries
        .iter()
        .map(|(prompt_id, delivery)| {
            (
                delivery.sequence,
                PromptDeliveryView {
                    prompt_id: prompt_id.clone(),
                    disposition: delivery.disposition,
                    attempt: delivery.attempt,
                    message_id: delivery.message_id.clone(),
                    reason: delivery.reason.clone(),
                },
            )
        })
        .collect::<Vec<_>>();
    view.sort_by_key(|(sequence, _)| *sequence);
    record.session.prompt_deliveries = view.into_iter().map(|(_, view)| view).collect();
}

/// Drops the receipts of prompts that left the queue before any hand-off
/// (Stop, Cancel, removal: nothing reached a runtime), and the oldest settled
/// receipts beyond the limit. Called wherever the queue is synchronized.
fn prune_prompt_deliveries(record: &mut SessionRecord) {
    if record.prompt_deliveries.is_empty() && record.session.prompt_deliveries.is_empty() {
        return;
    }
    let queued = &record.queued_prompts;
    record.prompt_deliveries.retain(|prompt_id, delivery| {
        delivery.turn_generation.is_some()
            || delivery.disposition.is_settled()
            || queued
                .iter()
                .any(|entry| entry.pending_prompt.id == *prompt_id)
    });
    let mut settled = record
        .prompt_deliveries
        .iter()
        .filter(|(_, delivery)| delivery.disposition.is_settled())
        .map(|(prompt_id, delivery)| (delivery.sequence, prompt_id.clone()))
        .collect::<Vec<_>>();
    if settled.len() > PROMPT_DELIVERY_SETTLED_LIMIT {
        settled.sort();
        let excess = settled.len() - PROMPT_DELIVERY_SETTLED_LIMIT;
        for (_, prompt_id) in settled.into_iter().take(excess) {
            record.prompt_deliveries.remove(&prompt_id);
        }
    }
    sync_prompt_delivery_view(record);
}

/// Restart settlement of a session's receipts, read back from its row. A
/// hand-off that was begun and not accepted is ambiguous: its entry, if
/// still queued, leaves the queue so that it is never replayed. Everything
/// else keeps its disposition.
fn settle_prompt_deliveries_after_load(record: &mut SessionRecord) {
    if record.prompt_deliveries.is_empty() {
        return;
    }
    let mut ambiguous = Vec::new();
    for (prompt_id, delivery) in &mut record.prompt_deliveries {
        delivery.write_ahead_acknowledged = false;
        if delivery.disposition == PromptDeliveryDisposition::HandingOff {
            delivery.disposition = PromptDeliveryDisposition::Ambiguous;
            delivery.reason = Some(
                "TermAl restarted during the hand-off: whether the runtime received the prompt is unknown, so it is never sent again".to_owned(),
            );
            delivery.entry = None;
            ambiguous.push(prompt_id.clone());
        }
    }
    if record
        .queued_prompts
        .iter()
        .any(|queued| ambiguous.contains(&queued.pending_prompt.id))
    {
        let dropped_begin = dropped_engram_intent_grant(
            record,
            record
                .queued_prompts
                .iter()
                .filter(|queued| ambiguous.contains(&queued.pending_prompt.id)),
        );
        record
            .queued_prompts
            .retain(|queued| !ambiguous.contains(&queued.pending_prompt.id));
        record_dropped_engram_intent(record, dropped_begin);
        sync_pending_prompts(record);
    }
    sync_prompt_delivery_view(record);
}

/// Takes an ambiguous receipted entry out of the queue, recording a begin
/// its Engram intent may own, as the restart settlement does
/// (`settle_prompt_deliveries_after_load`).
fn retire_queued_prompt(record: &mut SessionRecord, prompt_id: &str) {
    let dropped_begin = dropped_engram_intent_grant(
        record,
        record
            .queued_prompts
            .iter()
            .filter(|queued| queued.pending_prompt.id == prompt_id),
    );
    record
        .queued_prompts
        .retain(|queued| queued.pending_prompt.id != prompt_id);
    record_dropped_engram_intent(record, dropped_begin);
    sync_pending_prompts(record);
}

/// What `note_prompt_delivery_outcome` records, on the record itself, for a
/// caller that commits in its own transaction: the session's handing-off
/// delivery promoted under `turn_generation` takes `outcome`, once. A
/// rejection, the positive proof that nothing was sent, of an entry that is
/// still queued (an admission that kept its head queued) returns it to
/// pending, with a fresh nonce and no Resume. An ambiguity stays ambiguous
/// even then, since queue membership does not prove that nothing was sent:
/// its entry leaves the queue in this same write, as a restart retires one,
/// so no drain, Resume or reload ever sends it again. Returns the prompt id
/// that moved, if any.
fn apply_prompt_delivery_outcome(
    record: &mut SessionRecord,
    turn_generation: u64,
    outcome: PromptDeliveryOutcome,
) -> Option<String> {
    let prompt_id = record
        .prompt_deliveries
        .iter()
        .filter(|(_, delivery)| {
            delivery.disposition == PromptDeliveryDisposition::HandingOff
                && delivery.turn_generation == Some(turn_generation)
        })
        .max_by_key(|(_, delivery)| delivery.sequence)
        .map(|(prompt_id, _)| prompt_id.clone())?;
    let still_queued = record
        .queued_prompts
        .iter()
        .any(|queued| queued.pending_prompt.id == prompt_id);
    let delivery = record
        .prompt_deliveries
        .get_mut(&prompt_id)
        .expect("the delivery was just found");
    // A failure that cannot say what reached the runtime is ambiguous, unless
    // the prompt's one prompt-carrying write had positively not begun: then
    // nothing was exposed to the runtime, and the attempt is rejected. Once
    // that write has begun, the writer is never admitted again for it.
    let outcome = match outcome {
        PromptDeliveryOutcome::Ambiguous(reason)
            if delivery.runtime_write == PromptRuntimeWrite::Pending =>
        {
            PromptDeliveryOutcome::Rejected(format!(
                "{reason}; the prompt was never written to the runtime"
            ))
        }
        outcome => outcome,
    };
    match outcome {
        PromptDeliveryOutcome::Accepted => {
            delivery.disposition = PromptDeliveryDisposition::Accepted;
            delivery.reason = None;
            delivery.entry = None;
        }
        // A rejection proves nothing was sent: an entry still queued (an
        // admission keeps its head queued until the hand-off) returns to
        // pending rather than blocking the queue.
        PromptDeliveryOutcome::Rejected(reason) if still_queued => {
            delivery.disposition = PromptDeliveryDisposition::Pending;
            delivery.turn_generation = None;
            delivery.write_ahead = Uuid::new_v4().to_string();
            delivery.write_ahead_acknowledged = false;
            delivery.resume = None;
            delivery.entry = None;
            delivery.reason = Some(reason);
        }
        PromptDeliveryOutcome::Rejected(reason) => {
            delivery.disposition = PromptDeliveryDisposition::Rejected;
            delivery.reason = Some(reason);
        }
        PromptDeliveryOutcome::Ambiguous(reason) => {
            delivery.disposition = PromptDeliveryDisposition::Ambiguous;
            delivery.reason = Some(reason);
            delivery.resume = None;
            delivery.entry = None;
            if still_queued {
                retire_queued_prompt(record, &prompt_id);
            }
        }
    }
    prune_prompt_deliveries(record);
    Some(prompt_id)
}

/// Whether the session's receipt of `prompt_id` is still the exact unsent
/// attempt written ahead under `write_ahead`: handing off, acknowledged,
/// never promoted, and its entry still queued. Only such an attempt returns
/// to pending; a promoted, settled, superseded or removed one is left to its
/// own path, and nothing is queued again for it.
fn unsent_attempt_matches(record: &SessionRecord, prompt_id: &str, write_ahead: &str) -> bool {
    record.prompt_deliveries.get(prompt_id).is_some_and(|delivery| {
        delivery.write_ahead == write_ahead
            && delivery.disposition == PromptDeliveryDisposition::HandingOff
            && delivery.write_ahead_acknowledged
            && delivery.turn_generation.is_none()
    }) && record
        .queued_prompts
        .iter()
        .any(|queued| queued.pending_prompt.id == prompt_id)
}

/// The turn generation the queue head's receipted attempt was promoted
/// under, while that attempt is handing off: what a path that ends a
/// retained admission without a hand-off settles (`apply_prompt_delivery_outcome`).
fn head_prompt_delivery_turn(record: &SessionRecord) -> Option<u64> {
    let head = record.queued_prompts.front()?;
    let delivery = record.prompt_deliveries.get(&head.pending_prompt.id)?;
    (delivery.disposition == PromptDeliveryDisposition::HandingOff)
        .then_some(delivery.turn_generation)
        .flatten()
}

/// Settles, at a runtime's retirement, the attempt of `ending_turn` that the
/// retired runtime had not acknowledged. Call after the retirement took the
/// provably unsent deferred carriers (`discard_deferred_stop_callbacks`), in
/// the same write, with the turn captured from the exact runtime that ended:
/// what remains handing off under that turn may have reached the runtime,
/// and no callback can say so any more, so it is ambiguous, and its entry,
/// if still queued, is never sent again (`apply_prompt_delivery_outcome`).
/// Accepted, settled and other turns' receipts are untouched. Returns
/// whether a receipt was settled.
fn settle_retired_runtime_attempt(record: &mut SessionRecord, ending_turn: u64) -> bool {
    apply_prompt_delivery_outcome(
        record,
        ending_turn,
        PromptDeliveryOutcome::Ambiguous(
            "the runtime was retired before it acknowledged the prompt; whether it received it is unknown, so it is never sent again"
                .to_owned(),
        ),
    )
    .is_some()
}

/// The queue head, when it is a receipted prompt whose hand-off has not
/// begun: a promotion refuses it, and its caller wakes the ordinary drain
/// after its own commit (`wake_receipted_head_after_commit`).
fn receipted_head_awaiting_write_ahead(record: &SessionRecord) -> Option<String> {
    let head = record.queued_prompts.front()?;
    let delivery = record.prompt_deliveries.get(&head.pending_prompt.id)?;
    (delivery.disposition == PromptDeliveryDisposition::Pending)
        .then(|| head.pending_prompt.id.clone())
}

/// What a session shows while an automatic Engram retry of its receipted
/// head is parked until an explicit Resume.
const PROMPT_DELIVERY_PARKED_FOR_RESUME_PREVIEW: &str = "Paused, waiting for Resume: this receipted prompt is sent only after an explicit Resume; its automatic retry is parked and charges no attempt.";

/// Why an automatic Engram retry (bind, abort or admission) of the queue
/// head must park instead of beginning an attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiptedRetryPark {
    /// The paused queue may send this receipted head only on an explicit
    /// Resume, and no Resume authority holds for it.
    WaitingForResume,
    /// A Resume's write-ahead for the head is still in flight: its
    /// acknowledgement continues the head, so a retry now is no attempt.
    WriteAheadInFlight,
}

/// Whether an automatic retry of the session's head must park, and why: it
/// could only be refused at the receipt gate, so it begins nothing and is
/// charged nothing, and its retry record, lineage and spent count stay as
/// they are. `None` when the head is not receipted, or its attempt may go on.
fn receipted_head_retry_park(record: &SessionRecord) -> Option<ReceiptedRetryPark> {
    let head = record.queued_prompts.front()?;
    let prompt_id = &head.pending_prompt.id;
    let delivery = record.prompt_deliveries.get(prompt_id)?;
    let paused_without_resume =
        record.orchestrator_auto_dispatch_blocked && !prompt_delivery_resume_holds(record, prompt_id);
    match delivery.disposition {
        PromptDeliveryDisposition::Pending if paused_without_resume => {
            Some(ReceiptedRetryPark::WaitingForResume)
        }
        PromptDeliveryDisposition::HandingOff if delivery.turn_generation.is_none() => {
            if !delivery.write_ahead_acknowledged {
                Some(ReceiptedRetryPark::WriteAheadInFlight)
            } else if paused_without_resume {
                Some(ReceiptedRetryPark::WaitingForResume)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Parks an automatic retry of the session's head that
/// `receipted_head_retry_park` says must park: nothing is begun or charged,
/// and a head waiting for Resume says so. Returns whether it parked and
/// whether the session changed (to stamp and commit). Call with the state
/// lock held.
fn park_receipted_head_retry(record: &mut SessionRecord) -> (bool, bool) {
    match receipted_head_retry_park(record) {
        None => (false, false),
        Some(ReceiptedRetryPark::WriteAheadInFlight) => (true, false),
        Some(ReceiptedRetryPark::WaitingForResume) => {
            let changed = record.session.preview != PROMPT_DELIVERY_PARKED_FOR_RESUME_PREVIEW;
            if changed {
                record.session.preview = PROMPT_DELIVERY_PARKED_FOR_RESUME_PREVIEW.to_owned();
            }
            (true, changed)
        }
    }
}

/// The reason a removed, never handed-off head's receipt records.
const QUEUE_REMOVAL_UNSENT_REASON: &str =
    "the prompt left the queue before it reached the runtime; it was not sent";

/// The queue head promoted for its hand-off, as a removal captures it before
/// it changes the queue: its prompt and its attempt's turn.
fn handing_off_head(record: &SessionRecord) -> Option<(String, u64)> {
    let turn_generation = head_prompt_delivery_turn(record)?;
    let head = record.queued_prompts.front()?;
    Some((head.pending_prompt.id.clone(), turn_generation))
}

/// Settles the receipt of a head that `handing_off_head` captured before a
/// removal, once that removal has taken it out of the queue: an admission
/// keeps its head queued until the hand-off, so a head removed while still
/// queued was never sent, and its receipt turns terminal in the removal's own
/// write. A head the removal kept stays as it is. Returns whether a receipt
/// was settled.
fn settle_removed_handing_off_head(
    record: &mut SessionRecord,
    head: Option<(String, u64)>,
    reason: &str,
) -> bool {
    let Some((prompt_id, turn_generation)) = head else {
        return false;
    };
    if record
        .queued_prompts
        .iter()
        .any(|queued| queued.pending_prompt.id == prompt_id)
    {
        return false;
    }
    apply_prompt_delivery_outcome(
        record,
        turn_generation,
        PromptDeliveryOutcome::Rejected(reason.to_owned()),
    )
    .is_some()
}

/// Discards a session's deferred Stop callbacks, as a path that ends them
/// without replay does. A deferred Engram hand-off is the only carrier of its
/// attempt's prepared dispatch: taking that dispatch out under the carrier's
/// own mutex proves no other path took it for a hand-off, so the attempt was
/// never sent, and its receipt is settled in this same write (pending while
/// its retained head is still queued, otherwise rejected). A carrier already
/// emptied belongs to the path that took it, which keeps that receipt's own
/// course. Returns whether any receipt was settled.
fn discard_deferred_stop_callbacks(record: &mut SessionRecord) -> bool {
    let callbacks = std::mem::take(&mut record.deferred_stop_callbacks);
    let mut settled = false;
    for callback in callbacks {
        let DeferredStopCallback::EngramHandoff {
            active_turn_generation,
            dispatch,
        } = callback
        else {
            continue;
        };
        let taken = dispatch
            .0
            .lock()
            .expect("deferred handoff mutex poisoned")
            .take();
        if taken.is_some() {
            settled |= apply_prompt_delivery_outcome(
                record,
                active_turn_generation,
                PromptDeliveryOutcome::Rejected(
                    "the deferred hand-off was discarded before it reached the runtime; the prompt was not sent"
                        .to_owned(),
                ),
            )
            .is_some();
        }
    }
    settled
}

/// The runtime's identity for a promoted attempt.
fn prompt_delivery_attempt_key(dispatch: &TurnDispatch, attempt: u32) -> String {
    match dispatch {
        TurnDispatch::PersistentClaude { command, .. } => command.replay_generation.clone(),
        _ => format!(
            "{}:{}:{attempt}",
            dispatch.session_id(),
            dispatch.active_turn_generation()
        ),
    }
}

/// The command uuid a top-level Claude `command_lifecycle` frame reports
/// as `queued` or `started`: Claude received that command.
fn claude_lifecycle_receipt_uuid(message: &Value) -> Option<&str> {
    if message.get("type").and_then(Value::as_str) != Some("command_lifecycle")
        || !claude_frame_is_top_level(message)
        || !matches!(
            message.get("state").and_then(Value::as_str),
            Some("queued" | "started")
        )
    {
        return None;
    }
    message.get("command_uuid").and_then(Value::as_str)
}

/// Marks the receipted entry being promoted: its write-ahead hand-off record
/// now names the turn, the transcript message and the runtime's attempt key.
/// A Codex command carries that exact attempt from here to its turn/start
/// write, and the attempt is known unwritten until that write begins.
fn mark_prompt_delivery_promoted(
    record: &mut SessionRecord,
    queued: &QueuedPromptRecord,
    dispatch: &mut TurnDispatch,
    message_id: &str,
) {
    let active_turn_generation = dispatch.active_turn_generation();
    let Some(delivery) = record.prompt_deliveries.get_mut(&queued.pending_prompt.id) else {
        return;
    };
    delivery.turn_generation = Some(active_turn_generation);
    delivery.message_id = Some(message_id.to_owned());
    delivery.attempt_key = Some(prompt_delivery_attempt_key(dispatch, delivery.attempt));
    delivery.entry = Some(queued.clone());
    if let TurnDispatch::PersistentCodex { command, .. } = dispatch {
        delivery.runtime_write = PromptRuntimeWrite::Pending;
        command.delivery_receipt = Some(CodexPromptReceipt {
            prompt_id: queued.pending_prompt.id.clone(),
            write_ahead: delivery.write_ahead.clone(),
        });
    }
    sync_prompt_delivery_view(record);
}

#[cfg_attr(not(test), allow(dead_code))]
impl AppState {
    /// The gate a receipted queue head passes before promotion. True when it
    /// is not receipted, or when its write-ahead hand-off record has been
    /// acknowledged durable and, on a paused queue, the explicit Resume that
    /// began that attempt still holds (`prompt_delivery_resume_holds`). On a
    /// queue that is not paused it otherwise starts a write-ahead (or waits
    /// for the one in flight) and holds the queue; the acknowledgement
    /// dispatches it. A paused queue's head begins its write-ahead only from
    /// an explicit Resume (`resume_receipted_head`), never from another
    /// drain. With `promote`, this is the promotion's own gate, in the same
    /// critical section as the promotion: it consumes the Resume it relies
    /// on, and it never begins a write-ahead, since its caller's transaction
    /// owns that section's commit. A pending head refuses the promotion
    /// there; the caller wakes the ordinary drain once its own commit
    /// succeeded (`wake_receipted_head_after_commit`), and that drain begins
    /// the write-ahead. Call with the state lock held.
    fn prompt_delivery_gate_locked(
        &self,
        inner: &mut StateInner,
        index: usize,
        prompt_id: &str,
        promote: bool,
    ) -> bool {
        // Ordinary prompts pass without touching the record.
        if !inner.sessions[index].prompt_deliveries.contains_key(prompt_id) {
            return true;
        }
        let paused = inner.sessions[index].orchestrator_auto_dispatch_blocked;
        let resume_holds = paused && prompt_delivery_resume_holds(&inner.sessions[index], prompt_id);
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let Some(delivery) = record.prompt_deliveries.get_mut(prompt_id) else {
            return true;
        };
        match delivery.disposition {
            PromptDeliveryDisposition::HandingOff => {
                if !delivery.write_ahead_acknowledged || (paused && !resume_holds) {
                    return false;
                }
                if promote {
                    delivery.resume = None;
                }
                return true;
            }
            PromptDeliveryDisposition::Pending if paused || promote => return false,
            PromptDeliveryDisposition::Pending => {}
            // A settled delivery is never handed off again.
            PromptDeliveryDisposition::Accepted
            | PromptDeliveryDisposition::Rejected
            | PromptDeliveryDisposition::Ambiguous => return false,
        }
        self.begin_prompt_delivery_write_ahead_locked(inner, index, prompt_id, false);
        false
    }

    /// Begins a new hand-off attempt of the receipted head `prompt_id`: a
    /// write-ahead record under a fresh nonce, committed, and the request for
    /// its acknowledgement. With `explicit_resume`, the attempt carries the
    /// continuation of the explicit Resume of the paused queue that began it.
    fn begin_prompt_delivery_write_ahead_locked(
        &self,
        inner: &mut StateInner,
        index: usize,
        prompt_id: &str,
        explicit_resume: bool,
    ) {
        let session_id = inner.sessions[index].session.id.clone();
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let content = record
            .queued_prompts
            .front()
            .filter(|head| head.pending_prompt.id == prompt_id)
            .map(receipted_prompt_intent);
        let (dispatch_generation, active_turn_generation) =
            (record.engram.dispatch_generation, record.active_turn_generation);
        let (stop_generation, pause_epoch) =
            (record.runtime_stop_generation, record.auto_dispatch_pause_epoch);
        let Some(delivery) = record.prompt_deliveries.get_mut(prompt_id) else {
            return;
        };
        let write_ahead = Uuid::new_v4().to_string();
        delivery.disposition = PromptDeliveryDisposition::HandingOff;
        delivery.attempt = delivery.attempt.saturating_add(1);
        delivery.write_ahead = write_ahead.clone();
        delivery.write_ahead_acknowledged = false;
        delivery.resume = match (explicit_resume, content) {
            (true, Some(content)) => Some(PromptDeliveryResume {
                dispatch_generation,
                active_turn_generation,
                intent: content,
                write_ahead: write_ahead.clone(),
                stop_generation,
                pause_epoch,
            }),
            _ => None,
        };
        delivery.reason = None;
        sync_prompt_delivery_view(record);
        if let Err(error) = self.commit_locked(inner) {
            // The fence then fails, and the prompt returns to pending.
            eprintln!(
                "prompt delivery> failed to persist the hand-off record of `{prompt_id}` in session `{session_id}`: {error:#}"
            );
        }
        self.request_prompt_delivery_write_ahead(&session_id, prompt_id, &write_ahead);
    }

    /// An explicit Resume of the paused queue meeting a receipted head that
    /// is already durable (an unsaved one was refused before this), after the
    /// Engram release: under this lock it re-verifies exactly what the Resume
    /// accepted (`accepted`: the head, its content, the Stop and pause it
    /// crossed) and the resulting owner, and refuses with why if any of it
    /// changed, beginning nothing. It then begins the head's write-ahead
    /// itself, with its continuation captured from this verified state, and
    /// reports that the hand-off is being prepared. A repeated Resume while
    /// that attempt is in flight reports the same operation and begins no
    /// other. A hand-off that another attempt holds is refused with why.
    fn resume_receipted_head(
        &self,
        session_id: &str,
        owner: Option<&EngramQueuedAdmissionOwner>,
        accepted: &ReceiptedResumeIdentity,
    ) -> ReceiptedResume {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_visible_session_index(session_id) else {
            return ReceiptedResume::NotReceipted;
        };
        let record = &inner.sessions[index];
        // Unpaused meanwhile: the Resume goes on as for any head.
        if !record.orchestrator_auto_dispatch_blocked {
            return ReceiptedResume::NotReceipted;
        }
        let unchanged = record.queued_prompts.front().is_some_and(|head| {
            head.pending_prompt.id == accepted.prompt_id
                && receipted_prompt_intent(head) == accepted.intent
        }) && record.runtime_stop_generation == accepted.stop_generation
            && record.auto_dispatch_pause_epoch == accepted.pause_epoch
            && owner.is_none_or(|owner| owner.matches(record));
        if !unchanged {
            return ReceiptedResume::Refused(RESUME_QUEUE_CHANGED_REFUSAL.to_owned());
        }
        let prompt_id = accepted.prompt_id.clone();
        // On a busy or stopping session no turn could start: the Resume goes
        // on as for any head and begins no attempt it cannot use.
        if record.runtime_stop_in_progress
            || matches!(
                record.session.status,
                SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
            )
        {
            return ReceiptedResume::NotReceipted;
        }
        let Some(delivery) = record.prompt_deliveries.get(&prompt_id) else {
            return ReceiptedResume::NotReceipted;
        };
        match delivery.disposition {
            PromptDeliveryDisposition::Pending => {}
            PromptDeliveryDisposition::HandingOff
                if prompt_delivery_resume_holds(record, &prompt_id) =>
            {
                return ReceiptedResume::Preparing;
            }
            PromptDeliveryDisposition::HandingOff => {
                return ReceiptedResume::Refused(
                    "the next queued prompt's hand-off record is being saved by another \
                     dispatch; Resume again once it settles"
                        .to_owned(),
                );
            }
            PromptDeliveryDisposition::Accepted
            | PromptDeliveryDisposition::Rejected
            | PromptDeliveryDisposition::Ambiguous => return ReceiptedResume::NotReceipted,
        }
        self.begin_prompt_delivery_write_ahead_locked(&mut inner, index, &prompt_id, true);
        ReceiptedResume::Preparing
    }

    /// Asks the persist writer to prove the write-ahead hand-off record
    /// durable. Never waits; safe with the state lock held, since it only
    /// sends to the writer and settles on another thread.
    fn request_prompt_delivery_write_ahead(
        &self,
        session_id: &str,
        prompt_id: &str,
        write_ahead: &str,
    ) {
        let (fence, _unwaited) = PersistFence::new(
            PersistFenceTarget::PromptDeliveryWriteAhead {
                session_id: session_id.to_owned(),
                prompt_id: prompt_id.to_owned(),
                write_ahead: write_ahead.to_owned(),
            },
            std::time::Instant::now() + PROMPT_DELIVERY_WRITE_AHEAD_BUDGET,
        );
        let state = self.clone();
        let session_id = session_id.to_owned();
        let prompt_id = prompt_id.to_owned();
        let write_ahead = write_ahead.to_owned();
        // A fence the writer never receives resolves on drop, and its
        // settlement returns the prompt to pending. The writer thread only
        // hands the outcome over: a thread the OS refuses must not unwind the
        // writer, so the hand-off record then stays unacknowledged
        // (fail-closed: nothing is promoted from it) and the refusal is
        // logged, as for a queued prompt's durability hold.
        let fence = fence.on_resolved(move |result| {
            let spawned = std::thread::Builder::new()
                .name("prompt-delivery-write-ahead".to_owned())
                .spawn(move || {
                    state.settle_prompt_delivery_write_ahead(
                        &session_id,
                        &prompt_id,
                        &write_ahead,
                        result,
                    );
                });
            if let Err(error) = spawned {
                eprintln!(
                    "prompt delivery> could not start the write-ahead settlement thread; the hand-off record stays unacknowledged: {error}"
                );
            }
        });
        let _ = self.persist_tx.send(PersistRequest::Fence(Box::new(fence)));
    }

    /// Applies a write-ahead fence's outcome. Only the outcome for the
    /// current write-ahead acts. Its acknowledgement lets the entry be
    /// promoted and dispatches the queue. A failure returns the prompt to
    /// pending with a durable write that supersedes the hand-off record, under
    /// a fresh write-ahead nonce, so a late acknowledgement settles nothing.
    fn settle_prompt_delivery_write_ahead(
        &self,
        session_id: &str,
        prompt_id: &str,
        write_ahead: &str,
        result: PersistFenceResult,
    ) {
        let release = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            // The explicit Resume that began this attempt carries it across
            // the pause only while it still holds; its drain is bound to the
            // head it was captured for, and the promotion's own gate checks
            // and consumes it again.
            let resume_owner = (record.orchestrator_auto_dispatch_blocked
                && prompt_delivery_resume_holds(record, prompt_id))
            .then(|| {
                record.prompt_deliveries[prompt_id]
                    .resume
                    .as_ref()
                    .map(|resume| resume.owner(prompt_id))
            })
            .flatten();
            let Some(delivery) = record.prompt_deliveries.get_mut(prompt_id) else {
                return;
            };
            if delivery.write_ahead != write_ahead
                || delivery.disposition != PromptDeliveryDisposition::HandingOff
                || delivery.write_ahead_acknowledged
                || delivery.turn_generation.is_some()
            {
                return;
            }
            match result {
                Ok(()) => {
                    delivery.write_ahead_acknowledged = true;
                    if resume_owner.is_none() {
                        delivery.resume = None;
                    }
                    Some(resume_owner)
                }
                Err(error) => {
                    delivery.disposition = PromptDeliveryDisposition::Pending;
                    delivery.write_ahead = Uuid::new_v4().to_string();
                    delivery.resume = None;
                    delivery.reason = Some(format!(
                        "the hand-off record was not acknowledged durable ({error:?}); the prompt was not sent"
                    ));
                    sync_prompt_delivery_view(record);
                    if let Err(error) = self.commit_locked(&mut inner) {
                        eprintln!(
                            "prompt delivery> failed to persist the return of `{prompt_id}` in session `{session_id}` to pending: {error:#}"
                        );
                    }
                    None
                }
            }
        };
        if let Some(resume_owner) = release {
            self.dispatch_after_write_ahead(session_id, prompt_id, resume_owner);
            self.return_unpromoted_write_ahead_to_pending(session_id, prompt_id, write_ahead);
        }
    }

    /// The dispatch an acknowledged write-ahead releases: the ordinary drain,
    /// which never passes a pause, or, for an attempt an explicit Resume began
    /// and still holds, the Resume drain bound to `resume_owner`, the head it
    /// was captured for. That drain promotes nothing else, and the promotion's
    /// gate re-checks and consumes the Resume under the promotion's lock.
    fn dispatch_after_write_ahead(
        &self,
        session_id: &str,
        prompt_id: &str,
        resume_owner: Option<EngramQueuedAdmissionOwner>,
    ) {
        let drained = match resume_owner {
            Some(owner) => self.dispatch_next_queued_turn_for_resume(session_id, owner),
            None => self.dispatch_next_queued_turn(session_id, false),
        };
        match drained {
            Ok(Some(dispatch)) => {
                if let TurnDispatchDeliveryOutcome::Rejected(error) =
                    deliver_turn_dispatch(self, dispatch)
                {
                    eprintln!(
                        "prompt delivery> the runtime rejected the dispatch of `{prompt_id}` in session `{session_id}`: {}",
                        error.message
                    );
                }
            }
            Ok(None) => {}
            Err(error) => eprintln!(
                "prompt delivery> failed to dispatch `{prompt_id}` in session `{session_id}` after its hand-off record: {error:#}"
            ),
        }
    }

    /// The explicit continuation of a transition whose promotion refused a
    /// pending receipted head (a Stop's successor, a direct enqueue), run
    /// only once that transition's own commit succeeded and its deltas were
    /// published: the ordinary drain, which revalidates the session, its head
    /// and every Stop, pause, admission and durability fence under its own
    /// lock, carries no bypass of a pause, and begins the head's write-ahead
    /// (or awaits the one in flight); that acknowledgement then dispatches
    /// it. A head that changed meanwhile is the changing operation's to
    /// drain.
    fn wake_receipted_head_after_commit(&self, session_id: &str, prompt_id: &str) {
        match self.dispatch_next_queued_turn(session_id, false) {
            Ok(Some(dispatch)) => {
                if let TurnDispatchDeliveryOutcome::Rejected(error) =
                    deliver_turn_dispatch(self, dispatch)
                {
                    eprintln!(
                        "prompt delivery> the runtime rejected the dispatch after `{prompt_id}`'s wake in session `{session_id}`: {}",
                        error.message
                    );
                }
            }
            Ok(None) => {}
            Err(error) => eprintln!(
                "prompt delivery> failed to drain session `{session_id}` for `{prompt_id}`: {error:#}"
            ),
        }
    }

    /// A direct enqueue whose committed head did not start: when that head is
    /// a receipted prompt whose hand-off has not begun, the enqueue's commit
    /// is the event that wakes the ordinary drain for it, once the lock is
    /// released.
    fn queued_after_direct_enqueue(
        &self,
        inner: StateMutexGuard<'_, StateInner>,
        index: usize,
        session_id: &str,
    ) -> DispatchTurnResult {
        let wake = receipted_head_awaiting_write_ahead(&inner.sessions[index]);
        drop(inner);
        if let Some(prompt_id) = wake {
            self.wake_receipted_head_after_commit(session_id, &prompt_id);
        }
        DispatchTurnResult::Queued
    }

    /// After an acknowledged write-ahead's dispatch: if the entry was not
    /// promoted (the session was busy or paused, or another prompt went
    /// first), nothing was sent, and a hand-off record that stayed durable
    /// would make a restart drop a prompt that never reached a runtime. It
    /// returns to pending by a superseding write; the next dispatch of the
    /// queue starts a new attempt. While the queued admission of this very
    /// prompt is evaluating it off-lock (it passed the receipt gate after the
    /// acknowledgement, and this dispatch only left it a wake), the attempt
    /// is left to that admission instead, bound to its guard token, the
    /// prompt and this exact nonce, in this same critical section: that
    /// admission promotes the attempt, or returns it to pending when its
    /// guard is released (`release_admission_attempt_locked`). A return to
    /// pending here would make its promotion refuse the head and abandon its
    /// admission, stranding the queue.
    fn return_unpromoted_write_ahead_to_pending(
        &self,
        session_id: &str,
        prompt_id: &str,
        write_ahead: &str,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        if !unsent_attempt_matches(&inner.sessions[index], prompt_id, write_ahead) {
            return;
        }
        let record = &mut inner.sessions[index];
        let admitting = record
            .engram
            .admission_in_progress
            .as_ref()
            .filter(|_| {
                record.engram.admission_prompt_id.as_deref() == Some(prompt_id)
                    && record
                        .queued_prompts
                        .front()
                        .is_some_and(|head| head.pending_prompt.id == prompt_id)
            })
            .cloned();
        if let Some(token) = admitting {
            record.engram.admission_attempt = Some(EngramAdmissionAttempt {
                token,
                prompt_id: prompt_id.to_owned(),
                write_ahead: write_ahead.to_owned(),
            });
            return;
        }
        self.return_unsent_attempt_to_pending_locked(&mut inner, index, prompt_id);
    }

    /// Admits a Codex command to its turn/start write, the one write that
    /// carries its prompt, and records under the state lock, before any I/O,
    /// that the prompt may now escape. A command that carries no receipt was
    /// never receipted and is admitted as before. A receipted one is admitted
    /// once, and only while its exact attempt (runtime, turn, prompt and
    /// write-ahead nonce) is still handing off unwritten; a settled, reset,
    /// superseded or removed attempt fails closed, so an attempt settled
    /// rejected is never written afterwards.
    fn begin_prompt_delivery_runtime_write(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        active_turn_generation: u64,
        receipt: Option<&CodexPromptReceipt>,
    ) -> bool {
        let Some(receipt) = receipt else {
            return true;
        };
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return false;
        };
        let record = &mut inner.sessions[index];
        if !record.runtime.matches_runtime_token(token)
            || record.active_turn_generation != active_turn_generation
        {
            return false;
        }
        let Some(delivery) = record.prompt_deliveries.get_mut(&receipt.prompt_id) else {
            return false;
        };
        if delivery.write_ahead != receipt.write_ahead
            || delivery.turn_generation != Some(active_turn_generation)
            || delivery.disposition != PromptDeliveryDisposition::HandingOff
            || delivery.runtime_write != PromptRuntimeWrite::Pending
        {
            return false;
        }
        delivery.runtime_write = PromptRuntimeWrite::Begun;
        true
    }

    /// The release of the admission guard that `attempt` was left to: the
    /// admission ended, by a promotion or without one. If it is still that
    /// exact unsent attempt (same prompt and nonce, acknowledged, never
    /// promoted, still queued), it returns to pending; a promoted, settled,
    /// superseded or removed one is left to its own path. Never dispatches
    /// or waits; call with the state lock held, from the guard's release.
    fn release_admission_attempt_locked(
        &self,
        inner: &mut StateInner,
        index: usize,
        attempt: EngramAdmissionAttempt,
    ) {
        if unsent_attempt_matches(&inner.sessions[index], &attempt.prompt_id, &attempt.write_ahead) {
            self.return_unsent_attempt_to_pending_locked(inner, index, &attempt.prompt_id);
        }
    }

    /// Returns the session's unsent attempt of `prompt_id` to pending by a
    /// superseding write, and commits. The pause, Stop and retry holds of the
    /// queue are its own and stay as they are. An in-memory pending is not a
    /// durable settlement: until the writer saves it, a restart reads the
    /// earlier row, and no send can follow before a fresh write-ahead is read
    /// back acknowledged.
    fn return_unsent_attempt_to_pending_locked(
        &self,
        inner: &mut StateInner,
        index: usize,
        prompt_id: &str,
    ) {
        let session_id = inner.sessions[index].session.id.clone();
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let delivery = record
            .prompt_deliveries
            .get_mut(prompt_id)
            .expect("the delivery was just found");
        // The superseding write ends the attempt, and with it any Resume that
        // began it: the next one needs a fresh explicit Resume or drain.
        delivery.disposition = PromptDeliveryDisposition::Pending;
        delivery.write_ahead = Uuid::new_v4().to_string();
        delivery.write_ahead_acknowledged = false;
        delivery.resume = None;
        delivery.reason = Some(
            "not promoted when its hand-off record became durable; the prompt was not sent"
                .to_owned(),
        );
        sync_prompt_delivery_view(record);
        if let Err(error) = self.commit_locked(inner) {
            eprintln!(
                "prompt delivery> failed to persist the return of `{prompt_id}` in session `{session_id}` to pending: {error:#}"
            );
        }
    }

    /// Records what a runtime event or a hand-off result says about the
    /// session's handing-off delivery promoted under `turn_generation`. Only
    /// a handing-off delivery moves, and once: the first settlement stands. A
    /// rejection of an entry that is still queued (an admission that will be
    /// retried) returns it to pending instead.
    fn note_prompt_delivery_outcome(
        &self,
        session_id: &str,
        turn_generation: Option<u64>,
        outcome: PromptDeliveryOutcome,
    ) {
        // Every caller names the turn: an outcome is never matched to
        // whichever receipt happens to be handing off.
        let Some(turn_generation) = turn_generation else {
            return;
        };
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        if inner.sessions[index].prompt_deliveries.is_empty() {
            return;
        }
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let Some(prompt_id) = apply_prompt_delivery_outcome(record, turn_generation, outcome) else {
            return;
        };
        if let Err(error) = self.commit_locked(&mut inner) {
            eprintln!(
                "prompt delivery> failed to persist the delivery outcome of `{prompt_id}` in session `{session_id}`: {error:#}"
            );
        }
    }

    /// What a direct hand-off of a prepared dispatch says about its attempt's
    /// receipt, for a caller outside the ordinary delivery
    /// (`deliver_turn_dispatch_now`): a parked or deferred dispatch released
    /// later. `handoff_prepared_turn_dispatch` fails only before its channel
    /// send (the Codex Fast tier preparation, or an unbounded channel whose
    /// receiver is gone, which hands the prompt back undelivered), so its error
    /// and its supersession both prove the prompt was never sent: the attempt
    /// is rejected, or returns to pending while its retained head is queued.
    /// A deferred (Scheduled) hand-off moved the attempt to its new carrier,
    /// and a delivered one waits for the runtime's acceptance: both keep the
    /// receipt as it is. Only the attempt promoted under
    /// `active_turn_generation` that is still handing off moves.
    fn note_direct_hand_off_outcome(
        &self,
        session_id: &str,
        active_turn_generation: u64,
        outcome: &std::result::Result<HandoffPreparedTurnDispatchOutcome, ApiError>,
    ) {
        let reason = match outcome {
            Ok(HandoffPreparedTurnDispatchOutcome::Delivered)
            | Ok(HandoffPreparedTurnDispatchOutcome::Scheduled) => return,
            Ok(HandoffPreparedTurnDispatchOutcome::Superseded) => {
                "the dispatch was superseded before it reached the runtime".to_owned()
            }
            Err(error) => error.message.clone(),
        };
        self.note_prompt_delivery_outcome(
            session_id,
            Some(active_turn_generation),
            PromptDeliveryOutcome::Rejected(reason),
        );
    }

    /// Settles the handing-off receipt of the turn that `token`'s runtime is
    /// running on the session now, if any: for an event that names its
    /// runtime but not its turn (a turn's end, a runtime's exit, a failure
    /// before the prompt was written). An event of an earlier runtime
    /// settles nothing, so a late reply never settles a successor's receipt.
    fn settle_prompt_delivery_of_runtime_turn(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        expected_turn_generation: Option<u64>,
        outcome: PromptDeliveryOutcome,
    ) {
        let turn_generation = self
            .prompt_delivery_turn_of_runtime(session_id, token)
            .filter(|current| expected_turn_generation.is_none_or(|expected| expected == *current));
        self.note_prompt_delivery_outcome(session_id, turn_generation, outcome);
    }

    /// Accepts the session's handing-off delivery whose runtime attempt key
    /// is `attempt_key`, reported by `token`'s runtime (Claude's command
    /// uuid in a lifecycle frame).
    fn accept_prompt_delivery_by_attempt_key(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        attempt_key: &str,
    ) {
        let turn_generation = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            let record = &inner.sessions[index];
            if !record.runtime.matches_runtime_token(token) {
                return;
            }
            let Some(turn_generation) = record
                .prompt_deliveries
                .values()
                .find(|delivery| {
                    delivery.disposition == PromptDeliveryDisposition::HandingOff
                        && delivery.attempt_key.as_deref() == Some(attempt_key)
                })
                .and_then(|delivery| delivery.turn_generation)
            else {
                return;
            };
            turn_generation
        };
        self.note_prompt_delivery_outcome(
            session_id,
            Some(turn_generation),
            PromptDeliveryOutcome::Accepted,
        );
    }

    /// The turn `token`'s runtime is running on the session now, when the
    /// session keeps any receipt. A runtime that cannot name its turn reads
    /// it here when it takes up the prompt, and settles that turn later.
    fn prompt_delivery_turn_of_runtime(
        &self,
        session_id: &str,
        token: &RuntimeToken,
    ) -> Option<u64> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(session_id)?;
        let record = &inner.sessions[index];
        (!record.prompt_deliveries.is_empty() && record.runtime.matches_runtime_token(token))
            .then_some(record.active_turn_generation)
    }

    /// Re-queues a rejected delivery's prompt under a new prompt id, held for
    /// a new durability acknowledgement with a fresh nonce that no earlier
    /// acknowledgement can match. The rejected receipt stays as it was.
    /// Returns the new prompt id.
    fn requeue_rejected_prompt_delivery(
        &self,
        session_id: &str,
        prompt_id: &str,
    ) -> Result<String> {
        self.requeue_rejected_prompt_delivery_at(session_id, prompt_id, false)
    }

    /// `requeue_rejected_prompt_delivery`, at the front of the queue when
    /// `at_front` (a workflow prompt keeps its place ahead of prompts queued
    /// meanwhile, as the ordinary orphan re-queue does).
    fn requeue_rejected_prompt_delivery_at(
        &self,
        session_id: &str,
        prompt_id: &str,
        at_front: bool,
    ) -> Result<String> {
        let new_prompt_id = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let new_prompt_id = inner.next_message_id();
            // What a failed commit restores: the retry entry stays on the
            // rejected receipt, and no tentative prompt, receipt or
            // durability hold is left behind without its acknowledgement
            // request.
            let before = inner.sessions[index].clone();
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            let delivery = record
                .prompt_deliveries
                .get_mut(prompt_id)
                .filter(|delivery| delivery.disposition == PromptDeliveryDisposition::Rejected)
                .ok_or_else(|| anyhow!("prompt `{prompt_id}` has no rejected delivery"))?;
            let mut entry = delivery
                .entry
                .take()
                .ok_or_else(|| anyhow!("prompt `{prompt_id}` was already re-queued"))?;
            entry.pending_prompt.id = new_prompt_id.clone();
            entry.pending_prompt.timestamp = stamp_now();
            queue_receipted_orchestrator_prompt_on_record(
                record,
                entry.pending_prompt,
                entry.attachments,
            );
            if at_front && let Some(requeued) = record.queued_prompts.pop_back() {
                record.queued_prompts.push_front(requeued);
                sync_pending_prompts(record);
            }
            if let Err(error) = self.commit_locked(&mut inner) {
                inner.sessions[index] = before;
                return Err(error.context(format!(
                    "failed to persist the re-queue of `{prompt_id}`; it stays retryable"
                )));
            }
            new_prompt_id
        };
        self.request_queued_prompt_durability(session_id, &new_prompt_id);
        Ok(new_prompt_id)
    }
}
