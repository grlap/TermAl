// Owns one enrolled pair's listener/SSH lifetime and authenticated liveness.
// Does not change enrollment or retry application operations. New, not a split.

async fn federation_serve_peer(
    app: &AppState,
    snapshot: &FederationPeerSnapshot,
    basis: Arc<FederationTransportBasis>,
    peers: &FederationPeerHandles,
    stop: &mut tokio::sync::watch::Receiver<bool>,
    retry: &mut FederationBackoff,
    attempt: &mut FederationAttemptState,
    io: &FederationRuntimeIo,
) -> Result<(), ApiError> {
    let credentials_snapshot = snapshot.clone();
    let read = io.credentials.clone();
    let credentials = federation_os_operation(
        move || read(&credentials_snapshot),
        Some(stop),
        attempt.unresolved.clone(),
        io.operation_timeout,
    )
    .await?;
    app.ensure_federation_peer_current(snapshot)?;
    attempt.check_material(credentials.material_fingerprint)?;
    if *stop.borrow() {
        return Ok(());
    }
    let port = match snapshot.local_role {
        FederationRole::Central => snapshot.endpoint.central_federation_port,
        FederationRole::Satellite => snapshot.endpoint.satellite_federation_port,
        FederationRole::Standalone => return Err(federation_stale_link()),
    };
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port)))
        .await
        .map_err(|_| federation_transport_unavailable())?;
    disable_socket_inheritance(&listener);
    let channel = FederationChannel::for_app(snapshot.clone(), credentials.capability, app.clone());
    let mut channel = channel;
    channel.basis = basis;
    #[cfg(test)]
    {
        channel.connection_published = io.connection_published.clone();
    }
    let check_channel = channel.clone();
    let ensure_current: FederationAuthorityCheck =
        Arc::new(move || check_channel.current_authority().map(|_| ()));
    let forward_port = if snapshot.local_role == FederationRole::Central {
        allocate_remote_forward_port()
    } else {
        snapshot.endpoint.satellite_reverse_forward_port
    };
    let client = FederationRpcClient::new(forward_port, channel.clone(), ensure_current.clone())?;
    let (listener_stop, listener_stop_rx) = tokio::sync::watch::channel(false);
    let router = federation_router(channel.clone(), ensure_current);
    let server = tokio::spawn(serve_federation_listener(
        listener,
        router,
        listener_stop_rx,
    ));
    peers
        .lock()
        .expect("federation registry mutex poisoned")
        .insert(
            snapshot.peer.host_id.clone(),
            Arc::new(FederationPeerHandle {
                channel: channel.clone(),
                rpc: client.clone(),
            }),
        );
    let result = if snapshot.local_role == FederationRole::Central {
        match credentials.known_hosts {
            Some(known_hosts) => {
                federation_central_monitor(
                    snapshot,
                    &client,
                    known_hosts,
                    forward_port,
                    stop,
                    retry,
                    &server,
                    io,
                    attempt,
                )
                .await
            }
            None => Err(federation_credential_error()),
        }
    } else {
        federation_satellite_monitor(&client, stop, retry, &server, io).await
    };
    channel.invalidate();
    peers
        .lock()
        .expect("federation registry mutex poisoned")
        .remove(&snapshot.peer.host_id);
    listener_stop.send_replace(true);
    match server.await {
        Ok(Ok(())) => {}
        Ok(Err(_)) => return Err(federation_transport_unavailable()),
        Err(error) => {
            eprintln!("federation listener failed: {error}");
            return Err(federation_transport_unavailable());
        }
    }
    result
}
