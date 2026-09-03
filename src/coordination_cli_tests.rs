/*
 * Coordination CLI tests.
 *
 * Owns only assertions for `src/coordination_cli.rs`: the argument grammar,
 * exit-code classification, message-source resolution, the root-caller guard,
 * and the exact HTTP contracts the CLI forwards through the delegation MCP
 * bridge. It reuses the local HTTP fixture from `delegation_mcp_tests.rs` so
 * CLI and MCP behaviour are proved against the same fake backend. Declared from
 * the production fragment, like `delegation_mcp_tests.rs`, not from
 * `src/tests/mod.rs`.
 */

use super::delegation_mcp_tests::spawn_test_mcp_http_server;
use super::*;

fn cli_args(list: &[&str]) -> Vec<String> {
    list.iter().map(|argument| (*argument).to_owned()).collect()
}

fn usage_message(err: &anyhow::Error) -> String {
    err.downcast_ref::<CoordinationCliUsageError>()
        .map(|usage| usage.0.clone())
        .unwrap_or_else(|| panic!("expected a usage error, got: {err:#}"))
}

fn root_inventory_state() -> Value {
    json!({
        "sessions": [
            { "id": "session-root", "name": "Termal::Fable", "agent": "Claude", "status": "idle", "workdir": "C:/repo", "preview": "hi" },
            { "id": "session-peer", "name": "Termal::Codex", "agent": "Codex", "status": "active", "workdir": "C:/repo", "preview": "yo" },
            { "id": "session-child", "name": "Reviewer", "agent": "Codex", "status": "idle", "parentDelegationId": "delegation-x" },
            { "id": "session-child-unlinked", "name": "Leaked child", "agent": "Codex", "status": "idle" }
        ],
        "delegations": [
            { "id": "delegation-y", "childSessionId": "session-child-unlinked" }
        ]
    })
}

#[test]
fn coordination_cli_parses_sessions_list_variants() {
    let plain = parse_coordination_cli_args(cli_args(&["sessions", "list"]))
        .expect("bare sessions list should parse");
    assert_eq!(
        plain,
        CoordinationCliInvocation {
            command: CoordinationCliCommand::SessionsList { as_session: None },
            json: false,
            base_url: None,
        }
    );

    let scoped = parse_coordination_cli_args(cli_args(&[
        "sessions",
        "list",
        "--json",
        "--as-session=session-root",
        "--base-url",
        "http://127.0.0.1:9999/",
    ]))
    .expect("scoped sessions list should parse");
    assert_eq!(
        scoped,
        CoordinationCliInvocation {
            command: CoordinationCliCommand::SessionsList {
                as_session: Some("session-root".to_owned()),
            },
            json: true,
            base_url: Some("http://127.0.0.1:9999/".to_owned()),
        }
    );
}

#[test]
fn coordination_cli_parses_mailbox_send_with_both_flag_forms_and_optional_fields() {
    let invocation = parse_coordination_cli_args(cli_args(&[
        "mailbox",
        "send",
        "--as-session",
        "session-root",
        "--to=Termal::Codex",
        "--message",
        "hello there",
        "--idempotency-key=key-1",
        "--topic",
        "greeting",
        "--state-stamp=HEAD=abc",
        "--class",
        "routine",
    ]))
    .expect("mailbox send should parse");
    assert_eq!(
        invocation.command,
        CoordinationCliCommand::MailboxSend {
            as_session: "session-root".to_owned(),
            to: "Termal::Codex".to_owned(),
            message: CoordinationCliMessageSource::Inline("hello there".to_owned()),
            idempotency_key: "key-1".to_owned(),
            topic: Some("greeting".to_owned()),
            state_stamp: Some("HEAD=abc".to_owned()),
            class: Some("routine".to_owned()),
        }
    );
    assert!(!invocation.json);

    let from_stdin = parse_coordination_cli_args(cli_args(&[
        "mailbox",
        "send",
        "--as-session=session-root",
        "--to=session-peer",
        "--message-file=-",
        "--idempotency-key=key-2",
    ]))
    .expect("stdin message source should parse");
    match from_stdin.command {
        CoordinationCliCommand::MailboxSend { message, .. } => {
            assert_eq!(message, CoordinationCliMessageSource::Stdin);
        }
        other => panic!("unexpected command {other:?}"),
    }

    let from_file = parse_coordination_cli_args(cli_args(&[
        "mailbox",
        "send",
        "--as-session=session-root",
        "--to=session-peer",
        "--message-file",
        "C:/notes/message.txt",
        "--idempotency-key=key-3",
    ]))
    .expect("file message source should parse");
    match from_file.command {
        CoordinationCliCommand::MailboxSend { message, .. } => {
            assert_eq!(
                message,
                CoordinationCliMessageSource::File(PathBuf::from("C:/notes/message.txt"))
            );
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn coordination_cli_requires_exactly_one_message_source() {
    let both = parse_coordination_cli_args(cli_args(&[
        "mailbox",
        "send",
        "--as-session=session-root",
        "--to=session-peer",
        "--message=a",
        "--message-file=b.txt",
        "--idempotency-key=k",
    ]))
    .expect_err("both message sources must be rejected");
    assert!(usage_message(&both).contains("not both"));

    let none = parse_coordination_cli_args(cli_args(&[
        "mailbox",
        "send",
        "--as-session=session-root",
        "--to=session-peer",
        "--idempotency-key=k",
    ]))
    .expect_err("a missing message must be rejected");
    assert!(usage_message(&none).contains("--message"));

    let empty = parse_coordination_cli_args(cli_args(&[
        "mailbox",
        "send",
        "--as-session=session-root",
        "--to=session-peer",
        "--message=",
        "--idempotency-key=k",
    ]))
    .expect_err("an empty inline message must be rejected");
    assert!(usage_message(&empty).contains("empty"));
}

#[test]
fn coordination_cli_rejects_missing_required_unknown_duplicate_and_stray_arguments() {
    let missing = parse_coordination_cli_args(cli_args(&["mailbox", "list"]))
        .expect_err("mailbox list without --as-session must be rejected");
    assert!(usage_message(&missing).contains("`--as-session` is required"));
    assert_eq!(coordination_cli_exit_code(&missing), 2);

    let unknown = parse_coordination_cli_args(cli_args(&[
        "mailbox",
        "list",
        "--as-session=session-root",
        "--mailbox-id=mailbox-1",
    ]))
    .expect_err("a flag the command does not take must be rejected");
    assert!(usage_message(&unknown).contains("does not accept --mailbox-id"));

    let duplicate = parse_coordination_cli_args(cli_args(&[
        "mailbox",
        "list",
        "--as-session=session-root",
        "--as-session=session-other",
    ]))
    .expect_err("duplicate flags must be rejected");
    assert!(usage_message(&duplicate).contains("more than once"));

    let stray = parse_coordination_cli_args(cli_args(&["sessions", "list", "extra"]))
        .expect_err("positional leftovers must be rejected");
    assert!(usage_message(&stray).contains("unexpected argument `extra`"));

    let dangling = parse_coordination_cli_args(cli_args(&["mailbox", "list", "--as-session"]))
        .expect_err("a flag without its value must be rejected");
    assert!(usage_message(&dangling).contains("requires a value"));

    let unknown_verb = parse_coordination_cli_args(cli_args(&["mailbox", "purge"]))
        .expect_err("unknown subcommands must be rejected");
    assert!(usage_message(&unknown_verb).contains("unknown command `termal mailbox purge`"));

    let no_verb = parse_coordination_cli_args(cli_args(&["sessions"]))
        .expect_err("a group without a verb must be rejected");
    assert!(usage_message(&no_verb).contains("needs a subcommand"));

    let json_value = parse_coordination_cli_args(cli_args(&["sessions", "list", "--json=yes"]))
        .expect_err("--json with a value must be rejected");
    assert!(usage_message(&json_value).contains("takes no value"));
}

#[test]
fn coordination_cli_parses_numeric_cursors_and_rejects_invalid_ones() {
    let read = parse_coordination_cli_args(cli_args(&[
        "mailbox",
        "read",
        "--as-session=session-root",
        "--mailbox-id=mailbox-1",
        "--after=7",
        "--limit",
        "5",
    ]))
    .expect("mailbox read should parse");
    assert_eq!(
        read.command,
        CoordinationCliCommand::MailboxRead {
            as_session: "session-root".to_owned(),
            mailbox_id: "mailbox-1".to_owned(),
            after_sequence: Some(7),
            limit: Some(5),
        }
    );

    let acknowledge = parse_coordination_cli_args(cli_args(&[
        "mailbox",
        "acknowledge",
        "--as-session=session-root",
        "--mailbox-id=mailbox-1",
        "--expected=41",
        "--through=42",
    ]))
    .expect("mailbox acknowledge should parse");
    assert_eq!(
        acknowledge.command,
        CoordinationCliCommand::MailboxAcknowledge {
            as_session: "session-root".to_owned(),
            mailbox_id: "mailbox-1".to_owned(),
            expected_processed_through: 41,
            processed_through: 42,
        }
    );

    let negative = parse_coordination_cli_args(cli_args(&[
        "mailbox",
        "read",
        "--as-session=session-root",
        "--mailbox-id=mailbox-1",
        "--after=-1",
    ]))
    .expect_err("negative cursors must be rejected");
    assert!(usage_message(&negative).contains("non-negative integer"));

    let missing_cursor = parse_coordination_cli_args(cli_args(&[
        "mailbox",
        "acknowledge",
        "--as-session=session-root",
        "--mailbox-id=mailbox-1",
        "--through=42",
    ]))
    .expect_err("acknowledge without --expected must be rejected");
    assert!(usage_message(&missing_cursor).contains("`--expected` is required"));
}

#[test]
fn coordination_cli_help_is_not_a_usage_error() {
    for arguments in [
        vec!["mailbox", "--help"],
        vec!["sessions", "list", "-h"],
        vec!["mailbox", "send", "--as-session=session-root", "--help"],
    ] {
        let invocation = parse_coordination_cli_args(cli_args(&arguments))
            .expect("help must parse without an error");
        assert_eq!(invocation.command, CoordinationCliCommand::Help);
    }
    let mut rendered = Vec::new();
    render_coordination_cli_output(
        &CoordinationCliCommand::Help,
        &execute_coordination_cli(&CoordinationCliCommand::Help, "http://127.0.0.1:1")
            .expect("help needs no server"),
        &mut rendered,
    )
    .expect("help should render");
    assert!(String::from_utf8(rendered)
        .expect("help output should be UTF-8")
        .contains("termal mailbox send"));
}

#[test]
fn coordination_cli_message_file_strips_bom_and_rejects_empty_bodies() {
    let root = TestTempRoot::create("termal-coordination-cli-message-file");
    let path = root.path().join("message.txt");
    fs::write(&path, "\u{feff}line one\r\nline two\r\n").expect("message file should write");
    let text = resolve_coordination_cli_message(&CoordinationCliMessageSource::File(path.clone()))
        .expect("message file should resolve");
    assert_eq!(text, "line one\r\nline two\r\n");

    fs::write(&path, "\u{feff}   \r\n").expect("blank message file should write");
    let blank = resolve_coordination_cli_message(&CoordinationCliMessageSource::File(path))
        .expect_err("a blank message file must be rejected");
    assert!(usage_message(&blank).contains("empty"));

    let missing = resolve_coordination_cli_message(&CoordinationCliMessageSource::File(
        root.path().join("absent.txt"),
    ))
    .expect_err("a missing message file must be rejected before any request");
    assert!(usage_message(&missing).contains("absent.txt"));
    assert_eq!(coordination_cli_exit_code(&missing), 2);
}

#[test]
fn coordination_cli_sessions_list_returns_the_root_inventory_without_a_caller() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(1, |request| {
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/api/state");
        (200, root_inventory_state())
    });

    let output = execute_coordination_cli(
        &CoordinationCliCommand::SessionsList { as_session: None },
        &base_url,
    )
    .expect("sessions list should succeed");
    server.join().expect("test server should join");
    assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 1);

    let ids = output["sessions"]
        .as_array()
        .expect("sessions should be an array")
        .iter()
        .map(|session| session["sessionId"].as_str().expect("id").to_owned())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["session-root", "session-peer"]);
    assert_eq!(output["sessions"][0]["name"], "Termal::Fable");
    assert_eq!(output["sessions"][0]["workdir"], "C:/repo");
}

#[test]
fn coordination_cli_scoped_sessions_list_rejects_a_delegation_child_caller() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(1, |request| {
        assert_eq!(request.path, "/api/state");
        (200, root_inventory_state())
    });

    let err = execute_coordination_cli(
        &CoordinationCliCommand::SessionsList {
            as_session: Some("session-child".to_owned()),
        },
        &base_url,
    )
    .expect_err("a delegation child must not enumerate peers");
    server.join().expect("test server should join");
    assert!(err.to_string().contains("delegation-child session"));
    assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 1);
    assert_eq!(coordination_cli_exit_code(&err), 1);
}

#[test]
fn coordination_cli_mailbox_send_resolves_names_and_posts_through_the_bridge() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(3, |request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/api/state") => (200, root_inventory_state()),
            ("POST", "/api/sessions/session-root/mailboxes/send") => {
                let body: Value =
                    serde_json::from_str(&request.body).expect("send body should be JSON");
                assert_eq!(body["targetSessionId"], "session-peer");
                assert_eq!(body["message"], "hello peer");
                assert_eq!(body["idempotencyKey"], "cli-key-1");
                assert_eq!(body["topic"], "greeting");
                assert_eq!(body["stateStamp"], "HEAD=abc");
                assert!(body.get("class").is_none());
                (
                    202,
                    json!({
                        "mailboxId": "mailbox-1",
                        "messageId": "mailbox-message-1",
                        "sequence": 9,
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

    let output = execute_coordination_cli(
        &CoordinationCliCommand::MailboxSend {
            as_session: "session-root".to_owned(),
            to: "termal::codex".to_owned(),
            message: CoordinationCliMessageSource::Inline("hello peer".to_owned()),
            idempotency_key: "cli-key-1".to_owned(),
            topic: Some("greeting".to_owned()),
            state_stamp: Some("HEAD=abc".to_owned()),
            class: None,
        },
        &base_url,
    )
    .expect("mailbox send should succeed");
    server.join().expect("test server should join");

    let requests = requests.lock().expect("request log mutex poisoned");
    assert_eq!(
        requests
            .iter()
            .map(|request| (request.method.as_str(), request.path.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("GET", "/api/state"),
            ("GET", "/api/state"),
            ("POST", "/api/sessions/session-root/mailboxes/send"),
        ],
        "caller guard, then name resolution, then exactly one append"
    );
    assert_eq!(output["sessionId"], "session-peer");
    assert_eq!(output["resolvedFrom"], "termal::codex");
    assert_eq!(output["sequence"], 9);
    assert_eq!(output["duplicate"], false);
}

#[test]
fn coordination_cli_mailbox_commands_reject_child_and_self_targets_like_the_mcp_tools() {
    let (base_url, requests, server) = spawn_test_mcp_http_server(1, |request| {
        assert_eq!(request.path, "/api/state");
        (200, root_inventory_state())
    });
    let err = execute_coordination_cli(
        &CoordinationCliCommand::MailboxList {
            as_session: "session-child-unlinked".to_owned(),
        },
        &base_url,
    )
    .expect_err("a durable delegation child must be refused");
    server.join().expect("test server should join");
    assert!(err.to_string().contains("delegation-child session"));
    assert_eq!(
        requests.lock().expect("request log mutex poisoned").len(),
        1,
        "the refusal must happen before any mailbox request"
    );

    let (base_url, requests, server) = spawn_test_mcp_http_server(1, |request| {
        assert_eq!(request.path, "/api/state");
        (200, root_inventory_state())
    });
    let err = execute_coordination_cli(
        &CoordinationCliCommand::MailboxSend {
            as_session: "session-root".to_owned(),
            to: "session-root".to_owned(),
            message: CoordinationCliMessageSource::Inline("to myself".to_owned()),
            idempotency_key: "cli-key-self".to_owned(),
            topic: None,
            state_stamp: None,
            class: None,
        },
        &base_url,
    )
    .expect_err("sending to yourself must be refused");
    server.join().expect("test server should join");
    assert!(err.to_string().contains("is this session"));
    assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 1);
}

#[test]
fn coordination_cli_reports_unknown_callers_and_unreachable_servers_distinctly() {
    let (base_url, _requests, server) = spawn_test_mcp_http_server(1, |request| {
        assert_eq!(request.path, "/api/state");
        (200, root_inventory_state())
    });
    let unknown = execute_coordination_cli(
        &CoordinationCliCommand::MailboxList {
            as_session: "session-nobody".to_owned(),
        },
        &base_url,
    )
    .expect_err("an unknown caller must be refused");
    server.join().expect("test server should join");
    assert!(unknown.to_string().contains("not a session known"));
    assert!(unknown.to_string().contains("termal sessions list"));

    // Port 0 is never a listening port, so the connection fails immediately
    // and deterministically on every platform — no ephemeral port that could
    // be rebound between a probe and the request.
    let base_url = "http://127.0.0.1:0".to_owned();
    let unreachable = execute_coordination_cli(
        &CoordinationCliCommand::MailboxList {
            as_session: "session-root".to_owned(),
        },
        &base_url,
    )
    .expect_err("a closed port must be reported as unreachable");
    assert!(unreachable.to_string().contains("unreachable"));
    assert!(unreachable.to_string().contains(&base_url));
    assert_eq!(coordination_cli_exit_code(&unreachable), 1);
}

#[test]
fn coordination_cli_read_read_message_and_acknowledge_forward_exact_contracts() {
    let (base_url, _requests, server) = spawn_test_mcp_http_server(2, |request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/api/state") => (200, root_inventory_state()),
            ("POST", "/api/sessions/session-root/mailboxes/mailbox-1/read") => {
                let body: Value =
                    serde_json::from_str(&request.body).expect("read body should be JSON");
                assert_eq!(body, json!({ "afterSequence": 7, "limit": 5 }));
                (
                    200,
                    json!([{
                        "id": "mailbox-message-8",
                        "mailboxId": "mailbox-1",
                        "sequence": 8,
                        "senderSessionId": "session-peer",
                        "senderName": "Termal::Codex",
                        "targetSessionId": "session-root",
                        "targetName": "Termal::Fable",
                        "createdAt": "2026-09-03T00:00:00Z",
                        "class": "routine",
                        "topic": "hand-back",
                        "body": "verified",
                        "notificationState": "deliveredToIdleSession"
                    }]),
                )
            }
            _ => (
                404,
                json!({ "error": format!("unexpected {} {}", request.method, request.path) }),
            ),
        }
    });
    let read = execute_coordination_cli(
        &CoordinationCliCommand::MailboxRead {
            as_session: "session-root".to_owned(),
            mailbox_id: "mailbox-1".to_owned(),
            after_sequence: Some(7),
            limit: Some(5),
        },
        &base_url,
    )
    .expect("mailbox read should succeed");
    server.join().expect("test server should join");
    assert_eq!(read["mailboxId"], "mailbox-1");
    assert_eq!(read["messages"][0]["sequence"], 8);
    assert_eq!(read["messages"][0]["topic"], "hand-back");

    let (base_url, _requests, server) = spawn_test_mcp_http_server(2, |request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/api/state") => (200, root_inventory_state()),
            ("GET", "/api/sessions/session-root/mailbox-messages/mailbox-message-8") => (
                200,
                json!({
                    "id": "mailbox-message-8",
                    "mailboxId": "mailbox-1",
                    "sequence": 8,
                    "senderSessionId": "session-peer",
                    "senderName": "Termal::Codex",
                    "targetSessionId": "session-root",
                    "targetName": "Termal::Fable",
                    "createdAt": "2026-09-03T00:00:00Z",
                    "class": "routine",
                    "body": "verified",
                    "notificationState": "deliveredToIdleSession"
                }),
            ),
            _ => (
                404,
                json!({ "error": format!("unexpected {} {}", request.method, request.path) }),
            ),
        }
    });
    let message = execute_coordination_cli(
        &CoordinationCliCommand::MailboxReadMessage {
            as_session: "session-root".to_owned(),
            message_id: "mailbox-message-8".to_owned(),
        },
        &base_url,
    )
    .expect("mailbox read-message should succeed");
    server.join().expect("test server should join");
    assert_eq!(message["id"], "mailbox-message-8");
    assert_eq!(message["body"], "verified");

    let (base_url, _requests, server) = spawn_test_mcp_http_server(2, |request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/api/state") => (200, root_inventory_state()),
            ("POST", "/api/sessions/session-root/mailboxes/mailbox-1/acknowledge") => {
                let body: Value = serde_json::from_str(&request.body)
                    .expect("acknowledge body should be JSON");
                assert_eq!(
                    body,
                    json!({ "expectedProcessedThrough": 7, "processedThrough": 8 })
                );
                (
                    200,
                    json!({
                        "id": "mailbox-1",
                        "participants": [
                            { "sessionId": "session-root", "displayName": "Termal::Fable", "processedThrough": 8 },
                            { "sessionId": "session-peer", "displayName": "Termal::Codex", "processedThrough": 8 }
                        ],
                        "latestSequence": 8,
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
    let acknowledged = execute_coordination_cli(
        &CoordinationCliCommand::MailboxAcknowledge {
            as_session: "session-root".to_owned(),
            mailbox_id: "mailbox-1".to_owned(),
            expected_processed_through: 7,
            processed_through: 8,
        },
        &base_url,
    )
    .expect("mailbox acknowledge should succeed");
    server.join().expect("test server should join");
    assert_eq!(acknowledged["id"], "mailbox-1");
    assert_eq!(acknowledged["unreadCount"], 0);
    assert_eq!(acknowledged["participants"][0]["processedThrough"], 8);
}

#[test]
fn coordination_cli_surfaces_backend_rejections_as_runtime_failures() {
    let (base_url, _requests, server) = spawn_test_mcp_http_server(2, |request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/api/state") => (200, root_inventory_state()),
            ("POST", "/api/sessions/session-root/mailboxes/mailbox-1/acknowledge") => (
                409,
                json!({ "error": "processedThrough cursor moved; expected 7 but found 9" }),
            ),
            _ => (
                404,
                json!({ "error": format!("unexpected {} {}", request.method, request.path) }),
            ),
        }
    });
    let err = execute_coordination_cli(
        &CoordinationCliCommand::MailboxAcknowledge {
            as_session: "session-root".to_owned(),
            mailbox_id: "mailbox-1".to_owned(),
            expected_processed_through: 7,
            processed_through: 8,
        },
        &base_url,
    )
    .expect_err("a CAS conflict must fail the command");
    server.join().expect("test server should join");
    assert!(err.to_string().contains("expected 7 but found 9"));
    assert_eq!(coordination_cli_exit_code(&err), 1);
}

#[test]
fn coordination_cli_renders_concise_human_output() {
    let mut rendered = Vec::new();
    render_coordination_cli_output(
        &CoordinationCliCommand::SessionsList { as_session: None },
        &json!({
            "sessions": [
                { "sessionId": "session-root", "name": "Termal::Fable", "agent": "Claude", "status": "idle", "workdir": "C:/repo", "preview": "hi" }
            ]
        }),
        &mut rendered,
    )
    .expect("sessions should render");
    let sessions = String::from_utf8(rendered).expect("rendered sessions should be UTF-8");
    assert_eq!(
        sessions,
        "session-root\tClaude\tidle\tTermal::Fable\tC:/repo\n"
    );

    let mut rendered = Vec::new();
    render_coordination_cli_output(
        &CoordinationCliCommand::MailboxList {
            as_session: "session-root".to_owned(),
        },
        &json!({
            "mailboxes": [{
                "id": "mailbox-1",
                "participants": [
                    { "sessionId": "session-root", "displayName": "Termal::Fable", "processedThrough": 7 },
                    { "sessionId": "session-peer", "displayName": "Termal::Codex", "processedThrough": 8 }
                ],
                "latestSequence": 8,
                "unreadCount": 1,
                "latestMessagePreview": "verified"
            }]
        }),
        &mut rendered,
    )
    .expect("mailboxes should render");
    let mailboxes = String::from_utf8(rendered).expect("rendered mailboxes should be UTF-8");
    assert!(mailboxes.starts_with("mailbox-1\tlatest #8\tunread 1\tprocessedThrough 7\n"));
    assert!(mailboxes.contains("  Termal::Codex (session-peer) processedThrough 8\n"));
    assert!(mailboxes.contains("  latest: verified\n"));

    let mut rendered = Vec::new();
    render_coordination_cli_output(
        &CoordinationCliCommand::MailboxAcknowledge {
            as_session: "session-root".to_owned(),
            mailbox_id: "mailbox-1".to_owned(),
            expected_processed_through: 7,
            processed_through: 8,
        },
        &json!({ "id": "mailbox-1", "participants": [], "latestSequence": 8, "unreadCount": 0 }),
        &mut rendered,
    )
    .expect("acknowledgement should render");
    assert_eq!(
        String::from_utf8(rendered).expect("rendered acknowledgement should be UTF-8"),
        "acknowledged mailbox-1 through #8 (latest #8, unread 0)\n"
    );

    let mut rendered = Vec::new();
    render_coordination_cli_output(
        &CoordinationCliCommand::MailboxRead {
            as_session: "session-root".to_owned(),
            mailbox_id: "mailbox-1".to_owned(),
            after_sequence: Some(8),
            limit: None,
        },
        &json!({ "mailboxId": "mailbox-1", "messages": [] }),
        &mut rendered,
    )
    .expect("an empty read should render");
    assert_eq!(
        String::from_utf8(rendered).expect("rendered read should be UTF-8"),
        "no messages in mailbox-1 after #8\n"
    );
}

#[test]
fn coordination_cli_never_consumes_an_option_token_as_a_value() {
    let swallowed = parse_coordination_cli_args(cli_args(&[
        "mailbox",
        "list",
        "--as-session",
        "--json",
        "--base-url",
        "http://127.0.0.1:1",
    ]))
    .expect_err("a following option must not become the session id");
    assert!(usage_message(&swallowed).contains("next argument is the option `--json`"));
    assert_eq!(coordination_cli_exit_code(&swallowed), 2);

    let duplicate_json = parse_coordination_cli_args(cli_args(&["sessions", "list", "--json", "--json"]))
        .expect_err("a repeated --json must be rejected");
    assert!(usage_message(&duplicate_json).contains("`--json` was given more than once"));

    for help in ["-h", "--help"] {
        let invocation = parse_coordination_cli_args(cli_args(&[
            "mailbox",
            "list",
            "--as-session",
            help,
            "--base-url=http://127.0.0.1:1",
        ]))
        .expect("help after a value flag must win without a usage error");
        assert_eq!(
            invocation.command,
            CoordinationCliCommand::Help,
            "{help} must never be consumed as the session id"
        );
    }

    let dashed = parse_coordination_cli_args(cli_args(&[
        "mailbox",
        "send",
        "--as-session=session-root",
        "--to=session-peer",
        "--message=--starts-with-dashes",
        "--idempotency-key=k",
    ]))
    .expect("the equals form must carry values that start with dashes");
    match dashed.command {
        CoordinationCliCommand::MailboxSend { message, .. } => assert_eq!(
            message,
            CoordinationCliMessageSource::Inline("--starts-with-dashes".to_owned())
        ),
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn coordination_cli_human_output_neutralizes_terminal_control_sequences() {
    assert_eq!(
        sanitize_coordination_cli_text("a\u{1b}b\u{7f}c\u{85}d\u{9b}e\rf\tg\nh\u{7}", true),
        "a\u{FFFD}b\u{FFFD}c\u{FFFD}d\u{FFFD}e\u{FFFD}f\tg\nh\u{FFFD}",
        "a body keeps its line structure"
    );
    assert_eq!(
        sanitize_coordination_cli_text("a\u{1b}b\u{7f}c\u{85}d\u{9b}e\rf\tg\nh\u{7}", false),
        "a\u{FFFD}b\u{FFFD}c\u{FFFD}d\u{FFFD}e\u{FFFD}f\u{FFFD}g\u{FFFD}h\u{FFFD}",
        "single-line metadata loses newlines and tabs too"
    );

    let hostile = "\u{1b}]0;pwned\u{7}\u{1b}[31mred\u{1b}[0m\r\u{9b}csi\ttab\nline";
    let message = json!({
        "id": "mailbox-message-1",
        "mailboxId": "mailbox-1",
        "sequence": 1,
        "senderSessionId": "session-peer",
        "senderName": hostile,
        "targetSessionId": "session-root",
        "targetName": "Termal::Fable",
        "createdAt": "2026-09-03T00:00:00Z",
        "class": "routine",
        "topic": hostile,
        "stateStamp": hostile,
        "body": hostile,
        "notificationState": "deliveredToIdleSession"
    });
    let mut rendered = Vec::new();
    render_coordination_cli_output(
        &CoordinationCliCommand::MailboxRead {
            as_session: "session-root".to_owned(),
            mailbox_id: "mailbox-1".to_owned(),
            after_sequence: None,
            limit: None,
        },
        &json!({ "mailboxId": "mailbox-1", "messages": [message.clone()] }),
        &mut rendered,
    )
    .expect("hostile message should render");
    let read = String::from_utf8(rendered).expect("rendered read should be UTF-8");
    for forbidden in ['\u{1b}', '\u{7}', '\r', '\u{9b}'] {
        assert!(
            !read.contains(forbidden),
            "control character {forbidden:?} leaked into human output: {read:?}"
        );
    }
    assert!(read.contains("\u{FFFD}]0;pwned\u{FFFD}"));
    // Structure: header, id, topic, stateStamp, notificationState, blank,
    // then the body block (which alone keeps its embedded newline, so two
    // lines), then a blank line. A peer name or topic carrying newlines must
    // not add lines of its own.
    let lines = read.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 9, "unexpected line structure: {lines:?}");
    assert!(lines[0].starts_with("#1 2026-09-03T00:00:00Z from "));
    assert!(lines[0].contains("csi\u{FFFD}tab\u{FFFD}line"));
    assert_eq!(lines[1], "  id: mailbox-message-1");
    assert!(lines[2].starts_with("  topic: ") && lines[2].contains("csi\u{FFFD}tab\u{FFFD}line"));
    assert!(lines[3].starts_with("  stateStamp: ") && !lines[3].contains('\t'));
    assert_eq!(lines[4], "  notificationState: deliveredToIdleSession");
    assert_eq!(lines[5], "");
    assert_eq!(
        lines[6],
        "\u{FFFD}]0;pwned\u{FFFD}\u{FFFD}[31mred\u{FFFD}[0m\u{FFFD}\u{FFFD}csi\ttab"
    );
    assert_eq!(lines[7], "line");
    assert_eq!(lines[8], "");

    let mut rendered = Vec::new();
    render_coordination_cli_output(
        &CoordinationCliCommand::MailboxReadMessage {
            as_session: "session-root".to_owned(),
            message_id: "mailbox-message-1".to_owned(),
        },
        &message,
        &mut rendered,
    )
    .expect("hostile single message should render");
    assert!(!String::from_utf8(rendered)
        .expect("rendered message should be UTF-8")
        .contains('\u{1b}'));

    let mut rendered = Vec::new();
    render_coordination_cli_output(
        &CoordinationCliCommand::MailboxList {
            as_session: "session-root".to_owned(),
        },
        &json!({
            "mailboxes": [{
                "id": "mailbox-1",
                "participants": [
                    { "sessionId": "session-peer", "displayName": hostile, "processedThrough": 1 }
                ],
                "latestSequence": 1,
                "unreadCount": 1,
                "latestMessagePreview": hostile
            }]
        }),
        &mut rendered,
    )
    .expect("hostile mailbox list should render");
    let list = String::from_utf8(rendered).expect("rendered list should be UTF-8");
    assert!(!list.contains('\u{1b}') && !list.contains('\r') && !list.contains('\u{7}'));
    let list_lines = list.lines().collect::<Vec<_>>();
    assert_eq!(
        list_lines.len(),
        3,
        "a display name or preview with newlines must not add rows: {list_lines:?}"
    );
    assert_eq!(
        list_lines[1].matches('\t').count(),
        0,
        "participant rows must not gain columns from a hostile display name"
    );
    assert!(list_lines[2].starts_with("  latest: ") && !list_lines[2].contains('\t'));

    let mut rendered = Vec::new();
    render_coordination_cli_output(
        &CoordinationCliCommand::SessionsList { as_session: None },
        &json!({ "sessions": [{ "sessionId": "session-peer", "name": hostile, "agent": "Codex", "status": "idle", "workdir": hostile }] }),
        &mut rendered,
    )
    .expect("hostile session list should render");
    let sessions = String::from_utf8(rendered).expect("rendered sessions should be UTF-8");
    assert!(!sessions.contains('\u{1b}'));
    assert_eq!(sessions.lines().count(), 1, "a hostile name must stay on its row");
    assert_eq!(
        sessions.matches('\t').count(),
        4,
        "the five-column row must keep exactly four separators: {sessions:?}"
    );
}

#[test]
fn coordination_cli_message_sources_are_bounded_by_the_mailbox_body_cap() {
    let root = TestTempRoot::create("termal-coordination-cli-message-cap");
    let path = root.path().join("big.txt");
    let exact = "x".repeat(MAX_MAILBOX_BODY_BYTES);
    fs::write(&path, &exact).expect("cap-sized message file should write");
    let text = resolve_coordination_cli_message(&CoordinationCliMessageSource::File(path.clone()))
        .expect("a body exactly at the cap must be accepted");
    assert_eq!(text.len(), MAX_MAILBOX_BODY_BYTES);

    fs::write(&path, format!("{exact}y")).expect("oversized message file should write");
    let oversized =
        resolve_coordination_cli_message(&CoordinationCliMessageSource::File(path.clone()))
            .expect_err("a body one byte over the cap must be rejected");
    assert!(usage_message(&oversized).contains("exceeds the mailbox body limit"));
    assert_eq!(coordination_cli_exit_code(&oversized), 2);

    let inline = resolve_coordination_cli_message(&CoordinationCliMessageSource::Inline(
        format!("{exact}y"),
    ))
    .expect_err("an inline body over the cap must be rejected");
    assert!(usage_message(&inline).contains("mailbox body limit"));

    fs::write(&path, [0xff, 0xfe, 0x41]).expect("non-UTF-8 message file should write");
    let invalid = resolve_coordination_cli_message(&CoordinationCliMessageSource::File(path))
        .expect_err("a non-UTF-8 body must be rejected");
    assert!(usage_message(&invalid).contains("not valid UTF-8"));
}

#[test]
fn coordination_cli_rejects_malformed_successful_responses() {
    let (base_url, _requests, server) = spawn_test_mcp_http_server(1, |request| {
        assert_eq!(request.path, "/api/state");
        (200, json!({ "sessions": [{ "name": "no id at all" }] }))
    });
    let command = CoordinationCliCommand::SessionsList { as_session: None };
    let output =
        execute_coordination_cli(&command, &base_url).expect("the bridge tolerates the shape");
    server.join().expect("test server should join");
    let err = validate_coordination_cli_output(&command, &output)
        .expect_err("a session without an id must not be reported as a listing");
    assert!(err.to_string().contains("unusable response"));
    assert!(err.to_string().contains("sessions[0] has no sessionId"));
    assert_eq!(coordination_cli_exit_code(&err), 1);

    let (base_url, _requests, server) = spawn_test_mcp_http_server(2, |request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/api/state") => (200, root_inventory_state()),
            ("GET", "/api/sessions/session-root/mailboxes") => (200, json!({ "nope": 1 })),
            _ => (404, json!({ "error": "unexpected" })),
        }
    });
    let command = CoordinationCliCommand::MailboxList {
        as_session: "session-root".to_owned(),
    };
    let output = execute_coordination_cli(&command, &base_url).expect("bridge passes raw JSON");
    server.join().expect("test server should join");
    let err = validate_coordination_cli_output(&command, &output)
        .expect_err("a non-array mailbox listing must be unusable");
    assert!(err.to_string().contains("mailboxes:"));

    let (base_url, _requests, server) = spawn_test_mcp_http_server(2, |request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/api/state") => (200, root_inventory_state()),
            ("POST", "/api/sessions/session-root/mailboxes/mailbox-1/read") => {
                (200, json!([{ "id": 1 }]))
            }
            _ => (404, json!({ "error": "unexpected" })),
        }
    });
    let command = CoordinationCliCommand::MailboxRead {
        as_session: "session-root".to_owned(),
        mailbox_id: "mailbox-1".to_owned(),
        after_sequence: None,
        limit: None,
    };
    let output = execute_coordination_cli(&command, &base_url).expect("bridge passes raw JSON");
    server.join().expect("test server should join");
    let err = validate_coordination_cli_output(&command, &output)
        .expect_err("a message without the wire fields must be unusable");
    assert!(err.to_string().contains("messages:"));

    let (base_url, _requests, server) = spawn_test_mcp_http_server(2, |request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/api/state") => (200, root_inventory_state()),
            ("GET", "/api/sessions/session-root/mailbox-messages/mailbox-message-9") => {
                (200, json!({}))
            }
            _ => (404, json!({ "error": "unexpected" })),
        }
    });
    let command = CoordinationCliCommand::MailboxReadMessage {
        as_session: "session-root".to_owned(),
        message_id: "mailbox-message-9".to_owned(),
    };
    let output = execute_coordination_cli(&command, &base_url).expect("bridge passes raw JSON");
    server.join().expect("test server should join");
    let err = validate_coordination_cli_output(&command, &output)
        .expect_err("an empty object is not a mailbox message");
    assert!(err.to_string().contains("message:"));

    let missing_scalar = validate_coordination_cli_output(
        &CoordinationCliCommand::SessionsList { as_session: None },
        &json!({ "sessions": [{ "sessionId": "session-root", "name": "x", "agent": "Codex", "status": "idle", "preview": null }] }),
    )
    .expect_err("a session entry without its workdir key is not the tool contract");
    assert!(missing_scalar
        .to_string()
        .contains("sessions[0].workdir is missing or not a string"));
    validate_coordination_cli_output(
        &CoordinationCliCommand::SessionsList { as_session: None },
        &json!({ "sessions": [{ "sessionId": "session-root", "name": null, "agent": "Codex", "status": "idle", "workdir": null, "preview": null }] }),
    )
    .expect("null attributes are the contract for absent state fields");

    let well_formed = json!({
        "mailboxId": "mailbox-1",
        "messages": [{
            "id": "mailbox-message-8",
            "mailboxId": "mailbox-1",
            "sequence": 8,
            "senderSessionId": "session-peer",
            "senderName": "Termal::Codex",
            "targetSessionId": "session-root",
            "targetName": "Termal::Fable",
            "createdAt": "2026-09-03T00:00:00Z",
            "class": "routine",
            "body": "verified",
            "notificationState": "deliveredToIdleSession"
        }]
    });
    validate_coordination_cli_output(
        &CoordinationCliCommand::MailboxRead {
            as_session: "session-root".to_owned(),
            mailbox_id: "mailbox-1".to_owned(),
            after_sequence: None,
            limit: None,
        },
        &well_formed,
    )
    .expect("a well-formed read must validate");
}

#[test]
fn coordination_cli_sanitizes_backend_diagnostics_but_keeps_usage_text() {
    let hostile = anyhow!("server said \u{1b}]0;pwned\u{7} and \u{9b}31m\nsecond line");
    let sanitized = sanitize_coordination_cli_failure(hostile);
    let rendered = format!("{sanitized:#}");
    assert!(!rendered.contains('\u{1b}') && !rendered.contains('\u{7}'));
    assert!(!rendered.contains('\u{9b}') && !rendered.contains('\n'));
    assert!(rendered.contains("server said \u{FFFD}]0;pwned\u{FFFD}"));
    assert_eq!(coordination_cli_exit_code(&sanitized), 1);

    let usage = sanitize_coordination_cli_failure(coordination_cli_usage_error("bad flag"));
    assert!(usage.downcast_ref::<CoordinationCliUsageError>().is_some());
    assert!(format!("{usage:#}").contains("termal mailbox send"));
    assert_eq!(coordination_cli_exit_code(&usage), 2);
}

#[test]
fn coordination_cli_treats_a_closed_stdout_pipe_as_success() {
    let broken = anyhow::Error::from(io::Error::from(io::ErrorKind::BrokenPipe));
    assert!(coordination_cli_error_is_broken_pipe(&broken));
    let other = anyhow::Error::from(io::Error::from(io::ErrorKind::PermissionDenied));
    assert!(!coordination_cli_error_is_broken_pipe(&other));
    assert!(!coordination_cli_error_is_broken_pipe(&anyhow!("not an io error")));
}

#[test]
fn coordination_cli_validates_class_and_trims_identifier_values() {
    let wrong_class = parse_coordination_cli_args(cli_args(&[
        "mailbox",
        "send",
        "--as-session=session-root",
        "--to=session-peer",
        "--message=hi",
        "--idempotency-key=k",
        "--class=urgent",
    ]))
    .expect_err("only the routine class exists");
    assert!(usage_message(&wrong_class).contains("`--class` must be `routine`"));

    let trimmed = parse_coordination_cli_args(cli_args(&[
        "mailbox",
        "acknowledge",
        "--as-session",
        "  session-root  ",
        "--mailbox-id= mailbox-1 ",
        "--expected= 1 ",
        "--through=2",
    ]))
    .expect("padded identifier values should parse");
    assert_eq!(
        trimmed.command,
        CoordinationCliCommand::MailboxAcknowledge {
            as_session: "session-root".to_owned(),
            mailbox_id: "mailbox-1".to_owned(),
            expected_processed_through: 1,
            processed_through: 2,
        }
    );
}

#[test]
fn coordination_cli_words_select_the_entry_point_mode() {
    let mode = Mode::parse(cli_args(&["sessions", "list", "--json"])).expect("sessions list should parse");
    assert!(matches!(
        mode,
        Mode::CoordinationCli(CoordinationCliInvocation {
            command: CoordinationCliCommand::SessionsList { as_session: None },
            json: true,
            base_url: None,
        })
    ));

    let help = Mode::parse(cli_args(&["mailbox", "--help"])).expect("mailbox help should parse");
    assert!(matches!(
        help,
        Mode::CoordinationCli(CoordinationCliInvocation {
            command: CoordinationCliCommand::Help,
            ..
        })
    ));

    let unknown = match Mode::parse(cli_args(&["mailbox", "purge"])) {
        Ok(_) => panic!("an unknown mailbox verb must not fall through to the REPL"),
        Err(err) => err,
    };
    assert_eq!(coordination_cli_exit_code(&unknown), 2);

    assert!(matches!(
        Mode::parse(Vec::new()).expect("no arguments is server mode"),
        Mode::Server
    ));
}

#[test]
fn coordination_cli_reports_a_state_snapshot_without_sessions_as_unusable() {
    let (base_url, _requests, server) = spawn_test_mcp_http_server(1, |request| {
        assert_eq!(request.path, "/api/state");
        (200, json!({ "delegations": [] }))
    });
    let err = execute_coordination_cli(
        &CoordinationCliCommand::MailboxList {
            as_session: "session-root".to_owned(),
        },
        &base_url,
    )
    .expect_err("a snapshot without sessions must not be read as an unknown caller");
    server.join().expect("test server should join");
    let rendered = err.to_string();
    assert!(rendered.contains("unusable state snapshot"), "{rendered}");
    assert!(!rendered.contains("not a session known"), "{rendered}");
}

#[test]
fn coordination_cli_exit_codes_distinguish_usage_from_runtime_failures() {
    assert_eq!(
        coordination_cli_exit_code(&coordination_cli_usage_error("bad flag")),
        2
    );
    assert_eq!(coordination_cli_exit_code(&anyhow!("server said no")), 1);
    assert_eq!(
        coordination_cli_exit_code(&anyhow!("wrapped").context("with context")),
        1
    );
}
