//! A Codex delegation's provider process tree ends with the delegation.
//!
//! Owns: the cross-platform behaviour test that a completed or cancelled
//! Codex delegation leaves no process descended from its provider running,
//! including a sandbox re-parented away from its launcher whose tool blocks
//! on stdin, while an unrelated root session's tool keeps running; the same
//! when the app-server exits by itself first, and, on Linux, when TermAl's
//! end of the keeper's control pipe closes as at TermAl's own exit. Also
//! that a test state without a registered fixture never launches a real
//! provider for a delegation.
//! Does not own: dedicated Claude/ACP runtime trees, which
//! `delegation_process_cleanup.rs` covers on Windows.
//!
//! The app-server is a disposable protocol producer started by the production
//! shared-Codex launch path (only executable discovery is replaced, see
//! `TEST_CODEX_APP_SERVER_FIXTURES`). On each `turn/start` it launches a
//! short-lived launcher in the turn's cwd, which starts a sandbox and exits;
//! the sandbox (in a new session on Unix) starts a tool blocked on stdin.
//! Every fixture process reports its identity over its own TCP connection,
//! so exit is observed by process handle (Windows) or by connection close
//! (Unix: only that process holds its socket). No test kills a process:
//! fixtures exit when the test closes their connection at teardown.
// The fixture serves the process-tree cases, which run on Windows and Linux.
#![cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
use super::delegation_support::*;
use super::*;
use std::io::{BufRead, Read, Write};
use std::net::{TcpListener as StdTcpListener, TcpStream};

const ROLE: &str = "TERMAL_TEST_CODEX_TREE_ROLE";
const ENDPOINT: &str = "TERMAL_TEST_CODEX_TREE_ENDPOINT";
const FIXTURE_TEST: &str = "tests::codex_delegation_process_tree::codex_delegation_tree_fixture";

fn fixture_command(role: &str, endpoint: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            FIXTURE_TEST,
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(ROLE, role)
        .env(ENDPOINT, endpoint)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Reports this process to the test and exits it when the test closes the
/// connection, which happens only at teardown.
fn report(role: &str, endpoint: &str) {
    let mut socket = TcpStream::connect(endpoint).unwrap();
    #[cfg(unix)]
    let parent = Some(std::os::unix::process::parent_id());
    #[cfg(not(unix))]
    let parent: Option<u32> = None;
    let identity = json!({
        "role": role,
        "pid": std::process::id(),
        "parent": parent,
        "cwd": fs::canonicalize(std::env::current_dir().unwrap()).unwrap(),
    });
    writeln!(socket, "{identity}").unwrap();
    std::thread::spawn(move || {
        let _ = socket.read(&mut [0]);
        std::process::exit(0);
    });
}

fn app_server(endpoint: &str) {
    report("app-server", endpoint);
    let mut stdout = std::io::stdout();
    // libtest has already written this fixture's test name without a line
    // end; end that line so the first response is a line of its own.
    writeln!(stdout).unwrap();
    stdout.flush().unwrap();
    let mut threads = 0_u64;
    let mut turns = 0_u64;
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let (Some(id), Some(method)) = (
            message.get("id").cloned(),
            message.get("method").and_then(Value::as_str),
        ) else {
            continue;
        };
        let result = match method {
            "initialize" => json!({ "userAgent": "termal-process-tree-fixture" }),
            "thread/start" | "thread/resume" => {
                threads += 1;
                json!({ "thread": { "id": format!("fixture-thread-{threads}") } })
            }
            "turn/start" => {
                turns += 1;
                let cwd = message
                    .pointer("/params/cwd")
                    .and_then(Value::as_str)
                    .expect("turn/start carries the thread cwd");
                // The launcher exits at once, so its sandbox is orphaned.
                fixture_command("launcher", endpoint)
                    .current_dir(cwd)
                    .spawn()
                    .unwrap()
                    .wait()
                    .unwrap();
                json!({ "turn": {
                    "id": format!("fixture-turn-{turns}"),
                    "status": "inProgress",
                    "items": [],
                } })
            }
            _ => json!({}),
        };
        writeln!(stdout, "{}", json!({ "id": id, "result": result })).unwrap();
        stdout.flush().unwrap();
    }
}

#[test]
fn codex_delegation_tree_fixture() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let endpoint = std::env::var(ENDPOINT).unwrap();
    match role.as_str() {
        "app-server" => app_server(&endpoint),
        "launcher" => {
            fixture_command("sandbox", &endpoint).spawn().unwrap();
        }
        "sandbox" => {
            // Like a provider sandbox: re-parented away from its launcher.
            // On Linux it also leaves its app-server's process group, which
            // only the subreaper path can follow. The tree cases run on
            // Windows and Linux only; other Unix hosts end just the server.
            #[cfg(target_os = "linux")]
            assert_ne!(unsafe { libc::setsid() }, -1);
            report("sandbox", &endpoint);
            let mut tool = fixture_command("tool", &endpoint);
            tool.stdin(Stdio::piped());
            let mut tool = tool.spawn().unwrap();
            // Hold the tool's stdin open, exactly as a stalled verifier does.
            let _stdin = tool.stdin.take();
            tool.wait().unwrap();
        }
        "tool" => {
            report("tool", &endpoint);
            let _ = std::io::stdin().read(&mut [0]);
            // Its stdin closing when the sandbox dies must not end it: only
            // ending the tree itself (or the test's teardown) may.
            loop {
                std::thread::park();
            }
        }
        other => panic!("unknown fixture role {other}"),
    }
}

struct Peer {
    socket: TcpStream,
    role: String,
    cwd: PathBuf,
    identity: Value,
    #[cfg(windows)]
    process: std::os::windows::io::OwnedHandle,
}

impl Peer {
    fn accept(listener: &StdTcpListener) -> Self {
        let guard = phase_sync::PollGuard::new();
        let socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    guard.wait("Codex tree fixture report")
                }
                Err(error) => panic!("fixture accept: {error}"),
            }
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(phase_sync::DEADLOCK_GUARD))
            .unwrap();
        let mut line = String::new();
        BufReader::new(socket.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let identity: Value = serde_json::from_str(&line).unwrap();
        #[cfg(windows)]
        let process = {
            use std::os::windows::io::FromRawHandle;
            use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
            // The fixture is parked on this open connection, so its pid
            // cannot have been reused before the handle is captured.
            let handle = unsafe {
                OpenProcess(
                    PROCESS_SYNCHRONIZE,
                    0,
                    identity["pid"].as_u64().unwrap() as u32,
                )
            };
            assert!(
                !handle.is_null(),
                "open fixture process: {}",
                std::io::Error::last_os_error()
            );
            unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle) }
        };
        Self {
            socket,
            role: identity["role"].as_str().unwrap().to_owned(),
            cwd: PathBuf::from(identity["cwd"].as_str().unwrap()),
            identity,
            #[cfg(windows)]
            process,
        }
    }

    fn is_running(&self) -> bool {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::{
                Foundation::WAIT_TIMEOUT, System::Threading::WaitForSingleObject,
            };
            unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) == WAIT_TIMEOUT }
        }
        #[cfg(not(windows))]
        {
            self.socket.set_nonblocking(true).unwrap();
            let open = matches!(
                (&self.socket).read(&mut [0]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
            );
            self.socket.set_nonblocking(false).unwrap();
            open
        }
    }

    /// Waits for this process's own exit; it never closes its connection
    /// before then, and nothing here asks it to.
    fn assert_exited(&self, context: &str) {
        #[cfg(windows)]
        let exited = {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::{
                Foundation::WAIT_OBJECT_0, System::Threading::WaitForSingleObject,
            };
            unsafe {
                WaitForSingleObject(
                    self.process.as_raw_handle(),
                    phase_sync::DEADLOCK_GUARD.as_millis() as u32,
                ) == WAIT_OBJECT_0
            }
        };
        #[cfg(not(windows))]
        let exited = matches!((&self.socket).read(&mut [0]), Ok(0));
        assert!(
            exited,
            "{context}: provider descendant survived: {}",
            self.identity
        );
    }
}

/// Every fixture process the test started, released at teardown by closing
/// its connection; reaped before the temp roots holding their cwd go away.
struct FixtureTree {
    listener: StdTcpListener,
    peers: Vec<Peer>,
    workdir_key: String,
}

impl FixtureTree {
    fn install(state: &AppState) -> Self {
        let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        let fixture: TestCodexAppServerFixture = Arc::new(move || {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    FIXTURE_TEST,
                    "--nocapture",
                    "--test-threads",
                    "1",
                ])
                .env(ROLE, "app-server")
                .env(ENDPOINT, &endpoint);
            command
        });
        let workdir_key = state.default_workdir.clone();
        TEST_CODEX_APP_SERVER_FIXTURES
            .lock()
            .unwrap()
            .insert(workdir_key.clone(), fixture);
        Self {
            listener,
            peers: Vec::new(),
            workdir_key,
        }
    }

    /// The index of the report of `role` from `cwd`, accepting reports in
    /// arrival order until it is present. Each cwd runs exactly one turn.
    fn wait_for(&mut self, role: &str, cwd: &FsPath) -> usize {
        let cwd = fs::canonicalize(cwd).unwrap();
        loop {
            if let Some(index) = self
                .peers
                .iter()
                .position(|peer| peer.role == role && peer.cwd == cwd)
            {
                return index;
            }
            let peer = Peer::accept(&self.listener);
            self.peers.push(peer);
        }
    }

    /// The index of the report of the app-server that TermAl runs as process
    /// `held`: that process itself, or the server a Linux keeper started.
    fn wait_for_server(&mut self, held: u32) -> usize {
        let held = Some(u64::from(held));
        loop {
            if let Some(index) = self.peers.iter().position(|peer| {
                peer.role == "app-server"
                    && (peer.identity["pid"].as_u64() == held
                        || peer.identity["parent"].as_u64() == held)
            }) {
                return index;
            }
            let peer = Peer::accept(&self.listener);
            self.peers.push(peer);
        }
    }

    /// The sandbox and the tool of the one turn run in `cwd`.
    fn turn_tree(&mut self, cwd: &FsPath) -> [usize; 2] {
        let sandbox = self.wait_for("sandbox", cwd);
        let tool = self.wait_for("tool", cwd);
        [sandbox, tool]
    }
}

impl Drop for FixtureTree {
    fn drop(&mut self) {
        TEST_CODEX_APP_SERVER_FIXTURES
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&self.workdir_key);
        // Reports that arrived but were never awaited are released too.
        while let Ok((socket, _)) = self.listener.accept() {
            socket.set_nonblocking(false).unwrap();
            let _ = socket.shutdown(std::net::Shutdown::Write);
            let _ = socket.set_read_timeout(Some(phase_sync::DEADLOCK_GUARD));
            let _ = std::io::copy(&mut &socket, &mut std::io::sink());
        }
        // The test signals no process: a fixture ends on its own when its
        // connection closes.
        for peer in &self.peers {
            let _ = peer.socket.shutdown(std::net::Shutdown::Write);
        }
        for peer in &self.peers {
            #[cfg(windows)]
            {
                use std::os::windows::io::AsRawHandle;
                use windows_sys::Win32::System::Threading::WaitForSingleObject;
                unsafe {
                    WaitForSingleObject(
                        peer.process.as_raw_handle(),
                        phase_sync::DEADLOCK_GUARD.as_millis() as u32,
                    );
                }
            }
            #[cfg(not(windows))]
            let _ = (&peer.socket).read(&mut [0]);
        }
    }
}

#[cfg(any(windows, target_os = "linux"))]
enum DelegationEnd {
    Completed,
    Canceled,
    /// The app-server exits by itself, as a crash or an OOM kill would end
    /// it, before anything ends the delegation.
    ServerExited,
    /// TermAl's end of the keeper's control pipe closes, as at TermAl's own
    /// exit, without the delegation being ended.
    #[cfg(target_os = "linux")]
    TermalExited,
}

#[cfg(any(windows, target_os = "linux"))]
async fn codex_delegation_tree_case(end: DelegationEnd) {
    let _directory = TestTempRootDirectoryScope::repository_local();
    let temp = TestTempRoot::create("codex-delegation-tree");
    let worktree = temp.path().join("worktree");
    fs::create_dir(&worktree).unwrap();
    let mut state = test_app_state();
    state.agent_runtime_spawning_enabled = true;
    let mut tree = FixtureTree::install(&state);

    // An unrelated root session with a tool of its own in flight, on the
    // same read-only profile as the delegation, so ending every server of
    // that profile could not pass for ending the delegation's own tree.
    let parent = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&parent).unwrap();
        inner.sessions[index].codex_sandbox_mode = CodexSandboxMode::ReadOnly;
    }
    dispatch_turn_and_snapshot(
        &state,
        &parent,
        SendMessageRequest {
            text: "Root turn".to_owned(),
            expanded_text: None,
            attachments: Vec::new(),
            source_session_id: None,
            source_mailbox: None,
        },
    )
    .unwrap();
    let [_, root_tool] = tree.turn_tree(FsPath::new(&state.default_workdir));
    let root_server = tree.wait_for_server(codex_runtime_pid(&state, &parent));

    let created = state
        .create_read_only_delegation(
            &parent,
            CreateDelegationRequest {
                prompt: "Review the fixture".to_owned(),
                title: None,
                cwd: Some(worktree.to_string_lossy().into_owned()),
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Explorer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .unwrap();
    let delegation_tree = tree.turn_tree(&worktree);
    let delegation_server = tree.wait_for_server(codex_runtime_pid(
        &state,
        &created.delegation.child_session_id,
    ));
    assert_ne!(
        delegation_server, root_server,
        "the delegation runs on an app-server of its own"
    );

    // Held while the tree is judged in the server-exit case, so that no
    // ending through the owner (on Linux, the keeper's control pipe) can
    // be what ends it.
    let mut held_owner = None;
    let expected = match end {
        DelegationEnd::Completed => {
            finish_delegation_child_with_assistant_text(
                &state,
                &created.delegation.child_session_id,
                "## Result\nStatus: completed\n\nSummary:\nReviewed the fixture.",
            );
            state
                .refresh_delegation_for_child_session(&created.delegation.child_session_id)
                .unwrap();
            Some(DelegationStatus::Completed)
        }
        DelegationEnd::Canceled => {
            let app = app_router(state.clone());
            let (status, _response): (StatusCode, DelegationStatusResponse) = request_json(
                &app,
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/sessions/{parent}/delegations/{}/cancel",
                        created.delegation.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            Some(DelegationStatus::Canceled)
        }
        DelegationEnd::ServerExited => {
            held_owner = Some(delegation_owner(
                &state,
                &created.delegation.child_session_id,
            ));
            // The fixture exits by itself once its connection closes; the
            // test signals nothing.
            tree.peers[delegation_server]
                .socket
                .shutdown(std::net::Shutdown::Write)
                .unwrap();
            None
        }
        #[cfg(target_os = "linux")]
        DelegationEnd::TermalExited => {
            delegation_owner(&state, &created.delegation.child_session_id)
                .close_control_as_if_termal_exited();
            None
        }
    };
    if let Some(expected) = expected {
        let inner = state.inner.lock().unwrap();
        let record = inner
            .delegations
            .iter()
            .find(|record| record.id == created.delegation.id)
            .unwrap();
        assert_eq!(record.status, expected);
    }
    // Every process of the delegation's tree, its own server included, must
    // have exited before the unrelated root session's processes are judged.
    for index in delegation_tree.into_iter().chain([delegation_server]) {
        tree.peers[index].assert_exited("delegation ended");
    }
    for index in [root_tool, root_server] {
        assert!(
            tree.peers[index].is_running(),
            "an unrelated root session's process must keep running: {}",
            tree.peers[index].identity
        );
    }
    drop(held_owner);
}

/// The owner of the app-server the delegation child `session_id` runs on.
#[cfg(any(windows, target_os = "linux"))]
fn delegation_owner(state: &AppState, session_id: &str) -> Arc<OwnedCodexServer> {
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(session_id).unwrap()];
    let SessionRuntime::Codex(handle) = &record.runtime else {
        panic!("the delegation child runs on Codex");
    };
    handle
        .shared_session
        .as_ref()
        .and_then(|session| session.runtime.owner.clone())
        .expect("the delegation's app-server is its own")
}

/// The process id TermAl holds for the Codex app-server `session_id` runs
/// on: the server itself, or on Linux a delegation's keeper.
#[cfg(any(windows, target_os = "linux"))]
fn codex_runtime_pid(state: &AppState, session_id: &str) -> u32 {
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(session_id).unwrap()];
    let SessionRuntime::Codex(handle) = &record.runtime else {
        panic!("{session_id} runs on Codex");
    };
    handle.process.id()
}

// Descendants are followed on Windows (job) and Linux (the keeper, a child
// subreaper); other Unix hosts end only the server itself.
#[cfg(any(windows, target_os = "linux"))]
#[tokio::test]
async fn completed_codex_delegation_leaves_no_provider_descendant_running() {
    codex_delegation_tree_case(DelegationEnd::Completed).await;
}

#[cfg(any(windows, target_os = "linux"))]
#[tokio::test]
async fn canceled_codex_delegation_leaves_no_provider_descendant_running() {
    codex_delegation_tree_case(DelegationEnd::Canceled).await;
}

#[cfg(any(windows, target_os = "linux"))]
#[tokio::test]
async fn a_delegation_server_that_exits_by_itself_leaves_no_descendant_running() {
    codex_delegation_tree_case(DelegationEnd::ServerExited).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_closed_keeper_control_pipe_leaves_no_descendant_running() {
    codex_delegation_tree_case(DelegationEnd::TermalExited).await;
}

// A state that may launch processes but has no registered fixture must never
// start the real provider executable (or seed a Codex home from the user's):
// its delegation child stays on the wire the test installed.
#[test]
fn a_spawning_test_state_without_a_fixture_keeps_delegations_on_its_installed_wire() {
    let (mut state, input_rx) = test_app_state_with_delegation_codex_runtime("installed-wire-only");
    state.agent_runtime_spawning_enabled = true;
    let parent = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent,
            CreateDelegationRequest {
                prompt: "Stay on the installed wire".to_owned(),
                title: None,
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Explorer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .unwrap();
    // The child's prompt reaches the installed wire, which no launched
    // server could receive.
    phase_sync::receive(&input_rx, "child prompt on the installed wire");
    let inner = state.inner.lock().unwrap();
    let child = &inner.sessions[inner
        .find_session_index(&created.delegation.child_session_id)
        .unwrap()];
    let SessionRuntime::Codex(handle) = &child.runtime else {
        panic!("the delegation child runs on Codex");
    };
    assert_eq!(handle.runtime_id, "installed-wire-only");
    drop(inner);
    assert!(
        state.owned_codex_servers.live_server_count() == 0,
        "no delegation-owned app-server was launched"
    );
}

// A keeper that has not finished ending its tree when the worker stops
// waiting is reported and left running, never killed: only the keeper can
// still end what a child stuck in uninterruptible sleep leaves behind. The
// stand-in keeper is the test's own process, which never exits by itself
// while its input stays open; a zero wait only skips waiting.
#[cfg(target_os = "linux")]
#[test]
fn a_keeper_still_ending_its_tree_is_left_running_not_killed() {
    let mut command = Command::new("cat");
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn().unwrap();
    let input = child.stdin.take();
    let keeper = Arc::new(shared_child::SharedChild::new(child).unwrap());

    let report = end_kept_tree_within(&keeper, Duration::ZERO).unwrap_err();
    assert!(
        format!("{report:#}").contains("left to finish"),
        "the unfinished ending is reported: {report:#}"
    );
    assert!(
        keeper.try_wait().unwrap().is_none(),
        "a keeper still ending its tree must not be killed"
    );

    // The test ends its own process by closing its input. A clean exit
    // shows that it ended at end of input, not by a signal sent earlier.
    drop(input);
    assert!(
        keeper.wait().unwrap().success(),
        "the stand-in ends at end of input, not killed"
    );
}

/// Marks a state's host as having its /proc in another PID namespace, for
/// as long as the guard lives.
#[cfg(target_os = "linux")]
struct ForeignProcfs(String);

#[cfg(target_os = "linux")]
impl ForeignProcfs {
    fn mark(state: &AppState) -> Self {
        TEST_FOREIGN_PROCFS
            .lock()
            .unwrap()
            .insert(state.default_workdir.clone());
        Self(state.default_workdir.clone())
    }
}

#[cfg(target_os = "linux")]
impl Drop for ForeignProcfs {
    fn drop(&mut self) {
        TEST_FOREIGN_PROCFS
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&self.0);
    }
}

// Where /proc is not in TermAl's own PID namespace, a delegation's tree could
// not be ended, so its server is never started: the delegation fails at once
// and its result names the reason.
#[cfg(target_os = "linux")]
#[test]
fn a_delegation_whose_tree_could_not_be_ended_fails_at_once_with_the_reason() {
    let mut state = test_app_state();
    state.agent_runtime_spawning_enabled = true;
    let _tree = FixtureTree::install(&state);
    let _foreign = ForeignProcfs::mark(&state);
    let parent = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent,
            CreateDelegationRequest {
                prompt: "Review nothing".to_owned(),
                title: None,
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Explorer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .unwrap();
    assert_eq!(created.delegation.status, DelegationStatus::Failed);
    let summary = created
        .delegation
        .result
        .as_ref()
        .map(|result| result.summary.clone())
        .unwrap_or_default();
    assert!(
        summary.contains("PID namespace") && summary.contains("process tree could not be ended"),
        "the failed delegation names the reason: {summary}"
    );
    assert!(
        state.owned_codex_servers.live_server_count() == 0,
        "no delegation-owned app-server was started"
    );
}
