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
            "version":inner.federation.version,
            "trackingDurability":if inner.federation_durable { "durable" } else { "unresolved" },
            "continuityEstablished":inner.federation.local.is_some() && !inner.federation_continuity_lost && inner.federation.admission_quarantine.is_none(),
            "admissionQuarantine":inner.federation.admission_quarantine,
            "role":inner.preferences.federation.role, "local":inner.federation.local,
            "central":inner.preferences.federation.central, "peers":peers,
            "satellite":federation_satellite_view(&inner)}))
    }
}

/// What a satellite operator checks before the central runs O1: the bound
/// central, that central's peer record here with its generations, whether the
/// CURRENT durable link to it is up with admission open (never a past
/// observation), and how many local sessions are bound to it. Null unless
/// this host is a satellite.
/// Redacted like the peer list: references only, never key material.
fn federation_satellite_view(inner: &StateInner) -> Value {
    let settings = &inner.preferences.federation;
    if settings.role != FederationRole::Satellite {
        return Value::Null;
    }
    let Some(central) = settings.central.as_ref() else {
        return Value::Null;
    };
    let domain = &inner.federation;
    let peer = domain
        .peers
        .get(&central.host_id)
        .filter(|peer| &peer.identity == central);
    let link = domain.links.get(&central.host_id);
    let central_peer = match peer {
        Some(peer) => json!({
            "enrolled": peer.membership != FederationMembershipState::Revoked,
            "membership": peer.membership,
            "recordGeneration": peer.record_generation,
            "transportGeneration": peer.transport_generation,
            "verificationCurrent": peer.verified_on(link),
            "linkConnected": settings.enabled
                && inner.federation_durable
                && !inner.federation_continuity_lost
                && domain.admission_quarantine.is_none()
                && link.is_some()
                && peer.membership != FederationMembershipState::Revoked,
            "lastObserved": peer.last_observed,
        }),
        None => json!({
            "enrolled": false,
            "membership": null,
            "recordGeneration": null,
            "transportGeneration": null,
            "verificationCurrent": false,
            "linkConnected": false,
            "lastObserved": null,
        }),
    };
    json!({
        "central": central,
        "centralPeer": central_peer,
        "boundSessions": federation_bound_session_count(inner, central),
    })
}
