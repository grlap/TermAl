// Behaviour of a queue pause's recorded reason: what a coordinator sees in
// the session listing and what the sender of a held wake sees in its exact
// read, without reading the paused session itself.

use super::mailboxes::mailbox_test_state;
use super::*;

fn send_request(target_session_id: &str, key: &str) -> SendMailboxMessageRequest {
    SendMailboxMessageRequest {
        target_session_id: target_session_id.to_owned(),
        message: "A wake for the target's queue.".to_owned(),
        idempotency_key: key.to_owned(),
        topic: None,
        state_stamp: None,
        class: Some("routine".to_owned()),
    }
}

/// Starts a mailbox-woken turn on the target and returns its runtime token
/// and the turn's generation.
fn start_mailbox_turn(state: &AppState, target_id: &str) -> (RuntimeToken, u64) {
    let (runtime, input_rx) = test_claude_runtime_handle("queue-pause-reason-runtime");
    let token = RuntimeToken::Claude(runtime.runtime_id.clone());
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(target_id).expect("target exists");
        let target = inner.session_mut_by_index(index).expect("target exists");
        target.session.status = SessionStatus::Idle;
        target.runtime = SessionRuntime::Claude(runtime);
    }
    let dispatch = state
        .dispatch_next_queued_turn(target_id, false)
        .expect("queue drain succeeds")
        .expect("the mailbox wake dispatches");
    deliver_turn_dispatch(state, dispatch).expect("the wake reaches the runtime");
    assert!(matches!(
        recv_within_guard(&input_rx, "queue pause reason: the wake's prompt"),
        Ok(ClaudeRuntimeCommand::Prompt(_))
    ));
    let generation = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner.sessions[inner.find_session_index(target_id).expect("target exists")]
            .active_turn_generation
    };
    (token, generation)
}

fn listed_row(state: &AppState, session_id: &str) -> Value {
    let snapshot = serde_json::to_value(state.snapshot()).expect("snapshot serializes");
    mailbox_peer_session_listing(&snapshot, false)
        .into_iter()
        .find(|row| row["sessionId"] == session_id)
        .expect("the session is listed")
}

#[tokio::test]
async fn a_failed_mailbox_turn_names_its_pause_reason_to_the_listing_and_the_held_wake_sender() {
    let (state, sender_id, target_id) = mailbox_test_state();
    state
        .append_mailbox_message_and_notify(&sender_id, send_request(&target_id, "pause-1"))
        .expect("the first wake queues");
    let (token, generation) = start_mailbox_turn(&state, &target_id);

    // The provider reports a typed usage-limit failure for this turn, then
    // the turn fails.
    state.note_turn_failure_kind(
        &target_id,
        &token,
        generation,
        TurnFailureKind::from_claude_api_status(429),
    );
    state
        .fail_turn_if_runtime_matches(
            &target_id,
            &token,
            "usage limit reached for key fixtureToken000000000000000a1
retry after reset",
        )
        .expect("the failure settles");

    // The coordinator's listing shows the pause and why.
    let row = listed_row(&state, &target_id);
    assert_eq!(row["queuePaused"], true);
    assert_eq!(row["queuePauseReason"]["cause"], "turn_failed");
    assert_eq!(
        row["queuePauseReason"]["failureKind"],
        "provider_usage_limit"
    );
    assert!(row["queuePauseReason"]["pausedAt"].is_string());
    // The failure's one-line summary: its first line, with everything after
    // the key's label redacted.
    assert_eq!(
        row["queuePauseReason"]["summary"],
        "usage limit reached for key [redacted]"
    );

    // A later wake is held, and its sender's exact read names the reason.
    let held = state
        .append_mailbox_message_and_notify(&sender_id, send_request(&target_id, "pause-2"))
        .expect("the second wake is stored");
    assert_eq!(held.notification_disposition, "heldBehindPausedQueue");
    let read = read_mailbox_message(
        AxumPath((sender_id.clone(), held.message_id.clone())),
        State(state.clone()),
    )
    .await
    .expect("the sender reads its message")
    .0;
    let status = read
        .current_status
        .expect("an exact read has a current status");
    let pause = status
        .recipient_queue_pause
        .expect("the held wake's sender sees why the queue is paused");
    assert_eq!(pause.cause, QueuePauseCause::TurnFailed);
    assert_eq!(
        pause.failure_kind,
        Some(TurnFailureKind::ProviderUsageLimit)
    );

    // Resuming lifts the pause and its reason with it.
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&target_id).expect("target exists");
        inner.sessions[index].set_auto_dispatch_blocked(false);
    }
    let row = listed_row(&state, &target_id);
    assert_eq!(row["queuePaused"], false);
    assert!(row["queuePauseReason"].is_null());
}

#[test]
fn a_failed_mailbox_turn_without_a_typed_provider_kind_is_unclassified() {
    let (state, sender_id, target_id) = mailbox_test_state();
    state
        .append_mailbox_message_and_notify(&sender_id, send_request(&target_id, "pause-3"))
        .expect("the wake queues");
    let (token, _) = start_mailbox_turn(&state, &target_id);
    state
        .fail_turn_if_runtime_matches(&target_id, &token, "the turn failed")
        .expect("the failure settles");
    let row = listed_row(&state, &target_id);
    assert_eq!(row["queuePauseReason"]["cause"], "turn_failed");
    assert_eq!(row["queuePauseReason"]["failureKind"], "unclassified");
}

#[test]
fn a_strict_session_row_still_decodes_beside_the_pause_reasons() {
    let (state, sender_id, target_id) = mailbox_test_state();
    state
        .append_mailbox_message_and_notify(&sender_id, send_request(&target_id, "pause-4"))
        .expect("the wake queues");
    let (token, _) = start_mailbox_turn(&state, &target_id);
    state
        .fail_turn_if_runtime_matches(&target_id, &token, "the turn failed")
        .expect("the failure settles");
    let snapshot = serde_json::to_value(state.snapshot()).expect("snapshot serializes");
    assert!(snapshot["queuePauseReasons"][&target_id].is_object());
    // An older peer decodes each row strictly; the reason is not in the row.
    for row in snapshot["sessions"]
        .as_array()
        .expect("sessions are listed")
    {
        serde_json::from_value::<StateSessionSummary>(row.clone())
            .expect("a strict row decodes without the pause reason");
    }
}

#[test]
fn a_pause_summary_keeps_only_leading_plain_prose_and_redacts_the_rest() {
    for (failure, expected) in [
        // Plain prose stays whole.
        ("the server is overloaded", "the server is overloaded"),
        (
            "HTTP 503 Service Unavailable",
            "HTTP 503 Service Unavailable",
        ),
        ("the monkey ate 3 bananas", "the monkey ate 3 bananas"),
        (
            "(rate-limited) try again later.",
            "(rate-limited) try again later.",
        ),
        ("invalid token", "invalid token"),
        // A credential label is kept and ends the summary.
        (
            "usage limit reached for key ...",
            "usage limit reached for key [redacted]",
        ),
        (
            "upstream said Authorization: Bearer tok123xyz denied",
            "upstream said Authorization: [redacted]",
        ),
        (
            "upstream said Authorization: Basic dXNlcjpwYXNzd29yZA== denied",
            "upstream said Authorization: [redacted]",
        ),
        (
            r#"password: "correct horse battery" rejected"#,
            "password: [redacted]",
        ),
        (
            r#"Authorization: Digest username="alice", nonce="nonce9""#,
            "Authorization: [redacted]",
        ),
        ("Cookie: session=one9; sid=two9", "Cookie: [redacted]"),
        (
            "gateway refused Bearer bear9tok",
            "gateway refused Bearer [redacted]",
        ),
        (
            "gateway refused Basic YWxpY2U6cHc5 at the edge",
            "gateway refused Basic [redacted]",
        ),
        (
            "gateway refused Digest response=resp9",
            "gateway refused Digest [redacted]",
        ),
        (
            "gateway refused Token tokv9",
            "gateway refused Token [redacted]",
        ),
        // Plural, camelCase and acronym labels.
        (
            "webhook signatures v1 abc9",
            "webhook signatures [redacted]",
        ),
        ("otps 123456 jwts abc pwds hunter", "otps [redacted]"),
        ("refused myAPIToken short9", "refused myAPIToken [redacted]"),
        ("OAuth error", "OAuth [redacted]"),
        // A label is kept only through itself and any trailing punctuation;
        // anything joined to it in the same word is its value.
        (
            "request rejected Token-sk-abcdefghijklmnop",
            "request rejected Token [redacted]",
        ),
        (
            "login failed password-hunter",
            "login failed password [redacted]",
        ),
        ("x-api-key-value rejected", "x-api-key [redacted]"),
        ("accessToken-abc rejected", "accessToken [redacted]"),
        ("(token) expired", "(token) [redacted]"),
        ("the password: hunter", "the password: [redacted]"),
        ("bad token.", "bad token."),
        // Every way of joining text to a label inside one word: an
        // apostrophe, camelCase or an acronym, a label in the middle, and a
        // label a part starts with.
        ("failed token's-value", "failed token [redacted]"),
        ("failed password'x", "failed password [redacted]"),
        ("failed passwordHunter", "failed password [redacted]"),
        ("failed APITokenabc", "failed APIToken [redacted]"),
        ("failed my-password-hunter", "failed my-password [redacted]"),
        ("tokenizer crashed", "token [redacted]"),
        ("Passwords expired", "Passwords [redacted]"),
        // A case change inside a label does not hide it: segments between
        // ' and - are also read with case ignored.
        (
            "login failed APIkey: BlueAtlas",
            "login failed APIkey: [redacted]",
        ),
        ("failed TOKENhunter", "failed TOKEN [redacted]"),
        ("failed APIKEY value", "failed APIKEY [redacted]"),
        ("failed my-PASSWORD-x", "failed my-PASSWORD [redacted]"),
        ("the Keyboard broke", "the Keyboard broke"),
        ("HTTPServer down", "HTTPServer down"),
        // A short label glued to lowercase letters inside one part is not
        // recognised: the stated residual.
        ("failed pwdhunter", "failed pwdhunter"),
        ("failed KEYabc", "failed KEYabc"),
        // A digit or underscore glued to a label makes the word not plain.
        ("failed token_abc", "failed [redacted]"),
        ("failed pwd123", "failed [redacted]"),
        // Only the longer labels match as a word's start, so ordinary words
        // that begin like a short label stay readable.
        ("keyboard error", "keyboard error"),
        ("authentication failed", "authentication failed"),
        ("basically broken", "basically broken"),
        ("the authority refused", "the authority refused"),
        // The first word that is not plain prose ends the summary.
        (
            "request rejected: x-api-key:abc123SECRETvalue9 is not valid",
            "request rejected: [redacted]",
        ),
        (
            r#"provider error {"api_key":"short9secret","code":401}"#,
            "provider error [redacted]",
        ),
        (
            r#"provider error {"type":"overloaded"}"#,
            "provider error [redacted]",
        ),
        ("token=plainvalue0000 expired", "[redacted]"),
        (
            r#"login failed {"password":"alpha!bravo"}"#,
            "login failed [redacted]",
        ),
        ("password=p@ss#word rejected", "[redacted]"),
        (
            r#"refresh failed {"accessToken":"short1"}"#,
            "refresh failed [redacted]",
        ),
        ("clientSecret=hunter2 was refused", "[redacted]"),
        (
            r#"login failed {"password":"first\"tail9"}"#,
            "login failed [redacted]",
        ),
        (r#"rejected {"auth":["abc9"]}"#, "rejected [redacted]"),
        (
            r#"rejected {"token":{"value":"inner9"}}"#,
            "rejected [redacted]",
        ),
        (r#"{"api_keys":["many9"]}"#, "[redacted]"),
        (
            "fetch https://alice:pw9word@example.test/repo failed",
            "fetch [redacted]",
        ),
        (
            "GET /v1?x=1&access_token=qs9val&y=2 failed",
            "GET [redacted]",
        ),
        ("OPENAI_API_KEY=env9val is invalid", "[redacted]"),
        ("secret=sec9val was refused", "[redacted]"),
        ("GET /token/short9 failed", "GET [redacted]"),
        (
            "fetch https://api.example.test/reset-password/QwErTyUiOp failed",
            "fetch [redacted]",
        ),
        (
            "fetch https://alice:pw/9word@example.test/repo failed",
            "fetch [redacted]",
        ),
        ("GET /blob?sv=2024&sig=abc9%2Fdef failed", "GET [redacted]"),
        (
            r#"login failed {"password":"==::"}"#,
            "login failed [redacted]",
        ),
        ("fetch /ghp_abcdefghijklmnopqrst failed", "fetch [redacted]"),
        ("refused ghp_abcdefghijklmnop now", "refused [redacted]"),
        ("sk-abcdefghijklmnop leaked", "[redacted]"),
        ("myAPIToken=short9 rejected", "[redacted]"),
        (
            r#"webhook rejected {"signatures":["v1,abc9"]}"#,
            "webhook rejected [redacted]",
        ),
        (
            "You've hit your weekly limit \u{b7} resets Nov 28, 9pm (America/Los_Angeles)",
            "You've hit your weekly limit [redacted]",
        ),
        // Only the first non-empty line counts, at any line break.
        (
            "usage limit reached for key fixtureToken000000000000000a1\nretry after reset",
            "usage limit reached for key [redacted]",
        ),
        (
            "\nprovider refused the request\npassword=hunter2",
            "provider refused the request",
        ),
        ("provider refused\rdiagnostic detail", "provider refused"),
        (
            "provider refused\u{2028}diagnostic detail",
            "provider refused",
        ),
        (
            "provider refused\u{2029}diagnostic detail",
            "provider refused",
        ),
        (
            "provider refused\u{85}diagnostic detail",
            "provider refused",
        ),
        // Within a line only spaces and tabs separate words; other
        // whitespace ends the line.
        ("provider refused\u{b}BlueAtlas", "provider refused"),
        ("provider refused\u{c}BlueAtlas", "provider refused"),
        ("provider refused\u{a0}BlueAtlas", "provider refused"),
        ("provider refused\tagain", "provider refused\tagain"),
    ] {
        assert_eq!(
            queue_pause_summary(failure).as_deref(),
            Some(expected),
            "summary of {failure:?}"
        );
    }
    assert_eq!(queue_pause_summary(""), None);
    assert_eq!(queue_pause_summary(" \n\r "), None);
}

#[test]
fn a_plain_word_is_at_most_32_characters_with_its_punctuation() {
    let letters = "a".repeat(32);
    let at_limit = format!("{}!", "a".repeat(31));
    let past_limit = format!("{}!", "a".repeat(32));
    let parenthesised = format!("({})", "a".repeat(30));
    let parenthesised_past = format!("({})", "a".repeat(31));
    for (failure, expected) in [
        (format!("ok {letters} done"), format!("ok {letters} done")),
        (format!("ok {at_limit} done"), format!("ok {at_limit} done")),
        (format!("ok {past_limit} done"), "ok [redacted]".to_owned()),
        (
            format!("ok {parenthesised} done"),
            format!("ok {parenthesised} done"),
        ),
        (
            format!("ok {parenthesised_past} done"),
            "ok [redacted]".to_owned(),
        ),
    ] {
        assert_eq!(
            queue_pause_summary(&failure).as_deref(),
            Some(expected.as_str())
        );
    }
}

#[tokio::test]
async fn a_domain_message_sender_on_the_recipients_host_sees_its_pause_reason() {
    // Both sessions are federation-bound on the same host, so the sender's
    // message and exact read go through the federation domain store.
    let state = super::federation_mailboxes::central_store();
    let (sender_id, _) = super::federation_mailboxes::local_participant(&state, "Domain sender");
    let (target_id, target) =
        super::federation_mailboxes::local_participant(&state, "Domain recipient");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&target_id).expect("target exists");
        let record = &mut inner.sessions[index];
        record.session.status = SessionStatus::Idle;
        record.pause_auto_dispatch(QueuePauseReason::turn(
            QueuePauseCause::TurnFailed,
            Some(TurnFailureKind::ProviderUsageLimit),
            queue_pause_summary("weekly limit reached"),
        ));
    }
    let sent = state
        .execute_federation_mailbox(
            super::federation_mailboxes::request(
                &state,
                &sender_id,
                FederationMailboxOperation::Send {
                    target: target.binding.participant_key(),
                    message: "A wake for the paused recipient.".into(),
                    key: "domain-pause-1".into(),
                    topic: None,
                    state_stamp: None,
                },
            ),
            None,
        )
        .expect("the domain message is stored")
        .value;
    let message_id = sent["messageId"]
        .as_str()
        .expect("the receipt names the message")
        .to_owned();

    let read = read_mailbox_message(
        AxumPath((sender_id.clone(), message_id)),
        State(state.clone()),
    )
    .await
    .expect("the sender reads its domain message")
    .0;
    assert_eq!(read.target_session_id, target.binding.participant_key());
    let pause = read
        .current_status
        .expect("an exact read has a current status")
        .recipient_queue_pause
        .expect("the sender on the recipient's host sees why its queue is paused");
    assert_eq!(pause.cause, QueuePauseCause::TurnFailed);
    assert_eq!(
        pause.failure_kind,
        Some(TurnFailureKind::ProviderUsageLimit)
    );
    assert_eq!(pause.summary.as_deref(), Some("weekly limit reached"));
}

#[test]
fn a_typed_failure_reported_during_a_failed_stop_keeps_its_kind_when_replayed() {
    let (state, sender_id, target_id) = mailbox_test_state();
    state
        .append_mailbox_message_and_notify(&sender_id, send_request(&target_id, "pause-stop"))
        .expect("the wake queues");
    let (token, generation) = start_mailbox_turn(&state, &target_id);
    {
        // A Stop is in progress when the provider reports the failure.
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&target_id).expect("target exists");
        inner.sessions[index].claim_runtime_stop(RuntimeStopOwnerKind::UserStop, token.clone());
    }
    state.note_turn_failure_kind(
        &target_id,
        &token,
        generation,
        TurnFailureKind::from_claude_api_status(401),
    );
    state
        .mark_turn_error_if_runtime_and_generation_match(
            &target_id,
            &token,
            generation,
            "authentication failed",
        )
        .expect("the failure is deferred behind the Stop");
    assert_eq!(listed_row(&state, &target_id)["queuePaused"], false);

    // The Stop fails: its fence is released and the deferred failure replays.
    let deferred = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&target_id).expect("target exists");
        let record = &mut inner.sessions[index];
        record.clear_runtime_stop();
        std::mem::take(&mut record.deferred_stop_callbacks)
    };
    assert_eq!(deferred.len(), 1);
    state.replay_deferred_runtime_stop_callbacks(&target_id, &token, deferred);

    let row = listed_row(&state, &target_id);
    assert_eq!(row["queuePaused"], true);
    assert_eq!(row["queuePauseReason"]["cause"], "turn_failed");
    assert_eq!(row["queuePauseReason"]["failureKind"], "provider_auth");
}

#[test]
fn a_pause_reason_is_persisted_with_the_paused_state_and_only_with_it() {
    let (state, sender_id, target_id) = mailbox_test_state();
    state
        .append_mailbox_message_and_notify(&sender_id, send_request(&target_id, "pause-persist"))
        .expect("the wake queues");
    let (token, _) = start_mailbox_turn(&state, &target_id);
    state
        .fail_turn_if_runtime_matches(&target_id, &token, "the turn failed")
        .expect("the failure settles");
    let record = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner.sessions[inner.find_session_index(&target_id).expect("target exists")].clone()
    };
    let recorded = record
        .queue_pause_reason
        .clone()
        .expect("the failed turn's pause records its reason");

    // Saved and loaded while paused, the reason comes back with the pause.
    let saved = serde_json::to_value(PersistedSessionRecord::from_record(&record))
        .expect("the record saves");
    let loaded = serde_json::from_value::<PersistedSessionRecord>(saved)
        .expect("the record loads")
        .into_record()
        .expect("the record restores");
    assert!(loaded.orchestrator_auto_dispatch_blocked);
    assert_eq!(loaded.queue_pause_reason, Some(recorded));

    // A record that is not paused saves no reason.
    let mut unpaused = record;
    unpaused.set_auto_dispatch_blocked(false);
    let saved = serde_json::to_value(PersistedSessionRecord::from_record(&unpaused))
        .expect("the record saves");
    assert!(saved.get("queuePauseReason").is_none());
}

#[test]
fn a_pause_summary_is_bounded_to_120_characters_including_its_marker() {
    // The last word read is cut by the read limit, so the rest is redacted,
    // and the kept words give way so that the marker is shown whole.
    let words = "word ".repeat(100);
    let expected = format!("{}w... [redacted]", "word ".repeat(21));
    let summary = queue_pause_summary(&words).expect("a failure has a summary");
    assert_eq!(summary, expected);
    assert_eq!(summary.chars().count(), 120);
}

#[test]
fn a_pause_summary_never_shows_part_of_a_word_it_did_not_keep() {
    // A credential, a labelled value, or a word the read limit cuts, near the
    // 120-character bound: none of it is shown.
    let filler = "word ".repeat(22);
    let redacted = format!("{}[redacted]", "word ".repeat(22));
    for failure in [
        format!("{filler}sk-proj0123456789abcdef"),
        format!("{filler}password=hunter2hunter2"),
    ] {
        assert_eq!(
            queue_pause_summary(&failure).as_deref(),
            Some(redacted.as_str())
        );
    }
    // The last word reaches past what is read, so its visible letters are
    // not judged and not shown.
    let long_tail = format!("{}abcdefghijklmnopqrstuvwxyz0123", "word ".repeat(21));
    assert_eq!(
        queue_pause_summary(&long_tail).as_deref(),
        Some(format!("{}[redacted]", "word ".repeat(21)).as_str())
    );
}

#[test]
fn a_pause_summary_reads_only_the_start_of_a_huge_failure() {
    // A failure far longer than the bound, with no line break and repeated
    // URL-like text, is summarised from its first characters only.
    let huge = "x://".repeat(2_000_000);
    assert_eq!(queue_pause_summary(&huge).as_deref(), Some("[redacted]"));
    let blank_lines = format!("{}ok", " \n".repeat(1_000));
    assert_eq!(queue_pause_summary(&blank_lines).as_deref(), Some("ok"));
}

#[test]
fn a_mailbox_turn_failing_under_provider_capacity_keeps_the_capacity_cause() {
    let (state, sender_id, target_id) = mailbox_test_state();
    state
        .append_mailbox_message_and_notify(&sender_id, send_request(&target_id, "pause-cap"))
        .expect("the wake queues");
    let (token, generation) = start_mailbox_turn(&state, &target_id);
    {
        // The provider reported no capacity: its automatic retry owns the
        // pause before the turn's failure settles.
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&target_id).expect("target exists");
        let record = &mut inner.sessions[index];
        record.provider_capacity_retry = Some(ProviderCapacityRetry {
            nonce: "capacity-nonce".to_owned(),
            server_instance: "test-instance".to_owned(),
            runtime_id: "queue-pause-reason-runtime".to_owned(),
            generation,
            stop_generation: 0,
            pause_epoch: 0,
            settings: String::new(),
            attempts: 0,
            due_at: None,
            recovering: false,
            dispatched: false,
            original_preview: String::new(),
            saved: false,
            fence: None,
        });
        record.pause_auto_dispatch(QueuePauseReason::now(QueuePauseCause::ProviderCapacity));
    }
    state.note_turn_failure_kind(
        &target_id,
        &token,
        generation,
        TurnFailureKind::from_codex_error_info("serverOverloaded"),
    );
    state
        .fail_turn_if_runtime_matches(&target_id, &token, "the server is overloaded")
        .expect("the failure settles");
    let row = listed_row(&state, &target_id);
    assert_eq!(row["queuePauseReason"]["cause"], "provider_capacity");
    assert_eq!(
        row["queuePauseReason"]["failureKind"],
        "provider_overloaded"
    );
    assert_eq!(
        row["queuePauseReason"]["summary"],
        "the server is overloaded"
    );
}
