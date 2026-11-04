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
        Self::install(state, phase, None)
    }

    fn report_candidate(state: &AppState, session: &str) -> Self {
        Self::install(state, "capture", Some(session.to_owned()))
    }

    fn install(state: &AppState, phase: &'static str, report_owner: Option<String>) -> Self {
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let gate = release.clone();
        let inner = Arc::downgrade(&state.inner);
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
            let matches_report = report_owner.as_ref().is_none_or(|session| {
                event == "capture" && inner.upgrade().is_some_and(|inner| {
                    let inner = inner.lock().unwrap();
                    inner.find_session_index(session).is_some_and(|index| {
                        inner.sessions[index].engram.unadmitted_closing_jobs.iter()
                            .any(|job| job.policy.is_some())
                    })
                })
            });
            if event == phase && matches_report && !once.swap(true, std::sync::atomic::Ordering::AcqRel) {
                let _ = entered.send(());
                phase_sync::wait_for_gate(&gate, true);
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
        phase_sync::set_gate(&self.release, true);
    }
}

impl Drop for InstallBarrier {
    fn drop(&mut self) {
        self.release();
    }
}

pub(super) fn stored_session(state: &AppState, session: &str) -> String {
    let db = open_sqlite_state_read_connection(&state.persistence_path).unwrap();
    db.query_row(
        "SELECT value_json FROM sessions WHERE id = ?1",
        rusqlite::params![session],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn a_retained_hazard_before_report_install_is_sticky_after_its_work_ends_and_reload() {
    let mut fixture = ReportingTurn::new("retained-before-report-install", true, ReplyMode::Recorded, false);
    fixture.check("completed-before-hazard", false);
    // Join the existing writer before the interleaving. Later production
    // persistence calls use their synchronous fallback, so reload below
    // cannot race a queued write or add a test-side write that masks it.
    fixture.turn.state.shutdown_persist_blocking();
    fixture.turn.change_readme("ordinary source changed before the closing capture\n");
    let project = fixture.turn.record(|record| record.session.project_id.clone().unwrap());
    let other = create_test_project_session(&fixture.turn.state, Agent::Claude, &project, &fixture.turn.root);
    let first = InstallBarrier::report_candidate(&fixture.turn.state, &fixture.turn.session_id);
    fixture.finish();
    first.entered.recv_timeout(DEADLOCK_GUARD).unwrap();
    let original = fixture.turn.record(|record| record.engram.unadmitted_closing_jobs[0].clone());
    assert!(original.policy.is_some());
    assert!(!original.trace.source_attribution_uncertain);
    let second = InstallBarrier::report_candidate(&fixture.turn.state, &fixture.turn.session_id);
    {
        let mut inner = fixture.turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&other).unwrap();
        inner.sessions[index].claude_outstanding.register(ClaudeOutstandingEntry {
            token: RuntimeToken::Claude("retained-install-gone".to_owned()),
            key: "orphaned".to_owned(),
            origin: ClaudeWorkOrigin::Unattributed,
            background: false,
            nested: false,
            self_gate: false,
            locations: vec![Some(engram_worktree_root(&fixture.turn.root))],
            registered_at: EngramHost::interference_tick(),
        });
    }
    first.release();
    second.entered.recv_timeout(DEADLOCK_GUARD).unwrap();
    let changed = fixture.turn.record(|record| record.engram.unadmitted_closing_jobs[0].clone());
    assert_eq!(changed.trace.host_turn_ref, original.trace.host_turn_ref);
    assert!(changed.trace.source_attribution_uncertain,
        "the stale off-lock sourceful candidate must be discarded");
    let loaded = load_state(fixture.turn.state.persistence_path.as_path()).unwrap().unwrap();
    let index = loaded.find_session_index(&fixture.turn.session_id).unwrap();
    assert!(loaded.sessions[index].engram.unadmitted_closing_jobs.iter().any(|job|
        job.trace.host_turn_ref == original.trace.host_turn_ref && job.trace.source_attribution_uncertain),
        "the sticky restriction is durable before a replacement candidate installs");
    {
        let mut inner = fixture.turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&other).unwrap();
        inner.sessions[index].claude_outstanding = ClaudeOutstandingWork::default();
    }
    second.release();
    let sent = fixture.observation();
    let facts = occurrence(&sent);
    assert_eq!(facts["host_turn_ref"], original.trace.host_turn_ref);
    assert!(facts["source_change"].is_null(), "ending work cannot restore discarded source credit");
    let checks = facts["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["observed_result"], "passed");
    assert_eq!(sent.receipt["observed_checks"][0]["credit"], "uncredited");
    assert!(sent.durable_before_send);
    fixture.assert_no_granted_reporting();
}

fn observed_bash(fixture: &mut ReportingTurn, key: &str, command: &str, output: &str, failed: bool) {
    observed_bash_with_effect(fixture, key, command, output, failed, |_| {});
}

fn observed_bash_with_effect(
    fixture: &mut ReportingTurn, key: &str, command: &str, output: &str, failed: bool,
    effect: impl FnOnce(&EOCheckedTurn),
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

// A full synchronous save can hide a missing session stamp. Keep a live
// persist receiver and apply the production-selected delta to real SQLite.
#[test]
fn unadmitted_review_abandoned_command_drop_reaches_the_delta_persist() {
    let mut fixture = ReportingTurn::new("unadmitted-abandoned-delta", true, ReplyMode::Recorded, false);
    fixture.reader.feed(json!({"type": "assistant", "message": {"content": [{
        "type": "tool_use", "id": "abandoned-test", "name": "Bash", "input": {"command": SIZE_TEST}
    }]}}));
    let traced = |turn: &EOCheckedTurn| turn.record(|record| {
        record.engram.unadmitted_active_turn.as_ref().unwrap().commands.iter()
            .any(|entry| entry.tool_key == "abandoned-test")
    });
    assert!(traced(&fixture.turn), "the started command is in the adopted trace");
    let provenance = fixture.turn.record(|record| {
        assert_eq!(record.adopted_claude_turn_generation, Some(record.active_turn_generation));
        EngramObservationProvenance::Claude(ClaudeObservationProvenance {
            token: record.runtime.runtime_token().unwrap(),
            origin: ClaudeWorkOrigin::Adopted { turn_generation: record.active_turn_generation },
        })
    });
    fixture.turn.state.shutdown_persist_blocking();
    let (tx, _rx) = mpsc::channel();
    fixture.turn.state.persist_tx = tx;
    let watermark = fixture.turn.state.inner.lock().unwrap().last_mutation_stamp;
    let snapshot = crate::tests::session_stamp_delta::DurableSessionSnapshot::take(&fixture.turn.state);

    fixture.turn.state.note_engram_command_abandoned(&fixture.turn.session_id, &provenance, "abandoned-test");

    assert!(!traced(&fixture.turn), "the abandoned command left the trace");
    let delta = collect_persist_delta_from_shared_state(&fixture.turn.state.inner, watermark);
    let persisted = delta.changed_sessions.iter()
        .find(|record| record.session.id == fixture.turn.session_id)
        .expect("the trace change must be selected by the delta persist");
    assert!(!persisted.engram_unadmitted_active_turn.as_ref().unwrap().commands.iter()
        .any(|entry| entry.tool_key == "abandoned-test"),
        "the persisted trace no longer lists the abandoned command");
    snapshot.assert_changes_reach_the_delta(&fixture.turn.state);
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
            assert!(error.message.contains("Stop"), "an active turn requires Stop, not Resume: {}", error.message);
            assert!(!error.message.contains("Resume"));
            assert_eq!(fixture.turn.record(|record| record.engram.unadmitted_active_turn.clone()), Some(original.clone()));
            let durable: Value = serde_json::from_str(&stored_session(&fixture.turn.state, &fixture.turn.session_id)).unwrap();
            assert_eq!(durable["engramUnadmittedActiveTurn"], serde_json::to_value(&original).unwrap());
            let retry = fixture.turn.state.kill_session(&fixture.turn.session_id)
                .err().expect("a retried Delete keeps the claimed active custody");
            assert!(retry.message.contains("Stop") && !retry.message.contains("Resume"), "{}", retry.message);
            assert_eq!(fixture.turn.record(|record| record.engram.unadmitted_active_turn.clone()), Some(original.clone()));
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
fn deletion_after_adopted_report_discharge_never_replays_passive_grant_history() {
    let mut fixture = ReportingTurn::start_with_history(
        "adopted-deletion", true, ReplyMode::Recorded, false, true, Some("historical-open-grant"),
    );
    fixture.check("adopted-check", false);
    let (original, binding, generation) = fixture.turn.record(|record| (
        record.engram.unadmitted_active_turn.clone().unwrap(),
        record.engram.work_binding.clone().unwrap(),
        record.active_turn_generation,
    ));
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
        assert!(record.engram.observation_delete_requested);
        assert!(record.orchestrator_auto_dispatch_blocked);
    });
    let durable: PersistedSessionRecord = serde_json::from_str(
        &stored_session(&fixture.turn.state, &fixture.turn.session_id),
    ).unwrap();
    assert_eq!(durable.engram_unadmitted_active_turn.as_ref(), Some(&original));
    assert!(durable.engram_observation_delete_requested);
    assert!(durable.orchestrator_auto_dispatch_blocked);
    fixture.turn.record(|record| assert_eq!(
        record.engram.historical_control.get("engramOpenGrantId"), Some(&json!("historical-open-grant"))));
    fixture.assert_no_granted_reporting();
    assert!(fixture.producer.requests().is_empty());

    let installed = InstallBarrier::new(&fixture.turn.state, "unused");
    fixture.finish();
    fixture.assert_no_granted_reporting();
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
    // The turn's measured sighting is the report's own next envelope, sent
    // once after the turn report.
    let sighting = fixture.observation();
    assert_eq!(occurrence(&sighting)["kind"], "source_sighting");
    assert!(sighting.durable_before_send);
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
    fixture.turn.record(|record| assert!(record.engram.observation_delete_requested,
        "the retained Delete marker stays until the retried Delete removes the session"));
    fixture.turn.record(|record| assert_eq!(
        record.engram.historical_control.get("engramOpenGrantId"), Some(&json!("historical-open-grant"))));
    fixture.assert_no_granted_reporting();
    fixture.turn.record(|record| assert!(record.orchestrator_auto_dispatch_blocked));
    assert!(matches!(fixture.turn._runtime_rx.lock().unwrap().try_recv(), Err(mpsc::TryRecvError::Empty)));

    fixture.turn.state.kill_session(&fixture.turn.session_id).unwrap();
    fixture.assert_no_granted_reporting();
    assert!(fixture.turn.state.inner.lock().unwrap().find_session_index(&fixture.turn.session_id).is_none());
    let kinds: Vec<Value> = fixture.producer.requests().iter()
        .map(|request| request["occurrence"]["kind"].clone())
        .collect();
    assert_eq!(kinds, [json!("unadmitted_turn"), json!("source_sighting")],
        "Delete neither resends nor omits the adopted report");
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
        assert!(retained.engram_observation_delete_requested);
        assert!(retained.orchestrator_auto_dispatch_blocked);
        if let Some(request) = request {
            // The setup turn's own settled reports come first; the adopted
            // turn's report follows them.
            let setup = fixture.setup_reports.len();
            assert_eq!(retained.engram_unadmitted_reports[setup].envelopes[0].request, request);
            let durable: Value = serde_json::from_str(&stored_session(&fixture.turn.state, &fixture.turn.session_id)).unwrap();
            assert_eq!(durable["engramUnadmittedReports"][setup]["envelopes"][0]["request"], request);
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
        assert!(fixture.turn.record(|record| record.engram.observation_delete_requested),
            "{phase}: the retained Delete marker survives until the retried Delete");
        fixture.turn.state.kill_session(&fixture.turn.session_id).unwrap();
        assert!(fixture.turn.state.inner.lock().unwrap().find_session_index(&fixture.turn.session_id).is_none());
        fixture.turn.state.trigger_shutdown_signal();
        fixture.turn.state.shutdown_persist_blocking();
    }
}

#[test]
fn unadmitted_review_terminal_custody_delete_retry_and_restart_stay_truthful() {
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
        // The setup turn's own settled reports come first; the adopted turn's
        // report follows them.
        let setup = fixture.setup_reports.len();
        let expected = fixture.turn.record(|record| record.engram.unadmitted_reports[setup].clone());
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
            let retry = fixture.turn.state.kill_session(&fixture.turn.session_id).err().expect("a retried Delete keeps terminal custody");
            for diagnostic in [&error.message, &retry.message] {
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
            assert_eq!(fixture.turn.record(|record| record.engram.unadmitted_reports[setup].clone()), failed);
            assert!(runtime_observation_report_is_durable(&fixture.turn.state.persistence_path,
                &fixture.turn.session_id, &failed).unwrap());
            assert_eq!(fixture.producer.requests(), requests, "a retried Delete or restart cannot renew a terminal send");
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
        let report = fixture.observation();
        assert_eq!(occurrence(&report)["kind"], "unadmitted_turn");
        // The turn's measured sighting is sent once after it, refused or not.
        let sighting = fixture.observation();
        assert_eq!(occurrence(&sighting)["kind"], "source_sighting");
        idle.idle.recv_timeout(DEADLOCK_GUARD).unwrap();
        // The setup turn's own settled reports come first; the adopted turn's
        // report follows them.
        let setup = fixture.setup_reports.len();
        let settled = fixture.turn.record(|record| record.engram.unadmitted_reports[setup].clone());
        // Persist the pre-settlement image, then install only the in-memory
        // settled image. A failed coupled ACK must not erase that sole request.
        {
            let mut inner = fixture.turn.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.turn.session_id).unwrap();
            inner.sessions[index].engram.unadmitted_reports[setup].envelopes[0].delivery =
                EngramRuntimeObservationDelivery::Pending { last_error: Some("reply not yet durable".to_owned()) };
            inner.stamp_session_at_index(index);
            fixture.turn.state.persist_internal_locked(&inner).unwrap();
            inner.sessions[index].engram.unadmitted_reports[setup] = settled.clone();
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
                assert!(matches!(&fence.target, PersistFenceTarget::EngramObservationRemoval { .. }));
                fence_seen = true;
                fence.finish(Err(PersistFenceError::WriteFailed("controlled missing settlement ACK".to_owned())));
            }
        }
        let result = deletion.join().unwrap();
        fixture.turn.state.persist_tx = original_writer;
        assert!(fence_seen, "Delete must fence the exact settled report before removal");
        assert!(result.is_err(), "failed settlement ACK must keep the original session");
        assert_eq!(fixture.turn.record(|record| record.engram.unadmitted_reports[setup].clone()), settled);
        fixture.turn.state.kill_session(&fixture.turn.session_id).unwrap();
        assert!(fixture.turn.state.inner.lock().unwrap().find_session_index(&fixture.turn.session_id).is_none());
        let kinds: Vec<Value> = fixture.producer.requests().iter()
            .map(|request| request["occurrence"]["kind"].clone())
            .collect();
        assert_eq!(kinds, [json!("unadmitted_turn"), json!("source_sighting")],
            "durable settlement needs no resend");
        fixture.turn.state.trigger_shutdown_signal();
        fixture.turn.state.shutdown_persist_blocking();
    }
}

#[test]
fn delete_without_observation_custody_needs_no_slow_writer_fence() {
    let mut state = test_app_state();
    let session = test_session_id(&state, Agent::Codex);
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.engram.unadmitted_active_turn.is_none());
        assert!(record.engram.unadmitted_closing_jobs.is_empty());
        assert!(record.engram.unadmitted_reports.is_empty());
        assert!(record.engram.source_root_bind_owner.is_none());
    }
    // Hold ordinary writes and reject any fence, without waiting for its
    // production deadline. Empty custody must not request an observation ACK.
    let original_writer = state.persist_tx.clone();
    let (tx, rx) = mpsc::channel();
    state.persist_tx = tx;
    let deleting = state.clone();
    let selected = session.clone();
    let deletion = std::thread::spawn(move || deleting.kill_session(&selected));
    let guard = phase_sync::PollGuard::new();
    let mut fence_seen = false;
    while !deletion.is_finished() {
        if let Ok(PersistRequest::Fence(fence)) = rx.recv_timeout(Duration::from_millis(20)) {
            fence_seen = true;
            fence.finish(Err(PersistFenceError::WriteFailed(
                "controlled slow writer has not acknowledged observation removal".to_owned(),
            )));
        }
        guard.wait("ordinary Delete without an observation-removal ACK");
    }
    let result = deletion.join().unwrap();
    state.persist_tx = original_writer;
    assert!(!fence_seen, "empty-custody Delete must request no observation fence");
    result.expect("ordinary deletion must complete without an observation acknowledgement");
    assert!(state.inner.lock().unwrap().find_session_index(&session).is_none());
    state.trigger_shutdown_signal();
    state.shutdown_persist_blocking();
}

#[test]
fn unadmitted_review_capture_absence_retains_exact_job_without_reporting() {
    let mut fixture = ReportingTurn::new("unadmitted-capture-absence", true, ReplyMode::Recorded, false);
    let local_runs = super::source_refusals::observe_declared_failure(&mut fixture);
    let (installed, received) = mpsc::channel();
    *fixture.turn.state.runtime_observation_lifetime.install_hook.lock().unwrap() = Some(Arc::new(move |phase| {
        if phase == "closing_installed" { let _ = installed.send(()); }
    }));
    fixture.turn.state.inner.lock().unwrap().test_engram_source_capture = Some(Arc::new(|_, budget| {
        assert_eq!(budget, Duration::from_millis(250), "fixture must not inflate the capture deadline");
        Ok(None)
    }));
    let original = fixture.turn.record(|record| record.engram.unadmitted_active_turn.clone().unwrap());
    fixture.finish();
    let job = loop {
        received.recv_timeout(DEADLOCK_GUARD).unwrap();
        let job = fixture.turn.record(|record| record.engram.unadmitted_closing_jobs[0].clone());
        if job.attempts > 0 { break job; }
    };
    assert_eq!(original.observed_outcome, None, "an open trace has no outcome yet");
    let mut closed = original.clone();
    closed.observed_outcome = Some(EngramObservedTurnOutcome::Unknown);
    assert_eq!(job.trace, closed, "closing records only the turn's outcome");
    assert!(job.policy.is_some());
    assert_eq!(job.attempts, 1);
    assert_eq!(job.last_error.as_deref(), Some("runtime closing source could not be measured"));
    assert!(job.terminal_failure.is_none());
    assert_eq!(fixture.turn.record(|record| record.engram.unadmitted_reports.clone()),
        fixture.setup_reports, "the adopted turn adds no report");
    assert!(fixture.producer.requests().is_empty(), "missing source is not an empty or invented observation");
    let durable: Value = serde_json::from_str(&stored_session(&fixture.turn.state, &fixture.turn.session_id)).unwrap();
    assert_eq!(durable["engramUnadmittedClosingJobs"][0], serde_json::to_value(&job).unwrap());
    super::source_refusals::assert_declared_failure_unchanged(&fixture, &local_runs);
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
    assert_eq!(restored.engram.unadmitted_reports, fixture.setup_reports,
        "recovery adds no report of the archived turn");
    adopt_runtime_observation(&mut restored, "replacement-adoption-notice", 1, None);
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
        // EOCheckedTurn uses the shared synchronous-persist fixture, whose
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
        let restarted = restart_reporting_state(&fixture.turn.root, &old, fixture.producer.clone());
        fixture.turn.state = restarted.clone();
        let (active, archive, closing_count, report_count) = fixture.turn.record(|record| {
            let archive = record.session.messages.iter().find_map(|message| match message {
                Message::Text { text, expanded_text: Some(details), .. }
                    if text.contains(&original.host_turn_ref) && text.contains("not reported") => Some(details.clone()),
                _ => None,
            });
            (record.engram.unadmitted_active_turn.clone(), archive,
             record.engram.unadmitted_closing_jobs.len(), record.engram.unadmitted_reports.clone())
        });
        // Assertions run after releasing the state mutex so a failed control
        // does not poison or strand the restarted host's real worker threads.
        assert!(active.is_none());
        let recovered: EngramRuntimeObservedTurn = serde_json::from_str(
            &archive.expect("real boot must append the recovery archive"),
        ).unwrap();
        assert_eq!(recovered, original);
        assert_eq!((closing_count, report_count), (0, fixture.setup_reports.clone()),
            "the archived turn leaves no closing job and no report of its own");
        let (runtime, rx) = test_codex_runtime_handle("after-unadmitted-restart");
        fixture.turn._runtime_rx = Arc::new(Mutex::new(rx));
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
            // The report this delivery was installing: the adopted turn's,
            // after the setup turn's own settled reports.
            let setup = fixture.setup_reports.len();
            let report = fixture
                .turn
                .record(|record| record.engram.unadmitted_reports[setup].clone());
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
        retire_reporting_hosts(fixture, vec![restarted, state]);
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
    assert!(serde_json::to_value(&PersistedSessionRecord::from_record(&restored)).unwrap()
        .get("engramContinuityAnchor").is_none());
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

/// Park the actual ordinary root read before source capture, so a restored
/// worker cannot race installation of the existing source-measurement hook.
struct ClosingReadBarrier {
    producer: Arc<ObservationProducer>,
    entered: mpsc::Receiver<()>,
    release: Arc<(Mutex<bool>, Condvar)>,
}

impl ClosingReadBarrier {
    fn new(fixture: &ReportingTurn) -> Self {
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let gate = release.clone();
        let (entered, received) = mpsc::channel();
        *fixture.producer.request_hook.lock().unwrap() = Some(Arc::new(move |request| {
            if !matches!(request, EngramControlRequest::NamedRootRead { .. }) { return; }
            let _ = entered.send(());
            phase_sync::wait_for_gate(&gate, true);
        }));
        Self { producer: fixture.producer.clone(), entered: received, release }
    }

    fn wait(&self) {
        phase_sync::receive(&self.entered, "the restored ordinary closing root read");
    }

    fn release(&self) {
        phase_sync::set_gate(&self.release, true);
    }
}

impl Drop for ClosingReadBarrier {
    fn drop(&mut self) {
        self.release();
        *self.producer.request_hook.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

#[test]
fn restored_closing_definitively_unclaimed_work_does_not_wait_forever() {
    let mut fixture = ReportingTurn::new(
        "unadmitted-definitively-unclaimed", true, ReplyMode::Recorded, false,
    );
    let expected = retire_with_pinned_closing_job(&mut fixture);
    *fixture.producer.binding.write().unwrap() = None;
    let restarted = restart_reporting_state(&fixture.turn.root, &fixture.turn.state, fixture.producer.clone());
    wait_for_observation_worker_idle(&restarted, &fixture.turn.session_id);
    {
        let inner = restarted.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.turn.session_id).unwrap()];
        let job = &record.engram.unadmitted_closing_jobs[0];
        assert_eq!(job.scope, expected.scope);
        assert_eq!(job.trace, expected.trace);
        assert!(job.terminal_failure.as_ref().is_some_and(|cause| cause.contains("scope changed")));
        assert_eq!(record.engram.unadmitted_reports, fixture.setup_reports,
            "a definitively unclaimed closing job makes no report");
    }
    assert!(fixture.producer.requests().is_empty());
    fixture.assert_no_granted_reporting();
    restarted.kill_session(&fixture.turn.session_id).unwrap();
    restarted.trigger_shutdown_signal();
    restarted.shutdown_persist_blocking();
}

#[test]
fn restored_closing_delete_retry_preserves_the_paused_successor_without_provider_replay() {
    let mut fixture = ReportingTurn::new(
        "unadmitted-ordinary-delete-resume", true, ReplyMode::Recorded, true,
    );
    fixture.turn.record_mut(|record| record.set_auto_dispatch_blocked(true));
    queue_test_engram_prompt(&fixture.turn.state, &fixture.turn.session_id,
        "Do not dispatch while recovering tracking.", QueuedPromptSource::User, None);
    let successor = fixture.turn.record(|record| serde_json::to_value(&record.queued_prompts[0]).unwrap());
    let expected = retire_with_pinned_closing_job(&mut fixture);
    let read = ClosingReadBarrier::new(&fixture);
    let restarted = restart_reporting_state(&fixture.turn.root, &fixture.turn.state, fixture.producer.clone());
    read.wait();
    install_reporting_source_capture(&restarted);
    read.release();
    let sent = fixture.observation();
    assert!(sent.durable_before_send);
    assert_eq!(occurrence(&sent)["host_turn_ref"], expected.trace.host_turn_ref);
    assert!(restarted.kill_session(&fixture.turn.session_id).is_err(),
        "Delete retains a report whose actual producer reply is still held");
    assert!(restarted.kill_session(&fixture.turn.session_id).is_err(),
        "a retried Delete is still refused while the producer reply is held");
    {
        let inner = restarted.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.turn.session_id).unwrap()];
        assert!(record.engram.observation_delete_requested);
        assert!(record.orchestrator_auto_dispatch_blocked && record.session.queue_paused);
        assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), successor);
        assert!(record.runtime.runtime_token().is_none());
    }
    fixture.producer.release();
    wait_for_observation_worker_idle(&restarted, &fixture.turn.session_id);
    {
        let inner = restarted.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.turn.session_id).unwrap()];
        assert!(record.engram.observation_delete_requested);
        assert!(record.orchestrator_auto_dispatch_blocked && record.session.queue_paused);
        assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), successor);
    }
    // The turn's measured sighting is the report's own next envelope, sent
    // once after the released turn report; neither is replayed.
    let sighting = fixture.observation();
    assert_eq!(occurrence(&sighting)["kind"], "source_sighting");
    assert!(sighting.durable_before_send);
    let kinds: Vec<Value> = fixture.producer.requests().iter()
        .map(|request| request["occurrence"]["kind"].clone())
        .collect();
    assert_eq!(kinds, [json!("unadmitted_turn"), json!("source_sighting")]);
    assert!(fixture.turn._runtime_rx.lock().unwrap().try_recv().is_err(), "observation recovery sends no provider command");
    fixture.assert_no_granted_reporting();
    restarted.kill_session(&fixture.turn.session_id).unwrap();
    assert!(restarted.inner.lock().unwrap().find_session_index(&fixture.turn.session_id).is_none());
    restarted.trigger_shutdown_signal();
    restarted.shutdown_persist_blocking();
}

#[test]
fn restored_closing_capture_uses_its_pinned_work_without_a_control_rebind() {
    let mut fixture = ReportingTurn::new(
        "unadmitted-ordinary-closing-restore", true, ReplyMode::Recorded, false,
    );
    fixture.check("before-ordinary-restart", false);
    let expected = retire_with_pinned_closing_job(&mut fixture);
    let read = ClosingReadBarrier::new(&fixture);
    let restarted = restart_reporting_state(&fixture.turn.root, &fixture.turn.state, fixture.producer.clone());
    read.wait();
    install_reporting_source_capture(&restarted);
    {
        let inner = restarted.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.turn.session_id).unwrap()];
        assert!(record.engram.work_binding.is_none(), "restore does not invent a live work association");
        assert_eq!(record.engram.unadmitted_closing_jobs, vec![expected.clone()]);
    }
    read.release();
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
    let mut fixture = ReportingTurn::new(
        "unadmitted-slow-closing-measurement", true, ReplyMode::Recorded, false,
    );
    fixture.check("before-slow-measurement-restart", false);
    let expected = retire_with_pinned_closing_job(&mut fixture);
    let read = ClosingReadBarrier::new(&fixture);
    let restarted = restart_reporting_state(&fixture.turn.root, &fixture.turn.state, fixture.producer.clone());
    read.wait();
    let cap = {
        let inner = restarted.inner.lock().unwrap();
        engram_project_for_session_locked(&inner, &fixture.turn.session_id)
            .and_then(|project| project.engram.as_ref()).unwrap().call_timeout()
    };
    assert!(cap < Duration::from_secs(1));
    let hold = cap * 2;
    // This retained delay models slow source I/O, not an ordering barrier.
    // The actual root-read barrier above orders installation of this hook.
    restarted.inner.lock().unwrap().test_engram_source_capture = Some(Arc::new(move |place, _budget| {
        std::thread::sleep(hold);
        engram_place_source_capture(place)
    }));
    read.release();
    let sent = fixture.observation();
    assert!(sent.durable_before_send);
    assert_eq!(occurrence(&sent)["host_turn_ref"], expected.trace.host_turn_ref);
    assert_eq!(sent.request["binding"], serde_json::to_value(&expected.scope.binding).unwrap());
    assert_eq!(fixture.producer.requests().len(), 1);
    fixture.assert_no_granted_reporting();
    restarted.trigger_shutdown_signal();
    restarted.shutdown_persist_blocking();
}

#[tokio::test]
async fn a_restored_closing_job_keeps_its_original_scope_and_policy() {
    let mut fixture = ReportingTurn::new("unadmitted-closing-restart", true, ReplyMode::Recorded, false);
    fixture.turn.record_mut(|record| record.set_auto_dispatch_blocked(true));
    queue_test_engram_prompt(&fixture.turn.state, &fixture.turn.session_id,
        "Keep this successor paused across closing-job recovery.", QueuedPromptSource::User, None);
    let successor = fixture.turn.record(|record| serde_json::to_value(&record.queued_prompts[0]).unwrap());
    let expected = retire_with_pinned_closing_job(&mut fixture);
    let old = fixture.turn.state.clone();
    let durable: PersistedSessionRecord = serde_json::from_str(&stored_session(&old, &fixture.turn.session_id)).unwrap();
    assert!(durable.orchestrator_auto_dispatch_blocked);
    assert_eq!(serde_json::to_value(&durable.queued_prompts[0]).unwrap(), successor);
    assert!(expected.policy.is_some());
    let replacement = test_control_work_binding("different-work-after-closing-restart", 10);
    *fixture.producer.binding.write().unwrap() = Some(replacement.clone());
    let restarted = restart_reporting_state(&fixture.turn.root, &old, fixture.producer.clone());
    let mut events = restarted.subscribe_stream_events();
    restarted.resume_runtime_observation_reports();
    tokio::time::timeout(DEADLOCK_GUARD, async {
        loop {
            let terminal = {
                let inner = restarted.inner.lock().unwrap();
                inner.sessions[inner.find_session_index(&fixture.turn.session_id).unwrap()]
                    .engram.unadmitted_closing_jobs[0].terminal_failure.is_some()
            };
            if terminal { break; }
            events.recv().await.unwrap();
        }
    }).await.expect("the restored scope mismatch must settle visibly");
    let (job, notices) = {
        let inner = restarted.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.turn.session_id).unwrap()];
        assert!(record.orchestrator_auto_dispatch_blocked && record.session.queue_paused);
        assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), successor);
        (record.engram.unadmitted_closing_jobs[0].clone(), system_notices(record))
    };
    restarted.shutdown_persist_blocking();
    assert_eq!(job.trace, expected.trace);
    assert_eq!(job.baseline, expected.baseline);
    assert_eq!(job.scope, expected.scope);
    assert_eq!(job.policy, expected.policy);
    assert!(job.terminal_failure.as_ref().unwrap().contains("scope changed"));
    assert!(notices.iter().any(|notice| notice.contains(&replacement.work_id)), "{notices:?}");
    assert!(fixture.producer.requests().is_empty());
    fixture.assert_no_granted_reporting();
    restarted.trigger_shutdown_signal();
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
            let isolated = inner.engram_session_obligations.entries.is_empty()
                && inner.engram_session_obligations.checkpoint_operations.is_empty()
                && inner.quarantined_persisted_session_ids.is_empty()
                && inner.sessions.iter().all(|record| record.engram.historical_control.is_empty());
            (PersistedState::from_inner(&inner), isolated)
        };
        let source_image = serde_json::to_value(&persisted).unwrap();
        persisted.metadata_capture = PersistMetadataCapture::new(
            Arc::new(std::sync::atomic::AtomicU64::new(generation)),
        );
        assert_eq!(serde_json::to_value(&persisted).unwrap(), source_image,
            "copy setup must not alter serialized custody or authority");
        if migrate_outbox {
            // This ordinary fixture has no control checkpoint to project out.
            // Keep the complete real custody image; only the metadata marker
            // below is converted to exercise the predecessor-reader floor.
            fs::write(data.join("current-format-snapshot.json"),
                serde_json::to_vec(&persisted).unwrap()).unwrap();
            assert!(baseline_has_no_other_owner,
                "the disposable predecessor fixture has no unrelated historical custody");
            let ledger = &persisted.engram_session_obligations;
            ledger.validate().unwrap();
            assert!(ledger.entries.is_empty(), "no linked independent obligation may be omitted");
            assert!(ledger.checkpoint_operations.is_empty());
            assert_eq!(serde_json::to_value(&persisted).unwrap(), source_image,
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
    let setup = fixture.setup_reports.len();
    assert_eq!(retained["engramUnadmittedReports"].as_array().unwrap()[..setup].to_vec(),
        fixture.setup_reports.iter().map(|report| serde_json::to_value(report).unwrap()).collect::<Vec<_>>(),
        "the setup turn's settled report is retained unchanged");
    assert_eq!(retained["engramUnadmittedReports"][setup]["envelopes"][0]["request"],
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
    let original = fixture.observation();
    assert_eq!(occurrence(&original)["kind"], "unadmitted_turn");
    // The turn's measured sighting follows its report.
    assert_eq!(occurrence(&fixture.observation())["kind"], "source_sighting");
    settled.idle.recv_timeout(DEADLOCK_GUARD).unwrap();
    {
        let mut inner = fixture.turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.turn.session_id).unwrap();
        // The adopted turn's report, after the setup turn's own settled
        // reports, is the valid report retried behind the malformed one.
        let setup = fixture.setup_reports.len();
        let mut good = inner.sessions[index].engram.unadmitted_reports[setup].next_version();
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
    // Only the valid report's pending turn request is sent again, identically.
    assert_eq!(fixture.observation().request, original.request);
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
