// Owns terminal-duty handoff, reviewer-local dedupe and unsaved parent-wait controls.
// Local fixture schedules and SQLite faults are not live-provider evidence.
use super::controls::{archive_and_result, drive_ready, next_command, ready_page, respond_start};
use super::terminal_controls::{join_worker, saved_delegation, worker_at_status_wait};
use super::*;

type GapHook = Box<dyn FnOnce() + Send>;

fn gap_hooks() -> &'static Mutex<HashMap<String, GapHook>> {
    static HOOKS: std::sync::OnceLock<Mutex<HashMap<String, GapHook>>> = std::sync::OnceLock::new();
    HOOKS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn run_gap_hook(gate: &str) {
    // Taking the closure before invoking it releases this mutex. The caller
    // must likewise have released its state lock before entering this seam.
    let hook = gap_hooks().lock().unwrap().remove(gate);
    if let Some(hook) = hook {
        hook();
    }
}

struct GapCleanup(String);

impl Drop for GapCleanup {
    fn drop(&mut self) {
        gap_hooks().lock().unwrap().remove(&self.0);
    }
}

fn with_gap(gate: &str, hook: impl FnOnce() + Send + 'static, call: impl FnOnce()) {
    assert!(gap_hooks()
        .lock()
        .unwrap()
        .insert(gate.to_owned(), Box::new(hook))
        .is_none());
    let _cleanup = GapCleanup(gate.to_owned());
    call();
    assert!(
        !gap_hooks().lock().unwrap().contains_key(gate),
        "the exact off-lock handoff cut must run"
    );
}

fn scope(fixture: &ReviewerFixture) -> CodexReviewerMcpScope {
    codex_reviewer_mcp_scope(
        &fixture.state,
        &fixture.child,
        REVIEW_THREAD,
        &fixture.runtime.runtime_id,
    )
    .unwrap()
}

fn assert_terminal_memory(fixture: &ReviewerFixture) {
    let inner = fixture.state.inner.lock().unwrap();
    let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
    assert!(matches!(
        child.session.status,
        SessionStatus::Error | SessionStatus::Idle
    ));
    let d = &inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()];
    assert_eq!(
        d.status,
        DelegationStatus::Failed,
        "no-submission review must have a terminal parent result"
    );
    assert_eq!(d.result.as_ref().unwrap().status, DelegationStatus::Failed);
}

fn replay_release(fixture: &ReviewerFixture, batch: EngramMcpRuntimeRevocationBatch) {
    let released = fixture
        .state
        .release_engram_mcp_runtime_revocations_without_teardown(batch);
    for (session, token, callbacks) in released.deferred_callbacks {
        fixture
            .state
            .replay_deferred_runtime_stop_callbacks(&session, &token, callbacks);
    }
}

fn queued_ready_revocation(released_before_send: bool) {
    let fixture = ReviewerFixture::new("queued-ready-revocation");
    let written = fixture.start();
    respond_start(&fixture, &written, ready_page());
    let ready = next_command(&fixture);
    assert!(matches!(
        &ready,
        CodexRuntimeCommand::ReviewerMcpReady { .. }
    ));
    let batch = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let batch =
            claim_engram_mcp_runtime_revocations_locked(&mut inner, &[fixture.child.clone()]);
        assert_eq!(batch.targets.len(), 1);
        batch
    };
    let held = if released_before_send {
        replay_release(&fixture, batch);
        None
    } else {
        Some(batch)
    };
    assert!(
        drive_ready(&fixture, ready).is_empty(),
        "revoked Ready cannot authorize model work"
    );
    assert!(fixture.pending.lock().unwrap().is_empty());
    if let Some(batch) = held {
        let inner = fixture.state.inner.lock().unwrap();
        let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
        assert_eq!(child.session.status, SessionStatus::Active);
        assert_eq!(
            child.deferred_stop_callbacks.len(),
            1,
            "queued Ready must transfer terminal duty to the fence"
        );
        drop(inner);
        replay_release(&fixture, batch);
    }
    assert_terminal_memory(&fixture);
    assert_eq!(
        archive_and_result(&fixture).status,
        DelegationStatus::Failed
    );
    assert!(fixture.input_rx.try_recv().is_err());
}

#[test]
fn codex_reviewer_mcp_queued_ready_held_revocation_keeps_terminal_duty() {
    queued_ready_revocation(false);
}

#[test]
fn codex_reviewer_mcp_queued_ready_released_revocation_keeps_terminal_duty() {
    queued_ready_revocation(true);
}

#[test]
fn codex_reviewer_mcp_initial_start_under_revocation_keeps_terminal_duty() {
    let fixture = ReviewerFixture::new("initial-start-revocation");
    let batch = {
        let mut inner = fixture.state.inner.lock().unwrap();
        claim_engram_mcp_runtime_revocations_locked(&mut inner, &[fixture.child.clone()])
    };
    assert_eq!(batch.targets.len(), 1);
    assert!(
        fixture.start().is_empty(),
        "initial fenced start may not query or send model work"
    );
    {
        let inner = fixture.state.inner.lock().unwrap();
        let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
        assert_eq!(child.session.status, SessionStatus::Active);
        assert_eq!(
            child.deferred_stop_callbacks.len(),
            1,
            "initial start must not silently lose its duty"
        );
    }
    replay_release(&fixture, batch);
    assert_terminal_memory(&fixture);
    assert_eq!(
        archive_and_result(&fixture).status,
        DelegationStatus::Failed
    );
}

fn install_terminal_fault(fixture: &ReviewerFixture, status: &str) {
    assert!(matches!(status, "idle" | "error"));
    let child = fixture.child.replace('\'', "''");
    let connection = rusqlite::Connection::open(fixture.state.persistence_path.as_path()).unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER reject_settlement_terminal BEFORE INSERT ON sessions
         WHEN NEW.id = '{child}' AND json_extract(NEW.value_json, '$.session.status') = '{status}'
         BEGIN SELECT RAISE(ABORT, 'fixture terminal-only settlement save unavailable'); END;"
        ))
        .unwrap();
}

#[test]
fn codex_reviewer_mcp_normal_finish_terminal_save_failure_keeps_parent_result() {
    let fixture = ReviewerFixture::new("normal-finish-terminal-save");
    // Arrange the finish phase on the actual status worker so its JoinHandle
    // supplies a deterministic terminal boundary, not a timing/poll premise.
    let worker = worker_at_status_wait(&fixture, false);
    let CodexRuntimeCommand::JsonRpcRequest {
        method,
        response_tx,
        ..
    } = next_command(&fixture)
    else {
        panic!("normal finish worker must own the status request");
    };
    assert_eq!(method, "mcpServerStatus/list");
    install_terminal_fault(&fixture, "idle");
    response_tx.send(Ok(ready_page())).unwrap();
    join_worker(worker);
    let durable = saved_delegation(&fixture);
    assert_eq!(durable.attempt.reviewer_mcp_observations.len(), 1);
    assert_eq!(durable.attempt.reviewer_mcp_observations[0].phase, "finish");
    assert_eq!(
        durable.attempt.reviewer_mcp_observations[0].outcome,
        CodexReviewerMcpOutcome::Ready
    );
    assert_eq!(
        durable.status,
        DelegationStatus::Running,
        "terminal result was not saved"
    );
    assert!(durable.result.is_none());
    assert_terminal_memory(&fixture);
    assert!(fixture.pending.lock().unwrap().is_empty());
    assert!(
        fixture.input_rx.try_recv().is_err(),
        "unsaved fallback cannot dispatch model work or cleanup"
    );
}

#[test]
fn codex_reviewer_mcp_failure_handoff_revocation_buffers_exact_duty() {
    let fixture = ReviewerFixture::new("failure-handoff-revocation");
    let scope = scope(&fixture);
    let held = Arc::new(Mutex::new(None));
    let captured = held.clone();
    let state = fixture.state.clone();
    let child = fixture.child.clone();
    with_gap(
        &scope.gate,
        move || {
            let mut inner = state.inner.lock().unwrap();
            let batch = claim_engram_mcp_runtime_revocations_locked(&mut inner, &[child]);
            assert_eq!(batch.targets.len(), 1);
            *captured.lock().unwrap() = Some(batch);
        },
        || fail_codex_reviewer_mcp(&fixture.state, &scope, "failure at the exact handoff"),
    );
    {
        let inner = fixture.state.inner.lock().unwrap();
        let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
        assert_eq!(child.session.status, SessionStatus::Active);
        assert!(child.runtime_stop_in_progress);
        assert_eq!(
            child.deferred_stop_callbacks.len(),
            1,
            "fence claimed in the handoff gap must retain failure"
        );
        assert!(
            matches!(&child.deferred_stop_callbacks[0], DeferredStopCallback::TurnFailed {
            active_turn_generation: 0, message
        } if message == "failure at the exact handoff")
        );
    }
    replay_release(&fixture, held.lock().unwrap().take().unwrap());
    assert_terminal_memory(&fixture);
    assert_eq!(
        archive_and_result(&fixture).status,
        DelegationStatus::Failed
    );
}

#[test]
fn codex_reviewer_mcp_unsaved_fallback_does_not_consume_parent_wait() {
    let fixture = ReviewerFixture::new("unsaved-parent-wait");
    let mut delta_events = fixture.state.subscribe_delta_events();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let p = inner.find_session_index(&fixture.parent).unwrap();
        // Capture the real parent dispatch at a local Codex runtime channel;
        // no provider writer/model runs in this fixture.
        inner.sessions[p].session.agent = Agent::Codex;
        inner.sessions[p].session.model = "gpt-5.4".to_owned();
        inner.sessions[p].session.workdir = fixture.state.test_temp_root.as_ref()
            .unwrap().path().to_string_lossy().into_owned();
        inner.sessions[p].session.status = SessionStatus::Idle;
        inner.sessions[p].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: fixture.runtime.runtime_id.clone(),
            input_tx: fixture.runtime.input_tx.clone(),
            process: fixture.process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: fixture.runtime.clone(), session_id: fixture.parent.clone(),
            }),
        });
        fixture.state.commit_locked(&mut inner).unwrap();
    }
    let created = fixture
        .state
        .create_delegation_wait(
            &fixture.parent,
            CreateDelegationWaitRequest {
                delegation_ids: vec![fixture.delegation.clone()],
                mode: DelegationWaitMode::All,
                title: Some("Retain until settlement can be saved".to_owned()),
            },
        )
        .unwrap();
    assert!(!created.resume_prompt_queued);
    let scope = scope(&fixture);
    install_terminal_fault(&fixture, "error");
    fail_codex_reviewer_mcp(&fixture.state, &scope, "unsaved terminal result");
    assert_terminal_memory(&fixture);
    {
        let inner = fixture.state.inner.lock().unwrap();
        assert!(
            inner
                .delegation_waits
                .iter()
                .any(|w| w.id == created.wait.id),
            "own in-memory fallback must leave the durable wait duty discoverable"
        );
        let parent = &inner.sessions[inner.find_session_index(&fixture.parent).unwrap()];
        assert_eq!(parent.session.status, SessionStatus::Idle);
        assert!(
            parent.queued_prompts.is_empty(),
            "do not consume or dispatch an unsaved wait resume"
        );
    }
    assert!(saved_delegation(&fixture).result.is_none());
    let connection = rusqlite::Connection::open(fixture.state.persistence_path.as_path()).unwrap();
    connection
        .execute_batch("DROP TRIGGER reject_settlement_terminal;")
        .unwrap();
    drop(connection);
    let recovered = fixture
        .state
        .get_delegation_result(&fixture.parent, &fixture.delegation)
        .unwrap();
    assert_eq!(recovered.result.status, DelegationStatus::Failed);
    let saved = saved_delegation(&fixture);
    assert_eq!(saved.status, DelegationStatus::Failed);
    assert_eq!(saved.result.as_ref(), Some(&recovered.result),
        "the public recovery owner must durably save the exact terminal result");
    // Cleanup and parent delivery can arrive in either order on this channel.
    let mut resumes = 0;
    for _ in 0..2 {
        match next_command(&fixture) {
            CodexRuntimeCommand::JsonRpcRequest { method, response_tx, .. } => {
                assert_eq!(method, "thread/archive");
                response_tx.send(Ok(json!({}))).unwrap();
            }
            CodexRuntimeCommand::Prompt { session_id, command } => {
                assert_eq!(session_id, fixture.parent);
                assert!(command.prompt.contains(&fixture.delegation));
                assert!(command.prompt.contains("Retain until settlement can be saved"));
                resumes += 1;
            }
            _ => panic!("only child cleanup and one captured parent resume are expected"),
        }
    }
    assert_eq!(resumes, 1, "the real parent resume is dispatched exactly once");
    let repeated = fixture.state.get_delegation_result(&fixture.parent, &fixture.delegation).unwrap();
    assert_eq!(repeated.result, recovered.result);
    assert!(fixture.input_rx.try_recv().is_err(), "a second poll must not redispatch the resume");
    {
        let inner = fixture.state.inner.lock().unwrap();
        assert!(!inner.delegation_waits.iter().any(|w| w.id == created.wait.id));
        let parent = &inner.sessions[inner.find_session_index(&fixture.parent).unwrap()];
        assert_eq!(parent.session.status, SessionStatus::Active);
        assert!(parent.queued_prompts.is_empty());
    }
    let mut consumed = 0;
    loop {
        let payload = match delta_events.try_recv() {
            Ok(payload) => payload,
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(error) => panic!("wait-consumed evidence must not be lost: {error}"),
        };
        if let DeltaEvent::DelegationWaitConsumed { wait_id, .. } =
            serde_json::from_str::<DeltaEvent>(&payload).unwrap()
        {
            if wait_id == created.wait.id { consumed += 1; }
        }
    }
    assert_eq!(consumed, 1, "the registered wait is consumed exactly once");
}

fn settle_after_submission(failure: Option<&str>, assistant_output: bool) {
    let fixture = ReviewerFixture::new("settle-after-submission");
    let scope = scope(&fixture);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let i = inner.find_session_index(&fixture.child).unwrap();
        if assistant_output {
            // Arrange current-turn final prose without bypassing the settlement
            // primitive or changing the child to Idle before it runs.
            let id = inner.next_message_id();
            push_message_on_record(inner.session_mut_by_index(i).unwrap(), Message::Text {
                id, timestamp: stamp_now(), author: Author::Assistant,
                text: "The review is complete; the authoritative findings were submitted.".to_owned(),
                attachments: Vec::new(), expanded_text: None, source: None,
            });
            fixture.state.commit_locked(&mut inner).unwrap();
        }
        let child = &inner.sessions[i];
        assert_eq!(child.session.status, SessionStatus::Active);
        assert_eq!(latest_assistant_delegation_result(&child.session, true).is_some(), assistant_output,
            "the positive control and missing-output witness must differ in the final-output instrument");
    }
    fixture.state.submit_delegation_review_result(&fixture.child, structured_review_request()).unwrap();
    let submitted = {
        let inner = fixture.state.inner.lock().unwrap();
        let d = &inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()];
        d.submitted_review_result.clone().unwrap()
    };
    settle_codex_reviewer_mcp(&fixture.state, &scope, failure);
    assert_eq!(archive_and_result(&fixture), submitted);
    let saved = saved_delegation(&fixture);
    assert_eq!(saved.status, DelegationStatus::Completed);
    assert_eq!(saved.result.as_ref(), Some(&submitted));
    assert!(saved.submitted_review_result.is_none());
    assert_eq!(saved.review_result_schema_version, Some(DELEGATION_REVIEW_RESULT_SCHEMA_VERSION));
    match failure {
        Some(detail) => assert!(saved.post_submission_transport_error.as_deref()
            .is_some_and(|error| error.contains(detail)), "the real failure detail stays separate from the payload"),
        None if assistant_output => assert!(saved.post_submission_transport_error.is_none()),
        None => assert_eq!(saved.post_submission_transport_error.as_deref(), Some(
            "Child became idle without a final assistant packet after structured result submission.")),
    }
    // The failure primitive first writes Error; authoritative promotion then
    // returns the child to Idle. Assert that existing final owner, not Error.
    let inner = fixture.state.inner.lock().unwrap();
    let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
    assert_eq!(child.session.status, SessionStatus::Idle);
    if let Some(detail) = failure {
        assert!(child.session.messages.iter().any(|message| matches!(message,
            Message::Text { text, .. } if text.contains(&format!("Turn failed: {detail}")))));
    }
    assert!(fixture.input_rx.try_recv().is_err(), "settlement cannot dispatch further model work");
}

#[test]
fn codex_reviewer_mcp_submission_before_failed_settle_keeps_payload_and_transport_detail() {
    settle_after_submission(Some("exact post-submission transport failure"), false);
}

#[test]
fn codex_reviewer_mcp_submission_before_completed_settle_keeps_payload_without_transport_error() {
    settle_after_submission(None, true);
}

#[test]
fn codex_reviewer_mcp_submission_before_completed_settle_without_output_keeps_missing_packet_metadata() {
    settle_after_submission(None, false);
}

#[test]
fn codex_reviewer_mcp_settlement_drops_redundant_completion_but_keeps_failures() {
    let fixture = ReviewerFixture::new("terminal-duty-dedupe");
    let scope = scope(&fixture);
    let batch = {
        let mut inner = fixture.state.inner.lock().unwrap();
        claim_engram_mcp_runtime_revocations_locked(&mut inner, &[fixture.child.clone()])
    };
    assert_eq!(batch.targets.len(), 1);
    // This policy belongs to reviewer settlement, not the generic primitives.
    settle_codex_reviewer_mcp(&fixture.state, &scope, None);
    settle_codex_reviewer_mcp(&fixture.state, &scope, None);
    settle_codex_reviewer_mcp(&fixture.state, &scope, Some("first failure"));
    settle_codex_reviewer_mcp(&fixture.state, &scope, Some("duplicate failure"));
    settle_codex_reviewer_mcp(&fixture.state, &scope, None);
    {
        let inner = fixture.state.inner.lock().unwrap();
        let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
        assert_eq!(
            child.deferred_stop_callbacks.len(),
            1,
            "first failure replaces completion before terminal replay can detach the runtime"
        );
        assert!(
            matches!(&child.deferred_stop_callbacks[0], DeferredStopCallback::TurnFailed {
                active_turn_generation: 0, message,
            } if message == "first failure")
        );
    }
    replay_release(&fixture, batch);
    assert_terminal_memory(&fixture);
    let inner = fixture.state.inner.lock().unwrap();
    let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
    assert!(child.session.messages.iter().any(|message| matches!(message,
        Message::Text { text, .. } if text.contains("Turn failed: first failure"))),
        "completion replay must not silently discard the later failure duty");
}

#[test]
fn codex_reviewer_mcp_direct_observation_push_dedupes_terminal_duty() {
    let fixture = ReviewerFixture::new("observation-duty-dedupe");
    let scope = scope(&fixture);
    let batch = {
        let mut inner = fixture.state.inner.lock().unwrap();
        claim_engram_mcp_runtime_revocations_locked(&mut inner, &[fixture.child.clone()])
    };
    let token = RuntimeToken::Codex(fixture.runtime.runtime_id.clone());
    fixture
        .state
        .finish_turn_ok_if_runtime_matches_guarded(&fixture.child, &token, Some(0))
        .unwrap();
    for _ in 0..2 {
        assert!(!record_codex_reviewer_mcp_observation(
            &fixture.state,
            &scope,
            CodexReviewerMcpObservation {
                phase: "finish".to_owned(),
                outcome: CodexReviewerMcpOutcome::Ready,
                elapsed_ms: 0,
                budget_ms: 60_000,
                measurement: "local arranged cut".to_owned(),
                reason: "No startup reason supplied.".to_owned(),
            }
        )
        .unwrap());
    }
    {
        let inner = fixture.state.inner.lock().unwrap();
        let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
        assert_eq!(
            child.deferred_stop_callbacks.len(),
            1,
            "direct observation push must use the same dedupe invariant"
        );
        assert!(matches!(
            &child.deferred_stop_callbacks[0],
            DeferredStopCallback::TurnFailed {
                active_turn_generation: 0,
                ..
            }
        ));
    }
    replay_release(&fixture, batch);
    assert_terminal_memory(&fixture);
}

fn handoff_gap(gap: &'static str) {
    // These mutations arrange the documented action-level gaps; they do not
    // claim that identity cannot change or that a public reset/follow-up ran.
    let fixture = ReviewerFixture::new(gap);
    let scope = scope(&fixture);
    let original_attempt = scope.attempt;
    let state = fixture.state.clone();
    let child = fixture.child.clone();
    let delegation = fixture.delegation.clone();
    let submitted_before = Arc::new(Mutex::new(None));
    let submitted_in_gap = submitted_before.clone();
    let replacement_runtime = SessionRuntime::Codex(CodexRuntimeHandle {
        runtime_id: "replacement-runtime".to_owned(),
        input_tx: fixture.runtime.input_tx.clone(),
        process: fixture.process.clone(),
        shared_session: None,
    });
    with_gap(
        &scope.gate,
        move || {
            if gap == "submission" {
                state
                    .submit_delegation_review_result(&child, structured_review_request())
                    .unwrap();
                let inner = state.inner.lock().unwrap();
                let d = &inner.delegations[inner.find_delegation_index(&delegation).unwrap()];
                *submitted_in_gap.lock().unwrap() =
                    Some(d.submitted_review_result.clone().expect(
                        "the actual submit must publish a provisional authoritative payload",
                    ));
                return;
            }
            let mut inner = state.inner.lock().unwrap();
            let i = inner.find_session_index(&child).unwrap();
            match gap {
                "thread-clear" => inner.sessions[i].external_session_id = None,
                "thread-replace" => {
                    inner.sessions[i].external_session_id = Some("replacement-thread".to_owned())
                }
                "reset" => inner.sessions[i].runtime_reset_required = true,
                "generation" => inner.sessions[i].active_turn_generation += 1,
                // A successor has a different live token. No runtime at all
                // instead invokes the existing same-generation recovery path.
                "runtime" => inner.sessions[i].runtime = replacement_runtime,
                "attempt" => {
                    // Arrange the admitted queued follow-up phase, using the
                    // real queue and re-arm owners. No new generation is promoted.
                    inner.sessions[i].session.status = SessionStatus::Error;
                    let d = inner.find_delegation_index(&delegation).unwrap();
                    refresh_delegation_from_child_locked(&mut inner, d);
                    assert_eq!(inner.delegations[d].status, DelegationStatus::Failed);
                    let prompt_id = inner.next_message_id();
                    queue_prompt_on_record(
                        &mut inner.sessions[i],
                        PendingPrompt {
                            id: prompt_id.clone(),
                            timestamp: stamp_now(),
                            text: "Queued follow-up".to_owned(),
                            expanded_text: None,
                            attachments: Vec::new(),
                            source: None,
                            engram_interrupted: false,
                            is_engram_retained: false,
                        },
                        Vec::new(),
                    );
                    inner.delegations[d].queued_followup_prompt_id = Some(prompt_id);
                    rearm_terminal_delegation_for_followup_locked(&mut inner, d).unwrap();
                    assert!(delegation_followup_awaits_first_turn(
                        &inner,
                        &inner.delegations[d]
                    ));
                    assert_eq!(
                        inner.delegations[d].review_result_submission_attempt,
                        original_attempt + 1
                    );
                    // Prevent unrelated queue promotion while testing this old duty.
                    inner.sessions[i].set_auto_dispatch_blocked(true);
                }
                _ => unreachable!(),
            }
        },
        || fail_codex_reviewer_mcp(&fixture.state, &scope, "original-generation duty"),
    );
    let inner = fixture.state.inner.lock().unwrap();
    let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
    let d = &inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()];
    if matches!(gap, "generation" | "runtime") {
        assert_eq!(
            child.session.status,
            SessionStatus::Active,
            "successor owner must stay untouched"
        );
        assert_eq!(d.status, DelegationStatus::Running);
    } else if gap == "attempt" {
        assert_eq!(d.review_result_submission_attempt, scope.attempt + 1);
        assert!(
            d.result.is_none(),
            "old duty cannot publish into the queued next attempt"
        );
        assert_eq!(d.status, DelegationStatus::Running);
        assert!(d.attempt.reviewer_mcp_observations.is_empty());
    } else if gap == "submission" {
        // Terminal promotion clears the provisional slot by design. Compare
        // the full payload at its terminal destination, not the cleared slot.
        assert!(d.submitted_review_result.is_none());
        assert_eq!(
            d.result.as_ref(),
            submitted_before.lock().unwrap().as_ref(),
            "terminal promotion must preserve the entire authoritative payload"
        );
        assert_eq!(d.status, DelegationStatus::Completed);
        assert_eq!(
            d.review_result_schema_version,
            Some(DELEGATION_REVIEW_RESULT_SCHEMA_VERSION)
        );
        assert!(
            d.post_submission_transport_error
                .as_deref()
                .is_some_and(|detail| detail.contains("original-generation duty")),
            "later failure belongs in separate transport metadata"
        );
    } else {
        assert_eq!(
            child.session.status,
            SessionStatus::Error,
            "end only the original runtime generation"
        );
        assert_eq!(child.active_turn_generation, scope.generation);
        assert!(d.attempt.reviewer_mcp_observations.is_empty());
        if gap == "reset" {
            assert_eq!(d.status, DelegationStatus::Failed);
            assert!(d.result.is_some(), "publish the original generation's terminal result under reset");
        }
    }
}

#[test]
fn codex_reviewer_mcp_gap_thread_clear_ends_original_generation() {
    handoff_gap("thread-clear");
}

#[test]
fn codex_reviewer_mcp_gap_thread_replace_ends_original_generation() {
    handoff_gap("thread-replace");
}

#[test]
fn codex_reviewer_mcp_gap_reset_ends_original_generation() {
    handoff_gap("reset");
}

#[test]
fn codex_reviewer_mcp_gap_generation_keeps_successor_inert() {
    handoff_gap("generation");
}

#[test]
fn codex_reviewer_mcp_gap_runtime_keeps_successor_inert() {
    handoff_gap("runtime");
}

#[test]
fn codex_reviewer_mcp_gap_submission_keeps_authoritative_result() {
    handoff_gap("submission");
}

#[test]
fn codex_reviewer_mcp_gap_attempt_keeps_queued_followup_inert() {
    handoff_gap("attempt");
}
