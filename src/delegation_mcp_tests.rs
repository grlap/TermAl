/*
 * Delegation MCP bridge unit/integration tests and their local HTTP fixtures.
 *
 * This module owns only test support and assertions for the production bridge in
 * `src/delegation_mcp.rs`; it does not define runtime behavior or public APIs.
 * It intentionally lives beside that production fragment instead of under
 * `src/tests/`, whose files are declared through `src/tests/mod.rs`. The explicit
 * placement avoids accidental double compilation and makes the crate-root test
 * module path unambiguous.
 */

use super::*;
use std::sync::atomic::AtomicUsize;
use std::thread;

const TEST_MCP_HTTP_ACCEPT_DEADLINE: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
struct TestMcpHttpRequest {
    method: String,
    path: String,
    body: String,
}

fn try_read_test_mcp_http_request(
    stream: &mut std::net::TcpStream,
) -> Result<TestMcpHttpRequest> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let bytes_read = stream
            .read(&mut chunk)
            .context("test request headers should read")?;
        if bytes_read == 0 {
            bail!("test request closed before headers completed");
        }
        buffer.extend_from_slice(&chunk[..bytes_read]);
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
    };
    let headers = String::from_utf8_lossy(&buffer[..header_end]);
    let request_line = headers
        .lines()
        .next()
        .expect("request line should be present");
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .expect("request method should be present")
        .to_owned();
    let path = request_parts
        .next()
        .expect("request path should be present")
        .to_owned();
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then_some(value.trim())
                .and_then(|value| value.parse::<usize>().ok())
        })
        .unwrap_or(0);
    let body_start = header_end + 4;
    while buffer.len() < body_start + content_length {
        let bytes_read = stream
            .read(&mut chunk)
            .context("test request body should read")?;
        if bytes_read == 0 {
            bail!("test request closed before its declared body completed");
        }
        buffer.extend_from_slice(&chunk[..bytes_read]);
    }
    let body =
        String::from_utf8_lossy(&buffer[body_start..body_start + content_length]).to_string();
    Ok(TestMcpHttpRequest { method, path, body })
}

fn read_test_mcp_http_request(stream: &mut std::net::TcpStream) -> TestMcpHttpRequest {
    try_read_test_mcp_http_request(stream).expect("test request should read")
}

fn write_test_mcp_http_json_response(
    stream: &mut std::net::TcpStream,
    status: u16,
    body: Value,
) {
    write_test_mcp_http_response(stream, status, &body.to_string());
}

fn write_test_mcp_http_response(
    stream: &mut std::net::TcpStream,
    status: u16,
    body: &str,
) {
    stream
        .write_all(
            format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .as_bytes(),
        )
        .expect("response should write");
}

fn accept_test_mcp_http_stream(
    listener: &std::net::TcpListener,
    listener_address: std::net::SocketAddr,
    timeout: Duration,
) -> Result<std::net::TcpStream> {
    // `TcpListener` has no portable accept timeout. Block normally and use one
    // deadline watchdog connection solely to unblock a broken fixture
    // deterministically; the ordinary path cancels it as soon as the real
    // client connects. No polling or scheduler-sensitive sleeps.
    let (cancel_watchdog_tx, cancel_watchdog_rx) = mpsc::channel();
    let watchdog = thread::spawn(move || {
        if cancel_watchdog_rx.recv_timeout(timeout).is_err() {
            let _ = std::net::TcpStream::connect(listener_address);
        }
    });
    let accepted = listener.accept();
    let _ = cancel_watchdog_tx.send(());
    watchdog
        .join()
        .map_err(|_| anyhow!("test accept watchdog panicked"))?;
    let (stream, _) =
        accepted.context("test request should connect before the fixture deadline")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .context("test request should have a bounded read deadline")?;
    Ok(stream)
}

fn spawn_test_mcp_http_server(
    expected_requests: usize,
    handler: impl Fn(TestMcpHttpRequest) -> (u16, Value) + Send + Sync + 'static,
) -> (
    String,
    Arc<Mutex<Vec<TestMcpHttpRequest>>>,
    thread::JoinHandle<()>,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test server should bind");
    let base_url = format!(
        "http://{}",
        listener
            .local_addr()
            .expect("test server address should be readable")
    );
    let listener_address = listener
        .local_addr()
        .expect("test server address should be readable");
    let handler = Arc::new(handler);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let thread_requests = requests.clone();
    let server = thread::spawn(move || {
        for _ in 0..expected_requests {
            let mut stream = accept_test_mcp_http_stream(
                &listener,
                listener_address,
                TEST_MCP_HTTP_ACCEPT_DEADLINE,
            )
            .expect("test request should connect");
            let request = read_test_mcp_http_request(&mut stream);
            thread_requests
                .lock()
                .expect("request log mutex poisoned")
                .push(request.clone());
            let (status, body) = handler(request);
            write_test_mcp_http_json_response(&mut stream, status, body);
        }
    });
    (base_url, requests, server)
}

fn spawn_test_mcp_http_server_with_raw_response(
    status: u16,
    body: String,
) -> (String, thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test server should bind");
    let base_url = format!(
        "http://{}",
        listener
            .local_addr()
            .expect("test server address should be readable")
    );
    let listener_address = listener
        .local_addr()
        .expect("test server address should be readable");
    let server = thread::spawn(move || {
        let mut stream = accept_test_mcp_http_stream(
            &listener,
            listener_address,
            TEST_MCP_HTTP_ACCEPT_DEADLINE,
        )
        .expect("test request should connect");
        let _request = read_test_mcp_http_request(&mut stream);
        write_test_mcp_http_response(&mut stream, status, &body);
    });
    (base_url, server)
}

fn spawn_test_mcp_http_server_without_response() -> (
    String,
    mpsc::Receiver<TestMcpHttpRequest>,
    mpsc::Sender<()>,
    thread::JoinHandle<Result<TestMcpHttpRequest>>,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test server should bind");
    let listener_address = listener
        .local_addr()
        .expect("test server address should be readable");
    let base_url = format!(
        "http://{}",
        listener_address
    );
    let (request_tx, request_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = thread::spawn(move || -> Result<TestMcpHttpRequest> {
        let mut stream = accept_test_mcp_http_stream(
            &listener,
            listener_address,
            TEST_MCP_HTTP_ACCEPT_DEADLINE,
        )?;
        let request = try_read_test_mcp_http_request(&mut stream)?;
        request_tx
            .send(request.clone())
            .context("test request observer should remain connected")?;
        release_rx
            .recv()
            .context("test response hold should be released")?;
        Ok(request)
    });
    (base_url, request_rx, release_tx, server)
}

#[test]
fn test_mcp_http_accept_watchdog_bounds_a_missing_request() {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("test server should bind");
    let listener_address = listener
        .local_addr()
        .expect("test server address should be readable");
    let server = thread::spawn(move || -> Result<()> {
        let mut stream =
            accept_test_mcp_http_stream(&listener, listener_address, Duration::from_millis(50))?;
        try_read_test_mcp_http_request(&mut stream)?;
        Ok(())
    });

    let result = server
        .join()
        .expect("bounded missing-request fixture should not panic");
    assert!(
        result.is_err(),
        "watchdog connection should turn a missing request into a bounded fixture error"
    );
}

#[test]
fn delegation_mcp_base_tools_list_includes_role_scoped_tools() {
    let tools = mcp_tools_list_result();
    let names = tools
        .get("tools")
        .and_then(Value::as_array)
        .expect("tools list should be an array")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "termal_spawn_session",
            "termal_list_delegations",
            "termal_get_session_status",
            "termal_get_session_result",
            "termal_cancel_session",
            "termal_wait_delegations",
            "termal_resume_after_delegations",
            "termal_followup_session",
            "termal_submit_review_result",
            "termal_send_to_session",
            "termal_list_sessions",
            "termal_list_mailboxes",
            "termal_read_mailbox",
            "termal_read_mailbox_message",
            "termal_acknowledge_mailbox",
            "termal_board_list",
            "termal_board_get",
            "termal_board_set",
        ]
    );
    // The delegation inventory is parent-scoped; termal_list_sessions is the one
    // intentionally broad peer-discovery tool.
    assert_eq!(
        names
            .iter()
            .copied()
            .filter(|name| name.contains("list"))
            .collect::<Vec<_>>(),
        vec![
            "termal_list_delegations",
            "termal_list_sessions",
            "termal_list_mailboxes",
            "termal_board_list"
        ]
    );
}

#[test]
fn delegation_review_result_tool_advertises_bounded_control_plane_semantics() {
    let tools = mcp_tools_list_result();
    let tool = tools["tools"]
        .as_array()
        .expect("tools list should be an array")
        .iter()
        .find(|tool| tool["name"] == TERMAL_SUBMIT_REVIEW_RESULT_TOOL_NAME)
        .expect("review result tool should be advertised");

    assert_eq!(tool["description"], TERMAL_SUBMIT_REVIEW_RESULT_TOOL_DESCRIPTION);
    assert_eq!(tool["annotations"]["readOnlyHint"], false);
    assert_eq!(tool["annotations"]["destructiveHint"], false);
    assert_eq!(tool["annotations"]["idempotentHint"], true);
    assert_eq!(tool["annotations"]["openWorldHint"], false);
    assert_eq!(
        tool["inputSchema"]["properties"]["commandsRun"]["items"]["properties"]
            ["status"]["enum"],
        json!(["success", "error"])
    );
}

#[test]
fn claude_control_plane_tool_identity_requires_the_injected_server_scope() {
    assert_eq!(
        delegation_control_plane_capability_for_claude_tool_name(
            TERMAL_SUBMIT_REVIEW_RESULT_QUALIFIED_TOOL_NAME
        ),
        Some(DelegationControlPlaneCapability::SubmitReviewResult)
    );
    assert_eq!(
        delegation_control_plane_capability_for_claude_tool_name(
            "mcp__termal_delegation__termal_submit_review_result"
        ),
        None,
        "a foreign server whose name resembles a normalized alias must not inherit approval"
    );
    assert_eq!(
        delegation_control_plane_capability_for_claude_tool_name(
            TERMAL_SUBMIT_REVIEW_RESULT_TOOL_NAME
        ),
        None,
        "an unscoped leaf name cannot authenticate the TermAl MCP server"
    );
    assert_eq!(
        delegation_control_plane_capability_for_claude_tool_name("termal_send_to_session"),
        None
    );
}

#[test]
fn delegation_mcp_result_tool_advertises_bounded_full_output_paging() {
    let tools = mcp_tools_list_result();
    let result_tool = tools["tools"]
        .as_array()
        .expect("tools list should be an array")
        .iter()
        .find(|tool| tool["name"] == "termal_get_session_result")
        .expect("result tool should be advertised");

    assert!(result_tool["description"]
        .as_str()
        .expect("result tool should have a description")
        .contains("authoritative untruncated child output"));
    assert_eq!(
        result_tool["inputSchema"]["properties"]["outputOffset"]["minimum"],
        0
    );
    assert_eq!(
        result_tool["inputSchema"]["properties"]["outputLimit"]["minimum"],
        256
    );
    assert_eq!(
        result_tool["inputSchema"]["properties"]["outputLimit"]["maximum"],
        8192
    );
}

#[test]
fn delegation_mcp_result_tool_switches_to_paged_output_when_requested() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
        assert_eq!(request.method, "GET");
        if request.path.ends_with("/result") {
            return (200, json!({ "result": { "summary": "compact" } }));
        }
        assert_eq!(
            request.path,
            "/api/sessions/session-parent/delegations/delegation-large/result/output?offsetBytes=8192&limitBytes=4096"
        );
        (
            200,
            json!({
                "delegationId": "delegation-large",
                "output": "page",
                "offsetBytes": 8192,
                "nextOffsetBytes": 12288,
                "totalBytes": 20000,
                "complete": false
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let compact = bridge
        .tool_get_session_result(json!({ "delegationId": "delegation-large" }))
        .expect("compact result should succeed");
    assert_eq!(compact["result"]["summary"], "compact");

    let page = bridge
        .tool_get_session_result(json!({
            "delegationId": "delegation-large",
            "outputOffset": 8192,
            "outputLimit": 4096
        }))
        .expect("paged output should succeed");
    assert_eq!(page["output"], "page");
    assert_eq!(page["nextOffsetBytes"], 12288);

    server.join().expect("test server should join");
    assert_eq!(
        requests
            .lock()
            .expect("request log mutex poisoned")
            .len(),
        2
    );
}

#[test]
fn delegation_mcp_acknowledgement_description_teaches_idempotent_replay() {
    let tools = mcp_tools_list_result();
    let description = tools["tools"]
        .as_array()
        .expect("tools list should be an array")
        .iter()
        .find(|tool| tool["name"] == "termal_acknowledge_mailbox")
        .and_then(|tool| tool["description"].as_str())
        .expect("acknowledgement tool should have a description");

    assert!(description.contains("New progress requires the observed cursor to match"));
    assert!(description.contains("succeeds idempotently"));
    assert!(description.contains("advance past the durable cursor conflicts"));
}

#[test]
fn delegation_mcp_initialize_reports_tool_capability() {
    let result = mcp_initialize_result();
    assert_eq!(
        result.get("protocolVersion").and_then(Value::as_str),
        Some(TERMAL_DELEGATION_MCP_PROTOCOL_VERSION)
    );
    assert!(result.pointer("/capabilities/tools").is_some());
    assert_eq!(
        result.pointer("/serverInfo/name").and_then(Value::as_str),
        Some(TERMAL_DELEGATION_MCP_SERVER_NAME)
    );
}

#[test]
fn delegation_mcp_configs_bind_parent_session_and_base_url() {
    let command = "C:\\termal\\termal.exe";
    let parent = "session-parent";
    let base_url = "http://127.0.0.1:9999/";

    let claude =
        termal_delegation_mcp_claude_config_json_with_command(command, parent, base_url);
    let claude: Value = serde_json::from_str(&claude).expect("Claude config should be JSON");
    assert_eq!(
        claude.pointer("/mcpServers/termal-delegation/command"),
        Some(&Value::String(command.to_owned()))
    );
    assert_eq!(
        claude.pointer("/mcpServers/termal-delegation/args/2"),
        Some(&Value::String(parent.to_owned()))
    );
    assert_eq!(
        claude.pointer("/mcpServers/termal-delegation/args/4"),
        Some(&Value::String("http://127.0.0.1:9999".to_owned()))
    );

    let acp = termal_delegation_mcp_acp_servers_with_command(command, parent, base_url);
    assert_eq!(
        acp.pointer("/0/name").and_then(Value::as_str),
        Some(TERMAL_DELEGATION_MCP_SERVER_NAME)
    );
    assert_eq!(
        acp.pointer("/0/command"),
        Some(&Value::String(command.to_owned()))
    );
    assert_eq!(
        acp.pointer("/0/args/2"),
        Some(&Value::String(parent.to_owned()))
    );
    assert_eq!(acp.pointer("/0/env"), Some(&json!([])));

    let codex = termal_delegation_mcp_codex_config_with_command(command, parent, base_url);
    assert_eq!(
        codex.pointer("/mcp_servers/termal-delegation/args/2"),
        Some(&Value::String(parent.to_owned()))
    );
}

#[test]
fn delegation_mcp_acp_server_emits_spec_env_variable_array() {
    let server = TermalDelegationMcpStdioConfig {
        command: "termal".to_owned(),
        args: vec!["delegation-mcp".to_owned()],
        env: BTreeMap::from([
            ("EMPTY".to_owned(), String::new()),
            ("FOO".to_owned(), "bar".to_owned()),
        ]),
    };

    let descriptor = json!(termal_delegation_mcp_acp_server_from_stdio_config(
        &server
    ));
    let env = descriptor
        .get("env")
        .and_then(Value::as_array)
        .expect("emitted ACP McpServer.env should be an array");

    assert_eq!(env.len(), 2);
    assert!(env.contains(&json!({ "name": "EMPTY", "value": "" })));
    assert!(env.contains(&json!({ "name": "FOO", "value": "bar" })));
    assert!(env.iter().all(|variable| {
        variable
            .as_object()
            .is_some_and(|object| object.len() == 2)
    }));
}

#[test]
fn delegation_mcp_rejects_path_unsafe_parent_and_delegation_ids() {
    let err = match TermalDelegationMcpBridge::new(
        "session-parent/other".to_owned(),
        "http://127.0.0.1:9999".to_owned(),
    ) {
        Ok(_) => panic!("path-unsafe parent id should be rejected"),
        Err(err) => err,
    };
    assert!(err
        .to_string()
        .contains("delegation MCP serving session id must not contain"));

    let bridge = TermalDelegationMcpBridge::new(
        "session-parent".to_owned(),
        "http://127.0.0.1:9999".to_owned(),
    )
    .expect("path-safe parent id should be accepted");

    let err = bridge
        .tool_get_session_status(json!({ "delegationId": "delegation-bad/result" }))
        .expect_err("path-unsafe status delegation id should be rejected");
    assert!(err
        .to_string()
        .contains("delegationId must not contain /, \\, ?, #, %, or control characters"));

    let err = bridge
        .tool_wait_delegations(json!({
            "delegationIds": ["delegation-good", "delegation-bad?x"],
            "timeoutMs": 1
        }))
        .expect_err("path-unsafe wait delegation id should be rejected before polling");
    assert!(err
        .to_string()
        .contains("delegationIds must not contain /, \\, ?, #, %, or control characters"));

    let err = bridge
        .tool_get_session_status(json!({ "delegationId": "delegation%2Fbad" }))
        .expect_err("encoded slash delegation id should be rejected");
    assert!(err
        .to_string()
        .contains("delegationId must not contain /, \\, ?, #, %, or control characters"));

    let err = bridge
        .tool_get_session_status(json!({ "delegationId": ".." }))
        .expect_err("navigation-only delegation id should be rejected");
    assert!(err.to_string().contains("delegationId must not be . or .."));
}

#[test]
fn parse_mcp_slash_command_prompt_pins_ui_compatible_shape() {
    let parsed = parse_mcp_slash_command_prompt("/review-code staged -- include tests")
        .expect("valid slash command should parse");
    assert_eq!(parsed.command_name, "review-code");
    assert_eq!(parsed.arguments.as_deref(), Some("staged"));
    assert_eq!(parsed.note.as_deref(), Some("include tests"));

    let parsed = parse_mcp_slash_command_prompt("/review-code   ")
        .expect("trailing whitespace should not prevent parsing");
    assert_eq!(parsed.command_name, "review-code");
    assert_eq!(parsed.arguments, None);
    assert_eq!(parsed.note, None);

    let parsed = parse_mcp_slash_command_prompt("/review-code staged -- include tests\r")
        .expect("trailing carriage return should be trimmed like other trailing whitespace");
    assert_eq!(parsed.command_name, "review-code");
    assert_eq!(parsed.arguments.as_deref(), Some("staged"));
    assert_eq!(parsed.note.as_deref(), Some("include tests"));

    for prompt in [
        " /review-code",
        "/ review-code",
        "/",
        "/review/local",
        "/review-code\nextra",
        "/review-code\rextra",
        "review-code",
    ] {
        assert!(
            parse_mcp_slash_command_prompt(prompt).is_none(),
            "`{prompt}` should not be treated as an MCP slash command"
        );
    }
}

#[test]
fn split_mcp_agent_command_tail_pins_note_separator_edges() {
    let cases = [
        ("", None, None),
        ("staged", Some("staged"), None),
        ("staged -- include tests", Some("staged"), Some("include tests")),
        ("--", None, None),
        ("  --  ", None, None),
        ("-- include tests", None, Some("include tests")),
        ("staged --", Some("staged"), None),
        ("staged -- -- second", Some("staged"), Some("-- second")),
        ("staged ---x", Some("staged ---x"), None),
        ("staged-- include tests", Some("staged-- include tests"), None),
        ("  staged   --   include tests  ", Some("staged"), Some("include tests")),
        ("\tstaged\t--\tinclude tests\t", Some("staged"), Some("include tests")),
        ("\u{2003}staged\u{2003}", Some("staged"), None),
        (
            "staged\u{2003}--\u{2003}include tests",
            Some("staged\u{2003}--\u{2003}include tests"),
            None,
        ),
    ];

    for (tail, expected_arguments, expected_note) in cases {
        let (arguments, note) = split_mcp_agent_command_tail(tail);
        assert_eq!(
            arguments.as_deref(),
            expected_arguments,
            "arguments mismatch for `{tail}`"
        );
        assert_eq!(
            note.as_deref(),
            expected_note,
            "note mismatch for `{tail}`"
        );
    }
}

#[test]
fn delegation_mcp_indexed_child_with_null_marker_is_not_a_root_peer() {
    let session = json!({
        "id": "session-indexed-child",
        "name": "Reattached reviewer",
        "parentDelegationId": null
    });
    let delegation_child_ids = HashSet::from(["session-indexed-child"]);

    assert!(
        !is_root_peer_session(&session, &delegation_child_ids),
        "the durable delegation-child index must deny root eligibility even while link repair has not restored the redundant parent marker"
    );
}

#[test]
fn delegation_mcp_list_sessions_returns_root_sessions_only() {
    let (base_url, _requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/api/state");
        (
            200,
            json!({
                "sessions": [
                    { "id": "session-root-a", "name": "HelloMe", "agent": "Codex", "status": "idle", "workdir": "C:/a", "preview": "hi" },
                    { "id": "session-root-b", "name": "HelloMe2", "agent": "Codex", "status": "active", "workdir": "C:/b", "preview": "yo" },
                    { "id": "session-child", "name": "Codex /review-code", "agent": "Codex", "status": "idle", "parentDelegationId": "delegation-x" },
                    { "id": "session-child-unlinked", "name": "Leaked child", "agent": "Codex", "status": "idle" }
                ],
                "delegations": [
                    { "id": "delegation-y", "childSessionId": "session-child-unlinked" }
                ]
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let response = bridge
        .tool_list_sessions(json!({}))
        .expect("list should succeed");
    let sessions = response
        .get("sessions")
        .and_then(Value::as_array)
        .expect("sessions should be an array");
    let ids = sessions
        .iter()
        .filter_map(|session| session.get("sessionId").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["session-root-a", "session-root-b"]);
    assert_eq!(sessions[1]["name"], "HelloMe2");
    server.join().expect("test server should join");
}

#[test]
fn delegation_mcp_hides_and_rejects_peer_tools_for_delegation_child() {
    let (base_url, _requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/api/state");
        (
            200,
            json!({
                "sessions": [
                    { "id": "session-parent", "name": "Reviewer", "parentDelegationId": "delegation-x" }
                ],
                "delegations": [{
                    "id": "delegation-x",
                    "childSessionId": "session-parent",
                    "mode": "reviewer",
                    "reviewResultRequired": true
                }]
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    // tools/list omits the peer tools for a delegation-child caller...
    let names = bridge
        .tools_list_for_caller()
        .get("tools")
        .and_then(Value::as_array)
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_owned))
        .collect::<Vec<_>>();
    assert!(!names.iter().any(|name| name == "termal_send_to_session"));
    assert!(!names.iter().any(|name| name == "termal_list_sessions"));
    assert!(
        names
            .iter()
            .any(|name| name == "termal_submit_review_result"),
        "delegation children should receive only the dedicated result submission capability"
    );
    assert!(
        names.iter().any(|name| name == "termal_spawn_session"),
        "delegation tools stay available to children"
    );
    assert!(
        names.iter().any(|name| name == "termal_list_delegations"),
        "parent-scoped delegation recovery stays available to children"
    );

    // ...and invoking one through the dispatch is rejected.
    let err = bridge
        .handle_tool_call(json!({
            "name": "termal_send_to_session",
            "arguments": { "sessionId": "session-x", "message": "hi" }
        }))
        .expect_err("a delegation child must not invoke a peer tool");
    assert!(
        err.to_string().contains("root sessions"),
        "error should explain the root-only restriction: {err}"
    );
    server.join().expect("test server should join");
}

#[test]
fn delegation_mcp_hides_and_rejects_peer_tools_for_unlinked_durable_child() {
    let (base_url, _requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/api/state");
        (
            200,
            json!({
                "sessions": [
                    { "id": "session-parent", "name": "Reattached reviewer" }
                ],
                "delegations": [
                    {
                        "id": "delegation-x",
                        "childSessionId": "session-parent",
                        "mode": "reviewer",
                        "reviewResultRequired": true
                    }
                ]
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let names = bridge
        .tools_list_for_caller()
        .get("tools")
        .and_then(Value::as_array)
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_owned))
        .collect::<Vec<_>>();
    assert!(!names.iter().any(|name| name == "termal_send_to_session"));
    assert!(!names.iter().any(|name| name == "termal_list_sessions"));
    assert!(
        names
            .iter()
            .any(|name| name == "termal_submit_review_result")
    );

    let err = bridge
        .handle_tool_call(json!({
            "name": "termal_send_to_session",
            "arguments": { "sessionId": "session-x", "message": "hi" }
        }))
        .expect_err("a durable delegation child must not invoke a peer tool");
    assert!(
        err.to_string().contains("root sessions"),
        "error should explain the root-only restriction: {err}"
    );
    server.join().expect("test server should join");
}

#[test]
fn delegation_mcp_hides_and_rejects_review_submission_for_non_reviewer_child() {
    let (base_url, _requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/api/state");
        (
            200,
            json!({
                "sessions": [
                    { "id": "session-worker", "name": "Worker", "parentDelegationId": "delegation-worker" }
                ],
                "delegations": [{
                    "id": "delegation-worker",
                    "childSessionId": "session-worker",
                    "mode": "worker",
                    "reviewResultRequired": false
                }]
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-worker".to_owned(), base_url)
        .expect("bridge should initialize");

    let tools = bridge.tools_list_for_caller();
    let names = tools["tools"]
        .as_array()
        .expect("tools should be an array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert!(!names.contains(&TERMAL_SUBMIT_REVIEW_RESULT_TOOL_NAME));
    let error = bridge
        .handle_tool_call(json!({
            "name": TERMAL_SUBMIT_REVIEW_RESULT_TOOL_NAME,
            "arguments": {}
        }))
        .expect_err("non-reviewer child must not invoke structured review submission");
    assert!(error.to_string().contains("reviewer child"));
    server.join().expect("test server should join");
}

#[test]
fn delegation_mcp_submit_review_result_forwards_only_to_child_endpoint() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.path,
            "/api/sessions/session-reviewer/delegation-review-result"
        );
        let body: Value =
            serde_json::from_str(&request.body).expect("review result body should parse");
        assert_eq!(body["schemaVersion"], 1);
        assert_eq!(body["status"], "completed");
        assert_eq!(body["findings"][0]["severity"], "High");
        (
            202,
            json!({
                "mailboxId": "mailbox-review",
                "messageId": "mailbox-message-review",
                "sequence": 1,
                "unreadDepth": 1,
                "notificationDisposition": "durableButNotWoken",
                "duplicate": false
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new(
        "session-reviewer".to_owned(),
        base_url,
    )
    .expect("bridge should initialize");
    let response = bridge
        .tool_submit_review_result(json!({
            "schemaVersion": 1,
            "status": "completed",
            "summary": "One high issue.",
            "findings": [{
                "severity": "High",
                "file": "src/example.rs",
                "line": 7,
                "message": "Example finding"
            }],
            "commandsRun": [],
            "filesInspected": ["src/example.rs"],
            "notes": [],
            "suggestedTrackerUpdates": []
        }))
        .expect("structured result should reach the dedicated endpoint");
    assert_eq!(response["notificationDisposition"], "durableButNotWoken");
    server.join().expect("test server should join");
    assert_eq!(
        requests.lock().expect("request log mutex poisoned").len(),
        1
    );
}

#[test]
fn delegation_mcp_caller_eligibility_fails_closed_without_caching_transport_failure() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let handler_attempts = attempts.clone();
    let (base_url, requests, server) =
        spawn_test_mcp_http_server(2, move |request| {
            assert_eq!(request.method, "GET");
            assert_eq!(request.path, "/api/state");
            if handler_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                (
                    503,
                    json!({ "error": "temporary eligibility lookup failure" }),
                )
            } else {
                (
                    200,
                    json!({
                        "sessions": [
                            { "id": "session-parent", "name": "Root coordinator" }
                        ],
                        "delegations": []
                    }),
                )
            }
        });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let first_names = bridge
        .tools_list_for_caller()["tools"]
        .as_array()
        .expect("tools should be an array")
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert!(
        !first_names.iter().any(|name| name == "termal_send_to_session"),
        "uncertain caller eligibility must fail closed"
    );

    for _ in 0..2 {
        let recovered_names = bridge
            .tools_list_for_caller()["tools"]
            .as_array()
            .expect("tools should be an array")
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        assert!(
            recovered_names
                .iter()
                .any(|name| name == "termal_send_to_session"),
            "a successful root classification should recover and then cache"
        );
        assert!(
            !recovered_names
                .iter()
                .any(|name| name == "termal_submit_review_result"),
            "root sessions must not see the child-only review submission tool"
        );
    }

    server.join().expect("test server should join");
    assert_eq!(
        requests.lock().expect("request log mutex poisoned").len(),
        2,
        "the failed lookup must not cache, while the successful lookup must"
    );
}

#[test]
fn delegation_mcp_caller_eligibility_recovers_after_parent_becomes_visible() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let handler_attempts = attempts.clone();
    let (base_url, requests, server) =
        spawn_test_mcp_http_server(2, move |request| {
            assert_eq!(request.method, "GET");
            assert_eq!(request.path, "/api/state");
            if handler_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                (
                    200,
                    json!({
                        "sessions": [],
                        "delegations": []
                    }),
                )
            } else {
                (
                    200,
                    json!({
                        "sessions": [
                            { "id": "session-parent", "name": "Visible parent" }
                        ],
                        "delegations": []
                    }),
                )
            }
        });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let first_names = bridge
        .tools_list_for_caller()["tools"]
        .as_array()
        .expect("tools should be an array")
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert!(
        !first_names.iter().any(|name| name == "termal_send_to_session"),
        "a hidden caller omitted from state must fail closed before promotion"
    );

    for _ in 0..2 {
        let promoted_names = bridge
            .tools_list_for_caller()["tools"]
            .as_array()
            .expect("tools should be an array")
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        assert!(
            promoted_names
                .iter()
                .any(|name| name == "termal_send_to_session"),
            "the promoted visible root should recover and then cache"
        );
    }

    server.join().expect("test server should join");
    assert_eq!(
        requests.lock().expect("request log mutex poisoned").len(),
        2,
        "missing callers must not cache, while a present root classification must"
    );
}

#[test]
fn delegation_mcp_send_to_session_resolves_name_across_projects() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/api/state") => (
                200,
                json!({
                    "sessions": [
                        { "id": "session-kadry", "name": "Kadry", "projectId": "project-kadry" },
                        { "id": "session-legal", "name": "LegalCodex", "projectId": "project-rincon" }
                    ]
                }),
            ),
            ("POST", "/api/sessions/session-parent/mailboxes/send") => {
                let body: Value =
                    serde_json::from_str(&request.body).expect("send body should be JSON");
                assert_eq!(body["targetSessionId"], "session-legal");
                assert_eq!(body["message"], "hi legal");
                assert_eq!(body["idempotencyKey"], "legal-1");
                (
                    202,
                    json!({
                        "mailboxId": "mailbox-1",
                        "messageId": "mailbox-message-1",
                        "sequence": 1,
                        "unreadDepth": 1,
                        "notificationDisposition": "deliveredToIdleSession",
                        "duplicate": false
                    }),
                )
            }
            _ => (
                404,
                json!({ "error": format!("unexpected {} {}", request.method, request.path) }),
            ),
        }
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let response = bridge
        .tool_send_to_session(json!({
            "sessionId": "LegalCodex",
            "message": "hi legal",
            "idempotencyKey": "legal-1"
        }))
        .expect("send by name should resolve + deliver");
    assert_eq!(response["sessionId"], "session-legal");
    assert_eq!(response["resolvedFrom"], "LegalCodex");
    assert_eq!(response["mailboxId"], "mailbox-1");
    assert_eq!(
        response["notificationDisposition"],
        "deliveredToIdleSession"
    );
    assert_eq!(response["duplicate"], false);
    server.join().expect("test server should join");
    let requests = requests.lock().expect("request log mutex poisoned");
    assert_eq!(requests.len(), 2);
}

#[test]
fn delegation_mcp_send_to_session_unknown_name_errors() {
    let (base_url, _requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(request.path, "/api/state");
        (
            200,
            json!({ "sessions": [ { "id": "session-a", "name": "Alpha", "projectId": "p" } ] }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let err = bridge
        .tool_send_to_session(json!({
            "sessionId": "Nonexistent",
            "message": "hi",
            "idempotencyKey": "unknown-1"
        }))
        .expect_err("unknown name should error");
    assert!(
        err.to_string().contains("termal_list_sessions"),
        "error should guide to termal_list_sessions: {err}"
    );
    server.join().expect("test server should join");
}

// Exact ids take the sustained-traffic fast path: the backend authoritatively validates
// peer eligibility, so the bridge must not serialize /api/state before every append.
#[test]
fn delegation_mcp_send_to_session_posts_message_to_target() {
    let idempotency_key = r"peer/1?#%\stable";
    let (base_url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/api/sessions/session-parent/mailboxes/send") => {
                let body: Value =
                    serde_json::from_str(&request.body).expect("send body should be JSON");
                assert_eq!(body["targetSessionId"], "session-peer");
                assert_eq!(body["message"], "hello peer");
                assert_eq!(body["idempotencyKey"], idempotency_key);
                (
                    202,
                    json!({
                        "mailboxId": "mailbox-1",
                        "messageId": "mailbox-message-1",
                        "sequence": 1,
                        "unreadDepth": 1,
                        "notificationDisposition": "queuedBehindActiveTurn",
                        "duplicate": false
                    }),
                )
            }
            _ => (
                404,
                json!({ "error": format!("unexpected {} {}", request.method, request.path) }),
            ),
        }
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let response = bridge
        .tool_send_to_session(json!({
            "sessionId": "session-peer",
            "message": "hello peer",
            "idempotencyKey": idempotency_key
        }))
        .expect("send-to-session should succeed");

    assert_eq!(response["sessionId"], "session-peer");
    assert_eq!(response["mailboxId"], "mailbox-1");
    assert_eq!(
        response["notificationDisposition"],
        "queuedBehindActiveTurn"
    );
    server.join().expect("test server should join");
    let requests = requests.lock().expect("request log mutex poisoned");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
}

#[test]
fn delegation_mcp_exact_id_burst_fetches_caller_eligibility_once() {
    const BURST_SIZE: usize = 32;
    let sequence = Arc::new(AtomicUsize::new(0));
    let handler_sequence = sequence.clone();
    let (base_url, requests, server) =
        spawn_test_mcp_http_server(BURST_SIZE + 1, move |request| {
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/api/state") => (
                    200,
                    json!({
                        "sessions": [
                            { "id": "session-parent", "name": "Root coordinator" }
                        ],
                        "delegations": []
                    }),
                ),
                ("POST", "/api/sessions/session-parent/mailboxes/send") => {
                    let body: Value =
                        serde_json::from_str(&request.body).expect("send body should be JSON");
                    assert_eq!(body["targetSessionId"], "session-peer");
                    let next = handler_sequence.fetch_add(1, Ordering::SeqCst) + 1;
                    assert_eq!(body["idempotencyKey"], format!("burst-{next}"));
                    (
                        202,
                        json!({
                            "mailboxId": "mailbox-1",
                            "messageId": format!("mailbox-message-{next}"),
                            "sequence": next,
                            "unreadDepth": next,
                            "notificationDisposition": "queuedBehindActiveTurn",
                            "duplicate": false
                        }),
                    )
                }
                _ => (
                    404,
                    json!({ "error": format!("unexpected {} {}", request.method, request.path) }),
                ),
            }
        });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    for expected_sequence in 1..=BURST_SIZE {
        let tool_result = bridge
            .handle_tool_call(json!({
                "name": "termal_send_to_session",
                "arguments": {
                    "sessionId": "session-peer",
                    "message": format!("burst message {expected_sequence}"),
                    "idempotencyKey": format!("burst-{expected_sequence}")
                }
            }))
            .expect("every exact-id burst send should return its receipt");
        let receipt: Value = serde_json::from_str(
            tool_result["content"][0]["text"]
                .as_str()
                .expect("tool result should contain receipt JSON"),
        )
        .expect("tool receipt should decode");
        assert_eq!(receipt["sequence"], expected_sequence);
        assert_eq!(receipt["duplicate"], false);
    }

    server.join().expect("test server should join");
    let requests = requests.lock().expect("request log mutex poisoned");
    assert_eq!(requests.len(), BURST_SIZE + 1);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "GET" && request.path == "/api/state")
            .count(),
        1,
        "caller eligibility should be cached for the bridge lifetime: {requests:?}"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| {
                request.method == "POST" && request.path.ends_with("/mailboxes/send")
            })
            .count(),
        BURST_SIZE
    );
}

#[test]
fn delegation_mcp_send_timeout_reports_unknown_outcome_and_safe_retry() {
    let (base_url, request_rx, release_server, server) =
        spawn_test_mcp_http_server_without_response();
    // The production client exposes one total request deadline, not a separate
    // response-read deadline. Use enough headroom that full-suite scheduling,
    // connect, and request upload cannot become the condition under test; the
    // server handshake below proves the request arrived before it withholds
    // the response.
    let bridge = TermalDelegationMcpBridge::new_with_timeout(
        "session-parent".to_owned(),
        base_url,
        Duration::from_secs(5),
    )
    .expect("bridge should initialize");

    let client = thread::spawn(move || bridge.tool_send_to_session(json!({
            "sessionId": "session-peer",
            "message": "durable intent",
            "idempotencyKey": "timeout-retry-1"
        })));
    let observed_request = request_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("server should observe the request before holding its response");
    let result = client.join().expect("test client should join");
    release_server
        .send(())
        .expect("test response hold should release");
    let request = server
        .join()
        .expect("test server should join")
        .expect("test server should receive the request");
    assert_eq!(request.path, observed_request.path);
    let err = result
        .expect_err("missing response must report a transport timeout");
    let message = err.to_string();
    assert!(
        message.contains("append outcome is unknown"),
        "committed-vs-uncommitted ambiguity must be explicit: {message}"
    );
    assert!(
        message.contains("same idempotencyKey"),
        "the diagnostic must prescribe the safe recovery path: {message}"
    );
    assert!(
        message.contains("POST /api/sessions/session-parent/mailboxes/send")
            && message.contains("timed out after"),
        "the diagnostic must retain route and transport classification: {message}"
    );

    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/api/sessions/session-parent/mailboxes/send");
}

#[test]
fn delegation_mcp_send_unusable_success_prescribes_same_key_retry() {
    for body in [
        "{".to_owned(),
        json!({
            "mailboxId": "mailbox-1",
            "messageId": "mailbox-message-1"
        })
        .to_string(),
    ] {
        let (base_url, server) = spawn_test_mcp_http_server_with_raw_response(202, body);
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        let error = bridge
            .tool_send_to_session(json!({
                "sessionId": "session-peer",
                "message": "durable intent",
                "idempotencyKey": "unusable-success-1"
            }))
            .expect_err("an unusable successful response must be an unknown outcome");
        let message = error.to_string();
        assert!(
            message.contains("append outcome is unknown")
                && message.contains("same idempotencyKey")
                && message.contains("unusable successful response"),
            "send decode/shape failures must retain the safe retry prescription: {message}"
        );
        server.join().expect("test server should join");
    }
}

#[test]
fn delegation_mcp_send_preserves_retryable_backend_details() {
    let (base_url, _requests, server) = spawn_test_mcp_http_server(1, move |_request| {
        (
            503,
            json!({
                "error": "mailbox storage is temporarily busy; retry the same request"
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let error = bridge
        .tool_send_to_session(json!({
            "sessionId": "session-peer",
            "message": "durable intent",
            "idempotencyKey": "busy-1"
        }))
        .expect_err("backend admission exhaustion must stay typed");
    assert_eq!(
        error.to_string(),
        "TermAl delegation API returned 503 Service Unavailable: mailbox storage is temporarily busy; retry the same request"
    );
    server.join().expect("test server should join");
}

// `sessionId` is interpolated into the request path, and the `url` crate resolves
// dot segments — so an unvalidated `session-`-prefixed reference turned
// termal_send_to_session into a POST primitive against arbitrary routes. The path
// validator must reject the traversal shape BEFORE any request is issued.
// Neuter-verified: swapping `required_path_identifier` back to `required_string` makes
// this fail (the reference reaches the resolver instead of being rejected outright).
#[test]
fn delegation_mcp_send_to_session_rejects_path_traversal_reference() {
    // Zero expected requests: rejection must happen before any HTTP call.
    let (base_url, requests, server) = spawn_test_mcp_http_server(0, move |request| {
        (
            500,
            json!({ "error": format!("no request expected: {} {}", request.method, request.path) }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    for reference in [
        "session-x/../../sessions/victim/stop#",
        "session-x/../../sessions/victim/stop",
        "session-a%2f..%2fvictim",
        "../session-victim",
        "session-x\\..\\victim",
    ] {
        let err = bridge
            .tool_send_to_session(json!({
                "sessionId": reference,
                "message": "hi",
                "idempotencyKey": "traversal-1"
            }))
            .expect_err("a path-traversal reference must be rejected");
        assert!(
            err.to_string().contains("sessionId must not contain"),
            "reference `{reference}` should be rejected by the path validator: {err}"
        );
    }

    server.join().expect("test server should join");
    let requests = requests.lock().expect("request log mutex poisoned");
    assert!(
        requests.is_empty(),
        "a rejected reference must not reach the backend: {requests:?}"
    );
}

// Exact-id sends intentionally skip target discovery, so the backend remains
// the authoritative root-only boundary. This fixture pins exact pass-through
// formatting using the production error text; the actual backend eligibility
// behavior is exercised by `mailbox_backend_rejects_exact_delegation_child_target_before_append`.
#[test]
fn delegation_mcp_send_to_session_rejects_delegation_child_and_self_targets() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
        assert_eq!(
            request.path,
            "/api/sessions/session-parent/mailboxes/send"
        );
        (
            400,
            json!({
                "error": "target must be a local root session"
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let child_err = bridge
        .tool_send_to_session(json!({
            "sessionId": "session-child",
            "message": "hi",
            "idempotencyKey": "child-1"
        }))
        .expect_err("a delegation child must not be a peer target");
    assert!(
        child_err.to_string().contains(
            "TermAl delegation API returned 400 Bad Request: target must be a local root session"
        ),
        "the production backend error text must survive the bridge: {child_err}"
    );

    let unlinked_child_err = bridge
        .tool_send_to_session(json!({
            "sessionId": "session-child-unlinked",
            "message": "hi",
            "idempotencyKey": "unlinked-child-1"
        }))
        .expect_err("a child named only by the durable delegation row must not be a peer target");
    assert!(
        unlinked_child_err
            .to_string()
            .contains("target must be a local root session"),
        "the backend error must pass through for an exact child id: {unlinked_child_err}"
    );

    let self_err = bridge
        .tool_send_to_session(json!({
            "sessionId": "session-parent",
            "message": "hi",
            "idempotencyKey": "self-1"
        }))
        .expect_err("a session must not peer-message itself");
    assert!(
        self_err.to_string().contains("is this session"),
        "self target should be rejected explicitly: {self_err}"
    );

    server.join().expect("test server should join");
    assert_eq!(
        requests.lock().expect("request log mutex poisoned").len(),
        2,
        "self-target rejection must remain local while child ids reach backend validation"
    );
}

#[test]
fn delegation_mcp_acknowledgement_preserves_backend_conflict_details() {
    let (base_url, _requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(
            request.path,
            "/api/sessions/session-parent/mailboxes/mailbox-1/acknowledge"
        );
        (
            409,
            json!({
                "error": "mailbox acknowledgement conflict: processedThrough no longer equals 35"
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let err = bridge
        .tool_acknowledge_mailbox(json!({
            "mailboxId": "mailbox-1",
            "expectedProcessedThrough": 35,
            "processedThrough": 62
        }))
        .expect_err("stale acknowledgement must conflict");
    assert_eq!(
        err.to_string(),
        "TermAl delegation API returned 409 Conflict: mailbox acknowledgement conflict: processedThrough no longer equals 35"
    );
    server.join().expect("test server should join");
}

#[test]
fn delegation_mcp_acknowledgement_timeout_prescribes_cursor_reconciliation() {
    let (base_url, request_rx, release_server, server) =
        spawn_test_mcp_http_server_without_response();
    // See the send-timeout test: this isolates response loss from local
    // scheduling/connect time, and the channel handshake carries the proof.
    let bridge = TermalDelegationMcpBridge::new_with_timeout(
        "session-parent".to_owned(),
        base_url,
        Duration::from_secs(5),
    )
    .expect("bridge should initialize");

    let client = thread::spawn(move || bridge.tool_acknowledge_mailbox(json!({
            "mailboxId": "mailbox-1",
            "expectedProcessedThrough": 35,
            "processedThrough": 62
        })));
    let observed_request = request_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("server should observe the request before holding its response");
    let result = client.join().expect("test client should join");
    release_server
        .send(())
        .expect("test response hold should release");
    let request = server
        .join()
        .expect("test server should join")
        .expect("test server should receive the request");
    assert_eq!(request.path, observed_request.path);
    let err = result
        .expect_err("missing acknowledgement response must report an unknown outcome");
    let message = err.to_string();
    assert!(
        message.contains("cursor outcome is unknown")
            && message.contains("termal_list_mailboxes")
            && message.contains("expectedProcessedThrough"),
        "acknowledgement transport diagnostics must prescribe cursor reconciliation: {message}"
    );
    assert!(
        message.contains(
            "POST /api/sessions/session-parent/mailboxes/mailbox-1/acknowledge"
        ) && message.contains("timed out after"),
        "the route and timeout classification must survive: {message}"
    );
    assert_eq!(request.method, "POST");
    assert_eq!(
        request.path,
        "/api/sessions/session-parent/mailboxes/mailbox-1/acknowledge"
    );
}

#[test]
fn delegation_mcp_acknowledgement_unusable_success_prescribes_cursor_reconciliation() {
    for body in [
        "{".to_owned(),
        json!({
            "id": "mailbox-1",
            "participants": []
        })
        .to_string(),
    ] {
        let (base_url, server) = spawn_test_mcp_http_server_with_raw_response(200, body);
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        let error = bridge
            .tool_acknowledge_mailbox(json!({
                "mailboxId": "mailbox-1",
                "expectedProcessedThrough": 35,
                "processedThrough": 62
            }))
            .expect_err("an unusable successful acknowledgement must be an unknown outcome");
        let message = error.to_string();
        assert!(
            message.contains("cursor outcome is unknown")
                && message.contains("termal_list_mailboxes")
                && message.contains("expectedProcessedThrough")
                && message.contains("unusable successful response"),
            "ack decode/shape failures must prescribe cursor reconciliation: {message}"
        );
        server.join().expect("test server should join");
    }
}

#[test]
fn delegation_mcp_mailbox_tools_list_read_exact_and_acknowledge() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(4, move |request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/api/sessions/session-parent/mailboxes") => (
                200,
                json!([{
                    "id": "mailbox-1",
                    "participants": [],
                    "latestSequence": 3,
                    "unreadCount": 2
                }]),
            ),
            ("POST", "/api/sessions/session-parent/mailboxes/mailbox-1/read") => {
                let body: Value =
                    serde_json::from_str(&request.body).expect("read body should be JSON");
                assert_eq!(body["afterSequence"], 1);
                assert_eq!(body["limit"], 25);
                (200, json!([{ "id": "mailbox-message-2", "sequence": 2 }]))
            }
            ("GET", "/api/sessions/session-parent/mailbox-messages/mailbox-message-2") => {
                (200, json!({ "id": "mailbox-message-2", "sequence": 2 }))
            }
            (
                "POST",
                "/api/sessions/session-parent/mailboxes/mailbox-1/acknowledge",
            ) => {
                let body: Value =
                    serde_json::from_str(&request.body).expect("ack body should be JSON");
                assert_eq!(body["expectedProcessedThrough"], 1);
                assert_eq!(body["processedThrough"], 3);
                (
                    200,
                    json!({
                        "id": "mailbox-1",
                        "participants": [],
                        "latestSequence": 3,
                        "unreadCount": 0
                    }),
                )
            }
            _ => (
                404,
                json!({ "error": format!("unexpected {} {}", request.method, request.path) }),
            ),
        }
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let listed = bridge
        .tool_list_mailboxes(json!({}))
        .expect("mailboxes should list");
    assert_eq!(listed["mailboxes"][0]["id"], "mailbox-1");
    let range = bridge
        .tool_read_mailbox(json!({
            "mailboxId": "mailbox-1",
            "afterSequence": 1,
            "limit": 25
        }))
        .expect("mailbox range should read");
    assert_eq!(range["messages"][0]["sequence"], 2);
    let exact = bridge
        .tool_read_mailbox_message(json!({
            "messageId": "mailbox-message-2"
        }))
        .expect("exact mailbox message should read");
    assert_eq!(exact["sequence"], 2);
    let ack = bridge
        .tool_acknowledge_mailbox(json!({
            "mailboxId": "mailbox-1",
            "expectedProcessedThrough": 1,
            "processedThrough": 3
        }))
        .expect("mailbox acknowledgement should succeed");
    assert_eq!(ack["unreadCount"], 0);

    server.join().expect("test server should join");
    assert_eq!(
        requests.lock().expect("request log mutex poisoned").len(),
        4
    );
}

#[test]
fn delegation_mcp_spawn_session_posts_parent_scoped_request() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/sessions/session-parent/delegations");
        let body: Value =
            serde_json::from_str(&request.body).expect("spawn body should be JSON");
        assert_eq!(body["prompt"], "Review this patch");
        assert_eq!(body["title"], "Codex review");
        assert_eq!(body["cwd"], "C:\\repo");
        assert_eq!(body["agent"], "Codex");
        assert_eq!(body["model"], "gpt-5.4");
        assert_eq!(body["mode"], "reviewer");
        assert_eq!(body.pointer("/writePolicy/kind"), Some(&json!("readOnly")));
        (
            200,
            json!({
                "delegation": {
                    "id": "delegation-one",
                    "status": "running"
                },
                "childSessionId": "session-child"
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let response = bridge
        .tool_spawn_session(json!({
            "prompt": "Review this patch",
            "title": "Codex review",
            "cwd": "C:\\repo",
            "agent": "Codex",
            "model": "gpt-5.4",
            "mode": "reviewer",
            "writePolicy": "readOnly"
        }))
        .expect("spawn should post delegation request");

    assert_eq!(response.pointer("/delegation/id"), Some(&json!("delegation-one")));
    assert_eq!(response["childSessionId"], "session-child");
    server.join().expect("test server should join");
    assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 1);
}

#[test]
fn delegation_mcp_spawn_schema_documents_mode_and_agent_boundaries() {
    let tools = mcp_tools_list_result();
    let spawn = tools["tools"]
        .as_array()
        .expect("tools should be an array")
        .iter()
        .find(|tool| tool["name"] == "termal_spawn_session")
        .expect("spawn tool should be advertised");
    let description = spawn["description"]
        .as_str()
        .expect("spawn description should be text");
    assert!(
        description.contains("defaults to reviewer")
            && description.contains("reviewer mode supports only Claude or Codex")
            && description.contains("Cursor and Gemini")
            && description.contains("pass explorer")
            && description.contains("OpenCode")
            && description.contains("isolatedWorktree"),
        "spawn description must explain the default, reviewer-agent boundary, and ACP alternatives: {description}"
    );
    assert!(
        spawn
            .pointer("/inputSchema/properties/agent/enum")
            .and_then(Value::as_array)
            .is_some_and(|agents| agents.contains(&json!("OpenCode"))),
        "OpenCode must be offered as a first-class delegated agent"
    );
    let agent_description = spawn
        .pointer("/inputSchema/properties/agent/description")
        .and_then(Value::as_str)
        .expect("agent schema should explain reviewer compatibility");
    let mode_description = spawn
        .pointer("/inputSchema/properties/mode/description")
        .and_then(Value::as_str)
        .expect("mode schema should explain its default and compatibility");
    assert!(
        agent_description.contains("Reviewer mode requires Claude or Codex")
            && agent_description.contains("explorer")
            && mode_description.contains("Defaults to reviewer")
            && mode_description.contains("ACP agents should pass explorer"),
        "spawn input schema must teach callers the same constraints as the tool description"
    );
}

#[test]
fn delegation_mcp_list_recovers_ids_for_resume_without_respawning() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/api/sessions/session-parent/delegations") => (
                200,
                json!({
                    "revision": 7,
                    "delegations": [
                        {
                            "id": "delegation-running",
                            "parentSessionId": "session-parent",
                            "childSessionId": "session-child-running",
                            "mode": "reviewer",
                            "status": "running",
                            "title": "Same review",
                            "agent": "Codex",
                            "writePolicy": { "kind": "readOnly" },
                            "createdAt": "2026-07-22T10:00:00Z"
                        },
                        {
                            "id": "delegation-completed",
                            "parentSessionId": "session-parent",
                            "childSessionId": "session-child-completed",
                            "mode": "reviewer",
                            "status": "completed",
                            "title": "Same review",
                            "agent": "Claude",
                            "writePolicy": { "kind": "readOnly" },
                            "createdAt": "2026-07-22T10:00:01Z"
                        }
                    ],
                    "serverInstanceId": "server-test"
                }),
            ),
            ("POST", "/api/sessions/session-parent/delegation-waits") => {
                let body: Value = serde_json::from_str(&request.body)
                    .expect("resume wait body should be JSON");
                assert_eq!(
                    body["delegationIds"],
                    json!(["delegation-running", "delegation-completed"])
                );
                assert_eq!(body["mode"], "all");
                (201, json!({ "wait": { "id": "wait-recovered" } }))
            }
            other => panic!("unexpected request: {other:?}"),
        }
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let listed = bridge
        .tool_list_delegations(json!({}))
        .expect("delegation inventory should be recoverable");
    let delegations = listed["delegations"]
        .as_array()
        .expect("delegations should be an array");
    assert_eq!(delegations.len(), 2);
    assert_eq!(delegations[0]["id"], "delegation-running");
    assert_eq!(delegations[0]["childSessionId"], "session-child-running");
    assert_eq!(delegations[0]["status"], "running");
    assert_eq!(delegations[1]["id"], "delegation-completed");
    assert_eq!(delegations[1]["childSessionId"], "session-child-completed");
    assert_eq!(delegations[1]["status"], "completed");
    assert_eq!(delegations[0]["title"], delegations[1]["title"]);

    let recovered_ids = delegations
        .iter()
        .map(|delegation| delegation["id"].clone())
        .collect::<Vec<_>>();
    let resumed = bridge
        .tool_resume_after_delegations(json!({
            "delegationIds": recovered_ids,
            "mode": "all"
        }))
        .expect("recovered ids should schedule a resume wait");
    assert_eq!(resumed.pointer("/wait/id"), Some(&json!("wait-recovered")));

    server.join().expect("test server should join");
    assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 2);
}

#[test]
fn delegation_mcp_spawn_session_resolves_known_slash_command_prompt() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/api/sessions/session-parent/agent-commands/review-code/resolve") => {
                let body: Value =
                    serde_json::from_str(&request.body).expect("resolve body should be JSON");
                assert_eq!(body["arguments"], "staged");
                assert_eq!(body["note"], "include tests");
                assert_eq!(body["cwd"], "C:\\repo\\child");
                assert_eq!(body["intent"], "delegate");
                (
                    200,
                    json!({
                        "name": "review-code",
                        "visiblePrompt": "/review-code staged",
                        "expandedPrompt": "Expanded review-code command body",
                        "title": "Review local changes",
                        "delegation": {
                            "mode": "explorer",
                            "writePolicy": { "kind": "isolatedWorktree", "ownedPaths": [] }
                        }
                    }),
                )
            }
            ("POST", "/api/sessions/session-parent/delegations") => {
                let body: Value = serde_json::from_str(&request.body)
                    .expect("delegation body should be JSON");
                assert_eq!(body["prompt"], "Expanded review-code command body");
                assert_eq!(body["title"], "Review local changes");
                assert_eq!(body["cwd"], "C:\\repo\\child");
                assert_eq!(body["mode"], "explorer");
                assert_eq!(
                    body.pointer("/writePolicy/kind"),
                    Some(&json!("isolatedWorktree"))
                );
                (
                    200,
                    json!({
                        "delegation": {
                            "id": "delegation-one",
                            "status": "running"
                        },
                        "childSessionId": "session-child"
                    }),
                )
            }
            other => panic!("unexpected request: {other:?}"),
        }
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let response = bridge
        .tool_spawn_session(json!({
            "prompt": "/review-code staged -- include tests",
            "agent": "Codex",
            "cwd": "C:\\repo\\child"
        }))
        .expect("spawn should resolve the slash command then post delegation request");

    assert_eq!(response.pointer("/delegation/id"), Some(&json!("delegation-one")));
    server.join().expect("test server should join");
    assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 2);
}

#[test]
fn delegation_mcp_spawn_session_preserves_literal_prompt_for_unknown_slash_command() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/api/sessions/session-parent/agent-commands/unknown/resolve") => {
                (
                    404,
                    json!({
                        "error": "agent command not found"
                    }),
                )
            }
            ("POST", "/api/sessions/session-parent/delegations") => {
                let body: Value = serde_json::from_str(&request.body)
                    .expect("delegation body should be JSON");
                assert_eq!(body["prompt"], "/unknown keep literal");
                (
                    200,
                    json!({
                        "delegation": {
                            "id": "delegation-one",
                            "status": "running"
                        },
                        "childSessionId": "session-child"
                    }),
                )
            }
            other => panic!("unexpected request: {other:?}"),
        }
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    bridge
        .tool_spawn_session(json!({
            "prompt": "/unknown keep literal"
        }))
        .expect("unknown slash-like prompts should remain literal");

    server.join().expect("test server should join");
    assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 2);
}

#[test]
fn delegation_mcp_spawn_session_surfaces_non_command_not_found_resolve_errors() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.path,
            "/api/sessions/session-parent/agent-commands/review-code/resolve"
        );
        (
            404,
            json!({
                "error": "session not found"
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let err = bridge
        .tool_spawn_session(json!({
            "prompt": "/review-code"
        }))
        .expect_err("non-command 404 should surface");

    assert!(err
        .to_string()
        .contains("TermAl delegation API returned 404 Not Found: session not found"));
    server.join().expect("test server should join");
    assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 1);
}

#[test]
fn delegation_mcp_spawn_session_encodes_slash_command_path_segment() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/api/sessions/session-parent/agent-commands/review%3Alocal/resolve") => {
                (
                    200,
                    json!({
                        "name": "review:local",
                        "visiblePrompt": "/review:local",
                        "expandedPrompt": "Expanded colon command"
                    }),
                )
            }
            ("POST", "/api/sessions/session-parent/delegations") => {
                let body: Value = serde_json::from_str(&request.body)
                    .expect("delegation body should be JSON");
                assert_eq!(body["prompt"], "Expanded colon command");
                (
                    200,
                    json!({
                        "delegation": {
                            "id": "delegation-one",
                            "status": "running"
                        },
                        "childSessionId": "session-child"
                    }),
                )
            }
            other => panic!("unexpected request: {other:?}"),
        }
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    bridge
        .tool_spawn_session(json!({
            "prompt": "/review:local"
        }))
        .expect("command names should be encoded as a path segment");

    server.join().expect("test server should join");
    assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 2);
}

#[test]
fn delegation_mcp_spawn_session_allows_percent_in_encoded_command_name() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/api/sessions/session-parent/agent-commands/review%25local/resolve") => {
                (
                    200,
                    json!({
                        "name": "review%local",
                        "visiblePrompt": "/review%local",
                        "expandedPrompt": "Expanded percent command"
                    }),
                )
            }
            ("POST", "/api/sessions/session-parent/delegations") => {
                let body: Value = serde_json::from_str(&request.body)
                    .expect("delegation body should be JSON");
                assert_eq!(body["prompt"], "Expanded percent command");
                (
                    200,
                    json!({
                        "delegation": {
                            "id": "delegation-one",
                            "status": "running"
                        },
                        "childSessionId": "session-child"
                    }),
                )
            }
            other => panic!("unexpected request: {other:?}"),
        }
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    bridge
        .tool_spawn_session(json!({
            "prompt": "/review%local"
        }))
        .expect("literal percent command names should be encoded as a path segment");

    server.join().expect("test server should join");
    assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 2);
}

#[test]
fn delegation_mcp_spawn_session_rejects_parent_known_command_missing_from_requested_cwd() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.path,
            "/api/sessions/session-parent/agent-commands/review-code/resolve"
        );
        let body: Value =
            serde_json::from_str(&request.body).expect("resolve body should be JSON");
        if body.get("cwd").is_some() {
            return (
                404,
                json!({
                    "error": "agent command not found"
                }),
            );
        }
        (
            200,
            json!({
                "name": "review-code",
                "visiblePrompt": "/review-code",
                "expandedPrompt": "Parent-scope review command"
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let err = bridge
        .tool_spawn_session(json!({
            "prompt": "/review-code",
            "cwd": "C:\\repo\\child"
        }))
        .expect_err("parent-known command missing from requested cwd should fail");

    assert!(err
        .to_string()
        .contains("agent command `review-code` was not found in requested cwd"));
    server.join().expect("test server should join");
    assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 2);
}

#[test]
fn delegation_mcp_spawn_session_preserves_multiline_slash_like_prompt() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/sessions/session-parent/delegations");
        let body: Value =
            serde_json::from_str(&request.body).expect("delegation body should be JSON");
        assert_eq!(body["prompt"], "/review-code\nleave this literal");
        (
            200,
            json!({
                "delegation": {
                    "id": "delegation-one",
                    "status": "running"
                },
                "childSessionId": "session-child"
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    bridge
        .tool_spawn_session(json!({
            "prompt": "/review-code\nleave this literal"
        }))
        .expect("multiline prompts should not be slash-expanded");

    server.join().expect("test server should join");
    assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 1);
}

#[test]
fn delegation_mcp_spawn_session_preserves_spaced_slash_like_prompt() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/sessions/session-parent/delegations");
        let body: Value =
            serde_json::from_str(&request.body).expect("delegation body should be JSON");
        assert_eq!(body["prompt"], "/ review-code");
        (
            200,
            json!({
                "delegation": {
                    "id": "delegation-one",
                    "status": "running"
                },
                "childSessionId": "session-child"
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    bridge
        .tool_spawn_session(json!({
            "prompt": "/ review-code"
        }))
        .expect("slash followed by whitespace should stay literal like the UI parser");

    server.join().expect("test server should join");
    assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 1);
}

#[test]
fn delegation_mcp_spawn_session_explicit_options_override_resolved_defaults() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/api/sessions/session-parent/agent-commands/review-code/resolve") => {
                let body: Value =
                    serde_json::from_str(&request.body).expect("resolve body should be JSON");
                assert!(body.get("arguments").is_none());
                assert!(body.get("note").is_none());
                assert_eq!(body["intent"], "delegate");
                (
                    200,
                    json!({
                        "name": "review-code",
                        "visiblePrompt": "/review-code",
                        "expandedPrompt": "Expanded review-code command body",
                        "title": "Resolved title",
                        "delegation": {
                            "mode": "explorer",
                            "writePolicy": { "kind": "isolatedWorktree", "ownedPaths": [] }
                        }
                    }),
                )
            }
            ("POST", "/api/sessions/session-parent/delegations") => {
                let body: Value = serde_json::from_str(&request.body)
                    .expect("delegation body should be JSON");
                assert_eq!(body["prompt"], "Expanded review-code command body");
                assert_eq!(body["title"], "Explicit title");
                assert_eq!(body["mode"], "reviewer");
                assert_eq!(body.pointer("/writePolicy/kind"), Some(&json!("readOnly")));
                (
                    200,
                    json!({
                        "delegation": {
                            "id": "delegation-one",
                            "status": "running"
                        },
                        "childSessionId": "session-child"
                    }),
                )
            }
            other => panic!("unexpected request: {other:?}"),
        }
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    bridge
        .tool_spawn_session(json!({
            "prompt": "/review-code",
            "title": "Explicit title",
            "mode": "reviewer",
            "writePolicy": "readOnly"
        }))
        .expect("explicit spawn options should override resolved defaults");

    server.join().expect("test server should join");
    assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 2);
}

#[test]
fn delegation_mcp_spawn_session_rejects_empty_resolved_prompt() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.path,
            "/api/sessions/session-parent/agent-commands/review-code/resolve"
        );
        (
            200,
            json!({
                "name": "review-code",
                "visiblePrompt": " ",
                "expandedPrompt": ""
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let err = bridge
        .tool_spawn_session(json!({
            "prompt": "/review-code"
        }))
        .expect_err("empty resolved prompts should be rejected before spawning");

    assert!(err
        .to_string()
        .contains("agent command `review-code` resolved without prompt content"));
    server.join().expect("test server should join");
    assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 1);
}

#[test]
fn delegation_mcp_resume_after_delegations_posts_backend_wait() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/sessions/session-parent/delegation-waits");
        let body: Value =
            serde_json::from_str(&request.body).expect("resume wait body should be JSON");
        assert_eq!(
            body["delegationIds"],
            json!(["delegation-codex", "delegation-claude"])
        );
        assert_eq!(body["mode"], "all");
        assert_eq!(body["title"], "Delegated review fan-in");
        (
            200,
            json!({
                "waitId": "delegation-wait-one",
                "mode": "all",
                "queued": true
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let response = bridge
        .tool_resume_after_delegations(json!({
            "delegationIds": ["delegation-codex", "delegation-claude"],
            "mode": "all",
            "title": "Delegated review fan-in"
        }))
        .expect("resume wait should post request");

    assert_eq!(response["waitId"], "delegation-wait-one");
    assert_eq!(response["queued"], true);
    server.join().expect("test server should join");
    assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 1);
}

#[test]
fn delegation_mcp_tools_call_wraps_api_result_as_text_content() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(request.method, "GET");
        assert_eq!(
            request.path,
            "/api/sessions/session-parent/delegations/delegation-one"
        );
        (200, json!({ "delegation": { "status": "completed" } }))
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let response = bridge
        .handle_single_message(json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "termal_get_session_status",
                "arguments": {
                    "delegationId": "delegation-one"
                }
            }
        }))
        .expect("tools/call should handle request")
        .expect("tools/call should return a response");

    assert_eq!(response["id"], 7);
    assert_eq!(response.pointer("/result/isError"), Some(&json!(false)));
    let text = response
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .expect("tool response should contain JSON text");
    let payload: Value = serde_json::from_str(text).expect("tool text should be JSON");
    assert_eq!(payload.pointer("/delegation/status"), Some(&json!("completed")));
    server.join().expect("test server should join");
    assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 1);
}

#[test]
fn delegation_mcp_wait_polls_until_terminal_then_fetches_result() {
    let status_calls = Arc::new(AtomicUsize::new(0));
    let handler_status_calls = status_calls.clone();
    let (base_url, requests, server) = spawn_test_mcp_http_server(3, move |request| {
        assert_eq!(request.method, "GET");
        assert!(request.body.is_empty());
        match request.path.as_str() {
            "/api/sessions/session-parent/delegations/delegation-done" => {
                let call = handler_status_calls.fetch_add(1, Ordering::SeqCst);
                let status = if call == 0 { "running" } else { "completed" };
                (200, json!({ "delegation": { "status": status } }))
            }
            "/api/sessions/session-parent/delegations/delegation-done/result" => (
                200,
                json!({
                    "result": {
                        "status": "completed",
                        "summary": "MCP wait observed completion."
                    }
                }),
            ),
            _ => (
                404,
                json!({ "error": format!("unexpected path {}", request.path) }),
            ),
        }
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let response = bridge
        .tool_wait_delegations(json!({
            "delegationIds": ["delegation-done"],
            "mode": "all",
            "pollIntervalMs": 100,
            "timeoutMs": 2_000
        }))
        .expect("wait should complete");

    assert_eq!(response["timedOut"], false);
    assert_eq!(
        response.pointer("/statuses/0/delegation/status"),
        Some(&Value::String("completed".to_owned()))
    );
    assert_eq!(
        response.pointer("/results/0/result/result/summary"),
        Some(&Value::String("MCP wait observed completion.".to_owned()))
    );
    assert_eq!(status_calls.load(Ordering::SeqCst), 2);
    server.join().expect("test server should join");
    let requests = requests.lock().expect("request log mutex poisoned");
    assert_eq!(requests.len(), 3);
    assert!(
        requests
            .iter()
            .any(|request| request.path.ends_with("/delegation-done/result")),
        "terminal wait must fetch the result packet after status turns terminal"
    );
}

#[test]
fn delegation_mcp_wait_treats_completed_failed_and_canceled_as_terminal() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(6, move |request| {
        assert_eq!(request.method, "GET");
        assert!(request.body.is_empty());
        match request.path.as_str() {
            "/api/sessions/session-parent/delegations/delegation-completed" => {
                (200, json!({ "delegation": { "status": "completed" } }))
            }
            "/api/sessions/session-parent/delegations/delegation-failed" => {
                (200, json!({ "delegation": { "status": "failed" } }))
            }
            "/api/sessions/session-parent/delegations/delegation-canceled" => {
                (200, json!({ "delegation": { "status": "canceled" } }))
            }
            "/api/sessions/session-parent/delegations/delegation-completed/result" => (
                200,
                json!({ "result": { "status": "completed", "summary": "completed" } }),
            ),
            "/api/sessions/session-parent/delegations/delegation-failed/result" => (
                200,
                json!({ "result": { "status": "failed", "summary": "failed" } }),
            ),
            "/api/sessions/session-parent/delegations/delegation-canceled/result" => (
                200,
                json!({ "result": { "status": "canceled", "summary": "canceled" } }),
            ),
            _ => (
                404,
                json!({ "error": format!("unexpected path {}", request.path) }),
            ),
        }
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let response = bridge
        .tool_wait_delegations(json!({
            "delegationIds": [
                "delegation-completed",
                "delegation-failed",
                "delegation-canceled"
            ],
            "mode": "all",
            "pollIntervalMs": 100,
            "timeoutMs": 2_000
        }))
        .expect("wait should complete");

    assert_eq!(response["timedOut"], false);
    assert_eq!(
        response.pointer("/statuses/0/delegation/status"),
        Some(&Value::String("completed".to_owned()))
    );
    assert_eq!(
        response.pointer("/statuses/1/delegation/status"),
        Some(&Value::String("failed".to_owned()))
    );
    assert_eq!(
        response.pointer("/statuses/2/delegation/status"),
        Some(&Value::String("canceled".to_owned()))
    );
    let results = response
        .get("results")
        .and_then(Value::as_array)
        .expect("results should be an array");
    assert_eq!(results.len(), 3);
    assert!(
        results.iter().all(|result| result.get("error").is_none()),
        "all terminal statuses should get result fetch attempts without synthetic errors"
    );
    server.join().expect("test server should join");
    let requests = requests.lock().expect("request log mutex poisoned");
    assert_eq!(requests.len(), 6);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.path.ends_with("/result"))
            .count(),
        3
    );
}

thread_local! {
    // Per-test-thread record of retry sleeps; the Rust test harness gives each
    // #[test] its own thread, so recordings never bleed between tests.
    static RECORDED_SAFE_REPLAY_RETRY_SLEEPS: std::cell::RefCell<Vec<Duration>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn recording_safe_replay_retry_sleeper(delay: Duration) {
    RECORDED_SAFE_REPLAY_RETRY_SLEEPS.with(|sleeps| sleeps.borrow_mut().push(delay));
}

fn take_recorded_safe_replay_retry_sleeps() -> Vec<Duration> {
    RECORDED_SAFE_REPLAY_RETRY_SLEEPS
        .with(|sleeps| std::mem::take(&mut *sleeps.borrow_mut()))
}

// The exact jittered schedule the production bridge derives for a session:
// tests assert recorded sleeps against this so cadence stays pinned while the
// jitter keeps peers dephased.
fn expected_safe_replay_retry_sleeps(session_id: &str, replays: u32) -> Vec<Duration> {
    (1..=replays)
        .map(|attempt| safe_replay_retry_delay(session_id, attempt))
        .collect()
}

// Keep the bridge replay fixture coupled to the production SQLite mapper so
// BEGIN/COMMIT contention cannot silently drift away from the classifier.
fn test_no_commit_busy_error() -> String {
    mailbox_sqlite_write_error(
        "waiting to begin mailbox append",
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("database is locked".to_owned()),
        ),
    )
    .to_string()
}
const TEST_SAFE_READ_BUSY_ERROR: &str = "coordination board storage is temporarily busy while \
     waiting for the coordination board connection; no mutation was attempted by this read \
     operation, so retry the same request";

#[test]
fn delegation_mcp_bridge_replays_no_commit_storage_busy_rejections_until_success() {
    let handler_attempts = Arc::new(AtomicUsize::new(0));
    let attempts = handler_attempts.clone();
    let (base_url, requests, server) = spawn_test_mcp_http_server(3, move |_request| {
        if attempts.fetch_add(1, Ordering::SeqCst) < 2 {
            (503, json!({ "error": test_no_commit_busy_error() }))
        } else {
            (200, json!({ "ok": true }))
        }
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should build")
        .with_safe_replay_retry_sleeper(recording_safe_replay_retry_sleeper);
    let value = bridge
        .post_json_with_safe_replay("/api/mailboxes/append", &json!({ "payload": 1 }))
        .expect("a no-commit rejection cleared by a later attempt should succeed");
    assert_eq!(value, json!({ "ok": true }));
    assert_eq!(
        take_recorded_safe_replay_retry_sleeps(),
        expected_safe_replay_retry_sleeps("session-parent", 2),
        "sleeps must follow the session's exact jittered doubling schedule"
    );
    server.join().expect("test server should join");
    let requests = requests.lock().expect("request log mutex poisoned");
    assert_eq!(requests.len(), 3, "two rejected attempts plus the success");
    assert!(
        requests
            .iter()
            .all(|request| request.method == "POST"
                && request.path == "/api/mailboxes/append"
                && request.body == requests[0].body),
        "every replay must be byte-identical to the original request"
    );
}

#[test]
fn delegation_mcp_bridge_generic_requests_never_inherit_replay_from_prose() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(1, |_request| {
        (503, json!({ "error": test_no_commit_busy_error() }))
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should build")
        .with_safe_replay_retry_sleeper(recording_safe_replay_retry_sleeper);
    let error = bridge
        .post_json("/api/future-non-idempotent-route", &json!({ "payload": 1 }))
        .expect_err("generic requests must remain single-attempt even with matching prose");
    assert_eq!(
        error
            .downcast_ref::<TermalDelegationApiError>()
            .expect("failure should surface the API error")
            .status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(
        take_recorded_safe_replay_retry_sleeps().is_empty(),
        "generic requests must not enter the safe-replay policy"
    );
    server.join().expect("test server should join");
    assert_eq!(
        requests.lock().expect("request log mutex poisoned").len(),
        1
    );
}

#[test]
fn delegation_mcp_bridge_fails_fast_on_a_503_without_the_no_commit_marker() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(1, |_request| {
        (
            503,
            json!({ "error": "service unavailable while restarting; write outcome unknown" }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should build")
        .with_safe_replay_retry_sleeper(recording_safe_replay_retry_sleeper);
    let error = bridge
        .post_json_with_safe_replay("/api/mailboxes/append", &json!({ "payload": 1 }))
        .expect_err("an unmarked 503 offers no replay guarantee and must fail fast");
    let api_error = error
        .downcast_ref::<TermalDelegationApiError>()
        .expect("failure should surface the API error");
    assert_eq!(api_error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        take_recorded_safe_replay_retry_sleeps().is_empty(),
        "no sleeps: unmarked rejections must not be retried"
    );
    server.join().expect("test server should join");
    assert_eq!(
        requests.lock().expect("request log mutex poisoned").len(),
        1
    );
}

#[test]
fn delegation_mcp_bridge_bounds_no_commit_replays_and_surfaces_the_final_rejection() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(5, |_request| {
        (503, json!({ "error": test_no_commit_busy_error() }))
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should build")
        .with_safe_replay_retry_sleeper(recording_safe_replay_retry_sleeper);
    let error = bridge
        .post_json_with_safe_replay("/api/mailboxes/append", &json!({ "payload": 1 }))
        .expect_err("a persistently saturated writer must surface the typed rejection");
    let api_error = error
        .downcast_ref::<TermalDelegationApiError>()
        .expect("failure should surface the API error");
    assert_eq!(api_error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        message_carries_safe_replay_clause(&api_error.message),
        "the caller must still see the typed no-commit message after exhaustion"
    );
    assert_eq!(
        take_recorded_safe_replay_retry_sleeps(),
        expected_safe_replay_retry_sleeps("session-parent", 4),
        "exactly four bounded jittered replays follow the initial attempt"
    );
    server.join().expect("test server should join");
    assert_eq!(
        requests.lock().expect("request log mutex poisoned").len(),
        5
    );
}

#[test]
fn delegation_mcp_bridge_applies_no_commit_replay_to_get_requests_too() {
    let handler_attempts = Arc::new(AtomicUsize::new(0));
    let attempts = handler_attempts.clone();
    let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |_request| {
        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            (503, json!({ "error": TEST_SAFE_READ_BUSY_ERROR }))
        } else {
            (200, json!({ "mailboxes": [] }))
        }
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should build")
        .with_safe_replay_retry_sleeper(recording_safe_replay_retry_sleeper);
    let value = bridge
        .get_json_with_safe_replay("/api/mailboxes")
        .expect("a safe-read rejection on GET should replay and succeed");
    assert_eq!(value, json!({ "mailboxes": [] }));
    assert_eq!(
        take_recorded_safe_replay_retry_sleeps(),
        expected_safe_replay_retry_sleeps("session-parent", 1)
    );
    server.join().expect("test server should join");
    let requests = requests.lock().expect("request log mutex poisoned");
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.method == "GET"));
}

#[test]
fn delegation_mcp_bridge_replays_a_durable_append_while_dispatch_finalizes() {
    let handler_attempts = Arc::new(AtomicUsize::new(0));
    let attempts = handler_attempts.clone();
    let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |_request| {
        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            (
                503,
                json!({
                    "error": format!(
                        "mailbox dispatch outcome is still finalizing; {}",
                        TERMAL_DELEGATION_DURABLE_APPEND_RETRY_CLAUSE
                    )
                }),
            )
        } else {
            (200, json!({ "ok": true }))
        }
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should build")
        .with_safe_replay_retry_sleeper(recording_safe_replay_retry_sleeper);
    let value = bridge
        .post_json_with_safe_replay("/api/mailboxes/append", &json!({ "payload": 1 }))
        .expect("durable same-key append replay should wait through finalization");
    assert_eq!(value, json!({ "ok": true }));
    assert_eq!(
        take_recorded_safe_replay_retry_sleeps(),
        expected_safe_replay_retry_sleeps("session-parent", 1)
    );
    server.join().expect("test server should join");
    assert_eq!(
        requests.lock().expect("request log mutex poisoned").len(),
        2
    );
}

#[test]
fn delegation_mcp_bridge_fails_fast_when_a_write_was_actually_committed() {
    // Cross-review finding: the instruction tail alone must never classify a
    // POSITIVE commit statement as replayable — only a clause carrying the
    // `no …` negation does.
    let (base_url, requests, server) = spawn_test_mcp_http_server(1, |_request| {
        (
            503,
            json!({
                "error": "a mailbox write was committed by this operation, so retry the same request"
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should build")
        .with_safe_replay_retry_sleeper(recording_safe_replay_retry_sleeper);
    let error = bridge
        .post_json_with_safe_replay("/api/mailboxes/append", &json!({ "payload": 1 }))
        .expect_err("a positive commit statement must never be replayed");
    let api_error = error
        .downcast_ref::<TermalDelegationApiError>()
        .expect("failure should surface the API error");
    assert_eq!(api_error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        take_recorded_safe_replay_retry_sleeps().is_empty(),
        "no sleeps: positive phrasing carries no structural no-commit guarantee"
    );
    server.join().expect("test server should join");
    assert_eq!(
        requests.lock().expect("request log mutex poisoned").len(),
        1
    );
}

#[test]
fn delegation_mcp_bridge_stops_replaying_when_the_overall_budget_cannot_fund_a_retry() {
    // With a 1s total budget, even the smallest possible first jittered delay
    // (150ms) plus the 2s minimum replay window exceeds what remains, so the
    // bridge must surface the typed rejection after ONE attempt and zero
    // sleeps. The stop decision is one-sided: any additional elapsed time only
    // strengthens it, so this holds under arbitrary scheduler stalls.
    let (base_url, requests, server) = spawn_test_mcp_http_server(1, |_request| {
        (503, json!({ "error": test_no_commit_busy_error() }))
    });
    let bridge = TermalDelegationMcpBridge::new_with_timeout(
        "session-parent".to_owned(),
        base_url,
        Duration::from_secs(1),
    )
    .expect("bridge should build")
    .with_safe_replay_retry_sleeper(recording_safe_replay_retry_sleeper);
    let error = bridge
        .post_json_with_safe_replay("/api/mailboxes/append", &json!({ "payload": 1 }))
        .expect_err("budget-starved replay must surface the typed rejection");
    let api_error = error
        .downcast_ref::<TermalDelegationApiError>()
        .expect("failure should surface the API error, not a transport timeout");
    assert_eq!(api_error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        take_recorded_safe_replay_retry_sleeps().is_empty(),
        "no sleep may start when the budget cannot fund delay plus a useful attempt"
    );
    server.join().expect("test server should join");
    assert_eq!(
        requests.lock().expect("request log mutex poisoned").len(),
        1
    );
}

#[test]
fn delegation_mcp_no_commit_jitter_is_bounded_stable_and_session_dephased() {
    for session_id in ["session-parent", "session-a", "session-b", "session-2098"] {
        for attempt in 1..=4u32 {
            let percent = safe_replay_retry_jitter_percent(session_id, attempt);
            assert!(
                (75..=125).contains(&percent),
                "jitter percent {percent} out of range for {session_id} attempt {attempt}"
            );
            assert_eq!(
                percent,
                safe_replay_retry_jitter_percent(session_id, attempt),
                "jitter must be deterministic for a fixed session and attempt"
            );
        }
    }
    assert_ne!(
        expected_safe_replay_retry_sleeps("session-a", 4),
        expected_safe_replay_retry_sleeps("session-b", 4),
        "distinct sessions must dephase onto distinct replay schedules"
    );
}

fn test_board_receipt_for(
    key: &str,
    prior_revision: u64,
    value: Value,
    deleted: bool,
    duplicate: bool,
) -> Value {
    json!({
        "key": key,
        "revision": prior_revision + 1,
        "priorRevision": prior_revision,
        "generation": 7,
        "value": value,
        "deleted": deleted,
        "authorSessionId": "session-parent",
        "authorName": "Fable",
        "updatedAt": "2026-07-26T00:00:00.000Z",
        "duplicate": duplicate
    })
}

#[test]
fn delegation_mcp_board_tools_are_advertised_to_roots_and_hidden_from_children() {
    let advertised = mcp_tools_list_result();
    let names = advertised
        .get("tools")
        .and_then(Value::as_array)
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_owned))
        .collect::<Vec<_>>();
    for tool in ["termal_board_list", "termal_board_get", "termal_board_set"] {
        assert!(
            names.iter().any(|name| name == tool),
            "{tool} must be advertised to root sessions"
        );
    }

    let (base_url, _requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(request.path, "/api/state");
        (
            200,
            json!({
                "sessions": [
                    { "id": "session-parent", "name": "Reviewer", "parentDelegationId": "delegation-x" }
                ]
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");
    let child_names = bridge
        .tools_list_for_caller()
        .get("tools")
        .and_then(Value::as_array)
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_owned))
        .collect::<Vec<_>>();
    for tool in ["termal_board_list", "termal_board_get", "termal_board_set"] {
        assert!(
            !child_names.iter().any(|name| name == tool),
            "{tool} must be hidden from delegation children"
        );
    }
    server.join().expect("test server should join");
}

#[test]
fn delegation_mcp_board_set_forwards_null_value_and_delete_distinctly() {
    let handler_calls = Arc::new(AtomicUsize::new(0));
    let calls = handler_calls.clone();
    let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/sessions/session-parent/board/set");
        let body: Value =
            serde_json::from_str(&request.body).expect("board set body should parse");
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            // Explicit JSON null must arrive as a PRESENT null value, not a
            // delete.
            assert!(body.get("value").is_some_and(Value::is_null));
            assert!(body.get("delete").is_none());
            (200, test_board_receipt_for("status.gate", 1, Value::Null, false, false))
        } else {
            assert!(body.get("value").is_none());
            assert_eq!(body.get("delete"), Some(&Value::Bool(true)));
            (200, test_board_receipt_for("status.gate", 2, Value::Null, true, false))
        }
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    bridge
        .tool_board_set(json!({
            "key": "status.gate",
            "value": null,
            "expectedRevision": 1,
            "idempotencyKey": "set-null-1"
        }))
        .expect("null-value set should succeed");
    bridge
        .tool_board_set(json!({
            "key": "status.gate",
            "delete": true,
            "expectedRevision": 2,
            "idempotencyKey": "delete-1"
        }))
        .expect("delete should succeed");
    server.join().expect("test server should join");
    assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 2);
}

#[test]
fn delegation_mcp_board_get_routes_to_the_key_path() {
    let (base_url, _requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(request.method, "GET");
        assert_eq!(
            request.path,
            "/api/sessions/session-parent/board/keys/activity.rust-suite"
        );
        (
            200,
            json!({
                "key": "activity.rust-suite",
                "revision": 2,
                "updatedAtGeneration": 3,
                "scopeGeneration": 7,
                "value": { "holder": "Fable" },
                "deleted": false,
                "authorSessionId": "session-parent",
                "authorName": "Fable",
                "updatedAt": "2026-07-26T00:00:00.000Z"
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");
    let head = bridge
        .tool_board_get(json!({ "key": "activity.rust-suite" }))
        .expect("get should succeed");
    assert_eq!(
        head.get("key").and_then(Value::as_str),
        Some("activity.rust-suite")
    );
    assert_eq!(head.get("updatedAtGeneration"), Some(&json!(3)));
    assert_eq!(head.get("scopeGeneration"), Some(&json!(7)));
    server.join().expect("test server should join");
}

#[test]
fn delegation_mcp_board_tools_reject_transport_unsafe_keys_client_side() {
    // No server: validation must fail before any request is attempted.
    let bridge = TermalDelegationMcpBridge::new(
        "session-parent".to_owned(),
        "http://127.0.0.1:9".to_owned(),
    )
    .expect("bridge should initialize");
    for unsafe_key in [
        ".",
        "..",
        "../evil",
        "a/b",
        "Upper.case",
        "a b",
        "k&e=y",
    ] {
        let err = bridge
            .tool_board_get(json!({ "key": unsafe_key }))
            .expect_err("transport-unsafe key must be rejected client-side");
        assert!(
            err.to_string().contains("lowercase alphanumerics"),
            "unexpected rejection for {unsafe_key:?}: {err}"
        );
    }
    // Empty keys are caught one layer earlier by the required-string check —
    // still client-side, different message.
    let err = bridge
        .tool_board_get(json!({ "key": "" }))
        .expect_err("empty key must be rejected client-side");
    assert!(err.to_string().contains("required"));
}

#[test]
fn delegation_mcp_board_set_preserves_state_stamp_bytes_for_idempotency() {
    let state_stamp = "  repo@abc123  ";
    let (base_url, _requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(request.path, "/api/sessions/session-parent/board/set");
        let body: Value =
            serde_json::from_str(&request.body).expect("board set body should be JSON");
        assert_eq!(body.get("stateStamp"), Some(&json!(state_stamp)));
        let mut receipt =
            test_board_receipt_for("status.gate", 0, json!(true), false, false);
        receipt
            .as_object_mut()
            .expect("test receipt should be an object")
            .insert("stateStamp".to_owned(), json!(state_stamp));
        (201, receipt)
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");

    let receipt = bridge
        .tool_board_set(json!({
            "key": "status.gate",
            "value": true,
            "expectedRevision": 0,
            "idempotencyKey": "state-stamp-preservation",
            "stateStamp": state_stamp
        }))
        .expect("board set should preserve the exact optional state stamp");

    assert_eq!(receipt.get("stateStamp"), Some(&json!(state_stamp)));
    server.join().expect("test server should join");
}

#[test]
fn delegation_mcp_board_set_rejects_invalid_state_stamp_instead_of_dropping_it() {
    // No server: validation must fail before any request is attempted.
    let bridge = TermalDelegationMcpBridge::new(
        "session-parent".to_owned(),
        "http://127.0.0.1:9".to_owned(),
    )
    .expect("bridge should initialize");
    for state_stamp in [json!(42), json!(null), json!("   ")] {
        let error = bridge
            .tool_board_set(json!({
                "key": "status.gate",
                "value": true,
                "expectedRevision": 0,
                "idempotencyKey": "invalid-state-stamp",
                "stateStamp": state_stamp
            }))
            .expect_err("present invalid stateStamp must be rejected client-side");
        assert!(
            error
                .to_string()
                .contains("stateStamp must be a non-empty string"),
            "unexpected validation error: {error:#}"
        );
    }
}

#[test]
fn delegation_mcp_board_child_direct_call_is_denied() {
    let (base_url, _requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(request.path, "/api/state");
        (
            200,
            json!({
                "sessions": [
                    { "id": "session-parent", "name": "Reviewer", "parentDelegationId": "delegation-x" }
                ]
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");
    let err = bridge
        .handle_tool_call(json!({
            "name": "termal_board_set",
            "arguments": {
                "key": "status.gate",
                "value": 1,
                "expectedRevision": 0,
                "idempotencyKey": "child-attempt"
            }
        }))
        .expect_err("a delegation child must not invoke board tools");
    assert!(
        err.to_string().contains("coordination tools are restricted"),
        "denial should name the coordination restriction: {err}"
    );
    server.join().expect("test server should join");
}

#[test]
fn delegation_mcp_board_set_passes_typed_conflict_through_unchanged() {
    let (base_url, _requests, server) = spawn_test_mcp_http_server(1, move |_request| {
        (
            409,
            json!({
                "error": "coordination board revision conflict for `status.gate`: expected 3, current revision is 5; detail: {\"currentGeneration\":9}"
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");
    let err = bridge
        .tool_board_set(json!({
            "key": "status.gate",
            "value": { "x": 1 },
            "expectedRevision": 3,
            "idempotencyKey": "cas-1"
        }))
        .expect_err("a typed conflict must fail");
    let message = err.to_string();
    assert!(
        message.contains("revision conflict") && message.contains("current revision is 5"),
        "typed conflict must pass through unchanged: {message}"
    );
    assert!(
        !message.contains("outcome is unknown"),
        "a typed 409 has a KNOWN outcome and must not carry the unknown-outcome diagnostic"
    );
    server.join().expect("test server should join");
}

#[test]
fn delegation_mcp_board_set_marks_unknown_outcome_on_malformed_success() {
    let (base_url, server) =
        spawn_test_mcp_http_server_with_raw_response(200, "not-json!".to_owned());
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");
    let err = bridge
        .tool_board_set(json!({
            "key": "status.gate",
            "value": 1,
            "expectedRevision": 0,
            "idempotencyKey": "unknown-1"
        }))
        .expect_err("malformed success body means the outcome is unknown");
    let message = err.to_string();
    assert!(
        message.contains("outcome is unknown") && message.contains("SAME idempotencyKey"),
        "unknown-outcome diagnostic must teach same-key retry: {message}"
    );
    server.join().expect("test server should join");
}

#[test]
fn delegation_mcp_board_set_inherits_no_commit_replay_and_validates_duplicate_receipts() {
    let handler_calls = Arc::new(AtomicUsize::new(0));
    let calls = handler_calls.clone();
    let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |_request| {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            (
                503,
                json!({
                    "error": "coordination board storage is temporarily busy while beginning coordination board update; no coordination board write was committed by this operation, so retry the same request"
                }),
            )
        } else {
            (
                200,
                test_board_receipt_for(
                    "activity.rust-suite",
                    1,
                    json!({ "holder": "Fable" }),
                    false,
                    true,
                ),
            )
        }
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize")
        .with_safe_replay_retry_sleeper(recording_safe_replay_retry_sleeper);
    let receipt = bridge
        .tool_board_set(json!({
            "key": "activity.rust-suite",
            "value": { "holder": "Fable" },
            "expectedRevision": 1,
            "idempotencyKey": "replay-1"
        }))
        .expect("marked 503 then success should replay and succeed");
    assert_eq!(receipt.get("duplicate"), Some(&Value::Bool(true)));
    assert_eq!(
        take_recorded_safe_replay_retry_sleeps(),
        expected_safe_replay_retry_sleeps("session-parent", 1),
        "board tools must inherit the bridge no-commit replay policy"
    );
    server.join().expect("test server should join");
    assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 2);
}

#[test]
fn delegation_mcp_board_list_routes_first_page_and_continuation_queries() {
    let handler_calls = Arc::new(AtomicUsize::new(0));
    let calls = handler_calls.clone();
    let (base_url, _requests, server) = spawn_test_mcp_http_server(2, move |request| {
        assert_eq!(request.method, "GET");
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            assert_eq!(
                request.path,
                "/api/sessions/session-parent/board?knownGeneration=5"
            );
        } else {
            assert_eq!(
                request.path,
                "/api/sessions/session-parent/board?afterKey=alpha.key&limit=50&snapshotGeneration=7"
            );
        }
        (
            200,
            json!({ "generation": 7, "entries": [], "unchanged": false }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");
    bridge
        .tool_board_list(json!({ "knownGeneration": 5 }))
        .expect("first-page list should succeed");
    bridge
        .tool_board_list(json!({
            "afterKey": "alpha.key",
            "limit": 50,
            "snapshotGeneration": 7
        }))
        .expect("continuation list should succeed");
    server.join().expect("test server should join");
}

#[test]
fn delegation_mcp_board_set_transport_timeout_is_unknown_outcome() {
    let (base_url, _request_rx, release_tx, server) =
        spawn_test_mcp_http_server_without_response();
    let bridge = TermalDelegationMcpBridge::new_with_timeout(
        "session-parent".to_owned(),
        base_url,
        Duration::from_millis(300),
    )
    .expect("bridge should initialize");
    let err = bridge
        .tool_board_set(json!({
            "key": "status.gate",
            "value": 1,
            "expectedRevision": 0,
            "idempotencyKey": "timeout-1"
        }))
        .expect_err("a transport timeout leaves the write outcome unknown");
    let message = err.to_string();
    assert!(
        message.contains("outcome is unknown") && message.contains("SAME idempotencyKey"),
        "transport timeout must teach same-key retry: {message}"
    );
    let _ = release_tx.send(());
    let _ = server.join();
}

#[test]
fn delegation_mcp_board_set_rejects_a_receipt_for_the_wrong_key_as_unknown_outcome() {
    let (base_url, _requests, server) = spawn_test_mcp_http_server(1, move |_request| {
        // Structurally valid receipt — for a DIFFERENT key than requested.
        (
            200,
            test_board_receipt_for("other.key", 0, json!(1), false, false),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");
    let err = bridge
        .tool_board_set(json!({
            "key": "status.gate",
            "value": 1,
            "expectedRevision": 0,
            "idempotencyKey": "wrong-key-1"
        }))
        .expect_err("a receipt for the wrong key must not confirm the mutation");
    let message = err.to_string();
    assert!(
        message.contains("does not correlate") && message.contains("outcome is unknown"),
        "mismatched receipt must map to unknown-outcome guidance: {message}"
    );
    server.join().expect("test server should join");
}

#[test]
fn delegation_mcp_board_set_rejects_a_delete_receipt_carrying_a_non_null_value() {
    let (base_url, _requests, server) = spawn_test_mcp_http_server(1, move |_request| {
        (
            200,
            json!({
                "key": "status.gate",
                "revision": 3,
                "priorRevision": 2,
                "generation": 9,
                "value": { "ghost": true },
                "deleted": true,
                "authorSessionId": "session-parent",
                "authorName": "Fable",
                "updatedAt": "2026-07-26T00:00:00.000Z",
                "duplicate": false
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");
    let err = bridge
        .tool_board_set(json!({
            "key": "status.gate",
            "delete": true,
            "expectedRevision": 2,
            "idempotencyKey": "ghost-delete-1"
        }))
        .expect_err("a deleted receipt with a non-null value is not our delete");
    let message = err.to_string();
    assert!(
        message.contains("non-null value") && message.contains("outcome is unknown"),
        "unexpected: {message}"
    );
    server.join().expect("test server should join");
}

#[test]
fn delegation_mcp_board_set_treats_a_max_prior_revision_receipt_as_mismatch_not_panic() {
    let (base_url, _requests, server) = spawn_test_mcp_http_server(1, move |_request| {
        (
            200,
            json!({
                "key": "status.gate",
                "revision": 0,
                "priorRevision": u64::MAX,
                "generation": 9,
                "value": 1,
                "deleted": false,
                "authorSessionId": "session-parent",
                "authorName": "Fable",
                "updatedAt": "2026-07-26T00:00:00.000Z",
                "duplicate": false
            }),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
        .expect("bridge should initialize");
    let err = bridge
        .tool_board_set(json!({
            "key": "status.gate",
            "value": 1,
            "expectedRevision": u64::MAX,
            "idempotencyKey": "max-prior-1"
        }))
        .expect_err("an untrusted MAX priorRevision must be a mismatch, never an overflow");
    let message = err.to_string();
    assert!(
        message.contains("does not correlate") && message.contains("outcome is unknown"),
        "unexpected: {message}"
    );
    server.join().expect("test server should join");
}
