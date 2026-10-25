// Owns: the end-to-end live remote wake at a satellite. A wake received for an
// idle native root whose queue is not paused must reach that root's provider
// through the production activation worker, with no Resume or local action.
// Does not own the paused-queue, shutdown or recovery cases
// (federation_executor_round_*.rs) or the central drain
// (federation_executor_recovery_regressions.rs). New module, a child of
// federation_executor_recovery_regressions.rs, whose Fixture it uses.
use super::*;

/// A persist writer that acknowledges every fence, as a healthy production
/// writer does once the state is durable. The fixture's satellite otherwise
/// has none, and a fence no writer receives resolves as `WorkerStopped`.
// The thread ends when the state's last sender drops.
fn acknowledge_all_fences(state: &mut AppState) {
    acknowledge_fences_failing_write_aheads(state, 0);
}

/// As `acknowledge_all_fences`, except that the first `failures` hand-off
/// write-ahead fences fail, as a transient store error does.
fn acknowledge_fences_failing_write_aheads(state: &mut AppState, failures: usize) {
    state.shutdown_persist_blocking();
    let (tx, rx) = mpsc::channel::<PersistRequest>();
    state.persist_tx = tx;
    std::thread::spawn(move || {
        let mut failures = failures;
        while let Ok(request) = rx.recv() {
            let PersistRequest::Fence(fence) = request else { continue; };
            if failures > 0 && matches!(fence.target, PersistFenceTarget::PromptDeliveryWriteAhead { .. }) {
                failures -= 1;
                fence.finish(Err(PersistFenceError::WriteFailed("injected transient store error".into())));
            } else {
                fence.finish(Ok(()));
            }
        }
    });
}

/// Whether the received wake is still queued on the satellite root.
fn wake_still_queued(fixture: &Fixture) -> bool {
    let message = fixture.request["wake"]["messageId"].as_str().unwrap();
    let inner = fixture.satellite.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
    record.queued_prompts.iter().any(|entry| entry.pending_prompt.source.as_ref()
        .and_then(|source| source.mailbox.as_ref())
        .is_some_and(|mailbox| mailbox.message_id == message))
}

/// Waits until the write-ahead that just failed has returned the head to
/// pending, so the next step observes its settled state.
fn await_pending_receipt(fixture: &Fixture) {
    let prompt = fixture.operation().queue_identity();
    let wait = phase_sync::PollGuard::new();
    loop {
        let pending = {
            let inner = fixture.satellite.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
            record.prompt_deliveries.get(&prompt).is_some_and(|receipt|
                receipt.disposition == PromptDeliveryDisposition::Pending && receipt.attempt > 0)
        };
        if pending { return; }
        wait.wait("the failed write-ahead returns the received wake to pending");
    }
}

/// A transient failure of the first hand-off record must not strand the
/// received wake on an idle root: the receiver's bounded recovery pass drives
/// the same queue entry again under the same-boot activation and its fences,
/// and the provider receives the wake exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_live_wake_survives_a_failed_first_hand_off_record() {
    let mut fixture = Fixture::new();
    acknowledge_fences_failing_write_aheads(&mut fixture.satellite, 1);
    let input = idle_unpaused_root(&fixture, "live-remote-wake-retry");
    let (status, body) = fixture.receive(&fixture.request).await;
    assert_eq!(status, StatusCode::ACCEPTED, "authenticated durable wake admission: {body}");
    await_pending_receipt(&fixture);
    assert!(wake_still_queued(&fixture), "the failed hand-off keeps the wake queued");
    assert!(input.try_recv().is_err(), "nothing reached the provider yet");
    let mut recovery = FederationExecutorRecovery::default();
    recovery.recover(&fixture.satellite);
    let CodexRuntimeCommand::Prompt { command, .. } =
        phase_sync::receive(&input, "recovery drives the stranded wake to the idle root's provider")
    else {
        panic!("the recovered wake must start the root's turn");
    };
    assert!(command.delivery_receipt.is_some());
    recovery.recover(&fixture.satellite);
    assert!(input.try_recv().is_err(), "a later recovery pass delivers no second wake");
}

/// The satellite's native root, idle with a live Codex runtime and its queue
/// not paused, as a placed remote root waiting for mail is.
fn idle_unpaused_root(fixture: &Fixture, label: &str) -> mpsc::Receiver<CodexRuntimeCommand> {
    let (runtime, input) = test_codex_runtime_handle(label);
    let mut inner = fixture.satellite.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.native).unwrap();
    let record = &mut inner.sessions[index];
    record.runtime = SessionRuntime::Codex(runtime);
    record.session.codex_fast_mode = false;
    record.session.status = SessionStatus::Idle;
    record.orchestrator_auto_dispatch_blocked = false;
    fixture.satellite.commit_locked(&mut inner).unwrap();
    input
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_live_wake_reaches_an_idle_unpaused_remote_root() {
    let mut fixture = Fixture::new();
    acknowledge_all_fences(&mut fixture.satellite);
    let input = idle_unpaused_root(&fixture, "live-remote-wake");
    let (status, body) = fixture.receive(&fixture.request).await;
    assert_eq!(status, StatusCode::ACCEPTED, "authenticated durable wake admission: {body}");
    let CodexRuntimeCommand::Prompt { command, .. } =
        phase_sync::receive(&input, "the received wake reaches the idle root's provider")
    else {
        panic!("the received wake must start the root's turn");
    };
    assert!(command.delivery_receipt.is_some(), "the wake uses the receipted delivery gate");
    let message = fixture.request["wake"]["messageId"].as_str().unwrap();
    let inner = fixture.satellite.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
    assert!(record.queued_prompts.iter().all(|entry| entry.pending_prompt.source.as_ref()
        .and_then(|source| source.mailbox.as_ref())
        .is_none_or(|mailbox| mailbox.message_id != message)),
        "the delivered wake left the queue");
}

/// The retained same-boot intent is still fenced: a pause that comes after a
/// failed hand-off record ends it, so recovery leaves the wake queued for an
/// explicit Resume instead of starting the paused root.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_live_wake_retry_is_refused_after_the_root_is_paused() {
    let mut fixture = Fixture::new();
    acknowledge_fences_failing_write_aheads(&mut fixture.satellite, 1);
    let input = idle_unpaused_root(&fixture, "live-remote-wake-paused");
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    await_pending_receipt(&fixture);
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        inner.sessions[index].set_auto_dispatch_blocked(true);
        fixture.satellite.commit_locked(&mut inner).unwrap();
    }
    let mut recovery = FederationExecutorRecovery::default();
    recovery.recover(&fixture.satellite);
    recovery.recover(&fixture.satellite);
    assert!(input.try_recv().is_err(), "a paused root is never started by a remote wake retry");
    assert!(wake_still_queued(&fixture), "the wake waits for an explicit Resume");
    assert!(fixture.satellite.federation_executor_wake_signal.live.lock().unwrap()
        .get(&AppState::federation_executor_live_key(&fixture.operation())).is_none(),
        "the pause ended the same-boot activation");
}

/// A lost reply makes central send the exact same wake again, and a
/// reconnect replays it: once the wake reached the provider, neither adds a
/// queue entry or a second prompt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_live_wake_resent_after_delivery_adds_no_second_wake() {
    let mut fixture = Fixture::new();
    acknowledge_all_fences(&mut fixture.satellite);
    let input = idle_unpaused_root(&fixture, "live-remote-wake-resend");
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let CodexRuntimeCommand::Prompt { .. } =
        phase_sync::receive(&input, "the first receive delivers the wake")
    else { panic!("the first receive must start the root's turn"); };
    let (status, body) = fixture.receive(&fixture.request).await;
    assert_eq!(status, StatusCode::ACCEPTED, "an exact resend is an idempotent receive: {body}");
    let mut recovery = FederationExecutorRecovery::default();
    recovery.recover(&fixture.satellite);
    assert!(input.try_recv().is_err(), "a resend or recovery pass delivers no second wake");
    assert!(!wake_still_queued(&fixture), "no second queue entry for the same operation");
}

/// A dispatch that fails before any hand-off (the provider cannot be
/// started) must not strand the wake either: the unpromoted head returns to
/// pending, and once the root can start a runtime, the receiver's recovery
/// pass drives the same queue entry again; the provider receives it once. A
/// definite runtime rejection is terminal by design and is not redriven.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_live_wake_survives_a_failed_first_dispatch() {
    let mut fixture = Fixture::new();
    acknowledge_all_fences(&mut fixture.satellite);
    {
        // No runtime yet, and this fixture cannot spawn one: the first
        // dispatch fails to start the provider before any hand-off.
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        let record = &mut inner.sessions[index];
        assert!(matches!(record.runtime, SessionRuntime::None));
        record.session.status = SessionStatus::Idle;
        record.orchestrator_auto_dispatch_blocked = false;
        fixture.satellite.commit_locked(&mut inner).unwrap();
    }
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    await_pending_receipt(&fixture);
    assert!(wake_still_queued(&fixture), "the failed dispatch keeps the wake queued");
    let input = idle_unpaused_root(&fixture, "live-remote-wake-after-start-failure");
    let mut recovery = FederationExecutorRecovery::default();
    recovery.recover(&fixture.satellite);
    let CodexRuntimeCommand::Prompt { command, .. } =
        phase_sync::receive(&input, "recovery drives the stranded wake once a runtime can start")
    else {
        panic!("the recovered wake must start the root's turn");
    };
    assert!(command.delivery_receipt.is_some());
    recovery.recover(&fixture.satellite);
    assert!(input.try_recv().is_err(), "a later recovery pass delivers no second wake");
}

/// A writer that holds the first hand-off write-ahead fence for the test to
/// resolve, and acknowledges every other fence.
fn hold_first_write_ahead(state: &mut AppState) -> mpsc::Receiver<Box<PersistFence>> {
    state.shutdown_persist_blocking();
    let (tx, rx) = mpsc::channel::<PersistRequest>();
    state.persist_tx = tx;
    let (held_tx, held) = mpsc::channel();
    std::thread::spawn(move || {
        let mut holding = true;
        while let Ok(request) = rx.recv() {
            let PersistRequest::Fence(fence) = request else { continue; };
            if holding && matches!(fence.target, PersistFenceTarget::PromptDeliveryWriteAhead { .. }) {
                holding = false;
                let _ = held_tx.send(fence);
            } else {
                fence.finish(Ok(()));
            }
        }
    });
    held
}

/// A recovery pass that lands while the first hand-off record is still being
/// saved (a slow store) must not end the same-boot activation: when that
/// record then fails, a later recovery pass still delivers the wake, once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_live_wake_survives_recovery_during_a_slow_failed_hand_off_record() {
    let mut fixture = Fixture::new();
    let held = hold_first_write_ahead(&mut fixture.satellite);
    let input = idle_unpaused_root(&fixture, "live-remote-wake-slow");
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let fence = phase_sync::receive(&held, "the first hand-off record is being saved");
    {
        let inner = fixture.satellite.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
        assert_eq!(record.prompt_deliveries[&fixture.operation().queue_identity()].disposition,
            PromptDeliveryDisposition::HandingOff);
    }
    let mut recovery = FederationExecutorRecovery::default();
    recovery.recover(&fixture.satellite);
    assert!(input.try_recv().is_err(), "nothing is delivered while the record is unsaved");
    fence.finish(Err(PersistFenceError::WriteFailed("injected slow store failure".into())));
    await_pending_receipt(&fixture);
    recovery.recover(&fixture.satellite);
    let CodexRuntimeCommand::Prompt { command, .. } =
        phase_sync::receive(&input, "recovery drives the wake after the slow record failed")
    else {
        panic!("the recovered wake must start the root's turn");
    };
    assert!(command.delivery_receipt.is_some());
    recovery.recover(&fixture.satellite);
    assert!(input.try_recv().is_err(), "a later recovery pass delivers no second wake");
}

/// The activation worker re-running its captured work while another drive's
/// hand-off record is still being saved must not end the same-boot
/// activation either: a hand-off in flight is not a retired association.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_live_wake_survives_an_activation_rerun_during_a_slow_failed_hand_off_record() {
    let mut fixture = Fixture::new();
    let held = hold_first_write_ahead(&mut fixture.satellite);
    let input = idle_unpaused_root(&fixture, "live-remote-wake-rerun");
    // The authenticated receive captures the activation work; the worker has
    // not run yet.
    assert_eq!(fixture.receive_raw(&fixture.request).await.0, StatusCode::ACCEPTED);
    // Receiver recovery associates the wake first and begins its hand-off.
    let mut recovery = FederationExecutorRecovery::default();
    recovery.recover(&fixture.satellite);
    let fence = phase_sync::receive(&held, "recovery's hand-off record is being saved");
    // The activation worker now runs the captured work while that record is unsaved.
    let mut worker = FederationExecutorActivationWorker::default();
    worker.tick(&fixture.satellite, true).await;
    worker.shutdown(&fixture.satellite).await;
    assert!(input.try_recv().is_err(), "nothing is delivered while the record is unsaved");
    fence.finish(Err(PersistFenceError::WriteFailed("injected slow store failure".into())));
    await_pending_receipt(&fixture);
    recovery.recover(&fixture.satellite);
    let CodexRuntimeCommand::Prompt { command, .. } =
        phase_sync::receive(&input, "recovery drives the wake after the activation rerun")
    else {
        panic!("the recovered wake must start the root's turn");
    };
    assert!(command.delivery_receipt.is_some());
    recovery.recover(&fixture.satellite);
    assert!(input.try_recv().is_err(), "a later recovery pass delivers no second wake");
}
