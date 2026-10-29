// Owns identity-matched settlement evidence for retained control obligations.
// Does not infer grant state from a refusal, bind a new session or grant credit.

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EngramContainmentGrantRead {
    control_schema_version: u16,
    session_id: String,
    grant_id: String,
    status: String,
    state: Option<String>,
    begun_at: Option<String>,
    completed_at: Option<String>,
}

fn engram_containment_grant_is_terminal(value: Value, session_id: &str, grant_id: &str) -> bool {
    if ![
        "control_schema_version",
        "session_id",
        "grant_id",
        "status",
        "state",
        "begun_at",
        "completed_at",
    ]
    .iter()
    .all(|field| value.get(*field).is_some())
    {
        return false;
    }
    let Ok(read) = parse_engram_result::<EngramContainmentGrantRead>(value) else {
        return false;
    };
    if read.control_schema_version != ENGRAM_CONTROL_SCHEMA_VERSION
        || read.session_id != session_id
        || read.grant_id != grant_id
        || read.status != "found"
    {
        return false;
    }
    match read.state.as_deref() {
        Some("expired" | "superseded") => read.begun_at.is_none() && read.completed_at.is_none(),
        Some("completed") => {
            let Some((begun, completed)) =
                read.begun_at.as_deref().zip(read.completed_at.as_deref())
            else {
                return false;
            };
            match (
                chrono::DateTime::parse_from_rfc3339(begun),
                chrono::DateTime::parse_from_rfc3339(completed),
            ) {
                (Ok(begun), Ok(completed)) => completed >= begun,
                _ => false,
            }
        }
        _ => false,
    }
}

fn engram_containment_settlement_evidence_matches(entry: &EngramSessionObligation) -> bool {
    let evidence = &entry.settlement_evidence;
    match evidence.get("operation").and_then(Value::as_str) {
        Some("existing_checkpoint_progress") => contained_checkpoint_progress_matches(entry),
        Some("retained_admission_transfer") => containment_admission_transfer_matches(entry),
        Some("retained_admission_decision") => containment_admission_decision_matches(entry),
        Some("turn_checkpoint")
            if entry.kind == EngramSessionObligationKind::CheckpointContainment =>
        {
            matches!(parse_engram_result::<EngramTurnCheckpointResponse>(evidence["response"].clone()),
                Ok(EngramTurnCheckpointResponse::Checkpointed { receipt }) if receipt.grant_id == entry.grant_id)
        }
        Some("turn_grant_read")
            if entry.kind == EngramSessionObligationKind::CheckpointContainment =>
        {
            !entry.grant_id.is_empty()
                && engram_containment_grant_is_terminal(
                    evidence["response"].clone(),
                    &entry.connection.session_id,
                    &entry.grant_id,
                )
        }
        Some("session_bind")
            if entry.kind == EngramSessionObligationKind::IssuedGrantRetirement =>
        {
            matches!(parse_engram_result::<EngramSessionBindingResponse>(evidence["response"].clone()),
                Ok(binding) if !binding.routing_token.is_empty()
                    && binding.status.session_id.as_deref() == Some(entry.connection.session_id.as_str())
                    && engram_status_proves_grant_retired(&binding.status))
        }
        _ => false,
    }
}

impl EngramBindingTarget {
    fn terminal_project_grant_evidence_off_lock(&self) -> Option<Value> {
        let (Some(routing_token), Some(grant_id)) = (
            &self.routing_token,
            self.active_grant_id
                .as_ref()
                .or(self.uncertain_grant_id.as_ref()),
        ) else {
            return None;
        };
        if grant_id == ENGRAM_UNCERTAIN_GRANT_UNKNOWN {
            return None;
        }
        // The exact original connection is used. No SessionBind, session_status
        // or proof of NeverSent is inferred from this supported grant read.
        let response = self
            .adapter
            .request(
                &self.connection,
                &EngramControlRequest::TurnGrantRead {
                    routing_token: routing_token.clone(),
                    grant_id: grant_id.clone(),
                },
                self.settings.call_timeout(),
            )
            .ok()?;
        engram_containment_grant_is_terminal(
            response.clone(),
            &self.connection.session_id,
            grant_id,
        )
        .then(|| json!({"operation": "turn_grant_read", "response": response}))
    }
}
