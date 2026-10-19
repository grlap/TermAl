// Authenticated read-only proof of an already installed native binding.
// Never installs a binding, creates a runtime, or registers a participant.

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationNativeBindingRequest {
    protocol_version: u16,
    request_id: String,
    binding: FederationMailboxBinding,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationNativeBindingProof {
    protocol_version: u16,
    request_id: String,
    central: FederationHostIdentity,
    host: FederationHostIdentity,
    native_session_id: String,
    native_creation_id: String,
    binding: FederationMailboxBinding,
    link_incarnation: String,
    authority: FederationPeerSnapshot,
}

async fn serve_federation_native_binding(
    axum::Extension(peer): axum::Extension<FederationAuthenticatedPeer>,
    Json(request): Json<FederationNativeBindingRequest>,
) -> Result<Json<FederationNativeBindingProof>, ApiError> {
    request.binding.validate()?;
    if request.protocol_version != FEDERATION_PROTOCOL_VERSION
        || Uuid::parse_str(&request.request_id).is_err()
    {
        return Err(ApiError::bad_request("invalid native binding proof request"));
    }
    let lease = peer.lease.ok_or_else(federation_stale_link)?;
    let app = lease.channel.app.clone().ok_or_else(federation_transport_unavailable)?;
    // One coherent current, durable state observation under link -> state.
    let proof = lease.with_current(&app, |inner, authority| {
        if authority.local_role != FederationRole::Satellite
            || inner.federation.admission_quarantine.is_some()
            || request.binding.central != authority.peer
            || request.binding.identity.host_id != authority.local.host_id
        {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        let record = federation_mailbox_root_locked(inner, &request.binding.identity.session_id)?;
        if record.federation_mailbox_binding.as_ref() != Some(&request.binding) {
            return Err(ApiError::conflict("native binding is missing or changed"));
        }
        Ok(FederationNativeBindingProof {
            protocol_version: FEDERATION_PROTOCOL_VERSION,
            request_id: request.request_id.clone(),
            central: authority.central.clone(),
            host: authority.local.clone(),
            native_session_id: record.session.id.clone(),
            native_creation_id: record.federation_native_creation_id.clone(),
            binding: request.binding.clone(),
            link_incarnation: lease.incarnation.clone(),
            authority: authority.clone(),
        })
    })?;
    Ok(Json(proof))
}
