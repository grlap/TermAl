// Terminal callbacks can dispatch a queued turn, whose reset detaches from the
// same shared map. Carry owned work across an explicit map-guard release.
#[derive(Clone)]
struct SharedCodexTerminalIdentity {
    session_id: String,
    runtime: RuntimeToken,
    generation: u64,
    shared_generation: Option<u64>,
    thread_id: Option<String>,
    attachment: Arc<()>,
}

impl SharedCodexTerminalIdentity {
    fn capture(
        state: &AppState,
        session_id: &str,
        runtime: &RuntimeToken,
        shared: &SharedCodexSessionState,
    ) -> Option<Self> {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner.find_session_index(session_id)?];
        if !record.runtime.matches_runtime_token(runtime)
            || shared
                .active_turn_generation
                .is_some_and(|g| g != record.active_turn_generation)
        {
            return None;
        }
        Some(Self {
            session_id: session_id.to_owned(),
            runtime: runtime.clone(),
            generation: record.active_turn_generation,
            shared_generation: shared.active_turn_generation,
            thread_id: shared.thread_id.clone(),
            attachment: shared.attachment.clone(),
        })
    }

    fn matches_attachment(&self, shared: &SharedCodexSessionState) -> bool {
        Arc::ptr_eq(&self.attachment, &shared.attachment)
            && self.shared_generation == shared.active_turn_generation
            && self.thread_id == shared.thread_id
    }

    fn is_current(&self, state: &AppState) -> bool {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .find_session_index(&self.session_id)
            .is_some_and(|index| {
                let record = &inner.sessions[index];
                record.runtime.matches_runtime_token(&self.runtime)
                    && record.active_turn_generation == self.generation
            })
    }
}

enum SharedCodexTerminalOutcome {
    Completed {
        turn_id: Option<String>,
    },
    Failed {
        error: Value,
        detail: String,
        confirmed_terminal: bool,
        accept_delivery: bool,
        compacted: bool,
    },
}

struct SharedCodexTerminalEffect {
    owner: SharedCodexTerminalIdentity,
    outcome: SharedCodexTerminalOutcome,
}

impl SharedCodexTerminalEffect {
    /// No caller may retain the sessions guard here. Preparation is short and
    /// cannot dispatch; lifecycle effects below can detach and replace entries.
    fn apply(self, state: &AppState, sessions: &SharedCodexSessionMap) -> Result<()> {
        let Self { owner, outcome } = self;
        let (capacity, deferred) = {
            let mut shared = sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            let Some(session) = shared
                .get_mut(&owner.session_id)
                .filter(|session| owner.matches_attachment(session) && owner.is_current(state))
            else {
                return Ok(());
            };
            let capacity = match &outcome {
                SharedCodexTerminalOutcome::Completed { .. } => None,
                SharedCodexTerminalOutcome::Failed {
                    error,
                    confirmed_terminal,
                    compacted,
                    ..
                } => state.prepare_provider_capacity_failure(
                    &owner.session_id,
                    &owner.runtime,
                    Some(owner.generation),
                    *confirmed_terminal,
                    error,
                    *compacted,
                ),
            };
            if capacity.is_none() && matches!(&outcome, SharedCodexTerminalOutcome::Failed { .. }) {
                state.cancel_provider_capacity_retry_if_runtime_and_generation_match(
                    &owner.session_id,
                    &owner.runtime,
                    owner.generation,
                );
            }
            if matches!(
                &outcome,
                SharedCodexTerminalOutcome::Failed {
                    accept_delivery: true,
                    ..
                }
            ) {
                state.settle_prompt_delivery_of_runtime_turn(
                    &owner.session_id,
                    &owner.runtime,
                    Some(owner.generation),
                    PromptDeliveryOutcome::Accepted,
                );
            }
            let detail = match &outcome {
                SharedCodexTerminalOutcome::Completed { .. } => None,
                SharedCodexTerminalOutcome::Failed { detail, .. } => Some(detail.clone()),
            };
            let deferred = defer_codex_reviewer_mcp_finish(
                state,
                sessions,
                &owner.session_id,
                &owner.runtime,
                owner.thread_id.as_deref(),
                owner.generation,
                &mut session.reviewer_mcp_gate,
                detail,
            );
            if !deferred && matches!(&outcome, SharedCodexTerminalOutcome::Completed { .. }) {
                state.cancel_provider_capacity_retry_if_runtime_and_generation_match(
                    &owner.session_id,
                    &owner.runtime,
                    owner.generation,
                );
            }
            (capacity, deferred)
        };
        match outcome {
            SharedCodexTerminalOutcome::Completed { turn_id } => {
                if !deferred {
                    state.finish_turn_ok_if_runtime_and_generation_match(
                        &owner.session_id,
                        &owner.runtime,
                        owner.generation,
                    )?;
                }
                if let Some(turn_id) = turn_id {
                    schedule_shared_codex_completed_turn_cleanup(sessions, owner, &turn_id);
                }
            }
            SharedCodexTerminalOutcome::Failed { detail, .. } => {
                if !deferred {
                    state.fail_turn_if_runtime_matches_and_report(
                        &owner.session_id,
                        &owner.runtime,
                        Some(owner.generation),
                        &detail,
                    )?;
                    if let Some(nonce) = capacity {
                        state.finish_provider_capacity_failure(&owner.session_id, &nonce)?;
                    }
                }
            }
        }
        Ok(())
    }
}
