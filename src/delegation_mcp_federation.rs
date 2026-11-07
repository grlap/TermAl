// Owns the read-only agent facade over existing federation/utilization HTTP
// projections. Does not sample, infer link authority, persist, or recover peers.
// New bridge adapter; runtime and projection owners remain unchanged.
const AGENT_FEDERATION_SOURCE_BYTES: u64 = 18 * 1024 * 1024;
const AGENT_FEDERATION_OUTPUT_BYTES: usize = 1024 * 1024;

fn federation_status_tool_definition() -> Value {
    json!({
        "name": "termal_federation_status",
        "description": "Read this host's federation and host-utilization projections through the existing routes. Reports roles, peers, worker stages and CPU/memory/disk metrics with source freshness, states and reasons. No running-build claim, placement or recovery action. Utilization refusal is reported separately without fabricating an empty roster. Root sessions only. Takes no arguments.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
    })
}

impl TermalDelegationMcpBridge {
    fn tool_federation_status(&self, arguments: Value) -> Result<Value> {
        if !arguments
            .as_object()
            .is_some_and(|object| object.is_empty())
        {
            bail!("federation status takes no arguments");
        }
        let (_, federation, refusal) = self.federation_status_get("/api/federation");
        if refusal.is_some() {
            bail!("federation status route unavailable");
        }
        let (status, utilization, reason) =
            self.federation_status_get("/api/federation/host-utilization");
        agent_federation_status(&federation, &utilization, status, reason)
    }

    fn federation_status_get(&self, path: &str) -> (Option<u16>, Value, Option<&'static str>) {
        let Ok(response) = self
            .client
            .get(self.url(path))
            .timeout(self.request_timeout)
            .send()
        else {
            return (None, Value::Null, Some("routeUnavailable"));
        };
        let status = response.status().as_u16();
        // Do not return a backend error body: it is not a status projection.
        if status != 200 {
            return (Some(status), Value::Null, Some("routeRefused"));
        }
        let mut bytes = Vec::new();
        if std::io::Read::read_to_end(
            &mut std::io::Read::take(response, AGENT_FEDERATION_SOURCE_BYTES + 1),
            &mut bytes,
        )
        .is_err()
        {
            return (Some(status), Value::Null, Some("responseUnavailable"));
        }
        if bytes.len() as u64 > AGENT_FEDERATION_SOURCE_BYTES {
            return (Some(status), Value::Null, Some("sourceLimit"));
        }
        match serde_json::from_slice(&bytes) {
            Ok(value) => (Some(status), value, None),
            Err(_) => (Some(status), Value::Null, Some("invalidResponse")),
        }
    }
}

// Allowlisting removes paths, capability references, toolchain strings, keys
// and raw diagnostics. Numeric values and existing state/reason pairs are
// copied, never recalculated. Excess input fails; no truncated roster is made.
fn agent_status_fields(source: &Value, keys: &[&str]) -> Result<Value> {
    let object = source
        .as_object()
        .context("unusable federation status object")?;
    let mut result = serde_json::Map::new();
    for key in keys {
        if let Some(value) = object.get(*key) {
            if value.is_array() || value.is_object() {
                bail!("unusable federation status scalar");
            }
            if let Some(text) = value.as_str() {
                if text.len() > 128
                    || text
                        .chars()
                        .any(|c| c.is_control() || matches!(c, '/' | '\\'))
                {
                    bail!("unusable federation status label");
                }
            }
            result.insert((*key).to_owned(), value.clone());
        }
    }
    Ok(Value::Object(result))
}

fn agent_status_identity(source: &Value) -> Result<Value> {
    if source.is_null() {
        return Ok(Value::Null);
    }
    agent_status_fields(source, &["hostId", "hostGeneration"])
}

fn agent_status_optional(
    target: &mut Value,
    source: &Value,
    key: &str,
    project: impl FnOnce(&Value) -> Result<Value>,
) -> Result<()> {
    if let Some(value) = source.get(key) {
        target[key] = if value.is_null() {
            Value::Null
        } else {
            project(value)?
        };
    }
    Ok(())
}

fn agent_status_attestation(source: &Value) -> Result<Value> {
    let mut out = agent_status_fields(
        source,
        &[
            "state",
            "consecutiveFailures",
            "ownerGeneration",
            "lastSuccess",
            "lastStage",
            "lastReason",
            "currentBasis",
        ],
    )?;
    agent_status_optional(&mut out, source, "worker", |worker| {
        let mut out = agent_status_fields(
            worker,
            &[
                "workerId",
                "attempt",
                "serverInstanceId",
                "hostId",
                "enrollmentGeneration",
                "transportGeneration",
                "currentBasis",
                "startedAt",
                "updatedAt",
                "stage",
                "retryAfterMs",
                "quarantinedForRuntime",
                "recoveryReason",
            ],
        )?;
        agent_status_optional(&mut out, worker, "ssh", |ssh| {
            agent_status_fields(
                ssh,
                &[
                    "class",
                    "stderrComplete",
                    "truncated",
                    "exited",
                    "exitCode",
                    "cleanupRequested",
                ],
            )
        })?;
        Ok(out)
    })?;
    Ok(out)
}

fn agent_status_peer(source: &Value) -> Result<Value> {
    let mut out = agent_status_fields(
        source,
        &[
            "enrollmentGeneration",
            "recordGeneration",
            "transportGeneration",
            "platform",
            "arch",
            "state",
            "verificationCurrent",
        ],
    )?;
    agent_status_optional(&mut out, source, "host", agent_status_identity)?;
    agent_status_optional(&mut out, source, "attestation", agent_status_attestation)?;
    agent_status_optional(&mut out, source, "lastObserved", agent_status_history)?;
    // profile is deliberately omitted: its hash is enrollment/profile evidence,
    // and this route does not assert that it is the current running build.
    Ok(out)
}

fn agent_status_history(source: &Value) -> Result<Value> {
    agent_status_fields(
        source,
        &["linkIncarnation", "lastSuccessfulRpc", "lastSuccessfulWake"],
    )
}

fn agent_status_federation(source: &Value) -> Result<Value> {
    let mut out = agent_status_fields(
        source,
        &[
            "enabled",
            "version",
            "trackingDurability",
            "continuityEstablished",
            "admissionQuarantine",
            "role",
        ],
    )?;
    if !matches!(
        out["role"].as_str(),
        Some("central" | "satellite" | "standalone")
    ) {
        bail!("unusable federation role");
    }
    for key in ["local", "central"] {
        agent_status_optional(&mut out, source, key, agent_status_identity)?;
    }
    let peers = source["peers"]
        .as_array()
        .context("unusable federation peers")?;
    if peers.len() > 256 {
        bail!("federation peer count exceeds limit");
    }
    out["peers"] = Value::Array(peers.iter().map(agent_status_peer).collect::<Result<_>>()?);
    agent_status_optional(&mut out, source, "satellite", |satellite| {
        let mut out = agent_status_fields(satellite, &["boundSessions"])?;
        agent_status_optional(&mut out, satellite, "central", agent_status_identity)?;
        agent_status_optional(&mut out, satellite, "centralPeer", |peer| {
            let mut projected = agent_status_fields(
                peer,
                &[
                    "enrolled",
                    "membership",
                    "recordGeneration",
                    "transportGeneration",
                    "verificationCurrent",
                    "linkConnected",
                ],
            )?;
            agent_status_optional(&mut projected, peer, "lastObserved", agent_status_history)?;
            Ok(projected)
        })?;
        Ok(out)
    })?;
    Ok(out)
}

fn agent_status_metric(source: &Value) -> Result<Value> {
    let out = agent_status_fields(source, &["state", "value", "reason"])?;
    if !matches!(
        out["state"].as_str(),
        Some("available" | "stale" | "unavailable" | "unsupported")
    ) || !(out["value"].is_number() || out["value"].is_null())
    {
        bail!("unusable utilization metric");
    }
    Ok(out)
}

fn agent_status_snapshot(source: &Value) -> Result<Value> {
    let mut out = agent_status_fields(
        source,
        &[
            "schemaVersion",
            "os",
            "architecture",
            "measuredAt",
            "sourceAgeMs",
            "staleAfterMs",
        ],
    )?;
    agent_status_optional(&mut out, source, "host", agent_status_identity)?;
    for (key, metrics) in [
        ("cpu", &["logicalCount", "busyPercent"][..]),
        (
            "memory",
            &["totalBytes", "availableBytes", "inUseBytes"][..],
        ),
    ] {
        let mut projected = json!({});
        for metric in metrics {
            projected[*metric] = agent_status_metric(&source[key][*metric])?;
        }
        out[key] = projected;
    }
    let volumes = &source["volumes"];
    let mut projected = agent_status_fields(
        volumes,
        &[
            "complete",
            "failedPathCount",
            "unavailableRequiredPathCount",
            "omittedPathCountAtLeast",
        ],
    )?;
    let data = &volumes["data"];
    let mut projected_data = agent_status_fields(data, &["state", "reason"])?;
    projected_data["value"] = if data["value"].is_null() {
        Value::Null
    } else {
        let rows = data["value"]
            .as_array()
            .context("unusable volume collection")?;
        if rows.len() > 32 {
            bail!("volume count exceeds limit");
        }
        Value::Array(
            rows.iter()
                .map(|row| {
                    let mut out = json!({});
                    // Volume identity can be a filesystem path; row order is retained.
                    for metric in ["totalBytes", "freeBytes", "availableToCallerBytes"] {
                        out[metric] = agent_status_metric(&row[metric])?;
                    }
                    Ok(out)
                })
                .collect::<Result<_>>()?,
        )
    };
    projected["data"] = projected_data;
    if let Some(reasons) = volumes.get("failureReasons") {
        let reasons = reasons
            .as_array()
            .context("unusable volume failure reasons")?;
        if reasons.len() > 2
            || reasons
                .iter()
                .any(|reason| !matches!(reason.as_str(), Some("probeFailed" | "collectionLimit")))
        {
            bail!("unusable volume failure reasons");
        }
        projected["failureReasons"] = Value::Array(reasons.clone());
    }
    out["volumes"] = projected;
    Ok(out)
}

fn agent_status_utilization(source: &Value) -> Result<Value> {
    let mut out = agent_status_fields(
        source,
        &[
            "schemaVersion",
            "rosterState",
            "rosterAgeMs",
            "rosterTotal",
            "complete",
        ],
    )?;
    let rows = source["hosts"]
        .as_array()
        .context("unusable utilization roster")?;
    if rows.len() > 257 {
        bail!("utilization host count exceeds limit");
    }
    out["hosts"] = Value::Array(
        rows.iter()
            .map(|row| {
                let mut out = agent_status_fields(
                    row,
                    &["os", "architecture", "state", "reason", "receivedAt"],
                )?;
                agent_status_optional(&mut out, row, "host", agent_status_identity)?;
                agent_status_optional(&mut out, row, "snapshot", agent_status_snapshot)?;
                Ok(out)
            })
            .collect::<Result<_>>()?,
    );
    Ok(out)
}

fn agent_federation_status(
    federation: &Value,
    utilization: &Value,
    status: Option<u16>,
    reason: Option<&str>,
) -> Result<Value> {
    let (utilization, reason) = if status == Some(200) && reason.is_none() {
        match agent_status_utilization(utilization) {
            Ok(value) => (value, None),
            Err(_) => (Value::Null, Some("invalidProjection")),
        }
    } else {
        (Value::Null, reason)
    };
    let mut out = json!({
        "federation": agent_status_federation(federation)?,
        "hostUtilization": utilization,
        "hostUtilizationStatus": status,
        "hostUtilizationReason": reason,
    });
    if serde_json::to_vec(&out)?.len() > AGENT_FEDERATION_OUTPUT_BYTES {
        out["hostUtilization"] = Value::Null;
        out["hostUtilizationReason"] = json!("outputLimit");
        if serde_json::to_vec(&out)?.len() > AGENT_FEDERATION_OUTPUT_BYTES {
            bail!("federation status output exceeds byte limit");
        }
    }
    Ok(out)
}

#[cfg(test)]
#[path = "delegation_mcp_federation_tests.rs"]
mod delegation_mcp_federation_tests;
