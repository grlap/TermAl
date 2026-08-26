// Remote terminal stream forwarding — proxies a local `/api/terminal/run/stream`
// SSE request to a remote TermAl backend and funnels the output back to
// the local client while maintaining matching output-cap + truncation
// semantics.
//
// Covers: per-stream budget accounting (`RemoteTerminalForwardState`),
// the main forwarding loop (`forward_remote_terminal_stream_response`,
// `forward_remote_terminal_stream_reader`, `_capped`, SSE frame handler),
// the cancellable reader that lets the local SSE client drop the stream
// and kill the remote proxy mid-flight (`InterruptibleRemoteStreamReader`),
// the JSON-fallback reader (`read_remote_stream_response`,
// `remote_response_is_event_stream`), output-cap utilities
// (`cap_terminal_response_output`, `truncate_string_to_byte_limit`),
// and the SSE frame parser (`parse_terminal_sse_frame`,
// `find_sse_frame_delimiter`).
//
// Extracted from remote.rs into its own `include!()` fragment so remote.rs
// stays focused on SSH transport + connection lifecycle.

/// Live-forwarding accounting for a remote terminal stream. Tracks per-stream
/// byte budgets so the truncation semantics applied to intermediate `output`
/// events match the per-stream semantics applied to the final `complete`
/// response by [`cap_terminal_response_output`]. A shared counter would drop
/// a legitimate stderr event whenever stdout had already filled the combined
/// budget, and then fold that spurious truncation into the completion via
/// [`RemoteTerminalForwardState::output_truncated`], marking responses that
/// `cap_terminal_response_output` did not actually truncate.
struct RemoteTerminalForwardState {
    forwarded_stdout_bytes: usize,
    forwarded_stderr_bytes: usize,
    output_truncated: bool,
}

impl RemoteTerminalForwardState {
    fn new() -> Self {
        Self {
            forwarded_stdout_bytes: 0,
            forwarded_stderr_bytes: 0,
            output_truncated: false,
        }
    }

    fn forwarded_bytes_for(&mut self, stream: TerminalOutputStream) -> &mut usize {
        match stream {
            TerminalOutputStream::Stdout => &mut self.forwarded_stdout_bytes,
            TerminalOutputStream::Stderr => &mut self.forwarded_stderr_bytes,
        }
    }
}

#[derive(Debug)]
struct RemoteAuthorityIoError {
    message: String,
    status: StatusCode,
}

impl std::fmt::Display for RemoteAuthorityIoError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RemoteAuthorityIoError {}

fn remote_authority_io_error(error: ApiError) -> io::Error {
    io::Error::new(
        io::ErrorKind::ConnectionAborted,
        RemoteAuthorityIoError {
            message: error.message,
            status: error.status,
        },
    )
}

fn remote_stream_read_api_error(
    error: io::Error,
    cancellation: &Arc<AtomicBool>,
) -> ApiError {
    if error.kind() == io::ErrorKind::Interrupted && cancellation.load(Ordering::SeqCst) {
        return ApiError::bad_gateway("terminal stream client disconnected");
    }
    if let Some(authority_error) = error
        .get_ref()
        .and_then(|source| source.downcast_ref::<RemoteAuthorityIoError>())
    {
        return ApiError::from_status(authority_error.status, authority_error.message.clone());
    }
    ApiError::bad_gateway(format!("failed to read remote stream: {error}"))
}

fn forward_remote_terminal_stream_response(
    response: RemoteStreamingResponse,
    event_tx: &TerminalCommandStreamSender,
    cancellation: &Arc<AtomicBool>,
) -> Result<(), ApiError> {
    response.ensure_current()?;
    if !response.status().is_success() {
        return response.decode_json();
    }
    if !remote_response_is_event_stream(&response) {
        response.ensure_current()?;
        return Err(ApiError::bad_gateway(
            "remote returned unexpected content type for terminal stream",
        ));
    }

    let authority = response.authority();
    let mut reader = InterruptibleRemoteStreamReader::spawn_with_authority(
        response,
        cancellation.clone(),
        authority.clone(),
    );
    let response = forward_remote_terminal_stream_reader_with_authority(
        &mut reader,
        event_tx,
        cancellation,
        &authority,
    )?;
    send_remote_terminal_stream_event(
        event_tx,
        TerminalCommandStreamEvent::Complete(response),
        Some(cancellation),
        Some(&authority),
    )
}

/// Core SSE-framing loop for a remote terminal stream. Extracted from
/// [`forward_remote_terminal_stream_response`] so tests can drive it with an
/// in-memory [`std::io::Read`] (e.g. a `Cursor`) instead of a live HTTP
/// response.
#[cfg(test)]
fn forward_remote_terminal_stream_reader<R: std::io::Read>(
    reader: &mut R,
    event_tx: &TerminalCommandStreamSender,
    cancellation: &Arc<AtomicBool>,
) -> Result<TerminalCommandResponse, ApiError> {
    forward_remote_terminal_stream_reader_capped_with_authority(
        reader,
        event_tx,
        cancellation,
        TERMINAL_REMOTE_SSE_PENDING_MAX_BYTES,
        None,
    )
}

fn forward_remote_terminal_stream_reader_with_authority<R: std::io::Read>(
    reader: &mut R,
    event_tx: &TerminalCommandStreamSender,
    cancellation: &Arc<AtomicBool>,
    authority: &RemoteStreamingAuthority,
) -> Result<TerminalCommandResponse, ApiError> {
    forward_remote_terminal_stream_reader_capped_with_authority(
        reader,
        event_tx,
        cancellation,
        TERMINAL_REMOTE_SSE_PENDING_MAX_BYTES,
        Some(authority),
    )
}

/// Implementation of the SSE-framing loop with an explicit pending-buffer
/// cap. The production caller uses [`TERMINAL_REMOTE_SSE_PENDING_MAX_BYTES`];
/// tests can pass a smaller cap to exercise the rejection path without
/// pushing megabytes of bytes through the reader.
#[cfg(test)]
fn forward_remote_terminal_stream_reader_capped<R: std::io::Read>(
    reader: &mut R,
    event_tx: &TerminalCommandStreamSender,
    cancellation: &Arc<AtomicBool>,
    pending_cap: usize,
) -> Result<TerminalCommandResponse, ApiError> {
    forward_remote_terminal_stream_reader_capped_with_authority(
        reader,
        event_tx,
        cancellation,
        pending_cap,
        None,
    )
}

fn forward_remote_terminal_stream_reader_capped_with_authority<R: std::io::Read>(
    reader: &mut R,
    event_tx: &TerminalCommandStreamSender,
    cancellation: &Arc<AtomicBool>,
    pending_cap: usize,
    authority: Option<&RemoteStreamingAuthority>,
) -> Result<TerminalCommandResponse, ApiError> {
    let mut forward_state = RemoteTerminalForwardState::new();
    let mut pending = Vec::new();
    let mut scratch = [0u8; 8192];
    loop {
        if cancellation.load(Ordering::SeqCst) {
            return Err(ApiError::bad_gateway("terminal stream client disconnected"));
        }

        let bytes_read = reader
            .read(&mut scratch)
            .map_err(|err| remote_stream_read_api_error(err, cancellation))?;
        if bytes_read == 0 {
            break;
        }
        pending.extend_from_slice(&scratch[..bytes_read]);
        if pending.len() > pending_cap {
            return Err(ApiError::bad_gateway(
                "remote terminal stream frame exceeded the allowed size",
            ));
        }

        while let Some((frame_end, delimiter_len)) = find_sse_frame_delimiter(&pending) {
            ensure_remote_terminal_dispatch_current(cancellation, authority)?;
            let frame = String::from_utf8_lossy(&pending[..frame_end]).into_owned();
            pending.drain(..frame_end + delimiter_len);
            if let Some(response) = handle_remote_terminal_sse_frame_with_authority(
                &frame,
                event_tx,
                &mut forward_state,
                Some(cancellation),
                authority,
            )? {
                ensure_remote_terminal_dispatch_current(cancellation, authority)?;
                return Ok(response);
            }
        }
    }

    // Note: there is no post-loop `pending.len() > pending_cap` check
    // because the loop body already enforces the cap after every non-empty
    // read. The loop only exits when `bytes_read == 0`, which happens on an
    // iteration that did not extend `pending`, so the last in-loop check
    // already observed the final pending size. A post-loop check would
    // therefore be unreachable.

    if !pending.iter().all(|byte| byte.is_ascii_whitespace()) {
        let frame = String::from_utf8_lossy(&pending).into_owned();
        ensure_remote_terminal_dispatch_current(cancellation, authority)?;
        if let Some(response) = handle_remote_terminal_sse_frame_with_authority(
            &frame,
            event_tx,
            &mut forward_state,
            Some(cancellation),
            authority,
        )? {
            ensure_remote_terminal_dispatch_current(cancellation, authority)?;
            return Ok(response);
        }
    }

    Err(ApiError::bad_gateway(
        "remote terminal stream ended before the command completed",
    ))
}

#[cfg(test)]
fn handle_remote_terminal_sse_frame(
    frame: &str,
    event_tx: &TerminalCommandStreamSender,
    forward_state: &mut RemoteTerminalForwardState,
) -> Result<Option<TerminalCommandResponse>, ApiError> {
    handle_remote_terminal_sse_frame_with_authority(frame, event_tx, forward_state, None, None)
}

fn ensure_remote_terminal_dispatch_current(
    cancellation: &Arc<AtomicBool>,
    authority: Option<&RemoteStreamingAuthority>,
) -> Result<(), ApiError> {
    if cancellation.load(Ordering::SeqCst) {
        return Err(ApiError::bad_gateway("terminal stream client disconnected"));
    }
    if let Some(authority) = authority {
        authority.ensure_current()?;
    }
    Ok(())
}

fn prefer_remote_terminal_authority<T>(
    authority: Option<&RemoteStreamingAuthority>,
    result: Result<T, ApiError>,
) -> Result<T, ApiError> {
    match authority {
        Some(authority) => authority.prefer_current(result),
        None => result,
    }
}

fn send_remote_terminal_stream_event(
    event_tx: &TerminalCommandStreamSender,
    mut event: TerminalCommandStreamEvent,
    cancellation: Option<&Arc<AtomicBool>>,
    authority: Option<&RemoteStreamingAuthority>,
) -> Result<(), ApiError> {
    if cancellation.is_none() && authority.is_none() {
        return event_tx
            .blocking_send(event)
            .map_err(|_| ApiError::bad_gateway("terminal stream client disconnected"));
    }

    loop {
        if let Some(cancellation) = cancellation {
            ensure_remote_terminal_dispatch_current(cancellation, authority)?;
        } else if let Some(authority) = authority {
            authority.ensure_current()?;
        }

        #[cfg(test)]
        if let Some(authority) = authority {
            authority.run_test_before_terminal_event_enqueue();
        }

        // `try_send` is deliberately nonblocking, so it is safe to keep the
        // registry configs lock across the enqueue. Settings publication takes
        // that same lock before retiring the connection, which makes the event
        // either precede publication or fail the authority fence after it.
        let send_result = match authority {
            Some(authority) => authority.with_current(|| event_tx.try_send(event))?,
            None => event_tx.try_send(event),
        };
        match send_result {
            Ok(()) => return Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(returned)) => {
                event = returned;
                std::thread::sleep(TERMINAL_COMMAND_CANCEL_POLL_INTERVAL);
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                return Err(ApiError::bad_gateway(
                    "terminal stream client disconnected",
                ));
            }
        }
    }
}

fn handle_remote_terminal_sse_frame_with_authority(
    frame: &str,
    event_tx: &TerminalCommandStreamSender,
    forward_state: &mut RemoteTerminalForwardState,
    cancellation: Option<&Arc<AtomicBool>>,
    authority: Option<&RemoteStreamingAuthority>,
) -> Result<Option<TerminalCommandResponse>, ApiError> {
    let Some((event_name, data)) = parse_terminal_sse_frame(frame) else {
        return Ok(None);
    };

    match event_name.as_str() {
        "output" => {
            let payload: TerminalOutputStreamPayload = prefer_remote_terminal_authority(
                authority,
                serde_json::from_str(&data).map_err(|err| {
                    ApiError::bad_gateway(format!(
                        "failed to decode remote terminal output event: {err}"
                    ))
                }),
            )?;
            // Track per-stream forwarding budgets independently so they match
            // the per-stream caps that `cap_terminal_response_output` applies
            // to the final completion response. A shared counter here would
            // drop a legitimate stderr event whenever the combined budget was
            // already exhausted by stdout (or vice versa), then fold that
            // spurious live truncation into the completion via
            // `forward_state.output_truncated` and mark a response that
            // `cap_terminal_response_output` did not actually truncate.
            let stream = payload.stream;
            let forwarded_bytes = forward_state.forwarded_bytes_for(stream);
            let remaining = TERMINAL_OUTPUT_MAX_BYTES.saturating_sub(*forwarded_bytes);
            let (text, truncated) = truncate_string_to_byte_limit(&payload.text, remaining);
            *forwarded_bytes = forwarded_bytes.saturating_add(text.len());
            forward_state.output_truncated |= truncated;
            if text.is_empty() {
                return Ok(None);
            }
            send_remote_terminal_stream_event(
                event_tx,
                TerminalCommandStreamEvent::Output {
                    stream,
                    text,
                },
                cancellation,
                authority,
            )?;
            Ok(None)
        }
        "complete" => {
            let mut response: TerminalCommandResponse = prefer_remote_terminal_authority(
                authority,
                serde_json::from_str(&data).map_err(|err| {
                    ApiError::bad_gateway(format!(
                        "failed to decode remote terminal completion event: {err}"
                    ))
                }),
            )?;
            if cap_terminal_response_output(&mut response) || forward_state.output_truncated {
                response.output_truncated = true;
            }
            if let Some(authority) = authority {
                authority.ensure_current()?;
            }
            Ok(Some(response))
        }
        "error" => {
            let payload: TerminalStreamErrorPayload = prefer_remote_terminal_authority(
                authority,
                serde_json::from_str(&data).map_err(|err| {
                    ApiError::bad_gateway(format!(
                        "failed to decode remote terminal error event: {err}"
                    ))
                }),
            )?;
            let detail = match payload.status {
                Some(status) => format!("remote terminal stream error ({status}): {}", payload.error),
                None => format!("remote terminal stream error: {}", payload.error),
            };
            let result = if payload.status == Some(StatusCode::TOO_MANY_REQUESTS.as_u16()) {
                Err(ApiError::from_status(StatusCode::TOO_MANY_REQUESTS, detail))
            } else {
                Err(ApiError::bad_gateway(detail))
            };
            prefer_remote_terminal_authority(authority, result)
        }
        _ => Ok(None),
    }
}

struct InterruptibleRemoteStreamReader {
    rx: std::sync::mpsc::Receiver<io::Result<Vec<u8>>>,
    cancellation: Arc<AtomicBool>,
    authority: Option<RemoteStreamingAuthority>,
    buffered: Vec<u8>,
    offset: usize,
}

impl InterruptibleRemoteStreamReader {
    /// Spawn an OS worker thread that drains `source` into an internal
    /// channel so the main forwarding thread can observe a cancellation
    /// flag between chunks without being stuck inside a blocking body
    /// read. Generic over the reader so tests can pass a mock whose
    /// `Read::read` blocks on a channel instead of a live
    /// [`BlockingHttpResponse`]; production callers pass the reqwest
    /// blocking body directly.
    #[cfg(test)]
    fn spawn<R>(source: R, cancellation: Arc<AtomicBool>) -> Self
    where
        R: std::io::Read + Send + 'static,
    {
        Self::spawn_inner(source, cancellation, None)
    }

    fn spawn_with_authority<R>(
        source: R,
        cancellation: Arc<AtomicBool>,
        authority: RemoteStreamingAuthority,
    ) -> Self
    where
        R: std::io::Read + Send + 'static,
    {
        Self::spawn_inner(source, cancellation, Some(authority))
    }

    fn spawn_inner<R>(
        source: R,
        cancellation: Arc<AtomicBool>,
        authority: Option<RemoteStreamingAuthority>,
    ) -> Self
    where
        R: std::io::Read + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let reader_cancellation = cancellation.clone();
        std::thread::spawn(move || {
            read_remote_stream_response(source, tx, reader_cancellation)
        });
        Self::new_inner(rx, cancellation, authority)
    }

    #[cfg(test)]
    fn new(
        rx: std::sync::mpsc::Receiver<io::Result<Vec<u8>>>,
        cancellation: Arc<AtomicBool>,
    ) -> Self {
        Self::new_inner(rx, cancellation, None)
    }

    #[cfg(test)]
    fn new_with_authority(
        rx: std::sync::mpsc::Receiver<io::Result<Vec<u8>>>,
        cancellation: Arc<AtomicBool>,
        authority: RemoteStreamingAuthority,
    ) -> Self {
        Self::new_inner(rx, cancellation, Some(authority))
    }

    fn new_inner(
        rx: std::sync::mpsc::Receiver<io::Result<Vec<u8>>>,
        cancellation: Arc<AtomicBool>,
        authority: Option<RemoteStreamingAuthority>,
    ) -> Self {
        Self {
            rx,
            cancellation,
            authority,
            buffered: Vec::new(),
            offset: 0,
        }
    }

    fn ensure_current_authority(&mut self) -> io::Result<()> {
        let Some(authority) = self.authority.as_ref() else {
            return Ok(());
        };
        authority.ensure_current().map_err(|err| {
            self.buffered.clear();
            self.offset = 0;
            remote_authority_io_error(err)
        })
    }

    fn read_buffered(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        if self.offset >= self.buffered.len() {
            return Ok(None);
        }

        self.ensure_current_authority()?;
        let available = &self.buffered[self.offset..];
        let len = available.len().min(buf.len());
        buf[..len].copy_from_slice(&available[..len]);
        self.offset += len;
        if self.offset >= self.buffered.len() {
            self.buffered.clear();
            self.offset = 0;
        }
        // A replacement can publish between the pre-copy check and return.
        // Returning an error makes the copied bytes invalid to Read callers.
        self.ensure_current_authority()?;
        Ok(Some(len))
    }
}

impl std::io::Read for InterruptibleRemoteStreamReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if let Some(len) = self.read_buffered(buf)? {
            return Ok(len);
        }

        loop {
            if self.cancellation.load(Ordering::SeqCst) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "terminal stream client disconnected",
                ));
            }

            match self.rx.recv_timeout(TERMINAL_REMOTE_STREAM_READ_CANCEL_POLL_INTERVAL) {
                Ok(Ok(chunk)) if chunk.is_empty() => {
                    self.ensure_current_authority()?;
                    return Ok(0);
                }
                Ok(Ok(chunk)) => {
                    self.buffered = chunk;
                    self.offset = 0;
                    return Ok(self
                        .read_buffered(buf)?
                        .expect("non-empty chunk should be readable"));
                }
                Ok(Err(err)) => {
                    self.ensure_current_authority()?;
                    return Err(err);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // The producer may remain blocked in an idle HTTP body
                    // read even after settings retire its route. Poll the
                    // independent authority lease here so retirement remains
                    // promptly observable without waiting for remote bytes.
                    self.ensure_current_authority()?;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    self.ensure_current_authority()?;
                    return Ok(0);
                }
            }
        }
    }
}

fn read_remote_stream_response<R: std::io::Read>(
    mut source: R,
    tx: std::sync::mpsc::SyncSender<io::Result<Vec<u8>>>,
    cancellation: Arc<AtomicBool>,
) {
    let mut scratch = [0u8; 8192];
    loop {
        if cancellation.load(Ordering::SeqCst) {
            break;
        }
        match source.read(&mut scratch) {
            Ok(bytes_read) => {
                if cancellation.load(Ordering::SeqCst) {
                    break;
                }
                let chunk = scratch[..bytes_read].to_vec();
                if tx.send(Ok(chunk)).is_err() || bytes_read == 0 {
                    break;
                }
            }
            Err(err) => {
                let _ = tx.send(Err(err));
                break;
            }
        }
    }
}

fn remote_response_is_event_stream(response: &BlockingHttpResponse) -> bool {
    response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("text/event-stream"))
        })
}

fn cap_terminal_response_output(response: &mut TerminalCommandResponse) -> bool {
    let (stdout, stdout_truncated) =
        truncate_string_to_byte_limit(&response.stdout, TERMINAL_OUTPUT_MAX_BYTES);
    let (stderr, stderr_truncated) =
        truncate_string_to_byte_limit(&response.stderr, TERMINAL_OUTPUT_MAX_BYTES);
    response.stdout = stdout;
    response.stderr = stderr;
    stdout_truncated || stderr_truncated
}

fn truncate_string_to_byte_limit(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    if max_bytes == 0 {
        return (String::new(), true);
    }

    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

fn parse_terminal_sse_frame(frame: &str) -> Option<(String, String)> {
    let mut event_name = "message".to_owned();
    let mut data_lines = Vec::new();
    let mut saw_field = false;
    for line in frame.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').map_or((line, ""), |(field, value)| {
            (field, value.strip_prefix(' ').unwrap_or(value))
        });
        match field {
            "event" => {
                event_name = value.to_owned();
                saw_field = true;
            }
            "data" => {
                data_lines.push(value.to_owned());
                saw_field = true;
            }
            _ => {}
        }
    }

    saw_field.then(|| (event_name, data_lines.join("\n")))
}

fn find_sse_frame_delimiter(bytes: &[u8]) -> Option<(usize, usize)> {
    let lf = bytes
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|index| (index, 2));
    let cr = bytes
        .windows(2)
        .position(|window| window == b"\r\r")
        .map(|index| (index, 2));
    let crlf = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| (index, 4));
    [lf, cr, crlf].into_iter().flatten().min_by_key(|(index, _)| *index)
}
