// A closing read can discover a different focused held claim or a forward
// revision of the same claim. It cannot change the closed turn's scope or
// report. Validate one prospective root off-thread; publish its cache only
// while the same session is still between turns.

// Focus chooses a future opening, not the owner of an already observed turn.
// Continue only an exact, still-held claim and the named root confirmed at
// that turn's start. An omitted, unavailable or changed claim proves nothing.
fn runtime_observation_keeps_opening_scope(
    job: &EngramRuntimeClosingJob,
    held: &EngramHeldClaims,
) -> bool {
    let Some(binding) = job.scope.binding.as_ref() else {
        return false;
    };
    let Some(opening) = job.trace.opening_root.as_ref() else {
        return false;
    };
    !job.trace.source_attribution_uncertain
        && opening.store == job.scope.store
        && &opening.binding == binding
        && matches!((&opening.provenance, &job.scope.root), (
            EngramRootCapture::Recorded { generation, state: EngramSourceRootState::Named, workspace_id },
            EngramNamedRootState::Bound { generation: current, workspace_id: root, .. }
        ) if generation == current && workspace_id == root)
        && held.items.iter().any(|claim| {
            claim.work_id == binding.work_id
                && claim.claim_id == binding.claim_id
                && claim.claim_fence == binding.claim_fence
                && claim.control_binding.as_ref() == Some(binding)
        })
}

fn runtime_observation_forward_revision(
    previous: &EngramControlWorkBinding,
    current: &EngramControlWorkBinding,
) -> bool {
    previous.work_revision < current.work_revision
        && EngramControlWorkBinding {
            work_revision: current.work_revision,
            ..previous.clone()
        } == *current
}

impl AppState {
    fn refresh_runtime_observation_focus(
        &self,
        session: &str,
        job: &EngramRuntimeClosingJob,
        target: &EngramBindingTarget,
        binding: &EngramControlWorkBinding,
        deadline: std::time::Instant,
    ) -> Result<(), ApiError> {
        let store = &job.scope.store;
        let revision_only = job
            .scope
            .binding
            .as_ref()
            .is_some_and(|previous| runtime_observation_forward_revision(previous, binding));
        let (generation, was_settled) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            job.scope.require_current(&inner, session).map_err(|_| {
                ApiError::conflict("the closing scope changed before focus refresh")
            })?;
            let index = inner
                .find_session_index(session)
                .ok_or_else(|| ApiError::conflict("the observing session disappeared"))?;
            let record = &inner.sessions[index];
            if !matches!(
                record.session.status,
                SessionStatus::Idle | SessionStatus::Error
            ) || record.engram.unadmitted_active_turn.is_some()
            {
                return Err(ApiError::conflict("another turn already owns its opening"));
            }
            let settled = !engram_authority_work_unresolved(&inner, store, &binding.work_id);
            if revision_only
                && (!settled || !matches!(&job.scope.root, EngramNamedRootState::Bound { .. }))
            {
                return Err(ApiError::conflict(
                    "revision refresh requires a settled acknowledged named root",
                ));
            }
            (record.active_turn_generation, settled)
        };
        let current = |inner: &StateInner| {
            let Some(index) = inner.find_session_index(session) else {
                return false;
            };
            let record = &inner.sessions[index];
            !self
                .runtime_observation_lifetime
                .stopping
                .load(std::sync::atomic::Ordering::Acquire)
                && record.active_turn_generation == generation
                && target.budget_clock.now() < deadline
                && matches!(
                    record.session.status,
                    SessionStatus::Idle | SessionStatus::Error
                )
                && record.engram.unadmitted_active_turn.is_none()
                && record.engram.source_root_bind_owner.is_none()
                && !record.runtime_stop_in_progress
                && !record.engram.observation_delete_requested
                && !session_resource_reset_in_progress(inner, record)
                && Self::engram_root_runtime_is_current(record, &target.runtime_snapshot)
                && job.scope.require_current(inner, session).is_ok()
                && target
                    .source_root_read_fence
                    .same_store(inner, &target.project_id)
                && target
                    .source_root_read_fence
                    .still_current(inner, store, binding)
                && Self::engram_source_root_target_locked(inner, session)
                    .ok()
                    .flatten()
                    .is_some_and(|now| {
                        now.connection == target.connection
                            && now.settings.same_work_service_settings(&target.settings)
                    })
        };
        if !current(&self.inner.lock().expect("state mutex poisoned")) {
            return Err(ApiError::conflict(
                "focus refresh no longer owns the session",
            ));
        }
        // Read first. Preparing an owner would withdraw a settled work's
        // authority even when this prospective refresh is refused.
        let proof = target
            .adapter
            .request(
                &target.connection,
                &EngramControlRequest::NamedRootRead {
                    run_id: binding.run_id.clone(),
                    claim_id: binding.claim_id.clone(),
                },
                target
                    .rpc_timeout_until(deadline)
                    .map_err(|_| ApiError::conflict("focus refresh spent its closing budget"))?,
            )
            .and_then(parse_engram_result::<EngramNamedRootReadResponse>)
            .map_err(|_| ApiError::conflict("the focused claim's root proof is unavailable"))?;
        proof.validate_authority(store, binding)?;
        if revision_only && !runtime_observation_same_root(&proof.named_root, &job.scope.root) {
            return Err(ApiError::conflict(
                "revision refresh cannot change the acknowledged root",
            ));
        }
        let (selected, settled_history) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if !current(&inner) {
                return Err(ApiError::conflict(
                    "the session changed during focus refresh",
                ));
            }
            if was_settled && engram_authority_work_unresolved(&inner, store, &binding.work_id) {
                return Err(ApiError::conflict(
                    "another owner changed the settled work during focus refresh",
                ));
            }
            let selected = engram_work_source_root_for_claim(
                &inner.engram_work_source_roots,
                store,
                &binding.work_id,
                &binding.claim_id,
            );
            let root_matches = match &proof.named_root {
                EngramNamedRootState::Bound {
                    workspace_id,
                    generation,
                    named_at,
                } => {
                    selected.is_some_and(|root| {
                        root.root == *workspace_id
                            && u64::try_from(*generation).ok() == Some(root.generation)
                            && engram_named_at_matches(&root.named_at, named_at)
                    }) && engram_root_journal(
                        &inner.engram_named_root_journal,
                        store,
                        &binding.claim_id,
                    )
                    .is_some_and(|journal| {
                        !journal.requires_reconciliation() && proof.covers(journal)
                    })
                }
                EngramNamedRootState::None | EngramNamedRootState::UnboundByRelease { .. } => {
                    selected.is_none()
                }
                EngramNamedRootState::Unknown => false,
            };
            if !root_matches {
                return Err(ApiError::conflict(
                    "the focused claim's root does not match the acknowledged local selection",
                ));
            }
            let settled_history =
                (!engram_authority_work_unresolved(&inner, store, &binding.work_id)).then(|| {
                    inner
                        .engram_work_naming_history
                        .iter()
                        .find(|history| {
                            &history.store == store && history.work_id == binding.work_id
                        })
                        .expect("settled work has acknowledged history")
                        .clone()
                });
            (selected.cloned(), settled_history)
        };
        if let Some(history) = &settled_history {
            let learns_nothing = if let Some(root) = &selected {
                self.engram_root_read_learns_nothing(root, binding, &Ok(proof.clone()))
            } else {
                let mut learned = history.clone();
                history
                    .proofs
                    .iter()
                    .any(|previous| engram_same_recovery_run(&previous.binding, binding))
                    && engram_proof_keeps_acknowledged_history(history, binding, &proof).is_ok()
                    && engram_learn_canonical_history(&mut learned, &proof).is_ok()
                    && learned == *history
                    && !matches!(proof.run.state.as_str(), "completed" | "cancelled")
            };
            if !learns_nothing {
                return Err(ApiError::conflict(
                    "the focused work needs authority recovery before a later opening",
                ));
            }
        } else {
            // Only already-unresolved work needs the existing complete-image
            // owner. All proof and session checks precede its preparation.
            if !current(&self.inner.lock().expect("state mutex poisoned")) {
                return Err(ApiError::conflict(
                    "focus refresh changed before authority preparation",
                ));
            }
            let owner = self.prepare_engram_authority_until(store, binding, deadline)?;
            {
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                if !current(&inner) {
                    return Err(ApiError::conflict(
                        "focus refresh changed during authority preparation",
                    ));
                }
                Self::learn_engram_authority_locked(&mut inner, store, &owner, &proof)?;
            }
            // No recovery batch or naming replay. Other unresolved facts
            // keep the existing owner guarded.
            self.publish_engram_authority_image_until(store, &owner, deadline)?;
        }
        // Remote proof/recovery work must not hold the retirement barrier.
        // Only the final cache mutation and its persistence need to drain
        // before restore; a stopped worker cannot enter this publication.
        let _install = self
            .runtime_observation_lifetime
            .enter("focus")
            .ok_or_else(|| ApiError::conflict("focus refresh stopped before publication"))?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if !current(&inner)
            || engram_authority_work_unresolved(&inner, store, &binding.work_id)
            || settled_history.as_ref().is_some_and(|expected| {
                inner
                    .engram_work_naming_history
                    .iter()
                    .find(|history| &history.store == store && history.work_id == binding.work_id)
                    != Some(expected)
            })
        {
            return Err(ApiError::conflict(
                "focus refresh changed before publication",
            ));
        }
        let index = inner
            .find_session_index(session)
            .expect("validated focus refresh session");
        let previous_binding = inner.sessions[index].engram.work_binding.clone();
        let previous_root = inner.sessions[index].engram.named_root.clone();
        inner.sessions[index].engram.work_binding = Some(binding.clone());
        inner.sessions[index].engram.named_root = Some(proof.named_root);
        if engram_root_capture_explained_locked(&inner, session).is_err() {
            inner.sessions[index].engram.work_binding = previous_binding;
            inner.sessions[index].engram.named_root = previous_root;
            return Err(ApiError::conflict(
                "the prospective opening is not confirmed",
            ));
        }
        if let Err(error) = self.persist_session_internal_locked(&inner, session) {
            inner.sessions[index].engram.work_binding = previous_binding;
            inner.sessions[index].engram.named_root = previous_root;
            return Err(ApiError::internal(format!(
                "saving prospective focus: {error}"
            )));
        }
        Ok(())
    }

    fn notice_runtime_observation_focus_refusal(
        &self,
        session: &str,
        job: &EngramRuntimeClosingJob,
        generation: u64,
    ) -> Result<()> {
        let Some(_install) = self.runtime_observation_lifetime.enter("focus_notice") else {
            return Ok(());
        };
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session) else {
            return Ok(());
        };
        let record = &inner.sessions[index];
        // A refusal of this old refresh must not overwrite a newer opening,
        // binding, reset or deletion's notice. It never changes the old report.
        if self
            .runtime_observation_lifetime
            .stopping
            .load(std::sync::atomic::Ordering::Acquire)
            || record.active_turn_generation != generation
            || !matches!(
                record.session.status,
                SessionStatus::Idle | SessionStatus::Error
            )
            || record.engram.unadmitted_active_turn.is_some()
            || record.runtime_stop_in_progress
            || record.engram.observation_delete_requested
            || session_resource_reset_in_progress(&inner, record)
            || job.scope.require_current(&inner, session).is_err()
        {
            return Ok(());
        }
        let notice = "[TermAl] The previous turn's report is retained under its original item. No future source root was installed after the focus change. Name or confirm the focused item's root for a later turn; until then the session's source association still refers to the previous item.".to_owned();
        inner.sessions[index]
            .engram
            .set_pending_source_root_line(notice.clone());
        let message = Message::Text {
            attachments: Vec::new(),
            id: inner.next_message_id(),
            timestamp: stamp_now(),
            author: Author::System,
            text: notice,
            expanded_text: None,
            source: None,
        };
        push_message_on_record(&mut inner.sessions[index], message);
        inner.stamp_session_at_index(index);
        self.commit_session_locked(&mut inner, session).map(|_| ())
    }
}

fn runtime_observation_focus_notice(
    turn: &str,
    named_work: Option<&str>,
    refreshed: bool,
) -> String {
    let turn: String = turn
        .chars()
        .filter(|ch| !ch.is_control())
        .take(80)
        .collect();
    // A local name's short reference is already public. Never copy a raw
    // canonical work/claim id or the retained error body into agent context.
    let work: String = named_work
        .unwrap_or("")
        .chars()
        .filter(|ch| !ch.is_control())
        .take(80)
        .collect();
    let work = if work.trim().is_empty() {
        "another held item"
    } else {
        &work
    };
    let remedy = if refreshed {
        "The session's source association was refreshed for a later opening. Start another ordinary turn; that earlier turn remains uncaptured."
    } else {
        "No future source root was installed. Name that work's root for a later turn, then continue; no capture is promised until its authority is confirmed."
    };
    format!(
        "[TermAl] Runtime-started turn {turn} not reported to Engram (runtime_observation_scope_changed): source scope changed; not captured because focus moved to {work}. {remedy}"
    )
}
