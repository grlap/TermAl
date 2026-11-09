//! The bridge must carry a command's caller-supplied brief into the child request.
use super::*;

fn spawn_preserves_brief(agent: &str) {
    let brief = "BASE aabb COMMIT ccdd TREE eeff manifest C:/fixture/input.json; inspect `git diff --no-index -- before after`";
    for (kind, template, arguments, note) in [
        (AgentCommandKind::PromptTemplate, "Inspect the supplied input.", brief, None),
        (AgentCommandKind::PromptTemplate, "Inspect the supplied input.\n", brief, Some("Focus on privacy.")),
        (AgentCommandKind::PromptTemplate, "Inspect the supplied input.\n\n", brief, None),
        (AgentCommandKind::PromptTemplate, "Inspect $ARGUMENTS.", brief, None),
        (AgentCommandKind::PromptTemplate, "Inspect the supplied input.", "", Some("Focus on privacy.")),
        (AgentCommandKind::NativeSlash, "/review-code", brief, None),
    ] {
        let expected_agent = agent.to_owned();
        let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
            let body: Value = serde_json::from_str(&request.body).unwrap();
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/api/sessions/session-parent/agent-commands/review-code/resolve") => {
                    if arguments.is_empty() {
                        assert!(body.get("arguments").is_none());
                    } else {
                        assert_eq!(body["arguments"], arguments);
                    }
                    assert_eq!(body["intent"], "delegate");
                    let resolved = resolve_agent_command_payload(
                        AgentCommand {
                            kind,
                            name: "review-code".to_owned(),
                            description: "Inspect supplied input".to_owned(),
                            content: template.to_owned(),
                            source: "fixture/review-code.md".to_owned(),
                            argument_hint: None,
                            resolver_frontmatter: None,
                            resolver_frontmatter_trusted: false,
                        },
                        serde_json::from_value(body).unwrap(),
                        None,
                    )
                    .unwrap();
                    (200, serde_json::to_value(resolved).unwrap())
                }
                ("POST", "/api/sessions/session-parent/delegations") => {
                    assert_eq!(body["agent"], expected_agent);
                    assert_eq!(body["mode"], "reviewer");
                    assert_eq!(body["writePolicy"]["kind"], "readOnly");
                    let prompt = body["prompt"].as_str().unwrap();
                    let prefix = if kind == AgentCommandKind::PromptTemplate {
                        "Inspect "
                    } else {
                        "/review-code "
                    };
                    assert!(prompt.starts_with(prefix));
                    if !arguments.is_empty() {
                        assert_eq!(prompt.matches(arguments).count(), 1, "{prompt}");
                    }
                    assert!(!prompt.contains("$ARGUMENTS"));
                    if let Some(note) = note {
                        assert!(prompt.ends_with(note));
                        if !arguments.is_empty() {
                            assert!(prompt.find(arguments).unwrap() < prompt.find(note).unwrap());
                        }
                    }
                    (200, json!({
                        "delegation": {"id": "delegation-brief", "status": "running"},
                        "childSessionId": "session-child"
                    }))
                }
                other => panic!("unexpected request: {other:?}"),
            }
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url).unwrap();
        let prompt = match note {
            Some(note) => format!("/review-code {arguments} -- {note}"),
            None => format!("/review-code {arguments}"),
        };
        let response = bridge.tool_spawn_session(json!({
            "agent": agent, "mode": "reviewer", "writePolicy": {"kind": "readOnly"},
            "prompt": prompt,
        })).unwrap();
        assert_eq!(response["childSessionId"], "session-child");
        server.join().unwrap();
        assert_eq!(requests.lock().unwrap().len(), 2);
    }
}

#[test]
fn codex_spawn_preserves_a_single_line_template_brief() {
    spawn_preserves_brief("Codex");
}

#[test]
fn claude_spawn_preserves_a_single_line_template_brief() {
    spawn_preserves_brief("Claude");
}
