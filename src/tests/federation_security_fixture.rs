// Owns a real satellite AppState/runtime with fake credential storage only.
// Does not fabricate attestation or run SSH. Split from the security protocol tests.

use super::*;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct PeerInfo {
    pub port: u16,
    pub central_id: String,
    pub satellite_id: String,
    pub enrollment: u64,
    pub epoch: u64,
    pub boot: String,
}

pub(super) struct Satellite {
    pub app: AppState,
    pub port: u16,
    pub handle: Arc<FederationPeerHandle>,
    pub clock: FederationControlClock,
    events: tokio::sync::mpsc::UnboundedReceiver<(&'static str, u64)>,
    runtime: Option<FederationRuntime>,
}

impl Satellite {
    pub async fn new() -> Self {
        let app = super::super::federation_registry::domain_state();
        let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = reserve.local_addr().unwrap().port();
        let central_port = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer =
            super::super::federation_registry::peer(central_port.local_addr().unwrap().port());
        peer.endpoint.satellite_federation_port = port;
        app.configure_federation(FederationSettings {
            enabled: true,
            role: FederationRole::Satellite,
            central: Some(peer.identity.clone()),
        })
        .unwrap();
        let id = peer.identity.host_id.clone();
        app.enroll_federation_peer(peer).unwrap();
        let clock = FederationControlClock::scripted();
        let (events_tx, events) = tokio::sync::mpsc::unbounded_channel();
        let io = FederationRuntimeIo {
            credentials: Arc::new(|_| {
                Ok(FederationTransportCredentials {
                    capability: FederationPairCapability::new("a".repeat(64)).unwrap(),
                    known_hosts: None,
                    material_fingerprint: [1; 32],
                })
            }),
            ssh: Arc::new(|_, _, _| panic!("a satellite never launches SSH")),
            reconcile_interval: Duration::from_millis(20),
            // Reverse RPC probing is not the heartbeat under test. Leave it
            // outside the control deadline, so a failed reverse TCP probe
            // cannot accidentally satisfy a heartbeat-expiry assertion.
            probe_interval: Duration::from_secs(3600),
            control_clock: clock.clone(),
            control_observed: Arc::new(move |event, number| {
                let _ = events_tx.send((event, number));
            }),
            ..FederationRuntimeIo::default()
        };
        drop(reserve);
        drop(central_port);
        let runtime = start_federation_runtime_with_io(app.clone(), io);
        let handle = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
            loop {
                if let Some(handle) = runtime.peer(&id) {
                    break handle;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        Self {
            app,
            port,
            handle,
            clock,
            events,
            runtime: Some(runtime),
        }
    }

    pub fn authority_headers(&self, incarnation: &str) -> axum::http::HeaderMap {
        let mut peer = self.handle.channel.snapshot.clone();
        std::mem::swap(&mut peer.local, &mut peer.peer);
        peer.local_role = FederationRole::Central;
        let authority =
            serde_json::to_string(&FederationRequestAuthority::outgoing(&peer)).unwrap();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            FEDERATION_AUTHORITY_HEADER,
            HeaderValue::from_str(&authority).unwrap(),
        );
        headers.insert(
            FEDERATION_INCARNATION_HEADER,
            HeaderValue::from_str(incarnation).unwrap(),
        );
        headers
    }

    pub fn info(&self) -> PeerInfo {
        let snapshot = &self.handle.channel.snapshot;
        PeerInfo {
            port: self.port,
            central_id: snapshot.central.host_id.clone(),
            satellite_id: snapshot.local.host_id.clone(),
            enrollment: snapshot.enrollment_generation,
            epoch: snapshot.central.host_generation,
            boot: self.app.server_instance_id.clone(),
        }
    }

    pub async fn finish(mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown().await;
        }
    }

    pub async fn event(&mut self, expected: &'static str, number: u64) {
        tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
            while let Some(event) = self.events.recv().await {
                if event == (expected, number) {
                    return;
                }
            }
            panic!("control observer closed before expected phase");
        })
        .await
        .expect("control phase deadline");
    }
}

impl Drop for Satellite {
    fn drop(&mut self) {
        if let Some(runtime) = &self.runtime {
            runtime.stop.send_replace(true);
        }
    }
}

pub(super) struct Control {
    pub io: tokio::net::TcpStream,
    pub session: ReferenceSession,
    pub hello_c: Vec<u8>,
}

impl Control {
    pub async fn start(
        peer: &Satellite,
        secret: char,
        change: impl FnOnce(&mut Vec<Vec<u8>>),
    ) -> Result<Self, String> {
        Self::start_paused(peer, secret, change, None).await
    }

    pub async fn start_paused(
        peer: &Satellite,
        secret: char,
        change: impl FnOnce(&mut Vec<Vec<u8>>),
        pause: Option<(tokio::sync::oneshot::Sender<()>, Arc<tokio::sync::Notify>)>,
    ) -> Result<Self, String> {
        Self::open(&peer.info(), secret, change, pause).await
    }

    pub async fn open(
        info: &PeerInfo,
        secret: char,
        change: impl FnOnce(&mut Vec<Vec<u8>>),
        pause: Option<(tokio::sync::oneshot::Sender<()>, Arc<tokio::sync::Notify>)>,
    ) -> Result<Self, String> {
        let mut io = tokio::net::TcpStream::connect(("127.0.0.1", info.port))
            .await
            .map_err(|_| "control connection refused")?;
        io.write_all(CONTROL_PREFACE)
            .await
            .map_err(|_| "control preface refused")?;
        let hello = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, read_frame(&mut io))
            .await
            .map_err(|_| "control hello timed out")??;
        let fields = decode(&hello, b"termal-hello-s-v1", 4)?;
        if fields[0] != info.satellite_id.as_bytes()
            || fields[1] != info.enrollment.to_be_bytes()
            || fields[2] != info.boot.as_bytes()
            || fields[3].len() != 32
        {
            return Err("satellite hello basis mismatch".into());
        }
        if let Some((entered, release)) = pause {
            let _ = entered.send(());
            tokio::time::timeout(phase_sync::DEADLOCK_GUARD, release.notified())
                .await
                .map_err(|_| "controlled handshake release missing")?;
        }
        let incarnation = Uuid::new_v4();
        let mut transcript_fields = vec![
            info.central_id.as_bytes().to_vec(),
            info.satellite_id.as_bytes().to_vec(),
            info.enrollment.to_be_bytes().to_vec(),
            info.epoch.to_be_bytes().to_vec(),
            Uuid::new_v4().to_string().into_bytes(),
            fields[2].clone(),
            incarnation.as_bytes().to_vec(),
            format!("127.0.0.1:{}", info.port).into_bytes(),
        ];
        change(&mut transcript_fields);
        let transcript = encode(b"termal-transcript-v1", &transcript_fields);
        let nc = joined(&[Uuid::new_v4().as_bytes(), Uuid::new_v4().as_bytes()]);
        let tag = mac(
            &pair_key(secret),
            &joined(&[b"termal-link-attest-c2s-v1", &transcript, &fields[3], &nc]),
        );
        let hello_c = encode(b"termal-hello-c-v1", &[transcript.clone(), nc.clone(), tag]);
        write_frame(&mut io, &hello_c).await?;
        let proof = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, read_frame(&mut io))
            .await
            .map_err(|_| "control proof timed out")??;
        let proof = decode(&proof, b"termal-hello-proof-v1", 1)?;
        verify(
            &pair_key(secret),
            &joined(&[b"termal-link-attest-s2c-v1", &transcript, &nc, &fields[3]]),
            &proof[0],
        )?;
        let session =
            ReferenceSession::derive(transcript, incarnation.to_string(), &fields[3], &nc, secret);
        let mut control = Self {
            io,
            session,
            hello_c,
        };
        control.heartbeat(1, &[7; 16]).await?;
        Ok(control)
    }

    pub async fn heartbeat(&mut self, sequence: u64, challenge: &[u8]) -> Result<Vec<u8>, String> {
        write_frame(&mut self.io, &self.session.heartbeat(sequence, challenge)).await?;
        let response = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, read_frame(&mut self.io))
            .await
            .map_err(|_| "heartbeat timed out")??;
        self.session
            .check_heartbeat(&response, sequence, challenge)?;
        Ok(response)
    }
}

pub(super) async fn established() -> (Satellite, Control) {
    let satellite = Satellite::new().await;
    match Control::start(&satellite, 'a', |_| {}).await {
        Ok(control) => (satellite, control),
        Err(error) => {
            satellite.finish().await;
            panic!("real protocol establishment required: {error}");
        }
    }
}
