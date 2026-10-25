// Owns disposable central/satellite hosts joined by L1 loopback fake forwards.
// Does not simulate a verified result, launch SSH or open live stores. New O1
// fixture runs the real authenticated router, handshake and registry writer.
use super::*;

pub(super) fn profile() -> FederationExecutionProfile {
    FederationExecutionProfile {
        protocol_version: 1,
        schema_version: FEDERATION_PROFILE_SCHEMA_VERSION.to_owned(),
        binary_sha256: "a".repeat(64),
        toolchain_identity: "fake-platform-rustc".to_owned(),
    }
}

pub(super) fn io() -> FederationOnboardingIo {
    FederationOnboardingIo {
        profile: Arc::new(|| Ok(profile())),
        image: Arc::new(image),
        platform: Arc::new(|| {
            Ok(FederationPlatformProbe {
                platform: std::env::consts::OS.to_owned(),
                arch: std::env::consts::ARCH.to_owned(),
                shell_dialect: if cfg!(windows) { "cmd" } else { "posix" }.to_owned(),
                paths_resolved: true,
                child_spawned_and_reaped: true,
                toolchain_identity: profile().toolchain_identity,
            })
        }),
        engram: Arc::new(|_| FederationEngramOnboardingOutcome {
            status: FederationEngramProbeStatus::Fail,
            exit_code: Some(1),
            summary: None,
            reason: "stub external failure".to_owned(),
        }),
    }
}

pub(super) fn image() -> FederationImageObservation {
    FederationImageObservation::Known(FederationImageIdentity {
        sha256: profile().binary_sha256,
        file_identity: if cfg!(windows) {
            FederationImageFileIdentity::Windows {
                volume: 1,
                index: 1,
            }
        } else {
            FederationImageFileIdentity::Unix { dev: 1, inode: 1 }
        },
        size: 1,
        path_at_start: "fake-image-path-at-start".to_owned(),
    })
}

pub(super) struct Pair {
    pub(super) central: AppState,
    pub(super) satellite: AppState,
    pub(super) id: String,
    pub(super) rpc: FederationRpcClient,
    pub(super) lease: FederationLinkLease,
    pub(super) satellite_channel: FederationChannel,
    proof: Option<super::federation_control_fixture::Attested>,
    stops: Vec<tokio::sync::watch::Sender<bool>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    listeners: Vec<std::net::TcpListener>,
    binding_writers: bool,
}

impl Pair {
    pub(super) async fn new(central: AppState, satellite_io: FederationOnboardingIo) -> Self {
        Self::with_central_io(central, satellite_io, io()).await
    }

    pub(super) async fn with_central_io(
        central: AppState,
        satellite_io: FederationOnboardingIo,
        central_io: FederationOnboardingIo,
    ) -> Self {
        Self::initialize(central, satellite_io, central_io, true, false, profile()).await
    }

    pub(super) async fn with_enrolled_profile(
        central: AppState,
        satellite_io: FederationOnboardingIo,
        central_io: FederationOnboardingIo,
        enrolled: FederationExecutionProfile,
    ) -> Self {
        Self::initialize(central, satellite_io, central_io, true, true, enrolled).await
    }

    pub(super) async fn unattested(central: AppState) -> Self {
        Self::initialize(central, io(), io(), false, false, profile()).await
    }

    async fn initialize(
        mut central: AppState,
        satellite_io: FederationOnboardingIo,
        central_io: FederationOnboardingIo,
        attest: bool,
        binding_writers: bool,
        enrolled: FederationExecutionProfile,
    ) -> Self {
        let central_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let satellite_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let central_port = central_listener.local_addr().unwrap().port();
        let satellite_port = satellite_listener.local_addr().unwrap().port();
        let mut satellite = federation_registry::domain_state();
        if binding_writers {
            // INSTALL/placement require a queued writer and a real SQLite
            // fence; the ordinary onboarding fixture uses synchronous fallback.
            super::federation_entrypoints_writer::start_writer(&mut central);
            super::federation_entrypoints_writer::start_writer(&mut satellite);
        }
        let local = central
            .inner
            .lock()
            .unwrap()
            .federation
            .local
            .clone()
            .unwrap();
        let remote = satellite
            .inner
            .lock()
            .unwrap()
            .federation
            .local
            .clone()
            .unwrap();
        satellite
            .configure_federation(FederationSettings {
                enabled: true,
                role: FederationRole::Satellite,
                central: Some(local.clone()),
            })
            .unwrap();
        let mut record = federation_registry::peer(central_port);
        record.identity = remote.clone();
        record.platform = std::env::consts::OS.to_owned();
        record.arch = std::env::consts::ARCH.to_owned();
        record.profile = enrolled;
        record.endpoint.satellite_federation_port = satellite_port;
        record.endpoint.satellite_reverse_forward_port = central_port;
        central.enroll_federation_peer(record.clone()).unwrap();
        record.identity = local;
        satellite.enroll_federation_peer(record).unwrap();
        Self::serve(
            central,
            satellite,
            central_listener,
            satellite_listener,
            satellite_io,
            central_io,
            attest,
            binding_writers,
        )
        .await
    }

    // Resume the same disposable hosts with their retained endpoints. No
    // configure/enroll call may repair the state the restart test is judging.
    pub(super) async fn resume(
        mut central: AppState,
        mut satellite: AppState,
        listeners: [TcpListener; 2],
        satellite_io: FederationOnboardingIo,
        central_io: FederationOnboardingIo,
    ) -> Self {
        let peer = central.federation_peer_snapshots().unwrap().remove(0);
        let [central_listener, satellite_listener] = listeners;
        assert_eq!(
            central_listener.local_addr().unwrap().port(),
            peer.endpoint.central_federation_port
        );
        assert_eq!(
            satellite_listener.local_addr().unwrap().port(),
            peer.endpoint.satellite_federation_port
        );
        super::federation_entrypoints_writer::start_writer(&mut central);
        super::federation_entrypoints_writer::start_writer(&mut satellite);
        Self::serve(
            central,
            satellite,
            central_listener,
            satellite_listener,
            satellite_io,
            central_io,
            true,
            true,
        )
        .await
    }

    async fn serve(
        central: AppState,
        satellite: AppState,
        central_listener: TcpListener,
        satellite_listener: TcpListener,
        satellite_io: FederationOnboardingIo,
        central_io: FederationOnboardingIo,
        attest: bool,
        binding_writers: bool,
    ) -> Self {
        let central_port = central_listener.local_addr().unwrap().port();
        let satellite_port = satellite_listener.local_addr().unwrap().port();
        let id = satellite
            .inner
            .lock()
            .unwrap()
            .federation
            .local
            .as_ref()
            .unwrap()
            .host_id
            .clone();
        let cap = || FederationPairCapability::new("a".repeat(64)).unwrap();
        let mut channel = FederationChannel::for_app(
            central.federation_peer_snapshot(&id).unwrap(),
            cap(),
            central.clone(),
        );
        let mut satellite_channel = FederationChannel::for_app(
            satellite.federation_peer_snapshots().unwrap().remove(0),
            cap(),
            satellite.clone(),
        );
        if attest {
            // These witnesses judge onboarding, not elapsed host scheduling.
            // Deadline tests opt in and advance the same clock explicitly.
            channel.control.clock = FederationControlClock::scripted_io();
            satellite_channel.control.clock = FederationControlClock::scripted_io();
        }
        let check = |channel: FederationChannel| -> FederationAuthorityCheck {
            Arc::new(move || channel.current_authority().map(|_| ()))
        };
        let rpc = FederationRpcClient::new(satellite_port, channel.clone(), check(channel.clone()))
            .unwrap();
        let reverse = FederationRpcClient::new(
            central_port,
            satellite_channel.clone(),
            check(satellite_channel.clone()),
        )
        .unwrap();
        if binding_writers {
            // INSTALL registers the native participant back at C through this
            // runtime handle, just as the production satellite does.
            satellite.federation_peer_handles.lock().unwrap().insert(
                satellite_channel.snapshot.peer.host_id.clone(),
                Arc::new(FederationPeerHandle {
                    channel: satellite_channel.clone(),
                    rpc: reverse.clone(),
                }),
            );
        }
        let central_router = federation_router(channel.clone(), check(channel.clone())).layer(
            axum::Extension(FederationOnboardingHost {
                app: central.clone(),
                rpc: rpc.clone(),
                io: central_io,
            }),
        );
        let satellite_router =
            federation_router(satellite_channel.clone(), check(satellite_channel.clone())).layer(
                axum::Extension(FederationOnboardingHost {
                    app: satellite.clone(),
                    rpc: reverse,
                    io: satellite_io,
                }),
            );
        let mut stops = Vec::new();
        let mut tasks = Vec::new();
        let mut listeners = Vec::new();
        for (listener, router) in [
            (central_listener, central_router),
            (satellite_listener, satellite_router),
        ] {
            // Retain ownership across a simulated host restart. Do not depend
            // on an immediately closed port being bindable on Windows or
            // still free of another parallel fixture's listener.
            let listener = listener.into_std().unwrap();
            listeners.push(listener.try_clone().unwrap());
            let listener = TcpListener::from_std(listener).unwrap();
            let (stop, mut rx) = tokio::sync::watch::channel(false);
            tasks.push(tokio::spawn(async move {
                axum::serve(listener, router)
                    .with_graceful_shutdown(async move { federation_wait_stop(&mut rx).await })
                    .await
                    .unwrap();
            }));
            stops.push(stop);
        }
        central.federation_peer_handles.lock().unwrap().insert(
            id.clone(),
            Arc::new(FederationPeerHandle {
                channel: channel.clone(),
                rpc: rpc.clone(),
            }),
        );
        let proof = if attest {
            Some(super::federation_control_fixture::attest(&channel, &satellite_channel).await)
        } else {
            None
        };
        let lease = if attest {
            rpc.open_link().await.unwrap()
        } else {
            // An empty placeholder is deliberately unusable: the fresh
            // wrong-key case must never fabricate an installed incarnation.
            FederationLinkLease {
                channel,
                incarnation: String::new(),
            }
        };
        Self {
            central,
            satellite,
            id,
            rpc,
            lease,
            satellite_channel,
            proof,
            stops,
            tasks,
            listeners,
            binding_writers,
        }
    }

    pub(super) async fn verify(&self) -> Result<FederationOnboardingReport, ApiError> {
        verify_federation_satellite(self.central.clone(), self.id.clone()).await
    }

    pub(super) async fn reconnect(&mut self) {
        self.lease.channel.invalidate();
        self.satellite_channel.invalidate();
        self.proof = Some(
            super::federation_control_fixture::attest(&self.lease.channel, &self.satellite_channel)
                .await,
        );
        self.lease = self.rpc.open_link().await.unwrap();
    }

    pub(super) async fn suspend(mut self) -> (AppState, AppState, [TcpListener; 2]) {
        self.shutdown().await;
        let listeners: [std::net::TcpListener; 2] =
            std::mem::take(&mut self.listeners).try_into().unwrap();
        let listeners = listeners.map(|listener| TcpListener::from_std(listener).unwrap());
        (self.central.clone(), self.satellite.clone(), listeners)
    }

    pub(super) async fn finish(mut self) {
        self.shutdown().await;
    }

    async fn shutdown(&mut self) {
        self.lease.channel.invalidate();
        self.satellite_channel.invalidate();
        self.central
            .federation_peer_handles
            .lock()
            .unwrap()
            .remove(&self.id);
        self.satellite
            .federation_peer_handles
            .lock()
            .unwrap()
            .remove(&self.satellite_channel.snapshot.peer.host_id);
        for stop in &self.stops {
            stop.send_replace(true);
        }
        for task in self.tasks.drain(..) {
            task.await.unwrap();
        }
        if self.binding_writers {
            self.central.shutdown_persist_blocking();
            self.satellite.shutdown_persist_blocking();
        }
    }
}

impl Drop for Pair {
    fn drop(&mut self) {
        for stop in &self.stops {
            stop.send_replace(true);
        }
        if self.binding_writers {
            self.central
                .federation_peer_handles
                .lock()
                .unwrap()
                .remove(&self.id);
            self.satellite
                .federation_peer_handles
                .lock()
                .unwrap()
                .remove(&self.satellite_channel.snapshot.peer.host_id);
            self.central.shutdown_persist_blocking();
            self.satellite.shutdown_persist_blocking();
        }
    }
}
