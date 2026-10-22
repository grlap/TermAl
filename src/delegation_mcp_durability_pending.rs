// Bridge-only validation and propagation of the post-enqueue Deadline
// contract. Owns no retry, persistence, operation admission or success path.

/// Distinct from both a proven no-write refusal and a transport-unknown reply.
#[derive(Debug)]
struct TermalDurabilityPendingError {
    envelope: Value,
}

impl std::fmt::Display for TermalDurabilityPendingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.envelope)
    }
}

impl std::error::Error for TermalDurabilityPendingError {}

fn durability_pending_nonempty_string(value: &Value) -> bool {
    value.as_str().is_some_and(|text| !text.is_empty())
}

/// The original POST body is supplied by the single-attempt long-call path.
/// Malformed or foreign envelopes remain ordinary non-success errors. Retain
/// only the versioned fields, not arbitrary server extensions or instructions.
fn durability_pending_for_request(
    status: StatusCode,
    method: &str,
    path: &str,
    caller: &str,
    request: &Value,
    response: &Value,
) -> Option<TermalDurabilityPendingError> {
    if status != StatusCode::SERVICE_UNAVAILABLE
        || method != "POST"
        || response["contract"] != "termal-durability-pending"
        || response["version"] != 1
        || response["code"] != "durability_pending"
        || response["outcome"] != "pending"
        || response["retry"] != json!({"mode": "same_request"})
    {
        return None;
    }
    let error = response["error"].as_str()?;
    if error.is_empty() {
        return None;
    }
    let identity = response["identity"].as_object()?;
    let field = |key: &str| identity.get(key).unwrap_or(&Value::Null);
    let mut retained = serde_json::Map::new();
    let operation;
    let keys: &[&str];
    if path == format!("/api/sessions/{caller}/engram-source-root") {
        operation = "named_root_naming";
        let action = if request.get("path").is_some() {
            "name"
        } else {
            "clear"
        };
        if response["operation"] != operation
            || field("action") != action
            || field("sessionId") != caller
            || field("requestedWork") != &request["work"]
            || field("requestedPath") != &request["path"]
            || !["workId", "claimId", "ownerId"]
                .iter()
                .all(|key| durability_pending_nonempty_string(field(key)))
            || field("ownerVersion").as_u64().is_none()
            || !(field("generation").is_null() || field("generation").as_u64().is_some())
        {
            return None;
        }
        keys = &[
            "workId",
            "claimId",
            "generation",
            "ownerId",
            "ownerVersion",
            "action",
            "sessionId",
            "requestedWork",
            "requestedPath",
        ];
    } else if path == format!("/api/sessions/{caller}/acceptance-evaluation") {
        operation = "acceptance_evaluation_submission";
        if response["operation"] != operation
            || field("evaluatorSessionId") != caller
            || !["delegationId", "attemptKey"]
                .iter()
                .all(|key| durability_pending_nonempty_string(field(key)))
            || request
                .get("attemptKey")
                .is_some_and(|key| key != field("attemptKey"))
        {
            return None;
        }
        // Legacy submissions may omit the key; the server still reports the
        // existing attempt. Current briefs require the exact echoed key.
        keys = &["delegationId", "attemptKey", "evaluatorSessionId"];
    } else if path == format!("/api/sessions/{caller}/engram-named-root-release") {
        operation = "named_root_release";
        let reason = request["reason"].as_str()?.trim();
        let work = request["work"].as_str()?.trim();
        if response["operation"] != operation
            || field("requesterSessionId") != caller
            || field("reason") != reason
            || !(field("workId") == work || field("workRef") == work)
            || !["workId", "claimId"]
                .iter()
                .all(|key| durability_pending_nonempty_string(field(key)))
            || request["generation"].as_u64().is_none()
        {
            return None;
        }
        if field("releaseStage") == "prior_authority" {
            // An older Candidate is settling before a release audit exists.
            // Its generation can differ from the requested selection's. Keep
            // both exact identities; never manufacture an audit for it.
            let selection = field("requestedSelection").as_object()?;
            if identity.get("auditId") != Some(&Value::Null)
                || field("phase") != "candidate"
                || !durability_pending_nonempty_string(field("ownerId"))
                || field("ownerVersion").as_u64().is_none()
                || !(field("generation").is_null() || field("generation").as_u64().is_some())
                || selection.get("workId") != identity.get("workId")
                || selection.get("generation") != request.get("generation")
                || !["claimId", "root"].iter().all(|key| {
                    selection
                        .get(*key)
                        .is_some_and(durability_pending_nonempty_string)
                })
            {
                return None;
            }
            let selection = ["workId", "claimId", "generation", "root"]
                .into_iter()
                .map(|key| Some((key.to_owned(), selection.get(key)?.clone())))
                .collect::<Option<serde_json::Map<String, Value>>>()?;
            retained.insert("requestedSelection".to_owned(), Value::Object(selection));
            keys = &[
                "workId",
                "workRef",
                "claimId",
                "generation",
                "auditId",
                "phase",
                "requesterSessionId",
                "reason",
                "releaseStage",
                "ownerId",
                "ownerVersion",
            ];
        } else {
            if field("generation") != &request["generation"]
                || !durability_pending_nonempty_string(field("auditId"))
                || !(field("phase").is_null() || field("phase") == "enqueued")
                || identity.contains_key("releaseStage")
            {
                return None;
            }
            keys = &[
                "workId",
                "workRef",
                "claimId",
                "generation",
                "auditId",
                "phase",
                "requesterSessionId",
                "reason",
            ];
        }
    } else {
        // Registration has its launcher validator. The plural evaluator-start
        // endpoint is not submission, and cannot inherit this contract.
        return None;
    }
    for key in keys {
        retained.insert((*key).to_owned(), identity.get(*key)?.clone());
    }
    Some(TermalDurabilityPendingError {
        envelope: json!({
            "error": error, "contract": "termal-durability-pending", "version": 1,
            "code": "durability_pending", "outcome": "pending", "operation": operation,
            "identity": retained, "retry": {"mode": "same_request"},
        }),
    })
}

fn mcp_json_rpc_durability_pending_error(
    id: Value,
    pending: &TermalDurabilityPendingError,
) -> Value {
    let mut result = mcp_tool_text_result(&pending.envelope, true);
    result["structuredContent"] = pending.envelope.clone();
    mcp_json_rpc_result(id, result)
}

#[cfg(test)]
#[path = "delegation_mcp_durability_pending_tests.rs"]
mod delegation_mcp_durability_pending_tests;
