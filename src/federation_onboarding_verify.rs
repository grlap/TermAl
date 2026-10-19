// Owns the central host's explicit, off-lock onboarding checklist.
// Does not provision hosts or activate membership. New O1 orchestration uses
// L1's typed authenticated RPC and M1's exact captured completion snapshot.

impl FederationOnboardingReport {
    fn observe(&mut self, check: FederationOnboardingCheck, passed: bool, reason: &str) {
        if let Some(outcome) = self
            .outcomes
            .iter_mut()
            .find(|outcome| outcome.check == check)
        {
            outcome.passed = passed;
            outcome.reason = reason.to_owned();
        }
    }
}

async fn verify_federation_satellite(
    app: AppState,
    id: String,
) -> Result<FederationOnboardingReport, ApiError> {
    let (mut report, record, engram_enabled) = {
        let inner = app.inner.lock().expect("state mutex poisoned");
        let expected = federation_snapshot_locked(&inner, &id)?;
        if expected.local_role != FederationRole::Central
            || !matches!(
                expected.membership,
                FederationMembershipState::Enrolled | FederationMembershipState::Draining
            )
        {
            return Err(ApiError::conflict(
                "onboarding requires an unverified satellite on the central host",
            ));
        }
        let record = inner
            .federation
            .peers
            .get(&id)
            .ok_or_else(federation_stale_link)?
            .clone();
        let report = FederationOnboardingReport {
            attempt_id: Uuid::new_v4().to_string(),
            basis: record.verification_basis(),
            link_incarnation: inner.federation.links.get(&id).cloned().unwrap_or_default(),
            expected,
            outcomes: FEDERATION_ONBOARDING_CHECKS
                .into_iter()
                .map(|check| FederationOnboardingOutcome {
                    check,
                    passed: false,
                    reason: "notRun".to_owned(),
                })
                .collect(),
            probe_session_id: None,
            wake_outcome_id: None,
            running_image: FederationImageObservation::Unknown,
            engram: None,
        };
        (
            report,
            record,
            inner.federation.onboarding.engram_enabled.contains(&id),
        )
    };
    if let Some(peer) = app.federation_peer_handle(&id) {
        match peer.channel.current_lease() {
            Ok(lease)
                if lease.incarnation == report.link_incarnation
                    && peer.channel.current_authority()? == report.expected =>
            {
                run_federation_onboarding_checks(
                    &peer.rpc,
                    &lease,
                    &record,
                    engram_enabled,
                    &mut report,
                )
                .await;
            }
            _ => report.observe(
                FederationOnboardingCheck::Reachability,
                false,
                "linkUnavailable",
            ),
        }
    } else {
        report.observe(
            FederationOnboardingCheck::Reachability,
            false,
            "linkUnavailable",
        );
    }
    if report.link_incarnation.is_empty()
        && app.federation_observed_attestation_failure(&report.expected)
    {
        report.observe(
            FederationOnboardingCheck::CapabilityExchange,
            false,
            "attestationFailed",
        );
    }
    let saved = report.clone();
    let expected = report.expected.clone();
    let complete = report.termal_complete();
    // A complete report and its membership proof commit in one candidate.
    // Incomplete or older attempts cannot overwrite a successor's proof report.
    tokio::task::spawn_blocking(move || {
        app.ensure_federation_peer_current(&expected)?;
        if complete {
            app.verify_federation_peer_with(&expected, saved.registry_checks()?, |domain| {
                let peer = domain.peers.get(&id).ok_or_else(federation_stale_link)?;
                if peer.verification_basis() != saved.basis
                    || domain.links.get(&id) != Some(&saved.link_incarnation)
                {
                    return Err(federation_stale_link());
                }
                domain.onboarding.reports.insert(id, saved);
                Ok(())
            })?;
        } else {
            app.save_federation_onboarding_report(saved)?;
        }
        Ok::<_, ApiError>(())
    })
    .await
    .map_err(|_| federation_transport_unavailable())??;
    Ok(report)
}

async fn run_federation_onboarding_checks(
    rpc: &FederationRpcClient,
    lease: &FederationLinkLease,
    record: &FederationPeerRecord,
    engram_enabled: bool,
    report: &mut FederationOnboardingReport,
) {
    use FederationOnboardingCheck as Check;
    let reachable = rpc.health(lease).await.is_ok();
    report.observe(
        Check::Reachability,
        reachable,
        if reachable {
            "authenticatedHealth"
        } else {
            "healthFailed"
        },
    );
    // A recorded L1 lease is issued only after the pinned OpenSSH channel and
    // fresh mutual attestation have completed. Never infer these
    // checks from an unauthenticated TCP connection or a shell exit status.
    report.observe(
        Check::HostKey,
        reachable,
        if reachable {
            "pinnedLinkHandshake"
        } else {
            "hostKeyHandshakeUnconfirmed"
        },
    );
    report.observe(
        Check::CapabilityExchange,
        reachable,
        if reachable {
            "pairCapabilityHandshake"
        } else {
            "capabilityHandshakeUnconfirmed"
        },
    );
    let identity = rpc
        .request::<FederationOnboardingIdentity>(FederationRpcRoute::Onboarding(
            lease.clone(),
            FederationOnboardingRpc::Identity,
        ))
        .await;
    let mut identity_ok = false;
    let mut version_ok = false;
    match identity {
        Ok(identity) => {
            let image_ok = identity
                .running_image
                .matches_profile(&identity.profile, &identity.platform);
            report.running_image = if image_ok {
                identity.running_image
            } else {
                FederationImageObservation::Unknown
            };
            identity_ok = identity.host == report.expected.peer
                && identity.central == report.expected.central
                && identity.enrollment_generation == report.expected.enrollment_generation
                && identity.link_incarnation == report.link_incarnation;
            version_ok = image_ok
                && identity.profile == record.profile
                && identity.profile.protocol_version == u32::from(FEDERATION_PROTOCOL_VERSION)
                && identity.profile.schema_version == SQLITE_SCHEMA_VERSION;
            report.observe(Check::ForwardRpc, true, "authenticatedIdentityResponse");
            report.observe(
                Check::QualifiedIdentity,
                identity_ok,
                if identity_ok {
                    "identityMatched"
                } else {
                    "identityMismatch"
                },
            );
            report.observe(
                Check::VersionSchema,
                version_ok,
                if !image_ok {
                    "runningImageUnknownOrMismatched"
                } else if version_ok {
                    "executionProfileMatched"
                } else {
                    "versionSchemaOrBuildMismatch"
                },
            );
        }
        Err(_) => {
            report.observe(Check::ForwardRpc, false, "forwardRpcFailed");
            report.observe(Check::QualifiedIdentity, false, "identityUnavailable");
            report.observe(Check::VersionSchema, false, "versionSchemaUnavailable");
        }
    }
    let reverse = rpc
        .request::<FederationReverseProbe>(FederationRpcRoute::Onboarding(
            lease.clone(),
            FederationOnboardingRpc::Reverse(FederationProbeRequest {
                attempt_id: report.attempt_id.clone(),
            }),
        ))
        .await;
    let reverse_ok = reverse.is_ok_and(|reverse| {
        reverse.attempt_id == report.attempt_id
            && reverse.identity.host == report.expected.local
            && reverse.identity.central == report.expected.central
            && reverse.identity.enrollment_generation == report.expected.enrollment_generation
            && reverse.identity.link_incarnation == report.link_incarnation
            && reverse.identity.profile.protocol_version == u32::from(FEDERATION_PROTOCOL_VERSION)
            && reverse.identity.profile.schema_version == SQLITE_SCHEMA_VERSION
            && reverse
                .identity
                .running_image
                .matches_profile(&reverse.identity.profile, &reverse.identity.platform)
    });
    report.observe(
        Check::ReverseRpc,
        reverse_ok,
        if reverse_ok {
            "authenticatedReverseResponse"
        } else {
            "reverseRpcFailed"
        },
    );
    if reachable && identity_ok && version_ok {
        let prepared = rpc
            .request::<FederationProbeSession>(FederationRpcRoute::Onboarding(
                lease.clone(),
                FederationOnboardingRpc::Prepare(FederationProbeRequest {
                    attempt_id: report.attempt_id.clone(),
                }),
            ))
            .await;
        if let Ok(probe) = prepared {
            let valid = probe.attempt_id == report.attempt_id
                && validate_federation_probe_id(&probe.session_id).is_ok()
                && probe.expected.local == report.expected.peer
                && probe.expected.peer == report.expected.local
                && probe.expected.central == report.expected.central
                && probe.expected.enrollment_generation == report.expected.enrollment_generation
                && probe.link_incarnation == report.link_incarnation
                && probe.wake.is_none();
            if valid {
                report.probe_session_id = Some(probe.session_id.clone());
                let wake = rpc
                    .request::<FederationProbeWakeOutcome>(FederationRpcRoute::Onboarding(
                        lease.clone(),
                        FederationOnboardingRpc::Wake(FederationProbeWakeRequest {
                            attempt_id: report.attempt_id.clone(),
                            session_id: probe.session_id.clone(),
                        }),
                    ))
                    .await;
                if let Ok(wake) = wake {
                    let delivered = wake.session_id == probe.session_id
                        && wake.outcome_id != probe.session_id
                        && validate_federation_probe_id(&wake.outcome_id).is_ok()
                        && wake.state == FederationProbeWakeState::Delivered;
                    let platform_ok = wake.platform.as_ref().is_some_and(|platform| {
                        platform.platform == record.platform
                            && platform.arch == record.arch
                            && platform.toolchain_identity == record.profile.toolchain_identity
                            && platform.paths_resolved
                            && platform.child_spawned_and_reaped
                            && matches!(
                                (platform.platform.as_str(), platform.shell_dialect.as_str()),
                                ("windows", "cmd") | ("linux" | "macos", "posix")
                            )
                    });
                    if delivered {
                        report.wake_outcome_id = Some(wake.outcome_id);
                    }
                    report.observe(
                        Check::IsolatedWake,
                        delivered,
                        if delivered {
                            "durableWakeDelivered"
                        } else {
                            "wakeNotDelivered"
                        },
                    );
                    report.observe(
                        Check::PlatformPathsSupervisor,
                        delivered && platform_ok,
                        if delivered && platform_ok {
                            "isolatedPlatformMatched"
                        } else {
                            "platformOrSupervisorFailed"
                        },
                    );
                } else {
                    report.observe(Check::IsolatedWake, false, "wakeRpcFailed");
                }
            } else {
                report.observe(Check::IsolatedWake, false, "probeSessionIdentityMismatch");
            }
        } else {
            report.observe(Check::IsolatedWake, false, "probeSessionUnavailable");
        }
    }
    if engram_enabled {
        report.engram = Some(
            rpc.request(FederationRpcRoute::Onboarding(
                lease.clone(),
                FederationOnboardingRpc::Engram,
            ))
            .await
            .unwrap_or_else(|_| FederationEngramOnboardingOutcome::unavailable()),
        );
    }
}
