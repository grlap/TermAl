//! The worst case of an Engram bind off the admission path, as before a
//! spawn: a recording transport and a test that the path spends only the
//! phases `engram_unqueued_bind_worst_case` counts, each within the timeout
//! it counts for it.
//!
//! Owns that test and its recorder. Does not own the bind path or its
//! worst case (`engram_host_adapter.rs`), the bridge allowances derived from
//! it (`delegation_mcp_timeouts.rs`), or the other adapter tests
//! (`engram_host_adapter.rs` in tests). New module beside the adapter tests,
//! created instead of growing them.

use super::*;

/// Records each Engram call and work-binding read a bind makes, with the
/// timeout it is given, and answers from the scripted transport it wraps.
struct BindPhaseRecorder {
    inner: Arc<ScriptedEngramControlTransport>,
    phases: Mutex<Vec<(String, Duration)>>,
}

impl BindPhaseRecorder {
    fn record(&self, phase: &str, timeout: Duration) {
        self.phases
            .lock()
            .expect("bind phase mutex poisoned")
            .push((phase.to_owned(), timeout));
    }

    fn phases(&self) -> Vec<(String, Duration)> {
        self.phases
            .lock()
            .expect("bind phase mutex poisoned")
            .clone()
    }
}

impl EngramControlTransport for BindPhaseRecorder {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        let request_json = serde_json::to_value(request).expect("Engram request should serialize");
        let operation = request_json["operation"]
            .as_str()
            .expect("operation should be present");
        self.record(operation, timeout);
        self.inner.request(connection, request, timeout)
    }

    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.record("work_binding_read", timeout);
        self.inner
            .read_work_binding(connection, preference, timeout)
    }

    fn shutdown_session(&self, session_id: &str) {
        self.inner.shutdown_session(session_id);
    }
}

#[test]
fn an_unqueued_rebind_spends_only_the_phases_its_worst_case_counts() {
    // The longest bind off the admission path, as before a spawn: a rebind
    // that recovers an open grant (status, then checkpoint), a stale-fence
    // refusal, and the retry. Each timed phase it makes, with the timeout it
    // is given, is one `engram_unqueued_bind_worst_case` counts.
    let (state, _runtime_rx) =
        test_app_state_with_delegation_codex_runtime("engram-unqueued-bind-worst-case");
    // The timeouts below are budgets taken on the scripted clock, which no
    // scheduling delay can shrink: each is compared with its bound as given.
    select_scripted_engram_budget_clock_before_enable(&state);
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("engram-unqueued-bind-worst-case-project");
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Engram unqueued bind worst case");
    let parent_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    enable_test_project_engram(&state, &project_id, &root);
    state.install_control_test_transport(ScriptedEngramControlTransport::new([
        bind_reply("parent-token"),
        bind_reply("child-token"),
        grant_reply("open-grant"),
        begin_reply("open-grant"),
    ]));
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Open a turn to recover.".to_owned(),
                title: Some("Engram unqueued bind".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should start");
    let child_id = created.delegation.child_session_id;
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&child_id)
            .expect("child should exist");
        inner
            .session_mut_by_index(index)
            .expect("child should be mutable")
            .engram
            .rebind_required = true;
    }
    let recorder = Arc::new(BindPhaseRecorder {
        inner: ScriptedEngramControlTransport::new([
            ScriptedEngramControlResponse::Reply(Ok(json!({
                "phase": "turn_open",
                "open_grant_id": "open-grant"
            }))),
            checkpoint_reply("open-grant"),
            remote_error_reply("stale_fence"),
            rebind_reply("rebound"),
        ]),
        phases: Mutex::new(Vec::new()),
    });
    state.install_control_test_transport(recorder.clone());
    state
        .ensure_engram_session_bound_off_lock(&child_id)
        .expect("the retried rebind should bind")
        .expect("child should remain in Engram scope");

    let phases = recorder.phases();
    let operations = phases
        .iter()
        .map(|(operation, _)| operation.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        operations,
        [
            "session_status",
            "turn_checkpoint",
            "work_binding_read",
            "session_bind",
            "work_binding_read",
            "session_bind",
        ]
    );
    let dispatch = Duration::from_millis(ENGRAM_DISPATCH_BUDGET_MS);
    let call = Duration::from_millis(ENGRAM_MAX_CALL_TIMEOUT_MS);
    for (operation, timeout) in &phases {
        let bound = match operation.as_str() {
            // The recovery's two calls share one dispatch budget.
            "session_status" | "turn_checkpoint" => dispatch,
            "work_binding_read" => ENGRAM_WORK_BINDING_COMMAND_TIMEOUT,
            _ => call,
        };
        assert!(*timeout <= bound, "{operation}: {timeout:?} > {bound:?}");
    }
    // One recovery budget, and two attempts of a read, its lock-retry delay
    // and a bind; a session with nothing to recover spends the attempts only.
    let attempt = ENGRAM_WORK_BINDING_COMMAND_TIMEOUT + ENGRAM_WORK_BINDING_LOCK_RETRY_DELAY + call;
    assert_eq!(
        engram_unqueued_bind_worst_case(true),
        dispatch + attempt * 2
    );
    assert_eq!(engram_unqueued_bind_worst_case(false), attempt * 2);
}
