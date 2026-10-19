// Owns bounded accepted connections and shutdown cancellation of their IO.
// Does not authenticate HTTP or change enrollment. Split from federation_peer.rs.

struct FederationSocketListener {
    listener: TcpListener,
    stop: tokio::sync::watch::Receiver<bool>,
    connections: Arc<tokio::sync::Semaphore>,
    control: Option<FederationChannel>,
}

struct FederationSocket {
    stream: tokio::net::TcpStream,
    closed: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
    cancelled: bool,
    _connection: Option<tokio::sync::OwnedSemaphorePermit>,
    prefix: Vec<u8>,
    prefix_offset: usize,
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
                Ok((mut stream, address)) => {
                    let prefix = if self.control.is_some() {
                        match tokio::time::timeout(
                            Duration::from_secs(5),
                            federation_classify_preface(&mut stream),
                        )
                        .await
                        {
                            Ok(Ok(prefix)) => prefix,
                            _ => continue,
                        }
                    } else {
                        Vec::new()
                    };
                    let mut stop = self.stop.clone();
                    let mut socket = FederationSocket {
                        stream,
                        cancelled: false,
                        _connection: Some(permit),
                        prefix,
                        prefix_offset: 0,
                        closed: Box::pin(async move { federation_wait_stop(&mut stop).await }),
                    };
                    if socket.prefix == FEDERATION_CONTROL_PREFACE {
                        let Some(channel) = self.control.clone().filter(|channel| {
                            channel.snapshot.local_role == FederationRole::Satellite
                        }) else {
                            continue;
                        };
                        let Ok(slot) = channel.control_slots.clone().try_acquire_owned() else {
                            continue;
                        };
                        socket.prefix.clear();
                        socket._connection.take();
                        let tasks = channel.control_tasks.clone();
                        let task = tokio::spawn(async move {
                            let _slot = slot;
                            federation_control_satellite(channel, Box::new(socket)).await;
                        });
                        let mut tasks = tasks.lock().expect("control tasks mutex poisoned");
                        tasks.retain(|task| !task.is_finished());
                        tasks.push(task);
                        continue;
                    }
                    return (socket, address);
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
        if this.prefix_offset < this.prefix.len() {
            let count = buffer
                .remaining()
                .min(this.prefix.len() - this.prefix_offset);
            buffer.put_slice(&this.prefix[this.prefix_offset..this.prefix_offset + count]);
            this.prefix_offset += count;
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
