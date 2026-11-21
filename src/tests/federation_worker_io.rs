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
    let view = fixture.app.federation_registry_view().unwrap();
    let worker = view["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|peer| peer["host"]["hostId"] == fixture.id)
        .unwrap()["attestation"]["worker"]
        .clone();
    let id = fixture.id.clone();
    let shutdown = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, fixture.finish(runtime)).await;
    // Shutdown must finish while the actual OS-boundary call is still held.
    drop(release);
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, finished_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(shutdown.is_ok());
    assert_eq!(worker["stage"], "credentialSettlement");
    assert_eq!(worker["currentBasis"], true);
    assert!(worker["startedAt"].as_str().is_some());
    assert!(!quarantined.lock().unwrap().contains(&id));
}

async fn observe_first_worker_retry(mut fixture: Fixture) -> Value {
    let app = fixture.app.clone();
    let id = fixture.id.clone();
    let (observed, result) = tokio::sync::oneshot::channel();
    let observed = Arc::new(Mutex::new(Some(observed)));
    fixture.io.attempt_finished = Arc::new(move |_| {
        let view = app.federation_registry_view().unwrap();
        let peer = view["peers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|peer| peer["host"]["hostId"] == id)
            .unwrap()
            .clone();
        if let Some(observed) = observed.lock().unwrap().take() {
            let _ = observed.send(peer);
        }
    });
    let runtime = start_federation_runtime_with_io(fixture.app.clone(), fixture.io.clone());
    let observed = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, result)
        .await
        .unwrap()
        .unwrap();
    fixture.finish(runtime).await;
    observed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_worker_reports_retry_with_current_attempt_and_basis() {
    let mut fixture = Fixture::new().await;
    let released = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let release_on_failure = ReleaseRead(released.clone());
    let release_on_pending = released.clone();
    fixture.io.credential_reporting_deadline = Duration::ZERO;
    fixture.io.credential_observed = Arc::new(move |state| {
        if state.starts_with("pending;") {
            drop(ReleaseRead(release_on_pending.clone()));
        }
    });
    fixture.io.credentials = Arc::new(move |_| {
        let mut ready = released.0.lock().unwrap();
        while !*ready {
            ready = released.1.wait(ready).unwrap();
        }
        Err(federation_credential_error())
    });
    let observed = observe_first_worker_retry(fixture).await;
    drop(release_on_failure);
    let worker = &observed["attestation"]["worker"];
    assert!(observed["attestation"]["currentBasis"].is_null());
    assert_eq!(observed["attestation"]["state"], "unverified");
    assert_eq!(worker["stage"], "retryBackoff");
    assert_eq!(worker["attempt"], 1);
    assert_eq!(worker["currentBasis"], true);
    assert_eq!(
        worker["enrollmentGeneration"],
        observed["enrollmentGeneration"]
    );
    assert_eq!(
        worker["transportGeneration"],
        observed["transportGeneration"]
    );
    assert!(
        worker["retryAfterMs"]
            .as_u64()
            .is_some_and(|delay| (1000..=2000).contains(&delay))
    );
    let terminal = &observed["attestation"]["lastFailedAttempt"];
    assert_eq!(
        terminal["stage"], "credentialRead",
        "retry backoff must retain the credential failure's terminal phase"
    );
    assert_eq!(terminal["reason"], "credentialsUnavailable");
    assert_eq!(terminal["status"], StatusCode::SERVICE_UNAVAILABLE.as_u16());
    for field in [
        "workerId",
        "attempt",
        "enrollmentGeneration",
        "transportGeneration",
    ] {
        assert_eq!(terminal[field], worker[field]);
    }
    assert!(terminal["ssh"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_worker_reports_retry_with_a_registered_live_link() {
    let mut fixture = Fixture::new().await;
    let app = fixture.app.clone();
    let credentials_read = Arc::new(AtomicBool::new(false));
    let read = credentials_read.clone();
    fixture.io.credentials = Arc::new(move |_| {
        read.store(true, Ordering::SeqCst);
        Err(federation_credential_error())
    });
    let basis = app.federation_peer_snapshot(&fixture.id).unwrap();
    let incarnation = Uuid::new_v4().to_string();
    let installed = app
        .federation_peer_connected(&basis, incarnation.clone())
        .unwrap();
    let observed = observe_first_worker_retry(fixture).await;
    assert!(app.federation_peer_disconnected(&installed, &incarnation));
    let registry = app.federation_registry_view().unwrap();
    let after = &registry["peers"][0];

    assert!(!credentials_read.load(Ordering::SeqCst));
    assert_eq!(observed["attestation"]["worker"]["stage"], "retryBackoff");
    let terminal = &observed["attestation"]["lastFailedAttempt"];
    assert_eq!(
        terminal["reason"], "liveLinkStillRegistered",
        "reconnect must identify the live-link guard before credentials"
    );
    assert_eq!(terminal["stage"], "basisValidation");
    assert_eq!(terminal["status"], StatusCode::CONFLICT.as_u16());
    assert_eq!(observed["liveLinkPresent"], true);
    assert_eq!(after["liveLinkPresent"], false);
    assert_eq!(observed["lastObserved"]["linkIncarnation"], incarnation);
    assert_eq!(after["lastObserved"], observed["lastObserved"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_worker_reports_retry_with_listener_bind_refusal() {
    let mut fixture = Fixture::new().await;
    let occupant = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut peer = fixture.app.inner.lock().unwrap().federation.peers[&fixture.id].clone();
    peer.endpoint.central_federation_port = occupant.local_addr().unwrap().port();
    fixture.app.enroll_federation_peer(peer).unwrap();
    let started_ssh = Arc::new(AtomicBool::new(false));
    let started = started_ssh.clone();
    fixture.io.ssh = Arc::new(move |_, _, _, _| {
        started.store(true, Ordering::SeqCst);
        Err(federation_transport_unavailable())
    });

    let observed = observe_first_worker_retry(fixture).await;
    drop(occupant);
    assert!(!started_ssh.load(Ordering::SeqCst));
    assert_eq!(observed["attestation"]["worker"]["stage"], "retryBackoff");
    let failure = &observed["attestation"]["lastFailedAttempt"];
    assert_eq!(failure["stage"], "listenerBind");
    assert_eq!(failure["reason"], "listenerAddressInUse");
    assert_eq!(failure["status"], StatusCode::SERVICE_UNAVAILABLE.as_u16());
    assert!(failure["ssh"].is_null());
}

#[test]
fn federation_worker_old_basis_and_owner_cannot_publish_fresh_attempt() {
    let app = federation_registry::central();
    let peer = federation_registry::peer(47104);
    let id = peer.identity.host_id.clone();
    app.enroll_federation_peer(peer).unwrap();
    let fresh = app.federation_registry_view().unwrap();
    assert!(fresh["peers"][0]["attestation"]["currentBasis"].is_null());
    let snapshot = app.federation_peer_snapshot(&id).unwrap();
    let basis = Arc::new(FederationTransportBasis::new(snapshot.clone()));
    let mut old = FederationWorkerReporter::new(app.clone(), basis);
    old.begin();
    old.stage("controlStartup");
    old.failure(
        None,
        "controlStreamUnavailable",
        &federation_transport_unavailable(),
    );
    let failure =
        app.federation_registry_view().unwrap()["peers"][0]["attestation"]["lastFailedAttempt"]
            .clone();
    assert_eq!(failure["currentBasis"], true);
    let channel = FederationChannel::for_app(
        snapshot,
        FederationPairCapability::new("a".repeat(64)).unwrap(),
        app.clone(),
    );
    let owner = channel.next_security_owner().unwrap();
    channel.observe_attestation_failure(Some(owner), "startup", "startupExpired", false);
    let mut record = app.inner.lock().unwrap().federation.peers[&id].clone();
    record.endpoint.ssh_host = "moved.example".to_owned();
    app.enroll_federation_peer(record).unwrap();
    old.stage("authenticated");
    let view = app.federation_registry_view().unwrap();
    let observed = &view["peers"][0]["attestation"];
    assert_eq!(observed["state"], "historical");
    assert_eq!(observed["currentBasis"], false);
    assert_eq!(observed["worker"]["currentBasis"], false);
    assert_eq!(observed["worker"]["stage"], "controlStartup");
    let snapshot = app.federation_peer_snapshot(&id).unwrap();
    let basis = Arc::new(FederationTransportBasis::new(snapshot));
    let mut late = FederationWorkerReporter::new(app.clone(), basis.clone());
    late.begin();
    let mut successor = FederationWorkerReporter::new(app.clone(), basis);
    successor.begin();
    successor.stage("credentialRead");
    drop(old);
    late.stage("authenticated");
    late.failure(
        None,
        "controlMonitorFailed",
        &federation_transport_unavailable(),
    );
    drop(late);
    let view = app.federation_registry_view().unwrap();
    assert_eq!(
        view["peers"][0]["attestation"]["worker"]["stage"],
        "credentialRead"
    );
    assert_eq!(
        view["peers"][0]["attestation"]["worker"]["currentBasis"],
        true
    );
    let historical = &view["peers"][0]["attestation"]["lastFailedAttempt"];
    assert_eq!(historical["workerId"], failure["workerId"]);
    assert_eq!(historical["reason"], "controlStreamUnavailable");
    assert_eq!(historical["currentBasis"], false);
    channel.invalidate();
}

#[test]
fn federation_worker_reporter_drop_does_not_panic_during_unwind_or_after_basis_poison() {
    let app = federation_registry::central();
    let peer = federation_registry::peer(47104);
    let id = peer.identity.host_id.clone();
    app.enroll_federation_peer(peer).unwrap();
    let snapshot = app.federation_peer_snapshot(&id).unwrap();
    let basis = Arc::new(FederationTransportBasis::new(snapshot));
    let mut reporter = FederationWorkerReporter::new(app.clone(), basis.clone());
    reporter.begin();
    let poison = basis.clone();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _held = poison.write().unwrap();
            let _reporter = reporter;
            panic!("injected worker failure with held basis");
        }))
        .is_err()
    );
    let reporter = FederationWorkerReporter::new(app, basis);
    reporter.stage("stopped");
    drop(reporter);
}

struct DiagnosticSsh {
    child: Box<dyn FederationSshChild>,
    diagnostic: FederationSshDiagnostics,
}
impl FederationSshChild for DiagnosticSsh {
    fn take_control_stream(&mut self) -> Result<FederationControlIo, ApiError> {
        self.child.take_control_stream()
    }
    fn child_exited(&mut self) -> Result<bool, ApiError> {
        self.child.child_exited()
    }
    fn diagnostics(&self) -> Option<FederationSshDiagnostics> {
        Some(self.diagnostic.clone())
    }
    fn shutdown(&mut self) -> Result<(), ApiError> {
        self.child.shutdown()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_worker_monitor_attaches_owned_ssh_diagnostics_to_registry_view() {
    let mut fixture = Fixture::new().await;
    let diagnostic = Arc::new(Mutex::new(FederationSshDiagnostic::default()));
    let attached = diagnostic.clone();
    let start = fixture.io.ssh.clone();
    fixture.io.ssh = Arc::new(move |snapshot, port, pin, owned| {
        start(snapshot, port, pin, owned)?;
        *owned = Some(Box::new(DiagnosticSsh {
            child: owned.take().unwrap(),
            diagnostic: attached.clone(),
        }));
        Ok(())
    });
    let runtime = start_federation_runtime_with_io(fixture.app.clone(), fixture.io.clone());
    let healthy = fixture
        .event(|e| {
            matches!(e, Event::Response(path, status)
        if path.ends_with("health") && status.is_success())
        })
        .await;
    // Mutate the exact process-owned diagnostic after monitor attachment.
    diagnostic.lock().unwrap().class = Some("forward");
    let view = fixture.app.federation_registry_view().unwrap();
    let worker = view["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|peer| peer["host"]["hostId"] == fixture.id)
        .unwrap()["attestation"]["worker"]
        .clone();
    fixture.finish(runtime).await;
    assert!(healthy);
    assert_eq!(worker["stage"], "authenticated");
    assert_eq!(worker["currentBasis"], true);
    assert_eq!(worker["ssh"]["class"], "forward");
    assert_eq!(worker["ssh"]["exited"], false);
    assert_eq!(worker["ssh"]["cleanupRequested"], false);
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
