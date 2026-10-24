// Owns local reference commit visibility, real SQLite failure and reload tests.
// Does not supply successful reference/proof payloads or claim that a reload
// without a transport re-establishes current authority. Controlled acknowledgments
// require the production content target to read back from a real committed DB.
use super::*;

pub(super) struct HeldWriter {
    tx: mpsc::Sender<PersistRequest>,
    pub(super) fences: tokio::sync::mpsc::UnboundedReceiver<Box<PersistFence>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl HeldWriter {
    pub(super) async fn attach(fixture: &mut ReferenceFixture) -> Self {
        fixture.api.finish().await;
        fixture.proof.central.shutdown_persist_blocking();
        let (tx, rx) = mpsc::channel();
        fixture.proof.central.persist_tx = tx.clone();
        fixture.proof.central.persist_worker_alive.store(true, Ordering::SeqCst);
        let (send, fences) = tokio::sync::mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            while let Ok(request) = rx.recv() {
                if stopped.load(Ordering::SeqCst) { break; }
                match request {
                    PersistRequest::Fence(fence) => { let _ = send.send(fence); }
                    PersistRequest::Shutdown => break,
                    PersistRequest::Delta => {}
                }
            }
        });
        // Rebuild the ordinary API with this real persistence dispatch channel.
        // Existing transport proofs keep the same owned channel and inner state.
        fixture.api = DomainServer::start(app_router(fixture.proof.central.clone())).await;
        Self { tx, fences, stop, thread: Some(thread) }
    }

    pub(super) fn commit(app: &AppState, fence: &PersistFence) {
        let before = rusqlite::Connection::open(app.persistence_path.as_path()).unwrap();
        assert!(!fence.target.is_already_durable(&before).unwrap(),
            "held content fence must name an uncommitted reference, not unrelated durable state");
        drop(before);
        persist_state(app.persistence_path.as_path(), &app.inner.lock().unwrap()).unwrap();
        let committed = rusqlite::Connection::open(app.persistence_path.as_path()).unwrap();
        assert!(fence.target.is_already_durable(&committed).unwrap(),
            "a requested write alone is not a successful durability acknowledgement");
        fence.finish(Ok(()));
    }
}

impl Drop for HeldWriter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.tx.send(PersistRequest::Delta);
        if let Some(thread) = self.thread.take() { thread.join().unwrap(); }
    }
}

struct RejectWrites(rusqlite::Connection);

impl RejectWrites {
    fn install(app: &AppState) -> Self {
        let connection = rusqlite::Connection::open(app.persistence_path.as_path()).unwrap();
        connection.execute_batch(
            "CREATE TRIGGER reject_reference_insert BEFORE INSERT ON state_metadata_units
             BEGIN SELECT RAISE(ABORT,'reference fixture write failure'); END;
             CREATE TRIGGER reject_reference_update BEFORE UPDATE ON state_metadata_units
             BEGIN SELECT RAISE(ABORT,'reference fixture write failure'); END;"
        ).unwrap();
        Self(connection)
    }
}

impl Drop for RejectWrites {
    fn drop(&mut self) {
        let _ = self.0.execute_batch(
            "DROP TRIGGER IF EXISTS reject_reference_insert;
             DROP TRIGGER IF EXISTS reject_reference_update;"
        );
    }
}

pub(super) async fn list_at(fixture: &ReferenceFixture, port: u16) -> Value {
    let response = fixture.proof.client.get(format!(
        "http://127.0.0.1:{port}/api/sessions/{}/mailbox-references", fixture.caller,
    )).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "committed reference descriptor list");
    let body: Value = response.json().await.unwrap();
    assert!(body["references"].is_array());
    body["references"].clone()
}

pub(super) async fn reloaded_api(fixture: &ReferenceFixture) -> (DomainServer, AppState) {
    let path = fixture.proof.central.persistence_path.clone();
    let loaded = tokio::task::spawn_blocking(move || load_state(path.as_path()).unwrap().unwrap())
        .await.unwrap();
    assert!(loaded.federation.links.is_empty(), "reload never inherits a live channel");
    let app = AppState {
        inner: Arc::new(StateMutex::new(loaded)),
        persistence_path: fixture.proof.central.persistence_path.clone(),
        mailbox_store: fixture.proof.central.mailbox_store.clone(),
        ..test_app_state()
    };
    assert!(app.federation_peer_handles.lock().unwrap().is_empty());
    let server = DomainServer::start(app_router(app.clone())).await;
    (server, app)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_durability_pending_commit_is_not_visible_or_reloadable() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let mut fixture = ReferenceFixture::new(true).await;
    let mut writer = HeldWriter::attach(&mut fixture).await;
    let client = fixture.proof.client.clone();
    let url = fixture.url("mailbox-references");
    let target = fixture.canonical.clone();
    let mut request = tokio::spawn(async move {
        client.post(url).json(&json!({
            "participantId":target,"idempotencyKey":"held-reference",
        })).send().await.unwrap()
    });
    let fence = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        tokio::select! {
            result = &mut request => {
                let status = result.unwrap().status();
                assert_eq!(status, StatusCode::CREATED,
                    "production reference provisioner must exist before its commit contract is tested");
                panic!("reference response was published before its content commit fence");
            }
            fence = writer.fences.recv() => fence.expect("production reference content fence"),
        }
    }).await.expect("provision response or commit-fence phase");
    let early = fixture.proof.client.get(fixture.url("mailbox-references"))
        .send().await.unwrap();
    let early_status = early.status();
    let early_rows = if early_status == StatusCode::OK {
        Some(early.json::<Value>().await.unwrap()["references"].clone())
    } else { None };
    let (mut before_reload, _) = reloaded_api(&fixture).await;
    let before_rows = list_at(&fixture, before_reload.port).await;
    before_reload.finish().await;
    let no_response_before_commit = !request.is_finished();
    // This is a real full-snapshot write plus exact production fence read-back,
    // not an injected Ok or evidence about background delta-worker scheduling.
    HeldWriter::commit(&fixture.proof.central, &fence);
    let response = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, request)
        .await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created: Value = response.json().await.unwrap();
    let id = created["reference"]["id"].as_str().unwrap();
    assert!(no_response_before_commit);
    if let Some(rows) = early_rows {
        assert_eq!(rows, json!([]), "pending reference must not be listed");
    } else {
        assert!(early_status == StatusCode::LOCKED || early_status == StatusCode::SERVICE_UNAVAILABLE,
            "held persistence may refuse listing, but may not silently use another path");
    }
    assert_eq!(before_rows, json!([]), "reload before commit sees no pending reference");
    let after_rows = list_at(&fixture, fixture.api.port).await;
    assert_eq!(after_rows.as_array().unwrap().len(), 1);
    assert_eq!(after_rows[0]["id"], id);
    drop(writer);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_durability_reload_keeps_committed_handle_but_not_authority() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = ReferenceFixture::new(true).await;
    let reference = fixture.provision("committed-reload").await;
    let id = reference["id"].as_str().unwrap();
    let (mut server, reloaded) = reloaded_api(&fixture).await;
    let rows = list_at(&fixture, server.port).await;
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["id"], id);
    assert_eq!(rows[0]["participantId"], fixture.canonical);
    assert_eq!(rows[0]["nativeCreationId"], fixture.proof.native_creation);
    let response = fixture.proof.client.get(format!(
        "http://127.0.0.1:{}/api/sessions/{}/mailbox-references/{id}/resolve",
        server.port, fixture.caller,
    )).send().await.unwrap();
    let status = response.status();
    assert!(status == StatusCode::CONFLICT || status == StatusCode::SERVICE_UNAVAILABLE,
        "persisted descriptor without current transport must not be usable authority");
    assert!(reloaded.federation_peer_handles.lock().unwrap().is_empty());
    assert_eq!(fixture.mail_counts(), (0, 0, 0));
    server.finish().await;
    drop(server);
    drop(reloaded);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_durability_real_write_failure_is_visible_and_exact_retry_recovers() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = ReferenceFixture::new(true).await;
    let reject = RejectWrites::install(&fixture.proof.central);
    let before = fixture.proof.native_snapshot();
    let response = fixture.create_response("failed-reference-commit").await;
    assert_eq!(fixture.proof.native_snapshot(), before);
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR,
        "failed SQLite commit must visibly fail reference publication");
    let failure: Value = response.json().await.unwrap();
    assert!(failure["error"].as_str().is_some_and(|message| !message.is_empty()));
    let (mut server, _) = reloaded_api(&fixture).await;
    assert_eq!(list_at(&fixture, server.port).await, json!([]));
    server.finish().await;
    let pending = fixture.proof.client.get(fixture.url("mailbox-references"))
        .send().await.unwrap();
    if pending.status() == StatusCode::OK {
        assert_eq!(pending.json::<Value>().await.unwrap()["references"], json!([]));
    } else {
        assert!(pending.status() == StatusCode::LOCKED
            || pending.status() == StatusCode::SERVICE_UNAVAILABLE);
    }
    assert_eq!(fixture.mail_counts(), (0, 0, 0));
    drop(reject); // remove only the two fixture-owned SQLite triggers
    let recovered = fixture.provision("failed-reference-commit").await;
    let rows = list_at(&fixture, fixture.api.port).await;
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["id"], recovered["id"]);
    assert_eq!(fixture.proof.native_snapshot(), before);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_authority_link_lost_at_commit_cannot_publish_after_fence() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let mut fixture = ReferenceFixture::new(true).await;
    let mut writer = HeldWriter::attach(&mut fixture).await;
    let client = fixture.proof.client.clone();
    let url = fixture.url("mailbox-references");
    let target = fixture.canonical.clone();
    let mut request = tokio::spawn(async move {
        client.post(url).json(&json!({
            "participantId":target,"idempotencyKey":"link-lost-at-reference-commit",
        })).send().await.unwrap()
    });
    let fence = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        tokio::select! {
            response = &mut request => {
                assert_eq!(response.unwrap().status(), StatusCode::CREATED,
                    "production provisioner must exist before its commit-race contract");
                panic!("reference published without its durability fence");
            }
            fence = writer.fences.recv() => fence.expect("reference commit fence"),
        }
    }).await.unwrap();
    let native_before = fixture.proof.native_snapshot();
    // Actual runtime retirement is write-free at C, so it can occur while the
    // reference's content transaction is held. No forged persistence success.
    fixture.proof.channel.invalidate();
    assert!(fixture.proof.channel.current_lease().is_err());
    HeldWriter::commit(&fixture.proof.central, &fence);
    let response = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, request)
        .await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT,
        "durable content alone cannot publish under a retired captured link");
    let visible = fixture.proof.client.get(fixture.url("mailbox-references"))
        .send().await.unwrap();
    if visible.status() == StatusCode::OK {
        assert_eq!(visible.json::<Value>().await.unwrap()["references"], json!([]));
    } else {
        assert!(visible.status() == StatusCode::CONFLICT || visible.status() == StatusCode::LOCKED
            || visible.status() == StatusCode::SERVICE_UNAVAILABLE);
    }
    assert_eq!(fixture.proof.native_snapshot(), native_before);
    assert_eq!(fixture.mail_counts(), (0, 0, 0));
    let (mut server, reloaded) = reloaded_api(&fixture).await;
    assert_eq!(list_at(&fixture, server.port).await, json!([]),
        "committed content without a publication decision must stay hidden after reload");
    server.finish().await;
    drop(server);
    drop(reloaded);
    drop(writer);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_durability_logical_deadline_keeps_late_commit_hidden() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let mut fixture = ReferenceFixture::new(true).await;
    let mut writer = HeldWriter::attach(&mut fixture).await;
    let client = fixture.proof.client.clone();
    let url = fixture.url("mailbox-references");
    let target = fixture.canonical.clone();
    let request = tokio::spawn(async move {
        client.post(url).json(&json!({
            "participantId":target,"idempotencyKey":"logical-deadline-reference",
        })).send().await.unwrap()
    });
    let fence = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, writer.fences.recv())
        .await.unwrap().expect("reference content fence");
    // Logical time, not a sleep or an inflated production deadline. A real
    // later SQL commit may not retroactively turn the expired fence into PASS.
    fixture.proof.channel.durability_clock.advance(Duration::from_secs(5));
    let response = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, request)
        .await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    HeldWriter::commit(&fixture.proof.central, &fence);
    let (mut server, reloaded) = reloaded_api(&fixture).await;
    assert_eq!(list_at(&fixture, server.port).await, json!([]),
        "a late content commit is not a durable publication decision");
    server.finish().await;
    drop(server);
    drop(reloaded);
    drop(writer);
    fixture.finish().await;
}
