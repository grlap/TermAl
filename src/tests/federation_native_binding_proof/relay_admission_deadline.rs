// Owns the relay home's admission deadline while the owner check is held,
// through authenticated HTTP: the check never runs on an async worker, the
// unchanged deadline still fires, and its record names the phase.
// Does not own the failure-stage records in relay_home_rpc_failures.rs.
// New module beside relay_home_rpc_failures.rs.
use super::*;

/// Opens the held owner check however the test ends, so no run hangs on it.
struct OwnerCheckRelease(Arc<(Mutex<bool>, std::sync::Condvar)>);

impl Drop for OwnerCheckRelease {
    fn drop(&mut self) {
        phase_sync::set_gate(&self.0, true);
    }
}

/// True when called on an async runtime worker, where blocking on the
/// runtime is refused; false on a blocking-pool thread.
fn on_async_worker() -> bool {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tokio::runtime::Handle::current().block_on(async {})
    }))
    .is_err()
}

/// A slow owner check during a relay request's admission (in production, a
/// StateInner wait) no longer runs on an async worker: the unchanged deadline
/// fires while the check is still held, the answer is an UNKNOWN outcome, and
/// the failure record names the phase the request was waiting in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_deadline_during_a_held_owner_check_names_the_admission_phase() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let armed = Arc::new(AtomicBool::new(false));
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let (entered_tx, entered) = std::sync::mpsc::channel::<bool>();
    let entered_tx = Mutex::new(entered_tx);
    let checked = fixture.channel.clone();
    let holding = armed.clone();
    let held = gate.clone();
    let ensure_current: FederationAuthorityCheck = Arc::new(move || {
        if holding.swap(false, Ordering::SeqCst) {
            let on_worker = on_async_worker();
            let _ = entered_tx.lock().unwrap().send(on_worker);
            if !on_worker {
                phase_sync::wait_for_gate(&held, true);
            }
        }
        checked.current_authority().map(|_| ())
    });
    let channel = fixture.channel.clone();
    let proxy = ProxySend::with_home_router(&fixture, move |_| {
        federation_router(channel, ensure_current)
    })
    .await;
    let sent = proxy.send(&fixture, "held-relay-read").await;
    read_relay_page(&fixture.satellite, &fixture.native, &sent.mailbox_id)
        .await
        .unwrap();
    let release = OwnerCheckRelease(gate);
    armed.store(true, Ordering::SeqCst);
    let satellite = fixture.satellite.clone();
    let reader = fixture.native.clone();
    let mailbox = sent.mailbox_id.clone();
    let read = tokio::spawn(async move { read_relay_page(&satellite, &reader, &mailbox).await });
    let checked_on_worker = phase_sync::receive(&entered, "the relay owner check started");
    if checked_on_worker {
        drop(release);
        panic!("the relay owner check ran on an async worker, where its wait blocks the runtime");
    }
    // The check is still held. Step past the unchanged deadline: the answer
    // arrives while it is held, so nothing here waits on host time.
    fixture
        .channel
        .control
        .clock
        .advance(Duration::from_secs(5));
    let outcome = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, read)
        .await
        .unwrap()
        .unwrap();
    drop(release);
    let failures = get_mailbox_relay_wake_diagnostics(State(fixture.central.clone()))
        .await
        .unwrap()
        .0["homeRpcFailures"]
        .as_array()
        .unwrap()
        .clone();
    proxy.finish().await;
    fixture.finish().await;
    assert_eq!(
        outcome.err().map(|error| error.status),
        Some(StatusCode::SERVICE_UNAVAILABLE),
        "an expired request is an UNKNOWN outcome"
    );
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0]["stage"], "handlerDeadline");
    assert_eq!(failures[0]["operation"], "read");
    assert_eq!(failures[0]["phase"], "admission");
}
