// Terminal command wire types + size/timeout constants.
//
// Groups the DTOs + tuning constants consumed by the terminal
// execution routes in `terminal.rs` + `api.rs` and the remote-proxy
// path in `remote_terminal.rs`. Split out of wire.rs because the
// terminal command pipeline has the densest constant-vocabulary in
// the crate (output caps, timeout windows, SSE framing bounds) and
// those live better next to the DTOs that reference them.
//
// `TerminalCommandSseStream` implements `futures_core::Stream` so
// the Axum SSE handler can drive it directly; `TerminalStreamCancelGuard`
// is the RAII guard that signals the background executor to cancel
// when the HTTP response is dropped mid-stream.

const TERMINAL_COMMAND_MAX_CHARS: usize = 20_000;
/// Upper bound on the `workdir` field of a terminal command request. Real
/// filesystem paths stay well under this; the cap is a defense-in-depth
/// limit so a client cannot POST a megabyte of whitespace-stripped text
/// that then flows into `resolve_project_scoped_requested_path` or over
/// the wire to the remote proxy. Paired with explicit NUL-byte rejection
/// in `validate_terminal_workdir`.
const TERMINAL_WORKDIR_MAX_CHARS: usize = 4_096;
/// Maximum captured terminal output per stream. Stdout and stderr each get
/// their own budget on both local runs and remote JSON proxy responses.
const TERMINAL_OUTPUT_MAX_BYTES: usize = 512 * 1024;
const TERMINAL_STREAM_EVENT_QUEUE_CAPACITY: usize = 256;
const TERMINAL_COMMAND_CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Upper bound on the bytes the remote proxy will buffer while waiting to
/// find the next SSE frame delimiter. A completion frame carries the full
/// `TerminalCommandResponse`, including up to `TERMINAL_OUTPUT_MAX_BYTES` of
/// stdout plus the same of stderr, plus the echoed command string and
/// workdir. JSON encoding can expand each byte up to 6× (ASCII control
/// characters become `\u00XX`) and SSE framing adds further overhead, so the
/// worst-case legitimate completion frame is roughly
/// `TERMINAL_OUTPUT_MAX_BYTES * 12 + ~200 KiB`. Cap at 16× the raw output
/// limit (8 MiB) so that envelope fits with comfortable headroom while still
/// bounding memory if a remote misbehaves and never emits a delimiter.
const TERMINAL_REMOTE_SSE_PENDING_MAX_BYTES: usize = TERMINAL_OUTPUT_MAX_BYTES * 16;

/// Remote proxy timeout for terminal commands. This must cover the remote child
/// wait, post-timeout process cleanup, stdout/stderr reader joins, JSON
/// encoding/decoding, and a small network scheduling margin.
const REMOTE_TERMINAL_COMMAND_TIMEOUT: Duration = Duration::from_secs(90);

/// Maximum time to wait for terminal output-reader threads after the child
/// process exits. Background children that inherit stdout/stderr can keep the
/// pipe open indefinitely; this prevents the request from blocking forever.
const TERMINAL_OUTPUT_READER_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Represents a terminal command request.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TerminalCommandRequest {
    command: String,
    workdir: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

/// Represents a terminal command response.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalCommandResponse {
    command: String,
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    output_truncated: bool,
    shell: String,
    stderr: String,
    stdout: String,
    success: bool,
    timed_out: bool,
    workdir: String,
}

type TerminalCommandStreamSender = tokio::sync::mpsc::Sender<TerminalCommandStreamEvent>;

struct TerminalStreamCancelGuard {
    cancellation: Arc<AtomicBool>,
}

impl Drop for TerminalStreamCancelGuard {
    fn drop(&mut self) {
        self.cancellation.store(true, Ordering::SeqCst);
    }
}

/// SSE stream adapter for a streaming terminal command.
///
/// **Field drop order is load-bearing.** Rust drops struct fields in
/// declaration order, so `event_rx` is dropped *before* `_cancel_on_drop`.
/// That order is required so that any worker still parked inside
/// `blocking_send(..)` on the matching sender observes the channel closing
/// and returns immediately — the spawned worker then releases its
/// concurrency permit and exits. Only after `event_rx` is torn down does
/// the cancellation guard flip, which asks other parts of the pipeline
/// (the SSE forwarder, the remote read adapter, the streaming child wait)
/// to stop. Swapping the field order to "flip the cancellation flag
/// first" would leave the worker parked inside `blocking_send` for up to
/// one `TERMINAL_COMMAND_CANCEL_POLL_INTERVAL` tick before the next
/// `try_send` sees the flag, regressing cancellation latency without
/// failing any existing test.
struct TerminalCommandSseStream {
    event_rx: tokio::sync::mpsc::Receiver<TerminalCommandStreamEvent>,
    _cancel_on_drop: TerminalStreamCancelGuard,
}

impl futures_core::Stream for TerminalCommandSseStream {
    type Item = std::result::Result<Event, Infallible>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match std::pin::Pin::new(&mut this.event_rx).poll_recv(cx) {
            std::task::Poll::Ready(Some(event)) => {
                std::task::Poll::Ready(Some(Ok(terminal_command_sse_event(event))))
            }
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

enum TerminalCommandStreamEvent {
    Output {
        stream: TerminalOutputStream,
        text: String,
    },
    Complete(TerminalCommandResponse),
    Error {
        error: String,
        status: u16,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum TerminalOutputStream {
    Stdout,
    Stderr,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalOutputStreamPayload {
    stream: TerminalOutputStream,
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TerminalStreamErrorPayload {
    error: String,
    #[serde(default)]
    status: Option<u16>,
}
