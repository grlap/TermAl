use super::*;
use std::collections::VecDeque;
use std::time::Duration;

#[test]
fn telegram_chat_work_rate_limit_is_per_chat_and_windowed() {
    let mut state = TelegramBotState::default();
    let now = std::time::Instant::now();
    for _ in 0..TELEGRAM_CHAT_WORK_RATE_LIMIT_MAX {
        assert!(!telegram_chat_work_is_rate_limited_at(&mut state, 42, now));
    }
    assert!(telegram_chat_work_is_rate_limited_at(&mut state, 42, now));
    assert!(!telegram_chat_work_is_rate_limited_at(&mut state, 43, now));
    assert!(!telegram_chat_work_is_rate_limited_at(
        &mut state,
        42,
        now + TELEGRAM_CHAT_WORK_RATE_LIMIT_WINDOW + Duration::from_millis(1),
    ));
}

#[test]
fn telegram_chat_work_rate_limit_wrapper_uses_current_time() {
    let mut state = TelegramBotState::default();
    let now = std::time::Instant::now();
    state.chat_work_rate_limit.insert(
        42,
        VecDeque::from(vec![now; TELEGRAM_CHAT_WORK_RATE_LIMIT_MAX]),
    );

    assert!(telegram_chat_work_is_rate_limited(&mut state, 42));
}

#[test]
fn telegram_chat_work_rate_limit_prunes_idle_chat_buckets() {
    let mut state = TelegramBotState::default();
    let now = std::time::Instant::now();
    let expired = now - TELEGRAM_CHAT_WORK_RATE_LIMIT_WINDOW - Duration::from_millis(1);
    state
        .chat_work_rate_limit
        .insert(7, VecDeque::from(vec![expired]));

    assert!(!telegram_chat_work_is_rate_limited_at(&mut state, 42, now));

    assert!(!state.chat_work_rate_limit.contains_key(&7));
    assert!(state.chat_work_rate_limit.contains_key(&42));
}

#[test]
fn telegram_slash_action_rate_limit_blocks_dispatch() {
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
    let now = std::time::Instant::now();
    for _ in 0..TELEGRAM_CHAT_WORK_RATE_LIMIT_MAX {
        assert!(!telegram_chat_work_is_rate_limited_at(&mut state, 42, now));
    }

    let changed = handle_telegram_message(
        &telegram,
        &termal,
        &config,
        &mut state,
        telegram_text_message(42, "/commit"),
    )
    .expect("rate-limited action command should be handled");

    assert!(!changed);
    assert!(termal.events.borrow().is_empty());
    assert_eq!(
        telegram.sent_texts.borrow().as_slice(),
        [TELEGRAM_CHAT_WORK_RATE_LIMIT_TEXT.to_owned()]
    );
}

#[test]
fn telegram_free_text_rate_limit_blocks_prompt_forwarding() {
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
    let now = std::time::Instant::now();
    for _ in 0..TELEGRAM_CHAT_WORK_RATE_LIMIT_MAX {
        assert!(!telegram_chat_work_is_rate_limited_at(&mut state, 42, now));
    }

    let changed = handle_telegram_message(
        &telegram,
        &termal,
        &config,
        &mut state,
        telegram_text_message(42, "run the next task"),
    )
    .expect("rate-limited prompt should be handled");

    assert!(!changed);
    assert!(termal.sent_prompts.borrow().is_empty());
    assert!(termal.events.borrow().is_empty());
    assert_eq!(
        telegram.sent_texts.borrow().as_slice(),
        [TELEGRAM_CHAT_WORK_RATE_LIMIT_TEXT.to_owned()]
    );
}

#[test]
fn telegram_read_and_select_commands_are_rate_limited_before_backend_work() {
    for command in [
        "/status",
        "/projects",
        "/sessions",
        "/project project-1",
        "/session session-1",
    ] {
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
        let now = std::time::Instant::now();
        for _ in 0..TELEGRAM_CHAT_WORK_RATE_LIMIT_MAX {
            assert!(!telegram_chat_work_is_rate_limited_at(&mut state, 42, now));
        }

        let changed = handle_telegram_message(
            &telegram,
            &termal,
            &config,
            &mut state,
            telegram_text_message(42, command),
        )
        .expect("rate-limited command should be handled");

        assert!(!changed, "command {command} should not dirty state");
        assert!(
            termal.events.borrow().is_empty(),
            "command {command} should not reach backend work"
        );
        assert_eq!(
            telegram.sent_texts.borrow().as_slice(),
            [TELEGRAM_CHAT_WORK_RATE_LIMIT_TEXT.to_owned()],
            "command {command} should report rate limiting"
        );
    }
}

#[test]
fn telegram_help_fallback_is_rate_limited() {
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
    let now = std::time::Instant::now();
    for _ in 0..TELEGRAM_CHAT_WORK_RATE_LIMIT_MAX {
        assert!(!telegram_chat_work_is_rate_limited_at(&mut state, 42, now));
    }

    let changed = handle_telegram_message(
        &telegram,
        &termal,
        &config,
        &mut state,
        telegram_text_message(42, "/not-a-command"),
    )
    .expect("rate-limited unknown command should be handled");

    assert!(!changed);
    assert_eq!(
        telegram.sent_texts.borrow().as_slice(),
        [TELEGRAM_CHAT_WORK_RATE_LIMIT_TEXT.to_owned()]
    );
}

#[test]
fn telegram_callback_rate_limit_blocks_dispatch() {
    let mut state = TelegramBotState::default();
    let now = std::time::Instant::now();
    for _ in 0..TELEGRAM_CHAT_WORK_RATE_LIMIT_MAX {
        assert!(!telegram_chat_work_is_rate_limited_at(&mut state, 42, now));
    }

    let telegram = FakeTelegramSender::new(None);
    let termal = FakeTelegramActionClient::succeeded(telegram_project_digest(Some("session-1")));
    let config = telegram_test_config();

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
                message_id: 7,
                chat: TelegramChat {
                    id: 42,
                    _kind: "private".to_owned(),
                },
                text: Some("digest".to_owned()),
            }),
        },
    )
    .expect("rate-limited callback should be handled");

    assert!(!changed);
    assert!(termal.dispatches.borrow().is_empty());
    assert_eq!(
        telegram.answered_callbacks.borrow().as_slice(),
        [(
            "callback-1".to_owned(),
            TELEGRAM_CHAT_WORK_RATE_LIMIT_TEXT.to_owned()
        )]
    );
}
