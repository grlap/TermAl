/*
HTTP handler layer
Request flow:
axum route
  -> parse path/query/json
  -> run_blocking_api(...)
  -> AppState method
  -> optional runtime or remote dispatch
  -> JSON response or SSE payload
This file stays intentionally thin. Transport details live here, durable state
changes live in state.rs, runtime process logic lives in runtime.rs, and the
turn normalization layer lives in turns.rs.
*/

/// Returns a stable content identity for source-editor conflict detection.
fn file_content_hash(content: &[u8]) -> String {
    let digest = Sha256::digest(content);
    format!("sha256:{digest:x}")
}

/// Converts filesystem modified time to JavaScript-friendly milliseconds.
fn file_metadata_mtime_ms(metadata: &fs::Metadata) -> Option<u64> {
    let millis = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis();
    Some(millis.min(u128::from(u64::MAX)) as u64)
}

fn record_rejected_turn_dispatch(
    state: &AppState,
    session_id: &str,
    error_message: &str,
    mailbox_notification: Option<&MailboxNotificationDelivery>,
    engram_dispatch_generation: Option<u64>,
    runtime_token: &RuntimeToken,
    active_turn_generation: u64,
) -> bool {
    let rejected_by_engram_generation = if let Some(dispatch_generation) =
        engram_dispatch_generation
    {
        match state.reject_engram_turn_delivery_if_current(
            session_id,
            dispatch_generation,
            error_message,
        ) {
            Ok(rejected) => rejected,
            Err(err) => {
                eprintln!(
                    "turn dispatch> failed recording guarded Engram rejection for `{session_id}`: {err:#}"
                );
                false
            }
        }
    } else {
        false
    };
    if !rejected_by_engram_generation {
        // A policy refusal/defer/degradation persists its Engram card and
        // consumes the pending-dispatch marker before returning Rejected. The
        // Engram-generation cleanup is therefore expected to no-op. A held
        // Defer has also advanced its operation identity and left Active, so
        // the runtime-token cleanup intentionally no-ops too; other refusals
        // finish the exact turn by runtime token + active generation.
        // If this callback is stale, the atomic helper returns false without
        // touching the successor.
        match state.fail_rejected_turn_delivery(
            session_id,
            runtime_token,
            active_turn_generation,
            engram_dispatch_generation,
            error_message,
        ) {
            Ok(true) => {}
            // A false guard means a newer runtime/turn or an already-claimed
            // terminalization owns the record. That owner still holds the
            // record's exact active mailbox boundary and is responsible for
            // restoring it; requeueing this stale dispatch here could
            // duplicate the successor wake.
            Ok(false) => return false,
            Err(err) => {
                eprintln!("turn dispatch> failed recording rejection for `{session_id}`: {err:#}");
                // The terminal transaction may have mutated the in-memory
                // record before persistence failed, including taking the
                // active mailbox boundary. The dispatch still owns its exact
                // boundary, so restore it below just as the pre-atomic path
                // did; boot recovery remains the durable fallback if this
                // requeue cannot persist either.
            }
        }
    }
    if let Some(notification) = mailbox_notification {
        if let Err(err) = state.requeue_rejected_mailbox_notification(notification) {
            eprintln!(
                "mailbox> failed restoring rejected wake for `{}` / `{}`: {err:#}",
                notification.session_id, notification.mailbox_id
            );
        }
    }
    true
}

#[derive(Debug)]
enum TurnDispatchDeliveryOutcome {
    Delivered,
    Scheduled,
    Held { error: Option<ApiError> },
    Superseded,
    Rejected(ApiError),
}

impl TurnDispatchDeliveryOutcome {
    fn into_public_result(self) -> Result<(), ApiError> {
        match self {
            Self::Delivered | Self::Scheduled | Self::Held { error: None } | Self::Superseded => {
                Ok(())
            }
            Self::Held { error: Some(error) } | Self::Rejected(error) => Err(error),
        }
    }

    fn into_background_result(self, context: &str) -> Result<(), ApiError> {
        match self {
            Self::Rejected(error) => Err(error),
            Self::Held { error: Some(error) } => {
                eprintln!(
                    "{context}> dispatch held after persistence uncertainty: {}",
                    error.message
                );
                Ok(())
            }
            Self::Delivered
            | Self::Scheduled
            | Self::Held { error: None }
            | Self::Superseded => Ok(()),
        }
    }

    #[cfg(test)]
    fn expect(self, message: &str) {
        self.into_public_result().expect(message);
    }

    #[cfg(test)]
    fn unwrap(self) {
        self.into_public_result().unwrap();
    }

    #[cfg(test)]
    fn expect_err(self, message: &str) -> ApiError {
        self.into_public_result().expect_err(message)
    }

    #[cfg(test)]
    fn unwrap_err(self) -> ApiError {
        self.into_public_result().unwrap_err()
    }

    #[cfg(test)]
    fn is_err(&self) -> bool {
        matches!(
            self,
            Self::Held { error: Some(_) } | Self::Rejected(_)
        )
    }
}

fn engram_persistence_unknown_error() -> ApiError {
    ApiError::internal(
        "Engram authorization persistence is unknown. Provider delivery was withheld and the prompt remains retained for recovery.",
    )
}

/// Keeps transport readers free to route replies needed by Fast discovery.
fn deliver_turn_dispatch(state: &AppState, dispatch: TurnDispatch) -> TurnDispatchDeliveryOutcome {
    if matches!(
        &dispatch,
        TurnDispatch::PersistentCodex {
            service_tier: Err(_),
            ..
        }
    ) {
        let worker_state = state.clone();
        let session_id = dispatch.session_id().to_owned();
        let runtime_token = dispatch.runtime_token().clone();
        let generation = dispatch.active_turn_generation();
        let engram_generation = dispatch.engram_dispatch_generation();
        let notification = dispatch.mailbox_notification().cloned();
        // Completion callbacks run on the shared stdout reader. Waiting there
        // for model/list would prevent that same reader from routing its reply.
        // Only unresolved Fast needs a worker; ready tiers retain direct delivery.
        if let Err(err) = std::thread::Builder::new()
            .name("codex-fast-dispatch".to_owned())
            .spawn(move || {
                match deliver_turn_dispatch_now(&worker_state, dispatch) {
                    TurnDispatchDeliveryOutcome::Held { error: Some(error) } => {
                        eprintln!("Codex Fast dispatch held> {}", error.message);
                    }
                    TurnDispatchDeliveryOutcome::Rejected(error) => {
                        // The delivery path records the guarded session error.
                        // Do not escalate a destination failure to the reader.
                        eprintln!("Codex Fast dispatch> {}", error.message);
                    }
                    TurnDispatchDeliveryOutcome::Delivered
                    | TurnDispatchDeliveryOutcome::Scheduled
                    | TurnDispatchDeliveryOutcome::Held { error: None }
                    | TurnDispatchDeliveryOutcome::Superseded => {}
                }
            })
        {
            let detail = format!("failed to start Codex Fast discovery worker: {err}");
            return if record_rejected_turn_dispatch(
                state,
                &session_id,
                &detail,
                notification.as_ref(),
                engram_generation,
                &runtime_token,
                generation,
            ) {
                TurnDispatchDeliveryOutcome::Rejected(ApiError::internal(detail))
            } else {
                TurnDispatchDeliveryOutcome::Superseded
            };
        }
        return TurnDispatchDeliveryOutcome::Scheduled;
    }
    deliver_turn_dispatch_now(state, dispatch)
}

/// Performs delivery on the caller for ready commands, or on the Fast worker.
fn deliver_turn_dispatch_now(
    state: &AppState,
    dispatch: TurnDispatch,
) -> TurnDispatchDeliveryOutcome {
    let active_turn_generation = dispatch.active_turn_generation();
    let runtime_token = dispatch.runtime_token().clone();
    if let Some(dispatch_generation) = dispatch.engram_dispatch_generation() {
        match state
            .prepare_engram_turn_delivery_off_lock(dispatch.session_id(), dispatch_generation)
        {
            EngramTurnDeliveryPreparation::Ready => {}
            EngramTurnDeliveryPreparation::Superseded => {
                return TurnDispatchDeliveryOutcome::Superseded;
            }
            EngramTurnDeliveryPreparation::PersistenceUnknown => {
                return TurnDispatchDeliveryOutcome::Held {
                    error: Some(engram_persistence_unknown_error()),
                };
            }
            EngramTurnDeliveryPreparation::Rejected => {
                let session_id = dispatch.session_id().to_owned();
                let mailbox_notification = dispatch.mailbox_notification().cloned();
                let error_message = "Engram did not authorize this turn for runtime delivery";
                match state.park_unknown_engram_authorization(
                    &session_id,
                    dispatch_generation,
                    &runtime_token,
                    active_turn_generation,
                ) {
                    EngramAuthorizationParkOutcome::Parked => {
                        return TurnDispatchDeliveryOutcome::Held { error: None };
                    }
                    EngramAuthorizationParkOutcome::PersistenceUnknown => {
                        return TurnDispatchDeliveryOutcome::Held {
                            error: Some(engram_persistence_unknown_error()),
                        };
                    }
                    EngramAuthorizationParkOutcome::Superseded => {}
                }
                let rejected = record_rejected_turn_dispatch(
                    state,
                    &session_id,
                    error_message,
                    mailbox_notification.as_ref(),
                    Some(dispatch_generation),
                    &runtime_token,
                    active_turn_generation,
                );
                return if rejected {
                    TurnDispatchDeliveryOutcome::Rejected(ApiError::conflict(error_message))
                } else {
                    TurnDispatchDeliveryOutcome::Superseded
                };
            }
        }
    }
    match handoff_prepared_turn_dispatch(state, dispatch) {
        Ok(HandoffPreparedTurnDispatchOutcome::Delivered) => {
            TurnDispatchDeliveryOutcome::Delivered
        }
        Ok(HandoffPreparedTurnDispatchOutcome::Scheduled) => {
            TurnDispatchDeliveryOutcome::Scheduled
        }
        Ok(HandoffPreparedTurnDispatchOutcome::Superseded) => {
            TurnDispatchDeliveryOutcome::Superseded
        }
        Err(error) => TurnDispatchDeliveryOutcome::Rejected(error),
    }
}

#[derive(Debug)]
enum HandoffPreparedTurnDispatchOutcome {
    Delivered,
    Scheduled,
    Superseded,
}

// Admission and its durable receipt are complete. The Stop fence and provider
// enqueue are arbitrated under the same lock; a pending Stop is not yet final.
fn handoff_prepared_turn_dispatch(
    state: &AppState,
    mut dispatch: TurnDispatch,
) -> Result<HandoffPreparedTurnDispatchOutcome, ApiError> {
    let engram_generation = dispatch.engram_dispatch_generation();
    let active_turn_generation = dispatch.active_turn_generation();
    let runtime_token = dispatch.runtime_token().clone();
    let session_id = dispatch.session_id().to_owned();
    let mailbox_notification = dispatch.mailbox_notification().cloned();

    loop {
        let preparation = match &dispatch {
            TurnDispatch::PersistentCodex {
                command,
                service_tier,
                sender,
                ..
            } => Some(state.prepare_codex_service_tier_off_lock(
                &session_id,
                &runtime_token,
                active_turn_generation,
                &command.model,
                service_tier,
                sender,
            )),
            _ => None,
        };
        match preparation {
            None => break,
            Some(Ok(CodexServiceTierPreparation::Ready(tier))) => {
                let TurnDispatch::PersistentCodex {
                    command,
                    service_tier,
                    ..
                } = &mut dispatch
                else {
                    unreachable!("only Codex dispatches prepare a service tier")
                };
                command.service_tier = tier.clone();
                // A deferred replay must not repeat discovery or lose its resolved tier.
                *service_tier = Ok(tier);
                break;
            }
            Some(Ok(CodexServiceTierPreparation::PendingStop)) => {
                let mut inner = state.inner.lock().expect("state mutex poisoned");
                let Some(index) = inner.find_session_index(&session_id) else {
                    return Ok(HandoffPreparedTurnDispatchOutcome::Superseded);
                };
                let record = &mut inner.sessions[index];
                if !record.runtime.matches_runtime_token(&runtime_token)
                    || record.active_turn_generation != active_turn_generation
                    || engram_generation.is_some_and(|generation| {
                        record.engram.dispatch_generation != generation
                            || record.engram.active_grant_id.is_none()
                    })
                {
                    return Ok(HandoffPreparedTurnDispatchOutcome::Superseded);
                }
                if record.runtime_stop_in_progress {
                    if engram_generation.is_some() {
                        record
                            .deferred_stop_callbacks
                            .push(DeferredStopCallback::EngramHandoff {
                                active_turn_generation,
                                dispatch: DeferredEngramHandoff(Arc::new(Mutex::new(Some(
                                    dispatch,
                                )))),
                            });
                        return Ok(HandoffPreparedTurnDispatchOutcome::Scheduled);
                    }
                    return Ok(HandoffPreparedTurnDispatchOutcome::Superseded);
                }
                if record.session.status != SessionStatus::Active {
                    return Ok(HandoffPreparedTurnDispatchOutcome::Superseded);
                }
                // Stop failed between discovery arbitration and callback
                // capture. Retry with the original unresolved Fast intent.
            }
            Some(Ok(CodexServiceTierPreparation::Superseded)) => {
                return Ok(HandoffPreparedTurnDispatchOutcome::Superseded);
            }
            Some(Err(error)) => {
                let detail = format!(
                    "failed to resolve Codex Fast before dispatch: {error:#}. Retry or select Standard with /fast or settings"
                );
                return if record_rejected_turn_dispatch(
                    state,
                    &session_id,
                    &detail,
                    mailbox_notification.as_ref(),
                    engram_generation,
                    &runtime_token,
                    active_turn_generation,
                ) {
                    Err(ApiError::internal(detail))
                } else {
                    Ok(HandoffPreparedTurnDispatchOutcome::Superseded)
                };
            }
        }
    }

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(&session_id);
    let guarded =
        engram_generation.is_some() || matches!(&dispatch, TurnDispatch::PersistentCodex { .. });
    if guarded {
        let Some(index) = index else {
            return Ok(HandoffPreparedTurnDispatchOutcome::Superseded);
        };
        let record = &mut inner.sessions[index];
        if !record.runtime.matches_runtime_token(&runtime_token)
            || record.active_turn_generation != active_turn_generation
            || engram_generation.is_some_and(|generation| {
                record.engram.dispatch_generation != generation
                    || record.engram.active_grant_id.is_none()
            })
        {
            return Ok(HandoffPreparedTurnDispatchOutcome::Superseded);
        }
        if record.runtime_stop_in_progress {
            if engram_generation.is_some() {
                record
                    .deferred_stop_callbacks
                    .push(DeferredStopCallback::EngramHandoff {
                        active_turn_generation,
                        dispatch: DeferredEngramHandoff(Arc::new(Mutex::new(Some(dispatch)))),
                    });
                return Ok(HandoffPreparedTurnDispatchOutcome::Scheduled);
            }
            return Ok(HandoffPreparedTurnDispatchOutcome::Superseded);
        }
        if record.session.status != SessionStatus::Active {
            return Ok(HandoffPreparedTurnDispatchOutcome::Superseded);
        }
    }

    // Channels are unbounded. ACP lifecycle lock order is state -> lifecycle;
    // lifecycle waiters must release that lock before accessing AppState.
    let (agent, sent) = match dispatch {
        TurnDispatch::PersistentClaude {
            command, sender, ..
        } => (
            "Claude",
            sender
                .send(ClaudeRuntimeCommand::Prompt(command))
                .map_err(|error| error.to_string()),
        ),
        TurnDispatch::PersistentCodex {
            command, sender, ..
        } => (
            "Codex",
            sender
                .send(CodexRuntimeCommand::Prompt {
                    session_id: session_id.clone(),
                    command,
                })
                .map_err(|error| error.to_string()),
        ),
        TurnDispatch::PersistentAcp {
            command,
            sender,
            turn_lifecycle,
            ..
        } => {
            set_acp_turn_active(&turn_lifecycle, true);
            let sent = sender
                .send(AcpRuntimeCommand::Prompt(command))
                .map_err(|error| error.to_string());
            if sent.is_err() {
                set_acp_turn_active(&turn_lifecycle, false);
            }
            ("ACP", sent)
        }
    };
    if let Err(error) = sent {
        drop(inner);
        let detail = format!("failed to queue prompt for {agent} session: {error}");
        let rejected = record_rejected_turn_dispatch(
            state,
            &session_id,
            &detail,
            mailbox_notification.as_ref(),
            engram_generation,
            &runtime_token,
            active_turn_generation,
        );
        return if rejected {
            Err(ApiError::internal(detail))
        } else {
            Ok(HandoffPreparedTurnDispatchOutcome::Superseded)
        };
    }
    if engram_generation.is_some() {
        let index = index.expect("guarded handoff owns session");
        if inner.sessions[index]
            .queued_prompts
            .front()
            .is_some_and(QueuedPromptRecord::has_engram_intent)
        {
            let record = inner
                .session_mut_by_index(index)
                .expect("guarded handoff session should be valid");
            record.queued_prompts.pop_front();
            sync_pending_prompts(record);
            if let Err(error) = state.commit_locked(&mut inner) {
                eprintln!("engram> failed persisting provider handoff: {error:#}");
            }
        }
    }
    drop(inner);
    state.acknowledge_engram_context_nudge_delivery(&session_id, active_turn_generation);
    if let Some(notification) = mailbox_notification.as_ref() {
        state.mark_mailbox_notification_delivered(notification);
    }
    Ok(HandoffPreparedTurnDispatchOutcome::Delivered)
}

#[derive(Debug)]
struct RemoteLifecycleActionGuard {
    remote_id: String,
    in_flight: Arc<Mutex<HashSet<String>>>,
}

impl Drop for RemoteLifecycleActionGuard {
    fn drop(&mut self) {
        let mut in_flight = self
            .in_flight
            .lock()
            .expect("remote lifecycle action mutex poisoned");
        in_flight.remove(&self.remote_id);
    }
}

fn dispatch_turn_and_snapshot(
    state: &AppState,
    session_id: &str,
    request: SendMessageRequest,
) -> Result<SendMessageRouteResponse, ApiError> {
    let is_peer_message = request.source_session_id.is_some();
    let dispatch = state.dispatch_turn(session_id, request)?;
    let message_disposition = is_peer_message.then(|| match &dispatch {
        DispatchTurnResult::Dispatched(_) => PeerMessageDisposition::DeliveredToIdleSession,
        DispatchTurnResult::DispatchedAfterQueue(_) | DispatchTurnResult::Queued => {
            PeerMessageDisposition::QueuedBehindActiveTurn
        }
    });
    match dispatch {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => {
            deliver_turn_dispatch(state, dispatch).into_public_result()?;
        }
        DispatchTurnResult::Queued => {}
    }
    Ok(SendMessageRouteResponse {
        state: state.summary_snapshot(),
        message_disposition,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SendMessageRouteResponse {
    #[serde(flatten)]
    state: StateResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_disposition: Option<PeerMessageDisposition>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
enum PeerMessageDisposition {
    DeliveredToIdleSession,
    QueuedBehindActiveTurn,
}

/// Returns the backend health response.
async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        server_instance_id: state.server_instance_id.clone(),
    })
}

/// Gets state.
///
/// Builds AND serializes the snapshot inside `spawn_blocking` so the tokio
/// worker does not spend milliseconds-to-seconds of CPU running
/// `serde_json::to_writer` on a `Vec<Session>` that contains every session's
/// full `Vec<Message>`. The worker thread only handles the pre-serialized
/// `Vec<u8>` body, which is a fixed-cost hand-off to hyper.
async fn get_state(State(state): State<AppState>) -> Result<Response, ApiError> {
    let body = run_blocking_api(move || {
        let snapshot = state.summary_snapshot();
        serde_json::to_vec(&snapshot)
            .map_err(|err| ApiError::internal(format!("failed to serialize state: {err}")))
    })
    .await?;
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        body,
    )
        .into_response())
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetSessionQuery {
    #[serde(default)]
    tail: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetSessionHistoryQuery {
    #[serde(default)]
    before: Option<String>,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    around: Option<usize>,
    #[serde(default)]
    from: Option<String>,
    #[serde(default = "default_session_history_page_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetSessionOverviewQuery {
    #[serde(default = "default_session_overview_bucket_count")]
    buckets: usize,
}

fn default_session_history_page_limit() -> usize {
    SESSION_HISTORY_PAGE_MAX_MESSAGES
}

fn default_session_overview_bucket_count() -> usize {
    SESSION_OVERVIEW_DEFAULT_BUCKETS
}

/// Gets one bounded recent suffix of a session.
async fn get_session(
    State(state): State<AppState>,
    AxumPath(session_id): AxumPath<String>,
    query: Result<Query<GetSessionQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(query) =
        query.map_err(|rejection| api_query_rejection("session query", rejection))?;
    if matches!(query.tail, Some(0)) {
        return Err(ApiError::bad_request("session tail must be at least 1"));
    }
    if let Some(message_limit) = query.tail {
        if message_limit > SESSION_TAIL_HYDRATION_MAX_MESSAGES {
            return Err(ApiError::bad_request(format!(
                "session tail must be at most {SESSION_TAIL_HYDRATION_MAX_MESSAGES}"
            )));
        }
    }

    let body = run_blocking_api(move || {
        let response = state.get_session_tail(
            &session_id,
            query.tail.unwrap_or(SESSION_TAIL_DEFAULT_MESSAGES),
        )?;
        serde_json::to_vec(&response)
            .map_err(|err| ApiError::internal(format!("failed to serialize session: {err}")))
    })
    .await?;
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        body,
    )
        .into_response())
}

/// Gets one bounded page of transcript history before an optional message id.
async fn get_session_history(
    State(state): State<AppState>,
    AxumPath(session_id): AxumPath<String>,
    query: Result<Query<GetSessionHistoryQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(query) =
        query.map_err(|rejection| api_query_rejection("session history query", rejection))?;
    if query.limit == 0 {
        return Err(ApiError::bad_request(
            "session history limit must be at least 1",
        ));
    }
    if query.limit > SESSION_HISTORY_PAGE_MAX_MESSAGES {
        return Err(ApiError::bad_request(format!(
            "session history limit must be at most {SESSION_HISTORY_PAGE_MAX_MESSAGES}"
        )));
    }
    if query.before.as_deref().is_some_and(str::is_empty) {
        return Err(ApiError::bad_request(
            "session history before cursor must not be empty",
        ));
    }
    if query.after.as_deref().is_some_and(str::is_empty) {
        return Err(ApiError::bad_request(
            "session history after cursor must not be empty",
        ));
    }
    let selector_count = usize::from(query.before.is_some())
        + usize::from(query.after.is_some())
        + usize::from(query.around.is_some())
        + usize::from(query.from.is_some());
    if selector_count > 1 {
        return Err(ApiError::bad_request(
            "session history accepts only one of before, after, around, or from",
        ));
    }
    if query.from.as_deref().is_some_and(|from| from != "start") {
        return Err(ApiError::bad_request(
            "session history from must be `start` when provided",
        ));
    }
    let body = run_blocking_api(move || {
        let response = state.get_session_history(
            &session_id,
            query.before.as_deref(),
            query.after.as_deref(),
            query.around,
            query.from.as_deref() == Some("start"),
            query.limit,
        )?;
        serde_json::to_vec(&response).map_err(|err| {
            ApiError::internal(format!("failed to serialize session history page: {err}"))
        })
    })
    .await?;
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        body,
    )
        .into_response())
}

/// Gets one whole-conversation, position-linear overview.
async fn get_session_overview(
    State(state): State<AppState>,
    AxumPath(session_id): AxumPath<String>,
    query: Result<Query<GetSessionOverviewQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(query) =
        query.map_err(|rejection| api_query_rejection("session overview query", rejection))?;
    if query.buckets == 0 {
        return Err(ApiError::bad_request(
            "session overview buckets must be at least 1",
        ));
    }
    if query.buckets > SESSION_OVERVIEW_MAX_BUCKETS {
        return Err(ApiError::bad_request(format!(
            "session overview buckets must be at most {SESSION_OVERVIEW_MAX_BUCKETS}"
        )));
    }
    let body = run_blocking_api(move || {
        let response = state.get_session_overview(&session_id, query.buckets)?;
        serde_json::to_vec(&response).map_err(|err| {
            ApiError::internal(format!("failed to serialize session overview: {err}"))
        })
    })
    .await?;
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        body,
    )
        .into_response())
}

/// Lists workspace layouts.
async fn list_workspace_layouts(
    State(state): State<AppState>,
) -> Result<Json<WorkspaceLayoutsResponse>, ApiError> {
    let response = run_blocking_api(move || state.list_workspace_layouts()).await?;
    Ok(Json(response))
}

/// Gets workspace layout.
async fn get_workspace_layout(
    State(state): State<AppState>,
    AxumPath(workspace_id): AxumPath<String>,
) -> Result<Json<WorkspaceLayoutResponse>, ApiError> {
    let response = run_blocking_api(move || state.get_workspace_layout(&workspace_id)).await?;
    Ok(Json(response))
}

/// Stores a workspace layout.
///
/// Intentionally returns the saved document, while DELETE on the same route
/// returns the remaining summaries. Save callers may need the full persisted
/// document; delete callers only need the switcher list.
async fn put_workspace_layout(
    State(state): State<AppState>,
    AxumPath(workspace_id): AxumPath<String>,
    Json(request): Json<PutWorkspaceLayoutRequest>,
) -> Result<Json<WorkspaceLayoutResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.put_workspace_layout(&workspace_id, request)).await?;
    Ok(Json(response))
}

async fn patch_workspace_label(
    State(state): State<AppState>,
    AxumPath(workspace_id): AxumPath<String>,
    Json(request): Json<PatchWorkspaceLabelRequest>,
) -> Result<Json<WorkspaceLayoutResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.patch_workspace_label(&workspace_id, request)).await?;
    Ok(Json(response))
}

/// Deletes a workspace layout.
///
/// Intentionally returns the remaining workspace summaries, while PUT on the
/// same route returns the single saved document. See put_workspace_layout for
/// the rationale behind the asymmetric response shapes.
async fn delete_workspace_layout(
    State(state): State<AppState>,
    AxumPath(workspace_id): AxumPath<String>,
) -> Result<Json<WorkspaceLayoutsResponse>, ApiError> {
    let response = run_blocking_api(move || state.delete_workspace_layout(&workspace_id)).await?;
    Ok(Json(response))
}

impl AppState {
    /// Returns the project digest payload rendered for the Telegram
    /// bot and mobile-dashboard surfaces. Thin wrapper around
    /// [`Self::build_project_digest_summary`] that converts into
    /// the wire response shape.
    fn project_digest(&self, project_id: &str) -> Result<ProjectDigestResponse, ApiError> {
        Ok(self
            .build_project_digest_summary(project_id)?
            .into_response())
    }

    /// Runs a digest action (approve / reject / fix-it / continue / stop)
    /// for a project. Validates the action is still in the current
    /// `proposed_actions` set before dispatching — rejects with 409
    /// if the project state has advanced and the requested action is
    /// no longer valid.
    fn execute_project_action(
        &self,
        project_id: &str,
        action_id: &str,
    ) -> Result<ProjectDigestResponse, ApiError> {
        let action = ProjectActionId::parse(action_id)?;
        let summary = self.build_project_digest_summary(project_id)?;
        if !summary.proposed_actions.contains(&action) {
            return Err(ApiError::conflict(format!(
                "action `{}` is not currently available for project `{}`",
                action.as_str(),
                summary.headline
            )));
        }

        match action {
            ProjectActionId::Approve => {
                let target = summary
                    .pending_approval_target
                    .ok_or_else(|| ApiError::conflict("project does not have a live approval"))?;
                let _ = self.update_approval(
                    &target.session_id,
                    &target.message_id,
                    ApprovalDecision::Accepted,
                )?;
            }
            ProjectActionId::Reject => {
                let target = summary
                    .pending_approval_target
                    .ok_or_else(|| ApiError::conflict("project does not have a live approval"))?;
                let _ = self.update_approval(
                    &target.session_id,
                    &target.message_id,
                    ApprovalDecision::Rejected,
                )?;
            }
            ProjectActionId::Continue
            | ProjectActionId::FixIt
            | ProjectActionId::KeepIterating
            | ProjectActionId::AskAgentToCommit => {
                let session_id = summary.primary_session_id.clone().ok_or_else(|| {
                    ApiError::conflict("project does not have a session to target")
                })?;
                let prompt = action
                    .prompt()
                    .ok_or_else(|| ApiError::internal("project action prompt is missing"))?;
                let dispatch = self.dispatch_turn(
                    &session_id,
                    SendMessageRequest {
                        text: prompt.to_owned(),
                        expanded_text: None,
                        attachments: Vec::new(),
                        source_session_id: None,
                        source_mailbox: None,
                    },
                )?;
                match dispatch {
                    DispatchTurnResult::Dispatched(dispatch)
                    | DispatchTurnResult::DispatchedAfterQueue(dispatch) => {
                        deliver_turn_dispatch(self, dispatch).into_public_result()?;
                    }
                    DispatchTurnResult::Queued => {}
                }
            }
            ProjectActionId::Stop => {
                let session_id = summary
                    .primary_session_id
                    .clone()
                    .ok_or_else(|| ApiError::conflict("project does not have a session to stop"))?;
                let _ = self.request_stop_session(&session_id)?;
            }
            ProjectActionId::ReviewInTermal => {}
        }

        self.project_digest(project_id)
    }

    fn remote_config_for_action(&self, remote_id: &str) -> Result<RemoteConfig, ApiError> {
        validate_remote_id_value(remote_id)?;
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner
            .find_remote(remote_id)
            .cloned()
            .ok_or_else(|| ApiError::not_found(format!("remote `{remote_id}` not found")))
    }

    fn begin_remote_lifecycle_action(
        &self,
        remote_id: &str,
        remote_name: &str,
    ) -> Result<RemoteLifecycleActionGuard, ApiError> {
        let mut in_flight = self
            .remote_lifecycle_actions_in_flight
            .lock()
            .expect("remote lifecycle action mutex poisoned");
        if !in_flight.insert(remote_id.to_owned()) {
            return Err(ApiError::conflict(format!(
                "remote `{remote_name}` already has a lifecycle action running"
            )));
        }
        Ok(RemoteLifecycleActionGuard {
            remote_id: remote_id.to_owned(),
            in_flight: self.remote_lifecycle_actions_in_flight.clone(),
        })
    }

    fn register_remote_termal(
        &self,
        remote_id: &str,
        request: RemoteRegisterRequest,
    ) -> Result<RemoteActionResponse, ApiError> {
        let remote = self.remote_config_for_action(remote_id)?;
        let script = remote_register_script(&request.source_path)?;
        let _guard = self.begin_remote_lifecycle_action(&remote.id, &remote.name)?;
        run_remote_ssh_script(&remote, "registration", &script)
    }

    fn upgrade_remote_termal(&self, remote_id: &str) -> Result<RemoteActionResponse, ApiError> {
        let remote = self.remote_config_for_action(remote_id)?;
        let _guard = self.begin_remote_lifecycle_action(&remote.id, &remote.name)?;
        upgrade_remote_ssh(&remote)
    }

    /// Builds project digest summary.
    fn build_project_digest_summary(
        &self,
        project_id: &str,
    ) -> Result<ProjectDigestSummary, ApiError> {
        let inputs = self.project_digest_inputs(project_id)?;
        let git_status = self.load_project_git_status_best_effort(&inputs.project);
        let pending_approval = find_latest_project_pending_approval(&inputs.sessions);
        let pending_interaction = if pending_approval.is_none() {
            find_latest_project_pending_nonapproval_interaction(&inputs.sessions)
        } else {
            None
        };
        let error_session = if pending_approval.is_none() && pending_interaction.is_none() {
            inputs
                .sessions
                .iter()
                .rev()
                .find(|record| record.status == SessionStatus::Error)
        } else {
            None
        };
        let active_session = if pending_approval.is_none()
            && pending_interaction.is_none()
            && error_session.is_none()
        {
            inputs
                .sessions
                .iter()
                .rev()
                .find(|record| record.status == SessionStatus::Active)
        } else {
            None
        };
        let prompt_target_session = latest_project_prompt_target_session(&inputs.sessions);
        let summary_session = pending_approval
            .as_ref()
            .map(|(record, _)| *record)
            .or_else(|| pending_interaction.as_ref().map(|(record, _)| *record))
            .or(error_session)
            .or(active_session)
            .or_else(|| {
                inputs
                    .sessions
                    .iter()
                    .rev()
                    .find(|record| record.has_messages)
            })
            .or_else(|| inputs.sessions.last());
        let summary_session_id = summary_session.map(|record| record.id.clone());
        let prompt_target_session_id = prompt_target_session.map(|record| record.id.clone());
        let summary_deep_link = Some(build_project_deep_link(
            &inputs.project.id,
            summary_session_id.as_deref(),
        ));
        let worktree_dirty = git_status.as_ref().is_some_and(|status| !status.is_clean);

        if let Some((record, message_id)) = pending_approval {
            let (done_summary, mut source_message_ids) =
                select_project_done_summary(summary_session, git_status.as_ref(), false);
            if !source_message_ids.contains(&message_id) {
                source_message_ids.insert(0, message_id.clone());
            }
            return Ok(ProjectDigestSummary {
                headline: inputs.project.name,
                project_id: inputs.project.id,
                primary_session_id: summary_session_id,
                done_summary: normalize_project_text(
                    &done_summary,
                    "Work paused while waiting for approval.",
                ),
                current_status: "Waiting on your decision.".to_owned(),
                proposed_actions: vec![
                    ProjectActionId::Approve,
                    ProjectActionId::Reject,
                    ProjectActionId::ReviewInTermal,
                ],
                deep_link: summary_deep_link,
                pending_approval_target: Some(ProjectApprovalTarget {
                    session_id: record.id.clone(),
                    message_id,
                }),
                source_message_ids,
            });
        }

        if let Some((record, message_id)) = pending_interaction {
            let (done_summary, mut source_message_ids) =
                select_project_done_summary(summary_session, git_status.as_ref(), false);
            if !source_message_ids.contains(&message_id) {
                source_message_ids.insert(0, message_id);
            }
            let mut proposed_actions = vec![ProjectActionId::ReviewInTermal];
            if summary_session_id.is_some() {
                proposed_actions.push(ProjectActionId::Stop);
            }
            return Ok(ProjectDigestSummary {
                headline: inputs.project.name,
                project_id: inputs.project.id,
                primary_session_id: summary_session_id,
                done_summary: normalize_project_text(
                    &done_summary,
                    "Work is waiting on a response in TermAl.",
                ),
                current_status: normalize_project_text(
                    &record.preview,
                    "Waiting on input in TermAl.",
                ),
                proposed_actions,
                deep_link: summary_deep_link,
                pending_approval_target: None,
                source_message_ids,
            });
        }

        if let Some(record) = error_session {
            let (done_summary, source_message_ids) =
                select_project_done_summary(summary_session, git_status.as_ref(), false);
            let mut proposed_actions = vec![ProjectActionId::ReviewInTermal];
            let action_target_session_id = prompt_target_session_id.clone();
            let action_target_deep_link = Some(build_project_deep_link(
                &inputs.project.id,
                action_target_session_id.as_deref(),
            ));
            if action_target_session_id.is_some() {
                proposed_actions.insert(0, ProjectActionId::FixIt);
            }
            return Ok(ProjectDigestSummary {
                headline: inputs.project.name,
                project_id: inputs.project.id,
                primary_session_id: action_target_session_id,
                done_summary: normalize_project_text(
                    &done_summary,
                    "The last turn ended in an error.",
                ),
                current_status: normalize_project_text(&record.preview, "Needs attention."),
                proposed_actions,
                deep_link: action_target_deep_link,
                pending_approval_target: None,
                source_message_ids,
            });
        }

        if let Some(record) = active_session {
            let (done_summary, source_message_ids) =
                select_project_done_summary(summary_session, git_status.as_ref(), false);
            return Ok(ProjectDigestSummary {
                headline: inputs.project.name,
                project_id: inputs.project.id,
                primary_session_id: summary_session_id,
                done_summary: normalize_project_text(&done_summary, "The agent is still working."),
                current_status: active_project_status_text(record),
                proposed_actions: vec![ProjectActionId::Stop, ProjectActionId::ReviewInTermal],
                deep_link: summary_deep_link,
                pending_approval_target: None,
                source_message_ids,
            });
        }

        if worktree_dirty {
            let (done_summary, source_message_ids) =
                select_project_done_summary(summary_session, git_status.as_ref(), true);
            let mut proposed_actions = vec![ProjectActionId::ReviewInTermal];
            if prompt_target_session_id.is_some() {
                proposed_actions.push(ProjectActionId::AskAgentToCommit);
                proposed_actions.push(ProjectActionId::KeepIterating);
            }
            let action_target_session_id = prompt_target_session_id.clone();
            let action_target_deep_link = Some(build_project_deep_link(
                &inputs.project.id,
                action_target_session_id.as_deref(),
            ));
            return Ok(ProjectDigestSummary {
                headline: inputs.project.name,
                project_id: inputs.project.id,
                primary_session_id: action_target_session_id,
                done_summary: normalize_project_text(
                    &done_summary,
                    "The working tree has changes ready for review.",
                ),
                current_status: "Changes are ready for review.".to_owned(),
                proposed_actions,
                deep_link: action_target_deep_link,
                pending_approval_target: None,
                source_message_ids,
            });
        }

        let (done_summary, source_message_ids) =
            select_project_done_summary(summary_session, git_status.as_ref(), false);
        let action_target_session_id = prompt_target_session_id;
        let action_target_deep_link = Some(build_project_deep_link(
            &inputs.project.id,
            action_target_session_id.as_deref(),
        ));
        let proposed_actions = if action_target_session_id.is_some() {
            vec![ProjectActionId::Continue, ProjectActionId::ReviewInTermal]
        } else {
            vec![ProjectActionId::ReviewInTermal]
        };
        Ok(ProjectDigestSummary {
            headline: inputs.project.name,
            project_id: inputs.project.id,
            primary_session_id: action_target_session_id,
            done_summary: normalize_project_text(&done_summary, "No agent work has started yet."),
            current_status: "Idle and unblocked.".to_owned(),
            proposed_actions,
            deep_link: action_target_deep_link,
            pending_approval_target: None,
            source_message_ids,
        })
    }

    /// Collects the `ProjectDigestInputs` bundle (project metadata
    /// + visible sessions + orchestrator instances) under a single
    /// state-mutex acquisition so the caller can compute the digest
    /// without re-locking per field.
    fn project_digest_inputs(&self, project_id: &str) -> Result<ProjectDigestInputs, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let project = inner
            .find_project(project_id)
            .cloned()
            .ok_or_else(|| ApiError::not_found("project not found"))?;
        let sessions = inner
            .sessions
            .iter()
            .filter(|record| {
                !record.hidden && record.session.project_id.as_deref() == Some(project_id)
            })
            .map(project_digest_session_from_record)
            .collect();
        Ok(ProjectDigestInputs { project, sessions })
    }

    /// Loads project Git status best effort.
    fn load_project_git_status_best_effort(&self, project: &Project) -> Option<GitStatusResponse> {
        if project.remote_id == LOCAL_REMOTE_ID {
            return load_git_status_for_path(FsPath::new(&project.root_path)).ok();
        }
        let scope = self
            .remote_scope_for_request(None, Some(project.id.as_str()))
            .ok()
            .flatten()?;
        self.remote_get_json(
            &scope,
            "/api/git/status",
            vec![("path".to_owned(), project.root_path.clone())],
        )
        .ok()
    }
}

fn latest_project_prompt_target_session(
    sessions: &[ProjectDigestSession],
) -> Option<&ProjectDigestSession> {
    // Keep this aligned with `find_latest_telegram_project_prompt_session`;
    // both choose the automatic Telegram prompt target when the digest primary
    // points at a delegated child or another non-promptable session.
    sessions.iter().rev().find(|record| {
        record.parent_delegation_id.is_none() && record.status != SessionStatus::Error
    })
}

/// Runs blocking API.
async fn run_blocking_api<T, F>(operation: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|err| ApiError::internal(format!("blocking task failed: {err}")))?
}

fn api_json_rejection(label: &str, rejection: JsonRejection) -> ApiError {
    let status = match &rejection {
        JsonRejection::JsonDataError(_) | JsonRejection::JsonSyntaxError(_) => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        _ => rejection.status(),
    };
    ApiError::from_status(
        status,
        format!("invalid {label} JSON: {}", rejection.body_text()),
    )
}

fn api_query_rejection(label: &str, rejection: QueryRejection) -> ApiError {
    ApiError::from_status(
        rejection.status(),
        format!("invalid {label}: {}", rejection.body_text()),
    )
}

/// Creates session.
async fn create_session(
    State(state): State<AppState>,
    Json(request): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<CreateSessionResponse>), ApiError> {
    let response = run_blocking_api(move || state.create_session(request)).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// Creates a Phase 1 read-only child delegation session.
async fn create_session_delegation(
    AxumPath(parent_session_id): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<CreateDelegationRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<DelegationResponse>), ApiError> {
    let Json(request) =
        request.map_err(|rejection| api_json_rejection("delegation request", rejection))?;
    let response =
        run_blocking_api(move || state.create_read_only_delegation(&parent_session_id, request))
            .await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// Lists compact delegation metadata owned by one parent session.
async fn list_session_delegations(
    AxumPath(parent_session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<DelegationListResponse>, ApiError> {
    let response = run_blocking_api(move || state.list_delegations(&parent_session_id)).await?;
    Ok(Json(response))
}

/// Gets delegation status and metadata.
async fn get_delegation_status(
    AxumPath((parent_session_id, delegation_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<DelegationStatusResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.get_delegation(&parent_session_id, &delegation_id)).await?;
    Ok(Json(response))
}

/// Gets a completed delegation result packet.
async fn get_delegation_result(
    AxumPath((parent_session_id, delegation_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<DelegationResultResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.get_delegation_result(&parent_session_id, &delegation_id))
            .await?;
    Ok(Json(response))
}

/// Gets one bounded UTF-8-safe page of the authoritative final child output.
async fn get_delegation_result_output(
    AxumPath((parent_session_id, delegation_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    query: Result<Query<DelegationResultOutputQuery>, QueryRejection>,
) -> Result<Json<DelegationResultOutputResponse>, ApiError> {
    let Query(query) = query
        .map_err(|rejection| api_query_rejection("delegation result output query", rejection))?;
    let response = run_blocking_api(move || {
        state.get_delegation_result_output(
            &parent_session_id,
            &delegation_id,
            query.offset_bytes,
            query.limit_bytes,
        )
    })
    .await?;
    Ok(Json(response))
}

/// Cancels a running delegation child session.
async fn cancel_delegation(
    AxumPath((parent_session_id, delegation_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<DelegationStatusResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.cancel_delegation(&parent_session_id, &delegation_id))
            .await?;
    Ok(Json(response))
}

/// Delivers a follow-up prompt to a completed delegation, resuming its child session.
async fn followup_delegation(
    AxumPath((parent_session_id, delegation_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<FollowupDelegationRequest>,
) -> Result<Json<DelegationStatusResponse>, ApiError> {
    let response = run_blocking_api(move || {
        state.followup_delegation(&parent_session_id, &delegation_id, request.message)
    })
    .await?;
    Ok(Json(response))
}

/// Schedules a parent resume after one or more delegations become terminal.
async fn create_delegation_wait(
    AxumPath(parent_session_id): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<CreateDelegationWaitRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<DelegationWaitResponse>), ApiError> {
    let Json(request) =
        request.map_err(|rejection| api_json_rejection("delegation wait request", rejection))?;
    let response =
        run_blocking_api(move || state.create_delegation_wait(&parent_session_id, request)).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// Lists conversation markers for one session.
async fn list_session_markers(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<ConversationMarkersResponse>, ApiError> {
    let response = run_blocking_api(move || state.list_conversation_markers(&session_id)).await?;
    Ok(Json(response))
}

/// Creates a conversation marker.
async fn create_session_marker(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<CreateConversationMarkerRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ConversationMarkerResponse>), ApiError> {
    let Json(request) = request
        .map_err(|rejection| api_json_rejection("conversation marker request", rejection))?;
    let response =
        run_blocking_api(move || state.create_conversation_marker(&session_id, request)).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// Updates a conversation marker.
async fn update_session_marker(
    AxumPath((session_id, marker_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    request: Result<Json<UpdateConversationMarkerRequest>, JsonRejection>,
) -> Result<Json<ConversationMarkerResponse>, ApiError> {
    let Json(request) = request
        .map_err(|rejection| api_json_rejection("conversation marker request", rejection))?;
    let response = run_blocking_api(move || {
        state.update_conversation_marker(&session_id, &marker_id, request)
    })
    .await?;
    Ok(Json(response))
}

/// Deletes a conversation marker.
async fn delete_session_marker(
    AxumPath((session_id, marker_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<DeleteConversationMarkerResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.delete_conversation_marker(&session_id, &marker_id)).await?;
    Ok(Json(response))
}

/// Creates project.
async fn create_project(
    State(state): State<AppState>,
    Json(request): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<CreateProjectResponse>), ApiError> {
    let response = run_blocking_api(move || state.create_project(request)).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// Deletes project.
async fn delete_project(
    AxumPath(project_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || state.delete_project(&project_id)).await?;
    Ok(Json(response))
}

/// Patches a project's Engram host-adapter settings after validating the
/// external installation without holding TermAl's global state mutex.
async fn update_project_engram_settings(
    AxumPath(project_id): AxumPath<String>,
    State(state): State<AppState>,
    axum::Extension(limiter): axum::Extension<EngramReadinessLimiter>,
    request: Result<Json<UpdateProjectEngramSettingsRequest>, JsonRejection>,
) -> Result<Json<StateResponse>, ApiError> {
    let Json(request) =
        request.map_err(|rejection| api_json_rejection("Engram project settings", rejection))?;
    // Disable remains an unconditional recovery path, including under load.
    let permit = if request.enabled {
        Some(acquire_engram_readiness(limiter)?)
    } else {
        None
    };
    let response = run_blocking_api(move || {
        let _permit = permit;
        state.patch_project_engram_settings(&project_id, request)
    })
    .await?;
    Ok(Json(response))
}

/// Verifies proposed Engram settings without changing the project. The
/// response is redacted and the subsequent PATCH repeats all validation.
async fn verify_project_engram_settings(
    AxumPath(project_id): AxumPath<String>,
    State(state): State<AppState>,
    axum::Extension(limiter): axum::Extension<EngramReadinessLimiter>,
    request: Result<Json<UpdateProjectEngramSettingsRequest>, JsonRejection>,
) -> Result<Json<VerifyProjectEngramSettingsResponse>, ApiError> {
    let Json(request) = request
        .map_err(|rejection| api_json_rejection("Engram settings verification", rejection))?;
    let permit = acquire_engram_readiness(limiter)?;
    let response = run_blocking_api(move || {
        let _permit = permit;
        state.verify_project_engram_settings(&project_id, request)
    })
    .await?;
    Ok(Json(response))
}

/// Waives one exact open Engram obligation on the session's currently bound
/// work run. Policy refusals remain typed successful responses; routing and
/// idempotency faults surface as conflicts.
async fn waive_engram_obligation(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<WaiveEngramObligationRequest>, JsonRejection>,
) -> Result<Json<EngramObligationWaiverDecisionResponse>, ApiError> {
    let Json(request) =
        request.map_err(|rejection| api_json_rejection("Engram obligation waiver", rejection))?;
    let response =
        run_blocking_api(move || state.waive_engram_obligation(&session_id, request)).await?;
    Ok(Json(response))
}

/// Updates the machine-scoped Engram executable and home. Repository
/// declarations and per-project tier selection are separate contracts.
async fn update_engram_host_settings(
    State(state): State<AppState>,
    request: Result<Json<UpdateEngramHostSettingsRequest>, JsonRejection>,
) -> Result<Json<StateResponse>, ApiError> {
    let Json(request) =
        request.map_err(|rejection| api_json_rejection("Engram host settings", rejection))?;
    let response = run_blocking_api(move || state.update_engram_host_settings(request)).await?;
    Ok(Json(response))
}

/// Gets project digest.
async fn get_project_digest(
    AxumPath(project_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<ProjectDigestResponse>, ApiError> {
    if !PROJECT_DIGESTS_ENABLED {
        return Err(ApiError::not_found(
            "project digests are temporarily disabled",
        ));
    }
    let response = run_blocking_api(move || state.project_digest(&project_id)).await?;
    Ok(Json(response))
}

/// Dispatches project action.
async fn dispatch_project_action(
    AxumPath((project_id, action_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<ProjectDigestResponse>, ApiError> {
    if !PROJECT_DIGESTS_ENABLED {
        return Err(ApiError::not_found(
            "project digest actions are temporarily disabled",
        ));
    }
    let response =
        run_blocking_api(move || state.execute_project_action(&project_id, &action_id)).await?;
    Ok(Json(response))
}

/// Updates app settings.
async fn update_app_settings(
    State(state): State<AppState>,
    Json(request): Json<UpdateAppSettingsRequest>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || state.update_app_settings(request)).await?;
    Ok(Json(response))
}

async fn register_remote_termal(
    AxumPath(remote_id): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<RemoteRegisterRequest>, JsonRejection>,
) -> Result<Json<RemoteActionResponse>, ApiError> {
    let Json(request) =
        request.map_err(|rejection| api_json_rejection("remote register request", rejection))?;
    let response =
        run_blocking_api(move || state.register_remote_termal(&remote_id, request)).await?;
    Ok(Json(response))
}

async fn upgrade_remote_termal(
    AxumPath(remote_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<RemoteActionResponse>, ApiError> {
    let response = run_blocking_api(move || state.upgrade_remote_termal(&remote_id)).await?;
    Ok(Json(response))
}

/// Picks project root.
async fn pick_project_root(
    State(state): State<AppState>,
) -> Result<Json<PickProjectRootResponse>, ApiError> {
    let default_workdir = state.default_workdir.clone();
    let path = tokio::task::spawn_blocking(move || pick_project_root_path(&default_workdir))
        .await
        .map_err(|err| ApiError::internal(format!("folder picker task failed: {err}")))??;
    Ok(Json(PickProjectRootResponse { path }))
}

/// Updates session settings.
async fn update_session_settings(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    Json(request): Json<UpdateSessionSettingsRequest>,
) -> Result<Json<StateResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.update_session_settings(&session_id, request)).await?;
    Ok(Json(response))
}

/// Refreshes session model options.
async fn refresh_session_model_options(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.refresh_session_model_options(&session_id)).await?;
    Ok(Json(response))
}

/// Lists the MCP servers visible to the owning Codex app-server.
async fn list_codex_mcp_servers(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<CodexMcpServersResponse>, ApiError> {
    let response = run_blocking_api(move || state.list_codex_mcp_servers(&session_id)).await?;
    Ok(Json(response))
}

/// Forks Codex thread.
async fn fork_codex_thread(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<(StatusCode, Json<CreateSessionResponse>), ApiError> {
    let response = run_blocking_api(move || state.fork_codex_thread(&session_id)).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// Archives Codex thread.
async fn archive_codex_thread(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || state.archive_codex_thread(&session_id)).await?;
    Ok(Json(response))
}

/// Unarchives Codex thread.
async fn unarchive_codex_thread(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || state.unarchive_codex_thread(&session_id)).await?;
    Ok(Json(response))
}

/// Compacts Codex thread.
async fn compact_codex_thread(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || state.compact_codex_thread(&session_id)).await?;
    Ok(Json(response))
}

/// Rolls back Codex thread.
async fn rollback_codex_thread(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    Json(request): Json<CodexThreadRollbackRequest>,
) -> Result<Json<StateResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.rollback_codex_thread(&session_id, request.num_turns))
            .await?;
    Ok(Json(response))
}

async fn send_message(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    Json(request): Json<SendMessageRequest>,
) -> Result<(StatusCode, Json<SendMessageRouteResponse>), ApiError> {
    let snapshot = run_blocking_api({
        let state = state.clone();
        let session_id = session_id.clone();
        move || {
            // Explicit UI input may reopen a finished child. Runtime queues and
            // peer deliveries still pass through the terminal dispatch fence.
            let followup = if request.source_session_id.is_none()
                && request.source_mailbox.is_none()
            {
                let inner = state.inner.lock().expect("state mutex poisoned");
                inner
                    .delegations
                    .iter()
                    .find(|delegation| {
                        delegation.child_session_id == session_id
                            && delegation.agent == Agent::Codex
                            && matches!(
                                delegation.status,
                                DelegationStatus::Completed | DelegationStatus::Failed
                            )
                    })
                    .map(|delegation| (delegation.parent_session_id.clone(), delegation.id.clone()))
            } else {
                None
            };
            if let Some((parent, delegation)) = followup {
                state.followup_delegation_request(&parent, &delegation, request)?;
                Ok(SendMessageRouteResponse {
                    state: state.summary_snapshot(),
                    message_disposition: None,
                })
            } else {
                dispatch_turn_and_snapshot(&state, &session_id, request)
            }
        }
    })
    .await?;

    Ok((StatusCode::ACCEPTED, Json(snapshot)))
}

/// Cancels queued prompt.
async fn cancel_queued_prompt(
    AxumPath((session_id, prompt_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.cancel_queued_prompt(&session_id, &prompt_id)).await?;
    Ok(Json(response))
}

/// Resumes a queue paused by Stop.
async fn resume_session_queue(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || state.resume_session_queue(&session_id)).await?;
    Ok(Json(response))
}

/// Stops session.
async fn stop_session(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || state.request_stop_session(&session_id)).await?;
    Ok(Json(response))
}

/// Kills session.
async fn kill_session(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || state.kill_session(&session_id)).await?;
    Ok(Json(response))
}

/// Submits approval.
async fn submit_approval(
    AxumPath((session_id, message_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<ApprovalRequest>,
) -> Result<Json<StateResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.update_approval(&session_id, &message_id, request.decision))
            .await?;
    Ok(Json(response))
}

/// Submits user input.
async fn submit_user_input(
    AxumPath((session_id, message_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<UserInputSubmissionRequest>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || {
        state.submit_user_input(&session_id, &message_id, request.answers, request.declined)
    })
    .await?;
    Ok(Json(response))
}

/// Submits MCP elicitation.
async fn submit_mcp_elicitation(
    AxumPath((session_id, message_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<McpElicitationSubmissionRequest>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || {
        state.submit_codex_mcp_elicitation(
            &session_id,
            &message_id,
            request.action,
            request.content,
        )
    })
    .await?;
    Ok(Json(response))
}

/// Submits Codex app request.
async fn submit_codex_app_request(
    AxumPath((session_id, message_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<CodexAppRequestSubmissionRequest>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || {
        state.submit_codex_app_request(&session_id, &message_id, request.result)
    })
    .await?;
    Ok(Json(response))
}
