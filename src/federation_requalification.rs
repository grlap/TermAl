// Restores previously authorized membership/placements after fresh pair proof.
// Never enrolls, rotates keys, creates bindings, or authorizes an old channel.

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationRequalification {
    expected: FederationPeerSnapshot,
    basis: String,
    previous_basis: String,
    qualified_probe: FederationQualifiedProbe,
}

impl FederationRequalification {
    fn matches(&self, peer: &FederationPeerRecord, current: &FederationPeerSnapshot) -> bool {
        // Prevent a changed identity, enrollment, key reference, or configuration
        // from inheriting authorization. Transport generations intentionally differ.
        self.qualified_probe.valid()
            && self.qualified_probe.same_authority_as(current)
            && self.basis == peer.verification_basis()
            && self.expected.local == current.local
            && self.expected.peer == current.peer
            && self.expected.central == current.central
            && self.expected.local_role == current.local_role
            && self.expected.enrollment_generation == current.enrollment_generation
            && self.expected.pair_capability_ref == current.pair_capability_ref
            && self.expected.endpoint == current.endpoint
            && matches!(
                self.expected.membership,
                FederationMembershipState::Verified
                    | FederationMembershipState::Active
                    | FederationMembershipState::Draining
            )
    }
}

fn federation_requalification_capture(
    domain: &FederationState,
    expected: &FederationPeerSnapshot,
) -> Option<FederationRequalification> {
    let peer = domain.peers.get(&expected.peer.host_id)?;
    if let Some(retained) = &peer.requalification {
        return retained.matches(peer, expected).then(|| retained.clone());
    }
    let proof = peer.verification.as_ref()?;
    let report = domain.onboarding.reports.get(&expected.peer.host_id)?;
    if !proof.checks.complete()
        || !report.termal_complete()
        || report.basis != proof.basis
        || !report.expected.same_transport_as(expected)
        || (proof.basis != peer.verification_basis()
            && proof.basis != peer.legacy_verification_basis())
    {
        return None;
    }
    let retained = FederationRequalification {
        expected: expected.clone(),
        basis: peer.verification_basis(),
        previous_basis: proof.basis.clone(),
        qualified_probe: FederationQualifiedProbe::from_report(report)?,
    };
    retained.matches(peer, expected).then_some(retained)
}

fn federation_requalification_finish(
    domain: &mut FederationState,
    id: &str,
    current: &FederationPeerSnapshot,
) -> Result<(), ApiError> {
    let peer = domain.peers.get(id).ok_or_else(federation_stale_link)?;
    let Some(retained) = peer.requalification.clone() else {
        return Ok(());
    };
    if !retained.matches(peer, current) {
        return Err(federation_stale_link());
    }
    let mut resumed = current.clone();
    // Quarantine is an independent admission fence; never promote membership
    // past it merely because the transport was re-attested successfully.
    if domain.admission_quarantine.is_some() {
        return Err(ApiError::from_status(
            StatusCode::LOCKED,
            "federation admission is quarantined",
        ));
    }
    resumed.membership = retained.expected.membership;
    resumed.record_generation = peer.record_generation;
    let incarnation = domain
        .links
        .get(id)
        .cloned()
        .ok_or_else(federation_stale_link)?;
    // Only migrate reservations from the exact earlier qualified epoch. Old
    // HTTP leases/nonces remain fenced; bindings and creation ids never change.
    for (key, placement) in &mut domain.placements {
        if placement.state != FederationPlacementState::Completed
            && placement.peer.same_transport_as(&retained.expected)
            && (placement.execution_basis == retained.previous_basis
                || placement.execution_basis == retained.basis)
        {
            placement.peer = resumed.clone();
            placement.link_incarnation = incarnation.clone();
            placement.execution_basis = retained.basis.clone();
            if let Some(intent) = domain.binding_install_intents.get_mut(key) {
                intent.execution_basis = retained.basis.clone();
                intent.peer_verification_basis = retained.basis.clone();
            }
        }
    }
    let peer = domain.peers.get_mut(id).expect("captured peer");
    peer.membership = resumed.membership;
    peer.requalification = None;
    Ok(())
}
