// Public API/MCP discovery and denial tests; no future implementation types
// are referenced, so missing routes/tools are assertion failures at RED.
use super::*;

#[test]
fn review_identity_mcp_has_a_dedicated_strict_read_capability() {
    let tools = mcp_tools_list_result();
    let tool = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "termal_review_identity")
        .expect("host-owned review identity operation must be discoverable");
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    assert_eq!(tool["annotations"]["destructiveHint"], false);
    let schema = &tool["inputSchema"];
    assert_eq!(schema["additionalProperties"], false);
    let properties = schema["properties"].as_object().unwrap();
    for name in [
        "operation",
        "schemaVersion",
        "expectedFingerprint",
        "platformProfile",
        "pageLimit",
        "cursor",
    ] {
        assert!(
            properties.contains_key(name),
            "missing explicit argument {name}"
        );
    }
    for forbidden in [
        "command",
        "executable",
        "env",
        "script",
        "manifestPath",
        "callback",
    ] {
        assert!(!properties.contains_key(forbidden));
    }
}

#[tokio::test]
async fn review_identity_http_rejects_unknown_fields_at_the_real_route() {
    let state = test_app_state();
    let parent = test_session_id(&state, Agent::Codex);
    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/sessions/{parent}/review-identity"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "operation":"capture", "schemaVersion":1,
                "command":"must never execute"
            })
            .to_string(),
        ))
        .unwrap();
    let response = app_router(state).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn review_identity_http_never_offers_capture_to_a_read_only_child() {
    let state = test_app_state();
    let parent = test_session_id(&state, Agent::Codex);
    let (_, child) = super::delegation_support::install_required_review_delegation(&state, &parent);
    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/sessions/{child}/review-identity"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"operation":"capture","schemaVersion":1}).to_string(),
        ))
        .unwrap();
    let response = app_router(state).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn review_identity_http_rejects_a_stopped_or_nonreviewer_check() {
    let state = test_app_state();
    let parent = test_session_id(&state, Agent::Claude);
    let (_, child) = super::delegation_support::install_required_review_delegation(&state, &parent);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner
            .find_delegation_index_by_child_session_id(&child)
            .unwrap();
        inner.delegations[index].mode = DelegationMode::Evaluator;
    }
    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/sessions/{child}/review-identity"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "operation":"check","schemaVersion":1,
                "expectedFingerprint":"0".repeat(64),
                "platformProfile":if cfg!(windows) {"windows"} else {"unix"}
            })
            .to_string(),
        ))
        .unwrap();
    let response = app_router(state).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

fn capture_request() -> ReviewIdentityRequest {
    serde_json::from_value(json!({"operation":"capture","schemaVersion":1})).unwrap()
}
fn local_root(prefix: &str) -> TestTempRoot {
    let _placement = TestTempRootDirectoryScope::repository_local();
    TestTempRoot::create(prefix)
}
fn check_request() -> ReviewIdentityRequest {
    serde_json::from_value(json!({"operation":"check","schemaVersion":1,
        "expectedFingerprint":"a".repeat(64),"platformProfile":identity_platform()}))
    .unwrap()
}
fn worker_output(value: Value) -> std::process::Output {
    #[cfg(windows)]
    let status = {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(if value["complete"] == true { 0 } else { 1 })
    };
    #[cfg(unix)]
    let status = {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(if value["complete"] == true { 0 } else { 256 })
    };
    std::process::Output {
        status,
        stdout: format!("{value}\n").into_bytes(),
        stderr: b"separate native diagnostic".to_vec(),
    }
}
fn admitted_child(state: &AppState, root: &FsPath) -> String {
    let parent = test_session_id(state, Agent::Codex);
    let (delegation, child) =
        super::delegation_support::install_required_review_delegation(state, &parent);
    let root = identity_utf8_path(&fs::canonicalize(root).unwrap()).unwrap();
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&child).unwrap();
    inner.sessions[index].session.workdir = root.clone();
    inner
        .delegations
        .iter_mut()
        .find(|d| d.id == delegation)
        .unwrap()
        .cwd = root;
    child
}

#[test]
fn review_identity_request_values_and_unknown_fields_fail_closed() {
    for request in [
        json!({"operation":"check","schemaVersion":1,"expectedFingerprint":"a".repeat(64),"platformProfile":"windows","command":"x"}),
        json!({"operation":"capture","schemaVersion":1,"callback":"x"}),
    ] {
        assert!(serde_json::from_value::<ReviewIdentityRequest>(request).is_err());
    }
    for request in [
        json!({"operation":"capture","schemaVersion":2}),
        json!({"operation":"capture","schemaVersion":1,"pageLimit":101}),
        json!({"operation":"check","schemaVersion":1,"expectedFingerprint":"A".repeat(64),"platformProfile":"windows"}),
        json!({"operation":"check","schemaVersion":1,"expectedFingerprint":"a".repeat(64),"platformProfile":"windows","root":"elsewhere"}),
    ] {
        assert!(
            serde_json::from_value::<ReviewIdentityRequest>(request)
                .unwrap()
                .validate()
                .is_err()
        );
    }
}

#[test]
fn review_identity_framing_matches_independent_golden_bytes() {
    // Literal golden wire bytes: BE lengths then tag/value. No production
    // field encoder is used to construct the oracle.
    let bytes = b"\0\0\0\0\0\0\0\x06\0\0\0\0\0\0\0\x19domaintermal-review-identity-v1\
\0\0\0\0\0\0\0\x06\0\0\0\0\0\0\0\x01schema1\
\0\0\0\0\0\0\0\x04\0\0\0\0\0\0\0\x05root/repo\
\0\0\0\0\0\0\0\x07\0\0\0\0\0\0\0\x03contentabc\
\0\0\0\0\0\0\0\x08\0\0\0\0\0\0\0\x04platformunix";
    assert_eq!(
        identity_composite("/repo", "abc", "unix"),
        format!("{:x}", Sha256::digest(bytes))
    );
    assert_ne!(
        identity_composite("/repo", "abc", "unix"),
        identity_composite("/repo", "abc", "windows")
    );
}

#[test]
fn review_identity_nonempty_entry_has_an_independent_field_order_oracle() {
    let mut entry = ReviewIdentityEntry::new("src/a".to_owned(), "index", "blob");
    entry.stage = Some(0);
    entry.mode = Some("100644".to_owned());
    entry.oid = Some("1".repeat(40));
    entry.flags = Some("00000000".to_owned());
    entry.size = Some(3);
    let oracle = [
        ("domain", "termal-review-entry-v1"),
        ("path", "src/a"),
        ("layer", "index"),
        ("kind", "blob"),
        ("stage:present", "0"),
        ("mode:present", "100644"),
        ("oid:present", "1111111111111111111111111111111111111111"),
        ("flags:present", "00000000"),
        ("objectFormat:absent", ""),
        ("size:present", "3"),
        ("content:absent", ""),
        ("fsMode:absent", ""),
        ("target:absent", ""),
    ];
    let mut bytes = Vec::new();
    for (tag, payload) in oracle {
        bytes.extend_from_slice(&(tag.len() as u64).to_be_bytes());
        bytes.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        bytes.extend_from_slice(tag.as_bytes());
        bytes.extend_from_slice(payload.as_bytes());
    }
    assert_eq!(entry.digest(), format!("{:x}", Sha256::digest(bytes)));
    let mut capture = ReviewIdentityCapture::new();
    capture.add(entry).unwrap();
    let leaf = capture.entries[0].digest();
    let mut independent = Sha256::new();
    for (tag, payload) in [
        ("domain", "termal-review-inventory-v1"),
        ("entry", leaf.as_str()),
    ] {
        independent.update((tag.len() as u64).to_be_bytes());
        independent.update((payload.len() as u64).to_be_bytes());
        independent.update(tag.as_bytes());
        independent.update(payload.as_bytes());
    }
    assert_eq!(capture.finish(), format!("{:x}", independent.finalize()));
}

#[test]
fn review_identity_limits_are_refusals_not_truncated_success() {
    let mut capture = ReviewIdentityCapture::new();
    assert_eq!(
        identity_error_code(
            &capture
                .consume(REVIEW_IDENTITY_PAYLOAD_LIMIT + 1)
                .unwrap_err()
        ),
        "input_limit_exceeded"
    );
    let mut capture = ReviewIdentityCapture::new();
    capture.bytes = REVIEW_IDENTITY_AGGREGATE_LIMIT;
    assert!(capture.consume(1).is_err());
    let mut capture = ReviewIdentityCapture::new();
    capture.encoded = REVIEW_IDENTITY_INVENTORY_LIMIT;
    assert!(capture.add(ReviewIdentityEntry::default()).is_err());
    let mut capture = ReviewIdentityCapture::new();
    capture
        .entries
        .resize(REVIEW_IDENTITY_RECORD_LIMIT, ReviewIdentityEntry::default());
    assert!(capture.add(ReviewIdentityEntry::default()).is_err());
    let root = local_root("review-identity-visited-limit");
    init_git_document_test_repo(&root);
    let git = ReviewIdentityGit::new(&root).unwrap();
    let mut capture = ReviewIdentityCapture::new();
    capture.visited_entries = REVIEW_IDENTITY_RECORD_LIMIT;
    assert_eq!(
        identity_error_code(
            &identity_discover_untracked(
                &git,
                "",
                &HashMap::new(),
                &mut BTreeSet::new(),
                &mut BTreeSet::new(),
                &mut capture
            )
            .unwrap_err()
        ),
        "input_limit_exceeded"
    );
}

#[test]
fn review_identity_mutation_between_captures_is_a_named_failure() {
    let root = local_root("review-identity-race");
    init_git_document_test_repo(&root);
    fs::write(root.join("raw"), b"before").unwrap();
    IDENTITY_CAPTURE_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(|root| {
            fs::write(root.join("raw"), b"after").unwrap();
        }))
    });
    let error = identity_result(&root, &capture_request()).unwrap_err();
    assert_eq!(identity_error_code(&error), "concurrent_change");
}

#[test]
fn review_identity_deadline_is_one_shared_budget() {
    let root = local_root("review-identity-deadline");
    init_git_document_test_repo(&root);
    let mut git = ReviewIdentityGit::new(&root).unwrap();
    git.runner.deadline = std::time::Instant::now();
    assert_eq!(
        identity_error_code(
            &identity_repository(&git, "", 0, &mut ReviewIdentityCapture::new()).unwrap_err()
        ),
        "deadline_exceeded"
    );
}

#[test]
fn review_identity_qualified_claude_capability_never_grants_another_server() {
    assert_eq!(
        delegation_control_plane_capability_for_claude_tool_name(
            TERMAL_REVIEW_IDENTITY_QUALIFIED_TOOL_NAME
        ),
        Some(DelegationControlPlaneCapability::ReviewIdentity)
    );
    for name in [
        "termal_review_identity",
        "mcp__other__termal_review_identity",
    ] {
        assert_eq!(
            delegation_control_plane_capability_for_claude_tool_name(name),
            None
        );
    }
    for agent in [Agent::Claude, Agent::Codex, Agent::Kimi] {
        let state = test_app_state();
        let parent = test_session_id(&state, agent);
        let (delegation, child) =
            super::delegation_support::install_required_review_delegation(&state, &parent);
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&child).unwrap();
            inner.sessions[index].session.agent = agent;
            inner
                .delegations
                .iter_mut()
                .find(|d| d.id == delegation)
                .unwrap()
                .agent = agent;
        }
        assert!(state.delegation_control_plane_capability_allowed(
            &child,
            DelegationControlPlaneCapability::ReviewIdentity
        ));
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        inner.sessions[index].hidden = true;
        assert!(!delegation_control_plane_capability_allowed_locked(
            &inner,
            &child,
            DelegationControlPlaneCapability::ReviewIdentity
        ));
    }
}

fn kimi_identity_decision(title: &str, args: Value, allowed: bool) -> KimiReadOnlyDecision {
    let observation = KimiToolCallObservation {
        title: Some(title.to_owned()),
        streamed_args: Some(args.to_string()),
        oversized: false,
        approved_args: None,
    };
    let params = json!({"toolCall":{"title":title,"content":[{
        "type":"content","content":{"type":"text","text":format!("{KIMI_MCP_SUMMARY_PREFIX}{title}")}
    }]}});
    kimi_read_only_permission_decision(&params, Some(&observation), "/work/repo", &|capability| {
        assert_eq!(capability, DelegationControlPlaneCapability::ReviewIdentity);
        allowed
    })
}

#[test]
fn review_identity_kimi_qualified_check_requires_current_capability() {
    let args = serde_json::to_value(check_request()).unwrap();
    assert_eq!(
        kimi_identity_decision(
            TERMAL_REVIEW_IDENTITY_QUALIFIED_TOOL_NAME,
            args.clone(),
            true
        ),
        KimiReadOnlyDecision::Allow { args: args.clone() }
    );
    assert!(matches!(
        kimi_identity_decision(TERMAL_REVIEW_IDENTITY_QUALIFIED_TOOL_NAME, args, false),
        KimiReadOnlyDecision::Reject { .. }
    ));
}

#[test]
fn review_identity_kimi_refuses_capture_root_aliases_and_invalid_check_shapes() {
    let valid = serde_json::to_value(check_request()).unwrap();
    for title in [
        "termal_review_identity",
        "mcp__other__termal_review_identity",
    ] {
        assert!(matches!(
            kimi_identity_decision(title, valid.clone(), true),
            KimiReadOnlyDecision::Reject { .. }
        ));
    }
    let mut with_root = valid.clone();
    with_root["root"] = json!("/work/repo");
    let mut unknown = valid.clone();
    unknown["command"] = json!("must never execute");
    let mut malformed = valid;
    malformed["expectedFingerprint"] = json!("invalid");
    for args in [
        json!({"operation":"capture","schemaVersion":1}),
        json!({"operation":"check","schemaVersion":1}),
        with_root,
        unknown,
        malformed,
    ] {
        assert!(matches!(
            kimi_identity_decision(TERMAL_REVIEW_IDENTITY_QUALIFIED_TOOL_NAME, args, true),
            KimiReadOnlyDecision::Reject { .. }
        ));
    }
}

#[test]
fn review_identity_worker_transport_and_observed_mismatch_are_different() {
    let root = local_root("review-identity-worker");
    let state = test_app_state();
    let child = admitted_child(&state, &root);
    let result = state
        .observe_review_identity_with(&child, check_request(), |command| {
            assert!(command.get_args().any(|arg| arg == "review-identity"));
            Ok(worker_output(identity_failure("fingerprint_mismatch")))
        })
        .unwrap();
    assert_eq!(result["verified"], false);
    assert!(result["observer"]["stderrLength"].as_u64().unwrap() > 0);
    assert_eq!(
        delegation_mcp_tool_result(TERMAL_REVIEW_IDENTITY_TOOL_NAME, &result)["isError"],
        true
    );
    let error = state
        .observe_review_identity_with(&child, check_request(), |_| {
            Err(anyhow!("fixture transport unavailable"))
        })
        .unwrap_err();
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn review_identity_worker_success_requires_the_expected_root_and_fingerprint() {
    let root = local_root("review-identity-worker-success");
    let state = test_app_state();
    let child = admitted_child(&state, &root);
    let root = fs::canonicalize(&root).unwrap();
    let value = json!({"schemaVersion":1,"algorithm":REVIEW_IDENTITY_ALGORITHM,"complete":true,"verified":true,
        "canonicalRoot":root,"requestedRoot":root,"platformProfile":identity_platform(),"objectFormat":"sha1","fingerprint":"a".repeat(64)});
    let result = state
        .observe_review_identity_with(&child, check_request(), |_| {
            Ok(worker_output(value.clone()))
        })
        .unwrap();
    assert_eq!(result["verified"], true);
    let mut wrong = value;
    wrong["fingerprint"] = json!("b".repeat(64));
    assert_eq!(
        state
            .observe_review_identity_with(&child, check_request(), |_| Ok(worker_output(wrong)))
            .unwrap_err()
            .status,
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[test]
fn review_identity_attempt_change_during_observation_cannot_publish() {
    let root = local_root("review-identity-authority-race");
    let state = test_app_state();
    let child = admitted_child(&state, &root);
    let error = state
        .observe_review_identity_with(&child, check_request(), |_| {
            let mut inner = state.inner.lock().unwrap();
            let index = inner
                .find_delegation_index_by_child_session_id(&child)
                .unwrap();
            inner.delegations[index].review_result_submission_attempt += 1;
            Ok(worker_output(identity_failure("fingerprint_mismatch")))
        })
        .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
}

#[test]
fn review_identity_codex_capability_requires_authenticated_server_and_check_shape() {
    let state = test_app_state();
    let parent = test_session_id(&state, Agent::Codex);
    let (_, child) = super::delegation_support::install_required_review_delegation(&state, &parent);
    let request = json!({"id":"identity-approval","params":{
        "threadId":"thread-reviewer","turnId":"turn-reviewer","serverName":TERMAL_DELEGATION_MCP_SERVER_NAME,
        "mode":"form","message":"Approval copy","requestedSchema":{"type":"object","properties":{}},
        "_meta":{"codex_approval_kind":"mcp_tool_call","tool_description":TERMAL_REVIEW_IDENTITY_TOOL_DESCRIPTION,
            "tool_params":{"operation":"check","schemaVersion":1,"expectedFingerprint":"a".repeat(64),"platformProfile":identity_platform()}}}});
    let (tx, rx) = mpsc::channel();
    assert!(
        try_auto_respond_delegation_control_plane_request(
            "mcpServer/elicitation/request",
            &request,
            &state,
            &child,
            &tx
        )
        .unwrap()
    );
    assert!(rx.try_recv().is_ok());
    for (pointer, bad) in [
        ("/params/serverName", json!("other")),
        ("/params/_meta/tool_description", json!("wrong")),
        (
            "/params/_meta/tool_params",
            json!({"operation":"capture","schemaVersion":1}),
        ),
        (
            "/params/_meta/tool_params",
            json!({"operation":"check","schemaVersion":1,"expectedFingerprint":"a".repeat(64),"platformProfile":identity_platform(),"command":"evil"}),
        ),
    ] {
        let mut invalid = request.clone();
        *invalid.pointer_mut(pointer).unwrap() = bad;
        assert!(
            !try_auto_respond_delegation_control_plane_request(
                "mcpServer/elicitation/request",
                &invalid,
                &state,
                &child,
                &tx
            )
            .unwrap()
        );
        assert!(rx.try_recv().is_err());
    }
}

#[test]
fn review_identity_unauthorized_children_never_start_the_worker() {
    for reason in ["hidden", "remote", "cwd", "write", "stopped"] {
        let root = local_root("review-identity-denied");
        let state = test_app_state();
        let child = admitted_child(&state, &root);
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&child).unwrap();
            match reason {
                "hidden" => inner.sessions[index].hidden = true,
                "remote" => {
                    inner.sessions[index].remote_id = Some("remote".to_owned());
                    inner.sessions[index].remote_session_id = Some("child".to_owned());
                }
                "cwd" => inner.sessions[index].session.workdir.push_str("/different"),
                "write" => {
                    let i = inner
                        .find_delegation_index_by_child_session_id(&child)
                        .unwrap();
                    inner.delegations[i].write_policy = DelegationWritePolicy::SharedWorktree {
                        owned_paths: vec![],
                    };
                }
                "stopped" => {
                    let i = inner
                        .find_delegation_index_by_child_session_id(&child)
                        .unwrap();
                    inner.delegations[i].status = DelegationStatus::Completed;
                }
                _ => unreachable!(),
            }
        }
        assert!(
            state
                .observe_review_identity_with(&child, check_request(), |_| panic!(
                    "denied worker started: {reason}"
                ))
                .is_err()
        );
    }
}

#[test]
fn review_identity_old_backend_metadata_does_not_offer_the_new_tool() {
    // The bridge's lifetime capability is false unless the new backend's
    // explicit reviewIdentityAllowed property was present and true.
    for state in [
        json!({}),
        json!({"delegations":[{"childSessionId":"child","reviewFreezeAllowed":true}]}),
        json!({"delegations":[{"childSessionId":"other","reviewIdentityAllowed":true}]}),
        json!({"delegations":[{"childSessionId":"child","reviewIdentityAllowed":"true"}]}),
    ] {
        assert!(!review_identity_snapshot_allows(&state, "child"));
    }
    assert!(review_identity_snapshot_allows(
        &json!({"delegations":[{"childSessionId":"child","reviewIdentityAllowed":true}]}),
        "child"
    ));
}

#[test]
fn review_identity_capture_scope_admits_only_an_exact_project_registration() {
    let root = local_root("review-identity-scope");
    init_git_document_test_repo(&root);
    let path = identity_utf8_path(&fs::canonicalize(&root).unwrap()).unwrap();
    let scope = ReviewIdentityScope {
        cwd: path.clone(),
        project: path,
        named: vec![],
    };
    assert_eq!(
        scope.admit(&root).unwrap(),
        fs::canonicalize(root.join(".git")).unwrap()
    );
    let other = local_root("review-identity-other-scope");
    init_git_document_test_repo(&other);
    assert_eq!(
        identity_error_code(&scope.admit(&other).unwrap_err()),
        "root_not_admitted"
    );
}

#[test]
fn review_identity_capture_passes_scope_as_data_without_server_filesystem_reads() {
    let root = local_root("review-identity-capture-scope");
    let state = test_app_state();
    let parent = test_session_id(&state, Agent::Codex);
    let path = identity_utf8_path(&fs::canonicalize(&root).unwrap()).unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&parent).unwrap();
        inner.sessions[index].session.workdir = path.clone();
        inner.sessions[index].session.project_id = None;
    }
    let result = state
        .observe_review_identity_with(&parent, capture_request(), |command| {
            let args: Vec<_> = command.get_args().collect();
            assert_eq!(args.len(), 4);
            assert_eq!(args[0], "review-identity");
            assert_eq!(args[1], std::ffi::OsStr::new(&path));
            let scope: ReviewIdentityScope =
                serde_json::from_str(args[3].to_str().unwrap()).unwrap();
            assert_eq!(scope.cwd, path);
            assert_eq!(scope.project, path);
            Ok(worker_output(identity_failure("root_not_admitted")))
        })
        .unwrap_err();
    assert_eq!(result.status, StatusCode::CONFLICT);
}

#[test]
fn review_identity_index_debug_parser_keeps_flags_and_discards_stat_cache_fields() {
    let bytes=b"100644 1111111111111111111111111111111111111111 0\tfile\0  ctime: 1:2\n  mtime: 3:4\n  dev: 5\tino: 6\n  uid: 7\tgid: 8\n  size: 3\tflags: 8000\n";
    let records = identity_index_records(bytes).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].0,
        b"100644 1111111111111111111111111111111111111111 0\tfile"
    );
    assert_eq!(records[0].1, 0x8000);
    let mut later = bytes.to_vec();
    later[bytes.iter().position(|b| *b == b'3').unwrap()] = b'9';
    assert_eq!(identity_index_records(&later).unwrap()[0].1, 0x8000);
    assert!(identity_index_records(b"100644 truncated").is_err());
    assert!(identity_index_records(b"100644 x 0\tfile\0missing debug flag record").is_err());
}

#[cfg(windows)]
#[test]
fn review_identity_git_argv_preserves_native_identity_without_verbatim_aliases() {
    let root = local_root("review-identity-git-argv");
    let canonical = fs::canonicalize(&root).unwrap();
    let argument = identity_git_path(&canonical).unwrap();
    assert!(!argument.starts_with(r"\\?\"));
    assert_eq!(fs::canonicalize(&argument).unwrap(), canonical);
    for path in [
        r"\\?\C:\work\literal.",
        r"\\?\C:\work\literal ",
        r"\\?\C:\work\CON",
    ] {
        assert_eq!(
            identity_error_code(&identity_git_path(FsPath::new(path)).unwrap_err()),
            "path_encoding_unsupported"
        );
    }
}

#[cfg(windows)]
#[test]
fn review_identity_junction_candidates_and_ancestors_refuse_before_child_probes() {
    let fixture = local_root("review-identity-junction-boundary");
    let root = fixture.join("repo");
    let outside = fixture.join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(
        outside.join(".git"),
        b"must not be probed through the junction",
    )
    .unwrap();
    let junction = root.join("junction");
    let created = Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&junction)
        .arg(&outside)
        .creation_flags(0x08000000)
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let outcomes: Vec<_> = [
        ("junction", "link_kind_unsupported"),
        ("junction/absent/.gitignore", "ancestor_link"),
    ]
    .into_iter()
    .map(|(path, expected)| {
        (
            identity_safe_path(&root, path).map_err(|error| identity_error_code(&error)),
            expected,
        )
    })
    .collect();
    fs::remove_dir(&junction).unwrap();
    for (outcome, expected) in outcomes {
        assert_eq!(outcome.unwrap_err(), expected);
    }
    assert_eq!(
        fs::read(outside.join(".git")).unwrap(),
        b"must not be probed through the junction"
    );
}

#[test]
fn review_identity_ignore_protocol_distinguishes_ignored_negated_and_unmatched_paths() {
    let requests = vec![b"ignored/".to_vec(), b"keep".to_vec(), b"plain".to_vec()];
    let output = concat!(
        ".gitignore",
        "\0",
        "1",
        "\0",
        "ignored/",
        "\0",
        "ignored/",
        "\0",
        ".gitignore",
        "\0",
        "2",
        "\0",
        "!keep",
        "\0",
        "keep",
        "\0",
        "\0",
        "\0",
        "\0",
        "plain",
        "\0"
    )
    .as_bytes();
    assert_eq!(
        identity_ignore_matches(&requests, output).unwrap(),
        HashSet::from([b"ignored/".to_vec()])
    );
    assert!(identity_ignore_matches(&requests, &output[..output.len() - 1]).is_err());
    let duplicate = output.windows(5).position(|part| part == b"plain").unwrap();
    let mut invalid = output.to_vec();
    invalid.splice(duplicate..duplicate + 5, b"keep".iter().copied());
    assert!(identity_ignore_matches(&requests, &invalid).is_err());
}

#[test]
fn review_identity_ignore_queries_keep_root_names_literal() {
    let root = local_root("review-identity-literal-ignore");
    init_git_document_test_repo(&root);
    fs::write(root.join(".gitignore"), b"foo\n:ignored\n").unwrap();
    let git = ReviewIdentityGit::new(&root).unwrap();
    // Negative control: the old raw query aliases :foo to foo, even on
    // Windows where the filesystem cannot create a colon-named file.
    let raw = vec![b":foo".to_vec()];
    let raw_reply = git
        .run(
            &[
                "check-ignore",
                "-z",
                "--stdin",
                "--non-matching",
                "-v",
                "--no-index",
            ],
            true,
            Some(b":foo\0".to_vec()),
        )
        .unwrap();
    assert!(
        identity_ignore_matches(&raw, &raw_reply)
            .unwrap()
            .contains(b":foo".as_slice())
    );
    let paths: Vec<_> = [
        "foo",
        ":foo",
        "::foo",
        ":(top)foo",
        ":!x",
        ":^x",
        ":ignored",
        "plain",
    ]
    .into_iter()
    .map(|path| path.as_bytes().to_vec())
    .collect();
    assert_eq!(
        identity_ignored_candidates(&git, &paths).unwrap(),
        HashSet::from([b"foo".to_vec(), b":ignored".to_vec()])
    );
    // Include the query prefix and NUL in the existing input ceiling.
    assert_eq!(
        identity_error_code(
            &identity_ignored_candidates(&git, &[vec![b'x'; REVIEW_IDENTITY_INVENTORY_LIMIT - 2]])
                .unwrap_err()
        ),
        "input_limit_exceeded"
    );
}

#[test]
fn review_identity_discovery_refuses_a_replaced_directory_before_enumeration() {
    let fixture = local_root("review-identity-directory-race");
    init_git_document_test_repo(&fixture);
    let git = ReviewIdentityGit::new(&fixture).unwrap();
    let directory = git.root.join("tree");
    fs::create_dir(&directory).unwrap();
    let before = ReviewIdentityDirectory {
        relative: "tree".to_owned(),
        metadata: fs::symlink_metadata(&directory).unwrap(),
        key: identity_physical_key(&directory).unwrap(),
    };
    fs::rename(&directory, git.root.join("old-tree")).unwrap();
    fs::create_dir(&directory).unwrap();
    assert_eq!(
        identity_error_code(&identity_discovery_directory(&git, &before).unwrap_err()),
        "concurrent_change"
    );
}

#[path = "review_identity_vanishing.rs"]
mod vanishing;
