// Owns the tests of a fresh evaluator spawn whose first brief is not
// acknowledged as durable: the persist writer fails the delegation's fence, so
// the request must refuse with a typed, retryable error after retiring the
// spawned delegation, and never leave a Running evaluator that no turn will
// start and that blocks every later request for the task. Also covers what
// may happen while that acknowledgement is awaited: a requester taking the
// evaluator over, and a wait registered on it. Does not own the submission's
// own acknowledgements (the stepped writer tests in the parent module) or the
// reuse path. New module, a child of the acceptance evaluation tests whose
// spawn fixture it uses.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// A persist writer that answers every fence itself without writing, except
/// the first acknowledgement of a fresh evaluator's delegation: that fence is
/// handed to the test, which decides when it fails. Failing it with
/// `Deadline` is what a writer reports when the fence's deadline passes,
/// without any clock or load.
struct FirstBriefAckWriter {
    tx: mpsc::Sender<PersistRequest>,
    held: mpsc::Receiver<(String, Box<PersistFence>)>,
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl FirstBriefAckWriter {
    fn attach(state: &mut AppState) -> Self {
        state.shutdown_persist_blocking();
        let (tx, rx) = mpsc::channel::<PersistRequest>();
        state.persist_tx = tx.clone();
        let (held_tx, held) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let writer_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut armed = true;
            while let Ok(request) = rx.recv() {
                if writer_stop.load(Ordering::SeqCst) {
                    break;
                }
                let PersistRequest::Fence(fence) = request else {
                    continue;
                };
                let first_brief = match &fence.target {
                    PersistFenceTarget::Delegation(record)
                        if armed
                            && record
                                .acceptance_evaluation
                                .as_ref()
                                .is_some_and(|target| target.attempt_history.is_some()) =>
                    {
                        Some(record.id.clone())
                    }
                    _ => None,
                };
                match first_brief {
                    Some(id) => {
                        armed = false;
                        let _ = held_tx.send((id, fence));
                    }
                    None => fence.finish(Ok(())),
                }
            }
        });
        Self { tx, held, stop, thread: Mutex::new(Some(thread)) }
    }

    /// Stops the writer and waits until its receiver is gone, so a later
    /// commit finds the channel disconnected and saves synchronously.
    fn disconnect(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.tx.send(PersistRequest::Delta);
        if let Some(thread) = self.thread.lock().unwrap().take() {
            let _ = thread.join();
        }
    }
}

impl Drop for FirstBriefAckWriter {
    fn drop(&mut self) {
        self.disconnect();
    }
}

fn first_brief_fixture(runtime: &str) -> (AppState, String, FirstBriefAckWriter) {
    let (mut state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    super::super::delegation_support::install_delegation_codex_runtime(&state, runtime);
    let writer = FirstBriefAckWriter::attach(&mut state);
    (state, parent, writer)
}

/// Runs a spawn request whose first-brief fence the writer holds, lets
/// `meanwhile` act on the spawned delegation while its acknowledgement is
/// awaited, then fails that fence and returns the request's refusal.
fn refused_with_first_brief_unacknowledged(
    state: &AppState,
    parent: &str,
    writer: &FirstBriefAckWriter,
    meanwhile: impl FnOnce(&str),
) -> ApiError {
    std::thread::scope(|scope| {
        let request = scope.spawn(|| spawn_request(state, parent));
        let (id, fence) = phase_sync::receive(&writer.held, "the evaluator's first-brief fence");
        // The request stays blocked until the fence resolves, so it is failed
        // even when `meanwhile` panics; the panic is raised after the join.
        let meanwhile =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| meanwhile(&id)));
        fence.finish(Err(PersistFenceError::Deadline));
        let outcome = request.join().unwrap();
        if let Err(panic) = meanwhile {
            std::panic::resume_unwind(panic);
        }
        outcome
            .err()
            .expect("an unacknowledged first brief cannot start an evaluator")
    })
}

#[test]
fn acceptance_spawn_unacknowledged_first_brief_retires_the_evaluator_and_refuses_retryably() {
    let (state, parent, writer) = first_brief_fixture("acceptance-first-brief-ack-runtime");

    // Every observation is taken before any assertion, so a failure shows the
    // whole outcome: the refusal, what the spawned delegation was left as,
    // whether the task stayed blocked and what a later read made of it.
    let refused = refused_with_first_brief_unacknowledged(&state, &parent, &writer, |_| {});
    let (spawned, child) = {
        let inner = state.inner.lock().unwrap();
        let record = inner.delegations.first().cloned().expect("the evaluator was spawned");
        let child = inner.sessions[inner.find_session_index(&record.child_session_id).unwrap()].clone();
        (record, child)
    };
    // The writer acknowledges from here on.
    let second = spawn_request(&state, &parent);
    let second_outcome = match &second {
        Ok(AcceptanceEvaluationRequestResponse::Spawned { delegation, .. }) => {
            format!("spawned `{}`", delegation.delegation.id)
        }
        Ok(_) => "answered without a spawn".to_owned(),
        Err(error) => format!("{} {}", error.status, error.message),
    };
    state.get_delegation(&parent, &spawned.id).unwrap();
    let (read_status, read_summary) = {
        let inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&spawned.id).unwrap();
        let record = &inner.delegations[index];
        (record.status, record.result.as_ref().map(|result| result.summary.clone()))
    };
    let observed = format!(
        "refusal: {} {:?} {:?}; spawned left {:?} with {} child messages; \
         second request: {second_outcome}; after a read: {read_status:?} {read_summary:?}",
        refused.status,
        refused.kind,
        refused.message,
        spawned.status,
        child.session.messages.len(),
    );

    assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE, "{observed}");
    assert_eq!(
        refused.kind,
        Some(ApiErrorKind::AcceptanceEvaluationFirstBriefUnacknowledged),
        "{observed}"
    );
    assert!(refused.message.contains("first-brief durable acknowledgement"), "{observed}");
    // The persist phase comes from the fence's own progress: this writer took
    // the fence off the channel without a tick, so it never left the queue.
    assert!(refused.message.contains(PersistFencePhase::Queued.label()), "{observed}");
    // Retired, not left Running with no turn; no evaluator turn was started.
    assert_eq!(spawned.status, DelegationStatus::Canceled, "{observed}");
    assert!(
        child.session.messages.is_empty() && child.queued_prompts.is_empty(),
        "{observed}"
    );
    // The task is not blocked: once the writer acknowledges, the same request
    // spawns a fresh evaluator.
    let Ok(AcceptanceEvaluationRequestResponse::Spawned { delegation: second, .. }) = second else {
        panic!("the request succeeds once the first brief is acknowledged: {observed}");
    };
    assert_ne!(second.delegation.id, spawned.id, "{observed}");
    // A later read does not turn it into a failure without a result packet.
    assert_eq!(read_status, DelegationStatus::Canceled, "{observed}");
    drop(writer);
}

#[test]
fn acceptance_spawn_unacknowledged_first_brief_spares_an_evaluator_a_requester_took_over() {
    let (state, parent, writer) = first_brief_fixture("acceptance-first-brief-takeover-runtime");
    let mut admitted = None;
    // While the first brief's acknowledgement is awaited, a requester's direct
    // prompt passes the evaluator's admission: it marks the evaluation tainted
    // and holds the single-flight boundary its dispatch then runs under.
    let refused = refused_with_first_brief_unacknowledged(&state, &parent, &writer, |id| {
        let child = state.inner.lock().unwrap().delegations
            .iter().find(|record| record.id == id).unwrap().child_session_id.clone();
        let request: SendMessageRequest =
            serde_json::from_value(json!({ "text": "A requester's own question." })).unwrap();
        admitted = Some(
            state
                .reserve_direct_requester_acceptance_prompt(&child, &request)
                .expect("a Running evaluator admits a direct requester prompt")
                .expect("the child belongs to an evaluator"),
        );
    });
    let spawned = state.inner.lock().unwrap().delegations[0].clone();
    drop(admitted);

    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.message);
    assert_eq!(refused.kind, None, "{}", refused.message);
    assert!(
        refused.message.contains("taken over") && refused.message.contains(&spawned.id),
        "{}",
        refused.message
    );
    assert_eq!(
        spawned.status,
        DelegationStatus::Running,
        "an evaluator a requester took over is not retired under it"
    );
    drop(writer);
}

#[test]
fn acceptance_spawn_unacknowledged_first_brief_settles_a_wait_registered_meanwhile() {
    let (state, parent, writer) = first_brief_fixture("acceptance-first-brief-wait-runtime");
    // The delegation is published before its first brief is acknowledged, so
    // its parent can already wait on it.
    let mut pending_waits = None;
    let refused = refused_with_first_brief_unacknowledged(&state, &parent, &writer, |id| {
        let request: CreateDelegationWaitRequest =
            serde_json::from_value(json!({ "delegationIds": [id] })).unwrap();
        state.create_delegation_wait(&parent, request).expect("the parent waits on its evaluator");
        pending_waits = Some(state.inner.lock().unwrap().delegation_waits.len());
    });

    assert_eq!(pending_waits, Some(1), "the wait was pending while the brief was unacknowledged");
    assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE, "{}", refused.message);
    let inner = state.inner.lock().unwrap();
    assert_eq!(inner.delegations[0].status, DelegationStatus::Canceled);
    assert!(
        inner.delegation_waits.is_empty(),
        "the retirement settles the wait as a cancel does, with no later refresh"
    );
    drop(inner);
    drop(writer);
}

#[test]
fn acceptance_spawn_unacknowledged_first_brief_hands_off_a_wait_when_the_retirement_save_fails() {
    let (mut state, parent, writer) = first_brief_fixture("acceptance-first-brief-save-fail-runtime");
    // While the writer runs, a commit only queues its write. Once it is gone a
    // commit saves synchronously, and this store, a directory, refuses that.
    let failing = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("first-brief-retirement-store-is-directory");
    fs::create_dir_all(&failing).unwrap();
    state.persistence_path = Arc::new(failing.clone());
    let mut deltas = state.subscribe_delta_events();
    let mut wait_id = None;
    let refused = refused_with_first_brief_unacknowledged(&state, &parent, &writer, |id| {
        let request: CreateDelegationWaitRequest =
            serde_json::from_value(json!({ "delegationIds": [id] })).unwrap();
        let wait = state.create_delegation_wait(&parent, request).expect("the parent waits on its evaluator");
        wait_id = Some(wait.wait.id);
        // The retirement's commit is the first to find the writer gone.
        writer.disconnect();
    });
    let wait_id = wait_id.expect("the wait was registered");
    let mut events = Vec::new();
    loop {
        match deltas.try_recv() {
            Ok(event) => events.push(event),
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
    let handed_off = events
        .iter()
        .any(|event| event.contains(&wait_id) && event.contains("WaitConsumed"));

    assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE, "{}", refused.message);
    let inner = state.inner.lock().unwrap();
    assert_eq!(inner.delegations[0].status, DelegationStatus::Canceled);
    assert!(inner.delegation_waits.is_empty(), "the wait was consumed by the retirement");
    drop(inner);
    assert!(
        handed_off,
        "a failed save still hands the consumed wait off, so its parent's resume is dispatched"
    );
    drop(writer);
    fs::remove_dir_all(failing).unwrap();
}
