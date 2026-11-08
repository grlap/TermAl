/// Small diagnostic facts only; never a URL, query, body, grant or authority.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct HttpPanicContext {
    route: String,
    session: String,
}

impl Default for HttpPanicContext {
    fn default() -> Self {
        Self {
            route: "unknown".into(),
            session: "unknown".into(),
        }
    }
}

fn capped_http_diagnostic(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || "/{}_.:-".contains(*ch))
        .take(128)
        .collect()
}

impl HttpPanicContext {
    fn from_request(request: &axum::extract::Request) -> Self {
        let Some(route) = request.extensions().get::<axum::extract::MatchedPath>() else {
            return Self::default();
        };
        let session = if route.as_str().contains("/sessions/{id}") {
            request
                .uri()
                .path()
                .strip_prefix("/api/sessions/")
                .and_then(|path| path.split('/').next())
                .filter(|id| {
                    !id.is_empty()
                        && id.len() <= 128
                        && id
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                })
                .unwrap_or("unknown")
        } else {
            "unknown"
        };
        Self {
            route: capped_http_diagnostic(route.as_str()),
            session: session.to_owned(),
        }
    }
}

#[derive(Default)]
struct HttpConnectionDiagnostic {
    next_response: u64,
    buffered: Option<(u64, HttpPanicContext)>,
    ambiguous: bool,
}

tokio::task_local! {
    // Each spawned connection owns one bounded slot. No task/thread registry.
    static HTTP_CONNECTION_DIAGNOSTIC: std::cell::RefCell<HttpConnectionDiagnostic>;
    static HTTP_REQUEST_DIAGNOSTIC: HttpPanicContext;
}

fn current_http_panic_context() -> HttpPanicContext {
    HTTP_REQUEST_DIAGNOSTIC
        .try_with(Clone::clone)
        .ok()
        .unwrap_or_else(|| {
            HTTP_CONNECTION_DIAGNOSTIC
                .try_with(|slot| {
                    slot.try_borrow().ok().and_then(|slot| {
                        (!slot.ambiguous)
                            .then(|| slot.buffered.as_ref().map(|(_, context)| context.clone()))
                            .flatten()
                    })
                })
                .ok()
                .flatten()
                .unwrap_or_default()
        })
}

fn begin_http_response_diagnostic() -> Option<u64> {
    HTTP_CONNECTION_DIAGNOSTIC
        .try_with(|slot| {
            let mut slot = slot.try_borrow_mut().ok()?;
            // A newer handler is not proof that older buffered bytes were flushed.
            if slot.buffered.is_some() {
                slot.ambiguous = true;
            }
            slot.next_response = slot.next_response.checked_add(1)?;
            Some(slot.next_response)
        })
        .ok()
        .flatten()
}

fn claim_http_write_diagnostic(owner: Option<u64>, context: &HttpPanicContext) {
    let _ = HTTP_CONNECTION_DIAGNOSTIC.try_with(|slot| {
        if let Ok(mut slot) = slot.try_borrow_mut() {
            let Some(owner) = owner else {
                slot.ambiguous = true;
                return;
            };
            if slot.buffered.as_ref().is_some_and(|(old, _)| *old != owner) {
                slot.ambiguous = true;
            }
            if !slot.ambiguous {
                slot.buffered = Some((owner, context.clone()));
            }
        }
    });
}

fn clear_http_write_diagnostic() {
    let _ = HTTP_CONNECTION_DIAGNOSTIC.try_with(|slot| {
        if let Ok(mut slot) = slot.try_borrow_mut() {
            slot.buffered = None;
            slot.ambiguous = false;
        }
    });
}

/// Hyper calls the underlying flush only after its buffered bytes are drained.
/// Body EOF alone never clears attribution. Reads/idle connections are unknown.
struct HttpDiagnosticIo<T>(T);

impl<T: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for HttpDiagnosticIo<T> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        HTTP_REQUEST_DIAGNOSTIC.sync_scope(HttpPanicContext::default(), || {
            std::pin::Pin::new(&mut self.get_mut().0).poll_read(cx, buf)
        })
    }
}

impl<T: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for HttpDiagnosticIo<T> {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(&mut self.get_mut().0).poll_write(cx, buf)
    }

    fn is_write_vectored(&self) -> bool {
        self.0.is_write_vectored()
    }

    fn poll_write_vectored(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(&mut self.get_mut().0).poll_write_vectored(cx, bufs)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        let result = std::pin::Pin::new(&mut self.get_mut().0).poll_flush(cx);
        if matches!(result, std::task::Poll::Ready(Ok(()))) {
            clear_http_write_diagnostic();
        }
        result
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        HTTP_REQUEST_DIAGNOSTIC.sync_scope(HttpPanicContext::default(), || {
            std::pin::Pin::new(&mut self.get_mut().0).poll_shutdown(cx)
        })
    }
}

/// The same concrete listener/Router/Hyper path as axum::serve, with its missing
/// connection-future scoping hook. Reuse axum's accept/error handling and watch
/// completion guards; SSE signalling and the post-serve persist drain stay in
/// run_server. No connection registry, second listener or shutdown deadline.
async fn serve_http_with_panic_context(
    mut listener: TcpListener,
    router: Router,
    shutdown: impl std::future::Future<Output = ()>,
) -> io::Result<()> {
    let (shutdown_tx, _) = tokio::sync::watch::channel(false);
    let (finished_tx, finished_rx) = tokio::sync::watch::channel(());
    tokio::pin!(shutdown);
    loop {
        let (io, _) = tokio::select! {
            accepted = axum::serve::Listener::accept(&mut listener) => accepted,
            _ = &mut shutdown => break,
        };
        let receiver = shutdown_tx.subscribe();
        let finished = finished_rx.clone();
        let router = router.clone();
        tokio::spawn(async move {
            // Drop on completion OR panic: the server's drain never loses a task.
            let _finished = finished;
            scoped_http_connection(io, router, receiver).await;
        });
    }
    drop(listener);
    shutdown_tx.send_replace(true);
    drop(finished_rx);
    finished_tx.closed().await;
    Ok(())
}

async fn scoped_http_connection<I>(
    io: I,
    router: Router,
    shutdown: tokio::sync::watch::Receiver<bool>,
) where
    I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    HTTP_CONNECTION_DIAGNOSTIC
        .scope(
            std::cell::RefCell::default(),
            run_http_connection(io, router, shutdown),
        )
        .await;
}

async fn run_http_connection<I>(
    io: I,
    router: Router,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) where
    I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let builder =
        hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
    let connection = builder.serve_connection_with_upgrades(
        hyper_util::rt::TokioIo::new(HttpDiagnosticIo(io)),
        hyper_util::service::TowerToHyperService::new(router),
    );
    tokio::pin!(connection);
    let stopped = async {
        if !*shutdown.borrow_and_update() {
            let _ = shutdown.changed().await;
        }
    };
    tokio::pin!(stopped);
    tokio::select! {
        _ = &mut connection => {}, // Connection errors stay local, as in axum::serve.
        _ = &mut stopped => {
            connection.as_mut().graceful_shutdown();
            let _ = connection.await;
        },
    }
}

const HOST_PANIC_TRACE_FRAMES: usize = 32;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "capture", rename_all = "camelCase")]
enum HostPanicTrace {
    WindowsStack {
        count: u16,
        addresses: [u64; HOST_PANIC_TRACE_FRAMES],
    },
    #[default]
    Unavailable,
}

fn capture_host_panic_trace() -> HostPanicTrace {
    #[cfg(windows)]
    {
        let mut frames = [std::ptr::null_mut(); HOST_PANIC_TRACE_FRAMES];
        // SAFETY: fixed writable pointer array; the OS captures at most its
        // supplied length. No symbolization or whole-stack capture/formatting.
        let count = unsafe {
            windows_sys::Win32::System::Diagnostics::Debug::RtlCaptureStackBackTrace(
                1,
                HOST_PANIC_TRACE_FRAMES as u32,
                frames.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };
        if count > 0 {
            return HostPanicTrace::WindowsStack {
                count,
                addresses: frames.map(|frame| frame as usize as u64),
            };
        }
    }
    // Other platforms need a bounded native capture owner; do not substitute
    // an unbounded std::Backtrace followed by truncation. Location remains.
    HostPanicTrace::Unavailable
}
