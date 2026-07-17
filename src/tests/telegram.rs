// Residual Telegram relay adapter tests split from this file into focused
// siblings. This module owns command routing, Telegram/TermAl wire projections,
// validation, error classification, log sanitization, prompt/message size
// guards, route behavior, and rate-limit coverage that has not yet moved
// elsewhere.
//
// It deliberately does not own the assistant-forwarding, settings
// persistence, or relay lifecycle suites now split into
// `telegram_forwarding.rs`, `telegram_settings.rs`, and
// `telegram_relay_lifecycle.rs`. Shared Telegram fixtures live in
// `telegram_support.rs`.

use super::telegram_support::{
    FakeTelegramActionClient, FakeTelegramPromptClient, FakeTelegramSender,
    TELEGRAM_TEST_RATE_LIMIT_TEST_LOCK, create_telegram_settings_project_and_session,
    telegram_project_digest, telegram_state_sessions_with_project_session, telegram_test_config,
    telegram_text_message,
};
use super::*;

mod rate_limit;

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
fn telegram_unlinked_start_links_chat_and_sends_help() {
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
    let mut config = telegram_test_config();
    config.chat_id = None;
    let mut state = TelegramBotState::default();

    let changed = handle_telegram_message(
        &telegram,
        &termal,
        &config,
        &mut state,
        TelegramChatMessage {
            message_id: 1,
            chat: TelegramChat {
                id: 123,
                _kind: "private".to_owned(),
            },
            text: Some("/start".to_owned()),
        },
    )
    .expect("unlinked startup message should link chat");

    assert!(changed);
    assert_eq!(state.chat_id, Some(123));
    assert_eq!(telegram.sent_texts.borrow().len(), 1);
    assert!(
        telegram.sent_texts.borrow()[0].contains("TermAl Telegram relay for project `project-1`.")
    );
}

#[test]
fn telegram_unlinked_start_with_matching_bot_suffix_links_chat() {
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
    let mut config = telegram_test_config();
    config.chat_id = None;
    let mut state = TelegramBotState::default();

    let changed = handle_telegram_message(
        &telegram,
        &termal,
        &config,
        &mut state,
        TelegramChatMessage {
            message_id: 1,
            chat: TelegramChat {
                id: 123,
                _kind: "private".to_owned(),
            },
            text: Some("/start@termal_bot".to_owned()),
        },
    )
    .expect("matching suffixed startup command should link chat");

    assert!(changed);
    assert_eq!(state.chat_id, Some(123));
    assert_eq!(telegram.sent_texts.borrow().len(), 1);
    assert!(telegram.sent_texts.borrow()[0].contains("TermAl Telegram relay"));
}

#[test]
fn telegram_linked_foreign_bot_command_is_ignored() {
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
    let mut state = TelegramBotState::default();

    let changed = handle_telegram_message(
        &telegram,
        &termal,
        &config,
        &mut state,
        TelegramChatMessage {
            message_id: 1,
            chat: TelegramChat {
                id: 42,
                _kind: "private".to_owned(),
            },
            text: Some("/start@other_bot".to_owned()),
        },
    )
    .expect("foreign bot command should be ignored");

    assert!(!changed);
    assert!(telegram.sent_texts.borrow().is_empty());
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
                session_mutation_stamp: Some(10),
                parent_delegation_id: None,
            },
            TelegramStateSession {
                id: "session-other".to_owned(),
                name: "Other Project".to_owned(),
                project_id: Some("project-2".to_owned()),
                status: TelegramSessionStatus::Active,
                message_count: 1,
                session_mutation_stamp: Some(99),
                parent_delegation_id: None,
            },
            TelegramStateSession {
                id: "session-2".to_owned(),
                name: "Current".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Active,
                message_count: 7,
                session_mutation_stamp: Some(20),
                parent_delegation_id: None,
            },
            TelegramStateSession {
                id: "session-3".to_owned(),
                name: "Newer Idle".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 3,
                session_mutation_stamp: Some(30),
                parent_delegation_id: None,
            },
            TelegramStateSession {
                id: "session-child".to_owned(),
                name: "Delegated Reviewer".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Active,
                message_count: 9,
                session_mutation_stamp: Some(100),
                parent_delegation_id: Some("delegation-1".to_owned()),
            },
        ],
    };

    let text = render_telegram_project_sessions("project-1", &state);

    assert!(text.starts_with("Sessions for TermAl:\n- Current"));
    assert!(!text.contains("id: session-2"));
    assert!(!text.contains("Working on the Telegram sessions list"));
    assert!(
        text.find("- Current")
            .expect("active session should render")
            < text
                .find("- Newer Idle")
                .expect("newer idle session should render")
    );
    assert!(
        text.find("- Newer Idle")
            .expect("newer idle session should render")
            < text
                .find("- Older")
                .expect("older idle session should render")
    );
    assert!(text.contains("- Older (idle, 2 messages)"));
    assert!(!text.contains("Delegated Reviewer"));
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
                session_mutation_stamp: Some(index as u64),
                parent_delegation_id: None,
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
fn telegram_sessions_renderer_falls_back_to_message_count_when_stamp_is_missing() {
    let state = TelegramStateSessionsResponse {
        projects: vec![TelegramStateProject {
            id: "project-1".to_owned(),
            name: "TermAl".to_owned(),
        }],
        sessions: vec![
            TelegramStateSession {
                id: "session-a".to_owned(),
                name: "Idle older".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 2,
                session_mutation_stamp: None,
                parent_delegation_id: None,
            },
            TelegramStateSession {
                id: "session-b".to_owned(),
                name: "Idle newer".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 9,
                session_mutation_stamp: None,
                parent_delegation_id: None,
            },
        ],
    };

    let text = render_telegram_project_sessions("project-1", &state);

    assert!(
        text.find("- Idle newer")
            .expect("newer idle session should render")
            < text
                .find("- Idle older")
                .expect("older idle session should render")
    );
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
                "messageCount": 7,
                "sessionMutationStamp": 42,
                "parentDelegationId": "delegation-1"
            },
            { "id": "session-future", "name": "Future", "status": "queued" }
        ]
    }))
    .expect("state projection should decode");

    assert_eq!(state.sessions[0].status, TelegramSessionStatus::Active);
    assert_eq!(state.sessions[0].project_id.as_deref(), Some("project-1"));
    assert_eq!(state.sessions[0].message_count, 7);
    assert_eq!(state.sessions[0].session_mutation_stamp, Some(42));
    assert_eq!(
        state.sessions[0].parent_delegation_id.as_deref(),
        Some("delegation-1")
    );
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
            session_mutation_stamp: None,
            parent_delegation_id: None,
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
    assert!(!sent_texts[0].contains("id: session-1"));
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
            session_mutation_stamp: None,
            parent_delegation_id: None,
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
fn telegram_relay_iteration_caps_oversized_update_batches_and_persists_cursor() {
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
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();
    let start_update_id = 100_i64;
    let updates = (0..TELEGRAM_MAX_UPDATES_PER_ITERATION + 5)
        .map(|index| TelegramUpdate {
            update_id: start_update_id + index as i64,
            callback_query: None,
            message: Some(TelegramChatMessage {
                message_id: index as i64,
                chat: TelegramChat {
                    id: 999,
                    _kind: "private".to_owned(),
                },
                text: Some("/status".to_owned()),
            }),
        })
        .collect::<Vec<_>>();

    let dirty = drain_telegram_updates_then_sync_digest(
        &telegram, &termal, &config, &mut state, updates, &None,
    );

    assert!(dirty);
    let expected_next_update_id = start_update_id + TELEGRAM_MAX_UPDATES_PER_ITERATION as i64;
    assert_eq!(state.next_update_id, Some(expected_next_update_id));
    assert_eq!(
        termal.digest_project_ids.borrow().as_slice(),
        ["project-1".to_owned()]
    );
    let persisted: TelegramBotFile = serde_json::from_slice(
        &fs::read(&config.state_path).expect("cursor state should persist per handled update"),
    )
    .expect("persisted Telegram state should decode");
    assert_eq!(
        persisted.state.next_update_id,
        Some(expected_next_update_id)
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
    let termal = termal.with_state_sessions(telegram_state_sessions_with_project_session(
        "session-1",
        None,
    ));
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
            session_mutation_stamp: None,
            parent_delegation_id: None,
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
            session_mutation_stamp: None,
            parent_delegation_id: None,
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
                session_mutation_stamp: Some(index as u64),
                parent_delegation_id: None,
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
                session_mutation_stamp: None,
                parent_delegation_id: None,
            },
            TelegramStateSession {
                id: "session-2".to_owned(),
                name: "Other".to_owned(),
                project_id: Some("project-2".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 1,
                session_mutation_stamp: None,
                parent_delegation_id: None,
            },
        ],
    };

    let text = render_telegram_projects(&config, &bot_state, &state);

    assert!(text.contains("- TermAl (1 session)\n  id: project-1"));
    assert!(text.contains("* Side Project (1 session)\n  id: project-2"));
    assert!(text.contains("Send /project <project-id> to switch."));
}

#[test]
fn telegram_projects_renderer_counts_only_project_root_sessions() {
    let mut config = telegram_test_config();
    config.subscribed_project_ids = vec!["project-1".to_owned()];
    let bot_state = TelegramBotState::default();
    let state = TelegramStateSessionsResponse {
        projects: vec![TelegramStateProject {
            id: "project-1".to_owned(),
            name: "TermAl".to_owned(),
        }],
        sessions: vec![
            TelegramStateSession {
                id: "session-root".to_owned(),
                name: "Root".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 1,
                session_mutation_stamp: None,
                parent_delegation_id: None,
            },
            TelegramStateSession {
                id: "session-child".to_owned(),
                name: "Delegated child".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 1,
                session_mutation_stamp: None,
                parent_delegation_id: Some("delegation-1".to_owned()),
            },
        ],
    };

    let text = render_telegram_projects(&config, &bot_state, &state);

    assert!(text.contains("TermAl (1 session)\n  id: project-1"));
    assert!(!text.contains("2 sessions"));
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
                session_mutation_stamp: None,
                parent_delegation_id: None,
            },
            TelegramStateSession {
                id: "session-2".to_owned(),
                name: "Target Session".to_owned(),
                project_id: Some("project-1".to_owned()),
                status: TelegramSessionStatus::Idle,
                message_count: 0,
                session_mutation_stamp: None,
                parent_delegation_id: None,
            },
        ],
    });
    let config = telegram_test_config();
    let mut state = TelegramBotState::default();

    let changed = select_telegram_project_session(
        &telegram,
        &termal,
        &config,
        &mut state,
        42,
        "Target Session",
    )
    .expect("session selection should succeed");

    assert!(changed);
    assert_eq!(state.selected_session_id.as_deref(), Some("session-2"));
    assert!(
        telegram.sent_texts.borrow()[0].contains("Telegram session target set to Target Session")
    );
    assert!(!telegram.sent_texts.borrow()[0].contains("id: session-2"));
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
fn telegram_session_command_does_not_baseline_when_assistant_forwarding_disabled() {
    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramPromptClient::new(
        Vec::new(),
        TelegramSessionFetchResponse {
            session: TelegramSessionFetchSession {
                status: TelegramSessionStatus::Idle,
                messages: vec![TelegramSessionFetchMessage::Text {
                    id: "baseline".to_owned(),
                    author: "assistant".to_owned(),
                    text: "Existing selected-session reply".to_owned(),
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
            name: "Target Session".to_owned(),
            project_id: Some("project-1".to_owned()),
            status: TelegramSessionStatus::Idle,
            message_count: 1,
            session_mutation_stamp: None,
            parent_delegation_id: None,
        }],
    });
    let mut config = telegram_test_config();
    config.forward_assistant_replies = false;
    let mut state = TelegramBotState::default();

    let changed =
        select_telegram_project_session(&telegram, &termal, &config, &mut state, 42, "session-2")
            .expect("session selection should succeed");

    assert!(changed);
    assert_eq!(state.selected_session_id.as_deref(), Some("session-2"));
    assert!(state.assistant_forwarding_cursors.is_empty());
    assert!(state.forward_next_assistant_message_session_ids.is_empty());
    assert_eq!(state.forward_next_assistant_message_session_id, None);
    let sent_texts = telegram.sent_texts.borrow();
    assert_eq!(sent_texts.len(), 1);
    assert!(sent_texts[0].contains("Telegram session target set to Target Session"));
    assert_eq!(
        termal.events.borrow().as_slice(),
        ["state-sessions".to_owned()],
        "session selection should not fetch a baseline when forwarding is disabled"
    );
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

#[test]
fn telegram_standalone_token_preserves_non_ascii_adjacent_text() {
    let token = telegram_redaction_token();
    let prefix = format!("botToken:{}{token}", '\u{0442}');
    let suffix = format!("botToken={token}{}", '\u{044f}');

    assert_eq!(sanitize_telegram_log_detail(&prefix), prefix);
    assert_eq!(sanitize_telegram_log_detail(&suffix), suffix);
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

    let null_project_list: UpdateTelegramConfigRequest =
        serde_json::from_value(json!({ "subscribedProjectIds": null }))
            .expect("request should deserialize");
    assert_eq!(null_project_list.subscribed_project_ids, None);

    let missing: UpdateTelegramConfigRequest =
        serde_json::from_value(json!({})).expect("request should deserialize");
    assert_eq!(missing.bot_token, None);
    assert_eq!(missing.subscribed_project_ids, None);
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
        forward_assistant_replies: false,
        running: false,
        lifecycle: TelegramLifecycle::InProcess,
        linked_chat_id: None,
        bot_token_masked: None,
        subscribed_project_ids: Vec::new(),
        default_project_id: None,
        default_session_id: None,
    })
    .expect("response should serialize");

    assert_eq!(value["subscribedProjectIds"], json!([]));
}

#[test]
fn telegram_status_response_serializes_in_process_lifecycle() {
    let value = serde_json::to_value(TelegramStatusResponse {
        configured: true,
        enabled: true,
        forward_assistant_replies: true,
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
                text_hash: None,
                text_start_chars: None,
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
        config_migrated_to_app_state: true,
        state: TelegramBotState::default(),
    }
}

fn telegram_ui_relay_token(file: &TelegramBotFile) -> Option<String> {
    file.config.bot_token.clone()
}

fn read_telegram_settings_file_without_plaintext_token(
    path: &std::path::Path,
    token: &str,
) -> Value {
    let raw = fs::read(path).expect("settings file should read");
    let text = String::from_utf8_lossy(&raw);
    assert!(
        !text.contains(token),
        "settings file should not contain plaintext Telegram bot token: {text}"
    );
    assert!(
        !text.contains("botToken"),
        "settings file should not retain legacy botToken field: {text}"
    );
    let value: Value =
        serde_json::from_slice(&raw).expect("settings file should remain valid JSON");
    assert!(value["config"].get("botToken").is_none());
    value
}

fn set_telegram_token_entry_error(state: &AppState, err: keyring_core::Error) {
    let entry = state
        .telegram_bot_token_entry()
        .expect("mock Telegram token entry should open");
    let mock = entry
        .as_any()
        .downcast_ref::<keyring_core::mock::Cred>()
        .expect("test Telegram secret store should use mock credentials");
    mock.set_error(err);
}

fn telegram_keyring_storage_error(label: &'static str) -> keyring_core::Error {
    keyring_core::Error::NoStorageAccess(Box::new(io::Error::new(
        io::ErrorKind::PermissionDenied,
        label,
    )))
}

#[test]
fn telegram_ui_file_uses_single_subscribed_project_for_relay_config() {
    let file = telegram_ui_relay_file(telegram_ui_relay_config());
    let config = TelegramBotConfig::from_ui_file("/tmp", &file, telegram_ui_relay_token(&file))
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
    let config = TelegramBotConfig::from_ui_file(
        "/tmp",
        &with_blank_default,
        telegram_ui_relay_token(&with_blank_default),
    )
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

    assert_eq!(
        TelegramBotConfig::from_ui_file(
            "/tmp",
            &without_any_project,
            telegram_ui_relay_token(&without_any_project),
        )
        .expect_err("relay config without a project target should be unavailable"),
        TelegramRelayConfigUnavailableReason::MissingProjectTarget
    );
}

#[test]
fn telegram_ui_file_requires_default_when_multiple_projects_for_relay_config() {
    let with_multiple_projects = telegram_ui_relay_file(TelegramUiConfig {
        subscribed_project_ids: vec!["project-1".to_owned(), "project-2".to_owned()],
        default_project_id: None,
        ..telegram_ui_relay_config()
    });

    assert_eq!(
        TelegramBotConfig::from_ui_file(
            "/tmp",
            &with_multiple_projects,
            telegram_ui_relay_token(&with_multiple_projects),
        )
        .expect_err("ambiguous relay project target should be unavailable"),
        TelegramRelayConfigUnavailableReason::MissingProjectTarget
    );
}

#[test]
fn telegram_ui_file_uses_trimmed_default_project_for_relay_config() {
    let with_default = TelegramBotFile {
        config: TelegramUiConfig {
            default_project_id: Some(" project-1 ".to_owned()),
            subscribed_project_ids: vec![" project-2 ".to_owned(), "project-1".to_owned()],
            ..telegram_ui_relay_config()
        },
        config_migrated_to_app_state: true,
        state: TelegramBotState {
            chat_id: Some(42),
            ..TelegramBotState::default()
        },
    };
    let config = TelegramBotConfig::from_ui_file(
        "/tmp",
        &with_default,
        telegram_ui_relay_token(&with_default),
    )
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

    assert_eq!(
        TelegramBotConfig::from_ui_file("/tmp", &disabled, telegram_ui_relay_token(&disabled))
            .expect_err("disabled relay config should be unavailable"),
        TelegramRelayConfigUnavailableReason::Disabled
    );
}

#[test]
fn telegram_ui_file_requires_bot_token_for_relay_config() {
    let missing_token = telegram_ui_relay_file(TelegramUiConfig {
        bot_token: None,
        ..telegram_ui_relay_config()
    });

    assert_eq!(
        TelegramBotConfig::from_ui_file(
            "/tmp",
            &missing_token,
            telegram_ui_relay_token(&missing_token),
        )
        .expect_err("relay config without a bot token should be unavailable"),
        TelegramRelayConfigUnavailableReason::MissingBotToken
    );
}

#[test]
fn telegram_ui_file_rejects_empty_bot_token_for_relay_config() {
    let empty_token = telegram_ui_relay_file(TelegramUiConfig {
        bot_token: Some(String::new()),
        ..telegram_ui_relay_config()
    });

    assert_eq!(
        TelegramBotConfig::from_ui_file(
            "/tmp",
            &empty_token,
            telegram_ui_relay_token(&empty_token)
        )
        .expect_err("relay config with an empty bot token should be unavailable"),
        TelegramRelayConfigUnavailableReason::MissingBotToken
    );
}

#[test]
fn telegram_ui_file_rejects_whitespace_bot_token_for_relay_config() {
    let whitespace_token = telegram_ui_relay_file(TelegramUiConfig {
        bot_token: Some("   ".to_owned()),
        ..telegram_ui_relay_config()
    });

    assert_eq!(
        TelegramBotConfig::from_ui_file(
            "/tmp",
            &whitespace_token,
            telegram_ui_relay_token(&whitespace_token),
        )
        .expect_err("relay config with a whitespace bot token should be unavailable"),
        TelegramRelayConfigUnavailableReason::MissingBotToken
    );
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

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
#[test]
#[ignore = "writes a disposable credential to the real OS store; run explicitly on Windows/macOS/Linux"]
fn telegram_bot_token_native_credential_store_round_trips() {
    let store = native_telegram_secret_store().expect("native credential store should initialize");
    let user = format!(
        "{TELEGRAM_BOT_TOKEN_KEYRING_USER_PREFIX}:platform-smoke:{}",
        Uuid::new_v4()
    );
    let token = format!("123456:{}:{}", Uuid::new_v4(), Uuid::new_v4());
    let entry = store
        .build(TELEGRAM_BOT_TOKEN_KEYRING_SERVICE, &user, None)
        .expect("TermAl Telegram token entry should be valid for the native store");

    if let Err(err) = entry.delete_credential() {
        assert!(
            matches!(err, keyring_core::Error::NoEntry),
            "pre-test credential cleanup should only fail for missing entry, got {err:?}"
        );
    }
    entry
        .set_password(&token)
        .expect("native credential store should save Telegram token");
    assert_eq!(
        entry
            .get_password()
            .expect("native credential store should read saved Telegram token"),
        token
    );
    entry
        .delete_credential()
        .expect("native credential store should delete smoke-test token");
    assert!(
        matches!(entry.get_password(), Err(keyring_core::Error::NoEntry)),
        "deleted native credential should not remain readable"
    );
}

#[test]
fn telegram_config_update_stores_token_only_in_credential_store() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-token-at-rest-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_home_dir(&home);
    let state = test_app_state();
    let (project_id, _session_id) = create_telegram_settings_project_and_session(&state);
    let token = "123456:secret-at-rest";

    let response = state
        .update_telegram_config(UpdateTelegramConfigRequest {
            enabled: Some(true),
            forward_assistant_replies: None,
            bot_token: Some(Some(token.to_owned())),
            subscribed_project_ids: Some(vec![project_id.clone()]),
            default_project_id: None,
            default_session_id: None,
        })
        .expect("Telegram token should save to credential store");

    assert!(response.configured);
    assert_eq!(response.bot_token_masked.as_deref(), Some("****rest"));
    assert_eq!(
        state
            .saved_telegram_bot_token()
            .expect("saved token should read")
            .as_deref(),
        Some(token)
    );
    let value =
        read_telegram_settings_file_without_plaintext_token(&state.telegram_bot_file_path(), token);
    assert_eq!(value["config"]["enabled"], json!(true));
    assert_eq!(
        value["config"]["subscribedProjectIds"],
        json!([project_id.clone()])
    );
    assert_eq!(value["config"]["defaultProjectId"], json!(project_id));

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn telegram_status_migrates_legacy_plaintext_token_out_of_settings_file() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-token-migration-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_home_dir(&home);
    let state = test_app_state();
    let path = state.telegram_bot_file_path();
    let token = "123456:legacy-secret";
    fs::create_dir_all(path.parent().expect("settings path should have a parent"))
        .expect("settings dir should create");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "config": {
                "enabled": false,
                "botToken": token
            },
            "chatId": 123
        }))
        .expect("fixture should encode"),
    )
    .expect("fixture should write");

    let response = state
        .telegram_status()
        .expect("status should migrate legacy plaintext token");

    assert!(response.configured);
    assert_eq!(response.bot_token_masked.as_deref(), Some("****cret"));
    assert_eq!(
        state
            .saved_telegram_bot_token()
            .expect("migrated token should read")
            .as_deref(),
        Some(token)
    );
    let value = read_telegram_settings_file_without_plaintext_token(&path, token);
    assert_eq!(value["configMigratedToAppState"], json!(true));
    assert_eq!(value["chatId"], json!(123));

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn telegram_config_update_keyring_write_failure_does_not_persist_plaintext_token() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-keyring-write-failure-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_home_dir(&home);
    let state = test_app_state();
    let (project_id, _session_id) = create_telegram_settings_project_and_session(&state);
    let token = "123456:write-failure-secret";
    let path = state.telegram_bot_file_path();

    let err = state
        .update_telegram_config_with_post_validation_hook(
            UpdateTelegramConfigRequest {
                enabled: Some(true),
                forward_assistant_replies: None,
                bot_token: Some(Some(token.to_owned())),
                subscribed_project_ids: Some(vec![project_id]),
                default_project_id: None,
                default_session_id: None,
            },
            |state| {
                set_telegram_token_entry_error(
                    state,
                    telegram_keyring_storage_error("forced Telegram token write failure"),
                );
                Ok(())
            },
        )
        .expect_err("credential-store write failure should abort settings save");

    assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        err.message
            .contains("failed to write Telegram bot token in OS credential store")
    );
    assert!(
        !path.exists(),
        "settings file should not be written after token save fails"
    );
    assert_eq!(
        state.saved_telegram_bot_token().expect("token should read"),
        None
    );
    assert_eq!(
        state.snapshot().preferences.telegram,
        TelegramUiConfig::default()
    );

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn telegram_config_update_keyring_write_failure_preserves_existing_settings() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-keyring-write-existing-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_home_dir(&home);
    let state = test_app_state();
    let (project_id, session_id) = create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();
    state
        .update_telegram_config(UpdateTelegramConfigRequest {
            enabled: Some(true),
            forward_assistant_replies: Some(true),
            bot_token: Some(Some("123456:existing-secret".to_owned())),
            subscribed_project_ids: Some(vec![project_id.clone()]),
            default_project_id: Some(Some(project_id.clone())),
            default_session_id: Some(Some(session_id.clone())),
        })
        .expect("initial Telegram settings should save");
    let original_file = fs::read(&path).expect("settings file should read before failure");
    let original_config = state.snapshot().preferences.telegram;

    let err = state
        .update_telegram_config_with_post_validation_hook(
            UpdateTelegramConfigRequest {
                enabled: Some(false),
                forward_assistant_replies: Some(false),
                bot_token: Some(Some("123456:new-secret".to_owned())),
                subscribed_project_ids: Some(vec![project_id]),
                default_project_id: None,
                default_session_id: None,
            },
            |state| {
                set_telegram_token_entry_error(
                    state,
                    telegram_keyring_storage_error("forced Telegram token overwrite failure"),
                );
                Ok(())
            },
        )
        .expect_err("credential-store overwrite failure should abort settings save");

    assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        err.message
            .contains("failed to write Telegram bot token in OS credential store")
    );
    assert_eq!(
        fs::read(&path).expect("settings file should still read"),
        original_file
    );
    assert_eq!(state.snapshot().preferences.telegram, original_config);
    assert_eq!(
        state
            .saved_telegram_bot_token()
            .expect("mock write error should be one-shot")
            .as_deref(),
        Some("123456:existing-secret")
    );

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn telegram_config_update_post_validation_hook_error_aborts_before_persist() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-post-validation-error-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_home_dir(&home);
    let state = test_app_state();
    let (project_id, _session_id) = create_telegram_settings_project_and_session(&state);
    let path = state.telegram_bot_file_path();

    let err = state
        .update_telegram_config_with_post_validation_hook(
            UpdateTelegramConfigRequest {
                enabled: Some(true),
                forward_assistant_replies: None,
                bot_token: Some(Some("123456:hook-error-secret".to_owned())),
                subscribed_project_ids: Some(vec![project_id]),
                default_project_id: None,
                default_session_id: None,
            },
            |_| Err(ApiError::internal("forced post-validation hook failure")),
        )
        .expect_err("post-validation hook failure should abort settings save");

    assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(err.message, "forced post-validation hook failure");
    assert!(!path.exists());
    assert_eq!(
        state.saved_telegram_bot_token().expect("token should read"),
        None
    );
    assert_eq!(
        state.snapshot().preferences.telegram,
        TelegramUiConfig::default()
    );

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn telegram_status_keyring_read_failure_surfaces_without_unconfigured_fallback() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-keyring-read-failure-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_home_dir(&home);
    let state = test_app_state();
    state
        .save_telegram_bot_token("123456:read-failure-secret")
        .expect("token should save before injecting read failure");
    set_telegram_token_entry_error(
        &state,
        telegram_keyring_storage_error("forced Telegram token read failure"),
    );

    let err = state
        .telegram_status()
        .expect_err("credential-store read failure should surface");

    assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        err.message
            .contains("failed to read Telegram bot token in OS credential store")
    );
    let value = read_telegram_settings_file_without_plaintext_token(
        &state.telegram_bot_file_path(),
        "123456:read-failure-secret",
    );
    assert_eq!(value["configMigratedToAppState"], json!(true));
    // `keyring_core::mock::Cred::set_error` is one-shot; this proves the
    // failure path surfaced the read error without deleting the secret.
    assert_eq!(
        state
            .saved_telegram_bot_token()
            .expect("mock read error should be one-shot")
            .as_deref(),
        Some("123456:read-failure-secret")
    );

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn telegram_test_rate_limit_rejects_immediate_retry() {
    let _rate_limit_lock = TELEGRAM_TEST_RATE_LIMIT_TEST_LOCK
        .lock()
        .expect("telegram test rate-limit test mutex poisoned");
    reset_telegram_test_rate_limit_for_tests();
    let token = format!("123456:{}:{}", Uuid::new_v4(), Uuid::new_v4());

    check_telegram_test_rate_limit(&token).expect("first attempt should pass");
    let err = check_telegram_test_rate_limit(&token).expect_err("retry should be rate-limited");

    assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
    reset_telegram_test_rate_limit_for_tests();
}

#[test]
fn telegram_test_rate_limit_rejects_immediate_retry_with_different_token() {
    let _rate_limit_lock = TELEGRAM_TEST_RATE_LIMIT_TEST_LOCK
        .lock()
        .expect("telegram test rate-limit test mutex poisoned");
    reset_telegram_test_rate_limit_for_tests();
    let first_token = format!("123456:{}:{}", Uuid::new_v4(), Uuid::new_v4());
    let second_token = format!("123456:{}:{}", Uuid::new_v4(), Uuid::new_v4());

    check_telegram_test_rate_limit(&first_token).expect("first attempt should pass");
    let err = check_telegram_test_rate_limit(&second_token)
        .expect_err("endpoint-level retry should be rate-limited");

    assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
    reset_telegram_test_rate_limit_for_tests();
}

#[test]
fn telegram_test_rate_limit_allows_retry_after_cooldown_expires() {
    let _rate_limit_lock = TELEGRAM_TEST_RATE_LIMIT_TEST_LOCK
        .lock()
        .expect("telegram test rate-limit test mutex poisoned");
    reset_telegram_test_rate_limit_for_tests();
    let token = format!("123456:{}:{}", Uuid::new_v4(), Uuid::new_v4());

    check_telegram_test_rate_limit(&token).expect("first attempt should pass");
    age_telegram_test_rate_limit_for_tests(TELEGRAM_TEST_COOLDOWN + Duration::from_millis(1));
    check_telegram_test_rate_limit(&token).expect("retry after cooldown should pass");
    let err = check_telegram_test_rate_limit(&token)
        .expect_err("successful retry should re-prime the cooldown");

    assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
    reset_telegram_test_rate_limit_for_tests();
}

#[tokio::test]
async fn telegram_test_route_rate_limit_includes_retry_after_header() {
    let _rate_limit_lock = TELEGRAM_TEST_RATE_LIMIT_TEST_LOCK
        .lock()
        .expect("telegram test rate-limit test mutex poisoned");
    reset_telegram_test_rate_limit_for_tests();
    let token = format!("123456:{}:{}", Uuid::new_v4(), Uuid::new_v4());
    check_telegram_test_rate_limit(&token).expect("first attempt should prime rate limit");

    let response = request_response(
        &app_router(test_app_state()),
        Request::post("/api/telegram/test")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "botToken": token,
                    "useSavedToken": false
                })
                .to_string(),
            ))
            .expect("request should build"),
    )
    .await;
    let status = response.status();
    let retry_after = response
        .headers()
        .get(axum::http::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should read");
    let error: ErrorResponse =
        serde_json::from_slice(&body).expect("rate-limit response should be JSON");

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        retry_after.as_deref(),
        Some(TELEGRAM_TEST_COOLDOWN_RETRY_AFTER)
    );
    assert_eq!(error.error, TELEGRAM_TEST_RATE_LIMIT_MESSAGE);
    reset_telegram_test_rate_limit_for_tests();
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
fn telegram_state_persist_rejects_malformed_existing_file_without_overwriting() {
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
    let err = persist_telegram_bot_state(&path, &state)
        .expect_err("malformed existing settings should not be replaced with defaults");

    assert!(
        err.to_string()
            .contains("failed to parse existing Telegram bot file"),
        "unexpected error: {err}"
    );
    assert_eq!(
        fs::read(&path).expect("state file should still exist"),
        b"{"
    );

    fs::remove_file(&path).ok();
}

#[test]
fn telegram_state_corrupt_backup_falls_back_to_copy_when_rename_fails() {
    let path = std::env::temp_dir().join(format!(
        "termal-telegram-copy-backup-state-{}.json",
        Uuid::new_v4()
    ));
    fs::write(&path, b"{").expect("fixture should write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
            .expect("fixture permissions should set");
    }
    let backup_path = corrupt_telegram_bot_file_backup_path(&path);

    backup_corrupt_telegram_bot_file_with_rename(&path, &backup_path, |_, _| {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "forced rename failure",
        ))
    })
    .expect("copy fallback should quarantine corrupt state");

    assert!(!path.exists());
    assert_eq!(fs::read(&backup_path).expect("backup should read"), b"{");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let mode = fs::metadata(&backup_path)
            .expect("backup metadata should read")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    fs::remove_file(&backup_path).ok();
    fs::remove_file(&path).ok();
}

#[test]
fn telegram_state_load_quarantines_corrupt_file_with_hardened_backup() {
    let root = std::env::temp_dir().join(format!(
        "termal-telegram-corrupt-backup-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir(&root).expect("fixture directory should create");
    let path = root.join("telegram-bot.json");
    fs::write(&path, b"{").expect("fixture should write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
            .expect("fixture permissions should set");
    }

    let state = load_telegram_bot_state(&path).expect("corrupt state should default after backup");

    assert_eq!(state.chat_id, None);
    assert_eq!(state.next_update_id, None);
    assert_eq!(state.selected_project_id, None);
    assert_eq!(state.selected_session_id, None);
    assert!(!path.exists());
    let backups: Vec<PathBuf> = fs::read_dir(&root)
        .expect("fixture directory should read")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|candidate| {
            candidate
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| {
                    name.starts_with("telegram-bot.json.corrupt-") && name.ends_with(".json")
                })
        })
        .collect();
    assert_eq!(backups.len(), 1);
    let backup_path = &backups[0];
    assert_eq!(fs::read(backup_path).expect("backup should read"), b"{");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let mode = fs::metadata(backup_path)
            .expect("backup metadata should read")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    fs::remove_dir_all(&root).ok();
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
fn telegram_bot_file_write_removes_temp_after_write_failure() {
    let root =
        std::env::temp_dir().join(format!("termal-telegram-write-cleanup-{}", Uuid::new_v4()));
    fs::create_dir(&root).expect("fixture directory should create");
    let path = root.join("telegram-bot.json");

    let err = write_telegram_bot_file_with_writer(
        &path,
        b"{\"chatId\":123}",
        |temp_path, _| {
            fs::write(temp_path, b"{\"chatId\"").expect("partial temp file should write");
            Err(io::Error::new(
                io::ErrorKind::Other,
                "forced temp write failure",
            ))
        },
        |_, _| Ok(()),
    )
    .expect_err("write failure should propagate");

    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert!(!path.exists());
    let leaked_temps: Vec<PathBuf> = fs::read_dir(&root)
        .expect("fixture directory should read")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|candidate| {
            candidate
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| {
                    name.starts_with(".telegram-bot.json.") && name.ends_with(".tmp")
                })
        })
        .collect();
    assert!(leaked_temps.is_empty());

    fs::remove_dir_all(&root).ok();
}

#[cfg(windows)]
#[test]
fn telegram_bot_file_replace_overwrites_existing_file_on_windows() {
    let root = std::env::temp_dir().join(format!(
        "termal-telegram-windows-replace-{}",
        Uuid::new_v4()
    ));
    fs::create_dir(&root).expect("fixture directory should create");
    let path = root.join("telegram-bot.json");
    let temp_path = root.join(".telegram-bot.json.replace-test.tmp");
    fs::write(&path, b"{\"chatId\":1}").expect("existing file should write");
    fs::write(&temp_path, b"{\"chatId\":2}").expect("temp file should write");

    replace_telegram_bot_file(&temp_path, &path).expect("replacement should succeed");

    assert_eq!(
        fs::read(&path).expect("replaced file should read"),
        b"{\"chatId\":2}"
    );
    assert!(!temp_path.exists());

    fs::remove_dir_all(&root).ok();
}

#[test]
fn telegram_turn_settled_footer_covers_known_and_fallback_statuses() {
    assert_eq!(
        telegram_turn_settled_footer(&TelegramSessionStatus::Idle),
        "─────────── ✓ turn complete ───────────"
    );
    assert_eq!(
        telegram_turn_settled_footer(&TelegramSessionStatus::Approval),
        "─────────── ⏸ approval needed ───────────"
    );
    assert_eq!(
        telegram_turn_settled_footer(&TelegramSessionStatus::Error),
        "─────────── ⚠ stopped on error ───────────"
    );
    assert_eq!(
        telegram_turn_settled_footer(&TelegramSessionStatus::Unknown),
        "─────────── ✓ turn complete ───────────"
    );
}

#[test]
fn telegram_update_decodes_real_snake_case_bot_api_shape() {
    let update: TelegramUpdate = serde_json::from_value(json!({
        "update_id": 42,
        "callback_query": {
            "id": "callback-1",
            "data": "project-1:review",
            "message": {
                "message_id": 123,
                "chat": {
                    "id": 99,
                    "type": "private"
                },
                "text": "Digest"
            }
        },
        "message": {
            "message_id": 124,
            "chat": {
                "id": 99,
                "type": "private"
            },
            "text": "/status"
        }
    }))
    .expect("Telegram update should decode");

    assert_eq!(update.update_id, 42);
    let callback = update
        .callback_query
        .as_ref()
        .expect("callback query should decode");
    assert_eq!(callback.id, "callback-1");
    assert_eq!(callback.data.as_deref(), Some("project-1:review"));
    assert_eq!(
        callback.message.as_ref().map(|message| message.message_id),
        Some(123)
    );
    let message = update.message.as_ref().expect("message should decode");
    assert_eq!(message.message_id, 124);
    assert_eq!(message.chat.id, 99);
    assert_eq!(message.text.as_deref(), Some("/status"));
}

#[test]
fn telegram_message_chunks_cover_empty_under_limit_and_soft_breaks() {
    assert_eq!(chunk_telegram_message_text(""), vec![String::new()]);
    assert_eq!(
        chunk_telegram_message_text("short assistant reply"),
        vec!["short assistant reply".to_owned()]
    );

    let first_line = "a".repeat(TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS - 1);
    let text = format!("{first_line}\nsecond line");
    let chunks = chunk_telegram_message_text(&text);

    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks.concat(), text);
    assert!(chunks[0].ends_with('\n'));
    assert_eq!(chunks[1], "second line");
    assert!(
        chunks
            .iter()
            .all(|chunk| chunk.encode_utf16().count() <= TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS)
    );
}

#[test]
fn telegram_message_chunks_hard_split_when_no_newline_fits() {
    let text = format!(
        "{}{}",
        "a".repeat(TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS),
        "b".repeat(8)
    );
    let chunks = chunk_telegram_message_text(&text);

    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks.concat(), text);
    assert_eq!(chunks[0], "a".repeat(TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS));
    assert_eq!(chunks[1], "b".repeat(8));
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
fn telegram_settings_validation_rejects_delegated_default_session() {
    let state = test_app_state();
    let (project_id, session_id) = create_telegram_settings_project_and_session(&state);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter_mut()
            .find(|record| record.session.id == session_id)
            .expect("session should exist");
        record.session.parent_delegation_id = Some("delegation-1".to_owned());
        state
            .commit_locked(&mut inner)
            .expect("state should commit");
    }
    let mut config = TelegramUiConfig {
        subscribed_project_ids: vec![project_id.clone()],
        default_project_id: Some(project_id.clone()),
        default_session_id: Some(session_id.clone()),
        ..TelegramUiConfig::default()
    };

    let err = state
        .validate_and_normalize_telegram_config(&mut config)
        .expect_err("delegated child session should not validate as a Telegram default");

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(
        err.message
            .contains("default Telegram session cannot be a delegated child session")
    );
    assert_eq!(
        config.default_project_id.as_deref(),
        Some(project_id.as_str())
    );
    assert_eq!(
        config.default_session_id.as_deref(),
        Some(session_id.as_str())
    );
}

#[test]
fn telegram_settings_sanitization_clears_delegated_default_session() {
    let state = test_app_state();
    let (project_id, session_id) = create_telegram_settings_project_and_session(&state);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter_mut()
            .find(|record| record.session.id == session_id)
            .expect("session should exist");
        record.session.parent_delegation_id = Some("delegation-1".to_owned());
        state
            .commit_locked(&mut inner)
            .expect("state should commit");
    }
    let mut config = TelegramUiConfig {
        subscribed_project_ids: vec![project_id.clone()],
        default_project_id: Some(project_id.clone()),
        default_session_id: Some(session_id),
        ..TelegramUiConfig::default()
    };

    assert!(state.sanitize_telegram_config_for_current_state_in_place(&mut config));

    assert_eq!(
        config.default_project_id.as_deref(),
        Some(project_id.as_str())
    );
    assert_eq!(config.default_session_id, None);
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
fn telegram_settings_validation_rejects_overlong_target_ids() {
    let state = test_app_state();
    let overlong_id = "x".repeat(TELEGRAM_TARGET_ID_MAX_BYTES + 1);

    let cases = [
        (
            "Telegram subscribed project id",
            TelegramUiConfig {
                subscribed_project_ids: vec![overlong_id.clone()],
                ..TelegramUiConfig::default()
            },
        ),
        (
            "default Telegram project id",
            TelegramUiConfig {
                default_project_id: Some(overlong_id.clone()),
                ..TelegramUiConfig::default()
            },
        ),
        (
            "default Telegram session id",
            TelegramUiConfig {
                default_session_id: Some(overlong_id.clone()),
                ..TelegramUiConfig::default()
            },
        ),
    ];

    for (label, mut config) in cases {
        let err = state
            .validate_and_normalize_telegram_config(&mut config)
            .expect_err("overlong target id should fail validation");

        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(
            err.message.contains(label),
            "error `{}` should name `{label}`",
            err.message
        );
        assert!(
            err.message
                .contains(&format!("at most {TELEGRAM_TARGET_ID_MAX_BYTES} bytes")),
            "error `{}` should name the byte cap",
            err.message
        );
    }
}

#[test]
fn telegram_settings_validation_rejects_too_many_subscribed_projects() {
    let state = test_app_state();
    let mut config = TelegramUiConfig {
        subscribed_project_ids: (0..=TELEGRAM_SUBSCRIBED_PROJECT_IDS_MAX_COUNT)
            .map(|index| format!("project-{index}"))
            .collect(),
        ..TelegramUiConfig::default()
    };

    let err = state
        .validate_and_normalize_telegram_config(&mut config)
        .expect_err("oversized subscribed project list should fail validation");

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("Telegram subscribed projects"));
    assert!(
        err.message.contains(&format!(
            "at most {TELEGRAM_SUBSCRIBED_PROJECT_IDS_MAX_COUNT} projects"
        )),
        "error `{}` should name the list cap",
        err.message
    );
}

#[test]
fn telegram_config_update_rejects_too_many_subscribed_projects() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-too-many-projects-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_home_dir(&home);
    let state = test_app_state();

    let err = state
        .update_telegram_config(UpdateTelegramConfigRequest {
            enabled: None,
            forward_assistant_replies: None,
            bot_token: None,
            subscribed_project_ids: Some(
                (0..=TELEGRAM_SUBSCRIBED_PROJECT_IDS_MAX_COUNT)
                    .map(|index| format!("project-{index}"))
                    .collect(),
            ),
            default_project_id: None,
            default_session_id: None,
        })
        .expect_err("oversized subscribed project update should fail");

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("Telegram subscribed projects"));
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn telegram_config_update_rejects_delegated_default_session() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-delegated-default-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_home_dir(&home);
    let state = test_app_state();
    let (project_id, session_id) = create_telegram_settings_project_and_session(&state);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter_mut()
            .find(|record| record.session.id == session_id)
            .expect("session should exist");
        record.session.parent_delegation_id = Some("delegation-1".to_owned());
        state
            .commit_locked(&mut inner)
            .expect("state should commit");
    }

    let err = state
        .update_telegram_config(UpdateTelegramConfigRequest {
            enabled: None,
            forward_assistant_replies: None,
            bot_token: None,
            subscribed_project_ids: Some(vec![project_id.clone()]),
            default_project_id: Some(Some(project_id)),
            default_session_id: Some(Some(session_id)),
        })
        .expect_err("delegated child session should not save as Telegram default");

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(
        err.message
            .contains("default Telegram session cannot be a delegated child session")
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn telegram_config_update_allows_enabled_without_token_or_project_target() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-enabled-unconfigured-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_home_dir(&home);
    let state = test_app_state();
    let initial_revision = state.snapshot().revision;

    let response = state
        .update_telegram_config(UpdateTelegramConfigRequest {
            enabled: Some(true),
            forward_assistant_replies: None,
            bot_token: None,
            subscribed_project_ids: Some(Vec::new()),
            default_project_id: None,
            default_session_id: None,
        })
        .expect("enabled-but-unconfigured relay settings should save");

    assert!(response.enabled);
    assert!(!response.configured);
    assert_eq!(response.subscribed_project_ids, Vec::<String>::new());
    assert_eq!(response.default_project_id, None);
    let snapshot = state.snapshot();
    assert!(snapshot.revision > initial_revision);
    assert!(snapshot.preferences.telegram.enabled);
    assert!(
        snapshot
            .preferences
            .telegram
            .subscribed_project_ids
            .is_empty()
    );

    let path = state.telegram_bot_file_path();
    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    assert_eq!(value["config"]["enabled"], json!(true));
    assert!(value["config"].get("botToken").is_none());
    assert!(value["config"].get("subscribedProjectIds").is_none());
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn telegram_config_update_blank_token_clears_saved_token_before_project_target_check() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-blank-token-clears-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_home_dir(&home);
    let state = test_app_state();
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("settings path should have a parent"))
        .expect("settings dir should create");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "config": {
                "enabled": false,
                "botToken": "123456:secret"
            },
            "chatId": 123
        }))
        .expect("fixture should encode"),
    )
    .expect("fixture should write");

    let response = state
        .update_telegram_config(UpdateTelegramConfigRequest {
            enabled: Some(true),
            forward_assistant_replies: None,
            bot_token: Some(Some("   ".to_owned())),
            subscribed_project_ids: Some(Vec::new()),
            default_project_id: None,
            default_session_id: None,
        })
        .expect("blank token clears saved token before project-target validation");

    assert!(response.enabled);
    assert!(!response.configured);
    assert_eq!(response.subscribed_project_ids, Vec::<String>::new());
    assert_eq!(response.linked_chat_id, Some(123));

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    assert_eq!(value["config"]["enabled"], json!(true));
    assert!(value["config"].get("botToken").is_none());
    assert!(value["config"].get("subscribedProjectIds").is_none());
    assert_eq!(value["chatId"], json!(123));
    assert_eq!(
        state
            .saved_telegram_bot_token()
            .expect("cleared token should read"),
        None
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn telegram_config_update_rejects_saved_token_without_project_target() {
    let _env_lock = TEST_HOME_ENV_MUTEX.lock().expect("test env mutex poisoned");
    let home = std::env::temp_dir().join(format!(
        "termal-telegram-saved-token-no-project-home-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&home).expect("test home should exist");
    let _home = ScopedEnvVar::set_home_dir(&home);
    let state = test_app_state();
    let path = state.telegram_bot_file_path();
    fs::create_dir_all(path.parent().expect("settings path should have a parent"))
        .expect("settings dir should create");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "config": {
                "enabled": false,
                "botToken": "123456:secret"
            },
            "chatId": 123
        }))
        .expect("fixture should encode"),
    )
    .expect("fixture should write");

    let err = state
        .update_telegram_config(UpdateTelegramConfigRequest {
            enabled: Some(true),
            forward_assistant_replies: None,
            bot_token: None,
            subscribed_project_ids: Some(Vec::new()),
            default_project_id: None,
            default_session_id: None,
        })
        .expect_err("saved token plus enabled empty project target should fail");

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("choose at least one Telegram project"));

    let value: Value = serde_json::from_slice(&fs::read(&path).expect("settings file should read"))
        .expect("settings file should parse");
    assert_eq!(value["config"]["enabled"], json!(false));
    assert!(value["config"].get("botToken").is_none());
    assert_eq!(value["chatId"], json!(123));
    assert_eq!(
        state
            .saved_telegram_bot_token()
            .expect("migrated token should read")
            .as_deref(),
        Some("123456:secret")
    );
    let _ = fs::remove_dir_all(&home);
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
