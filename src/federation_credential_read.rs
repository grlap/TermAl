// Owns one read-only credential lookup through reporting, adoption or discard.
// SSH start/reap operations retain their separate unknown-outcome fences.

struct FederationCredentialResult {
    // Drop the material before releasing the replacement-worker fence.
    result: Result<FederationTransportCredentials, ApiError>,
    settlement: FederationBasisSettlement,
}

struct FederationCredentialRead {
    receive: tokio::sync::oneshot::Receiver<FederationCredentialResult>,
}

impl FederationCredentialRead {
    fn start(
        snapshot: FederationPeerSnapshot,
        basis: &Arc<FederationTransportBasis>,
        read: FederationCredentialReader,
    ) -> Result<Self, ApiError> {
        Self::start_with(snapshot, basis, read, |job| {
            std::thread::Builder::new()
                .name("federation-credentials".to_owned())
                .spawn(job)
                .map(|_| ())
        })
    }

    fn start_with(
        snapshot: FederationPeerSnapshot,
        basis: &Arc<FederationTransportBasis>,
        read: FederationCredentialReader,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
    ) -> Result<Self, ApiError> {
        // Acquire BEFORE spawn. The result envelope, not a successful send,
        // owns settlement while queued. Receiver loss destroys it on this
        // thread; panic/spawn failure also release the guard by unwinding.
        let settlement = basis.begin_settlement();
        let (send, receive) = tokio::sync::oneshot::channel();
        spawn(Box::new(move || {
            let result = read(&snapshot);
            let _ = send.send(FederationCredentialResult { result, settlement });
        }))
        .map_err(|_| federation_credential_error())?;
        Ok(Self { receive })
    }

    async fn wait(
        mut self,
        stop: &mut tokio::sync::watch::Receiver<bool>,
        reporting_deadline: Duration,
        mut observe: impl FnMut(&'static str),
    ) -> Result<Option<FederationCredentialResult>, ApiError> {
        let deadline = tokio::time::sleep(reporting_deadline);
        tokio::pin!(deadline);
        let mut pending = false;
        loop {
            tokio::select! {
                // Stop wins a simultaneously queued completion. Dropping the
                // receiver never joins the uninterruptible reader thread.
                biased;
                _ = federation_wait_stop(stop) => {
                    observe("stopped; original result discarded when settled");
                    return Ok(None);
                },
                result = &mut self.receive => return match result {
                    Ok(completed) => {
                        if completed.result.is_err() {
                            observe("completed failure");
                        } else if pending {
                            observe("completed after pending; awaiting basis validation");
                        }
                        Ok(Some(completed))
                    },
                    Err(_) => {
                        observe("worker result lost; no credentials adopted");
                        Err(federation_credential_error())
                    },
                },
                _ = &mut deadline, if !pending => {
                    pending = true;
                    observe("pending; reporting deadline reached, original read retained");
                },
            }
        }
    }
}

fn federation_credential_read_stopped(
    app: &AppState,
    stop: &tokio::sync::watch::Receiver<bool>,
) -> bool {
    *stop.borrow()
        || stop.has_changed().is_err()
        || *app.subscribe_shutdown_signal().borrow()
        || app.federation_executor_wake_signal.is_stopping()
}

fn federation_validate_credential_basis(
    app: &AppState,
    snapshot: &FederationPeerSnapshot,
    basis: &Arc<FederationTransportBasis>,
    stop: &tokio::sync::watch::Receiver<bool>,
) -> Result<(), ApiError> {
    if federation_credential_read_stopped(app, stop)
        || !basis
            .read()
            .expect("federation basis lock poisoned")
            .same_transport_as(snapshot)
    {
        return Err(federation_stale_link());
    }
    app.ensure_federation_peer_current(snapshot)
}
