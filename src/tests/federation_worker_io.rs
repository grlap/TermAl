// Owns bounded OS-operation and worker-basis lifecycle regressions.
// Does not touch an OS store or run SSH. Split from federation_runtime.rs.

use super::*;

struct ReleaseRead(Arc<(Mutex<bool>, std::sync::Condvar)>);
impl Drop for ReleaseRead {
    fn drop(&mut self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_pending_credential_read_keeps_stop_bounded_without_quarantine() {
    let mut fixture = Fixture::new().await;
    let released = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let release = ReleaseRead(released.clone());
    let (began, began_rx) = tokio::sync::oneshot::channel();
    let began = Arc::new(Mutex::new(Some(began)));
    let (finished, finished_rx) = tokio::sync::oneshot::channel();
    let finished = Arc::new(Mutex::new(Some(finished)));
    fixture.io.credential_reporting_deadline = Duration::ZERO;
    let (pending, pending_rx) = tokio::sync::oneshot::channel();
    let pending = Arc::new(Mutex::new(Some(pending)));
    fixture.io.credential_observed = Arc::new(move |state| {
        if state.starts_with("pending;") {
            if let Some(pending) = pending.lock().unwrap().take() {
                let _ = pending.send(());
            }
        }
    });
    fixture.io.credentials = Arc::new(move |_| {
        if let Some(began) = began.lock().unwrap().take() {
            let _ = began.send(());
        }
        let mut ready = released.0.lock().unwrap();
        while !*ready {
            ready = released.1.wait(ready).unwrap();
        }
        if let Some(finished) = finished.lock().unwrap().take() {
            let _ = finished.send(());
        }
        Err(federation_credential_error())
    });
    let runtime = start_federation_runtime_with_io(fixture.app.clone(), fixture.io.clone());
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, began_rx)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, pending_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(runtime.peer(&fixture.id).is_none());
    let quarantined = runtime.quarantined.clone();
    let id = fixture.id.clone();
    let shutdown = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, fixture.finish(runtime)).await;
    // Shutdown must finish while the actual OS-boundary call is still held.
    drop(release);
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, finished_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(shutdown.is_ok());
    assert!(!quarantined.lock().unwrap().contains(&id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_worker_survives_bookkeeping_changes_on_the_same_incarnation() {
    let mut fixture = Fixture::new().await;
    let runtime = start_federation_runtime_with_io(fixture.app.clone(), fixture.io.clone());
    assert!(fixture.event(|e| matches!(e, Event::Response(path, status) if path.ends_with("health") && status.is_success())).await);
    let original = runtime.peer(&fixture.id).unwrap().channel.clone();
    let incarnation = original.current_lease().unwrap().incarnation;
    fixture
        .app
        .verify_federation_peer(
            &original.current_authority().unwrap(),
            federation_registry::checks(),
        )
        .unwrap();
    let verified = original.current_authority().unwrap();
    fixture
        .app
        .transition_federation_peer(&verified, FederationMembershipState::Active)
        .unwrap();
    let active = original.current_authority().unwrap();
    fixture
        .app
        .transition_federation_peer(&active, FederationMembershipState::Draining)
        .unwrap();
    assert!(fixture.event(|e| matches!(e, Event::Response(path, status) if path.ends_with("health") && status.is_success())).await);
    let current = runtime.peer(&fixture.id).unwrap().channel.clone();
    let same_instance = current.server_instance_id == original.server_instance_id;
    let same_incarnation = current.current_lease().unwrap().incarnation == incarnation;
    let membership = current.current_authority().unwrap().membership;
    fixture.finish(runtime).await;
    assert!(same_instance);
    assert!(same_incarnation);
    assert_eq!(membership, FederationMembershipState::Draining);
}
