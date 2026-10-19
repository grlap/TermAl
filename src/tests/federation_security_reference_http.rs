// Owns independent canonical HTTP representations for the adversarial peer.
// Does not invoke product signing or routing. Split from the security reference.

use super::*;

pub(super) const REQUEST_FIELDS: &[&str] = &[
    "content-encoding",
    "content-type",
    "x-termal-idempotency-key",
    "x-termal-link-incarnation",
    "x-termal-peer-authority",
    "x-termal-qualified-sender",
    "x-termal-state-stamp",
    "x-termal-topic",
];
pub(super) const RESPONSE_FIELDS: &[&str] = &[
    "content-type",
    "x-termal-operation-status",
    "x-termal-receipt",
];

fn headers_repr(headers: &axum::http::HeaderMap, names: &[&str]) -> Vec<u8> {
    let mut fields = Vec::new();
    for name in names {
        if let Some(value) = headers.get(*name) {
            fields.push(name.as_bytes().to_vec());
            fields.push(value.as_bytes().to_vec());
        }
    }
    encode(b"termal-headers-v1", &fields)
}

pub(super) fn request_repr(
    session: &ReferenceSession,
    counter: u64,
    direction: &str,
    method: &str,
    target: &str,
    headers: &axum::http::HeaderMap,
    body: &[u8],
) -> Vec<u8> {
    encode(
        b"termal-request-v1",
        &[
            Uuid::parse_str(&session.incarnation)
                .unwrap()
                .as_bytes()
                .to_vec(),
            direction.as_bytes().to_vec(),
            counter.to_be_bytes().to_vec(),
            method.as_bytes().to_vec(),
            target.as_bytes().to_vec(),
            digest(body),
            headers_repr(headers, REQUEST_FIELDS),
        ],
    )
}

pub(super) fn response_repr(
    session: &ReferenceSession,
    counter: u64,
    direction: &str,
    request: &[u8],
    status: StatusCode,
    headers: &axum::http::HeaderMap,
    body: &[u8],
) -> Vec<u8> {
    encode(
        b"termal-response-v1",
        &[
            Uuid::parse_str(&session.incarnation)
                .unwrap()
                .as_bytes()
                .to_vec(),
            direction.as_bytes().to_vec(),
            counter.to_be_bytes().to_vec(),
            digest(request),
            status.as_u16().to_be_bytes().to_vec(),
            digest(body),
            headers_repr(headers, RESPONSE_FIELDS),
        ],
    )
}

pub(super) fn authentication(
    session: &ReferenceSession,
    counter: u64,
    direction: &str,
    tag: &[u8],
) -> String {
    format!(
        "v1:{}:{direction}:{counter}:{}",
        session.incarnation,
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(tag)
    )
}

pub(super) fn sign_request(
    session: &ReferenceSession,
    counter: u64,
    method: &str,
    target: &str,
    mut headers: axum::http::HeaderMap,
    body: &[u8],
) -> (axum::http::HeaderMap, Vec<u8>) {
    let representation = request_repr(session, counter, "c2s", method, target, &headers, body);
    let value = authentication(
        session,
        counter,
        "c2s",
        &mac(&session.keys[0], &representation),
    );
    headers.insert(AUTH_HEADER, HeaderValue::from_str(&value).unwrap());
    (headers, representation)
}

pub(super) struct WireResponse {
    pub status: StatusCode,
    pub headers: axum::http::HeaderMap,
    pub body: Vec<u8>,
}

impl WireResponse {
    pub fn verified(&self, session: &ReferenceSession, counter: u64, request: &[u8]) -> bool {
        let mut values = self.headers.get_all(AUTH_HEADER).iter();
        let Some(value) = values.next().and_then(|value| value.to_str().ok()) else {
            return false;
        };
        if values.next().is_some() {
            return false;
        }
        let prefix = format!("v1:{}:c2s:{counter}:", session.incarnation);
        let Some(tag) = value.strip_prefix(&prefix).and_then(|tag| {
            base64::engine::general_purpose::STANDARD_NO_PAD
                .decode(tag)
                .ok()
        }) else {
            return false;
        };
        let representation = response_repr(
            session,
            counter,
            "c2s",
            request,
            self.status,
            &self.headers,
            &self.body,
        );
        verify(&session.keys[1], &representation, &tag).is_ok()
    }
}

pub(super) async fn wire(
    port: u16,
    method: &str,
    target: &str,
    headers: axum::http::HeaderMap,
    body: &[u8],
) -> WireResponse {
    let response = reqwest::Client::builder()
        .no_proxy()
        .no_gzip()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
        .request(
            method.parse::<reqwest::Method>().unwrap(),
            format!("http://127.0.0.1:{port}{target}"),
        )
        .headers(headers)
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.bytes().await.unwrap().to_vec();
    assert!(body.len() <= 65536, "reference response bound");
    WireResponse {
        status,
        headers,
        body,
    }
}
