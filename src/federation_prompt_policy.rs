// Owns permission values within FederationState and its existing durable mutations.
// Does not enroll peers, create roots, schedule prompts or own runtime state.
// Recovered selectively from the retained permission policy component.
// enqueueTextPrompt permission values inside FederationState. These records
// create no host, session, participant, placement, runnable work or runtime.
// Mutation/persistence and currentness belong to the existing federation owner.
const FEDERATION_PROMPT_PERMISSION_LIMIT: usize = 1024;
const FEDERATION_PROMPT_POLICY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
struct FederationPromptScope {
    source_host: FederationHostIdentity,
    source: FederationSessionIdentity,
    target_host: FederationHostIdentity,
    target: FederationSessionIdentity,
    enrollment_generation: u64,
    #[serde(deserialize_with = "federation_prompt_optional_generation")]
    placement_generation: Option<u64>,
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_optional_generation<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<u64>, D::Error> {
    Option::<u64>::deserialize(deserializer)
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_required_option<'de, D, T>(deserializer: D)
    -> std::result::Result<Option<T>, D::Error>
where D: serde::Deserializer<'de>, T: Deserialize<'de> {
    Option::<T>::deserialize(deserializer)
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_uuid(value: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(value)
        .ok()
        .filter(|id| !id.is_nil() && id.to_string() == value)
        .ok_or_else(|| ApiError::bad_request("canonical nonnil prompt permission UUID required"))
}

#[cfg_attr(not(test), allow(dead_code))]
impl FederationPromptScope {
    fn validate(&self) -> Result<(), ApiError> {
        self.source_host.validate()?;
        self.target_host.validate()?;
        self.source.validate()?;
        self.target.validate()?;
        if self.source.host_id != self.source_host.host_id
            || self.target.host_id != self.target_host.host_id
            || self.source_host.host_id == self.target_host.host_id
            || self.enrollment_generation == 0
            || self.placement_generation == Some(0)
        {
            return Err(ApiError::bad_request(
                "invalid enqueueTextPrompt permission scope",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(not(test), allow(dead_code))]
enum FederationPromptPermissionKind {
    CallerGrant,
    DestinationConsent,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
struct FederationPromptPermissionIdentity {
    id: String,
    generation: u64,
}

#[cfg_attr(not(test), allow(dead_code))]
impl FederationPromptPermissionIdentity {
    fn validate(&self) -> Result<(), ApiError> {
        federation_prompt_uuid(&self.id)?;
        if self.generation == 0 {
            return Err(ApiError::bad_request(
                "prompt permission generation must be positive",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
struct FederationPromptPermission {
    kind: FederationPromptPermissionKind,
    identity: FederationPromptPermissionIdentity,
    issuer: FederationHostIdentity,
    scope: FederationPromptScope,
    creation_key: String,
    // A grant selects one independent destination consent; consent does not
    // grant the source permission and does not depend on a source-issued grant.
    #[serde(deserialize_with = "federation_prompt_required_option")]
    consent: Option<FederationPromptPermissionIdentity>,
    revoked: bool,
    // Evidence only. Local revocation is not a claim of remote observation.
    #[serde(deserialize_with = "federation_prompt_required_option")]
    receiver_observed_generation: Option<u64>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl FederationPromptPermission {
    fn validate(&self) -> Result<(), ApiError> {
        self.identity.validate()?;
        self.scope.validate()?;
        self.issuer.validate()?;
        validate_federation_reference(&self.creation_key)?;
        let expected_issuer = match self.kind {
            FederationPromptPermissionKind::CallerGrant => &self.scope.source_host,
            FederationPromptPermissionKind::DestinationConsent => &self.scope.target_host,
        };
        if &self.issuer != expected_issuer
            || (self.kind == FederationPromptPermissionKind::CallerGrant) != self.consent.is_some()
            || (!self.revoked && self.identity.generation != 1)
            || (self.revoked && self.identity.generation < 2)
            || self
                .receiver_observed_generation
                .is_some_and(|g| !self.revoked || g == 0 || g > self.identity.generation)
        {
            return Err(ApiError::bad_request(
                "invalid enqueueTextPrompt permission record",
            ));
        }
        if let Some(consent) = &self.consent {
            consent.validate()?;
        }
        Ok(())
    }

    fn same_permission(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.identity.id == other.identity.id
            && self.issuer == other.issuer
            && self.scope == other.scope
            && self.creation_key == other.creation_key
            && self.consent == other.consent
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
struct FederationPromptPermissions {
    schema_version: u8,
    records: Vec<FederationPromptPermission>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl Default for FederationPromptPermissions {
    fn default() -> Self {
        Self {
            schema_version: 1,
            records: Vec::new(),
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl FederationPromptPermissions {
    fn validate(&self) -> Result<(), ApiError> {
        if self.schema_version != 1 || self.records.len() > FEDERATION_PROMPT_PERMISSION_LIMIT {
            return Err(ApiError::bad_request(
                "unsupported or excessive prompt permission data",
            ));
        }
        let mut ids = BTreeSet::new();
        let mut keys = BTreeSet::new();
        for record in &self.records {
            record.validate()?;
            if !ids.insert(&record.identity.id)
                || !keys.insert((
                    record.issuer.host_id.as_str(),
                    record.issuer.host_generation,
                    match record.kind {
                        FederationPromptPermissionKind::CallerGrant => 0,
                        FederationPromptPermissionKind::DestinationConsent => 1,
                    },
                    record.creation_key.as_str(),
                ))
            {
                return Err(ApiError::bad_request(
                    "duplicate prompt permission identity or creation key",
                ));
            }
        }
        if serde_json::to_vec(self)
            .map_err(|_| ApiError::bad_request("invalid prompt permission encoding"))?
            .len()
            > FEDERATION_PROMPT_POLICY_BYTES
        {
            return Err(ApiError::bad_request(
                "prompt permission metadata capacity reached; retained identities are not evicted",
            ));
        }
        Ok(())
    }

    fn find(&self, id: &str) -> Option<&FederationPromptPermission> {
        self.records.iter().find(|record| record.identity.id == id)
    }

    // Called only on the candidate owned by mutate_federation_core*. This is
    // value validation, never an independent persistence or authority service.
    fn retain(
        &mut self,
        record: FederationPromptPermission,
    ) -> Result<FederationPromptPermission, ApiError> {
        record.validate()?;
        let mut candidate = self.clone();
        if let Some(index) = self
            .records
            .iter()
            .position(|old| old.identity.id == record.identity.id)
        {
            let old = &self.records[index];
            if old == &record {
                return Ok(old.clone());
            }
            if !old.same_permission(&record)
                || old.identity.generation > record.identity.generation
                || (old.revoked && !record.revoked)
                || (!record.revoked && old.identity.generation != record.identity.generation)
                || old
                    .receiver_observed_generation
                    .is_some_and(|g| record.receiver_observed_generation.is_none_or(|n| n < g))
            {
                return Err(ApiError::conflict(
                    "prompt permission identity, generation or retained revocation conflicts",
                ));
            }
            candidate.records[index] = record.clone();
        } else {
            candidate.records.push(record.clone());
        }
        candidate.validate()?;
        *self = candidate;
        Ok(record)
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl FederationMutationContext<'_> {
    fn prompt_native_root(&self, session: &str) -> Result<FederationSessionIdentity, ApiError> {
        let inner = self.native_state;
        let local = inner
            .federation
            .local
            .as_ref()
            .ok_or_else(federation_stale_link)?;
        // Reuse the existing complete-inventory/native-root owner. This helper
        // checks no mailbox membership and creates no binding.
        let record = federation_mailbox_root_locked(inner, session)?;
        let identity = FederationSessionIdentity {
            host_id: local.host_id.clone(),
            session_id: session.into(),
            creation_id: record.federation_native_creation_id.clone(),
        };
        identity.validate()?;
        Ok(identity)
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl AppState {
    // Operator-local boundary only. A consent is a policy record for an exact
    // existing target, not a session creation, binding, placement or activation.
    fn issue_federation_prompt_consent(
        &self,
        expected_version: u64,
        creation_key: String,
        scope: FederationPromptScope,
    ) -> Result<FederationPromptPermission, ApiError> {
        scope.validate()?;
        validate_federation_reference(&creation_key)?;
        if let Some(retained) = self.read_federation_prompt_creation_replay(
            FederationPromptPermissionKind::DestinationConsent, &creation_key, &scope, None,
        )? {
            return Ok(retained);
        }
        self.mutate_federation_permission(
            None,
            move |domain, settings, context| {
                if let Some(retained) = federation_prompt_creation_replay(
                    domain, settings, FederationPromptPermissionKind::DestinationConsent,
                    &creation_key, &scope, None,
                )? {
                    return Ok(retained);
                }
                if domain.version != expected_version {
                    return Err(ApiError::conflict("federation policy version changed"));
                }
                if settings.role != FederationRole::Satellite
                    || domain.local.as_ref() != Some(&scope.target_host)
                    || settings.central.as_ref() != Some(&scope.source_host)
                    || context.prompt_native_root(&scope.target.session_id)? != scope.target
                {
                    return Err(ApiError::from_status(
                        StatusCode::FORBIDDEN,
                        "destination consent must be issued by its existing native owner",
                    ));
                }
                federation_prompt_target_placement_locked(context.native_state, &scope)?;
                let source = domain
                    .peers
                    .get(&scope.source_host.host_id)
                    .ok_or_else(federation_stale_link)?;
                if source.identity != scope.source_host
                    || source.enrollment_generation != scope.enrollment_generation
                    || source.membership == FederationMembershipState::Revoked
                {
                    return Err(federation_stale_link());
                }
                let issuer = domain.local.clone().ok_or_else(federation_stale_link)?;
                let policy = domain
                    .prompt_permissions
                    .get_or_insert_with(FederationPromptPermissions::default);
                policy.retain(FederationPromptPermission {
                    kind: FederationPromptPermissionKind::DestinationConsent,
                    identity: FederationPromptPermissionIdentity {
                        id: Uuid::new_v4().to_string(),
                        generation: 1,
                    },
                    issuer,
                    scope,
                    creation_key,
                    consent: None,
                    revoked: false,
                    receiver_observed_generation: None,
                })
            },
        )
    }

    fn revoke_federation_prompt_permission(
        &self,
        expected_version: u64,
        id: &str,
    ) -> Result<FederationPromptPermission, ApiError> {
        federation_prompt_uuid(id)?;
        self.mutate_federation_core(
            false,
            |domain, settings, _| {
                if domain.version != expected_version {
                    return Err(ApiError::conflict("federation policy version changed"));
                }
                let local = domain.local.clone().ok_or_else(federation_stale_link)?;
                let policy = domain
                    .prompt_permissions
                    .as_mut()
                    .ok_or_else(|| ApiError::not_found("prompt permission not found"))?;
                let mut record = policy
                    .find(id)
                    .cloned()
                    .ok_or_else(|| ApiError::not_found("prompt permission not found"))?;
                let role = match record.kind {
                    FederationPromptPermissionKind::CallerGrant => FederationRole::Central,
                    FederationPromptPermissionKind::DestinationConsent => FederationRole::Satellite,
                };
                if settings.role != role || record.issuer.host_id != local.host_id {
                    return Err(ApiError::from_status(
                        StatusCode::FORBIDDEN,
                        "only the local permission issuer may revoke it",
                    ));
                }
                if !record.revoked {
                    record.identity.generation =
                        federation_next_generation(record.identity.generation)?;
                    record.revoked = true;
                }
                // Existing receiver observations remain history. A local source
                // commit does not fabricate a remote acknowledgement.
                policy.retain(record)
            },
            |_| {},
            EngramBudgetClock::Real,
        )
    }
}
