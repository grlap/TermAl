//! Temporary producer repair keeps the same closing facts and durable wire.
//! The scheduler is driven one due attempt at a time, without wall-clock waits.
use super::repairs::stored_session;
use super::*;

const REPAIR_ATTEMPTS: usize = 61;

fn refusal(code: &str) -> EngramTransportError {
    if code == "store_open_refused" {
        let mut error = EngramTransportError::transport("store temporarily unopenable");
        error.refusal_code = Some(code.into());
        error.refusal_kind = Some("busy".into());
        return error;
    }
    EngramTransportError::remote(EngramControlErrorBody {
        code: code.to_owned(),
        message: format!("store refused: {code}"),
    })
}

fn close_without_worker(fixture: &mut ReportingTurn) -> EngramRuntimeClosingJob {
    // Own the normal worker's iteration boundary; the production capture,
    // installation, persistence and delivery owners still do every transition.
    fixture
        .turn
        .record_mut(|record| record.engram.unadmitted_worker_running = true);
    fixture.check("repair-retained-check", false);
    fixture.turn.wait_for_snapshots();
    // Subsequent production installs use their synchronous fallback, so the
    // exact-row readbacks cannot race the fixture's background writer.
    fixture.turn.state.shutdown_persist_blocking();
    fixture.finish();
    fixture
        .turn
        .record(|record| record.engram.unadmitted_closing_jobs[0].clone())
}

fn ready_report(
    fixture: &ReportingTurn,
    mut job: EngramRuntimeClosingJob,
) -> EngramRuntimeObservationReport {
    for _ in 0..4 {
        let outcome = fixture
            .turn
            .state
            .capture_runtime_observation_report(&fixture.turn.session_id, &job)
            .expect("ready store must allow closing capture");
        fixture
            .turn
            .state
            .install_runtime_closing_result(&fixture.turn.session_id, &job, Ok(outcome))
            .unwrap();
        if let Some(report) = fixture.turn.record(|record| {
            record
                .engram
                .unadmitted_reports
                .iter()
                .find(|report| report.host_turn_ref == job.trace.host_turn_ref)
                .cloned()
        }) {
            return report;
        }
        job = fixture
            .turn
            .record(|record| record.engram.unadmitted_closing_jobs[0].clone());
    }
    panic!("binding and policy preparation must reach an immutable report");
}

fn install_attempt(
    fixture: &ReportingTurn,
    report: &EngramRuntimeObservationReport,
) -> EngramRuntimeObservationReport {
    install_attempt_at(fixture, report, chrono::Utc::now())
}

fn install_attempt_at(
    fixture: &ReportingTurn,
    report: &EngramRuntimeObservationReport,
    now: chrono::DateTime<chrono::Utc>,
) -> EngramRuntimeObservationReport {
    assert!(
        runtime_observation_report_is_durable(
            &fixture.turn.state.persistence_path,
            &fixture.turn.session_id,
            report
        )
        .unwrap()
    );
    let adapter = fixture
        .turn
        .state
        .inner
        .lock()
        .unwrap()
        .engram_host_adapter
        .clone();
    let index = report.pending().unwrap();
    let (mut next, unavailable) =
        deliver_runtime_observation_attempt(&adapter, report, index, Duration::from_secs(1))
            .unwrap();
    finish_runtime_observation_delivery_attempt_at(
        &mut next,
        &fixture.turn.session_id,
        index,
        unavailable,
        now,
    );
    fixture
        .turn
        .state
        .install_runtime_observation_result(&fixture.turn.session_id, report, next.clone(), index)
        .unwrap();
    next
}

#[test]
fn parked_observation_worker_inspects_once_per_wake_and_claims_only_due_work() {
    struct StopWorkerOnDrop(Arc<EngramRuntimeObservationLifetime>);
    impl Drop for StopWorkerOnDrop {
        fn drop(&mut self) {
            self.0.request_stop();
        }
    }

    let fixture = ReportingTurn::new("parked-store-wait", true, ReplyMode::Recorded, false);
    let state = &fixture.turn.state;
    let session = &fixture.turn.session_id;
    state.shutdown_persist_blocking();
    let job = {
        // Seed the real preparation path without a concurrent closing kick.
        let mut inner = state.inner.lock().unwrap();
        let scope = EngramRuntimeClosingScope::capture(&inner, session).unwrap();
        let record = inner.session_mut(session).unwrap();
        let trace = record.engram.unadmitted_active_turn.clone().unwrap();
        let job = EngramRuntimeClosingJob::new(trace, None, scope);
        record.engram.unadmitted_worker_running = true;
        record.engram.unadmitted_closing_jobs.push(job.clone());
        job
    };
    let report = ready_report(&fixture, job);
    let mut delayed = report.next_version();
    let start = chrono::DateTime::parse_from_rfc3339("2000-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let due = start + chrono::Duration::days(1);
    delayed.retry_at = due.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    state
        .install_runtime_observation_result(session, &report, delayed.clone(), 0)
        .unwrap();
    fixture
        .turn
        .transport
        .observation_policy_errors
        .lock()
        .unwrap()
        .push_back(refusal("projection_repair_required"));

    let (parked, events) = mpsc::channel();
    let claims = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let inspections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let claims_at_hook = claims.clone();
    let inspections_at_hook = inspections.clone();
    let script = Arc::new(Mutex::new(start));
    let clock_after_install = script.clone();
    *state
        .runtime_observation_lifetime
        .install_hook
        .lock()
        .unwrap() = Some(Arc::new(move |phase| match phase {
        "retry_parked" => {
            let _ = parked.send(());
        }
        "retry_claimed" => {
            claims_at_hook.fetch_add(1, Ordering::SeqCst);
        }
        "retry_inspected" => {
            inspections_at_hook.fetch_add(1, Ordering::SeqCst);
        }
        "delivery_installed" => {
            // Keep the second park far from timer expiry as well. The saved
            // retry still uses the scripted time of the completed attempt.
            *clock_after_install.lock().unwrap() = start;
        }
        _ => {}
    }));
    let worker_state = state.clone();
    let worker_session = session.to_owned();
    let inspection_clock = script.clone();
    let stop = StopWorkerOnDrop(state.runtime_observation_lifetime.clone());
    let worker = std::thread::spawn(move || {
        worker_state.run_runtime_observation_worker_with_clock(&worker_session, || {
            *inspection_clock.lock().unwrap()
        });
    });
    crate::tests::phase_sync::receive(&events, "the worker parks on the future retry");
    assert_eq!(inspections.load(Ordering::SeqCst), 1);
    assert_eq!(claims.load(Ordering::SeqCst), 0);

    *script.lock().unwrap() = due;
    // The start path is also the new-work notification to an existing worker.
    state.start_runtime_observation_worker(session);
    crate::tests::phase_sync::receive(&events, "the worker parks after one refused attempt");
    assert_eq!(inspections.load(Ordering::SeqCst), 3);
    assert_eq!(claims.load(Ordering::SeqCst), 1);
    assert!(fixture.producer.requests().is_empty());
    fixture.turn.record(|record| {
        let pending = record.engram.unadmitted_reports.last().unwrap();
        assert_eq!(pending.pending(), Some(0));
        assert_eq!(pending.attempts, delayed.attempts);
        assert_eq!(pending.envelopes[0].request, delayed.envelopes[0].request);
        assert_eq!(
            pending.retry_at,
            (due + runtime_observation_retry_delay(
                session,
                ENGRAM_RUNTIME_OBSERVATION_MAX_ATTEMPTS
            ))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        );
        assert!(pending.store_wait_notice_posted);
    });
    drop(stop);
    worker.join().unwrap();
    assert_eq!(inspections.load(Ordering::SeqCst), 3);
    assert_eq!(claims.load(Ordering::SeqCst), 1);
}

#[test]
fn closing_capture_store_refusal_still_exhausts_with_readable_notice() {
    for code in [
        "projection_repair_required",
        "work_projection_invalid",
        "store_open_refused",
        "control_startup_busy",
        "control_store_busy",
    ] {
        let mut fixture =
            ReportingTurn::new("closing-store-repair", true, ReplyMode::Recorded, false);
        let mut job = close_without_worker(&mut fixture);
        let original = job.clone();
        for attempt in 1..=ENGRAM_RUNTIME_OBSERVATION_MAX_ATTEMPTS {
            fixture
                .turn
                .state
                .install_runtime_closing_result(&fixture.turn.session_id, &job, Err(refusal(code)))
                .unwrap();
            job = fixture
                .turn
                .record(|record| record.engram.unadmitted_closing_jobs[0].clone());
            assert_eq!(job.attempts, attempt);
            assert_eq!(
                job.terminal_failure.is_some(),
                attempt == ENGRAM_RUNTIME_OBSERVATION_MAX_ATTEMPTS
            );
            assert_eq!(job.trace, original.trace);
            assert_eq!(job.scope, original.scope);
        }
        let saved: Value = serde_json::from_str(&stored_session(
            &fixture.turn.state,
            &fixture.turn.session_id,
        ))
        .unwrap();
        assert_eq!(
            saved["engramUnadmittedClosingJobs"][0],
            serde_json::to_value(&job).unwrap()
        );
        let notices = fixture.turn.record(system_notices);
        assert_eq!(
            notices
                .iter()
                .filter(|line| line.contains("runtime_observation_retry_exhausted"))
                .count(),
            1
        );
        assert!(fixture.producer.requests().is_empty());
    }
}

#[test]
fn retained_delivery_survives_store_repair_without_changing_request_or_prior_budget() {
    for mode in [
        ReplyMode::Recorded,
        ReplyMode::TransientFor("work_projection_invalid", REPAIR_ATTEMPTS),
        ReplyMode::TransientFor("control_startup_busy", REPAIR_ATTEMPTS),
        ReplyMode::TransientFor("control_store_busy", REPAIR_ATTEMPTS),
    ] {
        let policy_read = mode == ReplyMode::Recorded;
        let mut fixture = ReportingTurn::new("delivery-store-repair", true, mode, false);
        let job = close_without_worker(&mut fixture);
        let binding = job.scope.binding.clone().unwrap();
        let checks: Vec<_> = job
            .trace
            .commands
            .iter()
            .map(|entry| &entry.check)
            .collect();
        let checks = serde_json::to_value(checks).unwrap();
        let mut report = ready_report(&fixture, job);
        // A pre-existing ambiguous attempt remains owed, including across
        // serialization/restart; temporary repair neither spends nor erases it.
        let mut initial = report.next_version();
        initial.attempts = ENGRAM_RUNTIME_OBSERVATION_MAX_ATTEMPTS - 1;
        fixture
            .turn
            .state
            .install_runtime_observation_result(
                &fixture.turn.session_id,
                &report,
                initial.clone(),
                0,
            )
            .unwrap();
        report = initial;
        let wire: Vec<_> = report
            .envelopes
            .iter()
            .map(|entry| entry.request.clone())
            .collect();
        assert_eq!(wire[0]["binding"], json!(binding));
        assert_eq!(wire[0]["policy_basis"]["mode"], "account_if_eligible");
        assert_eq!(wire[0]["occurrence"]["observed_checks"], checks);
        let start = chrono::DateTime::parse_from_rfc3339(&report.retry_at)
            .unwrap()
            .with_timezone(&chrono::Utc);
        let mut now = start;
        for _ in 0..REPAIR_ATTEMPTS {
            assert!(runtime_observation_retry_due(&report.retry_at, now));
            if policy_read {
                fixture
                    .turn
                    .transport
                    .observation_policy_errors
                    .lock()
                    .unwrap()
                    .push_back(refusal("projection_repair_required"));
            }
            report = install_attempt_at(&fixture, &report, now);
            assert_eq!(report.pending(), Some(0));
            assert_eq!(report.attempts, ENGRAM_RUNTIME_OBSERVATION_MAX_ATTEMPTS - 1);
            let due = chrono::DateTime::parse_from_rfc3339(&report.retry_at)
                .unwrap()
                .with_timezone(&chrono::Utc);
            assert!(due >= now + chrono::Duration::minutes(1));
            assert!(!runtime_observation_retry_due(
                &report.retry_at,
                due - chrono::Duration::milliseconds(1)
            ));
            now = due;
            assert_eq!(
                report
                    .envelopes
                    .iter()
                    .map(|entry| entry.request.clone())
                    .collect::<Vec<_>>(),
                wire
            );
            report = serde_json::from_slice(&serde_json::to_vec(&report).unwrap()).unwrap();
        }
        assert!(now - start >= chrono::Duration::hours(1));
        let notices = fixture.turn.record(system_notices);
        let notices: Vec<_> = notices
            .iter()
            .filter(|line| line.contains("store is temporarily unavailable"))
            .collect();
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("Check Engram readiness"));
        assert!(!notices[0].contains("transient producer response"));
        while report.pending().is_some() {
            report = install_attempt_at(&fixture, &report, now);
        }
        assert!(report.is_discharged());
        assert_eq!(fixture.producer.records.lock().unwrap().len(), wire.len());
        let requests = fixture.producer.requests();
        let first_attempts = if policy_read { 1 } else { REPAIR_ATTEMPTS + 1 };
        assert!(
            requests[..first_attempts]
                .iter()
                .all(|request| request == &wire[0])
        );
        assert_eq!(&requests[first_attempts..], &wire[1..]);
        let adapter = fixture
            .turn
            .state
            .inner
            .lock()
            .unwrap()
            .engram_host_adapter
            .clone();
        assert_eq!(
            deliver_runtime_observation_attempt(&adapter, &report, 0, Duration::from_secs(1))
                .unwrap()
                .0,
            report
        );
        assert_eq!(
            fixture.producer.requests(),
            requests,
            "a recorded envelope is not sent again"
        );
    }
}

#[test]
fn readiness_io_refuses_delivery_without_a_temporary_store_wait() {
    let mut fixture = ReportingTurn::new("store-io-delivery", true, ReplyMode::Recorded, false);
    let job = close_without_worker(&mut fixture);
    let mut report = ready_report(&fixture, job);
    let original = report.envelopes[0].request.clone();
    let mut error = EngramTransportError::transport("private store path failure");
    error.refusal_code = Some("store_open_refused".into());
    error.refusal_kind = Some("io".into());
    fixture
        .turn
        .transport
        .observation_policy_errors
        .lock()
        .unwrap()
        .push_back(error);
    report = install_attempt(&fixture, &report);
    assert_eq!(report.envelopes[0].request, original);
    assert!(matches!(
        &report.envelopes[0].delivery,
        EngramRuntimeObservationDelivery::LocalFailure { code, .. } if code == "store_open_refused"
    ));
    assert!(!report.store_wait_notice_posted);
    assert!(fixture.producer.requests().is_empty());
    let notices = fixture.turn.record(system_notices).join("\n");
    assert!(notices.contains("could not open or resolve its store (store_open_refused)"));
    assert!(!notices.contains("private store path failure"));
}

#[test]
fn permanent_observation_refusal_still_settles_once_with_readable_notice() {
    let mut fixture = ReportingTurn::new("permanent-store-refusal", true, ReplyMode::Refuse, false);
    let job = close_without_worker(&mut fixture);
    let mut report = ready_report(&fixture, job);
    let count = report.envelopes.len();
    while report.pending().is_some() {
        report = install_attempt(&fixture, &report);
    }
    assert!(report.is_discharged());
    assert_eq!(fixture.producer.requests().len(), count);
    assert!(
        fixture
            .turn
            .record(system_notices)
            .iter()
            .any(|line| line.contains("Engram refused")
                && line.contains("execution_observation_policy_basis_mismatch"))
    );
}

#[test]
fn unknown_capture_failure_still_exhausts_with_named_recovery_guidance() {
    let mut fixture = ReportingTurn::new("unknown-store-failure", true, ReplyMode::Recorded, false);
    let mut job = close_without_worker(&mut fixture);
    job.attempts = ENGRAM_RUNTIME_OBSERVATION_MAX_ATTEMPTS - 1;
    fixture
        .turn
        .record_mut(|record| record.engram.unadmitted_closing_jobs[0] = job.clone());
    fixture
        .turn
        .state
        .install_runtime_closing_result(
            &fixture.turn.session_id,
            &job,
            Err(EngramTransportError::transport(
                "private unknown producer failure",
            )),
        )
        .unwrap();
    fixture.turn.record(|record| {
        let failed = &record.engram.unadmitted_closing_jobs[0];
        assert!(failed.terminal_failure.is_some());
        assert_eq!(failed.trace, job.trace);
        assert_eq!(failed.scope, job.scope);
    });
    let notices = fixture.turn.record(system_notices).join("\n");
    assert!(notices.contains("runtime_observation_retry_exhausted"));
    assert!(notices.contains("No observation was sent."));
    assert!(notices.contains("The original closing facts and error remain retained locally."));
    assert!(notices.contains("Check the Engram producer and store"));
    assert!(!notices.contains("private unknown producer failure"));
    assert!(fixture.producer.requests().is_empty());
}

#[test]
fn typed_store_refusals_survive_cli_and_readiness_decoding_without_parsing_prose() {
    for code in [
        "projection_repair_required",
        "work_projection_invalid",
        "store_open_refused",
    ] {
        let value = json!({"schema_version":1,"scope":"readiness","ready":false,
            "mutation_enabled":false,"full_audit":"not_run","code":code,"kind":"busy","phase":"open",
            "project_id":"fixture-project","database":"/fixture/engram.db"});
        let bytes = serde_json::to_vec(&value).unwrap();
        let refusal = engram_readiness_store_refusal(&bytes).unwrap();
        assert_eq!(refusal.code, code);
        assert_eq!(
            refusal.refusal_kind,
            (code == "store_open_refused").then_some("busy")
        );
        assert_eq!(refusal.project_id, "fixture-project");
        assert_eq!(
            refusal.database.as_deref(),
            Some(FsPath::new("/fixture/engram.db"))
        );
        for field in ["project_id", "database"] {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                engram_readiness_store_refusal(&serde_json::to_vec(&missing).unwrap()).is_none()
            );
        }
        for phase in [
            None,
            Some(json!(1)),
            Some(json!("resolve")),
            Some(json!("path")),
        ] {
            let mut invalid = value.clone();
            if let Some(phase) = phase {
                invalid["phase"] = phase;
            } else {
                invalid.as_object_mut().unwrap().remove("phase");
            }
            assert!(
                engram_readiness_store_refusal(&serde_json::to_vec(&invalid).unwrap()).is_none(),
                "{code} accepted an invalid readiness phase: {invalid}"
            );
        }
        for kind in ["permission", "path_policy"] {
            let mut permanent = value.clone();
            permanent["code"] = json!("store_open_refused");
            permanent["kind"] = json!(kind);
            assert!(
                engram_readiness_store_refusal(&serde_json::to_vec(&permanent).unwrap()).is_none()
            );
        }
        let cli = |exit| EngramCliOutput {
            success: false,
            exit,
            status: "fixture refusal".into(),
            stdout: serde_json::to_vec(&json!({"error":{"code":code,"message":"repair pending"}}))
                .unwrap(),
            stderr: Vec::new(),
        };
        assert!(!engram_store_transient(
            &decode_engram_cli_json(cli(EngramCliExit::Code(1)), "fixture held claims")
                .unwrap_err()
        ));
        let mut stderr = cli(EngramCliExit::Code(1));
        stderr.stderr = std::mem::take(&mut stderr.stdout);
        assert_eq!(
            engram_store_transient(
                &decode_engram_cli_json(stderr.clone(), "fixture held claims").unwrap_err()
            ),
            code != "store_open_refused"
        );
        // The shared decoder's Code(101) contract still needs one whole JSON
        // refusal, not panic prose, and makes no claim about write application.
        let mut code_101 = stderr.clone();
        code_101.exit = EngramCliExit::Code(101);
        assert_eq!(
            engram_store_transient(
                &decode_engram_cli_json(code_101.clone(), "fixture held claims").unwrap_err()
            ),
            code != "store_open_refused"
        );
        code_101.stderr = [
            b"thread 'fixture' panicked at a fixture:\n".as_slice(),
            code_101.stderr.as_slice(),
        ]
        .concat();
        assert!(!engram_store_transient(
            &decode_engram_cli_json(code_101, "fixture held claims").unwrap_err()
        ));
        stderr.exit = EngramCliExit::Abnormal;
        assert!(!engram_store_transient(
            &decode_engram_cli_json(stderr, "fixture held claims").unwrap_err()
        ));
        let mut ready = value;
        ready["ready"] = json!(true);
        assert!(engram_readiness_store_refusal(&serde_json::to_vec(&ready).unwrap()).is_none());
    }
    assert!(engram_readiness_store_refusal(b"projection_repair_required").is_none());
    assert!(
        engram_readiness_store_refusal(&vec![b' '; ENGRAM_DIAGNOSTIC_REPORT_LIMIT + 1]).is_none()
    );
    assert!(!engram_store_transient(&EngramTransportError::transport(
        "projection_repair_required"
    )));
    for kind in [
        None,
        Some("permission"),
        Some("path_policy"),
        Some("busy"),
        Some("io"),
    ] {
        let mut error = EngramTransportError::remote(EngramControlErrorBody {
            code: "store_open_refused".into(),
            message: "busy io projection_repair_required".into(),
        });
        error.refusal_kind = kind.map(str::to_owned);
        assert!(!engram_store_transient(&error));
        error.code = Some("permanent_failure".into());
        assert!(!engram_store_transient(&error));
    }
}

fn readiness_fixture_connection(home: &FsPath, project_id: &str) -> EngramConnectionConfig {
    fs::create_dir_all(home).unwrap();
    let marker = home.join(".engram-project");
    fs::write(&marker, format!("{project_id}\n")).unwrap();
    EngramConnectionConfig {
        binary_path: real_engram_control_fixture_path(),
        project_file: marker,
        home: home.to_path_buf(),
        project_root: home.to_path_buf(),
        actor_id: "fixture".into(),
        actor_context: None,
        session_id: "repair-policy-reader".into(),
    }
}

fn readiness_fixture_store(home: &FsPath, project_id: &str) -> EngramAuthorityStoreKey {
    let database = work_database_path(home, project_id);
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    fs::write(&database, "fixture database").unwrap();
    EngramAuthorityStoreKey {
        database_path: normalize_user_facing_path(&fs::canonicalize(database).unwrap()),
        project_id: project_id.into(),
    }
}

#[test]
fn observation_policy_reader_preserves_a_real_readiness_process_repair_refusal() {
    for (mode, code, kind, transient) in [
        (
            "fixture-readiness-repair",
            Some("projection_repair_required"),
            None,
            true,
        ),
        (
            "fixture-readiness-store-busy",
            Some("store_open_refused"),
            Some("busy"),
            true,
        ),
        (
            "fixture-readiness-store-io",
            Some("store_open_refused"),
            Some("io"),
            false,
        ),
        (
            "fixture-readiness-path-io",
            Some("store_open_refused"),
            Some("io"),
            false,
        ),
        ("fixture-readiness-store-permission", None, None, false),
    ] {
        let state = test_app_state();
        let root = state
            .test_temp_root
            .as_ref()
            .unwrap()
            .path()
            .join("repair-policy");
        let connection = readiness_fixture_connection(&root, mode);
        let store = readiness_fixture_store(&root, mode);
        let error =
            read_engram_observation_policy(&connection, &store, ENGRAM_MIN_CALL_TIMEOUT, &root)
                .unwrap_err();
        assert_eq!(engram_store_transient(&error), transient, "{mode}");
        assert_eq!(error.refusal_code.as_deref(), code);
        assert_eq!(error.refusal_kind.as_deref(), kind);
        assert_eq!(
            fs::read_to_string(root.join("diagnostic-commands")).unwrap(),
            "readiness\n"
        );
    }
}

#[test]
fn prepared_delivery_only_waits_for_readiness_refusals_from_its_pinned_store() {
    let mut fixture = ReportingTurn::new(
        "readiness-refusal-identity",
        true,
        ReplyMode::Recorded,
        false,
    );
    let job = close_without_worker(&mut fixture);
    let prepared = ready_report(&fixture, job);
    let now = chrono::DateTime::parse_from_rfc3339(&prepared.retry_at)
        .unwrap()
        .with_timezone(&chrono::Utc);
    // Each case reuses the prepared request. The marker and selected home
    // model what the real readiness subprocess sees on a later replay.
    for (case, mode, wait) in [
        ("match-busy", "fixture-readiness-store-busy", true),
        ("match-repair", "fixture-readiness-repair", true),
        ("changed-marker-busy", "fixture-readiness-store-busy", false),
        ("changed-marker-repair", "fixture-readiness-repair", false),
        ("changed-marker-io", "fixture-readiness-store-io", false),
        ("changed-home-io", "fixture-readiness-store-io", false),
        (
            "foreign-database-field",
            "fixture-readiness-other-database-io",
            false,
        ),
        ("missing-route", "fixture-readiness-path-io", false),
    ] {
        let home = fixture.turn.root.join("readiness").join(case);
        let connection = readiness_fixture_connection(&home, mode);
        let mut report = prepared.clone();
        report.connection = connection;
        report.store = match case {
            "missing-route" => EngramAuthorityStoreKey {
                project_id: mode.into(),
                database_path: normalize_user_facing_path(&work_database_path(&home, mode)),
            },
            "changed-home-io" => {
                readiness_fixture_store(&home, mode);
                readiness_fixture_store(&home.join("prior"), mode)
            }
            _ => {
                let selected = readiness_fixture_store(&home, mode);
                if case.starts_with("changed-marker-") {
                    readiness_fixture_store(&home, "fixture-prior-project")
                } else {
                    selected
                }
            }
        };
        report.attempts = ENGRAM_RUNTIME_OBSERVATION_MAX_ATTEMPTS - 2;
        let adapter = EngramHostAdapter::new(home.clone());
        let (mut next, unavailable) =
            deliver_runtime_observation_attempt(&adapter, &report, 0, ENGRAM_MIN_CALL_TIMEOUT)
                .unwrap();
        assert_eq!(unavailable, wait, "{case}");
        assert!(
            matches!(
                &next.envelopes[0].delivery,
                EngramRuntimeObservationDelivery::Pending { last_error: Some(error) }
                    if error.contains("Engram readiness unavailable or refused")
            ),
            "{case}: an unmatched I/O refusal must not settle the report"
        );
        finish_runtime_observation_delivery_attempt_at(
            &mut next,
            &fixture.turn.session_id,
            0,
            unavailable,
            now,
        );
        assert_eq!(next.attempts, report.attempts + u32::from(!wait), "{case}");
        assert_eq!(next.store_wait_notice_posted, wait, "{case}");
        assert_eq!(next.envelopes[0].request, prepared.envelopes[0].request);
        assert_eq!(
            fs::read_to_string(home.join("diagnostic-commands")).unwrap(),
            "readiness\n",
            "{case}",
        );
    }
}
