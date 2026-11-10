// A census of resident report custody, not an admission or drain decision.
// Read only known wire metadata; never clone, serialize or return requests,
// receipts, identities, error text or connection details, and never open a store.

#[derive(Default, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeObservationCounters {
    reports: u64,
    unsettled_reports: u64,
    local_failure_reports: u64,
    envelopes: u64,
    pending: u64,
    recorded: u64,
    refused: u64,
    local_failure: u64,
}

#[derive(Default, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeObservationCounts {
    total: RuntimeObservationCounters,
    by_wire: RuntimeObservationWireCounts,
}

#[derive(Default, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeObservationWireCounts {
    source_id: RuntimeObservationCounters,
    source_free: RuntimeObservationCounters,
    unknown: RuntimeObservationCounters,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RuntimeObservationWire {
    SourceId,
    SourceFree,
    Unknown,
}

impl RuntimeObservationWire {
    // An absent optional source is neutral within an occurrence or report. Unknown
    // metadata dominates: it must never look like an understood source-free row.
    fn with_source(self, other: Self) -> Self {
        use RuntimeObservationWire::*;
        match (self, other) {
            (Unknown, _) | (_, Unknown) => Unknown,
            (SourceFree, next) => next,
            (previous, SourceFree) => previous,
            (SourceId, SourceId) => SourceId,
        }
    }
}

impl RuntimeObservationWireCounts {
    fn get_mut(&mut self, wire: RuntimeObservationWire) -> &mut RuntimeObservationCounters {
        match wire {
            RuntimeObservationWire::SourceId => &mut self.source_id,
            RuntimeObservationWire::SourceFree => &mut self.source_free,
            RuntimeObservationWire::Unknown => &mut self.unknown,
        }
    }
}

impl RuntimeObservationCounters {
    fn report(&mut self, report: &EngramRuntimeObservationReport) {
        self.reports += 1;
        self.unsettled_reports += u64::from(!report.is_settled());
        self.local_failure_reports += u64::from(report.envelopes.iter().any(|envelope| {
            matches!(
                envelope.delivery,
                EngramRuntimeObservationDelivery::LocalFailure { .. }
            )
        }));
    }

    fn envelope(&mut self, delivery: &EngramRuntimeObservationDelivery) {
        self.envelopes += 1;
        match delivery {
            EngramRuntimeObservationDelivery::Pending { .. } => self.pending += 1,
            EngramRuntimeObservationDelivery::Recorded { .. } => self.recorded += 1,
            EngramRuntimeObservationDelivery::Refused { .. } => self.refused += 1,
            EngramRuntimeObservationDelivery::LocalFailure { .. } => self.local_failure += 1,
        }
    }
}

// This classifies wire metadata, not the validity/eligibility of the identity
// payload. In particular it does not traverse source_id changes or child paths.
fn runtime_observation_basis_wire(basis: &Value) -> RuntimeObservationWire {
    use RuntimeObservationWire::*;
    let Some(object) = basis.as_object() else {
        return Unknown;
    };
    // Any revision-form source is unsupported. Historical bodies remain intact
    // in custody and contribute to unknown counts without a format fallback.
    match object.get("source_revision") {
        None => {}
        Some(Value::String(revision)) if revision.is_empty() => {}
        _ => return Unknown,
    }
    match object.get("source_id") {
        Some(Value::Object(source))
            if source.get("commit").is_some_and(Value::is_string)
                && source.get("tree").is_some_and(Value::is_string)
                && source.get("changes").is_some_and(Value::is_array)
                && source.get("submodules").is_some_and(Value::is_array) =>
        {
            SourceId
        }
        _ => Unknown,
    }
}

fn runtime_observation_change_wire(change: &Value) -> RuntimeObservationWire {
    use RuntimeObservationWire::*;
    let sighting = change
        .pointer("/sighting/source_basis")
        .map(runtime_observation_basis_wire)
        .unwrap_or(Unknown);
    match change.get("detection").and_then(Value::as_str) {
        Some("content_comparison") => change
            .get("baseline")
            .map(runtime_observation_basis_wire)
            .unwrap_or(Unknown)
            .with_source(sighting),
        Some("assumed_missing_baseline") => sighting,
        _ => Unknown,
    }
}

fn runtime_observation_check_wire(check: &Value) -> RuntimeObservationWire {
    if !check.is_object() {
        return RuntimeObservationWire::Unknown;
    }
    match check.get("source_basis") {
        None | Some(Value::Null) => RuntimeObservationWire::SourceFree,
        Some(basis) => runtime_observation_basis_wire(basis),
    }
}

fn runtime_observation_request_wire(request: &Value) -> RuntimeObservationWire {
    use RuntimeObservationWire::*;
    if request.get("operation").and_then(Value::as_str) != Some("execution_observe") {
        return Unknown;
    }
    let Some(occurrence) = request.get("occurrence") else {
        return Unknown;
    };
    match occurrence.get("kind").and_then(Value::as_str) {
        Some("source_sighting") => occurrence
            .get("source_basis")
            .map(runtime_observation_basis_wire)
            .unwrap_or(Unknown),
        Some("inter_turn_change") => occurrence
            .get("source_change")
            .map(runtime_observation_change_wire)
            .unwrap_or(Unknown),
        Some("observed_check") => occurrence
            .get("check")
            .map(runtime_observation_check_wire)
            .unwrap_or(Unknown),
        Some("unadmitted_turn") => {
            let source = match occurrence.get("source_change") {
                None | Some(Value::Null) => SourceFree,
                Some(change) => runtime_observation_change_wire(change),
            };
            let Some(checks) = occurrence.get("observed_checks").and_then(Value::as_array) else {
                return Unknown;
            };
            checks.iter().fold(source, |wire, check| {
                wire.with_source(runtime_observation_check_wire(check))
            })
        }
        _ => Unknown,
    }
}

fn runtime_observation_counts<'a>(
    reports: impl Iterator<Item = &'a EngramRuntimeObservationReport>,
) -> RuntimeObservationCounts {
    use RuntimeObservationWire::*;
    let mut counts = RuntimeObservationCounts::default();
    for report in reports {
        counts.total.report(report);
        let mut report_wire: Option<RuntimeObservationWire> = None;
        for envelope in &report.envelopes {
            let wire = runtime_observation_request_wire(&envelope.request);
            counts.total.envelope(&envelope.delivery);
            counts.by_wire.get_mut(wire).envelope(&envelope.delivery);
            report_wire = Some(match report_wire {
                None => wire,
                Some(previous) => previous.with_source(wire),
            });
        }
        counts
            .by_wire
            .get_mut(report_wire.unwrap_or(Unknown))
            .report(report);
    }
    counts
}

async fn get_runtime_observation_counts(
    State(state): State<AppState>,
) -> Result<Json<RuntimeObservationCounts>, ApiError> {
    run_blocking_api(move || {
        let inner = state.inner.lock().expect("state mutex poisoned");
        Ok(Json(runtime_observation_counts(
            inner
                .sessions
                .iter()
                .flat_map(|session| session.engram.unadmitted_reports.iter()),
        )))
    })
    .await
}

#[cfg(test)]
#[path = "tests/engram_runtime_observation_counts.rs"]
mod runtime_observation_counts_tests;
