// New host-owned freeze checker tests. Owns schema/ownership/negative-path
// coverage; real Claude acceptance is an operator step, not simulated here.
use super::*;

type FixtureFreezeRunner = Arc<dyn Fn(&mut Command) -> Result<std::process::Output> + Send + Sync>;

fn freeze_fixture_router(state: AppState, runner: FixtureFreezeRunner) -> Router {
    // Private admission for each router; no parallel test drains a global.
    let permits = Arc::new(tokio::sync::Semaphore::new(2));
    Router::new()
        .route(
            "/api/sessions/{id}/delegation-review-freeze",
            axum::routing::post(
                move |AxumPath(child): AxumPath<String>,
                      State(state): State<AppState>,
                      request: Result<Json<ReviewFreezeRequest>, JsonRejection>| {
                    let runner = runner.clone();
                    run_review_freeze_request(
                        child,
                        state,
                        request,
                        permits.clone(),
                        move |state, child, request| {
                            state.verify_review_freeze_with_runner(child, request, |command| {
                                runner(command)
                            })
                        },
                    )
                },
            ),
        )
        .with_state(state)
}

fn freeze_fixture_output(expected: &str, exit: i32) -> Result<std::process::Output> {
    // A real bounded fixture process supplies status and distinct stdout/stderr.
    // It does NOT claim to be the compiled checker, covered by tests/review_freeze_cli.rs.
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("powershell.exe");
        command.args(["-NoProfile", "-NonInteractive", "-Command",
            "[Console]::Out.Write($env:TERMAL_FREEZE_FIXTURE_HASH + [char]10); [Console]::Error.Write('fixture diagnostic'); exit ([int]$env:TERMAL_FREEZE_FIXTURE_EXIT)"]);
        command
    };
    #[cfg(unix)]
    let mut command = {
        let mut command = Command::new("sh");
        command.args(["-c", "printf '%s\\n' \"$TERMAL_FREEZE_FIXTURE_HASH\"; printf 'fixture diagnostic' >&2; exit \"$TERMAL_FREEZE_FIXTURE_EXIT\""]);
        command
    };
    command
        .env("TERMAL_FREEZE_FIXTURE_HASH", expected)
        .env("TERMAL_FREEZE_FIXTURE_EXIT", exit.to_string());
    run_review_freeze_checker(&mut command)
}

#[tokio::test]
async fn review_freeze_http_observes_failure_and_rejects_concurrent_attempt_change() {
    let state = test_app_state();
    let parent = test_session_id(&state, Agent::Codex);
    let (delegation, child) =
        super::delegation_support::install_required_review_delegation(&state, &parent);
    let (root, request) = fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        inner.sessions[index].session.workdir = root.to_string_lossy().into_owned();
        inner
            .delegations
            .iter_mut()
            .find(|d| d.id == delegation)
            .unwrap()
            .cwd = root.to_string_lossy().into_owned();
    }
    let expected = request.expected_fingerprint;
    let http_request = || {
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{child}/delegation-review-freeze"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"manifestPath":".git/freeze.json","expectedFingerprint":expected})
                    .to_string(),
            ))
            .unwrap()
    };

    for exit in [7, 0] {
        let expected = expected.clone();
        let root = root.clone();
        let runner = Arc::new(move |command: &mut Command| {
            assert_eq!(
                command.get_program(),
                std::env::current_exe().unwrap().as_os_str()
            );
            assert_eq!(
                command
                    .get_args()
                    .map(|s| s.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
                vec![
                    "review-freeze-check".to_owned(),
                    root.to_string_lossy().into_owned(),
                    ".git/freeze.json".to_owned(),
                    expected.clone()
                ]
            );
            freeze_fixture_output(&expected, exit)
        });
        let (status, body): (StatusCode, Value) = request_json(
            &freeze_fixture_router(state.clone(), runner),
            http_request(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["verified"], exit == 0);
        assert_eq!(body["observer"]["status"], exit);
        assert_eq!(body["observer"]["stdoutExact"], exit == 0);
        assert_eq!(body["observer"]["stdoutLength"], 65);
        assert_eq!(
            body["observer"]["stderrBase64"],
            base64::engine::general_purpose::STANDARD.encode("fixture diagnostic")
        );
    }
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let entered_tx = Mutex::new(Some(entered_tx));
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let expected_for_runner = expected.clone();
    let runner = Arc::new(move |_: &mut Command| {
        // The old attempt has produced a valid real-process result. Hold it
        // before returning to the post-run authority check, without sleeping.
        let output = freeze_fixture_output(&expected_for_runner, 0)?;
        assert!(output.status.success());
        entered_tx.lock().unwrap().take().unwrap().send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        Ok(output)
    });
    let app = freeze_fixture_router(state.clone(), runner);
    let request = http_request();
    let pending = tokio::spawn(async move { request_json::<Value>(&app, request).await });
    tokio::time::timeout(Duration::from_secs(10), entered_rx)
        .await
        .unwrap()
        .unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        inner
            .delegations
            .iter_mut()
            .find(|d| d.id == delegation)
            .unwrap()
            .review_result_submission_attempt += 1;
    }
    release_tx.send(()).unwrap();
    let (status, body) = pending.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        body.to_string().contains("review attempt changed"),
        "{body}"
    );
    assert!(
        body.get("verified").is_none(),
        "obsolete success must never escape"
    );
}

// Independent fixture/control Git, not the production command builder.
// Override only in the individual control that deliberately tests a setting.
fn git_command() -> Command {
    let mut command = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            command.env_remove(name);
        }
    }
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    command
        .env("GIT_CONFIG_GLOBAL", null)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .args([
            "-c",
            &format!("core.excludesFile={null}"),
            "-c",
            &format!("core.attributesFile={null}"),
            "-c",
            &format!("core.hooksPath={null}"),
            "-c",
            "core.fsmonitor=false",
            "-c",
            "commit.gpgsign=false",
        ]);
    command
}

fn run_git_test_command_output(root: &FsPath, args: &[&str]) -> String {
    let output = git_command().current_dir(root).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn run_git_test_command(root: &FsPath, args: &[&str]) {
    run_git_test_command_output(root, args);
}

fn init_git_document_test_repo(root: &FsPath) {
    run_git_test_command(root, &["init", "--object-format=sha1"]);
    for (key, value) in [
        ("core.autocrlf", "false"),
        ("user.email", "termal@example.com"),
        ("user.name", "TermAl"),
    ] {
        run_git_test_command(root, &["config", key, value]);
    }
}

#[test]
fn review_freeze_ignores_xdg_files_but_keeps_repository_rules() {
    let (root, _) = fixture();
    let xdg = root.join(".git/host-xdg");
    fs::create_dir_all(xdg.join("git")).unwrap();
    fs::write(xdg.join("git/ignore"), "*.bin\n").unwrap();
    fs::write(xdg.join("git/attributes"), "*.txt -diff\n").unwrap();
    fs::write(root.join("evidence.bin"), b"evidence").unwrap();
    let git = ReviewFreezeGit::new(&root).unwrap();
    for args in [
        vec!["ls-files", "--others", "--exclude-standard", "-z"],
        vec![
            "diff",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
        ],
    ] {
        let clean = git.run(&args, false).unwrap();
        let protected = git
            .command(&args)
            .env("XDG_CONFIG_HOME", &xdg)
            .output()
            .unwrap();
        assert!(protected.status.success());
        assert_eq!(protected.stdout, clean);
        // Independent negative control: enable only the deliberately hostile
        // files, leaving all unrelated developer Git configuration excluded.
        let control = git_command()
            .current_dir(&root)
            .env("XDG_CONFIG_HOME", &xdg)
            .args([
                "-c",
                &format!("core.excludesFile={}", xdg.join("git/ignore").display()),
                "-c",
                &format!(
                    "core.attributesFile={}",
                    xdg.join("git/attributes").display()
                ),
            ])
            .args(&args)
            .output()
            .unwrap();
        assert!(control.status.success());
        assert_ne!(control.stdout, clean, "control must affect {args:?}");
    }
    fs::write(root.join(".gitignore"), "evidence.bin\n").unwrap();
    fs::write(root.join(".gitattributes"), "*.txt -diff\n").unwrap();
    let paths = git
        .run(&["ls-files", "--others", "--exclude-standard"], false)
        .unwrap();
    assert!(!String::from_utf8(paths).unwrap().contains("evidence.bin"));
    let diff = git
        .run(&["diff", "--no-ext-diff", "--no-textconv"], false)
        .unwrap();
    assert!(String::from_utf8(diff).unwrap().contains("Binary files"));
}

#[test]
fn review_freeze_explains_nested_repositories_and_root_cwd_requirement() {
    let (root, _) = fixture();
    let nested = root.join("embedded");
    fs::create_dir(&nested).unwrap();
    init_git_document_test_repo(&nested);
    fs::write(nested.join("file"), "nested").unwrap();
    run_git_test_command(&nested, &["add", "."]);
    run_git_test_command(&nested, &["commit", "-m", "nested"]);
    let error = capture_review_freeze(&ReviewFreezeGit::new(&root).unwrap()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("embedded untracked Git repositories"),
        "{error:#}"
    );
    let subdir = root.join("ordinary");
    fs::create_dir(&subdir).unwrap();
    let error = ReviewFreezeGit::new(&subdir).err().unwrap();
    assert!(error.to_string().contains("worktree root"));
}

#[tokio::test]
async fn review_freeze_route_rejects_invalid_json_requests_and_busy_reads() {
    let state = test_app_state();
    let parent = test_session_id(&state, Agent::Codex);
    let (_, child) = super::delegation_support::install_required_review_delegation(&state, &parent);
    let limiter = Arc::new(tokio::sync::Semaphore::new(2));
    let app = app_router(state).layer(axum::Extension(ReviewFreezeLimiter(limiter.clone())));
    for (body, expected) in [
        (
            json!({"manifestPath":"x", "expectedFingerprint":"a".repeat(64), "extra":true}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"manifestPath":"x", "expectedFingerprint":"bad"}),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (status, _): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{child}/delegation-review-freeze"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await;
        assert_eq!(status, expected);
    }
    let _permits = limiter.acquire_many(2).await.unwrap();
    let (status, _): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{child}/delegation-review-freeze"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"manifestPath":"x", "expectedFingerprint":"a".repeat(64)}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn review_freeze_http_rejects_identity_mismatch_before_running_checker() {
    for invalid in ["workdir", "hidden", "remote"] {
        let state = test_app_state();
        let parent = test_session_id(&state, Agent::Codex);
        let (_, child) =
            super::delegation_support::install_required_review_delegation(&state, &parent);
        assert!(state.delegation_control_plane_capability_allowed(
            &child,
            DelegationControlPlaneCapability::ReviewFreeze
        ));
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&child).unwrap();
            let record = &mut inner.sessions[index];
            match invalid {
                "workdir" => record.session.workdir.push_str("/different"),
                "hidden" => record.hidden = true,
                "remote" => {
                    record.remote_id = Some("remote-host".to_owned());
                    record.remote_session_id = Some("remote-child".to_owned());
                }
                _ => unreachable!(),
            }
        }
        assert!(
            !state.delegation_control_plane_capability_allowed(
                &child,
                DelegationControlPlaneCapability::ReviewFreeze
            ),
            "{invalid}"
        );
        let app = freeze_fixture_router(
            state,
            Arc::new(|_| panic!("unauthorized checker must not run")),
        );
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{child}/delegation-review-freeze"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"manifestPath":"x", "expectedFingerprint":"a".repeat(64)}).to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{invalid}: {body}");
        assert!(body.get("verified").is_none());
    }
}

#[tokio::test]
async fn review_freeze_http_transport_failures_are_not_observed_verifications() {
    for failure in ["deadline", "output limit"] {
        let state = test_app_state();
        let parent = test_session_id(&state, Agent::Codex);
        let (_, child) =
            super::delegation_support::install_required_review_delegation(&state, &parent);
        let runner = Arc::new(move |_: &mut Command| {
            if failure == "output limit" {
                // Exercise the production observer's 4096-byte bound with a
                // real fixture process, not a fabricated transport error.
                return freeze_fixture_output(&"a".repeat(4097), 0);
            }
            #[cfg(windows)]
            let mut command = {
                let mut command = Command::new("powershell.exe");
                command.args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "Start-Sleep -Seconds 60",
                ]);
                command
            };
            #[cfg(unix)]
            let mut command = {
                let mut command = Command::new("sh");
                command.args(["-c", "sleep 60"]);
                command
            };
            // Already expired: no timing race or long test sleep. The owned
            // child/group is killed by the same observer used in production.
            run_bounded_read_process(&mut command, std::time::Instant::now(), 4096, true)
        });
        let app = freeze_fixture_router(state, runner);
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{child}/delegation-review-freeze"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"manifestPath":"x", "expectedFingerprint":"a".repeat(64)}).to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{failure}: {body}"
        );
        assert!(body.to_string().contains("review checker did not complete"));
        assert!(body.to_string().contains(failure), "{body}");
        assert!(body.get("verified").is_none());
        assert!(body.get("observer").is_none());
    }
}

#[test]
fn review_freeze_normalization_requires_matching_local_git_settings() {
    let (root, _) = fixture();
    // A process-local config simulates a parent's global autocrlf setting;
    // no real user/system configuration or process-global environment changes.
    run_git_test_command(&root, &["config", "--unset", "core.autocrlf"]);
    fs::write(root.join("tracked.txt"), "working\r\n").unwrap();
    let args = [
        "diff",
        "--binary",
        "--full-index",
        "--no-ext-diff",
        "--no-textconv",
    ];
    let parent = git_command()
        .current_dir(&root)
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.autocrlf")
        .env("GIT_CONFIG_VALUE_0", "true")
        .args(args)
        .output()
        .unwrap();
    assert!(parent.status.success());
    let checker = ReviewFreezeGit::new(&root).unwrap();
    assert_ne!(
        checker.run(&args, false).unwrap(),
        parent.stdout,
        "a CRLF worktree really differs when the parent's normalization is omitted"
    );
    // Explicit repository normalization is retained by both implementations;
    // supporting it does not re-enable global hooks, filters or transports.
    run_git_test_command(&root, &["config", "core.autocrlf", "true"]);
    assert_eq!(checker.run(&args, false).unwrap(), parent.stdout);
}

#[test]
fn review_freeze_submodule_filter_sentinel() {
    let (root, request) = fixture();
    let sub = root.join("sub");
    fs::create_dir(&sub).unwrap();
    init_git_document_test_repo(&sub);
    fs::write(sub.join(".gitattributes"), "tracked.txt filter=review\n").unwrap();
    fs::write(sub.join("tracked.txt"), "base\n").unwrap();
    run_git_test_command(&sub, &["add", "."]);
    run_git_test_command(&sub, &["commit", "-m", "submodule base"]);
    fs::write(
        root.join(".gitmodules"),
        "[submodule \"sub\"]\n\tpath = sub\n\turl = ./sub\n",
    )
    .unwrap();
    run_git_test_command(&root, &["add", "sub", ".gitmodules"]);
    run_git_test_command(&root, &["commit", "-m", "submodule"]);
    run_git_test_command(
        &sub,
        &[
            "config",
            "filter.review.clean",
            "printf invoked > ../.git/submodule-filter-ran; cat",
        ],
    );
    // Same size as the indexed content: status must inspect/convert content,
    // rather than deciding it changed solely from the stat size.
    fs::write(sub.join("tracked.txt"), "work\n").unwrap();
    let marker = root.join(".git/submodule-filter-ran");
    let error = check_review_freeze(&root, &request).unwrap_err();
    assert!(error.to_string().contains("submodules"), "{error:#}");
    assert!(
        !marker.exists(),
        "verification must not execute submodule filters"
    );
    // Negative control: the pre-fix diff, with the same hardened Git
    // environment, really executes this local filter before detecting drift.
    let git = ReviewFreezeGit::new(&root).unwrap();
    git.run(
        &[
            "diff",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
        ],
        false,
    )
    .unwrap();
    assert!(
        marker.exists(),
        "vulnerable-path control must execute the filter"
    );
    fs::remove_file(&marker).unwrap();
    // Even without index gitlinks, a removed HEAD gitlink remains unsupported.
    run_git_test_command(&root, &["update-index", "--force-remove", "sub"]);
    assert!(
        capture_review_freeze(&git)
            .unwrap_err()
            .to_string()
            .contains("submodules")
    );
    assert!(!marker.exists());
    // And an unborn repository with a newly staged gitlink is rejected.
    let oid = run_git_test_command_output(&sub, &["rev-parse", "HEAD"]);
    let unborn = root.join("unborn");
    fs::create_dir(&unborn).unwrap();
    init_git_document_test_repo(&unborn);
    run_git_test_command(
        &unborn,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{oid},sub"),
        ],
    );
    assert!(
        capture_review_freeze(&ReviewFreezeGit::new(&unborn).unwrap())
            .unwrap_err()
            .to_string()
            .contains("submodules")
    );
}

#[test]
fn review_freeze_claude_authority_uses_the_requested_capability() {
    let state = test_app_state();
    let parent = test_session_id(&state, Agent::Claude);
    let (delegation, child) =
        super::delegation_support::install_required_review_delegation(&state, &parent);
    for policy in [
        DelegationWritePolicy::ReadOnly,
        DelegationWritePolicy::SharedWorktree {
            owned_paths: vec![],
        },
        DelegationWritePolicy::IsolatedWorktree {
            owned_paths: vec![],
            worktree_path: None,
        },
    ] {
        let read_only = matches!(policy, DelegationWritePolicy::ReadOnly);
        {
            let mut inner = state.inner.lock().unwrap();
            inner
                .delegations
                .iter_mut()
                .find(|d| d.id == delegation)
                .unwrap()
                .write_policy = policy;
            let d = inner
                .delegations
                .iter()
                .find(|d| d.id == delegation)
                .unwrap();
            assert_eq!(
                delegation_state_summary_from_record(d).review_freeze_allowed,
                read_only
            );
            assert_eq!(
                build_delegation_prompt(d).contains("use `termal_review_freeze_check`"),
                read_only
            );
        }
        for (tool, expected) in [
            (TERMAL_REVIEW_FREEZE_QUALIFIED_TOOL_NAME, read_only),
            (TERMAL_SUBMIT_REVIEW_RESULT_QUALIFIED_TOOL_NAME, true),
            ("mcp__other__termal_review_freeze_check", false),
        ] {
            let message = json!({"type":"control_request","request_id":"freeze-authority",
                "request":{"subtype":"can_use_tool","tool_name":tool,"input":{}}});
            let access = state.claude_control_plane_request_allowed(&child, &message);
            assert_eq!(access, expected);
            let action = classify_claude_control_request(
                &message,
                &mut ClaudeTurnState::default(),
                ClaudeApprovalMode::ReadOnlyAutoApprove,
                true,
                ".",
                access,
            )
            .unwrap();
            assert_eq!(
                matches!(
                    action,
                    Some(ClaudeControlRequestAction::Respond(
                        ClaudePermissionDecision::Allow { .. }
                    ))
                ),
                expected
            );
        }
    }
}

#[test]
fn review_freeze_cli_mode_parses_and_validates_arity() {
    let (root, request) = fixture();
    let args = vec![
        "review-freeze-check".to_owned(),
        root.to_string_lossy().into_owned(),
        request.manifest_path,
        request.expected_fingerprint,
    ];
    let Mode::ReviewFreeze(parsed) = Mode::parse(args.clone()).unwrap() else {
        panic!("wrong CLI mode")
    };
    assert_eq!(parsed, args);
    review_freeze_mode(&parsed).unwrap();
    for length in 0..4 {
        assert!(
            review_freeze_mode(&parsed[..length])
                .unwrap_err()
                .to_string()
                .contains("usage:")
        );
    }
    let mut extra = parsed.clone();
    extra.push("extra".to_owned());
    assert!(
        review_freeze_mode(&extra)
            .unwrap_err()
            .to_string()
            .contains("usage:")
    );
    let mut invalid = parsed;
    invalid[3] = "not-a-hash".to_owned();
    assert!(review_freeze_mode(&invalid).is_err());
}

#[test]
fn review_freeze_codex_auto_response_and_bridge_projection_fail_closed() {
    let state = test_app_state();
    let parent = test_session_id(&state, Agent::Codex);
    let (delegation, child) =
        super::delegation_support::install_required_review_delegation(&state, &parent);
    let request = json!({"id":"freeze-approval","params":{
        "threadId":"thread-reviewer","turnId":"turn-reviewer","serverName":TERMAL_DELEGATION_MCP_SERVER_NAME,
        "mode":"form","message":"Approval copy","requestedSchema":{"type":"object","properties":{}},
        "_meta":{"codex_approval_kind":"mcp_tool_call","tool_description":TERMAL_REVIEW_FREEZE_TOOL_DESCRIPTION,
            "tool_params":{"manifestPath":".git/freeze.json","expectedFingerprint":"a".repeat(64)}}}});
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
        ("/params/_meta/tool_description", json!("wrong")),
        (
            "/params/_meta/tool_params/expectedFingerprint",
            json!("not a hash"),
        ),
        (
            "/params/_meta/tool_params",
            json!({"manifestPath":"x","expectedFingerprint":"a".repeat(64),"command":"evil"}),
        ),
        ("/params/serverName", json!("other")),
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
    state
        .inner
        .lock()
        .unwrap()
        .delegations
        .iter_mut()
        .find(|d| d.id == delegation)
        .unwrap()
        .write_policy = DelegationWritePolicy::SharedWorktree {
        owned_paths: vec![],
    };
    assert!(
        !try_auto_respond_delegation_control_plane_request(
            "mcpServer/elicitation/request",
            &request,
            &state,
            &child,
            &tx
        )
        .unwrap()
    );
    assert!(rx.try_recv().is_err());
    for value in [
        json!({"verified":false}),
        json!({}),
        json!({"verified":"true"}),
    ] {
        assert_eq!(
            delegation_mcp_tool_result(TERMAL_REVIEW_FREEZE_TOOL_NAME, &value)["isError"],
            true
        );
    }
    assert_eq!(
        delegation_mcp_tool_result(TERMAL_REVIEW_FREEZE_TOOL_NAME, &json!({"verified":true}))["isError"],
        false
    );
}

#[test]
fn review_freeze_missing_promisor_object_never_invokes_transport() {
    let (root, request) = fixture();
    let oid = run_git_test_command_output(&root, &["rev-parse", "HEAD:tracked.txt"]);
    let object = root.join(".git/objects").join(&oid[..2]).join(&oid[2..]);
    assert!(object.is_file());
    fs::remove_file(&object).unwrap();
    for args in [
        [
            "config",
            "remote.origin.url",
            "ssh://invalid.example/review",
        ],
        ["config", "remote.origin.promisor", "true"],
        ["config", "extensions.partialClone", "origin"],
        [
            "config",
            "core.sshCommand",
            "printf invoked > .git/transport-ran; exit 1",
        ],
    ] {
        run_git_test_command(&root, &args);
    }
    let error = check_review_freeze(&root, &request).unwrap_err();
    assert!(error.to_string().contains("Git verification"), "{error:#}");
    assert!(!root.join(".git/transport-ran").exists());
    assert!(!object.exists(), "missing object must not be fetched");
    // Negative control: the same fixture really attempts the configured local
    // transport when both guards are removed. The sentinel exits before SSH.
    let output = git_command()
        .current_dir(&root)
        .env_remove("GIT_NO_LAZY_FETCH")
        .env_remove("GIT_ALLOW_PROTOCOL")
        .args(["cat-file", "-p", &oid])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        root.join(".git/transport-ran").exists(),
        "transport control did not execute"
    );
    assert!(!object.exists());
}

#[test]
fn review_freeze_git_failures_retain_operation_timing_budget_and_cause() {
    for cause in ["bounded read deadline exceeded", "process wait failed"] {
        let error = review_freeze_git_failure(
            anyhow!(cause),
            &["diff", "--binary", "--no-ext-diff"],
            Duration::from_millis(125),
            Duration::from_secs(7),
        );
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(
                "Git verification operation \"diff\" with arguments \
                 [\"diff\", \"--binary\", \"--no-ext-diff\"] failed after 125ms"
            ),
            "{rendered}"
        );
        assert!(
            rendered
                .contains("remaining shared budget at call start: 7s; total shared budget: 20s"),
            "{rendered}"
        );
        assert_eq!(error.chain().last().unwrap().to_string(), cause);
    }
}

#[test]
fn review_freeze_matches_independent_nonempty_engram_golden() {
    let root = test_temp_dir().join(format!("review-golden-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    run_git_test_command(&root, &["init", "--object-format=sha1"]);
    run_git_test_command(&root, &["config", "core.autocrlf", "false"]);
    fs::write(root.join("tracked.txt"), b"base\n").unwrap();
    run_git_test_command(&root, &["add", "tracked.txt"]);
    let output = git_command()
        .current_dir(&root)
        .env("GIT_AUTHOR_NAME", "TermAl")
        .env("GIT_AUTHOR_EMAIL", "termal@example.com")
        .env("GIT_COMMITTER_NAME", "TermAl")
        .env("GIT_COMMITTER_EMAIL", "termal@example.com")
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .args(["-c", "commit.gpgsign=false", "commit", "-m", "base"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        run_git_test_command_output(&root, &["rev-parse", "HEAD"]),
        "833d67a6b21bf6adfe2a9b16cbeaddee023d2074"
    );
    fs::write(root.join("tracked.txt"), b"staged\n").unwrap();
    run_git_test_command(&root, &["add", "tracked.txt"]);
    fs::write(root.join("tracked.txt"), b"working\n").unwrap();
    fs::create_dir(root.join("nested")).unwrap();
    fs::write(root.join("nested/z.txt"), b"z\n").unwrap();
    fs::write(root.join("nested/A.bin"), [0, 255, 10]).unwrap();
    // Fixed outputs generated with Engram's independent schema-1 reference
    // scripts/review-freeze-fingerprint.mjs at 3f1348b2 (2026-09-13).
    // Expected hashes are never generated with capture_review_freeze.
    let git = ReviewFreezeGit::new(&root).unwrap();
    assert_eq!(
        capture_review_freeze(&git).unwrap(),
        "43a94b4421cb69c5844666467bbe984bfb3ebd44b8f552635106c94df200756f"
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("nested/A.bin", root.join("link")).unwrap();
        assert_eq!(
            capture_review_freeze(&git).unwrap(),
            "43997039e70b9dd30a1d3c0922731c7716938bdc1913a59e51de1cc9a86d12a4"
        );
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.join("nested/z.txt"), fs::Permissions::from_mode(0o755)).unwrap();
        // Same independent Engram reference, with POSIX readlink spelling
        // (nested/A.bin, not Windows nested\A.bin) and lstat executable bit
        // supplied during Windows vector generation. Here both are real.
        assert_eq!(
            capture_review_freeze(&git).unwrap(),
            "33a024a79617d890b30e9f8ad0abdb9ef6c71333d351b2a1a9c71df0b0033d4a"
        );
    }
}

fn fixture() -> (PathBuf, ReviewFreezeRequest) {
    let root = test_temp_dir().join(format!("review-freeze-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    init_git_document_test_repo(&root);
    fs::write(root.join("tracked.txt"), b"base\n").unwrap();
    run_git_test_command(&root, &["add", "tracked.txt"]);
    run_git_test_command(&root, &["commit", "-m", "base"]);
    fs::write(root.join("tracked.txt"), b"staged\n").unwrap();
    run_git_test_command(&root, &["add", "tracked.txt"]);
    fs::write(root.join("tracked.txt"), b"working\n").unwrap();
    fs::write(root.join("untracked.txt"), b"extra\0bytes\n").unwrap();
    let git = ReviewFreezeGit::new(&root).unwrap();
    let expected = capture_review_freeze(&git).unwrap();
    fs::write(
        root.join(".git/freeze.json"),
        serde_json::to_vec(&json!({
            "schemaVersion":1,"root":fs::canonicalize(&root).unwrap(),"fingerprint":expected
        }))
        .unwrap(),
    )
    .unwrap();
    (
        root,
        ReviewFreezeRequest {
            manifest_path: ".git/freeze.json".to_owned(),
            expected_fingerprint: expected,
        },
    )
}

#[test]
fn review_freeze_matches_and_rejects_untracked_index_worktree_and_head_drift() {
    let (root, request) = fixture();
    assert_eq!(
        check_review_freeze(&root, &request).unwrap(),
        request.expected_fingerprint
    );
    for (path, changed, original) in [
        ("untracked.txt", &b"changed"[..], &b"extra\0bytes\n"[..]),
        ("tracked.txt", &b"different"[..], &b"working\n"[..]),
    ] {
        fs::write(root.join(path), changed).unwrap();
        assert!(check_review_freeze(&root, &request).is_err());
        fs::write(root.join(path), original).unwrap();
    }
    run_git_test_command(&root, &["add", "tracked.txt"]);
    assert!(check_review_freeze(&root, &request).is_err(), "index drift");
    run_git_test_command(&root, &["commit", "-m", "changed HEAD"]);
    assert!(check_review_freeze(&root, &request).is_err(), "HEAD drift");
}

#[test]
fn review_freeze_rejects_manifest_and_parent_literal_mismatch() {
    let (root, mut request) = fixture();
    request.expected_fingerprint = "a".repeat(64);
    assert!(
        check_review_freeze(&root, &request)
            .unwrap_err()
            .to_string()
            .contains("independent")
    );
    for body in [
        json!({}),
        json!({"schemaVersion":2}),
        json!({"schemaVersion":1,"root":"relative","fingerprint":"a".repeat(64)}),
    ] {
        fs::write(
            root.join(".git/freeze.json"),
            serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();
        assert!(check_review_freeze(&root, &request).is_err());
    }
    request.manifest_path = "../outside.json".to_owned();
    assert!(
        check_review_freeze(&root, &request)
            .unwrap_err()
            .to_string()
            .contains("traversal")
    );
}

/// Removes the directories it owns when dropped, on success and on an
/// assertion's unwind alike, so a direct `cargo test` leaves nothing behind.
struct RemoveDirsOnDrop(Vec<PathBuf>);

impl Drop for RemoveDirsOnDrop {
    fn drop(&mut self) {
        for dir in &self.0 {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

#[test]
fn review_freeze_accepts_a_manifest_in_the_linked_worktrees_own_git_dir_only() {
    let (main_root, _) = fixture();
    let mut cleanup = RemoveDirsOnDrop(vec![main_root.clone()]);
    let mut add_worktree = |label: &str| {
        let path = test_temp_dir().join(format!("review-freeze-{label}-{}", Uuid::new_v4()));
        cleanup.0.push(path.clone());
        run_git_test_command(
            &main_root,
            &["worktree", "add", "--detach", &path.to_string_lossy()],
        );
        path
    };
    let linked = add_worktree("linked");
    let sibling = add_worktree("sibling");
    let git = ReviewFreezeGit::new(&linked).unwrap();
    let expected = capture_review_freeze(&git).unwrap();
    let body = serde_json::to_vec(&json!({
        "schemaVersion": 1, "root": fs::canonicalize(&linked).unwrap(), "fingerprint": expected
    }))
    .unwrap();
    let check_at = |path: &str| {
        fs::write(path, &body).unwrap();
        check_review_freeze(
            &linked,
            &ReviewFreezeRequest {
                manifest_path: path.to_owned(),
                expected_fingerprint: expected.clone(),
            },
        )
    };
    let git_output = |root: &FsPath, args: &[&str]| run_git_test_command_output(root, args);

    // The path Engram's /review-changes resolves for a linked worktree lies
    // outside its root, in its own Git directory.
    let own = git_output(
        &linked,
        &["rev-parse", "--path-format=absolute", "--git-path", "engram-review-freeze.json"],
    );
    assert!(!fs::canonicalize(FsPath::new(&own).parent().unwrap())
        .unwrap()
        .starts_with(fs::canonicalize(&linked).unwrap()));
    assert_eq!(check_at(&own).unwrap(), expected);

    // For a linked worktree's review: not the shared common directory, nor
    // another worktree's Git directory.
    let common = git_output(
        &linked,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    let sibling_git = git_output(&sibling, &["rev-parse", "--absolute-git-dir"]);
    for dir in [common, sibling_git] {
        let path = FsPath::new(&dir).join("engram-review-freeze.json");
        let error = check_at(&path.to_string_lossy()).unwrap_err().to_string();
        assert!(error.contains("manifest must be inside"), "{dir}: {error}");
    }
}

#[test]
fn review_freeze_never_runs_git_filters_or_repository_node() {
    let (root, request) = fixture();
    let sentinel = root.join("executed");
    run_git_test_command(
        &root,
        &["config", "filter.review.process", "touch executed"],
    );
    fs::write(root.join(".gitattributes"), "*.txt filter=review\n").unwrap();
    fs::write(root.join("node.cmd"), "@echo unsafe > executed\r\n").unwrap();
    assert!(
        check_review_freeze(&root, &request)
            .unwrap_err()
            .to_string()
            .contains("filters")
    );
    assert!(!sentinel.exists());
}

#[test]
fn review_freeze_read_only_authority_and_request_shape_are_narrow() {
    let state = test_app_state();
    let parent = test_session_id(&state, Agent::Codex);
    let (delegation, child) =
        super::delegation_support::install_required_review_delegation(&state, &parent);
    assert!(state.review_freeze_child_identity(&parent).is_err());
    assert!(state.review_freeze_child_identity(&child).is_ok());
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner
            .delegations
            .iter()
            .position(|d| d.id == delegation)
            .unwrap();
        inner.delegations[index].status = DelegationStatus::Completed;
    }
    assert!(state.review_freeze_child_identity(&child).is_err());
    assert!(serde_json::from_value::<ReviewFreezeRequest>(json!({"manifestPath":"a","expectedFingerprint":"a".repeat(64),"command":"node -e evil"})).is_err());
    for name in [
        "termal_review_freeze_check",
        "mcp__other__termal_review_freeze_check",
    ] {
        assert_eq!(
            delegation_control_plane_capability_for_claude_tool_name(name),
            None
        );
    }
    assert_eq!(
        delegation_control_plane_capability_for_claude_tool_name(
            TERMAL_REVIEW_FREEZE_QUALIFIED_TOOL_NAME
        ),
        Some(DelegationControlPlaneCapability::ReviewFreeze)
    );
}

#[test]
fn review_freeze_observer_requires_exact_successful_stdout_and_keeps_stderr_separate() {
    let expected = "a".repeat(64);
    let mut child = test_exit_success_child();
    let status = child.wait().unwrap();
    for stdout in [
        format!("{expected}\n"),
        expected.clone(),
        format!("{expected}\nextra"),
    ] {
        let exact = stdout == format!("{expected}\n");
        let observation = review_freeze_observation(
            std::process::Output {
                status,
                stdout: stdout.into_bytes(),
                stderr: b"limitation\n".to_vec(),
            },
            &expected,
        );
        assert_eq!(observation.stdout_exact, exact);
        assert_eq!(observation.stderr_length, 11);
        assert_eq!(observation.stderr_base64, "bGltaXRhdGlvbgo=");
    }
}

#[test]
fn review_freeze_unborn_and_path_rejection_are_explicit() {
    let root = test_temp_dir().join(format!("review-freeze-unborn-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    init_git_document_test_repo(&root);
    let git = ReviewFreezeGit::new(&root).unwrap();
    // Independent golden: BE uint64 framing of schema=1, head=UNBORN,
    // staged-diff="", unstaged-diff=""; no untracked objects.
    assert_eq!(
        capture_review_freeze(&git).unwrap(),
        "3d168792d0dc7091e70b8c56fb55423b9bd7ebbf5b44a83d851662c6e26a2c53"
    );
    for path in [
        "../outside",
        "C:/outside",
        "/outside",
        "folder/../file",
        "folder\\file",
    ] {
        assert!(review_freeze_untracked_path(&git.root, path).is_err());
    }
}
