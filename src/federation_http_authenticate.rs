// Owns MAC-first request admission and authenticated responses/refusals.
// Does not grant membership or select peers from payloads. Split from federation_listener.rs.

async fn federation_authenticate_message(
    state: FederationListenerState,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<Response, ApiError> {
    // One unchanged five-second request budget includes body admission,
    // dispatch and bounded response collection. Mailbox handler expiry is an
    // authenticated unknown outcome, not evidence of transport failure.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    #[cfg(test)]
    let deadline = if state.channel.control.clock.io_deadlines {
        tokio::time::Instant::from_std(state.channel.control.clock.now()) + Duration::from_secs(5)
    } else {
        deadline
    };
    let (mut parts, body) = request.into_parts();
    let mailbox_request =
        parts.method == axum::http::Method::POST && parts.uri.path() == "/federation/v1/mailbox";
    let relay_request = parts.method == axum::http::Method::POST && parts.uri.path() == MAILBOX_RELAY_RPC_ROUTE;
    let executor_request = parts.method == axum::http::Method::POST
        && parts.uri.path() == FEDERATION_EXECUTOR_WAKE_ROUTE;
    let binding_request = parts.method == axum::http::Method::POST
        && parts.uri.path() == "/federation/v1/mailbox/binding";
    let durable_request = mailbox_request || relay_request || binding_request || executor_request;
    let request_limit = if executor_request {
        32 * 1024
    } else if mailbox_request || relay_request {
        FEDERATION_MAILBOX_REQUEST_BYTES
    } else {
        4096
    };
    let response_limit = if executor_request {
        32 * 1024
    } else if mailbox_request || relay_request {
        FEDERATION_MAILBOX_RESPONSE_BYTES
    } else {
        FEDERATION_FRAME_LIMIT
    };
    federation_check_request_headers(&parts.headers)?;
    let context = state.channel.security_context()?;
    let direction = if state.channel.snapshot.local_role == FederationRole::Satellite {
        "c2s"
    } else {
        "s2c"
    };
    let key = if direction == "c2s" { 0 } else { 2 };
    let (counter, tag) = federation_parse_mac(&parts.headers, &context, direction)?;
    state.channel.control.observe("requestBody", counter);
    let body =
        match state.channel.control.clock.timeout_at(deadline, axum::body::to_bytes(body, request_limit)).await {
            Ok(body) => body.map_err(|_| federation_auth_refused())?,
            Err(_) => return Ok(federation_request_timeout()),
        };
    let target = parts
        .uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or(parts.uri.path());
    let representation = federation_http_representation(
        &context,
        counter,
        direction,
        parts.method.as_str(),
        target,
        &parts.headers,
        &body,
    )?;
    federation_verify_mac(&context.keys[key], &[&representation], &tag)?;
    let mailbox_intent = if mailbox_request {
        serde_json::from_slice::<FederationMailboxRequest>(&body).ok()
    } else { None };
    let reserved = {
        let link = state
            .channel
            .state
            .lock()
            .expect("federation link mutex poisoned");
        if link
            .security
            .as_ref()
            .is_none_or(|active| !Arc::ptr_eq(active, &context))
            || link.recorded_incarnation.as_deref() != Some(context.incarnation.as_str())
        {
            return Err(federation_auth_refused());
        }
        context
            .received
            .lock()
            .expect("replay window poisoned")
            .reserve(counter)
    };
    let authority = || -> Result<FederationLinkLease, ApiError> {
        if let Err(error) = reserved {
            // An earlier delivery of this authenticated counter may have
            // committed and lost its reply. This transport refusal settles
            // the counter, while the application key still needs recovery.
            return Err(if mailbox_request || relay_request {
                federation_mailbox_unknown_outcome(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "authenticated mailbox transport counter refused",
                )
            } else if binding_request {
                federation_binding_unknown_outcome()
            } else {
                error
            });
        }
        if parts
            .headers
            .get(FEDERATION_INCARNATION_HEADER)
            .and_then(|value| value.to_str().ok())
            != Some(context.incarnation.as_str())
        {
            return Err(federation_auth_refused());
        }
        let header = parts
            .headers
            .get(FEDERATION_AUTHORITY_HEADER)
            .ok_or_else(federation_auth_refused)?;
        let authority: FederationRequestAuthority =
            serde_json::from_slice(header.as_bytes()).map_err(|_| federation_auth_refused())?;
        authority.ensure_expected(&state.channel.snapshot)?;
        (state.ensure_current)()?;
        state.channel.lease(&context.incarnation)
    };
    let response = match authority() {
        Err(error) => error.into_response(),
        Ok(lease) => match state.channel.bound.try_acquire() {
            Err(error) => error.into_response(),
            Ok(permit) => {
                let permit = Arc::new(permit);
                parts.extensions.insert(FederationAuthenticatedPeer {
                    lease: Some(lease.clone()),
                    mailbox_permit: durable_request.then(|| permit.clone()),
                });
                let request =
                    axum::extract::Request::from_parts(parts, axum::body::Body::from(body));
                let response = match state.channel.control.clock.timeout_at(deadline, next.run(request)).await {
                    Ok(response) => response,
                    Err(_) if binding_request => {
                        federation_binding_unknown_outcome().into_response()
                    }
                    Err(_) if relay_request => federation_mailbox_unknown_outcome(
                        StatusCode::SERVICE_UNAVAILABLE,"native mailbox relay handler deadline").into_response(),
                    Err(_) if mailbox_request => federation_mailbox_unknown_response(
                        mailbox_intent.as_ref(), "handler_deadline"),
                    Err(_) => {
                        state
                            .channel
                            .fence_security(context.owner, &context.incarnation);
                        return Ok(federation_request_timeout());
                    }
                };
                match (state.ensure_current)().and_then(|_| lease.with_incarnation(|| Ok(()))) {
                    Ok(()) => response,
                    Err(_) if binding_request => {
                        federation_binding_unknown_outcome().into_response()
                    }
                    Err(_) if relay_request => federation_mailbox_unknown_outcome(
                        StatusCode::SERVICE_UNAVAILABLE,"native mailbox relay authority changed").into_response(),
                    Err(_) if mailbox_request => federation_mailbox_unknown_response(
                        mailbox_intent.as_ref(), "authority_changed"),
                    Err(error) => error.into_response(),
                }
            }
        },
    };
    let (mut parts, body) = response.into_parts();
    let body =
        match state.channel.control.clock.timeout_at(deadline, axum::body::to_bytes(body, response_limit)).await {
            Ok(body) => body.map_err(|_| federation_transport_unavailable())?,
            Err(_) => return Ok(federation_request_timeout()),
        };
    let response = federation_response_representation(
        &context,
        counter,
        direction,
        &representation,
        parts.status,
        &parts.headers,
        &body,
    )?;
    let tag = federation_mac(&context.keys[key + 1], &[&response])?;
    parts.headers.insert(
        FEDERATION_MAC_HEADER,
        federation_mac_header(&context, direction, counter, &tag)?,
    );
    Ok(Response::from_parts(parts, axum::body::Body::from(body)))
}

fn federation_request_timeout() -> Response {
    (StatusCode::REQUEST_TIMEOUT, [("connection", "close")]).into_response()
}
