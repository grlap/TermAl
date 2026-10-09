// Stop on an Idle session that is waiting for automatic work.
//
// Owns: the public Stop path for an Idle session that holds a pending
// delegation wait or test-run wait. Such a session ended its turn and will be
// resumed by the awaited work; Stop is the operator's brake on that resume. It
// consumes the session's waits, sets the explicit-resume latch, drops queued
// workflow continuations and records one stopped message, all in one durable
// commit that is rolled back whole if persistence fails. It also consumes the
// waits left behind when a Stop is answered by the Engram admission or retry
// shortcut, so one Stop brakes every resume.
//
// Does not own: Stop of a running session and its runtime interrupt
// (`session_lifecycle.rs`), the Engram admission and retry Stop itself
// (`engram_queued_admission.rs`, `engram_abort_retry.rs`), wait settling
// (`delegations.rs`, `test_run_waits.rs`), or the explicit Resume
// (`session_lifecycle.rs`). An Idle session with no pending wait is left to
// the caller, which still refuses it as not running.
//
// New fragment; not split from an existing file.

/// Which Stop is consuming an Idle session's pending waits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum IdleWaitStop {
    /// The whole Stop: nothing else answered it.
    Brake,
    /// The Engram admission or retry Stop already answered it, held the queue
    /// head and wrote its own preview; only the waits remain.
    AfterAdmissionStop,
}

impl AppState {
    /// Stops an Idle session that waits for delegations or test runs.
    /// Returns `Ok(None)` when the session is not Idle or holds no pending
    /// wait, so the caller keeps its ordinary Stop path.
    fn stop_idle_waiting_session(
        &self,
        session_id: &str,
    ) -> std::result::Result<Option<StateResponse>, ApiError> {
        self.stop_pending_waits(session_id, IdleWaitStop::Brake)
    }

    /// Consumes the pending waits of a session whose Stop the Engram
    /// admission or retry shortcut answered. Does nothing without a wait.
    fn stop_pending_waits_after_admission_stop(
        &self,
        session_id: &str,
    ) -> std::result::Result<(), ApiError> {
        self.stop_pending_waits(session_id, IdleWaitStop::AfterAdmissionStop)
            .map(|_| ())
    }

    fn stop_pending_waits(
        &self,
        session_id: &str,
        mode: IdleWaitStop,
    ) -> std::result::Result<Option<StateResponse>, ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_visible_session_index(session_id) else {
            return Ok(None);
        };
        let status = inner.sessions[index].session.status;
        let status_allows = match mode {
            IdleWaitStop::Brake => status == SessionStatus::Idle,
            // The admission and retry shortcuts answer Idle and Error.
            IdleWaitStop::AfterAdmissionStop => {
                matches!(status, SessionStatus::Idle | SessionStatus::Error)
            }
        };
        if !status_allows || inner.sessions[index].runtime_stop_in_progress {
            return Ok(None);
        }
        let has_pending_wait = inner
            .delegation_waits
            .iter()
            .any(|wait| wait.parent_session_id == session_id)
            || inner
                .test_run_waits
                .iter()
                .any(|wait| wait.session_id == session_id);
        if !has_pending_wait {
            return Ok(None);
        }

        // Consuming a wait is valid only as part of the durable Stop commit:
        // keep everything this Stop changes for the rollback below.
        let original_record = inner.sessions[index].clone();
        let delegation_waits_before_stop = inner.delegation_waits.clone();
        let test_run_waits_before_stop = inner.test_run_waits.clone();
        let stopped_delegation_waits =
            consume_delegation_waits_for_stopped_parent_locked(&mut inner, session_id);
        let stopped_test_run_waits =
            consume_test_run_waits_for_stopped_session_locked(&mut inner, session_id);
        let message_id = (mode == IdleWaitStop::Brake).then(|| inner.next_message_id());
        let created_messages = {
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            record.set_auto_dispatch_blocked(true);
            // The operator's pause, marked where the latch is set.
            record.engram.operator_paused = true;
            // A wait that settled just before Stop may already have queued
            // its continuation; user and mailbox prompts stay, paused.
            drop_queued_workflow_continuations_keeping_held_head(record);
            match message_id {
                Some(message_id) => {
                    record.session.preview = make_preview(SESSION_STOPPED_BY_USER_MESSAGE);
                    let message_index = push_message_on_record(
                        record,
                        Message::Text {
                            attachments: Vec::new(),
                            id: message_id,
                            timestamp: stamp_now(),
                            author: Author::Assistant,
                            text: SESSION_STOPPED_BY_USER_MESSAGE.to_owned(),
                            expanded_text: None,
                            source: None,
                        },
                    );
                    message_created_delta_parts_for_indices(record, vec![message_index])
                }
                None => Vec::new(),
            }
        };

        match self.commit_locked(&mut inner) {
            Ok(revision) => {
                self.publish_test_run_waits_consumed(
                    &inner,
                    revision,
                    &stopped_test_run_waits,
                    TestRunWaitConsumedReason::SessionStopped,
                );
                self.publish_message_created_delta_parts(&inner, revision, created_messages);
                self.publish_delegation_wait_consumed_deltas(
                    &inner,
                    revision,
                    &stopped_delegation_waits.consumed_waits,
                );
                Ok(Some(self.snapshot_from_inner(&inner)))
            }
            Err(error) => {
                inner.sessions[index] = original_record;
                inner.delegation_waits = delegation_waits_before_stop;
                inner.test_run_waits = test_run_waits_before_stop;
                Err(ApiError::internal(format!(
                    "failed to persist the stopped waits: {error:#}"
                )))
            }
        }
    }
}

/// Drops queued workflow continuations, except a queue head that Engram holds
/// (interrupted, waiting, or carrying intent): that head waits for the
/// operator's explicit cancel or Resume, as the admission Stop left it.
fn drop_queued_workflow_continuations_keeping_held_head(record: &mut SessionRecord) {
    let keep = |index: usize, queued: &QueuedPromptRecord| {
        queued.source != QueuedPromptSource::Orchestrator
            || (index == 0 && queued.is_engram_retained())
    };
    let original_len = record.queued_prompts.len();
    let dropped_begin = dropped_engram_intent_grant(
        record,
        record
            .queued_prompts
            .iter()
            .enumerate()
            .filter(|(index, queued)| !keep(*index, queued))
            .map(|(_, queued)| queued),
    );
    let mut index = 0usize;
    record.queued_prompts.retain(|queued| {
        let retain = keep(index, queued);
        index = index.saturating_add(1);
        retain
    });
    if record.queued_prompts.len() != original_len {
        record_dropped_engram_intent(record, dropped_begin);
        sync_pending_prompts(record);
    }
}
