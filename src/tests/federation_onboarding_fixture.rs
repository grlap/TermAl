// Owns disposable central/satellite hosts joined by L1 loopback fake forwards.
// Does not simulate a verified result, launch SSH or open live stores. New O1
// fixture runs the real authenticated router, handshake and registry writer.
use super::*;

pub(super) fn profile() -> FederationExecutionProfile {
    FederationExecutionProfile {
        protocol_version: 1,
        schema_version: SQLITE_SCHEMA_VERSION.to_owned(),
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
    stops: Vec<tokio::sync::watch::Sender<bool>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
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
        let central_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let satellite_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let central_port = central_listener.local_addr().unwrap().port();
        let satellite_port = satellite_listener.local_addr().unwrap().port();
        let satellite = federation_registry::domain_state();
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
        record.profile = profile();
        record.endpoint.satellite_federation_port = satellite_port;
        record.endpoint.satellite_reverse_forward_port = central_port;
        central.enroll_federation_peer(record.clone()).unwrap();
        let id = remote.host_id.clone();
        record.identity = local;
        satellite.enroll_federation_peer(record).unwrap();
        let cap = || FederationPairCapability::new("a".repeat(64)).unwrap();
        let channel = FederationChannel::for_app(
            central.federation_peer_snapshot(&id).unwrap(),
            cap(),
            central.clone(),
        );
        let satellite_channel = FederationChannel::for_app(
            satellite.federation_peer_snapshots().unwrap().remove(0),
            cap(),
            satellite.clone(),
        );
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
        for (listener, router) in [
            (central_listener, central_router),
            (satellite_listener, satellite_router),
        ] {
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
                channel,
                rpc: rpc.clone(),
            }),
        );
        let lease = rpc.open_link().await.unwrap();
        Self {
            central,
            satellite,
            id,
            rpc,
            lease,
            satellite_channel,
            stops,
            tasks,
        }
    }

    pub(super) async fn verify(&self) -> Result<FederationOnboardingReport, ApiError> {
        verify_federation_satellite(self.central.clone(), self.id.clone()).await
    }

    pub(super) async fn finish(mut self) {
        self.lease.channel.invalidate();
        self.satellite_channel.invalidate();
        self.central
            .federation_peer_handles
            .lock()
            .unwrap()
            .remove(&self.id);
        for stop in &self.stops {
            stop.send_replace(true);
        }
        for task in self.tasks.drain(..) {
            task.await.unwrap();
        }
    }
}

impl Drop for Pair {
    fn drop(&mut self) {
        for stop in &self.stops {
            stop.send_replace(true);
        }
    }
}
