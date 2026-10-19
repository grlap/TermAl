// Owns bounded sender counters and MAC-verified machine-to-machine replies.
// Does not settle application intents or retry with fresh keys. Split from federation_rpc.rs.

impl FederationRpcClient {
    async fn allocate_authenticated_counter(
        &self,
        context: &Arc<FederationSecurityContext>,
    ) -> Result<u64, ApiError> {
        loop {
            let changed = context.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            self.channel.security_current(context)?;
            let blocked = {
                let mut sender = context.sender.lock().expect("sender window poisoned");
                if sender.next == u64::MAX {
                    return Err(federation_auth_refused());
                }
                let floor = sender
                    .outstanding
                    .first_key_value()
                    .map(|(counter, _)| *counter);
                if floor.is_some_and(|floor| sender.next - floor >= 64) {
                    floor
                } else {
                    let counter = sender.next;
                    sender.next += 1;
                    sender.outstanding.insert(
                        counter,
                        self.channel.control.clock.now() + Duration::from_secs(10),
                    );
                    context.changed.notify_waiters();
                    return Ok(counter);
                }
            };
            #[cfg(test)]
            if let Some(floor) = blocked {
                (self.allocation_blocked)(floor);
            }
            #[cfg(not(test))]
            let _ = blocked;
            changed.await;
        }
    }

    async fn request<T: DeserializeOwned>(&self, route: FederationRpcRoute) -> Result<T, ApiError> {
        (self.ensure_current)()?;
        let _permit = self.channel.bound.try_acquire()?;
        let (lease, path) = match &route {
            FederationRpcRoute::Health(lease) => (lease, "/federation/v1/health"),
            FederationRpcRoute::Onboarding(lease, probe) => (lease, probe.path()),
            FederationRpcRoute::Mailbox(lease, _) => (lease, "/federation/v1/mailbox"),
            FederationRpcRoute::NativeBinding(lease, _) => {
                (lease, "/federation/v1/mailbox/native-binding")
            }
        };
        lease.with_incarnation(|| Ok(()))?;
        let context = self.channel.security_context()?;
        if context.incarnation != lease.incarnation {
            return Err(federation_auth_refused());
        }
        // Serialize before allocating a transport counter. Local size/encoding
        // refusals have not started an application operation on the peer.
        let url = format!("http://{}{path}", self.address);
        let builder = match &route {
            FederationRpcRoute::Health(_) => self.client.get(url),
            FederationRpcRoute::Onboarding(_, probe) => probe.request(&self.client, url),
            FederationRpcRoute::Mailbox(_, body) => self.client.post(url).json(body),
            FederationRpcRoute::NativeBinding(_, body) => self.client.post(url).json(body),
        };
        let mut prepared = builder.build().map_err(|_| federation_auth_refused())?;
        let body = prepared
            .body()
            .and_then(|body| body.as_bytes())
            .unwrap_or_default()
            .to_vec();
        let limit = if route.mailbox_lease().is_some() {
            FEDERATION_MAILBOX_REQUEST_BYTES
        } else {
            4096
        };
        if body.len() > limit {
            return Err(ApiError::from_status(
                StatusCode::PAYLOAD_TOO_LARGE,
                "federation request exceeds negotiated size",
            ));
        }
        let request = async {
            let counter = self.allocate_authenticated_counter(&context).await?;
            // A cancelled caller is not settlement. The independent watchdog
            // also fences abandoned futures, preserving the allocation floor.
            if !context.watchdog_started.swap(true, Ordering::SeqCst) {
                tokio::spawn(federation_sender_watchdog(
                    self.channel.clone(),
                    context.clone(),
                ));
            }
            let direction = if self.channel.snapshot.local_role == FederationRole::Central {
                "c2s"
            } else {
                "s2c"
            };
            let key = if direction == "c2s" { 0 } else { 2 };
            let mut headers = prepared.headers().clone();
            headers.insert(
                FEDERATION_INCARNATION_HEADER,
                HeaderValue::from_str(&lease.incarnation).map_err(|_| federation_auth_refused())?,
            );
            headers.insert(
                FEDERATION_AUTHORITY_HEADER,
                HeaderValue::from_str(
                    &serde_json::to_string(&FederationRequestAuthority::outgoing(
                        &self.channel.snapshot,
                    ))
                    .map_err(|_| federation_auth_refused())?,
                )
                .map_err(|_| federation_auth_refused())?,
            );
            let representation = federation_http_representation(
                &context,
                counter,
                direction,
                prepared.method().as_str(),
                path,
                &headers,
                &body,
            )?;
            let tag = federation_mac(&context.keys[key], &[&representation])?;
            headers.insert(
                FEDERATION_MAC_HEADER,
                federation_mac_header(&context, direction, counter, &tag)?,
            );
            *prepared.headers_mut() = headers;
            let mut response = self
                .client
                .execute(prepared)
                .await
                .map_err(|_| federation_transport_unavailable())?;
            let status = response.status();
            let response_headers = response.headers().clone();
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| federation_transport_unavailable())?
            {
                if chunk.len() > route.response_limit().saturating_sub(bytes.len()) {
                    return Err(federation_transport_unavailable());
                }
                if bytes.capacity() < bytes.len() + chunk.len() {
                    let capacity = bytes
                        .capacity()
                        .saturating_mul(2)
                        .max(bytes.len() + chunk.len())
                        .min(route.response_limit());
                    bytes
                        .try_reserve_exact(capacity - bytes.len())
                        .map_err(|_| federation_transport_unavailable())?;
                }
                bytes.extend_from_slice(&chunk);
            }
            let (returned, tag) = federation_parse_mac(&response_headers, &context, direction)?;
            if returned != counter {
                return Err(federation_auth_refused());
            }
            let authenticated = federation_response_representation(
                &context,
                counter,
                direction,
                &representation,
                status,
                &response_headers,
                &bytes,
            )?;
            federation_verify_mac(&context.keys[key + 1], &[&authenticated], &tag)?;
            if let Err(error) = self.channel.security_current(&context) {
                if matches!(&route, FederationRpcRoute::NativeBinding(_, _)) {
                    // A verified read-only proof cannot survive local lease
                    // retirement. Preserve its typed stale-authority refusal.
                    context.settle(counter);
                    return Ok(Err(error));
                }
                return Err(error);
            }
            if let Err(error) = (self.ensure_current)() {
                if matches!(&route, FederationRpcRoute::NativeBinding(_, _)) {
                    context.settle(counter);
                    return Ok(Err(error));
                }
                if error.status == StatusCode::LOCKED {
                    context.settle(counter);
                    // Dispatch may already have committed. A local durability
                    // hold cannot turn the authenticated receipt into a known
                    // negative application result or consume its recovery key.
                    return Ok(Err(route.response_failure(error)));
                }
                return Err(error);
            }
            if !status.is_success() {
                // An authenticated transport refusal settles only this attempt;
                // application uncertainty and its idempotency key remain owned.
                context.settle(counter);
                if route.mailbox_lease().is_some() {
                    return Ok(Err(federation_mailbox_rpc_error(status)));
                }
                return Ok(Err(match status {
                    StatusCode::LOCKED
                    | StatusCode::TOO_MANY_REQUESTS
                    | StatusCode::CONFLICT
                    | StatusCode::FORBIDDEN => {
                        ApiError::from_status(status, "authenticated federation transport refusal")
                    }
                    _ => federation_transport_unavailable(),
                }));
            }
            let decoded =
                serde_json::from_slice(&bytes).map_err(|_| federation_transport_unavailable())?;
            context.settle(counter);
            Ok(Ok(decoded))
        };
        match tokio::time::timeout(Duration::from_secs(10), request).await {
            Ok(Ok(result)) => result,
            _ => {
                self.channel
                    .fence_security(context.owner, &context.incarnation);
                Err(route.response_failure(federation_transport_unavailable()))
            }
        }
    }
}

async fn federation_sender_watchdog(
    channel: FederationChannel,
    context: Arc<FederationSecurityContext>,
) {
    loop {
        let changed = context.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if channel.security_current(&context).is_err() {
            return;
        }
        let deadline = context
            .sender
            .lock()
            .expect("sender window poisoned")
            .outstanding
            .values()
            .min()
            .copied();
        if let Some(deadline) = deadline {
            tokio::select! {
                _ = &mut changed => {},
                _ = channel.control.clock.deadline(deadline) => {
                    let expired = context.sender.lock().expect("sender window poisoned").outstanding
                        .values().any(|deadline| *deadline <= channel.control.clock.now());
                    if expired { channel.fence_security(context.owner, &context.incarnation); return; }
                },
            }
        } else {
            changed.await;
        }
    }
}
