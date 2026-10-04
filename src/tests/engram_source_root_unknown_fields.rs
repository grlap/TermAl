// Owns unknown-field refusal witnesses through the MCP adapter and the real
// HTTP naming handler, plus valid-call controls. Uses the parent naming
// fixtures; does not replace their authority, timing or publication tests.
use super::*;

struct NamingServer {
    url: String,
    posts: Arc<std::sync::atomic::AtomicUsize>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl NamingServer {
    fn start(state: AppState) -> Self {
        let posts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = posts.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                ready_tx
                    .send(format!("http://{}", listener.local_addr().unwrap()))
                    .unwrap();
                let app = Router::new()
                    .route(
                        "/api/sessions/{session_id}/engram-source-root",
                        post(name_engram_source_root),
                    )
                    .with_state(state)
                    .layer(axum::middleware::from_fn(
                        move |request: Request<Body>, next: axum::middleware::Next| {
                            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            async move { next.run(request).await }
                        },
                    ));
                axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = stopped.await;
                    })
                    .await
                    .unwrap();
            });
        });
        Self {
            url: recv_within_guard(&ready_rx, "naming server should start").unwrap(),
            posts,
            shutdown: Some(shutdown),
            thread: Some(thread),
        }
    }

    fn bridge(&self, session: &str) -> TermalDelegationMcpBridge {
        TermalDelegationMcpBridge::new_with_timeout(
            session.to_owned(),
            self.url.clone(),
            Duration::from_secs(2),
        )
        .unwrap()
    }

    fn posts(&self) -> usize {
        self.posts.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for NamingServer {
    fn drop(&mut self) {
        let _ = self.shutdown.take().unwrap().send(());
        self.thread.take().unwrap().join().unwrap();
    }
}

fn naming_snapshot(claimed: &ClaimedRoot) -> Value {
    let inner = claimed.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&claimed.session_id).unwrap()];
    json!({
        "roots": inner.engram_work_source_roots,
        "generation": inner.engram_source_root_generation,
        "journals": inner.engram_named_root_journal,
        "history": inner.engram_work_naming_history,
        "turnRoot": format!("{:?}", record.engram.active_turn_source_root),
        "basis": record.engram.active_turn_start_basis,
        "rootBasis": record.engram.active_turn_observation_root_basis,
        "line": record.engram.pending_source_root_line,
        "namedRoot": record.engram.named_root,
    })
}

fn assert_naming_unchanged(before: &Value, after: &Value, context: &str) {
    let changed = before
        .as_object()
        .unwrap()
        .keys()
        .filter(|key| before[*key] != after[*key])
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        before == after,
        "{context}: changed fields={changed:?}; roots {} -> {}; generation {} -> {}; turnRoot {} -> {}",
        before["roots"].as_array().unwrap().len(),
        after["roots"].as_array().unwrap().len(),
        before["generation"],
        after["generation"],
        before["turnRoot"],
        after["turnRoot"]
    );
}

#[test]
fn source_root_unknown_fields_mcp_typo_cannot_clear_or_seal_the_named_root() {
    let label = "unknown-field-typo";
    let (claimed, worktree, _token) = named_root_turn(label, true);
    assert!(claimed.record(|record| record.engram.active_turn_start_basis.is_some()));
    assert!(claimed.record(|record| {
        record
            .engram
            .active_turn_source_root
            .as_ref()
            .unwrap()
            .sealed_revision
            .is_none()
    }));
    let before = naming_snapshot(&claimed);
    let reads = claimed.transport.requests().len();
    let server = NamingServer::start(claimed.state.clone());
    let bridge = server.bridge(&claimed.session_id);
    let result = bridge.tool_name_source_root(
        json!({"work": format!("w-{label}"), "root": worktree.to_string_lossy()}),
    );
    let after = naming_snapshot(&claimed);
    let posts = server.posts();
    drop(bridge);
    drop(server);
    assert_naming_unchanged(
        &before,
        &after,
        &format!("a misspelled path must not become a clear; posts={posts}; result={result:?}"),
    );
    let error = result.expect_err("the adapter refuses the unknown field");
    assert!(error.to_string().contains("root"), "{error}");
    assert_eq!(posts, 0, "refusal precedes the HTTP side effect");
    assert_eq!(
        claimed.transport.requests().len(),
        reads,
        "no authority operation"
    );
}

#[test]
fn source_root_unknown_fields_mcp_extras_cannot_mutate_even_with_a_valid_path() {
    let label = "unknown-field-extra";
    let (claimed, worktree, _token) = named_root_turn(label, true);
    let server = NamingServer::start(claimed.state.clone());
    let bridge = server.bridge(&claimed.session_id);
    for field in ["root", "bogus", "clear", "Path", "WORK"] {
        let before = naming_snapshot(&claimed);
        let reads = claimed.transport.requests().len();
        let mut arguments =
            json!({"work": format!("w-{label}"), "path": worktree.to_string_lossy()});
        arguments[field] = json!(true);
        let result = bridge.tool_name_source_root(arguments);
        assert_naming_unchanged(
            &before,
            &naming_snapshot(&claimed),
            &format!("{field}: result={result:?}"),
        );
        let error = result.expect_err("unknown fields are refused, not silently stripped");
        assert!(error.to_string().contains(field), "{field}: {error}");
        assert_eq!(server.posts(), 0, "{field}: no HTTP request");
        assert_eq!(claimed.transport.requests().len(), reads);
    }
}

#[test]
fn source_root_unknown_fields_preserve_a_pending_intent_at_both_boundaries() {
    let label = "unknown-field-pending";
    let claimed = ClaimedRoot::new_scripted(label, Vec::new());
    let worktree = add_claimed_root_worktree(&claimed.root);
    prepare_claimed_root_naming(&claimed, label);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .lose_next_reply = true;
    name_root(
        &claimed,
        label,
        Some(&worktree),
        vec![claimed_root_held(label)],
    )
    .unwrap_err();
    assert!(
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_named_root_journal[0]
            .pending
            .is_some()
    );
    let before = naming_snapshot(&claimed);
    let reads = claimed.transport.requests().len();
    let server = NamingServer::start(claimed.state.clone());
    let bridge = server.bridge(&claimed.session_id);
    let arguments = json!({"work": format!("w-{label}"), "root": worktree.to_string_lossy()});
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let response = client
        .post(format!(
            "{}/api/sessions/{}/engram-source-root",
            server.url, claimed.session_id
        ))
        .json(&arguments)
        .send()
        .unwrap();
    assert_eq!(response.status().as_u16(), 422);
    let body = response.text().unwrap();
    assert!(body.contains("root"), "{body}");
    assert_eq!(
        naming_snapshot(&claimed),
        before,
        "HTTP refuses before settling the intent"
    );
    assert_eq!(claimed.transport.requests().len(), reads);
    let result = bridge.tool_name_source_root(arguments);
    let after = naming_snapshot(&claimed);
    let posts = server.posts();
    drop(client);
    drop(bridge);
    drop(server);
    assert_naming_unchanged(
        &before,
        &after,
        &format!("MCP must preserve the pending intent; posts={posts}; result={result:?}"),
    );
    let error = result.expect_err("MCP refuses too");
    assert!(error.to_string().contains("root"), "{error}");
    assert_eq!(posts, 1, "only the direct HTTP control reached the host");
    assert_eq!(claimed.transport.requests().len(), reads);
}

#[test]
fn source_root_unknown_fields_valid_mcp_name_and_omission_clear_still_publish() {
    let label = "unknown-field-valid";
    let (claimed, worktree, _token) = named_root_turn(label, true);
    let admitted = claimed.record(|record| record.engram.active_turn_source_root.clone().unwrap());
    let server = NamingServer::start(claimed.state.clone());
    let bridge = server.bridge(&claimed.session_id);
    let named = bridge
        .tool_name_source_root(
            json!({"work": format!("w-{label}"), "path": worktree.to_string_lossy()}),
        )
        .unwrap();
    assert!(named["root"].is_string());
    assert_eq!(
        named["generation"].as_u64().unwrap(),
        admitted.generation,
        "repeating the same name is idempotent"
    );
    assert!(named["notice"].as_str().unwrap().contains("next turn"));
    claimed.record(|record| {
        let current = record.engram.active_turn_source_root.as_ref().unwrap();
        assert_eq!(current.root, admitted.root);
        assert_eq!(
            current.generation, admitted.generation,
            "the running turn does not move"
        );
        assert!(
            current.sealed_revision.is_none(),
            "the same name does not seal the running turn"
        );
    });
    assert_eq!(
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_work_source_roots
            .len(),
        1
    );
    let cleared = bridge
        .tool_name_source_root(json!({"work": format!("w-{label}")}))
        .unwrap();
    assert_eq!(cleared["generation"], 0);
    assert!(cleared.get("root").is_none());
    let revision = content_revision_of(&worktree);
    assert_eq!(cleared["sealed"]["sourceRevision"], revision);
    claimed.record(|record| {
        let current = record.engram.active_turn_source_root.as_ref().unwrap();
        assert_eq!(current.root, admitted.root);
        assert_eq!(current.generation, admitted.generation);
        assert_eq!(
            current.sealed_revision.as_deref(),
            Some(revision.as_str()),
            "the documented clear seals the old turn revision"
        );
    });
    assert!(
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_work_source_roots
            .is_empty()
    );
    assert!(
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .engram_named_root_journal
            .iter()
            .all(|journal| journal.pending.is_none())
    );
    // An adapter that refuses all calls must fail this genuine new naming,
    // not pass merely because the original name was already present.
    let renamed = bridge
        .tool_name_source_root(
            json!({"work": format!("w-{label}"), "path": worktree.to_string_lossy()}),
        )
        .unwrap();
    let generation = renamed["generation"].as_u64().unwrap();
    assert!(generation > admitted.generation);
    assert!(renamed["root"].is_string());
    {
        let inner = claimed.state.inner.lock().unwrap();
        assert_eq!(inner.engram_work_source_roots.len(), 1);
        assert_eq!(inner.engram_work_source_roots[0].generation, generation);
        assert!(
            inner
                .engram_named_root_journal
                .iter()
                .all(|journal| journal.pending.is_none())
        );
    }
    claimed.record(|record| {
        let current = record.engram.active_turn_source_root.as_ref().unwrap();
        assert_eq!(
            current.generation, admitted.generation,
            "re-naming takes effect next turn"
        );
        assert_eq!(current.sealed_revision.as_deref(), Some(revision.as_str()));
    });
    assert_eq!(server.posts(), 3);
}

#[test]
fn source_root_unknown_fields_schema_and_description_advertise_the_refusal() {
    let tool = source_root_tool_definition();
    assert_eq!(tool["inputSchema"]["additionalProperties"], false);
    assert_eq!(tool["inputSchema"]["required"], json!(["work"]));
    assert_eq!(
        tool["inputSchema"]["properties"],
        json!({"work": {"type": "string"}, "path": {"type": "string"}})
    );
    let description = tool["description"].as_str().unwrap();
    assert!(description.contains("Unknown fields are refused"));
    assert!(description.contains("Omit `path` to clear the name."));
}
