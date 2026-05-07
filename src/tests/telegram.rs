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

struct FakeTelegramSender {
    fail_on_attempt: Option<usize>,
    send_attempts: Cell<usize>,
    sent_texts: RefCell<Vec<String>>,
}

impl FakeTelegramSender {
    fn new(fail_on_attempt: Option<usize>) -> Self {
        Self {
            fail_on_attempt,
            send_attempts: Cell::new(0),
            sent_texts: RefCell::new(Vec::new()),
        }
    }
}

impl TelegramMessageSender for FakeTelegramSender {
    fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        _reply_markup: Option<&TelegramInlineKeyboardMarkup>,
    ) -> Result<TelegramChatMessage> {
        let attempt = self.send_attempts.get() + 1;
        self.send_attempts.set(attempt);
        if self.fail_on_attempt == Some(attempt) {
            bail!("forced send failure");
        }
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

// Pins the outgoing digest shape: the rendered text exposes the project
// headline, a `Next: ...` line listing proposed action labels, and an
// `Open: ...` line resolving the relative deep link against the public
// base URL, while the inline keyboard emits one button per action with
// `callback_data` matching the `ProjectActionId` kebab-case. Without this
// the phone UI would either lose tap-to-act buttons or fire callbacks the
// server cannot parse, silently breaking remote control.
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

    let rendered = render_telegram_digest(&digest, Some("https://termal.local"));
    assert!(rendered.contains("Project: termal"));
    assert!(rendered.contains("Next: Review in TermAl, Ask Agent to Commit"));
    assert!(
        rendered.contains("Open: https://termal.local/?projectId=project-1&sessionId=session-1")
    );

    let keyboard = build_telegram_digest_keyboard(&digest).expect("keyboard should exist");
    assert_eq!(keyboard.inline_keyboard.len(), 1);
    assert_eq!(
        keyboard.inline_keyboard[0][0].callback_data,
        "review-in-termal"
    );
    assert_eq!(
        keyboard.inline_keyboard[0][1].callback_data,
        "ask-agent-to-commit"
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

    let result =
        forward_new_assistant_message_if_any(&telegram, &termal, &mut state, 42, "session-1");

    assert!(result.is_err());
    assert_eq!(
        telegram.sent_texts.borrow().as_slice(),
        ["First reply".to_owned()]
    );
    assert_eq!(
        state.last_forwarded_assistant_message_id.as_deref(),
        Some("message-1")
    );
    assert_eq!(
        state.last_forwarded_assistant_message_text_chars,
        Some("First reply".chars().count())
    );

    let mut dirty = false;
    merge_assistant_forward_result(&mut dirty, result);
    assert!(dirty);
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
    assert_eq!(
        state.last_forwarded_assistant_message_id.as_deref(),
        Some("message-1")
    );
    assert_eq!(
        state.last_forwarded_assistant_message_text_chars,
        Some("First reply".chars().count())
    );
    assert_eq!(state.forward_next_assistant_message_session_id, None);
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
fn telegram_assistant_forwarding_plan_skips_preexisting_active_turn() {
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

    assert_eq!(plan, TelegramAssistantForwardingPlan::Skip);
    assert!(!apply_assistant_forwarding_plan(&mut state, plan));
    assert_eq!(state.forward_next_assistant_message_session_id, None);
    assert_eq!(state.last_forwarded_assistant_message_id, None);
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
        state.last_forwarded_assistant_message_id.as_deref(),
        Some("message-1")
    );
    assert_eq!(state.forward_next_assistant_message_session_id, None);
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
    assert_eq!(
        state.last_forwarded_assistant_message_text_chars,
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

#[test]
fn telegram_log_sanitizer_redacts_bot_tokens_and_truncates() {
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

    let standalone =
        sanitize_telegram_log_detail(&format!("Telegram token rejected: botToken={token}."));
    assert!(!standalone.contains(token));
    assert!(standalone.contains("botToken=<redacted>."));

    let colon_delimited = sanitize_telegram_log_detail(&format!(
        "Telegram token rejected: botToken:{token}: invalid"
    ));
    assert!(!colon_delimited.contains(token));
    assert!(colon_delimited.contains("botToken:<redacted>: invalid"));

    let benign = "trace [12345:67890123] pid 12345:abcdefgh version 12345:6.7.8.9";
    assert_eq!(sanitize_telegram_log_detail(benign), benign);

    let malformed_url = "request failed for https://api.telegram.org/bot/bot/getMe";
    assert_eq!(sanitize_telegram_log_detail(malformed_url), malformed_url);
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
fn telegram_token_mask_only_exposes_short_suffix() {
    assert_eq!(
        mask_telegram_bot_token("123456:abcdefghi").as_deref(),
        Some("****fghi")
    );
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
fn telegram_connection_test_error_does_not_reuse_getme_validation_for_other_methods() {
    let send_message_auth_error = telegram_test_connection_error(telegram_api_error(
        "sendMessage",
        StatusCode::UNAUTHORIZED,
        Some(401),
    ));

    assert_eq!(send_message_auth_error.status, StatusCode::BAD_GATEWAY);
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
fn delete_project_prunes_telegram_config_on_disk() {
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
    assert_eq!(value["config"]["botToken"], json!("123456:secret"));
    assert!(
        value["config"].get("subscribedProjectIds").is_none()
            || value["config"]["subscribedProjectIds"] == json!([])
    );
    assert!(value["config"].get("defaultProjectId").is_none());
    assert!(value["config"].get("defaultSessionId").is_none());
    assert_eq!(value["chatId"], json!(123));
}
