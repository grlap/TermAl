// Owns redacted registry and capacity projection. Does not own credentials, transport or runtime execution.
// Split from federation_registry.rs while implementing the new domain foundation.

#[cfg_attr(not(test), allow(dead_code))]
impl AppState {
    fn federation_registry_view(&self) -> Result<Value, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let peers: Vec<Value> = inner.federation.peers.values().map(|peer| {
            let allocated: Vec<_> = inner.federation.placements.values().filter(|placement|
                placement.identity.host_id == peer.identity.host_id && placement.state != FederationPlacementState::Completed).collect();
            json!({"host":peer.identity, "enrollmentGeneration":peer.enrollment_generation,
                "recordGeneration":peer.record_generation,
                "transportGeneration":peer.transport_generation, "displayName":peer.display_name,
                "platform":peer.platform, "arch":peer.arch, "endpoint":peer.endpoint,
                "pairCapabilityRef":peer.pair_capability_ref, "profile":peer.profile,
                "capacity":peer.capacity, "state":peer.membership,
                "allocatedSessions":allocated.len(),
                "allocatedBuildSlots":allocated.iter().map(|row|u64::from(row.build_slots)).sum::<u64>(),
                "allocatedGateSlots":allocated.iter().map(|row|u64::from(row.gate_slots)).sum::<u64>(),
                "verificationCurrent":peer.verified_on(inner.federation.links.get(&peer.identity.host_id)),
                "lastObserved":peer.last_observed})
        }).collect();
        Ok(json!({"enabled":inner.preferences.federation.enabled,
            "trackingDurability":if inner.federation_durable { "durable" } else { "unresolved" },
            "continuityEstablished":inner.federation.local.is_some() && !inner.federation_continuity_lost && inner.federation.admission_quarantine.is_none(),
            "admissionQuarantine":inner.federation.admission_quarantine,
            "role":inner.preferences.federation.role, "local":inner.federation.local,
            "central":inner.preferences.federation.central, "peers":peers}))
    }
}
