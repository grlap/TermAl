// Exercises enrolled compatibility across rebuilds using authenticated loopback
// peers and real mailbox/direct-prompt admission. No live hosts or providers.
use super::federation_onboarding_fixture::{Pair, image, io, profile};
use super::*;

#[path = "federation_primary2_fixture.rs"]
mod primary2_fixture;
use primary2_fixture::reload_primary2_host;

pub(super) fn measured(profile: Arc<Mutex<FederationExecutionProfile>>) -> FederationOnboardingIo {
    let mut measured = io();
    let reported = profile.clone();
    measured.profile = Arc::new(move || Ok(reported.lock().unwrap().clone()));
    measured.image = Arc::new(move || {
        let FederationImageObservation::Known(mut image) = image() else {
            unreachable!()
        };
        image.sha256 = profile.lock().unwrap().binary_sha256.clone();
        FederationImageObservation::Known(image)
    });
    measured
}

async fn rebuilt_delivery(central_rebuilt: bool, reconnect: bool, restart: bool, legacy: bool) {
    rebuilt_delivery_case(central_rebuilt, reconnect, restart, legacy, false).await;
}

fn legacy_enrollment_profile() -> FederationExecutionProfile {
    // Frozen enrolled profile from the primary-2 generation, deliberately
    // independent of every current schema constant and profile producer.
    serde_json::from_value(json!({
        "protocolVersion": 1, "schemaVersion": "2",
        "binarySha256": "a".repeat(64), "toolchainIdentity": "fake-platform-rustc",
    }))
    .unwrap()
}

async fn rebuilt_delivery_case(
    central_rebuilt: bool,
    reconnect: bool,
    restart: bool,
    legacy: bool,
    storage_transition: bool,
) {
    let central = federation_mailboxes::central_store();
    let (local, recipient) = federation_mailboxes::local_participant(&central, "Recipient");
    let enrolled = if storage_transition {
        legacy_enrollment_profile()
    } else {
        profile()
    };
    let central_profile = Arc::new(Mutex::new(enrolled.clone()));
    let satellite_profile = Arc::new(Mutex::new(enrolled.clone()));
    let mut pair = Pair::with_enrolled_profile(
        central,
        measured(satellite_profile.clone()),
        measured(central_profile.clone()),
        enrolled,
    )
    .await;
    // Enrollment retains the old build. Simulate either peer restarting with
    // a new image, keeping protocol, schema, keys and host/enrollment intact.
    let changed = if central_rebuilt {
        central_profile.clone()
    } else {
        satellite_profile.clone()
    };
    if !reconnect {
        changed.lock().unwrap().binary_sha256 = "b".repeat(64);
    }
    let report = pair.verify().await.unwrap();
    assert!(
        report.termal_complete(),
        "hash-only rebuild: {:?}",
        report.outcomes
    );
    if !central_rebuilt && !reconnect {
        let FederationImageObservation::Known(observed) = &report.running_image else {
            panic!("new build must remain observed evidence")
        };
        assert_eq!(observed.sha256, "b".repeat(64));
    }
    if central_rebuilt && !reconnect {
        let FederationImageObservation::Known(observed) = &report.reverse_running_image else {
            panic!("central build must remain observed evidence")
        };
        assert_eq!(observed.sha256, "b".repeat(64));
    }
    let peer = pair.central.federation_peer_snapshot(&pair.id).unwrap();
    pair.central
        .transition_federation_peer(&peer, FederationMembershipState::Active)
        .unwrap();
    let peer = pair.central.federation_peer_snapshot(&pair.id).unwrap();
    let native = test_session_id(&pair.satellite, Agent::Codex);
    let (status, Json(installed)) = reserve_federation_mailbox_placement_route(
        AxumPath(pair.id.clone()),
        State(pair.central.clone()),
        Json(FederationPlacementRequest {
            native_session_id: native.clone(),
            admission_key: "rebuilt-delivery".to_owned(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK);
    let placement: FederationPlacement =
        serde_json::from_value(installed["placement"].clone()).unwrap();
    let original_intent = pair
        .central
        .inner
        .lock()
        .unwrap()
        .federation
        .binding_install_intents["rebuilt-delivery"]
        .clone();
    let actor = FederationMailboxActor {
        binding: FederationMailboxBinding {
            central: peer.central.clone(),
            identity: placement.identity,
            placement_generation: placement.placement_generation,
        },
        name: "Rebuilt peer".to_owned(),
    };
    let old_lease = pair.satellite_channel.current_lease().unwrap();
    if reconnect {
        if legacy {
            pair.central
                .mutate_federation(|domain, _| {
                    let peer = domain.peers.get_mut(&pair.id).unwrap();
                    let basis = peer.legacy_verification_basis();
                    peer.verification.as_mut().unwrap().basis = basis.clone();
                    domain.onboarding.reports.get_mut(&pair.id).unwrap().basis = basis.clone();
                    domain
                        .placements
                        .get_mut("rebuilt-delivery")
                        .unwrap()
                        .execution_basis = basis.clone();
                    let intent = domain
                        .binding_install_intents
                        .get_mut("rebuilt-delivery")
                        .unwrap();
                    intent.execution_basis = basis.clone();
                    intent.peer_verification_basis = basis;
                    Ok(())
                })
                .unwrap();
        }
        changed.lock().unwrap().binary_sha256 = "b".repeat(64);
        pair.lease.channel.invalidate();
        pair.satellite_channel.invalidate();
        if storage_transition {
            let mut current = federation_execution_profile_with_probe(
                &std::env::current_dir().unwrap(),
                &FederationProcessImage::capture(None),
                |_, _, _| Ok("updated-platform-rustc".to_owned()),
            )
            .unwrap();
            assert_eq!(current.schema_version, "2");
            current.binary_sha256 = "b".repeat(64);
            *central_profile.lock().unwrap() = current.clone();
            *satellite_profile.lock().unwrap() = current;
            let (central, satellite, listeners) = pair.suspend().await;
            reload_primary2_host(&central);
            reload_primary2_host(&satellite);
            pair = Pair::resume(
                central,
                satellite,
                listeners,
                measured(satellite_profile.clone()),
                measured(central_profile.clone()),
            )
            .await;
        } else if restart {
            // Reload the durable store as boot does, preserving enrollment,
            // reservation and native identities rather than constructing peers.
            *pair.central.inner.lock().unwrap() =
                load_state(pair.central.persistence_path.as_path())
                    .unwrap()
                    .unwrap();
        }
        if !storage_transition {
            pair.reconnect().await;
        }
        assert_eq!(
            pair.central
                .federation_peer_snapshot(&pair.id)
                .unwrap()
                .membership,
            FederationMembershipState::Active
        );
        // Health checks the authenticated pair, without refreshing a diagnostic
        // report or transferring execution authority to the new incarnation.
        pair.rpc.health(&pair.lease).await.unwrap();
        let inner = pair.central.inner.lock().unwrap();
        let peer = &inner.federation.peers[&pair.id];
        assert_eq!(peer.membership, FederationMembershipState::Active);
        assert!(!peer.verified_on(inner.federation.links.get(&pair.id)));
        assert!(peer.requalification.is_some());
        let intent = &inner.federation.binding_install_intents["rebuilt-delivery"];
        assert_eq!(intent.operation_key, original_intent.operation_key);
        assert_eq!(
            intent.native_creation_id,
            original_intent.native_creation_id
        );
        assert_eq!(intent.binding, original_intent.binding);
        let expected_basis = if legacy { peer.legacy_verification_basis() } else { peer.verification_basis() };
        assert_eq!(intent.execution_basis, expected_basis);
        assert_eq!(intent.peer_verification_basis, expected_basis);
        assert_eq!(
            inner.federation.placements["rebuilt-delivery"].identity,
            actor.binding.identity
        );
        {
            let report = &inner.federation.onboarding.reports[&pair.id];
            let observation = if central_rebuilt {
                &report.reverse_running_image
            } else {
                &report.running_image
            };
            let FederationImageObservation::Known(image) = observation else {
                panic!("reconnected image must remain diagnostic evidence")
            };
            assert_eq!(image.sha256, "a".repeat(64), "ordinary health does not fabricate a new report");
        }
        drop(inner);
        let recovered = reserve_federation_mailbox_placement_route(
            AxumPath(pair.id.clone()),
            State(pair.central.clone()),
            Json(FederationPlacementRequest {
                native_session_id: native.clone(),
                admission_key: "rebuilt-delivery".to_owned(),
            }),
        )
        .await;
        assert!(recovered.is_err(), "ordinary reconnect cannot refresh explicit execution placement");
    }
    let channel = pair.satellite_channel.clone();
    let check = channel.clone();
    let rpc = FederationRpcClient::new(
        channel.snapshot.endpoint.central_federation_port,
        channel,
        Arc::new(move || check.current_authority().map(|_| ())),
    )
    .unwrap();
    let lease = pair.satellite_channel.current_lease().unwrap();
    let request = |operation| FederationMailboxRequest {
        protocol_version: FEDERATION_MAILBOX_WIRE_VERSION,
        schema_version: FEDERATION_MAILBOX_SCHEMA_VERSION.to_owned(),
        central: peer.central.clone(),
        actor: Some(actor.clone()),
        operation,
    };
    if reconnect {
        let refused = rpc
            .request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
                old_lease,
                request(FederationMailboxOperation::Register),
            ))
            .await;
        assert!(
            refused.is_err(),
            "old channel must never inherit refreshed authority"
        );
    }
    let exchange = async |operation| {
        rpc.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
            lease.clone(),
            request(operation),
        ))
        .await
        .unwrap()
        .value
    };
    exchange(FederationMailboxOperation::Register).await;
    let sent = exchange(FederationMailboxOperation::Send {
        target: recipient.session_id,
        message: "after rebuild".to_owned(),
        key: "rebuilt-send".to_owned(),
        topic: None,
        state_stamp: None,
    })
    .await;
    let mailbox = sent["mailboxId"].as_str().unwrap().to_owned();
    let reply: MailboxAppendReceipt = pair
        .central
        .federation_mailbox_operation(
            &local,
            FederationMailboxOperation::Send {
                target: actor.binding.participant_key(),
                message: "central reply after rebuild".to_owned(),
                key: "rebuilt-reply".to_owned(),
                topic: None,
                state_stamp: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(reply.mailbox_id, mailbox);
    let page = exchange(FederationMailboxOperation::Read {
        mailbox_id: mailbox.clone(),
        after_sequence: None,
        limit: 50,
        issue_receipt: true,
    })
    .await;
    assert_eq!(page["messages"].as_array().unwrap().len(), 1);
    let receipt = page["receipt"].as_str().unwrap().to_owned();
    exchange(FederationMailboxOperation::Acknowledge {
        mailbox_id: mailbox.clone(),
        receipt,
    })
    .await;
    let after = exchange(FederationMailboxOperation::Read {
        mailbox_id: mailbox,
        after_sequence: None,
        limit: 50,
        issue_receipt: true,
    })
    .await;
    assert!(after["messages"].as_array().unwrap().is_empty());
    // Exercise the same native HTTP route used by a direct remote prompt.
    // A busy fixture queues the prompt, so no provider process is started.
    {
        let mut inner = pair.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&native).unwrap();
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    let response = app_router(pair.satellite.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{native}/messages"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"text":"direct after rebuild"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(
        pair.satellite
            .get_session(&native)
            .unwrap()
            .session
            .pending_prompts[0]
            .text,
        "direct after rebuild"
    );
    // No re-enrollment or profile refresh occurred to admit any operation.
    let inner = pair.central.inner.lock().unwrap();
    assert_eq!(
        inner.federation.peers[&pair.id].profile.binary_sha256,
        "a".repeat(64)
    );
    assert_eq!(inner.federation.peers[&pair.id].enrollment_generation, 1);
    drop(inner);
    if storage_transition {
        for app in [&pair.central, &pair.satellite] {
            let inner = app.inner.lock().unwrap();
            let peer = inner.federation.peers.values().next().unwrap();
            assert_eq!(peer.profile, legacy_enrollment_profile());
            assert_eq!(peer.enrollment_generation, 1);
        }
    }
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_both_primary2_hosts_reload_without_profile_refresh() {
    rebuilt_delivery_case(true, true, true, true, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_satellite_hash_only_keeps_delivery() {
    rebuilt_delivery(false, false, false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_central_hash_only_keeps_delivery() {
    rebuilt_delivery(true, false, false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_satellite_reconnect_automatically_restores_delivery() {
    rebuilt_delivery(false, true, false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_central_reconnect_automatically_restores_delivery() {
    rebuilt_delivery(true, true, false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_restart_automatically_restores_delivery() {
    rebuilt_delivery(true, true, true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_legacy_proof_restarts_without_profile_refresh() {
    rebuilt_delivery(true, true, true, true).await;
}

async fn incompatible_profile(protocol: bool) {
    let mut changed = profile();
    changed.binary_sha256 = "b".repeat(64);
    if protocol {
        changed.protocol_version += 1;
    } else {
        changed.schema_version.push_str("-incompatible");
    }
    let pair = Pair::new(
        federation_registry::central(),
        measured(Arc::new(Mutex::new(changed))),
    )
    .await;
    let report = pair.verify().await.unwrap();
    assert!(!report.termal_complete());
    assert!(
        report
            .outcomes
            .iter()
            .any(|row| row.check == FederationOnboardingCheck::VersionSchema && !row.passed)
    );
    assert_eq!(
        pair.central
            .federation_peer_snapshot(&pair.id)
            .unwrap()
            .membership,
        FederationMembershipState::Enrolled
    );
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_protocol_change_is_refused() {
    incompatible_profile(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_schema_change_is_refused() {
    incompatible_profile(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_unknown_stored_schema_is_not_aliased_after_reload() {
    for schema in ["2-metadata-units-2", "2-unknown"] {
        let mut enrolled = legacy_enrollment_profile();
        enrolled.schema_version = schema.to_owned();
        let pair = Pair::with_enrolled_profile(
            federation_registry::central(),
            io(),
            io(),
            enrolled.clone(),
        )
        .await;
        let (central, satellite, listeners) = pair.suspend().await;
        for app in [&central, &satellite] {
            *app.inner.lock().unwrap() =
                load_state(app.persistence_path.as_path()).unwrap().unwrap();
            let inner = app.inner.lock().unwrap();
            let peer = inner.federation.peers.values().next().unwrap();
            assert_eq!(peer.profile, enrolled);
            assert_eq!(peer.enrollment_generation, 1);
        }
        let pair = Pair::resume(central, satellite, listeners, io(), io()).await;
        let report = pair.verify().await.unwrap();
        assert!(!report.termal_complete());
        for check in [
            FederationOnboardingCheck::VersionSchema,
            FederationOnboardingCheck::ReverseRpc,
        ] {
            assert!(
                report
                    .outcomes
                    .iter()
                    .any(|row| row.check == check && !row.passed)
            );
        }
        for app in [&pair.central, &pair.satellite] {
            let inner = app.inner.lock().unwrap();
            let peer = inner.federation.peers.values().next().unwrap();
            assert_eq!(peer.profile, enrolled);
            assert_eq!(peer.enrollment_generation, 1);
            assert_eq!(peer.membership, FederationMembershipState::Enrolled);
            assert!(peer.verification.is_none());
        }
        pair.finish().await;
    }
}

// Explicit diagnostic recovery helper for the retained verification tests.
// Production health never invokes requalification.
impl FederationRpcClient {
    pub(super) async fn requalify(&self, lease: &FederationLinkLease) -> Result<(), ApiError> {
        if self.channel.snapshot.local_role != FederationRole::Central {
            return Ok(());
        }
        let Some(app) = &self.channel.app else {
            return Ok(());
        };
        let pending = lease.with_current(app, |inner, current| {
            Ok(current.local_role == FederationRole::Central
                && inner
                    .federation
                    .peers
                    .get(&current.peer.host_id)
                    .is_some_and(|peer| peer.requalification.is_some()))
        })?;
        if !pending {
            return Ok(());
        }
        self.channel.control.observe("requalificationPrechecked", 0);
        // Explicit diagnostic recovery may refresh execution verification.
        // Ordinary health and mailbox routing never call this operation.
        let report = verify_federation_satellite_mode(
            app.clone(),
            self.channel.snapshot.peer.host_id.clone(),
            true,
        )
        .await?;
        if !report.termal_complete() {
            return Err(ApiError::from_status(
                federation_requalification_refusal_status(&report),
                "federation protocol/schema or re-attestation checks refused",
            ));
        }
        lease.with_current(app, |_, _| Ok(()))
    }
}

pub(super) fn federation_requalification_refusal_status(report: &FederationOnboardingReport) -> StatusCode {
    if report.outcomes.iter().any(|outcome| {
        !outcome.passed
            && matches!(
                outcome.reason.as_str(),
                "protocolSchemaMismatch"
                    | "identityMismatch"
                    | "reverseIdentityMismatch"
                    | "platformIdentityMismatch"
                    | "healthAuthorityRefused"
                    | "forwardAuthorityRefused"
                    | "reverseAuthorityRefused"
            )
    }) {
        StatusCode::CONFLICT
    } else {
        StatusCode::LOCKED
    }
}
