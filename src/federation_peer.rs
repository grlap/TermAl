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
    federation_validate_credential_basis(app, snapshot, &basis, stop)?;
    let read = FederationCredentialRead::start(snapshot.clone(), &basis, io.credentials.clone())
        .inspect_err(|_| {
            eprintln!(
                "federation peer {} credential read: could not start",
                snapshot.peer.host_id
            );
        })?;
    // Stay inside this attempt while pending: the worker must consume its
    // own result even though its basis settlement guard is still held.
    let Some(completed) = read
        .wait(stop, io.credential_reporting_deadline, |state| {
            eprintln!(
                "federation peer {} credential read: {state}",
                snapshot.peer.host_id
            );
            #[cfg(test)]
            (io.credential_observed)(state);
        })
        .await?
    else {
        return Ok(());
    };
    let FederationCredentialResult { result, settlement } = completed;
    let credentials = result?;
    federation_validate_credential_basis(app, snapshot, &basis, stop)?;
    attempt.check_material(credentials.material_fingerprint)?;
    let port = match snapshot.local_role {
        FederationRole::Central => snapshot.endpoint.central_federation_port,
        FederationRole::Satellite => snapshot.endpoint.satellite_federation_port,
        FederationRole::Standalone => return Err(federation_stale_link()),
    };
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port)))
        .await
        .map_err(|_| federation_transport_unavailable())?;
    federation_validate_credential_basis(app, snapshot, &basis, stop)?;
    attempt.check_material(credentials.material_fingerprint)?;
    disable_socket_inheritance(&listener);
    let channel = FederationChannel::for_app(snapshot.clone(), credentials.capability, app.clone());
    let mut channel = channel;
    channel.basis = basis.clone();
    channel.control = FederationControlSettings::from_io(io);
    #[cfg(test)]
    {
        channel.connection_published = io.connection_published.clone();
    }
    let check_channel = channel.clone();
    let ensure_current: FederationAuthorityCheck =
        Arc::new(move || check_channel.current_authority().map(|_| ()));
    let forward_port = if snapshot.local_role == FederationRole::Central {
        #[cfg(test)]
        if let Some(prepare) = &io.prepare_forward {
            prepare(snapshot)?
        } else if let Some(reserved) = &io.forward_listener {
            let mut socket = reserved.lock().unwrap();
            if socket.is_none() {
                *socket = Some(
                    std::net::TcpListener::bind("127.0.0.1:0")
                        .map_err(|_| federation_transport_unavailable())?,
                );
            }
            socket
                .as_ref()
                .unwrap()
                .local_addr()
                .map_err(|_| federation_transport_unavailable())?
                .port()
        } else {
            allocate_remote_forward_port()
        }
        #[cfg(not(test))]
        allocate_remote_forward_port()
    } else {
        snapshot.endpoint.satellite_reverse_forward_port
    };
    let client = FederationRpcClient::new(forward_port, channel.clone(), ensure_current.clone())?;
    let (listener_stop, listener_stop_rx) = tokio::sync::watch::channel(false);
    let directory = app
        .persistence_path
        .parent()
        .ok_or_else(federation_transport_unavailable)?
        .to_path_buf();
    let router = federation_router(channel.clone(), ensure_current).layer(axum::Extension(
        FederationOnboardingHost {
            app: app.clone(),
            rpc: client.clone(),
            io: FederationOnboardingIo::local(directory),
        },
    ));
    federation_validate_credential_basis(app, snapshot, &basis, stop)?;
    attempt.check_material(credentials.material_fingerprint)?;
    let server = {
        // Serialize publication with configuration changes and stop signals.
        // The preceding async bind may have yielded to either of them.
        let inner = app.inner.lock().expect("state mutex poisoned");
        let stopped = stop.borrow();
        let shutdown = app.subscribe_shutdown_signal();
        let shutting_down = shutdown.borrow();
        if *stopped
            || *shutting_down
            || stop.has_changed().is_err()
            || app.federation_executor_wake_signal.is_stopping()
        {
            return Err(federation_stale_link());
        }
        app.ensure_federation_peer_current_locked(&inner, snapshot)?;
        let server = tokio::spawn(serve_federation_control_listener(
            listener,
            router,
            listener_stop_rx,
            channel.clone(),
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
        server
    };
    // Material is adopted on the validated captured basis. Pending connection
    // commits take their own settlement guards as before.
    drop(settlement);
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
