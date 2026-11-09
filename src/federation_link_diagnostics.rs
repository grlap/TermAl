// Bounded process-local copies of existing static link observations. Does not
// change transport, authority, retry, or the latest attestation projection.
const FEDERATION_LINK_HISTORY_LIMIT: usize = 64;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FederationLinkDiagnosticRecord {
    at: String,
    kind: &'static str,
    peer_host_id: String,
    enrollment_generation: u64,
    transport_generation: u64,
    generation_basis: &'static str,
    stage: &'static str,
    reason: &'static str,
    owner_generation: Option<u64>,
    incarnation: Option<String>,
    io_kind: Option<&'static str>,
    elapsed_ms: Option<u64>,
    last_sequence: Option<u64>,
    rpc_stage: Option<&'static str>,
    status: Option<u16>,
}

impl FederationLinkDiagnosticRecord {
    fn new(
        kind: &'static str,
        basis: &FederationPeerSnapshot,
        stage: &'static str,
        reason: &'static str,
    ) -> Self {
        Self {
            at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            kind,
            peer_host_id: basis.peer.host_id.clone(),
            enrollment_generation: basis.enrollment_generation,
            transport_generation: basis.transport_generation,
            generation_basis: "channel_snapshot",
            stage,
            reason,
            owner_generation: None,
            incarnation: None,
            io_kind: None,
            elapsed_ms: None,
            last_sequence: None,
            rpc_stage: None,
            status: None,
        }
    }
}

#[derive(Default)]
struct FederationLinkDiagnosticHistory {
    records: VecDeque<FederationLinkDiagnosticRecord>,
    evicted: u64,
}

impl FederationDurabilityDiagnostics {
    fn link_event(&self, record: FederationLinkDiagnosticRecord) {
        let mut history = self
            .link_history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if history.records.len() == FEDERATION_LINK_HISTORY_LIMIT {
            history.records.pop_front();
            history.evicted = history.evicted.saturating_add(1);
        }
        history.records.push_back(record);
    }
}

impl FederationChannel {
    fn retain_link_diagnostic(&self, record: FederationLinkDiagnosticRecord) {
        if let Some(app) = &self.app {
            // This is historical evidence even if the next attempt is current.
            // Diagnostic publication cannot turn a poisoned owner into a panic.
            if let Ok(inner) = app.inner.lock() {
                inner.federation_durability_diagnostics.link_event(record);
            }
        }
    }

    fn context_link_diagnostic(
        &self,
        context: &FederationSecurityContext,
        kind: &'static str,
        stage: &'static str,
        reason: &'static str,
    ) -> FederationLinkDiagnosticRecord {
        let mut record = FederationLinkDiagnosticRecord::new(kind, &self.snapshot, stage, reason);
        if let Some((enrollment, transport)) = context.diagnostic_basis.get() {
            record.enrollment_generation = *enrollment;
            record.transport_generation = *transport;
            record.generation_basis = "installed_receipt";
        } else if let Some((enrollment, transport)) = context.diagnostic_attempt_basis.get() {
            record.enrollment_generation = *enrollment;
            record.transport_generation = *transport;
            record.generation_basis = "attempt_basis";
        }
        record.owner_generation = Some(context.owner);
        record.incarnation = Some(context.incarnation.clone());
        record.elapsed_ms = context.installed_at.get().map(|installed| {
            self.control
                .clock
                .now()
                .saturating_duration_since(*installed)
                .as_millis()
                .min(u128::from(u64::MAX)) as u64
        });
        record.last_sequence = Some(context.last_control_sequence.load(Ordering::SeqCst));
        record
    }

    fn retain_install_diagnostic(&self, context: &FederationSecurityContext, status: StatusCode) {
        let mut record = self.context_link_diagnostic(
            context,
            "install_failure",
            "install",
            "localInstallFailed",
        );
        record.status = Some(status.as_u16());
        self.retain_link_diagnostic(record);
    }

    fn retain_terminal_diagnostic(
        &self,
        context: &FederationSecurityContext,
        stage: &'static str,
        reason: &'static str,
        io_kind: Option<&'static str>,
        rpc: Option<(&str, &'static str, Option<u16>)>,
    ) {
        let mut record = self.context_link_diagnostic(context, "terminal", stage, reason);
        record.io_kind = io_kind;
        if let Some((_, rpc_stage, status)) = rpc {
            // Do not retain URL paths, query text, response text or stderr.
            record.rpc_stage = Some(rpc_stage);
            record.status = status;
        }
        self.retain_link_diagnostic(record);
    }
}
