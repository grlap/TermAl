// Tests for the Telegram relay adapter in `src/telegram.rs`.
//
// In production the Telegram bot is an optional remote-control surface for
// TermAl: it pushes periodic project digests (headline, status, proposed
// actions, deep link) to a linked chat and accepts slash commands like
// `/commit`, `/stop`, or `/status` that are mapped onto `ProjectActionId`
// values and dispatched against the same HTTP action API used by the desktop
// UI. These tests pin two pieces of that adapter: `parse_telegram_command`
// (slash-command routing) and `render_telegram_digest` /
// `build_telegram_digest_keyboard` (outgoing digest formatting and inline
// keyboard). Everything else in `src/telegram.rs` is thin I/O glue around
// those pure functions.

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

// Pins that the slash-command parser strips Telegram's `@botname` mention
// suffix and preserves trailing free-form args. Without this, group-chat
// messages like `/commit@termal_bot now please` would be rejected as
// unknown commands and silently dropped, breaking the bot for any chat
// where it shares room with other bots.
#[test]
fn telegram_command_parser_supports_suffixes_and_aliases() {
    let parsed =
        parse_telegram_command("/commit@termal_bot   now please").expect("command should parse");
    assert_eq!(
        parsed.command,
        TelegramIncomingCommand::Action(ProjectActionId::AskAgentToCommit)
    );
    assert_eq!(parsed.args, "now please");

    let parsed = parse_telegram_command("/status").expect("status should parse");
    assert_eq!(parsed.command, TelegramIncomingCommand::Status);
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
    let detail = format!(
        "request failed for https://api.telegram.org/bot123456:secretToken/getUpdates: {}",
        "x".repeat(300)
    );
    let sanitized = sanitize_telegram_log_detail(&detail);

    assert!(!sanitized.contains("123456:secretToken"));
    assert!(sanitized.contains("/bot<redacted>/getUpdates"));
    assert!(sanitized.ends_with("..."));
    assert!(sanitized.chars().count() <= 259);
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
