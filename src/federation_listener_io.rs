// Owns bounded accepted connections and shutdown cancellation of their IO.
// Does not authenticate HTTP or change enrollment. Split from federation_peer.rs.

struct FederationSocketListener {
    listener: TcpListener,
    stop: tokio::sync::watch::Receiver<bool>,
    connections: Arc<tokio::sync::Semaphore>,
}

struct FederationSocket {
    stream: tokio::net::TcpStream,
    closed: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
    cancelled: bool,
    _connection: tokio::sync::OwnedSemaphorePermit,
}

impl FederationSocket {
    fn cancelled(&mut self, cx: &mut std::task::Context<'_>) -> bool {
        if !self.cancelled && self.closed.as_mut().poll(cx).is_ready() {
            self.cancelled = true;
        }
        self.cancelled
    }
}

impl axum::serve::Listener for FederationSocketListener {
    type Io = FederationSocket;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let permit = self
                .connections
                .clone()
                .acquire_owned()
                .await
                .expect("connection semaphore stays open");
            match self.listener.accept().await {
                Ok((stream, address)) => {
                    let mut stop = self.stop.clone();
                    return (
                        FederationSocket {
                            stream,
                            cancelled: false,
                            _connection: permit,
                            closed: Box::pin(async move { federation_wait_stop(&mut stop).await }),
                        },
                        address,
                    );
                }
                Err(error) => {
                    eprintln!("federation listener accept failed: {error}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

impl tokio::io::AsyncRead for FederationSocket {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.cancelled(cx) {
            return std::task::Poll::Ready(Ok(()));
        }
        tokio::io::AsyncRead::poll_read(std::pin::Pin::new(&mut this.stream), cx, buffer)
    }
}

impl tokio::io::AsyncWrite for FederationSocket {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buffer: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if this.cancelled(cx) {
            return std::task::Poll::Ready(Err(std::io::ErrorKind::ConnectionAborted.into()));
        }
        tokio::io::AsyncWrite::poll_write(std::pin::Pin::new(&mut this.stream), cx, buffer)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.cancelled(cx) {
            return std::task::Poll::Ready(Ok(()));
        }
        tokio::io::AsyncWrite::poll_flush(std::pin::Pin::new(&mut this.stream), cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.cancelled(cx) {
            return std::task::Poll::Ready(Ok(()));
        }
        tokio::io::AsyncWrite::poll_shutdown(std::pin::Pin::new(&mut this.stream), cx)
    }
}

async fn serve_federation_listener(
    listener: TcpListener,
    router: Router,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> std::io::Result<()> {
    let listener = FederationSocketListener {
        listener,
        stop: stop.clone(),
        connections: Arc::new(tokio::sync::Semaphore::new(4)),
    };
    axum::serve(listener, router)
        .with_graceful_shutdown(async move { federation_wait_stop(&mut stop).await })
        .await
}
