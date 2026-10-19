// Owns bounded control/HTTP preface discrimination on the dedicated listener.
// Does not mount local APIs or authenticate RPCs. Split from federation_listener_io.rs.

async fn federation_classify_preface(
    stream: &mut tokio::net::TcpStream,
) -> Result<Vec<u8>, ApiError> {
    use tokio::io::AsyncReadExt;
    let mut prefix = Vec::with_capacity(FEDERATION_CONTROL_PREFACE.len());
    for expected in FEDERATION_CONTROL_PREFACE {
        let mut byte = [0];
        stream
            .read_exact(&mut byte)
            .await
            .map_err(|_| federation_transport_unavailable())?;
        prefix.push(byte[0]);
        if byte[0] != *expected {
            break;
        }
    }
    Ok(prefix)
}

async fn serve_federation_control_listener(
    listener: TcpListener,
    router: Router,
    mut stop: tokio::sync::watch::Receiver<bool>,
    channel: FederationChannel,
) -> std::io::Result<()> {
    let tasks = channel.control_tasks.clone();
    let listener = FederationSocketListener {
        listener,
        stop: stop.clone(),
        connections: Arc::new(tokio::sync::Semaphore::new(4)),
        control: Some(channel),
    };
    let result = axum::serve(listener, router)
        .with_graceful_shutdown(async move { federation_wait_stop(&mut stop).await })
        .await;
    let pending = std::mem::take(&mut *tasks.lock().expect("control tasks mutex poisoned"));
    for task in &pending {
        task.abort();
    }
    for task in pending {
        let _ = task.await;
    }
    result
}
