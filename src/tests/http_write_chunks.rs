use super::*;
use axum::body::Bytes;
use http_body::{Body as _, Frame};
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

const LIMIT: std::num::NonZeroUsize = std::num::NonZeroUsize::new(4).unwrap();

fn poll_frame(body: &mut HttpWriteChunks) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
    Pin::new(body).poll_frame(&mut Context::from_waker(Waker::noop()))
}

fn data(body: &mut HttpWriteChunks) -> Bytes {
    match poll_frame(body) {
        Poll::Ready(Some(Ok(frame))) => frame.into_data().expect("data frame"),
        other => panic!("expected data, got {other:?}"),
    }
}

#[test]
fn slices_are_zero_copy_complete_and_keep_the_exact_length_hint() {
    for bytes in [b"".as_slice(), b"abcd", b"abcde", b"abcdefghijklm"] {
        let original = Bytes::copy_from_slice(bytes);
        let mut body = HttpWriteChunks::new(Body::from(original.clone()), LIMIT);
        let mut received = Vec::new();
        while !body.is_end_stream() {
            assert_eq!(
                body.size_hint().exact(),
                Some((bytes.len() - received.len()) as u64)
            );
            let chunk = data(&mut body);
            assert!(chunk.len() <= LIMIT.get());
            assert_eq!(
                chunk.as_ptr(),
                original.as_ptr().wrapping_add(received.len())
            );
            received.extend_from_slice(&chunk);
        }
        assert_eq!(received, bytes);
        assert_eq!(body.size_hint().exact(), Some(0));
        assert!(matches!(poll_frame(&mut body), Poll::Ready(None)));
    }
}

thread_local! {
    static NATIVE_WRITE_REPORTS: std::cell::RefCell<Vec<NativeWriteSize>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn record_native_write(size: NativeWriteSize) {
    NATIVE_WRITE_REPORTS.with(|reports| reports.borrow_mut().push(size));
}

#[test]
fn prewrite_observes_original_bytes_once_at_the_band_and_keeps_delivery_without_a_sink() {
    for bytes in [b"abc".as_slice(), b"abcd", b"abcde", b"abcdefghijk"] {
        for report in [Some(record_native_write as fn(NativeWriteSize)), None] {
            NATIVE_WRITE_REPORTS.with(|reports| reports.borrow_mut().clear());
            let mut body = HttpWriteChunks::new(Body::from(bytes.to_vec()), LIMIT);
            body.observation = NativeWriteObservation::new(
                OutputSizeObserved {
                    producer: OutputProducer::Session,
                    revision: Some(23),
                },
                4,
                report,
            );
            let first = data(&mut body);
            NATIVE_WRITE_REPORTS.with(|reports| {
                let reports = reports.borrow();
                assert_eq!(
                    reports.len(),
                    usize::from(bytes.len() >= 4 && report.is_some())
                );
                if let Some(size) = reports.first() {
                    assert_eq!(
                        size.bytes,
                        bytes.len() as u64,
                        "before any suffix is published"
                    );
                    assert!(size.complete && !size.saturated);
                    assert_eq!(size.metadata.producer, OutputProducer::Session);
                    assert_eq!(size.metadata.revision, Some(23));
                }
            });
            let mut received = first.to_vec();
            while !body.is_end_stream() {
                received.extend_from_slice(&data(&mut body));
            }
            assert_eq!(received, bytes);
            NATIVE_WRITE_REPORTS.with(|reports| {
                assert_eq!(
                    reports.borrow().len(),
                    usize::from(bytes.len() >= 4 && report.is_some()),
                    "never emit per split chunk"
                )
            });
        }
    }

    // A known final length must be reported before even a small first frame,
    // not only once cumulative delivery has already crossed the band.
    for expected in [b"abcd".as_slice(), b"abcde"] {
        for report in [Some(record_native_write as fn(NativeWriteSize)), None] {
            NATIVE_WRITE_REPORTS.with(|reports| reports.borrow_mut().clear());
            let frames = ExactFrames(VecDeque::from([
                Bytes::copy_from_slice(&expected[..2]),
                Bytes::copy_from_slice(&expected[2..]),
            ]));
            let mut body = HttpWriteChunks::new(Body::new(frames), LIMIT);
            body.observation = NativeWriteObservation::new(
                OutputSizeObserved {
                    producer: OutputProducer::Session,
                    revision: Some(23),
                },
                4,
                report,
            );
            let first = data(&mut body);
            assert_eq!(first, &expected[..2]);
            NATIVE_WRITE_REPORTS.with(|reports| {
                let reports = reports.borrow();
                assert_eq!(reports.len(), usize::from(report.is_some()));
                if let Some(size) = reports.first() {
                    assert_eq!(size.bytes, expected.len() as u64);
                    assert!(size.complete && !size.saturated);
                    assert_eq!(size.metadata.revision, Some(23));
                }
            });
            let mut received = first.to_vec();
            while !body.is_end_stream() {
                received.extend_from_slice(&data(&mut body));
            }
            assert_eq!(received, expected);
            NATIVE_WRITE_REPORTS.with(|reports| {
                assert_eq!(reports.borrow().len(), usize::from(report.is_some()));
            });
        }
    }
}

struct ExactFrames(VecDeque<Bytes>);

impl http_body::Body for ExactFrames {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        Poll::Ready(self.0.pop_front().map(|bytes| Ok(Frame::data(bytes))))
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_empty()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::SizeHint::with_exact(self.0.iter().map(|bytes| bytes.len() as u64).sum())
    }
}

#[test]
fn unknown_stream_crossing_is_one_lower_bound_not_a_collected_response() {
    NATIVE_WRITE_REPORTS.with(|reports| reports.borrow_mut().clear());
    let inner = ScriptBody {
        steps: VecDeque::from([
            Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"ab"))))),
            Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"cde"))))),
            Poll::Pending,
            Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"fghijk"))))),
        ]),
        polls: Arc::new(AtomicU64::new(0)),
    };
    let mut body = HttpWriteChunks::new(Body::new(inner), LIMIT);
    body.observation = NativeWriteObservation::new(
        OutputSizeObserved {
            producer: OutputProducer::Other,
            revision: None,
        },
        4,
        Some(record_native_write),
    );
    assert_eq!(data(&mut body), b"ab".as_slice());
    assert_eq!(data(&mut body), b"cde".as_slice());
    NATIVE_WRITE_REPORTS.with(|reports| {
        let reports = reports.borrow();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].bytes, 5);
        assert!(!reports[0].complete);
    });
    assert!(matches!(poll_frame(&mut body), Poll::Pending));
    assert_eq!(data(&mut body), b"fghi".as_slice());
    assert_eq!(data(&mut body), b"jk".as_slice());
    assert!(matches!(poll_frame(&mut body), Poll::Ready(None)));
    NATIVE_WRITE_REPORTS.with(|reports| assert_eq!(reports.borrow().len(), 1));
}

struct ScriptBody {
    steps: VecDeque<Poll<Option<Result<Frame<Bytes>, io::Error>>>>,
    polls: Arc<AtomicU64>,
}

impl http_body::Body for ScriptBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let this = self.get_mut();
        this.polls.fetch_add(1, Ordering::Relaxed);
        let step = this.steps.pop_front().unwrap_or(Poll::Ready(None));
        if step.is_pending() {
            cx.waker().wake_by_ref();
        }
        step
    }
}

#[test]
fn retained_bytes_precede_pending_and_errors_without_polling_ahead() {
    let polls = Arc::new(AtomicU64::new(0));
    let inner = ScriptBody {
        steps: VecDeque::from([
            Poll::Pending,
            Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"abcdefghi"))))),
            Poll::Pending,
            Poll::Ready(Some(Err(io::Error::other("injected body error")))),
        ]),
        polls: polls.clone(),
    };
    let mut body = HttpWriteChunks::new(Body::new(inner), LIMIT);
    assert!(matches!(poll_frame(&mut body), Poll::Pending));
    assert_eq!(data(&mut body), b"abcd".as_slice());
    assert!(!body.is_end_stream());
    assert_eq!(body.size_hint().lower(), 5);
    assert_eq!(body.size_hint().upper(), None);
    assert_eq!(data(&mut body), b"efgh".as_slice());
    assert_eq!(data(&mut body), b"i".as_slice());
    assert_eq!(
        polls.load(Ordering::Relaxed),
        2,
        "do not poll ahead of retained bytes"
    );
    assert!(matches!(poll_frame(&mut body), Poll::Pending));
    match poll_frame(&mut body) {
        Poll::Ready(Some(Err(error))) => assert!(error.to_string().contains("injected body error")),
        other => panic!("expected original error, got {other:?}"),
    }
}

#[test]
fn trailers_follow_all_retained_data_and_are_not_modified() {
    let mut trailers = axum::http::HeaderMap::new();
    trailers.insert("x-finished", HeaderValue::from_static("yes"));
    let inner = ScriptBody {
        steps: VecDeque::from([
            Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"abcde"))))),
            Poll::Ready(Some(Ok(Frame::trailers(trailers.clone())))),
        ]),
        polls: Arc::new(AtomicU64::new(0)),
    };
    let mut body = HttpWriteChunks::new(Body::new(inner), LIMIT);
    assert_eq!(data(&mut body), b"abcd".as_slice());
    assert_eq!(data(&mut body), b"e".as_slice());
    match poll_frame(&mut body) {
        Poll::Ready(Some(Ok(frame))) => assert_eq!(frame.into_trailers().unwrap(), trailers),
        other => panic!("expected trailers, got {other:?}"),
    }
    assert!(matches!(poll_frame(&mut body), Poll::Ready(None)));
}

#[derive(Default)]
struct WriteProbe {
    bytes: Vec<u8>,
    payload_written: usize,
    payload_slice_lengths: Vec<usize>,
    pending_count: usize,
    contexts: Vec<(Vec<u8>, HttpPanicContext)>,
    read_contexts: Vec<HttpPanicContext>,
}

#[tokio::test]
async fn final_slicing_keeps_compressed_response_bytes_headers_and_sse_boundaries() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state);
    for uri in [
        "/api/state".to_owned(),
        format!("/api/sessions/{session_id}"),
        format!("/api/sessions/{session_id}/history"),
    ] {
        let response = app
            .clone()
            .oneshot(Request::get(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let metadata = *response.extensions().get::<OutputSizeObserved>().unwrap();
        assert!(
            metadata.revision.is_some(),
            "encoding owner knows {uri} revision"
        );
        let mut body =
            HttpWriteChunks::new(response.into_body(), LIMIT).with_output_metadata(metadata);
        let mut received = Vec::new();
        while let Some(frame) = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await
        {
            received.extend_from_slice(&frame.unwrap().into_data().unwrap());
        }
        let json: Value = serde_json::from_slice(&received).unwrap();
        assert_eq!(metadata.revision, json["revision"].as_u64());
    }
    for sse in [false, true] {
        let app = Router::new()
            .route(
                "/data",
                get(move || async move {
                    if sse {
                        Sse::new(async_stream::stream! {
                    yield Ok::<_, Infallible>(Event::default().event("delta").data("é\nline"));
                    yield Ok(Event::default().event("state").data("{}"));
                }).into_response()
                    } else {
                        (
                            StatusCode::CREATED,
                            [("content-type", "application/json"), ("etag", "original")],
                            format!("{{\"text\":\"{}\"}}", "x".repeat(2048)),
                        )
                            .into_response()
                    }
                }),
            )
            .layer(CompressionLayer::new());
        let request = || {
            Request::get("/data")
                .header("accept-encoding", "gzip")
                .body(Body::empty())
                .unwrap()
        };
        let baseline = app.clone().oneshot(request()).await.unwrap();
        let response = app.oneshot(request()).await.unwrap();
        assert_eq!(response.headers(), baseline.headers());
        assert_eq!(response.status(), baseline.status());
        if !sse {
            assert_eq!(response.headers()["content-encoding"], "gzip");
        }
        let expected = to_bytes(baseline.into_body(), 4096).await.unwrap();
        let mut body = HttpWriteChunks::new(response.into_body(), LIMIT);
        let mut actual = Vec::new();
        while let Some(frame) = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await
        {
            let bytes = frame.unwrap().into_data().unwrap();
            assert!(bytes.len() <= LIMIT.get());
            actual.extend_from_slice(&bytes);
        }
        assert_eq!(actual, expected);
        if sse {
            assert_eq!(
                actual,
                "event: delta\ndata: é\ndata: line\n\nevent: state\ndata: {}\n\n".as_bytes()
            );
        }
    }
}

/// Actual Hyper vectored writes: alternate Pending and short writes. Recognize
/// the retained source allocation so headers are not mistaken for body slices.
struct ProbeIo {
    request: &'static [u8],
    probe: Arc<Mutex<WriteProbe>>,
    payload_range: std::ops::Range<usize>,
    pending_next: bool,
    fail_after_payload: Option<usize>,
    panic_on_payload: bool,
}

impl tokio::io::AsyncRead for ProbeIo {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.probe
            .lock()
            .unwrap()
            .read_contexts
            .push(current_http_panic_context());
        // An open peer awaits the response; EOF would abort Hyper's default
        // non-half-closed connection before its service/writer is polled.
        if this.request.is_empty() {
            return Poll::Pending;
        }
        let n = this.request.len().min(buf.remaining());
        buf.put_slice(&this.request[..n]);
        this.request = &this.request[n..];
        Poll::Ready(Ok(()))
    }
}

impl tokio::io::AsyncWrite for ProbeIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.poll_write_vectored(cx, &[io::IoSlice::new(bytes)])
    }

    fn is_write_vectored(&self) -> bool {
        true
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let mut probe = this.probe.lock().unwrap();
        if this
            .fail_after_payload
            .is_some_and(|n| probe.payload_written >= n)
        {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        if this.pending_next {
            this.pending_next = false;
            probe.pending_count += 1;
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        this.pending_next = true;
        let mut written = 0;
        for buf in bufs {
            let is_payload = this.payload_range.contains(&(buf.as_ptr() as usize));
            if is_payload {
                if this.panic_on_payload {
                    drop(probe);
                    panic!("injected writer panic outside Body::poll_frame");
                }
                assert!(
                    buf.len() <= LIMIT.get(),
                    "oversized slice reached Hyper's write sink"
                );
                probe.payload_slice_lengths.push(buf.len());
            }
            let mut n = buf.len().min(2 - written);
            if is_payload && let Some(stop) = this.fail_after_payload {
                n = n.min(stop - probe.payload_written);
            }
            probe.bytes.extend_from_slice(&buf[..n]);
            if is_payload && n > 0 {
                probe
                    .contexts
                    .push((buf[..n].to_vec(), current_http_panic_context()));
            }
            probe.payload_written += if is_payload { n } else { 0 };
            written += n;
            if written == 2 || n < buf.len() {
                break;
            }
        }
        Poll::Ready(Ok(written))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn hyper_partial_writes_and_pending_preserve_every_body_byte_and_transport_errors() {
    for (payload, fail_after) in [
        (b"abcd".as_slice(), None),
        (b"abcde".as_slice(), None),
        (b"abcdefghijklm".as_slice(), None),
        (b"abcdefghijklm".as_slice(), Some(3)),
    ] {
        let bytes = Bytes::copy_from_slice(payload);
        let start = bytes.as_ptr() as usize;
        let probe = Arc::new(Mutex::new(WriteProbe::default()));
        let io = ProbeIo {
            request: b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            probe: probe.clone(),
            payload_range: start..start + bytes.len(),
            pending_next: true,
            fail_after_payload: fail_after,
            panic_on_payload: false,
        };
        let service = hyper::service::service_fn(move |_| {
            let mut response =
                axum::http::Response::new(HttpWriteChunks::new(Body::from(bytes.clone()), LIMIT));
            *response.status_mut() = StatusCode::CREATED;
            response
                .headers_mut()
                .insert("x-preserved", HeaderValue::from_static("original"));
            std::future::ready(Ok::<_, Infallible>(response))
        });
        let result = hyper::server::conn::http1::Builder::new()
            .auto_date_header(false)
            .serve_connection(hyper_util::rt::TokioIo::new(io), service)
            .await;
        assert_eq!(result.is_err(), fail_after.is_some());
        if let Err(error) = result {
            let source = std::error::Error::source(&error).unwrap();
            assert_eq!(
                source.downcast_ref::<io::Error>().unwrap().kind(),
                io::ErrorKind::BrokenPipe
            );
        }
        let probe = probe.lock().unwrap();
        let end = probe
            .bytes
            .windows(4)
            .position(|p| p == b"\r\n\r\n")
            .unwrap()
            + 4;
        let headers = std::str::from_utf8(&probe.bytes[..end]).unwrap();
        assert!(headers.starts_with("HTTP/1.1 201 Created\r\n"));
        assert!(headers.contains("x-preserved: original\r\n"));
        assert!(headers.contains(&format!("content-length: {}\r\n", payload.len())));
        let expected = &payload[..fail_after.unwrap_or(payload.len())];
        assert_eq!(
            &probe.bytes[end..],
            expected,
            "no repeated/skipped bytes after Pending/partial write"
        );
        assert_eq!(probe.payload_written, expected.len());
        assert!(
            !probe.payload_slice_lengths.is_empty(),
            "exercise actual queued zero-copy body IoSlices"
        );
        assert!(probe.pending_count > 0);
    }
}

tokio::task_local! {
    static PANIC_JOURNAL: Arc<HostLifecycle>;
    static PREVIOUS_HOOK_CALLS: Arc<AtomicU64>;
}

fn install_task_scoped_panic_journal() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        // Install once, not once per parallel test. Other test tasks lack
        // PANIC_JOURNAL and pass through to the original hook unchanged.
        std::panic::set_hook(host_lifecycle_panic_hook(
            Box::new(move |info| {
                let _ = PREVIOUS_HOOK_CALLS.try_with(|calls| {
                    calls.fetch_add(1, Ordering::Relaxed);
                });
                previous(info);
            }),
            || PANIC_JOURNAL.try_with(Arc::clone).ok(),
        ));
    });
}

fn diagnostic_router(bytes: Bytes) -> Router {
    let first = bytes.slice(..4);
    let second = bytes.slice(4..);
    Router::new()
        .route(
            "/first",
            get(move || {
                let bytes = first.clone();
                async move { bytes }
            }),
        )
        .route(
            "/second",
            get(move || {
                let bytes = second.clone();
                async move { bytes }
            }),
        )
        .layer(axum::middleware::from_fn(chunk_http_response_writes))
}

#[tokio::test]
async fn scoped_hyper_writer_has_request_context_and_keep_alive_never_leaks_the_previous_route() {
    let bytes = Bytes::from_static(b"aaaabbbb");
    let start = bytes.as_ptr() as usize;
    let probe = Arc::new(Mutex::new(WriteProbe::default()));
    let io = ProbeIo {
        request: b"GET /first HTTP/1.1\r\nHost: localhost\r\n\r\nGET /second HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        probe: probe.clone(), payload_range: start..start + bytes.len(),
        pending_next: true, fail_after_payload: None, panic_on_payload: false,
    };
    let (_signal, receiver) = tokio::sync::watch::channel(false);
    tokio::spawn(scoped_http_connection(
        io,
        diagnostic_router(bytes),
        receiver,
    ))
    .await
    .unwrap();
    assert_eq!(current_http_panic_context(), HttpPanicContext::default());
    let probe = probe.lock().unwrap();
    assert_eq!(probe.payload_written, 8);
    assert!(!probe.contexts.is_empty());
    for (bytes, context) in &probe.contexts {
        let expected = if bytes[0] == b'a' {
            "/first"
        } else {
            "/second"
        };
        assert_eq!(
            context.route, expected,
            "attribute the bytes actually being written"
        );
    }
    assert!(!probe.read_contexts.is_empty());
    assert!(
        probe
            .read_contexts
            .iter()
            .all(|context| context == &HttpPanicContext::default())
    );
}

#[tokio::test]
async fn task_panics_record_writer_context_unknown_context_bounded_trace_and_chain_the_hook() {
    install_task_scoped_panic_journal();
    let state = test_app_state();
    let directory = state
        .test_temp_root_path()
        .unwrap()
        .join("writer-panic-journal");
    fs::create_dir_all(&directory).unwrap();
    let lifecycle = Arc::new(HostLifecycle::open(directory, "writer-test".into(), None));
    let previous_calls = Arc::new(AtomicU64::new(0));
    let bytes = Bytes::from_static(b"aaaabbbb");
    let start = bytes.as_ptr() as usize;
    let io = ProbeIo {
        request: b"GET /first HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        probe: Arc::new(Mutex::new(WriteProbe::default())),
        payload_range: start..start + bytes.len(),
        pending_next: true,
        fail_after_payload: None,
        panic_on_payload: true,
    };
    let (_signal, receiver) = tokio::sync::watch::channel(false);
    let task = PANIC_JOURNAL.scope(
        lifecycle.clone(),
        PREVIOUS_HOOK_CALLS.scope(
            previous_calls.clone(),
            scoped_http_connection(io, diagnostic_router(bytes), receiver),
        ),
    );
    assert!(tokio::spawn(task).await.unwrap_err().is_panic());
    let missing = PANIC_JOURNAL.scope(
        lifecycle.clone(),
        PREVIOUS_HOOK_CALLS.scope(previous_calls.clone(), async {
            panic!("injected task without request context");
        }),
    );
    assert!(tokio::spawn(missing).await.unwrap_err().is_panic());
    assert_eq!(previous_calls.load(Ordering::Relaxed), 2);
    let records = lifecycle.journal.read_records();
    let panics: Vec<_> = records
        .iter()
        .filter_map(|record| match record {
            HostLifecycleRecord::Panic {
                context,
                backtrace,
                location,
                ..
            } => Some((context, backtrace, location)),
            _ => None,
        })
        .collect();
    assert_eq!(panics.len(), 2);
    assert_eq!(
        panics[0].0.route, "/first",
        "panic occurs in Hyper's IO, outside body polling"
    );
    assert_eq!(panics[1].0, &HttpPanicContext::default());
    for (_, trace, location) in panics {
        let (path, line) = location.as_ref().unwrap().rsplit_once(':').unwrap();
        assert!(line.parse::<u32>().is_ok());
        assert!(!path.contains(':') && !path.contains('\\') && !path.starts_with('/'));
        assert!(!path.contains('/') || path.starts_with("src/"));
        match trace {
            HostPanicTrace::WindowsStack { count, addresses } => {
                assert!((1..=HOST_PANIC_TRACE_FRAMES as u16).contains(count));
                assert!(
                    addresses[..*count as usize]
                        .iter()
                        .all(|address| *address != 0)
                );
                assert!(
                    addresses[*count as usize..]
                        .iter()
                        .all(|address| *address == 0)
                );
            }
            HostPanicTrace::Unavailable => {
                assert!(!cfg!(windows), "Windows capture must be exercised")
            }
        }
    }
    assert_eq!(current_http_panic_context(), HttpPanicContext::default());
}

#[tokio::test]
async fn diagnostic_scopes_restore_and_ambiguous_or_flushed_writes_become_unknown() {
    let context = HttpPanicContext {
        route: "/outer".into(),
        session: "session-a".into(),
    };
    let inner = HttpPanicContext {
        route: "/inner".into(),
        session: "session-b".into(),
    };
    HTTP_CONNECTION_DIAGNOSTIC
        .scope(std::cell::RefCell::default(), async {
            let first = begin_http_response_diagnostic();
            claim_http_write_diagnostic(first, &context);
            assert_eq!(current_http_panic_context(), context);
            HTTP_REQUEST_DIAGNOSTIC
                .scope(inner.clone(), async {
                    assert_eq!(current_http_panic_context(), inner);
                })
                .await;
            assert_eq!(current_http_panic_context(), context);
            let second = begin_http_response_diagnostic();
            claim_http_write_diagnostic(second, &inner);
            assert_eq!(current_http_panic_context(), HttpPanicContext::default());
            clear_http_write_diagnostic();
            assert_eq!(current_http_panic_context(), HttpPanicContext::default());
            claim_http_write_diagnostic(second, &inner);
            assert_eq!(current_http_panic_context(), inner);
        })
        .await;
    assert_eq!(current_http_panic_context(), HttpPanicContext::default());
}

#[test]
fn panic_journal_still_skips_contention_and_reads_legacy_records() {
    let legacy: HostLifecycleRecord = serde_json::from_value(json!({
        "kind": "panic", "at": "old", "instanceId": "previous", "thread": "worker",
        "location": "repr_windows.rs:21", "message": "literal"
    }))
    .unwrap();
    assert!(
        matches!(legacy, HostLifecycleRecord::Panic { context, backtrace: HostPanicTrace::Unavailable, .. }
        if context == HttpPanicContext::default())
    );
    let state = test_app_state();
    let path = state
        .test_temp_root_path()
        .unwrap()
        .join("contended-panic-journal");
    fs::create_dir_all(&path).unwrap();
    let lifecycle = Arc::new(HostLifecycle::open(path, "contended".into(), None));
    let held = lifecycle.journal.writing.lock().unwrap();
    let worker = lifecycle.clone();
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        worker.record_panic("fixture", Some("src/main.rs:1".into()), "literal".into());
        tx.send(()).unwrap();
    });
    let result = rx.recv_timeout(phase_sync::DEADLOCK_GUARD);
    drop(held);
    worker.join().unwrap();
    result.expect("panic recording must not wait on an in-flight journal append");
    assert_eq!(
        lifecycle.journal.read_records().len(),
        1,
        "contended panic append was skipped"
    );
}

#[tokio::test]
async fn serving_shutdown_drains_the_live_stream_before_returning() {
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let finish = Arc::new(Mutex::new(Some(finish_rx)));
        let app = Router::new()
            .route(
                "/stream",
                get(move || {
                    let finish = finish.lock().unwrap().take().unwrap();
                    async move {
                        Body::from_stream(async_stream::stream! {
                            yield Ok::<_, io::Error>(Bytes::from_static(b"first"));
                            finish.await.unwrap();
                            yield Ok(Bytes::from_static(b"last"));
                        })
                    }
                }),
            )
            .layer(axum::middleware::from_fn(chunk_http_response_writes));
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let (noticed_tx, noticed_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(serve_http_with_panic_context(listener, app, async move {
            stop_rx.await.unwrap();
            noticed_tx.send(()).unwrap();
        }));
        let mut response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}/stream"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.chunk().await.unwrap().unwrap(), "first");
        stop_tx.send(()).unwrap();
        noticed_rx.await.unwrap();
        assert!(
            !server.is_finished(),
            "shutdown must retain and drain the live connection"
        );
        finish_tx.send(()).unwrap();
        assert_eq!(response.bytes().await.unwrap(), "last");
        server.await.unwrap().unwrap();
    })
    .await
    .expect("server and graceful stream drain must complete");
}
