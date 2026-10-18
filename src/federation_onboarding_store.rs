// Owns durable bounded checklist reports and isolated probe wake outcomes.
// Does not start processes, transport messages or activate peers. New O1 state
// uses federation_mutations.rs and its existing writer/durability fence.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationOnboardingData {
    // Latest attempt per enrolled peer; no unbounded per-attempt payload log.
    reports: BTreeMap<String, FederationOnboardingReport>,
    probes: BTreeMap<String, FederationProbeSession>,
    #[serde(default)]
    retired_probes: BTreeMap<String, Vec<FederationRetiredProbe>>,
    engram_enabled: BTreeSet<String>,
    engram_commands: BTreeMap<String, FederationEngramProbeCommand>,
}

impl AppState {
    fn save_federation_onboarding_report(
        &self,
        report: FederationOnboardingReport,
    ) -> Result<(), ApiError> {
        self.mutate_federation(|domain, _| {
            let id = &report.expected.peer.host_id;
            let peer = domain.peers.get(id).ok_or_else(federation_stale_link)?;
            if peer.record_generation != report.expected.record_generation
                || peer.verification_basis() != report.basis
                || domain.links.get(id).map(String::as_str).unwrap_or_default()
                    != report.link_incarnation
            {
                return Err(federation_stale_link());
            }
            domain.onboarding.reports.insert(id.clone(), report);
            Ok(())
        })
    }

    fn prepare_federation_probe(
        &self,
        expected: &FederationPeerSnapshot,
        incarnation: &str,
        attempt_id: &str,
    ) -> Result<FederationProbeSession, ApiError> {
        validate_federation_probe_id(attempt_id)?;
        self.ensure_federation_peer_current(expected)?;
        self.mutate_federation(|domain, settings| {
            if settings.role != FederationRole::Satellite
                || domain.links.get(&expected.peer.host_id).map(String::as_str) != Some(incarnation)
                || domain
                    .peers
                    .get(&expected.peer.host_id)
                    .map(|p| p.record_generation)
                    != Some(expected.record_generation)
            {
                return Err(federation_stale_link());
            }
            if domain
                .onboarding
                .retired_probes
                .get(&expected.peer.host_id)
                .is_some_and(|rows| rows.iter().any(|row| row.probe.attempt_id == attempt_id))
            {
                return Err(ApiError::conflict("isolated probe identity is retired"));
            }
            if let Some(prior) = domain.onboarding.probes.get(&expected.peer.host_id) {
                if prior.attempt_id == attempt_id
                    && prior.expected == *expected
                    && prior.link_incarnation == incarnation
                {
                    return Ok(prior.clone());
                }
                if prior.attempt_id == attempt_id {
                    return Err(ApiError::conflict(
                        "isolated probe identity cannot cross a link or enrollment",
                    ));
                }
                if prior
                    .wake
                    .as_ref()
                    .is_some_and(|wake| wake.state == FederationProbeWakeState::Accepted)
                {
                    if prior.expected.same_transport_as(expected)
                        && prior.link_incarnation == incarnation
                    {
                        return Err(ApiError::conflict("isolated probe wake outcome unresolved"));
                    }
                    retire_federation_probe(domain, &expected.peer.host_id)?;
                }
            }
            let probe = FederationProbeSession {
                attempt_id: attempt_id.to_owned(),
                session_id: Uuid::new_v4().to_string(),
                expected: expected.clone(),
                basis: domain.peers[&expected.peer.host_id].verification_basis(),
                link_incarnation: incarnation.to_owned(),
                wake: None,
            };
            domain
                .onboarding
                .probes
                .insert(expected.peer.host_id.clone(), probe.clone());
            Ok(probe)
        })
    }

    fn accept_federation_probe_wake(
        &self,
        expected: &FederationPeerSnapshot,
        incarnation: &str,
        request: &FederationProbeWakeRequest,
    ) -> Result<(FederationProbeWakeOutcome, bool), ApiError> {
        self.ensure_federation_peer_current(expected)?;
        self.mutate_federation(|domain, settings| {
            if settings.role != FederationRole::Satellite
                || domain.links.get(&expected.peer.host_id).map(String::as_str) != Some(incarnation)
                || domain
                    .peers
                    .get(&expected.peer.host_id)
                    .map(|p| p.record_generation)
                    != Some(expected.record_generation)
            {
                return Err(federation_stale_link());
            }
            if observe_retired_federation_wake(domain, &expected.peer.host_id, request, None) {
                return Ok(None);
            }
            let probe = domain
                .onboarding
                .probes
                .get_mut(&expected.peer.host_id)
                .ok_or_else(federation_stale_link)?;
            if probe.expected != *expected
                || probe.link_incarnation != incarnation
                || probe.session_id != request.session_id
                || probe.attempt_id != request.attempt_id
            {
                return Err(federation_stale_link());
            }
            if let Some(outcome) = &probe.wake {
                return Ok(Some((outcome.clone(), false)));
            }
            let outcome = FederationProbeWakeOutcome {
                outcome_id: Uuid::new_v4().to_string(),
                session_id: probe.session_id.clone(),
                state: FederationProbeWakeState::Accepted,
                platform: None,
            };
            probe.wake = Some(outcome.clone());
            Ok(Some((outcome, true)))
        })?
        .ok_or_else(federation_stale_link)
    }

    fn finish_federation_probe_wake(
        &self,
        expected: &FederationPeerSnapshot,
        incarnation: &str,
        outcome: FederationProbeWakeOutcome,
    ) -> Result<(), ApiError> {
        self.mutate_federation(|domain, _| {
            // A late local child may settle before a new prepare has archived
            // its stale acceptance. Retire it here under the same epoch fence.
            let stale_accepted = domain
                .onboarding
                .probes
                .get(&expected.peer.host_id)
                .is_some_and(|probe| {
                    probe.expected == *expected
                        && probe.link_incarnation == incarnation
                        && probe.wake.as_ref().is_some_and(|wake| {
                            wake.state == FederationProbeWakeState::Accepted
                                && wake.outcome_id == outcome.outcome_id
                                && wake.session_id == outcome.session_id
                        })
                        && (domain.links.get(&expected.peer.host_id).map(String::as_str)
                            != Some(incarnation)
                            || domain.peers.get(&expected.peer.host_id).is_none_or(|peer| {
                                peer.enrollment_generation != expected.enrollment_generation
                                    || peer.transport_generation != expected.transport_generation
                                    || peer.identity != expected.peer
                            }))
                });
            if stale_accepted {
                retire_federation_probe(domain, &expected.peer.host_id)?;
            }
            let retired = domain
                .onboarding
                .retired_probes
                .get(&expected.peer.host_id)
                .and_then(|rows| {
                    rows.iter().find(|row| {
                        row.probe.expected == *expected
                            && row.probe.link_incarnation == incarnation
                            && row.probe.wake.as_ref().is_some_and(|wake| {
                                wake.outcome_id == outcome.outcome_id
                                    && wake.session_id == outcome.session_id
                            })
                    })
                })
                .map(|row| FederationProbeWakeRequest {
                    attempt_id: row.probe.attempt_id.clone(),
                    session_id: row.probe.session_id.clone(),
                });
            if let Some(request) = retired {
                observe_retired_federation_wake(
                    domain,
                    &expected.peer.host_id,
                    &request,
                    Some(outcome),
                );
                return Ok(false);
            }
            if domain.links.get(&expected.peer.host_id).map(String::as_str) != Some(incarnation)
                || domain
                    .peers
                    .get(&expected.peer.host_id)
                    .map(|p| p.record_generation)
                    != Some(expected.record_generation)
            {
                return Err(federation_stale_link());
            }
            let probe = domain
                .onboarding
                .probes
                .get_mut(&expected.peer.host_id)
                .ok_or_else(federation_stale_link)?;
            let prior = probe.wake.as_ref().ok_or_else(federation_stale_link)?;
            if probe.expected != *expected
                || probe.link_incarnation != incarnation
                || prior.outcome_id != outcome.outcome_id
                || prior.session_id != outcome.session_id
                || prior.state != FederationProbeWakeState::Accepted
            {
                return Err(federation_stale_link());
            }
            probe.wake = Some(outcome);
            Ok(true)
        })?
        .then_some(())
        .ok_or_else(federation_stale_link)
    }
}

fn validate_federation_probe_id(id: &str) -> Result<(), ApiError> {
    if Uuid::parse_str(id)
        .ok()
        .filter(|id| !id.is_nil())
        .map(|id| id.to_string())
        .as_deref()
        != Some(id)
    {
        return Err(ApiError::bad_request("invalid isolated probe identity"));
    }
    Ok(())
}
