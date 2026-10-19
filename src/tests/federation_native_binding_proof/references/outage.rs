// Owns loopback API/proof availability and retired-link recovery witnesses.
// A stopped listener is not a stopped host. Only the explicit hello/open test
// below claims a fresh incarnation; no supervisor, SSH or O1 probe is simulated.
use super::*;
use std::sync::atomic::AtomicUsize;

#[derive(Clone, Default)]
struct ProofTraffic(Arc<AtomicUsize>);

impl ProofTraffic {
    fn layer(&self, router: Router) -> Router {
        let successes = self.0.clone();
        router.layer(axum::middleware::from_fn(move |
            request: axum::extract::Request, next: axum::middleware::Next,
        | {
            let successes = successes.clone();
            async move {
                let proof = request.uri().path() == PROOF_PATH;
                let response = next.run(request).await;
                if proof && response.status() == StatusCode::OK {
                    successes.fetch_add(1, Ordering::SeqCst);
                }
                // Observe the real authenticated response, never synthesize it.
                response
            }
        }))
    }

    fn count(&self) -> usize { self.0.load(Ordering::SeqCst) }
}

fn satellite_mail_counts(fixture: &ReferenceFixture) -> (u64, u64, u64) {
    let connection = fixture.proof.satellite.mailbox_store.connection().unwrap();
    (
        connection.query_row("SELECT COUNT(*) FROM mailboxes", [], |r| r.get(0)).unwrap(),
        connection.query_row("SELECT COUNT(*) FROM mailbox_participants", [], |r| r.get(0)).unwrap(),
        connection.query_row("SELECT COUNT(*) FROM mailbox_messages", [], |r| r.get(0)).unwrap(),
    )
}

fn native_payload(fixture: &ReferenceFixture) -> Value {
    let mut snapshot = fixture.proof.native_snapshot();
    // Reconnecting legitimately changes registry bookkeeping, not native state.
    snapshot.as_object_mut().unwrap().remove("federationVersion");
    snapshot
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_outage_c_api_loss_fails_visibly_and_restoration_revalidates() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let traffic = ProofTraffic::default();
    let mut fixture = ReferenceFixture::with_router(true, |router| traffic.layer(router)).await;
    let reference = fixture.provision("before-c-api-outage").await;
    let id = reference["id"].as_str().unwrap();
    let original = fixture.send(id, "existing-intent").await.unwrap();
    assert!(traffic.count() > 0, "a real proof must precede the outage");
    let native_before = fixture.proof.native_snapshot();
    let central_before = fixture.mail_counts();
    let satellite_before = satellite_mail_counts(&fixture);
    assert_eq!(central_before, (1, 2, 1));

    // Close and await this fixture's C API listener, not the host or domain link.
    fixture.api.finish().await;
    assert!(fixture.proof.client.get(fixture.url(
        &format!("mailbox-references/{id}/resolve"),
    )).send().await.is_err(), "the C API is actually unreachable");
    assert!(fixture.send(id, "existing-intent").await.is_err(),
        "even an old duplicate may not be answered from local fallback");
    assert!(fixture.send(id, "outage-new-intent").await.is_err());
    assert!(fixture.send(&fixture.canonical, "outage-canonical").await.is_err());
    assert_eq!(fixture.mail_counts(), central_before);
    assert_eq!(satellite_mail_counts(&fixture), satellite_before);
    assert_eq!(fixture.proof.native_snapshot(), native_before);

    // Real new listener, same app/channel. This is availability restoration,
    // not a process restart or a new L1 incarnation.
    fixture.api = DomainServer::start(app_router(fixture.proof.central.clone())).await;
    let placement = fixture.proof.central.inner.lock().unwrap()
        .federation.placements["native-proof"].clone();
    fixture.proof.central.ensure_federation_placement_current(&placement).unwrap();
    let before_replay = traffic.count();
    let replay = fixture.send(id, "existing-intent").await.unwrap();
    assert!(traffic.count() > before_replay,
        "the restored facade must obtain current proof, not reuse the descriptor as authority");
    assert_eq!(replay["messageId"], original["messageId"]);
    assert_eq!(replay["mailboxId"], original["mailboxId"]);
    assert_eq!(replay["duplicate"], true);
    assert_eq!(replay["sessionId"], fixture.canonical);
    assert_eq!(fixture.mail_counts(), central_before);
    let recovered = fixture.send(id, "outage-new-intent").await.unwrap();
    assert_ne!(recovered["messageId"], original["messageId"]);
    let duplicate = fixture.send(id, "outage-new-intent").await.unwrap();
    assert_eq!(duplicate["messageId"], recovered["messageId"]);
    assert_eq!(duplicate["duplicate"], true);
    assert_eq!(fixture.mail_counts(), (1, 2, 2));
    assert_eq!(satellite_mail_counts(&fixture), satellite_before);
    assert_eq!(fixture.proof.native_snapshot(), native_before);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_outage_s_proof_listener_loss_cannot_use_cached_reference() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let mut fixture = ReferenceFixture::new(true).await;
    let reference = fixture.provision("before-proof-listener-loss").await;
    let id = reference["id"].as_str().unwrap();
    fixture.send(id, "before-proof-loss").await.unwrap();
    let native_before = native_payload(&fixture);
    let central_before = fixture.mail_counts();
    let satellite_before = satellite_mail_counts(&fixture);
    fixture.proof.server.finish().await;

    // C's ordinary API remains alive; only S's owned proof listener is gone.
    let response = fixture.resolve(id).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE,
        "unavailable proof must be a visible transport refusal, not a cached success");
    assert!(response.json::<Value>().await.unwrap()["error"].as_str()
        .is_some_and(|message| !message.is_empty()));
    assert!(fixture.send(id, "before-proof-loss").await.is_err());
    assert!(fixture.send(id, "after-proof-loss").await.is_err());
    assert_eq!(fixture.mail_counts(), central_before);
    assert_eq!(satellite_mail_counts(&fixture), satellite_before);
    assert_eq!(native_payload(&fixture), native_before);
    // The fixture does not assert whether a consumer retires the failed link.
    // Reconnect is the separate fresh-handshake witness below.
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_outage_fresh_handshake_does_not_revive_stale_placement() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = ReferenceFixture::new(true).await;
    let reference = fixture.provision("before-link-retirement").await;
    let id = reference["id"].as_str().unwrap();
    fixture.send(id, "old-link-intent").await.unwrap();
    let native_before = native_payload(&fixture);
    let satellite_before = satellite_mail_counts(&fixture);
    let old_lease = fixture.proof.channel.current_lease().unwrap();
    let old_placement = fixture.proof.central.inner.lock().unwrap()
        .federation.placements["native-proof"].clone();
    fixture.proof.channel.invalidate();
    assert!(fixture.proof.channel.current_lease().is_err());
    assert!(old_lease.with_incarnation(|| Ok(())).is_err());

    // The actual hello/challenge/open/record-connected exchange creates a new
    // lease over the existing owned router/channel; no activate() or lease copy.
    let fresh = fixture.proof.rpc.open_link().await.unwrap();
    assert_ne!(fresh.incarnation, old_lease.incarnation);
    fixture.proof.rpc.health(&fresh).await.unwrap();
    assert!(fixture.proof.central.ensure_federation_placement_current(&old_placement).is_err(),
        "health of a new link is not current placement authority");
    let response = fixture.resolve(id).await;
    assert_eq!(response.status(), StatusCode::CONFLICT,
        "a descriptor from the old link cannot revive its stale placement");
    assert!(fixture.send(id, "old-link-intent").await.is_err());
    assert!(fixture.send(id, "new-key-old-link-reference").await.is_err());
    assert_eq!(fixture.mail_counts(), (1, 2, 1));
    assert_eq!(satellite_mail_counts(&fixture), satellite_before);
    assert_eq!(native_payload(&fixture), native_before);
    assert_eq!(fixture.proof.central.inner.lock().unwrap()
        .federation.placements["native-proof"], old_placement,
        "reconnect cannot retarget or replace the original admission intent");
    fixture.finish().await;
}
