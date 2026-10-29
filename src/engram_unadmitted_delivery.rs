// Durable observation delivery state. Owns immutable request/receipt pairs,
// definitive-refusal settlement and exact-content read-back. Does not own
// admission, recovery holds, prompt dispatch or runtime control. Every call
// belongs on the observation worker, never on a turn's critical path.
// New fragment beside engram_unadmitted_capture.rs.

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum EngramRuntimeObservationDelivery {
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
struct EngramRuntimeObservationEnvelope {
    request: Value,
    delivery: EngramRuntimeObservationDelivery,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct EngramRuntimeObservationReport {
    host_turn_ref: String,
    adopted_at: String,
    notice_message_id: String,
    generation: u64,
    version: u64,
    observing_session: String,
    connection: EngramConnectionConfig,
    store: EngramAuthorityStoreKey,
    call_timeout_ms: u64,
    attempts: u32,
    retry_at: String,
    envelopes: Vec<EngramRuntimeObservationEnvelope>,
}

impl EngramRuntimeObservationReport {
    fn prepare(
        trace: &EngramRuntimeObservedTurn,
        capture: EngramRuntimeClosingCapture,
        connection: EngramConnectionConfig,
        store: EngramAuthorityStoreKey,
        routing_token: &str,
        observing_session: String,
        call_timeout: Duration,
    ) -> Result<Self, EngramTransportError> {
        if observing_session.trim().is_empty() || observing_session.len() > 512 {
            return Err(EngramTransportError::protocol(
                "runtime observation has no producer session identity",
            ));
        }
        let envelopes =
            prepare_runtime_observations(trace, capture, routing_token, chrono::Utc::now())?
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
            envelopes,
        })
    }

    fn pending(&self) -> Option<usize> {
        self.envelopes.iter().position(|envelope| {
            matches!(
                envelope.delivery,
                EngramRuntimeObservationDelivery::Pending { .. }
            )
        })
    }

    fn is_settled(&self) -> bool {
        self.pending().is_none()
    }

    fn next_version(&self) -> Self {
        let mut next = self.clone();
        next.version = next.version.saturating_add(1);
        next
    }
}

// The report has to be in the SESSION row, not only in an in-memory queue or
// a metadata snapshot that could be overwritten independently of the lane.
// This is a read, never a writer-side repair or an authority transition.
fn runtime_observation_report_is_durable(
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

fn runtime_observation_session_entry_is_durable(
    path: &FsPath,
    session_id: &str,
    field: &str,
    expected: &Value,
) -> Result<bool, EngramTransportError> {
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
        return Ok(false);
    };
    let row: Value = serde_json::from_str(&raw).map_err(|error| {
        EngramTransportError::local_state(format!("runtime observation row is invalid: {error}"))
    })?;
    let reports = row.get(field).and_then(Value::as_array);
    Ok(reports.is_some_and(|reports| reports.contains(expected)))
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
const ENGRAM_IDEMPOTENCY_CONFLICT_CODE: &str = "control_operation_idempotency_conflict";

/// How a failed send of one retained envelope, keyed `key`, settles. Only a
/// refusal Engram documents as recording nothing is final (`Refused`), and
/// so is an identity collision, which is told distinctly and never sent
/// again under this key or a new one. Every other error, transport
/// ambiguity and any other Remote code alike (a storage error, a code this
/// build does not know), proves nothing about what was recorded: the
/// identical request stays pending for its bounded replay.
fn engram_runtime_observation_failure_delivery(
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

// Return a new immutable version to install by CAS. Transport ambiguity keeps
// the original request pending. A documented record-nothing refusal settles
// that envelope with its code; it never pauses a queue or retries as if
// permission existed.
fn deliver_runtime_observation_envelope(
    adapter: &EngramHostAdapter,
    report: &EngramRuntimeObservationReport,
    index: usize,
    timeout: Duration,
) -> Result<EngramRuntimeObservationReport, EngramTransportError> {
    let Some(envelope) = report.envelopes.get(index) else {
        return Err(EngramTransportError::local_state(
            "runtime observation envelope does not exist",
        ));
    };
    if !matches!(
        envelope.delivery,
        EngramRuntimeObservationDelivery::Pending { .. }
    ) {
        return Ok(report.clone());
    }
    let request: EngramControlRequest =
        serde_json::from_value(envelope.request.clone()).map_err(|error| {
            EngramTransportError::protocol(format!("invalid retained observation: {error}"))
        })?;
    let EngramControlRequest::ExecutionObserve { observation, .. } = &request else {
        return Err(EngramTransportError::protocol(
            "retained runtime report may send only execution_observe",
        ));
    };
    if !matches!(
        observation.occurrence,
        EngramInterTurnOccurrence::UnadmittedTurn { .. }
            | EngramInterTurnOccurrence::ObservedCheck { .. }
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
        next.envelopes[index].delivery = if matches!(
            error.kind,
            EngramTransportErrorKind::Protocol | EngramTransportErrorKind::LocalState
        ) {
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
        return Ok(next);
    }
    next.envelopes[index].delivery = match adapter.request(&report.connection, &request, timeout) {
        Ok(value) => match EngramSourceObservationReceipt::from_unadmitted_result(
            value.clone(),
            observation,
            &report.observing_session,
        ) {
            Ok(_) => EngramRuntimeObservationDelivery::Recorded { receipt: value },
            // Invalid receipt bytes cannot prove acceptance or refusal. Keep
            // the exact request for bounded retry and a visible diagnostic.
            Err(error) => EngramRuntimeObservationDelivery::Pending {
                last_error: Some(error.message),
            },
        },
        Err(error) => engram_runtime_observation_failure_delivery(error, &observation.idempotency_key),
    };
    Ok(next)
}
