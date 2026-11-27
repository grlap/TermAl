// Owns bounded sender counters and MAC-verified machine-to-machine replies.
// Does not settle application intents or retry with fresh keys. Split from federation_rpc.rs.

use super::*;

/// Keeps the local refusals before anything is sent, the post-reply
/// authority failure and the request deadline apart from every other request
/// failure, so a relay caller can report which check refused it. Each carries
/// the same API error the caller would otherwise receive.
pub(crate) enum FederationRpcRequestError {
    /// Local route authority, the link incarnation, its security context or
    /// the sender counter refused before anything was sent.
    BeforeSendAuthority(ApiError),
    /// The local peer RPC bound was full, or the request deadline passed
    /// while it waited for a sender counter, before anything was sent. Any
    /// wait for the window counts, even if earlier local checks used most of
    /// the budget; either way the deciding check is local, not the origin.
    BeforeSendCapacity(ApiError),
    /// The caller's deadline passed in the local checks before sending,
    /// with a sender counter free; nothing was sent.
    BeforeSendDeadline(ApiError),
    AfterReplyAuthority(ApiError),
    /// The request deadline passed after a counter was allocated.
    TimedOut(ApiError),
    Other(ApiError),
}

impl From<ApiError> for FederationRpcRequestError {
    fn from(error: ApiError) -> Self {
        Self::Other(error)
    }
}

impl FederationRpcRequestError {
    fn into_api_error(self) -> ApiError {
        match self {
            Self::BeforeSendAuthority(error)
            | Self::BeforeSendCapacity(error)
            | Self::BeforeSendDeadline(error)
            | Self::AfterReplyAuthority(error)
            | Self::TimedOut(error)
            | Self::Other(error) => error,
        }
    }
}

impl FederationRpcClient {
    #[cfg(test)]
    pub(crate) async fn allocate_authenticated_counter(
        &self,
        context: &Arc<FederationSecurityContext>,
    ) -> Result<u64, ApiError> {
        self.allocate_authenticated_counter_for(context, "test")
            .await
            .map(|(counter, _)| counter)
    }

    /// Allocates the next sender counter for one request; `operation` is the
    /// request's fixed local name, which its retirement record names. Also
    /// says whether the allocation had to wait for the sender window.
    async fn allocate_authenticated_counter_for(
        &self,
        context: &Arc<FederationSecurityContext>,
        operation: &'static str,
    ) -> Result<(u64, bool), ApiError> {
        let mut waited = false;
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
                        FederationSenderDeadline::new(self.channel.control.clock.now(), operation),
                    );
                    context.changed.notify_waiters();
                    return Ok((counter, waited));
                }
            };
            #[cfg(test)]
            if let Some(floor) = blocked {
                (self.allocation_blocked)(floor);
            }
            #[cfg(not(test))]
            let _ = blocked;
            waited = true;
            changed.await;
        }
    }

    pub(crate) async fn request<T: DeserializeOwned>(
        &self,
        route: FederationRpcRoute,
    ) -> Result<T, ApiError> {
        let retry = match &route {
            FederationRpcRoute::Mailbox(lease, request)
                if !matches!(
                    request.operation,
                    FederationMailboxOperation::ReadKeyed { .. }
                ) =>
            {
                Some((lease.clone(), request.clone()))
            }
            _ => None,
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let first = self
            .request_once(route)
            .await
            .map_err(FederationRpcRequestError::into_api_error);
        if first.as_ref().err().is_some_and(|error| {
            matches!(
                error.kind,
                Some(
                    ApiErrorKind::FederationMailboxWriterAdmission
                        | ApiErrorKind::FederationMailboxDirectoryFence
                )
            )
        }) {
            if let Some((lease, request)) = retry {
                return match tokio::time::timeout_at(
                    deadline,
                    self.request_once(FederationRpcRoute::Mailbox(lease, request)),
                )
                .await
                {
                    Ok(result) => result.map_err(FederationRpcRequestError::into_api_error),
                    Err(_) => Err(federation_mailbox_unknown_outcome(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "central mailbox transport_loss during exact retry",
                    )),
                };
            }
        }
        first
    }

    pub(crate) async fn request_once<T: DeserializeOwned>(
        &self,
        route: FederationRpcRoute,
    ) -> Result<T, FederationRpcRequestError> {
        self.request_once_within(route, None).await
    }

    /// One request whose whole exchange, counter allocation included, ends
    /// at the caller's deadline when it gives one instead of the route
    /// default. The caller fixes the deadline, so its own earlier checks
    /// count against the same budget.
    pub(crate) async fn request_once_within<T: DeserializeOwned>(
        &self,
        route: FederationRpcRoute,
        caller_deadline: Option<tokio::time::Instant>,
    ) -> Result<T, FederationRpcRequestError> {
        let policy_deadline = matches!(&route, FederationRpcRoute::PromptPolicy(_, _)).then(|| {
            tokio::time::Instant::from_std(
                self.channel.control.clock.now() + Duration::from_secs(10),
            )
        });
        let policy_permit = if matches!(&route, FederationRpcRoute::PromptPolicy(_, _)) {
            let permit = Arc::new(self.channel.bound.try_acquire()?);
            let job_permit = permit.clone();
            let check = self.ensure_current.clone();
            self.channel
                .control
                .clock
                .timeout_at(
                    policy_deadline.expect("policy deadline"),
                    run_blocking_api(move || {
                        let _permit = job_permit;
                        check()
                    }),
                )
                .await
                .map_err(|_| federation_prompt_policy_unknown_outcome())??;
            Some(permit)
        } else {
            (self.ensure_current)().map_err(FederationRpcRequestError::BeforeSendAuthority)?;
            None
        };
        let _permit = match policy_permit {
            Some(permit) => permit,
            None => Arc::new(
                self.channel
                    .bound
                    .try_acquire()
                    .map_err(FederationRpcRequestError::BeforeSendCapacity)?,
            ),
        };
        let (lease, path) = match &route {
            FederationRpcRoute::Health(lease) => (lease, "/federation/v1/health"),
            FederationRpcRoute::PromptPolicy(lease, _) => (lease, FEDERATION_PROMPT_POLICY_ROUTE),
            FederationRpcRoute::HostUtilization(lease) => {
                (lease, "/federation/v1/host-utilization")
            }
            FederationRpcRoute::Onboarding(lease, probe) => (lease, probe.path()),
            FederationRpcRoute::Mailbox(lease, request) => (
                lease,
                if matches!(
                    request.operation,
                    FederationMailboxOperation::Directory { alias_basis: true }
                ) {
                    "/federation/v1/mailbox?aliasBasis=true"
                } else {
                    "/federation/v1/mailbox"
                },
            ),
            FederationRpcRoute::ExecutorWake(lease, _) => (lease, FEDERATION_EXECUTOR_WAKE_ROUTE),
            FederationRpcRoute::ExecutorOutcome(lease, _) => {
                (lease, FEDERATION_EXECUTOR_OUTCOME_ROUTE)
            }
            FederationRpcRoute::NativeBinding(lease, _) => {
                (lease, "/federation/v1/mailbox/native-binding")
            }
            FederationRpcRoute::Binding(lease, _) => (lease, "/federation/v1/mailbox/binding"),
            FederationRpcRoute::Participation(lease, _) => {
                (lease, "/federation/v1/mailbox/participation")
            }
            FederationRpcRoute::MailboxRelay(lease, _) => (lease, MAILBOX_RELAY_RPC_ROUTE),
        };
        lease
            .with_incarnation(|| Ok(()))
            .map_err(FederationRpcRequestError::BeforeSendAuthority)?;
        let context = self
            .channel
            .security_context()
            .map_err(FederationRpcRequestError::BeforeSendAuthority)?;
        if context.incarnation != lease.incarnation {
            return Err(FederationRpcRequestError::BeforeSendAuthority(
                federation_auth_refused(),
            ));
        }
        // Local install precedes central install on a satellite. Refuse before
        // allocating a counter or sending HTTP; this is not a link failure.
        if self.channel.snapshot.local_role == FederationRole::Satellite
            && !context.outgoing_rpc_ready.load(Ordering::SeqCst)
        {
            return Err(FederationRpcRequestError::BeforeSendAuthority(
                ApiError::from_status(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "federation peer handshake is completing; retry the original operation",
                ),
            ));
        }
        // Serialize before allocating a transport counter. Local size/encoding
        // refusals have not started an application operation on the peer.
        let url = format!("http://{}{path}", self.address);
        let builder = match &route {
            FederationRpcRoute::Health(_) => self.client.get(url),
            FederationRpcRoute::PromptPolicy(_, body) => self.client.post(url).json(body),
            FederationRpcRoute::HostUtilization(_) => self.client.get(url),
            FederationRpcRoute::Onboarding(_, probe) => probe.request(&self.client, url),
            FederationRpcRoute::Mailbox(_, body) => self.client.post(url).json(body),
            FederationRpcRoute::ExecutorWake(_, body) => self.client.post(url).json(body),
            FederationRpcRoute::ExecutorOutcome(_, body) => self.client.post(url).json(body),
            FederationRpcRoute::NativeBinding(_, body) => self.client.post(url).json(body),
            FederationRpcRoute::Binding(_, body) => self.client.post(url).json(body),
            FederationRpcRoute::Participation(_, body) => self.client.post(url).json(body),
            FederationRpcRoute::MailboxRelay(_, body) => self.client.post(url).json(body),
        };
        let mut prepared = builder.build().map_err(|_| federation_auth_refused())?;
        let body = prepared
            .body()
            .and_then(|body| body.as_bytes())
            .unwrap_or_default()
            .to_vec();
        let limit = match &route {
            FederationRpcRoute::Mailbox(_, _) | FederationRpcRoute::MailboxRelay(_, _) => {
                FEDERATION_MAILBOX_REQUEST_BYTES
            }
            FederationRpcRoute::ExecutorWake(_, _) | FederationRpcRoute::ExecutorOutcome(_, _) => {
                32 * 1024
            }
            FederationRpcRoute::PromptPolicy(_, body) => {
                body.validate()?;
                FEDERATION_PROMPT_POLICY_REQUEST_BYTES
            }
            _ => 4096,
        };
        if body.len() > limit {
            return Err(ApiError::from_status(
                StatusCode::PAYLOAD_TOO_LARGE,
                "federation request exceeds negotiated size",
            )
            .into());
        }
        let mut rpc_stage = "counter";
        // Whether a sender counter was allocated. Until then nothing is sent.
        // The stage name alone cannot say so: the reply's counter check after
        // sending uses the same stage name.
        let mut counter_allocated = false;
        let mut rpc_status = None;
        let mut after_reply_authority_refused = false;
        // Only a reply with no MAC header at all, such as the receiver's own
        // unsigned 408, is unanswered. A present but malformed, duplicate or
        // foreign MAC header is a security event.
        let mut mac_header_absent = false;
        let telemetry = matches!(&route, FederationRpcRoute::HostUtilization(_));
        let operation = route.operation();
        // A cancelled caller drops this guard, which retires its unsettled
        // counter as UNKNOWN, so it never holds the allocation floor.
        let mut allocated = FederationCounterGuard {
            context: &context,
            clock: &self.channel.control.clock,
            counter: None,
        };
        // Boxed so that its state lives on the heap: this future holds the
        // whole authenticated exchange across its awaits, and callers that
        // await it inline on one thread would otherwise carry it on their
        // stack.
        let request = Box::pin(async {
            let (counter, waited) = self
                .allocate_authenticated_counter_for(&context, operation)
                .await?;
            counter_allocated = true;
            allocated.counter = Some(counter);
            // A counter obtained only after the caller's deadline is not used,
            // and nothing is sent. If the allocation waited for the sender
            // window, local capacity took the budget; otherwise the local
            // checks before it did.
            if caller_deadline
                .is_some_and(|deadline| self.channel.control.clock.now() >= deadline.into_std())
            {
                rpc_stage = if waited {
                    "deadlineBeforeSend"
                } else {
                    "deadlineInLocalChecks"
                };
                return Err(federation_transport_unavailable());
            }
            // A verified reply settles its counter. If the watchdog already
            // retired it as UNKNOWN, the caller keeps that outcome, so the
            // caller's result and the retirement record agree.
            let mut settled_early = false;
            macro_rules! settle_verified {
                () => {
                    if !settled_early && !context.settle(counter) {
                        rpc_stage = "retired";
                        return Err(federation_transport_unavailable());
                    }
                };
            }
            let mut early_decoded: Option<T> = None;
            // The independent watchdog, as a backstop, retires a counter still
            // unsettled past the unchanged sender window. It never fences.
            if !context.watchdog_started.swap(true, Ordering::SeqCst) {
                tokio::spawn(federation_sender_watchdog(
                    self.channel.clone(),
                    context.clone(),
                ));
            }
            rpc_stage = "requestEncode";
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
            // Encoding and signing above are local work too. A caller
            // deadline that passed during them is not spent on the origin:
            // nothing is sent, as after the allocation check.
            #[cfg(test)]
            (self.before_dispatch)();
            // The allocation already returned within the budget, so any
            // waiting for the window ended in time: a deadline that passes
            // here was spent in local work.
            if caller_deadline
                .is_some_and(|deadline| self.channel.control.clock.now() >= deadline.into_std())
            {
                rpc_stage = "deadlineInLocalChecks";
                return Err(federation_transport_unavailable());
            }
            rpc_stage = "execute";
            let mut response = self.client.execute(prepared).await.map_err(|_error| {
                #[cfg(test)]
                eprintln!("FEDERATION_RPC_DIAGNOSTIC execute error={_error:?}");
                federation_transport_unavailable()
            })?;
            let status = response.status();
            rpc_status = Some(status.as_u16());
            let response_headers = response.headers().clone();
            // The MAC header and its counter are checked as soon as the
            // headers arrive, so a later body failure cannot hide a malformed,
            // duplicate or foreign header. Only a reply with no MAC header at
            // all, the receiver's unsigned 408, is unanswered.
            rpc_stage = "macParse";
            mac_header_absent = !response_headers.contains_key(FEDERATION_MAC_HEADER);
            let (returned, tag) = federation_parse_mac(&response_headers, &context, direction)?;
            rpc_stage = "counter";
            if returned != counter {
                return Err(federation_auth_refused());
            }
            rpc_stage = "responseBody";
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_error| {
                #[cfg(test)]
                eprintln!("FEDERATION_RPC_DIAGNOSTIC responseBody error={_error:?}");
                federation_transport_unavailable()
            })? {
                if chunk.len() > route.response_limit().saturating_sub(bytes.len()) {
                    // A body past the negotiated limit is a protocol
                    // violation, never verified, not a failed read: it fences
                    // like a MAC failure. A failed body stream stays local.
                    rpc_stage = "responseLimit";
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
            rpc_stage = "responseEncode";
            let authenticated = federation_response_representation(
                &context,
                counter,
                direction,
                &representation,
                status,
                &response_headers,
                &bytes,
            )?;
            rpc_stage = "verify";
            federation_verify_mac(&context.keys[key + 1], &[&authenticated], &tag)?;
            rpc_stage = "securityCurrent";
            if let Err(error) = self.channel.security_current(&context) {
                after_reply_authority_refused = true;
                if matches!(&route, FederationRpcRoute::NativeBinding(_, _)) {
                    // A verified read-only proof cannot survive local lease
                    // retirement. Preserve its typed stale-authority refusal.
                    settle_verified!();
                    return Ok(Err(error));
                }
                return Err(error);
            }
            rpc_stage = "authority";
            let current = if matches!(&route, FederationRpcRoute::PromptPolicy(_, _)) {
                // The MAC/counter and current security context already prove
                // receipt. A later local owner wait must not leave a watchdog
                // outstanding or turn its deadline into a pair-link failure.
                // An undecodable success is decided first, while the counter
                // is still outstanding, so it is retired with a record.
                if status.is_success() {
                    rpc_stage = "decode";
                    early_decoded = Some(
                        serde_json::from_slice(&bytes)
                            .map_err(|_| federation_transport_unavailable())?,
                    );
                    rpc_stage = "authority";
                }
                settle_verified!();
                settled_early = true;
                let check = self.ensure_current.clone();
                let job_permit = _permit.clone();
                run_blocking_api(move || {
                    let _permit = job_permit;
                    check()
                })
                .await
            } else {
                (self.ensure_current)()
            };
            if let Err(error) = current {
                after_reply_authority_refused = true;
                if matches!(&route, FederationRpcRoute::NativeBinding(_, _)) {
                    settle_verified!();
                    return Ok(Err(error));
                }
                if error.status == StatusCode::LOCKED {
                    settle_verified!();
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
                settle_verified!();
                if let FederationRpcRoute::MailboxRelay(_, relay_request) = &route {
                    return Ok(Err(
                        if status == StatusCode::NOT_FOUND
                            && serde_json::from_slice::<ErrorResponse>(&bytes).is_ok()
                        {
                            if matches!(
                                &relay_request.operation,
                                MailboxRelayOperation::Read { .. }
                                    | MailboxRelayOperation::Acknowledge { .. }
                                    | MailboxRelayOperation::ReadMessage { .. }
                                    | MailboxRelayOperation::LookupMailbox { .. }
                            ) && serde_json::from_slice::<Value>(&bytes).is_ok_and(|body| {
                                body["code"].as_str() == Some(UNKNOWN_NATIVE_MAILBOX_LOCATOR_CODE)
                            }) {
                                unknown_native_mailbox_locator()
                            } else {
                                ApiError::not_found(
                                    "authenticated native mailbox resource or membership not found",
                                )
                            }
                        } else if status == StatusCode::NOT_FOUND {
                            ApiError::conflict(
                                "native mailbox relay capability or membership unavailable; both hosts must support the relay protocol; no local fallback",
                            )
                        } else if status.is_client_error() {
                            mailbox_relay_verified_refusal(relay_request, &body, status, &bytes)
                        } else {
                            mailbox_relay_verified_writer_admission(
                                relay_request,
                                &body,
                                status,
                                &bytes,
                            )
                            .unwrap_or_else(|| {
                                federation_mailbox_unknown_outcome(
                                    status,
                                    "native mailbox relay response unavailable",
                                )
                            })
                        },
                    ));
                }
                if matches!(&route, FederationRpcRoute::Binding(_, _)) {
                    return Ok(Err(federation_binding_rpc_error(status)));
                }
                if matches!(&route, FederationRpcRoute::Participation(_, _)) {
                    return Ok(Err(if status == StatusCode::NOT_FOUND {
                        ApiError::conflict(
                            "automatic mailbox participation is unavailable on the peer; both hosts must support it; no local fallback",
                        )
                    } else {
                        federation_binding_rpc_error(status)
                    }));
                }
                if route.mailbox_lease().is_some() {
                    let FederationRpcRoute::Mailbox(_, request) = &route else {
                        unreachable!()
                    };
                    return Ok(Err(federation_mailbox_verified_error(
                        request, status, &bytes,
                    )));
                }
                return Ok(Err(match status {
                    StatusCode::LOCKED
                    | StatusCode::TOO_MANY_REQUESTS
                    | StatusCode::CONFLICT
                    | StatusCode::FORBIDDEN
                    | StatusCode::UNAUTHORIZED => {
                        ApiError::from_status(status, "authenticated federation transport refusal")
                    }
                    _ => federation_transport_unavailable(),
                }));
            }
            let decoded = match early_decoded {
                Some(decoded) => decoded,
                None => {
                    rpc_stage = "decode";
                    serde_json::from_slice(&bytes)
                        .map_err(|_| federation_transport_unavailable())?
                }
            };
            settle_verified!();
            Ok(Ok(decoded))
        });
        #[cfg(test)]
        let request_started_at = std::time::Instant::now();
        let timeout = if telemetry {
            host_utilization::PEER_DEADLINE
        } else {
            Duration::from_secs(10)
        };
        let result = if let Some(deadline) = policy_deadline.or(caller_deadline) {
            self.channel
                .control
                .clock
                .timeout_at(deadline, request)
                .await
        } else {
            self.channel.control.clock.timeout(timeout, request).await
        };
        let timed_out = result.is_err();
        // The receiver's own deadline answers with an unsigned 408: no
        // authenticated reply arrived, which proves nothing either way.
        let receiver_deadline = rpc_stage == "macParse"
            && mac_header_absent
            && rpc_status == Some(StatusCode::REQUEST_TIMEOUT.as_u16());
        let unanswered = timed_out
            || matches!(rpc_stage, "execute" | "responseBody" | "decode" | "retired")
            || receiver_deadline;
        let result = match result {
            Ok(Ok(result)) => result,
            Ok(Err(error))
                if matches!(rpc_stage, "deadlineBeforeSend" | "deadlineInLocalChecks") =>
            {
                // Nothing was sent. The unused counter is retired with a
                // record that says so; the link is not fenced.
                allocated.retire("unsentAfterDeadline");
                Err(error)
            }
            failure
                if policy_deadline.is_some() && rpc_stage == "authority" && failure.is_err() =>
            {
                // The blocking authority job still owns its permit. Its late
                // result cannot supply policy to the expired caller.
                allocated.retire("timeout");
                Err(federation_prompt_policy_unknown_outcome())
            }
            failure
                if telemetry
                    && (failure.is_err()
                        || matches!(
                            rpc_stage,
                            "execute" | "responseBody" | "decode" | "retired"
                        )) =>
            {
                // This GET has no application outcome to recover. Its counter
                // is retired with a record on a bounded read failure; it must
                // not retire a healthy pair merely because telemetry was
                // unavailable. MAC/replay/authority failures still fence.
                allocated.retire(if timed_out { "timeout" } else { "transport" });
                Err(federation_transport_unavailable())
            }
            _ if unanswered => {
                #[cfg(test)]
                eprintln!(
                    "FEDERATION_RPC_DIAGNOSTIC unanswered stage={rpc_stage} status={rpc_status:?} timed_out={timed_out}"
                );
                // A slow, unanswered, unreadable or undecodable ordinary
                // request is an UNKNOWN outcome for that request only. It
                // never tears down the authenticated link; its counter is
                // retired with a record, and the replay window still refuses
                // any late duplicate. Security failures fence below.
                allocated.retire(if timed_out {
                    "timeout"
                } else if rpc_stage == "macParse" {
                    "receiverDeadline"
                } else {
                    "transport"
                });
                Err(route.response_failure(federation_transport_unavailable()))
            }
            _failure => {
                // A MAC, replay, counter, authority or incarnation failure.
                // The fence below retires the whole incarnation instead.
                allocated.counter = None;
                self.channel
                    .fence_rpc_security_failure(&context, "rpcError", path, rpc_stage, rpc_status);
                #[cfg(test)]
                {
                    let reason = if let Ok(Err(error)) = &_failure {
                        format!(
                            "inner_error status={} error={}",
                            error.status, error.message
                        )
                    } else {
                        "unexpected".to_owned()
                    };
                    eprintln!(
                        "FEDERATION_RPC_FENCED peer={} path={path} elapsed_ms={} reason={reason}",
                        self.channel.snapshot.peer.host_id,
                        request_started_at.elapsed().as_millis()
                    );
                }
                Err(route.response_failure(federation_transport_unavailable()))
            }
        };
        // Every arm keeps the caller's original API error; only its class
        // differs. Nothing is sent before a counter is allocated: a refusal
        // there is local authority, and a deadline there is local capacity,
        // as is a counter freed only after the caller's deadline. A deadline
        // that passed before an allocation that did not wait was spent in the
        // local checks. The counter may already have been retired when the
        // local check after the reply refused; keep that outcome.
        let class: fn(ApiError) -> FederationRpcRequestError = if rpc_stage == "deadlineBeforeSend"
        {
            FederationRpcRequestError::BeforeSendCapacity
        } else if rpc_stage == "deadlineInLocalChecks" {
            FederationRpcRequestError::BeforeSendDeadline
        } else if !counter_allocated {
            if timed_out {
                FederationRpcRequestError::BeforeSendCapacity
            } else {
                FederationRpcRequestError::BeforeSendAuthority
            }
        } else if after_reply_authority_refused && rpc_stage != "retired" {
            FederationRpcRequestError::AfterReplyAuthority
        } else if timed_out || receiver_deadline {
            // The receiver's own unsigned 408 is its deadline for this read,
            // not an authenticated outcome; it is a timeout like the caller's.
            FederationRpcRequestError::TimedOut
        } else {
            FederationRpcRequestError::Other
        };
        result.map_err(class)
    }
}

impl FederationRpcRoute {
    /// The fixed local name of this request's operation. It never carries a
    /// path, query or payload, so a bounded record may repeat it.
    pub(crate) fn operation(&self) -> &'static str {
        match self {
            Self::Health(_) => "health",
            Self::PromptPolicy(_, _) => "promptPolicy",
            Self::HostUtilization(_) => "hostUtilization",
            Self::Onboarding(_, _) => "onboarding",
            Self::Mailbox(_, _) => "mailbox",
            Self::ExecutorWake(_, _) => "executorWake",
            Self::ExecutorOutcome(_, _) => "executorOutcome",
            Self::NativeBinding(_, _) => "nativeBinding",
            Self::Binding(_, _) => "binding",
            Self::Participation(_, _) => "participation",
            Self::MailboxRelay(_, _) => "mailboxRelay",
        }
    }
}

/// Holds one request's sender counter until it is settled or fenced. Dropped
/// with a counter still unsettled, as when the caller is cancelled, it
/// retires that counter as UNKNOWN; retiring a settled counter does nothing.
struct FederationCounterGuard<'a> {
    context: &'a FederationSecurityContext,
    clock: &'a FederationControlClock,
    counter: Option<u64>,
}

impl FederationCounterGuard<'_> {
    fn retire(&mut self, reason: &'static str) {
        if let Some(counter) = self.counter.take() {
            self.context
                .retire_unknown(counter, reason, self.clock.now());
        }
    }
}

impl Drop for FederationCounterGuard<'_> {
    fn drop(&mut self) {
        self.retire("dropped");
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
            .map(|entry| entry.retire_at)
            .min();
        if let Some(deadline) = deadline {
            tokio::select! {
                _ = &mut changed => {},
                _ = channel.control.clock.deadline(deadline) => {
                    federation_sender_window_tick(&context, channel.control.clock.now());
                },
            }
        } else {
            changed.await;
        }
    }
}

/// Retires each counter unsettled past the unchanged sender window as
/// UNKNOWN, a backstop to the request's own limit and drop guard. Nothing here
/// touches the link.
fn federation_sender_window_tick(context: &FederationSecurityContext, now: std::time::Instant) {
    let expired: Vec<u64> = context
        .sender
        .lock()
        .expect("sender window poisoned")
        .outstanding
        .iter()
        .filter(|(_, entry)| entry.retire_at <= now)
        .map(|(counter, _)| *counter)
        .collect();
    for counter in expired {
        context.retire_unknown(counter, "unsettled", now);
    }
}
