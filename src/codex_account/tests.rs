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
            "config/read" => json!({"config":{"cli_auth_credentials_store":"file"}}),
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
    std::os::unix::fs::symlink(managed.join("auth.json"), standalone.join("auth.json")).unwrap();
    *fixture.state.codex_account.standalone_home.lock().unwrap() = Some(standalone);
    let (status, body) = fixture.request(json!({"action":"login"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.to_string().contains("points into a managed home"));
    assert!(!managed.join("fixture-login-observed").exists());
    assert!(!managed.join("auth.json").exists());
}

#[tokio::test]
async fn codex_account_boot_verifies_with_saved_queue_before_dispatching_recovery() {
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
    fs::write(
        fixture.home(SharedCodexProfile::Default).join("auth.json"),
        FIXTURE_GRANT,
    )
    .unwrap();
    fixture.state.prepare_codex_account_restore();
    assert!(fixture.state.codex_account.check_dispatch().is_err());
    fixture
        .state
        .start_post_listen_boot()
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(fixture.settled().await["status"]["phase"], "signedIn");
    assert!(fixture.state.codex_account.check_dispatch().is_ok());

    let home = fixture.home(SharedCodexProfile::Default);
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
