//! End-to-end evidence and delivery witnesses using the existing run index,
//! registration, turn completion and runtime handoff. No mailbox wake drives
//! these tests. Clock-boundary classifier tests belong with the index tests.

use super::*;

fn heartbeat(at: chrono::DateTime<chrono::Utc>) -> Value {
    json!({ "at": at.to_rfc3339(), "everyMs": 600_000 })
}

fn unpublished(beat: Option<Value>) -> Value {
    let mut results = json!({ "state": "running", "stages": [
        { "name": "rust-tests", "state": "running" }
    ] });
    if let Some(beat) = beat {
        results["heartbeat"] = beat;
    }
    results
}

fn summary(fixture: &WaitFixture, run: &str) -> TestRunSummary {
    fixture.state.test_run_summaries(None).into_iter()
        .find(|summary| summary.run_id == run).expect("run should be indexed")
}

fn assert_pending(fixture: &WaitFixture, response: &TestRunWaitResponse) {
    assert!(!response.resume_prompt_queued,
        "uncertain liveness must not queue UNKNOWN/recover: {:?}", fixture.queued_prompts());
    assert_eq!(fixture.pending_waits().len(), 1);
    assert!(fixture.queued_prompts().is_empty());
    fixture.scan(&[]);
    assert_eq!(fixture.pending_waits().len(), 1, "another scan is not death evidence");
    assert!(fixture.queued_prompts().is_empty(), "no recover on absence alone");
}

fn fresh_then_terminal(success: bool) {
    let fixture = WaitFixture::new(if success { "fresh-pass" } else { "fresh-fail" });
    let run = "test-unpublished";
    let directory = fixture.write(run, Some(unpublished(Some(heartbeat(chrono::Utc::now())))));
    fixture.scan(&[]);
    fixture.busy();
    let response = fixture.register(&[run], "all").unwrap();
    assert_pending(&fixture, &response);
    assert_eq!(summary(&fixture, run).state, TestRunState::Running,
        "the index and wait must share fresh-heartbeat evidence");

    // A real published executor is indexed before its authoritative terminal result.
    let mut results = running(4242);
    results["runId"] = json!(run);
    fs::write(directory.join("results.json"), results.to_string()).unwrap();
    fixture.scan(&[4242]);
    assert_eq!(fixture.pending_waits().len(), 1);
    results = if success { passed() } else { failed("authoritative failure") };
    results["runId"] = json!(run);
    fs::write(directory.join("results.json"), results.to_string()).unwrap();
    fixture.scan(&[]);
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains(if success { "- Verdict: PASS" } else { "- Verdict: FAIL" }));
    assert!(!prompts[0].contains("recover"));
    assert!(fixture.pending_waits().is_empty());
    fixture.scan(&[]);
    assert_eq!(fixture.queued_prompts(), prompts, "settlement is consumed once");
}

#[test]
fn fresh_unpublished_executor_waits_for_actual_success_once() {
    fresh_then_terminal(true);
}

#[test]
fn fresh_unpublished_executor_waits_for_actual_failure_once() {
    fresh_then_terminal(false);
}

fn uncertain_without_executor(label: &str, beat: Option<Value>) {
    let fixture = WaitFixture::new(label);
    fixture.write("test-uncertain", Some(unpublished(beat)));
    fixture.scan(&[]);
    fixture.busy();
    let response = fixture.register(&["test-uncertain"], "all").unwrap();
    assert_pending(&fixture, &response);
    let observed = summary(&fixture, "test-uncertain");
    assert_eq!(observed.state, TestRunState::Unknown);
    assert!(observed.unknown_reason.is_some(), "uncertain liveness must remain visible");
}

#[test]
fn stale_heartbeat_without_executor_is_pending_not_dead() {
    uncertain_without_executor("stale-unpublished",
        Some(heartbeat(chrono::Utc::now() - chrono::Duration::days(1))));
}

#[test]
fn missing_heartbeat_without_executor_is_pending_not_dead() {
    uncertain_without_executor("missing-unpublished", None);
}

#[test]
fn malformed_heartbeat_without_executor_is_pending_not_dead() {
    uncertain_without_executor("malformed-unpublished",
        Some(json!({ "at": "not an instant", "everyMs": 600_000 })));
}

#[test]
fn far_future_heartbeat_without_executor_is_pending_not_dead() {
    uncertain_without_executor("future-unpublished",
        Some(heartbeat(chrono::Utc::now() + chrono::Duration::days(1))));
}

#[test]
fn proven_gone_executor_still_settles_unknown_once() {
    let fixture = WaitFixture::new("proven-gone-control");
    fixture.write("test-gone", Some(running(4242)));
    fixture.scan(&[4242]);
    fixture.busy();
    let response = fixture.register(&["test-gone"], "all").unwrap();
    assert!(!response.resume_prompt_queued);
    fixture.scan(&[]);
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("UNKNOWN (process gone)"));
    assert!(prompts[0].contains("test-launcher.mjs recover"));
    assert!(fixture.pending_waits().is_empty());
    fixture.scan(&[]);
    assert_eq!(fixture.queued_prompts(), prompts);
}

#[test]
fn alive_executor_with_stale_heartbeat_remains_pending() {
    let fixture = WaitFixture::new("alive-stale-control");
    let mut results = running(4242);
    results["heartbeat"] = heartbeat(chrono::Utc::now() - chrono::Duration::days(1));
    fixture.write("test-alive", Some(results));
    fixture.scan(&[4242]);
    fixture.busy();
    let response = fixture.register(&["test-alive"], "all").unwrap();
    assert!(!response.resume_prompt_queued);
    assert_eq!(summary(&fixture, "test-alive").state, TestRunState::Running);
    assert_eq!(fixture.pending_waits().len(), 1);
}

const DELIVERY_RUNTIME: &str = "run-wait-delivery-runtime";

fn runtime(fixture: &WaitFixture) -> mpsc::Receiver<ClaudeRuntimeCommand> {
    let (runtime, input) = test_claude_runtime_handle(DELIVERY_RUNTIME);
    let mut inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.session).unwrap();
    inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    input
}

fn complete(fixture: &WaitFixture) {
    fixture.state.finish_turn_ok_if_runtime_matches(&fixture.session,
        &RuntimeToken::Claude(DELIVERY_RUNTIME.to_owned())).unwrap();
}

fn receive(input: &mpsc::Receiver<ClaudeRuntimeCommand>) -> String {
    match input.try_recv() {
        Ok(ClaudeRuntimeCommand::Prompt(command)) => command.text,
        Ok(_) => panic!("next eligible boundary handed off a non-prompt command"),
        Err(error) => panic!("next eligible boundary did not hand off a prompt: {error}"),
    }
}

fn no_delivery(input: &mpsc::Receiver<ClaudeRuntimeCommand>) {
    assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "no unrelated or duplicate provider turn");
}

fn settled_delivery_fixture(label: &str) -> WaitFixture {
    let fixture = WaitFixture::new(label);
    fixture.write("test-done", Some(passed()));
    fixture.scan(&[]);
    fixture
}

#[test]
fn settled_registration_during_active_turn_delivers_at_its_completion() {
    let fixture = settled_delivery_fixture("active-delivery");
    let input = runtime(&fixture);
    fixture.busy();
    let response = fixture.register(&["test-done"], "all").unwrap();
    assert!(response.resume_prompt_queued);
    assert!(!response.resume_dispatch_requested);
    no_delivery(&input);
    complete(&fixture);
    let prompt = receive(&input);
    assert!(prompt.contains("`test-done`: PASS"), "{prompt}");
    assert!(fixture.queued_prompts().is_empty());
    fixture.state.refresh_test_run_waits();
    complete(&fixture);
    no_delivery(&input);
}

#[test]
fn settled_registration_on_idle_parent_delivers_without_another_wake() {
    let fixture = settled_delivery_fixture("idle-delivery");
    let input = runtime(&fixture);
    let response = fixture.register(&["test-done"], "all").unwrap();
    assert!(response.resume_prompt_queued);
    assert!(response.resume_dispatch_requested);
    let prompt = receive(&input);
    assert!(prompt.contains("`test-done`: PASS"), "{prompt}");
    fixture.state.refresh_test_run_waits();
    complete(&fixture);
    no_delivery(&input);
}

#[test]
fn settled_result_respects_older_prompt_fifo_and_then_delivers() {
    let fixture = settled_delivery_fixture("fifo-delivery");
    let input = runtime(&fixture);
    fixture.busy();
    let queued = fixture.state.dispatch_turn(&fixture.session, SendMessageRequest {
        text: "older user prompt".to_owned(), expanded_text: None,
        attachments: Vec::new(), source_session_id: None, source_mailbox: None,
    }).unwrap();
    assert!(matches!(queued, DispatchTurnResult::Queued));
    fixture.register(&["test-done"], "all").unwrap();
    no_delivery(&input);
    complete(&fixture);
    assert_eq!(receive(&input), "older user prompt");
    assert_eq!(fixture.queued_prompts().len(), 1);
    complete(&fixture);
    assert!(receive(&input).contains("`test-done`: PASS"));
    complete(&fixture);
    no_delivery(&input);
}

#[test]
fn settled_result_respects_stop_latch_until_explicit_resume() {
    let fixture = settled_delivery_fixture("stop-latch-delivery");
    fixture.busy();
    fixture.state.stop_session(&fixture.session).unwrap();
    let input = runtime(&fixture);
    let response = fixture.register(&["test-done"], "all").unwrap();
    assert!(response.resume_prompt_queued);
    assert!(!response.resume_dispatch_requested);
    no_delivery(&input);
    fixture.state.refresh_test_run_waits();
    no_delivery(&input);
    fixture.state.resume_session_queue(&fixture.session).unwrap();
    assert!(receive(&input).contains("`test-done`: PASS"));
    complete(&fixture);
    no_delivery(&input);
}
