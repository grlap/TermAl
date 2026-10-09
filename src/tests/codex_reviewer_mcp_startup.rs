//! Reviewer bridge checks at the shared app-server start and completion seams.
//! These are local transport witnesses, not provider/model execution evidence.

use super::delegation_support::{
    install_required_review_delegation, mark_delegation_as_unstructured_explorer,
    structured_review_request,
};
use super::mailboxes::mailbox_test_state;
use super::*;

const REVIEW_THREAD: &str = "reviewer-exact-thread";
const REVIEW_TURN: &str = "reviewer-exact-turn";

/// Waits for the event a step of these tests needs: the worker's next
/// command, a handler's return, a thread's finish. The only bound is the
/// fixtures' shared liveness guard (`TEST_PHASE_DEADLOCK_GUARD`), which turns a
/// lost or never-sent event into a failure. It is not a synchronization
/// point, and a merely slow host does not reach it.
#[track_caller]
fn await_event<T>(receiver: &mpsc::Receiver<T>, what: &str) -> T {
    receiver
        .recv_timeout(crate::TEST_PHASE_DEADLOCK_GUARD)
        .unwrap_or_else(|error| panic!("{what}: no event within the liveness guard ({error})"))
}

struct ReviewerFixture {
    state: AppState,
    runtime: SharedCodexRuntime,
    input_rx: mpsc::Receiver<CodexRuntimeCommand>,
    process: Arc<SharedChild>,
    pending: CodexPendingRequestMap,
    parent: String,
    delegation: String,
    child: String,
}

impl ReviewerFixture {
    fn new(label: &str) -> Self {
        let (state, _sender, parent) = mailbox_test_state();
        let (delegation, child) = install_required_review_delegation(&state, &parent);
        let (runtime, input_rx, process) = test_shared_codex_runtime(label);
        install_single_wire_codex_fixture(&state, Some(runtime.clone()));
        {
            let mut inner = state.inner.lock().unwrap();
            let cwd = state
                .test_temp_root
                .as_ref()
                .unwrap()
                .path()
                .to_string_lossy()
                .into_owned();
            let index = inner.find_session_index(&child).unwrap();
            let record = &mut inner.sessions[index];
            record.session.workdir = cwd.clone();
            record.session.status = SessionStatus::Active;
            record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                runtime_id: runtime.runtime_id.clone(),
                input_tx: runtime.input_tx.clone(),
                process: process.clone(),
                shared_session: Some(SharedCodexSessionHandle {
                    runtime: runtime.clone(),
                    session_id: child.clone(),
                }),
            });
            let index = inner.find_delegation_index(&delegation).unwrap();
            inner.delegations[index].cwd = cwd;
            state.commit_locked(&mut inner).unwrap();
        }
        state
            .set_external_session_id(&child, REVIEW_THREAD.to_owned())
            .unwrap();
        runtime.sessions.lock().unwrap().insert(
            child.clone(),
            SharedCodexSessionState {
                thread_id: Some(REVIEW_THREAD.to_owned()),
                turn_id: Some(REVIEW_TURN.to_owned()),
                active_turn_generation: Some(0),
                turn_started: true,
                ..SharedCodexSessionState::default()
            },
        );
        runtime
            .thread_sessions
            .lock()
            .unwrap()
            .insert(REVIEW_THREAD.to_owned(), child.clone());
        Self {
            state,
            runtime,
            input_rx,
            process,
            pending: Arc::new(Mutex::new(HashMap::new())),
            parent,
            delegation,
            child,
        }
    }

    fn start(&self) -> Vec<u8> {
        let mut writer = Vec::new();
        self.start_with(&mut writer);
        writer
    }

    fn start_with(&self, writer: &mut impl Write) {
        handle_shared_codex_start_turn(
            writer,
            &self.pending,
            &self.state,
            &self.runtime.runtime_id,
            &self.runtime.sessions,
            &self.runtime.thread_sessions,
            None,
            &self.child,
            REVIEW_THREAD,
            None,
            CodexPromptCommand {
                active_turn_generation: 0,
                approval_policy: CodexApprovalPolicy::Never,
                attachments: Vec::new(),
                cwd: self
                    .state
                    .test_temp_root
                    .as_ref()
                    .unwrap()
                    .path()
                    .to_string_lossy()
                    .into_owned(),
                model: "gpt-5.4".to_owned(),
                prompt: "Review the supplied source.".to_owned(),
                reasoning_effort: CodexReasoningEffort::Medium,
                service_tier: None,
                resume_thread_id: None,
                sandbox_mode: CodexSandboxMode::ReadOnly,
            },
        )
        .unwrap();
    }

    fn complete(&self) {
        self.complete_with(Value::Null);
    }

    fn complete_with(&self, error: Value) {
        // The actual reader handler must return before a status response is supplied.
        // Awaiting that response on this handler would deadlock its own JSON-RPC reader.
        let state = self.state.clone();
        let runtime = self.runtime.clone();
        let pending = self.pending.clone();
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = handle_shared_codex_app_server_message(
                &json!({ "method": "turn/completed", "params": {
                    "threadId": REVIEW_THREAD, "turn": { "id": REVIEW_TURN, "error": error }
                }}),
                &state,
                &runtime.runtime_id,
                &pending,
                &runtime.sessions,
                &runtime.thread_sessions,
                &runtime.input_tx,
            );
            let _ = done_tx.send(result);
        });
        await_event(
            &done_rx,
            "the JSON-RPC reader must not await its own status response",
        )
        .unwrap();
    }
}

mod controls {
    include!("codex_reviewer_mcp_controls.rs");
}

mod lifecycle {
    include!("codex_reviewer_mcp_lifecycle.rs");
}

mod regressions {
    include!("codex_reviewer_mcp_regressions.rs");
}

mod terminal_controls {
    include!("codex_reviewer_mcp_terminal_controls.rs");
}

mod settlement_controls {
    include!("codex_reviewer_mcp_settlement_controls.rs");
}

mod send_controls {
    include!("codex_reviewer_mcp_send_controls.rs");
}

pub(crate) fn run_settlement_gap_hook(gate: &str) {
    settlement_controls::run_gap_hook(gate);
}

impl Drop for ReviewerFixture {
    fn drop(&mut self) {
        fail_pending_codex_requests(&self.pending, "local fixture transport closed");
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

#[test]
fn codex_reviewer_mcp_injected_startup_budget_is_explicit() {
    let config = termal_delegation_mcp_codex_config_with_command(
        "termal-fixture",
        "review-child",
        "http://127.0.0.1:1",
    );
    assert_eq!(
        config["mcp_servers"][TERMAL_DELEGATION_MCP_SERVER_NAME]["startup_timeout_sec"],
        json!(60),
        "the injected bridge must explicitly carry the approved startup budget"
    );
    assert_eq!(
        config["mcp_servers"][TERMAL_DELEGATION_MCP_SERVER_NAME]["tool_timeout_sec"],
        json!(termal_delegation_mcp_codex_tool_timeout_secs()),
        "startup policy must not rewrite the existing tool-call budget"
    );
}

#[test]
fn codex_reviewer_mcp_start_observes_exact_thread_before_model_work() {
    let fixture = ReviewerFixture::new("reviewer-start-status");
    let written = fixture.start();
    let requests = String::from_utf8(written)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(
        !requests
            .iter()
            .any(|request| request["method"] == "turn/start"),
        "an unchecked reviewer must not start model work before exact-thread bridge readiness"
    );
    let request = requests
        .first()
        .expect("reviewer startup must query the bridge");
    assert_eq!(request["method"], "mcpServerStatus/list");
    assert_eq!(request["params"]["threadId"], REVIEW_THREAD);
    assert_eq!(
        request["params"]["serverName"],
        TERMAL_DELEGATION_MCP_SERVER_NAME
    );
    assert_eq!(request["params"]["detail"], "toolsAndAuthOnly");
    assert_eq!(
        fixture
            .state
            .get_delegation(&fixture.parent, &fixture.delegation)
            .unwrap()
            .delegation
            .status,
        DelegationStatus::Running
    );
}

#[test]
fn codex_reviewer_mcp_finish_without_submission_queries_before_archive() {
    let fixture = ReviewerFixture::new("reviewer-finish-status");
    fixture.complete();
    let command = await_event(
        &fixture.input_rx,
        "missing structured submission must trigger a bounded status observation",
    );
    let CodexRuntimeCommand::JsonRpcRequest {
        method,
        params,
        response_tx,
        ..
    } = command
    else {
        panic!("expected an exact-thread MCP status query before cleanup");
    };
    assert_eq!(
        method, "mcpServerStatus/list",
        "bridge evidence must precede thread/archive"
    );
    assert_eq!(params["threadId"], REVIEW_THREAD);
    assert_eq!(params["serverName"], TERMAL_DELEGATION_MCP_SERVER_NAME);
    response_tx
        .send(Ok(json!({ "data": [{
        "name": TERMAL_DELEGATION_MCP_SERVER_NAME, "authStatus": "unsupported",
        "runtimeStatus": "failed", "toolsError": "fixture bridge discovery failed",
        "tools": {}, "resources": [], "resourceTemplates": []
    }], "nextCursor": null })))
        .unwrap();
    let archive = await_event(
        &fixture.input_rx,
        "observed failure should finish and then archive the child",
    );
    let CodexRuntimeCommand::JsonRpcRequest {
        method,
        response_tx,
        ..
    } = archive
    else {
        panic!("expected child archive after observation");
    };
    assert_eq!(method, "thread/archive");
    response_tx.send(Ok(json!({}))).unwrap();
    let result = fixture
        .state
        .get_delegation_result(&fixture.parent, &fixture.delegation)
        .unwrap()
        .result;
    assert_eq!(result.status, DelegationStatus::Failed);
    let result_json = serde_json::to_string(&result).unwrap();
    assert!(
        result_json.contains("fixture bridge discovery failed"),
        "the failed result must retain the observed bounded reason"
    );
    let saved = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let child = saved
        .sessions
        .iter()
        .find(|record| record.session.id == fixture.child)
        .unwrap();
    assert!(
        serde_json::to_string(&child.session.messages)
            .unwrap()
            .contains("fixture bridge discovery failed"),
        "bridge diagnostics must be persisted in the child transcript before archive"
    );
}

#[test]
fn codex_reviewer_mcp_explorer_keeps_ordinary_turn_start() {
    let fixture = ReviewerFixture::new("explorer-start-control");
    mark_delegation_as_unstructured_explorer(&fixture.state, &fixture.delegation);
    let written = fixture.start();
    let requests = String::from_utf8(written)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["method"], "turn/start");
    assert_eq!(requests[0]["params"]["threadId"], REVIEW_THREAD);
    assert_eq!(
        requests[0]["params"]["input"][0]["text"],
        "Review the supplied source."
    );
}

#[test]
fn codex_reviewer_mcp_authoritative_submission_keeps_precedence() {
    let fixture = ReviewerFixture::new("reviewer-submitted-control");
    fixture
        .state
        .submit_delegation_review_result(&fixture.child, structured_review_request())
        .unwrap();
    fixture.complete();
    let command = await_event(
        &fixture.input_rx,
        "a durably submitted terminal review should archive normally",
    );
    let CodexRuntimeCommand::JsonRpcRequest {
        method,
        response_tx,
        ..
    } = command
    else {
        panic!("expected child archive");
    };
    assert_eq!(
        method, "thread/archive",
        "late bridge status must not override a valid submission"
    );
    response_tx.send(Ok(json!({}))).unwrap();
    let result = fixture
        .state
        .get_delegation_result(&fixture.parent, &fixture.delegation)
        .unwrap()
        .result;
    assert_eq!(result.status, DelegationStatus::Completed);
    assert_eq!(result.summary, "One medium issue found.");
    assert_eq!(result.findings.len(), 1);
    let saved = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let delegation = saved
        .delegations
        .iter()
        .find(|record| record.id == fixture.delegation)
        .unwrap();
    assert_eq!(delegation.result.as_ref().unwrap(), &result);
}
