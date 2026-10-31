//! Observation must preserve frames, errors, metadata and lazy polling.
use super::*;
use http_body::{Body as _, Frame, SizeHint};
use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

struct Frames {
    frames: VecDeque<io::Result<Frame<axum::body::Bytes>>>,
    pending: bool,
}

impl http_body::Body for Frames {
    type Data = axum::body::Bytes;
    type Error = io::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<io::Result<Frame<Self::Data>>>> {
        let this = self.get_mut();
        if std::mem::take(&mut this.pending) {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        Poll::Ready(this.frames.pop_front())
    }
    fn is_end_stream(&self) -> bool {
        self.frames.is_empty()
    }
    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}

async fn frame<B: http_body::Body + Unpin>(
    body: &mut B,
) -> Option<Result<Frame<B::Data>, B::Error>> {
    std::future::poll_fn(|cx| Pin::new(&mut *body).poll_frame(cx)).await
}

fn observed(body: axum::body::Body, sse: bool) -> OutputSizeBody {
    OutputSizeBody {
        inner: body,
        metadata: OutputSizeObserved {
            producer: OutputProducer::Other,
            revision: Some(7),
        },
        sse,
        count: OutputByteCount::default(),
        finished: false,
    }
}

#[tokio::test]
async fn observation_preserves_data_pointer_trailers_pending_and_original_error() {
    let data = axum::body::Bytes::from_static(b"unchanged encoded bytes");
    let pointer = data.as_ptr();
    let mut trailers = axum::http::HeaderMap::new();
    trailers.insert("digest", HeaderValue::from_static("original"));
    let source = Frames {
        pending: true,
        frames: VecDeque::from([
            Ok(Frame::data(data)),
            Ok(Frame::trailers(trailers.clone())),
            Err(io::Error::other("original delivery error")),
        ]),
    };
    let mut body = observed(axum::body::Body::new(source), false);
    assert_eq!(body.size_hint().exact(), None);
    let first = frame(&mut body)
        .await
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    assert_eq!(
        first.as_ptr(),
        pointer,
        "observation must not clone the body buffer"
    );
    assert_eq!(first, "unchanged encoded bytes");
    assert_eq!(
        frame(&mut body)
            .await
            .unwrap()
            .unwrap()
            .into_trailers()
            .unwrap(),
        trailers
    );
    let error = frame(&mut body).await.unwrap().unwrap_err();
    assert_eq!(error.to_string(), "original delivery error");
    assert!(body.finished);
    assert_eq!(body.count.bytes, first.len() as u64);
    assert!(frame(&mut body).await.is_none());
}

#[tokio::test]
async fn output_above_both_reporting_bands_is_delivered_lazily_without_collection() {
    // One reusable 64KiB buffer, not a threshold-sized allocation. Repeated
    // frames deliver over 64MiB, each retaining the same allocation identity.
    struct Repeated {
        bytes: axum::body::Bytes,
        left: usize,
    }
    impl http_body::Body for Repeated {
        type Data = axum::body::Bytes;
        type Error = Infallible;
        fn poll_frame(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Infallible>>> {
            let this = self.get_mut();
            if this.left == 0 {
                return Poll::Ready(None);
            }
            this.left -= 1;
            Poll::Ready(Some(Ok(Frame::data(this.bytes.clone()))))
        }
        fn is_end_stream(&self) -> bool {
            self.left == 0
        }
        fn size_hint(&self) -> SizeHint {
            SizeHint::with_exact((self.left * self.bytes.len()) as u64)
        }
    }
    let bytes = axum::body::Bytes::from(vec![b'x'; 64 * 1024]);
    let pointer = bytes.as_ptr();
    let expected = (1025 * bytes.len()) as u64;
    let mut body = observed(axum::body::Body::new(Repeated { bytes, left: 1025 }), false);
    assert_eq!(body.count.bytes, 0, "construction must not poll or collect");
    assert_eq!(body.size_hint().exact(), Some(expected));
    let mut count = 0;
    while let Some(next) = frame(&mut body).await {
        let data = next.unwrap().into_data().unwrap();
        assert_eq!(data.as_ptr(), pointer);
        assert!(data.iter().all(|byte| *byte == b'x'));
        count += data.len() as u64;
    }
    assert_eq!(count, expected);
    assert_eq!(body.count.bytes, expected);
    assert!(body.finished);
    assert!(body.is_end_stream());
}

#[tokio::test]
async fn sse_frames_and_json_bytes_are_identical_with_observation() {
    fn sse() -> Response {
        let events = async_stream::stream! {
            yield Ok::<_, Infallible>(Event::default().event("state").data("é\nline two"));
            yield Ok(Event::default().event("delta").data("{\"value\":1}"));
        };
        Sse::new(events).into_response()
    }
    let plain = axum::body::to_bytes(sse().into_body(), 1024).await.unwrap();
    let mut response = observe_http_response(sse(), OutputProducer::State);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let marker = *response.extensions().get::<OutputSizeObserved>().unwrap();
    assert!(matches!(marker.producer, OutputProducer::State));
    assert_eq!(marker.revision, None);
    let first = frame(response.body_mut())
        .await
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    assert_eq!(first, "event: state\ndata: é\ndata: line two\n\n");
    let second = frame(response.body_mut())
        .await
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    assert_eq!([first.as_ref(), second.as_ref()].concat(), plain);
    assert!(frame(response.body_mut()).await.is_none());

    let json = json!({"escaped":"é\n\"", "empty": []});
    let expected = axum::body::to_bytes(Json(&json).into_response().into_body(), 1024)
        .await
        .unwrap();
    let decorated = (StatusCode::CREATED, [("etag", "original")], Json(&json)).into_response();
    let response = observe_http_response(decorated, OutputProducer::Other);
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.headers()["etag"], "original");
    assert_eq!(
        axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap(),
        expected
    );
}

#[test]
fn counters_saturate_and_fixed_slots_coalesce_without_payloads() {
    let mut count = OutputByteCount {
        bytes: u64::MAX - 3,
        saturated: false,
    };
    count.add(4);
    assert_eq!(count.bytes, u64::MAX);
    assert!(count.saturated);
    let slot = OutputSizeSlot::new();
    slot.observations.store(u64::MAX, Ordering::Relaxed);
    slot.observe(u64::MAX, true, Some(u64::MAX));
    assert_eq!(slot.observations.load(Ordering::Relaxed), u64::MAX);
    assert_eq!(slot.max_bytes.load(Ordering::Relaxed), u64::MAX);
    assert!(slot.saturated.load(Ordering::Relaxed));
    let slot = OutputSizeSlot::new();
    slot.observe(1024 * 1024 * 1024, false, Some(42));
    slot.observe(64 * 1024 * 1024, false, Some(41));
    let report = slot
        .take_report(
            OutputProducer::Session,
            OutputSizeStage::NativePreWrite,
            false,
            1024 * 1024 * 1024,
        )
        .unwrap();
    assert_eq!(report.observations_since_drain, 2);
    assert!(
        slot.take_report(
            OutputProducer::Session,
            OutputSizeStage::NativePreWrite,
            false,
            1024 * 1024 * 1024
        )
        .is_none()
    );
    assert!(!report.complete);
    assert_eq!(report.band_bytes, 1024 * 1024 * 1024);
    assert_eq!(report.max_revision_observed, Some(42));
    assert_eq!(report.max_bytes_observed, 1024 * 1024 * 1024);
    let wire = serde_json::to_string(&report).unwrap();
    assert!(wire.len() < 512);
    assert!(!wire.contains("payload"));
}

#[tokio::test]
async fn nested_observers_keep_headers_size_hints_and_measure_once() {
    use tower::ServiceExt;
    let app = Router::new()
        .route(
            "/api/state",
            get(|| async {
                (
                    axum::Extension(OutputSizeRevision(91)),
                    [("etag", "same")],
                    "original",
                )
            }),
        )
        .layer(axum::middleware::from_fn(output_size_http_observer))
        .layer(axum::middleware::from_fn(output_size_http_observer));
    let response = app
        .oneshot(
            axum::http::Request::get("/api/state")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let metadata = response.extensions().get::<OutputSizeObserved>().unwrap();
    assert_eq!(metadata.producer, OutputProducer::State);
    assert_eq!(metadata.revision, Some(91));
    assert_eq!(response.headers()["etag"], "same");
    assert_eq!(response.body().size_hint().exact(), Some(8));
    assert_eq!(
        axum::body::to_bytes(response.into_body(), 100)
            .await
            .unwrap(),
        "original"
    );
}

#[tokio::test]
async fn compression_and_signed_response_bytes_are_unchanged() {
    use tower::ServiceExt;
    fn app(observe: bool) -> Router {
        let mut app = Router::new().route(
            "/api/state",
            get(|| async {
                (
                    [
                        ("content-type", "application/json"),
                        ("x-fixture-mac", "original"),
                    ],
                    format!("{{\"value\":\"{}\"}}", "x".repeat(2048)),
                )
            }),
        );
        if observe {
            app = app.layer(axum::middleware::from_fn(output_size_http_observer));
        }
        app = app.layer(CompressionLayer::new());
        if observe {
            app = app.layer(axum::middleware::from_fn(output_size_http_observer));
        }
        app
    }
    let request = || {
        axum::http::Request::get("/api/state")
            .header("accept-encoding", "gzip")
            .body(axum::body::Body::empty())
            .unwrap()
    };
    let plain = app(false).oneshot(request()).await.unwrap();
    let observed = app(true).oneshot(request()).await.unwrap();
    assert_eq!(observed.headers(), plain.headers());
    assert_eq!(observed.headers()["content-encoding"], "gzip");
    assert_eq!(observed.headers()["x-fixture-mac"], "original");
    assert_eq!(
        observed
            .extensions()
            .get::<OutputSizeObserved>()
            .unwrap()
            .producer,
        OutputProducer::State
    );
    assert_eq!(
        axum::body::to_bytes(observed.into_body(), 4096)
            .await
            .unwrap(),
        axum::body::to_bytes(plain.into_body(), 4096).await.unwrap()
    );
}

#[tokio::test]
async fn failed_telemetry_journal_does_not_affect_delivery_or_panic_classification() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let root = TestTempRoot::create("output-observation");
    let blocker = root.join("not-a-directory");
    fs::write(&blocker, "fixture").unwrap();
    let lifecycle = HostLifecycle::open(blocker, "test".to_owned(), None);
    observe_output_size(
        OutputProducer::Other,
        OutputSizeStage::NativePreWrite,
        64 * 1024 * 1024,
        true,
        false,
        None,
    );
    lifecycle.record_output_sizes(); // existing journal error is discarded
    let response = observe_http_response("still delivered".into_response(), OutputProducer::Other);
    assert_eq!(
        axum::body::to_bytes(response.into_body(), 100)
            .await
            .unwrap(),
        "still delivered"
    );
    let boot = HostLifecycleRecord::Boot {
        at: "boot".into(),
        instance_id: "test".into(),
        pid: 1,
        binary_sha256: None,
    };
    let panic = HostLifecycleRecord::Panic {
        at: "panic".into(),
        instance_id: "test".into(),
        thread: "worker".into(),
        location: None,
        message: "fixed".into(),
    };
    let before = host_lifecycle_classify(&[boot.clone(), panic.clone()]);
    let size = HostLifecycleRecord::OutputSize {
        at: "size".into(),
        instance_id: "test".into(),
        report: OutputSizeReport {
            producer: OutputProducer::State,
            stage: OutputSizeStage::HttpBodyBeforeCompression,
            complete: true,
            band_bytes: 16 * 1024 * 1024,
            observations_since_drain: 1,
            max_bytes_observed: 16 * 1024 * 1024,
            max_revision_observed: None,
            counter_saturated: false,
        },
    };
    assert_eq!(host_lifecycle_classify(&[boot, panic, size]), before);
}
