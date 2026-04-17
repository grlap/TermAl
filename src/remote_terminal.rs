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

fn forward_remote_terminal_stream_response(
    response: BlockingHttpResponse,
    event_tx: &TerminalCommandStreamSender,
    cancellation: &Arc<AtomicBool>,
) -> Result<TerminalCommandResponse, ApiError> {
    if !response.status().is_success() {
        return decode_remote_json(response);
    }
    if !remote_response_is_event_stream(&response) {
        return Err(ApiError::bad_gateway(
            "remote returned unexpected content type for terminal stream",
        ));
    }

    let mut reader = InterruptibleRemoteStreamReader::spawn(response, cancellation.clone());
    forward_remote_terminal_stream_reader(&mut reader, event_tx, cancellation)
}

/// Core SSE-framing loop for a remote terminal stream. Extracted from
/// [`forward_remote_terminal_stream_response`] so tests can drive it with an
/// in-memory [`std::io::Read`] (e.g. a `Cursor`) instead of a live HTTP
/// response.
fn forward_remote_terminal_stream_reader<R: std::io::Read>(
    reader: &mut R,
    event_tx: &TerminalCommandStreamSender,
    cancellation: &Arc<AtomicBool>,
) -> Result<TerminalCommandResponse, ApiError> {
    forward_remote_terminal_stream_reader_capped(
        reader,
        event_tx,
        cancellation,
        TERMINAL_REMOTE_SSE_PENDING_MAX_BYTES,
    )
}

/// Implementation of the SSE-framing loop with an explicit pending-buffer
/// cap. The production caller uses [`TERMINAL_REMOTE_SSE_PENDING_MAX_BYTES`];
/// tests can pass a smaller cap to exercise the rejection path without
/// pushing megabytes of bytes through the reader.
fn forward_remote_terminal_stream_reader_capped<R: std::io::Read>(
    reader: &mut R,
    event_tx: &TerminalCommandStreamSender,
    cancellation: &Arc<AtomicBool>,
    pending_cap: usize,
) -> Result<TerminalCommandResponse, ApiError> {
    let mut forward_state = RemoteTerminalForwardState::new();
    let mut pending = Vec::new();
    let mut scratch = [0u8; 8192];
    loop {
        if cancellation.load(Ordering::SeqCst) {
            return Err(ApiError::bad_gateway("terminal stream client disconnected"));
        }

        let bytes_read = reader.read(&mut scratch).map_err(|err| {
            if err.kind() == io::ErrorKind::Interrupted && cancellation.load(Ordering::SeqCst) {
                ApiError::bad_gateway("terminal stream client disconnected")
            } else {
                ApiError::bad_gateway(format!("failed to read remote stream: {err}"))
            }
        })?;
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
            let frame = String::from_utf8_lossy(&pending[..frame_end]).into_owned();
            pending.drain(..frame_end + delimiter_len);
            if let Some(response) = handle_remote_terminal_sse_frame(
                &frame,
                event_tx,
                &mut forward_state,
            )? {
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
        if let Some(response) = handle_remote_terminal_sse_frame(
            &frame,
            event_tx,
            &mut forward_state,
        )? {
            return Ok(response);
        }
    }

    Err(ApiError::bad_gateway(
        "remote terminal stream ended before the command completed",
    ))
}

fn handle_remote_terminal_sse_frame(
    frame: &str,
    event_tx: &TerminalCommandStreamSender,
    forward_state: &mut RemoteTerminalForwardState,
) -> Result<Option<TerminalCommandResponse>, ApiError> {
    let Some((event_name, data)) = parse_terminal_sse_frame(frame) else {
        return Ok(None);
    };

    match event_name.as_str() {
        "output" => {
            let payload: TerminalOutputStreamPayload = serde_json::from_str(&data).map_err(|err| {
                ApiError::bad_gateway(format!(
                    "failed to decode remote terminal output event: {err}"
                ))
            })?;
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
            event_tx
                .blocking_send(TerminalCommandStreamEvent::Output {
                    stream,
                    text,
                })
                .map_err(|_| ApiError::bad_gateway("terminal stream client disconnected"))?;
            Ok(None)
        }
        "complete" => {
            let mut response: TerminalCommandResponse = serde_json::from_str(&data).map_err(|err| {
                ApiError::bad_gateway(format!(
                    "failed to decode remote terminal completion event: {err}"
                ))
            })?;
            if cap_terminal_response_output(&mut response) || forward_state.output_truncated {
                response.output_truncated = true;
            }
            Ok(Some(response))
        }
        "error" => {
            let payload: TerminalStreamErrorPayload = serde_json::from_str(&data).map_err(|err| {
                ApiError::bad_gateway(format!(
                    "failed to decode remote terminal error event: {err}"
                ))
            })?;
            let detail = match payload.status {
                Some(status) => format!("remote terminal stream error ({status}): {}", payload.error),
                None => format!("remote terminal stream error: {}", payload.error),
            };
            if payload.status == Some(StatusCode::TOO_MANY_REQUESTS.as_u16()) {
                Err(ApiError::from_status(StatusCode::TOO_MANY_REQUESTS, detail))
            } else {
                Err(ApiError::bad_gateway(detail))
            }
        }
        _ => Ok(None),
    }
}

struct InterruptibleRemoteStreamReader {
    rx: std::sync::mpsc::Receiver<io::Result<Vec<u8>>>,
    cancellation: Arc<AtomicBool>,
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
    fn spawn<R>(source: R, cancellation: Arc<AtomicBool>) -> Self
    where
        R: std::io::Read + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let reader_cancellation = cancellation.clone();
        std::thread::spawn(move || {
            read_remote_stream_response(source, tx, reader_cancellation)
        });
        Self::new(rx, cancellation)
    }

    fn new(
        rx: std::sync::mpsc::Receiver<io::Result<Vec<u8>>>,
        cancellation: Arc<AtomicBool>,
    ) -> Self {
        Self {
            rx,
            cancellation,
            buffered: Vec::new(),
            offset: 0,
        }
    }

    fn read_buffered(&mut self, buf: &mut [u8]) -> Option<usize> {
        if self.offset >= self.buffered.len() {
            return None;
        }

        let available = &self.buffered[self.offset..];
        let len = available.len().min(buf.len());
        buf[..len].copy_from_slice(&available[..len]);
        self.offset += len;
        if self.offset >= self.buffered.len() {
            self.buffered.clear();
            self.offset = 0;
        }
        Some(len)
    }
}

impl std::io::Read for InterruptibleRemoteStreamReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if let Some(len) = self.read_buffered(buf) {
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
                Ok(Ok(chunk)) if chunk.is_empty() => return Ok(0),
                Ok(Ok(chunk)) => {
                    self.buffered = chunk;
                    self.offset = 0;
                    return Ok(self
                        .read_buffered(buf)
                        .expect("non-empty chunk should be readable"));
                }
                Ok(Err(err)) => return Err(err),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(0),
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
