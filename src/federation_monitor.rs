// Owns bounded central handshakes, health probes and exact SSH cleanup.
// Does not change enrollment or retry application commands. Split from federation_peer.rs.

async fn federation_establish_link(
    client: &FederationRpcClient,
    process: &mut dyn FederationSshChild,
    stop: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<FederationLinkLease, ApiError> {
    let startup = async {
        let mut retry = FederationBackoff::default();
        loop {
            if process.child_exited()? {
                return Err(federation_transport_unavailable());
            }
            match client.open_link().await {
                Ok(lease) => return Ok(lease),
                // A remote hold, incomplete bilateral enrollment, or busy
                // handshake can settle without any local configuration change.
                Err(error)
                    if matches!(
                        error.status,
                        StatusCode::SERVICE_UNAVAILABLE
                            | StatusCode::CONFLICT
                            | StatusCode::FORBIDDEN
                            | StatusCode::TOO_MANY_REQUESTS
                            | StatusCode::LOCKED
                    ) => {}
                Err(error) => return Err(error),
            }
            tokio::time::sleep(retry.next_delay()).await;
            retry.record_failure();
        }
    };
    tokio::select! {
        _ = federation_wait_stop(stop) => Err(federation_transport_unavailable()),
        result = tokio::time::timeout(REMOTE_STARTUP_TIMEOUT, startup) => result.map_err(|_| federation_transport_unavailable())?,
    }
}

async fn federation_central_monitor(
    snapshot: &FederationPeerSnapshot,
    client: &FederationRpcClient,
    known_hosts: PathBuf,
    forward_port: u16,
    stop: &mut tokio::sync::watch::Receiver<bool>,
    retry: &mut FederationBackoff,
    server: &tokio::task::JoinHandle<std::io::Result<()>>,
    io: &FederationRuntimeIo,
    attempt: &mut FederationAttemptState,
) -> Result<(), ApiError> {
    let process_snapshot = snapshot.clone();
    let start = io.ssh.clone();
    let mut process = federation_os_operation(
        move || start(&process_snapshot, forward_port, &known_hosts),
        Some(stop),
        attempt.unresolved.clone(),
        io.operation_timeout,
    )
    .await?;
    let outcome = async {
        let mut lease = federation_establish_link(client, process.as_mut(), stop).await?;
        loop {
            tokio::select! {
                _ = federation_wait_stop(stop) => return Ok(()),
                _ = tokio::time::sleep(io.probe_interval) => {}
            }
            if server.is_finished() || process.child_exited()? {
                return Err(federation_transport_unavailable());
            }
            if client.channel.current_authority().is_err() {
                continue;
            }
            let health = tokio::select! {
                _ = federation_wait_stop(stop) => return Ok(()),
                result = client.health(&lease) => result,
            };
            match health {
                Ok(()) => retry.reset(),
                // Admission pressure is not evidence that the transport died.
                // Retain this lease without stamping success or resetting retry.
                Err(error) if matches!(error.status,
                    StatusCode::LOCKED | StatusCode::TOO_MANY_REQUESTS) => {}
                Err(error) if error.status == StatusCode::CONFLICT => {
                    if client.channel.current_authority().is_err() {
                        continue;
                    }
                    // The local authority is current, so a peer-originated
                    // stale link must not become a permanent parked worker.
                    client.channel.invalidate_if_current(&lease);
                    let delay = retry.next_delay();
                    retry.record_failure();
                    tokio::select! {
                        _ = federation_wait_stop(stop) => return Ok(()),
                        _ = tokio::time::sleep(delay) => {}
                    }
                    lease = federation_establish_link(client, process.as_mut(), stop).await?;
                }
                Err(error) => return Err(error),
            }
        }
    }
    .await;
    client.channel.invalidate();
    // Cleanup is given its own bound even when stop is already true. An
    // unresolved owned child quarantines the peer rather than being replaced.
    if let Err(error) = federation_os_operation(
        move || process.shutdown(),
        None,
        attempt.unresolved.clone(),
        io.operation_timeout,
    )
    .await
    {
        attempt.unresolved.store(true, Ordering::SeqCst);
        return Err(error);
    }
    outcome
}

async fn federation_satellite_monitor(
    client: &FederationRpcClient,
    stop: &mut tokio::sync::watch::Receiver<bool>,
    retry: &mut FederationBackoff,
    server: &tokio::task::JoinHandle<std::io::Result<()>>,
    io: &FederationRuntimeIo,
) -> Result<(), ApiError> {
    loop {
        tokio::select! {
            _ = federation_wait_stop(stop) => return Ok(()),
            _ = tokio::time::sleep(io.probe_interval) => {}
        }
        if server.is_finished() {
            return Err(federation_transport_unavailable());
        }
        if client.channel.current_authority().is_err() {
            continue;
        }
        if let Ok(lease) = client.channel.current_lease() {
            let outcome = tokio::select! {
                _ = federation_wait_stop(stop) => return Ok(()),
                result = client.health(&lease) => result,
            };
            match outcome {
                Ok(()) => retry.reset(),
                Err(error) if matches!(error.status,
                    StatusCode::LOCKED | StatusCode::TOO_MANY_REQUESTS) => {}
                Err(_) => {
                    if client.channel.current_authority().is_err() {
                        continue;
                    }
                    // Availability is withdrawn immediately, retaining the
                    // listener so C can perform a fresh challenge handshake.
                    client.channel.invalidate_if_current(&lease);
                    retry.record_failure();
                }
            }
        }
    }
}
