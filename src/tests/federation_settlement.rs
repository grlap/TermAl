// Owns the supervisor race between durable publication and receipt adoption.
// Does not provision SSH or credentials. Split from federation_runtime.rs.
use super::*;

struct ReleaseSettlement(Arc<(Mutex<bool>, std::sync::Condvar)>);
impl Drop for ReleaseSettlement {
    fn drop(&mut self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_connect_cancelled_settlement_keeps_worker_material_and_retry() {
    let mut fixture = Fixture::new().await;
    // Make this the satellite so the production control handler's own deadline
    // cancels the opening caller, without stopping the per-peer worker.
    let mut record = fixture.app.inner.lock().unwrap().federation.peers[&fixture.id].clone();
    record.endpoint.satellite_federation_port = record.endpoint.central_federation_port;
    let local_port = record.endpoint.satellite_federation_port;
    fixture
        .app
        .configure_federation(FederationSettings {
            enabled: true,
            role: FederationRole::Satellite,
            central: Some(record.identity.clone()),
        })
        .unwrap();
    fixture.app.enroll_federation_peer(record).unwrap();
    let local = fixture.app.federation_peer_snapshot(&fixture.id).unwrap();
    let mut remote = local.clone();
    remote.local = local.peer.clone();
    remote.peer = local.local.clone();
    remote.local_role = FederationRole::Central;
    let central = FederationChannel::new(
        remote,
        FederationPairCapability::new("a".repeat(64)).unwrap(),
    );

    let read_count = Arc::new(AtomicUsize::new(0));
    let reads = read_count.clone();
    let read = fixture.io.credentials.clone();
    fixture.io.credentials = Arc::new(move |snapshot| {
        reads.fetch_add(1, Ordering::SeqCst);
        read(snapshot)
    });
    let cycles = Arc::new(AtomicUsize::new(0));
    let observed_cycles = cycles.clone();
    fixture.io.reconciled = Arc::new(move |_| {
        observed_cycles.fetch_add(1, Ordering::SeqCst);
    });
    let released = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let release = ReleaseSettlement(released.clone());
    let (published, published_rx) = tokio::sync::oneshot::channel();
    let published = Arc::new(Mutex::new(Some(published)));
    let (completed, completed_rx) = tokio::sync::oneshot::channel();
    let completed = Arc::new(Mutex::new(Some(completed)));
    fixture.io.connection_published = Arc::new(move || {
        let Some(published) = published.lock().unwrap().take() else {
            return;
        };
        let _ = published.send(());
        let mut ready = released.0.lock().unwrap();
        while !*ready {
            ready = released.1.wait(ready).unwrap();
        }
        if let Some(completed) = completed.lock().unwrap().take() {
            let _ = completed.send(());
        }
    });
    let runtime = start_federation_runtime_with_io(fixture.app.clone(), fixture.io.clone());
    let original = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        loop {
            if let Some(handle) = runtime.peer(&fixture.id) {
                break handle.channel.clone();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let opening = tokio::spawn(async move {
        let stream = tokio::net::TcpStream::connect(("127.0.0.1", local_port))
            .await
            .unwrap();
        let mut control: FederationControlIo = Box::new(stream);
        federation_attest_central(&central, &mut control)
            .await
            .map(|_| ())
    });
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, published_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(fixture.app.inner.lock().unwrap().federation_durable);
    assert!(
        fixture
            .app
            .federation_peer_snapshot(&fixture.id)
            .unwrap()
            .transport_generation
            > original.transport_basis().transport_generation
    );
    fixture.fingerprint.store(2, Ordering::SeqCst);
    // The real handler deadline cancels only the caller. The owned completion
    // remains explicitly held AFTER durability, BEFORE shared-basis adoption.
    let cancelled = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, opening)
        .await
        .unwrap()
        .unwrap();
    assert!(cancelled.is_err());
    assert!(
        original.opening.try_lock().is_err(),
        "owned completion retains the installation mutex after caller cancellation"
    );
    assert!(original.basis.is_settling());
    let before = cycles.load(Ordering::SeqCst);
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        while cycles.load(Ordering::SeqCst) < before + 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let same_worker = runtime
        .peer(&fixture.id)
        .is_some_and(|handle| handle.channel.server_instance_id == original.server_instance_id);
    let retained_material = read_count.load(Ordering::SeqCst) == 1;
    drop(release);
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, completed_rx)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        while original.current_authority().is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    fixture.finish(runtime).await;
    assert!(
        same_worker && retained_material,
        "cancelled caller must not reset worker material/backoff while its exact completion is settling"
    );
}
