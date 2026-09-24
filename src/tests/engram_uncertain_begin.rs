//! The uncertain turn-begin window (tm-8b7y): a begin whose outcome the host
//! never learns is recorded as the child's uncertain grant. A delegation child
//! canceled while its `turn_begin` is blocked records the exact grant in
//! flight, the released begin's compensating checkpoint settles or keeps it,
//! a begin that fails in transport records it as well, and restart recovery
//! then confirms or clears it against Engram.
//!
//! Owns the cancel-during-begin, queued-head-removal and
//! begin-transport-failure tests, the settlement of a recorded grant by
//! recovery, a project reset and a late begin outcome, and the choice of
//! grant when queued intent is dropped. Does not own the terminal-callback
//! variants of the same race
//! (`runtime_terminal_callbacks_abandon_blocked_engram_begin_before_queue_drain`
//! in `src/tests/engram_host_adapter.rs`) or boot-recovery target selection
//! (`engram_boot_recovery_targets.rs`). New module beside the adapter tests,
//! created instead of growing them.

use super::boot_recovery_targets::RecordingBootRecoveryTransport;
use super::*;

/// Whether the blocked begin is the first for its prompt or follows a
/// re-evaluation, whose grant differs from the originally evaluated one.
#[derive(Clone, Copy)]
enum BlockedBegin {
    Direct,
    AfterReevaluation,
}

fn refuse_reply(code: &str) -> ScriptedEngramControlResponse {
    ScriptedEngramControlResponse::Reply(Ok(json!({ "decision": "refuse", "code": code })))
}

/// Cancels a delegation while the child's `turn_begin` is blocked, then
/// releases the begin and lets its compensating checkpoint answer with
/// `settlement`. Returns nothing; every expectation is asserted inside.
fn assert_cancel_during_blocked_begin_records_the_grant(
    label: &str,
    blocked_begin: BlockedBegin,
    settlement: ScriptedEngramControlResponse,
    settles: bool,
) {
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime(&format!("engram-uncertain-begin-{label}"));
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join(format!("engram-uncertain-begin-{label}-project"));
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram uncertain begin");
    let parent_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    enable_test_project_engram(&state, &project_id, &root);

    let evaluated_grant_id = format!("uncertain-begin-{label}-evaluated");
    let begun_grant_id = match blocked_begin {
        BlockedBegin::Direct => evaluated_grant_id.clone(),
        BlockedBegin::AfterReevaluation => format!("uncertain-begin-{label}-reevaluated"),
    };
    let (begin_step, begin_gate) = gated_engram_step("turn_begin", begin_reply(&begun_grant_id));
    let mut steps = vec![
        immediate_engram_step(
            "session_bind",
            bind_reply(&format!("uncertain-parent-{label}")),
        ),
        immediate_engram_step(
            "session_bind",
            bind_reply(&format!("uncertain-child-{label}")),
        ),
        immediate_engram_step("turn_evaluate", grant_reply(&evaluated_grant_id)),
    ];
    if matches!(blocked_begin, BlockedBegin::AfterReevaluation) {
        // Engram expired the first grant; the retry evaluates a fresh one and
        // that is the grant whose begin the cancellation races.
        steps.push(immediate_engram_step(
            "turn_begin",
            refuse_reply("grant_expired"),
        ));
        steps.push(immediate_engram_step(
            "turn_evaluate",
            grant_reply(&begun_grant_id),
        ));
    }
    steps.push(begin_step);
    steps.push(immediate_engram_step("turn_checkpoint", settlement));
    let transport = GatedEngramControlTransport::new(steps);
    state.install_control_test_transport(transport.clone());

    let creating_state = state.clone();
    let creating_parent = parent_session_id.clone();
    let creating_prompt = format!("Keep the {label} begin blocked until cancellation.");
    let creating_title = format!("Engram uncertain begin {label}");
    let create_handle = std::thread::spawn(move || {
        creating_state.create_read_only_delegation(
            &creating_parent,
            CreateDelegationRequest {
                prompt: creating_prompt,
                title: Some(creating_title),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
    });
    let begin_request = begin_gate.wait();
    let child_id = begin_request.connection.session_id;
    assert_eq!(begin_request.request["grant_id"], begun_grant_id);
    let delegation_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let child = &inner.sessions[inner
            .find_session_index(&child_id)
            .expect("child should exist while begin is blocked")];
        let pending = child
            .engram
            .pending_dispatch
            .as_ref()
            .expect("the evaluated grant should remain pending during begin");
        assert_eq!(
            pending.begin_requested.as_deref(),
            Some(begun_grant_id.as_str()),
            "the pending dispatch must record exactly the grant whose begin reached the transport"
        );
        assert_eq!(child.engram.active_grant_id, None);
        assert_eq!(
            child.engram.uncertain_grant_id, None,
            "nothing is recorded before the begin outcome or an abandon"
        );
        child
            .session
            .parent_delegation_id
            .clone()
            .expect("blocked child should retain its delegation id")
    };

    state
        .cancel_delegation(&parent_session_id, &delegation_id)
        .expect("cancellation should complete while the begin is blocked");
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let delegation = &inner.delegations[inner
            .find_delegation_index(&delegation_id)
            .expect("delegation row should exist")];
        assert_eq!(delegation.status, DelegationStatus::Canceled);
        let child = &inner.sessions[inner
            .find_session_index(&child_id)
            .expect("canceled child should remain")];
        assert!(
            child.engram.pending_dispatch.is_none(),
            "cancellation must abandon the in-flight dispatch"
        );
        assert!(
            child.queued_prompts.is_empty(),
            "cancellation clears the queue, so the record itself must carry the evidence"
        );
        assert_eq!(
            child.engram.uncertain_grant_id.as_deref(),
            Some(begun_grant_id.as_str()),
            "the abandon must record the grant whose begin was in flight as uncertain"
        );
        assert_eq!(
            child.engram.active_grant_id, None,
            "an uncertain begin is not a mirrored begun grant"
        );
        assert!(child.engram.rebind_required);
    }

    begin_gate.release();
    let _ = create_handle
        .join()
        .expect("the canceled delegation thread should not panic");
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let child = &inner.sessions[inner
            .find_session_index(&child_id)
            .expect("canceled child should remain after the begin completes")];
        if settles {
            assert_eq!(
                child.engram.uncertain_grant_id, None,
                "a settled compensating checkpoint clears the uncertain grant"
            );
        } else {
            assert_eq!(
                child.engram.uncertain_grant_id.as_deref(),
                Some(begun_grant_id.as_str()),
                "an unsettled compensating checkpoint keeps the uncertain grant for recovery"
            );
        }
        assert_eq!(child.engram.active_grant_id, None);
        assert!(child.engram.rebind_required);
        assert!(child.engram.pending_dispatch.is_none());
    }
    let child_operations = transport
        .requests()
        .into_iter()
        .filter(|request| request.connection.session_id == child_id)
        .map(|request| {
            request.request["operation"]
                .as_str()
                .expect("operation should serialize")
                .to_owned()
        })
        .collect::<Vec<_>>();
    let expected: &[&str] = match blocked_begin {
        BlockedBegin::Direct => &[
            "session_bind",
            "turn_evaluate",
            "turn_begin",
            "turn_checkpoint",
        ],
        BlockedBegin::AfterReevaluation => &[
            "session_bind",
            "turn_evaluate",
            "turn_begin",
            "turn_evaluate",
            "turn_begin",
            "turn_checkpoint",
        ],
    };
    assert_eq!(
        child_operations, expected,
        "cancellation itself sends nothing; the released begin closes its grant"
    );
}

#[test]
fn cancel_during_blocked_begin_records_the_grant_until_a_checkpoint_receipt_settles_it() {
    assert_cancel_during_blocked_begin_records_the_grant(
        "receipt",
        BlockedBegin::Direct,
        checkpoint_reply("uncertain-begin-receipt-evaluated"),
        true,
    );
}

#[test]
fn cancel_during_blocked_begin_records_the_grant_until_engram_reports_it_never_begun() {
    assert_cancel_during_blocked_begin_records_the_grant(
        "not-begun",
        BlockedBegin::Direct,
        refuse_reply("grant_not_begun"),
        true,
    );
}

#[test]
fn cancel_during_blocked_begin_keeps_the_grant_when_the_compensating_checkpoint_fails() {
    assert_cancel_during_blocked_begin_records_the_grant(
        "transport-failure",
        BlockedBegin::Direct,
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::transport(
            "fixture compensating checkpoint failure",
        ))),
        false,
    );
}

#[test]
fn cancel_during_blocked_begin_keeps_the_grant_when_the_receipt_names_another_grant() {
    assert_cancel_during_blocked_begin_records_the_grant(
        "other-receipt",
        BlockedBegin::Direct,
        checkpoint_reply("some-other-grant"),
        false,
    );
}

#[test]
fn cancel_during_a_blocked_reevaluated_begin_records_the_reevaluated_grant() {
    assert_cancel_during_blocked_begin_records_the_grant(
        "reevaluated",
        BlockedBegin::AfterReevaluation,
        checkpoint_reply("uncertain-begin-reevaluated-reevaluated"),
        true,
    );
}

#[test]
fn a_begin_that_fails_in_transport_records_the_grant_until_recovery_confirms_it_clean() {
    // The request may have reached Engram before the transport failed, so
    // the grant is uncertain even though the owner was never superseded and
    // no abandon ran. Cancellation later clears the queue; the record alone
    // keeps the child a boot target, and a clean status settles it.
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-uncertain-begin-transport");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-uncertain-begin-transport-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram uncertain begin transport");
    let parent_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    enable_test_project_engram(&state, &project_id, &root);

    let grant_id = "uncertain-begin-transport-grant";
    let transport = GatedEngramControlTransport::new([
        immediate_engram_step("session_bind", bind_reply("uncertain-parent-transport")),
        immediate_engram_step("session_bind", bind_reply("uncertain-child-transport")),
        immediate_engram_step("turn_evaluate", grant_reply(grant_id)),
        immediate_engram_step(
            "turn_begin",
            ScriptedEngramControlResponse::Reply(Err(EngramTransportError::transport(
                "fixture begin transport failure",
            ))),
        ),
    ]);
    state.install_control_test_transport(transport.clone());
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Lose the begin reply.".to_owned(),
                title: Some("Engram uncertain begin transport".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("the delegation is created even though its first dispatch degrades");
    let child_id = created.delegation.child_session_id.clone();
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let child = &inner.sessions[inner
            .find_session_index(&child_id)
            .expect("child should exist")];
        assert!(child.engram.pending_dispatch.is_none());
        assert_eq!(child.engram.active_grant_id, None);
        assert_eq!(
            child.engram.uncertain_grant_id.as_deref(),
            Some(grant_id),
            "a begin that failed in transport is recorded as uncertain on the record"
        );
    }

    state
        .cancel_delegation(&parent_session_id, &created.delegation.id)
        .expect("cancellation should complete");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child = &inner.sessions[inner
            .find_session_index(&child_id)
            .expect("canceled child should remain")];
        assert!(child.queued_prompts.is_empty());
        assert_eq!(child.engram.uncertain_grant_id.as_deref(), Some(grant_id));
        // The failed begin armed the in-memory bind backoff, which a real
        // restart does not carry; recovery must not be refused by it here.
        let index = inner
            .find_session_index(&child_id)
            .expect("canceled child should remain");
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        record.engram.next_bind_retry_at = None;
        record.engram.circuit_open = false;
        record.engram.consecutive_transport_failures = 0;
    }
    // Targets capture the adapter when the plan is prepared, so the recovery
    // transport must be in place before preparing it.
    let recovery = RecordingBootRecoveryTransport::new();
    state.install_control_test_transport(recovery.clone());
    let plan = state
        .prepare_engram_sessions_for_boot_recovery()
        .expect("boot recovery plan should be prepared");
    assert!(
        plan.targets
            .iter()
            .any(|target| target.connection.session_id == child_id),
        "a canceled child with an uncertain grant stays a boot target"
    );
    state.recover_prepared_engram_sessions_after_boot(plan);
    assert_eq!(
        recovery.operations_for(&child_id),
        ["session_status", "session_bind"],
        "recovery asks Engram, which reports no open grant, then rebinds"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = &inner.sessions[inner
        .find_session_index(&child_id)
        .expect("canceled child should remain after recovery")];
    assert_eq!(
        child.engram.uncertain_grant_id, None,
        "a clean status settles the uncertain grant as never begun"
    );
    assert_eq!(
        child.engram.routing_token.as_deref(),
        Some(format!("recovered-{child_id}").as_str())
    );
}

#[test]
fn canceling_the_queued_head_during_a_blocked_begin_records_the_grant_for_recovery() {
    // Removing the queued head cancels the admission without the abandon
    // path, and the released begin then belongs to a superseded owner: no
    // compensating checkpoint runs. The detached marker's begin must still
    // become the child's uncertain grant, so a later boot settles it.
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-uncertain-begin-queue-cancel");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-uncertain-begin-queue-cancel-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram uncertain begin queue cancel");
    let parent_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    enable_test_project_engram(&state, &project_id, &root);

    let grant_id = "uncertain-begin-queue-cancel-grant";
    let (begin_step, begin_gate) = gated_engram_step(
        "turn_begin",
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::transport(
            "fixture begin reply lost",
        ))),
    );
    let transport = GatedEngramControlTransport::new([
        immediate_engram_step("session_bind", bind_reply("uncertain-parent-queue-cancel")),
        immediate_engram_step("session_bind", bind_reply("uncertain-child-queue-cancel")),
        immediate_engram_step("turn_evaluate", grant_reply(grant_id)),
        begin_step,
    ]);
    state.install_control_test_transport(transport.clone());

    let creating_state = state.clone();
    let creating_parent = parent_session_id.clone();
    let create_handle = std::thread::spawn(move || {
        creating_state.create_read_only_delegation(
            &creating_parent,
            CreateDelegationRequest {
                prompt: "Keep the begin blocked until the queued head is removed.".to_owned(),
                title: Some("Engram uncertain begin queue cancel".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
    });
    let begin_request = begin_gate.wait();
    let child_id = begin_request.connection.session_id;
    let prompt_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let child = &inner.sessions[inner
            .find_session_index(&child_id)
            .expect("child should exist while begin is blocked")];
        assert_eq!(
            child
                .engram
                .pending_dispatch
                .as_ref()
                .and_then(|pending| pending.begin_requested.as_deref()),
            Some(grant_id)
        );
        child
            .queued_prompts
            .front()
            .expect("the blocked begin belongs to the queued head")
            .pending_prompt
            .id
            .clone()
    };

    state
        .cancel_queued_prompt(&child_id, &prompt_id)
        .expect("the queued head should be removable while its begin is blocked");
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let child = &inner.sessions[inner
            .find_session_index(&child_id)
            .expect("child should remain")];
        assert!(child.engram.pending_dispatch.is_none());
        assert!(child.queued_prompts.is_empty());
        assert_eq!(
            child.engram.uncertain_grant_id.as_deref(),
            Some(grant_id),
            "removing the head must keep the in-flight begin as the uncertain grant"
        );
        assert_eq!(child.engram.active_grant_id, None);
        assert!(child.engram.rebind_required);
    }

    begin_gate.release();
    let _ = create_handle
        .join()
        .expect("the delegation thread should not panic");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&child_id)
            .expect("child should remain after the lost begin reply");
        let child = &inner.sessions[index];
        assert_eq!(
            child.engram.uncertain_grant_id.as_deref(),
            Some(grant_id),
            "a lost reply for a superseded owner cannot settle the grant"
        );
        assert!(child.engram.pending_dispatch.is_none());
        let index = inner
            .delegations
            .iter()
            .position(|delegation| delegation.child_session_id == child_id)
            .expect("delegation row should exist");
        inner.delegations[index].status = DelegationStatus::Canceled;
        inner
            .mark_delegation_mutated(index)
            .expect("delegation index should be valid");
        let child_index = inner
            .find_session_index(&child_id)
            .expect("child should remain");
        let record = inner
            .session_mut_by_index(child_index)
            .expect("session index should be valid");
        record.engram.next_bind_retry_at = None;
        record.engram.circuit_open = false;
        record.engram.consecutive_transport_failures = 0;
    }
    let recovery = RecordingBootRecoveryTransport::new();
    state.install_control_test_transport(recovery.clone());
    let plan = state
        .prepare_engram_sessions_for_boot_recovery()
        .expect("boot recovery plan should be prepared");
    assert!(
        plan.targets
            .iter()
            .any(|target| target.connection.session_id == child_id),
        "the canceled child stays a boot target through its uncertain grant alone"
    );
    state.recover_prepared_engram_sessions_after_boot(plan);
    assert_eq!(
        recovery.operations_for(&child_id),
        ["session_status", "session_bind"]
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = &inner.sessions[inner
        .find_session_index(&child_id)
        .expect("child should remain after recovery")];
    assert_eq!(child.engram.uncertain_grant_id, None);
}

#[test]
fn a_begin_answered_for_another_grant_records_the_evaluated_grant_until_recovery_confirms_it_clean()
{
    // A receipt naming a different grant proves nothing about the grant this
    // begin asked for: Engram may hold it begun. It is recorded as uncertain
    // when the dispatch record is finished, and a clean status settles it.
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-uncertain-begin-mismatch");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-uncertain-begin-mismatch-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram uncertain begin mismatch");
    let parent_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    enable_test_project_engram(&state, &project_id, &root);

    let grant_id = "uncertain-begin-mismatch-grant";
    let transport = GatedEngramControlTransport::new([
        immediate_engram_step("session_bind", bind_reply("uncertain-parent-mismatch")),
        immediate_engram_step("session_bind", bind_reply("uncertain-child-mismatch")),
        immediate_engram_step("turn_evaluate", grant_reply(grant_id)),
        immediate_engram_step("turn_begin", begin_reply("some-other-grant")),
    ]);
    state.install_control_test_transport(transport.clone());
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Answer the begin for another grant.".to_owned(),
                title: Some("Engram uncertain begin mismatch".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("the delegation is created even though its first dispatch degrades");
    let child_id = created.delegation.child_session_id.clone();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&child_id)
            .expect("child should exist");
        let child = &inner.sessions[index];
        assert!(child.engram.pending_dispatch.is_none());
        assert_eq!(child.engram.active_grant_id, None);
        assert_eq!(
            child.engram.uncertain_grant_id.as_deref(),
            Some(grant_id),
            "a mismatched receipt leaves the evaluated grant uncertain"
        );
        // A restart starts without the in-memory bind backoff.
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        record.engram.next_bind_retry_at = None;
        record.engram.circuit_open = false;
        record.engram.consecutive_transport_failures = 0;
    }
    // The parked prompt keeps its exact queued recovery; cancellation drops
    // it, and the recorded grant alone must then keep the child a target.
    state
        .cancel_delegation(&parent_session_id, &created.delegation.id)
        .expect("cancellation should complete");
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let child = &inner.sessions[inner
            .find_session_index(&child_id)
            .expect("canceled child should remain")];
        assert!(child.queued_prompts.is_empty());
        assert_eq!(child.engram.uncertain_grant_id.as_deref(), Some(grant_id));
    }
    let recovery = RecordingBootRecoveryTransport::new();
    state.install_control_test_transport(recovery.clone());
    let plan = state
        .prepare_engram_sessions_for_boot_recovery()
        .expect("boot recovery plan should be prepared");
    state.recover_prepared_engram_sessions_after_boot(plan);
    assert_eq!(
        recovery.operations_for(&child_id),
        ["session_status", "session_bind"]
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = &inner.sessions[inner
        .find_session_index(&child_id)
        .expect("child should remain after recovery")];
    assert_eq!(child.engram.uncertain_grant_id, None);
}

#[test]
fn canceling_a_child_whose_restored_intent_may_own_a_lost_begin_records_an_unknown_grant() {
    // After a restart only the durable evaluate intent survives an
    // in-flight begin; the runtime marker is gone and the reply never came.
    // Boot leaves such intent to its exact recovery, so canceling the
    // delegation before that runs drops the only evidence. The record must
    // then carry an unknown-grant marker that a session status settles.
    let (state, runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-uncertain-begin-restored");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-uncertain-begin-restored-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram uncertain begin restored");
    let parent_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    enable_test_project_engram(&state, &project_id, &root);

    let transport = GatedEngramControlTransport::new([
        immediate_engram_step("session_bind", bind_reply("uncertain-parent-restored")),
        immediate_engram_step("session_bind", bind_reply("uncertain-child-restored")),
        immediate_engram_step("turn_evaluate", grant_reply("setup-grant")),
        immediate_engram_step("turn_begin", begin_reply("setup-grant")),
    ]);
    state.install_control_test_transport(transport.clone());
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Set the child up.".to_owned(),
                title: Some("Engram uncertain begin restored".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should start");
    assert!(matches!(
        receive_synchronous_engram_prompt(
            &state,
            &runtime_rx,
            "runtime should receive the setup prompt"
        )
        .expect("runtime should receive the setup prompt"),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let child_id = created.delegation.child_session_id.clone();
    queue_test_engram_prompt(
        &state,
        &child_id,
        "retained",
        QueuedPromptSource::User,
        None,
    );
    let target = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        AppState::engram_binding_target_for_session_shape_locked(&inner, &child_id, true)
            .expect("child binding target should resolve")
            .expect("the child's project must have Engram enabled")
    };
    {
        // The state a crash between turn_begin and its reply leaves behind,
        // as reloaded: durable evaluate intent with no begun grant noted, no
        // runtime marker, nothing mirrored, an idle child.
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&child_id)
            .expect("child should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        record.session.status = SessionStatus::Idle;
        record.engram.active_grant_id = None;
        record.engram.uncertain_grant_id = None;
        record.engram.pending_dispatch = None;
        // The loader marks a record whose queue carries intent.
        record.engram.recovered_admission = true;
        let queued = record
            .queued_prompts
            .front_mut()
            .expect("the retained prompt should be queued");
        queued.engram_evaluate = Some(EngramQueuedEvaluate {
            begun_grant_id: None,
            connection: target.connection,
            settings: target.settings,
            operation_generation: None,
            request: EngramControlRequest::TurnEvaluate {
                routing_token: "uncertain-child-restored".to_owned(),
                intent_fingerprint: "retained".to_owned(),
                purpose: "ordinary".to_owned(),
                requested_effects: target.effects,
                resource_intents: Vec::new(),
                idempotency_key: "retained".to_owned(),
            },
        });
        sync_pending_prompts(record);
        state
            .commit_locked(&mut inner)
            .expect("restored intent should persist");
    }

    state
        .cancel_delegation(&parent_session_id, &created.delegation.id)
        .expect("cancellation should complete");
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let child = &inner.sessions[inner
            .find_session_index(&child_id)
            .expect("canceled child should remain")];
        assert!(
            child.queued_prompts.is_empty(),
            "cancellation dropped the intent"
        );
        assert_eq!(
            child.engram.uncertain_grant_id.as_deref(),
            Some(ENGRAM_UNCERTAIN_GRANT_UNKNOWN),
            "dropping evaluate intent that may own a begin records the unknown marker"
        );
    }
    let recovery = RecordingBootRecoveryTransport::new();
    state.install_control_test_transport(recovery.clone());
    let plan = state
        .prepare_engram_sessions_for_boot_recovery()
        .expect("boot recovery plan should be prepared");
    assert!(
        plan.targets
            .iter()
            .any(|target| target.connection.session_id == child_id),
        "the unknown marker alone keeps the canceled child a boot target"
    );
    state.recover_prepared_engram_sessions_after_boot(plan);
    assert_eq!(
        recovery.operations_for(&child_id),
        ["session_status", "session_bind"],
        "recovery asks Engram, which reports no open grant, then rebinds"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = &inner.sessions[inner
        .find_session_index(&child_id)
        .expect("canceled child should remain after recovery")];
    assert_eq!(child.engram.uncertain_grant_id, None);
}

/// A delegation whose child's only begin was lost in transport and which was
/// then canceled: the child is idle with an empty queue, keeps its token
/// `uncertain-child-{label}` and the evaluated grant as its uncertain grant,
/// and no longer carries the in-memory bind backoff the failure armed, as a
/// restart would leave it.
struct CanceledChildWithUncertainGrant {
    state: AppState,
    root: PathBuf,
    project_id: String,
    parent_session_id: String,
    child_id: String,
    grant_id: String,
    _runtime_rx: std::sync::mpsc::Receiver<CodexRuntimeCommand>,
}

fn canceled_child_with_uncertain_grant(label: &str) -> CanceledChildWithUncertainGrant {
    let (state, runtime_rx) =
        test_app_state_with_delegation_codex_runtime(&format!("engram-uncertain-begin-{label}"));
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join(format!("engram-uncertain-begin-{label}-project"));
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram uncertain begin");
    let parent_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    enable_test_project_engram(&state, &project_id, &root);

    let grant_id = format!("uncertain-begin-{label}-grant");
    let transport = GatedEngramControlTransport::new([
        immediate_engram_step(
            "session_bind",
            bind_reply(&format!("uncertain-parent-{label}")),
        ),
        immediate_engram_step(
            "session_bind",
            bind_reply(&format!("uncertain-child-{label}")),
        ),
        immediate_engram_step("turn_evaluate", grant_reply(&grant_id)),
        immediate_engram_step(
            "turn_begin",
            ScriptedEngramControlResponse::Reply(Err(EngramTransportError::transport(
                "fixture begin transport failure",
            ))),
        ),
    ]);
    state.install_control_test_transport(transport);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Lose the begin reply.".to_owned(),
                title: Some(format!("Engram uncertain begin {label}")),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("the delegation is created even though its first dispatch degrades");
    let child_id = created.delegation.child_session_id.clone();
    state
        .cancel_delegation(&parent_session_id, &created.delegation.id)
        .expect("cancellation should complete");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&child_id)
            .expect("canceled child should remain");
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        assert!(record.queued_prompts.is_empty());
        assert_eq!(
            record.engram.uncertain_grant_id.as_deref(),
            Some(grant_id.as_str())
        );
        assert_eq!(
            record.engram.routing_token.as_deref(),
            Some(format!("uncertain-child-{label}").as_str())
        );
        record.engram.next_bind_retry_at = None;
        record.engram.circuit_open = false;
        record.engram.consecutive_transport_failures = 0;
    }
    CanceledChildWithUncertainGrant {
        state,
        root,
        project_id,
        parent_session_id,
        child_id,
        grant_id,
        _runtime_rx: runtime_rx,
    }
}

#[test]
fn a_recovery_receipt_for_another_grant_keeps_the_grant_until_engram_answers_for_it() {
    // Status reported the grant open; a checkpoint receipt naming another
    // grant settles nothing about it. Recovery stops before any bind and
    // keeps the record, so the finished child stays a boot target, and the
    // next boot settles it once Engram answers for the grant it reported.
    let fixture = canceled_child_with_uncertain_grant("mismatched-receipt");
    let recovery = RecordingBootRecoveryTransport::new();
    recovery.report_open_grant(&fixture.child_id, &fixture.grant_id);
    recovery.answer_checkpoints_with(&fixture.child_id, "some-other-grant");
    fixture
        .state
        .install_control_test_transport(recovery.clone());
    let plan = fixture
        .state
        .prepare_engram_sessions_for_boot_recovery()
        .expect("boot recovery plan should be prepared");
    fixture
        .state
        .recover_prepared_engram_sessions_after_boot(plan);
    assert_eq!(
        recovery.operations_for(&fixture.child_id),
        ["session_status", "turn_checkpoint"],
        "a receipt for another grant stops recovery before any bind"
    );
    {
        let mut inner = fixture.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&fixture.child_id)
            .expect("child should remain after the failed recovery");
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        assert_eq!(
            record.engram.uncertain_grant_id.as_deref(),
            Some(fixture.grant_id.as_str()),
            "the grant status reported open stays recorded"
        );
        assert_eq!(
            record.engram.routing_token.as_deref(),
            Some("uncertain-child-mismatched-receipt"),
            "with the token that can ask about it again"
        );
        assert!(record.engram.rebind_required);
        assert_eq!(
            record.engram.disabled_reason, None,
            "an anomalous receipt is a refusal to settle, not a fault that disables the session"
        );
        assert_eq!(
            record.engram.next_bind_retry_at, None,
            "a refusal arms no transport backoff"
        );
    }

    let next_boot = RecordingBootRecoveryTransport::new();
    next_boot.report_open_grant(&fixture.child_id, &fixture.grant_id);
    fixture
        .state
        .install_control_test_transport(next_boot.clone());
    let plan = fixture
        .state
        .prepare_engram_sessions_for_boot_recovery()
        .expect("boot recovery plan should be prepared");
    assert!(
        plan.targets
            .iter()
            .any(|target| target.connection.session_id == fixture.child_id),
        "the kept grant keeps the finished child a boot target"
    );
    fixture
        .state
        .recover_prepared_engram_sessions_after_boot(plan);
    assert_eq!(
        next_boot.operations_for(&fixture.child_id),
        ["session_status", "turn_checkpoint", "session_bind"],
        "a receipt for the reported grant settles it, then the child rebinds"
    );
    let inner = fixture.state.inner.lock().expect("state mutex poisoned");
    let child = &inner.sessions[inner
        .find_session_index(&fixture.child_id)
        .expect("child should remain after recovery")];
    assert_eq!(child.engram.uncertain_grant_id, None);
    assert_eq!(
        child.engram.routing_token.as_deref(),
        Some(format!("recovered-{}", fixture.child_id).as_str())
    );
}

#[test]
fn a_project_reset_on_the_same_store_keeps_an_uncertain_grant_for_the_next_bind() {
    // Disabling Engram is the reset that keeps the store. It checkpoints
    // mirrored grants only; a grant whose begin outcome never arrived may
    // still be begun there, and a record loaded before begins were recorded
    // may hide one, so such records keep their evidence with their token,
    // exactly as a failed reset checkpoint keeps its grant, while a settled
    // idle binding is dropped as before. Once the project is enabled again
    // the next bind settles them through a session status.
    let fixture = canceled_child_with_uncertain_grant("reset-same-store");
    let settled_sibling = create_test_project_session(
        &fixture.state,
        Agent::Codex,
        &fixture.project_id,
        &fixture.root,
    );
    {
        let mut inner = fixture.state.inner.lock().expect("state mutex poisoned");
        // The parent reloaded from a store written before begins were
        // recorded; the sibling was bound and is settled.
        let index = inner
            .find_session_index(&fixture.parent_session_id)
            .expect("parent should exist");
        inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .engram
            .begins_recorded = false;
        let index = inner
            .find_session_index(&settled_sibling)
            .expect("sibling should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        record.engram.routing_token = Some("settled-sibling-token".to_owned());
        record.engram.rebind_required = true;
    }
    fixture
        .state
        .update_project_engram_settings(&fixture.project_id, EngramProjectSettings::default())
        .expect("disabling Engram keeps the store");
    {
        let inner = fixture.state.inner.lock().expect("state mutex poisoned");
        let child = &inner.sessions[inner
            .find_session_index(&fixture.child_id)
            .expect("child should remain after the reset")];
        assert_eq!(
            child.engram.uncertain_grant_id.as_deref(),
            Some(fixture.grant_id.as_str()),
            "the reset keeps the uncertain grant it could not settle"
        );
        assert_eq!(
            child.engram.routing_token.as_deref(),
            Some("uncertain-child-reset-same-store"),
            "with the token that can ask about it"
        );
        assert!(child.engram.rebind_required);
        assert_eq!(child.engram.active_grant_id, None);
        let parent = &inner.sessions[inner
            .find_session_index(&fixture.parent_session_id)
            .expect("parent should remain after the reset")];
        assert_eq!(
            parent.engram.routing_token.as_deref(),
            Some("uncertain-parent-reset-same-store"),
            "an unrecorded record keeps the token a bind can settle it with"
        );
        assert!(
            !parent.engram.begins_recorded,
            "and stays unrecorded until that bind"
        );
        assert!(parent.engram.rebind_required);
        let sibling = &inner.sessions[inner
            .find_session_index(&settled_sibling)
            .expect("sibling should remain after the reset")];
        assert_eq!(
            sibling.engram.routing_token, None,
            "a settled idle binding without any grant is dropped as before"
        );
        assert!(!sibling.engram.rebind_required);
    }

    enable_test_project_engram(&fixture.state, &fixture.project_id, &fixture.root);
    let recovery = RecordingBootRecoveryTransport::new();
    fixture
        .state
        .install_control_test_transport(recovery.clone());
    let plan = fixture
        .state
        .prepare_engram_sessions_for_boot_recovery()
        .expect("boot recovery plan should be prepared");
    assert!(
        plan.targets
            .iter()
            .any(|target| target.connection.session_id == fixture.child_id),
        "the kept grant keeps the canceled child a boot target"
    );
    assert!(
        plan.targets
            .iter()
            .any(|target| target.connection.session_id == fixture.parent_session_id),
        "the kept token keeps the unrecorded parent a boot target"
    );
    assert!(
        !plan
            .targets
            .iter()
            .any(|target| target.connection.session_id == settled_sibling),
        "the dropped binding leaves nothing to recover"
    );
    fixture
        .state
        .recover_prepared_engram_sessions_after_boot(plan);
    assert_eq!(
        recovery.operations_for(&fixture.child_id),
        ["session_status", "session_bind"],
        "recovery asks Engram, which reports no open grant, then rebinds"
    );
    assert_eq!(
        recovery.operations_for(&fixture.parent_session_id),
        ["session_status", "session_bind"],
        "the unrecorded parent is settled the same way"
    );
    let inner = fixture.state.inner.lock().expect("state mutex poisoned");
    let child = &inner.sessions[inner
        .find_session_index(&fixture.child_id)
        .expect("child should remain after recovery")];
    assert_eq!(child.engram.uncertain_grant_id, None);
    let parent = &inner.sessions[inner
        .find_session_index(&fixture.parent_session_id)
        .expect("parent should remain after recovery")];
    assert!(
        parent.engram.begins_recorded,
        "an accepted bind marks the carried-over record settled"
    );
}

#[test]
fn a_project_reset_to_another_store_drops_an_uncertain_grant_with_its_binding() {
    // A grant's identity cannot carry into a different store. The reset
    // drops it together with the binding, as it drops the grant of a failed
    // checkpoint on a home change.
    let fixture = canceled_child_with_uncertain_grant("reset-other-store");
    let other_home = fixture.root.join("other-home");
    fs::create_dir_all(&other_home).expect("the other home should exist");
    fixture
        .state
        .update_project_engram_settings(
            &fixture.project_id,
            EngramProjectSettings {
                enabled: false,
                home: Some(other_home.to_string_lossy().into_owned()),
                ..EngramProjectSettings::default()
            },
        )
        .expect("disabling Engram towards another store");
    let inner = fixture.state.inner.lock().expect("state mutex poisoned");
    let child = &inner.sessions[inner
        .find_session_index(&fixture.child_id)
        .expect("child should remain after the reset")];
    assert_eq!(child.engram.uncertain_grant_id, None);
    assert_eq!(child.engram.routing_token, None);
    assert!(!child.engram.rebind_required);
}

#[test]
fn dropping_intent_beside_a_live_begin_yields_that_begin_exactly() {
    // The runtime marker alone names the grant handed to the transport. A
    // queue dropped before the marker is detached must yield that grant, not
    // let the unknown marker take the single slot first; without a marker
    // the intent's own begun grant, else the unknown marker, is the answer.
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-uncertain-begin-live-marker");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-uncertain-begin-live-marker-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram uncertain begin live marker");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    enable_test_project_engram(&state, &project_id, &root);
    queue_test_engram_prompt(
        &state,
        &session_id,
        "retained",
        QueuedPromptSource::User,
        None,
    );
    let target = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        AppState::engram_binding_target_for_session_shape_locked(&inner, &session_id, true)
            .expect("binding target should resolve")
            .expect("the project must have Engram enabled")
    };
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    let record = inner
        .session_mut_by_index(index)
        .expect("session index should be valid");
    record
        .queued_prompts
        .front_mut()
        .expect("the retained prompt should be queued")
        .engram_evaluate = Some(EngramQueuedEvaluate {
        begun_grant_id: None,
        connection: target.connection,
        settings: target.settings,
        operation_generation: None,
        request: EngramControlRequest::TurnEvaluate {
            routing_token: "live-marker-token".to_owned(),
            intent_fingerprint: "retained".to_owned(),
            purpose: "ordinary".to_owned(),
            requested_effects: target.effects,
            resource_intents: Vec::new(),
            idempotency_key: "retained".to_owned(),
        },
    });
    record.engram.pending_dispatch = Some(EngramPendingDispatch {
        begin_requested: Some("live-grant".to_owned()),
        ..pending_engram_grant(record.engram.dispatch_generation, "live-grant")
    });
    assert_eq!(
        dropped_engram_intent_grant(record, record.queued_prompts.iter()).as_deref(),
        Some("live-grant"),
        "a live marker names the exact grant in flight"
    );

    record.engram.pending_dispatch = None;
    // Restored from an earlier process, as the loader marks it, and not yet
    // answered by its exact recovery.
    record.engram.recovered_admission = true;
    assert_eq!(
        dropped_engram_intent_grant(record, record.queued_prompts.iter()).as_deref(),
        Some(ENGRAM_UNCERTAIN_GRANT_UNKNOWN),
        "without a marker a restored intent only says a begin may exist"
    );
    record
        .queued_prompts
        .front_mut()
        .expect("the retained prompt should be queued")
        .engram_evaluate
        .as_mut()
        .expect("the intent should be set")
        .begun_grant_id = Some("noted-grant".to_owned());
    assert_eq!(
        dropped_engram_intent_grant(record, record.queued_prompts.iter()).as_deref(),
        Some("noted-grant"),
        "a begun grant noted on the intent keeps its id"
    );

    record.engram.uncertain_grant_id = Some("already-recorded".to_owned());
    assert_eq!(
        dropped_engram_intent_grant(record, record.queued_prompts.iter()),
        None,
        "one recorded grant is all the record can settle"
    );
    record.engram.uncertain_grant_id = None;
    record
        .queued_prompts
        .front_mut()
        .expect("the retained prompt should be queued")
        .engram_evaluate = None;
    assert_eq!(
        dropped_engram_intent_grant(record, record.queued_prompts.iter()),
        None,
        "a prompt without evaluate intent owns no begin"
    );
}

#[test]
fn a_begin_whose_outcome_arrives_after_it_was_recorded_uncertain_settles_the_record() {
    // A queue dropped while its begin was in flight recorded the grant as
    // uncertain. When that begin then completes for the still-current
    // dispatch, the outcome is known: the grant is begun and mirrored, the
    // turn-end checkpoint will close it, and the uncertain marker goes.
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-uncertain-begin-late-outcome");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-uncertain-begin-late-outcome-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram uncertain begin late outcome");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    enable_test_project_engram(&state, &project_id, &root);
    let dispatch_generation = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        record.session.status = SessionStatus::Active;
        record.engram.routing_token = Some("late-outcome-token".to_owned());
        record.engram.uncertain_grant_id = Some("late-grant".to_owned());
        record.engram.pending_dispatch = Some(EngramPendingDispatch {
            begin_requested: Some("late-grant".to_owned()),
            ..pending_engram_grant(record.engram.dispatch_generation, "late-grant")
        });
        record.engram.dispatch_generation
    };
    let finish = state.finish_engram_dispatch_record(
        &session_id,
        dispatch_generation,
        Some("late-grant".to_owned()),
        None,
        EngramControlCard {
            schema_version: ENGRAM_CONTROL_SCHEMA_VERSION,
            stage: EngramControlStage::Dispatch,
            assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
            decision: EngramControlCardDecision::Grant,
            dispatch: EngramControlCardDispatch::SentOnGrant,
            refusal_code: None,
            defer_code: None,
            grant_id: Some("late-grant".to_owned()),
            directives: Vec::new(),
            delivered_range: None,
            latency_ms: EngramControlLatencyCard {
                evaluate: Some(0),
                begin: Some(0),
                checkpoint: None,
                total: 0,
            },
            fail_mode: EngramControlFailMode::Enforced,
            repair_armed: false,
            next_intent: None,
        },
    );
    assert_eq!(finish, EngramDispatchRecordFinish::Ready);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = &inner.sessions[inner
        .find_session_index(&session_id)
        .expect("session should remain")];
    assert_eq!(record.engram.active_grant_id.as_deref(), Some("late-grant"));
    assert_eq!(
        record.engram.uncertain_grant_id, None,
        "a begun and mirrored grant is no longer uncertain"
    );
}

/// Queues a prompt at `session_id` and gives it the durable evaluate intent a
/// crash between `turn_begin` and its reply leaves behind, as reloaded: no
/// begun grant noted, no runtime marker. Returns the intent fingerprint a
/// dispatch of that prompt carries.
fn restore_engram_intent_at_head(
    state: &AppState,
    session_id: &str,
    target: &EngramBindingTarget,
    text: &str,
    source: QueuedPromptSource,
    promoted: bool,
) -> String {
    queue_test_engram_prompt(state, session_id, text, source, None);
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(session_id)
        .expect("session should exist");
    let record = inner
        .session_mut_by_index(index)
        .expect("session index should be valid");
    // The loader marks a record whose queue carries intent.
    record.engram.recovered_admission = true;
    let queued = record
        .queued_prompts
        .front_mut()
        .expect("the prompt should be queued");
    queued.engram_evaluate = Some(EngramQueuedEvaluate {
        begun_grant_id: None,
        connection: target.connection.clone(),
        settings: target.settings.clone(),
        operation_generation: None,
        request: EngramControlRequest::TurnEvaluate {
            routing_token: "restored-token".to_owned(),
            intent_fingerprint: text.to_owned(),
            purpose: "ordinary".to_owned(),
            requested_effects: target.effects.clone(),
            resource_intents: Vec::new(),
            idempotency_key: text.to_owned(),
        },
    });
    queued.promoted_message_index = promoted.then_some(0);
    engram_turn_intent_fingerprint(
        &queued.pending_prompt.text,
        queued.pending_prompt.expanded_text.as_deref(),
        &queued.attachments,
        queued.pending_prompt.source.as_ref(),
        queued.source,
    )
}

#[test]
fn abandoning_a_dispatch_retires_its_promoted_head_only_after_taking_over_its_intent() {
    // After a restart only the durable evaluate intent survives an in-flight
    // begin. When its exact recovery cannot ask Engram, the dispatch degrades
    // with no begin of its own, and a terminal callback abandons it by
    // retiring the promoted head: the intent goes with the head, so the
    // record takes over its evidence first. A marker with a begin of its own
    // names the exact grant instead, and a head that is not retired keeps
    // its intent for the exact recovery.
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-uncertain-begin-retired-head");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-uncertain-begin-retired-head-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram uncertain begin retired head");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    enable_test_project_engram(&state, &project_id, &root);
    let target = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        AppState::engram_binding_target_for_session_shape_locked(&inner, &session_id, true)
            .expect("binding target should resolve")
            .expect("the project must have Engram enabled")
    };
    let degraded_dispatch = |generation: u64, fingerprint: String| EngramPendingDispatch {
        intent_fingerprint: fingerprint,
        evaluated: EngramDispatchEvaluation::Degraded {
            code: "control_status_failed".to_owned(),
            detail: "fixture recovery status failure".to_owned(),
        },
        ..pending_engram_grant(generation, "unused")
    };

    let fingerprint = restore_engram_intent_at_head(
        &state,
        &session_id,
        &target,
        "retained-degraded",
        QueuedPromptSource::User,
        true,
    );
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        record.engram.pending_dispatch = Some(degraded_dispatch(
            record.engram.dispatch_generation,
            fingerprint,
        ));
        assert!(take_and_abandon_engram_pending_dispatch(record));
        assert!(
            record.queued_prompts.is_empty(),
            "the promoted head is retired with its dispatch"
        );
        assert_eq!(
            record.engram.uncertain_grant_id.as_deref(),
            Some(ENGRAM_UNCERTAIN_GRANT_UNKNOWN),
            "the retired intent's possible begin is recorded before the head goes"
        );
        assert!(record.engram.rebind_required);
        record.engram.uncertain_grant_id = None;
        record.engram.rebind_required = false;
    }

    let fingerprint = restore_engram_intent_at_head(
        &state,
        &session_id,
        &target,
        "retained-begun",
        QueuedPromptSource::User,
        true,
    );
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        record.engram.pending_dispatch = Some(EngramPendingDispatch {
            intent_fingerprint: fingerprint,
            begin_requested: Some("exact-grant".to_owned()),
            ..pending_engram_grant(record.engram.dispatch_generation, "exact-grant")
        });
        assert!(take_and_abandon_engram_pending_dispatch(record));
        assert!(record.queued_prompts.is_empty());
        assert_eq!(
            record.engram.uncertain_grant_id.as_deref(),
            Some("exact-grant"),
            "a marker with a begin of its own names the exact grant"
        );
        record.engram.uncertain_grant_id = None;
        record.engram.rebind_required = false;
    }

    let fingerprint = restore_engram_intent_at_head(
        &state,
        &session_id,
        &target,
        "retained-unpromoted",
        QueuedPromptSource::User,
        false,
    );
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    let record = inner
        .session_mut_by_index(index)
        .expect("session index should be valid");
    record.engram.pending_dispatch = Some(degraded_dispatch(
        record.engram.dispatch_generation,
        fingerprint,
    ));
    assert!(take_and_abandon_engram_pending_dispatch(record));
    assert!(
        record
            .queued_prompts
            .front()
            .is_some_and(|queued| queued.engram_evaluate.is_some()),
        "a head that was never promoted keeps its intent for the exact recovery"
    );
    assert_eq!(
        record.engram.uncertain_grant_id, None,
        "nothing is recorded while the intent itself remains"
    );
}

#[test]
fn clearing_prompts_by_source_records_a_dropped_intent_unless_the_head_is_kept() {
    // Stopping orchestrator work clears its queued prompts by source. A
    // dropped prompt with evaluate intent may own a begin an earlier process
    // lost, so the clear records it; the variant that keeps the promoted
    // head for the Stop that owns it drops nothing that could own one.
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-uncertain-begin-clear-by-source");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-uncertain-begin-clear-by-source-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram uncertain begin clear by source");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    enable_test_project_engram(&state, &project_id, &root);
    let target = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        AppState::engram_binding_target_for_session_shape_locked(&inner, &session_id, true)
            .expect("binding target should resolve")
            .expect("the project must have Engram enabled")
    };

    restore_engram_intent_at_head(
        &state,
        &session_id,
        &target,
        "orchestrated-owned",
        QueuedPromptSource::Orchestrator,
        true,
    );
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let owner = EngramQueuedAdmissionOwner::capture_promoted(record)
            .expect("the promoted head should be capturable");
        clear_queued_prompts_by_source_except_admission_owner(
            record,
            QueuedPromptSource::Orchestrator,
            Some(&owner),
        );
        assert_eq!(
            record.queued_prompts.len(),
            1,
            "the head the Stop owns is kept"
        );
        assert_eq!(
            record.engram.uncertain_grant_id, None,
            "a kept head's intent is not a dropped one"
        );
        clear_queued_prompts_by_source_except_admission_owner(
            record,
            QueuedPromptSource::Orchestrator,
            None,
        );
        assert!(record.queued_prompts.is_empty());
        assert_eq!(
            record.engram.uncertain_grant_id.as_deref(),
            Some(ENGRAM_UNCERTAIN_GRANT_UNKNOWN),
            "an unowned head is dropped and its possible begin recorded"
        );
        assert!(record.engram.rebind_required);
        record.engram.uncertain_grant_id = None;
        record.engram.rebind_required = false;
    }

    restore_engram_intent_at_head(
        &state,
        &session_id,
        &target,
        "orchestrated-plain",
        QueuedPromptSource::Orchestrator,
        false,
    );
    queue_test_engram_prompt(
        &state,
        &session_id,
        "user-kept",
        QueuedPromptSource::User,
        None,
    );
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    let record = inner
        .session_mut_by_index(index)
        .expect("session index should be valid");
    clear_queued_prompts_by_source(record, QueuedPromptSource::Orchestrator);
    assert_eq!(
        record.queued_prompts.len(),
        1,
        "prompts of another source stay"
    );
    assert_eq!(record.queued_prompts[0].source, QueuedPromptSource::User);
    assert_eq!(
        record.engram.uncertain_grant_id.as_deref(),
        Some(ENGRAM_UNCERTAIN_GRANT_UNKNOWN),
        "the plain clear records the dropped intent's possible begin"
    );
    assert!(record.engram.rebind_required);
}

#[test]
fn a_same_store_reset_keeps_the_token_of_a_record_whose_intent_alone_may_own_a_begin() {
    // After a crash during turn_begin, a record written since begins were
    // recorded holds no grant: its retained evaluate intent is the only
    // evidence. A same-store reset keeps that queue, so it keeps the token
    // too; otherwise canceling the intent later would record the unknown
    // marker with nothing to ask Engram with, and the fresh bind recovery
    // then attempts is refused while the grant is begun.
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-uncertain-begin-reset-intent");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-uncertain-begin-reset-intent-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram uncertain begin reset intent");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    enable_test_project_engram(&state, &project_id, &root);
    let target = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        AppState::engram_binding_target_for_session_shape_locked(&inner, &session_id, true)
            .expect("binding target should resolve")
            .expect("the project must have Engram enabled")
    };
    restore_engram_intent_at_head(
        &state,
        &session_id,
        &target,
        "retained-across-reset",
        QueuedPromptSource::User,
        false,
    );
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        record.engram.routing_token = Some("reset-intent-token".to_owned());
        record.engram.rebind_required = true;
        assert!(record.engram.begins_recorded);
    }

    state
        .update_project_engram_settings(&project_id, EngramProjectSettings::default())
        .expect("disabling Engram keeps the store");
    let prompt_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner
            .find_session_index(&session_id)
            .expect("session should remain after the reset")];
        assert_eq!(
            record.engram.routing_token.as_deref(),
            Some("reset-intent-token"),
            "the token stays with the intent that may own a begin"
        );
        assert!(record.engram.rebind_required);
        let queued = record
            .queued_prompts
            .front()
            .expect("the retained head survives the reset");
        assert!(queued.engram_evaluate.is_some());
        assert!(
            queued.engram_interrupted,
            "the reset retains it for explicit resolution"
        );
        queued.pending_prompt.id.clone()
    };

    state
        .cancel_queued_prompt(&session_id, &prompt_id)
        .expect("the retained prompt can be canceled");
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner
            .find_session_index(&session_id)
            .expect("session should remain after the cancel")];
        assert!(record.queued_prompts.is_empty());
        assert_eq!(
            record.engram.uncertain_grant_id.as_deref(),
            Some(ENGRAM_UNCERTAIN_GRANT_UNKNOWN),
            "canceling the intent records the begin it may own"
        );
        assert_eq!(
            record.engram.routing_token.as_deref(),
            Some("reset-intent-token"),
            "with the token recovery asks with"
        );
    }

    enable_test_project_engram(&state, &project_id, &root);
    let recovery = RecordingBootRecoveryTransport::new();
    state.install_control_test_transport(recovery.clone());
    let plan = state
        .prepare_engram_sessions_for_boot_recovery()
        .expect("boot recovery plan should be prepared");
    assert!(
        plan.targets
            .iter()
            .any(|target| target.connection.session_id == session_id),
        "the recorded marker keeps the session a boot target"
    );
    state.recover_prepared_engram_sessions_after_boot(plan);
    assert_eq!(
        recovery.operations_for(&session_id),
        ["session_status", "session_bind"],
        "recovery asks Engram with the kept token, which reports no open grant, then rebinds"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = &inner.sessions[inner
        .find_session_index(&session_id)
        .expect("session should remain after recovery")];
    assert_eq!(record.engram.uncertain_grant_id, None);
}

#[test]
fn an_intent_this_process_issued_records_nothing_when_its_marker_sent_no_begin() {
    // A marker of the current dispatch without a begin proves this process
    // handed none to the transport, and a fresh intent whose marker is
    // already gone was accounted for by that marker's own release. Only an
    // intent restored from an earlier process whose exact recovery has not
    // answered, or one retained as interrupted after an unknown delivery,
    // may own a begin nobody recorded.
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-uncertain-begin-fresh-intent");
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-uncertain-begin-fresh-intent-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram uncertain begin fresh intent");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    enable_test_project_engram(&state, &project_id, &root);
    let target = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        AppState::engram_binding_target_for_session_shape_locked(&inner, &session_id, true)
            .expect("binding target should resolve")
            .expect("the project must have Engram enabled")
    };
    let fingerprint = restore_engram_intent_at_head(
        &state,
        &session_id,
        &target,
        "fresh",
        QueuedPromptSource::User,
        true,
    );
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    let record = inner
        .session_mut_by_index(index)
        .expect("session index should be valid");
    // Issued by this process: nothing was restored.
    record.engram.recovered_admission = false;
    record.engram.pending_dispatch = Some(EngramPendingDispatch {
        intent_fingerprint: fingerprint.clone(),
        ..pending_engram_grant(record.engram.dispatch_generation, "fresh-grant")
    });
    assert_eq!(
        dropped_engram_intent_grant(record, record.queued_prompts.iter()),
        None,
        "a marker that sent no begin proves nothing was begun"
    );
    record.engram.pending_dispatch = None;
    assert_eq!(
        dropped_engram_intent_grant(record, record.queued_prompts.iter()),
        None,
        "a fresh intent without a marker was accounted for when the marker went"
    );

    record
        .queued_prompts
        .front_mut()
        .expect("the prompt should be queued")
        .engram_interrupted = true;
    assert_eq!(
        dropped_engram_intent_grant(record, record.queued_prompts.iter()).as_deref(),
        Some(ENGRAM_UNCERTAIN_GRANT_UNKNOWN),
        "an intent retained after an unknown delivery may own a begin"
    );
    record
        .queued_prompts
        .front_mut()
        .expect("the prompt should be queued")
        .engram_interrupted = false;
    record.engram.recovered_admission = true;
    assert_eq!(
        dropped_engram_intent_grant(record, record.queued_prompts.iter()).as_deref(),
        Some(ENGRAM_UNCERTAIN_GRANT_UNKNOWN),
        "so may one restored from an earlier process until its recovery answers"
    );

    // A terminal callback abandoning a fresh dispatch before its begin
    // retires the head and leaves no recovery target behind.
    record.engram.recovered_admission = false;
    record.engram.pending_dispatch = Some(EngramPendingDispatch {
        intent_fingerprint: fingerprint,
        ..pending_engram_grant(record.engram.dispatch_generation, "fresh-grant")
    });
    assert!(take_and_abandon_engram_pending_dispatch(record));
    assert!(record.queued_prompts.is_empty());
    assert_eq!(
        record.engram.uncertain_grant_id, None,
        "a Stop or terminal callback before the begin records nothing"
    );
}
