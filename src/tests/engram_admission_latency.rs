//! Measured admission exhaustion must not keep repeating the same budget.
//! All elapsed time is injected through the existing operation clock; no load
//! or real-time sleep supplies the failure. Writer receipts below are scripted
//! fence outcomes, not a live Engram or SQLite claim.
use super::*;

fn measured_exhausted_head() -> (SessionRecord, String, EngramBudgetClock) {
    let (state, session, _receiver, _transport) = parked_after_deadline([]);
    let clock = state.engram_budget_clock();
    clock.advance(Duration::from_millis(25_315));
    let mut record = with_record(&state, &session, Clone::clone);
    let prompt_id = record
        .queued_prompts
        .front()
        .unwrap()
        .pending_prompt
        .id
        .clone();
    let active_turn_generation = record.active_turn_generation;
    let authority = record
        .engram
        .admission_retry
        .as_ref()
        .unwrap()
        .authority
        .clone();
    record.engram.admission_retry = None;
    let card = record
        .session
        .messages
        .matching_mut(|message| matches!(message, Message::EngramControl { .. }))
        .rev()
        .find_map(|(_, message)| match message {
            Message::EngramControl { card, .. } => Some(card),
            _ => None,
        })
        .unwrap();
    card.refusal_code = Some("dispatch_budget_exhausted".to_owned());
    // The fence is already inside evaluate, not an additional 12,248 ms.
    card.latency_ms.evaluate = Some(18_248);
    card.latency_ms.admission_fence = Some(12_248);
    card.latency_ms.total = 25_315;
    let cause = card.causal_failure.as_mut().unwrap();
    cause.continuation_id = Some(prompt_id);
    cause.turn_generation = Some(active_turn_generation);
    cause.authority_fingerprint = Some(authority.clone());
    (record, authority, clock)
}

#[test]
fn admission_latency_hold_exhaustion_does_not_schedule_the_same_attempt() {
    let (mut record, authority, clock) = measured_exhausted_head();
    let head = record
        .queued_prompts
        .front()
        .unwrap()
        .pending_prompt
        .id
        .clone();
    let decision = decide_engram_admission_retry_locked(
        &mut record,
        Some("dispatch_budget_exhausted"),
        Some(&authority),
        clock.wall_now(),
        clock.now(),
    );
    let EngramAdmissionRetryDecision::LatencyHold(explanation) = decision else {
        panic!("a measured over-budget admission needs an explicit latency hold");
    };
    assert_eq!(explanation, record.session.preview);
    assert!(record.engram.admission_retry.is_none());
    assert_eq!(
        record.queued_prompts.front().unwrap().pending_prompt.id,
        head
    );
    assert!(record.queued_prompts.front().unwrap().engram_waiting);
}

#[test]
fn admission_latency_hold_does_not_label_a_failed_begin_reconstruction_schedule() {
    let (mut record, authority, clock) = measured_exhausted_head();
    record.engram.active_grant_id = None;
    record.engram.uncertain_grant_id = Some("reconstruction-grant".to_owned());
    assert!(engram_begin_reconstruction_shape(&record).is_some());
    assert!(schedule_engram_admission_retry_locked(
        &mut record,
        Some(ENGRAM_BEGIN_RECONSTRUCT_CODE),
        Some(&authority),
        clock.wall_now(),
        clock.now(),
    ));
    record.runtime_stop_in_progress = true;
    let decision = decide_engram_admission_retry_locked(
        &mut record,
        Some("dispatch_budget_exhausted"),
        Some(&authority),
        clock.wall_now(),
        clock.now(),
    );
    assert!(matches!(decision, EngramAdmissionRetryDecision::NotScheduled));
    assert!(record.engram.admission_retry.is_none());
    assert!(!record.session.preview.contains("admission latency hold"));
}

#[test]
fn admission_latency_hold_does_not_reclassify_success_or_other_failures() {
    let (record, authority, _) = measured_exhausted_head();
    for case in [
        "grant",
        "grantless",
        "transport",
        "other_code",
        "within_budget",
        "interrupted",
        "not_waiting",
    ] {
        let mut candidate = record.clone();
        let card = candidate
            .session
            .messages
            .matching_mut(|message| matches!(message, Message::EngramControl { .. }))
            .rev()
            .find_map(|(_, message)| match message {
                Message::EngramControl { card, .. } => Some(card),
                _ => None,
            })
            .unwrap();
        match case {
            "grant" => {
                card.decision = EngramControlCardDecision::Grant;
                card.latency_ms.total = ENGRAM_DISPATCH_BUDGET_MS;
            }
            "grantless" => card.dispatch = EngramControlCardDispatch::SentWithoutGrant,
            "transport" => {
                card.causal_failure.as_mut().unwrap().failure_class =
                    EngramCausalFailureClass::Transport
            }
            "other_code" => card.refusal_code = Some("control_unavailable".to_owned()),
            "within_budget" => card.latency_ms.total = ENGRAM_DISPATCH_BUDGET_MS - 1,
            "interrupted" => {
                candidate
                    .queued_prompts
                    .front_mut()
                    .unwrap()
                    .engram_interrupted = true
            }
            "not_waiting" => candidate.queued_prompts.front_mut().unwrap().engram_waiting = false,
            _ => unreachable!(),
        }
        assert!(
            engram_admission_latency_hold(
                &candidate,
                Some("dispatch_budget_exhausted"),
                Some(&authority)
            )
            .is_none(),
            "{case}: elapsed time alone is not permission to hold"
        );
    }
}

#[test]
fn admission_latency_hold_preserves_the_original_head_and_window() {
    let (mut record, authority, clock) = measured_exhausted_head();
    let head_before = serde_json::to_value(record.queued_prompts.front().unwrap()).unwrap();
    let lineage_before = serde_json::to_value(&record.engram.begin_preparation_lineage).unwrap();
    let generation_before = record.engram.dispatch_generation;
    assert!(!schedule_engram_admission_retry_locked(
        &mut record,
        Some("dispatch_budget_exhausted"),
        Some(&authority),
        clock.wall_now(),
        clock.now(),
    ));
    assert_eq!(
        serde_json::to_value(record.queued_prompts.front().unwrap()).unwrap(),
        head_before
    );
    assert_eq!(
        serde_json::to_value(&record.engram.begin_preparation_lineage).unwrap(),
        lineage_before
    );
    assert_eq!(record.engram.dispatch_generation, generation_before);
}

#[test]
fn admission_latency_hold_names_the_observed_cost_without_double_counting() {
    let (mut record, authority, clock) = measured_exhausted_head();
    schedule_engram_admission_retry_locked(
        &mut record,
        Some("dispatch_budget_exhausted"),
        Some(&authority),
        clock.wall_now(),
        clock.now(),
    );
    let explanation = &record.session.preview;
    for number in ["25315", "18248", "12248", "20000"] {
        assert!(
            explanation.contains(number),
            "latency hold must name measured total/evaluate/included fence and budget: {explanation}"
        );
    }
    assert!(
        !explanation.contains("retrying automatically"),
        "the operator must not be promised a doomed retry"
    );
}

#[test]
fn admission_latency_hold_renders_missing_measurements_without_units() {
    let (mut record, authority, clock) = measured_exhausted_head();
    let card = record
        .session
        .messages
        .matching_mut(|message| matches!(message, Message::EngramControl { .. }))
        .rev()
        .find_map(|(_, message)| match message {
            Message::EngramControl { card, .. } => Some(card),
            _ => None,
        })
        .unwrap();
    card.latency_ms.evaluate = None;
    card.latency_ms.admission_fence = None;
    assert!(!schedule_engram_admission_retry_locked(
        &mut record,
        Some("dispatch_budget_exhausted"),
        Some(&authority),
        clock.wall_now(),
        clock.now(),
    ));
    assert!(record.session.preview.contains(
        "admission/evaluate span unavailable (includes admission fence unavailable)"
    ));
    assert!(!record.session.preview.contains("unavailable ms"));
    assert!(record.session.preview.contains("total 25315 ms"));
    assert!(record.session.preview.contains("budget 20000 ms"));
}

/// The producer completes Evaluate after the attempt budget, on the same
/// operation clock used by admission. No measured card is fabricated here.
struct LateEvaluateProducer {
    producer: Arc<StatefulEngramControlTransport>,
    clock: EngramBudgetClock,
    late_once: AtomicBool,
    late_bind: bool,
}

impl EngramControlTransport for LateEvaluateProducer {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        let response = self.producer.request(connection, request, timeout);
        let late_operation = if self.late_bind {
            matches!(request, EngramControlRequest::SessionBind { .. })
        } else {
            matches!(request, EngramControlRequest::TurnEvaluate { .. })
        };
        if late_operation && self.late_once.swap(false, Ordering::SeqCst) {
            self.clock.advance(Duration::from_secs(21));
        }
        response
    }

    fn shutdown_session(&self, session_id: &str) {
        self.producer.shutdown_session(session_id);
    }
}

fn last_dispatch_card(record: &SessionRecord) -> &EngramControlCard {
    record
        .session
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::EngramControl { card, .. } if card.stage == EngramControlStage::Dispatch => {
                Some(card)
            }
            _ => None,
        })
        .expect("actual dispatch card")
}

#[test]
fn admission_latency_hold_actual_admission_uses_one_clock_and_survives_ticks() {
    let (state, session, receiver, _) = root_fixture([]);
    let clock = state.engram_budget_clock();
    clock.pin_wall();
    let producer = Arc::new(LateEvaluateProducer {
        producer: StatefulEngramControlTransport::new(),
        clock: clock.clone(),
        late_once: AtomicBool::new(true),
        late_bind: false,
    });
    state.install_control_test_transport(producer.clone());
    // The shared control-ordering fixture supplies a much larger deadlock
    // guard. This test opts back into the production 20 s dispatch bound.
    state.inner.lock().unwrap().test_engram_dispatch_budget = None;
    let dispatch = root_dispatch(&state, &session, false);
    let before = with_record(&state, &session, |record| {
        (
            record
                .queued_prompts
                .front()
                .unwrap()
                .pending_prompt
                .id
                .clone(),
            record.engram.dispatch_generation,
            serde_json::to_value(&record.engram.begin_preparation_lineage).unwrap(),
        )
    });
    deliver_turn_dispatch(&state, dispatch).unwrap();
    let parked = with_record(&state, &session, Clone::clone);
    let calls = producer.producer.requests().len();
    let card = last_dispatch_card(&parked);
    assert!(
        !producer.producer.requests().iter().any(|request| matches!(
            request.request["operation"].as_str(),
            Some("turn_begin" | "turn_checkpoint")
        )),
        "the exhausted pre-Begin path sends neither Begin nor checkpoint"
    );
    assert_eq!(card.latency_ms.evaluate, Some(21_000));
    assert_eq!(
        card.latency_ms.total, 21_000,
        "the card measures the same clock as the budget, not real time beside scripted time"
    );
    assert_eq!(
        card.refusal_code.as_deref(),
        Some("dispatch_budget_exhausted")
    );
    assert!(parked.session.preview.contains("admission latency hold"));
    assert!(parked.engram.admission_retry.is_none());
    assert!(parked.engram.bind_retry.is_none());
    assert!(receiver.try_recv().is_err());
    for offset in [1, 5, 30] {
        state.engram_abort_retry_tick(clock.wall_now() + chrono::Duration::seconds(offset));
    }
    let after = with_record(&state, &session, Clone::clone);
    assert_eq!(
        producer.producer.requests().len(),
        calls,
        "ordinary recovery ticks never re-evaluate an unchanged latency hold"
    );
    assert_eq!(
        after.queued_prompts.front().unwrap().pending_prompt.id,
        before.0
    );
    assert_eq!(after.engram.dispatch_generation, before.1);
    assert_eq!(
        serde_json::to_value(&after.engram.begin_preparation_lineage).unwrap(),
        before.2
    );
    let cause = card.causal_failure.as_ref().unwrap();
    assert!(
        cause
            .continuation_reason
            .contains("No automatic re-evaluation")
    );
    assert!(
        !cause
            .continuation_reason
            .contains("Automatic retry scheduled")
    );
}

#[test]
fn admission_latency_hold_real_clock_park_names_the_exact_current_attempt() {
    let (state, session, receiver, _) =
        root_fixture_on_real_clock([bind_reply("real-token"), grant_reply("real-grant")]);
    state.inner.lock().unwrap().test_engram_dispatch_budget = None;
    let dispatch = root_dispatch(&state, &session, false);
    let (prompt, generation, authority) = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let authority =
            AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
                .unwrap()
                .map(|target| engram_abort_authority(&target))
                .unwrap();
        let record = &mut inner.sessions[index];
        // Model a dispatch descheduled after Evaluate without sleeping or
        // creating load. The production Real clock still measures this span.
        record.engram.pending_dispatch.as_mut().unwrap().started_at =
            std::time::Instant::now()
                .checked_sub(Duration::from_secs(21))
                .expect("the test host monotonic clock has at least 21 seconds of history");
        (
            record
                .queued_prompts
                .front()
                .unwrap()
                .pending_prompt
                .id
                .clone(),
            record.active_turn_generation,
            authority,
        )
    };
    deliver_turn_dispatch(&state, dispatch).unwrap();
    let record = with_record(&state, &session, Clone::clone);
    let card = last_dispatch_card(&record);
    let cause = card.causal_failure.as_ref().unwrap();
    assert!(card.latency_ms.total >= 21_000);
    assert_eq!(cause.continuation_id.as_deref(), Some(prompt.as_str()));
    assert_eq!(cause.turn_generation, Some(generation));
    assert_eq!(
        cause.authority_fingerprint.as_deref(),
        Some(authority.as_str())
    );
    assert!(record.session.preview.contains("admission latency hold"));
    assert!(record.engram.admission_retry.is_none());
    assert!(receiver.try_recv().is_err());
}

#[test]
fn admission_latency_hold_rejects_a_stale_or_unattributed_dispatch_card() {
    let (record, authority, _) = measured_exhausted_head();
    for mismatch in [
        "prompt",
        "generation",
        "authority",
        "unattributed",
        "checkpoint",
    ] {
        let mut candidate = record.clone();
        let card = candidate
            .session
            .messages
            .matching_mut(|message| matches!(message, Message::EngramControl { .. }))
            .rev()
            .find_map(|(_, message)| match message {
                Message::EngramControl { card, .. } => Some(card),
                _ => None,
            })
            .unwrap();
        let cause = card.causal_failure.as_mut().unwrap();
        match mismatch {
            "prompt" => cause.continuation_id = Some("a-different-head".to_owned()),
            "generation" => cause.turn_generation = Some(u64::MAX),
            "authority" => cause.authority_fingerprint = Some("a-different-authority".to_owned()),
            "unattributed" => {
                cause.continuation_id = None;
                cause.turn_generation = None;
                cause.authority_fingerprint = None;
            }
            "checkpoint" => card.stage = EngramControlStage::Checkpoint,
            _ => unreachable!(),
        }
        assert!(
            engram_admission_latency_hold(
                &candidate,
                Some("dispatch_budget_exhausted"),
                Some(&authority)
            )
            .is_none(),
            "{mismatch}: a stale card cannot decide the current head's retry policy"
        );
    }
}

#[test]
fn admission_latency_hold_explicit_resume_retries_the_same_prompt_after_recovery() {
    let (state, session, receiver, _) = root_fixture([]);
    let clock = state.engram_budget_clock();
    clock.pin_wall();
    let producer = Arc::new(LateEvaluateProducer {
        producer: StatefulEngramControlTransport::new(),
        clock: clock.clone(),
        late_once: AtomicBool::new(true),
        late_bind: true,
    });
    state.install_control_test_transport(producer.clone());
    state.inner.lock().unwrap().test_engram_dispatch_budget = None;
    let dispatch = root_dispatch(&state, &session, false);
    let original = with_record(&state, &session, |record| {
        record
            .queued_prompts
            .front()
            .unwrap()
            .pending_prompt
            .clone()
    });
    deliver_turn_dispatch(&state, dispatch).unwrap();
    with_record(&state, &session, |record| {
        assert!(record.session.preview.contains("admission latency hold"));
        assert!(record.engram.admission_retry.is_none());
        assert_eq!(
            record.queued_prompts.front().unwrap().pending_prompt.id,
            original.id
        );
    });
    assert!(receiver.try_recv().is_err());
    let calls = producer.producer.requests().len();
    state.engram_abort_retry_tick(clock.wall_now() + chrono::Duration::seconds(10));
    assert_eq!(producer.producer.requests().len(), calls);
    // The producer is responsive now. Only explicit operator Resume is
    // allowed to start another attempt, preserving the original payload.
    state.resume_session_queue(&session).unwrap();
    assert!(
        matches!(receiver.try_recv(), Ok(CodexRuntimeCommand::Prompt { command, .. }) if command.prompt.contains(&original.text))
    );
    assert!(
        receiver.try_recv().is_err(),
        "one provider handoff after explicit Resume"
    );
    with_record(&state, &session, |record| {
        assert!(record.engram.admission_retry.is_none());
        assert!(!record.orchestrator_auto_dispatch_blocked);
    });
}

#[test]
fn admission_latency_hold_window_spent_precedes_the_per_attempt_hold() {
    for at_deadline in [false, true] {
        let (state, session, receiver, _) = root_fixture([]);
        let clock = state.engram_budget_clock();
        clock.pin_wall();
        let producer = StatefulEngramControlTransport::new();
        state.install_control_test_transport(producer.clone());
        state.inner.lock().unwrap().test_engram_dispatch_budget = None;
        let dispatch = root_dispatch(&state, &session, false);
        let before = with_record(&state, &session, |record| {
            record.engram.begin_preparation_lineage.clone().unwrap()
        });
        let deadline = chrono::DateTime::parse_from_rfc3339(&before.deadline)
            .unwrap()
            .with_timezone(&chrono::Utc);
        let remaining = (deadline - clock.wall_now()).to_std().unwrap();
        clock.advance(if at_deadline {
            remaining
        } else {
            remaining - Duration::from_millis(1)
        });
        deliver_turn_dispatch(&state, dispatch).unwrap();
        let parked = with_record(&state, &session, Clone::clone);
        let after = parked.engram.begin_preparation_lineage.as_ref().unwrap();
        assert_eq!(after.prompt_id, before.prompt_id);
        assert_eq!(after.admitted_at, before.admitted_at);
        assert_eq!(
            after.deadline, before.deadline,
            "neither branch grants another window"
        );
        assert!(parked.engram.admission_retry.is_none());
        assert!(parked.engram.bind_retry.is_none());
        assert!(receiver.try_recv().is_err());
        assert!(!producer.requests().iter().any(|request| matches!(
            request.request["operation"].as_str(),
            Some("turn_begin" | "turn_checkpoint")
        )));
        let cause = last_dispatch_card(&parked).causal_failure.as_ref().unwrap();
        assert!(
            !cause
                .continuation_reason
                .contains("Automatic retry scheduled")
        );
        if at_deadline {
            assert!(
                after.ended,
                "overall-window exhaustion wins at its exact deadline"
            );
            assert!(
                parked
                    .session
                    .preview
                    .contains("automatic re-admission exhausted")
            );
            assert!(
                cause
                    .continuation_reason
                    .contains("Automatic recovery ended")
            );
            assert!(
                !cause
                    .continuation_reason
                    .contains("committed no changes"),
                "a budget expiration is not a non-application certificate"
            );
        } else {
            assert!(!after.ended);
            assert!(parked.session.preview.contains("admission latency hold"));
            assert!(
                cause
                    .continuation_reason
                    .contains("No automatic re-evaluation")
            );
        }
    }
}
