// The opt-in durability prerequisite of a queued prompt: a producer that must
// never let its prompt reach a runtime before the prompt is durable enqueues
// it held, asks the persist writer to prove the exact entry durable, and only
// that content-specific acknowledgement releases it for dispatch.
//
// Owns the hold (prompt id, a fresh nonce and the entry's exact serialized
// content), the predicate every promotion path consults, the request for the
// acknowledgement, its settlement off the writer thread, and how a Resume of a
// paused queue that a held head stops is refused. Does not own the
// queue itself (session_interaction.rs), promotion (turn_dispatch.rs), the
// fence's read-back (persist_fence.rs) or any producer's own record. Prompts
// enqueued without a hold behave exactly as before. New module beside
// session_interaction.rs and turn_dispatch.rs.

/// How long the writer has to prove a held entry durable. A fence that runs
/// out leaves the entry held with that reason; nothing retries it by itself.
/// The producers of held prompts come in a later change; until then only
/// tests reach the request path.
#[cfg_attr(not(test), allow(dead_code))]
const QUEUED_PROMPT_DURABILITY_BUDGET: Duration = Duration::from_secs(30);

/// A queued prompt no entry point may promote until a persistence
/// acknowledgement of exactly this content.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueuedPromptDurabilityHold {
    /// Fresh for every hold, so an acknowledgement of an earlier hold or of a
    /// replaced entry with the same prompt id never matches this one.
    nonce: String,
    /// The queued entry exactly as it was held.
    content: Value,
    /// Why the last acknowledgement did not arrive, when it did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure: Option<String>,
    /// The exact entry was proven durable, here or by reading it back after
    /// a restart. The entry may now be promoted; the hold stays, as a marker
    /// that keeps the entry standalone, until the entry leaves the queue.
    #[serde(default, skip_serializing_if = "session_flag_is_false")]
    acknowledged: bool,
}

fn queued_prompt_durability_content(queued: &QueuedPromptRecord) -> Value {
    serde_json::to_value(queued).unwrap_or(Value::Null)
}

/// Enqueues an orchestrator prompt held until its exact content is durable,
/// and returns the hold's nonce. The caller commits, then asks for the
/// acknowledgement with `request_queued_prompt_durability`.
///
/// The prompt always gets its own queue entry under its own id: it is never
/// merged into an earlier peer message batch, and while it is queued no later
/// peer prompt is appended to it (`queued_prompt_kept_standalone`).
#[cfg_attr(not(test), allow(dead_code))]
fn queue_durable_orchestrator_prompt_on_record(
    record: &mut SessionRecord,
    pending_prompt: PendingPrompt,
    attachments: Vec<PromptImageAttachment>,
) -> String {
    let queued = QueuedPromptRecord {
        engram_waiting: false,
        promoted_message_index: None,
        promotion_disposition_known: true,
        engram_bind: None,
        engram_evaluate: None,
        engram_interrupted: false,
        source: QueuedPromptSource::Orchestrator,
        attachments,
        pending_prompt,
    };
    let prompt_id = queued.pending_prompt.id.clone();
    let nonce = Uuid::new_v4().to_string();
    record.queued_prompt_durability_holds.insert(
        prompt_id,
        QueuedPromptDurabilityHold {
            nonce: nonce.clone(),
            content: queued_prompt_durability_content(&queued),
            failure: None,
            acknowledged: false,
        },
    );
    record.queued_prompts.push_back(queued);
    sync_pending_prompts(record);
    nonce
}

/// Whether `queued` waits for its durability acknowledgement. Every
/// promotion path consults this; a held head also blocks the entries behind
/// it, since the queue is first in, first out, and like an Engram-retained
/// head it is never overtaken by reordering or front insertion.
fn queued_prompt_held_for_durability(record: &SessionRecord, queued: &QueuedPromptRecord) -> bool {
    record
        .queued_prompt_durability_holds
        .get(&queued.pending_prompt.id)
        .is_some_and(|hold| !hold.acknowledged)
}

/// Whether `queued` was enqueued with a durability prerequisite, held or
/// already acknowledged: its content stays exactly what was proven, so no
/// peer prompt is ever batched into it while it is queued.
fn queued_prompt_kept_standalone(record: &SessionRecord, queued: &QueuedPromptRecord) -> bool {
    record
        .queued_prompt_durability_holds
        .contains_key(&queued.pending_prompt.id)
}

/// Drops the holds whose entries left the queue (promoted, Stop, Cancel,
/// removal), whatever state their fences are in; a late outcome then finds no
/// hold and settles nothing. Called wherever the queue is synchronized.
fn prune_queued_prompt_durability_holds(record: &mut SessionRecord) {
    if record.queued_prompt_durability_holds.is_empty() {
        return;
    }
    let queued = &record.queued_prompts;
    record.queued_prompt_durability_holds.retain(|prompt_id, _| {
        queued
            .iter()
            .any(|entry| entry.pending_prompt.id == *prompt_id)
    });
}

/// What a durability fence of `prompt_id` compares, read from a persisted
/// session row: the row's hold nonce and its queued entry. None when the row
/// has no such hold or no such entry, which no acknowledgement can match.
fn queued_prompt_durability_persisted_content(
    record: &PersistedSessionRecord,
    prompt_id: &str,
) -> Option<Value> {
    let hold = record.queued_prompt_durability_holds.get(prompt_id)?;
    let queued = record
        .queued_prompts
        .iter()
        .find(|queued| queued.pending_prompt.id == prompt_id)?;
    Some(queued_prompt_durability_fence_content(
        &hold.nonce,
        &queued_prompt_durability_content(queued),
    ))
}

/// The persisted content a durability fence compares: the hold's nonce and
/// the exact entry, as one value.
fn queued_prompt_durability_fence_content(nonce: &str, content: &Value) -> Value {
    json!({ "nonce": nonce, "entry": content })
}

/// Whether the session's queue head waits for its durability
/// acknowledgement. A direct send behind such a head stays queued.
fn queued_head_held_for_durability(record: &SessionRecord) -> bool {
    record
        .queued_prompts
        .front()
        .is_some_and(|head| queued_prompt_held_for_durability(record, head))
}

/// Why a Resume of the paused queue cannot start its head now: the head still
/// waits for its durability acknowledgement. The Resume is refused with this
/// reason and the pause stays; nothing keeps the Resume, and an
/// acknowledgement never dispatches past a pause, so the user resumes again
/// once the head is saved. None when the head is not held.
fn queued_head_durability_resume_refusal(record: &SessionRecord) -> Option<String> {
    let head = record.queued_prompts.front()?;
    let hold = record
        .queued_prompt_durability_holds
        .get(&head.pending_prompt.id)
        .filter(|hold| !hold.acknowledged)?;
    Some(match &hold.failure {
        None => "the next queued prompt is still being saved; Resume again in a moment".to_owned(),
        Some(reason) => format!(
            "the next queued prompt could not be confirmed saved (the save check reported: \
             {reason}); it stays queued and the queue stays paused: cancel that prompt, then \
             Resume to continue the rest of the queue"
        ),
    })
}

/// The refusal of a Resume whose retried drain also started nothing, the
/// head being no longer held: the queue changed while it was resuming.
const RESUME_QUEUE_CHANGED_REFUSAL: &str =
    "nothing was started because the queue changed during Resume; Resume again";

/// What a Resume of a paused queue does next when its drain started nothing.
enum UnstartedResumeStep {
    /// The head is held now: refuse with its durability reason.
    Refuse(String),
    /// The head changed during the Resume to an entry already saved (its
    /// acknowledgement landed meanwhile): drain once more.
    RetryOnce,
    /// Nothing durability-related stopped it: the drain's outcome stands.
    Keep,
}

/// What one durability fence's outcome does to its hold.
#[cfg_attr(not(test), allow(dead_code))]
enum QueuedPromptDurabilitySettlement {
    /// The outcome is for no current hold: nothing changes.
    Stale,
    /// The entry is gone (Stop, Cancel, removal): its hold guards nothing.
    Orphaned,
    /// The entry is still queued but no longer holds the content the fence
    /// proved: it is held again for its current content under a fresh nonce.
    Changed,
    /// The exact held entry was acknowledged.
    Acknowledged,
    /// The acknowledgement failed; the entry stays held with this reason.
    Failed(String),
}

#[cfg_attr(not(test), allow(dead_code))]
impl AppState {
    /// Asks the persist writer to prove the held entry durable. Its exact
    /// acknowledgement releases the entry and dispatches the session's queue;
    /// a failed one leaves it held with the reason. Returns false when there
    /// is no such hold or the writer cannot be reached (the hold then stays,
    /// fail-closed). Never waits; call with the state lock released.
    fn request_queued_prompt_durability(&self, session_id: &str, prompt_id: &str) -> bool {
        let (target, nonce) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return false;
            };
            let Some(hold) = inner.sessions[index]
                .queued_prompt_durability_holds
                .get(prompt_id)
            else {
                return false;
            };
            (
                PersistFenceTarget::QueuedPromptDurability {
                    session_id: session_id.to_owned(),
                    prompt_id: prompt_id.to_owned(),
                    content: queued_prompt_durability_fence_content(&hold.nonce, &hold.content),
                },
                hold.nonce.clone(),
            )
        };
        let (fence, _unwaited) = PersistFence::new(
            target,
            std::time::Instant::now() + QUEUED_PROMPT_DURABILITY_BUDGET,
        );
        let state = self.clone();
        let session_id = session_id.to_owned();
        let prompt_id = prompt_id.to_owned();
        // The writer thread only hands the outcome over: settling takes the
        // state lock and may dispatch, which never happens on the writer.
        // A thread the OS refuses must not unwind the writer: the hold then
        // stays (fail-closed) and the refusal is logged.
        let fence = fence.on_resolved(move |result| {
            let spawned = std::thread::Builder::new()
                .name("queued-prompt-durability".to_owned())
                .spawn(move || {
                    state.settle_queued_prompt_durability(&session_id, &prompt_id, &nonce, result);
                });
            if let Err(error) = spawned {
                eprintln!(
                    "queued prompt durability> could not start the settlement thread; the prompt stays held: {error}"
                );
            }
        });
        self.persist_tx
            .send(PersistRequest::Fence(Box::new(fence)))
            .is_ok()
    }

    /// Applies a durability fence's outcome. Only the outcome for the current,
    /// unacknowledged hold (the same nonce) acts. An acknowledgement releases
    /// the entry only while it holds exactly the content the fence proved; the
    /// hold then stays as an acknowledged marker until the entry leaves the
    /// queue. An entry that was removed loses its hold and releases nothing.
    /// An entry whose content changed in place stays held: it is held again
    /// for its current content and a new acknowledgement is requested. A
    /// failure records its reason and leaves the entry held.
    fn settle_queued_prompt_durability(
        &self,
        session_id: &str,
        prompt_id: &str,
        nonce: &str,
        result: PersistFenceResult,
    ) {
        let settlement = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            let current = record
                .queued_prompts
                .iter()
                .find(|queued| queued.pending_prompt.id == prompt_id)
                .map(queued_prompt_durability_content);
            let settlement = match record.queued_prompt_durability_holds.get_mut(prompt_id) {
                Some(hold) if hold.nonce == nonce && !hold.acknowledged => match (current, result) {
                    (None, _) => QueuedPromptDurabilitySettlement::Orphaned,
                    (Some(current), _) if current != hold.content => {
                        hold.nonce = Uuid::new_v4().to_string();
                        hold.content = current;
                        hold.failure = None;
                        QueuedPromptDurabilitySettlement::Changed
                    }
                    (Some(_), Ok(())) => {
                        hold.acknowledged = true;
                        hold.failure = None;
                        QueuedPromptDurabilitySettlement::Acknowledged
                    }
                    (Some(_), Err(error)) => {
                        let reason = format!("{error:?}");
                        hold.failure = Some(reason.clone());
                        QueuedPromptDurabilitySettlement::Failed(reason)
                    }
                },
                // No hold, or a later one: this outcome settles nothing.
                _ => QueuedPromptDurabilitySettlement::Stale,
            };
            match settlement {
                QueuedPromptDurabilitySettlement::Stale => return,
                QueuedPromptDurabilitySettlement::Orphaned => {
                    record.queued_prompt_durability_holds.remove(prompt_id);
                }
                QueuedPromptDurabilitySettlement::Acknowledged
                | QueuedPromptDurabilitySettlement::Changed
                | QueuedPromptDurabilitySettlement::Failed(_) => {}
            }
            if let Err(error) = self.commit_locked(&mut inner) {
                eprintln!(
                    "queued prompt durability> failed to persist the settlement of `{prompt_id}` in session `{session_id}`: {error:#}"
                );
            }
            settlement
        };
        match settlement {
            QueuedPromptDurabilitySettlement::Acknowledged => {
                self.dispatch_released_queued_prompt(session_id, prompt_id)
            }
            QueuedPromptDurabilitySettlement::Changed => {
                if !self.request_queued_prompt_durability(session_id, prompt_id) {
                    eprintln!(
                        "queued prompt durability> could not request the acknowledgement of changed `{prompt_id}` in session `{session_id}`; it stays held"
                    );
                }
            }
            QueuedPromptDurabilitySettlement::Failed(reason) => eprintln!(
                "queued prompt durability> `{prompt_id}` in session `{session_id}` stays held: {reason}"
            ),
            QueuedPromptDurabilitySettlement::Stale
            | QueuedPromptDurabilitySettlement::Orphaned => {}
        }
    }

    /// The ordinary drain a released hold dispatches. It never passes a
    /// paused queue: a Resume that met the head held was refused, so the
    /// paused queue waits for the next explicit Resume.
    fn dispatch_released_queued_prompt(&self, session_id: &str, prompt_id: &str) {
        match self.dispatch_next_queued_turn(session_id, false) {
            Ok(Some(dispatch)) => {
                if let TurnDispatchDeliveryOutcome::Rejected(error) =
                    deliver_turn_dispatch(self, dispatch)
                {
                    eprintln!(
                        "queued prompt durability> the runtime rejected the dispatch after releasing `{prompt_id}` in session `{session_id}`: {}",
                        error.message
                    );
                }
            }
            Ok(None) => {}
            Err(error) => eprintln!(
                "queued prompt durability> failed to dispatch after releasing `{prompt_id}` in session `{session_id}`: {error:#}"
            ),
        }
    }

    /// Completes a Resume of a paused queue (`resume_local_session_queue`)
    /// whose first drain left `drained`, for the head `checked_head` its
    /// first check saw. A Resume that a durability hold stopped never ends
    /// in a success that started nothing. Re-checked under the lock, for the
    /// head this Resume may start (no owner, or the owner still matches):
    /// - a head held now refuses with its durability reason;
    /// - a head that changed during the Resume to an entry already saved
    ///   (its acknowledgement landed meanwhile) is drained exactly once
    ///   more, and if that also starts nothing the Resume is refused because
    ///   the queue changed.
    ///
    /// Any other drain that started nothing keeps its outcome. Nothing is
    /// kept for later, and the pause is never passed without this Resume.
    fn finish_resume_drain(
        &self,
        session_id: &str,
        owner: Option<EngramQueuedAdmissionOwner>,
        checked_head: Option<&str>,
        drained: Option<TurnDispatch>,
    ) -> std::result::Result<Option<TurnDispatch>, ApiError> {
        if drained.is_some() {
            return Ok(drained);
        }
        match self.unstarted_resume_step(session_id, owner.as_ref(), checked_head) {
            UnstartedResumeStep::Refuse(refusal) => return Err(ApiError::conflict(refusal)),
            UnstartedResumeStep::Keep => return Ok(None),
            UnstartedResumeStep::RetryOnce => {}
        }
        let retried = self.drain_resumed_queue(session_id, owner)?;
        if retried.is_some() {
            return Ok(retried);
        }
        let inner = self.inner.lock().expect("state mutex poisoned");
        let refusal = inner
            .find_visible_session_index(session_id)
            .and_then(|index| queued_head_durability_resume_refusal(&inner.sessions[index]))
            .unwrap_or_else(|| RESUME_QUEUE_CHANGED_REFUSAL.to_owned());
        Err(ApiError::conflict(refusal))
    }

    fn unstarted_resume_step(
        &self,
        session_id: &str,
        owner: Option<&EngramQueuedAdmissionOwner>,
        checked_head: Option<&str>,
    ) -> UnstartedResumeStep {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let Some(record) = inner
            .find_visible_session_index(session_id)
            .map(|index| &inner.sessions[index])
        else {
            return UnstartedResumeStep::Keep;
        };
        if !record.orchestrator_auto_dispatch_blocked
            || owner.is_some_and(|owner| !owner.matches(record))
        {
            return UnstartedResumeStep::Keep;
        }
        if let Some(refusal) = queued_head_durability_resume_refusal(record) {
            return UnstartedResumeStep::Refuse(refusal);
        }
        // Cold recovery can discover a previously begun turn after Resume's
        // initial check. Report the retained hold instead of a silent success.
        if checked_head.is_some_and(|id| {
            record
                .queued_prompts
                .front()
                .is_some_and(|head| head.pending_prompt.id == id)
        }) && engram_queue_head_refused_as_interrupted(record)
            && !engram_retry_attempt_in_flight(record)
        {
            return UnstartedResumeStep::Refuse(format!(
                "Engram recovery retained this prompt: {} Reconcile its prior turn or cancel the retained prompt before continuing.",
                record.session.preview,
            ));
        }
        let changed_to_saved_entry = record.queued_prompts.front().is_some_and(|head| {
            checked_head != Some(head.pending_prompt.id.as_str())
                && queued_prompt_kept_standalone(record, head)
        });
        if changed_to_saved_entry {
            UnstartedResumeStep::RetryOnce
        } else {
            UnstartedResumeStep::Keep
        }
    }
}
