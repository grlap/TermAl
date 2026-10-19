// Owns bounded stdio-to-async buffering for the one owned SSH control stream.
// Does not open a TCP port or carry keys in logs. New pipe bridge beside federation_process.rs.

fn federation_stdio_control(
    mut input: std::process::ChildStdin,
    mut output: std::process::ChildStdout,
) -> Result<FederationControlIo, ApiError> {
    let (local, bridge) = tokio::io::duplex(FEDERATION_FRAME_LIMIT);
    let (mut reader, mut writer) = tokio::io::split(bridge);
    let read_runtime = tokio::runtime::Handle::current();
    let write_runtime = read_runtime.clone();
    // Threads own pipe handles, never the child. Exact child reap closes
    // the pipes, and dropping the async stream closes the bounded buffers.
    std::thread::Builder::new()
        .name("federation-stdout".to_owned())
        .spawn(move || {
            use tokio::io::AsyncWriteExt;
            let mut bytes = [0; 4096];
            loop {
                let count = match std::io::Read::read(&mut output, &mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => count,
                };
                if read_runtime
                    .block_on(writer.write_all(&bytes[..count]))
                    .is_err()
                {
                    break;
                }
            }
        })
        .map_err(|_| federation_transport_unavailable())?;
    std::thread::Builder::new()
        .name("federation-stdin".to_owned())
        .spawn(move || {
            use tokio::io::AsyncReadExt;
            let mut bytes = [0; 4096];
            loop {
                let count = match write_runtime.block_on(reader.read(&mut bytes)) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => count,
                };
                if std::io::Write::write_all(&mut input, &bytes[..count]).is_err() {
                    break;
                }
            }
        })
        .map_err(|_| federation_transport_unavailable())?;
    Ok(Box::new(local))
}
