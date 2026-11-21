// Stop on an Idle session that is waiting for automatic work.
//
// Owns: the public Stop path for an Idle session that holds a pending
// delegation wait, queued delegation result, or test-run wait. Stop holds
// the automatic resume even after the last delegation wait has settled. It
// preserves delegation waits, consumes test-run waits, sets the explicit-resume
// latch and records one stopped message in the durable Stop commit.
//
// Does not own: Stop of a running session and its runtime interrupt
// (`session_lifecycle.rs`), wait settling
// (`delegations.rs`, `test_run_waits.rs`), or the explicit Resume
// (`session_lifecycle.rs`). An Idle session with no such work is left to
// the caller, which still refuses it as not running.
//
// New fragment; not split from an existing file.

fn has_runnable_delegation_result(record: &SessionRecord) -> bool {
    record.queued_prompts.iter().any(|entry| {
        entry.is_delegation_fan_in(&record.session.id)
            && ordinary_queue_entry_is_runnable(record, entry)
    })
}

impl AppState {
    /// Stops an Idle session that waits for delegations or test runs.
    /// Returns `Ok(None)` when the session is not Idle or holds no pending
    /// wait, so the caller keeps its ordinary Stop path.
    fn stop_idle_waiting_session(
        &self,
        session_id: &str,
    ) -> std::result::Result<Option<StateResponse>, ApiError> {
        self.stop_pending_waits(session_id)
    }

    fn stop_pending_waits(
        &self,
        session_id: &str,
    ) -> std::result::Result<Option<StateResponse>, ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_visible_session_index(session_id) else {
            return Ok(None);
        };
        let status = inner.sessions[index].session.status;
        if status != SessionStatus::Idle || inner.sessions[index].runtime_stop_in_progress {
            return Ok(None);
        }
        let record = &inner.sessions[index];
        let has_delegation_work = inner
            .delegation_waits
            .iter()
            .any(|wait| wait.parent_session_id == session_id)
            || has_runnable_delegation_result(record);
        let has_test_run_wait = inner
            .test_run_waits
            .iter()
            .any(|wait| wait.session_id == session_id);
        let already_paused = record.orchestrator_auto_dispatch_blocked
            && record.engram.operator_paused
            && !has_test_run_wait
            && record.provider_capacity_retry.is_none()
            && !record.session.provider_capacity_waiting;
        // Stop removes live test-run waits before it gets the writer ACK.
        // Let a repeat confirm that same pause while its retirement is pending.
        if !has_delegation_work
            && !has_test_run_wait
            && !(already_paused
                && inner.stopped_test_run_waits.iter().any(|retirement| {
                    retirement.wait.session_id == session_id
                        && retirement.pending()
                        && PersistSessionWriteStamp::capture(record).covers(&retirement.parent)
                }))
        {
            return Ok(None);
        }

        // Consuming a wait is valid only as part of the durable Stop commit:
        // keep everything this Stop changes for the rollback below.
        let (dispatch, stopped_waits, created_messages) = if already_paused {
            // A repeated Stop may have found an earlier unacknowledged pause.
            (PersistDispatch::BackgroundQueued, Vec::new(), Vec::new())
        } else {
            let original_record = inner.sessions[index].clone();
            let test_run_waits_before_stop = inner.test_run_waits.clone();
            let earlier_retirements = inner.stopped_test_run_waits.len();
            let stopped_waits =
                consume_test_run_waits_for_stopped_session_locked(&mut inner, session_id);
            let message_id = inner.next_message_id();
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            clear_provider_capacity_retry(record);
            // Clearing backoff restores its original Error preview. This
            // explicit Stop owns the subsequent Idle state.
            record.session.status = SessionStatus::Idle;
            record.set_auto_dispatch_blocked(true);
            // The operator's pause, marked where the latch is set.
            record.engram.operator_paused = true;
            // A wait that settled just before Stop may already have queued
            // its continuation; user and mailbox prompts stay, paused.
            drop_queued_workflow_continuations_keeping_held_head(record);
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
            let created_messages =
                message_created_delta_parts_for_indices(record, vec![message_index]);
            let parent = PersistSessionWriteStamp::capture(record);
            inner
                .stopped_test_run_waits
                .extend(
                    stopped_waits
                        .iter()
                        .cloned()
                        .map(|wait| PersistWaitRetirement {
                            wait,
                            parent: parent.clone(),
                            committed: Arc::default(),
                        }),
                );

            let dispatch = self
                .commit_persisted_delta_locked_with_outcome(&mut inner)
                .and_then(|(_, outcome)| {
                    outcome.require_session_saved(session_id)?;
                    Ok(outcome.dispatch)
                });
            match dispatch {
                Ok(dispatch) => (dispatch, stopped_waits, created_messages),
                Err(error) => {
                    inner.sessions[index] = original_record;
                    inner.test_run_waits = test_run_waits_before_stop.into();
                    inner.stopped_test_run_waits.truncate(earlier_retirements);
                    return Err(ApiError::internal(format!(
                        "failed to persist the stopped waits: {error:#}"
                    )));
                }
            }
        };

        if dispatch == PersistDispatch::BackgroundQueued {
            let record = &inner.sessions[index];
            let pause_epoch = record.auto_dispatch_pause_epoch;
            let minimum = PersistSessionWriteStamp::capture(record);
            let clock = inner.engram_budget_clock_snapshot();
            let (fence, waiter) = PersistFence::new_with_clock(
                PersistFenceTarget::SessionRowAtOrAfter {
                    session_id: session_id.to_owned(),
                    minimum: minimum.clone(),
                },
                clock.now() + Duration::from_secs(20),
                clock,
            );
            let queued = self
                .persist_tx
                .send(PersistRequest::Fence(Box::new(fence)))
                .is_ok();
            if queued {
                // The writer needs StateInner to take its cut. An uncertain
                // write leaves the live pause held so another Stop can retry
                // confirmation without recording a second stopped message.
                drop(inner);
                waiter.wait().map_err(|error| {
                    ApiError::internal(format!("failed to persist the stopped waits: {error:?}"))
                })?;
                inner = self.inner.lock().expect("state mutex poisoned");
            }
            let still_paused = inner
                .find_visible_session_index(session_id)
                .map(|index| &inner.sessions[index])
                .is_some_and(|record| {
                    record.orchestrator_auto_dispatch_blocked
                        && record.engram.operator_paused
                        && record.auto_dispatch_pause_epoch == pause_epoch
                        && PersistSessionWriteStamp::capture(record).covers(&minimum)
                });
            if !still_paused {
                return Err(ApiError::conflict(
                    "session pause changed before Stop was confirmed",
                ));
            }
            if !queued {
                // The worker disconnected during dispatch, or this was an
                // idempotent Stop with no worker. Confirm the current own row.
                self.persist_session_internal_locked(&inner, session_id)
                    .map_err(|error| {
                        ApiError::internal(format!(
                            "failed to persist the stopped waits: {error:#}"
                        ))
                    })?;
            }
        }

        let revision = inner.revision;
        self.publish_state_locked(&inner);
        if !already_paused {
            self.publish_test_run_waits_consumed(
                &inner,
                revision,
                &stopped_waits,
                TestRunWaitConsumedReason::SessionStopped,
            );
            self.publish_message_created_delta_parts(&inner, revision, created_messages);
        }
        Ok(Some(self.snapshot_from_inner(&inner)))
    }
}

/// Drops unsent workflow continuations while preserving historical records
/// and possible provider deliveries. Stop is not evidence they were unsent.
fn drop_queued_workflow_continuations_keeping_held_head(record: &mut SessionRecord) {
    let parent_session_id = record.session.id.clone();
    let keep = |queued: &QueuedPromptRecord| {
        queued.source != QueuedPromptSource::Orchestrator
            || queued.has_stored_history()
            || queued.is_delegation_fan_in(&parent_session_id)
    };
    let original_len = record.queued_prompts.len();
    record.queued_prompts.retain(keep);
    if record.queued_prompts.len() != original_len {
        sync_pending_prompts(record);
    }
}
