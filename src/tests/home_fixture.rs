//! Home-dependent fixtures run in one exact libtest child. The suite parent
//! never redirects its environment, and owns the home until that child and its
//! explicitly joined consumers finish. This is fixture isolation, not a new
//! production home resolver or a suite-wide serialization lock.
use super::*;

const CHILD: &str = "TERMAL_TEST_ISOLATED_HOME_CASE";

/// Joining the real persistence worker is required on assertion unwind too,
/// not just on the happy-path tail of a bootstrap test body.
pub(super) struct BootState(AppState);

impl BootState {
    pub(super) fn new(state: AppState) -> Self {
        Self(state)
    }
}

impl std::ops::Deref for BootState {
    type Target = AppState;
    fn deref(&self) -> &AppState {
        &self.0
    }
}

impl Drop for BootState {
    fn drop(&mut self) {
        self.0.shutdown_persist_blocking();
    }
}

#[derive(Serialize, Deserialize)]
struct ChildIdentity {
    case: String,
    nonce: String,
    executable: PathBuf,
    home: PathBuf,
}

fn completion(identity: &ChildIdentity) -> String {
    format!(
        "isolated-home-complete:{}:{}",
        identity.case, identity.nonce
    )
}

fn child_identity(case: &str) -> Option<ChildIdentity> {
    let raw = std::env::var_os(CHILD)?;
    let identity: ChildIdentity =
        serde_json::from_str(&raw.to_string_lossy()).expect("isolated-home identity must parse");
    assert_eq!(identity.case, case, "a child cannot delegate another case");
    assert_eq!(std::thread::current().name(), Some(case));
    assert_eq!(identity.executable, std::env::current_exe().unwrap());
    assert_eq!(
        std::env::var_os("HOME"),
        Some(identity.home.clone().into_os_string())
    );
    assert_eq!(
        std::env::var_os("USERPROFILE"),
        Some(identity.home.clone().into_os_string())
    );
    assert!(!identity.nonce.is_empty());
    // Descendants must not mistake an inherited marker for permission to run
    // another libtest case. All home/config variables were set before startup.
    unsafe {
        std::env::remove_var(CHILD);
    }
    Some(identity)
}

/// The body closure drops its states and joins fixture-owned workers before
/// returning. Only that normal return produces completion proof; a panic,
/// process exit, zero-selected case or early exit cannot report success.
pub(super) fn run(case: &str, body: impl FnOnce(&FsPath)) {
    if let Some(identity) = child_identity(case) {
        body(&identity.home);
        eprintln!("{}", completion(&identity));
        return;
    }
    run_parent(case, |_, _| {}, |_, _| {}).unwrap_or_else(|error| panic!("{error}"));
}

/// After a consumer can start, lexical-scope unwind cannot prove settlement.
/// Only verified normal completion is allowed to consume this root's guard.
struct UnprovenHomeRoot(Option<TestTempRoot>);

impl Drop for UnprovenHomeRoot {
    fn drop(&mut self) {
        if let Some(root) = self.0.take() {
            eprintln!("retaining unproven consumer home at {}", root.path().display());
            std::mem::forget(root);
        }
    }
}

fn run_parent(
    case: &str,
    configure: impl FnOnce(&mut Command, &FsPath),
    while_running: impl FnOnce(&FsPath, &FsPath),
) -> Result<(), String> {
    let root = TestTempRoot::create("termal-isolated-home");
    let path = root.path().to_owned();
    let home = path.join("user-home");
    fs::create_dir_all(&home).unwrap();
    let identity = ChildIdentity {
        case: case.to_owned(),
        nonce: Uuid::new_v4().to_string(),
        executable: std::env::current_exe().unwrap(),
        home: home.clone(),
    };
    let mut command = Command::new(&identity.executable);
    command
        .args(["--exact", case, "--test-threads=1", "--nocapture"])
        .env(CHILD, serde_json::to_string(&identity).unwrap())
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("APPDATA", home.join("AppData/Roaming"))
        .env("LOCALAPPDATA", home.join("AppData/Local"))
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("CODEX_HOME", home.join(".codex"))
        .env(
            "GEMINI_CLI_SYSTEM_SETTINGS_PATH",
            home.join("no-system-settings.json"),
        );
    for key in [
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "GOOGLE_CLOUD_PROJECT",
        "GOOGLE_CLOUD_LOCATION",
        "GOOGLE_GENAI_USE_VERTEXAI",
        "GOOGLE_GENAI_USE_GCA",
    ] {
        command.env_remove(key);
    }
    // Inherit the launcher's TEMP and TERMAL_TEST_* accounting roots unchanged.
    // No env_clear: platform executable lookup and the run ownership survive.
    let cleaned = root.observe_cleanup();
    // configure may itself start a separately owned consumer. Retention must
    // precede that boundary, not just the libtest child's spawn.
    let mut root = UnprovenHomeRoot(Some(root));
    configure(&mut command, &home);
    let child = phase_sync::CapturedStderrProcess::spawn(&mut command);
    let coordination = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        while_running(&path, &home);
    }));
    // Even a parent assertion failure releases its barrier and then lets the
    // child's resource owners settle before removing their home. A deadlocked
    // child is a failed fixture, not proof that descendants have settled.
    let (status, bytes) =
        match child.wait_with_limit(phase_sync::DEADLOCK_GUARD, "isolated home fixture") {
            Ok(result) => result,
            Err(error) => {
                return Err(format!(
                    "{error}; retaining unproven consumer home at {}",
                    path.display()
                ));
            }
        };
    let stderr = String::from_utf8_lossy(&bytes);
    if !status.success() {
        return Err(format!("isolated case {case} failed ({status}): {stderr}; retaining unproven consumer home at {}", path.display()));
    }
    let proof = completion(&identity);
    if stderr.lines().filter(|line| *line == proof).count() != 1 {
        return Err(format!(
            "isolated case {case} lacks unique completion proof: {stderr}; retaining unproven consumer home at {}",
            path.display()
        ));
    }
    // Clean exit plus the body/resource-completion proof permits cleanup.
    // A parent coordination failure is still reported, after this settlement.
    drop(root.0.take());
    cleaned
        .recv_timeout(phase_sync::DEADLOCK_GUARD)
        .map_err(|error| format!("home cleanup receipt missing: {error}"))??;
    if path.exists() || TestTempRoot::owner_marker(&path).exists() {
        return Err(format!(
            "home fixture cleanup incomplete: {}",
            path.display()
        ));
    }
    if coordination.is_err() {
        return Err(format!(
            "isolated case {case}: parent coordination panicked; child settled: {stderr}"
        ));
    }
    Ok(())
}

fn environment_snapshot() -> Vec<(&'static str, Option<std::ffi::OsString>)> {
    [
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "CODEX_HOME",
        "GEMINI_CLI_SYSTEM_SETTINGS_PATH",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "GOOGLE_CLOUD_PROJECT",
        "GOOGLE_CLOUD_LOCATION",
        "GOOGLE_GENAI_USE_VERTEXAI",
        "GOOGLE_GENAI_USE_GCA",
        CHILD,
    ]
    .into_iter()
    .map(|key| (key, std::env::var_os(key)))
    .collect()
}

#[cfg(windows)]
struct OwnedHomeConsumer {
    process: std::os::windows::io::OwnedHandle,
    root: PathBuf,
    release: mpsc::Sender<()>,
    supervisor: Option<std::thread::JoinHandle<Result<(), String>>>,
}

#[cfg(windows)]
impl OwnedHomeConsumer {
    fn is_live(&self) -> bool {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) == WAIT_TIMEOUT }
    }

    fn has_settled(&self) -> bool {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) == WAIT_OBJECT_0 }
    }

    fn start(home: &FsPath) -> Self {
        use std::os::windows::io::{AsRawHandle, BorrowedHandle};
        let root = home.parent().unwrap().to_owned();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = listener.local_addr().unwrap();
        let nonce = Uuid::new_v4().to_string();
        let mut command = Command::new("powershell.exe");
        command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &format!(
            "$c=[Net.Sockets.TcpClient]::new(); try {{ $a=$c.BeginConnect('127.0.0.1',{},$null,$null); if(-not $a.AsyncWaitHandle.WaitOne(60000)){{throw 'witness connect timed out'}}; $c.EndConnect($a); $s=$c.GetStream(); $s.ReadTimeout=120000; $s.WriteTimeout=60000; if(-not [IO.Directory]::Exists($env:HOME)){{throw 'witness HOME absent'}}; if($env:HOME -ne $env:USERPROFILE){{throw 'witness home mismatch'}}; $b=[Text.Encoding]::UTF8.GetBytes($env:TERMAL_TEST_HOME_EXIT_NONCE+'|'+$PID+'|'+$env:HOME+[char]10); $s.Write($b,0,$b.Length); if($s.ReadByte() -ne 2){{throw 'missing owned release'}} }} finally {{ $c.Dispose() }}",
            endpoint.port())])
            .env("HOME", home).env("USERPROFILE", home)
            .env("APPDATA", home.join("AppData/Roaming"))
            .env("LOCALAPPDATA", home.join("AppData/Local"))
            .env("TERMAL_TEST_HOME_EXIT_NONCE", &nonce)
            .stdout(Stdio::null()).stderr(Stdio::null()).stdin(Stdio::null());
        // Started by configure BEFORE run_parent creates its capture pipe.
        // This sibling owns no inner child's stderr writer or drain thread.
        let mut child = command.spawn().unwrap();
        let process = unsafe { BorrowedHandle::borrow_raw(child.as_raw_handle()) }
            .try_clone_to_owned().unwrap();
        let pid = child.id();
        eprintln!("HOME supervisor spawned nonce={nonce} pid={pid} root={}", root.display());
        let expected_home = home.to_string_lossy().into_owned();
        let (release, release_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let supervisor = std::thread::spawn(move || {
            let deadline = Instant::now() + phase_sync::DEADLOCK_GUARD;
            let mut peer = None;
            let handshake = (|| -> Result<(), String> {
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => { peer = Some(stream); break; }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            if Instant::now() >= deadline { return Err("witness accept timed out".into()); }
                            std::thread::park_timeout(Duration::from_millis(1));
                        }
                        Err(error) => return Err(format!("witness accept: {error}")),
                    }
                }
                let stream = peer.as_mut().unwrap();
                stream.set_nonblocking(false).map_err(|e| e.to_string())?;
                stream.set_read_timeout(Some(phase_sync::DEADLOCK_GUARD)).map_err(|e| e.to_string())?;
                stream.set_write_timeout(Some(phase_sync::DEADLOCK_GUARD)).map_err(|e| e.to_string())?;
                let mut line = String::new();
                std::io::BufRead::read_line(&mut std::io::BufReader::new(stream), &mut line)
                    .map_err(|e| format!("witness handshake: {e}"))?;
                let expected = format!("{nonce}|{pid}|{expected_home}");
                if line.trim_end() != expected { return Err(format!("witness identity mismatch: {line:?}")); }
                if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                    return Err("witness consumer exited before readiness".into());
                }
                eprintln!("HOME supervisor ready nonce={nonce} pid={pid}");
                Ok(())
            })();
            let _ = ready_tx.send(handshake.clone());
            // Independent of the thread in run_parent and its stderr join.
            // This is failure containment, never an elapsed-time success oracle.
            let watchdog = handshake.is_ok()
                && release_rx.recv_timeout(phase_sync::DEADLOCK_GUARD * 2).is_err();
            if let Some(mut stream) = peer {
                let _ = stream.write_all(&[2]);
                let _ = stream.shutdown(std::net::Shutdown::Write);
            }
            let exit_deadline = Instant::now() + phase_sync::DEADLOCK_GUARD;
            let status = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break status,
                    Ok(None) if Instant::now() < exit_deadline => std::thread::park_timeout(Duration::from_millis(1)),
                    Ok(None) => return Err(format!("witness consumer pid={pid} did not settle; retain root")),
                    Err(error) => return Err(format!("witness consumer wait: {error}; retain root")),
                }
            };
            eprintln!("HOME supervisor settled nonce={nonce} pid={pid} status={status} watchdog={watchdog}");
            handshake?;
            if watchdog { return Err("witness release watchdog timed out".into()); }
            if !status.success() { return Err(format!("witness consumer failed: {status}")); }
            Ok(())
        });
        let owner = Self { process, root, release, supervisor: Some(supervisor) };
        let ready = ready_rx.recv_timeout(phase_sync::DEADLOCK_GUARD)
            .expect("witness supervisor readiness timed out");
        ready.unwrap();
        owner
    }

    fn finish(mut self, child_settled: bool) -> Result<(), String> {
        let _ = self.release.send(());
        let result = self.supervisor.take().unwrap().join()
            .map_err(|_| "witness supervisor panicked".to_owned())?;
        // run_parent has returned and relinquished its root owner. Never create
        // a competing guard while it is running. The process handle, not EOF,
        // establishes consumer settlement even when its own diagnostic failed.
        if child_settled && self.has_settled() {
            drop(TestTempRoot::own(self.root.clone()));
        } else {
            eprintln!("unproven HOME ownership retained at {}", self.root.display());
        }
        result
    }
}

#[cfg(windows)]
impl Drop for OwnedHomeConsumer {
    fn drop(&mut self) {
        if let Some(supervisor) = self.supervisor.take() {
            let _ = self.release.send(());
            let _ = supervisor.join();
            // An unwind before run_parent returned proves no child/root-owner
            // settlement. Report and retain instead of cleaning by scope exit.
            eprintln!("HOME owner unwound before settlement; retain {}", self.root.display());
        }
    }
}

#[cfg(windows)]
#[test]
fn home_fixture_abrupt_child_retains_root_until_live_consumer_settles() {
    const CASE: &str = "tests::home_fixture::home_fixture_abrupt_child_retains_root_until_live_consumer_settles";
    if std::env::var_os(CHILD).is_some() {
        run(CASE, |_| {
            std::process::exit(0);
        });
        return;
    }
    let before = environment_snapshot();
    let owner = std::cell::RefCell::new(None);
    let error = run_parent(
        CASE,
        |_, home| {
            *owner.borrow_mut() = Some(OwnedHomeConsumer::start(home));
        },
        |_, _| assert!(owner.borrow().as_ref().unwrap().is_live()),
    ).unwrap_err();
    let owner = owner.into_inner().unwrap();
    let child_settled = error.contains("completion proof");
    let root = owner.root.clone();
    let assertions = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert!(owner.is_live(), "consumer remains independently parked after libtest exit");
        assert!(owner.root.is_dir(), "unproven live consumer root must be retained");
        assert!(TestTempRoot::owner_marker(&owner.root).is_file(), "unproven live consumer owner marker must be retained");
        assert!(child_settled && error.contains(&owner.root.display().to_string()), "{error}");
    }));
    owner.finish(child_settled).unwrap();
    if let Err(payload) = assertions { std::panic::resume_unwind(payload); }
    assert!(!root.exists());
    assert!(!TestTempRoot::owner_marker(&root).exists());
    assert_eq!(environment_snapshot(), before);
}

const PROBE: &str = "tests::home_fixture::home_fixture_completion_probe";

#[test]
fn home_fixture_completion_probe() {
    let Ok(mode) = std::env::var("TERMAL_TEST_HOME_PROBE") else {
        return;
    };
    if mode == "missing" {
        return;
    }
    if mode == "exit" {
        std::process::exit(0);
    }
    let duplicate_proof = (mode == "duplicate").then(|| {
        let identity: ChildIdentity = serde_json::from_str(&std::env::var(CHILD).unwrap()).unwrap();
        completion(&identity)
    });
    run(PROBE, |home| {
        assert!(home.is_dir());
        assert!(std::env::var_os(CHILD).is_none());
        if mode == "panic" {
            panic!("intentional isolated body failure");
        }
    });
    if let Some(proof) = duplicate_proof { eprintln!("{proof}"); }
}

/// These exact diagnostic bodies create no state, worker or descendant. Once
/// their captured child has positively exited, that independent ownership fact
/// permits explicit cleanup of the otherwise correctly retained negative root.
fn settled_consumer_free_probe_error(
    case: &str,
    configure: impl FnOnce(&mut Command, &FsPath),
) -> String {
    let mut retained = None;
    let error = run_parent(case, configure, |root, _| retained = Some(root.to_owned())).unwrap_err();
    assert!(error.contains("lacks unique completion proof") || error.contains(" failed ("), "not a settled diagnostic child: {error}");
    let root = retained.unwrap();
    assert!(root.is_dir(), "negative probe must retain its unproven root");
    assert!(TestTempRoot::owner_marker(&root).is_file());
    drop(TestTempRoot::own(root));
    error
}

#[test]
fn home_fixture_requires_body_completion_and_preserves_parent_environment() {
    let before = environment_snapshot();
    run_parent(
        PROBE,
        |command, _| {
            command.env("TERMAL_TEST_HOME_PROBE", "success");
        },
        |_, _| {},
    )
    .unwrap();
    assert_eq!(environment_snapshot(), before);
    let error = run_parent(
        PROBE,
        |command, _| {
            command.env("TERMAL_TEST_HOME_PROBE", "success");
        },
        |_, _| {
            panic!("intentional parent coordination failure");
        },
    )
    .unwrap_err();
    assert!(
        error.contains("parent coordination panicked; child settled"),
        "{error}"
    );
    assert_eq!(environment_snapshot(), before);
    for mode in ["panic", "missing", "exit", "duplicate"] {
        let error = settled_consumer_free_probe_error(
            PROBE,
            |command, _| {
                command.env("TERMAL_TEST_HOME_PROBE", mode);
            },
        );
        assert!(
            error.contains(if mode == "panic" {
                "intentional isolated body failure"
            } else {
                "completion proof"
            }),
            "{error}"
        );
        assert_eq!(environment_snapshot(), before);
    }
    let error = settled_consumer_free_probe_error(
        "tests::home_fixture::nonexistent_case",
        |_, _| {},
    );
    assert!(
        error.contains("completion proof"),
        "zero-selected case must fail: {error}"
    );
    assert_eq!(environment_snapshot(), before);
    let error = settled_consumer_free_probe_error(
        PROBE,
        |command, _| {
            let raw = command
                .get_envs()
                .find(|(key, _)| *key == CHILD)
                .unwrap()
                .1
                .unwrap();
            let mut identity: ChildIdentity = serde_json::from_str(&raw.to_string_lossy()).unwrap();
            identity.case = "another-exact-case".to_owned();
            command
                .env(CHILD, serde_json::to_string(&identity).unwrap())
                .env("TERMAL_TEST_HOME_PROBE", "success");
        },
    );
    assert!(
        error.contains("a child cannot delegate another case"),
        "{error}"
    );
    assert_eq!(environment_snapshot(), before);
}

// The counterfactual below retains the old failure mechanism in an isolated
// diagnostic only. It is not an attribution of the historical leftover to a PID.
#[cfg(windows)]
#[test]
fn home_fixture_legacy_captured_environment_recreates_home_after_cleanup() {
    const CASE: &str = "tests::home_fixture::home_fixture_legacy_captured_environment_recreates_home_after_cleanup";
    const DIAGNOSTIC: &str = "TERMAL_TEST_HOME_INHERITANCE_DIAGNOSTIC";
    if std::env::var(DIAGNOSTIC).as_deref() != Ok(CASE) {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", CASE, "--test-threads=1", "--nocapture"])
            .env(DIAGNOSTIC, CASE);
        let (status, stderr) = phase_sync::CapturedStderrProcess::spawn(&mut command)
            .wait_with_stderr("isolated legacy home inheritance witness");
        assert!(status.success(), "legacy home witness failed: {stderr}");
        assert!(
            stderr.contains("home-inheritance-witness-complete"),
            "body did not complete: {stderr}"
        );
        return;
    }
    unsafe {
        std::env::remove_var(DIAGNOSTIC);
    }
    let fixture = TestTempRoot::create("termal-home-inheritance");
    let fixture_path = fixture.path().to_owned();
    let redirected_home = fixture_path.join("user-home");
    let redirected = (
        ScopedEnvVar::set_path("HOME", &redirected_home),
        ScopedEnvVar::set_path("USERPROFILE", &redirected_home),
    );
    // A command/environment shim fixes the inherited snapshot at the same
    // boundary as child creation. Startup is deliberately released only after
    // restoration and cleanup: no sleep, scheduler race or explicit mkdir.
    let mut powershell = Command::new("powershell.exe");
    powershell
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::WriteLine('powershell-home-startup-ran')",
        ])
        .env("HOME", std::env::var_os("HOME").unwrap())
        .env("USERPROFILE", std::env::var_os("USERPROFILE").unwrap());
    eprintln!("redirected-home-captured");
    drop(redirected);
    drop(fixture);
    assert!(
        !fixture_path.exists(),
        "root must be absent before delayed startup"
    );
    // Own any recreated contents even when the regression assertion panics.
    let _recreated_cleanup = TestTempRoot::own(fixture_path.clone());
    let output = powershell.output().expect("PowerShell startup should run");
    assert!(output.status.success(), "startup failed: {output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("powershell-home-startup-ran"));
    assert!(
        fixture_path.join("user-home/AppData/Roaming").is_dir(),
        "the legacy counterfactual must really reproduce home creation"
    );
    eprintln!("home-inheritance-witness-complete");
}

#[cfg(windows)]
#[test]
fn home_fixture_parent_environment_does_not_reach_delayed_powershell_startup() {
    const CASE: &str = "tests::home_fixture::home_fixture_parent_environment_does_not_reach_delayed_powershell_startup";
    const ENDPOINT: &str = "TERMAL_TEST_HOME_CONSUMER_ENDPOINT";
    if std::env::var_os(CHILD).is_some() {
        run(CASE, |home| {
            let endpoint: std::net::SocketAddr = std::env::var(ENDPOINT).unwrap().parse().unwrap();
            let mut command = Command::new("powershell.exe");
            command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &format!(
                "$c=[Net.Sockets.TcpClient]::new('127.0.0.1',{}); $s=$c.GetStream(); $s.WriteByte(1); if($s.ReadByte() -ne 2){{throw 'missing release'}}; $c.Dispose(); [Console]::Error.WriteLine('owned-home-consumer-complete')",
                endpoint.port())]);
            let consumer = phase_sync::CapturedStderrProcess::spawn(&mut command);
            let (status, stderr) = consumer.wait_with_stderr("owned home consumer release");
            assert!(status.success(), "{stderr}");
            assert!(stderr.contains("owned-home-consumer-complete"));
            assert!(
                home.join("AppData/Roaming").is_dir(),
                "real startup must use the fixture home"
            );
        });
        return;
    }
    let before = environment_snapshot();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = listener.local_addr().unwrap();
    let mut captured = None;
    let mut removed_path = None;
    run_parent(
        CASE,
        |command, _| {
            command.env(ENDPOINT, endpoint.to_string());
        },
        |path, home| {
            let deadline = Instant::now() + phase_sync::DEADLOCK_GUARD;
            let mut peer = loop {
                match listener.accept() {
                    Ok((peer, _)) => break peer,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "owned consumer never reached the barrier"
                        );
                        std::thread::yield_now();
                    }
                    Err(error) => panic!("consumer barrier accept: {error}"),
                }
            };
            peer.set_nonblocking(false).unwrap();
            peer.set_read_timeout(Some(phase_sync::DEADLOCK_GUARD))
                .unwrap();
            let mut ready = [0];
            peer.read_exact(&mut ready).unwrap();
            assert_eq!(ready, [1]);
            assert!(home.join("AppData/Roaming").is_dir());
            assert!(
                path.exists(),
                "root must remain owned while the real consumer is parked"
            );
            assert!(
                TestTempRoot::owner_marker(path).is_file(),
                "the owning root must not have been dropped and recreated"
            );
            assert_eq!(environment_snapshot(), before);
            let output = Command::new("powershell.exe")
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "[Console]::WriteLine(($env:HOME + [char]0 + $env:USERPROFILE))",
                ])
                .output()
                .unwrap();
            assert!(output.status.success());
            let inherited = String::from_utf8(output.stdout).unwrap();
            let expected = format!(
                "{}\0{}",
                std::env::var("HOME").unwrap_or_default(),
                std::env::var("USERPROFILE").unwrap_or_default()
            );
            assert_eq!(
                inherited.trim_end_matches(['\r', '\n']),
                expected,
                "an unrelated concurrently started child must inherit the parent home"
            );
            // Capture an unrelated command's inherited environment while the home
            // consumer is alive, but release its startup only after fixture cleanup.
            let mut unrelated = Command::new("powershell.exe");
            unrelated.args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Console]::WriteLine('unrelated-home-startup-ran')",
            ]);
            for key in ["HOME", "USERPROFILE"] {
                match std::env::var_os(key) {
                    Some(value) => {
                        unrelated.env(key, value);
                    }
                    None => {
                        unrelated.env_remove(key);
                    }
                }
            }
            captured = Some(unrelated);
            removed_path = Some(path.to_owned());
            peer.write_all(&[2]).unwrap();
        },
    )
    .unwrap();
    let path = removed_path.unwrap();
    assert!(!path.exists());
    let output = captured.unwrap().output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("unrelated-home-startup-ran"));
    assert!(
        !path.exists(),
        "unrelated startup must not recreate the completed fixture"
    );
    assert_eq!(environment_snapshot(), before);
}
