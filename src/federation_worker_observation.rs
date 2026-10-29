// Owns the current worker's redacted progress in the existing attestation map.
// Does not schedule retries, clear quarantine, persist state or grant authority.
struct FederationWorkerObservation {
    worker_id: String,
    server_instance_id: String,
    attempt: u64,
    enrollment: u64,
    transport: u64,
    started_at: String,
    updated_at: String,
    stage: &'static str,
    retry_after_ms: Option<u64>,
    ssh: Option<FederationSshDiagnostics>,
}

impl FederationWorkerObservation {
    fn project(&self, peer: &FederationPeerRecord) -> Value {
        json!({"workerId":self.worker_id, "attempt":self.attempt,
            "serverInstanceId":self.server_instance_id, "hostId":peer.identity.host_id,
            "enrollmentGeneration":self.enrollment, "transportGeneration":self.transport,
            "currentBasis":self.enrollment == peer.enrollment_generation && self.transport == peer.transport_generation,
            "startedAt":self.started_at, "updatedAt":self.updated_at, "stage":self.stage,
            "retryAfterMs":self.retry_after_ms,
            "quarantinedForRuntime":self.stage == "runtimeQuarantine",
            "ssh":self.ssh.as_ref().map(|ssh| ssh.lock().unwrap_or_else(std::sync::PoisonError::into_inner).project())})
    }
}

struct FederationWorkerReporter {
    app: AppState,
    basis: Arc<FederationTransportBasis>,
    worker_id: String,
    attempt: u64,
}

impl FederationWorkerReporter {
    fn new(app: AppState, basis: Arc<FederationTransportBasis>) -> Self {
        Self {
            app,
            basis,
            worker_id: Uuid::new_v4().to_string(),
            attempt: 0,
        }
    }

    fn begin(&mut self) {
        self.attempt = self.attempt.saturating_add(1);
        self.update(true, |_, _| {});
    }

    fn stage(&self, stage: &'static str) {
        self.update(false, |worker, _| {
            worker.stage = stage;
            worker.retry_after_ms = None;
        });
    }

    fn retry(&self, delay: Duration) {
        self.update(false, |worker, _| {
            worker.stage = "retryBackoff";
            worker.retry_after_ms = Some(delay.as_millis().min(u64::MAX as u128) as u64);
        });
    }

    fn ssh(&self, ssh: Option<FederationSshDiagnostics>) {
        self.update(false, |worker, _| worker.ssh = ssh);
    }

    fn update(
        &self,
        begin: bool,
        update: impl FnOnce(&mut FederationWorkerObservation, &FederationPeerSnapshot),
    ) {
        // Diagnostic publication must not turn a contained task panic into
        // another panic during cleanup. Authority consumers still own refusal.
        let basis = match self.basis.read() {
            Ok(basis) => basis.clone(),
            Err(_) => return,
        };
        let Ok(mut inner) = self.app.inner.lock() else {
            return;
        };
        if !inner
            .federation
            .peers
            .get(&basis.peer.host_id)
            .is_some_and(|peer| {
                peer.identity == basis.peer
                    && peer.enrollment_generation == basis.enrollment_generation
                    && peer.transport_generation == basis.transport_generation
                    && peer.pair_capability_ref == basis.pair_capability_ref
                    && peer.endpoint == basis.endpoint
            })
        {
            return;
        }
        let observed = inner
            .federation_attestation
            .entry(basis.peer.host_id.clone())
            .or_default();
        let now = chrono::Utc::now().to_rfc3339();
        if begin {
            observed.worker = Some(FederationWorkerObservation {
                worker_id: self.worker_id.clone(),
                attempt: self.attempt,
                server_instance_id: self.app.server_instance_id.clone(),
                enrollment: basis.enrollment_generation,
                transport: basis.transport_generation,
                started_at: now.clone(),
                updated_at: now.clone(),
                stage: "basisValidation",
                retry_after_ms: None,
                ssh: None,
            });
        }
        let Some(worker) = observed
            .worker
            .as_mut()
            .filter(|worker| worker.worker_id == self.worker_id && worker.attempt == self.attempt)
        else {
            return;
        };
        worker.enrollment = basis.enrollment_generation;
        worker.transport = basis.transport_generation;
        worker.updated_at = now;
        update(worker, &basis);
    }
}

impl Drop for FederationWorkerReporter {
    fn drop(&mut self) {
        if std::thread::panicking() {
            return;
        }
        self.update(false, |worker, _| {
            if !matches!(worker.stage, "runtimeQuarantine" | "materialChanged") {
                worker.stage = "stopped";
                worker.retry_after_ms = None;
            }
        });
    }
}
