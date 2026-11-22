// Owns pair authentication and link-incarnation leases, not enrollment persistence.
// New federation core alongside remote.rs; does not reuse executor-proxy authority.

use super::*;

#[cfg(test)]
pub(crate) const FEDERATION_CAPABILITY_HEADER: &str = "x-termal-pair-capability";
pub(crate) const FEDERATION_INCARNATION_HEADER: &str = "x-termal-link-incarnation";
pub(crate) const FEDERATION_AUTHORITY_HEADER: &str = "x-termal-peer-authority";
pub(crate) const FEDERATION_PROTOCOL_VERSION: u16 = 1;

#[derive(Clone)]
pub(crate) struct FederationPairCapability(pub(crate) Arc<zeroize::Zeroizing<String>>);

impl FederationPairCapability {
    pub(crate) fn new(secret: String) -> Result<Self, ApiError> {
        let secret = zeroize::Zeroizing::new(secret);
        // Enrollment supplies 256 random bits, encoded as hex. References and
        // display aliases are never usable bearer capabilities.
        if secret.len() != 64 || !secret.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ApiError::bad_request("invalid federation pair capability"));
        }
        Ok(Self(Arc::new(secret)))
    }
}

#[derive(Default)]
pub(crate) struct FederationLinkState {
    pub(crate) incarnation: Option<String>,
    pub(crate) recorded_incarnation: Option<String>,
    pub(crate) security: Option<Arc<FederationSecurityContext>>,
    pub(crate) next_owner: u64,
    pub(crate) installed_owner: u64,
}

#[derive(Clone)]
pub(crate) struct FederationChannel {
    pub(crate) snapshot: FederationPeerSnapshot,
    pub(crate) basis: Arc<FederationTransportBasis>,
    pub(crate) app: Option<AppState>,
    pub(crate) capability: FederationPairCapability,
    pub(crate) server_instance_id: String,
    pub(crate) state: Arc<Mutex<FederationLinkState>>,
    pub(crate) bound: FederationRpcBound,
    pub(crate) opening: Arc<tokio::sync::Mutex<()>>,
    pub(crate) control_slots: Arc<tokio::sync::Semaphore>,
    pub(crate) control_tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    pub(crate) control: FederationControlSettings,
    // Controlled success fixtures share the fence's existing scripted clock.
    // This field cannot be configured by production settings or a request.
    #[cfg(test)]
    pub(crate) durability_clock: EngramBudgetClock,
    #[cfg(test)]
    pub(crate) connection_published: Arc<dyn Fn() + Send + Sync>,
    // Lost local commit acknowledgment fault, only after a REAL marker commit.
    #[cfg(test)]
    pub(crate) reference_publication_ack_lost: Arc<AtomicBool>,
}

impl FederationChannel {
    pub(crate) fn new(
        snapshot: FederationPeerSnapshot,
        capability: FederationPairCapability,
    ) -> Self {
        Self {
            basis: Arc::new(FederationTransportBasis::new(snapshot.clone())),
            app: None,
            snapshot,
            capability,
            server_instance_id: Uuid::new_v4().to_string(),
            state: Arc::new(Mutex::new(FederationLinkState::default())),
            bound: FederationRpcBound::default(),
            opening: Arc::new(tokio::sync::Mutex::new(())),
            control_slots: Arc::new(tokio::sync::Semaphore::new(4)),
            control_tasks: Arc::default(),
            control: FederationControlSettings::default(),
            #[cfg(test)]
            durability_clock: EngramBudgetClock::Real,
            #[cfg(test)]
            connection_published: Arc::new(|| {}),
            #[cfg(test)]
            reference_publication_ack_lost: Arc::new(AtomicBool::new(false)),
        }
    }

    #[cfg(test)]
    pub(crate) fn activate_pending(&self, incarnation: String) -> FederationLinkLease {
        let mut state = self.state.lock().expect("federation link mutex poisoned");
        self.retire_recorded(&mut state);
        state.incarnation = Some(incarnation.clone());
        FederationLinkLease {
            channel: self.clone(),
            incarnation,
        }
    }

    #[cfg(test)]
    pub(crate) fn activate(&self, incarnation: String) -> FederationLinkLease {
        let lease = self.activate_pending(incarnation.clone());
        self.state
            .lock()
            .expect("federation link mutex poisoned")
            .recorded_incarnation = Some(incarnation);
        lease
    }

    pub(crate) fn invalidate(&self) {
        let mut state = self.state.lock().expect("federation link mutex poisoned");
        self.retire_recorded(&mut state);
        state.incarnation = None;
        if let Some(context) = state.security.take() {
            context.retire();
        }
    }

    pub(crate) fn invalidate_if_current(&self, lease: &FederationLinkLease) {
        let mut state = self.state.lock().expect("federation link mutex poisoned");
        if state.incarnation.as_deref() == Some(lease.incarnation.as_str()) {
            self.retire_recorded(&mut state);
            state.incarnation = None;
            if let Some(context) = state.security.take() {
                context.retire();
            }
        }
    }

    pub(crate) fn lease(&self, incarnation: &str) -> Result<FederationLinkLease, ApiError> {
        let state = self.state.lock().expect("federation link mutex poisoned");
        if state.incarnation.as_deref() != Some(incarnation)
            || state.recorded_incarnation.as_deref() != Some(incarnation)
        {
            return Err(federation_stale_link());
        }
        Ok(FederationLinkLease {
            channel: self.clone(),
            incarnation: incarnation.to_owned(),
        })
    }

    pub(crate) fn current_lease(&self) -> Result<FederationLinkLease, ApiError> {
        let state = self.state.lock().expect("federation link mutex poisoned");
        let incarnation = state
            .incarnation
            .clone()
            .ok_or_else(federation_stale_link)?;
        if state.recorded_incarnation.as_deref() != Some(incarnation.as_str()) {
            return Err(federation_stale_link());
        }
        Ok(FederationLinkLease {
            channel: self.clone(),
            incarnation,
        })
    }
}

#[derive(Clone)]
pub(crate) struct FederationLinkLease {
    pub(crate) channel: FederationChannel,
    pub(crate) incarnation: String,
}

impl FederationLinkLease {
    // All local application follows link -> StateInner lock order. Transport
    // never holds either lock. No caller receives unfenced mutable state.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn with_current<T>(
        &self,
        app: &AppState,
        apply: impl FnOnce(&mut StateInner, &FederationPeerSnapshot) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        self.with_incarnation(|| {
            let basis = self.channel.transport_basis();
            let mut inner = app.inner.lock().expect("state mutex poisoned");
            let current = app.refresh_federation_transport_snapshot_locked(
                &inner,
                &basis,
                &self.incarnation,
            )?;
            apply(&mut inner, &current)
        })
    }

    pub(crate) fn with_incarnation<T>(
        &self,
        apply: impl FnOnce() -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        let state = self
            .channel
            .state
            .lock()
            .expect("federation link mutex poisoned");
        if state.incarnation.as_deref() != Some(self.incarnation.as_str()) {
            return Err(federation_stale_link());
        }
        apply()
    }
}

pub(crate) fn federation_stale_link() -> ApiError {
    ApiError::conflict("federation link changed; reconnect and reconcile the original operation")
}
