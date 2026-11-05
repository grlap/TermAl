// Provider event adapters for the disposable evaluation matrix. No provider
// binary is started: production command handlers receive its real wire shape.
use super::*;

pub(super) enum Provider {
    Codex,
    Claude(mpsc::Receiver<ClaudeRuntimeCommand>),
}

impl Provider {
    pub(super) fn install(fixture: &CompletionFixture, claude: bool) -> (String, Self) {
        if !claude {
            return (fixture.live.session_id.clone(), Self::Codex);
        }
        let holder = create_test_project_session(
            &fixture.live.state,
            Agent::Claude,
            &fixture.project_id,
            &fixture.root,
        );
        let (runtime, input) = test_claude_runtime_handle("evaluation-matrix-claude");
        {
            let mut inner = fixture.live.state.inner.lock().unwrap();
            let index = inner.find_session_index(&holder).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            record.runtime = SessionRuntime::Claude(runtime);
            record.session.status = SessionStatus::Idle;
            record.engram.context_nudge_pending = false;
            fixture.live.state.commit_locked(&mut inner).unwrap();
        }
        (holder, Self::Claude(input))
    }

    pub(super) fn receive(&self, fixture: &CompletionFixture, holder: &str) {
        match self {
            Self::Codex => loop {
                if let CodexRuntimeCommand::Prompt { session_id, .. } = fixture
                    .live
                    .receiver
                    .recv_timeout(DEADLOCK_GUARD)
                    .expect("ordinary Codex provider handoff")
                {
                    if session_id != holder {
                        let inner = fixture.live.state.inner.lock().unwrap();
                        assert!(
                            inner
                                .delegations
                                .iter()
                                .any(|row| row.child_session_id == session_id
                                    && row.mode == DelegationMode::Evaluator),
                            "unexpected provider handoff"
                        );
                        continue;
                    }
                    break;
                }
            },
            Self::Claude(input) => loop {
                if let ClaudeRuntimeCommand::Prompt(_) = input
                    .recv_timeout(DEADLOCK_GUARD)
                    .expect("ordinary Claude provider handoff")
                {
                    break;
                }
            },
        }
    }

    pub(super) fn events(
        &self,
        fixture: &CompletionFixture,
        holder: &str,
        key: &str,
        codex_pushd: bool,
    ) -> CommandEvents {
        let root = normalize_user_facing_path(&fixture.root)
            .to_string_lossy()
            .replace('\\', "/");
        let claude = matches!(self, Self::Claude(_));
        let line = if claude || codex_pushd {
            format!("pushd \"{root}\" && {LAUNCH}")
        } else {
            LAUNCH.to_owned()
        };
        CommandEvents {
            state: fixture.live.state.clone(),
            holder: holder.to_owned(),
            recorder: SessionRecorder::new(fixture.live.state.clone(), holder.to_owned()),
            key: key.to_owned(),
            root,
            line,
            claude,
            reported_cwd: true,
            codex: CodexTurnState::default(),
            claude_state: ClaudeTurnState::default(),
        }
    }
}

pub(super) struct CommandEvents {
    state: AppState,
    holder: String,
    recorder: SessionRecorder,
    key: String,
    root: String,
    line: String,
    claude: bool,
    reported_cwd: bool,
    codex: CodexTurnState,
    claude_state: ClaudeTurnState,
}

impl CommandEvents {
    pub(super) fn command_line(&mut self, line: &str) {
        self.line = line.to_owned();
    }

    pub(super) fn powershell_one_call_without_reported_cwd(&mut self) {
        assert!(!self.claude);
        self.line = format!("pwsh.exe -Command 'pushd \"{}\" && {LAUNCH}'", self.root);
        // The retained command history has this wrapper, but no outer tool
        // cwd. Do not turn the fixture's known execution root into telemetry
        // the historical record did not supply.
        self.reported_cwd = false;
        assert!(
            engram_check_command(&self.line).is_none(),
            "the current parser does not treat the nested one-call prefix as a simple check"
        );
    }

    fn codex_item(&self, status: &str, output: &str, exit: Option<i32>) -> Value {
        let mut item = json!({"type":"commandExecution", "id":self.key,
            "command":self.line, "status":status, "aggregatedOutput":output});
        if self.reported_cwd {
            item["cwd"] = json!(self.root);
        }
        if let Some(exit) = exit {
            item["exitCode"] = json!(exit);
        }
        item
    }

    pub(super) fn start(&mut self) {
        if self.claude {
            register_claude_tool_use(
                &json!({"type":"tool_use", "id":self.key,
                "name":"Bash", "input":{"command":self.line}}),
                &mut self.claude_state,
                &mut self.recorder,
            )
            .unwrap();
        } else {
            handle_codex_app_server_item_started(
                &self.codex_item("inProgress", "", None),
                &mut self.recorder,
            )
            .unwrap();
        }
    }

    pub(super) fn complete(&mut self, output: &str, exit: i32) {
        if self.claude {
            handle_claude_tool_result(
                &json!({"type":"user", "message":{"role":"user",
                "content":[{"type":"tool_result", "tool_use_id":self.key,
                    "is_error":exit != 0, "content":output}]},
                "tool_use_result":{"stdout":output,"stderr":"","interrupted":false}}),
                &mut self.claude_state,
                &mut self.recorder,
            )
            .unwrap();
        } else {
            handle_codex_app_server_item_completed(
                &self.codex_item("completed", output, Some(exit)),
                &self.state,
                &self.holder,
                &mut self.codex,
                &mut self.recorder,
            )
            .unwrap();
        }
    }
}
