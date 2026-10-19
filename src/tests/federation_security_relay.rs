// Owns the adversarial forward/control relay and exact fake-child cleanup.
// Does not hold a pair key or fabricate authenticated frames. Split from the security pair fixture.

use super::*;
use std::sync::atomic::{AtomicU8, AtomicU64};

pub(super) struct ObservedRequest {
    pub counter: Option<u64>,
    pub key_seen: bool,
}

#[derive(Default)]
pub(super) struct Relay {
    pub requests: Mutex<Vec<ObservedRequest>>,
    pub changed: tokio::sync::Notify,
    pub response_fault: AtomicU8,
    pub corrupt_proof: [AtomicBool; 2],
    pub attempts: AtomicU64,
    pub unproven: AtomicBool,
    pub hold_counter: AtomicU64,
    pub release_http: tokio::sync::Notify,
    pub mute: [AtomicBool; 2],
    pub last_heartbeat: Mutex<[Option<Vec<u8>>; 2]>,
    pub inject: Mutex<[Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>; 2]>,
}

pub(super) struct TestChild {
    control: Option<FederationControlIo>,
    stop: tokio::sync::watch::Sender<bool>,
    exited: mpsc::Receiver<()>,
}

impl TestChild {
    pub fn start(
        handle: &tokio::runtime::Handle,
        relay: Arc<Relay>,
        socket: std::net::TcpListener,
        target: u16,
    ) -> Result<Box<dyn FederationSshChild>, ApiError> {
        let _entered = handle.enter();
        socket.set_nonblocking(true).unwrap();
        let listener = TcpListener::from_std(socket).unwrap();
        let remote = std::net::TcpStream::connect(("127.0.0.1", target))
            .map_err(|_| federation_transport_unavailable())?;
        remote.set_nonblocking(true).unwrap();
        let remote = tokio::net::TcpStream::from_std(remote).unwrap();
        let (control, bridge) = tokio::io::duplex(65536);
        let (stop, stop_rx) = tokio::sync::watch::channel(false);
        let control_stop = stop_rx.clone();
        let control_relay = relay.clone();
        let control_task = handle.spawn(async move {
            relay_control(bridge, remote, control_relay, control_stop).await;
        });
        let forward = reqwest::Client::builder()
            .no_proxy()
            .no_gzip()
            .build()
            .unwrap();
        let router = Router::new().fallback(move |request: axum::extract::Request| {
            let relay = relay.clone();
            let forward = forward.clone();
            let mut stopped = stop_rx.clone();
            async move {
                let (parts, body) = request.into_parts();
                let body = axum::body::to_bytes(body, 4096).await.unwrap();
                let counter = parts.headers.get(AUTH_HEADER).and_then(|header| header.to_str().ok())
                    .and_then(|header| header.split(':').nth(3)).and_then(|counter| counter.parse().ok());
                let key = "a".repeat(64);
                let key_seen = parts.headers.values().any(|value| value.as_bytes().windows(64)
                    .any(|bytes| bytes == key.as_bytes())) || body.windows(64).any(|bytes| bytes == key.as_bytes());
                relay.requests.lock().unwrap().push(ObservedRequest { counter, key_seen });
                relay.changed.notify_waiters();
                if counter.is_some_and(|counter| counter == relay.hold_counter.load(Ordering::SeqCst)) {
                    tokio::select! {
                        _ = relay.release_http.notified() => {},
                        _ = federation_wait_stop(&mut stopped) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                    }
                }
                let response = forward
                    .request(parts.method, format!("http://127.0.0.1:{target}{}", parts.uri))
                    .headers(parts.headers).body(body).send().await.unwrap();
                let mut status = response.status();
                let mut headers = response.headers().clone();
                let body = response.bytes().await.unwrap();
                match relay.response_fault.swap(0, Ordering::SeqCst) {
                    1 => status = StatusCode::ACCEPTED,
                    2 => { headers.insert("content-type", HeaderValue::from_static("text/plain")); },
                    3 => { headers.insert("x-termal-receipt", HeaderValue::from_static("other-operation")); },
                    4 => { headers.remove(AUTH_HEADER); },
                    5 => { headers.append(AUTH_HEADER, HeaderValue::from_static("duplicate")); },
                    6 => { status = StatusCode::CONFLICT; headers.remove(AUTH_HEADER); },
                    _ => {},
                }
                (status, headers, body).into_response()
            }
        });
        let mut server_stop = stop.subscribe();
        let (exited_tx, exited) = mpsc::channel();
        handle.spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move { federation_wait_stop(&mut server_stop).await })
                .await;
            let _ = control_task.await;
            let _ = exited_tx.send(());
        });
        Ok(Box::new(Self {
            control: Some(Box::new(control)),
            stop,
            exited,
        }))
    }
}

impl FederationSshChild for TestChild {
    fn take_control_stream(&mut self) -> Result<FederationControlIo, ApiError> {
        self.control
            .take()
            .ok_or_else(federation_transport_unavailable)
    }
    fn child_exited(&mut self) -> Result<bool, ApiError> {
        Ok(false)
    }
    fn shutdown(self: Box<Self>) -> Result<(), ApiError> {
        self.stop.send_replace(true);
        self.exited
            .recv_timeout(phase_sync::DEADLOCK_GUARD)
            .map_err(|_| federation_transport_unavailable())?;
        Ok(())
    }
}
impl Drop for TestChild {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

async fn relay_control(
    mut local: tokio::io::DuplexStream,
    mut remote: tokio::net::TcpStream,
    relay: Arc<Relay>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let mut preface = vec![0; CONTROL_PREFACE.len()];
    tokio::select! {
        _ = federation_wait_stop(&mut stop) => return,
        read = local.read_exact(&mut preface) => if read.is_err() { return; },
    }
    if remote.write_all(&preface).await.is_err() {
        return;
    }
    let (local_read, local_write) = tokio::io::split(local);
    let (remote_read, remote_write) = tokio::io::split(remote);
    tokio::join!(
        relay_direction(local_read, remote_write, relay.clone(), 0, stop.clone()),
        relay_direction(remote_read, local_write, relay, 1, stop),
    );
}

async fn relay_direction(
    mut read: impl tokio::io::AsyncRead + Unpin,
    mut write: impl tokio::io::AsyncWrite + Unpin,
    relay: Arc<Relay>,
    direction: usize,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let (inject, mut injected) = tokio::sync::mpsc::unbounded_channel();
    relay.inject.lock().unwrap()[direction] = Some(inject);
    loop {
        let read_frame = async {
            let mut header = [0; 4];
            read.read_exact(&mut header).await?;
            let count = u32::from_be_bytes(header) as usize;
            if count > 65536 {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            let mut body = vec![0; count];
            read.read_exact(&mut body).await?;
            Ok::<_, std::io::Error>((header, body))
        };
        tokio::pin!(read_frame);
        let frame = loop {
            tokio::select! {
                _ = federation_wait_stop(&mut stop) => return,
                Some(frame) = injected.recv() => {
                    if write.write_all(&frame).await.is_err() { return; }
                },
                frame = &mut read_frame => break frame,
            }
        };
        {
            let Ok((header, mut body)) = frame else {
                return;
            };
            let proof = if direction == 0 {
                b"termal-hello-c-v1".as_slice()
            } else {
                b"termal-hello-proof-v1".as_slice()
            };
            if body.windows(proof.len()).any(|bytes| bytes == proof)
                && relay.corrupt_proof[direction].swap(false, Ordering::SeqCst)
            {
                if let Some(last) = body.last_mut() {
                    *last ^= 1;
                }
                relay.changed.notify_waiters();
            }
            let heartbeat = if direction == 0 {
                b"termal-hb-req-v1".as_slice()
            } else {
                b"termal-hb-resp-v1".as_slice()
            };
            let frame = joined(&[&header, &body]);
            if body
                .windows(heartbeat.len())
                .any(|bytes| bytes == heartbeat)
            {
                relay.last_heartbeat.lock().unwrap()[direction] = Some(frame.clone());
                relay.changed.notify_waiters();
            }
            if !relay.mute[direction].load(Ordering::SeqCst)
                && write.write_all(&frame).await.is_err()
            {
                return;
            }
        }
    }
}
