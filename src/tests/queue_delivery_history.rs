// Queue rows that are delivery history (the provider accepted the prompt,
// its delivery outcome is unknown, or a legacy row carries no receipt and is
// shown as unknown) never run again. They must not block Codex thread
// actions, while a pending or in-flight hand-off row still does. Dismissing
// one removes only the queue row: its delivery record stays, a receipt-less
// row gains a durable ambiguous one, and nothing is sent again, also after a
// reload.
use super::*;

fn reload(state: &AppState) -> AppState {
    state.shutdown_persist_blocking();
    AppState::new_with_paths(
        state.default_workdir.clone(),
        state.persistence_path.as_ref().clone(),
        state.orchestrator_templates_path.as_ref().clone(),
    )
    .unwrap()
}

fn delivery(disposition: PromptDeliveryDisposition, sequence: u64) -> PromptDelivery {
    PromptDelivery {
        remote_prompt: None,
        federation_operation: None,
        federation_handoff_started: false,
        disposition,
        attempt: 1,
        write_ahead: format!("write-ahead-{sequence}"),
        sequence,
        message_id: None,
        turn_generation: None,
        attempt_key: None,
        reason: Some("Retained delivery evidence".to_owned()),
        entry: None,
        write_ahead_acknowledged: true,
        resume: None,
        runtime_write: PromptRuntimeWrite::Untracked,
    }
}

/// The queue row a test starts from.
#[derive(Clone, Copy, Debug)]
enum Row {
    /// A row carrying a receipt with this disposition.
    Receipt(PromptDeliveryDisposition),
    /// A legacy row whose stored record cannot say whether it was handed off.
    LegacyUnknown,
    /// A row an Engram hand-off promoted, with no receipt.
    EngramHandoff,
}

fn idle_codex_session_with_row(state: &AppState, row: Row) -> (String, String) {
    let session_id = test_session_id(state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "thread-live".to_owned())
        .unwrap();
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&session_id)
        .expect("session exists");
    let prompt_id = inner.next_message_id();
    let record = &mut inner.sessions[index];
    record.session.status = SessionStatus::Idle;
    queue_prompt_on_record(
        record,
        PendingPrompt {
            engram_interrupted: false,
            is_engram_retained: false,
            attachments: Vec::new(),
            id: prompt_id.clone(),
            timestamp: stamp_now(),
            text: "earlier prompt".to_owned(),
            expanded_text: None,
            source: None,
        },
        Vec::new(),
    );
    let queued = record.queued_prompts.back_mut().expect("queued row");
    match row {
        Row::Receipt(disposition) => {
            record
                .prompt_deliveries
                .insert(prompt_id.clone(), delivery(disposition, 1));
        }
        Row::LegacyUnknown => queued.promotion_disposition_known = false,
        Row::EngramHandoff => queued.engram_ordinary_handoff = true,
    }
    sync_prompt_delivery_view(record);
    (session_id, prompt_id)
}

#[test]
fn codex_thread_actions_ignore_history_rows_but_not_runnable_ones() {
    for (row, blocks) in [
        (Row::Receipt(PromptDeliveryDisposition::Accepted), false),
        (Row::Receipt(PromptDeliveryDisposition::Ambiguous), false),
        (Row::LegacyUnknown, false),
        (Row::EngramHandoff, false),
        (Row::Receipt(PromptDeliveryDisposition::Pending), true),
        (Row::Receipt(PromptDeliveryDisposition::HandingOff), true),
    ] {
        let state = test_app_state();
        let (session_id, _) = idle_codex_session_with_row(&state, row);
        match state.resolve_codex_thread_action_context(&session_id) {
            Ok(_) => assert!(!blocks, "{row:?} must still block thread actions"),
            Err(error) => {
                assert!(
                    blocks,
                    "{row:?} must not block thread actions: {}",
                    error.message
                );
                assert!(
                    error
                        .message
                        .contains("wait for queued Codex prompts to finish")
                );
            }
        }
    }
}

#[test]
fn dismissing_a_history_row_keeps_its_delivery_record_and_never_resends_it() {
    for row in [
        Row::Receipt(PromptDeliveryDisposition::Ambiguous),
        Row::Receipt(PromptDeliveryDisposition::Accepted),
        Row::LegacyUnknown,
        Row::EngramHandoff,
    ] {
        let state = test_app_state();
        let (session_id, prompt_id) = idle_codex_session_with_row(&state, row);
        state
            .cancel_queued_prompt(&session_id, &prompt_id)
            .expect("dismiss succeeds");

        for state in [&state, &reload(&state)] {
            let inner = state.inner.lock().expect("state mutex poisoned");
            let record = &inner.sessions[inner.find_session_index(&session_id).expect("session")];
            assert!(
                record.queued_prompts.is_empty(),
                "{row:?}: the dismissed row stays out of the queue"
            );
            assert!(record.session.pending_prompts.is_empty());
            let receipt = record
                .prompt_deliveries
                .get(&prompt_id)
                .unwrap_or_else(|| panic!("{row:?}: delivery record kept"));
            match row {
                Row::Receipt(disposition) => {
                    assert_eq!(
                        receipt.disposition, disposition,
                        "the receipt is not rewritten"
                    );
                    assert_eq!(
                        receipt.reason.as_deref(),
                        Some("Retained delivery evidence")
                    );
                }
                Row::LegacyUnknown | Row::EngramHandoff => {
                    assert_eq!(receipt.disposition, PromptDeliveryDisposition::Ambiguous);
                    assert_eq!(receipt.reason.as_deref(), Some(UNRECEIPTED_HISTORY_REASON));
                }
            }
            assert!(
                record
                    .session
                    .prompt_deliveries
                    .iter()
                    .any(|view| view.prompt_id == prompt_id
                        && view.disposition == receipt.disposition),
                "{row:?}: clients still see the delivery record"
            );
            assert_eq!(
                ordinary_queue_position(record),
                None,
                "{row:?}: nothing is left to dispatch"
            );
        }
    }
}
