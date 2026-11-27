// Owns the bounded HTTP registration-confirmation projection beside the full
// detail route. Selection and identity fences stay with the existing exact
// query; this projection never truncates or changes durable run evidence.

use super::*;

#[derive(Deserialize)]
pub(crate) struct TestRunDetailQuery {
    pub(crate) view: Option<TestRunDetailView>,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum TestRunDetailView {
    Detail,
    Registration,
    /// The run's Engram verification record as the host holds it
    /// (`test_run_verification_read`), with no read of the run's files.
    Verification,
}

const TEST_RUN_REGISTRATION_CONFIRMATION_MAX_BYTES: usize = 64 * 1024;

pub(super) fn test_run_registration_confirmation(
    response: TestRunDetailResponse,
) -> Result<Response, ApiError> {
    let bound = response.registration_bound;
    let run = response.run;
    // Fixed identity fields only: no stages, logs, command, sourceId, bound
    // work lists or per-path data. Keep nulls so missing identity fails closed
    // in the adapter, rather than manufacturing an acknowledged identity.
    // `bound` says only whether the registration bound the run to work, so
    // the launcher can say why an unbound run earns no credit.
    let mut value = json!({
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
            "bound": bound,
        }
    });
    if let Some(selector) = &run.registration.request_message_id {
        value["run"]["requestMessageId"] = json!(selector);
    }
    let bytes = encode_test_run_confirmation(&value, TEST_RUN_REGISTRATION_CONFIRMATION_MAX_BYTES)?;
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response())
}

/// Encodes a registration confirmation within `limit` bytes, or refuses it
/// with 413.
fn encode_test_run_confirmation<T: Serialize + ?Sized>(
    value: &T,
    limit: usize,
) -> Result<Vec<u8>, ApiError> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        ApiError::internal(format!(
            "test run confirmation serialization failed: {error}"
        ))
    })?;
    if bytes.len() > limit {
        return Err(ApiError::from_status(
            StatusCode::PAYLOAD_TOO_LARGE,
            "test run registration metadata exceeds the confirmation response bound",
        ));
    }
    Ok(bytes)
}

pub(crate) fn serialize_test_run_detail_summary<S: serde::Serializer>(
    run: &TestRunSummary,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    let Some(selector) = &run.registration.request_message_id else {
        return serde::Serialize::serialize(run, serializer);
    };
    let mut value = serde_json::to_value(run).map_err(serde::ser::Error::custom)?;
    value
        .as_object_mut()
        .ok_or_else(|| serde::ser::Error::custom("run summary is not an object"))?
        .insert("requestMessageId".to_owned(), json!(selector));
    serde::Serialize::serialize(&value, serializer)
}

#[cfg(test)]
mod confirmation_encoding_tests {
    use super::*;
    use std::cell::Cell;

    /// A long JSON array that counts the elements it has serialized, so a test
    /// can see where encoding stopped.
    struct CountedArray {
        len: usize,
        serialized: Cell<usize>,
    }

    impl Serialize for CountedArray {
        fn serialize<S: serde::Serializer>(
            &self,
            serializer: S,
        ) -> std::result::Result<S::Ok, S::Error> {
            use serde::ser::SerializeSeq;
            let mut seq = serializer.serialize_seq(Some(self.len))?;
            for _ in 0..self.len {
                self.serialized.set(self.serialized.get() + 1);
                seq.serialize_element("0123456789abcdef0123456789abcdef")?;
            }
            seq.end()
        }
    }

    #[test]
    fn an_over_limit_confirmation_is_refused_before_it_is_fully_encoded() {
        let value = CountedArray {
            len: 100_000,
            serialized: Cell::new(0),
        };
        let error = encode_test_run_confirmation(&value, 4 * 1024).unwrap_err();
        assert_eq!(error.status, StatusCode::PAYLOAD_TOO_LARGE);
        // Each element encodes to 35 bytes, so 4 KiB holds about 117 of them.
        assert!(
            value.serialized.get() <= 4 * 1024 / 35 + 2,
            "encoding must stop at the limit, not after {} elements",
            value.serialized.get()
        );

        let small = json!({"runId": "test-1"});
        assert_eq!(
            encode_test_run_confirmation(&small, 4 * 1024).unwrap(),
            serde_json::to_vec(&small).unwrap(),
            "an in-limit confirmation is encoded exactly as before"
        );
    }
}
