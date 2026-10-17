// Owns doctor process-tree deadline fixtures and their startup/error regressions.
// The original two deadline tests moved here from engram_host_adapter.rs.
// Does not own production diagnostic budgets or the ordinary control fixture.
use super::*;
use crate::tests::phase_sync::WorkContextGate;

#[derive(Default)]
struct DoctorFixtureOptions {
    expired_startup: bool,
    worker_failure: Option<&'static str>,
}

fn check_doctor_descendant_deadline(mode: &str, options: DoctorFixtureOptions) {
    let root = TestTempRoot::create("termal-doctor-descendant");
    let (project_file, ready) = prepare_engram_control_process_tree_fixture(&root, mode);
    // A parked fixture cannot publish descendant readiness. Forced launch
    // expiry makes startup consume the budget without depending on machine
    // load or waiting for an arbitrary sleep to finish.
    let mut startup_gate = options
        .expired_startup
        .then(|| WorkContextGate::new(root.path()));
    let _expired_startup = options
        .expired_startup
        .then(|| expire_test_engram_diagnostic_launches(&[root.path()]));
    let fixture = FsPath::new(env!("CARGO_MANIFEST_DIR")).join(if cfg!(windows) {
        "src/tests/fixtures/engram-doctor-pipe-fixture.ps1"
    } else {
        "src/tests/fixtures/engram-doctor-pipe-fixture.sh"
    });
    let home = root.path().to_path_buf();
    let mut deadline_arm = register_test_engram_doctor_deadline(&home, DEADLOCK_GUARD);
    let (sender, receiver) = mpsc::channel();
    let worker_failure = options.worker_failure;
    let worker = std::thread::spawn(move || {
        let result = if let Some(message) = worker_failure {
            Err(ApiError::bad_request(message))
        } else {
            run_engram_doctor_result_within(
                &fixture,
                &project_file,
                &home,
                &home,
                Duration::from_secs(5),
                &home,
            )
        };
        let _ = sender.send(result.map(|report| report.healthy));
    });
    if options.worker_failure.is_some() {
        // No descendant is launched by this injected worker. Disconnect its
        // readiness publisher so the failure assertion need not wait 60 s.
        ready.cancel_pending();
    }
    // Acquires a process handle while the descendant is alive and keeps its
    // lifetime socket open. Dropping it releases an escaped Unix descendant.
    let readiness = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Some(gate) = &mut startup_gate {
            gate.wait();
            gate.release();
        }
        ready.wait()
    }));
    let descendant = match readiness {
        Ok(descendant) => descendant,
        Err(failure) => {
            drop(startup_gate);
            drop(deadline_arm);
            doctor_readiness_failed(doctor_panic_message(failure.as_ref()), receiver, worker);
        }
    };
    let started = std::time::Instant::now();
    if let Err(error) = deadline_arm.arm() {
        drop(descendant);
        drop(startup_gate);
        drop(deadline_arm);
        doctor_readiness_failed(error, receiver, worker);
    }
    // Read while the descendant is parked, so its launching process is alive.
    #[cfg(windows)]
    let fixture_parent = engram_fixture_descendant_parent(&descendant);
    let result = receiver.recv_timeout(DEADLOCK_GUARD);
    if result.is_err() {
        drop(descendant);
        let _ = worker.join();
        panic!("doctor did not return while its descendant held the pipe");
    }
    let error = result
        .unwrap()
        .expect_err("an incomplete doctor must not report success");
    assert!(started.elapsed() < DEADLOCK_GUARD);
    if mode == "doctor-tree-exit" {
        assert!(
            error.message.contains("output collection exceeded"),
            "{}",
            error.message
        );
        #[cfg(windows)]
        assert_engram_control_descendant_was_terminated(&descendant, "doctor output deadline");
        // Unix cleanup after reap deliberately does not signal a reused PGID.
    } else {
        assert!(
            error.message.contains("enablement deadline"),
            "{}",
            error.message
        );
        assert_engram_control_descendant_was_terminated(&descendant, "doctor process deadline");
    }
    #[cfg(windows)]
    assert_engram_fixture_teardown_covers_its_parent(&descendant, fixture_parent);
    drop(descendant);
    worker.join().expect("doctor worker should finish");
}

fn doctor_panic_message(failure: &(dyn std::any::Any + Send)) -> String {
    failure
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            failure
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
        })
        .unwrap_or_else(|| "non-text readiness panic".to_owned())
}

fn doctor_readiness_failed(
    phase: String,
    receiver: mpsc::Receiver<std::result::Result<bool, ApiError>>,
    worker: std::thread::JoinHandle<()>,
) -> ! {
    // The registration was cancelled before entering here. Keep cleanup
    // bounded, but preserve the actual launch/transport error the worker sent.
    let observed = receiver.recv_timeout(DEADLOCK_GUARD);
    let completed = !matches!(&observed, Err(mpsc::RecvTimeoutError::Timeout));
    let mut detail = match observed {
        Ok(Err(error)) => error.message,
        Ok(Ok(healthy)) => format!("worker returned healthy={healthy} before readiness completed"),
        Err(error) => format!("worker result unavailable: {error}"),
    };
    if completed && let Err(failure) = worker.join() {
        detail.push_str(&format!(
            "; worker panicked: {}",
            doctor_panic_message(failure.as_ref())
        ));
    }
    panic!("doctor fixture readiness failed: {phase}; worker: {detail}");
}

#[test]
fn doctor_deadline_covers_a_pipe_held_after_direct_child_exit() {
    check_doctor_descendant_deadline("doctor-tree-exit", DoctorFixtureOptions::default());
}

#[test]
fn doctor_deadline_terminates_a_live_process_tree() {
    check_doctor_descendant_deadline("doctor-tree-hang", DoctorFixtureOptions::default());
}

#[test]
fn doctor_deadline_expired_startup_cannot_kill_the_fixture_before_readiness() {
    check_doctor_descendant_deadline(
        "doctor-tree-hang",
        DoctorFixtureOptions {
            expired_startup: true,
            ..DoctorFixtureOptions::default()
        },
    );
}

#[test]
fn doctor_deadline_readiness_failure_reports_the_workers_actual_error() {
    const FAILURE: &str = "injected doctor worker startup failure";
    let failed = std::panic::catch_unwind(|| {
        check_doctor_descendant_deadline(
            "doctor-tree-hang",
            DoctorFixtureOptions {
                worker_failure: Some(FAILURE),
                ..DoctorFixtureOptions::default()
            },
        );
    })
    .expect_err("a worker failure must fail the readiness phase");
    let message = failed
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| failed.downcast_ref::<&str>().copied())
        .unwrap_or("non-text readiness panic");
    assert!(
        message.contains(FAILURE),
        "readiness failure hid the worker's actual error: {message}"
    );
}
