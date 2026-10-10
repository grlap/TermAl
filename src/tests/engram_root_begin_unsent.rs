// Owns the regression for a Begin that the named-root guard stopped before it
// was transmitted (remote application not_started), as when the guard's
// authority persistence fence misses its deadline: the head is not held for a
// Resume but settled by the exact begin replay, because the guard's code-less
// failure parks with a retry code and an ordinary retry on a head whose
// prepared begin names its uncertain grant is that replay
// (src/engram_admission_retry.rs). Drives the ClaimedRoot fixture on its
// scripted budget clock with a gated persist writer; no real load. Does not
// own the replay itself (src/engram_begin_replay.rs), the guard's share of the
// admission budget (src/tests/engram_root_begin_reserve.rs) or the hold of a
// begin whose own preparation was never acknowledged
// (src/tests/engram_begin_unknown_recovery.rs). New file.
use super::*;

/// The test's persist writer. Every request is written through, except a
/// fence the test stalls: the writer never reaches it before its deadline, as
/// on a saturated disk.
struct GatedWriter {
    rx: std::sync::mpsc::Receiver<PersistRequest>,
    batch: PersistFenceBatch,
    cache: SqlitePersistConnectionCache,
    /// Stalled fences, kept so that none is completed early by a drop.
    stalled: Vec<Box<PersistFence>>,
}

impl GatedWriter {
    fn install(claimed: &mut ClaimedRoot) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        claimed.state.persist_tx = tx;
        Self {
            rx,
            batch: PersistFenceBatch::default(),
            cache: SqlitePersistConnectionCache::new(),
            stalled: Vec::new(),
        }
    }

    /// Runs `work` on its own thread and serves the writer until it returns.
    /// The fence `stall` picks is stalled: once its waiter waits on the
    /// scripted clock, the clock is run out to the fence's deadline.
    fn serve<T: Send + 'static>(
        &mut self,
        claimed: &ClaimedRoot,
        mut stall: impl FnMut(&PersistFence) -> bool,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        let clock = claimed.state.engram_budget_clock();
        let task = std::thread::spawn(work);
        while !task.is_finished() {
            match self.rx.recv_timeout(Duration::from_millis(20)) {
                Ok(PersistRequest::Fence(fence)) if stall(fence.as_ref()) => {
                    let deadline = fence.completion.deadline;
                    clock.wait_for_scripted_waiter();
                    clock.advance(deadline.saturating_duration_since(clock.now()));
                    self.stalled.push(fence);
                }
                Ok(PersistRequest::Fence(fence)) => {
                    self.batch.accept(PersistRequest::Fence(fence));
                    self.write(claimed);
                }
                Ok(_) => self.write(claimed),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("writer fixture disconnected: {error}"),
            }
        }
        task.join().unwrap()
    }

    fn write(&mut self, claimed: &ClaimedRoot) {
        let delta = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
        persist_delta_with_fences(
            &mut self.cache,
            claimed.state.persistence_path.as_path(),
            &delta,
            &mut self.batch,
        )
        .unwrap();
    }
}

/// The guard before Begin sends its Prepared authority fence after the
/// grant: earlier Prepared fences belong to the guards before bind.
fn is_guard_before_begin_fence(claimed: &ClaimedRoot, fence: &PersistFence) -> bool {
    let PersistFenceTarget::EngramWorkAuthority(image) = &fence.target else {
        return false;
    };
    image
        .history
        .transition
        .as_ref()
        .is_some_and(|owner| owner.phase == EngramAuthorityPhase::Prepared)
        && claimed
            .transport
            .requests()
            .iter()
            .any(|request| request.request["operation"] == "turn_evaluate")
}

fn record_of<T>(claimed: &ClaimedRoot, read: impl FnOnce(&SessionRecord) -> T) -> T {
    let inner = claimed.state.inner.lock().unwrap();
    read(&inner.sessions[inner.find_session_index(&claimed.session_id).unwrap()])
}

fn sent(claimed: &ClaimedRoot, operation: &str) -> Vec<Value> {
    claimed
        .transport
        .requests()
        .iter()
        .filter(|request| request.request["operation"] == operation)
        .map(|request| request.request.clone())
        .collect()
}

fn prompts_delivered(claimed: &ClaimedRoot) -> usize {
    claimed
        .runtime_rx
        .try_iter()
        .filter(|command| matches!(command, CodexRuntimeCommand::Prompt { .. }))
        .count()
}

/// The newest control card's cause.
fn latest_cause(claimed: &ClaimedRoot) -> Option<EngramCausalFailure> {
    record_of(claimed, |record| {
        record.session.messages.iter().rev().find_map(|message| match message {
            Message::EngramControl { card, .. } => card.causal_failure.clone(),
            _ => None,
        })
    })
}

/// The two-second tick, run past the scheduled replay: the backoff the failed
/// guard left runs out, the retry record's durable acknowledgement is awaited,
/// one pass marks it acknowledged and the next starts the attempt.
fn replay_when_due(state: &AppState, session: &str) {
    let clock = state.engram_budget_clock();
    let (backoff, waiter, due) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(session).unwrap()];
        (
            record.engram.next_bind_retry_at,
            record.engram.abort_retry_fence.clone(),
            record
                .engram
                .admission_retry
                .as_ref()
                .map(|retry| retry.due_at.clone())
                .expect("the replay is scheduled"),
        )
    };
    if let Some(left) = backoff.and_then(|at| at.checked_duration_since(clock.now())) {
        clock.advance(left + Duration::from_millis(1));
    }
    if let Some(waiter) = waiter {
        waiter
            .0
            .wait_until(std::time::Instant::now() + crate::tests::phase_sync::DEADLOCK_GUARD);
    }
    let at = chrono::DateTime::parse_from_rfc3339(&due)
        .unwrap()
        .with_timezone(&chrono::Utc)
        + chrono::Duration::milliseconds(1);
    state.engram_abort_retry_tick(at);
    state.engram_abort_retry_tick(at);
}

/// The guard before Begin stalls past its authority fence's deadline, so it
/// fails with Begin unsent and the cause says so (not_started). The head is
/// not held for a Resume: it is not interrupted and the begin replay is
/// scheduled. When due, the same stored begin (key, grant, tokens) is sent
/// once, with no fresh evaluate, and the head is delivered exactly once.
#[test]
fn begin_stopped_before_transmission_is_replayed_exactly_and_delivered_once() {
    let label = "begin-unsent-replay";
    let grant = format!("{label}-grant");
    let mut claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply(&format!("{label}-token")),
            grant_reply(&grant),
            begin_reply(&grant),
            checkpoint_reply(&grant),
        ],
    );
    prepare_confirmed_claimed_opening(&claimed, label);
    let mut writer = GatedWriter::install(&mut claimed);
    let worker = claimed.state.clone();
    let session = claimed.session_id.clone();
    let mut stalled = false;
    let outcome = writer.serve(
        &claimed,
        |fence| {
            let stall = !stalled && is_guard_before_begin_fence(&claimed, fence);
            stalled |= stall;
            stall
        },
        move || {
            let dispatch = match worker
                .dispatch_turn(
                    &session,
                    SendMessageRequest {
                        text: "Continue after the guard before Begin failed.".to_owned(),
                        expanded_text: None,
                        attachments: Vec::new(),
                        source_session_id: None,
                        source_mailbox: None,
                    },
                )
                .unwrap()
            {
                DispatchTurnResult::Dispatched(dispatch)
                | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
                DispatchTurnResult::Queued => panic!("idle fixture must dispatch"),
            };
            deliver_turn_dispatch(&worker, dispatch)
        },
    );
    assert!(stalled, "the guard before Begin must send its authority fence");
    assert!(
        !matches!(outcome, TurnDispatchDeliveryOutcome::Delivered),
        "a failed guard holds the turn: {outcome:?}"
    );
    assert!(sent(&claimed, "turn_begin").is_empty(), "Begin stays unsent");
    let cause = latest_cause(&claimed).expect("the held turn names its cause");
    assert_eq!(cause.operation, "turn_begin");
    assert_eq!(
        cause.remote_application,
        EngramRemoteApplication::NotStarted,
        "the guard failed before transmission"
    );

    let (interrupted, retry_code, prepared) = record_of(&claimed, |record| {
        let head = record.queued_prompts.front().expect("the head is retained");
        (
            head.engram_interrupted,
            record
                .engram
                .admission_retry
                .as_ref()
                .map(|retry| retry.code.clone()),
            head.engram_evaluate
                .as_ref()
                .and_then(|evaluate| evaluate.prepared_begin.clone()),
        )
    });
    assert!(!interrupted, "an unsent begin is no interrupted authorization");
    assert_eq!(
        retry_code.as_deref(),
        Some(ENGRAM_BEGIN_REPLAY_CODE),
        "the exact begin replay is scheduled; no Resume is needed"
    );
    let prepared = prepared.expect("the acknowledged prepared begin stands");
    assert_eq!(prepared.grant_id, grant);

    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    writer.serve(&claimed, |_| false, move || replay_when_due(&state, &session));

    let begins = sent(&claimed, "turn_begin");
    assert_eq!(begins.len(), 1, "the begin is sent once: it was never sent before");
    assert_eq!(begins[0]["idempotency_key"], prepared.idempotency_key, "same key");
    assert_eq!(begins[0]["grant_id"], grant, "same grant");
    assert_eq!(begins[0]["delivery_tokens"], json!(prepared.delivery_tokens), "same tokens");
    assert_eq!(sent(&claimed, "turn_evaluate").len(), 1, "no fresh evaluate");
    assert_eq!(prompts_delivered(&claimed), 1, "delivered exactly once");
    assert!(
        record_of(&claimed, |record| record.queued_prompts.is_empty()),
        "the head was handed to the provider"
    );
}
