// Assistant-forwarding and digest-delivery tests split out of
// `telegram.rs`. This module owns Telegram-to-session forwarding, assistant
// cursor advancement, digest retries, and delivery regressions.
//
// It deliberately does not own Telegram settings persistence, relay lifecycle
// restart behavior, or generic route/rate-limit coverage.

use super::telegram_support::{
    FakeTelegramPromptClient, FakeTelegramSender, FakeTelegramSessionReader,
    FakeTelegramSessionReaderById, RecordingTelegramSessionReaderById, telegram_project_digest,
    telegram_state_sessions_with_project_session, telegram_test_config,
};
use super::*;

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
            session_mutation_stamp: None,
            parent_delegation_id: None,
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
            session_mutation_stamp: None,
            parent_delegation_id: None,
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
            session_mutation_stamp: None,
            parent_delegation_id: None,
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

#[test]
fn telegram_digest_renderer_includes_actions_and_public_link() {
    // Pins the outgoing digest shape: rendered text exposes the project
    // headline, proposed action labels, and public deep link, while the inline
    // keyboard emits one callback-bound button per action. Without this the
    // phone UI loses tap-to-act buttons or fires callbacks against whichever
    // project is active when an older digest is tapped.
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
fn telegram_digest_sync_does_not_forward_assistant_reply_when_disabled() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![Ok(telegram_project_digest(Some("session-1")))],
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
                        id: "reply".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Reply that must stay in TermAl".to_owned(),
                    },
                ],
            },
        },
    );
    let mut config = telegram_test_config();
    config.forward_assistant_replies = false;
    let mut state = TelegramBotState {
        assistant_forwarding_cursors: HashMap::from([(
            "session-1".to_owned(),
            TelegramAssistantForwardingCursor {
                message_id: Some("baseline".to_owned()),
                text_chars: Some("Baseline".chars().count()),
                text_hash: None,
                text_start_chars: None,
                resend_if_grown: false,
                sent_chunks: None,
                failed_chunk_send_attempts: None,
                footer_pending: false,
                baseline_while_active: false,
            },
        )]),
        ..TelegramBotState::default()
    };

    let changed = sync_telegram_digest(&telegram, &termal, &config, &mut state, 42)
        .expect("digest sync should succeed");

    assert!(changed);
    assert_eq!(
        termal.events.borrow().as_slice(),
        ["digest:project-1".to_owned()],
        "digest sync should not fetch assistant messages when forwarding is disabled"
    );
    let sent_texts = telegram.sent_texts.borrow();
    assert_eq!(sent_texts.len(), 1);
    assert!(sent_texts[0].contains("Project digest"));
    assert!(!sent_texts[0].contains("Reply that must stay in TermAl"));
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("disabled forwarding should leave the existing cursor untouched");
    assert_eq!(cursor.message_id.as_deref(), Some("baseline"));
    assert_eq!(cursor.text_chars, Some("Baseline".chars().count()));
}

#[test]
fn telegram_digest_sync_clears_stale_selected_session_when_forwarding_disabled() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![Ok(telegram_project_digest(Some("session-1")))],
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: Vec::new(),
            },
        },
    );
    let mut config = telegram_test_config();
    config.forward_assistant_replies = false;
    let mut state = TelegramBotState {
        selected_session_id: Some("stale-session".to_owned()),
        forward_next_assistant_message_session_ids: vec!["stale-session".to_owned()],
        forward_next_assistant_message_session_id: Some("stale-session".to_owned()),
        ..TelegramBotState::default()
    };

    let changed = sync_telegram_digest(&telegram, &termal, &config, &mut state, 42)
        .expect("digest sync should clear stale selection");

    assert!(changed);
    assert_eq!(state.selected_session_id, None);
    assert!(state.forward_next_assistant_message_session_ids.is_empty());
    assert_eq!(state.forward_next_assistant_message_session_id, None);
    assert_eq!(
        termal.events.borrow().as_slice(),
        ["digest:project-1".to_owned(), "state-sessions".to_owned()],
        "stale selection cleanup should not fetch assistant messages"
    );
}

#[test]
fn telegram_digest_sync_preserves_digest_progress_when_prompt_target_resolve_fails() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![Ok(telegram_project_digest(Some("session-1")))],
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: Vec::new(),
            },
        },
    )
    .with_state_sessions_error("state unavailable");
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();

    let changed = sync_telegram_digest(&telegram, &termal, &config, &mut state, 42)
        .expect("digest progress should survive target resolution failure");

    assert!(changed);
    assert_eq!(state.last_digest_message_id, Some(1));
    assert!(state.last_digest_hash.is_some());
    assert_eq!(
        termal.events.borrow().as_slice(),
        ["digest:project-1".to_owned(), "state-sessions".to_owned()]
    );
    let sent_texts = telegram.sent_texts.borrow();
    assert_eq!(sent_texts.len(), 1);
    assert!(sent_texts[0].contains("Project digest"));
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
    // Pins the dirty-merge policy for assistant forwarding: the helper can
    // update the persisted cursor after one successful send and then fail on a
    // later chunk/message. Callers must still persist that partial progress.
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
fn telegram_prompt_does_not_arm_assistant_forwarding_without_opt_in() {
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
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Pre-existing reply".to_owned(),
                }],
            },
        },
    )
    .with_state_sessions(telegram_state_sessions_with_project_session(
        "session-1",
        None,
    ));
    let mut config = telegram_test_config();
    config.forward_assistant_replies = false;
    let mut state = TelegramBotState::default();

    let outcome = forward_telegram_text_to_project_for_relay(
        &telegram,
        &termal,
        &config,
        &mut state,
        42,
        "hello from Telegram",
    )
    .expect("prompt should forward without assistant forwarding opt-in");

    assert!(outcome.final_sync_satisfied);
    assert_eq!(
        termal.sent_prompts.borrow().as_slice(),
        [("session-1".to_owned(), "hello from Telegram".to_owned())]
    );
    assert!(state.assistant_forwarding_cursors.is_empty());
    assert!(state.forward_next_assistant_message_session_ids.is_empty());
    assert_eq!(state.forward_next_assistant_message_session_id, None);
    assert!(
        termal
            .events
            .borrow()
            .iter()
            .all(|event| !event.starts_with("session:")),
        "assistant session reads should be skipped when full forwarding is off"
    );
}

#[test]
fn telegram_prompt_ignores_delegated_digest_primary_session() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![Ok(telegram_project_digest(Some("session-child")))],
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: Vec::new(),
            },
        },
    )
    .with_state_sessions(telegram_state_sessions_with_project_session(
        "session-child",
        Some("delegation-1"),
    ));
    let mut config = telegram_test_config();
    config.forward_assistant_replies = false;
    let mut state = TelegramBotState::default();

    let outcome = forward_telegram_text_to_project_for_relay(
        &telegram,
        &termal,
        &config,
        &mut state,
        42,
        "hello from Telegram",
    )
    .expect("delegated primary should not fail prompt routing");

    assert!(!outcome.final_sync_satisfied);
    assert!(termal.sent_prompts.borrow().is_empty());
    assert_eq!(
        termal.events.borrow().as_slice(),
        ["digest:project-1".to_owned(), "state-sessions".to_owned()]
    );
    assert!(telegram.sent_texts.borrow()[0].contains("No active project session is available yet"));
}

#[test]
fn telegram_prompt_routes_to_project_root_when_digest_primary_is_delegated() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![
            Ok(telegram_project_digest(Some("session-child"))),
            Ok(telegram_project_digest(Some("session-root"))),
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
        sessions: vec![
            TelegramStateSession {
                id: "session-root".to_owned(),
                name: "Project Root".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 1,
                session_mutation_stamp: Some(10),
                parent_delegation_id: None,
            },
            TelegramStateSession {
                id: "session-child".to_owned(),
                name: "Delegated Child".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Active,
                message_count: 1,
                session_mutation_stamp: Some(11),
                parent_delegation_id: Some("delegation-1".to_owned()),
            },
        ],
    });
    let mut config = telegram_test_config();
    config.forward_assistant_replies = false;
    let mut state = TelegramBotState::default();

    let outcome = forward_telegram_text_to_project_for_relay(
        &telegram,
        &termal,
        &config,
        &mut state,
        42,
        "hello from Telegram",
    )
    .expect("delegated primary should fall back to a project-root target");

    assert!(outcome.final_sync_satisfied);
    assert_eq!(
        termal.sent_prompts.borrow().as_slice(),
        [("session-root".to_owned(), "hello from Telegram".to_owned())]
    );
    assert_eq!(
        termal.events.borrow().as_slice(),
        [
            "digest:project-1".to_owned(),
            "state-sessions".to_owned(),
            "send:session-root".to_owned(),
            "digest:project-1".to_owned()
        ]
    );
    assert!(
        telegram
            .sent_texts
            .borrow()
            .iter()
            .all(|text| !text.contains("No active project session"))
    );
}

#[test]
fn telegram_prompt_fallback_uses_latest_project_root_when_primary_is_delegated() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![
            Ok(telegram_project_digest(Some("session-child"))),
            Ok(telegram_project_digest(Some("session-new"))),
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
        sessions: vec![
            TelegramStateSession {
                id: "session-old".to_owned(),
                name: "Older Project Root".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 1,
                session_mutation_stamp: Some(9),
                parent_delegation_id: None,
            },
            TelegramStateSession {
                id: "session-new".to_owned(),
                name: "Newer Project Root".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 2,
                session_mutation_stamp: Some(10),
                parent_delegation_id: None,
            },
            TelegramStateSession {
                id: "session-child".to_owned(),
                name: "Delegated Child".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Active,
                message_count: 3,
                session_mutation_stamp: Some(11),
                parent_delegation_id: Some("delegation-1".to_owned()),
            },
        ],
    });
    let mut config = telegram_test_config();
    config.forward_assistant_replies = false;
    let mut state = TelegramBotState::default();

    let outcome = forward_telegram_text_to_project_for_relay(
        &telegram,
        &termal,
        &config,
        &mut state,
        42,
        "hello from Telegram",
    )
    .expect("delegated primary should fall back to the latest project root");

    assert!(outcome.final_sync_satisfied);
    assert_eq!(
        termal.sent_prompts.borrow().as_slice(),
        [("session-new".to_owned(), "hello from Telegram".to_owned())]
    );
}

#[test]
fn telegram_prompt_uses_unknown_digest_primary_as_promptable_target() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![
            Ok(telegram_project_digest(Some("session-unknown"))),
            Ok(telegram_project_digest(Some("session-unknown"))),
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
            id: "session-unknown".to_owned(),
            name: "Future Status Project Root".to_owned(),
            project_id: Some("project-1".to_owned()),
            status: TelegramSessionStatus::Unknown,
            message_count: 1,
            session_mutation_stamp: Some(9),
            parent_delegation_id: None,
        }],
    });
    let mut config = telegram_test_config();
    config.forward_assistant_replies = false;
    let mut state = TelegramBotState::default();

    let outcome = forward_telegram_text_to_project_for_relay(
        &telegram,
        &termal,
        &config,
        &mut state,
        42,
        "hello from Telegram",
    )
    .expect("unknown digest primary should remain promptable");

    assert!(outcome.final_sync_satisfied);
    assert_eq!(
        termal.sent_prompts.borrow().as_slice(),
        [(
            "session-unknown".to_owned(),
            "hello from Telegram".to_owned()
        )]
    );
}

#[test]
fn telegram_prompt_fallback_skips_error_digest_primary() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![
            Ok(telegram_project_digest(Some("session-error"))),
            Ok(telegram_project_digest(Some("session-ok"))),
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
        sessions: vec![
            TelegramStateSession {
                id: "session-ok".to_owned(),
                name: "Promptable Project Root".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 1,
                session_mutation_stamp: Some(9),
                parent_delegation_id: None,
            },
            TelegramStateSession {
                id: "session-error".to_owned(),
                name: "Errored Project Root".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Error,
                message_count: 2,
                session_mutation_stamp: Some(10),
                parent_delegation_id: None,
            },
        ],
    });
    let mut config = telegram_test_config();
    config.forward_assistant_replies = false;
    let mut state = TelegramBotState::default();

    let outcome = forward_telegram_text_to_project_for_relay(
        &telegram,
        &termal,
        &config,
        &mut state,
        42,
        "hello from Telegram",
    )
    .expect("errored digest primary should fall back to a promptable root");

    assert!(outcome.final_sync_satisfied);
    assert_eq!(
        termal.sent_prompts.borrow().as_slice(),
        [("session-ok".to_owned(), "hello from Telegram".to_owned())]
    );
}

#[test]
fn telegram_prompt_fallback_skips_error_project_root_session() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![
            Ok(telegram_project_digest(Some("session-child"))),
            Ok(telegram_project_digest(Some("session-ok"))),
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
        sessions: vec![
            TelegramStateSession {
                id: "session-ok".to_owned(),
                name: "Promptable Project Root".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 1,
                session_mutation_stamp: Some(9),
                parent_delegation_id: None,
            },
            TelegramStateSession {
                id: "session-error".to_owned(),
                name: "Errored Project Root".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Error,
                message_count: 2,
                session_mutation_stamp: Some(10),
                parent_delegation_id: None,
            },
            TelegramStateSession {
                id: "session-child".to_owned(),
                name: "Delegated Child".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Active,
                message_count: 3,
                session_mutation_stamp: Some(11),
                parent_delegation_id: Some("delegation-1".to_owned()),
            },
        ],
    });
    let mut config = telegram_test_config();
    config.forward_assistant_replies = false;
    let mut state = TelegramBotState::default();

    let outcome = forward_telegram_text_to_project_for_relay(
        &telegram,
        &termal,
        &config,
        &mut state,
        42,
        "hello from Telegram",
    )
    .expect("delegated primary should skip errored project roots");

    assert!(outcome.final_sync_satisfied);
    assert_eq!(
        termal.sent_prompts.borrow().as_slice(),
        [("session-ok".to_owned(), "hello from Telegram".to_owned())]
    );
}

#[test]
fn telegram_prompt_honors_selected_error_session() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![
            Ok(telegram_project_digest(Some("session-root"))),
            Ok(telegram_project_digest(Some("session-root"))),
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
        sessions: vec![
            TelegramStateSession {
                id: "session-root".to_owned(),
                name: "Project Root".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 1,
                session_mutation_stamp: Some(9),
                parent_delegation_id: None,
            },
            TelegramStateSession {
                id: "session-error".to_owned(),
                name: "Explicit Error Target".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Error,
                message_count: 2,
                session_mutation_stamp: Some(10),
                parent_delegation_id: None,
            },
        ],
    });
    let mut config = telegram_test_config();
    config.forward_assistant_replies = false;
    let mut state = TelegramBotState {
        selected_session_id: Some("session-error".to_owned()),
        ..TelegramBotState::default()
    };

    let outcome = forward_telegram_text_to_project_for_relay(
        &telegram,
        &termal,
        &config,
        &mut state,
        42,
        "hello from Telegram",
    )
    .expect("explicitly selected error session should be honored");

    assert!(outcome.final_sync_satisfied);
    assert_eq!(state.selected_session_id.as_deref(), Some("session-error"));
    assert_eq!(
        termal.sent_prompts.borrow().as_slice(),
        [("session-error".to_owned(), "hello from Telegram".to_owned())]
    );
}

#[test]
fn telegram_digest_sync_ignores_delegated_primary_for_assistant_forwarding() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![Ok(telegram_project_digest(Some("session-child")))],
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "reply-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Delegated child reply".to_owned(),
                }],
            },
        },
    )
    .with_state_sessions(telegram_state_sessions_with_project_session(
        "session-child",
        Some("delegation-1"),
    ));
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();

    let changed = sync_telegram_digest(&telegram, &termal, &config, &mut state, 42)
        .expect("digest sync should ignore delegated primary target");

    assert!(changed);
    assert!(
        termal
            .events
            .borrow()
            .iter()
            .all(|event| !event.starts_with("session:"))
    );
    assert!(
        telegram
            .sent_texts
            .borrow()
            .iter()
            .all(|text| !text.contains("Delegated child reply"))
    );
}

#[test]
fn telegram_digest_sync_forwards_from_project_root_when_primary_is_delegated() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        vec![Ok(telegram_project_digest(Some("session-child")))],
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![
                    TelegramSessionFetchMessage::Text {
                        id: "baseline".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Root baseline".to_owned(),
                    },
                    TelegramSessionFetchMessage::Text {
                        id: "reply-1".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Root reply".to_owned(),
                    },
                ],
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
                id: "session-root".to_owned(),
                name: "Project Session".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 0,
                session_mutation_stamp: None,
                parent_delegation_id: None,
            },
            TelegramStateSession {
                id: "session-child".to_owned(),
                name: "Delegated Child".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Active,
                message_count: 0,
                session_mutation_stamp: None,
                parent_delegation_id: Some("delegation-1".to_owned()),
            },
        ],
    });
    let config = telegram_test_config();
    let mut state = TelegramBotState {
        assistant_forwarding_cursors: HashMap::from([(
            "session-root".to_owned(),
            TelegramAssistantForwardingCursor {
                message_id: Some("baseline".to_owned()),
                text_chars: Some("Root baseline".chars().count()),
                text_hash: None,
                text_start_chars: None,
                resend_if_grown: false,
                sent_chunks: None,
                failed_chunk_send_attempts: None,
                footer_pending: false,
                baseline_while_active: false,
            },
        )]),
        ..TelegramBotState::default()
    };

    let changed = sync_telegram_digest(&telegram, &termal, &config, &mut state, 42)
        .expect("digest sync should forward from the fallback root target");

    assert!(changed);
    assert_eq!(
        termal.events.borrow().as_slice(),
        [
            "digest:project-1".to_owned(),
            "state-sessions".to_owned(),
            "session:session-root".to_owned(),
        ]
    );
    let sent_texts = telegram.sent_texts.borrow();
    assert!(sent_texts.iter().any(|text| text == "Root reply"));
    assert!(
        sent_texts
            .iter()
            .all(|text| !text.contains("Delegated child reply"))
    );
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
    .with_state_sessions(telegram_state_sessions_with_project_session(
        "session-1",
        None,
    ))
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
fn telegram_prompt_keeps_pre_send_assistant_forwarding_baseline() {
    let telegram = FakeTelegramSender::new(None);
    let reply_text = "Assistant text visible before prompt accept returned";
    let pre_send_session = TelegramSessionFetchResponse {
        session: TelegramSessionFetchSession {
            status: TelegramSessionStatus::Idle,
            messages: vec![TelegramSessionFetchMessage::Text {
                id: "message-1".to_owned(),
                author: "assistant".to_owned(),
                text: "Old assistant text".to_owned(),
            }],
        },
    };
    let post_send_session = TelegramSessionFetchResponse {
        session: TelegramSessionFetchSession {
            status: TelegramSessionStatus::Idle,
            messages: vec![
                TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Old assistant text".to_owned(),
                },
                TelegramSessionFetchMessage::Text {
                    id: "message-2".to_owned(),
                    author: "assistant".to_owned(),
                    text: reply_text.to_owned(),
                },
            ],
        },
    };
    let termal = FakeTelegramPromptClient::new(
        vec![
            Ok(telegram_project_digest(Some("session-1"))),
            Ok(telegram_project_digest(Some("session-1"))),
        ],
        pre_send_session.clone(),
    )
    .with_state_sessions(telegram_state_sessions_with_project_session(
        "session-1",
        None,
    ))
    .with_session_responses(vec![pre_send_session, post_send_session]);
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();

    let changed =
        forward_telegram_text_to_project(&telegram, &termal, &config, &mut state, 42, "from chat")
            .expect("forwarding should succeed");

    assert!(changed);
    assert_eq!(
        termal.events.borrow().as_slice(),
        [
            "digest:project-1",
            "state-sessions",
            "session:session-1",
            "send:session-1",
            "digest:project-1",
            "session:session-1",
        ]
    );
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("accepted prompt should arm assistant forwarding");
    assert_eq!(cursor.message_id.as_deref(), Some("message-2"));
    assert_eq!(cursor.text_chars, Some(reply_text.chars().count()));
    assert!(
        telegram
            .sent_texts
            .borrow()
            .iter()
            .any(|text| text == reply_text)
    );
}

#[test]
fn telegram_prompt_forwards_active_session_reply_without_completion_footer() {
    let telegram = FakeTelegramSender::new(None);
    let baseline_termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Old assistant text".to_owned(),
                }],
            },
        },
    };
    let mut state = TelegramBotState::default();

    arm_assistant_forwarding_for_telegram_prompt(&baseline_termal, &mut state, "session-1")
        .expect("arming should succeed");

    let active_termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Active,
                messages: vec![
                    TelegramSessionFetchMessage::Text {
                        id: "message-1".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Old assistant text".to_owned(),
                    },
                    TelegramSessionFetchMessage::Text {
                        id: "message-2".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Active reply".to_owned(),
                    },
                ],
            },
        },
    };

    let forwarded = forward_new_assistant_message_if_any(
        &telegram,
        &active_termal,
        &mut state,
        42,
        "session-1",
    )
    .expect("active reply forwarding should succeed");

    assert!(forwarded);
    assert_eq!(telegram.sent_texts.borrow().as_slice(), ["Active reply"]);
    assert!(
        !telegram
            .sent_texts
            .borrow()
            .iter()
            .any(|text| text.contains("turn complete"))
    );
}

#[test]
fn telegram_prompt_reforwards_full_settled_reply_when_active_draft_is_replaced() {
    let telegram = FakeTelegramSender::new(None);
    let baseline_termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Old assistant text".to_owned(),
                }],
            },
        },
    };
    let mut state = TelegramBotState::default();

    arm_assistant_forwarding_for_telegram_prompt(&baseline_termal, &mut state, "session-1")
        .expect("arming should succeed");

    let active_termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Active,
                messages: vec![
                    TelegramSessionFetchMessage::Text {
                        id: "message-1".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Old assistant text".to_owned(),
                    },
                    TelegramSessionFetchMessage::Text {
                        id: "message-2".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Draft reply".to_owned(),
                    },
                ],
            },
        },
    };
    assert!(
        forward_new_assistant_message_if_any(
            &telegram,
            &active_termal,
            &mut state,
            42,
            "session-1"
        )
        .expect("active draft should forward")
    );

    let settled_termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![
                    TelegramSessionFetchMessage::Text {
                        id: "message-1".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Old assistant text".to_owned(),
                    },
                    TelegramSessionFetchMessage::Text {
                        id: "message-2".to_owned(),
                        author: "assistant".to_owned(),
                        text: "Final reply".to_owned(),
                    },
                ],
            },
        },
    };
    assert!(
        forward_new_assistant_message_if_any(
            &telegram,
            &settled_termal,
            &mut state,
            42,
            "session-1"
        )
        .expect("settled replacement should forward")
    );

    let sent = telegram.sent_texts.borrow();
    assert_eq!(sent[0], "Draft reply");
    assert_eq!(sent[1], "Final reply");
    assert!(sent[2].contains("turn complete"));
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("settled replacement should persist cursor");
    assert_eq!(cursor.message_id.as_deref(), Some("message-2"));
    assert_eq!(cursor.text_chars, Some("Final reply".chars().count()));
    assert_eq!(
        cursor.text_hash.as_deref(),
        Some(telegram_assistant_text_hash("Final reply").as_str())
    );
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
    )
    .with_state_sessions(telegram_state_sessions_with_project_session(
        "session-1",
        None,
    ));
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
            session_mutation_stamp: None,
            parent_delegation_id: None,
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
            message_count: 1,
            session_mutation_stamp: None,
            parent_delegation_id: None,
        }],
    });
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
    assert_eq!(telegram.sent_texts.borrow()[0], "Telegram reply");
    assert_eq!(state.forward_next_assistant_message_session_id, None);
}

#[test]
fn telegram_active_baseline_same_message_growth_on_first_settled_poll_stays_baseline() {
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
    let settled_with_same_message_growth = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Old turn partial\nTelegram reply already present".to_owned(),
                }],
            },
        },
    };

    let changed = forward_new_assistant_message_if_any(
        &telegram,
        &settled_with_same_message_growth,
        &mut state,
        42,
        "session-1",
    )
    .expect("first settled poll should baseline same-message growth");

    assert!(changed);
    assert!(
        telegram.sent_texts.borrow().is_empty(),
        "same-message growth already present on the first settled poll is treated as baseline"
    );
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("cursor should stay persisted");
    assert_eq!(cursor.message_id.as_deref(), Some("message-1"));
    assert_eq!(
        cursor.text_chars,
        Some(
            "Old turn partial\nTelegram reply already present"
                .chars()
                .count()
        )
    );
    assert!(cursor.resend_if_grown);
    assert!(!cursor.baseline_while_active);
    assert_eq!(
        state.forward_next_assistant_message_session_id.as_deref(),
        Some("session-1")
    );
}

#[test]
fn telegram_same_message_suffix_retry_resumes_inside_suffix_window() {
    let prefix = "Already forwarded. ";
    let suffix = format!(
        "{}\n{}",
        "a".repeat(TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS),
        "b".repeat(64)
    );
    let full_text = format!("{prefix}{suffix}");
    let termal = FakeTelegramSessionReader {
        response: TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "message-1".to_owned(),
                    author: "assistant".to_owned(),
                    text: full_text.clone(),
                }],
            },
        },
    };
    let mut state = TelegramBotState {
        assistant_forwarding_cursors: HashMap::from([(
            "session-1".to_owned(),
            TelegramAssistantForwardingCursor {
                message_id: Some("message-1".to_owned()),
                text_chars: Some(prefix.chars().count()),
                text_hash: Some(telegram_assistant_text_hash(prefix)),
                text_start_chars: None,
                resend_if_grown: true,
                sent_chunks: None,
                failed_chunk_send_attempts: None,
                footer_pending: false,
                baseline_while_active: false,
            },
        )]),
        ..TelegramBotState::default()
    };
    let suffix_chunks = chunk_telegram_message_text(&suffix);
    assert!(
        suffix_chunks.len() > 1,
        "test suffix must span multiple Telegram chunks"
    );

    let first_attempt = FakeTelegramSender::new(Some(2));
    let changed =
        forward_new_assistant_message_if_any(&first_attempt, &termal, &mut state, 42, "session-1")
            .expect("first suffix forward should handle chunk failure");

    assert!(changed);
    assert_eq!(first_attempt.sent_texts.borrow().len(), 1);
    assert_eq!(first_attempt.sent_texts.borrow()[0], suffix_chunks[0]);
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("failed suffix send should persist cursor");
    assert_eq!(cursor.text_chars, Some(full_text.chars().count()));
    assert_eq!(cursor.text_start_chars, Some(prefix.chars().count()));
    assert_eq!(cursor.sent_chunks, Some(1));

    let retry = FakeTelegramSender::new(None);
    let changed =
        forward_new_assistant_message_if_any(&retry, &termal, &mut state, 42, "session-1")
            .expect("retry should resume suffix window");

    assert!(changed);
    let retried = retry.sent_texts.borrow();
    assert_eq!(retried[0], suffix_chunks[1]);
    assert!(!retried[0].starts_with(prefix));
    assert!(retried[1].contains("turn complete"));
    let cursor = state
        .assistant_forwarding_cursors
        .get("session-1")
        .expect("completed retry should persist cursor");
    assert_eq!(cursor.text_chars, Some(full_text.chars().count()));
    assert_eq!(cursor.text_start_chars, None);
    assert_eq!(cursor.sent_chunks, None);
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
    let termal = RecordingTelegramSessionReaderById {
        requests: std::cell::RefCell::new(Vec::new()),
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
    assert_eq!(termal.requests.borrow().as_slice(), ["session-1"]);
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
                text_hash: None,
                text_start_chars: None,
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
                    text_hash: None,
                    text_start_chars: None,
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
                    text_hash: None,
                    text_start_chars: None,
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
                    text_hash: None,
                    text_start_chars: None,
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
                    text_hash: None,
                    text_start_chars: None,
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
                text_hash: None,
                text_start_chars: None,
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
                text_hash: None,
                text_start_chars: None,
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
                text_hash: None,
                text_start_chars: None,
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
                text_hash: None,
                text_start_chars: None,
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
fn telegram_forwarder_clear_ignores_non_matching_armed_session() {
    let mut state = TelegramBotState {
        forward_next_assistant_message_session_ids: vec!["session-a".to_owned()],
        forward_next_assistant_message_session_id: Some("session-a".to_owned()),
        ..TelegramBotState::default()
    };

    let changed = clear_forward_next_assistant_message_session_id(&mut state, "session-b");

    assert!(!changed);
    assert_eq!(
        state.forward_next_assistant_message_session_ids,
        vec!["session-a".to_owned()]
    );
    assert_eq!(
        state.forward_next_assistant_message_session_id.as_deref(),
        Some("session-a")
    );
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
                text_hash: None,
                text_start_chars: None,
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

#[test]
fn telegram_assistant_forward_error_marks_state_dirty() {
    let mut dirty = false;

    merge_assistant_forward_result(&mut dirty, Err(anyhow!("second send failed")));

    assert!(dirty);
}
