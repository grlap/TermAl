// Durable, explicitly federation-owned mailbox locators. These are not
// SessionRecords and can never acquire a legacy remote executor configuration.

// Failure-only test observations preserve details before API errors erase them.
// Non-test expansion is exactly the original expression, including its error.
#[cfg(test)]
macro_rules! reference_publication_observe {
    ($stage:literal, $operation:expr) => {
        ($operation).inspect_err(|error| reference_publication_observe_error($stage, error))
    };
}

#[cfg(not(test))]
macro_rules! reference_publication_observe {
    ($stage:literal, $operation:expr) => { $operation };
}

#[cfg(test)]
fn reference_publication_observe_error<E: std::fmt::Debug + 'static>(stage: &str, error: &E) {
    // libtest retains this stderr with a failing test; the launcher keeps it
    // in rust-tests.log. Use the same formatter that the forced-error test checks.
    eprintln!("{}", reference_publication_diagnostic_message(stage, error));
}

#[cfg(test)]
fn reference_publication_diagnostic_message<E: std::fmt::Debug + 'static>(
    stage: &str,
    error: &E,
) -> String {
    let erased = error as &dyn std::any::Any;
    let sqlite = erased.downcast_ref::<rusqlite::Error>().or_else(|| {
        erased.downcast_ref::<anyhow::Error>()
            .and_then(|error| error.downcast_ref::<rusqlite::Error>())
    });
    let sqlite_code = match sqlite {
        Some(rusqlite::Error::SqliteFailure(code, _)) => {
            format!("{:?}/extended={}", code.code, code.extended_code)
        }
        Some(error) => format!("non-engine SQLite error: {error:?}"),
        None => "not present in this error".to_owned(),
    };
    let io = erased.downcast_ref::<std::io::Error>().or_else(|| {
        erased.downcast_ref::<anyhow::Error>()
            .and_then(|error| error.downcast_ref::<std::io::Error>())
    });
    format!(
        "REFERENCE_PUBLICATION_DIAGNOSTIC stage={stage} sqlite={sqlite_code} io={io:?} error={error:?}"
    )
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationMailboxReference {
    id: String,
    generation: u64,
    participant_id: String,
    native_creation_id: String,
    binding: FederationMailboxBinding,
    owner: String,
    owner_creation: String,
    owner_binding: FederationMailboxBinding,
    intent: String,
    peer: FederationPeerSnapshot,
    placement: FederationPlacement,
    link_incarnation: String,
    // Cache only an exact durable local publication receipt. Loading prepared
    // descriptor bytes alone never opens visibility or establishes authority.
    #[serde(skip)]
    publication: Option<FederationReferencePublication>,
}

impl FederationMailboxReference {
    fn publication_marker(&self) -> Result<FederationReferencePublication> {
        Ok(FederationReferencePublication {
            schema_version: 1,
            reference_id: self.id.clone(), generation: self.generation,
            owner: self.owner.clone(), owner_creation: self.owner_creation.clone(),
            intent: self.intent.clone(),
            // This format follows the existing federation JSON-Value digest
            // ordering; changing serde_json map-order features needs an
            // explicit format decision. Runtime receipt data is excluded.
            digest: sha256_hex(&serde_json::to_vec(&json!({
                "schemaVersion": 1, "reference": self,
            }))?),
        })
    }

    fn is_published(&self) -> bool {
        self.publication.as_ref().is_some_and(|publication| {
            self.publication_marker().is_ok_and(|expected| expected == *publication)
        })
    }

    fn same_prepared(&self, other: &Self) -> bool {
        let mut left = self.clone();
        let mut right = other.clone();
        left.publication = None;
        right.publication = None;
        left == right
    }

    fn descriptor(&self) -> Value {
        json!({
            "id": self.id, "kind": "federationMailboxReference",
            "generation": self.generation, "participantId": self.participant_id,
            "nativeCreationId": self.native_creation_id, "binding": self.binding,
        })
    }

    fn validate(&self) -> Result<(), ApiError> {
        self.binding.validate()?;
        self.owner_binding.validate()?;
        self.peer.validate()?;
        if !mailbox_reference_handle(&self.id).is_ok_and(|handle| handle.as_deref() == Some(self.id.as_str()))
            || self.generation == 0
            || Uuid::parse_str(&self.native_creation_id).is_err()
            || Uuid::parse_str(&self.owner_creation).is_err()
            || self.intent.len() != 64
            || !self.intent.bytes().all(|b| b.is_ascii_hexdigit())
            || self.owner != self.owner_binding.identity.session_id
            || self.owner_binding.central != self.binding.central
            || self.owner_binding.identity.host_id != self.binding.central.host_id
            || self.peer.local != self.binding.central
            || self.peer.central != self.binding.central
            || self.binding.identity.host_id == self.binding.central.host_id
            || self.participant_id != self.binding.participant_key()
            || self.placement.identity != self.binding.identity
            || self.placement.placement_generation != self.binding.placement_generation
            || self.peer.peer.host_id != self.binding.identity.host_id
            || self.peer.local_role != FederationRole::Central
        {
            return Err(ApiError::bad_request("invalid mailbox reference identity"));
        }
        Ok(())
    }
}

struct FederationReferenceContext {
    owner: String,
    owner_creation: String,
    owner_binding: FederationMailboxBinding,
    binding: FederationMailboxBinding,
    placement: FederationPlacement,
    peer: FederationPeerSnapshot,
    lease: FederationLinkLease,
    handle: Arc<FederationPeerHandle>,
}

fn federation_reference_settled_locked(inner: &StateInner) -> Result<(), ApiError> {
    if !inner.federation_durable && !inner.federation_continuity_lost {
        return Err(ApiError::from_status(StatusCode::SERVICE_UNAVAILABLE,
            "REFERENCE_PUBLICATION_BUSY: federation persistence is settling; retry"));
    }
    Ok(())
}

impl FederationReferenceContext {
    fn with_current<T>(
        &self, app: &AppState,
        apply: impl FnOnce(&mut StateInner) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        self.lease.with_incarnation(|| {
            let basis = self.lease.channel.transport_basis();
            let mut inner = app.inner.lock().expect("state mutex poisoned");
            // Classify settlement before shared snapshot helpers label it
            // stale. Keep the same link -> state guard and currency checks.
            federation_reference_settled_locked(&inner)?;
            app.refresh_federation_transport_snapshot_locked(&inner, &basis, &self.lease.incarnation)?;
            self.check(app, &inner)?;
            apply(&mut inner)
        })
    }

    fn durability_clock(&self) -> EngramBudgetClock {
        #[cfg(test)]
        { self.handle.channel.durability_clock.clone() }
        #[cfg(not(test))]
        { EngramBudgetClock::Real }
    }

    fn check(&self, app: &AppState, inner: &StateInner) -> Result<(), ApiError> {
        federation_reference_settled_locked(inner)?;
        let central = federation_mailbox_central_locked(inner)?;
        let owner = federation_mailbox_root_locked(inner, &self.owner)?;
        if owner.federation_native_creation_id != self.owner_creation
            || owner.mailbox_binding() != Some(&self.owner_binding)
            || central != self.binding.central
            || self.owner_binding.central != central
            || inner.federation.placements.get(&self.placement.admission_key) != Some(&self.placement)
            || self.placement.state == FederationPlacementState::Completed
            || !federation_mailbox_placement_authority(&inner.federation, &inner.preferences.federation, &self.placement)
        {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        app.ensure_federation_peer_current_locked(inner, &self.peer)
    }

    fn matches(&self, row: &FederationMailboxReference) -> bool {
        row.owner == self.owner && row.owner_creation == self.owner_creation
            && row.owner_binding == self.owner_binding && row.binding == self.binding
            && row.placement == self.placement && row.peer == self.peer
            && row.link_incarnation == self.lease.incarnation
    }

    async fn proof(&self, app: &AppState) -> Result<FederationNativeBindingProof, ApiError> {
        self.with_current(app, |_| Ok(()))?;
        let request_id = Uuid::new_v4().to_string();
        let proof: FederationNativeBindingProof = self.handle.rpc.request(
            FederationRpcRoute::NativeBinding(self.lease.clone(), FederationNativeBindingRequest {
                protocol_version: FEDERATION_PROTOCOL_VERSION,
                request_id: request_id.clone(), binding: self.binding.clone(),
            }),
        ).await?;
        proof.authority.validate()?;
        if proof.protocol_version != FEDERATION_PROTOCOL_VERSION || proof.request_id != request_id
            || proof.binding != self.binding || proof.central != self.peer.central
            || proof.host != self.peer.peer || proof.native_session_id != self.binding.identity.session_id
            || Uuid::parse_str(&proof.native_creation_id).is_err()
            || proof.link_incarnation != self.lease.incarnation
            || proof.authority.local_role != FederationRole::Satellite
            || proof.authority.local != self.peer.peer || proof.authority.peer != self.peer.local
            || proof.authority.central != self.peer.central
            || proof.authority.enrollment_generation != self.peer.enrollment_generation
        {
            return Err(ApiError::conflict("native binding proof does not match the captured request"));
        }
        self.with_current(app, |_| Ok(()))?;
        Ok(proof)
    }
}

impl AppState {
    fn capture_federation_reference(
        &self, owner: &str, participant: &str,
    ) -> Result<FederationReferenceContext, ApiError> {
        let (binding, placement, owner_creation, owner_binding) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            federation_reference_settled_locked(&inner)?;
            let central = federation_mailbox_central_locked(&inner)?;
            let record = federation_mailbox_root_locked(&inner, owner)?;
            let owner_binding = record.mailbox_binding().cloned()
                .ok_or_else(|| ApiError::conflict("caller has no mailbox binding"))?;
            if owner_binding.central != central {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            let mut matches = inner.federation.placements.values().filter_map(|placement| {
                let binding = FederationMailboxBinding {
                    central: central.clone(), identity: placement.identity.clone(),
                    placement_generation: placement.placement_generation,
                };
                (binding.participant_key() == participant).then_some((binding, placement.clone()))
            });
            let (binding, placement) = matches.next()
                .ok_or_else(|| ApiError::conflict("current remote placement not found"))?;
            if matches.next().is_some() {
                return Err(ApiError::conflict("ambiguous remote placement"));
            }
            (binding, placement, record.federation_native_creation_id.clone(), owner_binding)
        };
        let handle = self.federation_peer_handle(&binding.identity.host_id)
            .ok_or_else(federation_transport_unavailable)?;
        let lease = handle.channel.current_lease()?;
        let peer = handle.channel.current_authority().map_err(|error| {
            if error.status == StatusCode::LOCKED {
                federation_reference_publication_busy()
            } else { error }
        })?;
        let context = FederationReferenceContext {
            owner: owner.to_owned(), owner_creation, owner_binding,
            binding, placement, peer, lease, handle,
        };
        context.with_current(self, |_| Ok(()))?;
        Ok(context)
    }
}
