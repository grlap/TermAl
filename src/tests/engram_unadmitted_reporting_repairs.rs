//! Regression interleavings for scope retention and old-host retirement.
//! Uses the actual worker, session SQLite row and terminal hooks. No clock
//! sleeps or relaxed deadlines stand in for the ordering barriers.
use super::*;

struct InstallBarrier {
    release: Arc<(Mutex<bool>, Condvar)>,
    entered: mpsc::Receiver<()>,
    stopping: mpsc::Receiver<()>,
    idle: mpsc::Receiver<()>,
}

impl InstallBarrier {
    fn new(state: &AppState, phase: &'static str) -> Self {
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let gate = release.clone();
        let once = std::sync::atomic::AtomicBool::new(false);
        let (entered, entered_rx) = mpsc::channel();
        let (stopping, stopping_rx) = mpsc::channel();
        let (idle, idle_rx) = mpsc::channel();
        *state
            .runtime_observation_lifetime
            .install_hook
            .lock()
            .unwrap() = Some(Arc::new(move |event| {
            if event == "stopping" {
                let _ = stopping.send(());
            }
            if event == "idle" {
                let _ = idle.send(());
            }
            if event == phase && !once.swap(true, std::sync::atomic::Ordering::AcqRel) {
                entered.send(()).unwrap();
                let released = gate.0.lock().unwrap();
                let (released, timeout) = gate
                    .1
                    .wait_timeout_while(released, DEADLOCK_GUARD, |ready| !*ready)
                    .unwrap();
                assert!(
                    *released && !timeout.timed_out(),
                    "test must release the post-check install barrier"
                );
            }
        }));
        Self {
            release,
            entered: entered_rx,
            stopping: stopping_rx,
            idle: idle_rx,
        }
    }

    fn release(&self) {
        *self.release.0.lock().unwrap() = true;
        self.release.1.notify_all();
    }
}

impl Drop for InstallBarrier {
    fn drop(&mut self) {
        self.release();
    }
}

fn stored_session(state: &AppState, session: &str) -> String {
    let db = open_sqlite_state_read_connection(&state.persistence_path).unwrap();
    db.query_row(
        "SELECT value_json FROM sessions WHERE id = ?1",
        rusqlite::params![session],
        |row| row.get(0),
    )
    .unwrap()
}

fn observed_bash(fixture: &mut ReportingTurn, key: &str, command: &str, output: &str, failed: bool) {
    observed_bash_with_effect(fixture, key, command, output, failed, |_| {});
}

fn observed_bash_with_effect(
    fixture: &mut ReportingTurn, key: &str, command: &str, output: &str, failed: bool,
    effect: impl FnOnce(&CheckedTurn),
) {
    fixture.reader.feed(json!({"type": "assistant", "message": {"content": [{
        "type": "tool_use", "id": key, "name": "Bash", "input": {"command": command}
    }]}}));
    effect(&fixture.turn);
    fixture.reader.feed(json!({"type": "user", "message": {"role": "user", "content": [{
        "type": "tool_result", "tool_use_id": key, "is_error": failed, "content": output
    }]}, "tool_use_result": {"stdout": output, "stderr": "", "interrupted": false}}));
}

#[test]
fn unadmitted_review_declared_artifact_not_shell_success_decides_result() {
    use super::super::super::declared_checks::{
        declare, write_artifact, DECLARED_ARTIFACT, DECLARED_TEST, PASSING_TRX, FAILING_TRX, ZERO_TRX,
    };
    for (name, artifact, expected) in [
        ("missing", None, "indeterminate"),
        ("unknown", Some(ZERO_TRX), "indeterminate"),
        ("failed", Some(FAILING_TRX), "failed"),
        ("passed", Some(PASSING_TRX), "passed"),
    ] {
        let mut fixture = ReportingTurn::new(&format!("unadmitted-declared-verdict-{name}"), true, ReplyMode::Recorded, false);
        declare(&fixture.turn, DECLARED_TEST, DECLARED_ARTIFACT);
        observed_bash_with_effect(&mut fixture, "declared-artifact", DECLARED_TEST, "process exited successfully", false,
            |turn| { if let Some(bytes) = artifact { write_artifact(turn, bytes); } });
        fixture.finish();
        let sent = fixture.observation();
        let checks = occurrence(&sent)["observed_checks"].as_array().unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0]["observed_result"], expected, "{name}: shell success is not the declared test verdict");
        assert_eq!(sent.receipt["observed_checks"][0]["credit"], "uncredited");
        fixture.assert_no_granted_reporting();
    }
}

#[test]
fn unadmitted_review_denied_test_is_not_an_observed_run_but_unknown_end_is() {
    let mut fixture = ReportingTurn::new("unadmitted-denied-check", true, ReplyMode::Recorded, false);
    observed_bash(&mut fixture, "denied-test", SIZE_TEST, "The user denied the requested permissions", true);
    observed_bash(&mut fixture, "denied-detached-test", "node scripts/test-launcher.mjs full --detach --notify session-next", "The user denied the requested permissions", true);
    // A real invocation with no terminal result is not a proved denial.
    fixture.reader.feed(json!({"type": "assistant", "message": {"content": [{
        "type": "tool_use", "id": "unknown-end", "name": "Bash", "input": {"command": SIZE_TEST}
    }]}}));
    let trace = fixture.turn.record(|record| record.engram.unadmitted_active_turn.clone().unwrap());
    assert_eq!(trace.commands.len(), 1, "proved denial must not be counted as a run");
    let expected_id = trace.commands.iter().find(|entry| entry.tool_key == "unknown-end").unwrap().check.host_check_id.clone();
    fixture.finish();
    let sent = fixture.observation();
    let checks = occurrence(&sent)["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["host_check_id"], expected_id);
    assert_eq!(checks[0]["observed_result"], "indeterminate");
    assert!(checks[0]["finished_at"].is_null());
    fixture.assert_no_granted_reporting();
}

#[test]
fn unadmitted_review_active_delete_without_connection_is_not_blocked() {
    active_delete_case("no-connection");
}

#[test]
fn unadmitted_review_active_delete_without_claim_is_not_blocked() {
    active_delete_case("no-claim");
}

#[test]
fn unadmitted_review_active_delete_with_claim_retains_custody_and_names_stop() {
    active_delete_case("claimed");
}

fn active_delete_case(scope: &str) {
        let mut fixture = ReportingTurn::new(&format!("unadmitted-active-delete-{scope}"), scope == "claimed", ReplyMode::Recorded, false);
        if scope == "no-connection" {
            let project = fixture.turn.record(|record| record.session.project_id.clone().unwrap());
            let mut inner = fixture.turn.state.inner.lock().unwrap();
            inner.projects.iter_mut().find(|entry| entry.id == project).unwrap().engram = None;
        }
        let original = fixture.turn.record(|record| record.engram.unadmitted_active_turn.clone().unwrap());
        let deletion = fixture.turn.state.kill_session(&fixture.turn.session_id);
        if scope == "claimed" {
            let error = deletion.err().expect("claimed active custody must retain the session");
            assert_eq!(error.status, axum::http::StatusCode::CONFLICT);
            assert!(error.message.contains("Stop"), "an active turn requires Stop, not tracking Resume: {}", error.message);
            assert!(!error.message.contains("Resume"));
            assert_eq!(fixture.turn.record(|record| record.engram.unadmitted_active_turn.clone()), Some(original.clone()));
            let durable: Value = serde_json::from_str(&stored_session(&fixture.turn.state, &fixture.turn.session_id)).unwrap();
            assert_eq!(durable["engramUnadmittedActiveTurn"], serde_json::to_value(&original).unwrap());
            let resume = fixture.turn.state.resume_source_observation_delete(&fixture.turn.session_id).unwrap_err();
            assert!(resume.message.contains("Stop"));
            assert!(fixture.producer.requests().is_empty());
            fixture.finish();
            assert_eq!(occurrence(&fixture.observation())["host_turn_ref"], original.host_turn_ref);
        } else {
            deletion.unwrap();
            assert!(fixture.turn.state.inner.lock().unwrap().find_session_index(&fixture.turn.session_id).is_none(), "{scope}: unreportable active trace must not prevent Delete");
            assert!(fixture.producer.requests().is_empty());
        }
        fixture.turn.state.trigger_shutdown_signal();
        fixture.turn.state.shutdown_persist_blocking();
}

#[test]
fn deletion_after_adopted_report_discharge_settles_only_the_leftover_grant() {
    use super::super::super::claude_adopted_terminalization::{
        assert_leftover_untouched, assert_settled_without_adopted_report, checkpoint_requests,
        LEFTOVER,
    };

    let mut fixture = ReportingTurn::start_with_leftover(
        "adopted-deletion", true, ReplyMode::Recorded, false, true, Some(LEFTOVER),
    );
    fixture.check("adopted-check", false);
    let (original, binding, generation) = fixture.turn.record(|record| (
        record.engram.unadmitted_active_turn.clone().unwrap(),
        record.engram.work_binding.clone().unwrap(),
        record.active_turn_generation,
    ));
    let before = checkpoint_requests(&fixture.turn).len();
    let error = fixture.turn.state.kill_session(&fixture.turn.session_id).err()
        .expect("active claimed capture must retain the session");
    assert_eq!(error.status, axum::http::StatusCode::CONFLICT);
    assert!(error.message.contains("Stop") && !error.message.contains("Resume"));
    fixture.turn.record(|record| {
        assert!(!record.hidden);
        assert_eq!(record.session.status, SessionStatus::Active);
        assert_eq!(record.active_turn_generation, generation);
        assert_eq!(record.engram.unadmitted_active_turn.as_ref(), Some(&original));
        assert_eq!(record.engram.work_binding.as_ref(), Some(&binding));
        assert!(record.engram.source_observation_delete_requested);
        assert!(record.orchestrator_auto_dispatch_blocked);
    });
    let durable: PersistedSessionRecord = serde_json::from_str(
        &stored_session(&fixture.turn.state, &fixture.turn.session_id),
    ).unwrap();
    assert_eq!(durable.engram_unadmitted_active_turn.as_ref(), Some(&original));
    assert!(durable.engram_source_observation_delete_requested);
    assert!(durable.orchestrator_auto_dispatch_blocked);
    assert_leftover_untouched(&fixture.turn, before, "active Delete retention");
    assert!(fixture.producer.requests().is_empty());

    let installed = InstallBarrier::new(&fixture.turn.state, "unused");
    fixture.finish();
    assert_leftover_untouched(&fixture.turn, before, "matching adopted result");
    let observed = fixture.observation();
    assert!(observed.durable_before_send);
    assert_eq!(occurrence(&observed)["kind"], "unadmitted_turn");
    assert_eq!(occurrence(&observed)["host_turn_ref"], original.host_turn_ref);
    assert_eq!(observed.request["binding"], serde_json::to_value(&binding).unwrap());
    assert_eq!(occurrence(&observed)["observed_checks"].as_array().unwrap().len(), 1);
    assert_eq!(observed.receipt["decision"], "recorded");
    let settled = loop {
        installed.idle.recv_timeout(DEADLOCK_GUARD).unwrap();
        let settled = fixture.turn.record(|record| {
            record.engram.unadmitted_reports.iter().find(|report| {
                report.host_turn_ref == original.host_turn_ref
                    && !report.envelopes.is_empty()
                    && report.envelopes.iter().all(|envelope| matches!(
                        envelope.delivery, EngramRuntimeObservationDelivery::Recorded { .. }
                    ))
            }).cloned()
        });
        if let Some(settled) = settled { break settled; }
    };
    assert!(runtime_observation_report_is_durable(
        &fixture.turn.state.persistence_path, &fixture.turn.session_id, &settled,
    ).unwrap());
    fixture.turn.record(|record| {
        assert_eq!(record.active_turn_generation, generation);
        assert!(record.engram.unadmitted_active_turn.is_none());
        assert!(record.engram.unadmitted_closing_jobs.is_empty());
        assert!(!runtime_observation_has_removal_duty(record));
    });
    // A recorded receipt alone is not the coupled local removal ACK.
    fixture.turn.state.confirm_source_observation_removal_durable(&fixture.turn.session_id).unwrap();
    let needs_tracking_recovery = {
        let inner = fixture.turn.state.inner.lock().unwrap();
        source_observation_removal_blocker(&inner, &[fixture.turn.session_id.clone()]).is_some()
    };
    if needs_tracking_recovery {
        assert!(fixture.turn.state.resume_source_observation_delete(&fixture.turn.session_id).unwrap());
    }
    assert_leftover_untouched(&fixture.turn, before, "observation discharge");
    fixture.assert_no_granted_reporting();
    fixture.turn.record(|record| assert!(record.orchestrator_auto_dispatch_blocked));
    assert!(matches!(fixture.turn._runtime_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));

    fixture.turn.state.kill_session(&fixture.turn.session_id).unwrap();
    assert_settled_without_adopted_report(&fixture.turn, before, "session deletion");
    assert!(fixture.turn.state.inner.lock().unwrap().find_session_index(&fixture.turn.session_id).is_none());
    assert_eq!(fixture.producer.requests().len(), 1, "Delete neither resends nor omits the adopted report");
    fixture.turn.state.trigger_shutdown_signal();
    fixture.turn.state.shutdown_persist_blocking();
}

#[test]
fn unadmitted_review_delete_retains_closing_and_uncertain_delivery_custody() {
    for phase in ["closing", "pending-reply"] {
        let mut fixture = ReportingTurn::new(&format!("unadmitted-delete-{phase}"), true, ReplyMode::Recorded, true);
        let barrier = InstallBarrier::new(&fixture.turn.state,
            if phase == "closing" { "capture" } else { "receipt-settlement-only" });
        fixture.finish();
        let request = if phase == "closing" {
            barrier.entered.recv_timeout(DEADLOCK_GUARD).unwrap();
            None
        } else {
            Some(fixture.observation().request)
        };
        let result = fixture.turn.state.kill_session(&fixture.turn.session_id);
        assert!(result.is_err(), "{phase}: Delete must retain unresolved observation custody");
        let retained = fixture.turn.record(PersistedSessionRecord::from_record);
        assert!(retained.engram_source_observation_delete_requested);
        assert!(retained.orchestrator_auto_dispatch_blocked);
        if let Some(request) = request {
            assert_eq!(retained.engram_unadmitted_reports[0].envelopes[0].request, request);
            let durable: Value = serde_json::from_str(&stored_session(&fixture.turn.state, &fixture.turn.session_id)).unwrap();
            assert_eq!(durable["engramUnadmittedReports"][0]["envelopes"][0]["request"], request);
        } else {
            assert_eq!(retained.engram_unadmitted_closing_jobs.len(), 1);
        }
        barrier.release();
        fixture.producer.release();
        // A retained session must not become a permanent deletion trap once
        // the original immutable request has a definitive recorded receipt.
        loop {
            barrier.idle.recv_timeout(DEADLOCK_GUARD).unwrap();
            let recorded = fixture.turn.record(|record| {
                !record.engram.unadmitted_reports.is_empty()
                    && record.engram.unadmitted_reports.iter().all(|report| {
                        report.envelopes.iter().all(|envelope| {
                            matches!(envelope.delivery, EngramRuntimeObservationDelivery::Recorded { .. })
                        })
                    })
            });
            if recorded { break; }
        }
        assert!(fixture.turn.state.resume_source_observation_delete(&fixture.turn.session_id).unwrap());
        fixture.turn.state.kill_session(&fixture.turn.session_id).unwrap();
        assert!(fixture.turn.state.inner.lock().unwrap().find_session_index(&fixture.turn.session_id).is_none());
        fixture.turn.state.trigger_shutdown_signal();
        fixture.turn.state.shutdown_persist_blocking();
    }
}

#[test]
fn unadmitted_review_terminal_custody_delete_resume_and_restart_stay_truthful() {
    for code in ["runtime_observation_retry_exhausted", "runtime_observation_invalid_retained_image"] {
        let mut fixture = ReportingTurn::new(code, true, ReplyMode::Recorded, false);
        let idle = InstallBarrier::new(&fixture.turn.state, "unused");
        for check in 0..17 { fixture.check(&format!("mixed-check-{check}"), false); }
        fixture.finish();
        fixture.observation();
        fixture.observation();
        idle.idle.recv_timeout(DEADLOCK_GUARD).unwrap();
        // Begin at the persisted terminal-outbox boundary. This is not a
        // claim that this fixture ran all eight delivery attempts.
        let expected = fixture.turn.record(|record| record.engram.unadmitted_reports[0].clone());
        let mut failed = expected.next_version();
        failed.envelopes[1].delivery = EngramRuntimeObservationDelivery::LocalFailure {
            code: code.to_owned(), message: "private retained transport detail".to_owned(),
        };
        fixture.turn.state.install_runtime_observation_result(
            &fixture.turn.session_id, &expected, failed.clone(), 1,
        ).unwrap();
        let requests = fixture.producer.requests();
        for restart in [false, true] {
            if restart {
                *fixture.turn.state.inner.lock().unwrap() = load_state(&fixture.turn.state.persistence_path).unwrap().unwrap();
            }
            let error = fixture.turn.state.kill_session(&fixture.turn.session_id).err().expect("terminal custody must keep the session");
            let resume = fixture.turn.state.resume_source_observation_delete(&fixture.turn.session_id).unwrap_err();
            for diagnostic in [&error.message, &resume.message] {
                assert!(diagnostic.contains(code), "{diagnostic}");
                assert!(diagnostic.contains(&failed.host_turn_ref), "{diagnostic}");
                assert!(diagnostic.contains("Resume cannot resolve this report"), "{diagnostic}");
                assert!(diagnostic.contains("automatic reporting has stopped"), "{diagnostic}");
                assert!(!diagnostic.contains("private retained transport detail"));
                assert!(!diagnostic.contains("routing_token"));
                if code == "runtime_observation_retry_exhausted" {
                    assert!(diagnostic.contains("Delivery outcome is unknown"), "{diagnostic}");
                } else {
                    assert!(diagnostic.contains("could not be validated"), "{diagnostic}");
                }
            }
            assert!(!fixture.turn.record(|record| record.engram.unadmitted_worker_running),
                "a terminal report must not schedule a new reporting worker");
            assert_eq!(fixture.turn.record(|record| record.engram.unadmitted_reports[0].clone()), failed);
            assert!(runtime_observation_report_is_durable(&fixture.turn.state.persistence_path,
                &fixture.turn.session_id, &failed).unwrap());
            assert_eq!(fixture.producer.requests(), requests, "Resume/restart cannot renew a terminal send");
        }
        fixture.turn.state.trigger_shutdown_signal();
        fixture.turn.state.shutdown_persist_blocking();
    }
}

#[test]
fn unadmitted_review_settlement_needs_durable_removal_ack_before_delete() {
    for mode in [ReplyMode::Recorded, ReplyMode::Refuse] {
        let mut fixture = ReportingTurn::new("unadmitted-settlement-fence", true, mode, false);
        let idle = InstallBarrier::new(&fixture.turn.state, "unused");
        fixture.finish();
        fixture.observation();
        idle.idle.recv_timeout(DEADLOCK_GUARD).unwrap();
        let settled = fixture.turn.record(|record| record.engram.unadmitted_reports[0].clone());
        // Persist the pre-settlement image, then install only the in-memory
        // settled image. A failed coupled ACK must not erase that sole request.
        {
            let mut inner = fixture.turn.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.turn.session_id).unwrap();
            inner.sessions[index].engram.unadmitted_reports[0].envelopes[0].delivery =
                EngramRuntimeObservationDelivery::Pending { last_error: Some("reply not yet durable".to_owned()) };
            inner.stamp_session_at_index(index);
            fixture.turn.state.persist_internal_locked(&inner).unwrap();
            inner.sessions[index].engram.unadmitted_reports[0] = settled.clone();
            inner.stamp_session_at_index(index);
        }
        let original_writer = fixture.turn.state.persist_tx.clone();
        let (tx, rx) = mpsc::channel();
        fixture.turn.state.persist_tx = tx;
        let state = fixture.turn.state.clone();
        let session = fixture.turn.session_id.clone();
        let deletion = std::thread::spawn(move || state.kill_session(&session));
        let mut fence_seen = false;
        while !deletion.is_finished() {
            if let Ok(PersistRequest::Fence(fence)) = rx.recv_timeout(Duration::from_millis(20)) {
                assert!(matches!(&fence.target, PersistFenceTarget::EngramSourceRemoval { .. }));
                fence_seen = true;
                fence.finish(Err(PersistFenceError::WriteFailed("controlled missing settlement ACK".to_owned())));
            }
        }
        let result = deletion.join().unwrap();
        fixture.turn.state.persist_tx = original_writer;
        assert!(fence_seen, "Delete must fence the exact settled report before removal");
        assert!(result.is_err(), "failed settlement ACK must keep the original session");
        assert_eq!(fixture.turn.record(|record| record.engram.unadmitted_reports[0].clone()), settled);
        fixture.turn.state.kill_session(&fixture.turn.session_id).unwrap();
        assert!(fixture.turn.state.inner.lock().unwrap().find_session_index(&fixture.turn.session_id).is_none());
        assert_eq!(fixture.producer.requests().len(), 1, "durable settlement needs no resend");
        fixture.turn.state.trigger_shutdown_signal();
        fixture.turn.state.shutdown_persist_blocking();
    }
}

#[test]
fn unadmitted_review_capture_absence_retains_exact_job_without_reporting() {
    let mut fixture = ReportingTurn::new("unadmitted-capture-absence", true, ReplyMode::Recorded, false);
    let (installed, received) = mpsc::channel();
    *fixture.turn.state.runtime_observation_lifetime.install_hook.lock().unwrap() = Some(Arc::new(move |phase| {
        if phase == "closing_installed" { let _ = installed.send(()); }
    }));
    fixture.turn.state.inner.lock().unwrap().test_engram_source_capture = Some(Arc::new(|_, budget| {
        assert_eq!(budget, Duration::from_millis(250), "fixture must not inflate the capture deadline");
        None
    }));
    let original = fixture.turn.record(|record| record.engram.unadmitted_active_turn.clone().unwrap());
    fixture.finish();
    let job = loop {
        received.recv_timeout(DEADLOCK_GUARD).unwrap();
        let job = fixture.turn.record(|record| record.engram.unadmitted_closing_jobs[0].clone());
        if job.attempts > 0 { break job; }
    };
    assert_eq!(job.trace, original);
    assert!(job.policy.is_some());
    assert_eq!(job.attempts, 1);
    assert_eq!(job.last_error.as_deref(), Some("runtime closing source could not be measured"));
    assert!(job.terminal_failure.is_none());
    assert!(fixture.turn.record(|record| record.engram.unadmitted_reports.is_empty()));
    assert!(fixture.producer.requests().is_empty(), "missing source is not an empty or invented observation");
    let durable: Value = serde_json::from_str(&stored_session(&fixture.turn.state, &fixture.turn.session_id)).unwrap();
    assert_eq!(durable["engramUnadmittedClosingJobs"][0], serde_json::to_value(&job).unwrap());
    fixture.assert_no_granted_reporting();
    fixture.turn.state.trigger_shutdown_signal();
    fixture.turn.state.shutdown_persist_blocking();
}

#[test]
fn unadmitted_review_detached_started_is_not_a_passing_finished_run() {
    let mut fixture = ReportingTurn::new("unadmitted-detached-outcome", true, ReplyMode::Recorded, false);
    observed_bash(
        &mut fixture,
        "detached-check",
        "node scripts/test-launcher.mjs full --detach --notify session-next",
        "STARTED review-runs/detached-check",
        false,
    );
    fixture.turn.record(|record| {
        let trace = record.engram.unadmitted_active_turn.as_ref().unwrap();
        assert_eq!(trace.commands.len(), 1);
        assert_eq!(trace.commands[0].check.observed_result, EngramObservedCheckResult::Indeterminate,
            "the launcher starting successfully is not the detached run passing");
        assert!(trace.commands[0].check.finished_at.is_none());
    });
    // A distinct, actually terminal failure remains a failure; it must not
    // turn the earlier launch's unknown result into a success or a failure.
    fixture.check("terminal-failure", true);
    fixture.finish();
    let sent = fixture.observation();
    let checks = occurrence(&sent)["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 2);
    assert_eq!(checks[0]["observed_result"], "indeterminate");
    assert!(checks[0]["finished_at"].is_null());
    assert_eq!(checks[1]["observed_result"], "failed");
    fixture.assert_no_granted_reporting();
}

#[test]
fn unadmitted_review_declared_only_command_keeps_one_uncredited_occurrence() {
    use super::super::super::declared_checks::{declare, DECLARED_ARTIFACT, DECLARED_TEST};
    let mut fixture = ReportingTurn::new("unadmitted-declared-only", true, ReplyMode::Recorded, false);
    declare(&fixture.turn, DECLARED_TEST, DECLARED_ARTIFACT);
    observed_bash(&mut fixture, "declared-check", DECLARED_TEST, "test process completed", false);
    let check_id = fixture.turn.record(|record| {
        let trace = record.engram.unadmitted_active_turn.as_ref().unwrap();
        assert_eq!(trace.commands.len(), 1,
            "the existing declared-command observer already recognized this test");
        trace.commands[0].check.host_check_id.clone()
    });
    fixture.finish();
    let sent = fixture.observation();
    let checks = occurrence(&sent)["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["host_check_id"], check_id);
    assert_eq!(checks[0]["check_kind"], "test");
    fixture.assert_no_granted_reporting();
}

#[test]
fn unadmitted_review_restart_settles_active_custody_before_next_adoption() {
    let mut fixture = ReportingTurn::new("unadmitted-active-recovery", true, ReplyMode::Recorded, false);
    fixture.check("before-restart", true);
    let persisted = fixture.turn.record(PersistedSessionRecord::from_record);
    let original = fixture.turn.record(|record| record.engram.unadmitted_active_turn.clone().unwrap());
    let mut restored = persisted.into_record().unwrap();
    assert_eq!(restored.adopted_claude_turn_generation, None);
    // This is the production boot recovery owner, before a replacement
    // runtime can adopt. Recovery must not invent a closing measurement.
    let notice = recover_interrupted_session_record(&mut restored).unwrap();
    assert!(restored.engram.unadmitted_active_turn.is_none(),
        "restored active custody must acquire a terminal recovery disposition");
    assert!(notice.text.contains("not reported") && notice.text.contains("restart"));
    push_message_on_record(&mut restored, Message::Text {
        attachments: Vec::new(), id: "restart-diagnostic".to_owned(), timestamp: stamp_now(),
        author: Author::Assistant, text: notice.text, expanded_text: notice.expanded_text, source: None,
    });
    let history = serde_json::to_string(&PersistedSessionRecord::from_record(&restored)).unwrap();
    assert!(history.contains(&original.host_turn_ref));
    assert!(history.contains(&original.commands[0].check.host_check_id));
    assert!(restored.engram.unadmitted_closing_jobs.is_empty());
    assert!(restored.engram.unadmitted_reports.is_empty());
    adopt_runtime_observation(&mut restored, "replacement-adoption-notice", 1);
    assert_ne!(restored.engram.unadmitted_active_turn.as_ref().unwrap().host_turn_ref, original.host_turn_ref);
    let history = serde_json::to_string(&PersistedSessionRecord::from_record(&restored)).unwrap();
    assert!(history.contains(&original.host_turn_ref), "next adoption must not erase recovered facts");
    fixture.assert_no_granted_reporting();
}

#[test]
fn unadmitted_review_actual_boot_archives_custody_before_new_router_adoption() {
    let _directory = TestTempRootDirectoryScope::repository_local();
    for status in [SessionStatus::Active, SessionStatus::Idle] {
        let mut fixture = ReportingTurn::new(
            &format!("unadmitted-boot-archive-{status:?}"), true, ReplyMode::Recorded, false,
        );
        fixture.check("pre-crash-check", true);
        let original = fixture.turn.record(|record| record.engram.unadmitted_active_turn.clone().unwrap());
        // Idle with active custody models a declined terminal callback at
        // shutdown; it needs recovery even without an ordinary busy status.
        fixture.turn.record_mut(|record| record.session.status = status);
        let old = fixture.turn.state.clone();
        assert!(old.persist_thread_handle.lock().unwrap().is_none());
        // CheckedTurn uses the shared synchronous-persist fixture, whose
        // shutdown has no writer to drain. Materialize the test's intended
        // crash image through the existing internal persist owner first;
        // record_mut stamps but does not itself request a write.
        old.persist_internal_locked(&old.inner.lock().unwrap()).unwrap();
        let before_boot: PersistedSessionRecord = serde_json::from_str(
            &stored_session(&old, &fixture.turn.session_id),
        ).unwrap();
        assert_eq!(before_boot.engram_unadmitted_active_turn.as_ref(), Some(&original),
            "the durable pre-boot image must include the actual completed check");
        old.trigger_shutdown_signal();
        old.shutdown_persist_blocking();
        fixture.turn.transport.responses.lock().unwrap().push_back(bind_reply("turn-check-token"));
        let restarted = restart_reporting_state(&fixture.turn.root, &old, fixture.producer.clone());
        fixture.turn.state = restarted.clone();
        let (active, archive, closing_count, report_count) = fixture.turn.record(|record| {
            let archive = record.session.messages.iter().find_map(|message| match message {
                Message::Text { text, expanded_text: Some(details), .. }
                    if text.contains(&original.host_turn_ref) && text.contains("not reported") => Some(details.clone()),
                _ => None,
            });
            (record.engram.unadmitted_active_turn.clone(), archive,
             record.engram.unadmitted_closing_jobs.len(), record.engram.unadmitted_reports.len())
        });
        // Assertions run after releasing the state mutex so a failed control
        // does not poison or strand the restarted host's real worker threads.
        assert!(active.is_none());
        let recovered: EngramRuntimeObservedTurn = serde_json::from_str(
            &archive.expect("real boot must append the recovery archive"),
        ).unwrap();
        assert_eq!(recovered, original);
        assert_eq!((closing_count, report_count), (0, 0));
        let (runtime, rx) = test_codex_runtime_handle("after-unadmitted-restart");
        fixture.turn._runtime_rx = rx;
        fixture.turn.record_mut(|record| {
            record.runtime = SessionRuntime::Codex(runtime);
            record.session.status = SessionStatus::Idle;
        });
        fixture.reader = ClaudeReaderOn::new(&fixture.turn, new_claude_turn_ownership());
        for frame in [task_notification(), init(), assistant_text("A distinct adopted turn.")] {
            fixture.reader.feed(frame);
        }
        let after_adoption = fixture.turn.record(PersistedSessionRecord::from_record);
        let next = after_adoption.engram_unadmitted_active_turn.as_ref().unwrap();
        assert_ne!(next.host_turn_ref, original.host_turn_ref);
        assert!(next.commands.is_empty());
        let history = serde_json::to_string(&after_adoption).unwrap();
        assert!(history.contains(&original.host_turn_ref));
        assert!(history.contains(&original.commands[0].check.host_check_id));
        assert!(fixture.producer.requests().is_empty(), "restart must not fabricate a closing report");
        restarted.trigger_shutdown_signal();
        restarted.shutdown_persist_blocking();
        // sessions.value_json is deliberately metadata-only: the archive is
        // a transcript body, stored separately. Read the complete durable
        // record through the same loader boot uses instead of searching a
        // metadata row for a message it must not contain.
        let connection = open_sqlite_state_read_connection(&restarted.persistence_path).unwrap();
        let records = load_session_records_from_sqlite(&connection, &restarted.persistence_path).unwrap();
        let durable = records.iter().find(|record| record.session.id == fixture.turn.session_id).unwrap();
        let archive = durable.session.messages.iter().find_map(|message| match message {
            Message::Text { text, expanded_text: Some(details), .. }
                if text.contains(&original.host_turn_ref) && text.contains("not reported") => Some(details),
            _ => None,
        }).expect("archive must survive the next durable transcript write");
        let durable_trace: EngramRuntimeObservedTurn = serde_json::from_str(archive).unwrap();
        assert_eq!(durable_trace, original);
        assert_eq!(durable.engram_unadmitted_active_turn, after_adoption.engram_unadmitted_active_turn);
        drop(connection);
        fixture.assert_no_granted_reporting();
    }
}

#[test]
fn unadmitted_review_invisible_closing_bookkeeping_keeps_public_revision() {
    let fixture = ReportingTurn::new("unadmitted-internal-revision", true, ReplyMode::Recorded, false);
    let state = fixture.turn.state.clone();
    let barrier = InstallBarrier::new(&state, "capture");
    let generation = fixture.turn.record(|record| record.adopted_claude_turn_generation);
    let before = state.inner.lock().unwrap().revision;
    state.close_runtime_observation(&fixture.turn.session_id, &fixture.reader.context.token, generation).unwrap();
    barrier.entered.recv_timeout(DEADLOCK_GUARD).unwrap();
    let after_close = state.inner.lock().unwrap().revision;
    assert_eq!(after_close, before,
        "invisible custody commits must not create a gap in the public SSE revision sequence");
    barrier.release();
    fixture.observation();
    // Policy pin and report installation also release the install guard.
    // Wait for the successful receipt's actual install, not an earlier idle.
    loop {
        barrier.idle.recv_timeout(DEADLOCK_GUARD).unwrap();
        let recorded = fixture.turn.record(|record| {
            record.engram.unadmitted_reports.iter().any(|report| {
                report.envelopes.iter().all(|envelope| {
                    matches!(envelope.delivery, EngramRuntimeObservationDelivery::Recorded { .. })
                })
            })
        });
        if recorded {
            break;
        }
    }
    let after_delivery = state.inner.lock().unwrap().revision;
    assert_eq!(after_delivery, before,
        "policy pin, report construction and successful delivery are also internal bookkeeping");
}

#[test]
fn retirement_joins_post_check_capture_and_delivery_installs_before_restore() {
    for phase in ["capture", "delivery"] {
        let mut fixture = ReportingTurn::new(
            &format!("unadmitted-retire-{phase}"),
            true,
            ReplyMode::Recorded,
            false,
        );
        let state = fixture.turn.state.clone();
        let barrier = InstallBarrier::new(&state, phase);
        fixture.finish();
        if phase == "delivery" {
            fixture.observation();
        }
        barrier.entered.recv_timeout(DEADLOCK_GUARD).unwrap();
        // The production hook is after registration and the stop check,
        // immediately before the install's state lock and durable commit.
        let retiring = state.clone();
        let (joined, join_rx) = mpsc::channel();
        let retire = std::thread::spawn(move || {
            retiring.trigger_shutdown_signal();
            retiring.shutdown_persist_blocking();
            joined.send(()).unwrap();
        });
        barrier.stopping.recv_timeout(DEADLOCK_GUARD).unwrap();
        assert_eq!(
            *state.runtime_observation_lifetime.installs.lock().unwrap(),
            1
        );
        assert!(
            matches!(join_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "retirement cannot pass an active installer"
        );
        barrier.release();
        join_rx.recv_timeout(DEADLOCK_GUARD).unwrap();
        retire.join().unwrap();
        assert_eq!(
            *state.runtime_observation_lifetime.installs.lock().unwrap(),
            0
        );
        assert!(state.persist_thread_handle.lock().unwrap().is_none());

        let restarted = restart_reporting_state(&fixture.turn.root, &state, fixture.producer.clone());
        {
            let mut inner = restarted.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.turn.session_id).unwrap();
            inner.sessions[index].session.name = format!("restored-host-{phase}");
            inner.stamp_session_at_index(index);
            restarted.commit_locked(&mut inner).unwrap();
        }
        restarted.shutdown_persist_blocking();
        let before = stored_session(&restarted, &fixture.turn.session_id);
        assert!(before.contains(&format!("restored-host-{phase}")));
        // Even an exact old-memory CAS must now refuse, including the
        // synchronous persistence fallback after its writer has been joined.
        if phase == "capture" {
            let job = fixture
                .turn
                .record(|record| record.engram.unadmitted_closing_jobs[0].clone());
            state
                .install_runtime_closing_result(
                    &fixture.turn.session_id,
                    &job,
                    Err(EngramTransportError::local_state("old host late capture")),
                )
                .unwrap();
        } else {
            let report = fixture
                .turn
                .record(|record| record.engram.unadmitted_reports[0].clone());
            state
                .install_runtime_observation_result(
                    &fixture.turn.session_id,
                    &report,
                    report.next_version(),
                    0,
                )
                .unwrap();
        }
        assert_eq!(stored_session(&restarted, &fixture.turn.session_id), before);
    }
}

#[test]
fn a_binding_change_after_close_is_visible_and_never_rescopes_the_report() {
    let mut fixture =
        ReportingTurn::new("unadmitted-closing-scope", true, ReplyMode::Recorded, false);
    let barrier = InstallBarrier::new(&fixture.turn.state, "capture");
    let original = fixture
        .turn
        .record(|record| record.engram.work_binding.clone().unwrap());
    fixture.check("old-work-check", false);
    fixture.finish();
    barrier.entered.recv_timeout(DEADLOCK_GUARD).unwrap();
    let replacement = test_control_work_binding("different-work-after-close", 9);
    fixture
        .turn
        .record_mut(|record| record.engram.work_binding = Some(replacement.clone()));
    *fixture.producer.binding.write().unwrap() = Some(replacement.clone());
    fixture
        .turn
        .transport
        .enable_named_roots(&fixture.turn.session_id, &replacement);
    barrier.release();
    barrier.idle.recv_timeout(DEADLOCK_GUARD).unwrap();
    assert!(
        fixture.producer.requests().is_empty(),
        "settled worker must never send the old trace to the new work"
    );
    let notices = fixture.turn.record(system_notices).join("\n");
    assert!(notices.contains("not reported: scope changed"), "{notices}");
    assert!(
        notices.contains(&original.work_id) && notices.contains(&replacement.work_id),
        "{notices}"
    );
    assert!(fixture.turn.record(|record| {
        record
            .engram
            .unadmitted_closing_jobs
            .iter()
            .all(|job| job.terminal_failure.is_some())
    }));
}

#[test]
fn runtime_checkpoint_survives_storage_without_extending_mediated_continuity() {
    let fixture = ReportingTurn::new(
        "unadmitted-checkpoint-storage",
        true,
        ReplyMode::Recorded,
        false,
    );
    let checkpoint = fixture
        .turn
        .record(|record| record.engram.runtime_checkpoint.clone().unwrap());
    let persisted = fixture.turn.record(PersistedSessionRecord::from_record);
    let wire = serde_json::to_value(&persisted).unwrap();
    assert!(wire.get("engramContinuityAnchor").is_none());
    assert_eq!(
        wire["engramRuntimeCheckpoint"],
        serde_json::to_value(&checkpoint).unwrap()
    );
    // Load through the same conversion the app uses, not a second reporting
    // implementation. Mediated continuity remains process-local.
    let decoded: PersistedSessionRecord = serde_json::from_value(wire).unwrap();
    let restored = decoded.into_record().unwrap();
    assert!(restored.engram.continuity_anchor.is_none());
    assert_eq!(restored.engram.runtime_checkpoint, Some(checkpoint));
}

fn retire_with_pinned_closing_job(fixture: &mut ReportingTurn) -> EngramRuntimeClosingJob {
    let old = fixture.turn.state.clone();
    let barrier = InstallBarrier::new(&old, "capture");
    fixture.finish();
    barrier.entered.recv_timeout(DEADLOCK_GUARD).unwrap();
    let (done, done_rx) = mpsc::channel();
    let retiring = old.clone();
    let retire = std::thread::spawn(move || {
        retiring.trigger_shutdown_signal();
        retiring.shutdown_persist_blocking();
        done.send(()).unwrap();
    });
    barrier.stopping.recv_timeout(DEADLOCK_GUARD).unwrap();
    barrier.release();
    done_rx.recv_timeout(DEADLOCK_GUARD).unwrap();
    retire.join().unwrap();
    let expected = fixture
        .turn
        .record(|record| record.engram.unadmitted_closing_jobs[0].clone());
    assert!(expected.policy.is_some(), "policy pin must drain before restore");
    let durable: PersistedSessionRecord =
        serde_json::from_str(&stored_session(&old, &fixture.turn.session_id)).unwrap();
    assert_eq!(durable.engram_unadmitted_closing_jobs, vec![expected.clone()]);
    expected
}

fn wait_for_observation_worker_idle(state: &AppState, session: &str) {
    let guard = phase_sync::PollGuard::new();
    loop {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(session).unwrap()];
        if !record.engram.unadmitted_worker_running {
            break;
        }
        drop(inner);
        guard.wait(format_args!("observation worker must defer or settle its selected work"));
    }
}

#[test]
fn restored_closing_r6_preparation_resume_keeps_unread_binding_pending() {
    let mut fixture = ReportingTurn::new(
        "unadmitted-preparation-unread-binding", true, ReplyMode::Recorded, false,
    );
    fixture.turn.record_mut(|record| record.set_auto_dispatch_blocked(true));
    queue_test_engram_prompt(&fixture.turn.state, &fixture.turn.session_id,
        "Retain this successor's exact preparation.", QueuedPromptSource::User, None);
    let target = AppState::engram_binding_target_for_session_shape_locked(
        &fixture.turn.state.inner.lock().unwrap(), &fixture.turn.session_id, true,
    ).unwrap().unwrap();
    let evaluate = fixture.turn.transport.requests().into_iter()
        .find(|entry| entry.request["operation"] == "turn_evaluate").unwrap().request;
    fixture.turn.record_mut(|record| {
        let head = record.queued_prompts.front_mut().unwrap();
        head.engram_interrupted = true;
        head.promotion_disposition_known = true;
        head.engram_evaluate = Some(EngramQueuedEvaluate {
            connection: target.connection.clone(), settings: target.settings.clone(),
            request: serde_json::from_value(evaluate).unwrap(), operation_generation: None,
            begun_grant_id: None, prepared_begin: None,
        });
    });
    let expected = retire_with_pinned_closing_job(&mut fixture);
    fixture.turn.transport.responses.lock().unwrap().push_back(
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::transport(
            "controlled unavailable boot bind",
        ))),
    );
    let old = &fixture.turn.state;
    let restarted = restart_reporting_state(&fixture.turn.root, old, fixture.producer.clone());
    install_reporting_source_capture(&restarted);
    wait_for_observation_worker_idle(&restarted, &fixture.turn.session_id);
    let prepared = EngramPreparedBegin {
        idempotency_key: "retained-preparation-after-boot".to_owned(),
        grant_id: CHECK_GRANT.to_owned(), delivery_tokens: Vec::new(),
        issued_no_later_than: stamp_now(), phase: EngramPreparedBeginPhase::Prepared,
        delivered_range: None,
    };
    let owner = {
        let mut inner = restarted.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.turn.session_id).unwrap();
        let record = &mut inner.sessions[index];
        assert!(!record.engram_boot_recovery_pending);
        assert!(record.engram.rebind_required);
        assert!(record.engram.work_binding.is_none());
        assert_eq!(record.engram.unadmitted_closing_jobs, vec![expected.clone()]);
        // Enter the existing local prepared-Begin acknowledgement hold after
        // degraded boot. This fixture does not send or acknowledge that Begin.
        let head = record.queued_prompts.front_mut().unwrap();
        head.engram_interrupted = true;
        head.engram_evaluate.as_mut().unwrap().prepared_begin = Some(prepared.clone());
        EngramQueuedAdmissionOwner::capture(record).unwrap()
    };
    restarted.note_engram_begin_preparation_hold(
        &fixture.turn.session_id, &owner, Some(prepared.grant_id.clone()),
    );
    {
        let inner = restarted.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.turn.session_id).unwrap()];
        assert!(matches!(engram_begin_preparation_resume(record),
            EngramBeginPreparationResume::ReleasablePending));
    }
    restarted.release_engram_begin_preparation_hold_for_resume(
        &fixture.turn.session_id, Some(&owner),
    ).unwrap().expect("the existing preparation owner releases this held head");
    let still_unread = {
        let inner = restarted.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.turn.session_id).unwrap()];
        assert!(record.engram.work_binding.is_none());
        record.engram.rebind_required
    };
    assert!(still_unread, "preparation Resume is not a binding recovery read");
    wait_for_observation_worker_idle(&restarted, &fixture.turn.session_id);
    {
        let inner = restarted.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.turn.session_id).unwrap()];
        assert_eq!(record.engram.unadmitted_closing_jobs, vec![expected]);
        assert!(record.engram.unadmitted_reports.is_empty());
        assert_eq!(record.queued_prompts[0].engram_evaluate.as_ref().unwrap()
            .prepared_begin.as_ref(), Some(&prepared));
        assert!(record.orchestrator_auto_dispatch_blocked);
    }
    assert!(fixture.producer.requests().is_empty());
    fixture.assert_no_granted_reporting();
    restarted.trigger_shutdown_signal();
    restarted.shutdown_persist_blocking();
}

#[test]
fn restored_closing_r6_definitively_unbound_binding_does_not_wait_forever() {
    let mut fixture = ReportingTurn::new(
        "unadmitted-definitively-unbound", true, ReplyMode::Recorded, false,
    );
    let expected = retire_with_pinned_closing_job(&mut fixture);
    *fixture.producer.binding.write().unwrap() = None;
    fixture.turn.transport.responses.lock().unwrap()
        .push_back(bind_reply(&expected.scope.routing_token));
    let old = &fixture.turn.state;
    let restarted = restart_reporting_state(&fixture.turn.root, old, fixture.producer.clone());
    install_reporting_source_capture(&restarted);
    wait_for_observation_worker_idle(&restarted, &fixture.turn.session_id);
    restarted.ensure_engram_session_bound_off_lock(&fixture.turn.session_id).unwrap().unwrap();
    wait_for_observation_worker_idle(&restarted, &fixture.turn.session_id);
    {
        let inner = restarted.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.turn.session_id).unwrap()];
        assert!(record.engram.routing_token.is_some());
        assert!(record.engram.work_binding.is_none());
        assert!(!AppState::runtime_observation_capture_waits_for_binding(record),
            "an actual no-claim bind is definitive, not an unread recovered binding");
        let job = &record.engram.unadmitted_closing_jobs[0];
        assert_eq!(job.scope, expected.scope);
        assert!(job.terminal_failure.as_ref().is_some_and(|cause| cause.contains("scope changed")));
        assert!(record.engram.unadmitted_reports.is_empty());
    }
    assert!(fixture.producer.requests().is_empty());
    fixture.assert_no_granted_reporting();
    restarted.kill_session(&fixture.turn.session_id).unwrap();
    restarted.trigger_shutdown_signal();
    restarted.shutdown_persist_blocking();
}

#[test]
fn restored_closing_r5_partial_bind_error_still_wakes_capture() {
    let mut fixture = ReportingTurn::new(
        "unadmitted-partial-bind-wake", true, ReplyMode::Recorded, false,
    );
    let runtime = fixture.turn.record(|record| record.runtime.clone());
    assert!(runtime.runtime_token().is_some());
    let expected = retire_with_pinned_closing_job(&mut fixture);
    fixture.turn.transport.responses.lock().unwrap()
        .push_back(bind_reply(&expected.scope.routing_token));
    let old = &fixture.turn.state;
    let restarted = restart_reporting_state(&fixture.turn.root, old, fixture.producer.clone());
    install_reporting_source_capture(&restarted);
    wait_for_observation_worker_idle(&restarted, &fixture.turn.session_id);
    let barrier = InstallBarrier::new(&restarted, "capture");
    let state = restarted.clone();
    let session = fixture.turn.session_id.clone();
    // The bind owns a snapshot with no runtime after restore. Change that
    // owner at the off-lock reply boundary, without starting a provider. The
    // bind still installs its claim, then root reconciliation rejects its
    // obsolete runtime snapshot with LocalState.
    *fixture.producer.request_hook.lock().unwrap() = Some(Arc::new(move |request| {
        if matches!(request, EngramControlRequest::SessionBind { .. }) {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].runtime = runtime.clone();
        }
    }));
    let bound = restarted.ensure_engram_session_bound_off_lock(&fixture.turn.session_id);
    *fixture.producer.request_hook.lock().unwrap() = None;
    let error = bound.err().expect("root reconciliation must reject the old runtime owner");
    assert_eq!(error.kind, EngramTransportErrorKind::LocalState);
    {
        let inner = restarted.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.turn.session_id).unwrap()];
        assert_eq!(record.engram.work_binding.as_ref(), Some(&expected.scope.binding));
        assert!(!AppState::runtime_observation_capture_waits_for_binding(record),
            "the partial bind installed the claim and cleared every capture fence");
        assert_eq!(record.engram.unadmitted_closing_jobs, vec![expected.clone()]);
    }
    barrier.entered.recv_timeout(DEADLOCK_GUARD)
        .expect("clearing the bind guard must wake eligible capture even after LocalState");
    barrier.release();
    let sent = fixture.observation();
    assert!(sent.durable_before_send);
    assert_eq!(occurrence(&sent)["host_turn_ref"], expected.trace.host_turn_ref);
    assert_eq!(fixture.producer.requests().len(), 1);
    fixture.assert_no_granted_reporting();
    restarted.trigger_shutdown_signal();
    restarted.shutdown_persist_blocking();
}

#[test]
fn restored_closing_r5_tracking_resume_binds_without_provider_replay() {
    let mut fixture = ReportingTurn::new(
        "unadmitted-lazy-delete-resume", true, ReplyMode::Recorded, true,
    );
    let expected = retire_with_pinned_closing_job(&mut fixture);
    fixture.turn.transport.responses.lock().unwrap()
        .push_back(bind_reply(&expected.scope.routing_token));
    let old = &fixture.turn.state;
    let restarted = restart_reporting_state(&fixture.turn.root, old, fixture.producer.clone());
    install_reporting_source_capture(&restarted);
    wait_for_observation_worker_idle(&restarted, &fixture.turn.session_id);
    {
        let inner = restarted.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.turn.session_id).unwrap()];
        assert!(record.engram.rebind_required);
        assert!(record.engram.work_binding.is_none());
        assert!(!record.engram_boot_recovery_pending);
        assert_eq!(record.engram.unadmitted_closing_jobs, vec![expected.clone()]);
    }
    assert!(restarted.kill_session(&fixture.turn.session_id).is_err(),
        "Delete must first retain the original closing custody");
    // Queue a successor only after lazy boot, behind Delete's pause latch.
    // Tracking Resume must recover the old history, never admit this prompt.
    queue_test_engram_prompt(&restarted, &fixture.turn.session_id,
        "Do not dispatch while recovering tracking.", QueuedPromptSource::User, None);
    let first_resume = restarted.resume_source_observation_delete(&fixture.turn.session_id);
    assert!(first_resume.is_err(), "the producer reply is still deliberately held");
    {
        let inner = restarted.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.turn.session_id).unwrap()];
        assert_eq!(record.engram.work_binding.as_ref(), Some(&expected.scope.binding),
            "explicit tracking Resume must recover the lazy binding without a provider turn");
        assert!(!record.engram.rebind_required);
        assert!(record.engram.source_observation_delete_requested);
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(record.session.queue_paused);
        assert_eq!(record.queued_prompts.len(), 1);
        assert!(!record.queued_prompts[0].has_engram_intent());
        assert!(record.engram.active_grant_id.is_none());
        assert!(record.runtime.runtime_token().is_none());
    }
    let sent = fixture.observation();
    assert!(sent.durable_before_send);
    assert_eq!(occurrence(&sent)["host_turn_ref"], expected.trace.host_turn_ref);
    assert!(restarted.kill_session(&fixture.turn.session_id).is_err(),
        "an uncertain reply still keeps the exact report");
    fixture.producer.release();
    wait_for_observation_worker_idle(&restarted, &fixture.turn.session_id);
    assert!(restarted.resume_source_observation_delete(&fixture.turn.session_id).unwrap());
    fixture.assert_no_granted_reporting();
    assert!(fixture.turn._runtime_rx.try_recv().is_err(), "tracking recovery sends no provider command");
    restarted.kill_session(&fixture.turn.session_id).unwrap();
    assert!(restarted.inner.lock().unwrap().find_session_index(&fixture.turn.session_id).is_none());
    assert_eq!(fixture.producer.requests().len(), 1);
    restarted.trigger_shutdown_signal();
    restarted.shutdown_persist_blocking();
}

#[test]
fn restored_closing_capture_waits_for_lazy_binding_recovery() {
    let mut fixture = ReportingTurn::new(
        "unadmitted-lazy-closing-rebind", true, ReplyMode::Recorded, false,
    );
    fixture.check("before-lazy-restart", false);
    let expected = retire_with_pinned_closing_job(&mut fixture);
    fixture.turn.transport.responses.lock().unwrap()
        .push_back(bind_reply(&expected.scope.routing_token));
    let operations_before = fixture.turn.transport.requests().len();
    let old = &fixture.turn.state;
    let restarted = restart_reporting_state(&fixture.turn.root, old, fixture.producer.clone());
    install_reporting_source_capture(&restarted);
    wait_for_observation_worker_idle(&restarted, &fixture.turn.session_id);
    {
        let inner = restarted.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.turn.session_id).unwrap()];
        assert!(record.engram.rebind_required);
        assert!(record.engram.work_binding.is_none());
        assert!(!record.engram_boot_recovery_pending,
            "an idle root without queued work remains lazily bound");
        assert_eq!(record.engram.unadmitted_closing_jobs, vec![expected.clone()],
            "unrecovered binding is not a scope change or a capture attempt");
        assert!(record.engram.unadmitted_reports.is_empty());
    }
    assert_eq!(fixture.turn.transport.requests().len(), operations_before,
        "observation custody must not make an idle root bind or dispatch at boot");
    assert!(fixture.producer.requests().is_empty());
    // Exercise the existing first-use binding owner, without evaluating or
    // dispatching a provider turn. Its completion must wake the retained job.
    restarted.ensure_engram_session_bound_off_lock(&fixture.turn.session_id)
        .unwrap().unwrap();
    {
        let inner = restarted.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.turn.session_id).unwrap()];
        assert_eq!(record.engram.work_binding.as_ref(), Some(&expected.scope.binding));
        assert!(!record.engram.rebind_required);
    }
    let sent = fixture.observation();
    assert!(sent.durable_before_send);
    assert_eq!(occurrence(&sent)["host_turn_ref"], expected.trace.host_turn_ref);
    assert_eq!(sent.request["binding"], serde_json::to_value(&expected.scope.binding).unwrap());
    assert_eq!(sent.request["policy_basis"], serde_json::to_value(expected.policy.as_ref().unwrap()).unwrap());
    assert_eq!(fixture.producer.requests().len(), 1);
    fixture.assert_no_granted_reporting();
    restarted.trigger_shutdown_signal();
    restarted.shutdown_persist_blocking();
}

#[test]
fn restored_closing_capture_survives_a_source_measurement_longer_than_the_rpc_cap() {
    // The restarted host's closing capture measures real Git and filesystem
    // contents, which a loaded machine can make slow. That time must not
    // spend the capture's RPC cap: the restarted state runs on the same kind
    // of clock as the state before the restart. The hook holds every
    // measurement for twice the whole cap, so a restart on real time fails
    // every attempt and backs off past the wait, while the fixture's clock
    // does not see the hold at all. It stands in for slow I/O, not for an
    // ordering barrier.
    let mut fixture = ReportingTurn::new(
        "unadmitted-slow-closing-measurement", true, ReplyMode::Recorded, false,
    );
    fixture.check("before-slow-measurement-restart", false);
    let expected = retire_with_pinned_closing_job(&mut fixture);
    fixture.turn.transport.responses.lock().unwrap()
        .push_back(bind_reply(&expected.scope.routing_token));
    let old = &fixture.turn.state;
    let restarted = restart_reporting_state(&fixture.turn.root, old, fixture.producer.clone());
    let cap = {
        let inner = restarted.inner.lock().unwrap();
        engram_project_for_session_locked(&inner, &fixture.turn.session_id)
            .and_then(|project| project.engram.as_ref())
            .map(|settings| settings.call_timeout())
            .expect("the fixture's project sets its Engram RPC cap")
    };
    assert!(cap < Duration::from_secs(1), "the fixture's RPC cap is a scripted short cap");
    let hold = cap * 2;
    restarted.inner.lock().unwrap().test_engram_source_capture =
        Some(Arc::new(move |place, _budget| {
            std::thread::sleep(hold);
            engram_place_source_basis(place)
        }));
    wait_for_observation_worker_idle(&restarted, &fixture.turn.session_id);
    restarted.ensure_engram_session_bound_off_lock(&fixture.turn.session_id)
        .unwrap().unwrap();
    let sent = fixture.observation();
    assert!(sent.durable_before_send);
    assert_eq!(occurrence(&sent)["host_turn_ref"], expected.trace.host_turn_ref);
    assert_eq!(sent.request["binding"], serde_json::to_value(&expected.scope.binding).unwrap());
    assert_eq!(fixture.producer.requests().len(), 1);
    fixture.assert_no_granted_reporting();
    restarted.trigger_shutdown_signal();
    restarted.shutdown_persist_blocking();
}

#[test]
fn restored_closing_capture_waits_past_eager_boot_budget() {
    let mut fixture = ReportingTurn::new(
        "unadmitted-late-closing-rebind", true, ReplyMode::Recorded, false,
    );
    fixture.turn.record_mut(|record| record.set_auto_dispatch_blocked(true));
    queue_test_engram_prompt(&fixture.turn.state, &fixture.turn.session_id,
        "Keep successor paused during late recovery.", QueuedPromptSource::User, None);
    let successor = fixture.turn.record(|record| record.queued_prompts[0].pending_prompt.id.clone());
    let expected = retire_with_pinned_closing_job(&mut fixture);
    fixture.turn.transport.responses.lock().unwrap()
        .push_back(bind_reply(&expected.scope.routing_token));
    let clock = EngramBudgetClock::scripted();
    let (entered, entered_rx) = mpsc::channel();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let gate = release.clone();
    // Hold the status phase, as the existing late-recovery fixture does.
    // The bind's own per-call deadline starts after status returns; consuming
    // only the coordinator budget must not also expire that later operation.
    *fixture.producer.request_hook.lock().unwrap() = Some(Arc::new(move |request| {
        if !matches!(request, EngramControlRequest::SessionStatus { .. }) {
            return;
        }
        let _ = entered.send(());
        let lock = gate.0.lock().unwrap();
        let (lock, timeout) = gate.1.wait_timeout_while(lock, DEADLOCK_GUARD, |ready| !*ready).unwrap();
        assert!(*lock && !timeout.timed_out(), "release the blocked rebind");
    }));
    // Release the test RPC gate even when a discriminator assertion fails.
    struct ReleaseBinding(Arc<(Mutex<bool>, Condvar)>);
    impl Drop for ReleaseBinding {
        fn drop(&mut self) {
            *self.0.0.lock().unwrap() = true;
            self.0.1.notify_all();
        }
    }
    let release_on_drop = ReleaseBinding(release);
    let old = &fixture.turn.state;
    let root = fixture.turn.root.to_string_lossy().into_owned();
    let path = old.persistence_path.as_ref().clone();
    let templates = old.orchestrator_templates_path.as_ref().clone();
    let producer = fixture.producer.clone();
    let boot_clock = clock.clone();
    let (boot_tx, boot_rx) = mpsc::channel();
    let boot = std::thread::spawn(move || {
        assert!(boot_tx.send(AppState::new_with_paths_engram_transport_and_clock_for_test(
            root, path, templates, producer, boot_clock,
        )).is_ok());
    });
    entered_rx.recv_timeout(DEADLOCK_GUARD).unwrap();
    clock.wait_for_scripted_waiter();
    assert!(boot_rx.try_recv().is_err(), "boot waits before its budget expires");
    clock.advance(Duration::from_millis(default_engram_boot_recovery_budget_ms()));
    let restarted = boot_rx.recv_timeout(DEADLOCK_GUARD).unwrap().unwrap();
    boot.join().unwrap();
    install_reporting_source_capture(&restarted);
    wait_for_observation_worker_idle(&restarted, &fixture.turn.session_id);
    {
        let inner = restarted.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.turn.session_id).unwrap()];
        assert!(record.engram_boot_recovery_pending);
        assert!(record.engram.work_binding.is_none());
        assert_eq!(record.engram.unadmitted_closing_jobs, vec![expected.clone()],
            "batch timeout cannot turn unfinished rebind into definitive scope loss");
        assert_eq!(record.queued_prompts[0].pending_prompt.id, successor);
        assert!(!record.queued_prompts[0].has_engram_intent());
        assert!(record.orchestrator_auto_dispatch_blocked);
    }
    assert!(fixture.producer.requests().is_empty());
    drop(release_on_drop);
    let sent = fixture.observation();
    assert!(sent.durable_before_send);
    assert_eq!(occurrence(&sent)["host_turn_ref"], expected.trace.host_turn_ref);
    assert_eq!(sent.request["binding"], serde_json::to_value(&expected.scope.binding).unwrap());
    assert_eq!(sent.request["policy_basis"], serde_json::to_value(expected.policy.as_ref().unwrap()).unwrap());
    assert_eq!(fixture.producer.requests().len(), 1);
    {
        let inner = restarted.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.turn.session_id).unwrap()];
        assert!(!record.engram_boot_recovery_pending);
        assert_eq!(record.queued_prompts[0].pending_prompt.id, successor);
        assert!(!record.queued_prompts[0].has_engram_intent());
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(record.engram.active_grant_id.is_none());
    }
    fixture.assert_no_granted_reporting();
    restarted.trigger_shutdown_signal();
    restarted.shutdown_persist_blocking();
}

#[tokio::test]
async fn a_restored_closing_job_keeps_its_original_scope_and_policy() {
    let mut fixture = ReportingTurn::new(
        "unadmitted-closing-restart",
        true,
        ReplyMode::Recorded,
        false,
    );
    // Settled idle roots intentionally bind lazily. A paused successor is a
    // real eager-boot obligation, without authorizing another turn to run.
    fixture.turn.record_mut(|record| record.set_auto_dispatch_blocked(true));
    queue_test_engram_prompt(
        &fixture.turn.state,
        &fixture.turn.session_id,
        "Keep this successor paused across closing-job recovery.",
        QueuedPromptSource::User,
        None,
    );
    let successor = fixture.turn.record(|record| {
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(record.session.queue_paused);
        assert_eq!(record.queued_prompts.len(), 1);
        assert!(!record.queued_prompts[0].has_engram_intent());
        record.queued_prompts[0].pending_prompt.id.clone()
    });
    let old = fixture.turn.state.clone();
    let expected = retire_with_pinned_closing_job(&mut fixture);
    let durable: PersistedSessionRecord =
        serde_json::from_str(&stored_session(&old, &fixture.turn.session_id)).unwrap();
    assert!(durable.orchestrator_auto_dispatch_blocked);
    assert_eq!(durable.queued_prompts.len(), 1);
    assert_eq!(durable.queued_prompts[0].pending_prompt.id, successor);
    assert!(!durable.queued_prompts[0].has_engram_intent(),
        "the paused successor must reach boot without an admission attempt");
    assert!(
        expected.policy.is_some(),
        "the guarded policy-pin commit drained before restore"
    );
    // The test constructor performs boot reconciliation and resumes reports
    // before returning. Supply its replacement claim and bind reply first,
    // so the worker observes the intended recovered scope on every ordering.
    let replacement = test_control_work_binding("rebound-after-closing-restart", 10);
    *fixture.producer.binding.write().unwrap() = Some(replacement.clone());
    fixture
        .turn
        .transport
        .responses
        .lock()
        .unwrap()
        .push_back(bind_reply(&expected.scope.routing_token));
    let restarted = restart_reporting_state(&fixture.turn.root, &old, fixture.producer.clone());
    let recovered_binding = {
        let inner = restarted.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.turn.session_id).unwrap();
        inner.sessions[index].engram.work_binding.clone()
    };
    assert_eq!(
        recovered_binding.as_ref(),
        Some(&replacement),
        "boot must recover the replacement claim before reporting resumes"
    );
    // Closing-job refusal publishes a state snapshot, not a delta. Subscribe
    // before checking the predicate so neither an early settlement nor its
    // subsequent snapshot can leave the waiter asleep.
    let mut events = restarted.subscribe_stream_events();
    restarted.resume_runtime_observation_reports();
    tokio::time::timeout(DEADLOCK_GUARD, async {
        loop {
            let terminal = {
                let inner = restarted.inner.lock().unwrap();
                let index = inner.find_session_index(&fixture.turn.session_id).unwrap();
                inner.sessions[index].engram.unadmitted_closing_jobs[0]
                    .terminal_failure
                    .is_some()
            };
            if terminal {
                break;
            }
            events.recv().await.unwrap();
        }
    })
    .await
    .expect("the restored scope mismatch must settle visibly");
    let (job, notices) = {
        let inner = restarted.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.turn.session_id).unwrap()];
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(record.session.queue_paused, "boot must not resume the successor");
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.queued_prompts[0].pending_prompt.id, successor);
        assert!(!record.queued_prompts[0].has_engram_intent());
        assert!(record.engram.active_grant_id.is_none());
        (
            record.engram.unadmitted_closing_jobs[0].clone(),
            system_notices(record),
        )
    };
    restarted.shutdown_persist_blocking();
    assert_eq!(job.trace, expected.trace);
    assert_eq!(job.baseline, expected.baseline);
    assert_eq!(job.scope, expected.scope);
    assert_eq!(job.policy, expected.policy);
    assert!(
        job.terminal_failure
            .as_ref()
            .unwrap()
            .contains("scope changed")
    );
    assert!(
        notices
            .iter()
            .any(|notice| notice.contains(&replacement.work_id)),
        "{:?}",
        notices
    );
    assert!(fixture.producer.requests().is_empty());
    fixture.assert_no_granted_reporting();
}

#[test]
fn metadata_only_projection_keeps_active_closing_and_retained_observation_state() {
    let mut fixture = ReportingTurn::new("unadmitted-metadata-projection", true, ReplyMode::Recorded, false);
    let assert_projection = |record: &SessionRecord| {
        let original = PersistedSessionRecord::from_record(record);
        let metadata = original.clone_without_messages();
        let original = serde_json::to_value(&original).unwrap();
        let metadata = serde_json::to_value(&metadata).unwrap();
        for field in ["engramRuntimeCheckpoint", "engramUnadmittedActiveTurn",
            "engramUnadmittedClosingJobs", "engramUnadmittedReports"] {
            assert_eq!(metadata.get(field), original.get(field),
                "metadata-only persistence must retain {field}");
        }
    };
    fixture.turn.record(|record| {
        assert!(record.engram.runtime_checkpoint.is_some());
        assert!(record.engram.unadmitted_active_turn.is_some());
        assert_projection(record);
    });
    let barrier = InstallBarrier::new(&fixture.turn.state, "capture");
    fixture.finish();
    barrier.entered.recv_timeout(DEADLOCK_GUARD).unwrap();
    fixture.turn.record(|record| {
        assert!(!record.engram.unadmitted_closing_jobs.is_empty());
        assert_projection(record);
    });
    barrier.release();
    fixture.observation();
    fixture.turn.record(|record| {
        assert!(!record.engram.unadmitted_reports.is_empty());
        assert_projection(record);
    });
}

#[test]
fn unadmitted_runtime_reporting_reader_floor_marker_guard_preserves_active_closing_and_pending_custody() {
    assert_runtime_custody_reader_refusal(None, true);
}

// This explicitly executed probe needs a hash-pinned repository-local binary.
// An ignored gate result is not rollback evidence.
#[cfg(windows)]
#[test]
#[ignore = "requires the pinned incompatible reader copy; run explicitly for custody evidence"]
fn unadmitted_runtime_reporting_pinned_reader_refuses_active_closing_and_pending_custody() {
    use crate::tests::federation_executor_store::verify_pinned_rollback_binary;
    let repository = fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap();
    let copy = fs::canonicalize(repository.join(".tmp/unadmitted-old-reader.exe"))
        .expect("provision the pinned reader evidence copy");
    assert!(copy.starts_with(repository.join(".tmp")),
        "never invoke an installed executable or an external reader path");
    let binary = verify_pinned_rollback_binary(&copy,
        "cba9ef654032bd0753f55bcbf74e14228138b459c2fb3ece27d264acacad4d12", 49_166_848);
    assert_runtime_custody_reader_refusal(Some(&binary), false);
}

fn assert_runtime_custody_reader_refusal(binary: Option<&FsPath>, migrate_outbox: bool) {
    let repository = fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap();
    let _directory = TestTempRootDirectoryScope::repository_local();
    let root = TestTempRoot::create("unadmitted-reader-custody");
    assert!(fs::canonicalize(root.path()).unwrap().starts_with(repository.join(".tmp")));
    let mut fixture = ReportingTurn::new("unadmitted-reader-states", true, ReplyMode::Recorded, true);
    let snapshot = |fixture: &ReportingTurn, phase: &str, field: &str| {
        let home = root.path().join(phase).join("isolated-home");
        let data = home.join(".termal");
        fs::create_dir_all(&data).unwrap();
        let path = data.join("termal.sqlite");
        // Initialize this independent destination before exporting the runtime
        // snapshot. The copy has its own confirmed predecessor, not the source
        // store's writer guard or a pre-initialization generation-zero claim.
        let connection = open_sqlite_state_connection_unconfigured(&path).unwrap();
        assert!(ensure_sqlite_state_schema_for_path_with_initialization(&connection, &path).unwrap());
        let generation = metadata_unit_generation(&connection).unwrap();
        assert_eq!(generation, 1);
        drop(connection);
        let (mut persisted, baseline_has_no_other_owner) = {
            let inner = fixture.turn.state.inner.lock().unwrap();
            let settled = inner.engram_session_obligations.checkpoint_operations.iter().all(|operation| {
                inner.engram_confirmed_containment_ids.contains(&operation.original.id)
            }) && inner.engram_pending_checkpoint_receipts.entries.is_empty()
                && inner.quarantined_persisted_session_ids.is_empty()
                && inner.sessions.iter().all(|record| {
                    !record.engram.checkpoint_in_progress
                        && record.engram.checkpoint_status_owner.is_none()
                        && record.engram.active_grant_id.is_none()
                        && record.engram.uncertain_grant_id.is_none()
                })
                && engram_admission_obligation_locked(&inner, &fixture.turn.session_id).is_none();
            (PersistedState::from_inner(&inner), settled)
        };
        let source_image = serde_json::to_value(&persisted).unwrap();
        persisted.metadata_capture = PersistMetadataCapture::new(
            Arc::new(std::sync::atomic::AtomicU64::new(generation)),
        );
        assert_eq!(serde_json::to_value(&persisted).unwrap(), source_image,
            "copy setup must not alter serialized custody or authority");
        if migrate_outbox {
            // The landed producer had neither the new ledger nor its mirror.
            // Project only this fixture's fully ACKed baseline on the disposable
            // copy; keep its complete current image and every older-format field.
            fs::write(data.join("current-format-snapshot.json"),
                serde_json::to_vec(&persisted).unwrap()).unwrap();
            assert!(baseline_has_no_other_owner,
                "predecessor projection must not omit live or unresolved checkpoint custody");
            let ledger = &persisted.engram_session_obligations;
            ledger.validate().unwrap();
            assert!(ledger.entries.is_empty(), "no linked independent obligation may be omitted");
            assert_eq!(ledger.checkpoint_operations.len(), 1, "only the known baseline is projected");
            let operation = &ledger.checkpoint_operations[0];
            operation.validate().unwrap();
            assert_eq!(operation.protection.local_session_id, fixture.turn.session_id);
            assert_eq!(operation.protection.phase, EngramSessionObligationPhase::Confirmed);
            assert!(operation.protection.authority_confirmed);
            assert!(matches!(operation.progress, EngramExistingCheckpointProgress::Accepted { .. }));
            assert_eq!(operation.attempts.len(), 1);
            assert!(matches!(operation.attempts[0].result, Some(EngramCheckpointAttemptResult::Accepted)));
            let baseline_requests: Vec<_> = fixture.turn.transport.requests().into_iter()
                .filter(|request| request.request["operation"] == "turn_checkpoint").collect();
            assert_eq!(baseline_requests.len(), 1);
            assert_eq!(baseline_requests[0].request["grant_id"], CHECK_GRANT);
            assert_eq!(serde_json::to_value(&operation.original.request).unwrap(), baseline_requests[0].request);
            for record in &mut persisted.sessions {
                if let Some(mirror) = &record.engram_checkpoint_operation {
                    assert_eq!(record.session.id, fixture.turn.session_id);
                    assert_eq!(serde_json::to_value(mirror).unwrap(), serde_json::to_value(operation).unwrap());
                }
                record.engram_checkpoint_operation = None;
            }
            persisted.engram_session_obligations = EngramSessionObligationLedger::empty();
            let mut expected = source_image.clone();
            expected["engramSessionObligations"] = serde_json::to_value(EngramSessionObligationLedger::empty()).unwrap();
            for record in expected["sessions"].as_array_mut().unwrap() {
                record.as_object_mut().unwrap().remove("engramCheckpointOperation");
            }
            assert_eq!(serde_json::to_value(&persisted).unwrap(), expected,
                "predecessor projection preserves all runtime checkpoint and observation custody");
        }
        persist_state_from_persisted(&path, &persisted).unwrap();
        let connection = open_sqlite_state_read_connection(&path).unwrap();
        let raw: String = connection.query_row("SELECT value_json FROM sessions WHERE id=?1",
            [&fixture.turn.session_id], |row| row.get(0)).unwrap();
        let encoded: Value = serde_json::from_str(&raw).unwrap();
        let value = encoded.get(field).expect("actual runtime custody must be present");
        assert!(!value.is_null() && value.as_array().is_none_or(|items| !items.is_empty()), "{phase}");
        (home, path, raw)
    };
    let active = snapshot(&fixture, "active", "engramUnadmittedActiveTurn");
    let barrier = InstallBarrier::new(&fixture.turn.state, "capture");
    fixture.finish();
    barrier.entered.recv_timeout(DEADLOCK_GUARD).unwrap();
    let closing = snapshot(&fixture, "closing", "engramUnadmittedClosingJobs");
    barrier.release();
    let observation = fixture.observation();
    assert!(observation.durable_before_send);
    let pending = snapshot(&fixture, "pending", "engramUnadmittedReports");
    let retained: Value = serde_json::from_str(&pending.2).unwrap();
    assert_eq!(retained["engramUnadmittedReports"][0]["envelopes"][0]["request"],
        observation.request, "the probe retains the actual immutable producer request");
    fixture.producer.release();
    fixture.turn.state.trigger_shutdown_signal();
    fixture.turn.state.shutdown_persist_blocking();

    for (home, path, raw) in [active, closing, pending] {
        if migrate_outbox {
            // Construct the exact landed format from actual runtime custody,
            // then exercise the production joint migration without a worker.
            let connection = rusqlite::Connection::open(&path).unwrap();
            let ledger = read_persisted_metadata_field(&connection, "engramSessionObligations").unwrap().unwrap();
            assert_eq!(ledger, serde_json::to_value(EngramSessionObligationLedger::empty()).unwrap());
            connection.execute("DELETE FROM state_metadata_units WHERE field_name='engramSessionObligations'", []).unwrap();
            connection.execute("UPDATE meta SET value=?1 WHERE key='schema_version'", [SQLITE_OBSERVATION_SCHEMA_VERSION]).unwrap();
            let original = read_metadata_units_unchecked(&connection).unwrap();
            let generation = metadata_unit_generation(&connection).unwrap();
            let capture = PersistMetadataCapture::new(Arc::new(std::sync::atomic::AtomicU64::new(generation)));
            ensure_sqlite_state_schema_for_load(&connection).unwrap();
            let mut expected = original.clone();
            expected.extend(metadata_value_to_units(json!({"engramSessionObligations": ledger})).unwrap());
            assert_eq!(read_metadata_units_unchecked(&connection).unwrap(), expected);
            assert_eq!(metadata_unit_generation(&connection).unwrap(), generation + 1);
            assert_eq!(connection.query_row("SELECT value_json FROM sessions WHERE id=?1",
                [&fixture.turn.session_id], |row| row.get::<_, String>(0)).unwrap(), raw,
                "active capture, closing job and pending request survive byte-for-byte");
            assert!(write_metadata_units(&connection, original, None, &[], Some(&capture), false).is_err());
            assert_eq!(read_metadata_units_unchecked(&connection).unwrap(), expected);
            ensure_sqlite_state_schema_for_load(&connection).unwrap();
            assert_eq!(metadata_unit_generation(&connection).unwrap(), generation + 1);
        }
        let connection = open_sqlite_state_read_connection(&path).unwrap();
        let metadata = read_metadata_units_unchecked(&connection).unwrap();
        let generation = metadata_unit_generation(&connection).unwrap();
        let changes = connection.total_changes();
        // This deterministic control checks marker incompatibility and
        // non-mutation through the parameterized validator in this build.
        // It is not predecessor-reader behavior evidence. Only the explicit
        // pinned-binary probe below exercises that application's startup.
        for old_floor in [SQLITE_OBSERVATION_PREDECESSOR, SQLITE_OBSERVATION_SCHEMA_VERSION] {
            let error = validate_sqlite_state_schema_for_version(&connection, old_floor).unwrap_err();
            assert!(format!("{error:#}").contains("unsupported state database schema"));
        }
        assert_eq!(connection.total_changes(), changes);
        drop(connection);
        #[cfg(windows)]
        if let Some(binary) = binary {
            let output = crate::tests::federation_executor_store::run_pinned_schema_opener(
                binary, root.path(), &home);
            assert!(!output.status.success(), "incompatible reader must refuse custody");
            let diagnostic = format!("{}{}", String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr));
            assert!(diagnostic.contains("unsupported state database schema"), "{diagnostic}");
            assert!(diagnostic.contains(SQLITE_SCHEMA_VERSION), "{diagnostic}");
            assert!(!diagnostic.contains("listening:"), "refusal must precede serving: {diagnostic}");
        }
        #[cfg(not(windows))]
        assert!(binary.is_none());
        let connection = open_sqlite_state_read_connection(&path).unwrap();
        assert_eq!(required_state_meta_value(&connection, "schema_version").unwrap(), SQLITE_SCHEMA_VERSION);
        assert_eq!(read_metadata_units_unchecked(&connection).unwrap(), metadata);
        assert_eq!(metadata_unit_generation(&connection).unwrap(), generation);
        assert_eq!(connection.query_row("SELECT value_json FROM sessions WHERE id=?1",
            [&fixture.turn.session_id], |row| row.get::<_, String>(0)).unwrap(), raw,
            "old application must not load and rewrite away custody");
        validate_current_sqlite_state_schema(&connection).unwrap();
        let loaded = load_session_records_from_sqlite(&connection, &path).unwrap();
        let loaded = loaded.iter().find(|record| record.session.id == fixture.turn.session_id).unwrap();
        let loaded = serde_json::to_value(loaded).unwrap();
        let expected: Value = serde_json::from_str(&raw).unwrap();
        for field in ["engramRuntimeCheckpoint", "engramUnadmittedActiveTurn",
            "engramUnadmittedClosingJobs", "engramUnadmittedReports"] {
            assert_eq!(loaded.get(field), expected.get(field), "new reader must preserve {field}");
        }
    }
    fixture.assert_no_granted_reporting();
    assert_eq!(fixture.producer.requests().len(), 1, "reader refusal creates no reporting or activation");
}

#[test]
fn a_malformed_retained_envelope_does_not_starve_a_later_valid_report() {
    let mut fixture = ReportingTurn::new("unadmitted-poison", true, ReplyMode::Recorded, false);
    let settled = InstallBarrier::new(&fixture.turn.state, "unused");
    fixture.finish();
    fixture.observation();
    settled.idle.recv_timeout(DEADLOCK_GUARD).unwrap();
    {
        let mut inner = fixture.turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.turn.session_id).unwrap();
        let mut good = inner.sessions[index].engram.unadmitted_reports[0].next_version();
        good.envelopes[0].delivery = EngramRuntimeObservationDelivery::Pending { last_error: None };
        let mut bad = good.clone();
        bad.host_turn_ref = "corrupt-retained-envelope".to_owned();
        bad.envelopes[0].request = json!({"invalid": "retained wire"});
        inner.sessions[index].engram.unadmitted_reports = vec![bad, good];
        inner.stamp_session_at_index(index);
        fixture.turn.state.commit_locked(&mut inner).unwrap();
    }
    fixture
        .turn
        .state
        .start_runtime_observation_worker(&fixture.turn.session_id);
    fixture.observation();
    settled.idle.recv_timeout(DEADLOCK_GUARD).unwrap();
    fixture.turn.record(|record| {
        assert!(
            matches!(&record.engram.unadmitted_reports[0].envelopes[0].delivery,
            EngramRuntimeObservationDelivery::LocalFailure { code, .. }
                if code == "runtime_observation_invalid_retained_image")
        );
        assert!(matches!(
            record.engram.unadmitted_reports[1].envelopes[0].delivery,
            EngramRuntimeObservationDelivery::Recorded { .. }
        ));
        assert!(
            system_notices(record)
                .iter()
                .any(|notice| notice.contains("runtime_observation_invalid_retained_image"))
        );
    });
}
