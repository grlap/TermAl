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

#[cfg(windows)]
use super::super::phase_sync::TEST_FIXTURE_TREE_PIDS_ENV;
use super::*;

#[test]
fn real_process_named_root_reclamation_startup_budget_keeps_unsent_process() {
    let temp_root = TestTempRoot::create("engram-control-startup-budget");
    let project_file = temp_root.path().join(".engram-project");
    fs::write(&project_file, "fixture-stateful-idempotency\n").unwrap();
    let connection = EngramConnectionConfig {
        binary_path: real_engram_control_fixture_path(),
        project_file,
        home: temp_root.path().to_path_buf(),
        project_root: temp_root.path().to_path_buf(),
        actor_id: "termal-fixture".to_owned(),
        actor_context: None,
        session_id: "engram-fixture-startup-budget".to_owned(),
    };
    let transport = real_process_fixture_transport(temp_root.path());
    let expired = EngramControlRequest::SessionBind {
        external_ref: format!("termal:{}", connection.session_id),
        title: "Unsent expired intent".to_owned(),
        assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
        mediated_effects: vec![EngramEffect::Observe],
        capability_map_revision: ENGRAM_CAPABILITY_MAP_REVISION,
        work_binding: None,
        idempotency_key: "startup-budget-bind".to_owned(),
    };
    // Zero remaining time deterministically selects the post-startup,
    // pre-dispatch deadline branch, independently of process startup speed.
    let error = transport.request(&connection, &expired, Duration::ZERO).unwrap_err();
    assert_eq!(error.kind, EngramTransportErrorKind::Deadline);
    assert!(error.message.contains("budget exhausted during process startup"));
    // The request was never handed to the worker: it is proven unsent, not
    // an unknown outcome, and keeps its deadline kind for circuit accounting.
    // An exhausted budget is no proof that control is unavailable, so the
    // cause withholds availability and the turn stays a hold.
    assert!(error.request_never_sent);
    assert!(!error.process_never_started);
    assert!(error.counts_for_circuit_breaker());
    let cause = EngramCausalFailure::request(&error, &expired);
    assert_eq!(cause.remote_application, EngramRemoteApplication::NotStarted);
    assert!(!cause.control_availability);
    assert!(!engram_ordinary_availability_cause(&cause));
    let prepared = transport.processes.lock().unwrap()
        .get(&connection.session_id).cloned().flatten()
        .expect("an unsent request must leave the prepared process available to a later pass");
    assert!(prepared.process.try_wait().unwrap().is_none());
    // A changed intent under the same key would conflict if the expired
    // request had reached Engram. A fresh budget sends only this later intent.
    let (token, _) = process_fixture_bind(&transport, &connection, "startup-budget-bind");
    assert!(!token.is_empty());
    assert!(Arc::ptr_eq(&prepared, &transport.process_for(&connection).unwrap()));
    transport.shutdown_session(&connection.session_id);
}

fn unsent_boundary_connection(
    temp_root: &TestTempRoot,
    binary_path: PathBuf,
    session_id: &str,
) -> EngramConnectionConfig {
    let project_file = temp_root.path().join(".engram-project");
    EngramConnectionConfig {
        binary_path,
        project_file,
        home: temp_root.path().to_path_buf(),
        project_root: temp_root.path().to_path_buf(),
        actor_id: "termal-fixture".to_owned(),
        actor_context: None,
        session_id: session_id.to_owned(),
    }
}

fn unsent_boundary_bind(title: String, key: &str) -> EngramControlRequest {
    EngramControlRequest::SessionBind {
        external_ref: "termal:unsent-boundary".to_owned(),
        title,
        assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
        mediated_effects: vec![EngramEffect::Observe],
        capability_map_revision: ENGRAM_CAPABILITY_MAP_REVISION,
        work_binding: None,
        idempotency_key: key.to_owned(),
    }
}

/// An oversize request is refused before any process is consulted, so no
/// byte of it can have reached Engram.
#[test]
fn an_oversize_request_frame_is_proven_unsent_and_starts_no_process() {
    let temp_root = TestTempRoot::create("engram-control-oversize-unsent");
    fs::write(temp_root.path().join(".engram-project"), "fixture-ok\n").unwrap();
    let connection = unsent_boundary_connection(
        &temp_root,
        real_engram_control_fixture_path(),
        "engram-fixture-oversize",
    );
    let transport = real_process_fixture_transport(temp_root.path());
    let oversize = unsent_boundary_bind(
        "x".repeat(ENGRAM_CONTROL_MAX_FRAME_BYTES + 1),
        "oversize-bind",
    );

    let error = transport
        .request(&connection, &oversize, DEADLOCK_GUARD)
        .expect_err("an oversize frame must be refused");

    assert_eq!(error.kind, EngramTransportErrorKind::Protocol);
    assert!(error.message.contains("exceeds the maximum control frame"));
    assert!(error.request_never_sent);
    // Still a protocol failure for the session's disable rule.
    assert!(error.disables_session());
    assert_eq!(
        EngramCausalFailure::request(&error, &oversize).remote_application,
        EngramRemoteApplication::NotStarted
    );
    assert!(
        transport.processes.lock().unwrap().is_empty(),
        "no control process may be started for a frame that cannot be sent"
    );
}

/// A control process whose startup handshake fails was only read from, so
/// it never received the request. The fixture starts and prints its own
/// ready line, which this transport does not expect: the failure is in
/// `process_for`, after the process ran, on every platform.
#[test]
fn a_startup_handshake_failure_is_proven_unsent() {
    let temp_root = TestTempRoot::create("engram-control-handshake-unsent");
    fs::write(temp_root.path().join(".engram-project"), "fixture-ok\n").unwrap();
    let connection = unsent_boundary_connection(
        &temp_root,
        real_engram_control_fixture_path(),
        "engram-fixture-handshake-unsent",
    );
    let transport = ProcessEngramControlTransport::with_startup_handshake(
        temp_root.path().to_path_buf(),
        "a-ready-line-the-fixture-never-prints",
        DEADLOCK_GUARD,
    );
    let request = unsent_boundary_bind("Handshake failure".to_owned(), "handshake-unsent-bind");

    let error = transport
        .request(&connection, &request, DEADLOCK_GUARD)
        .expect_err("a mismatched startup handshake must fail");

    assert!(error.request_never_sent, "{error}");
    assert!(!error.process_never_started, "the process did start: {error}");
    let cause = EngramCausalFailure::request(&error, &request);
    assert_eq!(cause.remote_application, EngramRemoteApplication::NotStarted);
    assert!(!cause.control_availability);
    assert!(transport.processes.lock().unwrap().is_empty());
}

/// The Begin consumer keeps a fresh begin pending, with no transport
/// accounting, only when the host stopped it before the transport. A begin
/// the transport proved unsent is still the transport's failure: it counts
/// for the circuit and a protocol failure disables the session. Only a
/// process that never started marks control unavailable.
#[test]
fn only_a_host_stopped_request_skips_transport_accounting() {
    let with_cause = |mut error: EngramTransportError, not_started: bool| {
        let mut cause = EngramCausalFailure::error(&error, "turn_begin", None, &[]);
        if not_started {
            cause.remote_application = EngramRemoteApplication::NotStarted;
        }
        error.causal_failure = Some(cause);
        error
    };

    // The adapter's own pre-transmission stop (a guard or budget): its own
    // hold, never the possibly-begun replay.
    let host_stopped = with_cause(EngramTransportError::deadline("Begin budget ran out"), true);
    assert!(host_stopped.stopped_before_transport());
    assert!(!engram_cause_allows_begin_replay(
        host_stopped.causal_failure.as_ref().unwrap()
    ));

    // The transport proved these unsent; they stay transport failures, and
    // the saved cause carries the distinction.
    for (transport_unsent, replays) in [
        (EngramTransportError::transport("worker gone").request_never_sent(), true),
        (
            EngramTransportError::deadline("budget exhausted during process startup")
                .request_never_sent(),
            true,
        ),
        // A protocol failure disables the session; it is never replayed.
        (EngramTransportError::protocol("oversize frame").request_never_sent(), false),
    ] {
        let cause = EngramCausalFailure::error(&transport_unsent, "turn_begin", None, &[]);
        assert_eq!(cause.remote_application, EngramRemoteApplication::NotStarted);
        assert!(cause.transport_unsent);
        assert!(!cause.control_availability, "{transport_unsent}");
        assert_eq!(engram_cause_allows_begin_replay(&cause), replays, "{transport_unsent}");
        let restored: EngramCausalFailure =
            serde_json::from_value(serde_json::to_value(&cause).unwrap()).unwrap();
        assert!(restored.transport_unsent, "a restored card keeps the distinction");
        let error = with_cause(transport_unsent, false);
        assert!(!error.stopped_before_transport(), "{error}");
        assert!(
            error.counts_for_circuit_breaker() || error.disables_session(),
            "the transport's failure still takes its accounting: {error}"
        );
    }

    // A possibly sent failure is neither.
    let possibly_sent = with_cause(EngramTransportError::deadline("call exceeded"), false);
    assert!(!possibly_sent.stopped_before_transport());

    // A process that never started keeps its availability meaning, and is
    // not marked as the transport's unsent request.
    let spawn = EngramTransportError::spawn_failed("missing binary");
    let cause = EngramCausalFailure::error(&spawn, "session_bind", None, &[]);
    assert_eq!(cause.remote_application, EngramRemoteApplication::NotStarted);
    assert!(cause.control_availability);
    assert!(!cause.transport_unsent);
}

/// The boundary of "proven unsent": once the request frame has been handed
/// to the control worker, a later deadline or early exit may follow a
/// partial or complete write, so the outcome stays unknown.
#[test]
fn a_failure_after_the_request_frame_is_handed_to_the_worker_stays_possibly_sent() {
    let temp_root = TestTempRoot::create("engram-control-sent-boundary");
    let project_file = temp_root.path().join(".engram-project");
    let connection = unsent_boundary_connection(
        &temp_root,
        real_engram_control_fixture_path(),
        "engram-fixture-sent-boundary",
    );
    let transport = real_process_fixture_transport(temp_root.path());
    let request = unsent_boundary_bind("Sent boundary".to_owned(), "sent-boundary-bind");

    // The hang fixture reads the frame and never replies. The process is
    // started first, and the budget is long enough that encoding and the
    // registry lookup cannot run it out before the hand-off on a loaded host:
    // the deadline this asserts is the one after the hand-off.
    fs::write(&project_file, "fixture-hang\n").unwrap();
    transport
        .process_for(&connection)
        .expect("the hang fixture should start before the request");
    let deadline = transport
        .request(&connection, &request, Duration::from_secs(2))
        .expect_err("the hang fixture must hit the deadline");
    assert_eq!(deadline.kind, EngramTransportErrorKind::Deadline);
    assert!(
        deadline.message.contains("control call exceeded"),
        "the post-hand-off deadline, not the startup budget: {deadline}"
    );
    assert!(!deadline.request_never_sent);
    assert!(!deadline.process_never_started);
    assert_eq!(
        EngramCausalFailure::request(&deadline, &request).remote_application,
        EngramRemoteApplication::Unknown
    );

    // The EOF fixture ends the exchange without a reply.
    fs::write(&project_file, "fixture-eof\n").unwrap();
    let eof = transport
        .request(&connection, &request, DEADLOCK_GUARD)
        .expect_err("the EOF fixture must fail");
    assert_eq!(eof.kind, EngramTransportErrorKind::Transport);
    assert!(!eof.request_never_sent);
    assert!(!eof.process_never_started);
    assert_eq!(
        EngramCausalFailure::request(&eof, &request).remote_application,
        EngramRemoteApplication::Unknown
    );
    transport.shutdown_session(&connection.session_id);
}

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
    let transport = real_process_fixture_transport(temp_root.path());

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

/// A control process that ends mid-request is named in the error by its pid,
/// with the exit state observed when the failure was seen, never inferred.
#[test]
fn real_process_eof_reports_the_control_process_identity_and_exit_state() {
    let temp_root = TestTempRoot::create("engram-control-fixture-evidence");
    let project_file = temp_root.path().join(".engram-project");
    fs::write(&project_file, "fixture-eof\n").expect("EOF mode should write");
    let connection = EngramConnectionConfig {
        binary_path: real_engram_control_fixture_path(),
        project_file,
        home: temp_root.path().to_path_buf(),
        project_root: temp_root.path().to_path_buf(),
        actor_id: "termal-fixture".to_owned(),
        actor_context: None,
        session_id: "engram-fixture-evidence".to_owned(),
    };
    let transport = real_process_fixture_transport(temp_root.path());
    let pid = transport
        .process_for(&connection)
        .expect("the fixture should complete its startup handshake")
        .process
        .id();
    let eof = transport
        .request(
            &connection,
            &engram_control_process_tree_request("evidence"),
            DEADLOCK_GUARD,
        )
        .expect_err("fixture EOF should be a transport error");
    assert_eq!(eof.kind, EngramTransportErrorKind::Transport);
    let cause =
        serde_json::to_value(EngramCausalFailure::error(&eof, "session_bind", None, &[])).unwrap();
    assert_eq!(cause["controlProcess"]["identity"], format!("pid {pid}"));
    let exit = cause["controlProcess"]["exitState"]
        .as_str()
        .expect("an observed exit state");
    assert!(
        exit.starts_with("exited: ") || exit == "running when the failure was seen",
        "the exit state is what was observed: {exit}"
    );
}

/// A call that misses its deadline names the control process it waited on,
/// seen still running before the host terminated it.
#[test]
fn real_process_deadline_reports_the_running_control_process() {
    let temp_root = TestTempRoot::create("engram-control-fixture-deadline-evidence");
    let project_file = temp_root.path().join(".engram-project");
    fs::write(&project_file, "fixture-hang\n").expect("hang mode should write");
    let connection = EngramConnectionConfig {
        binary_path: real_engram_control_fixture_path(),
        project_file,
        home: temp_root.path().to_path_buf(),
        project_root: temp_root.path().to_path_buf(),
        actor_id: "termal-fixture".to_owned(),
        actor_context: None,
        session_id: "engram-fixture-deadline-evidence".to_owned(),
    };
    let transport = real_process_fixture_transport(temp_root.path());
    let pid = transport
        .process_for(&connection)
        .expect("the fixture should complete its startup handshake")
        .process
        .id();
    let timeout = transport
        .request(
            &connection,
            &engram_control_process_tree_request("deadline-evidence"),
            Duration::from_millis(100),
        )
        .expect_err("the hung fixture should hit the deadline");
    assert_eq!(timeout.kind, EngramTransportErrorKind::Deadline);
    let cause = serde_json::to_value(EngramCausalFailure::error(
        &timeout,
        "session_bind",
        None,
        &[],
    ))
    .unwrap();
    assert_eq!(
        cause["controlProcess"],
        json!({"identity": format!("pid {pid}"), "exitState": "running when the failure was seen"})
    );
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
    let transport = Arc::new(real_process_fixture_transport(temp_root.path()));
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
    let transport = Arc::new(real_process_fixture_transport(temp_root.path()));
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
    let transport = real_process_fixture_transport(temp_root.path());

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
fn real_process_bounded_reset_shutdown_uses_the_existing_reaping_worker() {
    let temp_root = TestTempRoot::create("engram-control-bounded-shutdown");
    let (project_file, ready) = prepare_engram_control_process_tree_fixture(&temp_root, "fixture-tree-reply");
    let connection = engram_control_process_tree_connection(&temp_root, project_file, "bounded-shutdown");
    let transport = real_process_fixture_transport(temp_root.path());
    transport.request(&connection, &engram_control_process_tree_request("bounded-shutdown"), DEADLOCK_GUARD).unwrap();
    let descendant = ready.wait();
    let clock = EngramBudgetClock::Real;
    transport.shutdown_session_until(&connection.session_id, &clock,
        clock.now() + ENGRAM_CONTROL_SETTLE_TIMEOUT).unwrap();
    assert!(transport.processes.lock().unwrap().is_empty());
    assert_engram_control_descendant_was_terminated(&descendant, "bounded reset shutdown");
}

#[test]
fn real_process_concurrent_cold_start_reserves_one_child_and_keeps_other_sessions_available() {
    let temp_root = TestTempRoot::create("engram-control-concurrent-start");
    fs::write(temp_root.path().join(".engram-project"), "fixture-ok\n").unwrap();
    let connection = unsent_boundary_connection(
        &temp_root, real_engram_control_fixture_path(), "concurrent-start",
    );
    let transport = real_process_fixture_transport(temp_root.path());
    let (entered_tx, entered) = mpsc::sync_channel(1);
    let (release, release_rx) = mpsc::channel();
    *transport.start_gate.lock().unwrap() = Some((entered_tx, release_rx));
    let first = std::thread::scope(|scope| {
        let first = scope.spawn(|| transport.process_for(&connection));
        entered.recv_timeout(DEADLOCK_GUARD).unwrap();
        let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let competing = transport.process_for_until(&connection, std::time::Instant::now());
            let error = competing.err().expect("the second caller must not spawn a losing child");
            assert_eq!(error.code.as_deref(), Some("control_start_pending"));
            assert!(error.request_never_sent);
            assert!(!error.process_never_started);
            assert!(transport.processes.lock().unwrap()[&connection.session_id].is_none());
            let clock = EngramBudgetClock::Real;
            let stop = transport.shutdown_session_until(&connection.session_id, &clock, clock.now());
            assert_eq!(stop.unwrap_err().code.as_deref(), Some("control_start_pending"));
            assert!(transport.processes.lock().unwrap().contains_key(&connection.session_id));
            let mut other = connection.clone();
            other.session_id = "independent-start".to_owned();
            let independent = transport.process_for(&other).unwrap();
            assert!(independent.process.try_wait().unwrap().is_none());
            transport.shutdown_session(&other.session_id);
        }));
        drop(release);
        let first = first.join().unwrap().unwrap();
        observed.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        first
    });
    assert!(Arc::ptr_eq(&first, &transport.process_for(&connection).unwrap()));
    assert_eq!(transport.processes.lock().unwrap().len(), 1);
    transport.shutdown_session(&connection.session_id);
    assert!(first.process.try_wait().unwrap().is_some());
}

#[test]
fn real_process_reset_cleanup_deadline_retains_the_exact_child_and_refuses_a_successor() {
    let temp_root = TestTempRoot::create("engram-control-retained-shutdown");
    fs::write(temp_root.path().join(".engram-project"), "fixture-ok\n").unwrap();
    let connection = unsent_boundary_connection(&temp_root, real_engram_control_fixture_path(), "retained-shutdown");
    let transport = real_process_fixture_transport(temp_root.path());
    let request = unsent_boundary_bind("Retained shutdown".to_owned(), "retained-shutdown-bind");
    transport.request(&connection, &request, DEADLOCK_GUARD).unwrap();
    let original = transport.processes.lock().unwrap()[&connection.session_id].clone().unwrap();
    // Delay only the worker's cleanup publication. The real existing worker
    // still owns/reaps the child. Even an already-exited child must not be
    // replaced on the basis of a missing cleanup acknowledgement.
    let retained = Arc::new(EngramControlProcess {
        config: original.config.clone(), process: original.process.clone(),
        process_tree: original.process_tree.clone(), requests: original.requests.clone(),
        worker_finished: original.worker_finished.clone(), retiring: AtomicBool::new(false),
        cleanup_finished: Arc::new(AtomicBool::new(false)),
    });
    transport.processes.lock().unwrap().insert(connection.session_id.clone(), Some(retained.clone()));
    let clock = EngramBudgetClock::scripted();
    let expired = transport.shutdown_session_until(&connection.session_id, &clock, clock.now()).unwrap_err();
    assert_eq!(expired.kind, EngramTransportErrorKind::Deadline);
    assert!(Arc::ptr_eq(transport.processes.lock().unwrap()[&connection.session_id].as_ref().unwrap(), &retained));
    let refused = transport.request(&connection, &request, Duration::ZERO).unwrap_err();
    assert_eq!(refused.kind, EngramTransportErrorKind::LocalState);
    assert_eq!(refused.code.as_deref(), Some("control_cleanup_pending"));
    assert!(refused.request_never_sent);
    assert!(
        !refused.process_never_started,
        "the retained original child was already started"
    );
    assert!(!EngramCausalFailure::request(&refused, &request).control_availability,
        "unknown cleanup is not an ordinary availability fallback");
    assert!(Arc::ptr_eq(transport.processes.lock().unwrap()[&connection.session_id].as_ref().unwrap(), &retained));
    assert_eq!(retained.process.id(), original.process.id());
    transport.shutdown_session(&connection.session_id);
    assert!(original.process.try_wait().unwrap().is_some(), "explicit cleanup reaped the fixture");
}

#[test]
fn real_process_idle_reap_kills_the_entire_control_process_tree() {
    let temp_root = TestTempRoot::create("engram-control-process-tree-idle");
    let (project_file, ready) =
        prepare_engram_control_process_tree_fixture(&temp_root, "fixture-tree-reply");
    let connection = engram_control_process_tree_connection(&temp_root, project_file, "idle");
    let transport = ProcessEngramControlTransport::with_startup_handshake_and_idle_timeout(
        temp_root.path().to_path_buf(),
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
        format!("$env:TERMAL_TEST_DESCENDANT_ENDPOINT = '{endpoint}'\n# Names this process in the fixture tree (TEST_FIXTURE_TREE_PIDS_ENV).\nif ($env:{tree}) {{ $env:{tree} += \",$PID\" }} else {{ $env:{tree} = \"$PID\" }}\n& '{}' --exact tests::phase_sync::parked_control_descendant --nocapture\n", executable.to_string_lossy().replace('\'', "''"), tree = TEST_FIXTURE_TREE_PIDS_ENV),
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
    // The fixture processes that launched the descendant and named
    // themselves (Windows). They work in the temp root, so teardown waits for
    // them as well before the root is removed.
    #[cfg(windows)]
    ancestors: Vec<EngramDescendantProbe>,
    // The descendant's parent, read from a process snapshot before readiness
    // was acknowledged (Windows): see `engram_fixture_descendant_parent`.
    #[cfg(windows)]
    observed_parent: Option<u32>,
    // Kept open until after the termination assertion. Test teardown can then
    // release a surviving fixture without leaving a naturally-timed orphan.
    _lifetime: std::net::TcpStream,
}

impl EngramReadyProcess {
    /// The fixture processes its teardown waits for before the temp root
    /// drops.
    #[cfg(windows)]
    pub(super) fn awaited_pids(&self) -> Vec<u32> {
        std::iter::once(&self.probe)
            .chain(&self.ancestors)
            .map(EngramDescendantProbe::pid)
            .collect()
    }
}

/// The fixture process that launched the parked descendant, as the readiness
/// worker observed it before acknowledging readiness: the descendant was then
/// waiting for that acknowledgement and its launcher was waiting on it, so
/// both were alive, whatever the test thread's scheduling afterwards.
#[cfg(windows)]
pub(super) fn engram_fixture_descendant_parent(descendant: &EngramReadyProcess) -> Option<u32> {
    descendant.observed_parent
}

/// The parent of `leaf` in a process snapshot. `None` when the snapshot no
/// longer lists the parent (it exited), or when the parent is this test
/// process itself, which is not a fixture process: a fixture-tree
/// malfunction for a parked descendant.
#[cfg(windows)]
fn engram_snapshot_parent_of(leaf: u32) -> Option<u32> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    assert_ne!(
        snapshot, INVALID_HANDLE_VALUE,
        "process snapshot should open"
    );
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    let mut parent = None;
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while more {
        if entry.th32ProcessID == leaf {
            parent = Some(entry.th32ParentProcessID);
            break;
        }
        more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    unsafe { CloseHandle(snapshot) };
    let parent = parent?;
    let listed = {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        assert_ne!(
            snapshot, INVALID_HANDLE_VALUE,
            "process snapshot should open"
        );
        let mut found = false;
        let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while more && !found {
            found = entry.th32ProcessID == parent;
            more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }
        unsafe { CloseHandle(snapshot) };
        found
    };
    (listed && parent != std::process::id()).then_some(parent)
}

/// The teardown of `descendant` waits for the fixture process that launched
/// it: that process works in the temp root, so the root must not be dropped
/// while it can still hold a handle there.
#[cfg(windows)]
pub(super) fn assert_engram_fixture_teardown_covers_its_parent(
    descendant: &EngramReadyProcess,
    parent: Option<u32>,
) {
    let parent = parent.expect(
        "the descendant's launching fixture process should have been observed alive at readiness",
    );
    let awaited = descendant.awaited_pids();
    assert!(
        awaited.contains(&parent),
        "the fixture teardown does not wait for fixture process {parent}, which launched \
         the descendant and works in the temp root (awaited {awaited:?})"
    );
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
                .read_exact(&mut bytes)
                .expect("descendant should name its fixture tree");
            let count = u32::from_be_bytes(bytes);
            assert!(count <= 8, "fixture tree should be small, got {count}");
            let mut ancestor_pids = Vec::new();
            for _ in 0..count {
                stream
                    .read_exact(&mut bytes)
                    .expect("descendant should name each fixture process");
                ancestor_pids.push(u32::from_be_bytes(bytes));
            }
            // Opened before the acknowledgement: every fixture process above
            // the descendant waits for its readiness line, so all are alive.
            #[cfg(windows)]
            let ancestors: Vec<EngramDescendantProbe> = ancestor_pids
                .into_iter()
                .map(|pid| EngramDescendantProbe::open_labelled(pid, "fixture process"))
                .collect();
            #[cfg(not(windows))]
            drop(ancestor_pids);
            #[cfg(windows)]
            let observed_parent = engram_snapshot_parent_of(pid);
            stream
                .write_all(&[1])
                .expect("acknowledge descendant readiness");
            let _ = sender.send(EngramReadyProcess {
                probe,
                #[cfg(windows)]
                ancestors,
                #[cfg(windows)]
                observed_parent,
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

    /// Unblocks the fixture listener when no descendant will be launched.
    pub(super) fn cancel_pending(&self) {
        if let Ok(mut stream) = std::net::TcpStream::connect(self.address) {
            let _ = stream.write_all(&0_u32.to_be_bytes());
        }
    }
}

impl Drop for EngramDescendantReady {
    fn drop(&mut self) {
        // Unblock accept if startup failed before a descendant connected.
        self.cancel_pending();
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
        // Tree termination is asynchronous: the processes that launched the
        // descendant can still hold handles in the temp root after its exit
        // is published, so their exits are awaited too.
        for ancestor in &descendant.ancestors {
            let result = unsafe {
                WaitForSingleObject(
                    ancestor.0.as_raw_handle(),
                    DEADLOCK_GUARD.as_millis() as u32,
                )
            };
            assert_eq!(
                result,
                WAIT_OBJECT_0,
                "fixture process {} exit after {trigger} was not published after {:?}",
                ancestor.pid(),
                started.elapsed()
            );
        }
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
        Self::open_labelled(pid, "descendant")
    }

    /// Opens `pid`, which `what` names in the panic if it is already gone.
    fn open_labelled(pid: u32, what: &str) -> Self {
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
            "{what} {pid} must still be alive before cleanup"
        );
        Self(unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle) })
    }

    fn pid(&self) -> u32 {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::Threading::GetProcessId;

        let pid = unsafe { GetProcessId(self.0.as_raw_handle()) };
        assert_ne!(pid, 0, "fixture process id should be readable");
        pid
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

/// Budget for every call in the two stateful fixture tests below. They check
/// idempotency and stale-grant semantics, not latency, and a call's budget also
/// covers starting the PowerShell fixture when no process is running yet. A
/// two-second budget made their outcome depend on how fast the machine starts a
/// process: under load both failed at once with "admission budget exhausted
/// during process startup". So this is a guard against a hang, as in the
/// lifecycle test above; that test covers a deliberate deadline, and applies it
/// only after the startup handshake has been acquired.
const STATEFUL_FIXTURE_CALL_BUDGET: Duration = DEADLOCK_GUARD;

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
            STATEFUL_FIXTURE_CALL_BUDGET,
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
            STATEFUL_FIXTURE_CALL_BUDGET,
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
    let transport = real_process_fixture_transport(temp_root.path());

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
            STATEFUL_FIXTURE_CALL_BUDGET,
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
        .request(&connection, &refusal_request, STATEFUL_FIXTURE_CALL_BUDGET)
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
        .request(
            &connection,
            &refusal_replay_request,
            STATEFUL_FIXTURE_CALL_BUDGET,
        )
        .expect("persisted refusal should replay");
    assert_eq!(refusal_replay, refusal);

    let superseded_begin = EngramControlRequest::TurnBegin {
        routing_token: fresh_token.clone(),
        grant_id: grant_id.clone(),
        delivery_tokens: Vec::new(),
        idempotency_key: "superseded-known-begin".to_owned(),
    };
    let scope_refusal = transport
        .request(&connection, &superseded_begin, STATEFUL_FIXTURE_CALL_BUDGET)
        .expect("a known superseded fixture grant should return a refusal decision");
    assert_eq!(scope_refusal["decision"], "refuse");
    assert_eq!(scope_refusal["code"], "grant_scope_mismatch");
    assert_eq!(
        transport
            .request(&connection, &superseded_begin, STATEFUL_FIXTURE_CALL_BUDGET)
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
        .request(
            &connection,
            &superseded_checkpoint,
            STATEFUL_FIXTURE_CALL_BUDGET,
        )
        .expect("a known superseded fixture grant should return a checkpoint refusal decision");
    assert_eq!(checkpoint_scope_refusal["decision"], "refuse");
    assert_eq!(checkpoint_scope_refusal["code"], "grant_scope_mismatch");
    assert_eq!(
        transport
            .request(
                &connection,
                &superseded_checkpoint,
                STATEFUL_FIXTURE_CALL_BUDGET,
            )
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
            .request(&connection, &request, STATEFUL_FIXTURE_CALL_BUDGET)
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
            STATEFUL_FIXTURE_CALL_BUDGET,
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
            STATEFUL_FIXTURE_CALL_BUDGET,
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
        .request(&connection, &checkpoint, STATEFUL_FIXTURE_CALL_BUDGET)
        .expect("checkpoint should succeed");
    assert_eq!(
        transport
            .request(&connection, &checkpoint, STATEFUL_FIXTURE_CALL_BUDGET)
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
    let transport = real_process_fixture_transport(temp_root.path());

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
            STATEFUL_FIXTURE_CALL_BUDGET,
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
            STATEFUL_FIXTURE_CALL_BUDGET,
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
            STATEFUL_FIXTURE_CALL_BUDGET,
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
            STATEFUL_FIXTURE_CALL_BUDGET,
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
            STATEFUL_FIXTURE_CALL_BUDGET,
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
            STATEFUL_FIXTURE_CALL_BUDGET,
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
            STATEFUL_FIXTURE_CALL_BUDGET,
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
            STATEFUL_FIXTURE_CALL_BUDGET,
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
            STATEFUL_FIXTURE_CALL_BUDGET,
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
            STATEFUL_FIXTURE_CALL_BUDGET,
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
            STATEFUL_FIXTURE_CALL_BUDGET,
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
            STATEFUL_FIXTURE_CALL_BUDGET,
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
            STATEFUL_FIXTURE_CALL_BUDGET,
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
            STATEFUL_FIXTURE_CALL_BUDGET,
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
            STATEFUL_FIXTURE_CALL_BUDGET,
        )
        .expect("the begun replacement grant should checkpoint");
    transport.shutdown_session(&refusal_connection.session_id);
}

fn real_process_fixture_transport(host_workdir: &FsPath) -> ProcessEngramControlTransport {
    ProcessEngramControlTransport::with_startup_handshake(
        host_workdir.to_path_buf(),
        "termal-engram-control-fixture-ready",
        DEADLOCK_GUARD,
    )
}
