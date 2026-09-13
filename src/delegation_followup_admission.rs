// Follow-up admission and pre-prompt archive compensation.
// New admission transaction module, kept out of delegations.rs.
// Owns reservation, commit-bound re-arm publication, and the decision to
// compensate, plus explicit settlement of an admitted but undelivered failure.
// Turn dispatch forwards tagged queued-start errors here after releasing state;
// this module does not own or replace the generic queue dispatcher.
// codex_delegation_release.rs owns archive execution; runtime startup,
// unarchive RPC execution, and ordinary turn lifecycle remain elsewhere.

struct FollowupAdmissionReservation {
    last_user_prompt_id: Option<String>,
    canceled: bool,
    admitted: Option<(u64, DelegationRecord)>,
    failure: Option<DetachedDelegationChildRuntime>,
}

fn codex_followup_may_restore_record(record: &SessionRecord) -> bool {
    record_has_archived_codex_thread(record)
        || record
            .codex_delegation_release
            .as_ref()
            .is_some_and(|release| release.terminal_release)
}

#[derive(Debug)]
struct QueuedFollowupStartFailure {
    delegation_id: String,
    prompt_id: String,
}

impl std::fmt::Display for QueuedFollowupStartFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "queued follow-up {} could not start", self.prompt_id)
    }
}
impl std::error::Error for QueuedFollowupStartFailure {}

// Tag failures at the locked promotion boundary, not before off-lock Engram
// work. Only this exact persisted first-prompt identity may be settled later.
fn annotate_queued_followup_start_failure(
    inner: &StateInner,
    child: usize,
    error: anyhow::Error,
) -> anyhow::Error {
    let record = &inner.sessions[child];
    let Some(index) = inner.find_delegation_index_by_child_session_id(&record.session.id) else {
        return error;
    };
    let delegation = &inner.delegations[index];
    if delegation.status == DelegationStatus::Running
        && delegation_followup_awaits_first_turn(inner, delegation)
        && record.queued_prompts.front().is_some_and(|queued| {
            Some(&queued.pending_prompt.id) == delegation.queued_followup_prompt_id.as_ref()
        })
    {
        return error.context(QueuedFollowupStartFailure {
            delegation_id: delegation.id.clone(),
            prompt_id: delegation
                .queued_followup_prompt_id
                .clone()
                .expect("checked first prompt"),
        });
    }
    error
}

impl FollowupAdmissionReservation {
    fn new(last_user_prompt_id: Option<String>) -> Self {
        Self {
            last_user_prompt_id,
            canceled: false,
            admitted: None,
            failure: None,
        }
    }
}

impl AppState {
    fn finish_queued_followup_start_error(&self, error: anyhow::Error) -> ApiError {
        if let Some(failure) = error.downcast_ref::<QueuedFollowupStartFailure>() {
            if let Err(cleanup) =
                self.settle_queued_followup_start_failure(failure, &format!("{error:#}"))
            {
                return ApiError::internal(format!(
                    "{error:#}; failed to settle queued follow-up: {cleanup:#}"
                ));
            }
        }
        ApiError::internal(format!("{error:#}"))
    }

    // Wait consumption and the parent's queued wake are one transaction. No
    // runtime work happens here, so restoring these records under the same
    // lock safely leaves the wait available for a later successful refresh.
    fn commit_followup_wait_refresh_locked(
        &self,
        inner: &mut StateInner,
        delegation_id: Option<&str>,
    ) -> Result<(u64, DelegationWaitRefresh)> {
        let waits_before = inner.delegation_waits.clone();
        let parents_before = waits_before
            .iter()
            .filter(|wait| {
                delegation_id
                    .is_none_or(|id| wait.delegation_ids.iter().any(|candidate| candidate == id))
            })
            .filter_map(|wait| inner.find_session_index(&wait.parent_session_id))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            // Queueing a wait wake mutates only these queue projections and
            // the persistence stamp, never the parent's transcript/runtime.
            .map(|index| {
                let record = &inner.sessions[index];
                (
                    index,
                    record.queued_prompts.clone(),
                    record.queued_peer_messages.clone(),
                    record.session.pending_prompts.clone(),
                    record.mutation_stamp,
                )
            })
            .collect::<Vec<_>>();
        let refresh = match delegation_id {
            Some(id) => refresh_delegation_waits_for_delegation_locked(inner, id),
            None => refresh_delegation_waits_locked(inner),
        };
        if !refresh.did_mutate() {
            return Ok((inner.revision, refresh));
        }
        match self.commit_locked(inner) {
            Ok(revision) => Ok((revision, refresh)),
            Err(error) => {
                inner.delegation_waits = waits_before;
                for (index, queue, peers, pending, stamp) in parents_before {
                    let record = &mut inner.sessions[index];
                    record.queued_prompts = queue;
                    record.queued_peer_messages = peers;
                    record.session.pending_prompts = pending;
                    record.mutation_stamp = stamp;
                }
                Err(error)
            }
        }
    }

    fn settle_queued_followup_start_failure(
        &self,
        failure: &QueuedFollowupStartFailure,
        detail: &str,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_delegation_index(&failure.delegation_id) else {
            return Ok(());
        };
        let delegation = &inner.delegations[index];
        if delegation.status != DelegationStatus::Running
            || delegation.queued_followup_prompt_id.as_deref() != Some(&failure.prompt_id)
            || !delegation_followup_awaits_first_turn(&inner, delegation)
        {
            return Ok(());
        }
        if inner
            .delegation_followup_admissions
            .contains_key(&failure.delegation_id)
        {
            self.fail_undelivered_followup_locked(&mut inner, &failure.delegation_id, detail);
            return Ok(()); // The original admission still owns off-lock cleanup.
        }
        let delta =
            mark_delegation_failed_locked(&mut inner, index, detail).map(strip_parent_card_delta);
        let mut detached = detach_terminal_delegation_child_runtime_locked(&mut inner, index);
        retain_followup_archive_compensation_locked(&mut inner, index, &mut detached);
        let committed = self.commit_locked(&mut inner);
        let revision = inner.revision;
        if committed.is_ok() {
            if let Some(delta) = delta {
                self.publish_delegation_lifecycle_delta(revision, delta);
            }
        }
        let (wait_commit, waits) = if committed.is_ok() {
            match self.commit_followup_wait_refresh_locked(&mut inner, Some(&failure.delegation_id))
            {
                Ok((revision, waits)) => (Ok(revision), waits),
                Err(error) => (Err(error), DelegationWaitRefresh::default()),
            }
        } else {
            (Ok(revision), DelegationWaitRefresh::default())
        };
        let revision = inner.revision;
        drop(inner);
        // Full snapshots already carry the current child; only run owned
        // cleanup when persistence is unavailable, never replay stale deltas.
        detached.transcript_deltas.clear();
        self.publish_delegation_refresh_side_effects(
            revision,
            None,
            detached,
            if wait_commit.is_ok() {
                waits
            } else {
                DelegationWaitRefresh::default()
            },
        );
        committed?;
        wait_commit?;
        Ok(())
    }

    /// Called only after the prompt was started or put into the queue. Keep
    /// runtime-spawn errors outside re-arm, and publish its delta with this
    /// exact commit revision before releasing the state lock.
    fn commit_followup_prompt_locked(
        &self,
        inner: &mut StateInner,
        previous: Option<&DelegationRecord>,
        prompt_id: &str,
        full_snapshot: bool,
    ) -> Result<u64> {
        let parent_before = previous
            .and_then(|previous| inner.find_session_index(&previous.parent_session_id))
            .map(|index| (index, inner.sessions[index].clone()));
        let delta = previous.and_then(|previous| {
            inner.find_delegation_index(&previous.id).and_then(|index| {
                if inner.delegations[index] != *previous {
                    return None;
                }
                let child_id = &previous.child_session_id;
                let queued_prompt = inner
                    .delegation_followup_admissions
                    .get(&previous.id)
                    .filter(|before| {
                        before.last_user_prompt_id
                            == delegation_last_user_prompt_id_locked(inner, child_id)
                    })
                    .and_then(|_| inner.find_session_index(child_id))
                    .and_then(|child| {
                        inner.sessions[child]
                            .queued_prompts
                            .iter()
                            .find(|queued| queued.pending_prompt.id == prompt_id)
                    })
                    .map(|queued| queued.pending_prompt.id.clone());
                inner.delegations[index].queued_followup_prompt_id = queued_prompt;
                rearm_terminal_delegation_for_followup_locked(inner, index)
            })
        });
        let committed = if full_snapshot {
            self.commit_locked(inner)
        } else {
            self.commit_persisted_delta_locked(inner)
        };
        let revision = match committed {
            Ok(revision) => revision,
            Err(error) => {
                if let Some(previous) = previous {
                    let uncommitted_queue = inner
                        .delegation_followup_admissions
                        .get(&previous.id)
                        .is_some_and(|reservation| reservation.admitted.is_none())
                        && inner
                            .find_delegation_index(&previous.id)
                            .is_some_and(|index| {
                                delegation_followup_awaits_first_turn(
                                    inner,
                                    &inner.delegations[index],
                                )
                            });
                    if uncommitted_queue {
                        if let Some(child) = inner.find_session_index(&previous.child_session_id) {
                            let child = inner.session_mut_by_index(child).expect("validated child");
                            child
                                .queued_prompts
                                .retain(|queued| queued.pending_prompt.id != prompt_id);
                            sync_pending_prompts(child);
                        }
                        if let Some(index) = inner.find_delegation_index(&previous.id) {
                            inner.delegations[index] = previous.clone();
                            inner.sync_running_read_only_delegation_index(index);
                            inner.mark_delegation_mutated(index);
                        }
                        if let Some((index, parent)) = parent_before {
                            *inner.session_mut_by_index(index).expect("validated parent") = parent;
                        }
                        return Err(error);
                    }
                    self.fail_undelivered_followup_locked(
                        inner,
                        &previous.id,
                        &format!("Follow-up prompt admission could not be persisted: {error:#}"),
                    );
                }
                return Err(error);
            }
        };
        if let Some(previous) = previous {
            if let Some(index) = inner.find_delegation_index(&previous.id) {
                let admitted = inner.delegations[index].clone();
                if let Some(reservation) =
                    inner.delegation_followup_admissions.get_mut(&previous.id)
                {
                    reservation.admitted = Some((revision, admitted));
                }
            }
        }
        if let Some(delta) = delta {
            self.publish_delegation_lifecycle_delta(revision, delta);
        }
        Ok(revision)
    }

    // The caller still owns dispatch and has not delivered it. Settle under
    // this lock before an unrelated commit can persist a phantom active turn.
    // The reservation carries runtime cleanup to its off-lock release path,
    // including when persistence remains unavailable.
    fn fail_undelivered_followup_locked(&self, inner: &mut StateInner, id: &str, detail: &str) {
        if !inner.delegation_followup_admissions.contains_key(id) {
            return;
        }
        let Some(index) = inner.find_delegation_index(id) else {
            return;
        };
        if inner.delegations[index].status != DelegationStatus::Running {
            return;
        }
        mark_delegation_failed_locked(inner, index, detail);
        let mut detached = detach_terminal_delegation_child_runtime_locked(inner, index);
        retain_followup_archive_compensation_locked(inner, index, &mut detached);
        // Release publishes a fresh full snapshot. Do not replay a saved card
        // or transcript delta stamped with a later, unrelated commit revision.
        detached.transcript_deltas.clear();
        inner
            .delegation_followup_admissions
            .get_mut(id)
            .expect("reservation exists")
            .failure = Some(detached);
    }

    /// Removing a rejected reservation makes its retained terminal result
    /// eligible for waits again. Dispatch resumes only after releasing the lock.
    fn release_delegation_followup_reservation(&self, delegation_id: &str) -> Result<()> {
        let (committed, revision, refresh, failure) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(reservation) = inner.delegation_followup_admissions.remove(delegation_id)
            else {
                return Ok(());
            };
            // Do not consume waits while the failed attempt itself still cannot
            // be persisted. A later refresh can retry them after storage recovers.
            if reservation.failure.is_some() {
                if let Err(error) = self.commit_locked(&mut inner) {
                    let revision = inner.revision;
                    drop(inner);
                    if let Some(detached) = reservation.failure {
                        self.publish_delegation_refresh_side_effects(
                            revision,
                            None,
                            detached,
                            DelegationWaitRefresh::default(),
                        );
                    }
                    return Err(error);
                }
            }
            let (committed, refresh) =
                match self.commit_followup_wait_refresh_locked(&mut inner, Some(delegation_id)) {
                    Ok((revision, refresh)) => (Ok(revision), refresh),
                    Err(error) => (Err(error), DelegationWaitRefresh::default()),
                };
            (committed, inner.revision, refresh, reservation.failure)
        };
        if let Some(detached) = failure {
            // Cleanup must not be abandoned just because the second persistence
            // attempt failed too. Archive tickets retain their durability fence.
            self.publish_delegation_refresh_side_effects(
                revision,
                None,
                detached,
                DelegationWaitRefresh::default(),
            );
        }
        committed?;
        self.publish_delegation_wait_consumed_deltas(revision, &refresh.consumed_waits);
        self.dispatch_delegation_wait_resumes(revision, refresh.dispatch_parents);
        Ok(())
    }
}

/// Before prompt admission, the reservation leaves the prior terminal result intact.
/// Must never be dropped while holding the state mutex: release/Drop locks it.
/// Panic unwinding deliberately leaves the reservation fenced until restart;
/// cleanup must not risk a second panic or admit work through poisoned state.
struct DelegationFollowupAdmission {
    state: AppState,
    previous: DelegationRecord,
    restore_candidate: bool,
    released: bool,
}

impl Drop for DelegationFollowupAdmission {
    fn drop(&mut self) {
        if std::thread::panicking() {
            return; // Do not panic again while unwinding through a poisoned state mutex.
        }
        if let Err(error) = self.release() {
            eprintln!("failed to refresh waits after follow-up reservation release: {error:#}");
        }
    }
}

// Removing the first prompt does not create a new transcript boundary. If
// another User prompt remains, it owns the same not-yet-started attempt.
// Automatic mailbox/orchestrator wakes cannot replace an explicit follow-up;
// otherwise settle explicitly rather than deriving a result from old output.
fn reconcile_removed_followup_prompt_locked(
    inner: &mut StateInner,
    child_id: &str,
    removed_id: &str,
) -> Option<DelegationLifecycleDelta> {
    let Some(index) = inner.find_delegation_index_by_child_session_id(child_id) else {
        return None;
    };
    if inner.delegations[index].status != DelegationStatus::Running
        || inner.delegations[index]
            .queued_followup_prompt_id
            .as_deref()
            != Some(removed_id)
        || inner.find_session_index(child_id).is_some_and(|child| {
            inner.sessions[child].session.messages.iter().any(|message| {
                matches!(message, Message::Text { id, author: Author::You, .. } if id == removed_id)
            })
        })
    {
        return None;
    }
    let successor = inner
        .find_session_index(child_id)
        .and_then(|child| {
            inner.sessions[child]
                .queued_prompts
                .iter()
                .find(|queued| queued.source == QueuedPromptSource::User)
        })
        .map(|queued| queued.pending_prompt.id.clone());
    if successor.is_some() {
        if let Some(child) = inner.find_session_index(child_id) {
            prioritize_user_queued_prompts(
                inner.session_mut_by_index(child).expect("validated child"),
            );
        }
        inner.delegations[index].queued_followup_prompt_id = successor;
        inner.mark_delegation_mutated(index);
        None
    } else {
        mark_delegation_canceled_locked(
            inner,
            index,
            Some("Follow-up prompt canceled before it started.".to_owned()),
        )
    }
}

fn delegation_followup_awaits_first_turn(
    inner: &StateInner,
    delegation: &DelegationRecord,
) -> bool {
    let Some(prompt_id) = delegation.queued_followup_prompt_id.as_ref() else {
        return false;
    };
    inner
        .find_session_index(&delegation.child_session_id)
        .is_some_and(|index| {
            let child = &inner.sessions[index];
            matches!(
                child.session.status,
                SessionStatus::Idle | SessionStatus::Error
            ) && child
                .queued_prompts
                .iter()
                .any(|queued| queued.pending_prompt.id == *prompt_id)
                && !child.session.messages.iter().any(|message| {
                    matches!(message,
                Message::Text { id, author: Author::You, .. } if id == prompt_id)
                })
        })
}

fn reserved_followup_is_admissible_locked(
    inner: &StateInner,
    session_id: &str,
    previous: Option<&DelegationRecord>,
) -> bool {
    previous.is_some_and(|previous| {
        previous.child_session_id == session_id
            && inner
                .delegation_followup_admissions
                .get(&previous.id)
                .is_some_and(|reservation| !reservation.canceled)
            && inner
                .find_delegation_index(&previous.id)
                .is_some_and(|index| inner.delegations[index] == *previous)
    })
}

fn delegation_last_user_prompt_id_locked(inner: &StateInner, session_id: &str) -> Option<String> {
    inner.find_session_index(session_id).and_then(|index| {
        inner.sessions[index]
            .session
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::Text {
                    id,
                    author: Author::You,
                    ..
                } => Some(id.clone()),
                _ => None,
            })
    })
}

impl DelegationFollowupAdmission {
    fn admitted_response(&self) -> Option<DelegationStatusResponse> {
        let inner = self.state.inner.lock().expect("state mutex poisoned");
        let (revision, delegation) = inner
            .delegation_followup_admissions
            .get(&self.previous.id)?
            .admitted
            .as_ref()?;
        Some(DelegationStatusResponse {
            revision: *revision,
            delegation: delegation.clone(),
            server_instance_id: self.state.server_instance_id.clone(),
        })
    }

    fn fail_queued_start(&self, detail: &str) {
        let mut inner = self.state.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_delegation_index(&self.previous.id) else {
            return;
        };
        if delegation_followup_awaits_first_turn(&inner, &inner.delegations[index]) {
            self.state
                .fail_undelivered_followup_locked(&mut inner, &self.previous.id, detail);
        }
    }

    fn release(&mut self) -> Result<()> {
        if self.released {
            return Ok(());
        }
        // Drop must not remove a later caller's reservation after this release
        // has already made the terminal delegation available again.
        self.released = true;
        self.state
            .release_delegation_followup_reservation(&self.previous.id)
    }

    fn rollback_before_prompt(&self) -> Result<bool, ApiError> {
        let inner = self.state.inner.lock().expect("state mutex poisoned");
        let Some(before_prompt) = inner.delegation_followup_admissions.get(&self.previous.id)
        else {
            return Ok(false);
        };
        let child_id = &self.previous.child_session_id;
        if before_prompt.last_user_prompt_id
            != delegation_last_user_prompt_id_locked(&inner, child_id)
            || inner
                .find_session_index(child_id)
                .is_some_and(|index| !inner.sessions[index].queued_prompts.is_empty())
        {
            return Ok(false); // Accepted prompt owns the new attempt, even if delivery failed.
        }
        let Some(index) = inner.find_delegation_index(&self.previous.id) else {
            return Ok(false);
        };
        let restored = self.restore_candidate
            && delegation_is_terminal(inner.delegations[index].status)
            && inner.find_session_index(child_id).is_some_and(|index| {
                inner.sessions[index]
                    .codex_delegation_release
                    .as_ref()
                    .is_some_and(|release| release.needs_followup_compensation())
            });
        drop(inner);
        if restored {
            self.state
                .rearchive_undispatched_codex_child(child_id)
                .map_err(|error| {
                    ApiError::internal(format!(
                        "follow-up rejected; restoring the archive failed: {}",
                        error.message
                    ))
                })?;
        }
        Ok(restored)
    }
}
