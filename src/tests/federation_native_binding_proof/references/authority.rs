// Owns current-authority response races and immutable reference-intent tests.
// Does not fabricate proof responses, copy leases, alter production code, or
// open replacement transports. A barrier delays the real domain response.
use super::*;

struct ProofGateState {
    armed: AtomicBool,
    release: tokio::sync::Semaphore,
    seen: tokio::sync::mpsc::UnboundedSender<StatusCode>,
}

struct ProofGate {
    state: Arc<ProofGateState>,
    seen: tokio::sync::mpsc::UnboundedReceiver<StatusCode>,
}

impl ProofGate {
    fn new() -> Self {
        let (send, seen) = tokio::sync::mpsc::unbounded_channel();
        Self { state: Arc::new(ProofGateState {
            armed: AtomicBool::new(false),
            release: tokio::sync::Semaphore::new(0),
            seen: send,
        }), seen }
    }

    fn layer(&self, router: Router) -> Router {
        let state = self.state.clone();
        router.layer(axum::middleware::from_fn(move |
            request: axum::extract::Request, next: axum::middleware::Next,
        | {
            let state = state.clone();
            async move {
                let hold = request.uri().path() == PROOF_PATH
                    && state.armed.swap(false, Ordering::SeqCst);
                let response = next.run(request).await;
                if hold {
                    // Observe, but never replace, a production response after
                    // its handler/authentication guards have completed.
                    let _ = state.seen.send(response.status());
                    if let Ok(permit) = state.release.acquire().await { permit.forget(); }
                }
                response
            }
        }))
    }

    fn arm(&self) { self.state.armed.store(true, Ordering::SeqCst); }
    fn release(&self) { self.state.release.add_permits(1); }

    async fn reached(&mut self, request: &mut tokio::task::JoinHandle<reqwest::Response>,
        expected_status: StatusCode) {
        let status = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
            tokio::select! {
                response = request => {
                    let status = response.unwrap().status();
                    assert_eq!(status, expected_status,
                        "production provision/resolve must exist before its proof-race contract");
                    panic!("reference path completed without current authenticated native proof");
                }
                status = self.seen.recv() => status.expect("proof response phase"),
            }
        }).await.expect("proof response or ordinary API completion");
        if status != StatusCode::OK { self.release(); }
        assert_eq!(status, StatusCode::OK,
            "real authenticated proof producer must succeed before the race cut");
    }
}

impl Drop for ProofGate {
    fn drop(&mut self) { self.release(); }
}

#[derive(Clone, Copy, Debug)]
enum Change { Enrollment, Configuration, ConfigurationAba, Link, CentralEpoch, Placement }

fn change_authority(fixture: &ReferenceFixture, change: Change) {
    let app = &fixture.proof.central;
    match change {
        Change::Enrollment | Change::Configuration | Change::ConfigurationAba => {
            let record = app.inner.lock().unwrap().federation.peers[
                &fixture.proof.binding.identity.host_id
            ].clone();
            let old = app.federation_peer_snapshot(&record.identity.host_id).unwrap();
            let mut next = record.clone();
            if !matches!(change, Change::Enrollment) {
                next.endpoint.ssh_host = "changed-config.invalid".to_owned();
            }
            app.enroll_federation_peer(next).unwrap();
            if matches!(change, Change::ConfigurationAba) {
                app.enroll_federation_peer(record).unwrap();
            }
            let current = app.federation_peer_snapshot(&old.peer.host_id).unwrap();
            assert!(current.enrollment_generation > old.enrollment_generation);
            assert!(current.transport_generation > old.transport_generation);
            if matches!(change, Change::ConfigurationAba) {
                assert_eq!(current.endpoint, old.endpoint, "same strings do not restore authority");
            }
        }
        Change::Link => {
            fixture.proof.channel.invalidate();
            assert!(fixture.proof.channel.current_lease().is_err());
        }
        Change::CentralEpoch => {
            app.reset_federation_continuity(&fixture.proof.binding.central).unwrap();
            assert_ne!(app.inner.lock().unwrap().federation.local.as_ref(),
                Some(&fixture.proof.binding.central));
        }
        Change::Placement => {
            let placement = app.inner.lock().unwrap().federation.placements["native-proof"].clone();
            app.reconcile_federation_placement(&placement, true).unwrap();
            assert!(app.ensure_federation_placement_current(&placement).is_err());
        }
    }
}

fn stale_status(status: StatusCode) -> bool {
    // A generic transport timeout must not masquerade as a passing race test.
    // These deliberate, completed authority transitions use M1's stale 409.
    status == StatusCode::CONFLICT
}

async fn no_visible_reference(fixture: &ReferenceFixture) {
    let response = fixture.proof.client.get(fixture.url("mailbox-references"))
        .send().await.unwrap();
    if response.status() == StatusCode::OK {
        assert_eq!(response.json::<Value>().await.unwrap()["references"], json!([]));
    } else {
        assert!(stale_status(response.status()) || response.status() == StatusCode::LOCKED
            || response.status() == StatusCode::SERVICE_UNAVAILABLE);
    }
}

async fn cannot_publish(change: Change) {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let mut gate = ProofGate::new();
    let fixture = ReferenceFixture::with_router(true, |router| gate.layer(router)).await;
    let before = fixture.proof.native_snapshot();
    let client = fixture.proof.client.clone();
    let url = fixture.url("mailbox-references");
    let target = fixture.canonical.clone();
    gate.arm();
    let mut request = tokio::spawn(async move {
        client.post(url).json(&json!({
            "participantId":target,"idempotencyKey":"captured-before-authority-change",
        })).send().await.unwrap()
    });
    gate.reached(&mut request, StatusCode::CREATED).await;
    // Only real local authority operations run here. The wire payload remains
    // the old coherent proof; its consumer must reject it after this cut.
    change_authority(&fixture, change);
    gate.release();
    let response = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, request)
        .await.unwrap().unwrap();
    assert!(stale_status(response.status()), "{change:?}: stale proof may not publish a handle");
    assert!(response.json::<Value>().await.unwrap()["error"].as_str().is_some());
    no_visible_reference(&fixture).await;
    assert_eq!(fixture.mail_counts(), (0, 0, 0));
    assert_eq!(fixture.proof.native_snapshot(), before);
    fixture.finish().await;
}

async fn cannot_use(change: Change) {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let mut gate = ProofGate::new();
    let fixture = ReferenceFixture::with_router(true, |router| gate.layer(router)).await;
    let reference = fixture.provision("before-authority-change").await;
    let id = reference["id"].as_str().unwrap();
    fixture.send(id, "original-send-intent").await.unwrap();
    let before = fixture.proof.native_snapshot();
    let client = fixture.proof.client.clone();
    let url = fixture.url(&format!("mailbox-references/{id}/resolve"));
    gate.arm();
    let mut request = tokio::spawn(async move { client.get(url).send().await.unwrap() });
    gate.reached(&mut request, StatusCode::OK).await;
    change_authority(&fixture, change);
    gate.release();
    let response = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, request)
        .await.unwrap().unwrap();
    assert!(stale_status(response.status()), "{change:?}: cached proof may not authorize use");
    assert!(fixture.send(id, "original-send-intent").await.is_err());
    assert!(fixture.send(id, "new-key-stale-reference").await.is_err());
    assert_eq!(fixture.mail_counts(), (1, 2, 1));
    assert_eq!(fixture.proof.native_snapshot(), before);
    fixture.finish().await;
}

macro_rules! authority_cases {
    ($publish:ident, $use_reference:ident, $change:ident) => {
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn $publish() { cannot_publish(Change::$change).await; }
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn $use_reference() { cannot_use(Change::$change).await; }
    };
}

authority_cases!(federation_reference_authority_enrollment_publish,
    federation_reference_authority_enrollment_use, Enrollment);
authority_cases!(federation_reference_authority_configuration_publish,
    federation_reference_authority_configuration_use, Configuration);
authority_cases!(federation_reference_authority_configuration_aba_publish,
    federation_reference_authority_configuration_aba_use, ConfigurationAba);
authority_cases!(federation_reference_authority_link_publish,
    federation_reference_authority_link_use, Link);
authority_cases!(federation_reference_authority_central_epoch_publish,
    federation_reference_authority_central_epoch_use, CentralEpoch);
authority_cases!(federation_reference_authority_placement_publish,
    federation_reference_authority_placement_use, Placement);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_authority_old_provision_intent_cannot_select_successor_principal() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = ReferenceFixture::new(true).await;
    let reference = fixture.provision("immutable-provision-intent").await;
    let id = reference["id"].as_str().unwrap();
    fixture.send(id, "immutable-send-intent").await.unwrap();
    let before = fixture.proof.native_snapshot();
    change_authority(&fixture, Change::Placement);
    let peer = fixture.proof.central.federation_peer_snapshot(
        &fixture.proof.binding.identity.host_id).unwrap();
    let next = fixture.proof.central.reserve_federation_placement(
        &peer, "successor-placement".to_owned(), fixture.proof.native.clone(), 0, 0,
    ).unwrap();
    let successor = FederationMailboxBinding {
        central: peer.local,
        identity: next.identity,
        placement_generation: next.placement_generation,
    };
    assert_ne!(successor.participant_key(), fixture.canonical);
    assert!(successor.placement_generation > fixture.proof.binding.placement_generation);
    let response = fixture.proof.client.post(fixture.url("mailbox-references"))
        .json(&json!({"participantId":successor.participant_key(),
            "idempotencyKey":"immutable-provision-intent"}))
        .send().await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT,
        "exact old intent may not be rewritten to a newly allocated principal");
    assert!(fixture.send(id, "immutable-send-intent").await.is_err());
    assert_eq!(fixture.mail_counts(), (1, 2, 1));
    assert_eq!(fixture.proof.native_snapshot(), before,
        "reserving a successor at C must not silently install or adopt it at S");
    fixture.finish().await;
}
