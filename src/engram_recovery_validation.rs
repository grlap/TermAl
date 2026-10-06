//! Dormant raw validation and cancellation proof; no loader or operator callers.
use super::*;
#[derive(Clone, Debug, Default)]
pub(super) struct EngramBeginStoredRecovery {
    pub(super) raw_envelope: Option<Value>,
    pub(super) raw_audit: Value,
    pub(super) raw_cancel_audit: Option<Value>,
    pub(super) raw_uncertain_grant: Option<Value>,
    pub(super) raw_format: Value,
    pub(super) operation_id: Option<Value>,
    pub(super) cancel_capture: Option<Value>,
    pub(super) invalid: Option<EngramRecoveryDataInvalid>,
}

#[derive(Clone, Debug)]
pub(super) enum EngramRecoveryDataInvalid {
    Envelope(String),
    Marker,
    Audit,
    CancellationTuple,
    MissingModernEnvelope,
}

pub(super) struct EngramRecoveryRawRow<'a> {
    pub(super) session: &'a Session,
    pub(super) queued_prompts: Vec<QueuedPromptRecord>,
    pub(super) engram_begin_recovery: Option<Value>,
    pub(super) engram_begin_recovery_audit: Value,
    pub(super) engram_begin_recovery_format: Value,
    pub(super) engram_begin_operation_id: Option<Value>,
    pub(super) engram_begin_cancel_capture: Option<Value>,
    pub(super) engram_begin_cancel_audit: Option<Value>,
    pub(super) uncertain_grant: Option<Value>,
}

/// Authorization inputs exclude derived hold and promotion projections.
fn engram_recovery_head_inputs(head: &QueuedPromptRecord) -> Value {
    let prompt = &head.pending_prompt;
    json!((
        &prompt.id,
        &prompt.timestamp,
        &prompt.text,
        &prompt.expanded_text,
        &prompt.attachments,
        &prompt.source,
        &head.attachments,
        head.source
    ))
}

fn engram_recovery_head_matches(head: &QueuedPromptRecord, payload: &EngramBeginReplay) -> bool {
    serde_json::from_value::<QueuedPromptRecord>(payload.original_head.clone())
        .is_ok_and(|saved| engram_recovery_head_inputs(&saved) == engram_recovery_head_inputs(head))
        && payload.prompt_id == head.pending_prompt.id
        && payload.fingerprint
            == engram_turn_intent_fingerprint(
                &head.pending_prompt.text,
                head.pending_prompt.expanded_text.as_deref(),
                &head.attachments,
                head.pending_prompt.source.as_ref(),
                head.source,
            )
}

pub(super) fn decode_engram_begin_storage(
    record: &EngramRecoveryRawRow<'_>,
) -> (
    Option<EngramRecoveryEnvelope<EngramBeginReplay>>,
    Vec<EngramRecoveryEnvelope<EngramBeginReplay>>,
    EngramBeginStoredRecovery,
) {
    let mut stored = EngramBeginStoredRecovery {
        raw_envelope: record.engram_begin_recovery.clone(),
        raw_audit: record.engram_begin_recovery_audit.clone(),
        raw_cancel_audit: record.engram_begin_cancel_audit.clone(),
        raw_uncertain_grant: record.uncertain_grant.clone(),
        raw_format: record.engram_begin_recovery_format.clone(),
        operation_id: record.engram_begin_operation_id.clone(),
        cancel_capture: record.engram_begin_cancel_capture.clone(),
        invalid: None,
    };
    let envelope = stored.raw_envelope.as_ref().and_then(|raw| {
        match serde_json::from_value::<EngramRecoveryEnvelope<EngramBeginReplay>>(raw.clone()) {
            Ok(envelope)
                if envelope.schema_version == 1
                    && !envelope.envelope_id.is_empty()
                    && envelope.revision > 0
                    && !envelope.payload.grant_id.is_empty()
                    && !envelope.payload.idempotency_key.is_empty()
                    && envelope.authority.connection.session_id == record.session.id
                    && serde_json::from_value::<QueuedPromptRecord>(
                        envelope.payload.original_head.clone(),
                    )
                    .is_ok_and(|head| {
                        engram_recovery_head_matches(&head, &envelope.payload)
                            && record
                                .queued_prompts
                                .iter()
                                .filter(|current| {
                                    current.pending_prompt.id == head.pending_prompt.id
                                })
                                .all(|current| {
                                    engram_recovery_head_inputs(current)
                                        == engram_recovery_head_inputs(&head)
                                })
                    })
                    && record
                        .engram_begin_operation_id
                        .as_ref()
                        .and_then(Value::as_str)
                        == Some(envelope.envelope_id.as_str())
                    && record.uncertain_grant.as_ref().is_none_or(|grant| {
                        grant.as_str() == Some(envelope.payload.grant_id.as_str())
                    }) =>
            {
                Some(envelope)
            }
            Ok(_) => {
                stored.invalid = Some(EngramRecoveryDataInvalid::Envelope(
                    "Invalid schema or identity".to_owned(),
                ));
                None
            }
            Err(error) => {
                stored.invalid = Some(EngramRecoveryDataInvalid::Envelope(error.to_string()));
                None
            }
        }
    });
    let audit = if stored.raw_audit.is_null() {
        Vec::new()
    } else {
        serde_json::from_value(stored.raw_audit.clone()).unwrap_or_else(|_| {
            stored
                .invalid
                .get_or_insert(EngramRecoveryDataInvalid::Audit);
            Vec::new()
        })
    };
    if !stored.raw_format.is_null() && stored.raw_format.as_bool().is_none()
        || stored
            .operation_id
            .as_ref()
            .is_some_and(|id| id.as_str().is_none_or(str::is_empty))
    {
        stored
            .invalid
            .get_or_insert(EngramRecoveryDataInvalid::Marker);
    }
    let verified_legacy = stored.cancel_capture.as_ref().is_some_and(|capture| {
        capture.get("origin").and_then(Value::as_str) == Some("legacy_local_cancel")
            && stored.raw_envelope.is_none()
            && verify_engram_local_cancel_capture(record, capture)
    });
    if stored.has_modern_evidence() && envelope.is_none() && !verified_legacy {
        stored
            .invalid
            .get_or_insert(EngramRecoveryDataInvalid::MissingModernEnvelope);
    }
    if stored
        .cancel_capture
        .as_ref()
        .is_some_and(|raw| !verify_engram_local_cancel_capture(record, raw))
        || (stored.raw_cancel_audit.is_some()
            || envelope
                .as_ref()
                .is_some_and(|envelope| envelope.phase == EngramRecoveryPhase::OperatorCancelled))
            && !engram_recovery_cancel_audit_matches(record)
    {
        stored
            .invalid
            .get_or_insert(EngramRecoveryDataInvalid::CancellationTuple);
    }
    (envelope.filter(|_| stored.invalid.is_none()), audit, stored)
}

pub(super) fn verify_engram_local_cancel_capture(
    record: &EngramRecoveryRawRow<'_>,
    raw: &Value,
) -> bool {
    let legacy = raw.get("origin").and_then(Value::as_str) == Some("legacy_local_cancel");
    let Some(operation) = raw.get("operationId").and_then(Value::as_str) else {
        return false;
    };
    let Some(revision) = raw.get("revision").and_then(Value::as_u64) else {
        return false;
    };
    let Some(head) = raw
        .get("originalHead")
        .and_then(|value| serde_json::from_value::<QueuedPromptRecord>(value.clone()).ok())
    else {
        return false;
    };
    if legacy
        && (record.engram_begin_recovery.is_some()
            || !record.engram_begin_recovery_format.is_null()
            || !record.engram_begin_recovery_audit.is_null()
                && record.engram_begin_recovery_audit != json!([]))
    {
        return false;
    }
    let authority = raw
        .get("authority")
        .and_then(|value| serde_json::from_value::<EngramRecoveryAuthority>(value.clone()).ok());
    let envelope = record.engram_begin_recovery.as_ref().and_then(|value| {
        serde_json::from_value::<EngramRecoveryEnvelope<EngramBeginReplay>>(value.clone()).ok()
    });
    let coherent = envelope.as_ref().is_none_or(|envelope| {
        !legacy
            && envelope.schema_version == 1
            && envelope.envelope_id == operation
            && match envelope.phase {
                EngramRecoveryPhase::OperatorCancelled => {
                    revision.checked_add(1) == Some(envelope.revision)
                        && record
                            .engram_begin_cancel_audit
                            .as_ref()
                            .and_then(|audit| audit.get("cancellationRevision"))
                            .and_then(Value::as_u64)
                            == Some(envelope.revision)
                }
                _ => envelope.revision == revision,
            }
            && envelope.authority.connection.session_id == record.session.id
            && Some(&envelope.authority) == authority.as_ref()
            && engram_recovery_head_matches(&head, &envelope.payload)
            && raw.get("grantId").and_then(Value::as_str)
                == Some(envelope.payload.grant_id.as_str())
            && raw.get("beginKey").and_then(Value::as_str)
                == Some(envelope.payload.idempotency_key.as_str())
            && raw.get("dispatchGeneration").and_then(Value::as_u64)
                == Some(envelope.payload.dispatch_generation)
    });
    coherent
        && raw.get("sessionId").and_then(Value::as_str) == Some(record.session.id.as_str())
        && record
            .engram_begin_operation_id
            .as_ref()
            .and_then(Value::as_str)
            == Some(operation)
        && !operation.is_empty()
        && revision > 0
        && !head.pending_prompt.id.is_empty()
        && record
            .queued_prompts
            .iter()
            .filter(|current| current.pending_prompt.id == head.pending_prompt.id)
            .all(|current| {
                engram_recovery_head_inputs(current) == engram_recovery_head_inputs(&head)
            })
        && (legacy
            || raw
                .get("grantId")
                .and_then(Value::as_str)
                .is_some_and(|grant| !grant.is_empty())
                && raw
                    .get("beginKey")
                    .and_then(Value::as_str)
                    .is_some_and(|key| !key.is_empty())
                && raw
                    .get("dispatchGeneration")
                    .and_then(Value::as_u64)
                    .is_some()
                && authority
                    .is_some_and(|authority| authority.connection.session_id == record.session.id)
                && record
                    .uncertain_grant
                    .as_ref()
                    .is_none_or(|grant| Some(grant) == raw.get("grantId")))
}

impl EngramBeginStoredRecovery {
    pub(super) fn has_modern_evidence(&self) -> bool {
        self.raw_envelope.is_some()
            || self.operation_id.is_some()
            || self.cancel_capture.is_some()
            || self.raw_format.as_bool() == Some(true)
            || self.raw_cancel_audit.is_some()
            || !self.raw_format.is_null() && self.raw_format.as_bool().is_none()
            || !self.raw_audit.is_null() && self.raw_audit != json!([])
    }
}

fn engram_recovery_cancel_audit_matches(record: &EngramRecoveryRawRow<'_>) -> bool {
    let (Some(audit), Some(capture)) = (
        record.engram_begin_cancel_audit.as_ref(),
        record.engram_begin_cancel_capture.as_ref(),
    ) else {
        return false;
    };
    let cancel_revision = capture
        .get("revision")
        .and_then(Value::as_u64)
        .and_then(|revision| revision.checked_add(1));
    cancel_revision.is_some()
        && verify_engram_local_cancel_capture(record, capture)
        && audit.get("localCapture") == Some(capture)
        && audit.get("operationId") == capture.get("operationId")
        && audit.get("revision") == capture.get("revision")
        && audit.get("grantId") == capture.get("grantId")
        && audit.get("beginKey") == capture.get("beginKey")
        && audit.get("originalText") == capture.pointer("/originalHead/pending_prompt/text")
        && audit.get("promptId") == capture.pointer("/originalHead/pending_prompt/id")
        && audit
            .get("messageId")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty())
        && audit.get("decision").and_then(Value::as_str) == Some("operator_cancelled")
        && audit.get("cancellationRevision").and_then(Value::as_u64) == cancel_revision
        && record
            .engram_begin_recovery
            .as_ref()
            .and_then(|raw| {
                serde_json::from_value::<EngramRecoveryEnvelope<EngramBeginReplay>>(raw.clone())
                    .ok()
            })
            .is_none_or(|envelope| {
                envelope.phase == EngramRecoveryPhase::OperatorCancelled
                    && Some(envelope.revision) == cancel_revision
                    && envelope.settlement.as_ref() == Some(audit)
            })
}

/// Capture revision is immutable. Operator cancellation advances the envelope exactly once;
/// its audit binds the new revision, so an old attempt cannot equal the resulting owner.
/// Input must contain the complete hydrated transcript (or an exact persisted message lookup).
/// The complete persisted queue is required; a resident tail cannot prove non-terminal truth.
pub(super) fn engram_recovery_cancel_terminal(record: &EngramRecoveryRawRow<'_>) -> bool {
    let Some(audit) = record.engram_begin_cancel_audit.as_ref() else {
        return false;
    };
    let Some(capture) = record.engram_begin_cancel_capture.as_ref() else {
        return false;
    };
    if !engram_recovery_cancel_audit_matches(record) {
        return false;
    }
    let Some(head) = capture
        .get("originalHead")
        .and_then(|h| serde_json::from_value::<QueuedPromptRecord>(h.clone()).ok())
    else {
        return false;
    };
    let message = audit.get("messageId").and_then(Value::as_str);
    !record
        .queued_prompts
        .iter()
        .any(|h| h.pending_prompt.id == head.pending_prompt.id)
        && record.session.messages.iter().any(|m| {
            matches!(m, Message::Text { id, text, .. }
            if Some(id.as_str()) == message
                && text == &format!(
                    "Begin cancelled; resend this message when ready:\n\n{}",
                    head.pending_prompt.text
                ))
        })
}
