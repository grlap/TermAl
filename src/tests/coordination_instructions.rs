//! Host mailbox teaching wire regressions, split from the agent protocol suites.
//! Owns bootstrap-only guidance; repository discovery and mailbox delivery stay elsewhere.
use super::*;

const HOST_HEADER: &str = "TermAl host guidance";
const TASK: &str = "Only the user's task";

fn guidance_session(agent: Agent) -> (AppState, String) {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(agent),
            name: Some("Host guidance wire test".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    (state, created.session_id)
}

fn claude_initialize_frame(state: &AppState, session_id: &str) -> Value {
    let mut bytes = Vec::new();
    write_claude_initialize(&mut bytes, state, session_id).unwrap();
    serde_json::from_slice(bytes.trim_ascii_end()).unwrap()
}

#[test]
fn mailbox_guidance_claude_initialize_teaches_cli_fallback() {
    let (state, session_id) = guidance_session(Agent::Claude);
    for _ in 0..2 {
        // A fresh initialize on respawn/resume must re-teach, without replacing
        // Claude's own base instructions or repository settings discovery.
        let frame = claude_initialize_frame(&state, &session_id);
        assert_eq!(frame["request"]["systemPrompt"], "");
        assert_eq!(
            frame["request"]["appendSystemPrompt"],
            render_termal_host_guidance()
        );
        assert_eq!(
            frame["request"]["appendSystemPrompt"]
                .as_str()
                .unwrap()
                .matches(TERMAL_MAILBOX_GUIDANCE)
                .count(),
            1
        );
    }
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(&session_id).unwrap();
    assert!(inner.sessions[index].session.prompt_history.is_empty());
}

fn set_guidance_ineligible(state: &AppState, session_id: &str, kind: &str) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(session_id).unwrap();
    let record = &mut inner.sessions[index];
    match kind {
        "child" => record.session.parent_delegation_id = Some("delegation-test".to_owned()),
        "hidden" => record.hidden = true,
        "remote" => {
            record.remote_id = Some("remote-test".to_owned());
            record.remote_session_id = Some("remote-session".to_owned());
        }
        "invalid-remote" => record.remote_id = Some("remote-test".to_owned()),
        _ => panic!("unknown eligibility case"),
    }
}

#[test]
fn mailbox_guidance_claude_excludes_nonroots_and_missing_sessions() {
    for kind in ["child", "hidden", "remote", "invalid-remote"] {
        let (state, session_id) = guidance_session(Agent::Claude);
        set_guidance_ineligible(&state, &session_id, kind);
        let frame = claude_initialize_frame(&state, &session_id);
        assert_eq!(frame["request"]["appendSystemPrompt"], "", "{kind}");
    }
    assert_eq!(
        claude_initialize_frame(&test_app_state(), "missing-session")["request"]["appendSystemPrompt"],
        ""
    );
}

struct AcpGuidanceFixture {
    state: AppState,
    session_id: String,
    agent: AcpAgent,
    runtime_state: Arc<Mutex<AcpRuntimeState>>,
    pending: AcpPendingRequestMap,
    lifecycle: AcpTurnLifecycle,
    resume_session_id: Option<String>,
}

impl AcpGuidanceFixture {
    fn new(agent: AcpAgent, session_agent: Agent) -> Self {
        let (state, session_id) = guidance_session(session_agent);
        let (runtime, _input_rx) = test_acp_runtime_handle(agent, "host-guidance-runtime");
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(&session_id).unwrap();
            inner.sessions[index].runtime = SessionRuntime::Acp(runtime);
            // Pin preservation against nonempty user data, not only an empty
            // transcript. Wire teaching must not become prompt-history text.
            inner.sessions[index]
                .session
                .prompt_history
                .push(TASK.to_owned());
        }
        Self {
            state,
            session_id,
            agent,
            runtime_state: Self::ready_runtime(),
            pending: Arc::new(Mutex::new(HashMap::new())),
            lifecycle: Arc::new((Mutex::new(false), Condvar::new())),
            resume_session_id: None,
        }
    }

    fn ready_runtime() -> Arc<Mutex<AcpRuntimeState>> {
        // The same state a new runtime has after session/new or load/resume.
        Arc::new(Mutex::new(AcpRuntimeState {
            current_session_id: Some("external-host-guidance".to_owned()),
            ..Default::default()
        }))
    }

    fn write_prompt(&self, wire: &mut impl Write) -> Result<()> {
        handle_acp_prompt_command(
            wire,
            &self.pending,
            &self.state,
            &self.session_id,
            &self.runtime_state,
            &self.lifecycle,
            &RuntimeToken::Acp("host-guidance-runtime".to_owned()),
            None,
            self.agent,
            AcpPromptCommand {
                cwd: "/tmp".to_owned(),
                cursor_mode: None,
                model: "auto".to_owned(),
                opencode_effort: None,
                opencode_mode: None,
                prompt: TASK.to_owned(),
                resume_session_id: self.resume_session_id.clone(),
            },
        )
    }

    fn send(&self) -> Value {
        let before = self.user_data();
        let mut wire = Vec::new();
        self.write_prompt(&mut wire).unwrap();
        let (_, response) = take_pending_acp_request(&self.pending);
        response
            .send(Ok(json!({"stopReason": "end_turn"})))
            .unwrap();
        let guard = phase_sync::PollGuard::new();
        while *self
            .lifecycle
            .0
            .lock()
            .expect("ACP turn lifecycle mutex poisoned")
        {
            guard.wait("host guidance prompt response");
        }
        assert_eq!(
            self.user_data(),
            before,
            "wire guidance must not change stored user text"
        );
        let frame: Value = serde_json::from_slice(wire.trim_ascii_end()).unwrap();
        assert_eq!(frame["method"], "session/prompt");
        frame["params"]["prompt"].clone()
    }

    fn user_data(&self) -> Value {
        let inner = self.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&self.session_id).unwrap();
        let session = &inner.sessions[index].session;
        json!({"messages": session.messages, "history": session.prompt_history})
    }
}

fn assert_guided_prompt(blocks: &Value) {
    assert_eq!(
        blocks,
        &json!([
            {"type":"text", "text":TASK},
            {"type":"text", "text":render_termal_host_guidance()}
        ])
    );
    assert!(blocks[1]["text"].as_str().unwrap().starts_with(HOST_HEADER));
}

fn assert_acp_mailbox_guidance(agent: AcpAgent, session_agent: Agent) {
    let mut fixture = AcpGuidanceFixture::new(agent, session_agent);
    assert_guided_prompt(&fixture.send());
    assert_eq!(fixture.send(), json!([{"type":"text", "text":TASK}]));
    fixture.runtime_state = AcpGuidanceFixture::ready_runtime();
    assert_guided_prompt(&fixture.send());
}

#[test]
fn mailbox_guidance_gemini_first_wire_prompt_only() {
    assert_acp_mailbox_guidance(AcpAgent::Gemini, Agent::Gemini);
}

#[test]
fn mailbox_guidance_cursor_first_wire_prompt_only() {
    assert_acp_mailbox_guidance(AcpAgent::Cursor, Agent::Cursor);
}

#[test]
fn mailbox_guidance_opencode_first_wire_prompt_only() {
    assert_acp_mailbox_guidance(AcpAgent::OpenCode, Agent::OpenCode);
}

#[test]
fn mailbox_guidance_acp_external_resume_does_not_repeat_teaching() {
    for (agent, session_agent) in [
        (AcpAgent::Gemini, Agent::Gemini),
        (AcpAgent::Cursor, Agent::Cursor),
        (AcpAgent::OpenCode, Agent::OpenCode),
    ] {
        let mut fixture = AcpGuidanceFixture::new(agent, session_agent);
        fixture.resume_session_id = Some("external-host-guidance".to_owned());
        for _ in 0..2 {
            assert_eq!(fixture.send(), json!([{"type":"text", "text":TASK}]));
        }
        assert!(
            !fixture
                .runtime_state
                .lock()
                .expect("ACP runtime state mutex poisoned")
                .host_guidance_sent
        );
        // A new external conversation has no provider-owned teaching history.
        fixture.resume_session_id = None;
        fixture.runtime_state = AcpGuidanceFixture::ready_runtime();
        assert_guided_prompt(&fixture.send());
    }
}

#[test]
fn mailbox_guidance_acp_checks_live_root_ownership() {
    for (agent, session_agent) in [
        (AcpAgent::Gemini, Agent::Gemini),
        (AcpAgent::Cursor, Agent::Cursor),
        (AcpAgent::OpenCode, Agent::OpenCode),
    ] {
        for kind in ["child", "hidden", "remote", "invalid-remote"] {
            let fixture = AcpGuidanceFixture::new(agent, session_agent);
            set_guidance_ineligible(&fixture.state, &fixture.session_id, kind);
            assert_eq!(
                fixture.send(),
                json!([{"type":"text", "text":TASK}]),
                "{kind}"
            );
            assert!(
                !fixture
                    .runtime_state
                    .lock()
                    .expect("ACP runtime state mutex poisoned")
                    .host_guidance_sent
            );
        }
    }
}

struct FailedGuidanceFlush;
impl Write for FailedGuidanceFlush {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "fixture flush failed",
        ))
    }
}

#[test]
fn mailbox_guidance_acp_failed_wire_submission_does_not_consume_teaching() {
    for (agent, session_agent) in [
        (AcpAgent::Gemini, Agent::Gemini),
        (AcpAgent::Cursor, Agent::Cursor),
        (AcpAgent::OpenCode, Agent::OpenCode),
    ] {
        let fixture = AcpGuidanceFixture::new(agent, session_agent);
        let before = fixture.user_data();
        let error = fixture.write_prompt(&mut FailedGuidanceFlush).unwrap_err();
        assert!(format!("{error:#}").contains("fixture flush failed"));
        assert!(
            !fixture
                .runtime_state
                .lock()
                .expect("ACP runtime state mutex poisoned")
                .host_guidance_sent
        );
        assert!(
            fixture
                .pending
                .lock()
                .expect("ACP pending requests mutex poisoned")
                .is_empty()
        );
        assert_eq!(fixture.user_data(), before);
        assert_guided_prompt(&fixture.send());
    }
}

#[test]
fn mailbox_guidance_existing_surfaces_share_agent_neutral_body() {
    let section = termal_codex_agents_section();
    assert!(!section.contains("This Codex session"));
    assert_eq!(section.matches(TERMAL_MAILBOX_GUIDANCE).count(), 1);
    assert_eq!(
        coordination_cli_usage()
            .matches(TERMAL_MAILBOX_GUIDANCE)
            .count(),
        1
    );
    assert!(TERMAL_MAILBOX_GUIDANCE.len() <= 900);
    assert!(TERMAL_MAILBOX_GUIDANCE.lines().count() <= 12);
    for field in ["TERMAL_CLI", "TERMAL_SESSION_ID", "TERMAL_BASE_URL"] {
        assert!(TERMAL_MAILBOX_GUIDANCE.contains(field));
    }
    assert!(
        include_str!("../../docs/features/agent-mailboxes.md")
            .replace("\r\n", "\n")
            .contains(TERMAL_MAILBOX_GUIDANCE)
    );
}
