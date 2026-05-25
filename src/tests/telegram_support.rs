// Shared Telegram test fixtures split out of `telegram.rs`. This module owns
// fake Telegram senders/readers/clients and common Telegram config/message
// builders used by the focused Telegram test modules.
//
// It deliberately does not own test cases; behavior coverage stays in
// `telegram.rs`, `telegram_forwarding.rs`, `telegram_settings.rs`, and
// `telegram_relay_lifecycle.rs`.

use super::*;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

pub(super) static TELEGRAM_TEST_RATE_LIMIT_TEST_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

pub(super) struct FakeTelegramSender {
    pub(super) answered_callbacks: RefCell<Vec<(String, String)>>,
    pub(super) edited_messages: RefCell<Vec<(i64, i64, String, TelegramTextFormat)>>,
    pub(super) fail_edits: bool,
    pub(super) fail_first_attempts: Option<usize>,
    pub(super) fail_on_attempt: Option<usize>,
    pub(super) send_attempts: Cell<usize>,
    pub(super) sent_formats: RefCell<Vec<TelegramTextFormat>>,
    pub(super) sent_texts: RefCell<Vec<String>>,
}

impl FakeTelegramSender {
    pub(super) fn new(fail_on_attempt: Option<usize>) -> Self {
        Self {
            answered_callbacks: RefCell::new(Vec::new()),
            edited_messages: RefCell::new(Vec::new()),
            fail_edits: false,
            fail_first_attempts: None,
            fail_on_attempt,
            send_attempts: Cell::new(0),
            sent_formats: RefCell::new(Vec::new()),
            sent_texts: RefCell::new(Vec::new()),
        }
    }

    pub(super) fn with_edit_failure() -> Self {
        Self {
            fail_edits: true,
            ..Self::new(None)
        }
    }

    pub(super) fn failing_first_attempts(count: usize) -> Self {
        Self {
            fail_first_attempts: Some(count),
            ..Self::new(None)
        }
    }
}

impl TelegramMessageSender for FakeTelegramSender {
    fn send_message_with_format(
        &self,
        chat_id: i64,
        text: &str,
        _reply_markup: Option<&TelegramInlineKeyboardMarkup>,
        format: TelegramTextFormat,
    ) -> Result<TelegramChatMessage> {
        let attempt = self.send_attempts.get() + 1;
        self.send_attempts.set(attempt);
        if self.fail_on_attempt == Some(attempt)
            || self
                .fail_first_attempts
                .is_some_and(|attempts| attempt <= attempts)
        {
            bail!("forced send failure");
        }
        self.sent_formats.borrow_mut().push(format);
        self.sent_texts.borrow_mut().push(text.to_owned());
        Ok(TelegramChatMessage {
            message_id: attempt as i64,
            chat: TelegramChat {
                id: chat_id,
                _kind: "private".to_owned(),
            },
            text: Some(text.to_owned()),
        })
    }
}

impl TelegramDigestMessageSender for FakeTelegramSender {
    fn edit_message_with_format(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        _reply_markup: Option<&TelegramInlineKeyboardMarkup>,
        format: TelegramTextFormat,
    ) -> Result<i64> {
        if self.fail_edits {
            bail!("forced edit failure");
        }
        self.edited_messages
            .borrow_mut()
            .push((chat_id, message_id, text.to_owned(), format));
        Ok(message_id)
    }
}

impl TelegramCallbackResponder for FakeTelegramSender {
    fn answer_callback_query(&self, callback_query_id: &str, text: &str) -> Result<()> {
        self.answered_callbacks
            .borrow_mut()
            .push((callback_query_id.to_owned(), text.to_owned()));
        Ok(())
    }
}

pub(super) struct FakeTelegramSessionReader {
    pub(super) response: TelegramSessionFetchResponse,
}

impl TelegramSessionReader for FakeTelegramSessionReader {
    fn get_session(&self, _session_id: &str) -> Result<TelegramSessionFetchResponse> {
        Ok(self.response.clone())
    }
}

pub(super) struct FakeTelegramSessionReaderById {
    pub(super) responses: HashMap<String, TelegramSessionFetchResponse>,
}

impl TelegramSessionReader for FakeTelegramSessionReaderById {
    fn get_session(&self, session_id: &str) -> Result<TelegramSessionFetchResponse> {
        self.responses
            .get(session_id)
            .cloned()
            .with_context(|| format!("missing fake session `{session_id}`"))
    }
}

pub(super) struct RecordingTelegramSessionReaderById {
    pub(super) requests: RefCell<Vec<String>>,
    pub(super) responses: HashMap<String, TelegramSessionFetchResponse>,
}

impl TelegramSessionReader for RecordingTelegramSessionReaderById {
    fn get_session(&self, session_id: &str) -> Result<TelegramSessionFetchResponse> {
        self.requests.borrow_mut().push(session_id.to_owned());
        self.responses
            .get(session_id)
            .cloned()
            .with_context(|| format!("missing fake session `{session_id}`"))
    }
}

pub(super) struct FakeTelegramPromptClient {
    pub(super) digests: RefCell<VecDeque<std::result::Result<ProjectDigestResponse, String>>>,
    pub(super) digest_project_ids: RefCell<Vec<String>>,
    pub(super) events: RefCell<Vec<String>>,
    pub(super) session_responses: RefCell<VecDeque<TelegramSessionFetchResponse>>,
    pub(super) state_session_reads: Cell<usize>,
    pub(super) state_sessions: TelegramStateSessionsResponse,
    pub(super) send_error: Option<String>,
    pub(super) sent_prompts: RefCell<Vec<(String, String)>>,
}

impl FakeTelegramPromptClient {
    pub(super) fn new(
        digests: Vec<std::result::Result<ProjectDigestResponse, String>>,
        session_response: TelegramSessionFetchResponse,
    ) -> Self {
        Self {
            digests: RefCell::new(VecDeque::from(digests)),
            digest_project_ids: RefCell::new(Vec::new()),
            events: RefCell::new(Vec::new()),
            session_responses: RefCell::new(VecDeque::from([session_response])),
            state_session_reads: Cell::new(0),
            state_sessions: TelegramStateSessionsResponse {
                projects: Vec::new(),
                sessions: Vec::new(),
            },
            send_error: None,
            sent_prompts: RefCell::new(Vec::new()),
        }
    }

    pub(super) fn with_state_sessions(
        mut self,
        state_sessions: TelegramStateSessionsResponse,
    ) -> Self {
        self.state_sessions = state_sessions;
        self
    }

    pub(super) fn with_session_responses(
        mut self,
        responses: Vec<TelegramSessionFetchResponse>,
    ) -> Self {
        self.session_responses = RefCell::new(VecDeque::from(responses));
        self
    }

    pub(super) fn with_send_error(mut self, error: &str) -> Self {
        self.send_error = Some(error.to_owned());
        self
    }
}

impl TelegramSessionReader for FakeTelegramPromptClient {
    fn get_session(&self, session_id: &str) -> Result<TelegramSessionFetchResponse> {
        self.events
            .borrow_mut()
            .push(format!("session:{session_id}"));
        let mut responses = self.session_responses.borrow_mut();
        if responses.len() > 1 {
            responses
                .pop_front()
                .context("fake session response should be queued")
        } else {
            responses
                .front()
                .cloned()
                .context("fake session response should be queued")
        }
    }
}

impl TelegramPromptClient for FakeTelegramPromptClient {
    fn get_project_digest(&self, project_id: &str) -> Result<ProjectDigestResponse> {
        self.events
            .borrow_mut()
            .push(format!("digest:{project_id}"));
        self.digest_project_ids
            .borrow_mut()
            .push(project_id.to_owned());
        match self
            .digests
            .borrow_mut()
            .pop_front()
            .expect("fake digest response should be queued")
        {
            Ok(digest) => Ok(digest),
            Err(error) => bail!("{error}"),
        }
    }

    fn get_state_sessions(&self) -> Result<TelegramStateSessionsResponse> {
        self.events.borrow_mut().push("state-sessions".to_owned());
        self.state_session_reads
            .set(self.state_session_reads.get() + 1);
        Ok(self.state_sessions.clone())
    }

    fn send_session_message(&self, session_id: &str, text: &str) -> Result<()> {
        self.events.borrow_mut().push(format!("send:{session_id}"));
        if let Some(error) = self.send_error.as_deref() {
            bail!("{error}");
        }
        self.sent_prompts
            .borrow_mut()
            .push((session_id.to_owned(), text.to_owned()));
        Ok(())
    }
}

impl TelegramActionClient for FakeTelegramPromptClient {
    fn dispatch_project_action(
        &self,
        _project_id: &str,
        _action_id: &str,
    ) -> Result<ProjectDigestResponse> {
        bail!("unexpected fake action dispatch")
    }
}

pub(super) struct FakeTelegramActionClient {
    pub(super) dispatches: RefCell<Vec<(String, String)>>,
    pub(super) result: std::result::Result<ProjectDigestResponse, String>,
}

impl FakeTelegramActionClient {
    pub(super) fn succeeded(digest: ProjectDigestResponse) -> Self {
        Self {
            dispatches: RefCell::new(Vec::new()),
            result: Ok(digest),
        }
    }

    pub(super) fn failed(error: &str) -> Self {
        Self {
            dispatches: RefCell::new(Vec::new()),
            result: Err(error.to_owned()),
        }
    }
}

impl TelegramActionClient for FakeTelegramActionClient {
    fn dispatch_project_action(
        &self,
        project_id: &str,
        action_id: &str,
    ) -> Result<ProjectDigestResponse> {
        self.dispatches
            .borrow_mut()
            .push((project_id.to_owned(), action_id.to_owned()));
        match &self.result {
            Ok(digest) => Ok(digest.clone()),
            Err(error) => bail!("{error}"),
        }
    }
}

pub(super) fn telegram_project_digest(primary_session_id: Option<&str>) -> ProjectDigestResponse {
    ProjectDigestResponse {
        project_id: "project-1".to_owned(),
        headline: "termal".to_owned(),
        done_summary: "Working.".to_owned(),
        current_status: "Agent is working.".to_owned(),
        primary_session_id: primary_session_id.map(str::to_owned),
        proposed_actions: vec![],
        deep_link: None,
        source_message_ids: vec![],
    }
}

pub(super) struct TelegramTestConfig {
    pub(super) config: TelegramBotConfig,
}

impl std::ops::Deref for TelegramTestConfig {
    type Target = TelegramBotConfig;

    fn deref(&self) -> &Self::Target {
        &self.config
    }
}

impl std::ops::DerefMut for TelegramTestConfig {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.config
    }
}

impl Drop for TelegramTestConfig {
    fn drop(&mut self) {
        fs::remove_file(&self.config.state_path).ok();
    }
}

pub(super) fn telegram_test_config() -> TelegramTestConfig {
    TelegramTestConfig {
        config: TelegramBotConfig {
            bot_username: Some("termal_bot".to_owned()),
            chat_id: Some(42),
            forward_assistant_replies: true,
            project_id: "project-1".to_owned(),
            public_base_url: None,
            state_path: std::env::temp_dir()
                .join(format!("termal-telegram-{}.json", Uuid::new_v4())),
            subscribed_project_ids: vec!["project-1".to_owned()],
        },
    }
}

pub(super) fn telegram_text_message(
    chat_id: i64,
    message_id: i64,
    text: &str,
) -> TelegramChatMessage {
    TelegramChatMessage {
        message_id,
        chat: TelegramChat {
            id: chat_id,
            _kind: "private".to_owned(),
        },
        text: Some(text.to_owned()),
    }
}

pub(super) fn create_telegram_settings_project_and_session(state: &AppState) -> (String, String) {
    let root = std::env::temp_dir().join(format!("termal-telegram-project-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).expect("project root should exist");
    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Telegram Project".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .expect("project should create");
    let session = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Telegram Session".to_owned()),
            workdir: None,
            project_id: Some(project.project_id.clone()),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should create");
    (project.project_id, session.session_id)
}
