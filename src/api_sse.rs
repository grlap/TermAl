// State SSE stream handler + initial-snapshot payload builders.
//
// `state_events` is the big one: the Server-Sent Events endpoint
// that the frontend subscribes to for live state updates. It:
//
// 1. Subscribes to the two `broadcast::Receiver` channels created
//    in `sse_broadcast.rs` (`subscribe_stream_events` for full
//    snapshots and narrow delta events together, in the order they
//    were published, and `subscribe_file_events` for workspace file
//    changes).
// 2. Pushes an initial state snapshot over the wire so the client
//    has ground truth before any deltas arrive.
// 3. Multiplexes the two channels into one SSE stream with
//    `event:` tags identifying each payload kind. A connection opened with
//    `?stateDiff=1` forwards `stateDiff` events in place of the full `state`
//    paired with them; any other connection is counted as legacy and keeps
//    receiving every cut as a full `state` (the remote hub is one). An
//    opted-in connection, and `POST /api/events/full-state`, ask the
//    broadcaster for its next cut as a full `state` marked
//    `stateDiffBase: true`, the only kind a client builds diffs on.
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
// `StateResponse` carrying the current revision and server identity so the
// client can ack + wait. `stable_text_hash` is a tiny FNV-1a hash used to
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

fn empty_state_events_response(server_instance_id: String) -> StateResponse {
    debug_assert!(!server_instance_id.trim().is_empty());
    StateResponse {
        revision: 0,
        server_instance_id,
        codex: CodexState::default(),
        agent_readiness: Vec::new(),
        preferences: AppPreferences::default(),
        projects: Vec::new(),
        orchestrators: Vec::new(),
        workspaces: Vec::new(),
        sessions: Vec::new(),
        delegations: Vec::new(),
        delegation_waits: Vec::new(),
        test_runs: Vec::new(),
        test_run_waits: Vec::new(),
        pending_engram_mcp_revocation_session_ids: Vec::new(),
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

fn fallback_state_events_response(
    revision: u64,
    server_instance_id: String,
) -> FallbackStateEventPayload {
    let mut state = empty_state_events_response(server_instance_id);
    state.revision = revision;
    FallbackStateEventPayload {
        sse_fallback: true,
        state,
    }
}

fn fallback_state_events_payload(
    revision: u64,
    server_instance_id: String,
) -> Result<String, ApiError> {
    serde_json::to_string(&fallback_state_events_response(
        revision,
        server_instance_id,
    ))
    .map_err(|err| {
        ApiError::internal(format!(
            "failed to serialize fallback SSE state snapshot: {err}"
        ))
    })
}

/// Serializes a metadata-first state snapshot for SSE on the blocking pool
/// because summary_snapshot() acquires the synchronous app-state mutex.
async fn state_snapshot_payload_for_sse(state: AppState) -> String {
    let fallback_server_instance_id = state.server_instance_id.clone();
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
                fallback_state_events_payload(
                    snapshot.revision,
                    snapshot.server_instance_id.clone(),
                )
            }
        }
    })
    .await
    .unwrap_or_else(|err| {
        eprintln!(
            "state events warning> failed to build SSE fallback state snapshot: {}",
            err.message
        );
        fallback_state_events_payload(0, fallback_server_instance_id)
            .expect("fallback SSE state payload should serialize")
    })
}

/// Awaits a transition of the shutdown signal to `true`. Returns
/// immediately if the receiver's current value is already `true`, even
/// if `send(true)` was called before the receiver was constructed —
/// this is the sticky property the `tokio::sync::watch` choice provides
/// over `Notify`. See bugs.md "One-shot SSE shutdown notification can
/// be missed before waiter registration".
async fn wait_for_shutdown_signal(rx: &mut tokio::sync::watch::Receiver<bool>) {
    if *rx.borrow_and_update() {
        return;
    }
    while rx.changed().await.is_ok() {
        if *rx.borrow_and_update() {
            return;
        }
    }
}

async fn lagged_recovery_events(state: AppState) -> (Event, Event) {
    // Lagged means the consumer fell past broadcast channel capacity, so some
    // events were dropped before the client could read them. Emit a non-empty
    // marker first so the client force-adopts the following state snapshot even
    // when its revision equals the client's current revision.
    //
    // The `"1"` payload is intentional: empty `data` can be skipped by browsers
    // because the WHATWG EventSource spec requires a non-empty `data:` line per
    // dispatched event. Clients must NOT parse this body; it is reserved as a
    // control marker. The file-change receiver deliberately does not use this
    // helper because workspace file events are non-authoritative hints.
    let payload = state_snapshot_payload_for_sse(state).await;
    (
        Event::default().event("lagged").data("1"),
        Event::default().event("state").data(payload),
    )
}

/// `GET /api/events` query. Only `stateDiff=1` opts in; an absent, unknown or
/// malformed query keeps the legacy full-`state` stream.
#[derive(Deserialize)]
struct StateEventsQuery {
    #[serde(default, rename = "stateDiff")]
    state_diff: Option<String>,
}

fn state_events_query_opts_into_diffs(
    query: &Result<Query<StateEventsQuery>, QueryRejection>,
) -> bool {
    matches!(query, Ok(Query(StateEventsQuery { state_diff: Some(value) })) if value == "1")
}

/// Counts one legacy `/api/events` connection for as long as it lives. The
/// broadcaster pairs every diff with a full snapshot while any is counted.
struct LegacyStateSubscriber(Arc<std::sync::atomic::AtomicUsize>);

impl LegacyStateSubscriber {
    fn register(senders: &StateBroadcastSenders) -> Self {
        let count = senders.legacy_state_subscribers.clone();
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self(count)
    }
}

impl Drop for LegacyStateSubscriber {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Opens a new full-state request generation. Only a cut captured after this
/// (it reads the generation under StateInner) can answer it.
fn request_state_diff_base(senders: &StateBroadcastSenders) {
    senders
        .full_state_request_generation
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// `POST /api/events/full-state` body. Requiring JSON keeps the request off
/// the CORS simple-request path, so a cross-origin call needs the preflight
/// the shared CORS layer governs. No fields are defined; `{}` is the body.
#[derive(Deserialize)]
struct FullStateRequest {}

/// `POST /api/events/full-state`: an opted-in client that cannot apply a
/// `stateDiff` asks for a marked full state. Idempotent. It publishes the
/// current cut at once, with no revision bump and nothing to persist, so an
/// idle host answers without waiting for an unrelated commit.
async fn request_full_state(
    State(state): State<AppState>,
    request: Result<Json<FullStateRequest>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let Json(FullStateRequest {}) =
        request.map_err(|rejection| api_json_rejection("full-state request", rejection))?;
    run_blocking_api(move || {
        let inner = state.inner.lock().expect("state mutex poisoned");
        // Under the commit lock and before the capture: this cut carries the
        // new generation, and a cut captured earlier (even one the
        // broadcaster already dequeued) carries an older one and cannot
        // answer the request.
        request_state_diff_base(&state.state_broadcast_senders);
        state.publish_state_locked(&inner);
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Streams state and delta events over SSE. Opted-in connections receive
/// `stateDiff` instead of the full `state` that accompanies a diff; a full
/// `state` sent without a diff, the initial state and lag recovery reach both.
async fn state_events(
    State(state): State<AppState>,
    query: Result<Query<StateEventsQuery>, QueryRejection>,
) -> Sse<impl futures_core::Stream<Item = std::result::Result<Event, Infallible>>> {
    let state_diffs = state_events_query_opts_into_diffs(&query);
    // Counted before subscribing and before the initial capture: a diff this
    // connection can receive is then always paired with a full snapshot. The
    // guard moves into the stream, so every exit, including a dropped request
    // before the stream exists, uncounts it.
    let legacy_subscriber =
        (!state_diffs).then(|| LegacyStateSubscriber::register(&state.state_broadcast_senders));
    // Snapshots and deltas arrive on one channel, in the order they were
    // published. Merging two channels here cannot keep that order: whichever
    // the select polls first overtakes the other when both are ready.
    let mut stream_receiver = state.subscribe_stream_events();
    if state_diffs {
        // Ask for a marked full state so this connection gets a diff base.
        // Requested after subscribing: a cut the broadcaster sends for this
        // request is then always delivered here.
        request_state_diff_base(&state.state_broadcast_senders);
    }
    let mut file_receiver = state.subscribe_file_events();
    let mut shutdown_rx = state.subscribe_shutdown_signal();
    // Sticky pre-check: if shutdown was already triggered before this
    // request even began handler setup, refuse to yield the initial state
    // and exit immediately. This is the case `Notify::notify_waiters`
    // could not handle: a notification fired before the waiter registered
    // simply vanishes, so an `/api/events` connection accepted in the
    // window between Ctrl+C and graceful-shutdown completion would loop
    // forever. With `watch`, the `true` value is durable.
    let already_shutting_down = *shutdown_rx.borrow_and_update();
    let initial_payload = state_snapshot_payload_for_sse(state.clone()).await;

    let stream = async_stream::stream! {
        let _legacy_subscriber = legacy_subscriber;
        if already_shutting_down {
            return;
        }
        yield Ok(Event::default().event("state").data(initial_payload));

        // Re-check after the initial yield: shutdown may have flipped
        // during snapshot serialization. Without this loop's exit branch
        // the broadcast receivers below only return `Closed` when the
        // last `AppState` clone drops, which is held by `shutdown_state`
        // for the post-serve persist drain — so graceful shutdown would
        // block forever. See bugs.md "Graceful shutdown blocks forever
        // waiting for SSE streams to drain" and "One-shot SSE shutdown
        // notification can be missed before waiter registration".
        if *shutdown_rx.borrow_and_update() {
            return;
        }

        loop {
            tokio::select! {
                biased;

                _ = wait_for_shutdown_signal(&mut shutdown_rx) => break,

                result = stream_receiver.recv() => {
                    match result {
                        Ok(SseStreamEvent::State(payload)) => {
                            yield Ok(Event::default().event("state").data(&*payload));
                        }
                        Ok(SseStreamEvent::LegacyState(payload)) => {
                            if !state_diffs {
                                yield Ok(Event::default().event("state").data(&*payload));
                            }
                        }
                        Ok(SseStreamEvent::StateDiff(payload)) => {
                            if state_diffs {
                                yield Ok(Event::default().event("stateDiff").data(&*payload));
                            }
                        }
                        Ok(SseStreamEvent::Delta(payload)) => {
                            yield Ok(Event::default().event("delta").data(&*payload));
                        }
                        Ok(SseStreamEvent::Lagged) | Err(broadcast::error::RecvError::Lagged(_)) => {
                            // Lagged means the consumer fell past the broadcast
                            // channel capacity, or the producer mailbox overflowed,
                            // so some events were dropped on the
                            // floor before the client could read them. The
                            // recovery snapshot below restores authoritative state,
                            // but its revision may equal the client's existing
                            // revision (the client read some events from the burst
                            // before falling behind). Emit a `lagged` marker first
                            // so the client arms force-adopt for the next state
                            // event regardless of revision parity — otherwise the
                            // recovery snapshot can be silently ignored as a
                            // redundant catch-up. See bugs.md "SSE Lagged-recovery
                            // snapshot can be silently ignored".
                            //
                            // The `"1"` payload is intentional: empty `data` can
                            // be skipped by browsers because the WHATWG
                            // EventSource spec requires a non-empty `data:`
                            // line per dispatched event. Clients must NOT parse
                            // this body — it is reserved as a control marker.
                            // See bugs.md "Empty-data `lagged` SSE marker may
                            // not dispatch in browsers".
                            let (lagged, recovery) = lagged_recovery_events(state.clone()).await;
                            yield Ok(lagged);
                            yield Ok(recovery);
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
