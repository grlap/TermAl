// Owns the OS boundary and timing used by the federation worker.
// Does not select peers or grant authority. Split from federation_peer.rs.

use super::*;

// The owned process lends exactly one control stream. A TCP forward's
// liveness cannot substitute for this ownership boundary.
pub(crate) trait FederationControlStream:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send
{
}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> FederationControlStream for T {}
pub(crate) type FederationControlIo = Box<dyn FederationControlStream>;

pub(crate) trait FederationSshChild: Send {
    fn prepare_control(&mut self) -> Result<(), ApiError> {
        Ok(())
    }
    #[cfg_attr(not(test), allow(dead_code))]
    fn take_control_stream(&mut self) -> Result<FederationControlIo, ApiError> {
        Err(ApiError::from_status(
            StatusCode::FORBIDDEN,
            "unproven federation transport",
        ))
    }
    fn child_exited(&mut self) -> Result<bool, ApiError>;
    fn diagnostics(&self) -> Option<FederationSshDiagnostics> {
        None
    }
    fn shutdown(&mut self) -> Result<(), ApiError>;
}

impl FederationSshChild for FederationSshProcess {
    fn prepare_control(&mut self) -> Result<(), ApiError> {
        self.attach_stdio();
        Ok(())
    }
    fn diagnostics(&self) -> Option<FederationSshDiagnostics> {
        Some(self.diagnostic.clone())
    }
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

pub(crate) type FederationCredentialReader = Arc<
    dyn Fn(&FederationPeerSnapshot) -> Result<FederationTransportCredentials, ApiError>
        + Send
        + Sync,
>;
pub(crate) type FederationSshStarter = Arc<
    dyn Fn(
            &FederationPeerSnapshot,
            u16,
            &FsPath,
            &mut Option<Box<dyn FederationSshChild>>,
        ) -> Result<(), ApiError>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub(crate) struct FederationRuntimeIo {
    #[cfg(test)]
    pub(crate) prepare_forward:
        Option<Arc<dyn Fn(&FederationPeerSnapshot) -> Result<u16, ApiError> + Send + Sync>>,
    #[cfg(test)]
    pub(crate) forward_listener: Option<Arc<Mutex<Option<std::net::TcpListener>>>>,
    pub(crate) credentials: FederationCredentialReader,
    pub(crate) ssh: FederationSshStarter,
    pub(crate) reconcile_interval: Duration,
    pub(crate) probe_interval: Duration,
    pub(crate) operation_timeout: Duration,
    pub(crate) credential_reporting_deadline: Duration,
    pub(crate) control_interval: Duration,
    pub(crate) control_timeout: Duration,
    pub(crate) link_startup_timeout: Duration,
    pub(crate) attest_timeout: Duration,
    pub(crate) control_clock: FederationControlClock,
    #[cfg(test)]
    pub(crate) attempt_finished: Arc<dyn Fn(&Result<(), ApiError>) + Send + Sync>,
    #[cfg(test)]
    pub(crate) reconciled: Arc<dyn Fn(usize) + Send + Sync>,
    #[cfg(test)]
    pub(crate) connection_published: Arc<dyn Fn() + Send + Sync>,
    #[cfg(test)]
    pub(crate) control_observed: Arc<dyn Fn(&'static str, u64) + Send + Sync>,
    #[cfg(test)]
    pub(crate) credential_observed: Arc<dyn Fn(&'static str) + Send + Sync>,
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
pub(crate) struct FederationAttemptState {
    pub(crate) material: Option<[u8; 32]>,
    pub(crate) material_changed: bool,
    pub(crate) os: Arc<FederationOsAccount>,
    pub(crate) reporter: Option<FederationWorkerReporter>,
}

impl FederationAttemptState {
    pub(crate) fn check_material(&mut self, actual: [u8; 32]) -> Result<(), ApiError> {
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
