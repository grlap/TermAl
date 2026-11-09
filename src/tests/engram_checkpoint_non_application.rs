//! Invocation-local no-write classification and ordinary factual reporting.
//! No checkpoint replay, grant reconciliation or checkpoint-ACK dispatch exists.
use super::*;

#[test]
fn factual_non_application_is_only_the_current_remote_invocation_contract() {
    for code in [
        "control_request_expired",
        "control_store_busy",
        "storage_error",
        "redaction_refused",
        "invalid_routing_token",
    ] {
        let remote = EngramTransportError::remote(EngramControlErrorBody {
            code: code.into(),
            message: "scripted producer reply".into(),
        });
        assert_eq!(
            remote.invocation_application(),
            if matches!(code, "control_request_expired" | "control_store_busy") {
                EngramInvocationApplication::CertifiedNotApplied
            } else {
                EngramInvocationApplication::Unknown
            },
            "{code}"
        );
        let mut local = EngramTransportError::transport("local failure using the same text");
        local.code = Some(code.into());
        assert_eq!(
            local.invocation_application(),
            EngramInvocationApplication::Unknown,
            "a local error cannot impersonate the producer's no-write guarantee: {code}"
        );
    }
}

#[test]
fn ordinary_observation_reports_actual_checks_without_native_verification_or_checkpoint() {
    let turn = EOCheckedTurn::start("ordinary-factual-nonapplication", true);
    turn.run(
        "ordinary-observed-check",
        SIZE_TEST,
        EngramCommandExit::Code(0),
        || {},
    );
    let report = turn.finish();
    let checks = report["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["check_kind"], "test");
    assert_eq!(checks[0]["observed_result"], "passed");
    assert!(checks[0]["finished_at"].is_string());
    assert_ordinary_observation_has_no_native_credit(&turn, &report);
    let inner = turn.state.inner.lock().unwrap();
    assert!(
        inner
            .engram_session_obligations
            .checkpoint_operations
            .is_empty()
    );
    assert!(inner.engram_session_obligations.entries.is_empty());
}

#[test]
fn ordinary_healthy_turns_do_not_accumulate_checkpoint_history_through_reload() {
    const TURNS: usize = 24;
    let turn = EOCheckedTurn::start("ordinary-bounded-historical-checkpoints", true);
    for ordinal in 0..TURNS {
        let report = turn.finish();
        assert_ordinary_observation_has_no_native_credit(&turn, &report);
        {
            let inner = turn.state.inner.lock().unwrap();
            assert!(
                inner
                    .engram_session_obligations
                    .checkpoint_operations
                    .is_empty()
            );
            assert!(inner.engram_session_obligations.entries.is_empty());
        }
        if ordinal + 1 < TURNS {
            turn.begin("Next ordinary input without admission.");
        }
    }
    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(
        loaded
            .engram_session_obligations
            .checkpoint_operations
            .is_empty()
    );
    assert!(loaded.engram_session_obligations.entries.is_empty());
}
