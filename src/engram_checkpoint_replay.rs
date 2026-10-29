// Owns eligibility for the existing on-demand certified checkpoint replay.
// Does not release admission guards, create a new wire operation or schedule work.

thread_local! {
    static CHECKPOINT_PREFLIGHT_BUDGET: std::cell::RefCell<Vec<(usize, String, std::time::Instant, chrono::DateTime<chrono::Utc>)>>
        = const { std::cell::RefCell::new(Vec::new()) };
}

struct EngramCheckpointPreflightBudgetScope(bool);
impl Drop for EngramCheckpointPreflightBudgetScope {
    fn drop(&mut self) {
        if self.0 {
            CHECKPOINT_PREFLIGHT_BUDGET.with(|budgets| {
                budgets.borrow_mut().pop();
            });
        }
    }
}

fn checkpoint_retry_configuration_matches(
    inner: &StateInner,
    session_id: &str,
    original: &EngramExistingCheckpoint,
) -> bool {
    inner.find_session_index(session_id).is_some_and(|index| {
        let record = &inner.sessions[index];
        !record.runtime_stop_in_progress
            && !record.engram.project_reset_in_progress
            && engram_project_for_session_locked(inner, session_id)
                .and_then(|project| project.engram.as_ref())
                .is_some_and(|settings| {
                    settings.is_control_enabled()
                        && settings.same_admission_settings(&original.settings)
                })
    })
}

fn certified_checkpoint_replay_target_locked(
    inner: &StateInner,
    session_id: &str,
    owner: Option<&EngramQueuedAdmissionOwner>,
) -> Option<EngramBindingTarget> {
    EngramCheckpointRecoveryView::for_certified_replay(inner, session_id, owner)
}

impl AppState {
    fn checkpoint_preflight_budget(
        &self,
        session_id: &str,
    ) -> Option<(std::time::Instant, chrono::DateTime<chrono::Utc>)> {
        CHECKPOINT_PREFLIGHT_BUDGET.with(|budgets| {
            budgets
                .borrow()
                .last()
                .filter(|(state, session, ..)| {
                    *state == Arc::as_ptr(&self.inner) as usize && session == session_id
                })
                .map(|(_, _, started, wall)| (*started, *wall))
        })
    }

    fn begin_checkpoint_preflight_budget(
        &self,
        session_id: &str,
    ) -> EngramCheckpointPreflightBudgetScope {
        if self.checkpoint_preflight_budget(session_id).is_some() {
            return EngramCheckpointPreflightBudgetScope(false);
        }
        let eligible = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            certified_checkpoint_replay_target_locked(&inner, session_id, None).is_some()
                || inner.find_session_index(session_id)
                    .and_then(|index| inner.sessions[index].engram.checkpoint_operation.as_ref())
                    .is_some_and(|operation| EngramCheckpointResultOwner::capture(&inner, &operation.original.id).is_some())
        };
        if !eligible {
            return EngramCheckpointPreflightBudgetScope(false);
        }
        let clock = self.engram_budget_clock();
        CHECKPOINT_PREFLIGHT_BUDGET.with(|budgets| {
            budgets.borrow_mut().push((
                Arc::as_ptr(&self.inner) as usize,
                session_id.to_owned(),
                clock.now(),
                clock.wall_now(),
            ))
        });
        EngramCheckpointPreflightBudgetScope(true)
    }
    // A caller may enter only old-checkpoint settlement here. It still runs
    // ordinary eligibility checks after this action and before any admission.
    fn settle_certified_checkpoint_before_admission(
        &self,
        session_id: &str,
        owner: Option<&EngramQueuedAdmissionOwner>,
    ) -> Result<(), ApiError> {
        self.account_pending_checkpoint_receipt(session_id)
            .map_err(|error| ApiError::conflict(format!("{error:#}")))?;
        self.recover_original_checkpoint_before_admission(session_id, owner)
            .map_err(|error| ApiError::conflict(format!("{error:#}")))?;
        let target = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            certified_checkpoint_replay_target_locked(&inner, session_id, owner)
        };
        let Some(target) = target else { return Ok(()) };
        let started = self
            .checkpoint_preflight_budget(session_id)
            .map(|budget| budget.0)
            .unwrap_or_else(|| target.budget_clock.now());
        self.retry_certified_engram_checkpoint(&target, owner, Some(started))
            .map_err(|error| {
                ApiError::conflict(format!(
                    "Original checkpoint settlement remains withheld: {error}"
                ))
            })?;
        let inner = self.inner.lock().expect("state mutex poisoned");
        require_no_engram_session_obligation(&inner, session_id)?;
        if owner.is_some_and(|owner| {
            inner
                .find_visible_session_index(session_id)
                .is_none_or(|index| !owner.matches(&inner.sessions[index]))
        }) {
            return Err(ApiError::conflict(
                "Checkpoint settlement trigger ownership changed",
            ));
        }
        let current =
            AppState::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                .map_err(ApiError::conflict)?;
        if current.as_ref().is_none_or(|current| {
            current.connection != target.connection
                || !current.settings.same_admission_settings(&target.settings)
        }) {
            return Err(ApiError::conflict(
                "Checkpoint settlement settings changed before admission",
            ));
        }
        Ok(())
    }
}
