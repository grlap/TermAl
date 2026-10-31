//! Engram's terminal control-startup codes, `control_startup_busy` and
//! `control_startup_refused`, at the transport: each is one error line
//! answering the first frame followed by a nonzero exit, so the host evicts
//! and reaps that control worker and a later call starts a new one, while an
//! ordinary request-level error (a live `storage_error`, an unknown code)
//! keeps its worker as before. Also owns the classification of the two codes
//! as terminal remote errors that are neither "never started" nor certified
//! non-application, matched on the code and never on the message, and as no
//! refusal of a recovery checkpoint's report.
//!
//! Does not own the host's re-admission of a busy head or the hold of a
//! refused one (src/tests/engram_control_startup_admission.rs), the closing
//! checkpoint that keeps its report
//! (src/tests/engram_turn_checks.rs), or the fixture's other lifecycle modes
//! (src/tests/engram_control_transport.rs).
//! New module.

use super::*;

const STARTUP_CODES: [&str; 2] = ["control_startup_busy", "control_startup_refused"];

fn remote_error(code: &str, message: &str) -> EngramTransportError {
    EngramTransportError::remote(EngramControlErrorBody {
        code: code.to_owned(),
        message: message.to_owned(),
    })
}

fn startup_fixture(label: &str) -> (TestTempRoot, PathBuf, EngramConnectionConfig) {
    let temp_root = TestTempRoot::create(&format!("engram-control-startup-{label}"));
    let project_file = temp_root.path().join(".engram-project");
    fs::write(&project_file, "fixture-ok\n").unwrap();
    let connection = EngramConnectionConfig {
        binary_path: real_engram_control_fixture_path(),
        project_file: project_file.clone(),
        home: temp_root.path().to_path_buf(),
        project_root: temp_root.path().to_path_buf(),
        actor_id: "termal-fixture".to_owned(),
        actor_context: None,
        session_id: format!("engram-fixture-startup-{label}"),
    };
    (temp_root, project_file, connection)
}

fn fixture_transport(host_workdir: &FsPath) -> ProcessEngramControlTransport {
    ProcessEngramControlTransport::with_startup_handshake(
        host_workdir.to_path_buf(),
        "termal-engram-control-fixture-ready",
        DEADLOCK_GUARD,
    )
}

fn bind_request(key: &str) -> EngramControlRequest {
    EngramControlRequest::SessionBind {
        external_ref: "termal:startup-codes".to_owned(),
        title: "Startup codes".to_owned(),
        assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
        mediated_effects: vec![EngramEffect::Observe],
        capability_map_revision: ENGRAM_CAPABILITY_MAP_REVISION,
        work_binding: None,
        idempotency_key: key.to_owned(),
    }
}

/// The two startup codes are terminal remote errors: the worker is not kept
/// alive. Neither proves the process never started, neither certifies that
/// nothing was applied, and the decision rests on the code alone, whatever
/// the message says. Refused fails closed (it disables the session); busy
/// does not, so the safe re-admission can take it.
#[test]
fn control_startup_codes_are_terminal_remote_errors_classified_by_code() {
    for code in STARTUP_CODES {
        for message in [
            "Engram control startup could not acquire the store.",
            "any other wording",
            "",
        ] {
            let error = remote_error(code, message);
            assert_eq!(error.kind, EngramTransportErrorKind::Remote, "{code}");
            assert!(
                !error.keeps_control_process_alive(),
                "{code} ({message:?}): the worker exits, so it is evicted"
            );
            assert!(!error.process_never_started, "{code}: startup may have run");
            assert_eq!(
                error.invocation_application(),
                EngramInvocationApplication::Unknown,
                "{code}: never certified as not applied"
            );
            assert_eq!(
                error.disables_session(),
                code == "control_startup_refused",
                "{code} ({message:?}): only refused fails closed"
            );
        }
    }
}

/// A recovery checkpoint answered by a startup code was never dispatched, so
/// it is no refusal of the report it carries; a definitive refusal (a code
/// Engram uses to refuse the report) still is.
#[test]
fn a_recovery_checkpoint_startup_reply_is_not_a_report_refusal() {
    for code in STARTUP_CODES {
        let reply: std::result::Result<EngramTurnCheckpointResponse, EngramTransportError> =
            Err(remote_error(code, "Engram control startup could not acquire the store."));
        assert_eq!(engram_recovery_checkpoint_refusal(&reply), None, "{code}");
    }
    let refusal: std::result::Result<EngramTurnCheckpointResponse, EngramTransportError> =
        Err(remote_error("control_projection_invalid", "report payload refused"));
    assert_eq!(
        engram_recovery_checkpoint_refusal(&refusal),
        Some(Some("control_projection_invalid")),
        "a definitive refusal keeps its meaning"
    );
}

/// Every other request-level error keeps today's meaning: a live
/// `storage_error`, a code this host does not know, and a certified
/// non-application keep their worker and do not disable the session.
#[test]
fn ordinary_remote_errors_keep_their_worker_as_before() {
    for code in [
        "storage_error",
        "fixture_future_code",
        "control_request_expired",
        "control_store_busy",
        "control_startup",
        "control_startup_busy_v2",
    ] {
        let error = remote_error(code, "Engram control startup could not acquire the store.");
        assert!(error.keeps_control_process_alive(), "{code}: the worker is kept");
        assert!(!error.disables_session(), "{code}: not fatal");
    }
}

/// Each startup code evicts the worker it came from: the reply is reported as
/// that remote code, the registry retains its retiring cleanup owner, the
/// process has exited and been reaped, and the next call starts a new one.
#[test]
fn real_process_startup_code_evicts_and_reaps_the_worker() {
    for (code, mode) in [
        ("control_startup_busy", "fixture-startup-busy"),
        ("control_startup_refused", "fixture-startup-refused"),
    ] {
        let (temp_root, project_file, connection) = startup_fixture(mode);
        let transport = fixture_transport(temp_root.path());
        let worker = transport
            .process_for(&connection)
            .expect("the fixture completes its startup handshake");
        let pid = worker.process.id();

        fs::write(&project_file, format!("{mode}\n")).unwrap();
        let error = transport
            .request(&connection, &bind_request("startup-bind"), DEADLOCK_GUARD)
            .expect_err("the startup code is an error");
        assert_eq!(error.kind, EngramTransportErrorKind::Remote, "{code}");
        assert_eq!(error.code.as_deref(), Some(code));
        let retained = transport
            .processes
            .lock()
            .unwrap()
            .get(&connection.session_id)
            .and_then(Option::as_ref)
            .cloned()
            .expect("the retiring worker keeps registry custody until cleanup");
        assert!(Arc::ptr_eq(&retained, &worker), "{code}: exact cleanup owner");
        assert!(
            worker.retiring.load(std::sync::atomic::Ordering::Acquire),
            "{code}: the worker {pid} is retired from request service"
        );
        assert!(
            worker.process.wait().is_ok() && worker.process.try_wait().unwrap().is_some(),
            "{code}: the worker {pid} has exited and is reaped"
        );

        fs::write(&project_file, "fixture-ok\n").unwrap();
        let next = transport
            .request(&connection, &bind_request("startup-bind-next"), DEADLOCK_GUARD)
            .expect("a later call starts a new worker");
        assert_eq!(next["routing_token"], "fixture-token");
        assert_ne!(
            transport.process_for(&connection).unwrap().process.id(),
            pid,
            "{code}: a new worker"
        );
        transport.shutdown_session(&connection.session_id);
    }
}

/// An ordinary request-level error on a healthy connection keeps its worker
/// registered and running, and that same worker answers the next call.
#[test]
fn real_process_ordinary_remote_error_keeps_the_worker() {
    for (code, mode) in [
        ("storage_error", "fixture-remote-storage-error"),
        ("fixture_future_code", "fixture-remote-unknown-code"),
    ] {
        let (temp_root, project_file, connection) = startup_fixture(mode);
        let transport = fixture_transport(temp_root.path());
        let worker = transport
            .process_for(&connection)
            .expect("the fixture completes its startup handshake");

        fs::write(&project_file, format!("{mode}\n")).unwrap();
        let error = transport
            .request(&connection, &bind_request("ordinary-bind"), DEADLOCK_GUARD)
            .expect_err("the request-level error is reported");
        assert_eq!(error.code.as_deref(), Some(code));
        assert!(
            worker.process.try_wait().unwrap().is_none(),
            "{code}: the worker keeps running"
        );

        fs::write(&project_file, "fixture-ok\n").unwrap();
        transport
            .request(&connection, &bind_request("ordinary-bind-next"), DEADLOCK_GUARD)
            .expect("the same worker answers the next call");
        assert!(
            Arc::ptr_eq(&worker, &transport.process_for(&connection).unwrap()),
            "{code}: the same worker"
        );
        transport.shutdown_session(&connection.session_id);
    }
}
