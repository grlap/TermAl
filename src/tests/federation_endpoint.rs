// Address-only operator transitions use the real domain writer and router.
// No OS credentials, network discovery, pairing or live host changes.
use super::*;
use super::operator::{operator_request, operator_view};

#[tokio::test]
async fn federation_hostname_update_preserves_enrollment_and_fences_old_transport() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = central();
    let mut record = peer(47003);
    record.endpoint.ssh_host = "192.0.2.15".into();
    let old = active(&state, record);
    let before = operator_view(&state).await;
    let version = before["version"].as_u64().unwrap();
    let path = format!("/api/federation/peers/{}/ssh-host", old.peer.host_id);
    let after = operator_request(&state, "POST", &path,
        Some(json!({"expectedVersion": version, "sshHost": "satellite.local"})), StatusCode::OK).await;
    let mut expected = before["peers"][0].clone();
    expected["endpoint"]["sshHost"] = json!("satellite.local");
    expected["recordGeneration"] = json!(old.record_generation + 1);
    expected["transportGeneration"] = json!(old.transport_generation + 1);
    expected["verificationCurrent"] = json!(false);
    assert_eq!(after["peers"][0], expected,
        "only the locator and local fences change; identity, enrollment and keys survive");
    assert_eq!(after["version"], version + 1);
    assert_eq!(after["trackingDurability"], "durable");
    assert!(state.ensure_federation_peer_current(&old).is_err());
    let current = state.federation_peer_snapshot(&old.peer.host_id).unwrap();
    assert!(state.refresh_federation_transport_snapshot(&old,
        before["peers"][0]["lastObserved"]["linkIncarnation"].as_str().unwrap()).is_err());
    assert!(state.federation_reconnect_snapshot(&current).is_ok());
    // Existing readers deserialize exactly the same endpoint schema and can
    // reconnect using the new name; no new reader floor or address cache.
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    let image = read_federation_persist_image(&connection).unwrap().unwrap();
    let loaded: FederationState = serde_json::from_value(image["domain"].clone()).unwrap();
    let stored = loaded.peers.get(&old.peer.host_id).unwrap();
    assert_eq!(stored.endpoint.ssh_host, "satellite.local");
    assert_eq!(stored.enrollment_generation, old.enrollment_generation);
    assert_eq!(stored.pair_capability_ref, old.pair_capability_ref);
    assert_eq!(stored.membership, old.membership);
}

#[tokio::test]
async fn federation_hostname_update_rejects_stale_versions_numeric_and_unrelated_fields() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = central();
    let record = peer(47003);
    let id = record.identity.host_id.clone();
    state.enroll_federation_peer(record).unwrap();
    let before = operator_view(&state).await;
    let version = before["version"].as_u64().unwrap();
    let path = format!("/api/federation/peers/{id}/ssh-host");
    for hostname in ["192.0.2.25", "0xc000020f", "0xc0.0x00.0x02.0x0f", "0x0a.0.0.19",
        "0X0A000013", "0x0a000013.", "::1", "[::1]", "", "-option.local", "x\ny.local", "a..local", "a/b.local"] {
        operator_request(&state, "POST", &path,
            Some(json!({"expectedVersion": version, "sshHost": hostname})), StatusCode::BAD_REQUEST).await;
    }
    operator_request(&state, "POST", &path,
        Some(json!({"expectedVersion": version - 1, "sshHost": "current.local"})), StatusCode::CONFLICT).await;
    // Unknown JSON fields follow the shared extractor convention, just as
    // federation settings do; invalid hostname values above remain 400.
    operator_request(&state, "POST", &path,
        Some(json!({"expectedVersion": version, "sshHost": "current.local", "pairCapabilityRef": "different"})), StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(operator_view(&state).await, before);
    let expected = state.federation_peer_snapshot(&id).unwrap();
    state.transition_federation_peer(&expected, FederationMembershipState::Revoked).unwrap();
    let revoked = operator_view(&state).await;
    operator_request(&state, "POST", &path,
        Some(json!({"expectedVersion": revoked["version"], "sshHost": "current.local"})), StatusCode::CONFLICT).await;
    assert_eq!(operator_view(&state).await, revoked);
}

#[test]
fn federation_hostname_validation_keeps_dns_resolution_in_each_new_ssh_child() {
    for hostname in ["DESKTOP-EXAMPLE.local", "peer.example.org", "peer.example.org.", "peer", "0xabc.local"] {
        validate_federation_ssh_hostname(hostname).unwrap();
    }
    // A numeric address represented with a trailing root dot is not a DNS name.
    assert!(validate_federation_ssh_hostname("192.0.2.1.").is_err());
}

fn mailbox_reservation(state: &AppState, peer: &FederationPeerSnapshot) -> FederationPlacement {
    let native_creation = Uuid::new_v4().to_string();
    state.reserve_federation_placement_kind(peer, "retained-install".into(),
        "session-endpoint".into(), 0, 0, None, true,
        |domain, _, placement| {
            let intent = FederationBindingInstallIntent {
                operation_key: placement.admission_key.clone(),
                native_creation_id: native_creation,
                binding: FederationMailboxBinding {
                    central: peer.central.clone(), identity: placement.identity.clone(),
                    placement_generation: placement.placement_generation,
                },
                admission_key: placement.admission_key.clone(),
                execution_basis: placement.execution_basis.clone(),
                peer_enrollment_generation: peer.enrollment_generation,
                peer_verification_basis: placement.execution_basis.clone(),
                configuration_basis: peer.configuration_basis(),
            };
            intent.validate()?;
            domain.binding_install_intents.insert(placement.admission_key.clone(), intent);
            Ok(placement.clone())
        }).unwrap()
}

#[test]
fn federation_hostname_update_preserves_retained_mailbox_intent_but_never_executor_authority() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = central();
    let old = active(&state, peer(47003));
    let mailbox = mailbox_reservation(&state, &old);
    let executor = state.reserve_federation_placement(&old, "executor-original".into(),
        "session-executor".into(), 1, 0).unwrap();
    let (version, original_intent) = {
        let inner = state.inner.lock().unwrap();
        (inner.federation.version, inner.federation.binding_install_intents[&mailbox.admission_key].clone())
    };
    state.update_federation_ssh_host(&old.peer.host_id, version, "moved.local".into()).unwrap();
    let (migrated, intent, saved_executor, disconnected_authority) = {
        let inner = state.inner.lock().unwrap();
        let migrated = inner.federation.placements[&mailbox.admission_key].clone();
        (migrated.clone(), inner.federation.binding_install_intents[&mailbox.admission_key].clone(),
            inner.federation.placements[&executor.admission_key].clone(),
            federation_mailbox_placement_authority(&inner.federation, &inner.preferences.federation, &migrated))
    };
    let mut expected = mailbox.clone();
    expected.peer.endpoint.ssh_host = "moved.local".into();
    assert_eq!(migrated, expected);
    let mut expected_intent = original_intent;
    expected_intent.configuration_basis = migrated.peer.configuration_basis();
    assert_eq!(intent, expected_intent, "original binding, creation, key and execution proof remain");
    assert_eq!(saved_executor, executor, "executor placements are never rebased");
    assert!(!disconnected_authority, "address migration alone cannot grant mailbox admission");
    // The existing predecessor validator is unchanged, including its required
    // placement/INSTALL configuration equality. Reload the committed image.
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    let image = read_federation_persist_image(&connection).unwrap().unwrap();
    let loaded: FederationState = serde_json::from_value(image["domain"].clone()).unwrap();
    loaded.validate().unwrap();
    assert_eq!(loaded.placements[&mailbox.admission_key], migrated);
    assert_eq!(loaded.binding_install_intents[&mailbox.admission_key], intent);
    let current = state.federation_peer_snapshot(&old.peer.host_id).unwrap();
    let connected = state.federation_peer_connected(&current, Uuid::new_v4().to_string()).unwrap();
    let (ordinary, execution, original_intent_current) = {
        let inner = state.inner.lock().unwrap();
        (federation_mailbox_placement_authority(&inner.federation, &inner.preferences.federation, &migrated),
            federation_placement_authority(&inner.federation, &inner.preferences.federation, &executor),
            intent.ensure_authority(&inner.federation, &connected, &migrated).is_ok())
    };
    assert!(ordinary && original_intent_current, "the same pending binding recovers on a fresh authenticated link");
    assert!(!execution, "fresh mailbox transport never promotes old execution proof");
    assert_eq!(connected.enrollment_generation, old.enrollment_generation);
}

#[test]
fn federation_hostname_update_refuses_invalid_retained_pair_without_partial_install() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = central();
    let old = active(&state, peer(47003));
    let mailbox = mailbox_reservation(&state, &old);
    let before = {
        let mut inner = state.inner.lock().unwrap();
        inner.federation.binding_install_intents.get_mut(&mailbox.admission_key).unwrap()
            .configuration_basis = "b".repeat(64);
        inner.federation.clone()
    };
    assert!(state.update_federation_ssh_host(&old.peer.host_id, before.version, "moved.local".into()).is_err());
    let after = state.inner.lock().unwrap().federation.clone();
    assert_eq!(serde_json::to_value(after).unwrap(), serde_json::to_value(before).unwrap());
}

#[test]
fn federation_hostname_aba_never_revives_a_captured_credential_basis() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = central();
    let original = connected(&state, peer(47003));
    let basis = Arc::new(FederationTransportBasis::new(original.clone()));
    let (_stop, stop) = tokio::sync::watch::channel(false);
    let original_name = original.endpoint.ssh_host.clone();
    for host in ["moved.local".to_owned(), original_name] {
        let version = state.inner.lock().unwrap().federation.version;
        state.update_federation_ssh_host(&original.peer.host_id, version, host).unwrap();
    }
    let current = state.federation_peer_snapshot(&original.peer.host_id).unwrap();
    assert_eq!(current.endpoint, original.endpoint);
    assert_ne!(current.transport_generation, original.transport_generation);
    assert!(federation_validate_credential_basis(&state, &original, &basis, &stop).is_err());
    assert!(state.federation_reconnect_snapshot(&current).is_ok());
}

#[test]
fn federation_hostname_update_cannot_adopt_a_retained_mailbox_from_another_authority() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for changed in ["key", "enrollment", "endpoint", "host-generation"] {
        let state = central();
        let old = active(&state, peer(47003));
        let mailbox = mailbox_reservation(&state, &old);
        let before = {
            let mut inner = state.inner.lock().unwrap();
            let placement = inner.federation.placements.get_mut(&mailbox.admission_key).unwrap();
            match changed {
                "key" => placement.peer.pair_capability_ref = "other-key".into(),
                "enrollment" => placement.peer.enrollment_generation += 1,
                "endpoint" => placement.peer.endpoint.ssh_host = "other.local".into(),
                "host-generation" => placement.peer.peer.host_generation += 1,
                _ => unreachable!(),
            }
            // Keep the old placement/INSTALL pair internally valid. It still
            // must not inherit the current peer's authorized locator update.
            let generation = placement.peer.enrollment_generation;
            let configuration = placement.peer.configuration_basis();
            let intent = inner.federation.binding_install_intents.get_mut(&mailbox.admission_key).unwrap();
            intent.peer_enrollment_generation = generation;
            intent.configuration_basis = configuration;
            inner.federation.clone()
        };
        before.validate().unwrap();
        state.update_federation_ssh_host(&old.peer.host_id, before.version, "moved.local".into()).unwrap();
        let current = state.federation_peer_snapshot(&old.peer.host_id).unwrap();
        let connected = state.federation_peer_connected(&current, Uuid::new_v4().to_string()).unwrap();
        let (after, settings) = {
            let inner = state.inner.lock().unwrap();
            (inner.federation.clone(), inner.preferences.federation.clone())
        };
        after.validate().unwrap();
        assert_eq!(current.endpoint.ssh_host, "moved.local", "{changed}");
        assert_eq!(after.placements, before.placements, "{changed}: no historical placement adoption");
        assert_eq!(after.binding_install_intents, before.binding_install_intents, "{changed}: no intent rebase");
        let retained = &after.placements[&mailbox.admission_key];
        assert!(!federation_mailbox_placement_authority(&after, &settings, retained), "{changed}");
        assert!(after.binding_install_intents[&mailbox.admission_key]
            .ensure_authority(&after, &connected, retained).is_err(), "{changed}");
    }
}

#[test]
fn federation_hostname_update_skips_historical_mailbox_after_reenrollment() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = central();
    let record = peer(47003);
    let old = active(&state, record.clone());
    let mailbox = mailbox_reservation(&state, &old);
    let original_intent = state.inner.lock().unwrap().federation.binding_install_intents[&mailbox.admission_key].clone();
    // Re-enrollment is a real earlier authority change. A closed native root
    // never participates again, so its old reservation remains unretired.
    state.enroll_federation_peer(record).unwrap();
    let reenrolled = state.federation_peer_snapshot(&old.peer.host_id).unwrap();
    assert_eq!(reenrolled.enrollment_generation, old.enrollment_generation + 1);
    let version = state.inner.lock().unwrap().federation.version;
    state.update_federation_ssh_host(&old.peer.host_id, version, "recovered.local".into()).unwrap();
    let current = state.federation_peer_snapshot(&old.peer.host_id).unwrap();
    let connected = state.federation_peer_connected(&current, Uuid::new_v4().to_string()).unwrap();
    let (after, settings) = {
        let inner = state.inner.lock().unwrap();
        (inner.federation.clone(), inner.preferences.federation.clone())
    };
    after.validate().unwrap();
    assert_eq!(connected.endpoint.ssh_host, "recovered.local");
    assert_eq!(connected.enrollment_generation, reenrolled.enrollment_generation);
    assert_eq!(connected.pair_capability_ref, reenrolled.pair_capability_ref);
    assert_eq!(after.placements[&mailbox.admission_key], mailbox);
    assert_eq!(after.binding_install_intents[&mailbox.admission_key], original_intent);
    assert!(!federation_mailbox_placement_authority(&after, &settings, &mailbox));
    assert!(original_intent.ensure_authority(&after, &connected, &mailbox).is_err());
    // The persisted predecessor-readable pair also remains whole and inert.
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    let image = read_federation_persist_image(&connection).unwrap().unwrap();
    let loaded: FederationState = serde_json::from_value(image["domain"].clone()).unwrap();
    loaded.validate().unwrap();
    assert_eq!(loaded.placements[&mailbox.admission_key], mailbox);
    assert_eq!(loaded.binding_install_intents[&mailbox.admission_key], original_intent);
}
