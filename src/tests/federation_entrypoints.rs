// Real operator and authenticated domain entrypoints against two disposable
// processes. Loopback and fake platform probes do not prove live Mac readiness.
use super::federation_entrypoints_fixture::*;
use super::federation_onboarding_writer::ControlledWriter;
use super::*;

fn stale((status, body): (StatusCode, Value)) {
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "expected stale authority: {body}"
    );
    assert!(
        body.to_string().contains(FEDERATION_STALE_AUTHORITY),
        "{body}"
    );
}

fn prepare(session: &str) -> Value {
    json!({"operation":"prepare","nativeSessionId":session})
}

fn install(binding: &FederationMailboxBinding, creation: &str, key: &str) -> Value {
    json!({"operation":"install","binding":binding,
        "expectedNativeCreationId":creation,"operationKey":key})
}

fn schema(app: &AppState) -> Vec<(String, String, String)> {
    let connection = app.mailbox_store.connection().unwrap();
    let mut statement = connection
        .prepare("SELECT type,name,sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY type,name")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_process_fixture_verifies_without_activation_or_binding() {
    let mut pair = Pair::new().await;
    assert_eq!(pair.peer().membership, FederationMembershipState::Verified);
    assert!(
        pair.central
            .inner
            .lock()
            .unwrap()
            .federation
            .placements
            .is_empty()
    );
    let observed = pair.command(json!({"command":"inspect"})).await;
    assert!(observed["binding"].is_null());
    assert_eq!(observed["creation"], pair.native.creation);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activation_commits_verified_peer_and_duplicate_does_not_bump_generation() {
    let pair = Pair::new().await;
    let before = pair.peer();
    pair.activate().await;
    let active = pair.peer();
    assert_eq!(active.membership, FederationMembershipState::Active);
    assert!(active.record_generation > before.record_generation);
    pair.activate().await;
    assert_eq!(pair.peer(), active);
    assert_eq!(
        load_state(pair.central.persistence_path.as_path())
            .unwrap()
            .unwrap()
            .federation
            .peers[&pair.native.host.host_id]
            .membership,
        FederationMembershipState::Active
    );
}

macro_rules! refused_membership {
    ($name:ident,$membership:ident,$route:literal) => {
        #[tokio::test(flavor="multi_thread",worker_threads=2)]
        async fn $name() {
            let pair=Pair::new().await;
            pair.fixture_membership(FederationMembershipState::$membership);
            let before=pair.central.inner.lock().unwrap().federation.clone();
            let body=if $route == "activate" { json!({}) } else {
                json!({"nativeSessionId":pair.native.session,"admissionKey":"refused"}) };
            stale(pair.operator($route,body).await);
            assert_eq!(pair.central.inner.lock().unwrap().federation,before);
        }
    };
}

refused_membership!(activation_refuses_enrolled, Enrolled, "activate");
refused_membership!(activation_refuses_draining, Draining, "activate");
refused_membership!(activation_refuses_revoked, Revoked, "activate");
refused_membership!(placement_refuses_enrolled, Enrolled, "placements");
refused_membership!(placement_refuses_verified, Verified, "placements");
refused_membership!(placement_refuses_draining, Draining, "placements");
refused_membership!(placement_refuses_revoked, Revoked, "placements");

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activation_refuses_missing_current_report_even_with_verified_checklist() {
    let pair = Pair::new().await;
    let id = pair.native.host.host_id.clone();
    pair.central
        .mutate_federation(|domain, _| {
            domain.onboarding.reports.remove(&id);
            Ok(())
        })
        .unwrap();
    stale(pair.operator("activate", json!({})).await);
    assert_eq!(pair.peer().membership, FederationMembershipState::Verified);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activation_refuses_report_from_another_configuration_basis() {
    let pair = Pair::new().await;
    let id = pair.native.host.host_id.clone();
    pair.central
        .mutate_federation(|domain, _| {
            domain.onboarding.reports.get_mut(&id).unwrap().basis = "b".repeat(64);
            Ok(())
        })
        .unwrap();
    stale(pair.operator("activate", json!({})).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_activation_refuses_a_changed_report_basis() {
    let pair = Pair::new().await;
    pair.fixture_active();
    let id = pair.native.host.host_id.clone();
    pair.central
        .mutate_federation(|domain, _| {
            domain.onboarding.reports.get_mut(&id).unwrap().basis = "b".repeat(64);
            Ok(())
        })
        .unwrap();
    stale(pair.operator("activate", json!({})).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activation_ambiguous_persistence_never_reports_committed_or_duplicate_success() {
    let mut pair = Pair::new().await;
    let mut writer = ControlledWriter::attach(&mut pair.central);
    let app = pair.central.clone();
    let id = pair.native.host.host_id.clone();
    let mut job = tokio::spawn(async move { operator(&app, &id, "activate", json!({})).await });
    let fence = tokio::select! {
        result=&mut job => panic!("activation must reach durability before replying: {:?}",result.unwrap()),
        fence=writer.take()=>fence,
    };
    fence.finish(Err(PersistFenceError::WriteFailed(
        "injected activation persistence failure".into(),
    )));
    let (status, body) = job.await.unwrap();
    assert!(!status.is_success(), "ambiguous activation: {body}");
    assert!(!pair.central.inner.lock().unwrap().federation_durable);
    assert!(!pair.operator("activate", json!({})).await.0.is_success());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activation_revalidates_snapshot_after_a_delayed_commit() {
    let mut pair = Pair::new().await;
    let mut writer = ControlledWriter::attach(&mut pair.central);
    let app = pair.central.clone();
    let id = pair.native.host.host_id.clone();
    let mut job = tokio::spawn(async move { operator(&app, &id, "activate", json!({})).await });
    let fence = tokio::select! {
        result=&mut job => panic!("activation route did not reach commit cut: {:?}",result.unwrap()),
        fence=writer.take()=>fence,
    };
    let state = PersistedState::from_inner(&pair.central.inner.lock().unwrap());
    persist_persisted_state_to_sqlite(pair.central.persistence_path.as_path(), &state).unwrap();
    let connection = rusqlite::Connection::open(pair.central.persistence_path.as_path()).unwrap();
    assert!(fence.target.is_already_durable(&connection).unwrap());
    {
        let mut inner = pair.central.inner.lock().unwrap();
        inner
            .federation
            .peers
            .get_mut(&pair.native.host.host_id)
            .unwrap()
            .record_generation += 1;
    }
    fence.finish(Ok(()));
    stale(job.await.unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn operator_routes_refuse_noncentral_local_role() {
    let app = federation_registry::domain_state();
    let id = Uuid::new_v4().to_string();
    for suffix in ["activate", "placements"] {
        let body = if suffix == "activate" {
            json!({})
        } else {
            json!({"nativeSessionId":"session-1","admissionKey":"key"})
        };
        stale(operator(&app, &id, suffix, body).await);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepare_returns_exact_native_token_without_mutating_either_host() {
    let mut pair = Pair::new().await;
    let before = pair.command(json!({"command":"inspect"})).await;
    let central = pair.central.inner.lock().unwrap().federation.clone();
    let (status, body) = pair.domain(prepare(&pair.native.session), true).await;
    assert_eq!(status, StatusCode::OK, "PREPARE: {body}");
    assert_eq!(body["nativeCreationId"], pair.native.creation);
    assert_eq!(pair.command(json!({"command":"inspect"})).await, before);
    assert_eq!(pair.central.inner.lock().unwrap().federation, central);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binding_listener_requires_authenticated_pair() {
    let mut pair = Pair::new().await;
    let before = pair.command(json!({"command":"inspect"})).await;
    let (status, body) = pair.domain(prepare(&pair.native.session), false).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "federation message authentication refused");
    assert_eq!(pair.command(json!({"command":"inspect"})).await, before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binding_listener_rejects_bearer_key_without_request_mac() {
    let mut pair = Pair::new().await;
    let before = pair.command(json!({"command":"inspect"})).await;
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!(
            "http://127.0.0.1:{}/federation/v1/mailbox/binding",
            pair.native.port
        ))
        .header(FEDERATION_CAPABILITY_HEADER, "a".repeat(64))
        .json(&prepare(&pair.native.session))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"], "federation message authentication refused");
    assert_eq!(pair.command(json!({"command":"inspect"})).await, before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepare_refuses_absent_root() {
    let pair = Pair::new().await;
    let (status, body) = pair.domain(prepare("session-99999"), true).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "absent root: {body}");
    assert!(
        body.to_string().contains("session not found"),
        "handler refusal, not missing route: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepare_refuses_delegation_child() {
    let mut pair = Pair::new().await;
    pair.command(json!({"command":"nonRoot"})).await;
    assert_eq!(
        pair.domain(prepare(&pair.native.session), true).await.0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepare_refuses_already_bound_root() {
    let mut pair = Pair::new().await;
    pair.fixture_active();
    let binding = pair.existing_binding("bound-fixture");
    pair.command(json!({"command":"bindFixture","binding":binding}))
        .await;
    stale(pair.domain(prepare(&pair.native.session), true).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_is_durable_idempotent_and_refuses_a_different_binding() {
    let mut pair = Pair::new().await;
    pair.fixture_active();
    let binding = pair.existing_binding("install-fixture");
    let request = install(&binding, &pair.native.creation, "install-key");
    let (status, body) = pair.domain(request.clone(), true).await;
    assert_eq!(status, StatusCode::OK, "INSTALL: {body}");
    let before = pair.command(json!({"command":"inspect"})).await;
    assert_eq!(before["binding"], serde_json::to_value(&binding).unwrap());
    assert_eq!(before["durable"], true);
    assert_eq!(
        before["persistedBinding"],
        serde_json::to_value(&binding).unwrap()
    );
    assert_eq!(before["persistedCreation"], pair.native.creation);
    assert_eq!(pair.domain(request, true).await.0, StatusCode::OK);
    assert_eq!(pair.command(json!({"command":"inspect"})).await, before);
    let mut different = binding;
    different.identity.creation_id = Uuid::new_v4().to_string();
    assert_eq!(
        pair.domain(install(&different, &pair.native.creation, "another"), true)
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_refuses_wrong_token_and_recreated_native_session() {
    let mut pair = Pair::new().await;
    pair.fixture_active();
    let binding = pair.existing_binding("token-fixture");
    stale(
        pair.domain(
            install(&binding, &Uuid::new_v4().to_string(), "wrong-token"),
            true,
        )
        .await,
    );
    let recreated = pair.command(json!({"command":"recreate"})).await;
    assert_ne!(recreated["creation"], pair.native.creation);
    stale(
        pair.domain(install(&binding, &pair.native.creation, "old-root"), true)
            .await,
    );
    assert!(pair.command(json!({"command":"inspect"})).await["binding"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_refuses_stale_authenticated_peer() {
    let mut pair = Pair::new().await;
    pair.fixture_active();
    let binding = pair.existing_binding("stale-peer");
    pair.command(json!({"command":"revokeCentral"})).await;
    stale(
        pair.domain(install(&binding, &pair.native.creation, "old-peer"), true)
            .await,
    );
    assert!(pair.command(json!({"command":"inspect"})).await["binding"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn placement_refuses_bound_root_without_a_second_reservation() {
    let mut pair = Pair::new().await;
    pair.fixture_active();
    let binding = pair.existing_binding("existing-root");
    pair.command(json!({"command":"bindFixture","binding":binding}))
        .await;
    assert_eq!(pair.reserve("another-key").await.0, StatusCode::CONFLICT);
    assert_eq!(
        pair.central
            .inner
            .lock()
            .unwrap()
            .federation
            .placements
            .len(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn placement_refuses_nonroot_without_a_reservation() {
    let mut pair = Pair::new().await;
    pair.fixture_active();
    pair.command(json!({"command":"nonRoot"})).await;
    assert_eq!(pair.reserve("child").await.0, StatusCode::BAD_REQUEST);
    assert!(
        pair.central
            .inner
            .lock()
            .unwrap()
            .federation
            .placements
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn placement_refuses_absent_root_without_a_reservation() {
    let pair = Pair::new().await;
    pair.fixture_active();
    let (status, body) = pair
        .operator(
            "placements",
            json!({"nativeSessionId":"session-99999","admissionKey":"absent"}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        body.to_string().contains("session not found"),
        "handler refusal: {body}"
    );
    assert!(
        pair.central
            .inner
            .lock()
            .unwrap()
            .federation
            .placements
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn placement_installs_registers_and_recovers_original_key_without_a_second_slot() {
    let mut pair = Pair::new().await;
    pair.fixture_active();
    let original_schema = schema(&pair.central);
    let (status, body) = pair.reserve("admission").await;
    assert_eq!(status, StatusCode::OK, "placement: {body}");
    let observed = pair.command(json!({"command":"inspect"})).await;
    assert_eq!(
        observed["intentDurableBeforeInstall"], true,
        "C must retain the native token and operation key durably before INSTALL"
    );
    let binding: FederationMailboxBinding =
        serde_json::from_value(observed["binding"].clone()).unwrap();
    assert_ne!(binding.identity.creation_id, pair.native.creation);
    assert!(body.to_string().contains(&binding.participant_key()));
    let placements = pair
        .central
        .inner
        .lock()
        .unwrap()
        .federation
        .placements
        .clone();
    assert_eq!(placements.len(), 1);
    let (_, local) = federation_mailboxes::local_participant(&pair.central, "Operator");
    let rows: Vec<FederationMailboxParticipant> = pair
        .central
        .federation_mailbox_operation(
            &local.binding.identity.session_id,
            FederationMailboxOperation::Directory,
        )
        .await
        .unwrap();
    assert!(rows.iter().any(|row| row.binding == binding));
    let replay = pair.reserve("admission").await;
    assert_eq!(
        replay.0,
        StatusCode::OK,
        "replay must skip unbound-only PREPARE: {}",
        replay.1
    );
    assert_eq!(
        pair.central.inner.lock().unwrap().federation.placements,
        placements
    );
    assert_eq!(pair.command(json!({"command":"inspect"})).await, observed);
    assert_eq!(schema(&pair.central), original_schema);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn placement_refuses_capacity_exhaustion_without_installing() {
    let mut pair = Pair::new().await;
    pair.fixture_active();
    let peer = pair.peer();
    for n in 0..2 {
        pair.central
            .reserve_federation_placement(&peer, format!("full-{n}"), format!("occupied-{n}"), 0, 0)
            .unwrap();
    }
    assert_eq!(pair.reserve("overflow").await.0, StatusCode::CONFLICT);
    assert!(pair.command(json!({"command":"inspect"})).await["binding"].is_null());
    assert_eq!(
        pair.central
            .inner
            .lock()
            .unwrap()
            .federation
            .placements
            .len(),
        2
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn placement_retry_revalidates_current_membership_instead_of_returning_cached_success() {
    let pair = Pair::new().await;
    pair.fixture_active();
    assert_eq!(pair.reserve("admitted").await.0, StatusCode::OK);
    let original = pair
        .central
        .inner
        .lock()
        .unwrap()
        .federation
        .placements
        .clone();
    pair.central
        .transition_federation_peer(&pair.peer(), FederationMembershipState::Draining)
        .unwrap();
    stale(pair.reserve("admitted").await);
    assert_eq!(
        pair.central.inner.lock().unwrap().federation.placements,
        original
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn placement_retry_refuses_retired_link_without_returning_cached_success() {
    let pair = Pair::new().await;
    pair.fixture_active();
    assert_eq!(pair.reserve("retired-link").await.0, StatusCode::OK);
    let original = pair
        .central
        .inner
        .lock()
        .unwrap()
        .federation
        .placements
        .clone();
    pair.channel.invalidate();
    stale(pair.reserve("retired-link").await);
    assert_eq!(
        pair.central.inner.lock().unwrap().federation.placements,
        original
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_successful_install_reply_retries_original_intent_on_a_successor_link() {
    let mut pair = Pair::new().await;
    pair.fixture_active();
    let old = pair.channel.current_lease().unwrap().incarnation;
    let old_context = pair.channel.security_context().unwrap();
    pair.command(json!({"command":"loseInstallReply"})).await;
    let (status, pending) = pair.reserve("lost-reply").await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "ambiguous INSTALL must expose pending recovery: {pending}"
    );
    assert!(
        pending["operationKey"]
            .as_str()
            .is_some_and(|key| !key.is_empty())
    );
    let bound = pair.command(json!({"command":"inspect"})).await;
    assert!(!bound["binding"].is_null());
    assert_eq!(bound["intentDurableBeforeInstall"], true);
    let original = pair
        .central
        .inner
        .lock()
        .unwrap()
        .federation
        .placements
        .clone();
    assert!(
        pair.channel.current_lease().is_err(),
        "invalid MAC must retire the old lease"
    );
    assert!(pair.channel.security_current(&old_context).is_err());
    let original_intents = pair
        .central
        .inner
        .lock()
        .unwrap()
        .federation
        .binding_install_intents
        .clone();
    pair.replace_central_channel(true).await;
    assert_eq!(
        pair.central
            .inner
            .lock()
            .unwrap()
            .federation
            .binding_install_intents,
        original_intents
    );
    assert_ne!(pair.channel.current_lease().unwrap().incarnation, old);
    assert_eq!(pair.operator("verify", json!({})).await.0, StatusCode::OK);
    pair.activate().await;
    let (status, recovered) = pair.reserve("lost-reply").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "same key must recover bound root: {recovered}"
    );
    let now = pair
        .central
        .inner
        .lock()
        .unwrap()
        .federation
        .placements
        .clone();
    assert_eq!(now.len(), original.len());
    assert_eq!(now["lost-reply"].identity, original["lost-reply"].identity);
    assert_eq!(
        now["lost-reply"].placement_generation,
        original["lost-reply"].placement_generation
    );
    assert_eq!(
        pair.command(json!({"command":"inspect"})).await["binding"],
        bound["binding"]
    );
    let attempts = pair.command(json!({"command":"installRequests"})).await;
    let attempts = attempts.as_array().unwrap();
    assert!(attempts.len() >= 2, "lost reply must retry INSTALL");
    assert!(
        attempts.iter().all(|attempt| attempt == &attempts[0]),
        "retry must retain operation key, binding and native token across incarnations"
    );
}
