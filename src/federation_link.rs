// Owns pair authentication and link-incarnation leases, not enrollment persistence.
// New federation core alongside remote.rs; does not reuse executor-proxy authority.

#[cfg(test)]
const FEDERATION_CAPABILITY_HEADER: &str = "x-termal-pair-capability";
const FEDERATION_INCARNATION_HEADER: &str = "x-termal-link-incarnation";
const FEDERATION_AUTHORITY_HEADER: &str = "x-termal-peer-authority";
const FEDERATION_PROTOCOL_VERSION: u16 = 1;

#[derive(Clone)]
struct FederationPairCapability(Arc<zeroize::Zeroizing<String>>);

impl FederationPairCapability {
    fn new(secret: String) -> Result<Self, ApiError> {
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
struct FederationLinkState {
    incarnation: Option<String>,
    recorded_incarnation: Option<String>,
    security: Option<Arc<FederationSecurityContext>>,
    next_owner: u64,
    installed_owner: u64,
}

#[derive(Clone)]
struct FederationChannel {
    snapshot: FederationPeerSnapshot,
    basis: Arc<FederationTransportBasis>,
    app: Option<AppState>,
    capability: FederationPairCapability,
    server_instance_id: String,
    state: Arc<Mutex<FederationLinkState>>,
    bound: FederationRpcBound,
    opening: Arc<tokio::sync::Mutex<()>>,
    control_slots: Arc<tokio::sync::Semaphore>,
    control_tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    control: FederationControlSettings,
    // Controlled success fixtures share the fence's existing scripted clock.
    // This field cannot be configured by production settings or a request.
    #[cfg(test)]
    durability_clock: EngramBudgetClock,
    #[cfg(test)]
    connection_published: Arc<dyn Fn() + Send + Sync>,
    // Lost local commit acknowledgment fault, only after a REAL marker commit.
    #[cfg(test)]
    reference_publication_ack_lost: Arc<AtomicBool>,
}

impl FederationChannel {
    fn new(snapshot: FederationPeerSnapshot, capability: FederationPairCapability) -> Self {
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
    fn activate_pending(&self, incarnation: String) -> FederationLinkLease {
        let mut state = self.state.lock().expect("federation link mutex poisoned");
        self.retire_recorded(&mut state);
        state.incarnation = Some(incarnation.clone());
        FederationLinkLease {
            channel: self.clone(),
            incarnation,
        }
    }

    #[cfg(test)]
    fn activate(&self, incarnation: String) -> FederationLinkLease {
        let lease = self.activate_pending(incarnation.clone());
        self.state
            .lock()
            .expect("federation link mutex poisoned")
            .recorded_incarnation = Some(incarnation);
        lease
    }

    fn invalidate(&self) {
        let mut state = self.state.lock().expect("federation link mutex poisoned");
        self.retire_recorded(&mut state);
        state.incarnation = None;
        if let Some(context) = state.security.take() {
            context.retire();
        }
    }

    fn invalidate_if_current(&self, lease: &FederationLinkLease) {
        let mut state = self.state.lock().expect("federation link mutex poisoned");
        if state.incarnation.as_deref() == Some(lease.incarnation.as_str()) {
            self.retire_recorded(&mut state);
            state.incarnation = None;
            if let Some(context) = state.security.take() {
                context.retire();
            }
        }
    }

    fn lease(&self, incarnation: &str) -> Result<FederationLinkLease, ApiError> {
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

    fn current_lease(&self) -> Result<FederationLinkLease, ApiError> {
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
struct FederationLinkLease {
    channel: FederationChannel,
    incarnation: String,
}

impl FederationLinkLease {
    // All local application follows link -> StateInner lock order. Transport
    // never holds either lock. No caller receives unfenced mutable state.
    #[cfg_attr(not(test), allow(dead_code))]
    fn with_current<T>(
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

    fn with_incarnation<T>(
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

fn federation_stale_link() -> ApiError {
    ApiError::conflict("federation link changed; reconnect and reconcile the original operation")
}
