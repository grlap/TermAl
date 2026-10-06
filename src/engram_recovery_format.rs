//! Dormant recovery contracts. No session field, loader or admission caller
//! uses these types until ownership and its coherent cancel route land together.
//! Exact ACK uses the existing worker; it never falls back to a manual writer.

use super::*;
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct EngramRecoveryEnvelope<P> {
    pub(super) schema_version: u16,
    pub(super) envelope_id: String,
    pub(super) revision: u64,
    pub(super) authority: EngramRecoveryAuthority,
    pub(super) payload: P,
    pub(super) phase: EngramRecoveryPhase,
    pub(super) settlement: Option<Value>,
    pub(super) attempts: u32,
    pub(super) held_since: String,
    pub(super) due_at: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(super) enum EngramRecoveryPhase {
    Prepared,
    Settled,
    Held,
    OperatorCancelled,
    Delivered,
    Closed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct EngramRecoveryAuthority {
    pub(super) project_id: String,
    pub(super) connection: EngramConnectionConfig,
    pub(super) settings: EngramProjectSettings,
    pub(super) work_binding: Option<EngramControlWorkBinding>,
    #[serde(default)]
    pub(super) origin: EngramRecoveryOrigin,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub(super) enum EngramRecoveryOrigin {
    #[default]
    ModernBegin,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct EngramRecoveryAttempt {
    pub(super) envelope_id: String,
    pub(super) revision: u64,
    pub(super) attempt_id: String,
    pub(super) authority: EngramRecoveryAuthority,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct EngramBeginReplay {
    pub(super) routing_token: String,
    pub(super) original_head: Value,
    pub(super) prompt_id: String,
    pub(super) fingerprint: String,
    pub(super) dispatch_generation: u64,
    pub(super) grant_id: String,
    pub(super) delivery_tokens: Vec<String>,
    pub(super) idempotency_key: String,
    pub(super) grant_basis: Value,
}

#[derive(Clone, Debug)]
pub(super) enum EngramBeginLocalFault {
    PrepareAck(String),
    ResultAck(String),
    InvalidReply(String),
    StaleOwner,
}

impl AppState {
    /// Blocking boundary: callers release Inner before requesting this ACK.
    pub(super) fn acknowledge_engram_recovery(
        &self,
        session_id: &str,
        content: Value,
        deadline: std::time::Instant,
    ) -> PersistFenceResult {
        let clock = self.engram_budget_clock();
        if clock.now() >= deadline {
            return Err(PersistFenceError::Deadline);
        }
        let (fence, waiter) = PersistFence::new_with_clock(
            PersistFenceTarget::EngramRecovery {
                session_id: session_id.to_owned(),
                content,
            },
            deadline,
            clock,
        );
        self.persist_tx
            .send(PersistRequest::Fence(Box::new(fence)))
            .map_err(|_| PersistFenceError::WorkerStopped)?;
        waiter.wait()
    }
}

/// Messages must come from the complete durable transcript, not metadata or its resident tail.
pub(super) fn engram_recovery_image_from_row(row: &Value, messages: Value) -> Value {
    json!({"envelope":row.get("engramBeginRecovery"),
        "audit":row.get("engramBeginRecoveryAudit"),
        "queue":row.get("queuedPrompts"),
        "messages":messages,
        "generation":row.get("engramDispatchGeneration"),
        "uncertainGrant":row.get("engramUncertainGrantId"),
        "cancelAudit":row.get("engramBeginCancelAudit"),
        "format":row.get("engramBeginRecoveryFormat"),
        "operationId":row.get("engramBeginOperationId"),
        "cancelCapture":row.get("engramBeginCancelCapture")})
}
