// Owns bounded machine-to-machine federation HTTP, not routing or mailbox state.
// New async transport beside remote.rs; it never forwards through a browser.

#[derive(Clone)]
struct FederationRpcClient {
    client: reqwest::Client,
    address: SocketAddr,
    channel: FederationChannel,
    ensure_current: FederationAuthorityCheck,
}

enum FederationRpcRoute {
    Hello(String),
    OpenLink(FederationOpenLink),
    Health(FederationLinkLease),
    Mailbox(FederationLinkLease, FederationMailboxRequest),
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
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| ApiError::internal("failed to initialize federation HTTP client"))?;
        Ok(Self {
            client,
            address: SocketAddr::from(([127, 0, 0, 1], port)),
            channel,
            ensure_current,
        })
    }

    async fn request<T: DeserializeOwned>(&self, route: FederationRpcRoute) -> Result<T, ApiError> {
        (self.ensure_current)()?;
        let _permit = self.channel.bound.try_acquire()?;
        let path = match &route {
            FederationRpcRoute::Hello(_) => "/federation/v1/hello",
            FederationRpcRoute::OpenLink(_) => "/federation/v1/link",
            FederationRpcRoute::Health(_) => "/federation/v1/health",
            FederationRpcRoute::Mailbox(_,_) => "/federation/v1/mailbox",
        };
        let url = format!("http://{}{path}", self.address);
        let mut request = match &route {
            FederationRpcRoute::OpenLink(body) => self.client.post(url).json(body),
            FederationRpcRoute::Mailbox(_,body) => {
                if serde_json::to_vec(body).map_err(|_|ApiError::bad_request("invalid mailbox request"))?.len() > FEDERATION_MAILBOX_REQUEST_BYTES {
                    return Err(ApiError::from_status(StatusCode::PAYLOAD_TOO_LARGE,"central mailbox request exceeds negotiated size"));
                }
                self.client.post(url).json(body)
            }
            _ => self.client.get(url),
        }
        .header(
            FEDERATION_CAPABILITY_HEADER,
            self.channel.capability.header_value()?,
        )
        .header(
            FEDERATION_AUTHORITY_HEADER,
            serde_json::to_string(&FederationRequestAuthority::outgoing(
                &self.channel.snapshot,
            ))
            .map_err(|_| ApiError::internal("invalid federation request authority"))?,
        );
        match &route {
            FederationRpcRoute::Hello(incarnation) => {
                request = request.header(FEDERATION_INCARNATION_HEADER, incarnation)
            }
            FederationRpcRoute::OpenLink(body) => {
                request = request.header(FEDERATION_INCARNATION_HEADER, &body.incarnation)
            }
            FederationRpcRoute::Health(_) => {}
            FederationRpcRoute::Mailbox(_,_) => {}
        }
        if let FederationRpcRoute::Health(lease) = &route {
            lease.with_incarnation(|| Ok(()))?;
            request = request.header(FEDERATION_INCARNATION_HEADER, &lease.incarnation);
        }
        if let Some(lease) = route.mailbox_lease() {
            lease.with_incarnation(||Ok(()))?;
            request = request.header(FEDERATION_INCARNATION_HEADER,&lease.incarnation);
        }
        // Network awaits occur after all global/channel locks have been released.
        let mut response = request
            .send()
            .await
            .map_err(|_| route.response_failure(federation_transport_unavailable()))?;
        if !response.status().is_success() {
            if route.mailbox_lease().is_some() { return Err(federation_mailbox_rpc_error(response.status())); }
            return Err(match response.status() {
                StatusCode::CONFLICT => federation_stale_link(),
                StatusCode::LOCKED => ApiError::from_status(
                    StatusCode::LOCKED,
                    "federation persistence is settling; retry",
                ),
                StatusCode::FORBIDDEN => ApiError::from_status(
                    StatusCode::FORBIDDEN,
                    "federation pair authentication failed",
                ),
                StatusCode::TOO_MANY_REQUESTS => ApiError::from_status(
                    StatusCode::TOO_MANY_REQUESTS,
                    "federation peer RPC limit reached",
                ),
                _ => federation_transport_unavailable(),
            });
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| route.response_failure(federation_transport_unavailable()))?
        {
            if chunk.len() > route.response_limit().saturating_sub(bytes.len()) {
                return Err(route.response_failure(ApiError::bad_gateway(
                    "federation response exceeds protocol limit",
                )));
            }
            if route.mailbox_lease().is_some() && bytes.capacity() < bytes.len()+chunk.len() {
                let capacity=bytes.capacity().saturating_mul(2).max(bytes.len()+chunk.len()).min(route.response_limit());
                bytes.try_reserve_exact(capacity-bytes.len()).map_err(|_|route.response_failure(ApiError::from_status(
                    StatusCode::SERVICE_UNAVAILABLE,"central mailbox response allocation unavailable")))?;
            }
            bytes.extend_from_slice(&chunk);
        }
        let decoded = serde_json::from_slice(&bytes)
            .map_err(|_| route.response_failure(ApiError::bad_gateway("invalid federation response")))?;
        (self.ensure_current)().map_err(|error| route.response_failure(error))?;
        if let Some(lease) = route.mailbox_lease() {
            lease.with_incarnation(||Ok(())).map_err(|error| route.response_failure(error))?;
        }
        if let FederationRpcRoute::Health(lease) = &route {
            lease.with_incarnation(|| Ok(()))?;
        }
        Ok(decoded)
    }

    async fn open_link(&self) -> Result<FederationLinkLease, ApiError> {
        let _opening = self.channel.opening.try_lock().map_err(|_| {
            ApiError::from_status(
                StatusCode::TOO_MANY_REQUESTS,
                "federation link establishment already in progress",
            )
        })?;
        if self.channel.snapshot.local_role != FederationRole::Central {
            return Err(ApiError::bad_request(
                "only the central TermAl host initiates federation links",
            ));
        }
        let incarnation = Uuid::new_v4().to_string();
        let hello: FederationHello = self
            .request(FederationRpcRoute::Hello(incarnation.clone()))
            .await?;
        if hello.protocol_version != FEDERATION_PROTOCOL_VERSION
            || hello.host_id != self.channel.snapshot.peer.host_id
            || hello.host_generation != self.channel.snapshot.peer.host_generation
            || hello.enrollment_generation != self.channel.snapshot.enrollment_generation
        {
            return Err(federation_stale_link());
        }
        let lease = self.channel.activate_pending(incarnation.clone());
        let open = FederationOpenLink {
            protocol_version: FEDERATION_PROTOCOL_VERSION,
            receiver_instance_id: hello.server_instance_id,
            challenge: hello.challenge,
            incarnation,
            central: self.channel.snapshot.central.clone(),
            enrollment_generation: self.channel.snapshot.enrollment_generation,
        };
        let response = self
            .request::<FederationHealth>(FederationRpcRoute::OpenLink(open))
            .await;
        match response.and_then(|health| self.validate_health(&lease, health)) {
            Ok(()) => match self.channel.record_connected(&lease).await {
                Ok(()) => Ok(lease),
                Err(error) => {
                    self.channel.invalidate();
                    Err(error)
                }
            },
            Err(error) => {
                self.channel.invalidate();
                Err(error)
            }
        }
    }

    async fn health(&self, lease: &FederationLinkLease) -> Result<(), ApiError> {
        let response = self
            .request(FederationRpcRoute::Health(lease.clone()))
            .await?;
        self.validate_health(lease, response)
    }

    fn validate_health(
        &self,
        lease: &FederationLinkLease,
        health: FederationHealth,
    ) -> Result<(), ApiError> {
        lease.with_incarnation(|| {
            if health.protocol_version != FEDERATION_PROTOCOL_VERSION
                || health.host_id != self.channel.snapshot.peer.host_id
                || health.host_generation != self.channel.snapshot.peer.host_generation
                || health.enrollment_generation != self.channel.snapshot.enrollment_generation
                || health.link_incarnation != lease.incarnation
            {
                return Err(federation_stale_link());
            }
            Ok(())
        })
    }
}

fn federation_transport_unavailable() -> ApiError {
    ApiError::from_status(
        StatusCode::SERVICE_UNAVAILABLE,
        "federation peer unavailable; original operation outcome may be unknown",
    )
}
