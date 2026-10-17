// Invocation-local producer guarantees, shared by reporting and replay policy.
// Never evidence that a prior same-key attempt failed, or a process never ran.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngramInvocationApplication {
    Unknown,
    CertifiedNotApplied,
}

impl EngramInvocationApplication {
    /// Only producer error codes carry this contract. Callers classifying a
    /// transport error must also establish that it is an actual remote reply.
    fn from_remote_code(code: Option<&str>) -> Self {
        match code {
            Some("control_request_expired" | "control_store_busy") => Self::CertifiedNotApplied,
            _ => Self::Unknown,
        }
    }
}

impl EngramTransportError {
    fn invocation_application(&self) -> EngramInvocationApplication {
        if self.kind == EngramTransportErrorKind::Remote
            && self.causal_failure.as_ref().is_none_or(|cause| {
                cause.failure_class != EngramCausalFailureClass::ProducerRefusal
            })
        {
            EngramInvocationApplication::from_remote_code(self.code.as_deref())
        } else {
            EngramInvocationApplication::Unknown
        }
    }
}

/// The code on a degraded admission card, retained for an exact request replay.
/// This changes scheduling, never the retained request, key or binding owner.
fn engram_admission_code_retries(code: &str) -> bool {
    ENGRAM_ADMISSION_RETRY_CODES.contains(&code)
        || EngramInvocationApplication::from_remote_code(Some(code))
            == EngramInvocationApplication::CertifiedNotApplied
}

/// A live closing operation, not just its report. Memory-only: after a host
/// restart the existing status reconciliation owns the leftover grant.
#[derive(Clone, Debug)]
struct EngramRetainedCheckpoint {
    authority: String,
    work_binding: Option<EngramControlWorkBinding>,
    request: EngramControlRequest,
    error: EngramTransportError,
}

impl EngramRetainedCheckpoint {
    fn matches(&self, target: &EngramBindingTarget) -> bool {
        matches!(&self.request, EngramControlRequest::TurnCheckpoint { routing_token, grant_id, .. }
            if target.routing_token.as_deref() == Some(routing_token.as_str())
                && target.active_grant_id.as_deref() == Some(grant_id.as_str()))
            && self.authority == engram_abort_authority(target)
            && self.work_binding == target.work_binding
    }
}

impl AppState {
    fn retained_engram_checkpoint(
        &self,
        target: &EngramBindingTarget,
    ) -> Option<EngramRetainedCheckpoint> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner.sessions[inner.find_session_index(&target.connection.session_id)?]
            .engram
            .retained_checkpoint
            .as_ref()
            .filter(|retained| retained.matches(target))
            .cloned()
    }

    /// Settle the exact old close before a new admission can refresh its work
    /// binding, rebind, or evaluate. A later unknown failure goes back through
    /// normal reconciliation; the certificate never settles that attempt.
    fn retry_certified_engram_checkpoint(
        &self,
        target: &EngramBindingTarget,
        owner: Option<&EngramQueuedAdmissionOwner>,
        started_at: Option<std::time::Instant>,
    ) -> Result<bool, EngramTransportError> {
        let Some(retained) = self.retained_engram_checkpoint(target).filter(|retained| {
            !target.rebind_required
                && !target.circuit_open
                && retained.error.invocation_application()
                    == EngramInvocationApplication::CertifiedNotApplied
        }) else {
            return Ok(false);
        };
        if let Some(owner) = owner {
            self.require_queued_engram_owner(
                &target.connection.session_id,
                owner,
                "retained closing checkpoint",
            )?;
        }
        let timeout = match started_at {
            Some(started) => Some(target.rpc_timeout_until(target.dispatch_deadline(started))?),
            None => None,
        };
        let EngramControlRequest::TurnCheckpoint { next_intent, .. } = &retained.request else {
            return Ok(false);
        };
        let outcome = self.checkpoint_engram_turn_with_budget_off_lock(
            &target.connection.session_id,
            EngramCheckpointPurpose::Settlement,
            None,
            None,
            *next_intent,
            None,
            None,
            timeout,
        );
        if let Some(owner) = owner {
            self.require_queued_engram_owner(
                &target.connection.session_id,
                owner,
                "retained closing receipt",
            )?;
        }
        match outcome {
            EngramCheckpointOutcome::Succeeded => Ok(true),
            EngramCheckpointOutcome::Failed(_) => {
                let mut error = self
                    .retained_engram_checkpoint(target)
                    .map_or(retained.error.clone(), |retained| retained.error);
                // This checkpoint belongs to the new head's preparation, so
                // its existing durable admission retry can carry the error.
                if let Some(owner) = owner {
                    let mut cause = EngramCausalFailure::request(&error, &retained.request);
                    cause.continuation_id = Some(owner.prompt_id.clone());
                    cause.turn_generation = Some(owner.active_turn_generation);
                    error.causal_failure = Some(cause);
                }
                Err(error)
            }
            EngramCheckpointOutcome::Skipped => Err(EngramTransportError::backoff(
                "The retained closing checkpoint is owned by another lifecycle operation",
            )),
        }
    }
}
