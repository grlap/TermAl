// Owns permission image durability and monotone retention, not live remote setup.
use super::*;

fn satellite() -> (AppState, FederationPromptScope) {
    let state = federation_registry::domain_state();
    let peer = federation_registry::peer(48911);
    state
        .configure_federation(FederationSettings {
            enabled: true,
            role: FederationRole::Satellite,
            central: Some(peer.identity.clone()),
        })
        .unwrap();
    state.enroll_federation_peer(peer.clone()).unwrap();
    let session = test_session_id(&state, Agent::Codex);
    let inner = state.inner.lock().unwrap();
    let target = FederationMutationContext::locked(&inner)
        .prompt_native_root(&session)
        .unwrap();
    let local = inner.federation.local.clone().unwrap();
    drop(inner);
    (
        state,
        FederationPromptScope {
            source_host: peer.identity.clone(),
            source: FederationSessionIdentity {
                host_id: peer.identity.host_id,
                session_id: "source".into(),
                creation_id: Uuid::new_v4().to_string(),
            },
            target_host: local,
            target,
            enrollment_generation: 1,
            placement_generation: None,
        },
    )
}

fn version(state: &AppState) -> u64 {
    state.inner.lock().unwrap().federation.version
}

#[test]
fn federation_prompt_policy_consent_exact_replay_is_a_durable_history_read() {
    let (state, scope) = satellite();
    let original_version = version(&state);
    let consent = state
        .issue_federation_prompt_consent(original_version, "exact-replay".into(), scope.clone())
        .unwrap();
    let after_issue = version(&state);
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    let data_version = || {
        connection
            .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
            .unwrap()
    };
    let before_retry = data_version();
    assert_eq!(state.issue_federation_prompt_consent(
        original_version, "exact-replay".into(), scope.clone(),
    ).unwrap(), consent);
    assert_eq!(version(&state), after_issue);
    assert_eq!(data_version(), before_retry);
    let mut conflicting = scope.clone();
    conflicting.target.creation_id = Uuid::new_v4().to_string();
    assert!(
        state
            .issue_federation_prompt_consent(original_version, "exact-replay".into(), conflicting,)
            .is_err()
    );
    assert!(
        state
            .issue_federation_prompt_consent(original_version, "new-key".into(), scope.clone(),)
            .is_err()
    );
    let revoked = state
        .revoke_federation_prompt_permission(after_issue, &consent.identity.id)
        .unwrap();
    let after_revocation = version(&state);
    let revoked_data_version = data_version();
    assert_eq!(
        state
            .issue_federation_prompt_consent(original_version, "exact-replay".into(), scope,)
            .unwrap(),
        revoked
    );
    assert!(revoked.revoked);
    assert_eq!(version(&state), after_revocation);
    assert_eq!(data_version(), revoked_data_version);
    let image = read_federation_persist_image(&connection).unwrap().unwrap();
    verify_federation_image_marker(&connection, &image).unwrap();
}

#[test]
fn federation_prompt_policy_consent_and_revocation_use_primary_federation_image() {
    let (state, scope) = satellite();
    let before = version(&state);
    let consent = state
        .issue_federation_prompt_consent(before, "operator-consent".into(), scope.clone())
        .unwrap();
    assert!(version(&state) > before);
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    let image = read_federation_persist_image(&connection).unwrap().unwrap();
    assert_eq!(
        image["domain"]["promptPermissions"]["records"][0],
        serde_json::to_value(&consent).unwrap()
    );
    verify_federation_image_marker(&connection, &image).unwrap();
    let retained: FederationState = serde_json::from_value(image["domain"].clone()).unwrap();
    retained.validate().unwrap();
    assert!(state.inner.lock().unwrap().federation_durable);
    let duplicate = state
        .issue_federation_prompt_consent(version(&state), "operator-consent".into(), scope.clone())
        .unwrap();
    assert_eq!(consent, duplicate);
    let revoked = state
        .revoke_federation_prompt_permission(version(&state), &consent.identity.id)
        .unwrap();
    assert!(revoked.revoked);
    assert_eq!(revoked.identity.generation, 2);
    assert_eq!(
        revoked.receiver_observed_generation, None,
        "local revocation is not remote observation"
    );
    assert_eq!(
        state
            .issue_federation_prompt_consent(version(&state), "operator-consent".into(), scope)
            .unwrap(),
        revoked,
        "reusing an issuance key cannot revive the old identity"
    );
    let image = read_federation_persist_image(&connection).unwrap().unwrap();
    assert_eq!(
        image["domain"]["promptPermissions"]["records"][0]["revoked"],
        true
    );
    verify_federation_image_marker(&connection, &image).unwrap();
}

#[test]
fn federation_prompt_policy_issuance_is_version_and_native_creation_fenced() {
    let (state, scope) = satellite();
    let current = version(&state);
    assert!(
        state
            .issue_federation_prompt_consent(current - 1, "stale".into(), scope.clone())
            .is_err()
    );
    let mut successor = scope.clone();
    successor.target.creation_id = Uuid::new_v4().to_string();
    assert!(
        state
            .issue_federation_prompt_consent(current, "wrong-creation".into(), successor)
            .is_err()
    );
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&scope.target.session_id).unwrap();
        inner.sessions[index].hidden = true;
    }
    assert!(
        state
            .issue_federation_prompt_consent(current, "hidden".into(), scope)
            .is_err()
    );
    assert!(
        state
            .inner
            .lock()
            .unwrap()
            .federation
            .prompt_permissions
            .is_none()
    );
}

#[test]
fn federation_prompt_policy_retention_cannot_retarget_revive_or_downgrade() {
    let (state, scope) = satellite();
    let consent = state
        .issue_federation_prompt_consent(version(&state), "keep".into(), scope)
        .unwrap();
    let mut policy = FederationPromptPermissions::default();
    policy.retain(consent.clone()).unwrap();
    let mut changed = consent.clone();
    changed.scope.target.creation_id = Uuid::new_v4().to_string();
    assert!(policy.retain(changed).is_err());
    let mut revoked = consent.clone();
    revoked.revoked = true;
    revoked.identity.generation = 2;
    policy.retain(revoked.clone()).unwrap();
    assert!(policy.retain(consent).is_err());
    revoked.receiver_observed_generation = Some(2);
    policy.retain(revoked.clone()).unwrap();
    revoked.receiver_observed_generation = Some(1);
    assert!(policy.retain(revoked).is_err());
    let retained = policy.find(&policy.records[0].identity.id).unwrap();
    assert!(retained.revoked && retained.receiver_observed_generation == Some(2));
}

#[test]
fn federation_prompt_policy_scope_and_duplicate_records_fail_closed() {
    let (state, scope) = satellite();
    let consent = state
        .issue_federation_prompt_consent(version(&state), "bounded".into(), scope)
        .unwrap();
    let mut duplicate = FederationPromptPermissions {
        schema_version: 1,
        records: vec![consent.clone(), consent.clone()],
    };
    assert!(duplicate.validate().is_err());
    duplicate.records[1].identity.id = Uuid::new_v4().to_string();
    assert!(
        duplicate.validate().is_err(),
        "one creation key has one identity"
    );
    let mut wire = serde_json::to_value(&consent).unwrap();
    wire["scope"]["placementGeneration"] = json!(0);
    assert!(
        serde_json::from_value::<FederationPromptPermission>(wire)
            .unwrap()
            .validate()
            .is_err()
    );
    let mut wire = serde_json::to_value(&consent).unwrap();
    wire["scope"]["extra"] = json!(true);
    assert!(serde_json::from_value::<FederationPromptPermission>(wire).is_err());
    let encoded = serde_json::to_value(&consent).unwrap();
    for field in ["consent", "receiverObservedGeneration"] {
        let mut missing = encoded.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<FederationPromptPermission>(missing).is_err(),
            "nullable {field} remains required on the wire and in retained policy"
        );
    }
}
