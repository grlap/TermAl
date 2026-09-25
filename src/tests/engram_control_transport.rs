//! Real-process coverage of `ProcessEngramControlTransport`: the control
//! fixture's spawn/EOF/timeout/kill/respawn lifecycle, whole-process-tree
//! termination on timeout, EOF, explicit shutdown and idle reap, and the
//! stateful fixture's idempotency and stale-grant semantics over a live child.
//!
//! Owns the descendant readiness/probe fixture that proves tree termination.
//! The doctor deadline tests in `engram_host_adapter.rs` borrow
//! `prepare_engram_control_process_tree_fixture` and
//! `assert_engram_control_descendant_was_terminated` until the doctor suite
//! is extracted. Does not own the one-shot work-binding reader test
//! (`real_process_work_binding_reader_reads_the_held_claims`, tm-81vf),
//! the live-store e2e, or `real_engram_control_fixture_path`, which stays in
//! the parent because the readiness and compaction suites import it.
//! Split out of `src/tests/engram_host_adapter.rs` as a pure code move
//! (tm-tg1g).

use super::*;

#[test]
fn real_process_fixture_covers_spawn_eof_timeout_kill_and_respawn() {
    let temp_root = TestTempRoot::create("engram-control-fixture");
    let project_file = temp_root.path().join(".engram-project");
    fs::write(&project_file, "fixture-ok\n").expect("fixture mode should write");
    let binary_path = real_engram_control_fixture_path();
    let connection = EngramConnectionConfig {
        binary_path,
        project_file: project_file.clone(),
        home: temp_root.path().to_path_buf(),
        project_root: temp_root.path().to_path_buf(),
        actor_id: "termal-fixture".to_owned(),
        actor_context: None,
        session_id: "engram-fixture-session".to_owned(),
    };
    let request = EngramControlRequest::SessionBind {
        external_ref: "termal:fixture".to_owned(),
        title: "Fixture".to_owned(),
        assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
        mediated_effects: vec![EngramEffect::Observe],
        capability_map_revision: ENGRAM_CAPABILITY_MAP_REVISION,
        work_binding: None,
        idempotency_key: "fixture-bind".to_owned(),
    };
    let transport = real_process_fixture_transport();

    let first = transport
        .request(&connection, &request, DEADLOCK_GUARD)
        .expect("fixture process should spawn and reply");
    assert_eq!(first["routing_token"], "fixture-token");
    transport.shutdown_session(&connection.session_id);

    fs::write(&project_file, "fixture-eof\n").expect("EOF mode should write");
    let eof = transport
        .request(&connection, &request, DEADLOCK_GUARD)
        .expect_err("fixture EOF should be a transport error");
    assert_eq!(eof.kind, EngramTransportErrorKind::Transport);

    fs::write(&project_file, "fixture-malformed\n").expect("malformed mode should write");
    let malformed = transport
        .request(&connection, &request, DEADLOCK_GUARD)
        .expect_err("malformed fixture output should be a protocol error");
    assert_eq!(malformed.kind, EngramTransportErrorKind::Protocol);

    fs::write(&project_file, "fixture-hang\n").expect("hang mode should write");
    // Acquire the startup handshake before the deliberate deadline. Keep the
    // exact child handle so the assertion proves termination, not just a reply.
    let hung_process = transport
        .process_for(&connection)
        .expect("hang phase should be ready");
    let hung_pid = hung_process.process.id();
    let timeout = transport
        .request(&connection, &request, Duration::from_millis(100))
        .expect_err("fixture hang should hit the deadline");
    assert_eq!(timeout.kind, EngramTransportErrorKind::Deadline);
    assert!(
        hung_process
            .process
            .try_wait()
            .expect("hung child status should be readable")
            .is_some(),
        "deadline must reap the ready child {hung_pid}"
    );

    fs::write(&project_file, "fixture-ok\n").expect("ok mode should write");
    let respawned = transport
        .request(&connection, &request, DEADLOCK_GUARD)
        .expect("timeout must kill the old process and allow a clean respawn");
    assert_eq!(respawned["routing_token"], "fixture-token");
    assert_ne!(
        transport.process_for(&connection).unwrap().process.id(),
        hung_pid,
        "respawn must use a new process"
    );
    transport.shutdown_session(&connection.session_id);
}

#[test]
fn real_process_timeout_kills_the_entire_control_process_tree() {
    let temp_root = TestTempRoot::create("engram-control-process-tree-timeout");
    let (project_file, ready) =
        prepare_engram_control_process_tree_fixture(&temp_root, "fixture-tree-hang");

    let connection = EngramConnectionConfig {
        binary_path: real_engram_control_fixture_path(),
        project_file,
        home: temp_root.path().to_path_buf(),
        project_root: temp_root.path().to_path_buf(),
        actor_id: "termal-fixture".to_owned(),
        actor_context: None,
        session_id: "engram-fixture-process-tree".to_owned(),
    };
    let request = EngramControlRequest::SessionBind {
        external_ref: "termal:fixture-tree".to_owned(),
        title: "Fixture tree".to_owned(),
        assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
        mediated_effects: vec![EngramEffect::Observe],
        capability_map_revision: ENGRAM_CAPABILITY_MAP_REVISION,
        work_binding: None,
        idempotency_key: "fixture-tree-bind".to_owned(),
    };
    let transport = Arc::new(real_process_fixture_transport());
    transport
        .process_for(&connection)
        .expect("tree startup should complete before the request deadline");
    let descendant = ready.wait();
    let request_transport = transport.clone();
    let request_connection = connection.clone();
    let request_handle = std::thread::spawn(move || {
        request_transport.request(&request_connection, &request, Duration::from_secs(5))
    });

    let timeout = request_handle
        .join()
        .expect("request thread should not panic")
        .expect_err("fixture tree should hit the request deadline");
    assert_eq!(timeout.kind, EngramTransportErrorKind::Deadline);

    assert_engram_control_descendant_was_terminated(&descendant, "a timed-out request");
    transport.shutdown_session(&connection.session_id);
}

#[test]
fn real_process_eof_kills_the_entire_control_process_tree() {
    let temp_root = TestTempRoot::create("engram-control-process-tree-eof");
    let (project_file, ready) =
        prepare_engram_control_process_tree_fixture(&temp_root, "fixture-tree-eof");
    let connection = engram_control_process_tree_connection(&temp_root, project_file, "eof");
    let transport = Arc::new(real_process_fixture_transport());
    transport
        .process_for(&connection)
        .expect("tree startup should complete before EOF");
    let descendant = ready.wait();
    let request_transport = transport.clone();
    let request_connection = connection.clone();
    let request_handle = std::thread::spawn(move || {
        request_transport.request(
            &request_connection,
            &engram_control_process_tree_request("eof"),
            DEADLOCK_GUARD,
        )
    });

    let error = request_handle
        .join()
        .expect("request thread should not panic")
        .expect_err("fixture EOF should fail the request");
    assert_eq!(error.kind, EngramTransportErrorKind::Transport);
    assert_engram_control_descendant_was_terminated(&descendant, "control EOF");
}

#[test]
fn real_process_shutdown_kills_the_entire_control_process_tree() {
    let temp_root = TestTempRoot::create("engram-control-process-tree-shutdown");
    let (project_file, ready) =
        prepare_engram_control_process_tree_fixture(&temp_root, "fixture-tree-reply");
    let connection = engram_control_process_tree_connection(&temp_root, project_file, "shutdown");
    let transport = real_process_fixture_transport();

    transport
        .request(
            &connection,
            &engram_control_process_tree_request("shutdown"),
            DEADLOCK_GUARD,
        )
        .expect("fixture should reply before explicit shutdown");
    let descendant = ready.wait();
    transport.shutdown_session(&connection.session_id);
    assert_engram_control_descendant_was_terminated(&descendant, "explicit shutdown");
}

#[test]
fn real_process_idle_reap_kills_the_entire_control_process_tree() {
    let temp_root = TestTempRoot::create("engram-control-process-tree-idle");
    let (project_file, ready) =
        prepare_engram_control_process_tree_fixture(&temp_root, "fixture-tree-reply");
    let connection = engram_control_process_tree_connection(&temp_root, project_file, "idle");
    let transport = ProcessEngramControlTransport::with_startup_handshake_and_idle_timeout(
        "termal-engram-control-fixture-ready",
        DEADLOCK_GUARD,
        Duration::from_millis(250),
    );

    transport
        .request(
            &connection,
            &engram_control_process_tree_request("idle"),
            DEADLOCK_GUARD,
        )
        .expect("fixture should reply before the idle reap");
    let descendant = ready.wait();
    assert_engram_control_descendant_was_terminated(&descendant, "the idle reap");

    fs::write(&connection.project_file, "fixture-ok\n").expect("fixture mode should reset");
    let respawned = transport
        .request(
            &connection,
            &engram_control_process_tree_request("idle-respawn"),
            DEADLOCK_GUARD,
        )
        .expect("the first request after an idle reap should respawn immediately");
    assert_eq!(respawned["routing_token"], "fixture-token");
    transport.shutdown_session(&connection.session_id);
}

pub(super) fn prepare_engram_control_process_tree_fixture(
    temp_root: &TestTempRoot,
    mode: &str,
) -> (PathBuf, EngramDescendantReady) {
    let project_file = temp_root.path().join(".engram-project");
    fs::write(&project_file, format!("{mode}\n")).expect("fixture mode should write");
    let ready = EngramDescendantReady::listen();
    let endpoint = ready.address;
    let executable = std::env::current_exe().expect("test executable should resolve");
    #[cfg(windows)]
    fs::write(
        temp_root.path().join("engram-descendant.ps1"),
        format!("$env:TERMAL_TEST_DESCENDANT_ENDPOINT = '{endpoint}'\n& '{}' --exact tests::phase_sync::parked_control_descendant --nocapture\n", executable.to_string_lossy().replace('\'', "''")),
    )
    .expect("Windows descendant fixture should write");
    #[cfg(not(windows))]
    fs::write(
        temp_root.path().join("engram-descendant.sh"),
        format!("#!/bin/sh\nexport TERMAL_TEST_DESCENDANT_ENDPOINT='{endpoint}'\nexec '{}' --exact tests::phase_sync::parked_control_descendant --nocapture\n", executable.to_string_lossy().replace('\'', "'\\''")),
    )
    .expect("Unix descendant fixture should write");
    (project_file, ready)
}

fn engram_control_process_tree_connection(
    temp_root: &TestTempRoot,
    project_file: PathBuf,
    suffix: &str,
) -> EngramConnectionConfig {
    EngramConnectionConfig {
        binary_path: real_engram_control_fixture_path(),
        project_file,
        home: temp_root.path().to_path_buf(),
        project_root: temp_root.path().to_path_buf(),
        actor_id: "termal-fixture".to_owned(),
        actor_context: None,
        session_id: format!("engram-fixture-process-tree-{suffix}"),
    }
}

fn engram_control_process_tree_request(suffix: &str) -> EngramControlRequest {
    EngramControlRequest::SessionBind {
        external_ref: format!("termal:fixture-tree-{suffix}"),
        title: "Fixture tree".to_owned(),
        assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
        mediated_effects: vec![EngramEffect::Observe],
        capability_map_revision: ENGRAM_CAPABILITY_MAP_REVISION,
        work_binding: None,
        idempotency_key: format!("fixture-tree-bind-{suffix}"),
    }
}

pub(super) struct EngramReadyProcess {
    probe: EngramDescendantProbe,
    // Kept open until after the termination assertion. Test teardown can then
    // release a surviving fixture without leaving a naturally-timed orphan.
    _lifetime: std::net::TcpStream,
}

pub(super) struct EngramDescendantReady {
    address: std::net::SocketAddr,
    _listener: std::net::TcpListener,
    receiver: mpsc::Receiver<EngramReadyProcess>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl EngramDescendantReady {
    fn listen() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let accept_listener = listener.try_clone().unwrap();
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = accept_listener.accept().expect("descendant should connect");
            stream.set_read_timeout(Some(DEADLOCK_GUARD)).unwrap();
            let mut bytes = [0; 4];
            if stream.read_exact(&mut bytes).is_err() {
                return;
            }
            let pid = u32::from_be_bytes(bytes);
            if pid == 0 {
                return;
            } // Cancellation before the fixture started.
            let probe = EngramDescendantProbe::open(pid);
            assert!(probe.is_alive(), "descendant must be alive at readiness");
            stream
                .write_all(&[1])
                .expect("acknowledge descendant readiness");
            let _ = sender.send(EngramReadyProcess {
                probe,
                _lifetime: stream,
            });
        });
        Self {
            address,
            _listener: listener,
            receiver,
            worker: Some(worker),
        }
    }

    pub(super) fn wait(self) -> EngramReadyProcess {
        receive(
            &self.receiver,
            "control descendant started and process probe acquired",
        )
    }
}

impl Drop for EngramDescendantReady {
    fn drop(&mut self) {
        // Unblock accept if startup failed before a descendant connected.
        if let Ok(mut stream) = std::net::TcpStream::connect(self.address) {
            let _ = stream.write_all(&0_u32.to_be_bytes());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(super) fn assert_engram_control_descendant_was_terminated(
    descendant: &EngramReadyProcess,
    trigger: &str,
) {
    let started = std::time::Instant::now();
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        let result = unsafe {
            WaitForSingleObject(
                descendant.probe.0.as_raw_handle(),
                DEADLOCK_GUARD.as_millis() as u32,
            )
        };
        assert_eq!(
            result,
            WAIT_OBJECT_0,
            "descendant exit after {trigger} was not published after {:?}",
            started.elapsed()
        );
        assert!(
            !descendant.probe.is_alive(),
            "descendant must be exited after {trigger}"
        );
    }
    #[cfg(not(windows))]
    {
        // Only the parked descendant owns the peer socket. EOF proves death
        // without misclassifying an orphaned Unix zombie as live via kill(0).
        let mut lifetime = &descendant._lifetime;
        let result = lifetime.read(&mut [0]);
        assert!(
            matches!(result, Ok(0)),
            "descendant socket must close after {trigger}; elapsed {:?}, result {result:?}",
            started.elapsed()
        );
    }
}

#[cfg(windows)]
struct EngramDescendantProbe(std::os::windows::io::OwnedHandle);

#[cfg(windows)]
impl EngramDescendantProbe {
    fn open(pid: u32) -> Self {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        };

        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        assert!(
            !handle.is_null(),
            "descendant must still be alive before cleanup"
        );
        Self(unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle) })
    }

    fn is_alive(&self) -> bool {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::STILL_ACTIVE;
        use windows_sys::Win32::System::Threading::GetExitCodeProcess;

        let mut exit_code = 0;
        let succeeded = unsafe { GetExitCodeProcess(self.0.as_raw_handle(), &mut exit_code) };
        assert_ne!(succeeded, 0, "descendant process status should be readable");
        exit_code == STILL_ACTIVE as u32
    }
}

#[cfg(not(windows))]
struct EngramDescendantProbe(libc::pid_t);

#[cfg(not(windows))]
impl EngramDescendantProbe {
    fn open(pid: u32) -> Self {
        let pid = libc::pid_t::try_from(pid).expect("descendant PID should fit pid_t");
        let probe = Self(pid);
        assert!(
            probe.is_alive(),
            "descendant must still be alive before cleanup"
        );
        probe
    }

    fn is_alive(&self) -> bool {
        if unsafe { libc::kill(self.0, 0) } == 0 {
            return true;
        }
        io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

fn process_fixture_bind(
    transport: &ProcessEngramControlTransport,
    connection: &EngramConnectionConfig,
    idempotency_key: &str,
) -> (String, String) {
    let result = transport
        .request(
            connection,
            &EngramControlRequest::SessionBind {
                external_ref: format!("termal:{}", connection.session_id),
                title: "Stateful process fixture".to_owned(),
                assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
                mediated_effects: vec![EngramEffect::Observe],
                capability_map_revision: ENGRAM_CAPABILITY_MAP_REVISION,
                work_binding: None,
                idempotency_key: idempotency_key.to_owned(),
            },
            Duration::from_secs(2),
        )
        .expect("stateful process fixture bind should succeed");
    (
        result["routing_token"]
            .as_str()
            .expect("fixture bind should return a token")
            .to_owned(),
        result["status"]["phase"]
            .as_str()
            .expect("fixture bind should return a phase")
            .to_owned(),
    )
}

fn process_fixture_evaluate(
    transport: &ProcessEngramControlTransport,
    connection: &EngramConnectionConfig,
    routing_token: &str,
    idempotency_key: &str,
    intent_fingerprint: &str,
) -> String {
    transport
        .request(
            connection,
            &EngramControlRequest::TurnEvaluate {
                routing_token: routing_token.to_owned(),
                idempotency_key: idempotency_key.to_owned(),
                intent_fingerprint: intent_fingerprint.to_owned(),
                purpose: "Exercise process fixture protocol fidelity".to_owned(),
                requested_effects: vec![EngramEffect::Observe],
                resource_intents: Vec::new(),
            },
            Duration::from_secs(2),
        )
        .expect("stateful process fixture evaluation should succeed")["grant"]["grant_id"]
        .as_str()
        .expect("fixture evaluation should issue a grant")
        .to_owned()
}

#[test]
fn real_process_fixture_persists_idempotency_and_unknown_grant_semantics() {
    let temp_root = TestTempRoot::create("engram-control-idempotency-fixture");
    let project_file = temp_root.path().join(".engram-project");
    fs::write(&project_file, "fixture-stateful-idempotency\n")
        .expect("stateful fixture mode should write");
    let connection = EngramConnectionConfig {
        binary_path: real_engram_control_fixture_path(),
        project_file,
        home: temp_root.path().to_path_buf(),
        project_root: temp_root.path().to_path_buf(),
        actor_id: "termal-fixture".to_owned(),
        actor_context: None,
        session_id: "engram-fixture-idempotency".to_owned(),
    };
    let transport = real_process_fixture_transport();

    let (routing_token, _) = process_fixture_bind(&transport, &connection, "durable-bind");
    assert_eq!(
        process_fixture_bind(&transport, &connection, "durable-bind").0,
        routing_token
    );
    let bind_conflict = transport
        .request(
            &connection,
            &EngramControlRequest::SessionBind {
                external_ref: format!("termal:{}", connection.session_id),
                title: "Changed fixture bind intent".to_owned(),
                assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
                mediated_effects: vec![EngramEffect::Observe],
                capability_map_revision: ENGRAM_CAPABILITY_MAP_REVISION,
                work_binding: None,
                idempotency_key: "durable-bind".to_owned(),
            },
            Duration::from_secs(2),
        )
        .expect_err("a changed same-key bind intent should conflict");
    assert_eq!(
        bind_conflict.code.as_deref(),
        Some("control_session_bind_conflict")
    );
    let grant_id = process_fixture_evaluate(
        &transport,
        &connection,
        &routing_token,
        "open-evaluate",
        "open-intent",
    );
    let refusal_request = EngramControlRequest::TurnEvaluate {
        routing_token: routing_token.clone(),
        idempotency_key: "persisted-open-refusal".to_owned(),
        intent_fingerprint: "blocked-intent".to_owned(),
        purpose: "Persist the open-turn refusal".to_owned(),
        requested_effects: vec![EngramEffect::Observe],
        resource_intents: Vec::new(),
    };
    let refusal = transport
        .request(&connection, &refusal_request, Duration::from_secs(2))
        .expect("open turn should refuse");
    assert_eq!(refusal["directive"]["code"], "turn_already_open");
    let (fresh_token, _) = process_fixture_bind(&transport, &connection, "expire-open-grant");
    let refusal_replay_request = match refusal_request {
        EngramControlRequest::TurnEvaluate {
            idempotency_key,
            intent_fingerprint,
            purpose,
            requested_effects,
            resource_intents,
            ..
        } => EngramControlRequest::TurnEvaluate {
            routing_token: fresh_token.clone(),
            idempotency_key,
            intent_fingerprint,
            purpose,
            requested_effects,
            resource_intents,
        },
        _ => unreachable!(),
    };
    let refusal_replay = transport
        .request(&connection, &refusal_replay_request, Duration::from_secs(2))
        .expect("persisted refusal should replay");
    assert_eq!(refusal_replay, refusal);

    let superseded_begin = EngramControlRequest::TurnBegin {
        routing_token: fresh_token.clone(),
        grant_id: grant_id.clone(),
        delivery_tokens: Vec::new(),
        idempotency_key: "superseded-known-begin".to_owned(),
    };
    let scope_refusal = transport
        .request(&connection, &superseded_begin, Duration::from_secs(2))
        .expect("a known superseded fixture grant should return a refusal decision");
    assert_eq!(scope_refusal["decision"], "refuse");
    assert_eq!(scope_refusal["code"], "grant_scope_mismatch");
    assert_eq!(
        transport
            .request(&connection, &superseded_begin, Duration::from_secs(2))
            .expect("fixture scope refusal should replay"),
        scope_refusal
    );
    let superseded_checkpoint = EngramControlRequest::TurnCheckpoint {
        routing_token: fresh_token.clone(),
        grant_id: grant_id.clone(),
        next_intent: EngramNextIntent::Wait,
        report: EngramTurnReport::default(),
        idempotency_key: "superseded-known-checkpoint".to_owned(),
    };
    let checkpoint_scope_refusal = transport
        .request(&connection, &superseded_checkpoint, Duration::from_secs(2))
        .expect("a known superseded fixture grant should return a checkpoint refusal decision");
    assert_eq!(checkpoint_scope_refusal["decision"], "refuse");
    assert_eq!(checkpoint_scope_refusal["code"], "grant_scope_mismatch");
    assert_eq!(
        transport
            .request(&connection, &superseded_checkpoint, Duration::from_secs(2),)
            .expect("fixture checkpoint scope refusal should replay"),
        checkpoint_scope_refusal
    );

    for request in [
        EngramControlRequest::TurnBegin {
            routing_token: fresh_token.clone(),
            grant_id: "never-issued".to_owned(),
            delivery_tokens: Vec::new(),
            idempotency_key: "unknown-begin".to_owned(),
        },
        EngramControlRequest::TurnCheckpoint {
            routing_token: fresh_token.clone(),
            grant_id: "never-issued".to_owned(),
            next_intent: EngramNextIntent::Wait,
            report: EngramTurnReport::default(),
            idempotency_key: "unknown-checkpoint".to_owned(),
        },
    ] {
        let error = transport
            .request(&connection, &request, Duration::from_secs(2))
            .expect_err("unknown grant should fail");
        assert_eq!(error.code.as_deref(), Some("turn_grant_not_found"));
    }

    let replacement_grant = process_fixture_evaluate(
        &transport,
        &connection,
        &fresh_token,
        "replacement-evaluate",
        "replacement-intent",
    );
    transport
        .request(
            &connection,
            &EngramControlRequest::TurnBegin {
                routing_token: fresh_token.clone(),
                grant_id: replacement_grant.clone(),
                delivery_tokens: vec!["delivery-a".to_owned()],
                idempotency_key: "delivery-scoped-begin".to_owned(),
            },
            Duration::from_secs(2),
        )
        .expect("replacement grant should begin");
    let conflict = transport
        .request(
            &connection,
            &EngramControlRequest::TurnBegin {
                routing_token: fresh_token.clone(),
                grant_id: replacement_grant.clone(),
                delivery_tokens: vec!["delivery-b".to_owned()],
                idempotency_key: "delivery-scoped-begin".to_owned(),
            },
            Duration::from_secs(2),
        )
        .expect_err("delivery-token change must conflict");
    assert_eq!(
        conflict.code.as_deref(),
        Some("control_operation_idempotency_conflict")
    );
    let checkpoint = EngramControlRequest::TurnCheckpoint {
        routing_token: fresh_token,
        grant_id: replacement_grant,
        next_intent: EngramNextIntent::Wait,
        report: EngramTurnReport::default(),
        idempotency_key: "durable-checkpoint".to_owned(),
    };
    let first = transport
        .request(&connection, &checkpoint, Duration::from_secs(2))
        .expect("checkpoint should succeed");
    assert_eq!(
        transport
            .request(&connection, &checkpoint, Duration::from_secs(2))
            .expect("checkpoint replay should succeed"),
        first
    );
    assert!(grant_id.starts_with("fixture-grant-"));
    transport.shutdown_session(&connection.session_id);
}

#[test]
fn real_process_fixture_enforces_stale_begin_and_unbegun_grant_recovery() {
    let temp_root = TestTempRoot::create("engram-control-stateful-fixture");
    let project_file = temp_root.path().join(".engram-project");
    fs::write(&project_file, "fixture-stateful-stale-begin\n")
        .expect("stale-begin fixture mode should write");
    let base_connection = EngramConnectionConfig {
        binary_path: real_engram_control_fixture_path(),
        project_file: project_file.clone(),
        home: temp_root.path().to_path_buf(),
        project_root: temp_root.path().to_path_buf(),
        actor_id: "termal-fixture".to_owned(),
        actor_context: None,
        session_id: "engram-fixture-stale-begin".to_owned(),
    };
    let transport = real_process_fixture_transport();

    let (stale_token, stale_phase) =
        process_fixture_bind(&transport, &base_connection, "stale-bind");
    assert_eq!(stale_phase, "sync_required");
    let stale_grant = process_fixture_evaluate(
        &transport,
        &base_connection,
        &stale_token,
        "stale-evaluate",
        "stable-intent-fingerprint",
    );
    let stale_begin_key = format!("fixture-begin:{stale_grant}");
    let stale_begin = transport
        .request(
            &base_connection,
            &EngramControlRequest::TurnBegin {
                routing_token: stale_token.clone(),
                grant_id: stale_grant,
                delivery_tokens: Vec::new(),
                idempotency_key: stale_begin_key.clone(),
            },
            Duration::from_secs(2),
        )
        .expect("the fixture should return the configured stale-begin refusal");
    assert_eq!(stale_begin["decision"], "refuse");
    assert_eq!(stale_begin["code"], "policy_epoch_changed");

    let fresh_grant = process_fixture_evaluate(
        &transport,
        &base_connection,
        &stale_token,
        "fresh-reevaluate",
        "stable-intent-fingerprint",
    );
    transport
        .request(
            &base_connection,
            &EngramControlRequest::TurnBegin {
                routing_token: stale_token.clone(),
                grant_id: fresh_grant.clone(),
                delivery_tokens: Vec::new(),
                idempotency_key: format!("fixture-begin:{fresh_grant}"),
            },
            Duration::from_secs(2),
        )
        .expect("the replacement grant should begin with its own key");
    transport
        .request(
            &base_connection,
            &EngramControlRequest::TurnCheckpoint {
                routing_token: stale_token.clone(),
                grant_id: fresh_grant.clone(),
                next_intent: EngramNextIntent::Exit,
                report: EngramTurnReport::default(),
                idempotency_key: "stale-fixture-cleanup".to_owned(),
            },
            Duration::from_secs(2),
        )
        .expect("the replacement grant should checkpoint");
    let reused_key_error = transport
        .request(
            &base_connection,
            &EngramControlRequest::TurnBegin {
                routing_token: stale_token,
                grant_id: fresh_grant,
                delivery_tokens: Vec::new(),
                idempotency_key: stale_begin_key,
            },
            Duration::from_secs(2),
        )
        .expect_err("a stale grant's begin key must not be reused");
    assert_eq!(reused_key_error.kind, EngramTransportErrorKind::Remote);
    assert_eq!(
        reused_key_error.code.as_deref(),
        Some("control_operation_idempotency_conflict")
    );
    transport.shutdown_session(&base_connection.session_id);

    fs::write(&project_file, "fixture-stateful-orphan\n")
        .expect("orphaned-grant fixture mode should write");
    let orphan_connection = EngramConnectionConfig {
        session_id: "engram-fixture-orphaned-grant".to_owned(),
        ..base_connection
    };
    let (orphan_token, orphan_phase) =
        process_fixture_bind(&transport, &orphan_connection, "orphan-bind");
    assert_eq!(orphan_phase, "sync_required");
    let orphan_grant = process_fixture_evaluate(
        &transport,
        &orphan_connection,
        &orphan_token,
        "orphan-evaluate",
        "orphaned-intent",
    );
    let status = transport
        .request(
            &orphan_connection,
            &EngramControlRequest::SessionStatus {
                routing_token: orphan_token.clone(),
            },
            Duration::from_secs(2),
        )
        .expect("status should expose the issued grant");
    assert_eq!(status["open_grant_id"], orphan_grant);
    let unbegun_checkpoint = transport
        .request(
            &orphan_connection,
            &EngramControlRequest::TurnCheckpoint {
                routing_token: orphan_token.clone(),
                grant_id: orphan_grant.clone(),
                next_intent: EngramNextIntent::Wait,
                report: EngramTurnReport::default(),
                idempotency_key: "orphan-checkpoint".to_owned(),
            },
            Duration::from_secs(2),
        )
        .expect("an issued but unbegun grant should return a refusal decision");
    assert_eq!(unbegun_checkpoint["decision"], "refuse");
    assert_eq!(unbegun_checkpoint["code"], "grant_not_begun");
    let status_after_refusal = transport
        .request(
            &orphan_connection,
            &EngramControlRequest::SessionStatus {
                routing_token: orphan_token.clone(),
            },
            Duration::from_secs(2),
        )
        .expect("a checkpoint refusal decision must keep the control connection alive");
    assert_eq!(
        status_after_refusal["open_grant_id"], orphan_grant,
        "the checkpoint refusal decision must not rotate or reset the control session"
    );

    let (fresh_token, fresh_phase) =
        process_fixture_bind(&transport, &orphan_connection, "orphan-rebind");
    assert_ne!(fresh_token, orphan_token);
    assert_eq!(fresh_phase, "sync_required");
    let recovered_grant = process_fixture_evaluate(
        &transport,
        &orphan_connection,
        &fresh_token,
        "recovered-evaluate",
        "recovered-intent",
    );
    transport
        .request(
            &orphan_connection,
            &EngramControlRequest::TurnBegin {
                routing_token: fresh_token,
                grant_id: recovered_grant.clone(),
                delivery_tokens: Vec::new(),
                idempotency_key: format!("fixture-begin:{recovered_grant}"),
            },
            Duration::from_secs(2),
        )
        .expect("evaluation should resume after the fresh bind");
    transport.shutdown_session(&orphan_connection.session_id);

    fs::write(&project_file, "fixture-stateful-delivery-invalid-begin\n")
        .expect("non-expiring refusal fixture mode should write");
    let refusal_connection = EngramConnectionConfig {
        session_id: "engram-fixture-non-expiring-refusal".to_owned(),
        ..orphan_connection
    };
    let (refusal_token, refusal_phase) =
        process_fixture_bind(&transport, &refusal_connection, "refusal-bind");
    assert_eq!(refusal_phase, "sync_required");
    let refused_grant = process_fixture_evaluate(
        &transport,
        &refusal_connection,
        &refusal_token,
        "refusal-evaluate",
        "non-expiring-refusal-intent",
    );
    let begin_refusal = transport
        .request(
            &refusal_connection,
            &EngramControlRequest::TurnBegin {
                routing_token: refusal_token.clone(),
                grant_id: refused_grant.clone(),
                delivery_tokens: Vec::new(),
                idempotency_key: format!("fixture-begin:{refused_grant}"),
            },
            Duration::from_secs(2),
        )
        .expect("delivery_invalid should be a normal begin refusal");
    assert_eq!(begin_refusal["decision"], "refuse");
    assert_eq!(begin_refusal["code"], "delivery_invalid");

    let open_turn_refusal = transport
        .request(
            &refusal_connection,
            &EngramControlRequest::TurnEvaluate {
                routing_token: refusal_token.clone(),
                idempotency_key: "evaluate-while-refused-grant-open".to_owned(),
                intent_fingerprint: "next-intent".to_owned(),
                purpose: "Verify the real open-turn response shape".to_owned(),
                requested_effects: vec![EngramEffect::Observe],
                resource_intents: Vec::new(),
            },
            Duration::from_secs(2),
        )
        .expect("turn_already_open should be a refusal decision, not an error envelope");
    assert_eq!(open_turn_refusal["decision"], "refuse");
    assert_eq!(open_turn_refusal["directive"]["code"], "turn_already_open");
    let refusal_status = transport
        .request(
            &refusal_connection,
            &EngramControlRequest::SessionStatus {
                routing_token: refusal_token.clone(),
            },
            Duration::from_secs(2),
        )
        .expect("status should preserve the non-expiring issued grant");
    assert_eq!(refusal_status["phase"], "turn_open");
    assert_eq!(refusal_status["open_grant_id"], refused_grant);
    let refused_checkpoint = transport
        .request(
            &refusal_connection,
            &EngramControlRequest::TurnCheckpoint {
                routing_token: refusal_token.clone(),
                grant_id: refused_grant,
                next_intent: EngramNextIntent::Wait,
                report: EngramTurnReport::default(),
                idempotency_key: "non-expiring-refusal-checkpoint".to_owned(),
            },
            Duration::from_secs(2),
        )
        .expect("an issued grant should return a checkpoint refusal decision");
    assert_eq!(refused_checkpoint["decision"], "refuse");
    assert_eq!(refused_checkpoint["code"], "grant_not_begun");

    let (replacement_token, replacement_phase) =
        process_fixture_bind(&transport, &refusal_connection, "refusal-rebind");
    assert_ne!(replacement_token, refusal_token);
    assert_eq!(replacement_phase, "sync_required");
    let replacement_grant = process_fixture_evaluate(
        &transport,
        &refusal_connection,
        &replacement_token,
        "replacement-evaluate",
        "replacement-intent",
    );
    transport
        .request(
            &refusal_connection,
            &EngramControlRequest::TurnBegin {
                routing_token: replacement_token.clone(),
                grant_id: replacement_grant.clone(),
                delivery_tokens: Vec::new(),
                idempotency_key: format!("fixture-begin:{replacement_grant}"),
            },
            Duration::from_secs(2),
        )
        .expect("the replacement grant should begin");
    let begun_bind_error = transport
        .request(
            &refusal_connection,
            &EngramControlRequest::SessionBind {
                external_ref: "termal:engram-fixture-non-expiring-refusal".to_owned(),
                title: "Reject bind over begun grant".to_owned(),
                assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
                mediated_effects: vec![EngramEffect::Observe],
                capability_map_revision: ENGRAM_CAPABILITY_MAP_REVISION,
                work_binding: None,
                idempotency_key: "bind-over-begun".to_owned(),
            },
            Duration::from_secs(2),
        )
        .expect_err("bind over a begun grant must be rejected");
    assert_eq!(
        begun_bind_error.code.as_deref(),
        Some("invalid_control_session")
    );
    transport
        .request(
            &refusal_connection,
            &EngramControlRequest::TurnCheckpoint {
                routing_token: replacement_token,
                grant_id: replacement_grant,
                next_intent: EngramNextIntent::Exit,
                report: EngramTurnReport::default(),
                idempotency_key: "replacement-checkpoint".to_owned(),
            },
            Duration::from_secs(2),
        )
        .expect("the begun replacement grant should checkpoint");
    transport.shutdown_session(&refusal_connection.session_id);
}

fn real_process_fixture_transport() -> ProcessEngramControlTransport {
    ProcessEngramControlTransport::with_startup_handshake(
        "termal-engram-control-fixture-ready",
        DEADLOCK_GUARD,
    )
}
