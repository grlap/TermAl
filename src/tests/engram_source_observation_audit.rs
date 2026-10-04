//! Historical root transitions settle recording, never source accounting.
use super::*;

fn held_audit(label: &str) -> ClaimedRoot {
    let claimed = opening_fixture(label);
    let state = claimed.state.clone();
    let session_id = claimed.session_id.clone();
    let work = format!("w-{label}");
    TEST_ENGRAM_DURING_TURN_START_CAPTURE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            state.name_engram_source_root(&session_id, EngramSourceRootRequest { work, path: None }).unwrap();
        }));
    });
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap().lose_next_observation_reply = true;
    let outcome = deliver_turn_dispatch(&claimed.state, claimed.dispatch());
    assert!(matches!(outcome, TurnDispatchDeliveryOutcome::Held { .. }), "{outcome:?}");
    assert!(claimed.runtime_rx.try_recv().is_err());
    assert!(matches!(original_intent(&claimed).phase, EngramSourceObservationPhase::Prepared { .. }));
    claimed
}

#[test]
fn source_observation_audit_rejects_wrong_receipts_without_releasing_the_provider() {
    for mode in ["explicit_audit", "no_source_change", "unknown", "scope", "historical_binding", "finished_run"] {
        let claimed = held_audit(&format!("source-observation-audit-{mode}"));
        let before = original_intent(&claimed);
        {
            let mut roots = claimed.transport.named_roots.lock().unwrap();
            let receipt = &mut roots.as_mut().unwrap().observations.values_mut().next().unwrap().1;
            assert_eq!(receipt["accounting"], json!({ "kind": "audit_only", "reason": "root_basis_moved" }));
            match mode {
                "scope" => receipt["binding"]["claim_fence"] = json!(99),
                "no_source_change" => receipt["accounting"] = json!({ "kind": "no_source_change" }),
                _ => receipt["accounting"]["reason"] = json!(mode),
            }
        }
        let target = {
            let inner = claimed.state.inner.lock().unwrap();
            AppState::engram_binding_target_for_session_shape_locked(&inner, &claimed.session_id, true).unwrap().unwrap()
        };
        assert!(claimed.state.advance_engram_source_observation(&claimed.session_id, &target,
            target.dispatch_deadline(target.budget_clock.now())).is_err(), "accepted receipt mode {mode}");
        claimed.state.release_source_observation_continuation(&claimed.session_id);
        assert!(claimed.runtime_rx.try_recv().is_err());
        let after = original_intent(&claimed);
        assert_eq!(after.root_basis, before.root_basis);
        claimed.record(|record| assert!(!engram_source_observation_can_deliver(record)));
        assert!(claimed.state.inner.lock().unwrap().engram_source_sightings.iter().all(|owner| owner.latest.is_none()));
        let wires = claimed.transport.requests();
        let observe: Vec<_> = wires.iter().filter(|request| request.request["operation"] == "execution_observe").collect();
        assert_eq!(observe.len(), 2);
        assert_eq!(observe[0].request, observe[1].request, "retry cannot substitute current proof or policy");
    }
}

#[test]
fn source_observation_audit_late_naming_uncertainty_preserves_admission_but_withholds_evidence() {
    for pending in [false, true] {
        let label = if pending { "source-observation-audit-late-pending" } else { "source-observation-audit-late-unknown" };
        let claimed = held_audit(label);
        let original = original_intent(&claimed);
        let target = {
            let inner = claimed.state.inner.lock().unwrap();
            AppState::engram_binding_target_for_session_shape_locked(&inner, &claimed.session_id, true).unwrap().unwrap()
        };
        claimed.state.advance_engram_source_observation(&claimed.session_id, &target,
            target.dispatch_deadline(target.budget_clock.now())).unwrap();
        assert!(matches!(original_intent(&claimed).phase, EngramSourceObservationPhase::Recorded { .. }));
        let opening = claimed.record(|record| record.engram.active_turn_root_capture.clone());
        if pending {
            claimed.transport.named_roots.lock().unwrap().as_mut().unwrap().lose_next_reply = true;
            claimed.state.name_engram_source_root(&claimed.session_id, EngramSourceRootRequest {
                work: format!("w-{label}"), path: Some(Some(claimed.root.to_string_lossy().into_owned())),
            }).expect_err("a lost normal naming reply keeps current authority pending");
        } else {
            claimed.state.reconcile_engram_named_root(&claimed.session_id, &original.routing_token,
                Some(&original.binding), Some(EngramNamedRootState::Unknown), u64::MAX).unwrap();
        }
        assert_eq!(claimed.state.engram_turn_root_capture(&claimed.session_id), EngramRootCapture::Unconfirmed);
        claimed.state.release_source_observation_continuation(&claimed.session_id);
        received_prompt(&claimed);
        claimed.record(|record| assert_eq!(record.engram.active_turn_root_capture, opening,
            "late uncertainty cannot relabel an admitted historical opening"));
        claimed.state.note_engram_command_started(&claimed.session_id, &EngramObservationProvenance::Ambient,
            "late-uncertain-check", Some("cargo test"),
            Some(&original.sighting.basis.workspace_id));
        claimed.state.note_engram_command_finished(&claimed.session_id, &EngramObservationProvenance::Ambient,
            "late-uncertain-check", "cargo test",
            "test result: ok. 1 passed", Some(EngramCommandExit::Code(0)));
        let captures = claimed.record(|record| record.engram.active_turn_checks.iter()
            .flat_map(|check| std::iter::once(check.start_basis.clone())
                .chain(check.end.as_ref().map(|end| end.end_basis.clone()))).collect::<Vec<_>>());
        for capture in captures { capture.wait_until(std::time::Instant::now() + DEADLOCK_GUARD); }
        finish_claimed_turn(&claimed, &claimed.runtime_token());
        let requests = claimed.transport.requests();
        let close = requests.iter().find(|request| request.request["operation"] == "turn_checkpoint").unwrap();
        assert!(close.request.get("verification_evidence").is_none_or(|value| value.as_array().is_some_and(Vec::is_empty)),
            "late uncertainty must withhold the affected check: {}", close.request);
        assert!(claimed.state.inner.lock().unwrap().engram_source_sightings.iter().all(|owner| owner.latest.is_none()));
        assert_eq!(requests.iter().filter(|request| request.request["operation"] == "turn_begin").count(), 1);
    }
}

#[test]
fn source_observation_audit_retired_by_cancellation_records_without_replay_or_promotion() {
    let claimed = held_audit("source-observation-audit-cancel");
    let before = original_intent(&claimed);
    // A held prompt is Idle; Stop deliberately refuses it. Cancel its actual
    // queued head instead, using the lifecycle that owns a held admission.
    claimed.state.cancel_queued_prompt(&claimed.session_id, &before.prompt_id).unwrap();
    let stopped = claimed.record(|record| (record.engram.dispatch_generation,
        record.active_turn_generation, record.session.status));
    claimed.state.reconcile_retired_source_observations(&claimed.session_id).unwrap();
    let (request, receipt) = finalized_observation_receipt(&claimed, &before);
    assert_eq!(request["root_basis"], before.root_basis);
    assert_eq!(receipt["accounting"], json!({ "kind": "audit_only", "reason": "root_basis_moved" }));
    assert!(claimed.state.inner.lock().unwrap().engram_source_sightings.iter().all(|owner| owner.latest.is_none()));
    let saved = load_state(claimed.state.persistence_path.as_path()).unwrap().unwrap();
    assert!(saved.engram_source_sightings.iter().all(|owner| owner.latest.is_none()));
    claimed.state.release_source_observation_continuation(&claimed.session_id);
    assert!(claimed.runtime_rx.try_recv().is_err());
    claimed.record(|record| {
        assert_eq!((record.engram.dispatch_generation, record.active_turn_generation, record.session.status), stopped);
        assert!(record.engram.active_grant_id.is_none() && record.engram.uncertain_grant_id.is_none(),
            "explicit recovery settles only the original cancelled grant");
    });
    let wires = claimed.transport.requests();
    assert_eq!(wires.iter().filter(|request| request.request["operation"] == "turn_begin").count(), 1);
}
