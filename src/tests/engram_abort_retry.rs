// Owns the tests of the automatic recovery of a retained Engram prompt whose
// delivery this host withheld before provider handoff
// (src/engram_abort_retry.rs): the settlement once a matching checkpoint
// closes the begun grant, its durable acknowledgement, the fresh admission on
// backoff without Resume, a message or a restart, and the cases that keep the
// conservative hold (a checkpoint that does not close the grant, a settlement
// not yet acknowledged, a Stop, changed authority, a record written before
// the settlement), the Defer whose card cannot be saved, and a restart
// either side of the acknowledgement. The mailbox disposition of a held
// session is tested with the mailbox fixture, in src/tests/mailboxes.rs
// (`a_mailbox_wake_to_a_held_engram_head_reports_a_held_disposition`).
// Does not own the conservative hold's own tests
// (src/tests/engram_post_receipt.rs) or the retained dispositions
// (src/tests/engram_retained_disposition.rs). New module, a child of the
// root dispatch tests whose fixtures it uses.
use super::*;

const PROMPT: &str = "a prompt whose first delivery was withheld";

/// A persistence worker for these tests: it fails the next `failures`
/// fences with a deadline and acknowledges every other one. It writes
/// nothing, so the store keeps what was saved before it took over.
pub(super) struct ScriptedPersister {
    stop: Arc<AtomicBool>,
    failures: Arc<std::sync::atomic::AtomicUsize>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ScriptedPersister {
    pub(super) fn install(state: &mut AppState) -> Self {
        state.shutdown_persist_blocking();
        let (persist_tx, persist_rx) = mpsc::channel();
        state.persist_tx = persist_tx;
        let stop = Arc::new(AtomicBool::new(false));
        let failures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (worker_stop, worker_failures) = (stop.clone(), failures.clone());
        let handle = std::thread::spawn(move || {
            while let Ok(request) = persist_rx.recv() {
                if worker_stop.load(Ordering::SeqCst) {
                    break;
                }
                if let PersistRequest::Fence(fence) = request {
                    let fail = worker_failures
                        .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                            left.checked_sub(1)
                        })
                        .is_ok();
                    fence.finish(if fail {
                        Err(PersistFenceError::Deadline)
                    } else {
                        Ok(())
                    });
                }
            }
        });
        Self {
            stop,
            failures,
            handle: Some(handle),
        }
    }

    pub(super) fn fail_next(&self, fences: usize) {
        self.failures.store(fences, Ordering::SeqCst);
    }

    pub(super) fn stop(mut self, state: &AppState) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = state.persist_tx.send(PersistRequest::Delta);
        if let Some(handle) = self.handle.take() {
            handle.join().unwrap();
        }
    }
}

/// A root session whose public send of `PROMPT` is granted and begun, whose
/// admission durability fence then misses its deadline, and whose
/// stale-begin checkpoint answers `checkpoint`. `retry` scripts what the
/// transport answers after that. Returns the state, session, the provider's
/// receiver, the persister and the transport.
fn withheld_root_delivery(
    checkpoint: ScriptedEngramControlResponse,
    retry: Vec<GatedEngramControlStep>,
) -> (
    AppState,
    String,
    mpsc::Receiver<CodexRuntimeCommand>,
    ScriptedPersister,
    Arc<GatedEngramControlTransport>,
) {
    withheld_root_delivery_failing(checkpoint, retry, 1)
}

/// `withheld_root_delivery`, with the next `failed_fences` fences after the
/// begin failing: one fails the admission fence, two the settlement's
/// acknowledgement too.
fn withheld_root_delivery_failing(
    checkpoint: ScriptedEngramControlResponse,
    retry: Vec<GatedEngramControlStep>,
    failed_fences: usize,
) -> (
    AppState,
    String,
    mpsc::Receiver<CodexRuntimeCommand>,
    ScriptedPersister,
    Arc<GatedEngramControlTransport>,
) {
    let (mut state, session, receiver, _) = root_fixture([]);
    let persister = ScriptedPersister::install(&mut state);
    let (begin, begin_gate) = gated_engram_step("turn_begin", begin_reply("abort-grant-1"));
    let mut steps = vec![
        immediate_engram_step("session_bind", bind_reply("abort-token-1")),
        immediate_engram_step("turn_evaluate", grant_reply("abort-grant-1")),
        begin,
        immediate_engram_step("turn_checkpoint", checkpoint),
    ];
    steps.extend(retry);
    let transport = GatedEngramControlTransport::new(steps);
    state.install_control_test_transport(transport.clone());

    let result = std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            dispatch_turn_and_snapshot(
                &state,
                &session,
                SendMessageRequest {
                    text: PROMPT.to_owned(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
        });
        begin_gate.wait();
        persister.fail_next(failed_fences);
        begin_gate.release();
        worker.join().unwrap()
    });
    match result {
        Ok(_) => panic!("the withheld delivery must surface its persistence uncertainty"),
        Err(error) => assert!(error.message.contains("persistence is unknown")),
    }
    assert!(receiver.try_recv().is_err(), "nothing reached the provider");
    (state, session, receiver, persister, transport)
}

/// The retry steps of a fresh admission: the status check and the bind the
/// required rebind makes, then an evaluate and a begin of a new grant.
fn fresh_admission_steps() -> Vec<GatedEngramControlStep> {
    vec![
        immediate_engram_step("session_status", status_reply("ready")),
        immediate_engram_step("session_bind", bind_reply("abort-token-2")),
        immediate_engram_step("turn_evaluate", grant_reply("abort-grant-2")),
        immediate_engram_step("turn_begin", begin_reply("abort-grant-2")),
    ]
}

/// Waits, off the lock, for the pending acknowledgement of the session's
/// settlement to resolve, so a following tick reads its outcome.
pub(super) fn await_settlement_acknowledgement(state: &AppState, session: &str) {
    let waiter = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(session).unwrap()]
            .engram
            .abort_retry_fence
            .clone()
    };
    if let Some(waiter) = waiter {
        waiter
            .0
            .wait_until(std::time::Instant::now() + phase_sync::DEADLOCK_GUARD);
    }
}

fn with_record<T>(state: &AppState, session: &str, read: impl FnOnce(&SessionRecord) -> T) -> T {
    let inner = state.inner.lock().unwrap();
    read(&inner.sessions[inner.find_session_index(session).unwrap()])
}

pub(super) fn prompts_received(receiver: &mpsc::Receiver<CodexRuntimeCommand>) -> usize {
    let mut prompts = 0;
    while let Ok(command) = receiver.recv_timeout(Duration::from_millis(200)) {
        if matches!(command, CodexRuntimeCommand::Prompt { .. }) {
            prompts += 1;
        }
    }
    prompts
}

fn evaluate_keys(transport: &GatedEngramControlTransport) -> Vec<Value> {
    transport
        .requests()
        .iter()
        .filter(|request| request.request["operation"] == "turn_evaluate")
        .map(|request| request.request["idempotency_key"].clone())
        .collect()
}

/// Criterion 1: a writer too slow for the admission fence, then recovered.
/// The closed grant settles the withheld delivery, its acknowledgement
/// releases the prompt, and the tick admits it afresh when due: the provider
/// receives it exactly once, with no Resume, message or restart.
#[test]
fn a_withheld_delivery_whose_grant_closed_is_admitted_afresh_on_backoff() {
    let (state, session, receiver, persister, transport) =
        withheld_root_delivery(checkpoint_reply("abort-grant-1"), fresh_admission_steps());
    let (prompt_id, generation) = with_record(&state, &session, |record| {
        let retry = record.engram.abort_retry.clone().expect("an abort record");
        assert_eq!(retry.reason, EngramAbortReason::AdmissionFence);
        assert_eq!(retry.settled_grant_id.as_deref(), Some("abort-grant-1"));
        assert_eq!(retry.attempts, 1);
        assert!(!record.engram.abort_retry_acknowledged);
        assert!(
            record.queued_prompts[0].engram_interrupted,
            "held until acknowledged"
        );
        assert!(record.queued_prompts[0].engram_evaluate.is_none());
        assert!(record.engram.active_grant_id.is_none());
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(
            record
                .session
                .preview
                .contains("waiting for local durability")
        );
        (retry.prompt_id, record.engram.dispatch_generation)
    });

    // Not yet acknowledged and not yet due: nothing happens.
    await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now());
    with_record(&state, &session, |record| {
        assert!(record.engram.abort_retry_acknowledged);
        assert!(record.orchestrator_auto_dispatch_blocked);
        // The head stays retained, so delegation polling and mailbox
        // recovery keep protecting it, and only the retry passes it; it is
        // shown as a retryable hold, not as an unknown delivery.
        assert!(record.queued_prompts[0].engram_interrupted);
        assert!(record.queued_prompts[0].is_engram_retained());
        assert!(engram_abort_retry_holds_head(record));
        assert!(!engram_queue_head_refused_as_interrupted(record));
        assert!(!record.session.pending_prompts[0].engram_interrupted);
        assert!(record.session.pending_prompts[0].is_engram_retained);
    });
    state.engram_abort_retry_tick(chrono::Utc::now());
    assert_eq!(prompts_received(&receiver), 0, "not due before the backoff");

    // Due: the fresh admission reaches the provider once.
    state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(10));
    assert_eq!(prompts_received(&receiver), 1);
    with_record(&state, &session, |record| {
        assert!(record.engram.abort_retry.is_none());
        assert_eq!(
            record.engram.active_grant_id.as_deref(),
            Some("abort-grant-2")
        );
        // The settlement moved to a new generation; the retry runs under it.
        assert_eq!(record.engram.dispatch_generation, generation);
        // The same prompt identity was admitted, never a replacement.
        assert!(
            record
                .queued_prompts
                .front()
                .is_none_or(|queued| queued.pending_prompt.id == prompt_id)
        );
    });
    let keys = evaluate_keys(&transport);
    assert_eq!(keys.len(), 2);
    assert_ne!(
        keys[0], keys[1],
        "the retry is a fresh evaluation operation"
    );

    state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(3600));
    assert_eq!(prompts_received(&receiver), 0, "exactly once");
    persister.stop(&state);
}

/// Criterion 2: a checkpoint that does not close the begun grant keeps the
/// conservative hold: no abort record, and no tick ever admits the prompt.
#[test]
fn a_checkpoint_that_does_not_close_the_grant_keeps_the_conservative_hold() {
    for (label, checkpoint) in [
        ("mismatched receipt", checkpoint_reply("another-grant")),
        (
            "grant_not_begun",
            ScriptedEngramControlResponse::Reply(Ok(
                json!({ "decision": "refuse", "code": "grant_not_begun" }),
            )),
        ),
        (
            "transport unknown",
            ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
                "checkpoint outcome unknown".to_owned(),
            ))),
        ),
    ] {
        let (state, session, receiver, persister, _) =
            withheld_root_delivery(checkpoint, Vec::new());
        with_record(&state, &session, |record| {
            assert!(record.engram.abort_retry.is_none(), "{label}");
            assert!(record.queued_prompts[0].engram_interrupted, "{label}");
            assert!(
                record.queued_prompts[0].engram_evaluate.is_some(),
                "{label}"
            );
            assert!(record.orchestrator_auto_dispatch_blocked, "{label}");
        });
        state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(3600));
        assert_eq!(prompts_received(&receiver), 0, "{label}");
        persister.stop(&state);
    }
}

/// Criterion 2: until the settlement is acknowledged the prompt stays held,
/// even long after it is due; a failed acknowledgement is asked for again,
/// and once it is given the prompt is admitted.
#[test]
fn an_unacknowledged_settlement_holds_the_prompt_and_is_acknowledged_again() {
    // The writer is still slow: the settlement's acknowledgement fails too.
    let (state, session, receiver, persister, _) = withheld_root_delivery_failing(
        checkpoint_reply("abort-grant-1"),
        fresh_admission_steps(),
        2,
    );
    await_settlement_acknowledgement(&state, &session);
    let far = chrono::Utc::now() + chrono::Duration::seconds(3600);
    // Long past due, but the failed acknowledgement is only asked for again.
    state.engram_abort_retry_tick(far);
    assert_eq!(
        prompts_received(&receiver),
        0,
        "never admitted before acknowledgement"
    );
    with_record(&state, &session, |record| {
        assert!(!record.engram.abort_retry_acknowledged);
        assert!(record.queued_prompts[0].engram_interrupted);
        assert!(
            record.engram.abort_retry_fence.is_some(),
            "a new request is pending"
        );
    });
    // The writer recovers: the new request is acknowledged, then admitted.
    await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(far);
    with_record(&state, &session, |record| {
        assert!(record.engram.abort_retry_acknowledged);
        assert!(engram_abort_retry_holds_head(record));
    });
    state.engram_abort_retry_tick(far);
    assert_eq!(prompts_received(&receiver), 1);
    persister.stop(&state);
}

/// Criterion 2: a Stop in progress when the settlement would be made, and a
/// changed authority afterwards, never lead to an automatic retry.
#[test]
fn a_stop_or_a_changed_authority_never_retries_automatically() {
    let (state, session, receiver, persister, _) =
        withheld_root_delivery(checkpoint_reply("abort-grant-1"), Vec::new());
    await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now());

    // Another authority (project, connection or admission settings) drops
    // the record; the prompt stays behind the paused queue.
    let step = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        engram_abort_retry_step(
            &mut inner.sessions[index],
            Some("another-authority"),
            chrono::Utc::now() + chrono::Duration::seconds(3600),
        )
    };
    assert_eq!(step, EngramAbortRetryStep::Dropped);
    state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(3600));
    assert_eq!(prompts_received(&receiver), 0);
    with_record(&state, &session, |record| {
        assert!(record.engram.abort_retry.is_none());
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(
            record
                .session
                .preview
                .contains("Cancel it before continuing")
        );
        // Without the record the head is an ordinary interrupted hold.
        assert!(record.queued_prompts[0].engram_interrupted);
        assert!(record.session.pending_prompts[0].engram_interrupted);
    });
    persister.stop(&state);

    // A Stop in progress refuses the settlement itself.
    let (state, session, _receiver, _) = root_fixture([]);
    queue_test_engram_prompt(&state, &session, PROMPT, QueuedPromptSource::User, None);
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&session).unwrap();
    let record = &mut inner.sessions[index];
    record.runtime_stop_in_progress = true;
    assert!(!settle_engram_abort_before_handoff_locked(
        record,
        EngramAbortReason::AdmissionFence,
        None,
        "authority",
        None,
        chrono::Utc::now(),
    ));
    assert!(record.engram.abort_retry.is_none());
}

/// Criterion 2: the retried admission bypasses the paused queue only for the
/// exact head it was due for, while its abort record and authority still
/// release it: a head cancelled since (exposing a successor) or a changed
/// authority starts nothing.
#[test]
fn a_stale_owner_or_a_changed_authority_starts_no_retried_admission() {
    // A cancelled head: its successor is not started by the bypass.
    let (state, session, receiver, persister, _) =
        withheld_root_delivery(checkpoint_reply("abort-grant-1"), Vec::new());
    await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now());
    queue_test_engram_prompt(
        &state,
        &session,
        "a successor behind the held prompt",
        QueuedPromptSource::User,
        None,
    );
    let (owner, prompt_id) = with_record(&state, &session, |record| {
        (
            EngramQueuedAdmissionOwner::capture(record).unwrap(),
            record.queued_prompts[0].pending_prompt.id.clone(),
        )
    });
    state.cancel_queued_prompt(&session, &prompt_id).unwrap();
    assert!(
        state
            .dispatch_next_queued_turn_for_abort_retry(&session, owner)
            .unwrap()
            .is_none()
    );
    assert_eq!(prompts_received(&receiver), 0);
    with_record(&state, &session, |record| {
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert_eq!(
            record.queued_prompts[0].pending_prompt.text,
            "a successor behind the held prompt"
        );
    });
    persister.stop(&state);

    // A changed authority under the promotion lock starts nothing either.
    let (state, session, receiver, persister, _) =
        withheld_root_delivery(checkpoint_reply("abort-grant-1"), Vec::new());
    await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now());
    let owner = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record.engram.abort_retry.as_mut().unwrap().authority = "another-authority".to_owned();
        EngramQueuedAdmissionOwner::capture(record).unwrap()
    };
    assert!(
        state
            .dispatch_next_queued_turn_for_abort_retry(&session, owner)
            .unwrap()
            .is_none()
    );
    assert_eq!(prompts_received(&receiver), 0);
    persister.stop(&state);
}

/// Criterion 2: a public Stop cancels a pending automatic retry, both before
/// its settlement is acknowledged and during its backoff, and keeps the
/// prompt held for explicit cancellation.
#[test]
fn a_public_stop_cancels_a_pending_retry() {
    for acknowledged in [false, true] {
        let (state, session, receiver, persister, _) =
            withheld_root_delivery(checkpoint_reply("abort-grant-1"), Vec::new());
        if acknowledged {
            await_settlement_acknowledgement(&state, &session);
            state.engram_abort_retry_tick(chrono::Utc::now());
            assert!(with_record(&state, &session, |record| {
                record.engram.abort_retry_acknowledged
            }));
        }
        state
            .request_stop_session(&session)
            .expect("Stop of a pending retry succeeds");
        with_record(&state, &session, |record| {
            assert!(record.engram.abort_retry.is_none(), "{acknowledged}");
            assert!(
                record.queued_prompts[0].engram_interrupted,
                "{acknowledged}"
            );
            assert!(record.orchestrator_auto_dispatch_blocked, "{acknowledged}");
            assert!(record.session.preview.contains("automatic retry stopped"));
        });
        state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(3600));
        assert_eq!(prompts_received(&receiver), 0, "{acknowledged}");
        persister.stop(&state);
    }
}

/// Criterion 2: a public Stop pressed while the retried admission is running
/// but before it stores intent (its first control call still in flight, the
/// head still interrupted) stops it: the prompt never reaches the provider.
#[test]
fn a_public_stop_during_the_retried_admission_before_intent_stops_it() {
    let (status, status_gate) = gated_engram_step("session_status", status_reply("ready"));
    let (state, session, receiver, persister, _) = withheld_root_delivery(
        checkpoint_reply("abort-grant-1"),
        vec![
            status,
            immediate_engram_step("session_bind", bind_reply("abort-token-2")),
            immediate_engram_step("turn_evaluate", grant_reply("abort-grant-2")),
            immediate_engram_step("turn_begin", begin_reply("abort-grant-2")),
        ],
    );
    await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now());
    let due = chrono::Utc::now() + chrono::Duration::seconds(10);
    std::thread::scope(|scope| {
        let ticker = scope.spawn(|| state.engram_abort_retry_tick(due));
        status_gate.wait();
        with_record(&state, &session, |record| {
            assert!(record.engram.admission_in_progress.is_some());
            assert!(record.queued_prompts[0].engram_interrupted);
            assert!(!record.queued_prompts[0].has_engram_intent());
        });
        state
            .request_stop_session(&session)
            .expect("Stop of the retried admission succeeds");
        status_gate.release();
        ticker.join().unwrap();
    });
    assert_eq!(prompts_received(&receiver), 0, "stopped before delivery");
    with_record(&state, &session, |record| {
        assert!(record.engram.abort_retry.is_none());
        assert!(record.queued_prompts[0].engram_interrupted);
        assert!(record.orchestrator_auto_dispatch_blocked);
    });
    persister.stop(&state);
}

/// Criterion 2: while a settings transaction holds the project's fence (and
/// may still roll back), the tick waits rather than read the unavailable
/// binding target as a changed authority; once the fence drops with the
/// authority unchanged, the retry proceeds.
#[test]
fn a_retry_waits_through_a_project_settings_fence() {
    let (state, session, receiver, persister, _) =
        withheld_root_delivery(checkpoint_reply("abort-grant-1"), fresh_admission_steps());
    await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now());
    let (project_id, fence) = {
        let mut inner = state.inner.lock().unwrap();
        let project_id = engram_project_for_session_locked(&inner, &session)
            .unwrap()
            .id
            .clone();
        let fence = inner.engram_project_resets.claim(&project_id).unwrap();
        (project_id, fence)
    };
    let far = chrono::Utc::now() + chrono::Duration::seconds(3600);
    state.engram_abort_retry_tick(far);
    assert_eq!(prompts_received(&receiver), 0);
    with_record(&state, &session, |record| {
        assert!(
            record.engram.abort_retry.is_some(),
            "kept through the fence"
        );
    });
    // The transaction rolls back: the authority is unchanged.
    assert!(
        state
            .inner
            .lock()
            .unwrap()
            .engram_project_resets
            .release(&project_id, fence)
    );
    state.engram_abort_retry_tick(far);
    assert_eq!(prompts_received(&receiver), 1);
    persister.stop(&state);
}

/// Criterion 1: a retried admission withheld again keeps counting: the
/// second settlement records attempt two, the original held-since time and
/// the next, longer delay.
#[test]
fn a_retry_withheld_again_keeps_counting_attempts() {
    let (begin, begin_gate) = gated_engram_step("turn_begin", begin_reply("abort-grant-2"));
    let (state, session, receiver, persister, _) = withheld_root_delivery(
        checkpoint_reply("abort-grant-1"),
        vec![
            immediate_engram_step("session_status", status_reply("ready")),
            immediate_engram_step("session_bind", bind_reply("abort-token-2")),
            immediate_engram_step("turn_evaluate", grant_reply("abort-grant-2")),
            begin,
            immediate_engram_step("turn_checkpoint", checkpoint_reply("abort-grant-2")),
        ],
    );
    let held_since = with_record(&state, &session, |record| {
        record
            .engram
            .abort_retry
            .as_ref()
            .unwrap()
            .held_since
            .clone()
    });
    await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now());
    let due = chrono::Utc::now() + chrono::Duration::seconds(10);
    std::thread::scope(|scope| {
        let ticker = scope.spawn(|| state.engram_abort_retry_tick(due));
        begin_gate.wait();
        persister.fail_next(1);
        begin_gate.release();
        ticker.join().unwrap();
    });
    assert_eq!(prompts_received(&receiver), 0, "withheld again");
    with_record(&state, &session, |record| {
        let retry = record.engram.abort_retry.as_ref().expect("still recorded");
        assert_eq!(retry.attempts, 2);
        assert_eq!(retry.held_since, held_since);
        assert_eq!(retry.settled_grant_id.as_deref(), Some("abort-grant-2"));
        let due_at = chrono::DateTime::parse_from_rfc3339(&retry.due_at)
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert!(
            due_at >= chrono::Utc::now() + chrono::Duration::seconds(4),
            "the second delay is the longer one"
        );
    });
    persister.stop(&state);
}

/// Criterion 3: a known Defer whose card cannot be saved is settled, and its
/// fresh admission waits for retry_after as well as the backoff.
#[test]
fn a_defer_whose_card_cannot_be_saved_retries_no_earlier_than_retry_after() {
    let (mut state, session, receiver, _) = root_fixture([]);
    queue_test_engram_prompt(&state, &session, PROMPT, QueuedPromptSource::User, None);
    let target = {
        let inner = state.inner.lock().unwrap();
        AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
            .unwrap()
            .unwrap()
    };
    let generation = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        let queued = record.queued_prompts.front_mut().unwrap();
        let fingerprint = engram_turn_intent_fingerprint(
            &queued.pending_prompt.text,
            queued.pending_prompt.expanded_text.as_deref(),
            &queued.attachments,
            queued.pending_prompt.source.as_ref(),
            queued.source,
        );
        queued.engram_evaluate = Some(EngramQueuedEvaluate {
            connection: target.connection.clone(),
            settings: target.settings.clone(),
            operation_generation: None,
            request: EngramControlRequest::TurnEvaluate {
                routing_token: "defer-abort-token".to_owned(),
                intent_fingerprint: fingerprint.clone(),
                requested_effects: target.effects.clone(),
                resource_intents: Vec::new(),
                purpose: "ordinary".to_owned(),
                idempotency_key: "defer-abort-old-key".to_owned(),
            },
            begun_grant_id: None,
            prepared_begin: None,
        });
        record.engram.routing_token = Some("defer-abort-token".to_owned());
        record.session.status = SessionStatus::Active;
        record.engram.pending_dispatch = Some(EngramPendingDispatch {
            causal_failure: None,
            dispatch_generation: record.engram.dispatch_generation,
            intent_fingerprint: fingerprint,
            evaluated: EngramDispatchEvaluation::Defer {
                code: "busy".to_owned(),
                retry_after_ms: Some(600_000),
                wake_condition: "retry later".to_owned(),
            },
            evaluate_latency_ms: 0,
            started_at: std::time::Instant::now(),
            awaiting_runtime_stop_resolution: false,
            begin_requested: None,
            evaluated_work_binding: None,
        });
        record.engram.dispatch_generation
    };
    let durable_path = state.persistence_path.clone();
    state.shutdown_persist_blocking();
    let failing_path = durable_path.with_extension("defer-abort-card-is-directory");
    fs::create_dir_all(&failing_path).unwrap();
    state.persistence_path = Arc::new(failing_path.clone());
    let not_before = chrono::Utc::now() + chrono::Duration::milliseconds(600_000);

    assert_eq!(
        state.finish_engram_dispatch_record_with_defer_retry(
            &session,
            generation,
            None,
            None,
            EngramControlCard {
                causal_failure: None,
                schema_version: ENGRAM_CONTROL_SCHEMA_VERSION,
                stage: EngramControlStage::Dispatch,
                assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
                decision: EngramControlCardDecision::Defer,
                dispatch: EngramControlCardDispatch::Withheld,
                refusal_code: None,
                defer_code: Some("busy".to_owned()),
                grant_id: None,
                directives: Vec::new(),
                delivered_range: None,
                latency_ms: EngramControlLatencyCard {
                    evaluate: Some(0),
                    begin: None,
                    checkpoint: None,
                    total: 0,
                },
                fail_mode: EngramControlFailMode::Enforced,
                repair_armed: false,
                next_intent: None,
                source_root: None,
            },
            Some(not_before),
        ),
        EngramDispatchRecordFinish::PersistenceUnknown
    );
    let due_at = with_record(&state, &session, |record| {
        let retry = record.engram.abort_retry.clone().expect("an abort record");
        assert_eq!(retry.reason, EngramAbortReason::DeferCard);
        assert!(
            !record.engram.abort_retry_acknowledged,
            "the card could not be saved"
        );
        chrono::DateTime::parse_from_rfc3339(&retry.due_at)
            .unwrap()
            .with_timezone(&chrono::Utc)
    });
    // The due time is saved to the millisecond.
    assert!(
        due_at + chrono::Duration::milliseconds(1) > not_before,
        "retry_after is honoured: due {due_at}, not before {not_before}"
    );

    // The store recovers: the tick saves the settlement synchronously (no
    // worker) and acknowledges it on the next pass.
    fs::remove_dir_all(&failing_path).unwrap();
    state.persistence_path = durable_path;
    state.install_control_test_transport(GatedEngramControlTransport::new(fresh_admission_steps()));
    state.engram_abort_retry_tick(chrono::Utc::now());
    state.engram_abort_retry_tick(chrono::Utc::now());
    with_record(&state, &session, |record| {
        assert!(record.engram.abort_retry_acknowledged);
    });
    // Past the backoff but before retry_after: still held.
    state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(60));
    assert_eq!(prompts_received(&receiver), 0);
    // After retry_after: admitted afresh.
    state.engram_abort_retry_tick(due_at + chrono::Duration::seconds(1));
    assert_eq!(prompts_received(&receiver), 1);
}

/// The saved form of `session` and the authority its admission runs under.
fn saved_record_and_authority(state: &AppState, session: &str) -> (String, String) {
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    let authority = AppState::engram_binding_target_for_session_shape_locked(&inner, session, true)
        .unwrap()
        .map(|target| engram_abort_authority(&target))
        .unwrap();
    (
        serde_json::to_string(&PersistedSessionRecord::from_record(record)).unwrap(),
        authority,
    )
}

/// Criterion 4: a record saved after the settlement's acknowledgement
/// rebuilds the retry on load, due without a new wake; one saved before it
/// (a crash between the settlement write and its acknowledgement) and one
/// saved without any abort record keep the conservative hold.
#[test]
fn a_restart_rebuilds_an_acknowledged_retry_and_keeps_an_unsettled_hold() {
    let (state, session, _receiver, persister, _) =
        withheld_root_delivery(checkpoint_reply("abort-grant-1"), Vec::new());
    // Saved before the acknowledgement: the settlement row, not yet known
    // durable, loads as the interrupted hold.
    let (unacknowledged, authority) = saved_record_and_authority(&state, &session);
    let loaded = serde_json::from_str::<PersistedSessionRecord>(&unacknowledged)
        .unwrap()
        .into_record()
        .unwrap();
    assert!(loaded.engram.abort_retry.is_none());
    assert!(loaded.queued_prompts[0].engram_interrupted);
    assert!(loaded.orchestrator_auto_dispatch_blocked);

    // Saved after the acknowledgement: the retry is rebuilt.
    await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now());
    let (settled, _) = saved_record_and_authority(&state, &session);
    persister.stop(&state);
    let mut loaded = serde_json::from_str::<PersistedSessionRecord>(&settled)
        .unwrap()
        .into_record()
        .unwrap();
    assert!(loaded.engram.abort_retry.is_some());
    assert!(loaded.engram.abort_retry_acknowledged);
    assert!(
        loaded.queued_prompts[0].engram_interrupted,
        "still retained"
    );
    assert!(engram_abort_retry_holds_head(&loaded));
    assert!(!loaded.engram.recovered_admission);
    assert!(loaded.orchestrator_auto_dispatch_blocked);
    assert_eq!(
        engram_abort_retry_step(
            &mut loaded,
            Some(&authority),
            chrono::Utc::now() + chrono::Duration::seconds(3600),
        ),
        EngramAbortRetryStep::Due
    );
    assert!(
        loaded.orchestrator_auto_dispatch_blocked,
        "the hold is bypassed only by the exact retried admission"
    );

    // An acknowledged record saved beside a later hold (a Stop of the
    // retried admission moves the dispatch generation) keeps that hold.
    let mut stopped: PersistedSessionRecord = serde_json::from_str(&settled).unwrap();
    stopped.engram_dispatch_generation += 1;
    let mut loaded = stopped.into_record().unwrap();
    assert!(loaded.engram.abort_retry.is_none());
    assert!(loaded.queued_prompts[0].engram_interrupted);
    assert!(!engram_abort_retry_holds_head(&loaded));
    assert_eq!(
        engram_abort_retry_step(
            &mut loaded,
            Some(&authority),
            chrono::Utc::now() + chrono::Duration::seconds(3600),
        ),
        EngramAbortRetryStep::Wait
    );

    // Without any abort record (the settlement never reached the store) the
    // record loads as an interrupted, conservative hold, and no tick retries
    // it.
    let mut legacy: PersistedSessionRecord = serde_json::from_str(&settled).unwrap();
    legacy.engram_abort_retry = None;
    if let Some(head) = legacy.queued_prompts.front_mut() {
        head.engram_interrupted = true;
    }
    let mut loaded = legacy.into_record().unwrap();
    assert!(loaded.engram.abort_retry.is_none());
    assert!(loaded.queued_prompts[0].engram_interrupted);
    assert!(loaded.orchestrator_auto_dispatch_blocked);
    assert_eq!(
        engram_abort_retry_step(
            &mut loaded,
            Some(&authority),
            chrono::Utc::now() + chrono::Duration::seconds(3600),
        ),
        EngramAbortRetryStep::Wait
    );
    assert!(loaded.orchestrator_auto_dispatch_blocked);
}
