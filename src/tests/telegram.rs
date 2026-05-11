// Tests for the Telegram relay adapter in `src/telegram.rs`.
//
// In production the Telegram bot is an optional remote-control surface for
// TermAl: it pushes periodic project digests (headline, status, proposed
// actions, deep link) to a linked chat and accepts slash commands like
// `/commit`, `/stop`, or `/status` that are mapped onto `ProjectActionId`
// values and dispatched against the same HTTP action API used by the desktop
// UI. These tests pin command routing, digest rendering, assistant forwarding
// cursors, Telegram/TermAl wire projections, settings persistence, validation,
// error classification, log sanitization, and prompt/message size guards.

use super::*;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

struct FakeTelegramSender {
    answered_callbacks: RefCell<Vec<(String, String)>>,
    edited_messages: RefCell<Vec<(i64, i64, String, TelegramTextFormat)>>,
    fail_edits: bool,
    fail_first_attempts: Option<usize>,
    fail_on_attempt: Option<usize>,
    send_attempts: Cell<usize>,
    sent_formats: RefCell<Vec<TelegramTextFormat>>,
    sent_texts: RefCell<Vec<String>>,
}

impl FakeTelegramSender {
    fn new(fail_on_attempt: Option<usize>) -> Self {
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

    fn with_edit_failure() -> Self {
        Self {
            fail_edits: true,
            ..Self::new(None)
        }
    }

    fn failing_first_attempts(count: usize) -> Self {
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

struct FakeTelegramSessionReader {
    response: TelegramSessionFetchResponse,
}

impl TelegramSessionReader for FakeTelegramSessionReader {
    fn get_session(&self, _session_id: &str) -> Result<TelegramSessionFetchResponse> {
        Ok(self.response.clone())
    }
}

struct FakeTelegramSessionReaderById {
    responses: HashMap<String, TelegramSessionFetchResponse>,
}

impl TelegramSessionReader for FakeTelegramSessionReaderById {
    fn get_session(&self, session_id: &str) -> Result<TelegramSessionFetchResponse> {
        self.responses
            .get(session_id)
            .cloned()
            .with_context(|| format!("missing fake session `{session_id}`"))
    }
}

struct FakeTelegramPromptClient {
    digests: RefCell<VecDeque<std::result::Result<ProjectDigestResponse, String>>>,
    digest_project_ids: RefCell<Vec<String>>,
    events: RefCell<Vec<String>>,
    session_response: TelegramSessionFetchResponse,
    state_session_reads: Cell<usize>,
    state_sessions: TelegramStateSessionsResponse,
    send_error: Option<String>,
    sent_prompts: RefCell<Vec<(String, String)>>,
}

impl FakeTelegramPromptClient {
    fn new(
        digests: Vec<std::result::Result<ProjectDigestResponse, String>>,
        session_response: TelegramSessionFetchResponse,
    ) -> Self {
        Self {
            digests: RefCell::new(VecDeque::from(digests)),
            digest_project_ids: RefCell::new(Vec::new()),
            events: RefCell::new(Vec::new()),
            session_response,
            state_session_reads: Cell::new(0),
            state_sessions: TelegramStateSessionsResponse {
                projects: Vec::new(),
                sessions: Vec::new(),
            },
            send_error: None,
            sent_prompts: RefCell::new(Vec::new()),
        }
    }

    fn with_state_sessions(mut self, state_sessions: TelegramStateSessionsResponse) -> Self {
        self.state_sessions = state_sessions;
        self
    }

    fn with_send_error(mut self, error: &str) -> Self {
        self.send_error = Some(error.to_owned());
        self
    }
}

impl TelegramSessionReader for FakeTelegramPromptClient {
    fn get_session(&self, session_id: &str) -> Result<TelegramSessionFetchResponse> {
        self.events
            .borrow_mut()
            .push(format!("session:{session_id}"));
        Ok(self.session_response.clone())
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

struct FakeTelegramActionClient {
    dispatches: RefCell<Vec<(String, String)>>,
    result: std::result::Result<ProjectDigestResponse, String>,
}

impl FakeTelegramActionClient {
    fn succeeded(digest: ProjectDigestResponse) -> Self {
        Self {
            dispatches: RefCell::new(Vec::new()),
            result: Ok(digest),
        }
    }

    fn failed(error: &str) -> Self {
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

fn telegram_project_digest(primary_session_id: Option<&str>) -> ProjectDigestResponse {
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

struct TelegramTestConfig {
    config: TelegramBotConfig,
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

fn telegram_test_config() -> TelegramTestConfig {
    TelegramTestConfig {
        config: TelegramBotConfig {
            api_base_url: "http://127.0.0.1:8765".to_owned(),
            bot_username: Some("termal_bot".to_owned()),
            bot_token: "123456:TESTTOKEN".to_owned(),
            chat_id: Some(42),
            poll_timeout_secs: 1,
            project_id: "project-1".to_owned(),
            public_base_url: None,
            state_path: std::env::temp_dir()
                .join(format!("termal-telegram-{}.json", Uuid::new_v4())),
            subscribed_project_ids: vec!["project-1".to_owned()],
        },
    }
}

fn create_telegram_settings_project_and_session(state: &AppState) -> (String, String) {
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

// Pins that the slash-command parser only accepts Telegram's `@botname`
// mention suffix when it matches the relay bot username, and preserves
// trailing free-form args. Without the suffix check, group-chat messages
// like `/stop@other_bot` could trigger TermAl actions accidentally.
#[test]
fn telegram_command_parser_supports_suffixes_and_aliases() {
    let parsed =
        parse_telegram_command_for_bot("/commit@termal_bot   now please", Some("termal_bot"))
            .expect("command should parse");
    assert_eq!(
        parsed.command,
        TelegramIncomingCommand::Action(ProjectActionId::AskAgentToCommit)
    );
    assert_eq!(parsed.args, "now please");

    let parsed = parse_telegram_command_for_bot("/commit now please", Some("termal_bot"))
        .expect("private-chat command should parse without suffix");
    assert_eq!(
        parsed.command,
        TelegramIncomingCommand::Action(ProjectActionId::AskAgentToCommit)
    );
    assert_eq!(parsed.args, "now please");

    let parsed = parse_telegram_command("/status").expect("status should parse");
    assert_eq!(parsed.command, TelegramIncomingCommand::Status);

    let parsed = parse_telegram_command("/projects").expect("projects should parse");
    assert_eq!(parsed.command, TelegramIncomingCommand::Projects);

    let parsed = parse_telegram_command("/project project-2").expect("project should parse");
    assert_eq!(parsed.command, TelegramIncomingCommand::Project);
    assert_eq!(parsed.args, "project-2");

    let parsed = parse_telegram_command("/sessions").expect("sessions should parse");
    assert_eq!(parsed.command, TelegramIncomingCommand::Sessions);

    let parsed = parse_telegram_command("/session session-2").expect("session should parse");
    assert_eq!(parsed.command, TelegramIncomingCommand::Session);
    assert_eq!(parsed.args, "session-2");

    assert!(parse_telegram_command("/commit@termal_bot now please").is_none());
    assert!(
        parse_telegram_command_for_bot("/commit@other_bot now please", Some("termal_bot"))
            .is_none()
    );
    assert!(telegram_command_mentions_other_bot(
        "/commit@other_bot now please",
        Some("termal_bot")
    ));
    assert!(!telegram_command_mentions_other_bot(
        "/commit@termal_bot now please",
        Some("termal_bot")
    ));
}

// Pins that unknown slash commands return `None` rather than falling back
// to a default action. This guards against a regression where a typo such
// as `/commti` could be silently coerced into an action like
// `AskAgentToCommit`; the bot must instead reply with the help text so the
// user notices the mistake.
#[test]
fn telegram_command_parser_rejects_unknown_slash_commands() {
    assert!(parse_telegram_command("/unknown").is_none());
}

#[test]
fn telegram_action_error_text_uses_safe_detail_and_points_to_status() {
    let token = telegram_redaction_token();
    let err =
        anyhow!("failed to load C:\\Users\\grzeg\\.termal\\sessions\\abc.json; bot token={token}");

    let text = telegram_action_error_text(ProjectActionId::AskAgentToCommit, &err);

    assert!(text.contains("Could not run Ask Agent to Commit."));
    assert!(text.contains("Check TermAl for details"));
    assert!(text.contains("Send /status"));
    assert!(!text.contains("C:\\Users"));
    assert!(!text.contains("<redacted>"));
    assert!(!text.contains(&token));
}

#[test]
fn telegram_callback_action_failure_answers_and_sends_error_without_digest_refresh() {
    let telegram = FakeTelegramSender::new(None);
    let token = telegram_redaction_token();
    let termal =
        FakeTelegramActionClient::failed(&format!("action is unavailable; bot token={token}"));
    let config = telegram_test_config();
    let mut state = TelegramBotState {
        last_digest_hash: Some("previous-digest".to_owned()),
        last_digest_message_id: Some(99),
        ..TelegramBotState::default()
    };

    let changed = handle_telegram_callback_query(
        &telegram,
        &termal,
        &config,
        &mut state,
        TelegramCallbackQuery {
            id: "callback-1".to_owned(),
            data: Some(
                telegram_digest_callback_data("project-1", "ask-agent-to-commit")
                    .expect("callback data should fit"),
            ),
            message: Some(TelegramChatMessage {
                message_id: 123,
                chat: TelegramChat {
                    id: 42,
                    _kind: "private".to_owned(),
                },
                text: Some("Project digest".to_owned()),
            }),
        },
    )
    .expect("callback failure should be reported to the user");

    assert!(!changed);
    assert_eq!(
        termal.dispatches.borrow().as_slice(),
        &[("project-1".to_owned(), "ask-agent-to-commit".to_owned())]
    );
    assert_eq!(telegram.answered_callbacks.borrow().len(), 1);
    let (_, callback_text) = &telegram.answered_callbacks.borrow()[0];
    assert!(callback_text.contains("Ask Agent to Commit failed"));
    assert!(callback_text.contains("Check TermAl for details"));
    assert!(!callback_text.contains("action is unavailable"));
    assert!(!callback_text.contains("<redacted>"));
    assert!(!callback_text.contains(&token));

    let sent_texts = telegram.sent_texts.borrow();
    assert_eq!(sent_texts.len(), 1);
    assert!(sent_texts[0].contains("Could not run Ask Agent to Commit."));
    assert!(sent_texts[0].contains("Check TermAl for details"));
    assert!(sent_texts[0].contains("Send /status"));
    assert!(!sent_texts[0].contains("action is unavailable"));
    assert!(!sent_texts[0].contains("<redacted>"));
    assert!(!sent_texts[0].contains(&token));
    assert!(telegram.edited_messages.borrow().is_empty());
    assert_eq!(state.last_digest_hash.as_deref(), Some("previous-digest"));
    assert_eq!(state.last_digest_message_id, Some(99));
}

#[test]
fn telegram_callback_dispatches_against_digest_project_not_active_project() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramActionClient::succeeded(ProjectDigestResponse {
        project_id: "project-1".to_owned(),
        headline: "termal".to_owned(),
        done_summary: "Action dispatched.".to_owned(),
        current_status: "Ready.".to_owned(),
        primary_session_id: Some("session-1".to_owned()),
        proposed_actions: vec![],
        deep_link: None,
        source_message_ids: vec![],
    });
    let mut config = telegram_test_config();
    config.subscribed_project_ids = vec!["project-1".to_owned(), "project-2".to_owned()];
    let mut state = TelegramBotState {
        selected_project_id: Some("project-2".to_owned()),
        last_digest_hash: Some("project-2-digest".to_owned()),
        last_digest_message_id: Some(200),
        ..TelegramBotState::default()
    };

    let changed = handle_telegram_callback_query(
        &telegram,
        &termal,
        &config,
        &mut state,
        TelegramCallbackQuery {
            id: "callback-1".to_owned(),
            data: Some(
                telegram_digest_callback_data("project-1", "review-in-termal")
                    .expect("callback data should fit"),
            ),
            message: Some(TelegramChatMessage {
                message_id: 123,
                chat: TelegramChat {
                    id: 42,
                    _kind: "private".to_owned(),
                },
                text: Some("Project digest".to_owned()),
            }),
        },
    )
    .expect("callback should dispatch against its digest project");

    assert!(!changed);
    assert_eq!(
        termal.dispatches.borrow().as_slice(),
        &[("project-1".to_owned(), "review-in-termal".to_owned())]
    );
    assert_eq!(state.selected_project_id.as_deref(), Some("project-2"));
    assert_eq!(state.last_digest_hash.as_deref(), Some("project-2-digest"));
    assert_eq!(state.last_digest_message_id, Some(200));
    assert_eq!(telegram.answered_callbacks.borrow().len(), 1);
    assert_eq!(
        telegram.answered_callbacks.borrow()[0].1,
        "Review in TermAl"
    );
    assert_eq!(telegram.edited_messages.borrow().len(), 1);
}

#[test]
fn telegram_callback_does_not_send_untracked_fallback_for_non_active_digest_edit_failure() {
    let telegram = FakeTelegramSender::with_edit_failure();
    let termal = FakeTelegramActionClient::succeeded(ProjectDigestResponse {
        project_id: "project-1".to_owned(),
        headline: "termal".to_owned(),
        done_summary: "Action dispatched.".to_owned(),
        current_status: "Ready.".to_owned(),
        primary_session_id: Some("session-1".to_owned()),
        proposed_actions: vec![],
        deep_link: None,
        source_message_ids: vec![],
    });
    let mut config = telegram_test_config();
    config.subscribed_project_ids = vec!["project-1".to_owned(), "project-2".to_owned()];
    let mut state = TelegramBotState {
        selected_project_id: Some("project-2".to_owned()),
        last_digest_hash: Some("project-2-digest".to_owned()),
        last_digest_message_id: Some(200),
        ..TelegramBotState::default()
    };

    let changed = handle_telegram_callback_query(
        &telegram,
        &termal,
        &config,
        &mut state,
        TelegramCallbackQuery {
            id: "callback-1".to_owned(),
            data: Some(
                telegram_digest_callback_data("project-1", "review-in-termal")
                    .expect("callback data should fit"),
            ),
            message: Some(TelegramChatMessage {
                message_id: 123,
                chat: TelegramChat {
                    id: 42,
                    _kind: "private".to_owned(),
                },
                text: Some("Project digest".to_owned()),
            }),
        },
    )
    .expect("non-active callback edit failure should not fail dispatch");

    assert!(!changed);
    assert_eq!(
        termal.dispatches.borrow().as_slice(),
        &[("project-1".to_owned(), "review-in-termal".to_owned())]
    );
    assert_eq!(state.last_digest_hash.as_deref(), Some("project-2-digest"));
    assert_eq!(state.last_digest_message_id, Some(200));
    assert!(telegram.edited_messages.borrow().is_empty());
    assert!(telegram.sent_texts.borrow().is_empty());
}

#[test]
fn telegram_callback_resolves_default_project_even_when_not_in_subscribed_list() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramActionClient::succeeded(telegram_project_digest(Some("session-1")));
    let mut config = telegram_test_config();
    config.subscribed_project_ids = vec!["project-2".to_owned()];
    let mut state = TelegramBotState::default();

    let changed = handle_telegram_callback_query(
        &telegram,
        &termal,
        &config,
        &mut state,
        TelegramCallbackQuery {
            id: "callback-1".to_owned(),
            data: Some(
                telegram_digest_callback_data("project-1", "review-in-termal")
                    .expect("callback data should fit"),
            ),
            message: Some(TelegramChatMessage {
                message_id: 123,
                chat: TelegramChat {
                    id: 42,
                    _kind: "private".to_owned(),
                },
                text: Some("Project digest".to_owned()),
            }),
        },
    )
    .expect("default-project callback should dispatch");

    assert!(changed);
    assert_eq!(
        termal.dispatches.borrow().as_slice(),
        &[("project-1".to_owned(), "review-in-termal".to_owned())]
    );
}

#[test]
fn telegram_callback_rejects_legacy_unscoped_action_payload() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramActionClient::succeeded(telegram_project_digest(Some("session-1")));
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();

    let changed = handle_telegram_callback_query(
        &telegram,
        &termal,
        &config,
        &mut state,
        TelegramCallbackQuery {
            id: "callback-1".to_owned(),
            data: Some("review-in-termal".to_owned()),
            message: Some(TelegramChatMessage {
                message_id: 123,
                chat: TelegramChat {
                    id: 42,
                    _kind: "private".to_owned(),
                },
                text: Some("Project digest".to_owned()),
            }),
        },
    )
    .expect("legacy callback should be rejected without dispatching");

    assert!(!changed);
    assert!(termal.dispatches.borrow().is_empty());
    assert_eq!(telegram.answered_callbacks.borrow().len(), 1);
    assert_eq!(
        telegram.answered_callbacks.borrow()[0].1,
        "That action is from an older digest. Send /status to refresh."
    );
    assert!(telegram.edited_messages.borrow().is_empty());
}

#[test]
fn telegram_callback_rejects_project_tokens_that_are_no_longer_subscribed() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramActionClient::succeeded(telegram_project_digest(Some("session-1")));
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();

    let changed = handle_telegram_callback_query(
        &telegram,
        &termal,
        &config,
        &mut state,
        TelegramCallbackQuery {
            id: "callback-1".to_owned(),
            data: Some(
                telegram_digest_callback_data("removed-project", "review-in-termal")
                    .expect("callback data should fit"),
            ),
            message: Some(TelegramChatMessage {
                message_id: 123,
                chat: TelegramChat {
                    id: 42,
                    _kind: "private".to_owned(),
                },
                text: Some("Project digest".to_owned()),
            }),
        },
    )
    .expect("removed-project callback should be rejected without dispatching");

    assert!(!changed);
    assert!(termal.dispatches.borrow().is_empty());
    assert_eq!(telegram.answered_callbacks.borrow().len(), 1);
    assert_eq!(
        telegram.answered_callbacks.borrow()[0].1,
        "That project is no longer available to this relay."
    );
    assert!(telegram.edited_messages.borrow().is_empty());
}

#[test]
fn telegram_prompt_error_text_uses_safe_generic_detail() {
    let token = telegram_redaction_token();
    let err = anyhow!("failed to load /Users/me/.termal/session.json; bot token={token}");

    let text = telegram_prompt_error_text(&err);

    assert!(text.starts_with("Could not forward that message."));
    assert!(text.contains("Check TermAl for details"));
    assert!(!text.contains("/Users/me"));
    assert!(!text.contains("<redacted>"));
    assert!(!text.contains(&token));
}

#[test]
fn truncate_telegram_user_error_detail_respects_tiny_limits() {
    assert_eq!(truncate_telegram_user_error_detail("abcdef", 0), "");
    assert_eq!(truncate_telegram_user_error_detail("abcdef", 1), "a");
    assert_eq!(truncate_telegram_user_error_detail("abcdef", 2), "ab");
    assert_eq!(truncate_telegram_user_error_detail("abcdef", 3), "...");
    assert_eq!(truncate_telegram_user_error_detail("abcdef", 4), "a...");
}

#[test]
fn telegram_sessions_renderer_lists_active_project_sessions_first() {
    let state = TelegramStateSessionsResponse {
        projects: vec![TelegramStateProject {
            id: "project-1".to_owned(),
            name: "TermAl".to_owned(),
        }],
        sessions: vec![
            TelegramStateSession {
                id: "session-1".to_owned(),
                name: "Older".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 2,
            },
            TelegramStateSession {
                id: "session-other".to_owned(),
                name: "Other Project".to_owned(),
                project_id: Some("project-2".to_owned()),
                status: TelegramSessionStatus::Active,
                message_count: 1,
            },
            TelegramStateSession {
                id: "session-2".to_owned(),
                name: "Current".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Active,
                message_count: 7,
            },
            TelegramStateSession {
                id: "session-3".to_owned(),
                name: "Newer Idle".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 3,
            },
        ],
    };

    let text = render_telegram_project_sessions("project-1", &state);

    assert!(text.starts_with("Sessions for TermAl:\n- Current"));
    assert!(text.contains("id: session-2"));
    assert!(!text.contains("Working on the Telegram sessions list"));
    assert!(
        text.find("- Current")
            .expect("active session should render")
            < text
                .find("- Newer Idle")
                .expect("newer idle session should render")
    );
    assert!(text.contains("- Older (idle, 2 messages)"));
    assert!(!text.contains("Other Project"));
}

#[test]
fn telegram_sessions_renderer_reports_project_session_overflow() {
    let state = TelegramStateSessionsResponse {
        projects: vec![TelegramStateProject {
            id: "project-1".to_owned(),
            name: "TermAl".to_owned(),
        }],
        sessions: (0..13)
            .map(|index| TelegramStateSession {
                id: format!("session-{index}"),
                name: format!("Session {index}"),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: index,
            })
            .collect(),
    };

    let text = render_telegram_project_sessions("project-1", &state);

    assert!(text.contains("More sessions exist in TermAl."));
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with("- Session "))
            .count(),
        12
    );
    assert!(text.contains("- Session 12 (idle, 12 messages)"));
    assert!(text.contains("- Session 1 (idle, 1 message)"));
    assert!(!text.contains("- Session 0 (idle, 0 messages)"));
}

#[test]
fn telegram_state_sessions_response_decodes_statuses_as_enum() {
    let state: TelegramStateSessionsResponse = serde_json::from_value(serde_json::json!({
        "projects": [],
        "sessions": [
            {
                "id": "session-active",
                "name": "Active",
                "projectId": "project-1",
                "status": "active",
                "messageCount": 7
            },
            { "id": "session-future", "name": "Future", "status": "queued" }
        ]
    }))
    .expect("state projection should decode");

    assert_eq!(state.sessions[0].status, TelegramSessionStatus::Active);
    assert_eq!(state.sessions[0].project_id.as_deref(), Some("project-1"));
    assert_eq!(state.sessions[0].message_count, 7);
    assert_eq!(state.sessions[1].status, TelegramSessionStatus::Unknown);
    assert_eq!(
        telegram_state_session_status_label(&state.sessions[1].status),
        "unknown"
    );
}

#[test]
fn telegram_sessions_renderer_handles_empty_project() {
    let state = TelegramStateSessionsResponse {
        projects: Vec::new(),
        sessions: Vec::new(),
    };

    let text = render_telegram_project_sessions("project-1", &state);

    assert_eq!(
        text,
        "No sessions are attached to project `project-1` yet. Start one in TermAl first."
    );
}

#[test]
fn telegram_sessions_slash_command_reads_state_and_sends_rendered_list() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        Vec::new(),
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: Vec::new(),
            },
        },
    )
    .with_state_sessions(TelegramStateSessionsResponse {
        projects: vec![TelegramStateProject {
            id: "project-1".to_owned(),
            name: "TermAl".to_owned(),
        }],
        sessions: vec![TelegramStateSession {
            id: "session-1".to_owned(),
            name: "Current".to_owned(),
            project_id: Some("project-1".to_owned()),
            status: TelegramSessionStatus::Active,
            message_count: 3,
        }],
    });
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();

    let changed = handle_telegram_message(
        &telegram,
        &termal,
        &config,
        &mut state,
        TelegramChatMessage {
            message_id: 7,
            chat: TelegramChat {
                id: 42,
                _kind: "private".to_owned(),
            },
            text: Some("/sessions".to_owned()),
        },
    )
    .expect("sessions command should route through state projection");

    assert!(!changed);
    assert_eq!(termal.state_session_reads.get(), 1);
    let sent_texts = telegram.sent_texts.borrow();
    assert_eq!(sent_texts.len(), 1);
    assert!(sent_texts[0].contains("Sessions for TermAl:"));
    assert!(sent_texts[0].contains("- Current (active, 3 messages)"));
    assert!(sent_texts[0].contains("id: session-1"));
}

#[test]
fn telegram_relay_iteration_drains_updates_before_one_digest_sync() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![Ok(telegram_project_digest(Some("session-2")))],
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: Vec::new(),
            },
        },
    )
    .with_state_sessions(TelegramStateSessionsResponse {
        projects: vec![TelegramStateProject {
            id: "project-1".to_owned(),
            name: "TermAl".to_owned(),
        }],
        sessions: vec![TelegramStateSession {
            id: "session-2".to_owned(),
            name: "Selected".to_owned(),
            project_id: Some("project-1".to_owned()),
            status: TelegramSessionStatus::Idle,
            message_count: 0,
        }],
    });
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();
    let updates = vec![TelegramUpdate {
        update_id: 10,
        callback_query: None,
        message: Some(TelegramChatMessage {
            message_id: 7,
            chat: TelegramChat {
                id: 42,
                _kind: "private".to_owned(),
            },
            text: Some("/session session-2".to_owned()),
        }),
    }];

    let dirty = drain_telegram_updates_then_sync_digest(
        &telegram, &termal, &config, &mut state, updates, &None,
    );

    assert!(dirty);
    assert_eq!(state.next_update_id, Some(11));
    assert_eq!(state.selected_session_id.as_deref(), Some("session-2"));
    assert_eq!(
        termal.digest_project_ids.borrow().as_slice(),
        ["project-1".to_owned()]
    );
    let events = termal.events.borrow();
    let first_update_event = events
        .iter()
        .position(|event| event == "state-sessions")
        .expect("update handling should read sessions before selecting");
    let digest_event = events
        .iter()
        .position(|event| event == "digest:project-1")
        .expect("iteration should sync one digest after updates");
    assert!(first_update_event < digest_event, "{events:?}");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.as_str() == "digest:project-1")
            .count(),
        1
    );
}

#[test]
fn telegram_relay_iteration_skips_post_update_sync_after_prompt_refresh() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![
            Ok(telegram_project_digest(Some("session-1"))),
            Ok(telegram_project_digest(Some("session-1"))),
        ],
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "baseline".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Existing reply".to_owned(),
                }],
            },
        },
    );
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();
    let updates = vec![TelegramUpdate {
        update_id: 20,
        callback_query: None,
        message: Some(TelegramChatMessage {
            message_id: 8,
            chat: TelegramChat {
                id: 42,
                _kind: "private".to_owned(),
            },
            text: Some("from chat".to_owned()),
        }),
    }];

    let dirty = drain_telegram_updates_then_sync_digest(
        &telegram, &termal, &config, &mut state, updates, &None,
    );

    assert!(dirty);
    assert_eq!(state.next_update_id, Some(21));
    assert_eq!(
        termal.sent_prompts.borrow().as_slice(),
        [("session-1".to_owned(), "from chat".to_owned())]
    );
    assert_eq!(
        termal.digest_project_ids.borrow().as_slice(),
        ["project-1".to_owned(), "project-1".to_owned()]
    );
    let events = termal.events.borrow();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.as_str() == "digest:project-1")
            .count(),
        2
    );
}

#[test]
fn telegram_relay_iteration_runs_final_sync_after_status_digest() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![
            Ok(telegram_project_digest(Some("session-1"))),
            Ok(telegram_project_digest(Some("session-1"))),
        ],
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: Vec::new(),
            },
        },
    );
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();
    let updates = vec![TelegramUpdate {
        update_id: 30,
        callback_query: None,
        message: Some(TelegramChatMessage {
            message_id: 9,
            chat: TelegramChat {
                id: 42,
                _kind: "private".to_owned(),
            },
            text: Some("/status".to_owned()),
        }),
    }];

    let dirty = drain_telegram_updates_then_sync_digest(
        &telegram, &termal, &config, &mut state, updates, &None,
    );

    assert!(dirty);
    assert_eq!(state.next_update_id, Some(31));
    assert_eq!(
        termal.digest_project_ids.borrow().as_slice(),
        ["project-1".to_owned(), "project-1".to_owned()]
    );
    assert_eq!(
        termal
            .events
            .borrow()
            .iter()
            .filter(|event| event.as_str() == "digest:project-1")
            .count(),
        2
    );
}

#[test]
fn telegram_relay_iteration_resyncs_after_later_unsynced_update() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![
            Ok(telegram_project_digest(Some("session-1"))),
            Ok(telegram_project_digest(Some("session-2"))),
        ],
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: Vec::new(),
            },
        },
    )
    .with_state_sessions(TelegramStateSessionsResponse {
        projects: vec![TelegramStateProject {
            id: "project-1".to_owned(),
            name: "TermAl".to_owned(),
        }],
        sessions: vec![TelegramStateSession {
            id: "session-2".to_owned(),
            name: "Selected".to_owned(),
            project_id: Some("project-1".to_owned()),
            status: TelegramSessionStatus::Idle,
            message_count: 0,
        }],
    });
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();
    let updates = vec![
        TelegramUpdate {
            update_id: 40,
            callback_query: None,
            message: Some(TelegramChatMessage {
                message_id: 10,
                chat: TelegramChat {
                    id: 42,
                    _kind: "private".to_owned(),
                },
                text: Some("/status".to_owned()),
            }),
        },
        TelegramUpdate {
            update_id: 41,
            callback_query: None,
            message: Some(TelegramChatMessage {
                message_id: 11,
                chat: TelegramChat {
                    id: 42,
                    _kind: "private".to_owned(),
                },
                text: Some("/session session-2".to_owned()),
            }),
        },
    ];

    let dirty = drain_telegram_updates_then_sync_digest(
        &telegram, &termal, &config, &mut state, updates, &None,
    );

    assert!(dirty);
    assert_eq!(state.next_update_id, Some(42));
    assert_eq!(state.selected_session_id.as_deref(), Some("session-2"));
    assert_eq!(
        termal.digest_project_ids.borrow().as_slice(),
        ["project-1".to_owned(), "project-1".to_owned()]
    );
}

#[test]
fn telegram_relay_iteration_resyncs_after_later_update_error() {
    let telegram = FakeTelegramSender::new(Some(2));
    let termal = FakeTelegramPromptClient::new(
        vec![
            Ok(telegram_project_digest(Some("session-1"))),
            Ok(telegram_project_digest(Some("session-1"))),
            Ok(telegram_project_digest(Some("session-2"))),
        ],
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: Vec::new(),
            },
        },
    )
    .with_state_sessions(TelegramStateSessionsResponse {
        projects: vec![TelegramStateProject {
            id: "project-1".to_owned(),
            name: "TermAl".to_owned(),
        }],
        sessions: vec![TelegramStateSession {
            id: "session-2".to_owned(),
            name: "Selected".to_owned(),
            project_id: Some("project-1".to_owned()),
            status: TelegramSessionStatus::Idle,
            message_count: 0,
        }],
    });
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();
    let updates = vec![
        TelegramUpdate {
            update_id: 50,
            callback_query: None,
            message: Some(TelegramChatMessage {
                message_id: 12,
                chat: TelegramChat {
                    id: 42,
                    _kind: "private".to_owned(),
                },
                text: Some("from chat".to_owned()),
            }),
        },
        TelegramUpdate {
            update_id: 51,
            callback_query: None,
            message: Some(TelegramChatMessage {
                message_id: 13,
                chat: TelegramChat {
                    id: 42,
                    _kind: "private".to_owned(),
                },
                text: Some("/session session-2".to_owned()),
            }),
        },
    ];

    let dirty = drain_telegram_updates_then_sync_digest(
        &telegram, &termal, &config, &mut state, updates, &None,
    );

    assert!(dirty);
    assert_eq!(state.next_update_id, Some(52));
    assert_eq!(state.selected_session_id.as_deref(), Some("session-2"));
    assert_eq!(
        termal.digest_project_ids.borrow().as_slice(),
        [
            "project-1".to_owned(),
            "project-1".to_owned(),
            "project-1".to_owned()
        ]
    );
    assert_eq!(
        telegram.sent_texts.borrow().len(),
        1,
        "the /session acknowledgement should fail but final sync should still run"
    );
}

#[test]
fn telegram_sessions_command_chunks_oversized_output() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        Vec::new(),
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: Vec::new(),
            },
        },
    )
    .with_state_sessions(TelegramStateSessionsResponse {
        projects: vec![TelegramStateProject {
            id: "project-1".to_owned(),
            name: "TermAl".to_owned(),
        }],
        sessions: (0..12)
            .map(|index| TelegramStateSession {
                id: format!("session-{index}"),
                name: format!("Session {index} {}", "x".repeat(400)),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 1,
            })
            .collect(),
    });
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();

    send_telegram_project_sessions(&telegram, &termal, &config, &mut state, 42)
        .expect("sessions command should send chunks");

    let sent = telegram.sent_texts.borrow();
    assert!(sent.len() > 1);
    assert!(
        sent.iter()
            .all(|chunk| chunk.encode_utf16().count() <= TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS)
    );
    let reconstructed = sent.join("\n");
    let lines = reconstructed.lines().collect::<Vec<_>>();
    for index in 0..12 {
        assert!(
            lines.iter().any(|line| {
                line.starts_with(&format!("- Session {index} "))
                    && line.ends_with("(idle, 1 message)")
            }),
            "missing session name {index}"
        );
        assert!(
            lines
                .iter()
                .any(|line| *line == format!("  id: session-{index}")),
            "missing session id {index}"
        );
    }
}

#[test]
fn telegram_projects_renderer_lists_subscribed_projects_and_active_marker() {
    let mut config = telegram_test_config();
    config.subscribed_project_ids = vec!["project-1".to_owned(), "project-2".to_owned()];
    let bot_state = TelegramBotState {
        selected_project_id: Some("project-2".to_owned()),
        ..TelegramBotState::default()
    };
    let state = TelegramStateSessionsResponse {
        projects: vec![
            TelegramStateProject {
                id: "project-1".to_owned(),
                name: "TermAl".to_owned(),
            },
            TelegramStateProject {
                id: "project-2".to_owned(),
                name: "Side Project".to_owned(),
            },
        ],
        sessions: vec![
            TelegramStateSession {
                id: "session-1".to_owned(),
                name: "Main".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 1,
            },
            TelegramStateSession {
                id: "session-2".to_owned(),
                name: "Other".to_owned(),
                project_id: Some("project-2".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 1,
            },
        ],
    };

    let text = render_telegram_projects(&config, &bot_state, &state);

    assert!(text.contains("- TermAl (1 session)\n  id: project-1"));
    assert!(text.contains("* Side Project (1 session)\n  id: project-2"));
    assert!(text.contains("Send /project <project-id> to switch."));
}

#[test]
fn telegram_project_command_switches_active_project_and_clears_session_target() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        Vec::new(),
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: Vec::new(),
            },
        },
    )
    .with_state_sessions(TelegramStateSessionsResponse {
        projects: vec![
            TelegramStateProject {
                id: "project-1".to_owned(),
                name: "TermAl".to_owned(),
            },
            TelegramStateProject {
                id: "project-2".to_owned(),
                name: "Side Project".to_owned(),
            },
        ],
        sessions: Vec::new(),
    });
    let mut config = telegram_test_config();
    config.subscribed_project_ids = vec!["project-1".to_owned(), "project-2".to_owned()];
    let mut state = TelegramBotState {
        selected_session_id: Some("session-1".to_owned()),
        last_digest_hash: Some("old-digest".to_owned()),
        last_digest_message_id: Some(10),
        ..TelegramBotState::default()
    };

    let changed = select_telegram_project(&telegram, &termal, &config, &mut state, 42, "project-2")
        .expect("project selection should succeed");

    assert!(changed);
    assert_eq!(state.selected_project_id.as_deref(), Some("project-2"));
    assert_eq!(state.selected_session_id, None);
    assert_eq!(state.last_digest_hash, None);
    assert_eq!(state.last_digest_message_id, None);
    assert!(
        telegram.sent_texts.borrow()[0].contains("Telegram project target set to Side Project")
    );

    let changed = select_telegram_project(&telegram, &termal, &config, &mut state, 42, "default")
        .expect("project reset should succeed");

    assert!(changed);
    assert_eq!(state.selected_project_id, None);
}

#[test]
fn telegram_project_command_rejects_unsubscribed_project() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        Vec::new(),
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: Vec::new(),
            },
        },
    )
    .with_state_sessions(TelegramStateSessionsResponse {
        projects: vec![TelegramStateProject {
            id: "project-2".to_owned(),
            name: "Side Project".to_owned(),
        }],
        sessions: Vec::new(),
    });
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();

    let changed = select_telegram_project(&telegram, &termal, &config, &mut state, 42, "project-2")
        .expect("project selection rejection should not fail");

    assert!(!changed);
    assert_eq!(state.selected_project_id, None);
    assert!(telegram.sent_texts.borrow()[0].contains("is not subscribed"));
}

#[test]
fn telegram_session_command_selects_project_session_target() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        Vec::new(),
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: Vec::new(),
            },
        },
    )
    .with_state_sessions(TelegramStateSessionsResponse {
        projects: vec![TelegramStateProject {
            id: "project-1".to_owned(),
            name: "TermAl".to_owned(),
        }],
        sessions: vec![
            TelegramStateSession {
                id: "session-other".to_owned(),
                name: "Other".to_owned(),
                project_id: Some("project-2".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 0,
            },
            TelegramStateSession {
                id: "session-2".to_owned(),
                name: "Target".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 0,
            },
        ],
    });
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();

    let changed =
        select_telegram_project_session(&telegram, &termal, &config, &mut state, 42, "session-2")
            .expect("session selection should succeed");

    assert!(changed);
    assert_eq!(state.selected_session_id.as_deref(), Some("session-2"));
    assert!(telegram.sent_texts.borrow()[0].contains("Telegram session target set to Target"));
    assert!(
        state
            .forward_next_assistant_message_session_ids
            .iter()
            .any(|session_id| session_id == "session-2")
    );

    let changed =
        select_telegram_project_session(&telegram, &termal, &config, &mut state, 42, "clear")
            .expect("session clear should succeed");

    assert!(changed);
    assert_eq!(state.selected_session_id, None);
    assert!(state.forward_next_assistant_message_session_ids.is_empty());
}

#[test]
fn telegram_session_command_no_args_persists_stale_project_cleanup() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        Vec::new(),
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: Vec::new(),
            },
        },
    );
    let config = telegram_test_config();
    let mut state = TelegramBotState {
        selected_project_id: Some("stale-project".to_owned()),
        selected_session_id: Some("session-1".to_owned()),
        last_digest_hash: Some("old-digest".to_owned()),
        last_digest_message_id: Some(10),
        ..TelegramBotState::default()
    };

    let changed = select_telegram_project_session(&telegram, &termal, &config, &mut state, 42, "")
        .expect("session status should succeed");

    assert!(changed);
    assert_eq!(state.selected_project_id, None);
    assert_eq!(state.selected_session_id, None);
    assert_eq!(state.last_digest_hash, None);
    assert_eq!(state.last_digest_message_id, None);
    assert!(telegram.sent_texts.borrow()[0].contains("No Telegram session target is selected"));
}

#[test]
fn telegram_session_command_rejects_sessions_outside_project() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        Vec::new(),
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: Vec::new(),
            },
        },
    )
    .with_state_sessions(TelegramStateSessionsResponse {
        projects: Vec::new(),
        sessions: vec![TelegramStateSession {
            id: "session-other".to_owned(),
            name: "Other".to_owned(),
            project_id: Some("project-2".to_owned()),
            status: TelegramSessionStatus::Idle,
            message_count: 0,
        }],
    });
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();

    let changed = select_telegram_project_session(
        &telegram,
        &termal,
        &config,
        &mut state,
        42,
        "session-other",
    )
    .expect("session selection rejection should not fail");

    assert!(!changed);
    assert_eq!(state.selected_session_id, None);
    assert!(telegram.sent_texts.borrow()[0].contains("I couldn't find session"));
}

#[test]
fn telegram_session_command_uses_selected_project() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        Vec::new(),
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: Vec::new(),
            },
        },
    )
    .with_state_sessions(TelegramStateSessionsResponse {
        projects: vec![TelegramStateProject {
            id: "project-2".to_owned(),
            name: "Side Project".to_owned(),
        }],
        sessions: vec![TelegramStateSession {
            id: "session-2".to_owned(),
            name: "Selected Project Session".to_owned(),
            project_id: Some("project-2".to_owned()),
            status: TelegramSessionStatus::Idle,
            message_count: 0,
        }],
    });
    let mut config = telegram_test_config();
    config.subscribed_project_ids = vec!["project-1".to_owned(), "project-2".to_owned()];
    let mut state = TelegramBotState {
        selected_project_id: Some("project-2".to_owned()),
        ..TelegramBotState::default()
    };

    let changed =
        select_telegram_project_session(&telegram, &termal, &config, &mut state, 42, "session-2")
            .expect("session selection should use selected project");

    assert!(changed);
    assert_eq!(state.selected_session_id.as_deref(), Some("session-2"));
}

#[test]
fn telegram_selected_session_forwards_later_local_termal_reply() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        Vec::new(),
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "baseline".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Existing selected session reply".to_owned(),
                }],
            },
        },
    )
    .with_state_sessions(TelegramStateSessionsResponse {
        projects: vec![TelegramStateProject {
            id: "project-1".to_owned(),
            name: "TermAl".to_owned(),
        }],
        sessions: vec![TelegramStateSession {
            id: "session-2".to_owned(),
            name: "Selected".to_owned(),
            project_id: Some("project-1".to_owned()),
            status: TelegramSessionStatus::Idle,
            message_count: 1,
        }],
    });
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();

    select_telegram_project_session(&telegram, &termal, &config, &mut state, 42, "session-2")
        .expect("session selection should baseline selected session");

    assert_eq!(
        state
            .assistant_forwarding_cursors
            .get("session-2")
            .and_then(|cursor| cursor.message_id.as_deref()),
        Some("baseline")
    );

    let forward_telegram = FakeTelegramSender::new(None);
    let settled_after_local_prompt = FakeTelegramSessionReaderById {
        responses: HashMap::from([(
            "session-2".to_owned(),
            TelegramSessionFetchResponse {
                session: TelegramSessionFetchSession {
                    status: TelegramSessionStatus::Idle,
                    messages: vec![
                        TelegramSessionFetchMessage::Text {
                            id: "baseline".to_owned(),
                            author: "assistant".to_owned(),
                            text: "Existing selected session reply".to_owned(),
                        },
                        TelegramSessionFetchMessage::Text {
                            id: "reply".to_owned(),
                            author: "assistant".to_owned(),
                            text: "Reply to local TermAl prompt".to_owned(),
                        },
                    ],
                },
            },
        )]),
    };

    let changed = forward_relevant_assistant_messages(
        &forward_telegram,
        &settled_after_local_prompt,
        &mut state,
        42,
        Some("session-2"),
    );

    assert!(changed);
    assert_eq!(
        forward_telegram.sent_texts.borrow().as_slice(),
        [
            "Reply to local TermAl prompt".to_owned(),
            telegram_turn_settled_footer(&TelegramSessionStatus::Idle).to_owned()
        ]
    );
    assert_eq!(state.forward_next_assistant_message_session_id, None);
}

#[test]
fn telegram_selected_session_sync_baselines_persisted_selection_before_local_reply() {
    let termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "baseline".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Existing selected session reply".to_owned(),
                }],
            },
        },
    };
    let mut state = TelegramBotState {
        selected_session_id: Some("session-2".to_owned()),
        ..TelegramBotState::default()
    };

    let changed = ensure_selected_session_forwarding_baseline(&termal, &mut state, "session-2")
        .expect("selected session baseline should succeed");

    assert!(changed);
    assert_eq!(
        state
            .assistant_forwarding_cursors
            .get("session-2")
            .and_then(|cursor| cursor.message_id.as_deref()),
        Some("baseline")
    );
    assert!(
        state
            .forward_next_assistant_message_session_ids
            .iter()
            .any(|session_id| session_id == "session-2")
    );
}

// Pins the outgoing digest shape: the rendered text exposes the project
// headline, a `Next: ...` line listing proposed action labels, and an
// `Open: ...` line resolving the relative deep link against the public
// base URL, while the inline keyboard emits one button per action with
// `callback_data` bound to both the digest project and action. Without this
// the phone UI would either lose tap-to-act buttons or fire callbacks against
// whichever project is active when the user taps an older digest.
#[test]
fn telegram_digest_renderer_includes_actions_and_public_link() {
    let digest = ProjectDigestResponse {
        project_id: "project-1".to_owned(),
        headline: "termal".to_owned(),
        done_summary: "Updated the digest API.".to_owned(),
        current_status: "Changes are ready for review.".to_owned(),
        primary_session_id: Some("session-1".to_owned()),
        proposed_actions: vec![
            ProjectActionId::ReviewInTermal.into_digest_action(),
            ProjectActionId::AskAgentToCommit.into_digest_action(),
        ],
        deep_link: Some("/?projectId=project-1&sessionId=session-1".to_owned()),
        source_message_ids: vec!["message-1".to_owned()],
    };

    let (rendered, format) = render_telegram_digest_message(&digest, Some("https://termal.local"));
    assert_eq!(format, TelegramTextFormat::Html);
    assert!(rendered.contains("<b>Project digest</b>"));
    assert!(rendered.contains("Project  termal"));
    assert!(rendered.contains("Next     Review in TermAl, Ask Agent to Commit"));
    assert!(rendered.contains(
        "<a href=\"https://termal.local/?projectId=project-1&amp;sessionId=session-1\">Open in TermAl</a>"
    ));

    let keyboard = build_telegram_digest_keyboard(&digest)
        .expect("keyboard should build")
        .expect("keyboard should exist");
    assert_eq!(keyboard.inline_keyboard.len(), 1);
    assert_eq!(
        keyboard.inline_keyboard[0][0].callback_data,
        telegram_digest_callback_data("project-1", "review-in-termal")
            .expect("callback data should fit")
    );
    assert_eq!(
        keyboard.inline_keyboard[0][1].callback_data,
        telegram_digest_callback_data("project-1", "ask-agent-to-commit")
            .expect("callback data should fit")
    );
    assert!(
        keyboard.inline_keyboard[0]
            .iter()
            .all(|button| button.callback_data.len() <= 64)
    );
}

#[test]
fn telegram_digest_keyboard_rejects_oversized_callback_data() {
    let digest = ProjectDigestResponse {
        project_id: "project-1".to_owned(),
        headline: "termal".to_owned(),
        done_summary: "Updated the digest API.".to_owned(),
        current_status: "Changes are ready for review.".to_owned(),
        primary_session_id: Some("session-1".to_owned()),
        proposed_actions: vec![ProjectDigestAction {
            id: "x".repeat(46),
            label: "Too Long".to_owned(),
            prompt: None,
            requires_confirmation: false,
        }],
        deep_link: None,
        source_message_ids: vec![],
    };

    let err = build_telegram_digest_keyboard(&digest)
        .expect_err("oversized callback data should fail before Telegram send");

    assert!(err.to_string().contains("exceeds 64 bytes"));
}

#[test]
fn telegram_digest_html_renderer_escapes_fields() {
    let digest = ProjectDigestResponse {
        project_id: "project-1".to_owned(),
        headline: "TermAl <dev> & docs".to_owned(),
        done_summary: "Escaped \"quotes\" and <tags>.".to_owned(),
        current_status: "Ready > waiting.".to_owned(),
        primary_session_id: Some("session-1".to_owned()),
        proposed_actions: vec![ProjectDigestAction {
            id: "ask-fix".to_owned(),
            label: "Ask & <fix>".to_owned(),
            prompt: None,
            requires_confirmation: false,
        }],
        deep_link: Some("https://termal.local/?projectId=project-1&sessionId=<session>".to_owned()),
        source_message_ids: vec![],
    };

    let rendered = render_telegram_digest_html(&digest, None);

    assert!(rendered.starts_with("<b>Project digest</b>\n<pre>"));
    assert!(rendered.contains("Project  TermAl &lt;dev&gt; &amp; docs"));
    assert!(rendered.contains("Status   Ready &gt; waiting."));
    assert!(rendered.contains("Done     Escaped &quot;quotes&quot; and &lt;tags&gt;."));
    assert!(rendered.contains("Next     Ask &amp; &lt;fix&gt;"));
    assert!(rendered.contains(
        "<a href=\"https://termal.local/?projectId=project-1&amp;sessionId=&lt;session&gt;\">Open in TermAl</a>"
    ));
    assert!(rendered.contains("</pre>\n<a href="));
}

#[test]
fn telegram_digest_send_uses_html_parse_mode() {
    let telegram = FakeTelegramSender::new(None);
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();
    let digest = ProjectDigestResponse {
        project_id: "project-1".to_owned(),
        headline: "termal <dev>".to_owned(),
        done_summary: "Updated the digest API.".to_owned(),
        current_status: "Changes are ready for review.".to_owned(),
        primary_session_id: Some("session-1".to_owned()),
        proposed_actions: vec![],
        deep_link: None,
        source_message_ids: vec![],
    };

    let changed =
        send_fresh_telegram_digest_from_response(&telegram, &config, &mut state, 42, &digest)
            .expect("digest send should succeed");

    assert!(changed);
    assert_eq!(
        telegram.sent_formats.borrow().as_slice(),
        [TelegramTextFormat::Html]
    );
    assert!(telegram.sent_texts.borrow()[0].contains("<pre>"));
    assert!(telegram.sent_texts.borrow()[0].contains("termal &lt;dev&gt;"));
}

#[test]
fn telegram_digest_edit_uses_html_parse_mode() {
    let telegram = FakeTelegramSender::new(None);
    let config = telegram_test_config();
    let digest = ProjectDigestResponse {
        project_id: "project-1".to_owned(),
        headline: "termal <dev>".to_owned(),
        done_summary: "Updated the digest API.".to_owned(),
        current_status: "Changes are ready for review.".to_owned(),
        primary_session_id: Some("session-1".to_owned()),
        proposed_actions: vec![],
        deep_link: None,
        source_message_ids: vec![],
    };

    let message_id = edit_or_send_telegram_digest(&telegram, &config, 42, Some(99), &digest)
        .expect("digest edit should succeed");

    assert_eq!(message_id, 99);
    assert!(telegram.sent_texts.borrow().is_empty());
    let edited_messages = telegram.edited_messages.borrow();
    assert_eq!(edited_messages.len(), 1);
    assert_eq!(edited_messages[0].3, TelegramTextFormat::Html);
    assert!(edited_messages[0].2.contains("<pre>"));
    assert!(edited_messages[0].2.contains("termal &lt;dev&gt;"));
}

#[test]
fn telegram_digest_html_renderer_collapses_and_bounds_table_values() {
    let digest = ProjectDigestResponse {
        project_id: "project-1".to_owned(),
        headline: "termal".to_owned(),
        done_summary: format!("first line\n  second line {}", "x".repeat(400)),
        current_status: "ready".to_owned(),
        primary_session_id: Some("session-1".to_owned()),
        proposed_actions: vec![],
        deep_link: None,
        source_message_ids: vec![],
    };

    let rendered = render_telegram_digest_html(&digest, None);

    assert!(rendered.contains("Done     first line second line "));
    assert!(rendered.contains("..."));
    assert!(rendered.encode_utf16().count() <= TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS);
}

#[test]
fn telegram_digest_hash_includes_rendered_html_format() {
    let digest = ProjectDigestResponse {
        project_id: "project-1".to_owned(),
        headline: "termal".to_owned(),
        done_summary: "done".to_owned(),
        current_status: "ready".to_owned(),
        primary_session_id: Some("session-1".to_owned()),
        proposed_actions: vec![ProjectActionId::ReviewInTermal.into_digest_action()],
        deep_link: None,
        source_message_ids: vec![],
    };

    let (text, format) = render_telegram_digest_message(&digest, Some("https://termal.local"));
    let payload = json!({
        "format": format.parse_mode(),
        "callbackScheme": 1,
        "projectId": "project-1",
        "text": text,
        "actions": ["review-in-termal"],
    });
    let expected = stable_text_hash(
        &serde_json::to_string(&payload).expect("expected hash payload should encode"),
    );
    let plain_format_payload = json!({
        "format": Option::<&str>::None,
        "callbackScheme": 1,
        "projectId": "project-1",
        "text": text,
        "actions": ["review-in-termal"],
    });
    let plain_format_hash = stable_text_hash(
        &serde_json::to_string(&plain_format_payload)
            .expect("plain-format hash payload should encode"),
    );
    let actual = telegram_digest_hash(&digest, Some("https://termal.local"))
        .expect("digest hash should compute");

    assert_eq!(actual, expected);
    assert_ne!(actual, plain_format_hash);
}

#[test]
fn telegram_text_message_body_omits_parse_mode_for_plain_text_with_or_without_markup() {
    let body = telegram_send_message_body(42, "plain text", None, TelegramTextFormat::Plain)
        .expect("plain send body should build");

    assert_eq!(body["chat_id"], json!(42));
    assert_eq!(body["text"], json!("plain text"));
    assert_eq!(body["disable_web_page_preview"], json!(true));
    assert!(body.get("parse_mode").is_none());
    assert!(body.get("reply_markup").is_none());

    let keyboard = TelegramInlineKeyboardMarkup {
        inline_keyboard: vec![vec![TelegramInlineKeyboardButton {
            text: "Review".to_owned(),
            callback_data: "review".to_owned(),
        }]],
    };
    let plain_with_markup =
        telegram_send_message_body(42, "plain text", Some(&keyboard), TelegramTextFormat::Plain)
            .expect("plain send body with markup should build");
    assert!(plain_with_markup.get("parse_mode").is_none());
    assert_eq!(
        plain_with_markup["reply_markup"]["inline_keyboard"][0][0]["callback_data"],
        json!("review")
    );
}

#[test]
fn telegram_message_sender_default_uses_plain_text_format() {
    let telegram = FakeTelegramSender::new(None);

    telegram
        .send_message(42, "plain text", None)
        .expect("default sender should send plain text");

    assert_eq!(
        telegram.sent_formats.borrow().as_slice(),
        [TelegramTextFormat::Plain]
    );
}

#[test]
fn telegram_text_message_bodies_include_html_parse_mode_and_markup() {
    let keyboard = TelegramInlineKeyboardMarkup {
        inline_keyboard: vec![vec![TelegramInlineKeyboardButton {
            text: "Review".to_owned(),
            callback_data: "review".to_owned(),
        }]],
    };

    let send_without_markup =
        telegram_send_message_body(42, "<b>digest</b>", None, TelegramTextFormat::Html)
            .expect("html send body without markup should build");
    assert_eq!(send_without_markup["parse_mode"], json!("HTML"));
    assert!(send_without_markup.get("reply_markup").is_none());

    let send_body = telegram_send_message_body(
        42,
        "<b>digest</b>",
        Some(&keyboard),
        TelegramTextFormat::Html,
    )
    .expect("html send body should build");
    assert_eq!(send_body["parse_mode"], json!("HTML"));
    assert_eq!(
        send_body["reply_markup"]["inline_keyboard"][0][0]["callback_data"],
        json!("review")
    );

    let edit_body = telegram_edit_message_body(
        42,
        99,
        "<b>digest</b>",
        Some(&keyboard),
        TelegramTextFormat::Html,
    )
    .expect("html edit body should build");
    assert_eq!(edit_body["message_id"], json!(99));
    assert_eq!(edit_body["parse_mode"], json!("HTML"));
    assert_eq!(
        edit_body["reply_markup"]["inline_keyboard"][0][0]["text"],
        json!("Review")
    );
}

#[test]
fn telegram_forward_records_partial_progress_when_later_send_fails() {
    let telegram = FakeTelegramSender::new(Some(2));
    let termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![
                    TelegramSessionFetchMessage::Text {
                        id: "baseline".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Baseline".to_owned(),
                    },
                    TelegramSessionFetchMessage::Text {
                        id: "message-1".to_owned(),
                        author: "assistant".to_owned(),
                        text: "First reply".to_owned(),
                    },
                    TelegramSessionFetchMessage::Text {
                        id: "message-2".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Second reply".to_owned(),
                    },
                ],
            },
        },
    };
    let mut state = TelegramBotState {
        last_forwarded_assistant_message_id: Some("baseline".to_owned()),
        last_forwarded_assistant_message_text_chars: Some("Baseline".chars().count()),
        ..TelegramBotState::default()
    };

    let outcome =
        forward_new_assistant_message_outcome(&telegram, &termal, &mut state, 42, "session-1")
            .expect("partial visible send should be handled");

    assert!(outcome.dirty);
    assert!(outcome.sent_visible_content);
    assert_eq!(
        telegram.sent_texts.borrow().as_slice(),
        ["First reply".to_owned()]
    );
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("partial progress should persist cursor");
    assert_eq!(cursor.message_id.as_deref(), Some("message-2"));
    assert_eq!(cursor.text_chars, Some("Second reply".chars().count()));
    assert_eq!(cursor.sent_chunks, Some(0));
    assert_eq!(cursor.failed_chunk_send_attempts, Some(1));
}

#[test]
fn telegram_prompt_without_prior_assistant_baseline_forwards_first_reply() {
    let baseline_termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![],
            },
        },
    };
    let mut state = TelegramBotState::default();

    let changed =
        arm_assistant_forwarding_for_telegram_prompt(&baseline_termal, &mut state, "session-1")
            .expect("arming should succeed");

    assert!(changed);
    assert_eq!(
        state.forward_next_assistant_message_session_id.as_deref(),
        Some("session-1")
    );
    assert_eq!(state.last_forwarded_assistant_message_id, None);

    let telegram = FakeTelegramSender::new(None);
    let settled_termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "First reply".to_owned(),
                }],
            },
        },
    };

    let forwarded = forward_new_assistant_message_if_any(
        &telegram,
        &settled_termal,
        &mut state,
        42,
        "session-1",
    )
    .expect("forwarding should succeed");

    assert!(forwarded);
    assert_eq!(telegram.sent_texts.borrow()[0], "First reply");
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("forwarding should persist cursor");
    assert_eq!(cursor.message_id.as_deref(), Some("message-1"));
    assert_eq!(cursor.text_chars, Some("First reply".chars().count()));
    assert_eq!(state.forward_next_assistant_message_session_id, None);
}

#[test]
fn telegram_prompt_post_failure_does_not_arm_assistant_forwarding() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![Ok(telegram_project_digest(Some("session-1")))],
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "baseline".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Existing reply".to_owned(),
                }],
            },
        },
    )
    .with_send_error("prompt rejected");
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();

    let result =
        forward_telegram_text_to_project(&telegram, &termal, &config, &mut state, 42, "from chat");

    assert!(result.is_err());
    assert!(termal.sent_prompts.borrow().is_empty());
    assert!(telegram.sent_texts.borrow().is_empty());
    assert!(state.assistant_forwarding_cursors.is_empty());
    assert!(state.forward_next_assistant_message_session_ids.is_empty());
    assert_eq!(state.forward_next_assistant_message_session_id, None);
}

#[test]
fn telegram_prompt_digest_refresh_failure_keeps_single_accepted_prompt_armed() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![
            Ok(telegram_project_digest(Some("session-1"))),
            Err("digest refresh failed".to_owned()),
        ],
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "baseline".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Existing reply".to_owned(),
                }],
            },
        },
    );
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();

    let changed =
        forward_telegram_text_to_project(&telegram, &termal, &config, &mut state, 42, "from chat")
            .expect("digest refresh failure should not replay prompt");

    assert!(changed);
    assert_eq!(
        termal.sent_prompts.borrow().as_slice(),
        [("session-1".to_owned(), "from chat".to_owned())]
    );
    assert!(telegram.sent_texts.borrow().is_empty());
    assert!(
        state
            .forward_next_assistant_message_session_ids
            .iter()
            .any(|session_id| session_id == "session-1")
    );
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("accepted prompt should arm assistant forwarding");
    assert_eq!(cursor.message_id.as_deref(), Some("baseline"));
}

#[test]
fn telegram_prompt_uses_selected_session_before_digest_primary() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![
            Ok(telegram_project_digest(Some("session-1"))),
            Ok(telegram_project_digest(Some("session-1"))),
        ],
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "baseline".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Existing selected session reply".to_owned(),
                }],
            },
        },
    )
    .with_state_sessions(TelegramStateSessionsResponse {
        projects: Vec::new(),
        sessions: vec![TelegramStateSession {
            id: "session-2".to_owned(),
            name: "Selected".to_owned(),
            project_id: Some("project-1".to_owned()),
            status: TelegramSessionStatus::Idle,
            message_count: 1,
        }],
    });
    let config = telegram_test_config();
    let mut state = TelegramBotState {
        selected_session_id: Some("session-2".to_owned()),
        ..TelegramBotState::default()
    };

    let changed =
        forward_telegram_text_to_project(&telegram, &termal, &config, &mut state, 42, "from chat")
            .expect("forwarding should succeed");

    assert!(changed);
    assert_eq!(
        termal.sent_prompts.borrow().as_slice(),
        [("session-2".to_owned(), "from chat".to_owned())]
    );
    assert_eq!(state.selected_session_id.as_deref(), Some("session-2"));
    assert!(
        state
            .forward_next_assistant_message_session_ids
            .iter()
            .any(|session_id| session_id == "session-2")
    );
}

#[test]
fn telegram_prompt_uses_selected_project_digest_and_session() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![
            Ok(ProjectDigestResponse {
                project_id: "project-2".to_owned(),
                headline: "side".to_owned(),
                done_summary: "Working.".to_owned(),
                current_status: "Agent is working.".to_owned(),
                primary_session_id: Some("session-2".to_owned()),
                proposed_actions: vec![],
                deep_link: None,
                source_message_ids: vec![],
            }),
            Ok(ProjectDigestResponse {
                project_id: "project-2".to_owned(),
                headline: "side".to_owned(),
                done_summary: "Still working.".to_owned(),
                current_status: "Agent is working.".to_owned(),
                primary_session_id: Some("session-2".to_owned()),
                proposed_actions: vec![],
                deep_link: None,
                source_message_ids: vec![],
            }),
        ],
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "baseline".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Existing selected project reply".to_owned(),
                }],
            },
        },
    );
    let mut config = telegram_test_config();
    config.subscribed_project_ids = vec!["project-1".to_owned(), "project-2".to_owned()];
    let mut state = TelegramBotState {
        selected_project_id: Some("project-2".to_owned()),
        ..TelegramBotState::default()
    };

    let changed =
        forward_telegram_text_to_project(&telegram, &termal, &config, &mut state, 42, "from chat")
            .expect("forwarding should succeed");

    assert!(changed);
    assert_eq!(
        termal.digest_project_ids.borrow().as_slice(),
        ["project-2".to_owned(), "project-2".to_owned()]
    );
    assert_eq!(
        termal.sent_prompts.borrow().as_slice(),
        [("session-2".to_owned(), "from chat".to_owned())]
    );
}

#[test]
fn telegram_assistant_forwarding_plan_only_mutates_after_apply() {
    let termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![],
            },
        },
    };
    let mut state = TelegramBotState::default();

    let plan = prepare_assistant_forwarding_for_telegram_prompt(&termal, "session-1")
        .expect("prepare should succeed");

    assert_eq!(state.forward_next_assistant_message_session_id, None);
    assert_eq!(state.last_forwarded_assistant_message_id, None);
    assert!(apply_assistant_forwarding_plan(&mut state, plan));
    assert_eq!(
        state.forward_next_assistant_message_session_id.as_deref(),
        Some("session-1")
    );
}

#[test]
fn telegram_assistant_forwarding_plan_baselines_preexisting_active_turn_without_resend() {
    let termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Active,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Existing local turn".to_owned(),
                }],
            },
        },
    };
    let mut state = TelegramBotState::default();

    let plan = prepare_assistant_forwarding_for_telegram_prompt(&termal, "session-1")
        .expect("prepare should succeed");

    assert!(apply_assistant_forwarding_plan(&mut state, plan));
    assert_eq!(
        state.forward_next_assistant_message_session_id.as_deref(),
        Some("session-1")
    );
    let cursor = resolve_assistant_forwarding_cursor(&state, "session-1", &[]);
    assert_eq!(cursor.message_id.as_deref(), Some("message-1"));
    assert_eq!(
        cursor.text_chars,
        Some("Existing local turn".chars().count())
    );
    assert!(!cursor.resend_if_grown);
    assert!(cursor.baseline_while_active);

    let telegram = FakeTelegramSender::new(None);
    let settled_existing_turn = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Existing local turn finished".to_owned(),
                }],
            },
        },
    };

    let forwarded = forward_new_assistant_message_if_any(
        &telegram,
        &settled_existing_turn,
        &mut state,
        42,
        "session-1",
    )
    .expect("forwarding should succeed");

    assert!(forwarded);
    assert!(telegram.sent_texts.borrow().is_empty());
    assert_eq!(
        state.forward_next_assistant_message_session_id.as_deref(),
        Some("session-1")
    );

    let telegram_reply = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![
                    TelegramSessionFetchMessage::Text {
                        id: "message-1".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Existing local turn finished".to_owned(),
                    },
                    TelegramSessionFetchMessage::Text {
                        id: "message-2".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Telegram reply".to_owned(),
                    },
                ],
            },
        },
    };

    let forwarded = forward_new_assistant_message_if_any(
        &telegram,
        &telegram_reply,
        &mut state,
        42,
        "session-1",
    )
    .expect("telegram reply should forward");

    assert!(forwarded);
    assert_eq!(telegram.sent_texts.borrow()[0], "Telegram reply");
    assert_eq!(state.forward_next_assistant_message_session_id, None);
}

#[test]
fn telegram_active_session_without_assistant_text_baselines_old_turn_before_reply() {
    let termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Active,
                messages: vec![],
            },
        },
    };
    let mut state = TelegramBotState::default();

    let plan = prepare_assistant_forwarding_for_telegram_prompt(&termal, "session-1")
        .expect("prepare should succeed");
    assert!(apply_assistant_forwarding_plan(&mut state, plan));

    let telegram = FakeTelegramSender::new(None);
    let preexisting_turn_settled = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Pre-existing turn reply".to_owned(),
                }],
            },
        },
    };

    let baselined = forward_new_assistant_message_if_any(
        &telegram,
        &preexisting_turn_settled,
        &mut state,
        42,
        "session-1",
    )
    .expect("pre-existing turn should baseline");

    assert!(baselined);
    assert!(telegram.sent_texts.borrow().is_empty());
    assert_eq!(
        state.forward_next_assistant_message_session_id.as_deref(),
        Some("session-1")
    );

    let telegram_reply = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![
                    TelegramSessionFetchMessage::Text {
                        id: "message-1".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Pre-existing turn reply".to_owned(),
                    },
                    TelegramSessionFetchMessage::Text {
                        id: "message-2".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Telegram reply".to_owned(),
                    },
                ],
            },
        },
    };

    let forwarded = forward_new_assistant_message_if_any(
        &telegram,
        &telegram_reply,
        &mut state,
        42,
        "session-1",
    )
    .expect("telegram reply should forward");

    assert!(forwarded);
    assert_eq!(telegram.sent_texts.borrow()[0], "Telegram reply");
    assert_eq!(state.forward_next_assistant_message_session_id, None);
}

#[test]
fn telegram_active_baseline_advances_across_active_polls_before_reply() {
    let termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Active,
                messages: vec![],
            },
        },
    };
    let mut state = TelegramBotState::default();

    let plan = prepare_assistant_forwarding_for_telegram_prompt(&termal, "session-1")
        .expect("prepare should succeed");
    assert!(apply_assistant_forwarding_plan(&mut state, plan));

    let telegram = FakeTelegramSender::new(None);
    let active_old_turn = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Active,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Old turn partial".to_owned(),
                }],
            },
        },
    };

    let changed = forward_new_assistant_message_if_any(
        &telegram,
        &active_old_turn,
        &mut state,
        42,
        "session-1",
    )
    .expect("active old turn should update baseline");

    assert!(changed);
    assert!(telegram.sent_texts.borrow().is_empty());
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("cursor should be persisted");
    assert_eq!(cursor.message_id.as_deref(), Some("message-1"));
    assert_eq!(cursor.text_chars, Some("Old turn partial".chars().count()));
    assert!(cursor.baseline_while_active);

    let active_old_turn_grew = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Active,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Old turn partial, still growing".to_owned(),
                }],
            },
        },
    };

    let changed = forward_new_assistant_message_if_any(
        &telegram,
        &active_old_turn_grew,
        &mut state,
        42,
        "session-1",
    )
    .expect("active old turn growth should update baseline");

    assert!(changed);
    assert!(telegram.sent_texts.borrow().is_empty());
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("cursor should stay persisted");
    assert_eq!(
        cursor.text_chars,
        Some("Old turn partial, still growing".chars().count())
    );
    assert!(cursor.baseline_while_active);

    let settled_old_turn = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Old turn complete".to_owned(),
                }],
            },
        },
    };

    let changed = forward_new_assistant_message_if_any(
        &telegram,
        &settled_old_turn,
        &mut state,
        42,
        "session-1",
    )
    .expect("settled old turn should baseline");

    assert!(changed);
    assert!(telegram.sent_texts.borrow().is_empty());
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("cursor should stay persisted");
    assert_eq!(cursor.message_id.as_deref(), Some("message-1"));
    assert!(!cursor.baseline_while_active);
    assert_eq!(
        state.forward_next_assistant_message_session_id.as_deref(),
        Some("session-1")
    );

    let telegram_reply = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![
                    TelegramSessionFetchMessage::Text {
                        id: "message-1".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Old turn complete".to_owned(),
                    },
                    TelegramSessionFetchMessage::Text {
                        id: "message-2".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Telegram reply".to_owned(),
                    },
                ],
            },
        },
    };

    let changed = forward_new_assistant_message_if_any(
        &telegram,
        &telegram_reply,
        &mut state,
        42,
        "session-1",
    )
    .expect("telegram reply should forward");

    assert!(changed);
    assert_eq!(telegram.sent_texts.borrow()[0], "Telegram reply");
    assert_eq!(state.forward_next_assistant_message_session_id, None);
}

#[test]
fn telegram_active_baseline_reforwards_same_message_growth_after_settle() {
    let termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Active,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Old turn partial".to_owned(),
                }],
            },
        },
    };
    let mut state = TelegramBotState::default();

    let plan = prepare_assistant_forwarding_for_telegram_prompt(&termal, "session-1")
        .expect("prepare should succeed");
    assert!(apply_assistant_forwarding_plan(&mut state, plan));

    let telegram = FakeTelegramSender::new(None);
    let settled_old_turn = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Old turn complete".to_owned(),
                }],
            },
        },
    };

    let changed = forward_new_assistant_message_if_any(
        &telegram,
        &settled_old_turn,
        &mut state,
        42,
        "session-1",
    )
    .expect("settled old turn should baseline");

    assert!(changed);
    assert!(telegram.sent_texts.borrow().is_empty());
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("cursor should stay persisted");
    assert_eq!(cursor.message_id.as_deref(), Some("message-1"));
    assert_eq!(cursor.text_chars, Some("Old turn complete".chars().count()));
    assert!(cursor.resend_if_grown);
    assert!(!cursor.baseline_while_active);

    let same_message_reply = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Old turn complete\nTelegram reply".to_owned(),
                }],
            },
        },
    };

    let changed = forward_new_assistant_message_if_any(
        &telegram,
        &same_message_reply,
        &mut state,
        42,
        "session-1",
    )
    .expect("same-message reply growth should forward");

    assert!(changed);
    assert_eq!(
        telegram.sent_texts.borrow()[0],
        "Old turn complete\nTelegram reply"
    );
    assert_eq!(state.forward_next_assistant_message_session_id, None);
}

#[test]
fn telegram_active_baseline_survives_approval_without_text_before_reply() {
    let termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Active,
                messages: vec![],
            },
        },
    };
    let mut state = TelegramBotState::default();

    let plan = prepare_assistant_forwarding_for_telegram_prompt(&termal, "session-1")
        .expect("prepare should succeed");
    assert!(apply_assistant_forwarding_plan(&mut state, plan));

    let telegram = FakeTelegramSender::new(None);
    let approval_without_text = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Approval,
                messages: vec![],
            },
        },
    };

    let changed = forward_new_assistant_message_if_any(
        &telegram,
        &approval_without_text,
        &mut state,
        42,
        "session-1",
    )
    .expect("approval pause should not fail");

    assert!(!changed);
    assert!(telegram.sent_texts.borrow().is_empty());
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("approval pause should keep active baseline cursor");
    assert!(cursor.baseline_while_active);
    assert_eq!(cursor.message_id, None);
    assert_eq!(
        state.forward_next_assistant_message_session_id.as_deref(),
        Some("session-1")
    );

    let resumed_old_turn = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Active,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Approved old turn".to_owned(),
                }],
            },
        },
    };

    let changed = forward_new_assistant_message_if_any(
        &telegram,
        &resumed_old_turn,
        &mut state,
        42,
        "session-1",
    )
    .expect("resumed old turn should update baseline");

    assert!(changed);
    assert!(telegram.sent_texts.borrow().is_empty());

    let settled_old_turn = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Approved old turn complete".to_owned(),
                }],
            },
        },
    };

    let changed = forward_new_assistant_message_if_any(
        &telegram,
        &settled_old_turn,
        &mut state,
        42,
        "session-1",
    )
    .expect("settled old turn should baseline");

    assert!(changed);
    assert!(telegram.sent_texts.borrow().is_empty());

    let telegram_reply = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![
                    TelegramSessionFetchMessage::Text {
                        id: "message-1".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Approved old turn complete".to_owned(),
                    },
                    TelegramSessionFetchMessage::Text {
                        id: "message-2".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Telegram reply after approval".to_owned(),
                    },
                ],
            },
        },
    };

    let changed = forward_new_assistant_message_if_any(
        &telegram,
        &telegram_reply,
        &mut state,
        42,
        "session-1",
    )
    .expect("telegram reply should forward");

    assert!(changed);
    assert_eq!(
        telegram.sent_texts.borrow()[0],
        "Telegram reply after approval"
    );
    assert_eq!(state.forward_next_assistant_message_session_id, None);
}

#[test]
fn telegram_prompt_behind_approval_session_uses_active_baseline() {
    let termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Approval,
                messages: vec![],
            },
        },
    };
    let mut state = TelegramBotState::default();

    let plan = prepare_assistant_forwarding_for_telegram_prompt(&termal, "session-1")
        .expect("prepare should succeed");
    assert!(apply_assistant_forwarding_plan(&mut state, plan));

    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("approval prepare should persist cursor");
    assert!(cursor.baseline_while_active);
    assert_eq!(cursor.message_id, None);
}

#[test]
fn telegram_prompt_behind_initial_approval_session_forwards_later_reply() {
    let termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Approval,
                messages: vec![],
            },
        },
    };
    let mut state = TelegramBotState::default();

    let plan = prepare_assistant_forwarding_for_telegram_prompt(&termal, "session-1")
        .expect("prepare should succeed");
    assert!(apply_assistant_forwarding_plan(&mut state, plan));

    let telegram = FakeTelegramSender::new(None);
    let resumed_old_turn = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Active,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Old approved turn resumed".to_owned(),
                }],
            },
        },
    };

    let changed = forward_new_assistant_message_if_any(
        &telegram,
        &resumed_old_turn,
        &mut state,
        42,
        "session-1",
    )
    .expect("resumed old turn should baseline");

    assert!(changed);
    assert!(telegram.sent_texts.borrow().is_empty());
    assert_eq!(
        state.forward_next_assistant_message_session_id.as_deref(),
        Some("session-1")
    );

    let settled_with_reply = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![
                    TelegramSessionFetchMessage::Text {
                        id: "message-1".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Old approved turn complete".to_owned(),
                    },
                    TelegramSessionFetchMessage::Text {
                        id: "message-2".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Telegram reply after queued approval".to_owned(),
                    },
                ],
            },
        },
    };

    let changed = forward_new_assistant_message_if_any(
        &telegram,
        &settled_with_reply,
        &mut state,
        42,
        "session-1",
    )
    .expect("telegram reply should forward");

    assert!(changed);
    assert_eq!(
        telegram.sent_texts.borrow()[0],
        "Telegram reply after queued approval"
    );
    assert_eq!(state.forward_next_assistant_message_session_id, None);
}

#[test]
fn telegram_prompt_behind_initial_approval_with_prior_text_forwards_later_reply() {
    let termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Approval,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Old approved turn waiting".to_owned(),
                }],
            },
        },
    };
    let mut state = TelegramBotState::default();

    let plan = prepare_assistant_forwarding_for_telegram_prompt(&termal, "session-1")
        .expect("prepare should succeed");
    assert!(apply_assistant_forwarding_plan(&mut state, plan));
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("approval prepare should persist cursor");
    assert_eq!(cursor.message_id.as_deref(), Some("message-1"));
    assert!(cursor.baseline_while_active);

    let telegram = FakeTelegramSender::new(None);
    let resumed_old_turn = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Active,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Old approved turn resumed".to_owned(),
                }],
            },
        },
    };

    let _changed = forward_new_assistant_message_if_any(
        &telegram,
        &resumed_old_turn,
        &mut state,
        42,
        "session-1",
    )
    .expect("resumed old turn should baseline");

    assert!(telegram.sent_texts.borrow().is_empty());
    assert_eq!(
        state.forward_next_assistant_message_session_id.as_deref(),
        Some("session-1")
    );

    let settled_with_reply = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![
                    TelegramSessionFetchMessage::Text {
                        id: "message-1".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Old approved turn complete".to_owned(),
                    },
                    TelegramSessionFetchMessage::Text {
                        id: "message-2".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Telegram reply after prior approval".to_owned(),
                    },
                ],
            },
        },
    };

    let changed = forward_new_assistant_message_if_any(
        &telegram,
        &settled_with_reply,
        &mut state,
        42,
        "session-1",
    )
    .expect("telegram reply should forward");

    assert!(changed);
    assert_eq!(
        telegram.sent_texts.borrow()[0],
        "Telegram reply after prior approval"
    );
    assert_eq!(state.forward_next_assistant_message_session_id, None);
}

#[test]
fn telegram_unknown_status_preserves_old_turn_boundary_until_known_reply() {
    let termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Active,
                messages: vec![],
            },
        },
    };
    let mut state = TelegramBotState::default();

    let plan = prepare_assistant_forwarding_for_telegram_prompt(&termal, "session-1")
        .expect("prepare should succeed");
    assert!(apply_assistant_forwarding_plan(&mut state, plan));

    let telegram = FakeTelegramSender::new(None);
    let unknown_old_turn = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Unknown,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Old turn under future status".to_owned(),
                }],
            },
        },
    };

    let changed = forward_new_assistant_message_if_any(
        &telegram,
        &unknown_old_turn,
        &mut state,
        42,
        "session-1",
    )
    .expect("unknown status should keep boundary");

    assert!(changed);
    assert!(telegram.sent_texts.borrow().is_empty());
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("unknown status should persist old-turn baseline");
    assert_eq!(cursor.message_id.as_deref(), Some("message-1"));
    assert!(cursor.baseline_while_active);

    let settled_with_reply = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![
                    TelegramSessionFetchMessage::Text {
                        id: "message-1".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Old turn under future status, now settled".to_owned(),
                    },
                    TelegramSessionFetchMessage::Text {
                        id: "message-2".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Telegram reply".to_owned(),
                    },
                ],
            },
        },
    };

    let changed = forward_new_assistant_message_if_any(
        &telegram,
        &settled_with_reply,
        &mut state,
        42,
        "session-1",
    )
    .expect("telegram reply should forward after known status");

    assert!(changed);
    assert_eq!(telegram.sent_texts.borrow()[0], "Telegram reply");
    assert_eq!(state.forward_next_assistant_message_session_id, None);
}

#[test]
fn telegram_unknown_first_status_preserves_old_turn_boundary_until_known_reply() {
    let termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Unknown,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Old turn under future status".to_owned(),
                }],
            },
        },
    };
    let mut state = TelegramBotState::default();

    let plan = prepare_assistant_forwarding_for_telegram_prompt(&termal, "session-1")
        .expect("prepare should succeed");
    assert!(apply_assistant_forwarding_plan(&mut state, plan));
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("unknown status should create an active baseline");
    assert_eq!(cursor.message_id.as_deref(), Some("message-1"));
    assert!(cursor.baseline_while_active);

    let telegram = FakeTelegramSender::new(None);
    let active_old_turn = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Active,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Old turn still active".to_owned(),
                }],
            },
        },
    };

    let changed = forward_new_assistant_message_if_any(
        &telegram,
        &active_old_turn,
        &mut state,
        42,
        "session-1",
    )
    .expect("active status should keep boundary");

    assert!(changed);
    assert!(telegram.sent_texts.borrow().is_empty());

    let settled_with_reply = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![
                    TelegramSessionFetchMessage::Text {
                        id: "message-1".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Old turn settled".to_owned(),
                    },
                    TelegramSessionFetchMessage::Text {
                        id: "message-2".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Telegram reply after future status".to_owned(),
                    },
                ],
            },
        },
    };

    let changed = forward_new_assistant_message_if_any(
        &telegram,
        &settled_with_reply,
        &mut state,
        42,
        "session-1",
    )
    .expect("telegram reply should forward after known status");

    assert!(changed);
    assert_eq!(
        telegram.sent_texts.borrow()[0],
        "Telegram reply after future status"
    );
    assert_eq!(state.forward_next_assistant_message_session_id, None);
}

#[test]
fn telegram_forwarder_drains_armed_session_before_digest_primary() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramSessionReaderById {
        responses: HashMap::from([
            (
                "session-1".to_owned(),
                TelegramSessionFetchResponse {
                    session: TelegramSessionFetchSession {
                        status: TelegramSessionStatus::Idle,
                        messages: vec![TelegramSessionFetchMessage::Text {
                            id: "message-1".to_owned(),
                            author: "assistant".to_owned(),
                            text: "Telegram-originated reply".to_owned(),
                        }],
                    },
                },
            ),
            (
                "session-2".to_owned(),
                TelegramSessionFetchResponse {
                    session: TelegramSessionFetchSession {
                        status: TelegramSessionStatus::Idle,
                        messages: vec![TelegramSessionFetchMessage::Text {
                            id: "message-2".to_owned(),
                            author: "assistant".to_owned(),
                            text: "Digest primary reply".to_owned(),
                        }],
                    },
                },
            ),
        ]),
    };
    let mut state = TelegramBotState {
        forward_next_assistant_message_session_id: Some("session-1".to_owned()),
        ..TelegramBotState::default()
    };

    let changed =
        forward_relevant_assistant_messages(&telegram, &termal, &mut state, 42, Some("session-2"));

    assert!(changed);
    assert_eq!(
        telegram.sent_texts.borrow().as_slice(),
        [
            "Telegram-originated reply".to_owned(),
            telegram_turn_settled_footer(&TelegramSessionStatus::Idle).to_owned()
        ]
    );
    assert_eq!(
        state
            .assistant_forwarding_cursors
            .get("session-1")
            .and_then(|cursor| cursor.message_id.as_deref()),
        Some("message-1")
    );
    assert_eq!(state.forward_next_assistant_message_session_id, None);
}

#[test]
fn telegram_forwarder_checks_digest_primary_when_armed_session_makes_no_progress() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramSessionReaderById {
        responses: HashMap::from([
            (
                "session-1".to_owned(),
                TelegramSessionFetchResponse {
                    session: TelegramSessionFetchSession {
                        status: TelegramSessionStatus::Approval,
                        messages: vec![],
                    },
                },
            ),
            (
                "session-2".to_owned(),
                TelegramSessionFetchResponse {
                    session: TelegramSessionFetchSession {
                        status: TelegramSessionStatus::Idle,
                        messages: vec![
                            TelegramSessionFetchMessage::Text {
                                id: "baseline".to_owned(),
                                author: "assistant".to_owned(),
                                text: "Baseline".to_owned(),
                            },
                            TelegramSessionFetchMessage::Text {
                                id: "message-2".to_owned(),
                                author: "assistant".to_owned(),
                                text: "Digest primary reply".to_owned(),
                            },
                        ],
                    },
                },
            ),
        ]),
    };
    let mut state = TelegramBotState {
        forward_next_assistant_message_session_ids: vec!["session-1".to_owned()],
        forward_next_assistant_message_session_id: Some("session-1".to_owned()),
        assistant_forwarding_cursors: HashMap::from([(
            "session-2".to_owned(),
            TelegramAssistantForwardingCursor {
                message_id: Some("baseline".to_owned()),
                text_chars: Some("Baseline".chars().count()),
                resend_if_grown: false,
                sent_chunks: None,
                failed_chunk_send_attempts: None,
                footer_pending: false,
                baseline_while_active: false,
            },
        )]),
        ..TelegramBotState::default()
    };

    let changed =
        forward_relevant_assistant_messages(&telegram, &termal, &mut state, 42, Some("session-2"));

    assert!(changed);
    assert_eq!(telegram.sent_texts.borrow()[0], "Digest primary reply");
    assert_eq!(
        state.forward_next_assistant_message_session_id.as_deref(),
        Some("session-1")
    );
    assert!(
        state
            .forward_next_assistant_message_session_ids
            .iter()
            .any(|session_id| session_id == "session-1")
    );
}

#[test]
fn telegram_forwarder_checks_digest_primary_when_armed_session_only_updates_baseline() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramSessionReaderById {
        responses: HashMap::from([
            (
                "session-1".to_owned(),
                TelegramSessionFetchResponse {
                    session: TelegramSessionFetchSession {
                        status: TelegramSessionStatus::Active,
                        messages: vec![TelegramSessionFetchMessage::Text {
                            id: "old-turn".to_owned(),
                            author: "assistant".to_owned(),
                            text: "Still streaming old turn".to_owned(),
                        }],
                    },
                },
            ),
            (
                "session-2".to_owned(),
                TelegramSessionFetchResponse {
                    session: TelegramSessionFetchSession {
                        status: TelegramSessionStatus::Idle,
                        messages: vec![
                            TelegramSessionFetchMessage::Text {
                                id: "baseline".to_owned(),
                                author: "assistant".to_owned(),
                                text: "Baseline".to_owned(),
                            },
                            TelegramSessionFetchMessage::Text {
                                id: "message-2".to_owned(),
                                author: "assistant".to_owned(),
                                text: "Digest primary reply".to_owned(),
                            },
                        ],
                    },
                },
            ),
        ]),
    };
    let mut state = TelegramBotState {
        forward_next_assistant_message_session_ids: vec!["session-1".to_owned()],
        forward_next_assistant_message_session_id: Some("session-1".to_owned()),
        assistant_forwarding_cursors: HashMap::from([
            (
                "session-1".to_owned(),
                TelegramAssistantForwardingCursor {
                    message_id: None,
                    text_chars: None,
                    resend_if_grown: false,
                    sent_chunks: None,
                    failed_chunk_send_attempts: None,
                    footer_pending: false,
                    baseline_while_active: true,
                },
            ),
            (
                "session-2".to_owned(),
                TelegramAssistantForwardingCursor {
                    message_id: Some("baseline".to_owned()),
                    text_chars: Some("Baseline".chars().count()),
                    resend_if_grown: false,
                    sent_chunks: None,
                    failed_chunk_send_attempts: None,
                    footer_pending: false,
                    baseline_while_active: false,
                },
            ),
        ]),
        ..TelegramBotState::default()
    };

    let changed =
        forward_relevant_assistant_messages(&telegram, &termal, &mut state, 42, Some("session-2"));

    assert!(changed);
    assert_eq!(telegram.sent_texts.borrow()[0], "Digest primary reply");
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("armed session should keep cursor");
    assert_eq!(cursor.message_id.as_deref(), Some("old-turn"));
    assert_eq!(
        cursor.text_chars,
        Some("Still streaming old turn".chars().count())
    );
    assert!(cursor.baseline_while_active);
    assert!(
        state
            .forward_next_assistant_message_session_ids
            .iter()
            .any(|session_id| session_id == "session-1")
    );
}

#[test]
fn telegram_forwarder_keeps_armed_session_across_digest_primary_switch() {
    let telegram = FakeTelegramSender::new(None);
    let first_poll = FakeTelegramSessionReaderById {
        responses: HashMap::from([
            (
                "session-a".to_owned(),
                TelegramSessionFetchResponse {
                    session: TelegramSessionFetchSession {
                        status: TelegramSessionStatus::Active,
                        messages: vec![TelegramSessionFetchMessage::Text {
                            id: "old-turn-a".to_owned(),
                            author: "assistant".to_owned(),
                            text: "Old turn from armed session".to_owned(),
                        }],
                    },
                },
            ),
            (
                "session-b".to_owned(),
                TelegramSessionFetchResponse {
                    session: TelegramSessionFetchSession {
                        status: TelegramSessionStatus::Idle,
                        messages: vec![
                            TelegramSessionFetchMessage::Text {
                                id: "baseline-b".to_owned(),
                                author: "assistant".to_owned(),
                                text: "Baseline B".to_owned(),
                            },
                            TelegramSessionFetchMessage::Text {
                                id: "message-b".to_owned(),
                                author: "assistant".to_owned(),
                                text: "Digest primary reply".to_owned(),
                            },
                        ],
                    },
                },
            ),
        ]),
    };
    let mut state = TelegramBotState {
        forward_next_assistant_message_session_ids: vec!["session-a".to_owned()],
        forward_next_assistant_message_session_id: Some("session-a".to_owned()),
        assistant_forwarding_cursors: HashMap::from([
            (
                "session-a".to_owned(),
                TelegramAssistantForwardingCursor {
                    message_id: None,
                    text_chars: None,
                    resend_if_grown: false,
                    sent_chunks: None,
                    failed_chunk_send_attempts: None,
                    footer_pending: false,
                    baseline_while_active: true,
                },
            ),
            (
                "session-b".to_owned(),
                TelegramAssistantForwardingCursor {
                    message_id: Some("baseline-b".to_owned()),
                    text_chars: Some("Baseline B".chars().count()),
                    resend_if_grown: false,
                    sent_chunks: None,
                    failed_chunk_send_attempts: None,
                    footer_pending: false,
                    baseline_while_active: false,
                },
            ),
        ]),
        ..TelegramBotState::default()
    };

    let changed = forward_relevant_assistant_messages(
        &telegram,
        &first_poll,
        &mut state,
        42,
        Some("session-b"),
    );

    assert!(changed);
    assert_eq!(telegram.sent_texts.borrow()[0], "Digest primary reply");
    assert!(
        state
            .forward_next_assistant_message_session_ids
            .contains(&"session-a".to_owned())
    );

    let second_poll = FakeTelegramSessionReaderById {
        responses: HashMap::from([(
            "session-a".to_owned(),
            TelegramSessionFetchResponse {
                session: TelegramSessionFetchSession {
                    status: TelegramSessionStatus::Idle,
                    messages: vec![
                        TelegramSessionFetchMessage::Text {
                            id: "old-turn-a".to_owned(),
                            author: "assistant".to_owned(),
                            text: "Old turn from armed session, complete".to_owned(),
                        },
                        TelegramSessionFetchMessage::Text {
                            id: "message-a".to_owned(),
                            author: "assistant".to_owned(),
                            text: "Telegram reply from original armed session".to_owned(),
                        },
                    ],
                },
            },
        )]),
    };

    let changed =
        forward_relevant_assistant_messages(&telegram, &second_poll, &mut state, 42, None);

    assert!(changed);
    assert!(
        telegram
            .sent_texts
            .borrow()
            .contains(&"Telegram reply from original armed session".to_owned())
    );
    assert!(state.forward_next_assistant_message_session_ids.is_empty());
    assert_eq!(state.forward_next_assistant_message_session_id, None);
}

#[test]
fn telegram_forwarder_checks_digest_primary_when_armed_session_errors() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramSessionReaderById {
        responses: HashMap::from([(
            "session-2".to_owned(),
            TelegramSessionFetchResponse {
                session: TelegramSessionFetchSession {
                    status: TelegramSessionStatus::Idle,
                    messages: vec![
                        TelegramSessionFetchMessage::Text {
                            id: "baseline".to_owned(),
                            author: "assistant".to_owned(),
                            text: "Baseline".to_owned(),
                        },
                        TelegramSessionFetchMessage::Text {
                            id: "message-2".to_owned(),
                            author: "assistant".to_owned(),
                            text: "Digest primary reply".to_owned(),
                        },
                    ],
                },
            },
        )]),
    };
    let mut state = TelegramBotState {
        forward_next_assistant_message_session_ids: vec!["missing-session".to_owned()],
        forward_next_assistant_message_session_id: Some("missing-session".to_owned()),
        assistant_forwarding_cursors: HashMap::from([(
            "session-2".to_owned(),
            TelegramAssistantForwardingCursor {
                message_id: Some("baseline".to_owned()),
                text_chars: Some("Baseline".chars().count()),
                resend_if_grown: false,
                sent_chunks: None,
                failed_chunk_send_attempts: None,
                footer_pending: false,
                baseline_while_active: false,
            },
        )]),
        ..TelegramBotState::default()
    };

    let changed =
        forward_relevant_assistant_messages(&telegram, &termal, &mut state, 42, Some("session-2"));

    assert!(changed);
    assert_eq!(telegram.sent_texts.borrow()[0], "Digest primary reply");
    assert!(
        state
            .forward_next_assistant_message_session_ids
            .iter()
            .any(|session_id| session_id == "missing-session")
    );
}

#[test]
fn telegram_forwarder_suppresses_digest_primary_after_visible_armed_footer_send_error() {
    let telegram = FakeTelegramSender::new(Some(2));
    let termal = FakeTelegramSessionReaderById {
        responses: HashMap::from([
            (
                "session-1".to_owned(),
                TelegramSessionFetchResponse {
                    session: TelegramSessionFetchSession {
                        status: TelegramSessionStatus::Idle,
                        messages: vec![TelegramSessionFetchMessage::Text {
                            id: "message-1".to_owned(),
                            author: "assistant".to_owned(),
                            text: "Armed reply".to_owned(),
                        }],
                    },
                },
            ),
            (
                "session-2".to_owned(),
                TelegramSessionFetchResponse {
                    session: TelegramSessionFetchSession {
                        status: TelegramSessionStatus::Idle,
                        messages: vec![
                            TelegramSessionFetchMessage::Text {
                                id: "baseline".to_owned(),
                                author: "assistant".to_owned(),
                                text: "Baseline".to_owned(),
                            },
                            TelegramSessionFetchMessage::Text {
                                id: "message-2".to_owned(),
                                author: "assistant".to_owned(),
                                text: "Digest primary reply".to_owned(),
                            },
                        ],
                    },
                },
            ),
        ]),
    };
    let mut state = TelegramBotState {
        forward_next_assistant_message_session_ids: vec!["session-1".to_owned()],
        forward_next_assistant_message_session_id: Some("session-1".to_owned()),
        assistant_forwarding_cursors: HashMap::from([(
            "session-2".to_owned(),
            TelegramAssistantForwardingCursor {
                message_id: Some("baseline".to_owned()),
                text_chars: Some("Baseline".chars().count()),
                resend_if_grown: false,
                sent_chunks: None,
                failed_chunk_send_attempts: None,
                footer_pending: false,
                baseline_while_active: false,
            },
        )]),
        ..TelegramBotState::default()
    };

    let changed =
        forward_relevant_assistant_messages(&telegram, &termal, &mut state, 42, Some("session-2"));

    assert!(changed);
    assert_eq!(
        telegram.sent_texts.borrow().as_slice(),
        ["Armed reply".to_owned()]
    );
    let armed_cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("footer retry should keep armed session cursor");
    assert!(armed_cursor.footer_pending);
    assert!(
        state
            .assistant_forwarding_cursors
            .get("session-2")
            .is_some_and(|cursor| cursor.message_id.as_deref() == Some("baseline"))
    );

    let changed =
        forward_relevant_assistant_messages(&telegram, &termal, &mut state, 42, Some("session-2"));

    assert!(changed);
    assert_eq!(
        telegram.sent_texts.borrow().as_slice(),
        [
            "Armed reply".to_owned(),
            telegram_turn_settled_footer(&TelegramSessionStatus::Idle).to_owned()
        ]
    );
    assert!(
        state
            .assistant_forwarding_cursors
            .get("session-1")
            .is_some_and(|cursor| !cursor.footer_pending)
    );
}

#[test]
fn telegram_forwarder_suppresses_digest_primary_after_armed_first_chunk_send_error() {
    let telegram = FakeTelegramSender::new(Some(1));
    let termal = FakeTelegramSessionReaderById {
        responses: HashMap::from([
            (
                "session-1".to_owned(),
                TelegramSessionFetchResponse {
                    session: TelegramSessionFetchSession {
                        status: TelegramSessionStatus::Idle,
                        messages: vec![TelegramSessionFetchMessage::Text {
                            id: "message-1".to_owned(),
                            author: "assistant".to_owned(),
                            text: "Armed reply".to_owned(),
                        }],
                    },
                },
            ),
            (
                "session-2".to_owned(),
                TelegramSessionFetchResponse {
                    session: TelegramSessionFetchSession {
                        status: TelegramSessionStatus::Idle,
                        messages: vec![
                            TelegramSessionFetchMessage::Text {
                                id: "baseline".to_owned(),
                                author: "assistant".to_owned(),
                                text: "Baseline".to_owned(),
                            },
                            TelegramSessionFetchMessage::Text {
                                id: "message-2".to_owned(),
                                author: "assistant".to_owned(),
                                text: "Digest primary reply".to_owned(),
                            },
                        ],
                    },
                },
            ),
        ]),
    };
    let mut state = TelegramBotState {
        forward_next_assistant_message_session_ids: vec!["session-1".to_owned()],
        forward_next_assistant_message_session_id: Some("session-1".to_owned()),
        assistant_forwarding_cursors: HashMap::from([(
            "session-2".to_owned(),
            TelegramAssistantForwardingCursor {
                message_id: Some("baseline".to_owned()),
                text_chars: Some("Baseline".chars().count()),
                resend_if_grown: false,
                sent_chunks: None,
                failed_chunk_send_attempts: None,
                footer_pending: false,
                baseline_while_active: false,
            },
        )]),
        ..TelegramBotState::default()
    };

    let changed =
        forward_relevant_assistant_messages(&telegram, &termal, &mut state, 42, Some("session-2"));

    assert!(changed);
    assert!(telegram.sent_texts.borrow().is_empty());
    assert_eq!(telegram.send_attempts.get(), 1);
    let armed_cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("failed first chunk should keep retry cursor");
    assert_eq!(armed_cursor.message_id.as_deref(), Some("message-1"));
    assert_eq!(armed_cursor.sent_chunks, Some(0));
    assert_eq!(armed_cursor.failed_chunk_send_attempts, Some(1));
    assert!(
        state
            .assistant_forwarding_cursors
            .get("session-2")
            .is_some_and(|cursor| cursor.message_id.as_deref() == Some("baseline"))
    );
    assert!(
        state
            .forward_next_assistant_message_session_ids
            .iter()
            .any(|session_id| session_id == "session-1")
    );
}

#[test]
fn telegram_forwarder_skips_first_chunk_after_repeated_send_failures() {
    let telegram =
        FakeTelegramSender::failing_first_attempts(TELEGRAM_ASSISTANT_CHUNK_SEND_FAILURE_LIMIT);
    let termal = FakeTelegramSessionReaderById {
        responses: HashMap::from([(
            "session-1".to_owned(),
            TelegramSessionFetchResponse {
                session: TelegramSessionFetchSession {
                    status: TelegramSessionStatus::Idle,
                    messages: vec![TelegramSessionFetchMessage::Text {
                        id: "message-1".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Armed reply".to_owned(),
                    }],
                },
            },
        )]),
    };
    let mut state = TelegramBotState {
        forward_next_assistant_message_session_ids: vec!["session-1".to_owned()],
        forward_next_assistant_message_session_id: Some("session-1".to_owned()),
        ..TelegramBotState::default()
    };

    for expected_attempts in 1..TELEGRAM_ASSISTANT_CHUNK_SEND_FAILURE_LIMIT {
        let changed = forward_relevant_assistant_messages(&telegram, &termal, &mut state, 42, None);

        assert!(changed);
        assert!(telegram.sent_texts.borrow().is_empty());
        let cursor = state
            .assistant_forwarding_cursors
            .get("session-1")
            .expect("failed chunk should persist retry cursor");
        assert_eq!(cursor.sent_chunks, Some(0));
        assert_eq!(cursor.failed_chunk_send_attempts, Some(expected_attempts));
        assert!(
            state
                .forward_next_assistant_message_session_ids
                .contains(&"session-1".to_owned())
        );
    }

    let changed = forward_relevant_assistant_messages(&telegram, &termal, &mut state, 42, None);

    assert!(changed);
    assert_eq!(
        telegram.sent_texts.borrow().as_slice(),
        [
            telegram_assistant_chunk_skipped_notice(
                0,
                1,
                TELEGRAM_ASSISTANT_CHUNK_SEND_FAILURE_LIMIT,
            ),
            telegram_turn_settled_footer(&TelegramSessionStatus::Idle).to_owned()
        ]
    );
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("skipped message should persist completed cursor");
    assert_eq!(cursor.message_id.as_deref(), Some("message-1"));
    assert_eq!(cursor.sent_chunks, None);
    assert_eq!(cursor.failed_chunk_send_attempts, None);
    assert!(!cursor.footer_pending);
    assert!(state.forward_next_assistant_message_session_ids.is_empty());
    assert_eq!(state.forward_next_assistant_message_session_id, None);
}

#[test]
fn telegram_forwarder_resumes_long_armed_reply_after_content_chunk_failure() {
    let long_reply = format!(
        "{}{}",
        "a".repeat(TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS),
        "b".repeat(8)
    );
    let long_reply_chunks = chunk_telegram_message_text(&long_reply);
    assert_eq!(long_reply_chunks.len(), 2);

    let telegram = FakeTelegramSender::new(Some(2));
    let termal = FakeTelegramSessionReaderById {
        responses: HashMap::from([
            (
                "session-1".to_owned(),
                TelegramSessionFetchResponse {
                    session: TelegramSessionFetchSession {
                        status: TelegramSessionStatus::Idle,
                        messages: vec![TelegramSessionFetchMessage::Text {
                            id: "message-1".to_owned(),
                            author: "assistant".to_owned(),
                            text: long_reply.clone(),
                        }],
                    },
                },
            ),
            (
                "session-2".to_owned(),
                TelegramSessionFetchResponse {
                    session: TelegramSessionFetchSession {
                        status: TelegramSessionStatus::Idle,
                        messages: vec![
                            TelegramSessionFetchMessage::Text {
                                id: "baseline".to_owned(),
                                author: "assistant".to_owned(),
                                text: "Baseline".to_owned(),
                            },
                            TelegramSessionFetchMessage::Text {
                                id: "message-2".to_owned(),
                                author: "assistant".to_owned(),
                                text: "Digest primary reply".to_owned(),
                            },
                        ],
                    },
                },
            ),
        ]),
    };
    let mut state = TelegramBotState {
        forward_next_assistant_message_session_ids: vec!["session-1".to_owned()],
        forward_next_assistant_message_session_id: Some("session-1".to_owned()),
        assistant_forwarding_cursors: HashMap::from([(
            "session-2".to_owned(),
            TelegramAssistantForwardingCursor {
                message_id: Some("baseline".to_owned()),
                text_chars: Some("Baseline".chars().count()),
                resend_if_grown: false,
                sent_chunks: None,
                failed_chunk_send_attempts: None,
                footer_pending: false,
                baseline_while_active: false,
            },
        )]),
        ..TelegramBotState::default()
    };

    let changed =
        forward_relevant_assistant_messages(&telegram, &termal, &mut state, 42, Some("session-2"));

    assert!(changed);
    assert_eq!(
        telegram.sent_texts.borrow().as_slice(),
        [long_reply_chunks[0].clone()]
    );
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("partial chunk progress should persist");
    assert_eq!(cursor.message_id.as_deref(), Some("message-1"));
    assert_eq!(cursor.text_chars, Some(long_reply.chars().count()));
    assert_eq!(cursor.sent_chunks, Some(1));
    assert!(
        state
            .assistant_forwarding_cursors
            .get("session-2")
            .is_some_and(|cursor| cursor.message_id.as_deref() == Some("baseline"))
    );

    let changed =
        forward_relevant_assistant_messages(&telegram, &termal, &mut state, 42, Some("session-2"));

    assert!(changed);
    assert_eq!(
        telegram.sent_texts.borrow().as_slice(),
        [
            long_reply_chunks[0].clone(),
            long_reply_chunks[1].clone(),
            telegram_turn_settled_footer(&TelegramSessionStatus::Idle).to_owned()
        ]
    );
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("completed long message should persist cursor");
    assert_eq!(cursor.sent_chunks, None);
    assert!(
        !state
            .forward_next_assistant_message_session_ids
            .iter()
            .any(|session_id| session_id == "session-1")
    );
}

#[test]
fn telegram_forwarder_drains_multiple_armed_sessions() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramSessionReaderById {
        responses: HashMap::from([
            (
                "session-1".to_owned(),
                TelegramSessionFetchResponse {
                    session: TelegramSessionFetchSession {
                        status: TelegramSessionStatus::Idle,
                        messages: vec![TelegramSessionFetchMessage::Text {
                            id: "message-1".to_owned(),
                            author: "assistant".to_owned(),
                            text: "First armed reply".to_owned(),
                        }],
                    },
                },
            ),
            (
                "session-2".to_owned(),
                TelegramSessionFetchResponse {
                    session: TelegramSessionFetchSession {
                        status: TelegramSessionStatus::Approval,
                        messages: vec![],
                    },
                },
            ),
        ]),
    };
    let mut state = TelegramBotState {
        forward_next_assistant_message_session_ids: vec![
            "session-1".to_owned(),
            "session-2".to_owned(),
        ],
        forward_next_assistant_message_session_id: Some("session-2".to_owned()),
        ..TelegramBotState::default()
    };

    let changed = forward_relevant_assistant_messages(&telegram, &termal, &mut state, 42, None);

    assert!(changed);
    assert_eq!(
        telegram.sent_texts.borrow().as_slice(),
        [
            "First armed reply".to_owned(),
            telegram_turn_settled_footer(&TelegramSessionStatus::Idle).to_owned()
        ]
    );
    assert!(
        !state
            .forward_next_assistant_message_session_ids
            .iter()
            .any(|session_id| session_id == "session-1")
    );
    assert!(
        state
            .forward_next_assistant_message_session_ids
            .iter()
            .any(|session_id| session_id == "session-2")
    );
    assert_eq!(
        state.forward_next_assistant_message_session_id.as_deref(),
        Some("session-2")
    );
}

#[test]
fn telegram_forwarder_drains_multiple_armed_sessions_in_prompt_order() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramSessionReaderById {
        responses: HashMap::from([
            (
                "session-a".to_owned(),
                TelegramSessionFetchResponse {
                    session: TelegramSessionFetchSession {
                        status: TelegramSessionStatus::Idle,
                        messages: vec![TelegramSessionFetchMessage::Text {
                            id: "message-a".to_owned(),
                            author: "assistant".to_owned(),
                            text: "Reply from session A".to_owned(),
                        }],
                    },
                },
            ),
            (
                "session-b".to_owned(),
                TelegramSessionFetchResponse {
                    session: TelegramSessionFetchSession {
                        status: TelegramSessionStatus::Idle,
                        messages: vec![TelegramSessionFetchMessage::Text {
                            id: "message-b".to_owned(),
                            author: "assistant".to_owned(),
                            text: "Reply from session B".to_owned(),
                        }],
                    },
                },
            ),
        ]),
    };
    let mut state = TelegramBotState {
        forward_next_assistant_message_session_ids: vec![
            "session-b".to_owned(),
            "session-a".to_owned(),
        ],
        forward_next_assistant_message_session_id: Some("session-a".to_owned()),
        ..TelegramBotState::default()
    };

    let changed = forward_relevant_assistant_messages(&telegram, &termal, &mut state, 42, None);

    assert!(changed);
    assert_eq!(
        telegram.sent_texts.borrow().as_slice(),
        [
            "Reply from session B".to_owned(),
            telegram_turn_settled_footer(&TelegramSessionStatus::Idle).to_owned(),
            "Reply from session A".to_owned(),
            telegram_turn_settled_footer(&TelegramSessionStatus::Idle).to_owned()
        ]
    );
    assert!(state.forward_next_assistant_message_session_ids.is_empty());
    assert_eq!(state.forward_next_assistant_message_session_id, None);
}

#[test]
fn telegram_assistant_forwarding_cursors_are_scoped_per_session() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "session-2-message".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Other session history".to_owned(),
                }],
            },
        },
    };
    let mut state = TelegramBotState {
        assistant_forwarding_cursors: HashMap::from([(
            "session-1".to_owned(),
            TelegramAssistantForwardingCursor {
                message_id: Some("session-1-baseline".to_owned()),
                text_chars: Some("Session one baseline".chars().count()),
                resend_if_grown: false,
                sent_chunks: None,
                failed_chunk_send_attempts: None,
                footer_pending: false,
                baseline_while_active: false,
            },
        )]),
        ..TelegramBotState::default()
    };

    let changed =
        forward_new_assistant_message_if_any(&telegram, &termal, &mut state, 42, "session-2")
            .expect("baseline should succeed");

    assert!(changed);
    assert!(telegram.sent_texts.borrow().is_empty());
    let session_one_cursor = resolve_assistant_forwarding_cursor(&state, "session-1", &[]);
    assert_eq!(
        session_one_cursor.message_id.as_deref(),
        Some("session-1-baseline")
    );
    let session_two_cursor = resolve_assistant_forwarding_cursor(
        &state,
        "session-2",
        &[TelegramSessionFetchMessage::Text {
            id: "session-2-message".to_owned(),
            author: "assistant".to_owned(),
            text: "Other session history".to_owned(),
        }],
    );
    assert_eq!(
        session_two_cursor.message_id.as_deref(),
        Some("session-2-message")
    );
}

#[test]
fn telegram_assistant_forwarding_legacy_cursor_does_not_leak_to_other_session() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "session-2-message".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Other session history".to_owned(),
                }],
            },
        },
    };
    let mut state = TelegramBotState {
        last_forwarded_assistant_message_id: Some("session-1-baseline".to_owned()),
        last_forwarded_assistant_message_text_chars: Some("Session one baseline".chars().count()),
        ..TelegramBotState::default()
    };

    let changed =
        forward_new_assistant_message_if_any(&telegram, &termal, &mut state, 42, "session-2")
            .expect("baseline should succeed");

    assert!(changed);
    assert!(telegram.sent_texts.borrow().is_empty());
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-2")
        .expect("session-2 should get its own cursor");
    assert_eq!(cursor.message_id.as_deref(), Some("session-2-message"));
    assert_ne!(cursor.message_id.as_deref(), Some("session-1-baseline"));
}

#[test]
fn telegram_assistant_forwarding_legacy_cursor_does_not_request_resend() {
    let state = TelegramBotState {
        last_forwarded_assistant_message_id: Some("legacy-message".to_owned()),
        last_forwarded_assistant_message_text_chars: Some("Legacy text".chars().count()),
        ..TelegramBotState::default()
    };

    let cursor = resolve_assistant_forwarding_cursor(
        &state,
        "session-1",
        &[TelegramSessionFetchMessage::Text {
            id: "legacy-message".to_owned(),
            author: "assistant".to_owned(),
            text: "Legacy text has grown".to_owned(),
        }],
    );

    assert_eq!(cursor.message_id.as_deref(), Some("legacy-message"));
    assert_eq!(cursor.text_chars, Some("Legacy text".chars().count()));
    assert!(!cursor.resend_if_grown);
}

#[test]
fn telegram_session_cursor_updates_do_not_clobber_legacy_mirror() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "session-2-message".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Other session history".to_owned(),
                }],
            },
        },
    };
    let mut state = TelegramBotState {
        last_forwarded_assistant_message_id: Some("legacy-session-message".to_owned()),
        last_forwarded_assistant_message_text_chars: Some(11),
        ..TelegramBotState::default()
    };

    let changed =
        forward_new_assistant_message_if_any(&telegram, &termal, &mut state, 42, "session-2")
            .expect("baseline should succeed");

    assert!(changed);
    assert!(telegram.sent_texts.borrow().is_empty());
    assert_eq!(
        state.last_forwarded_assistant_message_id.as_deref(),
        Some("legacy-session-message")
    );
    assert_eq!(state.last_forwarded_assistant_message_text_chars, Some(11));
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-2")
        .expect("session cursor should be authoritative");
    assert_eq!(cursor.message_id.as_deref(), Some("session-2-message"));
}

#[test]
fn telegram_armed_session_without_assistant_text_clears_to_avoid_starvation() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![],
            },
        },
    };
    let mut state = TelegramBotState {
        forward_next_assistant_message_session_id: Some("session-1".to_owned()),
        ..TelegramBotState::default()
    };

    let changed =
        forward_new_assistant_message_if_any(&telegram, &termal, &mut state, 42, "session-1")
            .expect("empty settled session should not fail");

    assert!(changed);
    assert!(telegram.sent_texts.borrow().is_empty());
    assert_eq!(state.last_forwarded_assistant_message_id, None);
    assert_eq!(state.forward_next_assistant_message_session_id, None);
}

#[test]
fn telegram_armed_session_keeps_approval_pause_until_reply_arrives() {
    let telegram = FakeTelegramSender::new(None);
    let approval_pause = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Approval,
                messages: vec![],
            },
        },
    };
    let mut state = TelegramBotState {
        forward_next_assistant_message_session_id: Some("session-1".to_owned()),
        ..TelegramBotState::default()
    };

    let waiting = forward_new_assistant_message_if_any(
        &telegram,
        &approval_pause,
        &mut state,
        42,
        "session-1",
    )
    .expect("approval pause should not fail");

    assert!(!waiting);
    assert!(telegram.sent_texts.borrow().is_empty());
    assert_eq!(
        state.forward_next_assistant_message_session_id.as_deref(),
        Some("session-1")
    );

    let post_approval_reply = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Approved reply".to_owned(),
                }],
            },
        },
    };

    let forwarded = forward_new_assistant_message_if_any(
        &telegram,
        &post_approval_reply,
        &mut state,
        42,
        "session-1",
    )
    .expect("post-approval reply should forward");

    assert!(forwarded);
    assert_eq!(telegram.sent_texts.borrow()[0], "Approved reply");
    assert_eq!(state.forward_next_assistant_message_session_id, None);
}

#[test]
fn telegram_prompt_without_active_session_persists_stale_project_cleanup() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![Ok(telegram_project_digest(None))],
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: Vec::new(),
            },
        },
    );
    let config = telegram_test_config();
    let mut state = TelegramBotState {
        selected_project_id: Some("stale-project".to_owned()),
        selected_session_id: Some("session-1".to_owned()),
        last_digest_hash: Some("old-digest".to_owned()),
        last_digest_message_id: Some(10),
        ..TelegramBotState::default()
    };

    let changed =
        forward_telegram_text_to_project(&telegram, &termal, &config, &mut state, 42, "from chat")
            .expect("prompt forwarding should report missing active session");

    assert!(changed);
    assert_eq!(state.selected_project_id, None);
    assert_eq!(state.selected_session_id, None);
    assert_eq!(state.last_digest_hash, None);
    assert_eq!(state.last_digest_message_id, None);
    assert!(telegram.sent_texts.borrow()[0].contains("No active project session"));
}

#[test]
fn telegram_unknown_forwarded_char_count_reforwards_tracked_message_once() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Recovered full reply".to_owned(),
                }],
            },
        },
    };
    let mut state = TelegramBotState {
        last_forwarded_assistant_message_id: Some("message-1".to_owned()),
        last_forwarded_assistant_message_text_chars: None,
        ..TelegramBotState::default()
    };

    let forwarded =
        forward_new_assistant_message_if_any(&telegram, &termal, &mut state, 42, "session-1")
            .expect("forwarding should succeed");

    assert!(forwarded);
    assert_eq!(telegram.sent_texts.borrow()[0], "Recovered full reply");
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("reforward should persist session cursor");
    assert_eq!(cursor.message_id.as_deref(), Some("message-1"));
    assert_eq!(
        cursor.text_chars,
        Some("Recovered full reply".chars().count())
    );
}

#[test]
fn telegram_unknown_session_status_does_not_forward_assistant_text() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Unknown,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Do not forward yet".to_owned(),
                }],
            },
        },
    };
    let mut state = TelegramBotState::default();

    let forwarded =
        forward_new_assistant_message_if_any(&telegram, &termal, &mut state, 42, "session-1")
            .expect("forwarding should not fail");

    assert!(!forwarded);
    assert!(telegram.sent_texts.borrow().is_empty());
}

#[test]
fn telegram_session_fetch_projection_decodes_sample_json_statuses() {
    let response: TelegramSessionFetchResponse = serde_json::from_value(json!({
        "session": {
            "status": "idle",
            "messages": [
                {
                    "type": "text",
                    "id": "message-1",
                    "author": "assistant",
                    "text": "Ready."
                },
                {
                    "type": "thinking",
                    "id": "thinking-1",
                    "author": "assistant",
                    "title": "Plan",
                    "lines": ["Inspecting."]
                }
            ],
            "ignoredField": true
        }
    }))
    .expect("sample session JSON should decode");

    assert_eq!(response.session.status, TelegramSessionStatus::Idle);
    assert_eq!(response.session.messages.len(), 2);
    assert!(matches!(
        &response.session.messages[0],
        TelegramSessionFetchMessage::Text { id, author, text }
            if id == "message-1" && author == "assistant" && text == "Ready."
    ));
    assert!(matches!(
        response.session.messages[1],
        TelegramSessionFetchMessage::Other
    ));

    let unknown: TelegramSessionFetchResponse = serde_json::from_value(json!({
        "session": {
            "status": "paused-for-future-state",
            "messages": []
        }
    }))
    .expect("unknown session status should decode");
    assert_eq!(unknown.session.status, TelegramSessionStatus::Unknown);

    let missing: TelegramSessionFetchResponse = serde_json::from_value(json!({
        "session": {
            "messages": []
        }
    }))
    .expect("missing session status should decode");
    assert_eq!(missing.session.status, TelegramSessionStatus::Unknown);
}

#[test]
fn telegram_api_error_envelope_decodes_sample_json() {
    let envelope: TelegramApiEnvelope<Value> = serde_json::from_value(json!({
        "ok": false,
        "error_code": 401,
        "description": "Unauthorized"
    }))
    .expect("Telegram error envelope should decode");

    assert!(!envelope.ok);
    assert_eq!(envelope.error_code, Some(401));
    assert_eq!(envelope.description.as_deref(), Some("Unauthorized"));
    assert!(envelope.result.is_none());
}

#[test]
fn telegram_session_fetch_message_matches_canonical_text_wire_shape() {
    let canonical = Message::Text {
        attachments: Vec::new(),
        id: "message-1".to_owned(),
        timestamp: "2026-05-06T12:00:00Z".to_owned(),
        author: Author::Assistant,
        text: "Canonical assistant text.".to_owned(),
        expanded_text: None,
    };
    let projected: TelegramSessionFetchMessage =
        serde_json::from_value(serde_json::to_value(canonical).expect("message should encode"))
            .expect("canonical text message should decode into Telegram projection");

    assert!(matches!(
        projected,
        TelegramSessionFetchMessage::Text { id, author, text }
            if id == "message-1"
                && author == "assistant"
                && text == "Canonical assistant text."
    ));
}

// Pins the dirty-merge policy for assistant forwarding: the forwarding helper
// can update the persisted cursor after one successful send and then fail on a
// later chunk/message. Callers must still persist that partial progress.
#[test]
fn telegram_assistant_forward_error_marks_state_dirty() {
    let mut dirty = false;

    merge_assistant_forward_result(&mut dirty, Err(anyhow!("second send failed")));

    assert!(dirty);
}

#[test]
fn telegram_message_not_modified_classifier_requires_telegram_400_error() {
    let canonical = anyhow::Error::new(TelegramApiError {
        method: "editMessageText".to_owned(),
        status: StatusCode::BAD_REQUEST,
        error_code: Some(400),
        description: "Bad Request: message is not modified: specified new message content and reply markup are exactly the same as a current content and reply markup of the message".to_owned(),
    });
    assert!(telegram_error_is_message_not_modified(&canonical));

    let wrong_code = anyhow::Error::new(TelegramApiError {
        method: "editMessageText".to_owned(),
        status: StatusCode::TOO_MANY_REQUESTS,
        error_code: Some(429),
        description: "Bad Request: message is not modified, retry later".to_owned(),
    });
    assert!(!telegram_error_is_message_not_modified(&wrong_code));

    let untyped = anyhow!("Bad Request: message is not modified");
    assert!(!telegram_error_is_message_not_modified(&untyped));
}

fn telegram_redaction_token() -> &'static str {
    "123456:ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghi"
}

fn telegram_redaction_secret_35() -> &'static str {
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghi"
}

fn telegram_redaction_secret_34() -> &'static str {
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefgh"
}

#[test]
fn telegram_log_sanitizer_redacts_bot_url_and_truncates() {
    let token = "123456789:ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghi";
    let detail = format!(
        "request failed for https://api.telegram.org/bot{token}/getUpdates: {}",
        "x".repeat(300)
    );
    let sanitized = sanitize_telegram_log_detail(&detail);

    assert!(!sanitized.contains(token));
    assert!(sanitized.contains("/bot<redacted>/getUpdates"));
    assert!(sanitized.ends_with("..."));
    assert!(sanitized.chars().count() <= 259);
}

#[test]
fn telegram_log_sanitizer_redacts_standalone_key_value_token() {
    let token = telegram_redaction_token();

    let standalone =
        sanitize_telegram_log_detail(&format!("Telegram token rejected: botToken={token}."));
    assert!(!standalone.contains(token));
    assert!(standalone.contains("botToken=<redacted>."));
}

#[test]
fn telegram_log_sanitizer_redacts_colon_delimited_key_token() {
    let token = telegram_redaction_token();

    let colon_delimited = sanitize_telegram_log_detail(&format!(
        "Telegram token rejected: botToken:{token}: invalid"
    ));
    assert!(!colon_delimited.contains(token));
    assert!(colon_delimited.contains("botToken:<redacted>: invalid"));
}

#[test]
fn telegram_log_sanitizer_preserves_benign_token_like_values() {
    let benign = "trace [12345:67890123] pid 12345:abcdefgh version 12345:6.7.8.9";
    assert_eq!(sanitize_telegram_log_detail(benign), benign);
}

#[test]
fn telegram_log_sanitizer_preserves_malformed_bot_url() {
    let malformed_url = "request failed for https://api.telegram.org/bot/bot/getMe";
    assert_eq!(sanitize_telegram_log_detail(malformed_url), malformed_url);
}

#[test]
fn telegram_standalone_token_redacts_documented_key_context() {
    let token = telegram_redaction_token();

    let documented = sanitize_telegram_log_detail(&format!("botToken={token}"));
    assert_eq!(documented, "botToken=<redacted>");
}

#[test]
fn telegram_standalone_token_redacts_quoted_json_key_context() {
    let token = telegram_redaction_token();

    let quoted_json = sanitize_telegram_log_detail(&format!("\"botToken\":\"{token}\""));
    assert_eq!(quoted_json, "\"botToken\":\"<redacted>\"");
}

#[test]
fn telegram_standalone_token_redacts_spaced_key_context() {
    let token = telegram_redaction_token();

    let spaced = sanitize_telegram_log_detail(&format!("botToken = {token}"));
    assert_eq!(spaced, "botToken = <redacted>");
}

#[test]
fn telegram_standalone_token_redacts_snake_quoted_key_context() {
    let token = telegram_redaction_token();

    let snake_quoted = sanitize_telegram_log_detail(&format!("bot_token\": \"{token}\""));
    assert_eq!(snake_quoted, "bot_token\": \"<redacted>\"");
}

#[test]
fn telegram_standalone_token_redacts_bearer_context() {
    let token = format!("12345678:{}", telegram_redaction_secret_35());

    let bearer = sanitize_telegram_log_detail(&format!("Authorization: Bearer {token}"));
    assert_eq!(bearer, "Authorization: Bearer <redacted>");
}

#[test]
fn telegram_standalone_token_redacts_multiple_contextual_tokens() {
    let six_digit_token = telegram_redaction_token();
    let eight_digit_token = format!("12345678:{}", telegram_redaction_secret_35());

    let multi = sanitize_telegram_log_detail(&format!(
        "botToken={six_digit_token} telegramBotToken:{eight_digit_token}"
    ));
    assert_eq!(multi, "botToken=<redacted> telegramBotToken:<redacted>");
}

#[test]
fn telegram_standalone_token_ignores_short_bot_id() {
    let secret_35 = telegram_redaction_secret_35();

    let short_bot_id = format!("12345:{secret_35}");
    assert_eq!(
        sanitize_telegram_log_detail(&format!("botToken={short_bot_id}")),
        format!("botToken={short_bot_id}")
    );
}

#[test]
fn telegram_standalone_token_ignores_short_secret() {
    let secret_34 = telegram_redaction_secret_34();

    let short_secret = format!("12345678:{secret_34}");
    assert_eq!(
        sanitize_telegram_log_detail(&format!("botToken={short_secret}")),
        format!("botToken={short_secret}")
    );
}

#[test]
fn telegram_standalone_token_ignores_unanchored_value() {
    let token = telegram_redaction_token();

    let unanchored = format!("trace value {token}");
    assert_eq!(sanitize_telegram_log_detail(&unanchored), unanchored);
}

#[test]
fn telegram_standalone_token_ignores_access_token_key() {
    let token = telegram_redaction_token();

    let foreign_token_key = format!("accessToken={token}");
    assert_eq!(
        sanitize_telegram_log_detail(&foreign_token_key),
        foreign_token_key
    );
}

#[test]
fn telegram_standalone_token_ignores_csrf_token_key() {
    let token = telegram_redaction_token();

    let foreign_spaced_token_key = format!("csrfToken : {token}");
    assert_eq!(
        sanitize_telegram_log_detail(&foreign_spaced_token_key),
        foreign_spaced_token_key
    );
}

#[test]
fn telegram_standalone_token_ignores_generic_token_without_context() {
    let token = telegram_redaction_token();

    let ambiguous_token_key = format!("token={token}");
    assert_eq!(
        sanitize_telegram_log_detail(&ambiguous_token_key),
        ambiguous_token_key
    );
}

#[test]
fn telegram_standalone_token_ignores_false_bearer_prefix() {
    let token = telegram_redaction_token();

    let false_bearer_prefix = format!("notbearer {token}");
    assert_eq!(
        sanitize_telegram_log_detail(&false_bearer_prefix),
        false_bearer_prefix
    );
}

#[test]
fn telegram_standalone_token_redacts_escaped_json_context() {
    let token = telegram_redaction_token();

    let escaped_json = sanitize_telegram_log_detail(&format!("\\\"botToken\\\": \\\"{token}\\\""));
    assert_eq!(escaped_json, "\\\"botToken\\\": \\\"<redacted>\\\"");
}

#[test]
fn telegram_standalone_token_redacts_bearer_colon_context() {
    let token = telegram_redaction_token();

    let bearer_colon = sanitize_telegram_log_detail(&format!("Authorization: Bearer: {token}"));
    assert_eq!(bearer_colon, "Authorization: Bearer: <redacted>");
}

#[test]
fn telegram_standalone_token_redacts_lower_bearer_colon_context() {
    let token = telegram_redaction_token();

    let lower_bearer_colon = sanitize_telegram_log_detail(&format!("bearer:{token}"));
    assert_eq!(lower_bearer_colon, "bearer:<redacted>");
}

#[test]
fn telegram_standalone_token_redacts_bearer_equals_context() {
    let token = telegram_redaction_token();

    let bearer_equals = sanitize_telegram_log_detail(&format!("Authorization: Bearer = {token}"));
    assert_eq!(bearer_equals, "Authorization: Bearer = <redacted>");
}

#[test]
fn telegram_standalone_token_redacts_authorization_equals_bearer_context() {
    let token = telegram_redaction_token();

    let authorization_equals =
        sanitize_telegram_log_detail(&format!("Authorization=Bearer {token}"));
    assert_eq!(authorization_equals, "Authorization=Bearer <redacted>");
}

#[test]
fn telegram_standalone_token_redacts_lower_bearer_equals_context() {
    let token = telegram_redaction_token();

    let lower_bearer_equals = sanitize_telegram_log_detail(&format!("bearer={token}"));
    assert_eq!(lower_bearer_equals, "bearer=<redacted>");
}

#[test]
fn telegram_standalone_token_redacts_snake_telegram_key_context() {
    let token = telegram_redaction_token();

    let snake = sanitize_telegram_log_detail(&format!("telegram_bot_token={token}"));
    assert_eq!(snake, "telegram_bot_token=<redacted>");
}

#[test]
fn telegram_standalone_token_redacts_camel_telegram_key_context() {
    let token = telegram_redaction_token();

    let camel = sanitize_telegram_log_detail(&format!("telegramBotToken={token}"));
    assert_eq!(camel, "telegramBotToken=<redacted>");
}

#[test]
fn telegram_standalone_token_redacts_env_var_key_context() {
    let token = telegram_redaction_token();

    let env = sanitize_telegram_log_detail(&format!("TERMAL_TELEGRAM_BOT_TOKEN={token}"));
    assert_eq!(env, "TERMAL_TELEGRAM_BOT_TOKEN=<redacted>");
}

fn assert_generic_token_context_is_preserved(context: &str) {
    let token = telegram_redaction_token();
    let detail = format!("{context} token={token}");
    assert_eq!(sanitize_telegram_log_detail(&detail), detail);
}

#[test]
fn telegram_generic_token_ignores_robot_context() {
    assert_generic_token_context_is_preserved("robot pipeline");
}

#[test]
fn telegram_generic_token_ignores_bottom_context() {
    assert_generic_token_context_is_preserved("bottom panel");
}

#[test]
fn telegram_generic_token_ignores_slackbot_context() {
    assert_generic_token_context_is_preserved("slackbot relay");
}

#[test]
fn telegram_generic_token_ignores_botanical_context() {
    assert_generic_token_context_is_preserved("botanical job");
}

#[test]
fn telegram_generic_token_ignores_upper_lower_telegrambot_context() {
    assert_generic_token_context_is_preserved("TELEGRAMbot relay");
}

#[test]
fn telegram_generic_token_ignores_upper_lower_botanical_context() {
    assert_generic_token_context_is_preserved("BOTanical job");
}

#[test]
fn telegram_generic_token_redacts_telegram_api_context() {
    let token = telegram_redaction_token();

    assert_eq!(
        sanitize_telegram_log_detail(&format!("Telegram API error token={token}")),
        "Telegram API error token=<redacted>"
    );
}

#[test]
fn telegram_generic_token_redacts_bot_word_context() {
    let token = telegram_redaction_token();

    assert_eq!(
        sanitize_telegram_log_detail(&format!("bot config token:{token}")),
        "bot config token:<redacted>"
    );
}

#[test]
fn telegram_generic_token_redacts_snake_namespace_context() {
    let token = telegram_redaction_token();

    assert_eq!(
        sanitize_telegram_log_detail(&format!("telegram_bot: {{ token: {token} }}")),
        "telegram_bot: { token: <redacted> }"
    );
}

#[test]
fn telegram_generic_token_redacts_hyphen_namespace_context() {
    let token = telegram_redaction_token();

    assert_eq!(
        sanitize_telegram_log_detail(&format!("telegram-bot token={token}")),
        "telegram-bot token=<redacted>"
    );
}

#[test]
fn telegram_generic_token_redacts_camel_namespace_context() {
    let token = telegram_redaction_token();

    assert_eq!(
        sanitize_telegram_log_detail(&format!("telegramBot token={token}")),
        "telegramBot token=<redacted>"
    );
}

#[test]
fn telegram_prompt_limit_uses_utf8_byte_length() {
    assert!(!telegram_prompt_exceeds_byte_limit(
        &"x".repeat(MAX_DELEGATION_PROMPT_BYTES)
    ));
    assert!(telegram_prompt_exceeds_byte_limit(
        &"x".repeat(MAX_DELEGATION_PROMPT_BYTES + 1)
    ));
    assert!(telegram_prompt_exceeds_byte_limit(&format!(
        "{}界",
        "x".repeat(MAX_DELEGATION_PROMPT_BYTES - 1)
    )));
}

#[test]
fn telegram_settings_request_nulls_clear_optional_fields() {
    let request: UpdateTelegramConfigRequest = serde_json::from_value(json!({
        "botToken": null,
        "defaultProjectId": null,
        "defaultSessionId": null
    }))
    .expect("request should deserialize");

    assert_eq!(request.bot_token, Some(None));
    assert_eq!(request.default_project_id, Some(None));
    assert_eq!(request.default_session_id, Some(None));

    let missing: UpdateTelegramConfigRequest =
        serde_json::from_value(json!({})).expect("request should deserialize");
    assert_eq!(missing.bot_token, None);
    assert_eq!(missing.default_project_id, None);
    assert_eq!(missing.default_session_id, None);

    let test_request: TelegramTestRequest =
        serde_json::from_value(json!({ "useSavedToken": true }))
            .expect("test request should deserialize");
    assert_eq!(test_request.bot_token, None);
    assert!(test_request.use_saved_token);

    let clear_test_request: TelegramTestRequest =
        serde_json::from_value(json!({ "botToken": null }))
            .expect("test request should deserialize");
    assert_eq!(clear_test_request.bot_token, Some(None));
    assert!(!clear_test_request.use_saved_token);
}

#[test]
fn telegram_status_response_keeps_empty_project_list_on_wire() {
    let value = serde_json::to_value(TelegramStatusResponse {
        configured: false,
        enabled: false,
        running: false,
        lifecycle: TelegramLifecycle::Manual,
        linked_chat_id: None,
        bot_token_masked: None,
        subscribed_project_ids: Vec::new(),
        default_project_id: None,
        default_session_id: None,
    })
    .expect("response should serialize");

    assert_eq!(value["lifecycle"], json!("manual"));
    assert_eq!(value["subscribedProjectIds"], json!([]));
}

#[test]
fn telegram_status_response_serializes_in_process_lifecycle() {
    let value = serde_json::to_value(TelegramStatusResponse {
        configured: true,
        enabled: true,
        running: true,
        lifecycle: TelegramLifecycle::InProcess,
        linked_chat_id: None,
        bot_token_masked: Some("****oken".to_owned()),
        subscribed_project_ids: vec!["project-1".to_owned()],
        default_project_id: Some("project-1".to_owned()),
        default_session_id: None,
    })
    .expect("response should serialize");

    assert_eq!(value["lifecycle"], json!("inProcess"));
    assert_eq!(value["running"], json!(true));
}

#[test]
fn telegram_assistant_forwarding_cursor_state_uses_documented_wire_shape() {
    let value = serde_json::to_value(TelegramBotState {
        assistant_forwarding_cursors: HashMap::from([(
            "session-1".to_owned(),
            TelegramAssistantForwardingCursor {
                message_id: Some("message-1".to_owned()),
                text_chars: Some(42),
                resend_if_grown: true,
                sent_chunks: Some(3),
                failed_chunk_send_attempts: Some(2),
                footer_pending: true,
                baseline_while_active: true,
            },
        )]),
        forward_next_assistant_message_session_ids: vec!["session-1".to_owned()],
        selected_project_id: Some("project-2".to_owned()),
        selected_session_id: Some("session-2".to_owned()),
        ..TelegramBotState::default()
    })
    .expect("state should serialize");

    assert_eq!(
        value["assistantForwardingCursors"]["session-1"]["messageId"],
        json!("message-1")
    );
    assert_eq!(
        value["assistantForwardingCursors"]["session-1"]["textChars"],
        json!(42)
    );
    assert_eq!(
        value["assistantForwardingCursors"]["session-1"]["resendIfGrown"],
        json!(true)
    );
    assert_eq!(
        value["assistantForwardingCursors"]["session-1"]["sentChunks"],
        json!(3)
    );
    assert_eq!(
        value["assistantForwardingCursors"]["session-1"]["failedChunkSendAttempts"],
        json!(2)
    );
    assert_eq!(
        value["assistantForwardingCursors"]["session-1"]["footerPending"],
        json!(true)
    );
    assert_eq!(
        value["assistantForwardingCursors"]["session-1"]["baselineWhileActive"],
        json!(true)
    );
    assert_eq!(
        value["forwardNextAssistantMessageSessionIds"],
        json!(["session-1"])
    );
    assert_eq!(value["selectedProjectId"], json!("project-2"));
    assert_eq!(value["selectedSessionId"], json!("session-2"));

    let round_tripped: TelegramBotState =
        serde_json::from_value(value.clone()).expect("state should deserialize");
    let round_tripped_cursor = round_tripped
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("cursor should deserialize");
    assert_eq!(
        round_tripped_cursor.message_id.as_deref(),
        Some("message-1")
    );
    assert_eq!(round_tripped_cursor.text_chars, Some(42));
    assert!(round_tripped_cursor.resend_if_grown);
    assert_eq!(round_tripped_cursor.sent_chunks, Some(3));
    assert_eq!(round_tripped_cursor.failed_chunk_send_attempts, Some(2));
    assert!(round_tripped_cursor.footer_pending);
    assert!(round_tripped_cursor.baseline_while_active);
    assert!(
        round_tripped
            .forward_next_assistant_message_session_ids
            .iter()
            .any(|session_id| session_id == "session-1")
    );
    assert_eq!(
        round_tripped.selected_project_id.as_deref(),
        Some("project-2")
    );
    assert_eq!(
        round_tripped.selected_session_id.as_deref(),
        Some("session-2")
    );
    let reserialized =
        serde_json::to_value(&round_tripped).expect("round-tripped state should serialize");
    assert_eq!(reserialized, value);

    let default_value =
        serde_json::to_value(TelegramBotState::default()).expect("state should serialize");
    assert!(default_value.get("assistantForwardingCursors").is_none());
    assert!(
        default_value
            .get("forwardNextAssistantMessageSessionIds")
            .is_none()
    );
    assert!(default_value.get("selectedProjectId").is_none());
    assert!(default_value.get("selectedSessionId").is_none());
}

fn telegram_ui_relay_config() -> TelegramUiConfig {
    TelegramUiConfig {
        enabled: true,
        bot_token: Some("123456:secret".to_owned()),
        subscribed_project_ids: vec!["project-1".to_owned()],
        default_project_id: None,
        ..TelegramUiConfig::default()
    }
}

fn telegram_ui_relay_file(config: TelegramUiConfig) -> TelegramBotFile {
    TelegramBotFile {
        config,
        state: TelegramBotState::default(),
    }
}

#[test]
fn telegram_ui_file_uses_single_subscribed_project_for_relay_config() {
    let file = telegram_ui_relay_file(telegram_ui_relay_config());
    let config = TelegramBotConfig::from_ui_file("/tmp", &file)
        .expect("single subscribed project should produce relay config");

    assert_eq!(config.project_id, "project-1");
    assert_eq!(config.subscribed_project_ids, vec!["project-1"]);
}

#[test]
fn telegram_ui_file_falls_back_to_single_subscribed_project_for_blank_default() {
    let with_blank_default = telegram_ui_relay_file(TelegramUiConfig {
        default_project_id: Some("   ".to_owned()),
        ..telegram_ui_relay_config()
    });
    let config = TelegramBotConfig::from_ui_file("/tmp", &with_blank_default)
        .expect("blank default should fall back to single subscribed project");

    assert_eq!(config.project_id, "project-1");
}

#[test]
fn telegram_ui_file_requires_project_target_for_relay_config() {
    let without_any_project = telegram_ui_relay_file(TelegramUiConfig {
        subscribed_project_ids: Vec::new(),
        default_project_id: None,
        ..telegram_ui_relay_config()
    });

    assert!(TelegramBotConfig::from_ui_file("/tmp", &without_any_project).is_none());
}

#[test]
fn telegram_ui_file_requires_default_when_multiple_projects_for_relay_config() {
    let with_multiple_projects = telegram_ui_relay_file(TelegramUiConfig {
        subscribed_project_ids: vec!["project-1".to_owned(), "project-2".to_owned()],
        default_project_id: None,
        ..telegram_ui_relay_config()
    });

    assert!(TelegramBotConfig::from_ui_file("/tmp", &with_multiple_projects).is_none());
}

#[test]
fn telegram_ui_file_uses_trimmed_default_project_for_relay_config() {
    let with_default = TelegramBotFile {
        config: TelegramUiConfig {
            default_project_id: Some(" project-1 ".to_owned()),
            subscribed_project_ids: vec![" project-2 ".to_owned(), "project-1".to_owned()],
            ..telegram_ui_relay_config()
        },
        state: TelegramBotState {
            chat_id: Some(42),
            ..TelegramBotState::default()
        },
    };
    let config = TelegramBotConfig::from_ui_file("/tmp", &with_default)
        .expect("default project should produce relay config");

    assert_eq!(config.project_id, "project-1");
    assert_eq!(
        config.subscribed_project_ids,
        vec!["project-2", "project-1"]
    );
    assert_eq!(config.chat_id, Some(42));
}

#[test]
fn telegram_ui_file_omits_disabled_relay_config_even_with_token_and_project() {
    let disabled = telegram_ui_relay_file(TelegramUiConfig {
        enabled: false,
        ..telegram_ui_relay_config()
    });

    assert!(TelegramBotConfig::from_ui_file("/tmp", &disabled).is_none());
}

#[test]
fn telegram_ui_file_requires_bot_token_for_relay_config() {
    let missing_token = telegram_ui_relay_file(TelegramUiConfig {
        bot_token: None,
        ..telegram_ui_relay_config()
    });

    assert!(TelegramBotConfig::from_ui_file("/tmp", &missing_token).is_none());
}

#[test]
fn telegram_ui_file_rejects_empty_bot_token_for_relay_config() {
    let empty_token = telegram_ui_relay_file(TelegramUiConfig {
        bot_token: Some(String::new()),
        ..telegram_ui_relay_config()
    });

    assert!(TelegramBotConfig::from_ui_file("/tmp", &empty_token).is_none());
}

#[test]
fn telegram_ui_file_rejects_whitespace_bot_token_for_relay_config() {
    let whitespace_token = telegram_ui_relay_file(TelegramUiConfig {
        bot_token: Some("   ".to_owned()),
        ..telegram_ui_relay_config()
    });

    assert!(TelegramBotConfig::from_ui_file("/tmp", &whitespace_token).is_none());
}

#[test]
fn telegram_token_mask_omits_empty_token() {
    assert_eq!(mask_telegram_bot_token(""), None);
}

#[test]
fn telegram_token_mask_omits_whitespace_only_token() {
    assert_eq!(mask_telegram_bot_token("   \t\n"), None);
}

#[test]
fn telegram_token_mask_exposes_short_token_suffix() {
    assert_eq!(mask_telegram_bot_token("ab").as_deref(), Some("****ab"));
}

#[test]
fn telegram_token_mask_exposes_exactly_four_char_token() {
    assert_eq!(mask_telegram_bot_token("abcd").as_deref(), Some("****abcd"));
}

#[test]
fn telegram_token_mask_exposes_trimmed_token_suffix() {
    assert_eq!(
        mask_telegram_bot_token(" abcdef ").as_deref(),
        Some("****cdef")
    );
}

#[test]
fn telegram_token_mask_exposes_full_token_suffix() {
    assert_eq!(
        mask_telegram_bot_token("123456:abcdefghi").as_deref(),
        Some("****fghi")
    );
}

fn assert_telegram_token_mask_suffix_contract(token: &str) {
    let masked = mask_telegram_bot_token(token).expect("non-empty token should mask");
    let revealed = masked
        .strip_prefix("****")
        .expect("mask should keep fixed redaction prefix");
    assert!(!revealed.is_empty());
    assert!(revealed.chars().count() <= 4);
    assert!(token.trim().ends_with(revealed));
}

#[test]
fn telegram_token_mask_contract_limits_visible_suffix() {
    for token in ["a", "ab", "abc", "abcd", "abcde", "123456:abcdefghi"] {
        assert_telegram_token_mask_suffix_contract(token);
    }
}

#[test]
fn telegram_test_rate_limit_rejects_immediate_retry() {
    let token = format!("123456:{}:{}", Uuid::new_v4(), Uuid::new_v4());

    check_telegram_test_rate_limit(&token).expect("first attempt should pass");
    let err = check_telegram_test_rate_limit(&token).expect_err("retry should be rate-limited");

    assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
}

#[test]
fn telegram_token_validation_enforces_max_length_boundary() {
    validate_telegram_bot_token(&"x".repeat(TELEGRAM_BOT_TOKEN_MAX_CHARS - 1))
        .expect("under-limit token should pass");
    validate_telegram_bot_token(&"x".repeat(TELEGRAM_BOT_TOKEN_MAX_CHARS))
        .expect("max-length token should pass");

    let err = validate_telegram_bot_token(&"x".repeat(TELEGRAM_BOT_TOKEN_MAX_CHARS + 1))
        .expect_err("over-limit token should fail");

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("at most 256 characters"));

    let long_err =
        validate_telegram_bot_token(&"x".repeat(1024)).expect_err("very long token should fail");
    assert_eq!(long_err.status, StatusCode::BAD_REQUEST);
    assert!(long_err.message.contains("at most 256 characters"));
}

fn telegram_api_error(method: &str, status: StatusCode, error_code: Option<i64>) -> anyhow::Error {
    anyhow::Error::new(TelegramApiError {
        method: method.to_owned(),
        status,
        error_code,
        description: status
            .canonical_reason()
            .unwrap_or("Telegram API error")
            .to_owned(),
    })
}

fn telegram_getme_api_error(status: StatusCode, error_code: Option<i64>) -> anyhow::Error {
    telegram_api_error("getMe", status, error_code)
}

#[test]
fn telegram_connection_test_error_classifies_getme_auth_failures_as_validation() {
    let validation = telegram_test_connection_error(telegram_getme_api_error(
        StatusCode::UNAUTHORIZED,
        Some(401),
    ));
    assert_eq!(validation.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        validation
            .message
            .contains("Telegram connection test failed")
    );

    let wrapped =
        telegram_getme_api_error(StatusCode::UNAUTHORIZED, Some(401)).context("getMe call failed");
    let wrapped_validation = telegram_test_connection_error(wrapped);
    assert_eq!(wrapped_validation.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[test]
fn telegram_connection_test_error_classifies_getme_rate_limits_as_429() {
    let telegram_rate_limit = telegram_test_connection_error(telegram_getme_api_error(
        StatusCode::TOO_MANY_REQUESTS,
        Some(429),
    ));
    assert_eq!(telegram_rate_limit.status, StatusCode::TOO_MANY_REQUESTS);

    let wrapped_rate_limit = telegram_getme_api_error(StatusCode::TOO_MANY_REQUESTS, Some(429))
        .context("getMe call failed");
    let wrapped_rate_limit = telegram_test_connection_error(wrapped_rate_limit);
    assert_eq!(wrapped_rate_limit.status, StatusCode::TOO_MANY_REQUESTS);
}

#[test]
fn telegram_connection_test_error_gives_429_precedence_in_contradictory_envelopes() {
    let rate_limited_status_with_validation_code = telegram_test_connection_error(
        telegram_getme_api_error(StatusCode::TOO_MANY_REQUESTS, Some(401)),
    );
    assert_eq!(
        rate_limited_status_with_validation_code.status,
        StatusCode::TOO_MANY_REQUESTS
    );

    let validation_status_with_rate_limited_code = telegram_test_connection_error(
        telegram_getme_api_error(StatusCode::UNAUTHORIZED, Some(429)),
    );
    assert_eq!(
        validation_status_with_rate_limited_code.status,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[test]
fn telegram_connection_test_error_treats_non_rate_limited_contradictions_as_upstream() {
    let contradictory_non_rate_limited = telegram_test_connection_error(telegram_getme_api_error(
        StatusCode::BAD_GATEWAY,
        Some(400),
    ));
    assert_eq!(
        contradictory_non_rate_limited.status,
        StatusCode::BAD_GATEWAY
    );

    let unexpected_code_with_validation_status = telegram_test_connection_error(
        telegram_getme_api_error(StatusCode::UNAUTHORIZED, Some(999)),
    );
    assert_eq!(
        unexpected_code_with_validation_status.status,
        StatusCode::BAD_GATEWAY
    );

    let validation_code_with_non_error_status =
        telegram_test_connection_error(telegram_getme_api_error(StatusCode::OK, Some(401)));
    assert_eq!(
        validation_code_with_non_error_status.status,
        StatusCode::BAD_GATEWAY
    );
}

#[test]
fn telegram_connection_test_error_does_not_reuse_getme_classification_for_other_methods() {
    let send_message_auth_error = telegram_test_connection_error(telegram_api_error(
        "sendMessage",
        StatusCode::UNAUTHORIZED,
        Some(401),
    ));

    assert_eq!(send_message_auth_error.status, StatusCode::BAD_GATEWAY);

    let send_message_rate_limit = telegram_test_connection_error(telegram_api_error(
        "sendMessage",
        StatusCode::TOO_MANY_REQUESTS,
        Some(429),
    ));

    assert_eq!(send_message_rate_limit.status, StatusCode::BAD_GATEWAY);
}

#[test]
fn telegram_connection_test_error_classifies_transport_and_server_failures_as_bad_gateway() {
    let telegram_server_error = telegram_test_connection_error(telegram_getme_api_error(
        StatusCode::BAD_GATEWAY,
        Some(502),
    ));
    assert_eq!(telegram_server_error.status, StatusCode::BAD_GATEWAY);

    let upstream = telegram_test_connection_error(anyhow!("failed to call Telegram `getMe`"));
    assert_eq!(upstream.status, StatusCode::BAD_GATEWAY);
    assert!(upstream.message.contains("Telegram connection test failed"));
}

#[test]
fn telegram_state_persist_preserves_settings_config() {
    let path = std::env::temp_dir().join(format!("termal-telegram-state-{}.json", Uuid::new_v4()));
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "config": {
                "enabled": true,
                "botToken": "123456:secret",
                "subscribedProjectIds": ["project-1"],
                "defaultProjectId": "project-1",
                "defaultSessionId": "session-1"
            },
            "chatId": 123
        }))
        .expect("fixture should encode"),
    )
    .expect("fixture should write");

    let state = TelegramBotState {
        chat_id: Some(456),
        next_update_id: Some(99),
        ..TelegramBotState::default()
    };
    persist_telegram_bot_state(&path, &state).expect("state should persist");

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("state file should read"))
        .expect("state file should parse");
    assert_eq!(value["config"]["botToken"], json!("123456:secret"));
    assert_eq!(
        value["config"]["subscribedProjectIds"],
        json!(["project-1"])
    );
    assert_eq!(value["config"]["defaultProjectId"], json!("project-1"));
    assert_eq!(value["config"]["defaultSessionId"], json!("session-1"));
    assert_eq!(value["chatId"], json!(456));
    assert_eq!(value["nextUpdateId"], json!(99));

    fs::remove_file(&path).ok();
}

#[test]
fn telegram_state_persist_backs_up_malformed_existing_file() {
    let path =
        std::env::temp_dir().join(format!("termal-telegram-bad-state-{}.json", Uuid::new_v4()));
    fs::write(&path, b"{").expect("fixture should write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
            .expect("fixture permissions should set");
    }

    let state = TelegramBotState {
        chat_id: Some(456),
        next_update_id: Some(99),
        ..TelegramBotState::default()
    };
    persist_telegram_bot_state(&path, &state)
        .expect("malformed existing state should be backed up and replaced");

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("state file should read"))
        .expect("state file should parse");
    assert_eq!(value["chatId"], json!(456));
    assert_eq!(value["nextUpdateId"], json!(99));

    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .expect("path should have utf8 file name");
    let backup_prefix = format!("{file_name}.corrupt-");
    let backups: Vec<PathBuf> = fs::read_dir(path.parent().expect("path should have a parent"))
        .expect("temp dir should read")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|candidate| {
            candidate
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| name.starts_with(&backup_prefix) && name.ends_with(".json"))
        })
        .collect();

    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(&backups[0]).expect("backup should read"), b"{");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let mode = fs::metadata(&backups[0])
            .expect("backup metadata should read")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    fs::remove_file(&path).ok();
    for backup in backups {
        fs::remove_file(backup).ok();
    }
}

#[test]
fn telegram_poll_error_dirty_persist_failure_is_nonfatal() {
    let path = std::env::temp_dir().join(format!(
        "termal-telegram-poll-error-state-dir-{}",
        Uuid::new_v4()
    ));
    fs::create_dir(&path).expect("fixture directory should create");
    let state = TelegramBotState {
        chat_id: Some(456),
        next_update_id: Some(99),
        ..TelegramBotState::default()
    };

    assert!(!persist_dirty_telegram_state_after_poll_error(
        &path, &state, true
    ));
    assert!(persist_dirty_telegram_state_after_poll_error(
        &path, &state, false
    ));

    fs::remove_dir(&path).ok();
}

#[test]
fn telegram_state_load_defaults_missing_file() {
    let path = std::env::temp_dir().join(format!(
        "termal-telegram-missing-state-{}.json",
        Uuid::new_v4()
    ));
    fs::remove_file(&path).ok();

    let state = load_telegram_bot_state(&path).expect("missing state should default");

    assert_eq!(state.chat_id, None);
    assert_eq!(state.next_update_id, None);
}

#[test]
fn telegram_state_load_reports_unreadable_paths() {
    let path = std::env::temp_dir().join(format!("termal-telegram-state-dir-{}", Uuid::new_v4()));
    fs::create_dir(&path).expect("fixture directory should create");

    let err = load_telegram_bot_state(&path)
        .expect_err("unreadable state paths should fail instead of defaulting");

    assert!(err.to_string().contains("failed to read"));

    fs::remove_dir(&path).ok();
}

#[cfg(unix)]
#[test]
fn telegram_bot_file_write_sets_mode_600() {
    use std::os::unix::fs::PermissionsExt as _;

    let path = std::env::temp_dir().join(format!("termal-telegram-mode-{}.json", Uuid::new_v4()));

    write_telegram_bot_file(&path, b"{}").expect("telegram bot file should write");

    let mode = fs::metadata(&path)
        .expect("state file metadata should read")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);

    fs::remove_file(&path).ok();
}

#[test]
fn telegram_message_chunks_respect_utf16_limit() {
    let text = "🙂".repeat(TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS + 1);
    let chunks = chunk_telegram_message_text(&text);

    assert!(chunks.len() > 1);
    assert_eq!(chunks.concat(), text);
    assert!(
        chunks
            .iter()
            .all(|chunk| { chunk.encode_utf16().count() <= TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS })
    );
}

#[test]
fn telegram_message_chunks_keep_mixed_utf16_boundary_intact() {
    let emoji = "\u{1f642}";
    let emoji_units = emoji.encode_utf16().count();
    let exact_limit = format!(
        "{}{emoji}",
        "a".repeat(TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS - emoji_units)
    );
    assert_eq!(
        exact_limit.encode_utf16().count(),
        TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS
    );
    assert_eq!(
        chunk_telegram_message_text(&exact_limit),
        vec![exact_limit.clone()]
    );

    let overflow = format!("{exact_limit}b");
    let chunks = chunk_telegram_message_text(&overflow);

    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks.concat(), overflow);
    assert_eq!(chunks[0], exact_limit);
    assert_eq!(chunks[1], "b");
    assert!(
        chunks
            .iter()
            .all(|chunk| { chunk.encode_utf16().count() <= TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS })
    );
}

#[test]
fn telegram_settings_validation_autofills_session_project_subscription() {
    let state = test_app_state();
    let (project_id, session_id) = create_telegram_settings_project_and_session(&state);
    let mut config = TelegramUiConfig {
        default_session_id: Some(session_id),
        ..TelegramUiConfig::default()
    };

    state
        .validate_and_normalize_telegram_config(&mut config)
        .expect("config should validate");

    assert_eq!(
        config.default_project_id.as_deref(),
        Some(project_id.as_str())
    );
    assert_eq!(config.subscribed_project_ids, vec![project_id]);
}

#[test]
fn telegram_settings_validation_uses_single_subscribed_project_as_default() {
    let state = test_app_state();
    let (project_id, _session_id) = create_telegram_settings_project_and_session(&state);
    let mut config = TelegramUiConfig {
        subscribed_project_ids: vec![project_id.clone()],
        ..TelegramUiConfig::default()
    };

    state
        .validate_and_normalize_telegram_config(&mut config)
        .expect("config should validate");

    assert_eq!(
        config.default_project_id.as_deref(),
        Some(project_id.as_str())
    );
    assert_eq!(config.subscribed_project_ids, vec![project_id]);
}

#[test]
fn telegram_settings_validation_rejects_enabled_config_without_project_target() {
    let state = test_app_state();
    let mut config = TelegramUiConfig {
        enabled: true,
        bot_token: Some("123456:secret".to_owned()),
        ..TelegramUiConfig::default()
    };

    let err = state
        .validate_and_normalize_telegram_config(&mut config)
        .expect_err("enabled configured relay should require a project target");

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("choose at least one Telegram project"));
    assert!(config.subscribed_project_ids.is_empty());
}

#[test]
fn telegram_settings_validation_rejects_orphan_session_project() {
    let state = test_app_state();
    let session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .create_session(
                Agent::Codex,
                Some("Orphan Telegram Session".to_owned()),
                "/tmp".to_owned(),
                Some("missing-project".to_owned()),
                None,
            )
            .session
            .id
    };
    let mut config = TelegramUiConfig {
        default_session_id: Some(session_id),
        ..TelegramUiConfig::default()
    };

    let err = state
        .validate_and_normalize_telegram_config(&mut config)
        .expect_err("orphan session project should fail validation");

    assert!(
        err.message
            .contains("unknown default Telegram session project")
    );
    assert_eq!(config.default_project_id, None);
    assert!(config.subscribed_project_ids.is_empty());
}

#[test]
fn telegram_settings_validation_does_not_partially_mutate_on_late_errors() {
    let state = test_app_state();
    let (project_id, _session_id) = create_telegram_settings_project_and_session(&state);
    let mut config = TelegramUiConfig {
        default_project_id: Some(project_id.clone()),
        default_session_id: Some("missing-session".to_owned()),
        ..TelegramUiConfig::default()
    };

    let err = state
        .validate_and_normalize_telegram_config(&mut config)
        .expect_err("unknown default session should fail validation");

    assert!(err.message.contains("unknown default Telegram session"));
    assert_eq!(
        config.default_project_id.as_deref(),
        Some(project_id.as_str())
    );
    assert!(config.subscribed_project_ids.is_empty());
}

#[test]
fn telegram_settings_validation_does_not_partially_mutate_on_other_error_paths() {
    let state = test_app_state();
    let (default_project_id, _default_session_id) =
        create_telegram_settings_project_and_session(&state);
    let (_other_project_id, other_session_id) =
        create_telegram_settings_project_and_session(&state);
    let no_project_session_id = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Telegram Session Without Project".to_owned()),
            workdir: None,
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should create")
        .session_id;

    let mut unknown_default_project = TelegramUiConfig {
        default_project_id: Some("missing-project".to_owned()),
        ..TelegramUiConfig::default()
    };
    let err = state
        .validate_and_normalize_telegram_config(&mut unknown_default_project)
        .expect_err("unknown default project should fail validation");
    assert!(err.message.contains("unknown default Telegram project"));
    assert_eq!(
        unknown_default_project.default_project_id.as_deref(),
        Some("missing-project")
    );
    assert!(unknown_default_project.subscribed_project_ids.is_empty());

    let mut no_project_session = TelegramUiConfig {
        default_project_id: Some(default_project_id.clone()),
        default_session_id: Some(no_project_session_id),
        ..TelegramUiConfig::default()
    };
    let err = state
        .validate_and_normalize_telegram_config(&mut no_project_session)
        .expect_err("session without project should fail validation");
    assert!(
        err.message
            .contains("default Telegram session must belong to a project")
    );
    assert_eq!(
        no_project_session.default_project_id.as_deref(),
        Some(default_project_id.as_str())
    );
    assert!(no_project_session.subscribed_project_ids.is_empty());

    let mut mismatched_session_project = TelegramUiConfig {
        default_project_id: Some(default_project_id.clone()),
        default_session_id: Some(other_session_id),
        ..TelegramUiConfig::default()
    };
    let err = state
        .validate_and_normalize_telegram_config(&mut mismatched_session_project)
        .expect_err("mismatched default session project should fail validation");
    assert!(
        err.message
            .contains("default Telegram session must belong to the default project")
    );
    assert_eq!(
        mismatched_session_project.default_project_id.as_deref(),
        Some(default_project_id.as_str())
    );
    assert!(mismatched_session_project.subscribed_project_ids.is_empty());
}

#[test]
fn telegram_status_sanitizes_stale_project_and_session_references() {
    let state = test_app_state();
    let (project_id, _session_id) = create_telegram_settings_project_and_session(&state);
    let config = TelegramUiConfig {
        subscribed_project_ids: vec![project_id.clone(), "missing-project".to_owned()],
        default_project_id: Some(project_id.clone()),
        default_session_id: Some("missing-session".to_owned()),
        ..TelegramUiConfig::default()
    };

    let sanitized = state.sanitize_telegram_config_for_current_state(config);

    assert_eq!(sanitized.subscribed_project_ids, vec![project_id]);
    assert_eq!(sanitized.default_session_id, None);
}

#[test]
fn telegram_settings_load_defaults_only_for_missing_file() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-settings-load-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let path = state.telegram_bot_file_path();

    let missing = state
        .load_telegram_bot_file()
        .expect("missing settings file should default");
    assert_eq!(missing.config.bot_token, None);
    assert_eq!(missing.state.chat_id, None);

    fs::create_dir_all(path.parent().expect("settings path should have a parent"))
        .expect("settings dir should create");
    fs::create_dir(&path).expect("directory fixture should create");

    let err = match state.load_telegram_bot_file() {
        Ok(_) => panic!("non-file settings path should fail instead of defaulting"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(err.message.contains("failed to read Telegram settings"));
}

#[test]
fn telegram_config_update_sanitizes_stale_persisted_references_before_validation() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-stale-config-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let (project_id, _session_id) = create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("settings path should have a parent"))
        .expect("settings dir should create");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "config": {
                "enabled": false,
                "botToken": "123456:secret",
                "subscribedProjectIds": ["missing-project"],
                "defaultProjectId": "missing-project",
                "defaultSessionId": "missing-session"
            },
            "chatId": 123
        }))
        .expect("fixture should encode"),
    )
    .expect("fixture should write");

    let request: UpdateTelegramConfigRequest = serde_json::from_value(json!({
        "enabled": true,
        "subscribedProjectIds": [project_id.clone()]
    }))
    .expect("request should decode");
    let response = state
        .update_telegram_config(request)
        .expect("unrelated update should sanitize stale persisted references");

    assert!(response.enabled);
    assert_eq!(response.subscribed_project_ids, vec![project_id.clone()]);
    assert_eq!(
        response.default_project_id.as_deref(),
        Some(project_id.as_str())
    );
    assert_eq!(response.default_session_id, None);
    assert_eq!(response.linked_chat_id, Some(123));

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    assert_eq!(value["config"]["enabled"], json!(true));
    assert_eq!(value["config"]["botToken"], json!("123456:secret"));
    assert_eq!(
        value["config"]["subscribedProjectIds"],
        json!([project_id.clone()])
    );
    assert_eq!(value["config"]["defaultProjectId"], json!(project_id));
    assert!(value["config"].get("defaultSessionId").is_none());
    assert_eq!(value["chatId"], json!(123));
}

#[test]
fn delete_project_prunes_telegram_config_and_disables_relay_without_project_target() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!("termal-telegram-home-{}", Uuid::new_v4()));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let (project_id, session_id) = create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("state path should have a parent"))
        .expect("settings dir should create");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "config": {
                "enabled": true,
                "botToken": "123456:secret",
                "subscribedProjectIds": [project_id.clone()],
                "defaultProjectId": project_id.clone(),
                "defaultSessionId": session_id
            },
            "chatId": 123
        }))
        .expect("fixture should encode"),
    )
    .expect("fixture should write");

    state
        .delete_project(&project_id)
        .expect("project should delete");

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    assert_eq!(value["config"]["enabled"], json!(false));
    assert_eq!(value["config"]["botToken"], json!("123456:secret"));
    assert!(
        value["config"].get("subscribedProjectIds").is_none()
            || value["config"]["subscribedProjectIds"] == json!([])
    );
    assert!(value["config"].get("defaultProjectId").is_none());
    assert!(value["config"].get("defaultSessionId").is_none());
    assert_eq!(value["chatId"], json!(123));
}

#[test]
fn delete_project_prunes_telegram_config_and_keeps_relay_enabled_with_remaining_target() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-multi-project-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let (deleted_project_id, _deleted_session_id) =
        create_telegram_settings_project_and_session(&state);
    let (remaining_project_id, remaining_session_id) =
        create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("state path should have a parent"))
        .expect("settings dir should create");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "config": {
                "enabled": true,
                "botToken": "123456:secret",
                "subscribedProjectIds": [deleted_project_id.clone(), remaining_project_id.clone()],
                "defaultProjectId": remaining_project_id.clone(),
                "defaultSessionId": remaining_session_id
            },
            "chatId": 123
        }))
        .expect("fixture should encode"),
    )
    .expect("fixture should write");

    state
        .delete_project(&deleted_project_id)
        .expect("project should delete");

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    assert_eq!(value["config"]["enabled"], json!(true));
    assert_eq!(
        value["config"]["subscribedProjectIds"],
        json!([remaining_project_id.clone()])
    );
    assert_eq!(
        value["config"]["defaultProjectId"],
        json!(remaining_project_id)
    );
    assert_eq!(
        value["config"]["defaultSessionId"],
        json!(remaining_session_id)
    );
    assert_eq!(value["chatId"], json!(123));
}

#[test]
fn kill_session_prunes_telegram_state_and_config_references() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-session-prune-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &home);
    let state = test_app_state();
    let (project_id, session_id) = create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("state path should have a parent"))
        .expect("settings dir should create");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "config": {
                "enabled": true,
                "botToken": "123456:secret",
                "subscribedProjectIds": [project_id.clone()],
                "defaultProjectId": project_id,
                "defaultSessionId": session_id.clone()
            },
            "selectedSessionId": session_id.clone(),
            "lastDigestHash": "old-digest",
            "lastDigestMessageId": 44,
            "forwardNextAssistantMessageSessionIds": [session_id.clone(), "other-session"],
            "forwardNextAssistantMessageSessionId": session_id.clone(),
            "assistantForwardingCursors": {
                (session_id.clone()): {
                    "messageId": "message-1",
                    "textChars": 10
                },
                "other-session": {
                    "messageId": "message-2",
                    "textChars": 20
                }
            },
            "chatId": 123
        }))
        .expect("fixture should encode"),
    )
    .expect("fixture should write");

    state
        .kill_session(&session_id)
        .expect("session should kill");

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    assert!(value["config"].get("defaultSessionId").is_none());
    assert!(value.get("selectedSessionId").is_none());
    assert!(value.get("lastDigestHash").is_none());
    assert!(value.get("lastDigestMessageId").is_none());
    assert_eq!(
        value["forwardNextAssistantMessageSessionIds"],
        json!(["other-session"])
    );
    assert_eq!(
        value["forwardNextAssistantMessageSessionId"],
        json!("other-session")
    );
    assert!(
        value["assistantForwardingCursors"]
            .get(&session_id)
            .is_none()
    );
    assert_eq!(
        value["assistantForwardingCursors"]["other-session"]["messageId"],
        json!("message-2")
    );
    assert_eq!(value["chatId"], json!(123));
}
