// The exact replay of a begin whose outcome is unknown. A queue head whose
// retained evaluate carries a Prepared begin for the session's uncertain grant
// is admitted by resending that stored begin (its key string, grant and
// delivery tokens), never by an evaluate, until Engram answers definitely.
// Owns the begin-replay predicate, the replay's retry code and preview, the
// synchronous Begin call bound, and the producer answers that settle or hold a
// replay. Does not own the prepared-begin acknowledgement
// (`engram_queued_admission.rs`), the schedule, acknowledgement, cap and tick
// (`engram_retry_schedule.rs`, `engram_admission_retry.rs`) or the begin send
// itself (the Grant branch of `engram_host_adapter.rs`). New module, kept apart
// so that adapter does not grow.

/// The admission-retry code of a scheduled begin replay.
const ENGRAM_BEGIN_REPLAY_CODE: &str = "begin_unknown";

/// The least a synchronous Begin waits, below the shared call cap.
const ENGRAM_BEGIN_MIN_CALL_TIMEOUT_MS: u64 = 10_000;

/// The prepared begin a begin-unknown head replays: the retained evaluate's
/// Prepared begin names the session's uncertain grant and no grant is active.
fn engram_begin_replay_pending(record: &SessionRecord) -> Option<&EngramPreparedBegin> {
    let uncertain = record.engram.uncertain_grant_id.as_deref()?;
    if record.engram.active_grant_id.is_some() {
        return None;
    }
    record
        .queued_prompts
        .front()?
        .engram_evaluate
        .as_ref()?
        .prepared_begin
        .as_ref()
        .filter(|prepared| {
            prepared.grant_id == uncertain && prepared.phase == EngramPreparedBeginPhase::Prepared
        })
}

/// Whether a dispatch card's cause leaves its begin's outcome unknown: the
/// begin itself met a transport failure, deadline or structured error reply,
/// so Engram may have applied it. A producer refusal, a protocol mismatch or a
/// local hold that never sent the begin is not unknown, and keeps its own
/// distinguishable hold. Neither is a structured error saying the routing
/// token no longer names a bound session: the same begin under that token can
/// never be answered, and a rebind to get another could expire the grant, so
/// it ends the replay in an explicit hold naming that error.
fn engram_cause_leaves_begin_unknown(cause: &EngramCausalFailure) -> bool {
    cause.operation == "turn_begin"
        && cause.remote_application == EngramRemoteApplication::Unknown
        && matches!(
            cause.failure_class,
            EngramCausalFailureClass::Transport
                | EngramCausalFailureClass::Deadline
                | EngramCausalFailureClass::Remote
        )
        && !cause
            .original_code
            .as_deref()
            .is_some_and(engram_code_invalidates_routing_token)
}

/// Whether a dispatch card's cause says its begin met a structured error that
/// invalidates the routing token: an explicit hold, never a replay or a retry,
/// whatever card code (a circuit's included) the failure carries.
fn engram_cause_lost_begin_binding(cause: &EngramCausalFailure) -> bool {
    cause.operation == "turn_begin"
        && cause.failure_class == EngramCausalFailureClass::Remote
        && cause
            .original_code
            .as_deref()
            .is_some_and(engram_code_invalidates_routing_token)
}

/// The refusals that prove a grant was never begun (Engram's contract). Any
/// other refusal of a replay, `grant_scope_mismatch` included, proves nothing.
fn engram_begin_refusal_proves_never_begun(code: &str) -> bool {
    matches!(
        code,
        "stale_fence"
            | "task_unbound"
            | "task_access_denied"
            | "grant_expired"
            | "policy_epoch_changed"
            | "task_admission_epoch_changed"
    )
}

fn engram_begin_replay_preview(retry: &EngramAdmissionRetry) -> String {
    format!(
        "{ENGRAM_ADMISSION_RETRY_PREVIEW_PREFIX} (begin reconciliation: replaying the same begin), \
         attempt {}, next at {}.",
        retry.attempts, retry.due_at
    )
}

impl EngramBindingTarget {
    /// A synchronous Begin's call bound: the configured call bound, but at
    /// least `ENGRAM_BEGIN_MIN_CALL_TIMEOUT_MS`, at most the shared cap, and
    /// never past `deadline`.
    fn begin_rpc_timeout_until(
        &self,
        deadline: std::time::Instant,
    ) -> Result<Duration, EngramTransportError> {
        let remaining = deadline.saturating_duration_since(self.budget_clock.now());
        if remaining.is_zero() {
            return Err(EngramTransportError::deadline(
                "Engram operation budget exhausted before transport",
            ));
        }
        Ok(self
            .settings
            .call_timeout()
            .max(Duration::from_millis(ENGRAM_BEGIN_MIN_CALL_TIMEOUT_MS))
            .min(Duration::from_millis(ENGRAM_MAX_CALL_TIMEOUT_MS))
            .min(remaining))
    }
}

impl AppState {
    /// The prepared begin `session_id`'s head replays, when it is
    /// begin-unknown and, given an owner, still that owner's head.
    fn engram_begin_replay_for(
        &self,
        session_id: &str,
        owner: Option<&EngramQueuedAdmissionOwner>,
    ) -> Option<EngramPreparedBegin> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner.find_session_index(session_id)?];
        if owner.is_some_and(|owner| !owner.matches(record)) {
            return None;
        }
        engram_begin_replay_pending(record).cloned()
    }

    /// Engram proved `grant_id` never begun: the owner's possibly-begun marker
    /// for it is settled, so the head may proceed through a fresh evaluate.
    fn settle_engram_begin_never_begun(
        &self,
        session_id: &str,
        owner: Option<&EngramQueuedAdmissionOwner>,
        grant_id: &str,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if owner.is_some_and(|owner| owner.matches(record))
            && record.engram.uncertain_grant_id.as_deref() == Some(grant_id)
        {
            record.engram.uncertain_grant_id = None;
        }
    }

    /// Whether producer status names `grant_id` as the session's open grant
    /// in the begun state: the only answer that settles an ambiguous replay
    /// refusal as applied. Status names an issued grant open too, so an issued,
    /// unknown or absent state, a failed read, another grant or none settles
    /// nothing.
    fn engram_status_shows_grant_begun(
        &self,
        target: &EngramBindingTarget,
        routing_token: &str,
        grant_id: &str,
        deadline: std::time::Instant,
        owner: Option<&EngramQueuedAdmissionOwner>,
    ) -> bool {
        let Some(owner) = owner else {
            return false;
        };
        let Ok(timeout) = target.rpc_timeout_until(deadline) else {
            return false;
        };
        if !self.queued_engram_owner_is_current(&target.connection.session_id, owner) {
            return false;
        }
        let status = target
            .adapter
            .request(
                &target.connection,
                &EngramControlRequest::SessionStatus {
                    routing_token: routing_token.to_owned(),
                },
                timeout,
            )
            .and_then(parse_engram_result::<EngramSessionStatusResponse>);
        self.queued_engram_owner_is_current(&target.connection.session_id, owner)
            && status.is_ok_and(|status| {
                status.open_grant_id.as_deref() == Some(grant_id)
                    && status.open_grant_state.as_deref() == Some("begun")
            })
    }
}
