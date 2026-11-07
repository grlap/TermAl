// Owns the OS boundary and timing used by the federation worker.
// Does not select peers or grant authority. Split from federation_peer.rs.

// The owned process lends exactly one control stream. A TCP forward's
// liveness cannot substitute for this ownership boundary.
trait FederationControlStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> FederationControlStream for T {}
type FederationControlIo = Box<dyn FederationControlStream>;

trait FederationSshChild: Send {
    fn prepare_control(&mut self) -> Result<(), ApiError> { Ok(()) }
    #[cfg_attr(not(test), allow(dead_code))]
    fn take_control_stream(&mut self) -> Result<FederationControlIo, ApiError> {
        Err(ApiError::from_status(
            StatusCode::FORBIDDEN,
            "unproven federation transport",
        ))
    }
    fn child_exited(&mut self) -> Result<bool, ApiError>;
    fn diagnostics(&self) -> Option<FederationSshDiagnostics> { None }
    fn shutdown(&mut self) -> Result<(), ApiError>;
}

impl FederationSshChild for FederationSshProcess {
    fn prepare_control(&mut self) -> Result<(), ApiError> { self.attach_stdio(); Ok(()) }
    fn diagnostics(&self) -> Option<FederationSshDiagnostics> { Some(self.diagnostic.clone()) }
    fn take_control_stream(&mut self) -> Result<FederationControlIo, ApiError> {
        FederationSshProcess::take_control_stream(self)
    }
    fn child_exited(&mut self) -> Result<bool, ApiError> {
        FederationSshProcess::child_exited(self)
    }

    fn shutdown(&mut self) -> Result<(), ApiError> {
        self.reap()
    }
}

type FederationCredentialReader = Arc<
    dyn Fn(&FederationPeerSnapshot) -> Result<FederationTransportCredentials, ApiError>
        + Send
        + Sync,
>;
type FederationSshStarter = Arc<
    dyn Fn(&FederationPeerSnapshot, u16, &FsPath, &mut Option<Box<dyn FederationSshChild>>) -> Result<(), ApiError>
        + Send
        + Sync,
>;

#[derive(Clone)]
struct FederationRuntimeIo {
    #[cfg(test)]
    prepare_forward:
        Option<Arc<dyn Fn(&FederationPeerSnapshot) -> Result<u16, ApiError> + Send + Sync>>,
    #[cfg(test)]
    forward_listener: Option<Arc<Mutex<Option<std::net::TcpListener>>>>,
    credentials: FederationCredentialReader,
    ssh: FederationSshStarter,
    reconcile_interval: Duration,
    probe_interval: Duration,
    operation_timeout: Duration,
    credential_reporting_deadline: Duration,
    control_interval: Duration,
    control_timeout: Duration,
    link_startup_timeout: Duration,
    attest_timeout: Duration,
    control_clock: FederationControlClock,
    #[cfg(test)]
    attempt_finished: Arc<dyn Fn(&Result<(), ApiError>) + Send + Sync>,
    #[cfg(test)]
    reconciled: Arc<dyn Fn(usize) + Send + Sync>,
    #[cfg(test)]
    connection_published: Arc<dyn Fn() + Send + Sync>,
    #[cfg(test)]
    control_observed: Arc<dyn Fn(&'static str, u64) + Send + Sync>,
    #[cfg(test)]
    credential_observed: Arc<dyn Fn(&'static str) + Send + Sync>,
}

impl Default for FederationRuntimeIo {
    fn default() -> Self {
        Self {
            #[cfg(test)]
            prepare_forward: None,
            #[cfg(test)]
            forward_listener: None,
            credentials: Arc::new(resolve_federation_credentials),
            ssh: Arc::new(|snapshot, port, known_hosts, owned| {
                start_federation_ssh(snapshot, port, known_hosts, owned)
            }),
            reconcile_interval: Duration::from_secs(15),
            probe_interval: Duration::from_secs(15),
            operation_timeout: Duration::from_secs(5),
            credential_reporting_deadline: Duration::from_secs(5),
            control_interval: Duration::from_secs(15),
            control_timeout: Duration::from_secs(45),
            link_startup_timeout: federation_timeout_from_env(
                "TERMAL_FEDERATION_LINK_STARTUP_TIMEOUT_MS",
                FEDERATION_LINK_STARTUP_TIMEOUT,
            ),
            attest_timeout: federation_timeout_from_env(
                "TERMAL_FEDERATION_ATTEST_TIMEOUT_MS",
                FEDERATION_ATTEST_TIMEOUT,
            ),
            control_clock: FederationControlClock::default(),
            #[cfg(test)]
            attempt_finished: Arc::new(|_| {}),
            #[cfg(test)]
            reconciled: Arc::new(|_| {}),
            #[cfg(test)]
            connection_published: Arc::new(|| {}),
            #[cfg(test)]
            control_observed: Arc::new(|_, _| {}),
            #[cfg(test)]
            credential_observed: Arc::new(|_| {}),
        }
    }
}

#[derive(Default)]
struct FederationAttemptState {
    material: Option<[u8; 32]>,
    material_changed: bool,
    os: Arc<FederationOsAccount>,
    reporter: Option<FederationWorkerReporter>,
}

impl FederationAttemptState {
    fn check_material(&mut self, actual: [u8; 32]) -> Result<(), ApiError> {
        match self.material {
            Some(expected) if expected != actual => {
                self.material_changed = true;
                Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY))
            }
            None => {
                self.material = Some(actual);
                Ok(())
            }
            Some(_) => Ok(()),
        }
    }
}
