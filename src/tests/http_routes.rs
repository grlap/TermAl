// End-to-end HTTP route integration tests. Every case spins up the real
// `axum::Router` returned by `app_router(state)`, fires an actual HTTP
// request through `tower::ServiceExt`, and parses the real JSON or SSE
// response bytes — no handler is called directly.
//
// Contrast with the domain-specific submodules (sessions, orchestrators,
// workspaces, ...), which test production logic via direct `AppState`
// method calls. This module instead confirms the router wires request
// shapes, extractors, and response types (`StatusCode`,
// `CreateSessionResponse`, `SessionResponse`, `StateResponse`) correctly.
// SSE cases use `collect_sse_events` to drain the event stream and verify
// initial-state + delta ordering. The Codex thread action routes proxy to
// real `SharedCodex` runtime calls; tests stub those via fake JSON-RPC
// responses on a test TCP server driven by `test_shared_codex_runtime`.
// Production surfaces: `app_router` plus the `create_session`,
// `get_session`, `state_events`, `archive_codex_thread`, `unarchive_codex_thread`,
// `rollback_codex_thread`, `fork_codex_thread` handlers in src/api.rs.

use super::*;

#[tokio::test]
async fn project_digest_and_action_routes_are_disabled() {
    let app = app_router(test_app_state());

    let digest_response = request_response(
        &app,
        Request::get("/api/projects/project-1/digest")
            .body(Body::empty())
            .expect("digest request should build"),
    )
    .await;
    let action_response = request_response(
        &app,
        Request::post("/api/projects/project-1/actions/continue")
            .body(Body::empty())
            .expect("action request should build"),
    )
    .await;

    assert_eq!(digest_response.status(), StatusCode::NOT_FOUND);
    assert_eq!(action_response.status(), StatusCode::NOT_FOUND);
}

struct HttpRouteTestFiles {
    persistence_path: std::path::PathBuf,
    orchestrator_templates_path: std::path::PathBuf,
}

impl HttpRouteTestFiles {
    fn capture(state: &AppState) -> Self {
        Self {
            persistence_path: state.persistence_path.as_ref().clone(),
            orchestrator_templates_path: state.orchestrator_templates_path.as_ref().clone(),
        }
    }
}

impl Drop for HttpRouteTestFiles {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.persistence_path);
        let _ = fs::remove_file(&self.orchestrator_templates_path);
    }
}

fn seed_loaded_history_messages(
    state: &AppState,
    session_id: &str,
    message_count: usize,
) -> Vec<String> {
    let messages: Vec<Message> = (0..message_count)
        .map(|index| Message::Text {
            attachments: Vec::new(),
            id: format!("history-message-{index:05}"),
            timestamp: "12:00".to_owned(),
            author: Author::Assistant,
            text: format!("History message {index}"),
            expanded_text: None,
            source: None,
        })
        .collect();
    let message_ids = messages
        .iter()
        .map(|message| message.id().to_owned())
        .collect();
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(session_id)
        .expect("seeded session should exist");
    let record = &mut inner.sessions[index];
    record.session.messages = messages;
    record.session.messages_loaded = true;
    record.message_positions = build_message_positions(&record.session.messages);
    record.mutation_stamp = record.mutation_stamp.saturating_add(1);
    message_ids
}

fn fastest_duration(sample_count: usize, mut operation: impl FnMut()) -> Duration {
    assert!(sample_count > 0, "timing sample count must be positive");
    (0..sample_count)
        .map(|_| {
            let started = std::time::Instant::now();
            operation();
            started.elapsed()
        })
        .min()
        .expect("positive timing sample count should produce a duration")
}

struct TempProjectRoot {
    path: PathBuf,
}

impl TempProjectRoot {
    fn create(prefix: &str) -> Self {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("temp project root should exist");
        Self { path }
    }
}

impl Drop for TempProjectRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct OrderedStateBroadcasterHarness {
    state: AppState,
    session_id: String,
    mailbox: Arc<StateBroadcastMailbox>,
    start_tx: Option<mpsc::Sender<()>>,
    processed_rx: mpsc::Receiver<()>,
    stop: Arc<AtomicBool>,
    thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl OrderedStateBroadcasterHarness {
    fn new() -> Self {
        let state = test_app_state();
        let session_id = test_session_id(&state, Agent::Codex);
        let mailbox = Arc::new(StateBroadcastMailbox::default());
        let state_events_for_broadcast = state.state_events.clone();
        let delta_events_for_broadcast = state.delta_events.clone();
        let mailbox_for_thread = mailbox.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let (start_tx, start_rx) = mpsc::channel();
        let (processed_tx, processed_rx) = mpsc::channel();

        let thread_handle = std::thread::Builder::new()
            .name("termal-test-state-broadcast".to_owned())
            .spawn(move || {
                if start_rx.recv().is_err() {
                    return;
                }
                loop {
                    let work = mailbox_for_thread.recv_next();
                    if stop_for_thread.load(Ordering::SeqCst) {
                        break;
                    }
                    forward_state_broadcast_work(
                        work,
                        &state_events_for_broadcast,
                        &delta_events_for_broadcast,
                    );
                    let _ = processed_tx.send(());
                }
            })
            .expect("test state broadcaster thread should spawn");

        Self {
            state: AppState {
                state_broadcast_mailbox: Some(mailbox.clone()),
                ..state
            },
            session_id,
            mailbox,
            start_tx: Some(start_tx),
            processed_rx,
            stop,
            thread_handle: Some(thread_handle),
        }
    }

    fn release(&mut self) {
        if let Some(start_tx) = self.start_tx.take() {
            start_tx
                .send(())
                .expect("test broadcaster start gate should open");
        }
    }

    fn wait_for_processed(&self, expected_count: usize) {
        for _ in 0..expected_count {
            self.processed_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("test broadcaster should process queued work");
        }
    }
}

impl Drop for OrderedStateBroadcasterHarness {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(start_tx) = self.start_tx.take() {
            let _ = start_tx.send(());
        }
        self.mailbox
            .publish_delta_payload("__termal_test_stop__".to_owned());
        if let Some(thread_handle) = self.thread_handle.take() {
            let _ = thread_handle.join();
        }
    }
}

fn push_test_text_message(state: &AppState, session_id: &str, text: impl Into<String>) -> String {
    let message_id = state.allocate_message_id();
    state
        .push_message(
            session_id,
            Message::Text {
                attachments: Vec::new(),
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: text.into(),
                expanded_text: None,
                source: None,
            },
        )
        .expect("test message should be recorded");
    message_id
}

fn create_ordered_sse_test_project(state: &AppState) -> TempProjectRoot {
    let project_root = TempProjectRoot::create("termal-ordered-sse-project");
    state
        .create_project(CreateProjectRequest {
            name: Some("Ordered SSE Project".to_owned()),
            root_path: project_root.path.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .expect("project creation should queue a state snapshot");
    project_root
}

// Pins `POST /api/sessions` — asserts 201 Created with a
// `CreateSessionResponse` whose `session` field carries the normalized
// workdir and default `Agent::Codex`. Guards against handler regressions
// that drop the session payload or return the wrong status code.
#[tokio::test]
async fn create_session_route_returns_created_response() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let initial_session_count = state.snapshot().sessions.len();
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "name": "Route Created Session",
        "workdir": "/tmp"
    }))
    .expect("create session route body should serialize");
    let (status, response_body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/sessions")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(response_body["session"]["messageCount"], 0);
    let response: CreateSessionResponse =
        serde_json::from_value(response_body).expect("create session response should decode");
    let created_session = &response.session;
    assert_eq!(response.session_id, created_session.id);
    assert!(response.revision > 0);
    assert_eq!(state.snapshot().sessions.len(), initial_session_count + 1);
    assert_eq!(created_session.name, "Route Created Session");
    let expected_workdir = resolve_session_workdir("/tmp").expect("route workdir should normalize");
    assert_eq!(created_session.workdir, expected_workdir);
    assert_eq!(created_session.agent, Agent::Codex);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins `GET /api/sessions/{id}` — asserts 200 OK with bounded session detail
// and the current `revision`, without the caller needing a full
// `StateResponse` snapshot. Guards against the single-session handler drifting
// from the state snapshot revision.
#[tokio::test]
async fn get_session_route_returns_bounded_session_detail() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let app = app_router(state.clone());
    let created = state
        .create_session(CreateSessionRequest {
            name: Some("Route Session Detail".to_owned()),
            agent: None,
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should be created");
    let session_id = created.session_id;

    let (status, response): (StatusCode, SessionResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/sessions/{session_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(response.session.id, session_id);
    assert_eq!(response.session.name, "Route Session Detail");
    assert_eq!(response.revision, state.snapshot().revision);
    // `serverInstanceId` is carried on `SessionResponse` so
    // `adoptFetchedSession` can detect a server restart mid-hydration
    // and accept a revision downgrade. The per-process id must be
    // non-empty (the frontend treats `""` as "unknown / older server"
    // and cannot trigger the restart branch on it).
    assert_eq!(response.server_instance_id, state.server_instance_id);
    assert!(!response.server_instance_id.is_empty());
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins bounded session reads: an explicit tail returns exactly that suffix,
// while the no-query route still returns only the default recent tail. Neither
// path has an unbounded-transcript branch.
#[tokio::test]
async fn get_session_route_can_return_tail_only() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let app = app_router(state.clone());
    let created = state
        .create_session(CreateSessionRequest {
            name: Some("Route Session Tail".to_owned()),
            agent: None,
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should be created");
    let session_id = created.session_id;
    let message_ids = seed_loaded_history_messages(&state, &session_id, 80);

    let (status, response): (StatusCode, SessionResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/sessions/{session_id}?tail=3"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(response.session.id, session_id);
    assert_eq!(response.session.message_count, 80);
    assert!(!response.session.messages_loaded);
    assert_eq!(response.session.messages.len(), 3);
    let tail_message_ids: Vec<String> = response
        .session
        .messages
        .iter()
        .map(|message| message.id().to_owned())
        .collect();
    assert_eq!(tail_message_ids, message_ids[77..].to_vec());

    let (default_status, default_response): (StatusCode, SessionResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/sessions/{session_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(default_status, StatusCode::OK);
    assert_eq!(default_response.session.message_count, 80);
    assert!(!default_response.session.messages_loaded);
    assert_eq!(
        default_response.session.messages.len(),
        SESSION_TAIL_DEFAULT_MESSAGES
    );
    let default_suffix_ids: Vec<String> = default_response
        .session
        .messages
        .iter()
        .map(|message| message.id().to_owned())
        .collect();
    assert_eq!(
        default_suffix_ids,
        message_ids[80 - SESSION_TAIL_DEFAULT_MESSAGES..].to_vec()
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins bounded, stable-id backward paging against a 20k-message transcript.
// Each response is capped, ascending, and supplies the exclusive
// cursor for the next page instead of returning one unbounded JSON document.
#[tokio::test]
async fn get_session_history_route_pages_large_transcript_by_message_id() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let created = state
        .create_session(CreateSessionRequest {
            name: Some("Route Session History".to_owned()),
            agent: None,
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should be created");
    let message_ids = seed_loaded_history_messages(&state, &created.session_id, 20_001);
    let app = app_router(state.clone());

    let (status, newest): (StatusCode, SessionHistoryResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/sessions/{}/history?limit={SESSION_HISTORY_PAGE_MAX_MESSAGES}",
                created.session_id
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(newest.messages.len(), SESSION_HISTORY_PAGE_MAX_MESSAGES);
    assert_eq!(newest.message_count, 20_001);
    assert!(newest.has_more);
    assert_eq!(
        newest.messages.first().map(Message::id),
        Some(message_ids[20_001 - SESSION_HISTORY_PAGE_MAX_MESSAGES].as_str())
    );
    assert_eq!(
        newest.messages.last().map(Message::id),
        Some(message_ids[20_000].as_str())
    );
    assert_eq!(
        newest.next_before.as_deref(),
        Some(message_ids[20_001 - SESSION_HISTORY_PAGE_MAX_MESSAGES].as_str())
    );
    assert!(!newest.has_newer);
    assert!(newest.next_after.is_none());
    assert_eq!(newest.server_instance_id, state.server_instance_id);

    let before = newest.next_before.expect("older history should remain");
    let (older_status, older): (StatusCode, SessionHistoryResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/sessions/{}/history?before={before}&limit={SESSION_HISTORY_PAGE_MAX_MESSAGES}",
                created.session_id
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(older_status, StatusCode::OK);
    assert_eq!(older.messages.len(), SESSION_HISTORY_PAGE_MAX_MESSAGES);
    assert_eq!(
        older.messages.first().map(Message::id),
        Some(message_ids[20_001 - 2 * SESSION_HISTORY_PAGE_MAX_MESSAGES].as_str())
    );
    assert_eq!(
        older.messages.last().map(Message::id),
        Some(message_ids[20_001 - SESSION_HISTORY_PAGE_MAX_MESSAGES - 1].as_str())
    );
    assert!(older.messages.iter().all(|message| message.id() != before));
    assert!(older.has_newer);
    assert_eq!(
        older.next_after.as_deref(),
        older.messages.last().map(Message::id)
    );

    let (start_status, start): (StatusCode, SessionHistoryResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/sessions/{}/history?from=start&limit={SESSION_HISTORY_PAGE_MAX_MESSAGES}",
                created.session_id
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(start_status, StatusCode::OK);
    assert_eq!(start.messages.len(), SESSION_HISTORY_PAGE_MAX_MESSAGES);
    assert_eq!(
        start.messages.first().map(Message::id),
        Some(message_ids[0].as_str())
    );
    assert_eq!(
        start.messages.last().map(Message::id),
        Some(message_ids[SESSION_HISTORY_PAGE_MAX_MESSAGES - 1].as_str())
    );
    assert!(!start.has_more);
    assert!(start.next_before.is_none());
    assert!(start.has_newer);
    let after = start
        .next_after
        .clone()
        .expect("forward history should remain");

    let (forward_status, forward): (StatusCode, SessionHistoryResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/sessions/{}/history?after={after}&limit={SESSION_HISTORY_PAGE_MAX_MESSAGES}",
                created.session_id
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(forward_status, StatusCode::OK);
    assert_eq!(
        forward.messages.first().map(Message::id),
        Some(message_ids[SESSION_HISTORY_PAGE_MAX_MESSAGES].as_str())
    );
    assert!(forward.messages.iter().all(|message| message.id() != after));
}

#[tokio::test]
async fn get_session_overview_route_is_complete_for_a_retained_tail() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        let record = &mut inner.sessions[index];
        record.session.messages = (0..100)
            .map(|position| {
                let id = format!("overview-message-{position:03}");
                if position < 25 {
                    Message::Text {
                        attachments: Vec::new(),
                        id,
                        timestamp: "12:00".to_owned(),
                        author: Author::You,
                        text: format!("Prompt {position}"),
                        expanded_text: None,
                        source: None,
                    }
                } else if position < 50 {
                    Message::Command {
                        id,
                        timestamp: "12:00".to_owned(),
                        author: Author::Assistant,
                        command: format!("command-{position}"),
                        command_language: None,
                        output: String::new(),
                        output_language: None,
                        status: CommandStatus::Success,
                    }
                } else if position < 75 {
                    Message::Diff {
                        id,
                        timestamp: "12:00".to_owned(),
                        author: Author::Assistant,
                        change_set_id: None,
                        file_path: format!("file-{position}.rs"),
                        summary: "Changed".to_owned(),
                        diff: String::new(),
                        language: None,
                        change_type: ChangeType::Edit,
                    }
                } else {
                    Message::Command {
                        id,
                        timestamp: "12:00".to_owned(),
                        author: Author::Assistant,
                        command: format!("failing-{position}"),
                        command_language: None,
                        output: String::new(),
                        output_language: None,
                        status: CommandStatus::Error,
                    }
                }
            })
            .collect();
        record.session.messages_loaded = true;
        record.session.message_count = 100;
        record.message_start_index = 0;
        record.message_positions = build_message_positions(&record.session.messages);
        record.session.markers = vec![ConversationMarker {
            id: "overview-marker".to_owned(),
            session_id: session_id.clone(),
            kind: ConversationMarkerKind::Review,
            name: "Review bucket".to_owned(),
            body: None,
            color: "#4488ff".to_owned(),
            message_id: "overview-message-040".to_owned(),
            message_index_hint: 40,
            end_message_id: None,
            end_message_index_hint: None,
            created_at: "12:00".to_owned(),
            updated_at: "12:00".to_owned(),
            created_by: ConversationMarkerAuthor::User,
        }];
        record.mutation_stamp = 9;
        persist_state(state.persistence_path.as_ref(), &inner)
            .expect("overview fixture should persist");
    }
    let fully_resident_overview = state
        .get_session_overview(&session_id, 4)
        .expect("fully resident overview should load");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        let record = &mut inner.sessions[index];
        record.session.messages.drain(..70);
        record.message_start_index = 70;
        record.message_positions = build_message_positions(&record.session.messages);
        record.session.messages_loaded = false;
    }
    let app = app_router(state);

    let (status, overview): (StatusCode, SessionOverviewResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/sessions/{session_id}/overview?buckets=4"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(overview.session_id, session_id);
    assert_eq!(overview.message_count, 100);
    assert_eq!(overview.session_mutation_stamp, 9);
    assert_eq!(overview.buckets.len(), 4);
    assert_eq!(
        overview
            .buckets
            .iter()
            .map(|bucket| bucket.k)
            .collect::<Vec<_>>(),
        vec![
            ConversationOverviewKind::Text,
            ConversationOverviewKind::Command,
            ConversationOverviewKind::Diff,
            ConversationOverviewKind::Error,
        ]
    );
    assert_eq!(overview.buckets[0].u, 25);
    assert!(overview.buckets[1].m);
    assert_eq!(overview.markers[0].position, 40);
    assert_eq!(overview.latest_position, 99);
    assert_eq!(
        overview, fully_resident_overview,
        "overview must not depend on transcript residency"
    );
}

#[tokio::test]
async fn get_session_history_route_centers_an_around_position() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let message_ids = seed_loaded_history_messages(&state, &session_id, 100);
    let app = app_router(state);

    let (status, page): (StatusCode, SessionHistoryResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/sessions/{session_id}/history?around=50&limit=20"
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(page.message_start_index, 40);
    assert_eq!(page.messages.len(), 20);
    assert_eq!(
        page.messages.first().map(Message::id),
        Some(message_ids[40].as_str())
    );
    assert_eq!(
        page.messages.last().map(Message::id),
        Some(message_ids[59].as_str())
    );
    assert!(page.has_more);
    assert!(page.has_newer);
}

#[tokio::test]
async fn get_session_overview_meets_large_transcript_latency_and_network_budgets() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    seed_loaded_history_messages(&state, &session_id, 25_000);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        inner.sessions[index].session.message_count = 25_000;
    }

    let overview = state
        .get_session_overview(&session_id, SESSION_OVERVIEW_MAX_BUCKETS)
        .expect("large overview should load");
    assert_eq!(overview.buckets.len(), SESSION_OVERVIEW_MAX_BUCKETS);
    // A single wall-clock observation measures unrelated test-runner
    // descheduling as well as this synchronous operation. The best of several
    // warm samples keeps the service-time budget strict without making the
    // parallel suite depend on which test thread the OS happens to schedule.
    let elapsed = fastest_duration(5, || {
        std::hint::black_box(
            state
                .get_session_overview(&session_id, SESSION_OVERVIEW_MAX_BUCKETS)
                .expect("large overview timing sample should load"),
        );
    });
    assert!(
        elapsed < Duration::from_millis(10),
        "fastest 25k-message overview sample took {elapsed:?}"
    );

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        persist_state(state.persistence_path.as_ref(), &inner)
            .expect("large overview fixture should persist");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        let record = &mut inner.sessions[index];
        record.session.messages.drain(..24_970);
        record.message_start_index = 24_970;
        record.message_positions = build_message_positions(&record.session.messages);
        record.session.messages_loaded = false;
    }
    let bounded_overview = state
        .get_session_overview(&session_id, SESSION_OVERVIEW_MAX_BUCKETS)
        .expect("bounded-resident large overview should load");
    assert_eq!(
        bounded_overview, overview,
        "large overview must not depend on transcript residency"
    );
    let bounded_elapsed = fastest_duration(5, || {
        std::hint::black_box(
            state
                .get_session_overview(&session_id, SESSION_OVERVIEW_MAX_BUCKETS)
                .expect("bounded-resident overview timing sample should load"),
        );
    });
    assert!(
        bounded_elapsed < Duration::from_millis(10),
        "fastest 25k-message bounded-resident overview sample took {bounded_elapsed:?}"
    );

    let app = app_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/api/sessions/{session_id}/overview?buckets={SESSION_OVERVIEW_MAX_BUCKETS}"
                ))
                .header(axum::http::header::ACCEPT_ENCODING, "gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("overview request should complete");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CONTENT_ENCODING)
            .and_then(|value| value.to_str().ok()),
        Some("gzip")
    );
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("compressed overview body should load");
    assert!(
        body.len() < 8 * 1024,
        "compressed 512-bucket overview was {} bytes",
        body.len()
    );
}

#[tokio::test]
async fn get_session_history_route_rejects_missing_cursor() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    seed_loaded_history_messages(&state, &session_id, 10);
    let app = app_router(state);

    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/sessions/{session_id}/history?before=missing-message&limit=5"
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        response.get("error").and_then(Value::as_str),
        Some("session history cursor is no longer available; refresh the session tail")
    );
}

#[tokio::test]
async fn get_session_history_route_reports_missing_persisted_position_as_conflict() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    seed_loaded_history_messages(&state, &session_id, 2);
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        persist_state(state.persistence_path.as_path(), &inner)
            .expect("history fixture should persist");
    }
    {
        let connection = rusqlite::Connection::open(state.persistence_path.as_path())
            .expect("history fixture database should open");
        connection
            .execute(
                "DELETE FROM messages WHERE session_id = ?1 AND position = 0",
                rusqlite::params![session_id],
            )
            .expect("one persisted history position should be removed");
    }
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("session should be mutable");
        record.session.messages.clear();
        record.session.messages_loaded = false;
        record.message_start_index = 2;
        record.message_positions.clear();
    }
    let app = app_router(state);

    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/sessions/{session_id}/history?from=start&limit=2"
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        response.get("error").and_then(Value::as_str),
        Some("session history is missing persisted position 0; refresh the session tail")
    );
}

#[tokio::test]
async fn get_session_history_route_rejects_out_of_range_page_limits() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state);

    for (limit, expected_error) in [
        (0, "session history limit must be at least 1".to_owned()),
        (
            SESSION_HISTORY_PAGE_MAX_MESSAGES + 1,
            format!("session history limit must be at most {SESSION_HISTORY_PAGE_MAX_MESSAGES}"),
        ),
    ] {
        let (status, response): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("GET")
                .uri(format!("/api/sessions/{session_id}/history?limit={limit}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            response.get("error").and_then(Value::as_str),
            Some(expected_error.as_str())
        );
    }
}

#[tokio::test]
async fn get_session_overview_route_rejects_out_of_range_bucket_counts() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state);

    for (buckets, expected_error) in [
        (0, "session overview buckets must be at least 1".to_owned()),
        (
            SESSION_OVERVIEW_MAX_BUCKETS + 1,
            format!("session overview buckets must be at most {SESSION_OVERVIEW_MAX_BUCKETS}"),
        ),
    ] {
        let (status, response): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/api/sessions/{session_id}/overview?buckets={buckets}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            response.get("error").and_then(Value::as_str),
            Some(expected_error.as_str())
        );
    }
}

#[tokio::test]
async fn get_session_route_without_query_is_bounded_for_large_transcript() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let message_ids = seed_loaded_history_messages(&state, &session_id, 513);
    let app = app_router(state);

    let (status, response): (StatusCode, SessionResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/sessions/{session_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response.session.message_count, 513);
    assert!(!response.session.messages_loaded);
    assert_eq!(
        response.session.messages.len(),
        SESSION_TAIL_DEFAULT_MESSAGES
    );
    assert_eq!(
        response.session.messages.first().map(Message::id),
        Some(message_ids[513 - SESSION_TAIL_DEFAULT_MESSAGES].as_str())
    );
}

#[tokio::test]
async fn get_session_route_tail_limit_covering_transcript_preserves_loaded_flag() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let app = app_router(state.clone());
    let created = state
        .create_session(CreateSessionRequest {
            name: Some("Route Session Full Tail".to_owned()),
            agent: None,
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should be created");
    let session_id = created.session_id;
    for index in 1..=2 {
        let message_id = state.allocate_message_id();
        state
            .push_message(
                &session_id,
                Message::Text {
                    attachments: Vec::new(),
                    id: message_id,
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: format!("Short tail message {index}"),
                    expanded_text: None,
                    source: None,
                },
            )
            .expect("message should append");
    }

    let (status, response): (StatusCode, SessionResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/sessions/{session_id}?tail=5"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(response.session.message_count, 2);
    assert!(response.session.messages_loaded);
    assert_eq!(response.session.messages.len(), 2);
    assert!(matches!(
        response.session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Short tail message 1"
    ));
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn get_session_route_tail_returns_not_found_for_missing_or_hidden_sessions() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let app = app_router(state.clone());
    let created = state
        .create_session(CreateSessionRequest {
            name: Some("Route Session Hidden Tail".to_owned()),
            agent: None,
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should be created");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("created session should exist");
        inner.sessions[index].hidden = true;
    }

    for session_id in ["missing-session", created.session_id.as_str()] {
        let (status, response): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("GET")
                .uri(format!("/api/sessions/{session_id}?tail=3"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND, "{session_id}");
        assert_eq!(
            response.get("error").and_then(Value::as_str),
            Some("session not found"),
            "{session_id}"
        );
    }
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins `GET /api/sessions/{id}?tail=0`: zero-length tails are rejected
// instead of returning an ambiguous empty, not-loaded session snapshot that
// would make clients retry forever.
#[tokio::test]
async fn get_session_route_rejects_zero_tail_limit() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let app = app_router(state.clone());
    let created = state
        .create_session(CreateSessionRequest {
            name: Some("Route Session Zero Tail".to_owned()),
            agent: None,
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should be created");

    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/sessions/{}?tail=0", created.session_id))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        response.get("error").and_then(Value::as_str),
        Some("session tail must be at least 1")
    );
}

// Pins `GET /api/sessions/{id}?tail=N` above the backend cap: callers must get
// an explicit validation error instead of an unannounced truncated window.
#[tokio::test]
async fn get_session_route_rejects_tail_limit_above_cap() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let app = app_router(state.clone());
    let created = state
        .create_session(CreateSessionRequest {
            name: Some("Route Session Oversized Tail".to_owned()),
            agent: None,
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should be created");

    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/sessions/{}?tail={}",
                created.session_id,
                SESSION_TAIL_HYDRATION_MAX_MESSAGES + 1
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        response.get("error").and_then(Value::as_str),
        Some(
            format!("session tail must be at most {SESSION_TAIL_HYDRATION_MAX_MESSAGES}").as_str()
        )
    );
}

// Pins malformed query handling on routes that opt into the project envelope:
// Axum query extractor rejections must be returned as JSON `{ "error": ... }`
// instead of the default plain-text body.
#[tokio::test]
async fn get_session_route_query_rejection_uses_api_error_envelope() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let app = app_router(state.clone());
    let created = state
        .create_session(CreateSessionRequest {
            name: Some("Route Session Bad Tail".to_owned()),
            agent: None,
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should be created");

    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/sessions/{}?tail=abc", created.session_id))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message = response
        .get("error")
        .and_then(Value::as_str)
        .expect("query rejection should use the API error envelope");
    assert!(message.starts_with("invalid session query:"));
    assert!(message.contains("tail"));
}

// Pins `messageCount` on full snapshot-bearing routes. The count is computed
// from the transcript at wire-projection time so reconnect/state adoption can
// keep summary metadata without waiting for the next delta.
#[tokio::test]
async fn snapshot_bearing_routes_include_message_count() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let created = state
        .create_session(CreateSessionRequest {
            name: Some("Counted Session".to_owned()),
            agent: None,
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should be created");
    let session_id = created.session_id;
    for text in ["First counted message", "Second counted message"] {
        let message_id = state.allocate_message_id();
        state
            .push_message(
                &session_id,
                Message::Text {
                    attachments: Vec::new(),
                    id: message_id,
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: text.to_owned(),
                    expanded_text: None,
                    source: None,
                },
            )
            .expect("message should be recorded");
    }
    let app = app_router(state.clone());

    let (state_status, state_body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/state")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(state_status, StatusCode::OK);
    let state_session = state_body["sessions"]
        .as_array()
        .expect("state sessions should be an array")
        .iter()
        .find(|session| session["id"] == session_id)
        .expect("state should include counted session");
    assert_eq!(state_session["messageCount"], 2);
    assert_eq!(state_session["messagesLoaded"], false);
    assert!(
        state_session["messages"]
            .as_array()
            .expect("state session messages should stay adapter-compatible")
            .is_empty()
    );

    let (session_status, session_body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/sessions/{session_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(session_status, StatusCode::OK);
    assert_eq!(session_body["session"]["id"], session_id);
    assert_eq!(session_body["session"]["messageCount"], 2);
    assert_eq!(session_body["session"]["messagesLoaded"], true);
    assert_eq!(
        session_body["session"]["messages"]
            .as_array()
            .expect("bounded response should include the complete short transcript")
            .len(),
        2
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Global snapshots redact queued prompt bodies, while the authorized targeted
// session tail must include them so a live client can render mailbox wakeups
// without fetching an unbounded transcript.
#[tokio::test]
async fn targeted_session_tail_includes_pending_prompts_redacted_from_global_state() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let created = state
        .create_session(CreateSessionRequest {
            name: Some("Queued Prompt Projection".to_owned()),
            agent: None,
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should be created");
    let session_id = created.session_id;
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        inner.sessions[index]
            .session
            .pending_prompts
            .push(PendingPrompt {
                attachments: Vec::new(),
                id: "queued-mailbox-wakeup".to_owned(),
                timestamp: "10:00".to_owned(),
                text: "Queued mailbox wakeup".to_owned(),
                expanded_text: Some("Expanded queued mailbox wakeup".to_owned()),
                source: None,
            });
    }
    let app = app_router(state.clone());

    let (state_status, state_body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/state")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(state_status, StatusCode::OK);
    let state_session = state_body["sessions"]
        .as_array()
        .expect("state sessions should be an array")
        .iter()
        .find(|session| session["id"] == session_id)
        .expect("state should include queued session");
    assert!(
        state_session.get("pendingPrompts").is_none(),
        "global state must redact queued prompt bodies"
    );

    let (session_status, session_body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/sessions/{session_id}?tail=1"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(session_status, StatusCode::OK);
    assert_eq!(
        session_body["session"]["pendingPrompts"],
        json!([{
            "id": "queued-mailbox-wakeup",
            "timestamp": "10:00",
            "text": "Queued mailbox wakeup",
            "expandedText": "Expanded queued mailbox wakeup"
        }])
    );
}

// Pins `POST /api/sessions/{id}/messages`: prompt start must not advance the
// UI with a metadata-only session. The HTTP response carries the target
// transcript, and SSE carries a narrow `messageCreated` delta for other
// subscribers.
#[tokio::test]
async fn send_message_route_returns_full_session_and_publishes_prompt_delta() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, input_rx) = test_claude_runtime_handle("send-message-route-full-session");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }
    let mut state_events = state.subscribe_events();
    let mut delta_events = state.subscribe_delta_events();
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "text": "Visible route prompt"
    }))
    .expect("message route body should serialize");

    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/messages"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::ACCEPTED);
    match input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("route should enqueue the runtime prompt before returning")
    {
        ClaudeRuntimeCommand::Prompt(command) => {
            assert_eq!(command.text, "Visible route prompt");
            assert!(command.attachments.is_empty());
        }
        _ => panic!("expected Claude prompt command"),
    }
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("prompt session should be present");
    assert!(session.messages_loaded);
    assert_eq!(session.message_count, 1);
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        &session.messages[0],
        Message::Text {
            author: Author::You,
            text,
            ..
        } if text == "Visible route prompt"
    ));

    assert!(matches!(
        state_events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    let payload = delta_events
        .try_recv()
        .expect("prompt start should publish a message delta");
    let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta should decode");
    match delta {
        DeltaEvent::MessageCreated {
            revision,
            session_id: delta_session_id,
            message_index,
            message_count,
            message,
            status,
            ..
        } => {
            assert_eq!(revision, response.revision);
            assert_eq!(delta_session_id, session_id);
            assert_eq!(message_index, 0);
            assert_eq!(message_count, 1);
            assert_eq!(status, SessionStatus::Active);
            assert!(matches!(
                message,
                Message::Text {
                    author: Author::You,
                    text,
                    ..
                } if text == "Visible route prompt"
            ));
        }
        _ => panic!("expected prompt messageCreated delta"),
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

fn next_delta_event(delta_events: &mut broadcast::Receiver<String>) -> DeltaEvent {
    let payload = delta_events
        .try_recv()
        .expect("delta event should be published");
    serde_json::from_str(&payload).expect("delta event should decode")
}

// Pins `message_count` and byte-continuity offsets on representative local
// streaming delta emitters. The frontend uses this metadata to reconcile
// metadata-first snapshots and reject an append after a missed text chunk, so
// Rust-side wire regressions must fail here instead of only in frontend tests.
#[test]
fn local_streaming_delta_events_include_message_count() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Claude);
    let mut delta_events = state.subscribe_delta_events();

    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: "text-1".to_owned(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: String::new(),
                expanded_text: None,
                source: None,
            },
        )
        .expect("streaming text placeholder should be recorded");
    let _ = next_delta_event(&mut delta_events);

    state
        .append_text_delta(&session_id, "text-1", "hello")
        .expect("text delta should append");
    match next_delta_event(&mut delta_events) {
        DeltaEvent::TextDelta {
            message_count,
            text_start_byte,
            ..
        } => {
            assert_eq!(message_count, 1);
            assert_eq!(text_start_byte, Some(0));
        }
        _ => panic!("expected TextDelta"),
    }

    state
        .append_text_delta(&session_id, "text-1", " 👋")
        .expect("unicode text delta should append");
    match next_delta_event(&mut delta_events) {
        DeltaEvent::TextDelta {
            text_start_byte, ..
        } => assert_eq!(text_start_byte, Some(5)),
        _ => panic!("expected TextDelta"),
    }

    state
        .replace_text_message(&session_id, "text-1", "hello final")
        .expect("text replacement should publish");
    match next_delta_event(&mut delta_events) {
        DeltaEvent::TextReplace { message_count, .. } => assert_eq!(message_count, 1),
        _ => panic!("expected TextReplace"),
    }

    state
        .upsert_command_message(&session_id, "command-1", "pwd", "", CommandStatus::Running)
        .expect("command message should be created");
    let _ = next_delta_event(&mut delta_events);
    state
        .upsert_command_message(
            &session_id,
            "command-1",
            "pwd",
            "/tmp",
            CommandStatus::Success,
        )
        .expect("command update should publish");
    match next_delta_event(&mut delta_events) {
        DeltaEvent::CommandUpdate { message_count, .. } => assert_eq!(message_count, 2),
        _ => panic!("expected CommandUpdate"),
    }

    let running_agent = ParallelAgentProgress {
        detail: Some("Checking files".to_owned()),
        id: "agent-1".to_owned(),
        source: ParallelAgentSource::Tool,
        status: ParallelAgentStatus::Running,
        title: "Reviewer".to_owned(),
    };
    state
        .upsert_parallel_agents_message(&session_id, "agents-1", vec![running_agent])
        .expect("parallel agents message should be created");
    let _ = next_delta_event(&mut delta_events);
    state
        .upsert_parallel_agents_message(
            &session_id,
            "agents-1",
            vec![ParallelAgentProgress {
                detail: Some("Done".to_owned()),
                id: "agent-1".to_owned(),
                source: ParallelAgentSource::Tool,
                status: ParallelAgentStatus::Completed,
                title: "Reviewer".to_owned(),
            }],
        )
        .expect("parallel agents update should publish");
    match next_delta_event(&mut delta_events) {
        DeltaEvent::ParallelAgentsUpdate { message_count, .. } => {
            assert_eq!(message_count, 3)
        }
        _ => panic!("expected ParallelAgentsUpdate"),
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that the empty SSE fallback payload carries an explicit fallback marker.
#[test]
fn empty_state_events_payload_carries_explicit_fallback_marker() {
    let payload: Value = serde_json::from_str(EMPTY_STATE_EVENTS_PAYLOAD.as_str())
        .expect("SSE fallback payload should parse");
    assert_eq!(payload["_sseFallback"], true);
    assert_eq!(payload["revision"], 0);
    assert!(payload.get("preferences").is_some());
    assert!(payload.get("sessions").is_some());

    let decoded: StateEventPayload = serde_json::from_str(EMPTY_STATE_EVENTS_PAYLOAD.as_str())
        .expect("fallback payload should decode as a state event payload");
    assert!(decoded.sse_fallback);
    assert_eq!(decoded.state.revision, 0);
}

// Tests that fallback SSE payloads can carry the recovered revision.
#[test]
fn fallback_state_events_payload_uses_supplied_revision() {
    let decoded: StateEventPayload = serde_json::from_str(
        &fallback_state_events_payload(42).expect("fallback payload should encode"),
    )
    .expect("fallback payload should decode as a state event payload");
    assert!(decoded.sse_fallback);
    assert_eq!(decoded.state.revision, 42);
}

// Pins `GET /api/events` when graceful shutdown was already triggered before
// the request reached the SSE handler. The route must drain immediately instead
// of opening a long-lived stream that keeps graceful shutdown blocked.
#[tokio::test]
async fn state_events_route_ends_when_shutdown_precedes_stream_setup() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    state.trigger_shutdown_signal();
    let app = app_router(state.clone());

    let response = request_response(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/events")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Box::pin(response.into_body().into_data_stream());
    expect_sse_stream_end(&mut body, "shutdown-before-connect /api/events").await;
}

// Pins `GET /api/events` shutdown after the initial state frame has been sent.
// This exercises the production route's select branch, not only the lower-level
// shutdown watch helper.
#[tokio::test]
async fn state_events_route_ends_when_shutdown_fires_after_initial_state() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let app = app_router(state.clone());
    let response = request_response(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/events")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Box::pin(response.into_body().into_data_stream());
    let initial_event = next_sse_event(&mut body).await;
    let (initial_name, initial_data) = parse_sse_event(&initial_event);
    assert_eq!(initial_name, "state");
    let _initial_state: StateResponse =
        serde_json::from_str(&initial_data).expect("initial SSE payload should parse");

    state.trigger_shutdown_signal();
    expect_sse_stream_end(&mut body, "shutdown-after-initial-state /api/events").await;
}

// Pins `GET /api/events` (SSE) — asserts the `text/event-stream`
// content-type, that the first frame is a `state` event carrying a
// `StateResponse`, and that a subsequent `push_message` produces a
// live `delta` event with `type: "messageCreated"`. Guards against SSE
// frame ordering or naming regressions.
#[tokio::test]
async fn state_events_route_streams_initial_state_and_live_deltas() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());
    let response = request_response(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/events")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .expect("SSE route should set a content type");
    assert!(content_type.starts_with("text/event-stream"));
    let mut body = Box::pin(response.into_body().into_data_stream());
    let initial_event = next_sse_event(&mut body).await;
    let (initial_name, initial_data) = parse_sse_event(&initial_event);
    assert_eq!(initial_name, "state");
    let initial_state: StateResponse =
        serde_json::from_str(&initial_data).expect("initial SSE payload should parse");
    assert!(
        initial_state
            .sessions
            .iter()
            .any(|session| session.id == session_id)
    );
    let initial_session = initial_state
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("initial state should include test session");
    assert!(!initial_session.messages_loaded);
    assert!(initial_session.messages.is_empty());
    let message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Live delta".to_owned(),
                expanded_text: None,
                source: None,
            },
        )
        .expect("delta message should be recorded");
    let delta_event = next_sse_event(&mut body).await;
    let (delta_name, delta_data) = parse_sse_event(&delta_event);
    assert_eq!(delta_name, "delta");
    let delta: Value = serde_json::from_str(&delta_data).expect("delta SSE payload should parse");
    assert_eq!(delta["type"], "messageCreated");
    assert_eq!(delta["sessionId"], session_id);
    assert_eq!(delta["messageId"], message_id);
    assert_eq!(delta["messageCount"], 1);
    assert_eq!(delta["message"]["type"], "text");
    assert_eq!(delta["message"]["text"], "Live delta");
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins the production ordered state broadcaster, not only the raw SSE mux: a
// full snapshot queued before a delta must reach `/api/events` before that
// delta even when both are pending before the route is polled again.
#[tokio::test]
async fn state_events_route_preserves_queued_snapshot_before_following_delta() {
    let mut harness = OrderedStateBroadcasterHarness::new();
    let state = harness.state.clone();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = harness.session_id.clone();
    let app = app_router(state.clone());
    let response = request_response(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/events")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Box::pin(response.into_body().into_data_stream());

    let initial_event = next_sse_event(&mut body).await;
    let (initial_name, initial_data) = parse_sse_event(&initial_event);
    assert_eq!(initial_name, "state");
    let initial_state: StateResponse =
        serde_json::from_str(&initial_data).expect("initial SSE payload should parse");
    let expected_snapshot_revision = initial_state.revision + 1;
    let expected_delta_revision = expected_snapshot_revision + 1;

    let _project_root = create_ordered_sse_test_project(&state);
    let message_id = push_test_text_message(&state, &session_id, "Ordered delta");
    harness.release();
    harness.wait_for_processed(2);

    let snapshot_event = next_sse_event(&mut body).await;
    let (snapshot_name, snapshot_data) = parse_sse_event(&snapshot_event);
    assert_eq!(snapshot_name, "state");
    let snapshot_state: StateResponse =
        serde_json::from_str(&snapshot_data).expect("queued state payload should parse");
    assert_eq!(snapshot_state.revision, expected_snapshot_revision);
    assert!(
        snapshot_state
            .projects
            .iter()
            .any(|project| project.name == "Ordered SSE Project")
    );

    let delta_event = next_sse_event(&mut body).await;
    let (delta_name, delta_data) = parse_sse_event(&delta_event);
    assert_eq!(delta_name, "delta");
    let delta: Value = serde_json::from_str(&delta_data).expect("delta SSE payload should parse");
    assert_eq!(delta["type"], "messageCreated");
    assert_eq!(delta["sessionId"], session_id);
    assert_eq!(delta["messageId"], message_id);
    assert_eq!(delta["revision"].as_u64(), Some(expected_delta_revision));
}

// Pins the ordered broadcaster's downstream recovery contract through the real
// `/api/events` route: when the mailbox feeds more deltas into the bounded SSE
// broadcast channel than a client can retain, the route must emit the explicit
// lagged marker followed by an authoritative recovery state.
#[tokio::test]
async fn state_events_route_recovers_after_ordered_broadcaster_delta_overflow() {
    let mut harness = OrderedStateBroadcasterHarness::new();
    let state = harness.state.clone();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = harness.session_id.clone();
    let app = app_router(state.clone());
    let response = request_response(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/events")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Box::pin(response.into_body().into_data_stream());

    let initial_event = next_sse_event(&mut body).await;
    let (initial_name, initial_data) = parse_sse_event(&initial_event);
    assert_eq!(initial_name, "state");
    let initial_state: StateResponse =
        serde_json::from_str(&initial_data).expect("initial SSE payload should parse");

    const DELTA_COUNT: usize = 64;
    for index in 0..DELTA_COUNT {
        push_test_text_message(&state, &session_id, format!("Overflow delta {index}"));
    }
    let expected_recovery_revision = initial_state.revision + DELTA_COUNT as u64;
    harness.release();
    harness.wait_for_processed(DELTA_COUNT);

    let lagged_event = next_sse_event(&mut body).await;
    assert!(
        lagged_event.contains("event: lagged"),
        "expected lagged marker, got {lagged_event:?}"
    );
    assert!(
        lagged_event
            .lines()
            .any(|line| line.trim_end_matches('\r') == "data: 1"),
        "lagged marker must carry a non-empty reserved payload: {lagged_event:?}"
    );
    let (lagged_name, lagged_data) = parse_sse_event(&lagged_event);
    assert_eq!(lagged_name, "lagged");
    assert_eq!(lagged_data, "1");

    let recovery_event = next_sse_event(&mut body).await;
    let (recovery_name, recovery_data) = parse_sse_event(&recovery_event);
    assert_eq!(recovery_name, "state");
    let recovery_state: StateResponse =
        serde_json::from_str(&recovery_data).expect("recovery state payload should parse");
    assert_eq!(recovery_state.revision, expected_recovery_revision);
}

#[tokio::test]
async fn state_events_route_streams_parallel_agents_update_sources() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());
    let response = request_response(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/events")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Box::pin(response.into_body().into_data_stream());
    let _ = next_sse_event(&mut body).await;

    state
        .upsert_parallel_agents_message(
            &session_id,
            "agents-source-wire",
            vec![
                ParallelAgentProgress {
                    detail: Some("Tool is running".to_owned()),
                    id: "tool-1".to_owned(),
                    source: ParallelAgentSource::Tool,
                    status: ParallelAgentStatus::Running,
                    title: "Tool task".to_owned(),
                },
                ParallelAgentProgress {
                    detail: Some("Delegation is running".to_owned()),
                    id: "delegation-1".to_owned(),
                    source: ParallelAgentSource::Delegation,
                    status: ParallelAgentStatus::Running,
                    title: "Delegation task".to_owned(),
                },
            ],
        )
        .expect("parallel agents message should be created");
    let create_event = next_sse_event(&mut body).await;
    let (event_name, event_data) = parse_sse_event(&create_event);
    assert_eq!(event_name, "delta");
    let delta: Value = serde_json::from_str(&event_data).expect("delta SSE payload should parse");
    assert_eq!(delta["type"], "messageCreated");
    assert_eq!(delta["sessionId"], session_id);
    assert_eq!(delta["messageId"], "agents-source-wire");
    assert_eq!(delta["message"]["type"], "parallelAgents");
    assert_eq!(delta["message"]["agents"][0]["source"], "tool");
    assert_eq!(delta["message"]["agents"][0]["status"], "running");
    assert_eq!(delta["message"]["agents"][1]["source"], "delegation");
    assert_eq!(delta["message"]["agents"][1]["status"], "running");

    state
        .upsert_parallel_agents_message(
            &session_id,
            "agents-source-wire",
            vec![
                ParallelAgentProgress {
                    detail: Some("Tool completed".to_owned()),
                    id: "tool-1".to_owned(),
                    source: ParallelAgentSource::Tool,
                    status: ParallelAgentStatus::Completed,
                    title: "Tool task".to_owned(),
                },
                ParallelAgentProgress {
                    detail: Some("Delegation completed".to_owned()),
                    id: "delegation-1".to_owned(),
                    source: ParallelAgentSource::Delegation,
                    status: ParallelAgentStatus::Completed,
                    title: "Delegation task".to_owned(),
                },
            ],
        )
        .expect("parallel agents update should publish");
    let update_event = next_sse_event(&mut body).await;
    let (event_name, event_data) = parse_sse_event(&update_event);
    assert_eq!(event_name, "delta");
    let delta: Value = serde_json::from_str(&event_data).expect("delta SSE payload should parse");
    assert_eq!(delta["type"], "parallelAgentsUpdate");
    assert_eq!(delta["sessionId"], session_id);
    assert_eq!(delta["messageId"], "agents-source-wire");
    assert_eq!(delta["agents"][0]["source"], "tool");
    assert_eq!(delta["agents"][1]["source"], "delegation");

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins `GET /api/events` lagged recovery wire format. Browsers can skip
// empty-data EventSource frames, so the backend must serialize the control
// marker with a non-empty reserved body before the recovery `state` frame.
#[tokio::test]
async fn state_events_route_emits_non_empty_lagged_marker_before_recovery_state() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let app = app_router(state.clone());
    let response = request_response(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/events")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Box::pin(response.into_body().into_data_stream());

    let initial_event = next_sse_event(&mut body).await;
    let (initial_name, _) = parse_sse_event(&initial_event);
    assert_eq!(initial_name, "state");

    // `test_app_state()` uses a 16-slot broadcast channel; 64 sends is safely
    // past that capacity even if the route drains a few frames while we loop.
    for _ in 0..64 {
        let payload =
            serde_json::to_string(&state.summary_snapshot()).expect("state should serialize");
        state
            .state_events
            .send(payload)
            .expect("route receiver should still be subscribed");
    }

    let lagged_event = next_sse_event(&mut body).await;
    assert!(lagged_event.contains("event: lagged"));
    assert!(
        lagged_event
            .lines()
            .any(|line| line.trim_end_matches('\r') == "data: 1"),
        "lagged marker must carry a non-empty reserved payload: {lagged_event:?}"
    );
    let (lagged_name, lagged_data) = parse_sse_event(&lagged_event);
    assert_eq!(lagged_name, "lagged");
    assert_eq!(lagged_data, "1");

    let recovery_event = next_sse_event(&mut body).await;
    let (recovery_name, recovery_data) = parse_sse_event(&recovery_event);
    assert_eq!(recovery_name, "state");
    let recovery_state: StateResponse =
        serde_json::from_str(&recovery_data).expect("recovery state payload should parse");
    assert_eq!(recovery_state.revision, state.summary_snapshot().revision);
}

// Pins `GET /api/events` + `PUT/DELETE /api/workspaces/{id}` — asserts
// every workspace-layout mutation (create, update, delete) republishes
// a fresh `state` SSE frame whose `workspaces` summaries reflect the
// new revision and control-panel side. Guards against layout mutations
// that persist but fail to refresh the SSE stream.
#[tokio::test]
async fn state_events_route_streams_workspace_layout_summary_updates() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let app = app_router(state.clone());
    let response = request_response(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/events")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Box::pin(response.into_body().into_data_stream());

    let initial_event = next_sse_event(&mut body).await;
    let (initial_name, initial_data) = parse_sse_event(&initial_event);
    assert_eq!(initial_name, "state");
    let initial_state: StateResponse =
        serde_json::from_str(&initial_data).expect("initial SSE payload should parse");
    assert!(initial_state.workspaces.is_empty());

    let create_layout_body = serde_json::to_vec(&json!({
        "controlPanelSide": "left",
        "workspace": { "panes": [] }
    }))
    .expect("workspace layout body should serialize");
    let (save_status, _save_response): (StatusCode, WorkspaceLayoutResponse) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/workspaces/workspace-live")
            .header("content-type", "application/json")
            .body(Body::from(create_layout_body))
            .unwrap(),
    )
    .await;
    assert_eq!(save_status, StatusCode::OK);

    let saved_event = next_sse_event(&mut body).await;
    let (saved_name, saved_data) = parse_sse_event(&saved_event);
    assert_eq!(saved_name, "state");
    let saved_state: StateResponse =
        serde_json::from_str(&saved_data).expect("saved SSE payload should parse");
    assert_eq!(saved_state.workspaces.len(), 1);
    assert_eq!(saved_state.workspaces[0].id, "workspace-live");
    assert_eq!(saved_state.workspaces[0].revision, 1);
    assert_eq!(
        saved_state.workspaces[0].control_panel_side,
        WorkspaceControlPanelSide::Left
    );

    let update_layout_body = serde_json::to_vec(&json!({
        "controlPanelSide": "right",
        "workspace": {
            "panes": [
                {
                    "id": "pane-1",
                    "tabs": []
                }
            ]
        }
    }))
    .expect("updated workspace layout body should serialize");
    let (update_status, _update_response): (StatusCode, WorkspaceLayoutResponse) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/workspaces/workspace-live")
            .header("content-type", "application/json")
            .body(Body::from(update_layout_body))
            .unwrap(),
    )
    .await;
    assert_eq!(update_status, StatusCode::OK);

    let updated_event = next_sse_event(&mut body).await;
    let (updated_name, updated_data) = parse_sse_event(&updated_event);
    assert_eq!(updated_name, "state");
    let updated_state: StateResponse =
        serde_json::from_str(&updated_data).expect("updated SSE payload should parse");
    assert_eq!(updated_state.workspaces.len(), 1);
    assert_eq!(updated_state.workspaces[0].id, "workspace-live");
    assert_eq!(updated_state.workspaces[0].revision, 2);
    assert_eq!(
        updated_state.workspaces[0].control_panel_side,
        WorkspaceControlPanelSide::Right
    );

    let (delete_status, _delete_response): (StatusCode, WorkspaceLayoutsResponse) = request_json(
        &app,
        Request::builder()
            .method("DELETE")
            .uri("/api/workspaces/workspace-live")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(delete_status, StatusCode::OK);

    let deleted_event = next_sse_event(&mut body).await;
    let (deleted_name, deleted_data) = parse_sse_event(&deleted_event);
    assert_eq!(deleted_name, "state");
    let deleted_state: StateResponse =
        serde_json::from_str(&deleted_data).expect("deleted SSE payload should parse");
    assert!(deleted_state.workspaces.is_empty());
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins `GET /api/events` — asserts that creating an orchestrator
// instance republishes the full `state` frame (including the new
// instance and its session fan-out), and that pausing it emits a
// `delta` frame with `type: "orchestratorsUpdated"` listing the
// referenced sessions. Guards against orchestrator SSE routing
// that drops status transitions or session references.
#[tokio::test]
async fn state_events_route_streams_orchestrator_creation_state_and_live_orchestrator_deltas() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-events-route-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("events project root should exist");
    let project_id = create_test_project(&state, &project_root, "Events Orchestrator Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let app = app_router(state.clone());
    let response = request_response(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/events")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Box::pin(response.into_body().into_data_stream());

    let initial_event = next_sse_event(&mut body).await;
    let (initial_name, initial_data) = parse_sse_event(&initial_event);
    assert_eq!(initial_name, "state");
    let initial_state: StateResponse =
        serde_json::from_str(&initial_data).expect("initial SSE payload should parse");
    assert!(initial_state.orchestrators.is_empty());

    let created = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created");
    let instance_id = created.orchestrator.id.clone();
    let created_session_ids = created
        .orchestrator
        .session_instances
        .iter()
        .map(|instance| instance.session_id.clone())
        .collect::<Vec<_>>();

    let created_event = next_sse_event(&mut body).await;
    let (created_name, created_data) = parse_sse_event(&created_event);
    assert_eq!(created_name, "state");
    let created_state: StateResponse =
        serde_json::from_str(&created_data).expect("create SSE payload should parse");
    let created_orchestrator = created_state
        .orchestrators
        .iter()
        .find(|instance| instance.id == instance_id)
        .expect("create SSE state should include the orchestrator instance");
    assert_eq!(
        created_orchestrator.status,
        OrchestratorInstanceStatus::Running
    );
    for session_id in &created_session_ids {
        assert!(
            created_state
                .sessions
                .iter()
                .any(|session| session.id == *session_id),
            "create SSE state should include orchestrator session {session_id}"
        );
    }

    state
        .pause_orchestrator_instance(&instance_id)
        .expect("pause route should update orchestrator state");

    let delta_event = next_sse_event(&mut body).await;
    let (delta_name, delta_data) = parse_sse_event(&delta_event);
    assert_eq!(delta_name, "delta");
    let delta: Value = serde_json::from_str(&delta_data).expect("delta SSE payload should parse");
    assert_eq!(delta["type"], "orchestratorsUpdated");
    assert!(
        delta["orchestrators"]
            .as_array()
            .is_some_and(|instances| instances.iter().any(|instance| {
                instance["id"] == Value::String(instance_id.clone())
                    && instance["status"] == Value::String("paused".to_owned())
            }))
    );
    let delta_session_ids = delta["sessions"]
        .as_array()
        .expect("orchestrator delta should include referenced sessions")
        .iter()
        .map(|session| {
            session["id"]
                .as_str()
                .expect("delta session should include an ID")
                .to_owned()
        })
        .collect::<HashSet<_>>();
    assert_eq!(
        delta_session_ids,
        created_session_ids.into_iter().collect::<HashSet<_>>()
    );

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins `GET /api/sessions/{id}/codex/mcp-servers` to the app-server's
// paginated `mcpServerStatus/list` contract. The response intentionally keeps
// only display metadata; input schemas and other raw MCP protocol fields must
// not leak through this composer-facing endpoint.
#[test]
fn codex_mcp_request_timeout_enforces_the_shared_deadline() {
    let expired_deadline = std::time::Instant::now()
        .checked_sub(Duration::from_millis(1))
        .expect("test deadline should be representable");
    assert!(
        codex_mcp_request_timeout(expired_deadline).is_err(),
        "an elapsed overall deadline must reject the next page"
    );

    let distant_deadline = std::time::Instant::now() + Duration::from_secs(120);
    let timeout = codex_mcp_request_timeout(distant_deadline)
        .expect("a future deadline should allow another request");
    assert!(timeout > Duration::ZERO);
    assert!(timeout <= CODEX_MCP_STATUS_TIMEOUT);
}

#[tokio::test]
async fn codex_mcp_servers_route_paginates_and_sanitizes_status() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, _process) = test_shared_codex_runtime("shared-codex-mcp-status");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    let server = std::thread::spawn(move || {
        for expected_cursor in [None, Some("page-2")] {
            let command = input_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("Codex MCP status request should arrive");
            match command {
                CodexRuntimeCommand::JsonRpcRequest {
                    method,
                    params,
                    response_tx,
                    ..
                } => {
                    assert_eq!(method, "mcpServerStatus/list");
                    assert_eq!(params["detail"], "toolsAndAuthOnly");
                    assert_eq!(params["limit"], 100);
                    assert_eq!(
                        params.get("cursor").and_then(Value::as_str),
                        expected_cursor
                    );
                    let result = if expected_cursor.is_none() {
                        json!({
                            "data": [{
                                "name": "zeta",
                                "authStatus": "oAuth",
                                "tools": {
                                    "write": {
                                        "name": "write",
                                        "title": "Write",
                                        "description": "Writes a value",
                                        "inputSchema": {"type": "object"}
                                    },
                                    "read": {
                                        "name": "read",
                                        "description": "Reads a value",
                                        "inputSchema": {"type": "object"}
                                    }
                                },
                                "resources": [{"uri": "secret://not-forwarded"}]
                            }],
                            "nextCursor": "page-2"
                        })
                    } else {
                        json!({
                            "data": [{
                                "name": "alpha",
                                "authStatus": "notLoggedIn",
                                "tools": {}
                            }],
                            "nextCursor": null
                        })
                    };
                    let _ = response_tx.send(Ok(result));
                }
                _ => panic!("expected shared Codex JSON-RPC request"),
            }
        }
    });

    let app = app_router(state);
    let (status, response): (StatusCode, CodexMcpServersResponse) = request_json(
        &app,
        Request::get(format!("/api/sessions/{session_id}/codex/mcp-servers"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        response,
        CodexMcpServersResponse {
            servers: vec![
                CodexMcpServerStatus {
                    name: "alpha".to_owned(),
                    auth_status: "notLoggedIn".to_owned(),
                    tools: Vec::new(),
                },
                CodexMcpServerStatus {
                    name: "zeta".to_owned(),
                    auth_status: "oAuth".to_owned(),
                    tools: vec![
                        CodexMcpToolSummary {
                            name: "read".to_owned(),
                            title: None,
                            description: Some("Reads a value".to_owned()),
                        },
                        CodexMcpToolSummary {
                            name: "write".to_owned(),
                            title: Some("Write".to_owned()),
                            description: Some("Writes a value".to_owned()),
                        },
                    ],
                },
            ],
        }
    );
    join_test_server(server);
}

#[tokio::test]
async fn codex_mcp_servers_route_bounds_unfinished_pagination() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, _process) = test_shared_codex_runtime("shared-codex-mcp-limit");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    let server = std::thread::spawn(move || {
        for page_index in 0..CODEX_MCP_STATUS_MAX_PAGES {
            let command = input_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("bounded Codex MCP status request should arrive");
            match command {
                CodexRuntimeCommand::JsonRpcRequest {
                    method,
                    params,
                    response_tx,
                    ..
                } => {
                    assert_eq!(method, "mcpServerStatus/list");
                    let expected_cursor = (page_index > 0).then(|| format!("cursor-{page_index}"));
                    assert_eq!(
                        params.get("cursor").and_then(Value::as_str),
                        expected_cursor.as_deref()
                    );
                    let _ = response_tx.send(Ok(json!({
                        "data": [],
                        "nextCursor": format!("cursor-{}", page_index + 1)
                    })));
                }
                _ => panic!("expected shared Codex JSON-RPC request"),
            }
        }
    });

    let app = app_router(state);
    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::get(format!("/api/sessions/{session_id}/codex/mcp-servers"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        response["error"].as_str(),
        Some("Codex MCP status exceeded the pagination safety limit")
    );
    join_test_server(server);
}

#[tokio::test]
async fn codex_mcp_servers_route_rejects_a_repeated_pagination_cursor() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, _process) =
        test_shared_codex_runtime("shared-codex-mcp-repeated-cursor");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    let server = std::thread::spawn(move || {
        for expected_cursor in [None, Some("loop")] {
            let command = input_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("Codex MCP status request should arrive");
            match command {
                CodexRuntimeCommand::JsonRpcRequest {
                    method,
                    params,
                    response_tx,
                    ..
                } => {
                    assert_eq!(method, "mcpServerStatus/list");
                    assert_eq!(
                        params.get("cursor").and_then(Value::as_str),
                        expected_cursor
                    );
                    let _ = response_tx.send(Ok(json!({
                        "data": [],
                        "nextCursor": "loop"
                    })));
                }
                _ => panic!("expected shared Codex JSON-RPC request"),
            }
        }
    });

    let app = app_router(state);
    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::get(format!("/api/sessions/{session_id}/codex/mcp-servers"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        response["error"].as_str(),
        Some("Codex MCP status pagination repeated a cursor")
    );
    join_test_server(server);
}

#[tokio::test]
async fn codex_mcp_servers_route_proxies_remote_sessions_to_their_owner() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let remote = RemoteConfig {
        id: "ssh-mcp".to_owned(),
        name: "SSH MCP".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.agent == Agent::Codex)
    .expect("sample remote state should contain a Codex session");
    let remote_session_id = remote_session.id.clone();
    let local_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let local_session_id = upsert_remote_proxy_session_record(
            &mut inner,
            &remote.id,
            &remote_session,
            Some(local_project_id),
        );
        state
            .commit_locked(&mut inner)
            .expect("remote proxy session should persist");
        local_session_id
    };

    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let expected_request_prefix =
        format!("GET /api/sessions/{remote_session_id}/codex/mcp-servers ");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "remote MCP proxy listener");
            let request = read_test_http_request(&mut stream);
            requests_for_server
                .lock()
                .expect("requests mutex poisoned")
                .push(request.request_line.clone());

            if request.request_line.starts_with("GET /api/health ") {
                write_test_http_response(
                    &mut stream,
                    StatusCode::OK,
                    "application/json",
                    r#"{"ok":true}"#,
                );
            } else if request.request_line.starts_with(&expected_request_prefix) {
                write_test_http_response(
                    &mut stream,
                    StatusCode::OK,
                    "application/json",
                    r#"{"servers":[{"name":"remote-mcp","authStatus":"oAuth","tools":[]}]}"#,
                );
            } else {
                panic!("unexpected request: {}", request.request_line);
            }
        }
    });
    insert_test_remote_connection(&state, &remote, port);

    let app = app_router(state);
    let (status, response): (StatusCode, CodexMcpServersResponse) = request_json(
        &app,
        Request::get(format!(
            "/api/sessions/{local_session_id}/codex/mcp-servers"
        ))
        .body(Body::empty())
        .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        response,
        CodexMcpServersResponse {
            servers: vec![CodexMcpServerStatus {
                name: "remote-mcp".to_owned(),
                auth_status: "oAuth".to_owned(),
                tools: Vec::new(),
            }],
        }
    );
    let requests = requests.lock().expect("requests mutex poisoned");
    assert!(
        requests.iter().any(|request| request.starts_with(&format!(
            "GET /api/sessions/{remote_session_id}/codex/mcp-servers "
        ))),
        "expected the remote session id in proxied requests, saw {requests:?}"
    );
    drop(requests);
    join_test_server(server);
}

#[tokio::test]
async fn codex_mcp_servers_route_rejects_non_codex_sessions() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Gemini);
    let app = app_router(state);

    let response = request_response(
        &app,
        Request::get(format!("/api/sessions/{session_id}/codex/mcp-servers"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

// Pins `POST /api/sessions/{id}/codex/thread/{archive,unarchive,rollback}`
// — asserts each action returns 200 OK with a `StateResponse` whose
// session reflects the new `codex_thread_state`, and that rollback
// replaces stale local messages with the freshly-returned thread
// history. Guards against handler drift in the JSON-RPC thread action
// routes and their session-state synchronisation.
#[tokio::test]
async fn codex_thread_action_routes_update_session_state() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "thread-live".to_owned())
        .unwrap();
    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "stale local message".to_owned(),
                expanded_text: None,
                source: None,
            },
        )
        .unwrap();
    let (runtime, input_rx, _process) = test_shared_codex_runtime("shared-codex-route-actions");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        for expected_method in ["thread/archive", "thread/unarchive", "thread/rollback"] {
            let command = input_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("Codex thread action should arrive");
            match command {
                CodexRuntimeCommand::JsonRpcRequest {
                    method,
                    params,
                    response_tx,
                    ..
                } => {
                    assert_eq!(method, expected_method);
                    assert_eq!(params["threadId"], "thread-live");
                    if method == "thread/rollback" {
                        assert_eq!(params["numTurns"], 2);
                        let _ = response_tx.send(Ok(json!({
                            "thread": {
                                "preview": "Rolled back preview",
                                "turns": [
                                    {
                                        "id": "turn-rollback",
                                        "status": "completed",
                                        "items": [
                                            {
                                                "id": "rollback-user",
                                                "type": "userMessage",
                                                "content": [
                                                    {
                                                        "type": "text",
                                                        "text": "Current diff state"
                                                    }
                                                ]
                                            },
                                            {
                                                "id": "rollback-agent",
                                                "type": "agentMessage",
                                                "text": "Rollback synced."
                                            }
                                        ]
                                    }
                                ]
                            }
                        })));
                        continue;
                    }
                    let _ = response_tx.send(Ok(json!({})));
                }
                _ => panic!("expected shared Codex JSON-RPC request"),
            }
        }
    });

    let app = app_router(state.clone());
    let (archive_status, archive_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/codex/thread/archive"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(archive_status, StatusCode::OK);
    let archived_session = archive_response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(
        archived_session.codex_thread_state,
        Some(CodexThreadState::Archived)
    );

    let (unarchive_status, unarchive_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/codex/thread/unarchive"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(unarchive_status, StatusCode::OK);
    let restored_session = unarchive_response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(
        restored_session.codex_thread_state,
        Some(CodexThreadState::Active)
    );

    let (rollback_status, rollback_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/codex/thread/rollback"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"numTurns":2}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(rollback_status, StatusCode::OK);
    let rollback_session = rollback_response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(!rollback_session.messages_loaded);
    assert!(rollback_session.messages.is_empty());
    let rollback_session = state
        .get_session(&session_id)
        .expect("rolled back session should hydrate")
        .session;
    assert!(matches!(
        rollback_session.messages.first(),
        Some(Message::Text { author: Author::You, text, .. }) if text == "Current diff state"
    ));
    assert!(matches!(
        rollback_session.messages.get(1),
        Some(Message::Text { author: Author::Assistant, text, .. }) if text == "Rollback synced."
    ));
    assert!(!rollback_session.messages.iter().any(
        |message| matches!(message, Message::Markdown { title, .. }
            if title == "Archived Codex thread"
                || title == "Restored Codex thread"
                || title == "Rolled back Codex thread")
    ));
    assert!(!rollback_session.messages.iter().any(
        |message| matches!(message, Message::Text { text, .. } if text == "stale local message")
    ));
}

// Pins `POST /api/sessions/{id}/codex/thread/rollback` — asserts that
// when Codex returns no `turns`, the handler still replies 200 OK with
// a `StateResponse`, preserves the existing local history, and appends
// a `Markdown` notice explaining the missing thread payload. Guards
// against the fallback branch regressing into a 500 or silent data loss.
#[tokio::test]
async fn codex_thread_rollback_route_falls_back_when_history_is_unavailable() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "thread-live".to_owned())
        .unwrap();
    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "local history".to_owned(),
                expanded_text: None,
                source: None,
            },
        )
        .unwrap();
    let (runtime, input_rx, _process) =
        test_shared_codex_runtime("shared-codex-route-rollback-fallback");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Codex rollback command should arrive");
        match command {
            CodexRuntimeCommand::JsonRpcRequest {
                method,
                params,
                response_tx,
                ..
            } => {
                assert_eq!(method, "thread/rollback");
                assert_eq!(params["threadId"], "thread-live");
                assert_eq!(params["numTurns"], 1);
                let _ = response_tx.send(Ok(json!({
                    "thread": {
                        "preview": "Fallback preview"
                    }
                })));
            }
            _ => panic!("expected shared Codex JSON-RPC request"),
        }
    });

    let app = app_router(state.clone());
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/codex/thread/rollback"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"numTurns":1}"#))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(!session.messages_loaded);
    assert!(session.messages.is_empty());
    let session = state
        .get_session(&session_id)
        .expect("rolled back fallback session should hydrate")
        .session;
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "local history"
    ));
    assert!(matches!(
        session.messages.last(),
        Some(Message::Markdown { title, markdown, .. })
            if title == "Rolled back Codex thread"
                && markdown.contains("Codex did not return the updated thread history")
    ));
}

// Pins `POST /api/sessions/{id}/codex/thread/fork` — asserts 201 Created
// with a `CreateSessionResponse` whose `session` carries the forked
// `external_session_id`, `CodexThreadState::Active`, and the hydrated
// user/agent messages rebuilt from the fake `thread/fork` JSON-RPC
// response. Guards against fork regressions that drop thread metadata
// or return the wrong status code.
#[tokio::test]
async fn codex_thread_fork_route_returns_created_response() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Route Review".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5.4".to_owned()),
            approval_policy: Some(CodexApprovalPolicy::Never),
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    state
        .set_external_session_id(&created.session_id, "thread-origin".to_owned())
        .unwrap();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("source Codex session should exist");
        inner.sessions[index].session.model_options =
            vec![SessionModelOption::plain("gpt-5.4", "gpt-5.4")];
    }

    let (runtime, input_rx, _process) = test_shared_codex_runtime("shared-codex-route-fork");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Codex fork command should arrive");
        match command {
            CodexRuntimeCommand::JsonRpcRequest {
                method,
                params,
                response_tx,
                ..
            } => {
                assert_eq!(method, "thread/fork");
                assert_eq!(params["threadId"], "thread-origin");
                let _ = response_tx.send(Ok(json!({
                    "thread": {
                        "id": "thread-forked",
                        "name": "Forked Review",
                        "preview": "Forked preview",
                        "turns": [
                            {
                                "id": "turn-forked",
                                "status": "completed",
                                "items": [
                                    {
                                        "id": "fork-user",
                                        "type": "userMessage",
                                        "content": [
                                            {
                                                "type": "text",
                                                "text": "Fork context"
                                            }
                                        ]
                                    },
                                    {
                                        "id": "fork-agent",
                                        "type": "agentMessage",
                                        "text": "Ready to continue."
                                    }
                                ]
                            }
                        ]
                    },
                    "model": "gpt-5.5",
                    "approvalPolicy": "on-request",
                    "sandbox": {
                        "type": "workspaceWrite"
                    },
                    "reasoningEffort": "high",
                    "cwd": "/tmp/forked",
                })));
            }
            _ => panic!("expected shared Codex JSON-RPC request"),
        }
    });

    let app = app_router(state);
    let (status, response): (StatusCode, CreateSessionResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{}/codex/thread/fork",
                created.session_id
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    let forked_session = &response.session;
    assert_eq!(response.session_id, forked_session.id);
    assert!(response.revision > 0);
    assert_eq!(
        forked_session.codex_thread_state,
        Some(CodexThreadState::Active)
    );
    assert_eq!(
        forked_session.external_session_id.as_deref(),
        Some("thread-forked")
    );
    assert!(matches!(
        forked_session.messages.first(),
        Some(Message::Text { author: Author::You, text, .. }) if text == "Fork context"
    ));
    assert!(matches!(
        forked_session.messages.get(1),
        Some(Message::Text { author: Author::Assistant, text, .. }) if text == "Ready to continue."
    ));
}
