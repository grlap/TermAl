// Central placement and authenticated satellite binding command vocabulary.
// The native token is a recreation fence, never an agent or proxy identity.

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationBindingInstallIntent {
    operation_key: String,
    native_creation_id: String,
    binding: FederationMailboxBinding,
    admission_key: String,
    execution_basis: String,
    peer_enrollment_generation: u64,
    peer_verification_basis: String,
    configuration_basis: String,
}

#[derive(Clone)]
struct FederationNativeBindingFence {
    binding: FederationMailboxBinding,
    native_creation_id: String,
}

#[derive(Clone)]
struct FederationBindingsFence {
    image: Value,
    sessions: Vec<FederationNativeBindingFence>,
}

impl FederationBindingsFence {
    fn is_already_durable(&self, connection: &rusqlite::Connection) -> Result<bool> {
        if !federation_committed_image_matches(connection, &self.image)? {
            return Ok(false);
        }
        for expected in &self.sessions {
            let stored: Option<String> = connection
                .query_row(
                    "SELECT value_json FROM sessions WHERE id = ?1",
                    [&expected.binding.identity.session_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(stored) = stored else {
                return Ok(false);
            };
            let actual: PersistedSessionRecord = serde_json::from_str(&stored)?;
            if actual.session.id != expected.binding.identity.session_id
                || actual.federation_native_creation_id != expected.native_creation_id
                || actual.federation_mailbox_binding.as_ref() != Some(&expected.binding)
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn federation_validate_binding_install(
    binding: &FederationMailboxBinding,
    native_creation_id: &str,
    operation_key: &str,
) -> Result<(), ApiError> {
    binding.validate()?;
    validate_federation_reference(operation_key)?;
    if !Uuid::parse_str(native_creation_id)
        .is_ok_and(|id| !id.is_nil() && id.to_string() == native_creation_id)
    {
        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
    }
    Ok(())
}

impl FederationBindingInstallIntent {
    fn validate(&self) -> Result<(), ApiError> {
        validate_federation_reference(&self.admission_key)?;
        if self.peer_enrollment_generation == 0
            || self.execution_basis != self.peer_verification_basis
            || [&self.execution_basis, &self.configuration_basis]
                .into_iter()
                .any(|basis| {
                    basis.len() != 64 || !basis.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
        {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        federation_validate_binding_install(
            &self.binding,
            &self.native_creation_id,
            &self.operation_key,
        )
    }

    fn ensure_authority(
        &self,
        domain: &FederationState,
        current: &FederationPeerSnapshot,
        placement: &FederationPlacement,
    ) -> Result<(), ApiError> {
        let peer = domain
            .peers
            .get(&current.peer.host_id)
            .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
        if self.admission_key != placement.admission_key
            || self.binding.identity != placement.identity
            || self.binding.placement_generation != placement.placement_generation
            || self.binding.central != current.central
            || self.binding.identity.host_id != current.peer.host_id
            || self.execution_basis != placement.execution_basis
            || self.peer_enrollment_generation != current.enrollment_generation
            || self.peer_verification_basis != peer.verification_basis()
            || self.configuration_basis != current.configuration_basis()
            || placement.state == FederationPlacementState::Completed
        {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        Ok(())
    }
}

impl FederationPeerSnapshot {
    // Material central authority, shared by report and retained-intent checks.
    // Link and mutation sequence numbers deliberately do not enter this image.
    fn configuration_basis(&self) -> String {
        sha256_hex(
            &serde_json::to_vec(&json!({
                "local": self.local, "role": self.local_role, "central": self.central,
                "endpoint": self.endpoint,
            }))
            .expect("configuration fields serialize"),
        )
    }
}

impl AppState {
    // Entry-point mutations never fall back to synchronous disk I/O while a
    // link/state lock is held. A stopped writer leaves an explicit hold.
    fn queue_federation_entrypoint_persistence(&self) -> Result<PersistDispatch, ApiError> {
        self.persist_tx.send(PersistRequest::Delta).map_err(|_| {
            ApiError::internal("federation persistence unavailable; admission remains held")
        })?;
        Ok(PersistDispatch::BackgroundQueued)
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(
    tag = "operation",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum FederationBindingCommand {
    Prepare {
        native_session_id: String,
    },
    Install {
        binding: FederationMailboxBinding,
        expected_native_creation_id: String,
        operation_key: String,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationBindingPreparation {
    host: FederationHostIdentity,
    central: FederationHostIdentity,
    native_session_id: String,
    native_creation_id: String,
    link_incarnation: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationBindingInstalled {
    operation_key: String,
    native_creation_id: String,
    binding: FederationMailboxBinding,
    participant: FederationMailboxParticipant,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationPlacementRequest {
    native_session_id: String,
    admission_key: String,
}

// Called beside a mutation, with the current peer and link from that same cut.
fn federation_entrypoint_verified(
    domain: &FederationState,
    settings: &FederationSettings,
    current: &FederationPeerSnapshot,
    incarnation: &str,
) -> Result<(), ApiError> {
    let refused = || ApiError::conflict(FEDERATION_STALE_AUTHORITY);
    let peer = domain
        .peers
        .get(&current.peer.host_id)
        .ok_or_else(refused)?;
    let report = domain
        .onboarding
        .reports
        .get(&current.peer.host_id)
        .ok_or_else(refused)?;
    if !settings.enabled
        || settings.role != FederationRole::Central
        || domain.admission_quarantine.is_some()
        || domain.local.as_ref() != Some(&current.local)
        || settings.central.as_ref() != Some(&current.central)
        || peer.record_generation != current.record_generation
        || !peer.verified_on(domain.links.get(&current.peer.host_id))
        || !report.termal_complete()
        || report.basis != peer.verification_basis()
        || report.expected.configuration_basis() != current.configuration_basis()
        || !report.expected.same_transport_as(current)
        || report.link_incarnation != incarnation
        || domain.links.get(&current.peer.host_id).map(String::as_str) != Some(incarnation)
    {
        return Err(refused());
    }
    Ok(())
}

fn federation_entrypoint_peer(
    app: &AppState,
    id: &str,
) -> Result<Arc<FederationPeerHandle>, ApiError> {
    {
        let inner = app.inner.lock().expect("state mutex poisoned");
        if inner.preferences.federation.role != FederationRole::Central {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
    }
    app.federation_peer_handle(id)
        .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))
}

fn federation_binding_unknown_outcome() -> ApiError {
    ApiError::from_status(
        StatusCode::SERVICE_UNAVAILABLE,
        "binding command outcome unresolved; retry original operation",
    )
}

fn federation_binding_rpc_error(status: StatusCode) -> ApiError {
    match status {
        StatusCode::BAD_REQUEST => ApiError::bad_request("native mailbox root is ineligible"),
        StatusCode::NOT_FOUND => ApiError::not_found("session not found"),
        StatusCode::CONFLICT => ApiError::conflict(FEDERATION_STALE_AUTHORITY),
        StatusCode::FORBIDDEN => {
            ApiError::from_status(status, "federation pair authentication failed")
        }
        StatusCode::LOCKED => ApiError::from_status(
            status,
            "binding command is pending; retry original operation",
        ),
        StatusCode::TOO_MANY_REQUESTS => ApiError::from_status(
            status,
            "binding command was not admitted; retry the original operation",
        ),
        _ => federation_binding_unknown_outcome(),
    }
}
