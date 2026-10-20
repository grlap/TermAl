// Real same-database publication markers, failure/replay and writer controls.
// Fault injection loses an ACK only after a real SQL commit, never its proof.
use super::*;
use super::durability::{HeldWriter, list_at, reloaded_api};

#[test]
fn federation_reference_publication_diagnostic_preserves_forced_sqlite_failure() {
    let error = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY_SNAPSHOT),
        Some("forced publication write failure".to_owned()),
    );
    let original = format!("{error:?}");
    // Exercise the actual error observation, including its libtest-captured
    // eprintln!, without making a database fail or changing the returned error.
    let result: Result<(), _> = reference_publication_observe!("publish.insert_marker", Err(error));
    let error = result.unwrap_err();
    assert_eq!(format!("{error:?}"), original);
    let message = reference_publication_diagnostic_message("publish.insert_marker", &error);
    assert!(message.contains("REFERENCE_PUBLICATION_DIAGNOSTIC stage=publish.insert_marker"), "{message}");
    assert!(message.contains("sqlite=DatabaseBusy/extended=517"), "{message}");
    assert!(message.contains(&format!("error={original}")), "{message}");
    assert!(message.contains("forced publication write failure"), "{message}");

    let wrapped = anyhow::Error::new(error).context("forced wrapper context");
    let message = reference_publication_diagnostic_message("publish.insert_marker", &wrapped);
    assert!(message.contains("sqlite=DatabaseBusy/extended=517"), "{message}");
    assert!(message.contains("forced wrapper context"), "{message}");
    assert!(message.contains("forced publication write failure"), "{message}");
}

fn prepared(fixture: &ReferenceFixture) -> FederationMailboxReference {
    let inner = fixture.proof.central.inner.lock().unwrap();
    assert_eq!(inner.federation.mailbox_references.len(), 1);
    inner.federation.mailbox_references.values().next().unwrap().clone()
}

fn receipt(fixture: &ReferenceFixture, row: &FederationMailboxReference) -> Option<FederationReferencePublication> {
    let connection = open_sqlite_state_read_connection(fixture.proof.central.persistence_path.as_path()).unwrap();
    committed_federation_reference_publication(&connection, row).unwrap()
}

struct RejectPublication(rusqlite::Connection);

impl RejectPublication {
    fn install(fixture: &ReferenceFixture) -> Self {
        let connection = rusqlite::Connection::open(fixture.proof.central.persistence_path.as_path()).unwrap();
        connection.execute_batch(
            "CREATE TRIGGER reject_reference_publication BEFORE INSERT ON meta
             WHEN NEW.key GLOB 'federation_reference_publication_v1:*'
             BEGIN SELECT RAISE(ABORT,'publication fixture write failure'); END;"
        ).unwrap();
        Self(connection)
    }
}

impl Drop for RejectPublication {
    fn drop(&mut self) {
        self.0.execute_batch("DROP TRIGGER IF EXISTS reject_reference_publication;").unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_publication_marker_failure_stays_hidden_and_same_key_recovers() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = ReferenceFixture::new(true).await;
    let reject = RejectPublication::install(&fixture);
    let response = fixture.create_response("marker-failure").await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let row = prepared(&fixture);
    assert!(!row.is_published());
    assert!(receipt(&fixture, &row).is_none());
    let (mut server, reloaded) = reloaded_api(&fixture).await;
    assert_eq!(list_at(&fixture, server.port).await, json!([]));
    server.finish().await;
    drop(server);
    drop(reloaded);
    drop(reject);
    let recovered = fixture.provision("marker-failure").await;
    assert_eq!(recovered["id"], row.id);
    assert!(receipt(&fixture, &row).is_some());
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_publication_lost_commit_ack_and_offline_replay_keep_exact_descriptor() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = ReferenceFixture::new(true).await;
    fixture.proof.channel.reference_publication_ack_lost.store(true, Ordering::SeqCst);
    let reference = fixture.provision("lost-marker-ack").await;
    assert!(!fixture.proof.channel.reference_publication_ack_lost.load(Ordering::SeqCst));
    let row = prepared(&fixture);
    assert!(row.is_published());
    assert_eq!(receipt(&fixture, &row), Some(row.publication_marker().unwrap()));
    let (mut server, reloaded) = reloaded_api(&fixture).await;
    assert!(reloaded.federation_peer_handles.lock().unwrap().is_empty());
    let response = fixture.proof.client.post(format!(
        "http://127.0.0.1:{}/api/sessions/{}/mailbox-references", server.port, fixture.caller,
    )).json(&json!({"participantId":fixture.canonical,"idempotencyKey":"lost-marker-ack"}))
        .send().await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.json::<Value>().await.unwrap()["reference"], reference);
    let response = fixture.proof.client.get(format!(
        "http://127.0.0.1:{}/api/sessions/{}/mailbox-references/{}/resolve",
        server.port, fixture.caller, row.id,
    )).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(fixture.mail_counts(), (0, 0, 0));
    server.finish().await;
    drop(server);
    drop(reloaded);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_publication_delta_preserves_marker_but_cannot_bless_changed_content() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = ReferenceFixture::new(true).await;
    let reference = fixture.provision("metadata-after-publication").await;
    let row = prepared(&fixture);
    let marker = receipt(&fixture, &row);
    assert!(marker.is_some());
    // The production delta writer receives metadata from the test-only
    // collector, modelling a snapshot serialized before publication. This does
    // not exercise production collector scheduling; the receipt is not carried.
    let delta = {
        let mut inner = fixture.proof.central.inner.lock().unwrap();
        let mut delta = inner.collect_persist_delta(0);
        delta.metadata.federation.mailbox_references.get_mut(&row.id).unwrap().publication = None;
        delta
    };
    let mut cache = SqlitePersistConnectionCache::new();
    persist_delta_via_cache(&mut cache, fixture.proof.central.persistence_path.as_path(), &delta).unwrap();
    drop(cache);
    assert_eq!(receipt(&fixture, &row), marker);
    let (mut server, reloaded) = reloaded_api(&fixture).await;
    assert_eq!(list_at(&fixture, server.port).await[0], reference);
    server.finish().await;
    drop(server);
    drop(reloaded);
    // A later internally valid descriptor mutation cannot inherit its old
    // marker. The old receipt remains intact; no "repair" overwrites it.
    fixture.proof.central.mutate_federation(|domain, _| {
        domain.mailbox_references.get_mut(&row.id).unwrap().native_creation_id = Uuid::new_v4().to_string();
        Ok(())
    }).unwrap();
    assert!(!prepared(&fixture).is_published());
    let (mut server, reloaded) = reloaded_api(&fixture).await;
    assert_eq!(list_at(&fixture, server.port).await, json!([]));
    let connection = open_sqlite_state_read_connection(fixture.proof.central.persistence_path.as_path()).unwrap();
    assert_eq!(read_federation_reference_publication(&connection, &row.id).unwrap(), marker);
    drop(connection);
    assert_eq!(fixture.create_response("metadata-after-publication").await.status(), StatusCode::CONFLICT);
    server.finish().await;
    drop(server);
    drop(reloaded);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_publication_concurrent_same_key_has_one_descriptor_and_marker() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let mut fixture = ReferenceFixture::new(true).await;
    let mut writer = HeldWriter::attach(&mut fixture).await;
    let start = || {
        let client = fixture.proof.client.clone();
        let url = fixture.url("mailbox-references");
        let target = fixture.canonical.clone();
        tokio::spawn(async move {
            client.post(url).json(&json!({"participantId":target,"idempotencyKey":"same-key-race"}))
                .send().await.unwrap()
        })
    };
    let first = start();
    let fence = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, writer.fences.recv())
        .await.unwrap().unwrap();
    let second = start();
    let retry_fence = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, writer.fences.recv())
        .await.unwrap().unwrap();
    assert!(!first.is_finished() && !second.is_finished());
    HeldWriter::commit(&fixture.proof.central, &fence);
    let connection = rusqlite::Connection::open(fixture.proof.central.persistence_path.as_path()).unwrap();
    assert!(retry_fence.target.is_already_durable(&connection).unwrap());
    drop(connection);
    retry_fence.finish(Ok(()));
    let a = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, first).await.unwrap().unwrap();
    let b = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, second).await.unwrap().unwrap();
    let mut descriptors = Vec::new();
    for (contender, response) in [a, b].into_iter().enumerate() {
        let response = if response.status() == StatusCode::SERVICE_UNAVAILABLE {
            let error: Value = response.json().await.unwrap();
            assert!(error["error"].as_str().unwrap().starts_with("REFERENCE_PUBLICATION_BUSY:"),
                "contender={contender}; status=503; body={error}");
            // One explicit protocol replay after both contenders have settled,
            // not a test rerun or retry-until-green loop.
            fixture.create_response("same-key-race").await
        } else { response };
        let status = response.status();
        let body = response.text().await.unwrap();
        assert_eq!(status, StatusCode::CREATED,
            "contender={contender}; status={status}; body={body}; underlying stage/error/SQLite code is in REFERENCE_PUBLICATION_DIAGNOSTIC stderr");
        descriptors.push(serde_json::from_str::<Value>(&body).unwrap()["reference"].clone());
    }
    assert_eq!(descriptors[0], descriptors[1]);
    let row = prepared(&fixture);
    assert_eq!(receipt(&fixture, &row), Some(row.publication_marker().unwrap()));
    assert_eq!(list_at(&fixture, fixture.api.port).await.as_array().unwrap().len(), 1);
    assert_eq!(fixture.mail_counts(), (0, 0, 0));
    drop(writer);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_publication_other_key_settling_is_retryable_not_stale() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let mut fixture = ReferenceFixture::new(true).await;
    let published = fixture.provision("already-published").await;
    let context = fixture.proof.central.capture_federation_reference(&fixture.caller, &fixture.canonical).unwrap();
    let mut writer = HeldWriter::attach(&mut fixture).await;
    let client = fixture.proof.client.clone();
    let url = fixture.url("mailbox-references");
    let target = fixture.canonical.clone();
    let first = tokio::spawn(async move {
        client.post(url).json(&json!({"participantId":target,"idempotencyKey":"held-other-key"}))
            .send().await.unwrap()
    });
    let fence = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, writer.fences.recv())
        .await.unwrap().unwrap();
    assert!(!fixture.proof.central.inner.lock().unwrap().federation_durable);
    let response = fixture.create_response("retry-this-identical-key").await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let error: Value = response.json().await.unwrap();
    assert!(error["error"].as_str().unwrap().starts_with("REFERENCE_PUBLICATION_BUSY:"));
    let response = fixture.resolve(published["id"].as_str().unwrap()).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let error = context.with_current(&fixture.proof.central, |_| Ok(())).unwrap_err();
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    let error = context.with_publication_authority::<()>(&fixture.proof.central, |_| {
        panic!("settling state must not enter the publication cut")
    }).unwrap_err();
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(fixture.proof.central.inner.lock().unwrap().federation.mailbox_references.len(), 2,
        "refused contender must not allocate or orphan a prepared intent");
    HeldWriter::commit(&fixture.proof.central, &fence);
    let response = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, first).await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(fixture.resolve(published["id"].as_str().unwrap()).await.status(), StatusCode::OK);
    assert!(fixture.proof.central.capture_federation_reference(&fixture.caller, &fixture.canonical).is_ok());
    assert_eq!(fixture.mail_counts(), (0, 0, 0));
    drop(writer);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_publication_busy_state_returns_without_writer_lock_inversion() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = ReferenceFixture::new(true).await;
    fixture.provision("publication-admission").await;
    let row = prepared(&fixture);
    let context = fixture.proof.central.capture_federation_reference(&fixture.caller, &fixture.canonical).unwrap();
    let app = fixture.proof.central.clone();
    let held = fixture.proof.central.inner.lock().unwrap();
    let task = tokio::task::spawn_blocking(move || app.publish_federation_reference(&context, &row));
    let error = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, task).await.unwrap().unwrap().unwrap_err();
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(error.message.starts_with("REFERENCE_PUBLICATION_BUSY:"));
    drop(held);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_publication_recreated_owner_cannot_replay_old_namespace() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = ReferenceFixture::new(true).await;
    let reference = fixture.provision("old-owner-intent").await;
    let old_creation = prepared(&fixture).owner_creation;
    {
        let mut inner = fixture.proof.central.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.caller).unwrap();
        inner.remove_session_at(index).unwrap();
        inner.next_session_number = fixture.caller.trim_start_matches("session-").parse().unwrap();
    }
    assert_eq!(test_session_id(&fixture.proof.central, Agent::Codex), fixture.caller);
    {
        let inner = fixture.proof.central.inner.lock().unwrap();
        let root = federation_mailbox_root_locked(&inner, &fixture.caller).unwrap();
        assert_ne!(root.federation_native_creation_id, old_creation);
        assert!(root.federation_mailbox_binding.is_none());
    }
    assert_eq!(fixture.create_response("old-owner-intent").await.status(), StatusCode::CONFLICT);
    assert_eq!(fixture.resolve(reference["id"].as_str().unwrap()).await.status(), StatusCode::NOT_FOUND);
    assert_eq!(list_at(&fixture, fixture.api.port).await, json!([]));
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_publication_unknown_or_malformed_marker_never_backfills_or_overwrites() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = ReferenceFixture::new(true).await;
    fixture.provision("unsupported-marker").await;
    let row = prepared(&fixture);
    let mut unsupported = row.publication_marker().unwrap();
    unsupported.schema_version = 2;
    let connection = rusqlite::Connection::open(fixture.proof.central.persistence_path.as_path()).unwrap();
    connection.execute("UPDATE meta SET value=?1 WHERE key=?2", rusqlite::params![
        serde_json::to_string(&unsupported).unwrap(), federation_reference_publication_key(&row.id),
    ]).unwrap();
    let (mut server, reloaded) = reloaded_api(&fixture).await;
    assert_eq!(list_at(&fixture, server.port).await, json!([]));
    assert_eq!(fixture.create_response("unsupported-marker").await.status(), StatusCode::CONFLICT);
    assert_eq!(read_federation_reference_publication(&connection, &row.id).unwrap(), Some(unsupported));
    server.finish().await;
    drop(server);
    drop(reloaded);
    connection.execute("UPDATE meta SET value='not-json' WHERE key=?1",
        [federation_reference_publication_key(&row.id)]).unwrap();
    assert_eq!(fixture.create_response("unsupported-marker").await.status(), StatusCode::CONFLICT);
    assert!(load_state(fixture.proof.central.persistence_path.as_path()).is_err(),
        "malformed publication evidence must fail closed at boot");
    let unchanged: String = connection.query_row("SELECT value FROM meta WHERE key=?1",
        [federation_reference_publication_key(&row.id)], |row| row.get(0)).unwrap();
    assert_eq!(unchanged, "not-json");
    drop(connection);
    fixture.finish().await;
}
