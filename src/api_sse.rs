// State SSE stream handler + initial-snapshot payload builders.
//
// `state_events` is the big one: the Server-Sent Events endpoint
// that the frontend subscribes to for live state updates. It:
//
// 1. Subscribes to the three `broadcast::Receiver` channels created
//    in `sse_broadcast.rs` (`subscribe_events` for full snapshots,
//    `subscribe_delta_events` for narrow delta events,
//    `subscribe_file_events` for workspace file changes).
// 2. Pushes an initial state snapshot over the wire so the client
//    has ground truth before any deltas arrive.
// 3. Multiplexes the three channels into one SSE stream with
//    `event:` tags identifying each payload kind.
// 4. Configures keep-alive pings so the connection stays live
//    through idle periods and aggressive HTTP proxies.
//
// `state_snapshot_payload_for_sse` builds the initial snapshot by
// calling `AppState::snapshot` (see `state_accessors.rs`) and
// serializing it off-thread via a `spawn_blocking` so the async
// runtime doesn't block on JSON serialization.
//
// The `fallback_*` helpers exist for the degraded case where the
// state subscription hasn't emitted yet: we synthesize a minimal
// `StateResponse` carrying just the current revision so the client
// can ack + wait. `stable_text_hash` is a tiny FNV-1a hash used to
// tag fallback payloads so clients can dedup retransmits.
// `empty_state_events_response` is the minimal skeleton.


fn stable_text_hash(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn empty_state_events_response() -> StateResponse {
    StateResponse {
        revision: 0,
        // Empty string signals "unknown instance" — the client's
        // restart-detection logic only accepts a revision downgrade
        // when the id is non-empty AND changed, so this fallback
        // snapshot cannot accidentally masquerade as a restart signal.
        server_instance_id: String::new(),
        codex: CodexState::default(),
        agent_readiness: Vec::new(),
        preferences: AppPreferences::default(),
        projects: Vec::new(),
        orchestrators: Vec::new(),
        workspaces: Vec::new(),
        sessions: Vec::new(),
    }
}

#[derive(Deserialize)]
struct StateEventPayload {
    #[serde(default, rename = "_sseFallback")]
    sse_fallback: bool,
    #[serde(flatten)]
    state: StateResponse,
}

#[derive(Serialize)]
struct FallbackStateEventPayload {
    #[serde(rename = "_sseFallback")]
    sse_fallback: bool,
    #[serde(flatten)]
    state: StateResponse,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[cfg_attr(test, allow(dead_code))]
#[serde(rename_all = "camelCase")]
enum WorkspaceFileChangeKind {
    Created,
    Modified,
    Deleted,
    Other,
}

/// Represents a file changed during an agent turn.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileChangeSummaryEntry {
    path: String,
    kind: WorkspaceFileChangeKind,
}

#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(test, allow(dead_code))]
#[serde(rename_all = "camelCase")]
struct WorkspaceFileChangeEvent {
    path: String,
    kind: WorkspaceFileChangeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    root_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mtime_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size_bytes: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[cfg_attr(test, allow(dead_code))]
#[serde(rename_all = "camelCase")]
struct WorkspaceFilesChangedEvent {
    revision: u64,
    changes: Vec<WorkspaceFileChangeEvent>,
}

fn fallback_state_events_response(revision: u64) -> FallbackStateEventPayload {
    let mut state = empty_state_events_response();
    state.revision = revision;
    FallbackStateEventPayload {
        sse_fallback: true,
        state,
    }
}

fn fallback_state_events_payload(revision: u64) -> Result<String, ApiError> {
    serde_json::to_string(&fallback_state_events_response(revision)).map_err(|err| {
        ApiError::internal(format!(
            "failed to serialize fallback SSE state snapshot: {err}"
        ))
    })
}

static EMPTY_STATE_EVENTS_PAYLOAD: LazyLock<String> = LazyLock::new(|| {
    fallback_state_events_payload(0).expect("empty SSE state payload should serialize")
});

/// Serializes a metadata-first state snapshot for SSE on the blocking pool
/// because summary_snapshot() acquires the synchronous app-state mutex.
async fn state_snapshot_payload_for_sse(state: AppState) -> String {
    run_blocking_api(move || {
        let snapshot = state.summary_snapshot();
        match serde_json::to_string(&snapshot) {
            Ok(payload) => Ok(payload),
            Err(err) => {
                eprintln!(
                    "state events warning> failed to serialize SSE state snapshot at revision {}: {}",
                    snapshot.revision,
                    err
                );
                fallback_state_events_payload(snapshot.revision)
            }
        }
    })
    .await
    .unwrap_or_else(|err| {
        eprintln!(
            "state events warning> failed to build SSE fallback state snapshot: {}",
            err.message
        );
        EMPTY_STATE_EVENTS_PAYLOAD.clone()
    })
}

/// Streams state and delta events over SSE.
async fn state_events(
    State(state): State<AppState>,
) -> Sse<impl futures_core::Stream<Item = std::result::Result<Event, Infallible>>> {
    let mut state_receiver = state.subscribe_events();
    let mut delta_receiver = state.subscribe_delta_events();
    let mut file_receiver = state.subscribe_file_events();
    let initial_payload = state_snapshot_payload_for_sse(state.clone()).await;

    let stream = async_stream::stream! {
        yield Ok(Event::default().event("state").data(initial_payload));

        loop {
            tokio::select! {
                biased;

                result = state_receiver.recv() => {
                    match result {
                        Ok(payload) => yield Ok(Event::default().event("state").data(payload)),
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            let payload = state_snapshot_payload_for_sse(state.clone()).await;
                            yield Ok(Event::default().event("state").data(payload));
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }

                result = delta_receiver.recv() => {
                    match result {
                        Ok(payload) => yield Ok(Event::default().event("delta").data(payload)),
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            let payload = state_snapshot_payload_for_sse(state.clone()).await;
                            yield Ok(Event::default().event("state").data(payload));
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }

                result = file_receiver.recv() => {
                    match result {
                        Ok(payload) => yield Ok(Event::default().event("workspaceFilesChanged").data(payload)),
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}
