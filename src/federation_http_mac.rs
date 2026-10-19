// Owns exact request/response representations and strict MAC-header parsing.
// Does not dispatch handlers or mutate replay windows. New A1 HTTP primitive leaf.

const FEDERATION_REQUEST_FIELDS: &[&str] = &[
    "content-encoding",
    "content-type",
    "x-termal-idempotency-key",
    "x-termal-link-incarnation",
    "x-termal-peer-authority",
    "x-termal-qualified-sender",
    "x-termal-state-stamp",
    "x-termal-topic",
];
const FEDERATION_RESPONSE_FIELDS: &[&str] = &[
    "content-type",
    "x-termal-operation-status",
    "x-termal-receipt",
];

fn federation_header_representation(
    headers: &axum::http::HeaderMap,
    fields: &[&str],
) -> Result<Vec<u8>, ApiError> {
    let mut values = Vec::<&[u8]>::new();
    for name in fields {
        let mut supplied = headers.get_all(*name).iter();
        if let Some(value) = supplied.next() {
            if supplied.next().is_some() || value.as_bytes().len() > 1024 {
                return Err(federation_auth_refused());
            }
            values.push(name.as_bytes());
            values.push(value.as_bytes());
        }
    }
    Ok(federation_encode(b"termal-headers-v1", &values))
}

fn federation_check_request_headers(headers: &axum::http::HeaderMap) -> Result<(), ApiError> {
    for name in headers.keys() {
        let name = name.as_str();
        if !FEDERATION_REQUEST_FIELDS.contains(&name)
            && ![
                FEDERATION_MAC_HEADER,
                "host",
                "content-length",
                "connection",
                "accept",
                "accept-encoding",
                "user-agent",
            ]
            .contains(&name)
        {
            return Err(federation_auth_refused());
        }
        if headers.get_all(name).iter().count() != 1 {
            return Err(federation_auth_refused());
        }
    }
    Ok(())
}

fn federation_http_representation(
    context: &FederationSecurityContext,
    counter: u64,
    direction: &str,
    method: &str,
    target: &str,
    headers: &axum::http::HeaderMap,
    body: &[u8],
) -> Result<Vec<u8>, ApiError> {
    if target.len() > 4096 || method.len() > 32 {
        return Err(federation_auth_refused());
    }
    let incarnation =
        Uuid::parse_str(&context.incarnation).map_err(|_| federation_auth_refused())?;
    let header_bytes = federation_header_representation(headers, FEDERATION_REQUEST_FIELDS)?;
    Ok(federation_encode(
        b"termal-request-v1",
        &[
            incarnation.as_bytes(),
            direction.as_bytes(),
            &counter.to_be_bytes(),
            method.as_bytes(),
            target.as_bytes(),
            &Sha256::digest(body),
            &header_bytes,
        ],
    ))
}

fn federation_response_representation(
    context: &FederationSecurityContext,
    counter: u64,
    direction: &str,
    request: &[u8],
    status: StatusCode,
    headers: &axum::http::HeaderMap,
    body: &[u8],
) -> Result<Vec<u8>, ApiError> {
    let incarnation =
        Uuid::parse_str(&context.incarnation).map_err(|_| federation_auth_refused())?;
    let header_bytes = federation_header_representation(headers, FEDERATION_RESPONSE_FIELDS)?;
    Ok(federation_encode(
        b"termal-response-v1",
        &[
            incarnation.as_bytes(),
            direction.as_bytes(),
            &counter.to_be_bytes(),
            &Sha256::digest(request),
            &status.as_u16().to_be_bytes(),
            &Sha256::digest(body),
            &header_bytes,
        ],
    ))
}

fn federation_parse_mac(
    headers: &axum::http::HeaderMap,
    context: &FederationSecurityContext,
    direction: &str,
) -> Result<(u64, Vec<u8>), ApiError> {
    use base64::Engine;
    let mut supplied = headers.get_all(FEDERATION_MAC_HEADER).iter();
    let value = supplied
        .next()
        .and_then(|header| header.to_str().ok())
        .filter(|value| value.len() <= 256)
        .ok_or_else(federation_auth_refused)?;
    if supplied.next().is_some() {
        return Err(federation_auth_refused());
    }
    let fields: Vec<_> = value.split(':').collect();
    if fields.len() != 5
        || fields[0] != "v1"
        || fields[1] != context.incarnation
        || fields[2] != direction
        || fields[3].is_empty()
        || !fields[3].bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(federation_auth_refused());
    }
    let counter: u64 = fields[3].parse().map_err(|_| federation_auth_refused())?;
    if counter == 0 || fields[3] != counter.to_string() {
        return Err(federation_auth_refused());
    }
    let tag = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(fields[4])
        .map_err(|_| federation_auth_refused())?;
    if tag.len() != 32 || base64::engine::general_purpose::STANDARD_NO_PAD.encode(&tag) != fields[4]
    {
        return Err(federation_auth_refused());
    }
    Ok((counter, tag))
}

fn federation_mac_header(
    context: &FederationSecurityContext,
    direction: &str,
    counter: u64,
    tag: &[u8],
) -> Result<HeaderValue, ApiError> {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD_NO_PAD.encode(tag);
    let mut value = HeaderValue::from_str(&format!(
        "v1:{}:{direction}:{counter}:{encoded}",
        context.incarnation
    ))
    .map_err(|_| federation_auth_refused())?;
    value.set_sensitive(true);
    Ok(value)
}
