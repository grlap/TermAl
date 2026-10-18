// Owns the OS boundary and timing used by the federation worker.
// Does not select peers or grant authority. Split from federation_peer.rs.

trait FederationSshChild: Send {
    fn child_exited(&mut self) -> Result<bool, ApiError>;
    fn shutdown(self: Box<Self>) -> Result<(), ApiError>;
}

impl FederationSshChild for FederationSshProcess {
    fn child_exited(&mut self) -> Result<bool, ApiError> {
        FederationSshProcess::child_exited(self)
    }

    fn shutdown(self: Box<Self>) -> Result<(), ApiError> {
        FederationSshProcess::shutdown(*self)
    }
}

type FederationCredentialReader = Arc<
    dyn Fn(&FederationPeerSnapshot) -> Result<FederationTransportCredentials, ApiError>
        + Send
        + Sync,
>;
type FederationSshStarter = Arc<
    dyn Fn(&FederationPeerSnapshot, u16, &FsPath) -> Result<Box<dyn FederationSshChild>, ApiError>
        + Send
        + Sync,
>;

#[derive(Clone)]
struct FederationRuntimeIo {
    credentials: FederationCredentialReader,
    ssh: FederationSshStarter,
    reconcile_interval: Duration,
    probe_interval: Duration,
    operation_timeout: Duration,
    #[cfg(test)]
    attempt_finished: Arc<dyn Fn(&Result<(), ApiError>) + Send + Sync>,
    #[cfg(test)]
    reconciled: Arc<dyn Fn(usize) + Send + Sync>,
    #[cfg(test)]
    connection_published: Arc<dyn Fn() + Send + Sync>,
}

impl Default for FederationRuntimeIo {
    fn default() -> Self {
        Self {
            credentials: Arc::new(resolve_federation_credentials),
            ssh: Arc::new(|snapshot, port, known_hosts| {
                start_federation_ssh(snapshot, port, known_hosts)
                    .map(|child| Box::new(child) as Box<dyn FederationSshChild>)
            }),
            reconcile_interval: Duration::from_secs(15),
            probe_interval: Duration::from_secs(15),
            operation_timeout: Duration::from_secs(5),
            #[cfg(test)]
            attempt_finished: Arc::new(|_| {}),
            #[cfg(test)]
            reconciled: Arc::new(|_| {}),
            #[cfg(test)]
            connection_published: Arc::new(|| {}),
        }
    }
}

#[derive(Default)]
struct FederationAttemptState {
    material: Option<[u8; 32]>,
    material_changed: bool,
    unresolved: Arc<AtomicBool>,
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

// A dedicated owned thread prevents an uninterruptible OS keyring/reap call
// from keeping Tokio's blocking pool (and therefore host shutdown) alive.
// Timeout/cancellation fences the peer until this runtime ends; no replacement
// child may start while the previous operation's outcome is unresolved.
async fn federation_os_operation<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
    stop: Option<&mut tokio::sync::watch::Receiver<bool>>,
    unresolved: Arc<AtomicBool>,
    timeout: Duration,
) -> Result<T, ApiError> {
    let (send, receive) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("federation-io".to_owned())
        .spawn(move || {
            // A late returned child stays owned: send failure drops its RAII
            // process guard here, where it still performs its exact cleanup.
            let _ = send.send(operation());
        })
        .map_err(|_| federation_transport_unavailable())?;
    let stopped = async {
        if let Some(stop) = stop {
            federation_wait_stop(stop).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    tokio::select! {
        result = receive => result.map_err(|_| {
            unresolved.store(true, Ordering::SeqCst);
            federation_transport_unavailable()
        })?,
        _ = stopped => {
            unresolved.store(true, Ordering::SeqCst);
            Err(federation_transport_unavailable())
        },
        _ = tokio::time::sleep(timeout) => {
            unresolved.store(true, Ordering::SeqCst);
            Err(federation_transport_unavailable())
        },
    }
}
