// Owns the bounded HTTP registration-confirmation projection beside the full
// detail route. Selection and identity fences stay with the existing exact
// query; this projection never truncates or changes durable run evidence.

#[derive(Deserialize)]
struct TestRunDetailQuery {
    view: Option<TestRunDetailView>,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum TestRunDetailView {
    Detail,
    Registration,
}

const TEST_RUN_REGISTRATION_CONFIRMATION_MAX_BYTES: usize = 64 * 1024;

fn test_run_registration_confirmation(response: TestRunDetailResponse) -> Result<Response, ApiError> {
    let run = response.run;
    // Fixed identity fields only: no stages, logs, command, sourceId, bound
    // work lists or per-path data. Keep nulls so missing identity fails closed
    // in the adapter, rather than manufacturing an acknowledged identity.
    let value = json!({
        "source": response.source,
        "run": {
            "runId": run.run_id,
            "ownerSessionId": run.owner_session_id,
            "initiatingSessionId": run.registration.initiating_session_id,
            "registrationState": run.registration.registration_state,
            "trackingDurability": run.registration.tracking_durability,
            "trackingProvenances": run.registration.tracking_provenances,
            "creatorPid": run.registration.creator_pid,
            "startedAt": run.started_at,
            "detached": run.detached,
            "runDir": run.run_dir,
            "worktree": run.worktree,
        }
    });
    let bytes = serde_json::to_vec(&value)
        .map_err(|error| ApiError::internal(format!("test run confirmation serialization failed: {error}")))?;
    if bytes.len() > TEST_RUN_REGISTRATION_CONFIRMATION_MAX_BYTES {
        return Err(ApiError::from_status(StatusCode::PAYLOAD_TOO_LARGE,
            "test run registration metadata exceeds the confirmation response bound"));
    }
    Ok(([(axum::http::header::CONTENT_TYPE, "application/json")], bytes).into_response())
}
