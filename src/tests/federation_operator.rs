// Owns HTTP operator configuration, durable reload and local-behavior controls.
// Does not enroll satellites or provision transport. New coverage mounted below
// federation_registry.rs, sharing its isolated domain and peer fixtures.
use super::*;

pub(super) async fn operator_request(
    state: &AppState,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: StatusCode,
) -> Value {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |value| {
            Body::from(serde_json::to_vec(&value).unwrap())
        }))
        .unwrap();
    let response = request_response(&app_router(state.clone()), request).await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        status,
        expected,
        "{method} {path}: {}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).expect("operator response should be JSON")
}

pub(super) async fn operator_view(state: &AppState) -> Value {
    operator_request(state, "GET", "/api/federation", None, StatusCode::OK).await
}

pub(super) async fn operator_configure(state: &AppState, body: Value, expected: StatusCode) -> Value {
    operator_request(
        state,
        "POST",
        "/api/federation/settings",
        Some(body),
        expected,
    )
    .await
}

fn central_settings(local: &Value, enabled: bool) -> Value {
    json!({"enabled": enabled, "role": "central", "central": local})
}

pub(super) fn current_image(state: &AppState) -> Value {
    federation_persist_image(&PersistedState::metadata_from_inner(
        &state.inner.lock().unwrap(),
    ))
}

pub(super) struct OperatorPersistShutdown<'a>(pub(super) &'a AppState);

impl Drop for OperatorPersistShutdown<'_> {
    fn drop(&mut self) {
        self.0.shutdown_persist_blocking();
    }
}

#[tokio::test]
async fn federation_operator_default_read_identifies_this_host_without_enabling_or_writing() {
    let state = domain_state();
    let before = current_image(&state);
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    let stored_before = read_federation_persist_image(&connection).unwrap();
    let view = operator_view(&state).await;
    assert_eq!(view["enabled"], false);
    assert_eq!(view["role"], "standalone");
    assert!(view["central"].is_null());
    assert_eq!(view["local"], before["domain"]["local"]);
    assert!(Uuid::parse_str(view["local"]["hostId"].as_str().unwrap()).is_ok());
    assert!(view["local"]["hostGeneration"].as_u64().unwrap() > 0);
    assert_eq!(view["trackingDurability"], "durable");
    assert_eq!(view["peers"], json!([]));
    assert!(view.get("storeBinding").is_none());
    assert_eq!(operator_view(&state).await, view);
    assert_eq!(current_image(&state), before);
    assert_eq!(
        read_federation_persist_image(&connection).unwrap(),
        stored_before
    );
}

#[tokio::test]
async fn federation_operator_enable_uses_the_real_writer_and_survives_a_host_reload() {
    let root = TestTempRoot::create("federation-operator-reload");
    let path = root.path().join("termal.sqlite");
    let templates = root.path().join("orchestrators.json");
    let workdir = root.path().to_string_lossy().into_owned();
    let state = AppState::new_with_paths(workdir.clone(), path.clone(), templates.clone()).unwrap();
    let shutdown = OperatorPersistShutdown(&state);
    let before = current_image(&state);
    let local = operator_view(&state).await["local"].clone();
    let enabled = operator_configure(&state, central_settings(&local, true), StatusCode::OK).await;
    assert_eq!(enabled["enabled"], true);
    assert_eq!(enabled["role"], "central");
    assert_eq!(enabled["central"], local);
    assert_eq!(
        enabled["local"], local,
        "enabling must not mint a new identity"
    );
    assert_eq!(enabled["trackingDurability"], "durable");
    let image = current_image(&state);
    assert_eq!(image["settings"], central_settings(&local, true));
    assert_eq!(
        image["domain"]["version"].as_u64().unwrap(),
        before["domain"]["version"].as_u64().unwrap() + 1
    );
    let connection = rusqlite::Connection::open(&path).unwrap();
    assert!(
        PersistFenceTarget::Federation(image.clone())
            .is_already_durable(&connection)
            .unwrap()
    );
    let mut changed_content = image.clone();
    changed_content["settings"]["enabled"] = json!(false);
    assert!(
        !PersistFenceTarget::Federation(changed_content)
            .is_already_durable(&connection)
            .unwrap()
    );
    drop(connection);
    drop(shutdown);
    drop(state);

    // Use the real boot path at the same path, not an in-memory fixture restore.
    let reloaded = AppState::new_with_paths(workdir, path, templates).unwrap();
    let _shutdown = OperatorPersistShutdown(&reloaded);
    let view = operator_view(&reloaded).await;
    assert_eq!(view["enabled"], true);
    assert_eq!(view["role"], "central");
    assert_eq!(view["central"], local);
    assert_eq!(view["local"], local);
    assert_eq!(view["trackingDurability"], "durable");
}

#[tokio::test]
async fn federation_operator_disable_preserves_local_sessions_and_mailbox_history() {
    let (state, sender, target) = super::super::mailboxes::mailbox_test_state();
    {
        let inner = state.inner.lock().unwrap();
        state.persist_internal_locked(&inner).unwrap();
    }
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    bind_loaded_federation(
        &mut state.inner.lock().unwrap(),
        &connection,
        state.persistence_path.as_path(),
    )
    .unwrap();
    drop(connection);
    let receipt = operator_request(
        &state,
        "POST",
        &format!("/api/sessions/{sender}/mailboxes/send"),
        Some(json!({"targetSessionId": target, "message": "local history", "idempotencyKey": "local-before-enable"})),
        StatusCode::ACCEPTED,
    ).await;
    let read_path = format!(
        "/api/sessions/{target}/mailboxes/{}/read",
        receipt["mailboxId"].as_str().unwrap()
    );
    let read_body = json!({"afterSequence": 0, "limit": 20});
    let mailbox_before = operator_request(
        &state,
        "POST",
        &read_path,
        Some(read_body.clone()),
        StatusCode::OK,
    )
    .await;
    let sessions_before = serde_json::to_value(state.snapshot()).unwrap()["sessions"].clone();
    let local = operator_view(&state).await["local"].clone();
    operator_configure(&state, central_settings(&local, true), StatusCode::OK).await;
    let disabled =
        operator_configure(&state, central_settings(&local, false), StatusCode::OK).await;
    assert_eq!(disabled["enabled"], false);
    assert_eq!(disabled["role"], "central");
    assert_eq!(disabled["local"], local);
    assert_eq!(disabled["trackingDurability"], "durable");
    assert_eq!(
        serde_json::to_value(state.snapshot()).unwrap()["sessions"],
        sessions_before
    );
    let mailbox_after =
        operator_request(&state, "POST", &read_path, Some(read_body), StatusCode::OK).await;
    assert_eq!(mailbox_after["messages"], mailbox_before["messages"]);
    assert_eq!(
        mailbox_after["processedThrough"],
        mailbox_before["processedThrough"]
    );
    let stored = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(!stored.preferences.federation.enabled);
    assert_eq!(stored.preferences.federation.role, FederationRole::Central);
    // The existing local API still works after disable, not only old rows.
    operator_request(
        &state,
        "POST",
        &format!("/api/sessions/{sender}/mailboxes/send"),
        Some(json!({"targetSessionId": target, "message": "after disable", "idempotencyKey": "local-after-disable"})),
        StatusCode::ACCEPTED,
    ).await;
}

#[tokio::test]
async fn federation_operator_invalid_settings_never_partially_change_the_store() {
    let state = domain_state();
    let local = serde_json::to_value(
        state
            .inner
            .lock()
            .unwrap()
            .federation
            .local
            .clone()
            .unwrap(),
    )
    .unwrap();
    let before = current_image(&state);
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    let stored_before = read_federation_persist_image(&connection).unwrap();
    let mut stale = local.clone();
    stale["hostGeneration"] = json!(local["hostGeneration"].as_u64().unwrap() + 1);
    let invalid = [
        json!({"enabled": true, "role": "standalone", "central": null}),
        json!({"enabled": true, "role": "central", "central": stale}),
        json!({"enabled": true, "role": "central", "central": {"hostId": Uuid::new_v4().to_string(), "hostGeneration": 1}}),
        json!({"enabled": true, "role": "satellite", "central": {"hostId": Uuid::new_v4().to_string(), "hostGeneration": 1}}),
    ];
    for body in invalid {
        operator_configure(&state, body, StatusCode::BAD_REQUEST).await;
        assert_eq!(current_image(&state), before);
        assert_eq!(
            read_federation_persist_image(&connection).unwrap(),
            stored_before
        );
        assert!(state.inner.lock().unwrap().federation_durable);
    }
    // Match the existing API's JSON extractor convention: invalid shape is
    // 422, while a well-formed but invalid domain transition is 400.
    for body in [
        json!({"enabled": true, "role": "central", "central": local, "unexpected": true}),
        json!({"role": "central", "central": local}),
        json!({"enabled": true, "central": local}),
        json!({"enabled": "yes", "role": "central", "central": local}),
    ] {
        operator_configure(&state, body, StatusCode::UNPROCESSABLE_ENTITY).await;
        assert_eq!(current_image(&state), before);
        assert_eq!(
            read_federation_persist_image(&connection).unwrap(),
            stored_before
        );
        assert!(state.inner.lock().unwrap().federation_durable);
    }
}

#[tokio::test]
async fn federation_operator_failed_persistence_reports_an_unresolved_hold_not_success() {
    let state = domain_state();
    let local = serde_json::to_value(
        state
            .inner
            .lock()
            .unwrap()
            .federation
            .local
            .clone()
            .unwrap(),
    )
    .unwrap();
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    let stored_before = read_federation_persist_image(&connection).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_operator_write BEFORE INSERT ON state_metadata_units BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;").unwrap();
    operator_configure(
        &state,
        central_settings(&local, true),
        StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await;
    assert_eq!(
        read_federation_persist_image(&connection).unwrap(),
        stored_before
    );
    let view = operator_view(&state).await;
    assert_eq!(view["trackingDurability"], "unresolved");
    assert!(!state.inner.lock().unwrap().federation_durable);
    assert!(state.ensure_federation_admission_allowed().is_err());
}

#[tokio::test]
async fn federation_operator_read_reports_registry_capacity_without_granting_authority() {
    let state = central();
    let snapshot = active(&state, peer(48001));
    reserve(&state, &snapshot, "operator-reservation", "remote-session").unwrap();
    let before = current_image(&state);
    let view = operator_view(&state).await;
    assert_eq!(view["peers"].as_array().unwrap().len(), 1);
    assert_eq!(view["peers"][0]["host"]["hostId"], snapshot.peer.host_id);
    assert_eq!(view["peers"][0]["allocatedSessions"], 1);
    assert_eq!(view["peers"][0]["allocatedBuildSlots"], 1);
    assert_eq!(view["peers"][0]["capacity"]["maxSessions"], 1);
    assert_eq!(current_image(&state), before);
}

#[tokio::test]
async fn federation_operator_disable_retains_reservations_and_role_replacement_stays_fenced() {
    let state = central();
    let snapshot = active(&state, peer(48001));
    let placement = reserve(&state, &snapshot, "held-reservation", "remote-session").unwrap();
    let local = serde_json::to_value(&snapshot.local).unwrap();
    let disabled =
        operator_configure(&state, central_settings(&local, false), StatusCode::OK).await;
    assert_eq!(disabled["enabled"], false);
    assert_eq!(disabled["peers"][0]["allocatedSessions"], 1);
    assert_eq!(
        state.inner.lock().unwrap().federation.placements["held-reservation"],
        placement
    );
    assert!(
        state
            .ensure_federation_placement_current(&placement)
            .is_err()
    );
    let before = current_image(&state);
    operator_configure(
        &state,
        json!({"enabled": false, "role": "standalone", "central": null}),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(current_image(&state), before);
    let stored = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(stored.federation.placements["held-reservation"], placement);
}
