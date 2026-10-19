// Owns the operator surface that configures this host as a SATELLITE of a
// named central through the M1b settings route: its validation, version and
// content fencing, typed refusals, disable and standalone return, the bound
// session guard, the satellite read view and the O1 precondition it sets up.
// Does not enroll the central's side, provision transport or run the O1
// checklist itself. New coverage mounted below federation_registry.rs, beside
// federation_operator.rs, whose HTTP helpers it shares.
use super::operator::{
    current_image, operator_configure, operator_request, operator_view, OperatorPersistShutdown,
};
use super::*;

/// The settings a satellite operator sends: this host bound to `central`,
/// fenced on the version just read.
fn satellite_settings(central: &Value, enabled: bool, version: &Value) -> Value {
    json!({"enabled": enabled, "role": "satellite", "central": central, "expectedVersion": version})
}

fn standalone_settings(version: &Value) -> Value {
    json!({"enabled": false, "role": "standalone", "central": null, "expectedVersion": version})
}

/// The identity a central host reports as its own `local`, as an operator
/// copies it from that host's read view.
fn central_identity() -> (FederationPeerRecord, Value) {
    let record = peer(48001);
    let identity = serde_json::to_value(&record.identity).unwrap();
    (record, identity)
}

/// A host with local sessions and mailboxes on a real federation store.
fn satellite_host() -> (AppState, String, String) {
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
    (state, sender, target)
}

async fn version_of(state: &AppState) -> Value {
    let version = operator_view(state).await["version"].clone();
    assert!(version.is_u64(), "the read view names the domain version: {version}");
    version
}

fn bind_session_to(state: &AppState, session: &str, central: &FederationHostIdentity) {
    let mut inner = state.inner.lock().unwrap();
    let local = inner.federation.local.clone().unwrap();
    let index = inner.find_session_index(session).unwrap();
    inner.session_mut_by_index(index).unwrap().federation_mailbox_binding =
        Some(FederationMailboxBinding {
            central: central.clone(),
            identity: FederationSessionIdentity {
                host_id: local.host_id,
                session_id: session.to_owned(),
                creation_id: Uuid::new_v4().to_string(),
            },
            placement_generation: 1,
        });
}

async fn send_local(state: &AppState, sender: &str, target: &str, key: &str) -> Value {
    operator_request(
        state,
        "POST",
        &format!("/api/sessions/{sender}/mailboxes/send"),
        Some(json!({"targetSessionId": target, "message": key, "idempotencyKey": key})),
        StatusCode::ACCEPTED,
    )
    .await
}

/// The settings route configures this host as a satellite bound to the
/// central's exact identity, through the real writer, and the binding and
/// its version survive a host reload on the real boot path.
#[tokio::test]
async fn federation_satellite_settings_bind_a_named_central_and_survive_a_host_reload() {
    let root = TestTempRoot::create("federation-satellite-reload");
    let path = root.path().join("termal.sqlite");
    let templates = root.path().join("orchestrators.json");
    let workdir = root.path().to_string_lossy().into_owned();
    let state = AppState::new_with_paths(workdir.clone(), path.clone(), templates.clone()).unwrap();
    let shutdown = OperatorPersistShutdown(&state);
    let before = operator_view(&state).await;
    let local = before["local"].clone();
    let version = version_of(&state).await;
    let (_, central) = central_identity();
    let enabled =
        operator_configure(&state, satellite_settings(&central, true, &version), StatusCode::OK).await;
    assert_eq!(enabled["enabled"], true);
    assert_eq!(enabled["role"], "satellite");
    assert_eq!(enabled["central"], central);
    assert_eq!(enabled["local"], local, "binding a central never mints a new local identity");
    assert_eq!(enabled["trackingDurability"], "durable");
    assert_eq!(enabled["version"].as_u64().unwrap(), version.as_u64().unwrap() + 1);
    let image = current_image(&state);
    assert_eq!(image["settings"]["role"], "satellite");
    assert_eq!(image["settings"]["central"], central);
    let connection = rusqlite::Connection::open(&path).unwrap();
    assert!(
        PersistFenceTarget::Federation(image.clone())
            .is_already_durable(&connection)
            .unwrap(),
        "the exact content is durable before success"
    );
    drop(connection);
    drop(shutdown);
    drop(state);

    let reloaded = AppState::new_with_paths(workdir, path, templates).unwrap();
    let _shutdown = OperatorPersistShutdown(&reloaded);
    let view = operator_view(&reloaded).await;
    assert_eq!(view["enabled"], true);
    assert_eq!(view["role"], "satellite");
    assert_eq!(view["central"], central);
    assert_eq!(view["local"], local);
    assert_eq!(view["version"], enabled["version"]);
    assert_eq!(view["trackingDurability"], "durable");
}

/// A satellite change is fenced on the version the operator read: without
/// it, or against a version another change has moved past, it is refused
/// with a typed error and the store is untouched.
#[tokio::test]
async fn federation_satellite_settings_require_the_version_the_operator_read() {
    let state = domain_state();
    let (_, central) = central_identity();
    let version = version_of(&state).await;
    let before = current_image(&state);
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    let stored_before = read_federation_persist_image(&connection).unwrap();
    let missing = operator_configure(
        &state,
        json!({"enabled": true, "role": "satellite", "central": central}),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(missing["code"], "federation_expected_version_required");
    let stale = json!(version.as_u64().unwrap() + 1);
    let conflict =
        operator_configure(&state, satellite_settings(&central, true, &stale), StatusCode::CONFLICT).await;
    assert_eq!(conflict["code"], "federation_version_conflict");
    assert_eq!(current_image(&state), before);
    assert_eq!(read_federation_persist_image(&connection).unwrap(), stored_before);
    assert!(state.inner.lock().unwrap().federation_durable);
    operator_configure(&state, satellite_settings(&central, true, &version), StatusCode::OK).await;
    // Leaving the satellite role is fenced the same way.
    let again = version_of(&state).await;
    let wrong = standalone_settings(&json!(again.as_u64().unwrap() + 7));
    assert_eq!(
        operator_configure(&state, wrong, StatusCode::CONFLICT).await["code"],
        "federation_version_conflict"
    );
    let unfenced = json!({"enabled": false, "role": "standalone", "central": null});
    assert_eq!(
        operator_configure(&state, unfenced, StatusCode::BAD_REQUEST).await["code"],
        "federation_expected_version_required",
        "leaving the satellite role needs the version too"
    );
}

/// M1b's central configuration keeps working without the version field, and
/// is fenced when an operator sends it.
#[tokio::test]
async fn federation_central_settings_stay_compatible_and_honour_an_expected_version() {
    let state = domain_state();
    let local = operator_view(&state).await["local"].clone();
    operator_configure(
        &state,
        json!({"enabled": true, "role": "central", "central": local}),
        StatusCode::OK,
    )
    .await;
    let version = version_of(&state).await;
    let stale = json!({"enabled": false, "role": "central", "central": local, "expectedVersion": version.as_u64().unwrap() + 1});
    assert_eq!(
        operator_configure(&state, stale, StatusCode::CONFLICT).await["code"],
        "federation_version_conflict"
    );
    let fenced = json!({"enabled": false, "role": "central", "central": local, "expectedVersion": version});
    assert_eq!(operator_configure(&state, fenced, StatusCode::OK).await["enabled"], false);
}

/// A central identity that is stale (an older generation of the bound
/// central), mismatched (another central while the old one is enrolled) or
/// this host itself is refused with a typed error, leaving the store as it
/// was.
#[tokio::test]
async fn federation_satellite_stale_or_mismatched_central_identity_is_refused_typed() {
    let state = domain_state();
    let (record, _) = central_identity();
    let mut bound = record.identity.clone();
    bound.host_generation = 2;
    let bound_json = serde_json::to_value(&bound).unwrap();
    let version = version_of(&state).await;
    operator_configure(&state, satellite_settings(&bound_json, true, &version), StatusCode::OK).await;

    let assert_unchanged = |before: &Value, state: &AppState| {
        assert_eq!(&current_image(state), before);
        assert!(state.inner.lock().unwrap().federation_durable);
    };
    let mut older = bound.clone();
    older.host_generation = 1;
    let version = version_of(&state).await;
    let before = current_image(&state);
    let stale = operator_configure(
        &state,
        satellite_settings(&serde_json::to_value(&older).unwrap(), true, &version),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(stale["code"], "federation_central_stale");
    assert_unchanged(&before, &state);

    let mut enrolled = record.clone();
    enrolled.identity = bound.clone();
    state.enroll_federation_peer(enrolled).unwrap();
    let version = version_of(&state).await;
    let before = current_image(&state);
    let other = serde_json::to_value(&peer(48002).identity).unwrap();
    let mismatched =
        operator_configure(&state, satellite_settings(&other, true, &version), StatusCode::CONFLICT).await;
    assert_eq!(mismatched["code"], "federation_central_mismatch");
    assert_unchanged(&before, &state);
    let mut newer = bound.clone();
    newer.host_generation = 3;
    let rebound = operator_configure(
        &state,
        satellite_settings(&serde_json::to_value(&newer).unwrap(), true, &version),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(rebound["code"], "federation_central_mismatch", "a new epoch needs the old peer revoked");
    assert_unchanged(&before, &state);

    let local = operator_view(&state).await["local"].clone();
    let itself =
        operator_configure(&state, satellite_settings(&local, true, &version), StatusCode::BAD_REQUEST).await;
    assert_eq!(itself["code"], "federation_invalid_binding");
    assert_unchanged(&before, &state);
}

/// Staleness is judged against the highest generation of that central the
/// host knows, a retained peer record included: after its peer is revoked
/// and the host returned to standalone, an older generation is still stale.
#[tokio::test]
async fn federation_satellite_central_older_than_a_retained_peer_record_is_stale() {
    let state = domain_state();
    let (record, _) = central_identity();
    let mut bound = record.identity.clone();
    bound.host_generation = 2;
    let bound_json = serde_json::to_value(&bound).unwrap();
    let version = version_of(&state).await;
    operator_configure(&state, satellite_settings(&bound_json, true, &version), StatusCode::OK).await;
    let mut enrolled = record.clone();
    enrolled.identity = bound.clone();
    state.enroll_federation_peer(enrolled).unwrap();
    let snapshot = state.federation_peer_snapshot(&bound.host_id).unwrap();
    state
        .transition_federation_peer(&snapshot, FederationMembershipState::Revoked)
        .unwrap();
    let version = version_of(&state).await;
    operator_configure(&state, standalone_settings(&version), StatusCode::OK).await;

    let mut older = bound.clone();
    older.host_generation = 1;
    let version = version_of(&state).await;
    let before = current_image(&state);
    let stale = operator_configure(
        &state,
        satellite_settings(&serde_json::to_value(&older).unwrap(), true, &version),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(stale["code"], "federation_central_stale");
    assert_eq!(current_image(&state), before);
}

/// While a local session holds a domain mailbox binding to the bound
/// central, the role and the central cannot be replaced (the session would be
/// stranded): typed refusal. Disabling keeps the binding and is allowed.
#[tokio::test]
async fn federation_satellite_role_or_central_change_is_refused_while_sessions_are_domain_bound() {
    let (state, _sender, target) = satellite_host();
    let (record, central) = central_identity();
    let version = version_of(&state).await;
    operator_configure(&state, satellite_settings(&central, true, &version), StatusCode::OK).await;
    bind_session_to(&state, &target, &record.identity);
    let bound = operator_view(&state).await;
    assert_eq!(bound["satellite"]["boundSessions"], 1);

    let version = version_of(&state).await;
    let before = current_image(&state);
    let standalone = operator_configure(&state, standalone_settings(&version), StatusCode::CONFLICT).await;
    assert_eq!(standalone["code"], "federation_bound_sessions");
    assert_eq!(current_image(&state), before);
    let other = serde_json::to_value(&peer(48002).identity).unwrap();
    let elsewhere =
        operator_configure(&state, satellite_settings(&other, true, &version), StatusCode::CONFLICT).await;
    assert_eq!(elsewhere["code"], "federation_bound_sessions");
    assert_eq!(current_image(&state), before);

    let disabled =
        operator_configure(&state, satellite_settings(&central, false, &version), StatusCode::OK).await;
    assert_eq!(disabled["enabled"], false);
    assert_eq!(disabled["role"], "satellite");
    let inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&target).unwrap();
    assert!(
        inner.sessions[index].federation_mailbox_binding.is_some(),
        "disabling keeps the session's binding"
    );
}

/// Disable holds federation admission with the central binding retained; a
/// separate request returns the host to standalone; in each state the local
/// sessions and mailboxes without a domain binding behave unchanged.
#[tokio::test]
async fn federation_satellite_disable_holds_and_a_separate_request_returns_to_standalone() {
    let (state, sender, target) = satellite_host();
    let receipt = send_local(&state, &sender, &target, "before-satellite").await;
    let read_path = format!(
        "/api/sessions/{target}/mailboxes/{}/read",
        receipt["mailboxId"].as_str().unwrap()
    );
    let read_body = json!({"afterSequence": 0, "limit": 50});
    let (_, central) = central_identity();
    let sessions = |state: &AppState| serde_json::to_value(state.snapshot()).unwrap()["sessions"].clone();
    let mailbox = |state: &AppState| {
        let state = state.clone();
        let (path, body) = (read_path.clone(), read_body.clone());
        async move { operator_request(&state, "POST", &path, Some(body), StatusCode::OK).await["messages"].clone() }
    };

    // Each configuration change leaves every local session and the local
    // mailbox exactly as they were; local sends keep working in each state.
    let (sessions_before, mailbox_before) = (sessions(&state), mailbox(&state).await);
    let version = version_of(&state).await;
    operator_configure(&state, satellite_settings(&central, true, &version), StatusCode::OK).await;
    assert_eq!(sessions(&state), sessions_before, "enabling satellite changes no local session");
    assert_eq!(mailbox(&state).await, mailbox_before);
    send_local(&state, &sender, &target, "while-satellite").await;

    let (sessions_before, mailbox_before) = (sessions(&state), mailbox(&state).await);
    let version = version_of(&state).await;
    let disabled =
        operator_configure(&state, satellite_settings(&central, false, &version), StatusCode::OK).await;
    assert_eq!(disabled["enabled"], false);
    assert_eq!(disabled["role"], "satellite");
    assert_eq!(disabled["central"], central, "disable retains the central binding");
    assert!(state.ensure_federation_admission_allowed().is_err(), "disable holds admission");
    assert_eq!(sessions(&state), sessions_before, "disabling changes no local session");
    assert_eq!(mailbox(&state).await, mailbox_before);
    send_local(&state, &sender, &target, "while-disabled").await;

    let (sessions_before, mailbox_before) = (sessions(&state), mailbox(&state).await);
    let version = version_of(&state).await;
    let standalone = operator_configure(&state, standalone_settings(&version), StatusCode::OK).await;
    assert_eq!(standalone["role"], "standalone");
    assert!(standalone["central"].is_null());
    assert!(standalone.get("satellite").is_none_or(Value::is_null));
    assert_eq!(sessions(&state), sessions_before, "returning to standalone changes no local session");
    assert_eq!(mailbox(&state).await, mailbox_before);
    send_local(&state, &sender, &target, "after-standalone").await;

    assert_eq!(
        mailbox(&state).await.as_array().unwrap().len(),
        4,
        "every local send in every state was accepted and kept"
    );
    let stored = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    assert_eq!(stored.preferences.federation.role, FederationRole::Standalone);
}

/// The read view shows the satellite's bound central, the central's peer
/// record with its generations, and whether the CURRENT durable link is up,
/// never a stale observation; secrets stay out of it.
#[tokio::test]
async fn federation_satellite_read_view_shows_central_peer_generations_and_current_link() {
    let state = domain_state();
    let (record, central) = central_identity();
    let version = version_of(&state).await;
    let view = operator_configure(&state, satellite_settings(&central, true, &version), StatusCode::OK).await;
    assert_eq!(view["satellite"]["central"], central);
    assert_eq!(view["satellite"]["centralPeer"]["enrolled"], false);
    assert_eq!(view["satellite"]["centralPeer"]["linkConnected"], false);
    assert_eq!(view["satellite"]["boundSessions"], 0);

    state.enroll_federation_peer(record.clone()).unwrap();
    let snapshot = state.federation_peer_snapshot(&record.identity.host_id).unwrap();
    let enrolled = operator_view(&state).await;
    assert_eq!(enrolled["satellite"]["centralPeer"]["enrolled"], true);
    assert_eq!(enrolled["satellite"]["centralPeer"]["membership"], "enrolled");
    assert_eq!(enrolled["satellite"]["centralPeer"]["linkConnected"], false, "enrollment is not a link");

    let connected = state
        .federation_peer_connected(&snapshot, Uuid::new_v4().to_string())
        .unwrap();
    let linked = operator_view(&state).await;
    let peer_view = &linked["satellite"]["centralPeer"];
    assert_eq!(peer_view["linkConnected"], true);
    assert_eq!(peer_view["recordGeneration"], connected.record_generation);
    assert_eq!(peer_view["transportGeneration"], connected.transport_generation);
    assert_eq!(peer_view["verificationCurrent"], false);
    // The block carries exactly these facts: no endpoint, credential
    // reference or key material.
    let mut keys = peer_view
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(
        keys,
        [
            "enrolled",
            "lastObserved",
            "linkConnected",
            "membership",
            "recordGeneration",
            "transportGeneration",
            "verificationCurrent"
        ]
    );
    for secret in ["pair-capability", "pinned-host", "known-hosts"] {
        assert!(!linked["satellite"].to_string().contains(secret), "no credential reference: {secret}");
    }

    // Disabling invalidates the link: the view says so, even though the
    // last observation is kept.
    let version = version_of(&state).await;
    let held = operator_configure(&state, satellite_settings(&central, false, &version), StatusCode::OK).await;
    assert_eq!(held["satellite"]["centralPeer"]["linkConnected"], false);
}

/// O1 on the central runs probes this satellite accepts only when it is a
/// satellite of exactly that central, has the central enrolled, and holds the
/// current link: configured through the settings route, the satellite meets
/// that precondition, and refuses a missing or stale link.
#[tokio::test]
async fn federation_satellite_configured_through_the_route_meets_the_o1_probe_precondition() {
    let state = domain_state();
    let (record, central) = central_identity();
    let version = version_of(&state).await;
    operator_configure(&state, satellite_settings(&central, true, &version), StatusCode::OK).await;
    state.enroll_federation_peer(record.clone()).unwrap();
    let snapshot = state.federation_peer_snapshot(&record.identity.host_id).unwrap();
    let incarnation = Uuid::new_v4().to_string();
    assert!(
        state
            .prepare_federation_probe(&snapshot, &incarnation, &Uuid::new_v4().to_string())
            .is_err(),
        "no link yet: no probe"
    );
    let connected = state.federation_peer_connected(&snapshot, incarnation.clone()).unwrap();
    assert!(
        state
            .prepare_federation_probe(&connected, &Uuid::new_v4().to_string(), &Uuid::new_v4().to_string())
            .is_err(),
        "a stale link incarnation: no probe"
    );
    let probe = state
        .prepare_federation_probe(&connected, &incarnation, &Uuid::new_v4().to_string())
        .expect("the satellite accepts the central's probe on the current link");
    assert_eq!(probe.link_incarnation, incarnation);
}

/// A failed write of a satellite configuration is reported as an unresolved
/// hold, never as success.
#[tokio::test]
async fn federation_satellite_failed_persistence_reports_an_unresolved_hold() {
    let state = domain_state();
    let (_, central) = central_identity();
    let version = version_of(&state).await;
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    let stored_before = read_federation_persist_image(&connection).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_satellite_write BEFORE INSERT ON app_state BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;").unwrap();
    operator_configure(
        &state,
        satellite_settings(&central, true, &version),
        StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await;
    assert_eq!(read_federation_persist_image(&connection).unwrap(), stored_before);
    let view = operator_view(&state).await;
    assert_eq!(view["trackingDurability"], "unresolved");
    assert!(state.ensure_federation_admission_allowed().is_err());
}
