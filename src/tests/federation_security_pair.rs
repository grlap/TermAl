// Owns two real AppState runtimes connected at the fake owned-SSH boundary.
// Does not synthesize proof success or use the OS keyring. Split from runtime fixtures.

use super::*;
use relay::{Relay, TestChild};

pub(super) struct Pair {
    pub central_peer: Arc<FederationPeerHandle>,
    pub satellite_peer: Arc<FederationPeerHandle>,
    pub relay: Arc<Relay>,
    pub central_clock: FederationControlClock,
    control_ready: Arc<AtomicBool>,
    central_events: tokio::sync::mpsc::UnboundedReceiver<(&'static str, u64, Instant)>,
    runtimes: Vec<FederationRuntime>,
}

impl Pair {
    pub async fn new() -> Self {
        Self::with_central_control_timeout(Duration::from_secs(45)).await
    }

    pub async fn with_central_control_timeout(control_timeout: Duration) -> Self {
        Self::with_options(control_timeout, None, None).await
    }

    pub async fn with_starter(
        starter: FederationSshStarter,
        prepare: Arc<dyn Fn(&FederationPeerSnapshot) -> Result<u16, ApiError> + Send + Sync>,
    ) -> Self {
        Self::with_options(Duration::from_secs(45), Some(starter), Some(prepare)).await
    }

    async fn with_options(
        control_timeout: Duration,
        starter: Option<FederationSshStarter>,
        prepare: Option<
            Arc<dyn Fn(&FederationPeerSnapshot) -> Result<u16, ApiError> + Send + Sync>,
        >,
    ) -> Self {
        let central = super::super::federation_registry::central();
        let satellite = super::super::federation_registry::domain_state();
        let c_id = central
            .inner
            .lock()
            .unwrap()
            .federation
            .local
            .clone()
            .unwrap();
        let s_id = satellite
            .inner
            .lock()
            .unwrap()
            .federation
            .local
            .clone()
            .unwrap();
        let c_port = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let s_port = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let c_number = c_port.local_addr().unwrap().port();
        let s_number = s_port.local_addr().unwrap().port();
        let mut s_record = super::super::federation_registry::peer(c_number);
        s_record.identity = s_id.clone();
        s_record.endpoint.satellite_federation_port = s_number;
        // The fake reverse forward is the already bound central endpoint.
        s_record.endpoint.satellite_reverse_forward_port = c_number;
        let mut c_record = s_record.clone();
        c_record.identity = c_id.clone();
        satellite
            .configure_federation(FederationSettings {
                enabled: true,
                role: FederationRole::Satellite,
                central: Some(c_id.clone()),
            })
            .unwrap();
        satellite.enroll_federation_peer(c_record).unwrap();
        central.enroll_federation_peer(s_record).unwrap();
        let relay = Arc::new(Relay::default());
        let runtime_handle = tokio::runtime::Handle::current();
        let start_relay = relay.clone();
        let forward_listener = Arc::new(Mutex::new(Some(
            std::net::TcpListener::bind("127.0.0.1:0").unwrap(),
        )));
        let reserved_forward = forward_listener.clone();
        let attempt_relay = relay.clone();
        let satellite_clock = FederationControlClock::scripted();
        let (central_events_tx, central_events) = tokio::sync::mpsc::unbounded_channel();
        let io = FederationRuntimeIo {
            forward_listener: Some(forward_listener),
            credentials: Arc::new(|_| {
                Ok(FederationTransportCredentials {
                    capability: FederationPairCapability::new("a".repeat(64)).unwrap(),
                    known_hosts: Some(PathBuf::from("unused-test-pin")),
                    material_fingerprint: [1; 32],
                })
            }),
            ssh: Arc::new(move |_, port, _| {
                let socket = reserved_forward
                    .lock()
                    .unwrap()
                    .take()
                    .expect("reserved forward socket");
                assert_eq!(socket.local_addr().unwrap().port(), port);
                TestChild::start(&runtime_handle, start_relay.clone(), socket, s_number)
            }),
            reconcile_interval: Duration::from_millis(20),
            // Explicit RPCs drive the assertions; control heartbeats retain
            // their production deadline independently of legacy reverse probes.
            probe_interval: Duration::from_secs(3600),
            attempt_finished: Arc::new(move |result| {
                if result.as_ref().err().is_some_and(|error| {
                    error.status == StatusCode::FORBIDDEN
                        && error.message == "unproven federation transport"
                }) {
                    attempt_relay.unproven.store(true, Ordering::SeqCst);
                }
                attempt_relay.attempts.fetch_add(1, Ordering::SeqCst);
                attempt_relay.changed.notify_waiters();
            }),
            control_clock: satellite_clock.clone(),
            ..FederationRuntimeIo::default()
        };
        drop(c_port);
        drop(s_port);
        let s_runtime = start_federation_runtime_with_io(satellite.clone(), io.clone());
        let satellite_peer = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
            loop {
                if let Some(peer) = s_runtime.peer(&c_id.host_id) {
                    break peer;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut central_io = io;
        if let Some(starter) = starter {
            central_io.ssh = starter;
            central_io.prepare_forward = prepare;
        }
        let central_clock = FederationControlClock::scripted();
        let observer_clock = central_clock.clone();
        let control_ready = Arc::new(AtomicBool::new(false));
        let observed_ready = control_ready.clone();
        central_io.control_clock = central_clock.clone();
        central_io.control_observed = Arc::new(move |event, number| {
            if event == "replyAccepted" {
                observed_ready.store(true, Ordering::SeqCst);
            } else if event.starts_with("control") {
                observed_ready.store(false, Ordering::SeqCst);
            }
            let _ = central_events_tx.send((event, number, observer_clock.now()));
        });
        central_io.control_timeout = control_timeout;
        central_io.control_interval = Duration::from_secs(1);
        let c_runtime = start_federation_runtime_with_io(central.clone(), central_io);
        let central_peer = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
            loop {
                if let Some(peer) = c_runtime.peer(&s_id.host_id) {
                    break peer;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        Self {
            central_peer,
            satellite_peer,
            relay,
            central_clock,
            control_ready,
            central_events,
            runtimes: vec![c_runtime, s_runtime],
        }
    }

    pub async fn established() -> Self {
        let pair = Self::new().await;
        pair.wait_established().await
    }

    pub async fn wait_established(self) -> Self {
        let pair = self;
        let ready = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
            loop {
                // Observe the real control FSM's first fresh authenticated
                // reply before a test captures or replays a readiness frame.
                if !pair.control_ready.load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                    continue;
                }
                if let (Ok(c), Ok(_)) = (
                    pair.central_peer.channel.current_lease(),
                    pair.satellite_peer.channel.current_lease(),
                ) {
                    pair.central_peer.rpc.health(&c).await.unwrap();
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        let secure = {
            let requests = pair.relay.requests.lock().unwrap();
            !requests.is_empty()
                && requests
                    .iter()
                    .all(|request| request.counter.is_some() && !request.key_seen)
        };
        if ready.is_err() || !secure {
            pair.finish().await;
            panic!("two production runtimes must establish without a bearer or unsigned RPC");
        }
        pair
    }

    pub async fn finish(mut self) {
        self.relay.release_http.notify_waiters();
        for runtime in self.runtimes.drain(..) {
            runtime.shutdown().await;
        }
    }

    pub async fn central_event(&mut self, expected: &'static str, number: u64) {
        tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
            while let Some(event) = self.central_events.recv().await {
                eprintln!("central control phase={event:?}, expected=({expected},{number}), clock={:?}", self.central_clock.now());
                if (event.0, event.1) == (expected, number) {
                    return;
                }
                assert!(!event.0.starts_with("control"), "central terminated before expected phase: {event:?}; expected=({expected},{number})");
            }
            panic!("central control observer closed");
        })
        .await
        .expect("central control phase deadline");
    }

    pub async fn central_next_event(&mut self) -> (&'static str, u64, Instant) {
        let event = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, self.central_events.recv())
            .await
            .expect("central control observer phase deadline")
            .expect("central control observer closed");
        eprintln!(
            "central next phase={event:?}, clock={:?}",
            self.central_clock.now()
        );
        event
    }
}

impl Drop for Pair {
    fn drop(&mut self) {
        self.relay.release_http.notify_waiters();
        for runtime in &self.runtimes {
            runtime.stop.send_replace(true);
        }
    }
}
