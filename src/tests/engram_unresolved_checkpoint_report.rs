//! A closing checkpoint's report under an answer that does not resolve it.
//! Only a definitive refusal drops the cached report; any other Remote answer
//! keeps it in memory, the persisted operation keeps the exact request, key
//! and report, and a fresh host replays that request unchanged.
//! Mounted under `engram_checkpoint_result_recovery.rs` for its fixtures.

use super::*;

/// The cached report for the turn's grant, which must still be held.
fn cached_report(turn: &CheckedTurn) -> EngramTurnReport {
    turn.record(|record| {
        let (grant_id, report) = record
            .engram
            .active_turn_report
            .clone()
            .expect("the grant's report is cached");
        assert_eq!(grant_id, CHECK_GRANT);
        report
    })
}

/// The causal failures on the turn's degraded checkpoint cards.
fn degraded_checkpoint_causes(turn: &CheckedTurn) -> Vec<EngramCausalFailure> {
    turn.record(|record| {
        record
            .session
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::EngramControl { card, .. }
                    if card.stage == EngramControlStage::Checkpoint
                        && card.decision == EngramControlCardDecision::Degraded =>
                {
                    card.causal_failure.clone()
                }
                _ => None,
            })
            .collect()
    })
}

#[test]
fn an_unresolved_remote_answer_keeps_the_report_and_a_fresh_host_replays_it_exactly() {
    for code in ["storage_error", "a_code_this_build_does_not_know"] {
        let turn = CheckedTurn::start_with(
            &format!("unresolved-{}", code.replace('_', "-")),
            true,
            None,
            vec![remote_error_reply(code)],
        );
        turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
        let sent = turn.finish();
        assert!(sent.get("verification_evidence").is_some(), "{code}: {sent:#}");

        // In memory: the report keeps its evidence, and its fallback stays.
        let report = cached_report(&turn);
        assert!(!report.verification_evidence.is_empty(), "{code}: the report is kept");
        assert!(
            turn.record(|record| record.engram.active_turn_report_fallback.is_some()),
            "{code}: an unresolved answer does not spend the fallback"
        );

        // On the card: the cause names the code, and its application is unknown.
        let causes = degraded_checkpoint_causes(&turn);
        assert!(
            causes.iter().any(|cause| cause.original_code.as_deref() == Some(code)
                && cause.remote_application == EngramRemoteApplication::Unknown
                && !cause.transport_unsent),
            "{code}: {causes:#?}"
        );

        // Durably: the operation keeps the exact request with its unknown result.
        let original = turn.record(|record| record.engram.checkpoint_operation.clone().unwrap());
        assert_eq!(serde_json::to_value(&original.original.request).unwrap(), sent, "{code}");
        assert!(matches!(
            original.attempts[0].result,
            Some(EngramCheckpointAttemptResult::Unknown { .. })
        ), "{code}");
        assert!(original.has_uncertain_checkpoint_history(), "{code}");

        // A fresh host started from that file replays the identical request.
        let transport = ScriptedEngramControlTransport::new(vec![checkpoint_reply(CHECK_GRANT)]);
        transport.enable_named_roots(
            &turn.session_id,
            original.original.work_binding.as_ref().expect("original claimed binding"),
        );
        transport.work_bindings.lock().unwrap().extend(
            (0..8).map(|_| Ok(original.original.work_binding.clone())));
        let fresh = super::effects::fresh_host_with_transport(
            &turn.state,
            turn.root.join(format!("unresolved-{}.sqlite", code.replace('_', "-"))),
            turn.root.clone(),
            transport.clone(),
        );
        assert!(
            !transport.requests().iter().any(|request| request.request["operation"] == "turn_checkpoint"),
            "{code}: boot does not replay"
        );
        assert!(fresh.recover_original_checkpoint_result(&original.original.id).unwrap(), "{code}");
        let replayed = transport
            .requests()
            .into_iter()
            .filter(|request| request.request["operation"] == "turn_checkpoint")
            .map(|request| request.request)
            .collect::<Vec<_>>();
        assert_eq!(replayed, [sent], "{code}: the same request, key and report");
        assert_eq!(checkpoints(&turn).len(), 1, "{code}: the old host never replayed");
        fresh.shutdown_persist_blocking();
    }
}

#[test]
fn a_definitive_refusal_still_drops_the_report_to_its_fallback() {
    for (label, reply) in [
        ("listed-refusal", remote_error_reply("control_projection_invalid")),
        ("explicit-refusal", checkpoint_refusal_reply("evidence_rejected")),
    ] {
        let turn = CheckedTurn::start_with(label, true, None, vec![reply]);
        turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
        let sent = turn.finish();
        assert!(sent.get("verification_evidence").is_some(), "{label}: {sent:#}");
        let report = cached_report(&turn);
        assert!(
            report.verification_evidence.is_empty(),
            "{label}: the next closing attempt goes with the turn's own observation"
        );
        assert!(turn.record(|record| record.engram.active_turn_report_fallback.is_none()), "{label}");
    }
}

#[test]
fn a_proven_unsent_loss_keeps_the_report_and_says_so_on_the_card() {
    let turn = CheckedTurn::start_with(
        "proven-unsent",
        true,
        None,
        vec![ScriptedEngramControlResponse::Reply(Err(
            EngramTransportError::transport("the control process took no request").request_never_sent(),
        ))],
    );
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
    let sent = turn.finish();
    assert!(!cached_report(&turn).verification_evidence.is_empty(), "{sent:#}");
    let causes = degraded_checkpoint_causes(&turn);
    assert!(
        causes.iter().any(|cause| cause.transport_unsent
            && cause.remote_application == EngramRemoteApplication::NotStarted),
        "the card says the request never reached Engram: {causes:#?}"
    );
}

/// A terminal control-startup reply answered the frame before dispatching it,
/// so it never refuses the report it carried, even when its cause names a
/// producer refusal: whether the report was applied stays unresolved, and
/// neither the closing checkpoint nor a recovery checkpoint drops it.
#[test]
fn a_terminal_control_startup_reply_never_refuses_the_report() {
    for code in ["control_startup_busy", "control_startup_refused"] {
        let plain = EngramTransportError::remote(EngramControlErrorBody {
            code: code.to_owned(),
            message: "the control process answered the first frame and exited".to_owned(),
        });
        let mut blamed = plain.clone();
        let mut cause = EngramCausalFailure::error(&plain, "turn_checkpoint", None, &[]);
        cause.failure_class = EngramCausalFailureClass::ProducerRefusal;
        blamed.causal_failure = Some(cause);
        for (error, label) in [(&plain, "plain"), (&blamed, "producer-refusal cause")] {
            assert!(!engram_checkpoint_refuses_report(error), "{code} ({label}): not a refusal");
            assert!(
                engram_checkpoint_leaves_report_unresolved(error),
                "{code} ({label}): the report stays unresolved and kept"
            );
            assert!(
                engram_recovery_checkpoint_refusal(&Err(error.clone())).is_none(),
                "{code} ({label}): a recovery checkpoint is not refused either"
            );
        }
    }
    // The same producer-refusal cause on an ordinary remote code still refuses.
    let mut refused = EngramTransportError::remote(EngramControlErrorBody {
        code: "a_code_this_build_does_not_know".to_owned(),
        message: "refused".to_owned(),
    });
    let mut cause = EngramCausalFailure::error(&refused, "turn_checkpoint", None, &[]);
    cause.failure_class = EngramCausalFailureClass::ProducerRefusal;
    refused.causal_failure = Some(cause);
    assert!(engram_checkpoint_refuses_report(&refused), "the guard is specific to the startup codes");
}
