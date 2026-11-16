//! Naming RPCs may span turn boundaries; their acknowledgement cannot lose
//! the actual predecessor or leave a terminal predecessor blocking dispatch.
use super::*;

fn name_other_at_bind_reply(fixture: &std::rc::Rc<TwoClaims>, meanwhile: impl FnOnce() + 'static) {
    TEST_ENGRAM_AFTER_NAMING_BIND_REPLY.with(|hook| {
        assert!(hook.borrow().is_none());
        *hook.borrow_mut() = Some(Box::new(meanwhile));
    });
    fixture.name_other();
    TEST_ENGRAM_AFTER_NAMING_BIND_REPLY.with(|hook| assert!(hook.borrow().is_none()));
}

fn assert_exact_predecessor_pending(fixture: &TwoClaims, turn_ref: &str) {
    fixture.turn.record(|record| {
        assert_eq!(
            record.engram.work_binding.as_ref(),
            Some(&fixture.first),
            "an in-flight name cannot install B over a turn boundary"
        );
        let pending = record
            .engram
            .pending_naming_selection
            .as_ref()
            .expect("the acknowledged name remains pending for an actual opening");
        assert_eq!(
            pending.named_turn_ref.as_deref(),
            Some(turn_ref),
            "the barrier belongs to the actual predecessor, never a null or stale turn"
        );
        assert!(
            pending
                .predecessors
                .iter()
                .any(|member| member.turn_ref == turn_ref && !member.capture_durable)
        );
        assert!(engram_named_selection_capture_pending(record));
    });
}

fn assert_predecessor_capture_then_b(fixture: &TwoClaims, turn_ref: &str) {
    let result = fixture
        .turn
        .state
        .dispatch_turn(
            &fixture.turn.session_id,
            SendMessageRequest {
                text: "B waits for the actual predecessor.".into(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .unwrap();
    assert!(matches!(result, DispatchTurnResult::Queued));
    let job = fixture.turn.record(|record| {
        assert!(record.engram.unadmitted_active_turn.is_none());
        assert_eq!(record.queued_prompts.len(), 1);
        record
            .engram
            .unadmitted_closing_jobs
            .iter()
            .find(|job| job.trace.host_turn_ref == turn_ref)
            .unwrap()
            .clone()
    });
    fixture.release_worker();
    let request = fixture.turn.observation_for(turn_ref);
    assert_eq!(request["binding"], json!(fixture.first));
    assert_eq!(
        request["occurrence"]["observed_checks"][0]["host_check_id"],
        json!(job.trace.commands[0].check.host_check_id)
    );
    assert_ordinary_observation_has_no_native_credit(&fixture.turn, &request);
    let guard = phase_sync::PollGuard::new();
    let successor = loop {
        if let Some(trace) = fixture
            .turn
            .record(|record| record.engram.unadmitted_active_turn.clone())
        {
            break trace;
        }
        guard.wait("B opens after the actual predecessor's durable report");
    };
    assert_eq!(
        successor.opening_root.as_ref().unwrap().binding,
        fixture.second
    );
    fixture.wait_for_recorded_while_active(&job, &successor);
    fixture.turn.record(|record| {
        assert!(record.engram.pending_naming_selection.is_none());
        assert!(
            record
                .engram
                .unadmitted_reports
                .iter()
                .any(|report| report.host_turn_ref == turn_ref)
        );
    });
    fixture.check_in("B-after-racing-name", &fixture.second_root);
    fixture
        .turn
        .transport
        .enable_named_roots(&fixture.turn.session_id, &fixture.second);
    assert_eq!(fixture.turn.finish()["binding"], json!(fixture.second));
}

#[test]
fn a_turn_ending_during_naming_keeps_its_report_before_b_opens() {
    let fixture = std::rc::Rc::new(TwoClaims::new("name-bind-ends-A"));
    fixture.turn.begin("A ends while naming is off lock.");
    fixture.check_first("A-before-in-flight-name");
    let turn_ref = fixture.turn.record(|record| {
        record
            .engram
            .unadmitted_active_turn
            .as_ref()
            .unwrap()
            .host_turn_ref
            .clone()
    });
    fixture
        .turn
        .record_mut(|record| record.engram.unadmitted_worker_running = true);
    let during = std::rc::Rc::clone(&fixture);
    name_other_at_bind_reply(&fixture, move || during.turn.end());
    assert_exact_predecessor_pending(&fixture, &turn_ref);
    assert_predecessor_capture_then_b(&fixture, &turn_ref);
}

#[test]
fn naming_between_turns_preserves_the_predecessor_already_waiting_for_capture() {
    let fixture = TwoClaims::new("name-between-turns-before-capture");
    fixture
        .turn
        .begin("A completes before the naming RPC begins.");
    fixture.check_first("A-before-idle-name");
    let turn_ref = fixture.turn.record(|record| {
        record
            .engram
            .unadmitted_active_turn
            .as_ref()
            .unwrap()
            .host_turn_ref
            .clone()
    });
    fixture
        .turn
        .record_mut(|record| record.engram.unadmitted_worker_running = true);
    fixture.turn.end();
    fixture.name_other();
    assert_exact_predecessor_pending(&fixture, &turn_ref);
    assert_predecessor_capture_then_b(&fixture, &turn_ref);
}

#[test]
fn a_turn_starting_during_idle_naming_becomes_the_exact_predecessor() {
    let fixture = std::rc::Rc::new(TwoClaims::new("name-bind-idle-to-A"));
    // The first dispatch attaches a runtime. Settle it before testing a turn
    // boundary within the same runtime; naming must still reject replacement
    // of the runtime identity it captured before its remote reads.
    fixture.assert_next_turn(&fixture.first, &fixture.first_root);
    wait_for_observation_delivery(&fixture.turn);
    let before = {
        let inner = fixture.turn.state.inner.lock().unwrap();
        AppState::engram_source_root_target_locked(&inner, &fixture.turn.session_id)
            .unwrap()
            .unwrap()
    };
    assert!(before.runtime_snapshot.is_some());
    let idle_generation = assert_naming_fence(&fixture, &before, "before_naming_read");
    fixture
        .turn
        .record_mut(|record| record.engram.unadmitted_worker_running = true);
    let during = std::rc::Rc::clone(&fixture);
    name_other_at_bind_reply(&fixture, move || {
        during.turn.begin("A starts during the idle naming RPC.");
        let generation = assert_naming_fence(&during, &before, "after_A_opening");
        assert_eq!(generation, idle_generation + 1);
        // The existing hook returns directly to bind_fresh's guarded lock.
        assert_eq!(
            assert_naming_fence(&during, &before, "before_guard_compare"),
            generation
        );
    });
    fixture.check_first("A-after-in-flight-name");
    let turn_ref = fixture.turn.record(|record| {
        record
            .engram
            .unadmitted_active_turn
            .as_ref()
            .unwrap()
            .host_turn_ref
            .clone()
    });
    assert_exact_predecessor_pending(&fixture, &turn_ref);
    fixture.turn.end();
    assert_predecessor_capture_then_b(&fixture, &turn_ref);
}

fn assert_naming_fence(fixture: &TwoClaims, expected: &EngramBindingTarget, phase: &str) -> u64 {
    let inner = fixture.turn.state.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.turn.session_id).unwrap();
    let record = &inner.sessions[index];
    let now = AppState::engram_source_root_target_locked(&inner, &fixture.turn.session_id)
        .unwrap()
        .unwrap();
    let settings_current = now.settings.same_work_service_settings(&expected.settings);
    eprintln!(
        "NAMING_FENCE phase={phase} runtime={:?} external_ref={:?} generation={} connection={:?} settings_current={settings_current} binding={:?}",
        now.runtime_snapshot,
        now.external_ref,
        record.active_turn_generation,
        now.connection,
        now.work_binding
    );
    assert_eq!(now.runtime_snapshot, expected.runtime_snapshot);
    assert_eq!(now.external_ref, expected.external_ref);
    assert_eq!(now.connection, expected.connection);
    assert!(settings_current);
    assert_eq!(now.work_binding, expected.work_binding);
    record.active_turn_generation
}

#[test]
fn a_new_turn_during_naming_replaces_the_stale_predecessor_identity() {
    let fixture = std::rc::Rc::new(TwoClaims::new("name-bind-X-to-Y"));
    fixture.turn.begin("X is the request-entry turn.");
    fixture.check_first("X-before-racing-name");
    let old_turn = fixture.turn.record(|record| {
        record
            .engram
            .unadmitted_active_turn
            .as_ref()
            .unwrap()
            .host_turn_ref
            .clone()
    });
    fixture
        .turn
        .record_mut(|record| record.engram.unadmitted_worker_running = true);
    let during = std::rc::Rc::clone(&fixture);
    name_other_at_bind_reply(&fixture, move || {
        during.turn.end();
        during
            .turn
            .begin("Y is the actual predecessor at acknowledgement.");
    });
    fixture.check_first("Y-after-racing-name");
    let turn_ref = fixture.turn.record(|record| {
        record
            .engram
            .unadmitted_active_turn
            .as_ref()
            .unwrap()
            .host_turn_ref
            .clone()
    });
    assert_ne!(turn_ref, old_turn);
    assert_exact_predecessor_pending(&fixture, &turn_ref);
    fixture.turn.end();
    assert_predecessor_capture_then_b(&fixture, &turn_ref);
    fixture.turn.record(|record| {
        assert!(
            record
                .engram
                .unadmitted_reports
                .iter()
                .any(|report| report.host_turn_ref == old_turn),
            "X's report also remains on A"
        );
    });
}

#[test]
fn terminal_no_report_releases_the_named_successor_with_a_diagnostic() {
    let fixture = TwoClaims::new("name-terminal-no-report");
    let job = fixture.stop_before_capture(false);
    // A was released after its close; only B is now held and focused. The
    // real capture owner must retain its existing no-report refusal.
    fixture
        .turn
        .transport
        .set_current_work_binding(Some(fixture.second.clone()));
    fixture
        .turn
        .transport
        .replace_held_claims([Ok(EngramHeldClaims {
            items: vec![EngramHeldClaim {
                work_id: fixture.second.work_id.clone(),
                short_ref: "w-other-named-claim".into(),
                claim_id: fixture.second.claim_id.clone(),
                claim_fence: fixture.second.claim_fence,
                focused: true,
                control_binding: Some(fixture.second.clone()),
            }],
            omitted: 0,
        })]);
    fixture.release_worker();
    let guard = phase_sync::PollGuard::new();
    loop {
        let terminal = fixture.turn.record(|record| {
            record
                .engram
                .unadmitted_closing_jobs
                .iter()
                .find(|retained| retained.trace.host_turn_ref == job.trace.host_turn_ref)
                .is_some_and(|retained| retained.terminal_failure.is_some())
        });
        if terminal {
            break;
        }
        guard.wait("the exact A job reaches its real no-report terminal outcome");
    }
    fixture.turn.record(|record| {
        assert!(
            !engram_named_selection_capture_pending(record),
            "a terminal no-report outcome must not hold every later prompt"
        );
        assert!(
            record
                .engram
                .unadmitted_reports
                .iter()
                .all(|report| report.host_turn_ref != job.trace.host_turn_ref),
            "no fabricated A report"
        );
        assert!(
            record.session.messages.iter().any(|message| {
                matches!(message, Message::Text { text, .. }
                if text.contains(&job.trace.host_turn_ref)
                    && text.contains("no predecessor report:"))
            }),
            "the terminal release records its exact turn and reason"
        );
    });
    loop {
        if fixture
            .turn
            .record(|record| record.engram.unadmitted_active_turn.is_some())
        {
            break;
        }
        guard.wait("the acknowledged B successor dispatches after no-report settlement");
    }
    fixture.turn.record(|record| {
        let successor = record.engram.unadmitted_active_turn.as_ref().unwrap();
        assert_eq!(
            successor.opening_root.as_ref().unwrap().binding,
            fixture.second
        );
        assert!(record.queued_prompts.is_empty());
    });
}
