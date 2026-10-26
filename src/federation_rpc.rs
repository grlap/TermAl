// Owns bounded machine-to-machine federation HTTP, not routing or mailbox state.
// New async transport beside remote.rs; it never forwards through a browser.

#[derive(Clone)]
struct FederationRpcClient {
    client: reqwest::Client,
    address: SocketAddr,
    channel: FederationChannel,
    ensure_current: FederationAuthorityCheck,
    #[cfg(test)]
    allocation_blocked: Arc<dyn Fn(u64) + Send + Sync>,
}

enum FederationRpcRoute {
    Health(FederationLinkLease),
    Onboarding(FederationLinkLease, FederationOnboardingRpc),
    Mailbox(FederationLinkLease, FederationMailboxRequest),
    NativeBinding(FederationLinkLease, FederationNativeBindingRequest),
    ExecutorWake(FederationLinkLease, FederationExecutorWakeRequest),
    ExecutorOutcome(FederationLinkLease, FederationExecutorOutcomeRequest),
    Binding(FederationLinkLease, FederationBindingCommand),
    Participation(FederationLinkLease, FederationParticipationCommand),
    MailboxRelay(FederationLinkLease, MailboxRelayRequest),
}

impl FederationRpcClient {
    fn new(
        port: u16,
        channel: FederationChannel,
        ensure_current: FederationAuthorityCheck,
    ) -> Result<Self, ApiError> {
        if port == 0 {
            return Err(ApiError::bad_request("invalid federation forward port"));
        }
        let builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .no_gzip();
        #[cfg(test)]
        let builder = if channel.control.clock.io_deadlines {
            // The fixture's channel clock owns the same request budget; an
            // independent reqwest wall timer must not race its controlled time.
            builder
        } else {
            builder.timeout(Duration::from_secs(10))
        };
        #[cfg(not(test))]
        let builder = builder.timeout(Duration::from_secs(10));
        let client = builder
            .build()
            .map_err(|_| ApiError::internal("failed to initialize federation HTTP client"))?;
        Ok(Self {
            client,
            address: SocketAddr::from(([127, 0, 0, 1], port)),
            channel,
            ensure_current,
            #[cfg(test)]
            allocation_blocked: Arc::new(|_| {}),
        })
    }

    #[cfg(test)]
    async fn open_link(&self) -> Result<FederationLinkLease, ApiError> {
        let context = self.channel.security_context()?;
        self.channel.security_current(&context)?;
        self.channel.current_lease()
    }
    async fn health(&self, lease: &FederationLinkLease) -> Result<(), ApiError> {
        // Onboarding reports are diagnostic; their probes do not own the
        // lifetime of this already authenticated connection.
        self.authenticated_health(lease).await
    }

    async fn authenticated_health(&self, lease: &FederationLinkLease) -> Result<(), ApiError> {
        let response = self
            .request(FederationRpcRoute::Health(lease.clone()))
            .await?;
        self.validate_health(lease, response)
    }

    fn retain_health_refusal(&self, lease: &FederationLinkLease, error: &ApiError) -> bool {
        if lease.with_incarnation(|| Ok(())).is_err() {
            // MAC, transport and replay failures already retire their context
            // in the authenticated RPC owner; never revive that connection.
            return false;
        }
        if let Err(authority) = self.channel.current_authority() {
            if !matches!(authority.status, StatusCode::LOCKED | StatusCode::TOO_MANY_REQUESTS) {
                self.channel.invalidate_if_current(lease);
                return false;
            }
        }
        // A signed application refusal (including 409) is diagnostic while
        // the actual pair authority is current. Its status is not a new fence.
        let _ = error;
        self.channel.observe_attestation_failure(None, "health", "diagnosticUnavailable", false);
        true
    }

    fn validate_health(
        &self,
        lease: &FederationLinkLease,
        health: FederationHealth,
    ) -> Result<(), ApiError> {
        let outcome = lease.with_incarnation(|| {
            if health.protocol_version != FEDERATION_PROTOCOL_VERSION
                || health.host_id != self.channel.snapshot.peer.host_id
                || health.host_generation != self.channel.snapshot.peer.host_generation
                || health.enrollment_generation != self.channel.snapshot.enrollment_generation
                || health.link_incarnation != lease.incarnation
            {
                return Err(federation_stale_link());
            }
            Ok(())
        });
        if outcome.is_err() {
            self.channel.invalidate_if_current(lease);
        }
        outcome
    }
}

fn federation_transport_unavailable() -> ApiError {
    ApiError::from_status(
        StatusCode::SERVICE_UNAVAILABLE,
        "federation peer unavailable; original operation outcome may be unknown",
    )
}
