// Owns deterministic pre-listener work and full-inventory publication regressions.
// Does not certify live-host latency or exhaustive runtime recovery behavior.
// Uses the real boot loader and disposable primary stores, never a live store.

use super::*;

fn completed_history_startup_work(rows: usize, largest_prompt_bytes: usize) -> (usize, usize) {
    let root = TestTempRoot::create("delegation-startup-budget");
    let path = root.path().join("termal.sqlite");
    persist_state(&path, &StateInner::new()).unwrap();
    let mut connection = rusqlite::Connection::open(&path).unwrap();
    let tx = connection.transaction().unwrap();
    for number in 0..rows {
        let record: DelegationRecord = serde_json::from_value(json!({
            "id": format!("delegation-history-{number}"),
            "parentSessionId": "historical-parent",
            "childSessionId": format!("historical-child-{number}"),
            "mode": "explorer", "status": "completed", "title": "Completed history",
            "prompt": "x".repeat(if number + 1 == rows { largest_prompt_bytes } else { 128 }),
            "cwd": root.path().to_string_lossy(), "agent": Agent::Codex,
            "writePolicy": { "kind": "readOnly" },
            "createdAt": "2026-01-01T00:00:00Z", "completedAt": "2026-01-01T00:01:00Z"
        }))
        .expect("history fixture must use the current record schema");
        tx.execute(
            "INSERT INTO delegations(id,value_json) VALUES(?1,?2)",
            rusqlite::params![record.id, serde_json::to_string(&record).unwrap()],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    take_delegation_startup_payload_work();
    let (loaded, boot_connection) = load_state_for_boot(&path).unwrap();
    let work = take_delegation_startup_payload_work();
    assert!(loaded.is_some());
    let durable_count: usize = connection
        .query_row("SELECT count(*) FROM delegations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(durable_count, rows, "startup must preserve historical rows");
    drop(boot_connection);
    drop(connection);
    work
}

#[test]
fn delegation_startup_budget_does_not_materialize_unreferenced_completed_history_before_bind() {
    let small = completed_history_startup_work(1, 128);
    let many = completed_history_startup_work(64, 128);
    let large = completed_history_startup_work(64, 2 * 1024 * 1024);
    assert_eq!(
        (small.0, many.0, large.0),
        (0, 0, 0),
        "completed history must not increase pre-bind delegation payload rows"
    );
    assert_eq!(
        (small.1, many.1, large.1),
        (0, 0, 0),
        "completed history must not increase pre-bind serialized payload bytes"
    );
}

struct PendingInventoryFixture {
    state: AppState,
    parent_id: String,
    child_id: String,
    _root: TestTempRoot,
}

impl PendingInventoryFixture {
    fn new() -> Self {
        let root = TestTempRoot::create("delegation-startup-inventory");
        let path = root.path().join("termal.sqlite");
        let mut state = AppState::new_with_paths(
            root.path().to_string_lossy().into_owned(),
            path.clone(),
            root.path().join("orchestrators.json"),
        )
        .unwrap();
        state.agent_runtime_spawning_enabled = false;
        let (parent_id, child_id) = {
            let mut inner = state.inner.lock().unwrap();
            let cwd = root.path().to_string_lossy().into_owned();
            let parent = inner.create_session(
                Agent::Codex,
                Some("Historical parent".to_owned()),
                cwd.clone(),
                None,
                None,
            );
            let child = inner.create_session(
                Agent::Claude,
                Some("Historical child".to_owned()),
                cwd,
                None,
                None,
            );
            let child_index = inner.find_session_index(&child.session.id).unwrap();
            let message_id = inner.next_message_id();
            push_message_on_record(
                inner.session_mut_by_index(child_index).unwrap(),
                Message::Text {
                    attachments: Vec::new(),
                    id: message_id,
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: "Immutable historical output".to_owned(),
                    expanded_text: None,
                    source: None,
                },
            );
            (parent.session.id, child.session.id)
        };
        // Deterministically isolate the source-writing tests from the normal
        // asynchronous writer. Its full-snapshot fallback is also exercised.
        state.shutdown_persist_blocking();
        let record: DelegationRecord = serde_json::from_value(json!({
            "id":"delegation-history-one", "parentSessionId":parent_id,
            "childSessionId":child_id, "mode":"explorer", "status":"completed",
            "title":"Retained history", "prompt":"private original payload",
            "cwd":root.path().to_string_lossy(), "agent":Agent::Codex,
            "writePolicy":{"kind":"readOnly"},
            "createdAt":"2026-01-01T00:00:00Z", "completedAt":"2026-01-01T00:01:00Z",
            "result":{"delegationId":"delegation-history-one", "childSessionId":child_id,
                "status":"completed", "summary":"Historical result retained"}
        }))
        .unwrap();
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute(
                "INSERT INTO delegations(id,value_json) VALUES(?1,?2)",
                rusqlite::params![record.id, serde_json::to_string(&record).unwrap()],
            )
            .unwrap();
        let (inner, connection) = load_state_for_boot(&path).unwrap();
        *state.inner.lock().unwrap() = inner.unwrap();
        drop(connection);
        Self {
            state,
            parent_id,
            child_id,
            _root: root,
        }
    }
}

impl Drop for PendingInventoryFixture {
    fn drop(&mut self) {
        self.state.shutdown_persist_blocking();
        self.state.shutdown_state_broadcast_blocking();
    }
}

#[test]
fn delegation_startup_budget_full_publication_and_sql_boundaries_leave_state_available() {
    let fixture = PendingInventoryFixture::new();
    let state = &fixture.state;
    let mut phases = Vec::new();
    state
        .load_delegation_inventory_observed(|phase| {
            if phase == "publication_state_acquired" {
                assert!(state.inner.try_lock_now().is_none());
                phases.push(phase.to_owned());
                return;
            }
            let inner = state
                .inner
                .try_lock_now()
                .expect("SQL, decoding and preparation must be off-lock");
            assert!(!inner.delegation_inventory_complete());
            assert!(
                inner.delegations.is_empty(),
                "a partial inventory must never be exposed"
            );
            assert_eq!(
                inner.require_delegation_inventory().unwrap_err().status,
                StatusCode::SERVICE_UNAVAILABLE
            );
            phases.push(phase.to_owned());
        })
        .unwrap();
    assert_eq!(phases, ["before_sql", "source_checked", "connection_opened", "after_decode", "before_publish", "publication_reserved", "publication_state_acquired"]);
    let inner = state.inner.lock().unwrap();
    assert!(inner.delegation_inventory_complete());
    assert_eq!(inner.delegations.len(), 1);
    assert_eq!(inner.delegations[0].prompt, "private original payload");
    assert!(inner
        .find_delegation_index_by_child_session_id(&fixture.child_id)
        .is_some());
}

#[test]
fn delegation_startup_budget_catalog_waits_for_atomic_records_and_child_index() {
    let fixture = PendingInventoryFixture::new();
    let host = FederationHostIdentity {
        host_id: Uuid::new_v4().to_string(),
        host_generation: 1,
    };
    let mut checked_publication_lock = false;
    fixture.state.load_delegation_inventory_observed(|phase| {
        if phase == "publication_state_acquired" {
            assert!(fixture.state.inner.try_lock_now().is_none(),
                "catalog and record installation share the publication state lock");
            checked_publication_lock = true;
            return;
        }
        let inner = fixture.state.inner.try_lock_now().unwrap();
        let child = inner.find_session_index(&fixture.child_id).unwrap();
        assert!(inner.sessions[child].session.parent_delegation_id.is_none());
        // Empty records and a cold empty index are not proof that roots are
        // independent. Both production catalog and Describe selectors must hold.
        assert!(inner.delegation_catalog_children.lock().unwrap().ready(0));
        assert_eq!(federation_native_catalog_locked(
            &inner, None, host.clone(), host.clone(), String::new(),
        ).err().unwrap().status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(federation_mailbox_root_locked(&inner, &fixture.child_id)
            .err().unwrap().status, StatusCode::SERVICE_UNAVAILABLE);
    }).unwrap();
    assert!(checked_publication_lock);
    let inner = fixture.state.inner.lock().unwrap();
    {
        let index = inner.delegation_catalog_children.lock().unwrap();
        assert!(index.ready(inner.delegations.len()));
        assert_eq!(index.progress, 0, "publication installs an already built index");
        assert!(index.children.contains_key(&fixture.child_id));
    }
    let catalog = federation_native_catalog_locked(
        &inner, None, host.clone(), host, String::new(),
    ).unwrap();
    assert!(catalog.next.is_none());
    assert!(catalog.roots.iter().any(|root| root.session_id == fixture.parent_id));
    assert!(!catalog.roots.iter().any(|root| root.session_id == fixture.child_id));
}

#[test]
fn delegation_startup_budget_failed_inventory_cannot_publish_an_empty_native_catalog() {
    let fixture = PendingInventoryFixture::new();
    let connection = rusqlite::Connection::open(fixture.state.persistence_path.as_path()).unwrap();
    connection.execute("DROP TABLE delegations", []).unwrap();
    assert!(fixture.state.load_delegation_inventory_after_listen().is_err());
    let inner = fixture.state.inner.lock().unwrap();
    let host = FederationHostIdentity {
        host_id: Uuid::new_v4().to_string(),
        host_generation: 1,
    };
    assert!(inner.delegations.is_empty());
    assert_eq!(federation_native_catalog_locked(
        &inner, None, host.clone(), host, String::new(),
    ).err().unwrap().status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(federation_mailbox_root_locked(&inner, &fixture.parent_id)
        .err().unwrap().status, StatusCode::SERVICE_UNAVAILABLE);
}

#[test]
fn delegation_startup_budget_write_guards_hold_unknown_scopes_without_file_effects() {
    let fixture = PendingInventoryFixture::new();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let target = fixture._root.path().join("held-write.txt");
    fs::write(&target, "unchanged").unwrap();
    for failed in [false, true] {
        if failed {
            fixture.state.inner.lock().unwrap().delegation_inventory =
                DelegationInventoryState::Failed { reason: "loadFailed" };
        }
        for session in [None, Some(fixture.parent_id.as_str()), Some(fixture.child_id.as_str())] {
            assert_eq!(fixture.state.ensure_read_only_delegation_allows_session_write_action(
                session, "file writes",
            ).unwrap_err().status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(fixture.state.ensure_read_only_delegation_allows_write_action(
                session, None, Some(fixture._root.path().to_str().unwrap()), "terminal commands",
            ).unwrap_err().status, StatusCode::SERVICE_UNAVAILABLE);
        }
        runtime.block_on(async {
            let response = app_router(fixture.state.clone()).oneshot(
                Request::builder().method("PUT").uri("/api/file")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&json!({
                        "sessionId": fixture.parent_id, "path": target,
                        "content": "must not be written", "overwrite": true,
                    })).unwrap())).unwrap(),
            ).await.unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        });
        assert_eq!(fs::read_to_string(&target).unwrap(), "unchanged");
    }
}

#[test]
fn delegation_startup_budget_unrelated_writes_preserve_unloaded_history_and_restart() {
    let fixture = PendingInventoryFixture::new();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner.revision += 1;
        fixture.state.persist_internal_locked(&inner).unwrap();
    }
    fixture
        .state
        .load_delegation_inventory_observed(|phase| {
            if phase == "after_decode" {
                let mut inner = fixture.state.inner.lock().unwrap();
                inner.revision += 1;
                fixture.state.persist_internal_locked(&inner).unwrap();
            }
        })
        .unwrap();
    let loaded = load_state(&fixture.state.persistence_path)
        .unwrap()
        .unwrap();
    assert_eq!(loaded.delegations.len(), 1);
    assert_eq!(loaded.delegations[0].prompt, "private original payload");
}

#[test]
fn delegation_startup_budget_conflicting_durable_writer_refuses_stale_inventory() {
    let fixture = PendingInventoryFixture::new();
    let error = fixture.state.load_delegation_inventory_observed(|phase| {
        if phase == "after_decode" {
            let connection = rusqlite::Connection::open(fixture.state.persistence_path.as_path()).unwrap();
            connection.execute("UPDATE delegations SET value_json = json_set(value_json,'$.prompt','new durable payload')", []).unwrap();
        }
    }).err().unwrap();
    assert!(
        error.to_string().contains("changed while loading"),
        "{error:#}"
    );
    let inner = fixture.state.inner.lock().unwrap();
    assert!(matches!(
        inner.delegation_inventory,
        DelegationInventoryState::Failed { .. }
    ));
    assert!(inner.delegations.is_empty());
    fixture.state.persist_internal_locked(&inner).unwrap();
    drop(inner);
    let loaded = load_state(&fixture.state.persistence_path)
        .unwrap()
        .unwrap();
    assert_eq!(loaded.delegations[0].prompt, "new durable payload");
}

#[test]
fn delegation_startup_budget_late_durable_changes_cannot_cross_publication() {
    for sql in [
        "UPDATE delegations SET value_json=json_set(value_json,'$.prompt','late durable payload')",
        "INSERT INTO delegations(id,value_json) SELECT 'late-row',json_set(value_json,'$.id','late-row') FROM delegations",
        "DELETE FROM delegations",
    ] {
        let fixture = PendingInventoryFixture::new();
        let error = fixture.state.load_delegation_inventory_observed(|phase| {
            if phase == "before_publish" {
                rusqlite::Connection::open(fixture.state.persistence_path.as_path())
                    .unwrap().execute(sql, []).unwrap();
            }
        }).err().expect("a late durable change must prevent stale publication");
        assert!(error.to_string().contains("changed while loading"), "{error:#}");
        let inner = fixture.state.inner.lock().unwrap();
        assert!(!inner.delegation_inventory_complete());
        assert!(inner.delegations.is_empty());
        let connection = rusqlite::Connection::open(fixture.state.persistence_path.as_path()).unwrap();
        let before: Vec<(String, String)> = connection.prepare(
            "SELECT id,value_json FROM delegations ORDER BY rowid",
        ).unwrap().query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap().collect::<rusqlite::Result<_>>().unwrap();
        fixture.state.persist_internal_locked(&inner).unwrap();
        let after: Vec<(String, String)> = connection.prepare(
            "SELECT id,value_json FROM delegations ORDER BY rowid",
        ).unwrap().query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(before, after, "a failed inventory cannot undo the late writer");
    }
}

#[test]
fn delegation_startup_budget_unreadable_sql_row_remains_durable_and_unavailable() {
    let fixture = PendingInventoryFixture::new();
    let connection = rusqlite::Connection::open(fixture.state.persistence_path.as_path()).unwrap();
    connection.execute("INSERT INTO delegations(id,value_json) VALUES('unreadable-row',X'FF00')", []).unwrap();
    let error = fixture.state.load_delegation_inventory_after_listen().unwrap_err();
    assert!(error.to_string().contains("complete delegation inventory"), "{error:#}");
    let inner = fixture.state.inner.lock().unwrap();
    assert!(!inner.delegation_inventory_complete());
    assert!(inner.delegations.is_empty());
    assert_eq!(federation_mailbox_root_locked(&inner, &fixture.child_id)
        .err().unwrap().status, StatusCode::SERVICE_UNAVAILABLE);
    fixture.state.persist_internal_locked(&inner).unwrap();
    let bytes: Vec<u8> = connection.query_row(
        "SELECT value_json FROM delegations WHERE id='unreadable-row'", [], |row| row.get(0),
    ).unwrap();
    assert_eq!(bytes, [255, 0]);
    assert_eq!(connection.query_row("SELECT count(*) FROM delegations", [],
        |row| row.get::<_, usize>(0)).unwrap(), 2);
}

#[test]
fn delegation_startup_budget_connection_must_match_the_captured_physical_source() {
    let fixture = PendingInventoryFixture::new();
    let path = fixture.state.persistence_path.as_path();
    let source = delegation_inventory_file_identity(path).unwrap();
    let original = rusqlite::Connection::open(path).unwrap();
    verify_delegation_inventory_source(&original, path, &source).unwrap();
    let memory = rusqlite::Connection::open_in_memory().unwrap();
    assert!(verify_delegation_inventory_source(&memory, path, &source).is_err(),
        "unverifiable connection identity must not fall back to the pathname");
    let other = path.with_extension("other-source");
    let connection = rusqlite::Connection::open(&other).unwrap();
    connection.execute_batch("CREATE TABLE delegations(id TEXT PRIMARY KEY,value_json TEXT)").unwrap();
    let error = verify_delegation_inventory_source(&connection, path, &source)
        .expect_err("a valid pathname cannot certify a connection to a different file");
    assert!(error.to_string().contains("source"), "{error:#}");
    assert_eq!(delegation_inventory_file_identity(path).unwrap(), source);
}

#[cfg(unix)]
#[test]
fn delegation_startup_budget_open_swap_restore_cannot_publish_another_database() {
    let fixture = PendingInventoryFixture::new();
    let path = fixture.state.persistence_path.as_path();
    let original = path.with_extension("retained-source");
    let replacement = path.with_extension("replacement-source");
    let opened = path.with_extension("opened-source");
    {
        let connection = rusqlite::Connection::open(&replacement).unwrap();
        connection.execute_batch("CREATE TABLE delegations(id TEXT PRIMARY KEY,value_json TEXT)").unwrap();
    }
    let source = delegation_inventory_file_identity(path).unwrap();
    let error = fixture.state.load_delegation_inventory_observed(|phase| {
        if phase == "source_checked" {
            fs::rename(path, &original).unwrap();
            fs::rename(&replacement, path).unwrap();
        } else if phase == "connection_opened" {
            fs::rename(path, &opened).unwrap();
            fs::rename(&original, path).unwrap();
        }
    }).err().expect("the restored pathname cannot authorize the replacement connection");
    assert!(error.to_string().contains("source"), "{error:#}");
    assert_eq!(delegation_inventory_file_identity(path).unwrap(), source);
    let inner = fixture.state.inner.lock().unwrap();
    assert!(!inner.delegation_inventory_complete());
    assert!(inner.delegations.is_empty());
    assert_eq!(federation_mailbox_root_locked(&inner, &fixture.child_id)
        .err().unwrap().status, StatusCode::SERVICE_UNAVAILABLE);
    fixture.state.persist_internal_locked(&inner).unwrap();
    let reloaded = load_state(path).unwrap().unwrap();
    assert_eq!(reloaded.delegations.len(), 1);
    assert_eq!(reloaded.delegations[0].prompt, "private original payload");
}

#[test]
fn delegation_startup_budget_source_replacement_at_publication_is_not_adopted() {
    let fixture = PendingInventoryFixture::new();
    let path = fixture.state.persistence_path.as_path();
    let replacement = path.with_extension("late-replacement");
    let retained = path.with_extension("late-original");
    fs::copy(path, &replacement).unwrap();
    let mut replaced = false;
    let result = fixture.state.load_delegation_inventory_observed(|phase| {
        if phase == "before_publish" {
            match fs::rename(path, &retained) {
                Ok(()) => {
                    fs::rename(&replacement, path).unwrap();
                    replaced = true;
                }
                Err(error) => {
                    // Windows SQLite may exclude rename while the validated
                    // connection remains open. That is an enforced fence too.
                    assert!(cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32)), "{error}");
                }
            }
        }
    });
    if replaced {
        let error = result.err().expect("the replacement cannot receive stale records");
        assert!(error.to_string().contains("source was replaced"), "{error:#}");
        assert!(!fixture.state.inner.lock().unwrap().delegation_inventory_complete());
    } else {
        result.unwrap();
        assert!(fixture.state.inner.lock().unwrap().delegation_inventory_complete());
    }
}

#[test]
fn delegation_startup_budget_publication_releases_writer_before_waiting_for_state() {
    let fixture = PendingInventoryFixture::new();
    let mut worker = None;
    let mut writer_admitted = None;
    fixture.state.load_delegation_inventory_observed(|phase| {
        if phase == "publication_reserved" {
            let (held, held_rx) = mpsc::channel();
            let (admitted, admitted_rx) = mpsc::channel();
            let state = fixture.state.clone();
            worker = Some(std::thread::spawn(move || {
                let mut inner = state.inner.lock().unwrap();
                held.send(()).unwrap();
                inner.revision += 1;
                state.persist_internal_locked(&inner).unwrap();
                admitted.send(()).unwrap();
            }));
            held_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            writer_admitted = Some(admitted_rx);
        }
    }).unwrap();
    writer_admitted.unwrap().recv_timeout(Duration::from_secs(3)).unwrap();
    worker.unwrap().join().unwrap();
    assert!(fixture.state.inner.lock().unwrap().delegation_inventory_complete());
}

#[test]
fn delegation_startup_budget_sql_writer_is_excluded_through_installation() {
    let fixture = PendingInventoryFixture::new();
    let mut checked = Vec::new();
    fixture.state.load_delegation_inventory_observed(|phase| {
        if matches!(phase, "publication_reserved" | "publication_state_acquired") {
            assert_eq!(fixture.state.inner.try_lock_now().is_some(), phase == "publication_reserved");
            let connection = rusqlite::Connection::open(fixture.state.persistence_path.as_path()).unwrap();
            connection.busy_timeout(Duration::ZERO).unwrap();
            let error = connection.execute("DELETE FROM delegations", []).unwrap_err();
            assert!(matches!(error, rusqlite::Error::SqliteFailure(ref failure, _)
                if failure.code == rusqlite::ErrorCode::DatabaseBusy), "{error}");
            checked.push(phase.to_owned());
        }
    }).unwrap();
    assert_eq!(checked, ["publication_reserved", "publication_state_acquired"]);
    assert!(fixture.state.inner.lock().unwrap().delegation_inventory_complete());
    // The worker's reservation ends at install; it is not a retained global
    // write hold and does not block ordinary persistence after recovery.
    let connection = rusqlite::Connection::open(fixture.state.persistence_path.as_path()).unwrap();
    assert_eq!(connection.execute("DELETE FROM delegations WHERE id='no-such-record'", []).unwrap(), 0);
}

#[test]
fn delegation_startup_budget_state_wait_does_not_exhaust_sql_admission_budget() {
    let fixture = PendingInventoryFixture::new();
    let mut holder = None;
    let mut release = None;
    fixture.state.load_delegation_inventory_observed(|phase| {
        if phase == "publication_reserved" {
            let state = fixture.state.clone();
            let (held, held_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel::<()>();
            holder = Some(std::thread::spawn(move || {
                let _inner = state.inner.lock().unwrap();
                let started = std::time::Instant::now();
                held.send(()).unwrap();
                // The receive timeout establishes an actual state-lock hold
                // beyond the SQL budget, without relying on scheduling sleeps.
                assert!(matches!(release_rx.recv_timeout(
                    SQLITE_BUSY_TIMEOUT + Duration::from_millis(50),
                ), Err(mpsc::RecvTimeoutError::Timeout)));
                assert!(started.elapsed() > SQLITE_BUSY_TIMEOUT);
            }));
            held_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            release = Some(release_tx);
        }
    }).expect("ordinary state contention must not make inventory permanently unavailable");
    holder.unwrap().join().unwrap();
    drop(release);
    let inner = fixture.state.inner.lock().unwrap();
    assert!(inner.delegation_inventory_complete());
    assert_eq!(inner.delegations.len(), 1);
}

#[test]
fn delegation_startup_budget_shutdown_keeps_durable_inventory_unavailable_and_intact() {
    let fixture = PendingInventoryFixture::new();
    fixture.state.trigger_shutdown_signal();
    let error = fixture
        .state
        .load_delegation_inventory_after_listen()
        .err()
        .unwrap();
    assert!(error.to_string().contains("shutdown"), "{error:#}");
    assert!(!fixture
        .state
        .inner
        .lock()
        .unwrap()
        .delegation_inventory_complete());
    assert_eq!(
        load_state(&fixture.state.persistence_path)
            .unwrap()
            .unwrap()
            .delegations
            .len(),
        1
    );
}

#[test]
fn delegation_startup_budget_pending_http_is_typed_while_health_serves() {
    let fixture = PendingInventoryFixture::new();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let router = app_router(fixture.state.clone());
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/sessions/old-parent/delegations")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["code"], "delegation_inventory_starting");
    });
}

#[test]
fn delegation_startup_budget_real_listener_serves_before_the_owned_loader_runs() {
    let fixture = PendingInventoryFixture::new();
    take_delegation_startup_payload_work();
    let mut state = AppState::new_before_listen_with_paths(
        fixture._root.path().to_string_lossy().into_owned(),
        fixture.state.persistence_path.as_ref().clone(),
        fixture._root.path().join("orchestrators.json"),
    )
    .unwrap();
    state.agent_runtime_spawning_enabled = false;
    assert_eq!(take_delegation_startup_payload_work(), (0, 0));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        state.set_local_http_base_url(format!("http://{address}"));
        let router = app_router(state.clone());
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        let (entered, arrived) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let worker = state
            .start_post_listen_boot_observed(move |phase| {
                if phase == "before_sql" {
                    entered.send(()).unwrap();
                    let _ = released.recv();
                }
            })
            .unwrap();
        tokio::task::spawn_blocking(move || arrived.recv_timeout(Duration::from_secs(3)).unwrap())
            .await
            .unwrap();
        assert!(
            state.load_delegation_inventory_after_listen().is_err(),
            "only one inventory loader may own boot"
        );
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let health = client
            .get(format!("http://{address}/api/health"))
            .send()
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);
        let pending = client
            .get(format!("http://{address}/api/state"))
            .send()
            .await
            .unwrap();
        assert_eq!(pending.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            pending.json::<Value>().await.unwrap()["code"],
            "delegation_inventory_starting"
        );
        // Invalid independent Git input reaches its ordinary validation (400),
        // instead of being refused by the inventory gate (503).
        let independent = client
            .post(format!("http://{address}/api/git/diff"))
            .header("content-type", "application/json")
            .body("{")
            .send()
            .await
            .unwrap();
        assert_eq!(independent.status(), StatusCode::BAD_REQUEST);
        release.send(()).unwrap();
        tokio::task::spawn_blocking(move || worker.join().unwrap())
            .await
            .unwrap();
        let ready = client
            .get(format!("http://{address}/api/state"))
            .send()
            .await
            .unwrap();
        assert_eq!(ready.status(), StatusCode::OK);
        let ready: Value = ready.json().await.unwrap();
        assert!(ready["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == fixture.parent_id));
        let child = ready["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == fixture.child_id)
            .unwrap();
        assert_eq!(child["parentDelegationId"], "delegation-history-one");
        stop.send(()).unwrap();
        server.await.unwrap();
    });
    state.shutdown_persist_blocking();
    state.shutdown_state_broadcast_blocking();
}

#[test]
fn delegation_startup_budget_history_results_child_guards_and_waits_do_not_see_absence() {
    let fixture = PendingInventoryFixture::new();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner.delegation_waits.push(DelegationWaitRecord {
            id: "wait-history".to_owned(),
            parent_session_id: fixture.parent_id.clone(),
            delegation_ids: vec!["delegation-history-one".to_owned()],
            mode: DelegationWaitMode::All,
            created_at: stamp_now(),
            title: None,
        });
        assert_eq!(
            federation_mailbox_root_locked(&inner, &fixture.child_id)
                .err()
                .unwrap()
                .status,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(
            engram_parent_session_authority_locked(&inner, &fixture.child_id, false, None).is_err()
        );
        let child_index = inner.find_session_index(&fixture.child_id).unwrap();
        assert!(inner.remove_session_at(child_index).is_none());
        assert!(!refresh_delegation_waits_locked(&mut inner).did_mutate());
        assert_eq!(inner.delegation_waits.len(), 1);
    }
    assert_eq!(
        fixture
            .state
            .get_delegation_result(&fixture.parent_id, "delegation-history-one")
            .err()
            .unwrap()
            .status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(fixture
        .state
        .reconcile_delegation_waits_after_boot()
        .is_err());
    assert_eq!(
        fixture.state.schedule_engram_root_reclamation(true),
        "delegation inventory is unavailable; reclamation is deferred"
    );
    fixture
        .state
        .load_delegation_inventory_after_listen()
        .unwrap();
    {
        let inner = fixture.state.inner.lock().unwrap();
        let child = inner.find_session_index(&fixture.child_id).unwrap();
        assert_eq!(
            inner.sessions[child]
                .session
                .parent_delegation_id
                .as_deref(),
            Some("delegation-history-one")
        );
        assert!(federation_mailbox_root_locked(&inner, &fixture.child_id).is_err());
        assert!(federation_mailbox_root_locked(&inner, &fixture.parent_id).is_ok());
    }
    let result = fixture
        .state
        .get_delegation_result(&fixture.parent_id, "delegation-history-one")
        .unwrap();
    assert_eq!(result.result.summary, "Historical result retained");
    let output = fixture
        .state
        .get_delegation_result_output(&fixture.parent_id, "delegation-history-one", 0, 4096)
        .unwrap();
    assert!(
        output.output.contains("Immutable historical output"),
        "{}",
        output.output
    );
    assert!(output.complete);
}

#[test]
fn delegation_startup_budget_quarantine_survives_complete_publication_and_full_write() {
    let fixture = PendingInventoryFixture::new();
    let connection = rusqlite::Connection::open(fixture.state.persistence_path.as_path()).unwrap();
    connection
        .execute(
            "INSERT INTO delegations(id,value_json) VALUES('invalid-json','{')",
            [],
        )
        .unwrap();
    connection.execute("INSERT INTO delegations(id,value_json) SELECT 'mismatched-id',value_json FROM delegations WHERE id='delegation-history-one'",[]).unwrap();
    fixture
        .state
        .load_delegation_inventory_after_listen()
        .unwrap();
    let inner = fixture.state.inner.lock().unwrap();
    assert!(inner.delegation_inventory_complete());
    assert_eq!(inner.delegations.len(), 1);
    assert_eq!(
        inner.quarantined_persisted_delegation_ids,
        BTreeSet::from(["invalid-json".to_owned(), "mismatched-id".to_owned()])
    );
    fixture.state.persist_internal_locked(&inner).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM delegations", [], |row| row
                .get::<_, usize>(0))
            .unwrap(),
        3
    );
}

#[test]
fn delegation_startup_budget_failed_sql_is_typed_unavailable_not_empty_ready() {
    let fixture = PendingInventoryFixture::new();
    let connection = rusqlite::Connection::open(fixture.state.persistence_path.as_path()).unwrap();
    connection.execute("DROP TABLE delegations", []).unwrap();
    assert!(fixture
        .state
        .load_delegation_inventory_after_listen()
        .is_err());
    {
        let inner = fixture.state.inner.lock().unwrap();
        assert!(matches!(
            inner.delegation_inventory,
            DelegationInventoryState::Failed {
                reason: "loadFailed"
            }
        ));
        assert!(inner.delegations.is_empty());
    }
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let response = app_router(fixture.state.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/state")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["code"], "delegation_inventory_unavailable");
    });
}

#[test]
fn delegation_startup_budget_changed_owner_cannot_install_a_stale_result() {
    let fixture = PendingInventoryFixture::new();
    let new_token = Uuid::new_v4().to_string();
    let error = fixture
        .state
        .load_delegation_inventory_observed(|phase| {
            if phase == "before_publish" {
                let mut inner = fixture.state.inner.lock().unwrap();
                if let DelegationInventoryState::Loading {
                    token,
                    worker_started,
                    ..
                } = &mut inner.delegation_inventory
                {
                    *token = new_token.clone();
                    *worker_started = false;
                }
            }
        })
        .err()
        .unwrap();
    assert!(error.to_string().contains("owner changed"));
    let inner = fixture.state.inner.lock().unwrap();
    assert!(inner.delegations.is_empty());
    assert!(
        matches!(&inner.delegation_inventory,DelegationInventoryState::Loading{token,worker_started:false,..} if token==&new_token)
    );
}

#[test]
fn delegation_startup_budget_same_contents_in_a_replaced_file_cannot_gain_authority() {
    let fixture = PendingInventoryFixture::new();
    let path = fixture.state.persistence_path.as_path();
    let replacement = path.with_extension("replacement");
    let retained = path.with_extension("original");
    fs::copy(path, &replacement).unwrap();
    fs::rename(path, &retained).unwrap();
    fs::rename(&replacement, path).unwrap();
    let error = fixture
        .state
        .load_delegation_inventory_after_listen()
        .unwrap_err();
    assert!(
        error.to_string().contains("source was replaced"),
        "{error:#}"
    );
    let inner = fixture.state.inner.lock().unwrap();
    assert!(!inner.delegation_inventory_complete());
    assert!(inner.delegations.is_empty());
    let connection = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM delegations", [], |row| row
                .get::<_, usize>(0))
            .unwrap(),
        1
    );
}

#[test]
fn delegation_startup_budget_required_interrupted_recovery_uses_the_full_inventory() {
    let fixture = PendingInventoryFixture::new();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let child = inner.find_session_index(&fixture.child_id).unwrap();
        inner.session_mut_by_index(child).unwrap().session.status = SessionStatus::Error;
        fixture.state.persist_internal_locked(&inner).unwrap();
    }
    let connection = rusqlite::Connection::open(fixture.state.persistence_path.as_path()).unwrap();
    connection.execute("UPDATE delegations SET value_json=json_set(value_json,'$.status','running','$.result',NULL,'$.completedAt',NULL)",[]).unwrap();
    fixture
        .state
        .load_delegation_inventory_after_listen()
        .unwrap();
    let inner = fixture.state.inner.lock().unwrap();
    assert!(inner.delegation_inventory_complete());
    assert_eq!(inner.delegations[0].status, DelegationStatus::Failed);
    assert!(inner.delegations[0].result.is_some());
    assert!(inner.running_read_only_delegations.is_empty());
    drop(inner);
    assert_eq!(
        load_state(&fixture.state.persistence_path)
            .unwrap()
            .unwrap()
            .delegations[0]
            .status,
        DelegationStatus::Failed
    );
}

#[test]
fn delegation_startup_budget_pending_snapshot_and_delta_cannot_delete_unknown_ancestry() {
    let fixture = PendingInventoryFixture::new();
    let connection = rusqlite::Connection::open(fixture.state.persistence_path.as_path()).unwrap();
    let encoded: String = connection
        .query_row("SELECT value_json FROM delegations", [], |row| row.get(0))
        .unwrap();
    let mut unadmitted: DelegationRecord = serde_json::from_str(&encoded).unwrap();
    unadmitted.prompt = "unadmitted default".to_owned();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner.delegations.push(unadmitted);
        inner.mark_delegation_mutated(0);
        inner.record_removed_delegation("delegation-history-one".to_owned());
        inner.record_removed_session(fixture.child_id.clone());
        let child = inner.find_session_index(&fixture.child_id).unwrap();
        inner.session_mut_by_index(child).unwrap().hidden = true;
        let plan = inner.collect_persist_delta_plan(0);
        assert!(plan.changed_delegations.is_empty());
        assert!(plan.removed_delegation_ids.is_empty());
        assert!(plan.removed_session_ids.is_empty());
        assert!(inner
            .removed_delegation_ids
            .contains_key("delegation-history-one"));
        assert!(inner.removed_session_ids.contains(&fixture.child_id));
        fixture.state.persist_internal_locked(&inner).unwrap();
    }
    assert!(fixture
        .state
        .load_delegation_inventory_after_listen()
        .is_err());
    let inner = fixture.state.inner.lock().unwrap();
    assert!(matches!(
        inner.delegation_inventory,
        DelegationInventoryState::Failed {
            reason: "publicationConflict"
        }
    ));
    fixture.state.persist_internal_locked(&inner).unwrap();
    let encoded: String = connection
        .query_row("SELECT value_json FROM delegations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<DelegationRecord>(&encoded)
            .unwrap()
            .prompt,
        "private original payload"
    );
    let count: usize = connection
        .query_row(
            "SELECT count(*) FROM sessions WHERE id=?1",
            [&fixture.child_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}
