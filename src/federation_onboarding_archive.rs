// Owns bounded retirement evidence for isolated onboarding probes.
// Does not stop children, infer their success or authorize old wake replay.
// Split from federation_onboarding_store.rs for fenced recovery after epoch changes.

const FEDERATION_RETIRED_PROBES_PER_PEER: usize = 16;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationRetiredProbe {
    // The full original snapshot, basis and IDs remain attached to Unknown.
    probe: FederationProbeSession,
    retired_at: String,
    replay_observations: u64,
    last_completion: Option<FederationProbeWakeOutcome>,
}

fn retire_federation_probe(domain: &mut FederationState, id: &str) -> Result<(), ApiError> {
    let Some(prior) = domain.onboarding.probes.get(id) else {
        return Ok(());
    };
    if !prior
        .wake
        .as_ref()
        .is_some_and(|wake| wake.state == FederationProbeWakeState::Accepted)
    {
        domain.onboarding.probes.remove(id);
        return Ok(());
    }
    let rows = domain
        .onboarding
        .retired_probes
        .entry(id.to_owned())
        .or_default();
    if rows.len() >= FEDERATION_RETIRED_PROBES_PER_PEER {
        return Err(ApiError::conflict(
            "isolated probe recovery archive is full; operator reconciliation required",
        ));
    }
    let mut probe = domain.onboarding.probes.remove(id).expect("captured probe");
    probe.wake.as_mut().expect("accepted wake").state = FederationProbeWakeState::Unknown;
    rows.push(FederationRetiredProbe {
        probe,
        retired_at: chrono::Utc::now().to_rfc3339(),
        replay_observations: 0,
        last_completion: None,
    });
    Ok(())
}

fn observe_retired_federation_wake(
    domain: &mut FederationState,
    id: &str,
    request: &FederationProbeWakeRequest,
    completion: Option<FederationProbeWakeOutcome>,
) -> bool {
    let Some(row) = domain
        .onboarding
        .retired_probes
        .get_mut(id)
        .and_then(|rows| {
            rows.iter_mut().find(|row| {
                row.probe.attempt_id == request.attempt_id
                    && row.probe.session_id == request.session_id
            })
        })
    else {
        return false;
    };
    row.replay_observations = row.replay_observations.saturating_add(1);
    if let Some(completion) = completion {
        // Bounded observation only: never replace Unknown or the successor.
        row.last_completion = Some(completion);
    }
    true
}
