// Durable observation delivery state. Owns immutable request/receipt pairs,
// definitive-refusal settlement and exact-content read-back. Does not own
// admission, recovery holds, prompt dispatch or runtime control. Every call
// belongs off StateInner: the observation worker or naming's acknowledgement
// readback, never a turn's critical path.
// New module beside engram_unadmitted_capture.rs.

use super::*;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum EngramRuntimeObservationDelivery {
    Pending {
        last_error: Option<String>,
    },
    Recorded {
        receipt: Value,
    },
    Refused {
        code: String,
        message: String,
    },
    /// Retains the immutable request for inspection without blocking later
    /// reports or claiming a definitive producer refusal.
    LocalFailure {
        code: String,
        message: String,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct EngramRuntimeObservationEnvelope {
    pub(crate) request: Value,
    pub(crate) delivery: EngramRuntimeObservationDelivery,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct EngramRuntimeObservationReport {
    pub(crate) host_turn_ref: String,
    pub(crate) adopted_at: String,
    pub(crate) notice_message_id: String,
    pub(crate) generation: u64,
    pub(crate) version: u64,
    pub(crate) observing_session: String,
    pub(crate) connection: EngramConnectionConfig,
    pub(crate) store: EngramAuthorityStoreKey,
    pub(crate) call_timeout_ms: u64,
    pub(crate) attempts: u32,
    pub(crate) retry_at: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) store_wait_notice_posted: bool,
    pub(crate) envelopes: Vec<EngramRuntimeObservationEnvelope>,
}

impl EngramRuntimeObservationReport {
    pub(crate) fn prepare(
        trace: &EngramRuntimeObservedTurn,
        capture: EngramRuntimeClosingCapture,
        connection: EngramConnectionConfig,
        store: EngramAuthorityStoreKey,
        observing_session: String,
        call_timeout: Duration,
    ) -> Result<Self, EngramTransportError> {
        if observing_session.trim().is_empty() || observing_session.len() > 512 {
            return Err(EngramTransportError::protocol(
                "runtime observation has no producer session identity",
            ));
        }
        let envelopes = prepare_runtime_observations(trace, capture, chrono::Utc::now())?
            .into_iter()
            .map(|request| EngramRuntimeObservationEnvelope {
                request,
                delivery: EngramRuntimeObservationDelivery::Pending { last_error: None },
            })
            .collect();
        Ok(Self {
            host_turn_ref: trace.host_turn_ref.clone(),
            adopted_at: trace.adopted_at.clone(),
            notice_message_id: trace.notice_message_id.clone(),
            generation: trace.generation,
            version: 1,
            observing_session,
            connection,
            store,
            call_timeout_ms: duration_millis(call_timeout),
            attempts: 0,
            retry_at: unadmitted_observation_timestamp(),
            store_wait_notice_posted: false,
            envelopes,
        })
    }

    pub(crate) fn pending(&self) -> Option<usize> {
        self.envelopes.iter().position(|envelope| {
            matches!(
                envelope.delivery,
                EngramRuntimeObservationDelivery::Pending { .. }
            )
        })
    }

    pub(crate) fn is_settled(&self) -> bool {
        self.pending().is_none()
    }

    /// Engram answered every envelope definitively, so nothing about this
    /// report is still owed. A local failure is settled but not discharged: its
    /// outcome stays unresolved and it keeps its session from being removed.
    pub(crate) fn is_discharged(&self) -> bool {
        self.envelopes.iter().all(|envelope| {
            matches!(
                envelope.delivery,
                EngramRuntimeObservationDelivery::Recorded { .. }
                    | EngramRuntimeObservationDelivery::Refused { .. }
            )
        })
    }

    pub(crate) fn next_version(&self) -> Self {
        let mut next = self.clone();
        next.version = next.version.saturating_add(1);
        next
    }
}

// The report has to be in the SESSION row, not only in an in-memory queue or
// a metadata snapshot that could be overwritten independently of the lane.
// This is a read, never a writer-side repair or an authority transition.
pub(crate) fn runtime_observation_report_is_durable(
    path: &FsPath,
    session_id: &str,
    expected: &EngramRuntimeObservationReport,
) -> Result<bool, EngramTransportError> {
    let expected = serde_json::to_value(expected).map_err(|error| {
        EngramTransportError::local_state(format!("runtime observation encoding failed: {error}"))
    })?;
    runtime_observation_session_entry_is_durable(
        path,
        session_id,
        "engramUnadmittedReports",
        &expected,
    )
}

pub(crate) fn runtime_observation_session_entry_is_durable(
    path: &FsPath,
    session_id: &str,
    field: &str,
    expected: &Value,
) -> Result<bool, EngramTransportError> {
    let row = runtime_observation_session_row(path, session_id)?;
    Ok(runtime_observation_row_contains(
        row.as_ref(),
        field,
        expected,
    ))
}

pub(crate) fn runtime_observation_row_contains(
    row: Option<&Value>,
    field: &str,
    expected: &Value,
) -> bool {
    row.and_then(|row| row.get(field))
        .and_then(Value::as_array)
        .is_some_and(|entries| entries.contains(expected))
}

// Read one session image so every member of a naming cut is checked against
// the same SQLite row, using the observation outbox's exact-content rule.
pub(crate) fn runtime_observation_session_row(
    path: &FsPath,
    session_id: &str,
) -> Result<Option<Value>, EngramTransportError> {
    let db = open_sqlite_state_read_connection(path).map_err(|error| {
        EngramTransportError::local_state(format!("runtime observation read-back failed: {error}"))
    })?;
    let raw = db
        .query_row(
            "SELECT value_json FROM sessions WHERE id = ?1",
            rusqlite::params![session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| {
            EngramTransportError::local_state(format!(
                "runtime observation row read failed: {error}"
            ))
        })?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let row: Value = serde_json::from_str(&raw).map_err(|error| {
        EngramTransportError::local_state(format!("runtime observation row is invalid: {error}"))
    })?;
    Ok(Some(row))
}

/// Engram's `execution_observe` refusals that record nothing for the refused
/// request (Engram docs/features/behavioral-control-plane.md: "Each refusal
/// records nothing"). Only these settle an envelope as refused.
const ENGRAM_EXECUTION_OBSERVE_PRE_WRITE_REFUSALS: &[&str] = &[
    "invalid_request",
    "execution_observation_invalid",
    "execution_observation_basis_mismatch",
    "execution_observation_policy_basis_mismatch",
];

/// Engram's answer that an earlier, different operation already holds an
/// envelope's idempotency key. Engram compares the stored intent before any
/// write, so it rejected this report unapplied: a terminal identity
/// collision, settled as `Refused` with this code and its key named.
pub(crate) const ENGRAM_IDEMPOTENCY_CONFLICT_CODE: &str = "control_operation_idempotency_conflict";

/// How a failed send of one retained envelope, keyed `key`, settles. Only a
/// refusal Engram documents as recording nothing is final (`Refused`), and
/// so is an identity collision, which is told distinctly and never sent
/// again under this key or a new one. Other failures leave the identical
/// request pending. The delivery scheduler preserves the retry budget for
/// typed temporary store refusals and bounds retries for unresolved outcomes.
pub(crate) fn engram_runtime_observation_failure_delivery(
    error: EngramTransportError,
    key: &str,
) -> EngramRuntimeObservationDelivery {
    let remote_code = (error.kind == EngramTransportErrorKind::Remote
        && error.invocation_application() != EngramInvocationApplication::CertifiedNotApplied)
        .then(|| error.code.clone())
        .flatten();
    match remote_code.as_deref() {
        Some(code) if ENGRAM_EXECUTION_OBSERVE_PRE_WRITE_REFUSALS.contains(&code) => {
            EngramRuntimeObservationDelivery::Refused {
                code: code.to_owned(),
                message: error.message,
            }
        }
        Some(ENGRAM_IDEMPOTENCY_CONFLICT_CODE) => EngramRuntimeObservationDelivery::Refused {
            code: ENGRAM_IDEMPOTENCY_CONFLICT_CODE.to_owned(),
            message: format!("key {key}: {}", error.message),
        },
        _ => EngramRuntimeObservationDelivery::Pending {
            last_error: Some(error.message),
        },
    }
}

// Only inspect typed source-bearing fields. Historical values in arbitrary
// evidence text are not source identities and must not suppress delivery.
fn runtime_observation_has_legacy_source(occurrence: &EngramInterTurnOccurrence) -> bool {
    let legacy = |revision: &str, source_id: Option<&EngramSourceId>| {
        source_id.is_none() && !revision.is_empty()
    };
    let source_basis = |basis: &EngramExecutionSourceBasis| {
        legacy(&basis.source_revision, basis.source_id.as_ref())
    };
    let change = |change: &EngramObservedSourceChange| match change {
        EngramObservedSourceChange::ContentComparison {
            baseline, sighting, ..
        } => {
            legacy(&baseline.source_revision, baseline.source_id.as_ref())
                || source_basis(&sighting.source_basis)
        }
        EngramObservedSourceChange::AssumedMissingBaseline { sighting, .. } => {
            source_basis(&sighting.source_basis)
        }
    };
    let check = |check: &EngramObservedCheck| check.source_basis.as_ref().is_some_and(source_basis);
    match occurrence {
        EngramInterTurnOccurrence::SourceSighting {
            source_basis: basis,
            ..
        } => source_basis(basis),
        EngramInterTurnOccurrence::InterTurnChange { source_change } => change(source_change),
        EngramInterTurnOccurrence::UnadmittedTurn {
            source_change,
            observed_checks,
            ..
        } => source_change.as_ref().is_some_and(change) || observed_checks.iter().any(check),
        EngramInterTurnOccurrence::ObservedCheck {
            check: observed, ..
        } => check(observed),
    }
}

// Return a new immutable version to install by CAS. Transport ambiguity keeps
// the original request pending. A documented record-nothing refusal settles
// that envelope with its code; it never pauses a queue or retries as if
// permission existed.
#[cfg(test)]
pub(crate) fn deliver_runtime_observation_envelope(
    adapter: &EngramHostAdapter,
    report: &EngramRuntimeObservationReport,
    index: usize,
    timeout: Duration,
) -> Result<EngramRuntimeObservationReport, EngramTransportError> {
    deliver_runtime_observation_attempt(adapter, report, index, timeout).map(|(report, _)| report)
}

// The retry classification belongs to this attempt, not to remote message
// text or a new persisted request. The worker installs the unchanged wire.
pub(crate) fn deliver_runtime_observation_attempt(
    adapter: &EngramHostAdapter,
    report: &EngramRuntimeObservationReport,
    index: usize,
    timeout: Duration,
) -> Result<(EngramRuntimeObservationReport, bool), EngramTransportError> {
    let Some(envelope) = report.envelopes.get(index) else {
        return Err(EngramTransportError::local_state(
            "runtime observation envelope does not exist",
        ));
    };
    if !matches!(
        envelope.delivery,
        EngramRuntimeObservationDelivery::Pending { .. }
    ) {
        return Ok((report.clone(), false));
    }
    let request: EngramControlRequest = serde_json::from_value(envelope.request.clone())
        .map_err(|_| EngramTransportError::protocol("invalid retained observation shape"))?;
    if serde_json::to_value(&request).ok().as_ref() != Some(&envelope.request) {
        return Err(EngramTransportError::protocol(
            "retained observation contains unsupported fields; its original bytes remain retained",
        ));
    }
    let EngramControlRequest::ExecutionObserve { observation, .. } = &request else {
        return Err(EngramTransportError::protocol(
            "retained runtime report may send only execution_observe",
        ));
    };
    if runtime_observation_has_legacy_source(&observation.occurrence) {
        let mut next = report.next_version();
        let earlier_error = match &envelope.delivery {
            EngramRuntimeObservationDelivery::Pending {
                last_error: Some(error),
            } => format!("; earlier delivery diagnostic: {error}"),
            _ => String::new(),
        };
        next.envelopes[index].delivery = EngramRuntimeObservationDelivery::LocalFailure {
            code: "legacy_source_input_retired".to_owned(),
            message: format!(
                "retained revision-only source input is retired locally and not sent; any earlier producer outcome is unchanged{earlier_error}"
            ),
        };
        return Ok((next, false));
    }
    if !matches!(
        observation.occurrence,
        EngramInterTurnOccurrence::UnadmittedTurn { .. }
            | EngramInterTurnOccurrence::ObservedCheck { .. }
            | EngramInterTurnOccurrence::SourceSighting { .. }
    ) {
        return Err(EngramTransportError::protocol(
            "retained runtime report has another occurrence kind",
        ));
    }
    let mut next = report.next_version();
    // This read changes neither the pinned policy nor any retained byte. A
    // current eligible tuple permits replay of the original point-in-time
    // evidence; the producer still judges its account_if_eligible policy.
    if let Err(error) = adapter.read_observation_policy(&report.connection, &report.store, timeout)
    {
        let store_unavailable = engram_store_transient(&error);
        next.envelopes[index].delivery = if engram_store_readiness_open_kind(&error) == Some("io") {
            EngramRuntimeObservationDelivery::LocalFailure {
                code: "store_open_refused".to_owned(),
                message: error.message,
            }
        } else if !store_unavailable
            && matches!(
                error.kind,
                EngramTransportErrorKind::Protocol | EngramTransportErrorKind::LocalState
            )
        {
            EngramRuntimeObservationDelivery::LocalFailure {
                code: "runtime_observation_policy_unavailable".to_owned(),
                message: format!(
                    "not reported: observation policy disabled or unavailable: {}",
                    error.message
                ),
            }
        } else {
            EngramRuntimeObservationDelivery::Pending {
                last_error: Some(error.message),
            }
        };
        return Ok((next, store_unavailable));
    }
    let mut store_unavailable = false;
    next.envelopes[index].delivery = match adapter.request(&report.connection, &request, timeout) {
        Ok(value) => match if matches!(
            &observation.occurrence,
            EngramInterTurnOccurrence::SourceSighting { .. }
        ) {
            EngramSourceObservationReceipt::from_recorded_result(
                value.clone(),
                observation,
                &report.observing_session,
            )
        } else {
            EngramSourceObservationReceipt::from_unadmitted_result(
                value.clone(),
                observation,
                &report.observing_session,
            )
        } {
            Ok(_) => EngramRuntimeObservationDelivery::Recorded { receipt: value },
            // Invalid receipt bytes cannot prove acceptance or refusal. Keep
            // the exact request for bounded retry and a visible diagnostic.
            Err(error) => EngramRuntimeObservationDelivery::Pending {
                last_error: Some(error.message),
            },
        },
        Err(error) => {
            store_unavailable = engram_store_transient(&error);
            engram_runtime_observation_failure_delivery(error, &observation.idempotency_key)
        }
    };
    Ok((next, store_unavailable))
}
