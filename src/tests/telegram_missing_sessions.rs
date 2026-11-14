// Owns persisted forwarding retries after session fetch failures, including the
// shared API decoder's status classification. Transport and Telegram delivery
// behavior remain covered by their existing suites.

use super::telegram_support::FakeTelegramSender;
use super::*;
use std::cell::Cell;

struct FailedSessionReader {
    status: StatusCode,
    payload: Vec<u8>,
    transport_failure: bool,
    requests: Cell<usize>,
}

impl TelegramSessionReader for FailedSessionReader {
    fn get_session(&self, session_id: &str) -> Result<TelegramSessionFetchResponse> {
        self.requests.set(self.requests.get() + 1);
        if self.transport_failure {
            bail!("session not found: connection timed out");
        }
        decode_termal_api_response(
            &format!("/api/sessions/{}", encode_uri_component(session_id)),
            self.status,
            &self.payload,
        )
        .context("session poll failed")
    }
}

#[test]
fn telegram_missing_session_is_disarmed_across_persisted_polls() {
    let root = TestTempRoot::create("telegram-missing-session");
    let telegram = FakeTelegramSender::new(None);
    for (index, (armed, footer_pending, selected)) in [
        (true, false, false),
        (true, true, true),
        (false, true, false),
        (false, false, true),
    ]
    .into_iter()
    .enumerate()
    {
        let path = root.path().join(format!("state-{index}.json"));
        let mut initial = TelegramBotState::default();
        if armed {
            arm_forward_next_assistant_message_session_id(&mut initial, "deleted");
        }
        if footer_pending {
            remember_assistant_forwarding_footer_pending(&mut initial, "deleted", true);
        }
        if selected {
            initial.selected_session_id = Some("deleted".to_owned());
        }
        let unrelated = TelegramAssistantForwardingCursor {
            message_id: Some("keep-this-cursor".to_owned()),
            ..TelegramAssistantForwardingCursor::default()
        };
        initial
            .assistant_forwarding_cursors
            .insert("other".to_owned(), unrelated.clone());
        persist_telegram_bot_state(&path, &initial).expect("persist armed fixture");
        let termal = FailedSessionReader {
            status: StatusCode::NOT_FOUND,
            payload: br#"{"error":"session not found","code":"local_session_missing"}"#.to_vec(),
            transport_failure: false,
            requests: Cell::new(0),
        };

        for poll in 0..2 {
            // Reload between polls to cover a relay restart, not just memory.
            let mut state = load_telegram_bot_state(&path).expect("reload relay state");
            let dirty = sync_telegram_assistant_replies(&telegram, &termal, &mut state, 42);
            assert_eq!(dirty, poll == 0);
            if dirty {
                persist_telegram_bot_state(&path, &state).expect("persist disarm");
            }
        }

        let saved = load_telegram_bot_state(&path).expect("read durable disarm");
        assert!(saved.forward_next_assistant_message_session_ids.is_empty());
        assert!(!saved.assistant_forwarding_cursors.contains_key("deleted"));
        assert_eq!(saved.selected_session_id, None);
        assert_eq!(
            saved.assistant_forwarding_cursors.get("other"),
            Some(&unrelated)
        );
        assert_eq!(
            termal.requests.get(),
            1,
            "missing session must fail only once"
        );
    }
    assert!(telegram.sent_texts.borrow().is_empty());
}

#[test]
fn telegram_transient_session_failures_stay_armed_across_persisted_polls() {
    let root = TestTempRoot::create("telegram-transient-session");
    let telegram = FakeTelegramSender::new(None);
    for (index, (status, payload, transport_failure)) in [
        (
            StatusCode::SERVICE_UNAVAILABLE,
            br#"{"error":"session not found"}"#.as_slice(),
            false,
        ),
        (
            StatusCode::UNAUTHORIZED,
            br#"{"error":"not authorized"}"#.as_slice(),
            false,
        ),
        (
            StatusCode::NOT_FOUND,
            b"<html>proxy route not found</html>".as_slice(),
            false,
        ),
        (
            StatusCode::OK,
            b"malformed session response".as_slice(),
            false,
        ),
        (StatusCode::REQUEST_TIMEOUT, b"".as_slice(), true),
    ]
    .into_iter()
    .enumerate()
    {
        let path = root.path().join(format!("state-{index}.json"));
        let mut initial = TelegramBotState {
            selected_session_id: Some("retry".to_owned()),
            ..TelegramBotState::default()
        };
        arm_forward_next_assistant_message_session_id(&mut initial, "retry");
        remember_assistant_forwarding_footer_pending(&mut initial, "retry", true);
        persist_telegram_bot_state(&path, &initial).expect("persist retry fixture");
        let termal = FailedSessionReader {
            status,
            payload: payload.to_vec(),
            transport_failure,
            requests: Cell::new(0),
        };

        for _ in 0..2 {
            let mut state = load_telegram_bot_state(&path).expect("reload retry state");
            assert!(sync_telegram_assistant_replies(
                &telegram, &termal, &mut state, 42
            ));
            persist_telegram_bot_state(&path, &state).expect("persist poll state");
            assert_eq!(state.forward_next_assistant_message_session_ids, ["retry"]);
            assert_eq!(state.selected_session_id.as_deref(), Some("retry"));
            assert!(state.assistant_forwarding_cursors["retry"].footer_pending);
        }
        assert_eq!(termal.requests.get(), 2, "transient failure must retry");
    }
    assert!(telegram.sent_texts.borrow().is_empty());
}

#[test]
fn telegram_session_not_found_requires_matching_api_path() {
    let error = decode_termal_api_response::<TelegramSessionFetchResponse>(
        "/api/sessions/other",
        StatusCode::NOT_FOUND,
        br#"{"error":"session not found","code":"local_session_missing"}"#,
    )
    .expect_err("API request should fail");
    assert!(!termal_error_is_session_not_found(&error, "requested"));
    assert!(termal_error_is_session_not_found(&error, "other"));

    let response = decode_termal_api_response::<TelegramSessionFetchResponse>(
        "/api/sessions/present",
        StatusCode::OK,
        br#"{"session":{"status":"idle","messages":[]}}"#,
    )
    .expect("valid session still decodes");
    assert_eq!(response.session.status, TelegramSessionStatus::Idle);
}

#[tokio::test]
async fn telegram_local_missing_code_is_not_minted_for_remote_404s() {
    let root = TestTempRoot::create("telegram-remote-not-found");
    let telegram = FakeTelegramSender::new(None);
    for (index, error) in [
        ApiError::local_session_missing(),
        // These use the same status-to-error boundary as decode_remote_json.
        ApiError::from_status(StatusCode::NOT_FOUND, "remote session not found"),
        ApiError::from_status(StatusCode::NOT_FOUND, "<html>proxy route not found</html>"),
    ]
    .into_iter()
    .enumerate()
    {
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(response.into_body(), 4096)
            .await
            .expect("read server error");
        let envelope: Value = serde_json::from_slice(&body).expect("server error is JSON");
        if index == 0 {
            assert_eq!(envelope["code"], "local_session_missing");
        } else {
            assert!(
                envelope.get("code").is_none(),
                "remote 404 must not gain a local cause"
            );
        }

        // Cover both the armed loop and the selected-only automatic target.
        for armed in [true, false] {
            let path = root.path().join(format!("state-{index}-{armed}.json"));
            let mut initial = TelegramBotState {
                selected_session_id: Some("proxy".to_owned()),
                ..TelegramBotState::default()
            };
            if armed {
                arm_forward_next_assistant_message_session_id(&mut initial, "proxy");
                remember_assistant_forwarding_footer_pending(&mut initial, "proxy", true);
            }
            persist_telegram_bot_state(&path, &initial).expect("persist proxy fixture");
            let termal = FailedSessionReader {
                status: StatusCode::NOT_FOUND,
                payload: body.to_vec(),
                transport_failure: false,
                requests: Cell::new(0),
            };
            for _ in 0..2 {
                let mut state = load_telegram_bot_state(&path).expect("reload proxy fixture");
                if sync_telegram_assistant_replies(&telegram, &termal, &mut state, 42) {
                    persist_telegram_bot_state(&path, &state).expect("persist poll changes");
                }
            }
            let saved = load_telegram_bot_state(&path).expect("read durable poll state");
            if index == 0 {
                assert_eq!(termal.requests.get(), 1);
                assert_eq!(saved.selected_session_id, None);
                assert!(saved.forward_next_assistant_message_session_ids.is_empty());
                assert!(!saved.assistant_forwarding_cursors.contains_key("proxy"));
            } else {
                assert_eq!(
                    termal.requests.get(),
                    2,
                    "existing proxy must keep retrying"
                );
                assert_eq!(saved.selected_session_id.as_deref(), Some("proxy"));
                assert_eq!(
                    saved.forward_next_assistant_message_session_ids,
                    initial.forward_next_assistant_message_session_ids
                );
                assert_eq!(
                    saved.assistant_forwarding_cursors,
                    initial.assistant_forwarding_cursors
                );
            }
        }
    }
    assert!(telegram.sent_texts.borrow().is_empty());
}
