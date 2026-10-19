// Owns bounded central handshakes, health probes and exact SSH cleanup.
// Does not change enrollment or retry application commands. Split from federation_peer.rs.

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
    let started = tokio::time::Instant::now();
    let mut process = federation_os_operation(
        move || start(&process_snapshot, forward_port, &known_hosts),
        Some(stop),
        attempt.unresolved.clone(),
        io.operation_timeout,
    )
    .await
    .map_err(|error| {
        federation_attestation_error(
            &client.channel,
            None,
            "startup",
            "sshStartFailed",
            error,
            false,
        )
    })?;
    let outcome = async {
        let mut control = process.take_control_stream().map_err(|error| federation_attestation_error(
            &client.channel, None, "startup", "controlStreamUnavailable", error, false))?;
        let (context, guard) = tokio::select! {
            _ = federation_wait_stop(stop) => return Ok(()),
            result = federation_attest_central_started(&client.channel, &mut control, started) => result?,
        };
        let (control_stop, control_stop_rx) = tokio::sync::watch::channel(false);
        let (ready, ready_rx) = tokio::sync::oneshot::channel();
        let mut task = tokio::spawn(federation_control_central(client.channel.clone(), control, context, guard, control_stop_rx, ready));
        let mut joined = false;
        let outcome = async {
            tokio::select! {
                _ = federation_wait_stop(stop) => return Ok(()),
                result = tokio::time::timeout(Duration::from_secs(5), ready_rx) => {
                    result.map_err(|_| federation_transport_unavailable())?.map_err(|_| federation_transport_unavailable())?;
                },
            }
            loop {
                tokio::select! {
                    _ = federation_wait_stop(stop) => return Ok(()),
                    result = &mut task => {
                        joined = true;
                        return result.map_err(|_| federation_transport_unavailable())?;
                    },
                    _ = tokio::time::sleep(io.probe_interval) => {},
                }
                if server.is_finished() || process.child_exited()? { return Err(federation_transport_unavailable()); }
                if let Err(error) = client.channel.current_authority() {
                    if error.status == StatusCode::LOCKED { continue; }
                    return Err(error);
                }
                let lease = client.channel.current_lease()?;
                match client.health(&lease).await {
                    Ok(()) => retry.reset(),
                    Err(error) if matches!(error.status, StatusCode::LOCKED | StatusCode::TOO_MANY_REQUESTS) => {},
                    Err(error) => return Err(error),
                }
            }
        }.await;
        control_stop.send_replace(true);
        if !joined {
            if let Err(error) = task.await { eprintln!("federation control task failed: {error}"); }
        }
        outcome
    }.await;
    client.channel.invalidate();
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
                Err(error)
                    if matches!(
                        error.status,
                        StatusCode::LOCKED | StatusCode::TOO_MANY_REQUESTS
                    ) => {}
                Err(_) => {
                    if client.channel.current_authority().is_err() {
                        continue;
                    }
                    // Availability is withdrawn immediately, retaining the
                    // listener so C can establish a fresh owned control stream.
                    client.channel.invalidate_if_current(&lease);
                    retry.record_failure();
                }
            }
        }
    }
}
