//! Deferred selection closes the predecessor durably before controlled dispatch.
//! Unsolicited adoption instead retains the original facts without certainty.
use super::*;

impl TwoClaims {
    fn stop_before_capture(&self, uncertain: bool) -> EngramRuntimeClosingJob {
        self.turn.begin("A owns this stopped turn.");
        self.check_first("only-A-check");
        self.name_other();
        let runtime = {
            let mut inner = self.turn.state.inner.lock().unwrap();
            let id = inner.next_message_id();
            let record = inner.session_mut(&self.turn.session_id).unwrap();
            record.engram.unadmitted_worker_running = true;
            if uncertain {
                record
                    .engram
                    .unadmitted_active_turn
                    .as_mut()
                    .unwrap()
                    .source_attribution_uncertain = true;
            }
            // Keep the fixture's channel runtime for the later real dispatch;
            // Stop sees the provider detached and still owns its terminal facts.
            let runtime = record.runtime.clone();
            record.runtime = SessionRuntime::None;
            queue_prompt_on_record(
                record,
                PendingPrompt {
                    attachments: Vec::new(),
                    id,
                    timestamp: stamp_now(),
                    text: "The first successor belongs to B.".into(),
                    expanded_text: None,
                    source: None,
                    engram_interrupted: false,
                    is_engram_retained: false,
                },
                Vec::new(),
            );
            self.turn.state.commit_locked(&mut inner).unwrap();
            runtime
        };
        self.turn
            .state
            .stop_session_with_options(
                &self.turn.session_id,
                StopSessionOptions {
                    dispatch_queued_prompts_on_success: true,
                    pause_automatic_resumes_on_success: false,
                    orchestrator_stop_instance_id: None,
                },
            )
            .unwrap();
        self.turn.record_mut(|record| {
            assert!(record.engram.unadmitted_active_turn.is_none());
            assert_eq!(record.engram.work_binding.as_ref(), Some(&self.first));
            assert_eq!(
                record.queued_prompts.len(),
                1,
                "B cannot open before A capture is durable"
            );
            assert!(engram_named_selection_capture_pending(record));
            record.runtime = runtime;
            let job = record.engram.unadmitted_closing_jobs.last().unwrap();
            assert_eq!(job.scope.binding.as_ref(), Some(&self.first));
            assert_eq!(job.trace.opening_scope.as_ref(), Some(&job.scope));
            job.clone()
        })
    }

    fn release_worker(&self) {
        self.turn
            .record_mut(|record| record.engram.unadmitted_worker_running = false);
        self.turn
            .state
            .start_runtime_observation_worker(&self.turn.session_id);
    }

    fn wait_for_recorded_while_active(
        &self,
        job: &EngramRuntimeClosingJob,
        successor: &EngramRuntimeObservedTurn,
    ) {
        let guard = phase_sync::PollGuard::new();
        loop {
            let recorded = self.turn.record(|record| {
                assert_eq!(record.engram.work_binding.as_ref(), Some(&self.second));
                assert_eq!(
                    record.engram.unadmitted_active_turn.as_ref(),
                    Some(successor)
                );
                record
                    .engram
                    .unadmitted_reports
                    .iter()
                    .find(|report| report.host_turn_ref == job.trace.host_turn_ref)
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
            if recorded {
                break;
            }
            guard.wait("the exact A report is recorded without modifying active B");
        }
    }
}

fn assert_stop_capture_barrier(named: bool, uncertain: bool, label: &str) {
    let fixture = TwoClaims::with_named_root(label, named);
    let job = fixture.stop_before_capture(uncertain);
    if !named {
        assert_eq!(job.scope.root, EngramNamedRootState::None);
    }
    fixture.release_worker();
    let request = fixture.turn.observation_for(&job.trace.host_turn_ref);
    assert_eq!(request["binding"], json!(fixture.first));
    assert_eq!(
        request["occurrence"]["observed_checks"][0]["host_check_id"],
        json!(job.trace.commands[0].check.host_check_id)
    );
    assert_ordinary_observation_has_no_native_credit(&fixture.turn, &request);
    if uncertain {
        assert!(request["occurrence"]["source_change"].is_null());
    }
    let guard = phase_sync::PollGuard::new();
    let successor = loop {
        let opening = fixture
            .turn
            .record(|record| record.engram.unadmitted_active_turn.clone());
        if let Some(opening) = opening {
            break opening;
        }
        guard.wait("B opens only after A's exact report readback");
    };
    assert_ne!(successor.host_turn_ref, job.trace.host_turn_ref);
    assert_eq!(
        successor.opening_root.as_ref().unwrap().binding,
        fixture.second
    );
    let report = fixture.turn.record(|record| {
        assert!(record.engram.pending_naming_selection.is_none());
        record
            .engram
            .unadmitted_reports
            .iter()
            .find(|report| report.host_turn_ref == job.trace.host_turn_ref)
            .unwrap()
            .clone()
    });
    // Delivery may already advance the report's version. The committed B row
    // must retain the identical A intent, independently of receipt progress.
    let saved: PersistedSessionRecord = {
        let db = open_sqlite_state_read_connection(&fixture.turn.state.persistence_path).unwrap();
        let raw: String = db
            .query_row(
                "SELECT value_json FROM sessions WHERE id = ?1",
                [&fixture.turn.session_id],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&raw).unwrap()
    };
    assert_eq!(
        saved
            .engram_unadmitted_active_turn
            .as_ref()
            .unwrap()
            .host_turn_ref,
        successor.host_turn_ref
    );
    let saved_report = saved
        .engram_unadmitted_reports
        .iter()
        .find(|retained| retained.host_turn_ref == job.trace.host_turn_ref)
        .unwrap();
    assert_eq!(
        saved_report
            .envelopes
            .iter()
            .map(|e| &e.request)
            .collect::<Vec<_>>(),
        report
            .envelopes
            .iter()
            .map(|e| &e.request)
            .collect::<Vec<_>>()
    );
    fixture.check_in("only-B-check", &fixture.second_root);
    let successor = fixture
        .turn
        .record(|record| record.engram.unadmitted_active_turn.clone().unwrap());
    fixture.wait_for_recorded_while_active(&job, &successor);
    fixture
        .turn
        .transport
        .enable_named_roots(&fixture.turn.session_id, &fixture.second);
    let request = fixture.turn.finish();
    assert_eq!(request["binding"], json!(fixture.second));
    assert_eq!(
        request["occurrence"]["observed_checks"][0]["host_check_id"],
        json!(successor.commands[0].check.host_check_id)
    );
    assert_ne!(
        successor.commands[0].check.host_check_id,
        job.trace.commands[0].check.host_check_id
    );
    assert_ordinary_observation_has_no_native_credit(&fixture.turn, &request);
}

#[test]
fn stop_queued_successor_waits_for_the_named_predecessors_durable_capture() {
    assert_stop_capture_barrier(true, false, "stop-named-capture-barrier");
}

#[test]
fn stop_queued_successor_preserves_the_unnamed_predecessors_workdir_scope() {
    assert_stop_capture_barrier(false, false, "stop-workdir-capture-barrier");
}

#[test]
fn uncertain_predecessor_is_recorded_unadmitted_before_the_queued_successor() {
    assert_stop_capture_barrier(true, true, "stop-uncertain-capture-barrier");
}

#[test]
fn adopted_successor_opens_on_b_and_retains_a_uncertain_without_modifying_b() {
    let fixture = TwoClaims::new("adopted-before-predecessor-capture");
    fixture.turn.begin("A runs before the unsolicited turn.");
    fixture.check_first("A-before-adopted");
    fixture.name_other();
    fixture
        .turn
        .record_mut(|record| record.engram.unadmitted_worker_running = true);
    let token = fixture
        .turn
        .record(|record| record.runtime.runtime_token().unwrap());
    fixture.turn.end();
    let original = fixture.turn.record(|record| {
        record
            .engram
            .unadmitted_closing_jobs
            .last()
            .unwrap()
            .clone()
    });
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
    let job = fixture.turn.record(|record| {
        assert_eq!(record.engram.work_binding.as_ref(), Some(&fixture.second));
        assert_eq!(
            record
                .engram
                .unadmitted_active_turn
                .as_ref()
                .unwrap()
                .opening_root
                .as_ref()
                .unwrap()
                .binding,
            fixture.second
        );
        let job = record.engram.unadmitted_closing_jobs.last().unwrap();
        assert_eq!(job.trace.host_turn_ref, original.trace.host_turn_ref);
        assert_eq!(job.scope.binding, original.scope.binding);
        assert!(job.trace.source_attribution_uncertain);
        assert_eq!(
            job.scope
                .historical_fields
                .get("adopted_successor_reason")
                .and_then(Value::as_str),
            Some(ENGRAM_ADOPTED_PREDECESSOR_REASON)
        );
        job.clone()
    });
    fixture.check_in("B-after-adopted", &fixture.second_root);
    let successor = fixture
        .turn
        .record(|record| record.engram.unadmitted_active_turn.clone().unwrap());
    fixture.release_worker();
    let request = fixture.turn.observation_for(&job.trace.host_turn_ref);
    assert_eq!(request["binding"], json!(fixture.first));
    assert!(request["occurrence"]["source_change"].is_null());
    assert_eq!(
        request["occurrence"]["observed_checks"][0]["host_check_id"],
        json!(original.trace.commands[0].check.host_check_id)
    );
    assert_ordinary_observation_has_no_native_credit(&fixture.turn, &request);
    fixture.wait_for_recorded_while_active(&job, &successor);
    fixture.turn.record(|record| {
        assert!(record.session.messages.iter().any(|message| {
            matches!(message, Message::Text { text, .. }
                if text.contains(&job.trace.host_turn_ref)
                    && text.contains(ENGRAM_ADOPTED_PREDECESSOR_REASON))
        }))
    });
    fixture
        .turn
        .transport
        .enable_named_roots(&fixture.turn.session_id, &fixture.second);
    let request = fixture.turn.finish();
    assert_eq!(request["binding"], json!(fixture.second));
    assert_eq!(
        request["occurrence"]["observed_checks"][0]["host_check_id"],
        json!(successor.commands[0].check.host_check_id)
    );
}

#[test]
fn restored_generation_collision_never_selects_an_unrelated_closing_job() {
    let fixture = TwoClaims::new("restored-naming-generation-collision");
    fixture.turn.begin("Old generation one A.");
    let runtime = fixture.turn.record(|record| record.runtime.clone());
    fixture.check_first("old-A-check");
    fixture
        .turn
        .record_mut(|record| record.engram.unadmitted_worker_running = true);
    fixture.turn.end();
    let restored = load_state(&fixture.turn.state.persistence_path)
        .unwrap()
        .unwrap();
    let mut record = restored.sessions[restored
        .find_session_index(&fixture.turn.session_id)
        .unwrap()]
    .clone();
    assert_eq!(record.active_turn_generation, 0);
    let old = record
        .engram
        .unadmitted_closing_jobs
        .last()
        .unwrap()
        .clone();
    assert_eq!(old.trace.generation, 1);
    assert!(old.trace.opening_scope.is_none());
    record.runtime = runtime;
    record.engram.work_binding = Some(fixture.first.clone());
    record.engram.named_root = Some(old.scope.root.clone());
    record.engram.unadmitted_worker_running = true;
    {
        let mut inner = fixture.turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.turn.session_id).unwrap();
        inner.sessions[index] = record;
    }
    fixture.turn.begin("New generation one A after restore.");
    assert_eq!(
        fixture.turn.record(|record| record.active_turn_generation),
        old.trace.generation
    );
    fixture.name_other();
    fixture.turn.end();
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
    fixture.turn.record_mut(|record| {
        let retained = record
            .engram
            .unadmitted_closing_jobs
            .iter()
            .find(|job| job.trace.host_turn_ref == old.trace.host_turn_ref)
            .unwrap();
        assert_eq!(
            retained, &old,
            "the restored job never inherits the new turn's transition"
        );
        let current = record
            .engram
            .unadmitted_closing_jobs
            .iter()
            .find(|job| job.trace.host_turn_ref != old.trace.host_turn_ref)
            .unwrap();
        assert!(current.trace.source_attribution_uncertain);
        assert_ne!(current.trace.host_turn_ref, old.trace.host_turn_ref);
        record.engram.unadmitted_closing_jobs.clear();
        record.engram.unadmitted_worker_running = false;
    });
}
