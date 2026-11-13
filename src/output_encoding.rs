// Encoded HTTP/SSE output has one safety budget. Domain data and transcript
// retention remain owned by their existing producers.
const MAX_ENCODED_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

#[cfg(test)]
thread_local! {
    static TEST_OUTPUT_BUDGET: std::cell::Cell<usize> = const { std::cell::Cell::new(MAX_ENCODED_OUTPUT_BYTES) };
}

fn output_budget_bytes() -> usize {
    #[cfg(test)]
    {
        TEST_OUTPUT_BUDGET.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        MAX_ENCODED_OUTPUT_BYTES
    }
}

#[cfg(test)]
struct TestOutputBudget(usize);
#[cfg(test)]
impl TestOutputBudget {
    fn new(limit: usize) -> Self {
        Self(TEST_OUTPUT_BUDGET.with(|value| value.replace(limit)))
    }
}
#[cfg(test)]
impl Drop for TestOutputBudget {
    fn drop(&mut self) {
        TEST_OUTPUT_BUDGET.with(|value| value.set(self.0));
    }
}

#[derive(Debug)]
enum OutputEncodingError {
    Limit {
        limit: usize,
        attempted_at_least: usize,
    },
    Serialize(serde_json::Error),
}

impl std::fmt::Display for OutputEncodingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Limit { .. } => f.write_str("encoded output limit exceeded"),
            Self::Serialize(error) => std::fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for OutputEncodingError {}

struct BoundedOutput {
    bytes: Vec<u8>,
    limit: usize,
    overflow: Option<usize>,
}

impl BoundedOutput {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            overflow: None,
        }
    }
}

impl Write for BoundedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self.bytes.len().checked_add(bytes.len());
        if next.is_none_or(|next| next > self.limit) {
            self.overflow = Some(next.unwrap_or(usize::MAX));
            return Err(io::Error::other("encoded output limit exceeded"));
        }
        let next = next.expect("checked above");
        if next > self.bytes.capacity() {
            let capacity = self
                .bytes
                .capacity()
                .saturating_mul(2)
                .max(1024)
                .max(next)
                .min(self.limit);
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(|_| io::Error::other("encoded output allocation failed"))?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_json_bounded<T: Serialize + ?Sized>(
    value: &T,
    limit: usize,
) -> Result<Vec<u8>, OutputEncodingError> {
    let mut writer = BoundedOutput::new(limit);
    let result = serde_json::to_writer(&mut writer, value);
    if let Some(attempted_at_least) = writer.overflow {
        return Err(OutputEncodingError::Limit {
            limit,
            attempted_at_least,
        });
    }
    result.map_err(OutputEncodingError::Serialize)?;
    Ok(writer.bytes)
}

fn encode_sse_json<T: Serialize + ?Sized>(
    event: &'static str,
    value: &T,
) -> Result<String, OutputEncodingError> {
    encode_sse_json_with_limit(event, value, output_budget_bytes())
}

fn encode_sse_json_with_limit<T: Serialize + ?Sized>(
    event: &'static str,
    value: &T,
    limit: usize,
) -> Result<String, OutputEncodingError> {
    // Compact JSON has no literal CR/LF. Axum emits "event: NAME\ndata: JSON\n\n".
    let overhead =
        "event: \ndata: \n\n"
            .len()
            .checked_add(event.len())
            .ok_or(OutputEncodingError::Limit {
                limit,
                attempted_at_least: usize::MAX,
            })?;
    let available = limit
        .checked_sub(overhead)
        .ok_or(OutputEncodingError::Limit {
            limit,
            attempted_at_least: overhead,
        })?;
    let bytes = encode_json_bounded(value, available).map_err(|error| match error {
        OutputEncodingError::Limit {
            attempted_at_least, ..
        } => OutputEncodingError::Limit {
            limit,
            attempted_at_least: attempted_at_least.saturating_add(overhead),
        },
        error => error,
    })?;
    Ok(String::from_utf8(bytes).expect("JSON is UTF-8"))
}

fn output_encoding_api_error(error: OutputEncodingError, producer: &'static str) -> ApiError {
    output_encoding_api_error_at(error, producer, None, None)
}

fn output_encoding_api_error_at(
    error: OutputEncodingError,
    producer: &'static str,
    revision: Option<u64>,
    session_id: Option<&str>,
) -> ApiError {
    match error {
        error @ OutputEncodingError::Limit { limit, .. } => {
            observe_output_limit_at(producer, &error, revision, session_id);
            ApiError::internal("The response exceeded the output limit.").with_kind(
                ApiErrorKind::OutputLimitExceeded {
                    limit,
                    outcome: ResponseMutationOutcome::Unknown,
                },
            )
        }
        OutputEncodingError::Serialize(_) => ApiError::internal("Failed to encode response."),
    }
}

fn output_limit_sse_event() -> Event {
    Event::default().event("outputLimit").data(format!(
        "{{\"code\":\"output_limit_exceeded\",\"limitBytes\":{},\"phase\":\"response_encoding\"}}",
        output_budget_bytes()
    ))
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ResponseMutationOutcome {
    Committed,
    Accepted,
    Unknown,
    NotApplicable,
}

#[derive(Clone, Copy, Debug)]
struct OutputLimitResponse {
    limit: usize,
    outcome: ResponseMutationOutcome,
}

impl OutputLimitResponse {
    fn envelope(self) -> Value {
        let message = match self.outcome {
            ResponseMutationOutcome::Committed => {
                "The operation completed, but its response exceeded the output limit. Do not repeat the operation; refresh its status."
            }
            ResponseMutationOutcome::Accepted => {
                "The operation was accepted, but its response exceeded the output limit. Do not repeat the operation; check its status."
            }
            ResponseMutationOutcome::Unknown => {
                "The response exceeded the output limit. The operation may have taken effect; check its status before retrying."
            }
            ResponseMutationOutcome::NotApplicable => {
                "The response exceeded the output limit. Request a smaller result."
            }
        };
        // The fixed error envelope has its own small budget, independent of a
        // test's injected data budget. It never serializes the rejected value.
        json!({
            "error": message, "code": "output_limit_exceeded", "limitBytes": self.limit,
            "phase": "response_encoding", "mutationOutcome": self.outcome,
        })
    }

    fn response(self) -> Response {
        let body = encode_json_bounded(&self.envelope(), 2048).expect("fixed output error fits");
        let mut response = (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response();
        response.extensions_mut().insert(self);
        response
    }
}

fn output_limit_details(value: &Value) -> Option<OutputLimitResponse> {
    if value.get("code").and_then(Value::as_str) != Some("output_limit_exceeded") {
        return None;
    }
    let outcome = value
        .get("mutationOutcome")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or(ResponseMutationOutcome::Unknown);
    let limit = value
        .get("limitBytes")
        .and_then(Value::as_u64)
        .and_then(|limit| usize::try_from(limit).ok())
        .filter(|limit| *limit <= MAX_ENCODED_OUTPUT_BYTES)
        .unwrap_or(MAX_ENCODED_OUTPUT_BYTES);
    Some(OutputLimitResponse { limit, outcome })
}

fn observe_output_limit(
    producer: &'static str,
    error: &OutputEncodingError,
    revision: Option<u64>,
) {
    observe_output_limit_at(producer, error, revision, None);
}

fn observe_output_limit_at(
    producer: &'static str,
    error: &OutputEncodingError,
    revision: Option<u64>,
    session_id: Option<&str>,
) {
    // One bounded counter, rather than a map keyed by unbounded identities.
    // Log the first and power-of-two occurrences to coalesce repeated failures.
    static OVERFLOWS: AtomicU64 = AtomicU64::new(0);
    let count = OVERFLOWS
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            Some(value.saturating_add(1))
        })
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    if count.is_power_of_two() {
        if let OutputEncodingError::Limit {
            limit,
            attempted_at_least,
        } = error
        {
            let session: String = session_id
                .unwrap_or("")
                .chars()
                .filter(|ch| !ch.is_control())
                .take(64)
                .collect();
            eprintln!(
                "output limit> producer={producer} limitBytes={limit} attemptedAtLeastBytes={attempted_at_least} revision={revision:?} session={session} occurrences={count}"
            );
        }
    }
}

fn json_output_response<T: Serialize + ?Sized>(
    value: &T,
    limit: usize,
    outcome: ResponseMutationOutcome,
) -> Response {
    match encode_json_bounded(value, limit) {
        Ok(body) => (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response(),
        Err(error @ OutputEncodingError::Limit { .. }) => {
            observe_output_limit("http_json", &error, None);
            OutputLimitResponse { limit, outcome }.response()
        }
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to encode response.",
        )
            .into_response(),
    }
}

// Request extraction is delegated unchanged to Axum. Only response encoding
// differs, so existing Json(...) handler and extractor contracts stay intact.
#[derive(Debug, Clone, Copy, Default)]
struct Json<T>(T);

impl<T, S> axum::extract::FromRequest<S> for Json<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = JsonRejection;
    async fn from_request(
        request: axum::extract::Request,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let axum::Json(value) =
            <axum::Json<T> as axum::extract::FromRequest<S>>::from_request(request, state).await?;
        Ok(Self(value))
    }
}

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        json_output_response(
            &self.0,
            output_budget_bytes(),
            ResponseMutationOutcome::Unknown,
        )
    }
}

fn restore_output_failure(response: Response) -> Response {
    // The signing owner already normalized the failure. Rewriting its body
    // afterwards would invalidate the authenticated response.
    if response.headers().contains_key(FEDERATION_MAC_HEADER) {
        return response;
    }
    match response.extensions().get::<OutputLimitResponse>().copied() {
        Some(failure) => failure.response(),
        None => response,
    }
}

async fn bound_finite_http_response(response: Response, read: bool) -> Response {
    bound_finite_http_response_with_limit(response, read, MAX_ENCODED_OUTPUT_BYTES).await
}

async fn bound_finite_http_response_with_limit(
    response: Response,
    read: bool,
    limit: usize,
) -> Response {
    let response = restore_output_failure(response);
    // SSE is unbounded in lifetime, but every producer bounds each frame.
    if response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .is_some_and(|value| value.as_bytes().starts_with(b"text/event-stream"))
    {
        return response;
    }
    if response.extensions().get::<OutputLimitResponse>().is_some() {
        return response;
    }
    // Most responses already own one bounded Bytes/Vec. Preserve that buffer
    // rather than collecting and copying it a second time. Do not trust a
    // caller-supplied Content-Length header in place of the body's own hint.
    use axum::body::HttpBody as _;
    if let Some(length) = response.body().size_hint().exact() {
        if length <= limit as u64 {
            return response;
        }
        let attempted_at_least = usize::try_from(length).unwrap_or(usize::MAX);
        observe_output_limit(
            "http_finite_body",
            &OutputEncodingError::Limit {
                limit,
                attempted_at_least,
            },
            None,
        );
        return OutputLimitResponse {
            limit,
            outcome: if read {
                ResponseMutationOutcome::NotApplicable
            } else {
                ResponseMutationOutcome::Unknown
            },
        }
        .response();
    }
    let (parts, body) = response.into_parts();
    let mut data = Box::pin(body.into_data_stream());
    let mut output = BoundedOutput::new(limit);
    while let Some(chunk) =
        std::future::poll_fn(|cx| futures_core::Stream::poll_next(data.as_mut(), cx)).await
    {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(_) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to read response body.",
                )
                    .into_response();
            }
        };
        if output.write_all(&chunk).is_err() {
            if let Some(attempted_at_least) = output.overflow {
                observe_output_limit(
                    "http_finite_body",
                    &OutputEncodingError::Limit {
                        limit,
                        attempted_at_least,
                    },
                    None,
                );
                return OutputLimitResponse {
                    limit,
                    outcome: if read {
                        ResponseMutationOutcome::NotApplicable
                    } else {
                        ResponseMutationOutcome::Unknown
                    },
                }
                .response();
            }
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to prepare response.",
            )
                .into_response();
        }
    }
    Response::from_parts(parts, axum::body::Body::from(output.bytes))
}

async fn output_limit_http_guard(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let read = request.method().is_safe();
    let mut response = next.run(request).await;
    if read
        && !response.headers().contains_key(FEDERATION_MAC_HEADER)
        && let Some(failure) = response.extensions_mut().get_mut::<OutputLimitResponse>()
    {
        failure.outcome = ResponseMutationOutcome::NotApplicable;
    }
    bound_finite_http_response(response, read).await
}

#[cfg(test)]
mod output_encoding_tests {
    use super::*;
    use serde::ser::{SerializeSeq, Serializer};
    use tower::ServiceExt;

    #[test]
    fn encoded_output_counts_escaping_utf8_and_rejects_before_append() {
        let value = "é\n\"";
        let expected = serde_json::to_vec(value).unwrap();
        assert_eq!(
            encode_json_bounded(value, expected.len()).unwrap(),
            expected
        );
        assert!(matches!(
            encode_json_bounded(value, expected.len() - 1),
            Err(OutputEncodingError::Limit { .. })
        ));
        let mut writer = BoundedOutput::new(4);
        writer.write_all(b"abcd").unwrap();
        assert!(writer.write_all(b"e").is_err());
        assert_eq!(writer.bytes, b"abcd");
        assert!(writer.bytes.capacity() <= 4);
    }

    #[test]
    fn logically_multi_gib_json_stops_after_a_small_budget() {
        struct Huge<'a>(&'a std::cell::Cell<usize>);
        impl Serialize for Huge<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut seq = serializer.serialize_seq(None)?;
                for _ in 0..4_294_967_296_u64 {
                    self.0.set(self.0.get() + 1);
                    seq.serialize_element("a lazy element, never a huge allocation")?;
                }
                seq.end()
            }
        }
        let visits = std::cell::Cell::new(0);
        assert!(matches!(
            encode_json_bounded(&Huge(&visits), 128),
            Err(OutputEncodingError::Limit { limit: 128, .. })
        ));
        assert!(visits.get() <= 4);
    }

    #[test]
    fn serializer_failure_is_distinct_from_output_limit() {
        struct Broken;
        impl Serialize for Broken {
            fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("deliberate serializer failure"))
            }
        }
        assert!(matches!(
            encode_json_bounded(&Broken, 128),
            Err(OutputEncodingError::Serialize(_))
        ));
    }

    #[tokio::test]
    async fn sse_budget_includes_the_complete_axum_frame() {
        let value = "é\n\"";
        let json = serde_json::to_vec(value).unwrap();
        let frame_len = json.len() + "event: state\ndata: \n\n".len();
        let payload = encode_sse_json_with_limit("state", value, frame_len).unwrap();
        let stream = async_stream::stream! { yield Ok::<_, Infallible>(Event::default().event("state").data(payload)); };
        let response = Sse::new(stream).into_response();
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(body.len(), frame_len);
        assert!(matches!(
            encode_sse_json_with_limit("state", value, frame_len - 1),
            Err(OutputEncodingError::Limit { .. })
        ));
    }

    #[tokio::test]
    async fn committed_mutation_is_not_replayed_and_decorators_cannot_report_success() {
        let effects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handler_effects = effects.clone();
        let app = Router::new()
            .route(
                "/mutate",
                post(move || {
                    let effects = handler_effects.clone();
                    async move {
                        effects.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::CREATED,
                            [("content-length", "999"), ("etag", "obsolete")],
                            json_output_response(
                                &"oversized mutation response",
                                8,
                                ResponseMutationOutcome::Committed,
                            ),
                        )
                    }
                }),
            )
            .layer(axum::middleware::from_fn(output_limit_http_guard));
        let response = app
            .oneshot(
                axum::http::Request::post("/mutate")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(response.headers().get("etag").is_none());
        assert_ne!(
            response
                .headers()
                .get("content-length")
                .and_then(|value| value.to_str().ok()),
            Some("999")
        );
        let body = axum::body::to_bytes(response.into_body(), 2048)
            .await
            .unwrap();
        let error: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(error["code"], "output_limit_exceeded");
        assert_eq!(error["phase"], "response_encoding");
        assert_eq!(error["mutationOutcome"], "committed");
        assert_eq!(error["limitBytes"], 8);
    }

    #[tokio::test]
    async fn generic_json_and_raw_finite_responses_reject_without_partial_success() {
        let _budget = TestOutputBudget::new(16);
        let response = restore_output_failure(
            (StatusCode::OK, Json("a response larger than sixteen bytes")).into_response(),
        );
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let response =
            bound_finite_http_response_with_limit("abcdef".into_response(), true, 5).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(response.into_body(), 2048)
            .await
            .unwrap();
        let error: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(error["code"], "output_limit_exceeded");
        assert_eq!(error["mutationOutcome"], "not_applicable");
        let response =
            bound_finite_http_response_with_limit("abcde".into_response(), true, 5).await;
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 16)
                .await
                .unwrap()
                .as_ref(),
            b"abcde"
        );
    }
}
