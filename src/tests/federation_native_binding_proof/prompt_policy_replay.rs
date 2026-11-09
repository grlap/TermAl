// Owns historical grant recovery through the real permission and MAC fixture.
// Does not grant new execution authority or replace fresh-proof admission.
// New module using the policy fixture from prompt_policy.rs.
use super::*;

fn data_version(connection: &rusqlite::Connection) -> i64 {
    connection
        .query_row("PRAGMA data_version", [], |row| row.get(0))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_prompt_policy_concurrent_issuance_replays_at_the_owner_cut_without_writing() {
    let mut fixture = policy_fixture(true).await;
    let scope = scope(&fixture, true);
    let consent = consent(&fixture, &scope);
    let initial_version = version(&fixture.central);
    let (started, observed) = std::sync::mpsc::channel();
    let (resume, resumed) = std::sync::mpsc::channel();
    let resumed = Mutex::new(resumed);
    let paused = std::sync::atomic::AtomicBool::new(false);
    let mut channel = fixture.channel.clone();
    channel.control.observed = Arc::new(move |phase, _| {
        if phase == "promptPolicyObservationStarted"
            && !paused.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            // The entrypoint already missed the creation key. Another real
            // issuer now commits it before this request reaches its owner cut.
            started.send(()).unwrap();
            resumed
                .lock()
                .unwrap()
                .recv_timeout(phase_sync::DEADLOCK_GUARD)
                .unwrap();
        }
    });
    let loser_state = fixture.central.clone();
    let loser_rpc = fixture.rpc.clone();
    let loser_scope = scope.clone();
    let loser_consent = consent.identity.clone();
    let loser_lease = channel.current_lease().unwrap();
    let loser = tokio::spawn(async move {
        loser_state
            .issue_federation_prompt_grant_on_link(
                initial_version,
                "concurrent-grant".into(),
                loser_scope,
                loser_consent,
                &loser_lease,
                &loser_rpc,
            )
            .await
    });
    tokio::task::spawn_blocking(move || {
        observed.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
    })
    .await
    .unwrap();
    let winner = fixture
        .central
        .issue_federation_prompt_grant_on_link(
            initial_version,
            "concurrent-grant".into(),
            scope,
            consent.identity,
            &fixture.channel.current_lease().unwrap(),
            &fixture.rpc,
        )
        .await;
    let after_winner = version(&fixture.central);
    let connection =
        rusqlite::Connection::open(fixture.central.persistence_path.as_path()).unwrap();
    let after_write = data_version(&connection);
    resume.send(()).unwrap();
    let recovered = loser.await.unwrap().unwrap();
    let winner = winner.unwrap();
    assert_eq!(recovered, winner);
    assert_eq!(version(&fixture.central), after_winner);
    assert_eq!(data_version(&connection), after_write);
    {
        let inner = fixture.central.inner.lock().unwrap();
        let retained = &inner
            .federation
            .prompt_permissions
            .as_ref()
            .unwrap()
            .records;
        assert_eq!(
            retained
                .iter()
                .filter(
                    |record| record.kind == FederationPromptPermissionKind::CallerGrant
                        && record.creation_key == "concurrent-grant"
                )
                .count(),
            1
        );
    }
    let image = read_federation_persist_image(&connection).unwrap().unwrap();
    verify_federation_image_marker(&connection, &image).unwrap();
    fixture.server.finish().await;
}

#[tokio::test]
async fn federation_prompt_policy_grant_replay_survives_revoked_consent_without_another_write() {
    let mut fixture = policy_fixture(true).await;
    let scope = scope(&fixture, true);
    let consent = consent(&fixture, &scope);
    let lease = fixture.channel.current_lease().unwrap();
    let original_version = version(&fixture.central);
    let grant = fixture
        .central
        .issue_federation_prompt_grant_on_link(
            original_version,
            "retained-grant".into(),
            scope.clone(),
            consent.identity.clone(),
            &lease,
            &fixture.rpc,
        )
        .await
        .unwrap();
    let after_issue = version(&fixture.central);
    let connection =
        rusqlite::Connection::open(fixture.central.persistence_path.as_path()).unwrap();
    let before_retry = data_version(&connection);
    assert_eq!(
        fixture
            .central
            .issue_federation_prompt_grant_on_link(
                original_version,
                "retained-grant".into(),
                scope.clone(),
                consent.identity.clone(),
                &lease,
                &fixture.rpc,
            )
            .await
            .unwrap(),
        grant
    );

    fixture
        .satellite
        .revoke_federation_prompt_permission(version(&fixture.satellite), &consent.identity.id)
        .unwrap();
    for expected_version in [original_version, after_issue] {
        assert_eq!(
            fixture
                .central
                .issue_federation_prompt_grant_on_link(
                    expected_version,
                    "retained-grant".into(),
                    scope.clone(),
                    consent.identity.clone(),
                    &lease,
                    &fixture.rpc,
                )
                .await
                .unwrap(),
            grant
        );
    }
    assert_eq!(version(&fixture.central), after_issue);
    assert_eq!(data_version(&connection), before_retry);

    // Historical recovery is not permission to issue under a revoked consent.
    assert!(
        fixture
            .central
            .issue_federation_prompt_grant_on_link(
                after_issue,
                "new-grant".into(),
                scope.clone(),
                consent.identity.clone(),
                &lease,
                &fixture.rpc,
            )
            .await
            .is_err()
    );
    let mut changed_scope = scope.clone();
    changed_scope.target.creation_id = Uuid::new_v4().to_string();
    assert!(
        fixture
            .central
            .issue_federation_prompt_grant_on_link(
                after_issue,
                "retained-grant".into(),
                changed_scope,
                consent.identity.clone(),
                &lease,
                &fixture.rpc,
            )
            .await
            .is_err()
    );
    let mut changed_consent = consent.identity.clone();
    changed_consent.generation += 1;
    assert!(
        fixture
            .central
            .issue_federation_prompt_grant_on_link(
                after_issue,
                "retained-grant".into(),
                scope.clone(),
                changed_consent,
                &lease,
                &fixture.rpc,
            )
            .await
            .is_err()
    );
    assert_eq!(version(&fixture.central), after_issue);
    assert_eq!(data_version(&connection), before_retry);

    let revoked = fixture
        .central
        .revoke_federation_prompt_permission(after_issue, &grant.identity.id)
        .unwrap();
    let after_revocation = version(&fixture.central);
    let revoked_data_version = data_version(&connection);
    fixture.server.finish().await;
    assert!(
        fixture
            .central
            .federation_peer_handles
            .lock()
            .unwrap()
            .is_empty()
    );
    // The actual operator entrypoint recovers history without a peer handle,
    // listener or fresh lease. It returns the revoked record without revival.
    assert_eq!(
        fixture
            .central
            .issue_federation_prompt_grant(
                original_version,
                "retained-grant".into(),
                scope,
                consent.identity,
            )
            .await
            .unwrap(),
        revoked
    );
    assert!(revoked.revoked);
    assert_eq!(revoked.identity.id, grant.identity.id);
    assert_eq!(version(&fixture.central), after_revocation);
    assert_eq!(data_version(&connection), revoked_data_version);
    let image = read_federation_persist_image(&connection).unwrap().unwrap();
    verify_federation_image_marker(&connection, &image).unwrap();
    let domain: FederationState = serde_json::from_value(image["domain"].clone()).unwrap();
    assert_eq!(
        domain.prompt_permissions.unwrap().find(&grant.identity.id),
        Some(&revoked)
    );
}

#[tokio::test]
async fn federation_prompt_policy_grant_replay_refuses_unsettled_or_foreign_local_history() {
    let mut fixture = policy_fixture(true).await;
    let scope = scope(&fixture, true);
    let consent = consent(&fixture, &scope);
    let lease = fixture.channel.current_lease().unwrap();
    let original_version = version(&fixture.central);
    let grant = fixture
        .central
        .issue_federation_prompt_grant_on_link(
            original_version,
            "settled-grant".into(),
            scope.clone(),
            consent.identity.clone(),
            &lease,
            &fixture.rpc,
        )
        .await
        .unwrap();
    {
        fixture.central.inner.lock().unwrap().federation_durable = false;
    }
    let error = fixture
        .central
        .issue_federation_prompt_grant(
            original_version,
            "settled-grant".into(),
            scope.clone(),
            consent.identity.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::LOCKED);
    {
        let mut inner = fixture.central.inner.lock().unwrap();
        inner.federation_durable = true;
        inner.federation_continuity_lost = true;
    }
    assert!(
        fixture
            .central
            .issue_federation_prompt_grant(
                original_version,
                "settled-grant".into(),
                scope.clone(),
                consent.identity.clone(),
            )
            .await
            .is_err()
    );
    {
        let mut inner = fixture.central.inner.lock().unwrap();
        inner.federation_continuity_lost = false;
        inner.federation.local.as_mut().unwrap().host_generation += 1;
    }
    assert!(
        fixture
            .central
            .issue_federation_prompt_grant(
                original_version,
                "settled-grant".into(),
                scope,
                consent.identity,
            )
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .central
            .inner
            .lock()
            .unwrap()
            .federation
            .prompt_permissions
            .as_ref()
            .unwrap()
            .find(&grant.identity.id),
        Some(&grant)
    );
    fixture.server.finish().await;
}
