// Review counterexamples: metadata-only commits, configuration ABA and retained
// intent validation. All route effects still use the real two-host fixture.
use super::federation_entrypoints_fixture::*;
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activation_refuses_report_configuration_drift_with_unchanged_verification_basis() {
    let pair = Pair::new().await;
    let id = pair.native.host.host_id.clone();
    let original_basis =
        pair.central.inner.lock().unwrap().federation.peers[&id].verification_basis();
    pair.central
        .mutate_federation(|domain, _| {
            let expected = &mut domain.onboarding.reports.get_mut(&id).unwrap().expected;
            let port = &mut expected.endpoint.central_federation_port;
            *port = if *port == 47000 { 47001 } else { 47000 };
            Ok(())
        })
        .unwrap();
    assert_eq!(
        pair.central.inner.lock().unwrap().federation.peers[&id].verification_basis(),
        original_basis
    );
    let (status, body) = pair.operator("activate", json!({})).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "report configuration must still match: {body}"
    );
    assert!(body.to_string().contains(FEDERATION_STALE_AUTHORITY));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn installed_binding_hold_settled_before_signing_stays_pending() {
    let mut pair = Pair::new().await;
    pair.fixture_active();
    pair.command(json!({"command":"holdBindingPostcheck"}))
        .await;
    let context = pair.channel.security_context().unwrap();
    let (status, pending) = pair.reserve("post-install-hold").await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "committed INSTALL remains pending: {pending}"
    );
    assert!(pending.get("participant").is_none());
    let intent = pair
        .central
        .inner
        .lock()
        .unwrap()
        .federation
        .binding_install_intents["post-install-hold"]
        .clone();
    assert_eq!(pending["operationKey"], intent.operation_key);
    let observed = pair.command(json!({"command":"inspect"})).await;
    assert_eq!(observed["bindingCommittedAtHold"], true);
    assert_eq!(observed["bindingPostcheckFailed"], true);
    assert_eq!(
        observed["persistedBinding"],
        serde_json::to_value(&intent.binding).unwrap()
    );
    assert_eq!(observed["persistedCreation"], pair.native.creation);
    assert!(
        pair.central
            .inner
            .lock()
            .unwrap()
            .federation
            .mailbox_directory
            .is_empty()
    );
    assert!(pair.channel.security_current(&context).is_ok());
    assert!(
        context.sender.lock().unwrap().outstanding.is_empty(),
        "signed unknown settles only transport"
    );
    let (status, recovered) = pair.reserve("post-install-hold").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "original intent recovers: {recovered}"
    );
    assert_eq!(recovered["operationKey"], pending["operationKey"]);
    assert_eq!(
        recovered["participant"]["binding"],
        serde_json::to_value(&intent.binding).unwrap()
    );
    let mut wrong = intent.binding.clone();
    wrong.identity.creation_id = Uuid::new_v4().to_string();
    let (status, _) = pair
        .domain(
            json!({"operation":"install","binding":wrong,
        "expectedNativeCreationId":pair.native.creation,"operationKey":"known-conflict"}),
            true,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "proven pre-install conflict remains known"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_waits_for_exact_session_row_after_metadata_commits() {
    let mut pair = Pair::new().await;
    pair.fixture_active();
    let binding = pair.existing_binding("deferred-row");
    pair.command(json!({"command":"omitBindingRow"})).await;
    let lease = pair.channel.current_lease().unwrap();
    let command = FederationBindingCommand::Install {
        binding: binding.clone(),
        expected_native_creation_id: pair.native.creation.clone(),
        operation_key: "deferred-install".to_owned(),
    };
    let rpc = pair.rpc.clone();
    let job = tokio::spawn(async move {
        rpc.request::<FederationBindingInstalled>(FederationRpcRoute::Binding(lease, command))
            .await
    });
    let observed = pair.command(json!({"command":"waitOmittedFence"})).await;
    assert_eq!(observed["metadataCommitted"], true);
    assert_eq!(
        observed["fenceAcknowledged"], false,
        "metadata alone cannot acknowledge a missing session binding"
    );
    assert_eq!(observed["pending"], true);
    assert_eq!(observed["admissionHeld"], true);
    assert!(observed["persistedBinding"].is_null());
    assert_eq!(observed["centralDirectoryEntries"], 0);
    let installed = job.await.unwrap().unwrap();
    assert_eq!(installed.binding, binding);
    let committed = pair.command(json!({"command":"inspect"})).await;
    assert_eq!(
        committed["persistedBinding"],
        serde_json::to_value(&binding).unwrap()
    );
    assert_eq!(committed["persistedCreation"], pair.native.creation);
    assert!(
        pair.central
            .inner
            .lock()
            .unwrap()
            .federation
            .mailbox_directory
            .contains_key(&binding.participant_key())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn placement_freezes_while_disabled_and_resumes_identical_settings() {
    let mut pair = Pair::new().await;
    pair.fixture_active();
    let initial = pair.reserve("settings-freeze").await;
    assert_eq!(initial.0, StatusCode::OK);
    let original = pair
        .central
        .inner
        .lock()
        .unwrap()
        .federation
        .placements
        .clone();
    let settings = pair
        .central
        .inner
        .lock()
        .unwrap()
        .preferences
        .federation
        .clone();
    let mut disabled = settings.clone();
    disabled.enabled = false;
    pair.central.configure_federation(disabled).unwrap();
    let attempts = pair.command(json!({"command":"installRequests"})).await;
    assert_eq!(
        pair.reserve("settings-freeze").await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        pair.command(json!({"command":"installRequests"})).await,
        attempts
    );
    assert_eq!(
        pair.central.inner.lock().unwrap().federation.placements,
        original
    );
    pair.central.configure_federation(settings).unwrap();
    pair.replace_central_channel(false).await;
    assert_eq!(pair.operator("verify", json!({})).await.0, StatusCode::OK);
    pair.activate().await;
    let (status, body) = pair.reserve("settings-freeze").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "identical material settings may resume the frozen intent: {body}"
    );
    assert_eq!(body["operationKey"], initial.1["operationKey"]);
    let recovered = pair
        .central
        .inner
        .lock()
        .unwrap()
        .federation
        .placements
        .clone();
    assert_eq!(
        recovered["settings-freeze"].identity,
        original["settings-freeze"].identity
    );
    assert_eq!(
        recovered["settings-freeze"].placement_generation,
        original["settings-freeze"].placement_generation
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn placement_recovery_refuses_material_port_change_after_fresh_verification() {
    let mut pair = Pair::new().await;
    pair.fixture_active();
    assert_eq!(pair.reserve("material-change").await.0, StatusCode::OK);
    let original = pair
        .central
        .inner
        .lock()
        .unwrap()
        .federation
        .placements
        .clone();
    let intents = pair
        .central
        .inner
        .lock()
        .unwrap()
        .federation
        .binding_install_intents
        .clone();
    let attempts = pair.command(json!({"command":"installRequests"})).await;
    let id = pair.native.host.host_id.clone();
    // Retire the exact installed basis before changing material authority.
    // A stale teardown must never remove a link belonging to a new basis.
    pair.retire_central_channel().await;
    assert!(pair.channel.current_lease().is_err());
    assert!(!pair
        .central
        .inner
        .lock()
        .unwrap()
        .federation
        .links
        .contains_key(&id));
    pair.central
        .mutate_federation(|domain, _| {
            let peer = domain.peers.get_mut(&id).unwrap();
            peer.endpoint.ssh_port = if peer.endpoint.ssh_port == 22 { 23 } else { 22 };
            Ok(())
        })
        .unwrap();
    pair.replace_central_channel(false).await;
    assert_eq!(pair.operator("verify", json!({})).await.0, StatusCode::OK);
    pair.activate().await;
    let (status, body) = pair.reserve("material-change").await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "materially different authority must refuse: {body}"
    );
    assert!(body.to_string().contains(FEDERATION_STALE_AUTHORITY));
    assert_eq!(
        pair.command(json!({"command":"installRequests"})).await,
        attempts
    );
    assert_eq!(
        pair.central.inner.lock().unwrap().federation.placements,
        original
    );
    assert_eq!(
        pair.central
            .inner
            .lock()
            .unwrap()
            .federation
            .binding_install_intents,
        intents
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_intent_refuses_changed_enrollment_or_placement_identity() {
    let pair = Pair::new().await;
    pair.fixture_active();
    assert_eq!(pair.reserve("authority").await.0, StatusCode::OK);
    let domain = pair.central.inner.lock().unwrap().federation.clone();
    let intent = &domain.binding_install_intents["authority"];
    let placement = &domain.placements["authority"];
    let current = pair.peer();
    assert!(
        intent
            .ensure_authority(&domain, &current, placement)
            .is_ok()
    );
    for mutation in 0..5 {
        let mut changed = placement.clone();
        let mut peer = current.clone();
        match mutation {
            0 => peer.enrollment_generation += 1,
            1 => changed.placement_generation += 1,
            2 => changed.identity.creation_id = Uuid::new_v4().to_string(),
            3 => peer.local.host_generation += 1,
            _ => peer.local_role = FederationRole::Satellite,
        }
        assert!(
            intent.ensure_authority(&domain, &peer, &changed).is_err(),
            "retained content must reject changed authority {mutation}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn intent_sqlite_round_trip_and_invalid_maps_fail_validation() {
    let pair = Pair::new().await;
    pair.fixture_active();
    assert_eq!(pair.reserve("stored-intent").await.0, StatusCode::OK);
    let restored = load_state(pair.central.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        restored.federation.binding_install_intents,
        pair.central
            .inner
            .lock()
            .unwrap()
            .federation
            .binding_install_intents
    );
    let valid = restored.federation.clone();
    assert!(valid.validate().is_ok());
    for mutation in 0..4 {
        let mut invalid = valid.clone();
        match mutation {
            0 => {
                invalid.placements.clear();
            }
            1 => {
                invalid
                    .binding_install_intents
                    .get_mut("stored-intent")
                    .unwrap()
                    .binding
                    .identity
                    .creation_id = Uuid::new_v4().to_string();
            }
            2 => {
                invalid
                    .binding_install_intents
                    .get_mut("stored-intent")
                    .unwrap()
                    .binding
                    .placement_generation += 1;
            }
            _ => {
                invalid
                    .binding_install_intents
                    .get_mut("stored-intent")
                    .unwrap()
                    .peer_enrollment_generation += 1;
            }
        }
        assert!(
            invalid.validate().is_err(),
            "malformed retained intent {mutation} must refuse"
        );
        // Preserve the original store identity and a coherent content marker,
        // then inject only the invalid map. Boot must reject the authority
        // relation itself, rather than merely a broken digest.
        let scratch = TestTempRoot::create("entrypoint-invalid-intent");
        let path = scratch.path().join("state.sqlite");
        let source = rusqlite::Connection::open(pair.central.persistence_path.as_path()).unwrap();
        source
            .execute("VACUUM INTO ?1", [path.to_str().unwrap()])
            .unwrap();
        let mut snapshot = PersistedState::from_inner(&restored);
        invalid.version += 1;
        snapshot.federation = invalid;
        let mut connection = rusqlite::Connection::open(&path).unwrap();
        let transaction = connection.transaction().unwrap();
        write_federation_host_generation(&transaction, &snapshot).unwrap();
        replace_metadata_units_for_test(&transaction, serde_json::to_value(&snapshot).unwrap());
        transaction.commit().unwrap();
        drop(connection);
        let error = match load_state(&path) {
            Err(error) => format!("{error:#}"),
            Ok(_) => panic!("malformed retained intent {mutation} must refuse on boot"),
        };
        assert!(
            error.contains("binding install"),
            "boot must reject the retained intent relation itself: {error}"
        );
    }
}
