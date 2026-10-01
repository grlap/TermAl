// Automatic recovery of a retained Engram prompt whose delivery this host
// aborted before provider handoff. Owns the durable abort record
// (`EngramAbortRetry`), its settlement on the dispatch path that withheld the
// delivery (`settle_engram_abort_before_handoff_locked`), the backoff of the
// fresh admission that follows (`engram_abort_retry_delay`), and the tick that
// acknowledges the settlement durably and starts that admission when it is
// due (`engram_abort_retry_tick`). Does not own the dispatch itself
// (`prepare_engram_turn_delivery_off_lock` in `engram_host_adapter.rs`, which
// decides that a delivery was withheld and closes its grant), the retained
// prompt states it sits beside (`engram_queued_admission.rs`), or the
// conservative hold every other uncertain outcome keeps. New module: before
// it, a delivery withheld for a slow local save left its prompt interrupted
// until someone cancelled it, even after its grant was closed.

/// The delay before the fresh admission of a proven-unsent prompt, by the
/// number of aborted attempts so far; the last value repeats.
const ENGRAM_ABORT_RETRY_DELAYS_SECONDS: [u64; 6] = [2, 5, 10, 20, 30, 60];

/// The largest positive jitter added to a retry delay, in percent.
const ENGRAM_ABORT_RETRY_MAX_JITTER_PERCENT: u64 = 20;

/// How long one acknowledgement of the local settlement may take before the
/// tick asks again. A later request has its own deadline; the original
/// admission's dispatch budget is never stretched.
const ENGRAM_ABORT_SETTLEMENT_FENCE: Duration = Duration::from_secs(20);

/// Why a retained prompt's delivery was aborted before provider handoff.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum EngramAbortReason {
    /// The admission durability fence missed its deadline after the begin.
    AdmissionFence,
    /// The dispatch card of a granted admission could not be saved.
    DispatchCard,
    /// The dispatch card of a known Defer could not be saved.
    DeferCard,
}

/// A retained prompt whose last delivery this host withheld before provider
/// handoff, and whose authority is settled: its begun grant closed by a
/// matching checkpoint receipt, or no grant at all for a known Defer. Saved
/// with the session, so a restart after it is durable rebuilds the retry.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramAbortRetry {
    /// The queue head this record is for; any other head drops it.
    prompt_id: String,
    reason: EngramAbortReason,
    /// The grant the matching checkpoint closed; none for a Defer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    settled_grant_id: Option<String>,
    /// Aborted attempts so far, the first included.
    attempts: u32,
    /// When the prompt was first held (RFC 3339); later attempts keep it.
    held_since: String,
    /// When the next fresh admission is due (RFC 3339).
    due_at: String,
    /// The project, connection and admission settings the aborted admission
    /// ran under (`engram_abort_authority`). Any other authority drops the
    /// record and keeps the hold.
    authority: String,
    /// The settlement was durably acknowledged. Saved once the
    /// acknowledgement arrived, so a record read back without it (a crash
    /// between the settlement write and its acknowledgement) loads as the
    /// conservative hold, never as a runnable retry.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    acknowledged: bool,
    /// The session's dispatch generation the settlement moved to. Any path
    /// that holds the head again (a Stop of the retried admission, a
    /// cancellation, a project reset) moves the generation on, so a record
    /// whose generation no longer matches holds nothing any more.
    #[serde(default)]
    generation: u64,
}

/// Whether `record`'s acknowledged abort record holds its queue head, waiting
/// for the retry: the record names the head, the head carries no wire intent
/// and is not parked, and nothing moved the dispatch generation since the
/// settlement. Such a head stays interrupted, so every protection a retained
/// head has (delegation polling, mailbox coalescing, cancellation) keeps
/// applying; only this predicate lets the retried admission, or an explicit
/// Resume, past that interruption.
fn engram_abort_retry_holds_head(record: &SessionRecord) -> bool {
    record.engram.abort_retry.as_ref().is_some_and(|retry| {
        record.engram.abort_retry_acknowledged
            && retry.acknowledged
            && record.engram.dispatch_generation == retry.generation
            && record.queued_prompts.front().is_some_and(|queued| {
                queued.pending_prompt.id == retry.prompt_id
                    && !queued.engram_waiting
                    && !queued.has_engram_intent()
            })
    })
}

/// Whether the drain must refuse `record`'s queue head as interrupted: it is,
/// and no acknowledged abort record holds it for its retry.
fn engram_queue_head_refused_as_interrupted(record: &SessionRecord) -> bool {
    record
        .queued_prompts
        .front()
        .is_some_and(|queued| queued.engram_interrupted)
        && !engram_abort_retry_holds_head(record)
}

/// The pending durable acknowledgement of a settlement, kept on the session
/// for the tick to look at without blocking.
#[derive(Clone)]
struct EngramAbortAckWaiter(Arc<PersistFenceWaiter>);

impl std::fmt::Debug for EngramAbortAckWaiter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("EngramAbortAckWaiter")
    }
}

/// The delay before attempt `attempts + 1`: the schedule's value plus up to
/// `ENGRAM_ABORT_RETRY_MAX_JITTER_PERCENT`, spread by session and attempt so
/// sessions held together do not retry together.
fn engram_abort_retry_delay(session_id: &str, attempts: u32) -> chrono::Duration {
    let index = usize::try_from(attempts.saturating_sub(1))
        .unwrap_or(usize::MAX)
        .min(ENGRAM_ABORT_RETRY_DELAYS_SECONDS.len() - 1);
    let base_ms = ENGRAM_ABORT_RETRY_DELAYS_SECONDS[index] * 1000;
    let digest = sha256_hex(format!("{session_id}:{attempts}").as_bytes());
    let spread = u64::from_str_radix(&digest[..8], 16).unwrap_or(0)
        % (ENGRAM_ABORT_RETRY_MAX_JITTER_PERCENT + 1);
    let jittered = base_ms * (100 + spread) / 100;
    chrono::Duration::milliseconds(i64::try_from(jittered).unwrap_or(i64::MAX))
}

/// The authority an admission ran under: its project, connection and the
/// settings that decide admission (`same_admission_settings`).
fn engram_abort_authority(target: &EngramBindingTarget) -> String {
    let mut settings = target.settings.clone();
    settings.acceptance_evaluation = None;
    let identity = json!({
        "project": target.project_id,
        "connection": target.connection,
        "settings": settings,
    });
    sha256_hex(identity.to_string().as_bytes())
}

/// The admission content the durability fence compares for `record`, in
/// memory; `engram_admission_persisted_content` is the same for a saved one.
fn engram_admission_live_content(record: &SessionRecord) -> Value {
    json!({ "generation": record.engram.dispatch_generation,
        "routing": record.engram.routing_token, "grant": record.engram.active_grant_id,
        "queue": record.queued_prompts.front(),
        "abortRetry": record.engram.abort_retry })
}

/// Settles, on `record`, the delivery of its queue head that this host
/// withheld before provider handoff, once its authority is settled. The head
/// loses the evaluation and bind it was admitted with, the session forgets
/// the closed grant and moves to a new dispatch generation (so the next
/// admission is a fresh operation for the same prompt), and an abort record
/// holds the prompt until the settlement is durably acknowledged and its
/// retry is due. The head stays interrupted throughout: before the
/// acknowledgement nothing can admit it, and after it only the retried
/// admission or an explicit Resume can (`engram_abort_retry_holds_head`).
/// `grant_id` must be the grant the head's evaluation began (none for a
/// Defer, whose evaluation was already retired). Returns whether it settled.
fn settle_engram_abort_before_handoff_locked(
    record: &mut SessionRecord,
    reason: EngramAbortReason,
    grant_id: Option<&str>,
    authority: &str,
    not_before: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    if record.runtime_stop_in_progress {
        return false;
    }
    let session_id = record.session.id.clone();
    let Some(head) = record.queued_prompts.front_mut() else {
        return false;
    };
    let begun = head
        .engram_evaluate
        .as_ref()
        .and_then(|prepared| prepared.begun_grant_id.as_deref());
    if begun != grant_id {
        return false;
    }
    let prompt_id = head.pending_prompt.id.clone();
    let prior = record
        .engram
        .abort_retry
        .take()
        .filter(|retry| retry.prompt_id == prompt_id);
    let attempts = prior
        .as_ref()
        .map_or(1, |retry| retry.attempts.saturating_add(1));
    let held_since = prior
        .map(|retry| retry.held_since)
        .unwrap_or_else(|| now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    let mut due = now + engram_abort_retry_delay(&session_id, attempts);
    if let Some(not_before) = not_before {
        due = due.max(not_before);
    }
    head.engram_evaluate = None;
    head.engram_bind = None;
    head.engram_waiting = false;
    head.engram_interrupted = true;
    if let Some(grant_id) = grant_id {
        if record.engram.active_grant_id.as_deref() == Some(grant_id) {
            record.engram.active_grant_id = None;
        }
        if record.engram.uncertain_grant_id.as_deref() == Some(grant_id) {
            record.engram.uncertain_grant_id = None;
        }
    }
    record.engram.dispatch_generation = record.engram.dispatch_generation.saturating_add(1);
    record.engram.rebind_required = true;
    record.engram.recovered_admission = false;
    record.engram.abort_retry = Some(EngramAbortRetry {
        prompt_id,
        reason,
        settled_grant_id: grant_id.map(str::to_owned),
        attempts,
        held_since,
        due_at: due.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        authority: authority.to_owned(),
        acknowledged: false,
        generation: record.engram.dispatch_generation,
    });
    record.engram.abort_retry_acknowledged = false;
    record.engram.abort_retry_saved = false;
    record.engram.abort_retry_fence = None;
    record.set_auto_dispatch_blocked(true);
    record.session.status = SessionStatus::Idle;
    record.session.live_activity = None;
    clear_active_turn_file_change_tracking(record);
    record.session.preview = "Engram: delivery not attempted; waiting for local durability \
        before retrying automatically."
        .to_owned();
    sync_pending_prompts(record);
    true
}

/// What one tick does with a session's abort record.
#[derive(Debug, PartialEq, Eq)]
enum EngramAbortRetryStep {
    /// Nothing yet: the acknowledgement is pending or the retry not due.
    Wait,
    /// The record no longer applies and was dropped; the hold stays.
    Dropped,
    /// The settlement needs a (new) durable acknowledgement.
    Acknowledge,
    /// The settlement was acknowledged just now; the prompt waits for its
    /// retry.
    Acknowledged,
    /// The fresh admission is due. The hold stays: the admission bypasses it
    /// only for this exact head (`dispatch_next_queued_turn_for_abort_retry`).
    Due,
}

/// One tick's step for `record`'s abort record at `now`, given the authority
/// the session's admission would run under now (`None` when Engram no
/// longer admits it). Mutates the record; the caller persists.
fn engram_abort_retry_step(
    record: &mut SessionRecord,
    authority: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> EngramAbortRetryStep {
    let Some(retry) = record.engram.abort_retry.clone() else {
        return EngramAbortRetryStep::Wait;
    };
    let head = record.queued_prompts.front();
    let head_matches = head.is_some_and(|queued| queued.pending_prompt.id == retry.prompt_id);
    if !head_matches {
        record.engram.abort_retry = None;
        record.engram.abort_retry_fence = None;
        return EngramAbortRetryStep::Dropped;
    }
    // A head carrying the wire intent of its retried admission while that
    // admission is still running is in flight: whatever it ends in decides,
    // and the record keeps the attempt count should it abort too (the
    // settlement reads it). Intent left with no admission running is an
    // outcome that did not settle, which another path now holds.
    let has_intent = head.is_some_and(QueuedPromptRecord::has_engram_intent);
    let admission_running = record.engram.admission_in_progress.is_some()
        || record.engram.pending_dispatch.is_some()
        || matches!(
            record.session.status,
            SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
        );
    if has_intent && admission_running {
        return EngramAbortRetryStep::Wait;
    }
    // Another path holds the head: an unsettled outcome, a park, or anything
    // that moved the dispatch generation since the settlement (a Stop, a
    // cancellation, a project reset).
    let taken_over = has_intent
        || head.is_some_and(|queued| queued.engram_waiting)
        || record.engram.dispatch_generation != retry.generation;
    if taken_over {
        record.engram.abort_retry = None;
        record.engram.abort_retry_fence = None;
        return EngramAbortRetryStep::Dropped;
    }
    if authority != Some(retry.authority.as_str()) {
        // The project, connection or settings changed (a committed change:
        // the tick waits while the project's settings fence is raised): no
        // automatic retry. The head stays interrupted, and without the
        // record nothing passes that hold, so it is retained for
        // cancellation, as any interrupted head is.
        record.engram.abort_retry = None;
        record.engram.abort_retry_fence = None;
        record.session.preview = "Engram: settings changed after a delivery was withheld; \
            prompt retained. Cancel it before continuing."
            .to_owned();
        sync_pending_prompts(record);
        return EngramAbortRetryStep::Dropped;
    }
    if !record.engram.abort_retry_acknowledged {
        let saved = if record.engram.abort_retry_saved {
            Some(Ok(()))
        } else {
            record
                .engram
                .abort_retry_fence
                .as_ref()
                .map(|waiter| waiter.0.wait_until(std::time::Instant::now()))
                .unwrap_or(Some(Err(PersistFenceError::Deadline)))
        };
        return match saved {
            // Still being written: look again next tick.
            None => EngramAbortRetryStep::Wait,
            // Never asked, or the worker gave up: ask again.
            Some(Err(_)) => EngramAbortRetryStep::Acknowledge,
            Some(Ok(())) => {
                record.engram.abort_retry_acknowledged = true;
                record.engram.abort_retry_saved = false;
                record.engram.abort_retry_fence = None;
                if let Some(saved) = record.engram.abort_retry.as_mut() {
                    saved.acknowledged = true;
                }
                // The head stays interrupted, and so retained: only the
                // retried admission or an explicit Resume passes it now.
                record.session.preview = format!(
                    "Engram: delivery not attempted; retrying automatically at {}.",
                    retry.due_at
                );
                sync_pending_prompts(record);
                EngramAbortRetryStep::Acknowledged
            }
        };
    }
    let due = chrono::DateTime::parse_from_rfc3339(&retry.due_at)
        .map(|due| due.with_timezone(&chrono::Utc))
        .unwrap_or(now);
    let idle = matches!(
        record.session.status,
        SessionStatus::Idle | SessionStatus::Error
    );
    if due > now
        || !idle
        || record.runtime_stop_in_progress
        || record.engram.admission_in_progress.is_some()
        || record.engram.project_reset_in_progress
    {
        return EngramAbortRetryStep::Wait;
    }
    EngramAbortRetryStep::Due
}

/// Whether `record`'s abort record still releases its queue head for the
/// retried admission now, under the lock where that admission is promoted:
/// the record, acknowledged, names the head at the settled generation, the
/// head is not parked, and the admission would run under the record's
/// authority (`authority`, `None` when Engram no longer admits the session).
/// The head is either still in the record's own interrupted hold (no intent
/// yet) or carries the intent this admission prepared (no longer
/// interrupted); an interrupted head with intent is another path's.
fn engram_abort_retry_releases(record: &SessionRecord, authority: Option<&str>) -> bool {
    record.engram.abort_retry.as_ref().is_some_and(|retry| {
        record.engram.abort_retry_acknowledged
            && retry.acknowledged
            && record.engram.dispatch_generation == retry.generation
            && authority == Some(retry.authority.as_str())
            && record.queued_prompts.front().is_some_and(|queued| {
                queued.pending_prompt.id == retry.prompt_id
                    && !queued.engram_waiting
                    && !(queued.engram_interrupted && queued.has_engram_intent())
            })
    })
}

impl AppState {
    /// Settles the withheld delivery of `session_id`'s queue head under the
    /// lock (`settle_engram_abort_before_handoff_locked`), persists it and
    /// asks for its durable acknowledgement. Returns whether it settled.
    fn settle_engram_abort_before_handoff(
        &self,
        inner: &mut StateInner,
        index: usize,
        reason: EngramAbortReason,
        grant_id: Option<&str>,
        authority: &str,
        not_before: Option<chrono::DateTime<chrono::Utc>>,
    ) -> bool {
        let settled = settle_engram_abort_before_handoff_locked(
            inner
                .session_mut_by_index(index)
                .expect("session index should be valid"),
            reason,
            grant_id,
            authority,
            not_before,
            chrono::Utc::now(),
        );
        if settled {
            if let Err(error) = self.commit_locked(inner) {
                // The settlement stays in memory behind the interrupted head;
                // the tick keeps asking for its acknowledgement.
                eprintln!(
                    "engram> session={} failed persisting the aborted delivery's settlement: \
                     {error:#}",
                    inner.sessions[index].session.id
                );
                self.publish_state_locked(inner);
            }
            self.request_engram_abort_acknowledgement_locked(inner, index);
        }
        settled
    }

    /// Asks the persistence worker to acknowledge, against the saved record,
    /// the admission content `index`'s session holds now, abort record
    /// included, and keeps the waiter for the tick.
    fn request_engram_abort_acknowledgement_locked(&self, inner: &mut StateInner, index: usize) {
        let record = &mut inner.sessions[index];
        let (fence, waiter) = PersistFence::new(
            PersistFenceTarget::EngramAdmission {
                session_id: record.session.id.clone(),
                content: engram_admission_live_content(record),
            },
            std::time::Instant::now() + ENGRAM_ABORT_SETTLEMENT_FENCE,
        );
        if self
            .persist_tx
            .send(PersistRequest::Fence(Box::new(fence)))
            .is_ok()
        {
            record.engram.abort_retry_fence = Some(EngramAbortAckWaiter(Arc::new(waiter)));
            return;
        }
        // Shutdown or a test without a worker: a synchronous save of the same
        // content is the acknowledgement, as the admission fence falls back
        // to it.
        record.engram.abort_retry_fence = None;
        let saved = self.persist_internal_locked(inner);
        let record = &mut inner.sessions[index];
        match saved {
            Ok(()) => record.engram.abort_retry_saved = true,
            Err(error) => eprintln!(
                "engram> session={} failed saving the aborted delivery's settlement: {error:#}",
                record.session.id
            ),
        }
    }

    /// One pass over every session with an abort record at `now`: drops a
    /// record that no longer applies, asks for a missing or failed durable
    /// acknowledgement, releases an acknowledged prompt for its retry, and
    /// starts the fresh admission of each prompt that is due. Driven by the
    /// test-run index thread's tick; tests call it with their own clock.
    fn engram_abort_retry_tick(&self, now: chrono::DateTime<chrono::Utc>) {
        let mut due = Vec::new();
        // Sessions whose hold changed: a held delegation child reports it.
        let mut held_changes = Vec::new();
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let session_ids = inner
                .sessions
                .iter()
                .filter(|record| record.engram.abort_retry.is_some())
                .map(|record| record.session.id.clone())
                .collect::<Vec<_>>();
            let mut changed = false;
            for session_id in session_ids {
                let Some(index) = inner.find_session_index(&session_id) else {
                    continue;
                };
                // A settings transaction holds the project's fence while it
                // validates off the lock, and may still roll back: the
                // binding target reads as unavailable meanwhile. Wait for
                // the committed authority rather than read that as a change.
                let fenced = inner.sessions[index].engram.project_reset_in_progress
                    || engram_project_for_session_locked(&inner, &session_id)
                        .is_some_and(|project| inner.engram_project_resets.contains(&project.id));
                if fenced {
                    continue;
                }
                let authority =
                    Self::engram_binding_target_for_session_shape_locked(&inner, &session_id, true)
                        .ok()
                        .flatten()
                        .map(|target| engram_abort_authority(&target));
                let step =
                    engram_abort_retry_step(&mut inner.sessions[index], authority.as_deref(), now);
                match step {
                    EngramAbortRetryStep::Wait => {}
                    EngramAbortRetryStep::Acknowledge => {
                        self.request_engram_abort_acknowledgement_locked(&mut inner, index);
                    }
                    EngramAbortRetryStep::Dropped | EngramAbortRetryStep::Acknowledged => {
                        inner.stamp_session_at_index(index);
                        changed = true;
                        held_changes.push(session_id);
                    }
                    EngramAbortRetryStep::Due => {
                        // The admission bypasses the paused queue only for
                        // the head it was due for, revalidated under the
                        // promotion lock; a Cancel, takeover or changed
                        // authority in between starts nothing.
                        if let Some(owner) =
                            EngramQueuedAdmissionOwner::capture(&inner.sessions[index])
                        {
                            due.push((session_id, owner));
                        }
                    }
                }
            }
            if changed && let Err(error) = self.commit_locked(&mut inner) {
                eprintln!("engram> failed persisting the abort retry tick: {error:#}");
            }
        }
        for session_id in held_changes {
            self.sync_delegation_attempt_for_child_session(&session_id);
        }
        for (session_id, owner) in due {
            let prompt_id = owner.prompt_id.clone();
            let started = match self.dispatch_next_queued_turn_for_abort_retry(&session_id, owner) {
                Ok(Some(dispatch)) => {
                    if let Err(error) = deliver_turn_dispatch(self, dispatch)
                        .into_background_result("engram abort retry")
                    {
                        eprintln!(
                            "engram> session={session_id} failed delivering the retried prompt: {}",
                            error.message
                        );
                    }
                    true
                }
                Ok(None) => false,
                Err(error) => {
                    eprintln!(
                        "engram> session={session_id} failed preparing the retried prompt: \
                         {error:#}"
                    );
                    false
                }
            };
            if !started {
                self.postpone_engram_abort_retry(&session_id, &prompt_id, now);
            }
        }
    }

    /// The retried admission of `prompt_id` did not start (the drain found
    /// nothing it may promote, or failed before admission): count it as an
    /// attempt and move its due time along the backoff, so the tick does not
    /// try it again every pass.
    fn postpone_engram_abort_retry(
        &self,
        session_id: &str,
        prompt_id: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let record = &mut inner.sessions[index];
        // Another admission of the head is running (an explicit Resume, or
        // one that already prepared intent): it decides, and editing the
        // record now would change the content its durability fence checks.
        let admission_running = record.engram.admission_in_progress.is_some()
            || record
                .queued_prompts
                .front()
                .is_some_and(QueuedPromptRecord::has_engram_intent);
        if admission_running {
            return;
        }
        let Some(retry) = record
            .engram
            .abort_retry
            .as_mut()
            .filter(|retry| retry.prompt_id == prompt_id)
        else {
            return;
        };
        retry.attempts = retry.attempts.saturating_add(1);
        retry.due_at = (now + engram_abort_retry_delay(session_id, retry.attempts))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        inner.stamp_session_at_index(index);
        if let Err(error) = self.commit_locked(&mut inner) {
            eprintln!(
                "engram> session={session_id} failed persisting the postponed retry: {error:#}"
            );
        }
        drop(inner);
        self.sync_delegation_attempt_for_child_session(session_id);
    }

    /// A public Stop of a session whose prompt waits for its automatic retry:
    /// the retry is cancelled and the prompt kept, held for explicit
    /// cancellation as a Stop of a waiting admission keeps it. Returns
    /// whether there was such a retry to stop.
    fn stop_engram_abort_retry(&self, session_id: &str) -> std::result::Result<bool, ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_visible_session_index(session_id) else {
            return Ok(false);
        };
        let record = &mut inner.sessions[index];
        // A record left behind by a head since cancelled holds nothing: drop
        // it and let the ordinary Stop handle the session.
        let names_head = record.engram.abort_retry.as_ref().is_some_and(|retry| {
            record
                .queued_prompts
                .front()
                .is_some_and(|queued| queued.pending_prompt.id == retry.prompt_id)
        });
        if record.engram.abort_retry.is_some() && !names_head {
            record.engram.abort_retry = None;
            record.engram.abort_retry_fence = None;
            record.engram.abort_retry_acknowledged = false;
            record.engram.abort_retry_saved = false;
            return Ok(false);
        }
        // Waiting for the retry, or in the retried admission's window before
        // it stores intent (its context and work-focus reads run with the
        // head still interrupted). The generation moved below fails that
        // admission's owner check, so it never reaches the provider. Once
        // intent is stored the head is no longer interrupted and the Stop of
        // a waiting admission handles it.
        let waiting = record.engram.abort_retry.is_some()
            && matches!(
                record.session.status,
                SessionStatus::Idle | SessionStatus::Error
            )
            && !record
                .queued_prompts
                .front()
                .is_some_and(QueuedPromptRecord::has_engram_intent);
        if !waiting {
            return Ok(false);
        }
        record.engram.abort_retry = None;
        record.engram.abort_retry_fence = None;
        record.engram.abort_retry_acknowledged = false;
        record.engram.abort_retry_saved = false;
        record.engram.dispatch_generation = record.engram.dispatch_generation.saturating_add(1);
        detach_engram_pending_dispatch_keeping_uncertain_begin(record);
        record.engram.rebind_required = true;
        if let Some(queued) = record.queued_prompts.front_mut() {
            queued.engram_interrupted = true;
        }
        record.engram.stopped_prompt_id = record
            .queued_prompts
            .front()
            .map(|queued| queued.pending_prompt.id.clone());
        record.set_auto_dispatch_blocked(true);
        record.session.preview = "Engram: automatic retry stopped. Prompt retained; remove it \
            before starting a new operation."
            .to_owned();
        record.session.live_activity = None;
        sync_pending_prompts(record);
        inner.stamp_session_at_index(index);
        self.commit_locked(&mut inner).map_err(|error| {
            ApiError::internal(format!("Failed to persist the stopped retry: {error:#}"))
        })?;
        Ok(true)
    }

    /// Forgets the abort record of `session_id` once its prompt was admitted
    /// and handed to the provider: nothing is held any more.
    fn clear_engram_abort_retry_for_delivered_head(&self, session_id: &str) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(index) = inner.find_session_index(session_id) {
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            let head = record
                .queued_prompts
                .front()
                .map(|queued| queued.pending_prompt.id.clone());
            if record
                .engram
                .abort_retry
                .as_ref()
                .is_some_and(|retry| Some(&retry.prompt_id) == head.as_ref())
            {
                record.engram.abort_retry = None;
                record.engram.abort_retry_fence = None;
                record.engram.abort_retry_acknowledged = false;
            }
        }
    }
}
