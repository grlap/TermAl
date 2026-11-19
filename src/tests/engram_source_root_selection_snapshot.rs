//! A name must account for older closing jobs as well as its immediate turn.
//! Exercise retry ordering through the real capture, outbox and dispatch owners.
use super::*;

#[path = "engram_source_root_durable_snapshot.rs"]
mod durability;

fn saved_job(fixture: &TwoClaims, turn: &str) -> EngramRuntimeClosingJob {
    let db = open_sqlite_state_read_connection(&fixture.turn.state.persistence_path).unwrap();
    let raw: String = db
        .query_row(
            "SELECT value_json FROM sessions WHERE id = ?1",
            [&fixture.turn.session_id],
            |row| row.get(0),
        )
        .unwrap();
    let saved: PersistedSessionRecord = serde_json::from_str(&raw).unwrap();
    saved
        .engram_unadmitted_closing_jobs
        .into_iter()
        .find(|job| job.trace.host_turn_ref == turn)
        .expect("the complete closing intent was retained durably")
}

fn set_retry_due(fixture: &TwoClaims, turn: &str, due: String) {
    {
        let mut inner = fixture.turn.state.inner.lock().unwrap();
        let record = inner.session_mut(&fixture.turn.session_id).unwrap();
        let job = record
            .engram
            .unadmitted_closing_jobs
            .iter_mut()
            .find(|job| job.trace.host_turn_ref == turn)
            .unwrap();
        job.retry_at = due;
        let prepared = job.clone();
        fixture
            .turn
            .state
            .commit_session_locked(&mut inner, &fixture.turn.session_id)
            .unwrap();
        assert_eq!(
            serde_json::to_value(saved_job(fixture, turn)).unwrap(),
            serde_json::to_value(prepared).unwrap(),
            "the worker must read back exactly the retry image used to order X and Y"
        );
    }
    // The fixture changes the deadline out of band. Notify the worker after
    // the new retry image is durable and the state lock has been released.
    fixture
        .turn
        .state
        .start_runtime_observation_worker(&fixture.turn.session_id);
}

fn older_turn_in_retry(fixture: &TwoClaims) -> EngramRuntimeClosingJob {
    fixture.turn.begin("X closes before the naming turn.");
    fixture.check_first("older-X-check");
    fixture
        .turn
        .record_mut(|record| record.engram.unadmitted_worker_running = true);
    fixture.turn.end();
    let original = fixture.turn.record(|record| {
        record
            .engram
            .unadmitted_closing_jobs
            .last()
            .unwrap()
            .clone()
    });
    assert_eq!(
        serde_json::to_value(saved_job(fixture, &original.trace.host_turn_ref)).unwrap(),
        serde_json::to_value(&original).unwrap()
    );
    fixture
        .turn
        .transport
        .observation_policy_errors
        .lock()
        .unwrap()
        .push_back(EngramTransportError::transport(
            "temporary older-X policy outage",
        ));
    let failed = fixture
        .turn
        .state
        .capture_runtime_observation_report(&fixture.turn.session_id, &original);
    assert!(
        failed.is_err(),
        "X reaches a real transient capture refusal"
    );
    let before_install = chrono::Utc::now();
    fixture
        .turn
        .state
        .install_runtime_closing_result(&fixture.turn.session_id, &original, failed)
        .unwrap();
    let retry = saved_job(fixture, &original.trace.host_turn_ref);
    assert_eq!(retry.attempts, 1);
    assert!(retry.terminal_failure.is_none());
    assert!(
        retry
            .last_error
            .as_ref()
            .unwrap()
            .contains("older-X policy outage")
    );
    // The failure scheduled a backoff: the retry falls well after the install
    // began. Not "still pending now", which a slow host can outrun.
    let retry_at = chrono::DateTime::parse_from_rfc3339(&retry.retry_at)
        .expect("retry_at is a timestamp")
        .with_timezone(&chrono::Utc);
    assert!(
        retry_at >= before_install + chrono::Duration::seconds(1),
        "retry at {retry_at} carries no backoff from the install at {before_install}"
    );
    // Park only the fixture's due time so the newer job wins deterministically.
    // We explicitly make X due again below; no retry budget or wait is extended.
    set_retry_due(
        fixture,
        &retry.trace.host_turn_ref,
        (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
    );
    fixture.turn.record(|record| {
        record
            .engram
            .unadmitted_closing_jobs
            .iter()
            .find(|job| job.trace.host_turn_ref == retry.trace.host_turn_ref)
            .unwrap()
            .clone()
    })
}

fn close_newer_turn(fixture: &TwoClaims, name_b: bool) -> EngramRuntimeClosingJob {
    fixture.turn.begin("Y closes ahead of X's retry.");
    fixture.check_first("newer-Y-check");
    if name_b {
        fixture.name_other();
    }
    fixture.turn.end();
    fixture.turn.record(|record| {
        record
            .engram
            .unadmitted_closing_jobs
            .last()
            .unwrap()
            .clone()
    })
}

fn queue_b(fixture: &TwoClaims) {
    let result = fixture
        .turn
        .state
        .dispatch_turn(
            &fixture.turn.session_id,
            SendMessageRequest {
                text: "B waits for every acknowledged A predecessor.".into(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .unwrap();
    assert!(matches!(result, DispatchTurnResult::Queued));
}

fn assert_report(fixture: &TwoClaims, job: &EngramRuntimeClosingJob, uncertain: bool) {
    let request = fixture.turn.observation_for(&job.trace.host_turn_ref);
    assert_eq!(request["binding"], json!(fixture.first));
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(
        checks[0]["host_check_id"],
        json!(job.trace.commands[0].check.host_check_id)
    );
    if uncertain {
        assert!(request["occurrence"]["source_change"].is_null());
        assert!(checks[0]["source_basis"].is_null());
        assert!(checks[0]["host_evidence_ref"].is_null());
    }
    assert_ordinary_observation_has_no_native_credit(&fixture.turn, &request);
}

fn finish_b_after_older_capture(fixture: &TwoClaims, older: &EngramRuntimeClosingJob) {
    set_retry_due(
        fixture,
        &older.trace.host_turn_ref,
        unadmitted_observation_timestamp(),
    );
    assert_report(fixture, older, false);
    let guard = phase_sync::PollGuard::new();
    let successor = loop {
        if let Some(trace) = fixture
            .turn
            .record(|record| record.engram.unadmitted_active_turn.clone())
        {
            break trace;
        }
        guard.wait("B opens after X's own durable report");
    };
    assert_eq!(
        successor.opening_root.as_ref().unwrap().binding,
        fixture.second
    );
    fixture.wait_for_recorded_while_active(older, &successor);
    fixture.check_in("B-after-both-A-captures", &fixture.second_root);
    fixture
        .turn
        .transport
        .enable_named_roots(&fixture.turn.session_id, &fixture.second);
    assert_eq!(fixture.turn.finish()["binding"], json!(fixture.second));
}

#[test]
fn controlled_successor_waits_for_older_a_retry_after_newer_a_capture() {
    let fixture = TwoClaims::new("snapshot-controlled-older-retry");
    let older = older_turn_in_retry(&fixture);
    let newer = close_newer_turn(&fixture, true);
    assert_ne!(older.trace.host_turn_ref, newer.trace.host_turn_ref);
    queue_b(&fixture);
    fixture.release_worker();
    // Delivery happens after durable readback and its synchronous dispatch kick.
    // Seeing Y's request therefore cannot race a still-unattempted B opening.
    assert_report(&fixture, &newer, false);
    fixture.turn.record(|record| {
        assert!(
            record.engram.unadmitted_active_turn.is_none(),
            "B must not open while the older acknowledged A job is still pending"
        );
        assert_eq!(record.engram.work_binding.as_ref(), Some(&fixture.first));
        assert_eq!(record.queued_prompts.len(), 1);
        assert!(
            record
                .engram
                .unadmitted_closing_jobs
                .iter()
                .any(|job| job.trace.host_turn_ref == older.trace.host_turn_ref
                    && job.terminal_failure.is_none())
        );
    });
    finish_b_after_older_capture(&fixture, &older);
}

#[test]
fn idle_name_waits_for_older_a_retry_even_when_latest_a_report_is_durable() {
    let fixture = TwoClaims::new("snapshot-idle-older-retry");
    let older = older_turn_in_retry(&fixture);
    let newer = close_newer_turn(&fixture, false);
    fixture.release_worker();
    assert_report(&fixture, &newer, false);
    fixture.name_other();
    fixture.turn.record(|record| {
        assert_eq!(
            record.engram.work_binding.as_ref(),
            Some(&fixture.first),
            "idle naming cannot invalidate an older pending A closing scope"
        );
        assert!(record.engram.unadmitted_active_turn.is_none());
    });
    queue_b(&fixture);
    finish_b_after_older_capture(&fixture, &older);
}

#[test]
fn adopted_successor_preserves_all_live_a_jobs_at_the_acknowledgement_cut() {
    let fixture = TwoClaims::new("snapshot-adopted-older-retry");
    let older = older_turn_in_retry(&fixture);
    let (restored, foreign) = {
        let mut restored: EngramRuntimeClosingJob =
            serde_json::from_value(serde_json::to_value(&older).unwrap()).unwrap();
        restored.trace.host_turn_ref = format!("restored-{}", uuid::Uuid::new_v4());
        assert!(restored.trace.opening_scope.is_none());
        let mut foreign = older.clone();
        foreign.trace.host_turn_ref = format!("foreign-{}", uuid::Uuid::new_v4());
        foreign.scope.binding = Some(test_control_work_binding("foreign-pending", 1));
        let mut inner = fixture.turn.state.inner.lock().unwrap();
        let record = inner.session_mut(&fixture.turn.session_id).unwrap();
        record
            .engram
            .unadmitted_closing_jobs
            .extend([restored.clone(), foreign.clone()]);
        fixture
            .turn
            .state
            .commit_session_locked(&mut inner, &fixture.turn.session_id)
            .unwrap();
        (restored, foreign)
    };
    assert_eq!(saved_job(&fixture, &restored.trace.host_turn_ref), restored);
    assert_eq!(saved_job(&fixture, &foreign.trace.host_turn_ref), foreign);
    let newer = close_newer_turn(&fixture, true);
    let token = fixture
        .turn
        .record(|record| record.runtime.runtime_token().unwrap());
    fixture
        .turn
        .state
        .open_unmediated_claude_turn(
            &fixture.turn.session_id,
            &token,
            ClaudeUnownedCause::NoPrompt,
        )
        .unwrap()
        .unwrap();
    let successor = fixture.turn.record(|record| {
        let successor = record
            .engram
            .unadmitted_active_turn
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(
            successor.opening_root.as_ref().unwrap().binding,
            fixture.second
        );
        for original in [&older, &newer] {
            let job = record
                .engram
                .unadmitted_closing_jobs
                .iter()
                .find(|job| job.trace.host_turn_ref == original.trace.host_turn_ref)
                .unwrap();
            assert_eq!(job.scope.binding, original.scope.binding);
            assert_eq!(job.scope.root, original.scope.root);
            assert_eq!(job.trace.commands, original.trace.commands);
            assert!(
                job.trace.source_attribution_uncertain,
                "every live A job at the cut must retain facts as uncertain after adopted B"
            );
            assert_eq!(
                job.scope
                    .historical_fields
                    .get("adopted_successor_reason")
                    .and_then(Value::as_str),
                Some(ENGRAM_ADOPTED_PREDECESSOR_REASON)
            );
        }
        for original in [&restored, &foreign] {
            assert_eq!(
                record
                    .engram
                    .unadmitted_closing_jobs
                    .iter()
                    .find(|job| job.trace.host_turn_ref == original.trace.host_turn_ref)
                    .unwrap(),
                original,
                "restored and foreign jobs never gain authority from this naming"
            );
        }
        successor
    });
    fixture.check_in("B-after-snapshot-adoption", &fixture.second_root);
    let successor = fixture.turn.record(|record| {
        let current = record.engram.unadmitted_active_turn.as_ref().unwrap();
        assert_eq!(current.host_turn_ref, successor.host_turn_ref);
        current.clone()
    });
    set_retry_due(
        &fixture,
        &older.trace.host_turn_ref,
        unadmitted_observation_timestamp(),
    );
    fixture.release_worker();
    assert_report(&fixture, &newer, true);
    assert_report(&fixture, &older, true);
    fixture.wait_for_recorded_while_active(&older, &successor);
    for original in [&restored, &foreign] {
        set_retry_due(
            &fixture,
            &original.trace.host_turn_ref,
            unadmitted_observation_timestamp(),
        );
    }
    let guard = phase_sync::PollGuard::new();
    loop {
        if fixture.turn.record(|record| {
            [&restored, &foreign].iter().all(|original| {
                record
                    .engram
                    .unadmitted_closing_jobs
                    .iter()
                    .find(|job| job.trace.host_turn_ref == original.trace.host_turn_ref)
                    .is_some_and(|job| job.terminal_failure.is_some())
            })
        }) {
            break;
        }
        guard.wait("restored and foreign jobs settle only through their existing scope guards");
    }
    fixture.turn.record(|record| {
        for original in [&restored, &foreign] {
            assert!(
                record
                    .engram
                    .unadmitted_reports
                    .iter()
                    .all(|report| report.host_turn_ref != original.trace.host_turn_ref)
            );
        }
    });
    fixture
        .turn
        .transport
        .enable_named_roots(&fixture.turn.session_id, &fixture.second);
    assert_eq!(fixture.turn.finish()["binding"], json!(fixture.second));
}

#[test]
fn a_closing_job_added_after_acknowledgement_does_not_inherit_adopted_authority() {
    let fixture = TwoClaims::new("snapshot-no-later-enrollment");
    let older = older_turn_in_retry(&fixture);
    close_newer_turn(&fixture, true);
    let mut later = older.clone();
    later.trace.host_turn_ref = format!("later-{}", uuid::Uuid::new_v4());
    {
        let mut inner = fixture.turn.state.inner.lock().unwrap();
        inner
            .session_mut(&fixture.turn.session_id)
            .unwrap()
            .engram
            .unadmitted_closing_jobs
            .push(later.clone());
        fixture
            .turn
            .state
            .commit_session_locked(&mut inner, &fixture.turn.session_id)
            .unwrap();
    }
    let token = fixture
        .turn
        .record(|record| record.runtime.runtime_token().unwrap());
    fixture
        .turn
        .state
        .open_unmediated_claude_turn(
            &fixture.turn.session_id,
            &token,
            ClaudeUnownedCause::NoPrompt,
        )
        .unwrap()
        .unwrap();
    fixture.turn.record(|record| {
        assert_eq!(record.engram.work_binding.as_ref(), Some(&fixture.second));
        assert!(
            record
                .engram
                .unadmitted_closing_jobs
                .iter()
                .find(|job| job.trace.host_turn_ref == older.trace.host_turn_ref)
                .unwrap()
                .trace
                .source_attribution_uncertain
        );
        assert_eq!(
            record
                .engram
                .unadmitted_closing_jobs
                .iter()
                .find(|job| job.trace.host_turn_ref == later.trace.host_turn_ref)
                .unwrap(),
            &later
        );
    });
    let refused = fixture
        .turn
        .state
        .capture_runtime_observation_report(&fixture.turn.session_id, &later);
    assert_eq!(
        refused.as_ref().err().unwrap().code.as_deref(),
        Some("runtime_observation_scope_changed")
    );
    fixture
        .turn
        .state
        .install_runtime_closing_result(&fixture.turn.session_id, &later, refused)
        .unwrap();
    fixture.turn.record(|record| {
        assert!(
            record
                .engram
                .unadmitted_reports
                .iter()
                .all(|report| report.host_turn_ref != later.trace.host_turn_ref)
        );
        assert!(
            record
                .engram
                .unadmitted_closing_jobs
                .iter()
                .find(|job| job.trace.host_turn_ref == later.trace.host_turn_ref)
                .unwrap()
                .terminal_failure
                .is_some()
        );
    });
}

#[test]
fn an_older_snapshot_members_own_retry_terminal_releases_the_successor() {
    let fixture = TwoClaims::new("snapshot-older-terminal");
    let older = older_turn_in_retry(&fixture);
    let newer = close_newer_turn(&fixture, true);
    queue_b(&fixture);
    fixture.release_worker();
    assert_report(&fixture, &newer, false);
    fixture.turn.record(|record| {
        assert!(record.engram.unadmitted_active_turn.is_none());
        assert_eq!(record.queued_prompts.len(), 1);
    });
    // Seeing Y's first request does not settle its other envelopes. Their
    // delivery policy reads share the scripted error queue with X's capture.
    let delivery_guard = phase_sync::PollGuard::new();
    loop {
        let delivered = fixture.turn.record(|record| {
            record
                .engram
                .unadmitted_reports
                .iter()
                .find(|report| report.host_turn_ref == newer.trace.host_turn_ref)
                .is_some_and(|report| {
                    !report.envelopes.is_empty()
                        && report.envelopes.iter().all(|envelope| {
                            matches!(
                                envelope.delivery,
                                EngramRuntimeObservationDelivery::Recorded { .. }
                            )
                        })
                })
        });
        if delivered {
            break;
        }
        delivery_guard.wait("all Y envelopes settle before injecting X's capture failure");
    }
    assert!(
        fixture
            .turn
            .transport
            .observation_policy_errors
            .lock()
            .unwrap()
            .is_empty()
    );
    fixture
        .turn
        .transport
        .observation_policy_errors
        .lock()
        .unwrap()
        .push_back(EngramTransportError::transport(
            "older X exhausted its own capture budget",
        ));
    {
        let mut inner = fixture.turn.state.inner.lock().unwrap();
        let job = inner
            .session_mut(&fixture.turn.session_id)
            .unwrap()
            .engram
            .unadmitted_closing_jobs
            .iter_mut()
            .find(|job| job.trace.host_turn_ref == older.trace.host_turn_ref)
            .unwrap();
        assert!(job.policy.is_none(), "X must still need its policy capture");
        job.attempts = ENGRAM_RUNTIME_OBSERVATION_MAX_ATTEMPTS - 1;
        job.retry_at = unadmitted_observation_timestamp();
        let prepared = job.clone();
        fixture
            .turn
            .state
            .commit_session_locked(&mut inner, &fixture.turn.session_id)
            .unwrap();
        assert_eq!(
            serde_json::to_value(saved_job(&fixture, &older.trace.host_turn_ref)).unwrap(),
            serde_json::to_value(&prepared).unwrap()
        );
    }
    let guard = phase_sync::PollGuard::new();
    loop {
        if fixture
            .turn
            .record(|record| record.engram.unadmitted_active_turn.is_some())
        {
            break;
        }
        guard.wait("the older member's own terminal releases B without an A report");
    }
    fixture.turn.record(|record| {
        assert_eq!(record.engram.unadmitted_active_turn.as_ref().unwrap()
            .opening_root.as_ref().unwrap().binding, fixture.second);
        assert!(record.queued_prompts.is_empty());
        assert!(record.engram.unadmitted_reports.iter().all(|report|
            report.host_turn_ref != older.trace.host_turn_ref));
        let failed = record.engram.unadmitted_closing_jobs.iter().find(|job|
            job.trace.host_turn_ref == older.trace.host_turn_ref).unwrap();
        assert_eq!(failed.attempts, ENGRAM_RUNTIME_OBSERVATION_MAX_ATTEMPTS);
        assert!(failed.terminal_failure.is_some());
        assert!(failed.terminal_failure.as_ref().unwrap().contains("older X exhausted its own capture budget"));
        assert!(record.session.messages.iter().any(|message|
            matches!(message, Message::Text {text, ..}
                if text.contains(&older.trace.host_turn_ref) && text.contains("no predecessor report:"))));
    });
    fixture.check_in("B-after-older-terminal", &fixture.second_root);
    fixture
        .turn
        .transport
        .enable_named_roots(&fixture.turn.session_id, &fixture.second);
    assert_eq!(fixture.turn.finish()["binding"], json!(fixture.second));
}
