use super::*;
use axum::body::{Body, to_bytes};
use axum::http::Request;
use tower::ServiceExt;

const FIXTURE: &str = "TERMAL_CODEX_ACCOUNT_FIXTURE";
const SECRET_SENTINEL: &str = "fixture-private-challenge";
const LOGIN_ID: &str = "d5750c00-44b8-4dbe-8b5f-4c934fea7524";
const FIXTURE_GRANT: &[u8] = b"disposable fixture grant";
const FIXTURE_USER: &str = "fixture-managed-user";
const SAVED_THREAD: &str = "saved-provider-thread";

#[test]
fn disposable_account_app_server() {
    let Some(behavior) = std::env::var_os(FIXTURE) else {
        return;
    };
    let home = PathBuf::from(std::env::var_os("CODEX_HOME").expect("fixture home"));
    let fixture_grant = home.join("auth.json");
    let mut output = io::stdout().lock();
    writeln!(output).unwrap();
    output.flush().unwrap();
    for line in io::stdin().lock().lines().map_while(Result::ok) {
        let request: Value = serde_json::from_str(&line).unwrap();
        let method = request["method"].as_str().unwrap();
        if method == "shutdown" {
            break;
        }
        let Some(id) = request.get("id") else {
            continue;
        };
        let result = match method {
            "initialize" => json!({"userAgent":"termal-account-fixture"}),
            "config/read" => {
                let config = match fs::read_to_string(home.join("config.toml")) {
                    Ok(text) => toml_edit::de::from_str::<Value>(&text).unwrap(),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        json!({"cli_auth_credentials_store":"file"})
                    }
                    Err(error) => panic!("could not read fixture configuration: {error}"),
                };
                json!({"config":config})
            }
            "account/read" => {
                let user_id = fs::metadata(&fixture_grant)
                    .ok()
                    .filter(|metadata| metadata.is_file())
                    .map(|metadata| {
                        if behavior == "wrong-sibling"
                            && home.file_name().and_then(|name| name.to_str())
                                != Some(SharedCodexProfile::Default.codex_home_scope())
                        {
                            "fixture-wrong-user"
                        } else if metadata.len() == FIXTURE_GRANT.len() as u64 {
                            FIXTURE_USER
                        } else {
                            "fixture-legacy-user"
                        }
                    });
                let account = user_id.map(|_| json!({"type":"chatgpt"}));
                json!({"account":account,"requiresOpenaiAuth":true,"userId":user_id})
            }
            "account/login/start" => {
                fs::write(home.join("fixture-login-observed"), b"called").unwrap();
                if behavior != "cancel" {
                    fs::write(&fixture_grant, FIXTURE_GRANT).unwrap();
                    writeln!(
                        output,
                        "{}",
                        json!({"method":"account/login/completed",
                        "params":{"loginId":LOGIN_ID,"success":true}})
                    )
                    .unwrap();
                }
                json!({"type":"chatgpt","loginId":LOGIN_ID,
                    "authUrl":format!("https://auth.openai.com/authorize?state={SECRET_SENTINEL}")})
            }
            "account/login/cancel" => {
                fs::write(&fixture_grant, b"disposable late fixture grant").unwrap();
                writeln!(
                    output,
                    "{}",
                    json!({"method":"account/login/completed",
                    "params":{"loginId":LOGIN_ID,"success":true}})
                )
                .unwrap();
                json!({"status":"canceled"})
            }
            "account/logout" => {
                fs::write(home.join("fixture-logout-observed"), b"called").unwrap();
                if fixture_grant.exists() {
                    fs::remove_file(&fixture_grant).unwrap();
                }
                json!({})
            }
            "thread/resume" if behavior == "resume-refusal" => {
                assert_eq!(request["params"]["threadId"], SAVED_THREAD);
                writeln!(
                    output,
                    "{}",
                    json!({"id":id,"error":{"code":-32600,
                        "message":format!("no rollout found for thread id {SAVED_THREAD}")}})
                )
                .unwrap();
                output.flush().unwrap();
                continue;
            }
            "thread/start" if behavior == "boot" => {
                json!({"thread":{"id":"fixture-recovery-thread","turns":[]}})
            }
            "turn/start" if behavior == "boot" => {
                fs::write(home.join("fixture-turn-request"), request.to_string()).unwrap();
                json!({"turn":{"id":"fixture-recovery-turn","status":"inProgress"}})
            }
            _ => panic!("unexpected account fixture method {method}"),
        };
        writeln!(output, "{}", json!({"id":id,"result":result})).unwrap();
        output.flush().unwrap();
    }
}

struct Fixture {
    state: AppState,
}

impl Fixture {
    fn new(behavior: &'static str) -> Self {
        Self::install(crate::tests::test_app_state(), behavior)
    }

    fn install(mut state: AppState, behavior: &'static str) -> Self {
        state.agent_runtime_spawning_enabled = true;
        state.set_local_http_base_url("http://127.0.0.1:8787".into());
        TEST_CODEX_APP_SERVER_FIXTURES.lock().unwrap().insert(
            state.default_workdir.clone(),
            Arc::new(move || {
                let mut command = Command::new(std::env::current_exe().unwrap());
                command
                    .args([
                        "--exact",
                        "codex_account::tests::disposable_account_app_server",
                        "--nocapture",
                    ])
                    .env(FIXTURE, behavior);
                command
            }),
        );
        Self { state }
    }

    fn home(&self, profile: SharedCodexProfile) -> PathBuf {
        codex_profile_home(&self.state, profile)
    }

    async fn request(&self, action: Value) -> (StatusCode, Value) {
        self.request_at(action, "127.0.0.1:8787", "http://127.0.0.1:8787")
            .await
    }

    async fn request_at(&self, action: Value, host: &str, origin: &str) -> (StatusCode, Value) {
        let response = app_router(self.state.clone())
            .oneshot(
                Request::post("/api/codex/account")
                    .header("host", host)
                    .header("origin", origin)
                    .header("content-type", "application/json")
                    .body(Body::from(action.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            response.status().is_client_error()
                || response.headers()["cache-control"] == "no-store"
        );
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn settled(&self) -> Value {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let (status, body) = self.request(json!({"action":"status"})).await;
                assert_eq!(status, StatusCode::OK);
                assert!(!body.to_string().contains(SECRET_SENTINEL));
                if body["status"]["phase"] != "pending" {
                    return body;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("fixture account action should settle")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        TEST_CODEX_APP_SERVER_FIXTURES
            .lock()
            .unwrap()
            .remove(&self.state.default_workdir);
    }
}

#[tokio::test]
async fn codex_account_legacy_logout_and_foreign_origins_do_not_call_provider() {
    let fixture = Fixture::new("complete");
    let home = fixture.home(SharedCodexProfile::Default);
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("auth.json"), b"disposable legacy seed").unwrap();
    let (status, body) = fixture.request(json!({"action":"logout"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.to_string().contains("Use TermAl /login once"));
    assert!(!home.join("fixture-logout-observed").exists());
    let (status, invalid) = fixture.request(json!({"action":"nonexistent"})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(invalid["error"], "Invalid Codex account request.");

    for (host, origin) in [
        ("127.0.0.1:8787", "https://foreign.example"),
        ("remote.example:8787", "http://remote.example:8787"),
    ] {
        let (status, _) = fixture
            .request_at(json!({"action":"login"}), host, origin)
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    assert_eq!(
        operation::load_account_mode(&fixture.state).unwrap(),
        AccountMode::Standalone
    );
}

#[tokio::test]
async fn codex_account_login_refuses_aliases_to_the_standalone_store() {
    let fixture = Fixture::new("complete");
    let default = fixture.home(SharedCodexProfile::Default);
    let standalone = PathBuf::from(&fixture.state.default_workdir).join("standalone-codex");
    for home in [&default, &standalone] {
        fs::create_dir_all(home).unwrap();
    }
    fs::write(standalone.join("auth.json"), b"disposable standalone grant").unwrap();

    *fixture.state.codex_account.standalone_home.lock().unwrap() = Some(default.clone());
    let (status, body) = fixture.request(json!({"action":"login"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.to_string().contains("homes must be separate"));
    assert!(!default.join("auth.json").exists());

    fs::hard_link(standalone.join("auth.json"), default.join("auth.json")).unwrap();
    *fixture.state.codex_account.standalone_home.lock().unwrap() = Some(standalone);
    let (status, body) = fixture.request(json!({"action":"login"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.to_string().contains("credential entry is linked"));
    assert!(!default.join("fixture-login-observed").exists());
    assert_eq!(
        operation::load_account_mode(&fixture.state).unwrap(),
        AccountMode::Standalone
    );
}

#[cfg(unix)]
#[tokio::test]
async fn codex_account_login_refuses_a_standalone_link_to_the_managed_grant() {
    let fixture = Fixture::new("complete");
    let managed = fixture.home(SharedCodexProfile::Default);
    let standalone = PathBuf::from(&fixture.state.default_workdir).join("standalone-codex");
    fs::create_dir_all(&managed).unwrap();
    fs::create_dir_all(&standalone).unwrap();
    fs::write(managed.join("auth.json"), FIXTURE_GRANT).unwrap();
    std::os::unix::fs::symlink(managed.join("auth.json"), standalone.join("auth.json")).unwrap();
    *fixture.state.codex_account.standalone_home.lock().unwrap() = Some(standalone);
    let (status, body) = fixture.request(json!({"action":"login"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.to_string().contains("points into a managed home"));
    assert!(!managed.join("fixture-login-observed").exists());
    assert_eq!(fs::read(managed.join("auth.json")).unwrap(), FIXTURE_GRANT);
}

#[tokio::test]
async fn codex_account_login_refuses_dangling_and_managed_home_links() {
    let fixture = Fixture::new("complete");
    let managed = fixture.home(SharedCodexProfile::Default);
    let standalone = PathBuf::from(&fixture.state.default_workdir).join("standalone-codex");
    fs::create_dir_all(&managed).unwrap();
    crate::tests::fixture_link_directory(&standalone, &managed);
    fs::remove_dir(&managed).unwrap();
    *fixture.state.codex_account.standalone_home.lock().unwrap() = Some(standalone.clone());

    let (status, body) = fixture.request(json!({"action":"login"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.to_string().contains("dangling CODEX_HOME link"));
    assert!(!managed.exists());
    #[cfg(windows)]
    fs::remove_dir(&standalone).unwrap();
    #[cfg(unix)]
    fs::remove_file(&standalone).unwrap();

    let other = standalone.with_file_name("managed-link-target");
    fs::create_dir_all(&other).unwrap();
    crate::tests::fixture_link_directory(&managed, &other);
    let (status, body) = fixture.request(json!({"action":"login"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        body.to_string()
            .contains("managed Codex home traverses a link")
    );
    assert!(!other.join("fixture-login-observed").exists());
    #[cfg(windows)]
    fs::remove_dir(&managed).unwrap();
    #[cfg(unix)]
    fs::remove_file(&managed).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn codex_account_login_refuses_a_dangling_standalone_grant_chain() {
    let fixture = Fixture::new("complete");
    let managed = fixture.home(SharedCodexProfile::Default);
    let standalone = PathBuf::from(&fixture.state.default_workdir).join("standalone-codex");
    fs::create_dir_all(&standalone).unwrap();
    std::os::unix::fs::symlink("second-auth-link", standalone.join("auth.json")).unwrap();
    std::os::unix::fs::symlink(
        managed.join("auth.json"),
        standalone.join("second-auth-link"),
    )
    .unwrap();
    *fixture.state.codex_account.standalone_home.lock().unwrap() = Some(standalone);
    let (status, body) = fixture.request(json!({"action":"login"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        body.to_string()
            .contains("standalone Codex credential link could not be resolved")
    );
    assert!(!managed.exists());
}

#[tokio::test]
async fn codex_account_boot_verifies_with_saved_queue_before_dispatching_recovery() {
    assert_codex_recovery_with_saved_queue(true).await;
}

#[tokio::test]
async fn codex_account_login_releases_the_boot_workflow_held_while_signed_out() {
    assert_codex_recovery_with_saved_queue(false).await;
}

async fn assert_codex_recovery_with_saved_queue(signed_in_at_boot: bool) {
    let root = Arc::new(TestTempRoot::create("codex-account-boot"));
    let workdir = root.path().to_string_lossy().into_owned();
    let persistence_path = root.path().join("termal.sqlite");
    let (queued_session, recovering_session) = {
        let mut inner = StateInner::new();
        let queued_session = inner
            .create_session(Agent::Codex, None, workdir.clone(), None, None)
            .session
            .id;
        let queued = inner.session_mut(&queued_session).unwrap();
        queue_prompt_on_record(
            queued,
            PendingPrompt {
                id: "fixture-user-queue-id".into(),
                timestamp: stamp_now(),
                text: "A saved operator prompt".into(),
                expanded_text: None,
                source: None,
                attachments: Vec::new(),
                engram_interrupted: false,
                is_engram_retained: false,
            },
            Vec::new(),
        );
        let recovering_session = inner
            .create_session(Agent::Codex, None, workdir.clone(), None, None)
            .session
            .id;
        let recovering = inner.session_mut(&recovering_session).unwrap();
        recovering.session.status = SessionStatus::Active;
        recovering.codex_sandbox_mode = CodexSandboxMode::WorkspaceWrite;
        persist_state(&persistence_path, &inner).unwrap();
        (queued_session, recovering_session)
    };
    fs::write(
        persistence_path.with_extension("codex-account.json"),
        serde_json::to_vec(&AccountMode::Managed).unwrap(),
    )
    .unwrap();
    let state = boot_on_test_temp_root(&root, || {
        AppState::new_before_listen_with_paths(
            workdir,
            persistence_path,
            root.path().join("orchestrators.json"),
        )
        .unwrap()
    });
    let fixture = Fixture::install(state, "boot");
    for profile in SharedCodexProfile::ALL {
        fs::create_dir_all(fixture.home(profile)).unwrap();
    }
    if signed_in_at_boot {
        fs::write(
            fixture.home(SharedCodexProfile::Default).join("auth.json"),
            FIXTURE_GRANT,
        )
        .unwrap();
    }
    fixture.state.prepare_codex_account_restore();
    assert!(fixture.state.codex_account.check_dispatch().is_err());
    fixture
        .state
        .start_post_listen_boot()
        .unwrap()
        .join()
        .unwrap();
    let home = fixture.home(SharedCodexProfile::Default);
    if !signed_in_at_boot {
        assert_eq!(fixture.settled().await["status"]["phase"], "signedOut");
        assert!(!home.join("fixture-turn-request").exists());
        assert_eq!(
            fixture.request(json!({"action":"login"})).await.0,
            StatusCode::OK
        );
    }
    assert_eq!(fixture.settled().await["status"]["phase"], "signedIn");
    assert!(fixture.state.codex_account.check_dispatch().is_ok());

    let turn_request = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(bytes) = fs::read(home.join("fixture-turn-request"))
                && let Ok(request) = serde_json::from_slice::<Value>(&bytes)
            {
                break request;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the normal boot worker should dispatch the recovery opening");
    assert!(
        turn_request
            .to_string()
            .contains("[TermAl restart recovery]")
    );
    {
        let inner = fixture.state.inner.lock().unwrap();
        let queued = &inner.sessions[inner.find_session_index(&queued_session).unwrap()];
        assert_eq!(queued.queued_prompts.len(), 1);
        assert_eq!(
            queued.queued_prompts[0].pending_prompt.id,
            "fixture-user-queue-id"
        );
        assert!(!queued.orchestrator_auto_dispatch_blocked);
        let recovering = &inner.sessions[inner.find_session_index(&recovering_session).unwrap()];
        assert!(
            recovering
                .restart_recovery
                .as_ref()
                .unwrap()
                .prompt_id
                .is_some()
        );
    }
    let server = fixture
        .state
        .running_shared_codex_runtime(SharedCodexProfile::Default)
        .unwrap();
    fixture
        .state
        .handle_shared_codex_runtime_exit(&server.runtime_id, None)
        .unwrap();
    fixture.state.shutdown_persist_blocking();
}

#[cfg(windows)]
#[tokio::test]
async fn codex_account_mirror_requires_the_selected_account() {
    let fixture = Fixture::new("wrong-sibling");
    assert_eq!(
        fixture.request(json!({"action":"login"})).await.0,
        StatusCode::OK
    );
    let settled = fixture.settled().await;
    assert_eq!(settled["status"]["phase"], "unknown");
    assert!(
        settled
            .to_string()
            .contains("disagrees with the default account")
    );
    assert!(!settled.to_string().contains("fixture-wrong-user"));
    assert!(fixture.state.codex_account.check_dispatch().is_err());
}

#[tokio::test]
async fn codex_account_login_mirrors_profiles_and_logout_survives_restore() {
    let fixture = Fixture::new("complete");
    let home = fixture.home(SharedCodexProfile::Default);
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("auth.json"), b"disposable legacy seed").unwrap();
    let (status, body) = fixture.request(json!({"action":"login"})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["challenge"]["authorizationUrl"]
            .as_str()
            .unwrap()
            .contains(SECRET_SENTINEL)
    );
    assert_eq!(fixture.settled().await["status"]["phase"], "signedIn");
    assert!(!fixture.settled().await.to_string().contains(FIXTURE_USER));
    assert_eq!(
        operation::load_account_mode(&fixture.state).unwrap(),
        AccountMode::Managed
    );
    assert!(fixture.state.codex_account.check_dispatch().is_ok());
    if cfg!(windows) {
        for profile in [
            SharedCodexProfile::ReadOnlySandbox,
            SharedCodexProfile::WorkspaceWriteSandbox,
        ] {
            assert!(fixture.home(profile).join("auth.json").is_file());
            assert!(
                !fixture
                    .home(profile)
                    .join("fixture-logout-observed")
                    .exists()
            );
        }
    }
    let (status, body) = fixture.request(json!({"action":"logout"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"]["phase"], "signedOut");
    let default = fixture.home(SharedCodexProfile::Default);
    assert!(default.join("fixture-logout-observed").is_file());
    assert!(!default.join("auth.json").exists());
    if cfg!(windows) {
        // A crash or failed mirror cleanup can leave a stale derived file.
        fs::write(
            fixture
                .home(SharedCodexProfile::ReadOnlySandbox)
                .join("auth.json"),
            b"stale disposable fixture grant",
        )
        .unwrap();
    }
    let mut restarted = fixture.state.clone();
    restarted.codex_account = Arc::default();
    restarted.prepare_codex_account_restore();
    restarted.restore_codex_account();
    let restarted = Fixture { state: restarted };
    assert_eq!(restarted.settled().await["status"]["phase"], "signedOut");
    assert!(restarted.state.codex_account.check_dispatch().is_err());
    assert!(
        !restarted
            .home(SharedCodexProfile::ReadOnlySandbox)
            .join("auth.json")
            .exists()
    );
}

/// The browser challenge reaches only the client that started /login: never
/// the shared state stream, a diagnostics route or anything TermAl saves.
#[tokio::test]
async fn codex_account_login_keeps_the_challenge_out_of_shared_state_and_diagnostics() {
    let fixture = Fixture::new("complete");
    let mut stream = fixture.state.subscribe_stream_events();
    let (status, body) = fixture.request(json!({"action":"login"})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["challenge"]["authorizationUrl"]
            .as_str()
            .unwrap()
            .contains(SECRET_SENTINEL)
    );
    assert_eq!(fixture.settled().await["status"]["phase"], "signedIn");
    {
        let inner = fixture.state.inner.lock().expect("state mutex poisoned");
        fixture.state.publish_state_locked(&inner);
    }

    let mut published_states = 0;
    loop {
        match stream.try_recv() {
            Ok(event) => {
                published_states += usize::from(matches!(event, SseStreamEvent::State(_)));
                assert!(!format!("{event:?}").contains(SECRET_SENTINEL));
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(error) => panic!("state stream was not fully observed: {error}"),
        }
    }
    assert!(published_states > 0, "a state snapshot should be published");

    for route in ["/api/state", "/api/diagnostics/persistence"] {
        let response = app_router(fixture.state.clone())
            .oneshot(
                Request::get(route)
                    .header("host", "127.0.0.1:8787")
                    .header("origin", "http://127.0.0.1:8787")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{route}");
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(SECRET_SENTINEL),
            "{route}"
        );
    }

    // The state database, the saved account setting and every profile home
    // live under the fixture's own state directory.
    let persistence_path = fixture.state.persistence_path.as_ref().clone();
    let account_setting = persistence_path.with_extension("codex-account.json");
    let mut scanned = Vec::new();
    let mut pending = vec![persistence_path.parent().unwrap().to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(bytes) = fs::read(&path) {
                assert!(
                    !bytes
                        .windows(SECRET_SENTINEL.len())
                        .any(|window| window == SECRET_SENTINEL.as_bytes()),
                    "{}",
                    path.display()
                );
                scanned.push(path);
            }
        }
    }
    for required in [&persistence_path, &account_setting] {
        assert!(
            scanned.contains(required),
            "{} was not read",
            required.display()
        );
    }
}

#[tokio::test]
async fn codex_account_cancellation_fences_dispatch_and_does_not_enroll_late_completion() {
    let fixture = Fixture::new("cancel");
    let (status, body) = fixture.request(json!({"action":"login"})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(fixture.state.codex_account.check_dispatch().is_err());
    assert!(
        fixture
            .state
            .shared_codex_runtime_for(SharedCodexProfile::Default)
            .is_err()
    );
    assert_eq!(
        fixture.request(json!({"action":"logout"})).await.0,
        StatusCode::CONFLICT
    );
    let attempt_id = body["challenge"]["attemptId"].clone();
    let (status, _) = fixture
        .request(json!({"action":"cancel","attemptId":attempt_id}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fixture.settled().await["status"]["phase"], "needsLogin");
    assert_eq!(
        operation::load_account_mode(&fixture.state).unwrap(),
        AccountMode::Enrolling
    );
    assert_eq!(
        fixture.request(json!({"action":"logout"})).await.0,
        StatusCode::CONFLICT
    );
    assert!(
        !fixture
            .home(SharedCodexProfile::Default)
            .join("fixture-logout-observed")
            .exists()
    );
    assert_eq!(fixture.settled().await["status"]["canUseStandalone"], true);

    // The unconfirmed first sign-in still offers standalone Codex after a
    // restart, and returning to it reopens Codex work.
    let mut restarted = fixture.state.clone();
    restarted.codex_account = Arc::default();
    restarted.prepare_codex_account_restore();
    restarted.restore_codex_account();
    let restarted = Fixture { state: restarted };
    let status = restarted.settled().await;
    assert_eq!(status["status"]["phase"], "needsLogin");
    assert_eq!(status["status"]["canUseStandalone"], true);
    assert!(restarted.state.codex_account.check_dispatch().is_err());
    // The cancelled login still wrote its late grant into the default home.
    let late_grant = restarted
        .home(SharedCodexProfile::Default)
        .join("auth.json");
    assert!(late_grant.is_file());
    // Every other managed profile also holds a TermAl grant here.
    for profile in &operation::profiles()[1..] {
        let home = restarted.home(*profile);
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("auth.json"), FIXTURE_GRANT).unwrap();
    }
    assert_eq!(
        restarted.request(json!({"action":"useStandalone"})).await.0,
        StatusCode::OK
    );
    assert_eq!(restarted.settled().await["status"]["phase"], "standalone");
    // Standalone launches reseed from standalone Codex, which has no grant
    // here, so no unconfirmed TermAl sign-in remains for them to use.
    for profile in operation::profiles() {
        assert!(
            !restarted.home(*profile).join("auth.json").exists(),
            "{} keeps no managed grant",
            profile.codex_home_scope()
        );
    }
    assert_eq!(
        operation::load_account_mode(&restarted.state).unwrap(),
        AccountMode::Standalone
    );
    assert!(restarted.state.codex_account.check_dispatch().is_ok());
}

#[tokio::test]
async fn codex_account_reservation_refuses_prepared_local_codex_work() {
    let fixture = Fixture::new("complete");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let session_id = inner
            .create_session(
                Agent::Codex,
                Some("Fixture".into()),
                fixture.state.default_workdir.clone(),
                None,
                None,
            )
            .session
            .id;
        inner.session_mut(&session_id).unwrap().session.status = SessionStatus::Active;
    }
    let (status, body) = fixture.request(json!({"action":"login"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.to_string().contains("Local Codex work is active"));
    assert_eq!(
        operation::load_account_mode(&fixture.state).unwrap(),
        AccountMode::Standalone
    );
}

#[cfg(windows)]
#[tokio::test]
async fn codex_account_failed_mirror_stays_fenced_and_restore_repairs_it() {
    let fixture = Fixture::new("complete");
    let sibling_grant = fixture
        .home(SharedCodexProfile::ReadOnlySandbox)
        .join("auth.json");
    // Force the file replacement to fail without touching any real store.
    fs::create_dir_all(&sibling_grant).unwrap();
    assert_eq!(
        fixture.request(json!({"action":"login"})).await.0,
        StatusCode::OK
    );
    let failure = fixture.settled().await;
    assert_eq!(failure["status"]["phase"], "unknown");
    assert!(
        failure["status"]["message"]
            .as_str()
            .unwrap()
            .contains("profile synchronization")
    );
    assert!(
        fixture
            .state
            .shared_codex_runtime_for(SharedCodexProfile::ReadOnlySandbox)
            .is_err()
    );
    assert_eq!(
        operation::load_account_mode(&fixture.state).unwrap(),
        AccountMode::Managed
    );

    fs::remove_dir(&sibling_grant).unwrap();
    let mut restarted = fixture.state.clone();
    restarted.codex_account = Arc::default();
    restarted.prepare_codex_account_restore();
    restarted.restore_codex_account();
    let restarted = Fixture { state: restarted };
    assert_eq!(restarted.settled().await["status"]["phase"], "signedIn");
    assert!(sibling_grant.is_file());
}

#[tokio::test]
async fn codex_account_idle_attachment_survives_and_successor_launches_use_managed_sign_in() {
    let fixture = Fixture::new("resume-refusal");
    let standalone = PathBuf::from(&fixture.state.default_workdir).join("standalone-codex");
    fs::create_dir_all(&standalone).unwrap();
    let standalone_config = standalone.join("config.toml");
    fs::write(
        &standalone_config,
        concat!(
            "model_provider = 'openai'\n",
            "cli_auth_credentials_store = 'file'\n",
            "forced_login_method = 'chatgpt'\n",
            "model = 'original-model'\n",
            "[profiles.example]\n",
            "model_provider = 'openai'\n",
            "model = 'original-profile-model'\n",
        ),
    )
    .unwrap();
    *fixture.state.codex_account.standalone_home.lock().unwrap() = Some(standalone);
    let old = fixture
        .state
        .shared_codex_runtime_for(SharedCodexProfile::Default)
        .unwrap();
    let session_id = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let session_id = inner
            .create_session(
                Agent::Codex,
                Some("Existing session".into()),
                fixture.state.default_workdir.clone(),
                None,
                None,
            )
            .session
            .id;
        let record = inner.session_mut(&session_id).unwrap();
        record.external_session_id = Some(SAVED_THREAD.into());
        record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: old.runtime_id.clone(),
            input_tx: old.input_tx.clone(),
            process: old.process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: old.clone(),
                session_id: session_id.clone(),
            }),
        });
        session_id
    };
    assert_eq!(
        fixture.request(json!({"action":"login"})).await.0,
        StatusCode::OK
    );
    assert_eq!(fixture.settled().await["status"]["phase"], "signedIn");
    {
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session_id).unwrap()];
        assert_eq!(record.session.status, SessionStatus::Idle);
        assert!(record.session.messages.is_empty());
        assert_eq!(record.external_session_id.as_deref(), Some(SAVED_THREAD));
        assert!(matches!(record.runtime, SessionRuntime::None));
    }
    fs::write(
        &standalone_config,
        concat!(
            "model_provider = 'fixture-custom'\n",
            "cli_auth_credentials_store = 'keyring'\n",
            "forced_login_method = 'api'\n",
            "chatgpt_base_url = 'https://isolated.invalid/'\n",
            "model = 'updated-model'\n",
            "[profiles.example]\n",
            "model_provider = 'fixture-custom'\n",
            "model = 'updated-profile-model'\n",
        ),
    )
    .unwrap();
    let next = fixture
        .state
        .shared_codex_runtime_for(SharedCodexProfile::Default)
        .unwrap();
    assert_ne!(next.runtime_id, old.runtime_id);
    let owned = spawn_codex_runtime_owned_by(
        fixture.state.clone(),
        "new-delegation".into(),
        fixture.state.default_workdir.clone(),
        if cfg!(windows) {
            SharedCodexProfile::ReadOnlySandbox
        } else {
            SharedCodexProfile::Default
        },
        CodexServerOwnership::Delegation,
    )
    .unwrap();
    for runtime in [&next, &owned.shared_session.as_ref().unwrap().runtime] {
        let read = fixture
            .state
            .perform_codex_json_rpc_request_on_runtime(
                runtime,
                "account/read",
                json!({"refreshToken":false}),
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(read["account"]["type"], "chatgpt");
        assert_eq!(read["userId"], FIXTURE_USER);
        let config = fixture
            .state
            .perform_codex_json_rpc_request_on_runtime(
                runtime,
                "config/read",
                json!({"includeLayers":false}),
                Duration::from_secs(5),
            )
            .unwrap();
        let config = &config["config"];
        assert_eq!(config["model_provider"], "openai");
        assert_eq!(config["cli_auth_credentials_store"], "file");
        assert_eq!(config["forced_login_method"], "chatgpt");
        assert!(config.get("chatgpt_base_url").is_none());
        assert_eq!(config["profiles"]["example"]["model_provider"], "openai");
        if runtime.runtime_id == next.runtime_id {
            assert_eq!(config["model"], "updated-model");
            assert_eq!(
                config["profiles"]["example"]["model"],
                "updated-profile-model"
            );
        }
    }
    let response = app_router(fixture.state.clone())
        .oneshot(
            Request::post(format!("/api/sessions/{session_id}/messages"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "text": "Explicit send after changing accounts",
                        "origin": "human",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            {
                let inner = fixture.state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session_id).unwrap()];
                assert_eq!(record.external_session_id.as_deref(), Some(SAVED_THREAD));
                if record.session.status == SessionStatus::Error {
                    assert!(record.session.messages.iter().any(|message|
                        matches!(message, Message::Text { text, .. } if text.contains("no rollout found"))
                    ));
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.expect("saved thread refusal should reach the session");
    let owned_runtime_id = owned.runtime_id.clone();
    drop(owned);
    end_owned_codex_server(&fixture.state, &owned_runtime_id);
    fixture
        .state
        .handle_shared_codex_runtime_exit(&next.runtime_id, None)
        .unwrap();
}

fn managed_signed_in_control() -> CodexAccountControl {
    let control = CodexAccountControl::default();
    control.admission.lock().unwrap().view = AccountView {
        mode: AccountMode::Managed,
        phase: AccountPhase::SignedIn,
        message: "Codex is signed in across TermAl profiles.",
        can_use_standalone: false,
        notice: None,
    };
    control
}

#[test]
fn a_managed_unauthorized_adds_a_notice_without_changing_admission() {
    let control = Arc::new(managed_signed_in_control());

    control.note_managed_unauthorized();

    let view = control.view();
    assert_eq!(view.notice, Some(MANAGED_UNAUTHORIZED_NOTICE));
    assert!(matches!(view.phase, AccountPhase::SignedIn));
    assert!(
        control.check_dispatch().is_ok(),
        "the notice never fences dispatch"
    );
    assert!(
        control.admit_launch().is_ok(),
        "the notice never fences launches"
    );
    let wire = serde_json::to_value(&view).unwrap();
    assert_eq!(wire["phase"], "signedIn");
    assert_eq!(wire["notice"], MANAGED_UNAUTHORIZED_NOTICE);
}

#[test]
fn an_unauthorized_outside_a_signed_in_managed_account_adds_no_notice() {
    let standalone = CodexAccountControl::default();
    standalone.note_managed_unauthorized();
    assert_eq!(standalone.view().notice, None);
    assert!(
        serde_json::to_value(standalone.view())
            .unwrap()
            .get("notice")
            .is_none()
    );

    let during_operation = managed_signed_in_control();
    {
        let (signal, _signals) = mpsc::channel();
        during_operation.admission.lock().unwrap().operation = Some(AccountOperation {
            id: Uuid::new_v4(),
            signal,
        });
    }
    during_operation.note_managed_unauthorized();
    assert_eq!(during_operation.view().notice, None);
}

fn new_session(state: &AppState, agent: Agent) -> String {
    let mut inner = state.inner.lock().unwrap();
    let session_id = inner
        .create_session(
            agent,
            Some("Fixture".into()),
            state.default_workdir.clone(),
            None,
            None,
        )
        .session
        .id;
    state.commit_locked(&mut inner).unwrap();
    session_id
}

fn hold_account_action(control: &CodexAccountControl) -> mpsc::Receiver<CodexAccountSignal> {
    let (signal, signals) = mpsc::channel();
    control.admission.lock().unwrap().operation = Some(AccountOperation {
        id: Uuid::new_v4(),
        signal,
    });
    signals
}

#[tokio::test]
async fn a_local_codex_send_during_an_account_action_is_refused_before_it_is_saved() {
    let fixture = Fixture::new("complete");
    let session_id = new_session(&fixture.state, Agent::Codex);
    let _signals = hold_account_action(&fixture.state.codex_account);

    let response = app_router(fixture.state.clone())
        .oneshot(
            Request::post(format!("/api/sessions/{session_id}/messages"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"text":"sent while the account changes", "origin":"human"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let inner = fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session_id).unwrap()];
    assert!(record.queued_prompts.is_empty(), "nothing is saved");
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(record.session.messages.is_empty());
    assert_eq!(
        fixture
            .state
            .codex_account
            .admission
            .lock()
            .unwrap()
            .launches,
        0,
        "a refused send keeps no launch admission"
    );
}

#[tokio::test]
async fn a_lost_thread_restarts_only_when_saved_since_the_last_account_action() {
    let fixture = Fixture::new("complete");
    let before = new_session(&fixture.state, Agent::Codex);
    fixture
        .state
        .set_external_session_id(&before, "thread-before".into())
        .unwrap();
    assert!(
        fixture
            .state
            .lost_codex_thread_may_restart(&before, "thread-before")
    );

    assert_eq!(
        fixture.request(json!({"action":"login"})).await.0,
        StatusCode::OK
    );
    assert_eq!(fixture.settled().await["status"]["phase"], "signedIn");
    assert!(
        !fixture
            .state
            .lost_codex_thread_may_restart(&before, "thread-before"),
        "a thread saved before the account action is never silently replaced"
    );
    // Saving the same thread again, as a resume does, keeps its generation.
    fixture
        .state
        .set_external_session_id(&before, "thread-before".into())
        .unwrap();
    assert!(
        !fixture
            .state
            .lost_codex_thread_may_restart(&before, "thread-before")
    );
    let after = new_session(&fixture.state, Agent::Codex);
    fixture
        .state
        .set_external_session_id(&after, "thread-after".into())
        .unwrap();
    assert!(
        fixture
            .state
            .lost_codex_thread_may_restart(&after, "thread-after")
    );

    // The account generation survives a restart.
    let mut restarted = fixture.state.clone();
    restarted.codex_account = Arc::default();
    restarted.prepare_codex_account_restore();
    restarted.restore_codex_account();
    let restarted = Fixture { state: restarted };
    assert_eq!(restarted.settled().await["status"]["phase"], "signedIn");
    assert!(
        restarted
            .state
            .lost_codex_thread_may_restart(&after, "thread-after")
    );
    assert!(
        !restarted
            .state
            .lost_codex_thread_may_restart(&before, "thread-before")
    );
}

#[test]
fn an_account_action_cannot_begin_while_an_accepted_codex_send_is_starting() {
    let state = crate::tests::test_app_state();
    let session_id = new_session(&state, Agent::Codex);
    let refused = Arc::new(Mutex::new(None));
    let (observer, recorded) = (state.clone(), refused.clone());
    TEST_HUMAN_BEFORE_PROMOTION.with(|slot| {
        slot.replace(Some(Box::new(move || {
            let (signal, _signals) = mpsc::channel();
            let begun = observer.begin_codex_account_operation(
                AccountOperation {
                    id: Uuid::new_v4(),
                    signal,
                },
                false,
            );
            *recorded.lock().unwrap() = Some(begun.is_err());
        })));
    });

    let _ = state.dispatch_human_turn(
        &session_id,
        SendMessageRequest {
            text: "accepted before any account action".to_owned(),
            expanded_text: None,
            attachments: Vec::new(),
            source_session_id: None,
            source_mailbox: None,
        },
    );

    assert_eq!(
        *refused.lock().unwrap(),
        Some(true),
        "the accepted send held the launch admission through its start"
    );
    assert!(
        state
            .codex_account
            .admission
            .lock()
            .unwrap()
            .operation
            .is_none()
    );
}

#[test]
fn a_thread_account_generation_survives_persistence_and_is_absent_when_unset() {
    let state = crate::tests::test_app_state();
    let session_id = new_session(&state, Agent::Codex);
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&session_id).unwrap();
    let record = inner.session_mut_by_index(index).unwrap();
    set_record_external_session_id(record, Some("thread-persisted".into()));

    let unset = serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap();
    assert!(unset.get("codexThreadAccountGeneration").is_none());

    record.codex_thread_account_generation = Some(7);
    let saved = serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap();
    assert_eq!(saved["codexThreadAccountGeneration"], 7);
    let restored = serde_json::from_value::<PersistedSessionRecord>(saved)
        .unwrap()
        .into_record()
        .unwrap();
    assert_eq!(restored.codex_thread_account_generation, Some(7));
    let legacy = serde_json::from_value::<PersistedSessionRecord>(unset)
        .unwrap()
        .into_record()
        .unwrap();
    assert_eq!(legacy.codex_thread_account_generation, None);
}

#[test]
fn a_saved_account_setting_loads_from_a_bare_mode_or_with_its_generation() {
    let state = crate::tests::test_app_state();
    let path = state.persistence_path.with_extension("codex-account.json");

    fs::write(&path, serde_json::to_vec(&AccountMode::Managed).unwrap()).unwrap();
    let legacy = operation::load_saved_account(&state).unwrap();
    assert_eq!(legacy.mode, AccountMode::Managed);
    assert_eq!(legacy.generation, 0);

    fs::write(&path, br#"{"mode":"enrolling","generation":5}"#).unwrap();
    let saved = operation::load_saved_account(&state).unwrap();
    assert_eq!(saved.mode, AccountMode::Enrolling);
    assert_eq!(saved.generation, 5);

    // An unreadable setting never reuses a generation a saved thread records.
    {
        let session_id = new_session(&state, Agent::Codex);
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session_id).unwrap();
        inner
            .session_mut_by_index(index)
            .unwrap()
            .codex_thread_account_generation = Some(9);
    }
    fs::write(&path, b"not a setting").unwrap();
    state.prepare_codex_account_restore();
    assert_eq!(state.codex_account.account_generation(), 10);
}
