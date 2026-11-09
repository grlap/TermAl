// Process-local metadata only. Observes the existing authority decisions;
// never persists, retries, changes admission or reads request payloads.
const FEDERATION_DURABILITY_HISTORY_LIMIT: usize = 64;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FederationDurabilityRecord {
    sequence: u64,
    at: String,
    last_at: String,
    count: u64,
    last_false_for_ms: Option<u64>,
    kind: &'static str,
    site: &'static str,
    stage: Option<&'static str>,
    durable: bool,
    window_id: Option<u64>,
    false_for_ms: Option<u64>,
    status: Option<u16>,
}

#[derive(Default)]
struct FederationDurabilityHistory {
    records: VecDeque<FederationDurabilityRecord>,
    sequence: u64,
    evicted: u64,
    false_since: Option<std::time::Instant>,
    window_id: Option<u64>,
}

impl FederationDurabilityHistory {
    fn push(&mut self, kind: &'static str, site: &'static str, durable: bool, status: Option<u16>) {
        self.sequence = self.sequence.saturating_add(1);
        let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let elapsed = self.false_since
            .map(|start| start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
        let record = FederationDurabilityRecord {
            sequence: self.sequence,
            last_at: at.clone(),
            at,
            count: 1,
            last_false_for_ms: elapsed,
            kind,
            site,
            durable,
            status,
            stage: (kind == "refusal").then_some(site),
            window_id: self.window_id,
            false_for_ms: elapsed,
        };
        if self.records.len() == FEDERATION_DURABILITY_HISTORY_LIMIT {
            self.records.pop_front();
            self.evicted = self.evicted.saturating_add(1);
        }
        self.records.push_back(record);
    }
}

struct FederationDurabilityDiagnostics {
    observer_id: String,
    history: Mutex<FederationDurabilityHistory>,
    link_history: Mutex<FederationLinkDiagnosticHistory>,
}

impl FederationDurabilityDiagnostics {
    fn new(durable: bool, site: &'static str) -> Self {
        let mut history = FederationDurabilityHistory::default();
        if !durable {
            history.false_since = Some(std::time::Instant::now());
            history.window_id = Some(1);
        }
        history.push("initial", site, durable, None);
        Self {
            observer_id: uuid::Uuid::new_v4().to_string(),
            history: Mutex::new(history),
            link_history: Mutex::default(),
        }
    }

    fn transition(&self, durable: bool, site: &'static str) {
        let mut history = self
            .history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !durable {
            history.false_since = Some(std::time::Instant::now());
            history.window_id = Some(history.sequence.saturating_add(1));
        }
        history.push("transition", site, durable, None);
        if durable {
            history.false_since = None;
            history.window_id = None;
        }
    }

    fn refusal(&self, durable: bool, stage: &'static str, status: u16) {
        let mut history = self
            .history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Repeated wire refusals during one hold must not evict its cause.
        // Keep first-observation order and update only bounded scalar metadata.
        let window = history.window_id;
        let elapsed = history.false_since
            .map(|start| start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
        let repeated = if window.is_some() {
            history.records.iter_mut().rev().find(|row| row.kind == "refusal"
                && row.window_id == window && row.stage == Some(stage)
                && row.status == Some(status) && row.durable == durable)
        } else {
            history.records.back_mut().filter(|row| row.kind == "refusal"
                && row.window_id.is_none() && row.stage == Some(stage)
                && row.status == Some(status) && row.durable == durable)
        };
        if let Some(row) = repeated {
            row.count = row.count.saturating_add(1);
            row.last_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            row.last_false_for_ms = elapsed;
        } else {
            history.push("refusal", stage, durable, Some(status));
        }
    }

    fn snapshot(&self) -> Value {
        let history = self
            .history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let links = self
            .link_history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        json!({ "available": true, "observerId": self.observer_id,
            "records": history.records, "evicted": history.evicted,
            "limit": FEDERATION_DURABILITY_HISTORY_LIMIT, "resetOnRestart": true,
            "resetOnStateReconstruction": true,
            "linkEvents": links.records, "linkEventsEvicted": links.evicted,
            "linkEventsLimit": FEDERATION_LINK_HISTORY_LIMIT })
    }
}

impl StateInner {
    fn set_federation_durable(&mut self, durable: bool, site: &'static str) {
        let changed = self.federation_durable != durable;
        self.federation_durable = durable;
        if changed {
            self.federation_durability_diagnostics
                .transition(durable, site);
        }
    }
}

#[cfg(test)]
mod federation_durability_diagnostic_tests {
    use super::*;

    #[test]
    fn records_windows_and_refusals_without_restarting_the_false_clock() {
        let mut inner = StateInner::new();
        inner.set_federation_durable(true, "boot_loaded");
        inner.set_federation_durable(false, "mutation_install");
        {
            let mut history = inner
                .federation_durability_diagnostics
                .history
                .lock()
                .unwrap();
            history.false_since = Some(std::time::Instant::now() - Duration::from_secs(2));
        }
        inner.set_federation_durable(false, "native_binding_install");
        inner
            .federation_durability_diagnostics
            .refusal(false, "directory_durability", 409);
        inner
            .federation_durability_diagnostics
            .refusal(false, "channel_authority_check", 423);
        inner.set_federation_durable(true, "durability_finished");
        let snapshot = inner.federation_durability_diagnostics.snapshot();
        let rows = snapshot["records"].as_array().unwrap();
        assert_eq!(
            rows.len(),
            6,
            "a repeated false assignment is not a transition"
        );
        let window = &rows[2]["windowId"];
        for row in &rows[2..] {
            assert_eq!(&row["windowId"], window);
        }
        assert_eq!(rows[3]["status"], 409);
        assert_eq!(rows[4]["status"], 423);
        assert_eq!(rows[5]["durable"], true);
        assert!(rows[5]["falseForMs"].as_u64().unwrap() >= 2000);
        for row in rows {
            assert!(chrono::DateTime::parse_from_rfc3339(row["at"].as_str().unwrap()).is_ok());
        }
    }

    #[test]
    fn bounds_records_and_starts_a_new_history_on_construction() {
        let mut inner = StateInner::new();
        for _ in 0..70 {
            inner.set_federation_durable(!inner.federation_durable, "mutation_install");
        }
        let snapshot = inner.federation_durability_diagnostics.snapshot();
        assert_eq!(snapshot["records"].as_array().unwrap().len(), 64);
        assert_eq!(snapshot["evicted"], 7);
        let fresh = StateInner::new()
            .federation_durability_diagnostics
            .snapshot();
        assert_eq!(fresh["records"].as_array().unwrap().len(), 1);
        assert_eq!(fresh["records"][0]["kind"], "initial");
        assert_eq!(fresh["evicted"], 0);
    }
}
