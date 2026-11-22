// Owns redacted runtime attestation observations and owner-generation status.
// Does not persist keys or change membership. New projection beside federation_views.rs.

use super::*;

#[derive(Default)]
pub(crate) struct FederationAttestationObservation {
    pub(crate) failures: u64,
    pub(crate) owner: u64,
    pub(crate) transport_generation: u64,
    last_success: Option<String>,
    last_stage: Option<&'static str>,
    last_reason: Option<&'static str>,
    pub(crate) enrollment_generation: u64,
    attempt_owner: u64,
    attempt_channel: Option<String>,
    last_terminal: Option<FederationTerminalObservation>,
    pub(crate) worker: Option<FederationWorkerObservation>,
}

struct FederationTerminalObservation {
    role: FederationRole,
    stage: &'static str,
    reason: &'static str,
    io_kind: Option<&'static str>,
    incarnation: String,
    owner_generation: u64,
    enrollment_generation: u64,
    transport_generation: u64,
    last_accepted_sequence: Option<u64>,
    last_accepted_age_ms_at_terminal: Option<u64>,
}

impl FederationAttestationObservation {
    pub(crate) fn project_for(&self, peer: &FederationPeerRecord) -> Value {
        let mut value = self.project();
        let current = self.enrollment_generation == peer.enrollment_generation
            && self.transport_generation == peer.transport_generation;
        let attempted = self.attempt_owner != 0 || self.last_stage.is_some();
        value["currentBasis"] = json!(attempted.then_some(current));
        if !current && self.last_stage.is_some() {
            value["state"] = json!("historical");
        }
        value["lastTerminal"] = self.last_terminal.as_ref().map_or(Value::Null, |terminal| {
            let current = terminal.enrollment_generation == peer.enrollment_generation
                && terminal.transport_generation == peer.transport_generation;
            json!({
                "role": terminal.role,
                "stage": terminal.stage,
                "reason": terminal.reason,
                "ioKind": terminal.io_kind,
                "incarnation": terminal.incarnation,
                "ownerGeneration": terminal.owner_generation,
                "enrollmentGeneration": terminal.enrollment_generation,
                "transportGeneration": terminal.transport_generation,
                "lastAcceptedSequence": terminal.last_accepted_sequence,
                "lastAcceptedAgeMsAtTerminal": terminal.last_accepted_age_ms_at_terminal,
                "currentBasis": current,
            })
        });
        value["worker"] = self
            .worker
            .as_ref()
            .map(|worker| worker.project(peer))
            .unwrap_or(Value::Null);
        value
    }

    pub(crate) fn project(&self) -> Value {
        json!({"state": if self.failures >= 3 { "failing" } else if self.failures > 0 { "retrying" }
            else if self.owner > 0 { "authenticated" } else { "unverified" },
            "consecutiveFailures":self.failures, "ownerGeneration":self.owner, "lastSuccess":self.last_success,
            "lastStage":self.last_stage, "lastReason":self.last_reason})
    }
}

impl FederationChannel {
    pub(crate) fn observe_terminal(
        &self,
        context: &FederationSecurityContext,
        stage: &'static str,
        reason: &'static str,
    ) {
        self.observe_terminal_detail(context, stage, reason, None);
    }

    pub(crate) fn observe_terminal_detail(
        &self,
        context: &FederationSecurityContext,
        stage: &'static str,
        reason: &'static str,
        io_kind: Option<&'static str>,
    ) {
        self.observe_terminal_event(context, stage, reason, io_kind, None);
    }

    /// Fences the exact incarnation after an RPC security failure. Claim the
    /// first terminal event and read the control clock before fencing. The
    /// diagnostic locks, shared-state publication and console line come after
    /// the fence.
    pub(crate) fn fence_rpc_security_failure(
        &self,
        context: &FederationSecurityContext,
        reason: &'static str,
        path: &str,
        rpc_stage: &'static str,
        status: Option<u16>,
    ) {
        let claimed = !context.terminal_logged.swap(true, Ordering::SeqCst);
        let terminal_at = claimed.then(|| self.control.clock.now());
        self.fence_security(context.owner, &context.incarnation);
        if let Some(terminal_at) = terminal_at {
            let line = self.publish_terminal_event(
                context,
                "rpc",
                reason,
                None,
                Some((path, rpc_stage, status)),
                terminal_at,
            );
            eprintln!("{line}");
        }
    }

    fn observe_terminal_event(
        &self,
        context: &FederationSecurityContext,
        stage: &'static str,
        reason: &'static str,
        io_kind: Option<&'static str>,
        rpc: Option<(&str, &'static str, Option<u16>)>,
    ) {
        // This context owns its one console line even if a newer context owns
        // the current status. Neither diagnostic fact changes link authority.
        if context.terminal_logged.swap(true, Ordering::SeqCst) {
            return;
        }
        let terminal_at = self.control.clock.now();
        let line = self.publish_terminal_event(context, stage, reason, io_kind, rpc, terminal_at);
        eprintln!("{line}");
    }

    /// Records a context's claimed terminal diagnostic and returns its
    /// console line.
    fn publish_terminal_event(
        &self,
        context: &FederationSecurityContext,
        stage: &'static str,
        reason: &'static str,
        io_kind: Option<&'static str>,
        rpc: Option<(&str, &'static str, Option<u16>)>,
        terminal_at: std::time::Instant,
    ) -> String {
        let heartbeat = context.last_accepted_heartbeat();
        self.retain_terminal_diagnostic(context, stage, reason, io_kind, rpc);
        self.update_attestation(|observation, basis| {
            // An attempt that fails before installation leaves the installed
            // transport current. Its context can still terminate afterward.
            if context.installed_at.get().is_some()
                && context.diagnostic_basis.get()
                    == Some(&(basis.enrollment_generation, basis.transport_generation))
            {
                observation.last_terminal = Some(FederationTerminalObservation {
                    role: self.snapshot.local_role,
                    stage,
                    reason,
                    io_kind,
                    incarnation: context.incarnation.clone(),
                    owner_generation: context.owner,
                    enrollment_generation: basis.enrollment_generation,
                    transport_generation: basis.transport_generation,
                    last_accepted_sequence: heartbeat.map(|(sequence, _)| sequence),
                    last_accepted_age_ms_at_terminal: heartbeat.map(|(_, at)| {
                        terminal_at
                            .saturating_duration_since(at)
                            .as_millis()
                            .min(u128::from(u64::MAX)) as u64
                    }),
                });
            }

            // The legacy fields still describe the latest attestation attempt.
            if observation.attempt_owner == context.owner
                && observation.attempt_channel.as_deref() == Some(self.server_instance_id.as_str())
                && !matches!(observation.last_stage, Some("control" | "monitor" | "rpc"))
            {
                observation.last_stage = Some(stage);
                observation.last_reason = Some(reason);
            }
        });
        let role = match self.snapshot.local_role {
            FederationRole::Central => "central",
            FederationRole::Satellite => "satellite",
            FederationRole::Standalone => "standalone",
        };
        let elapsed_ms = context
            .installed_at
            .get()
            .map(|installed| {
                self.control
                    .clock
                    .now()
                    .saturating_duration_since(*installed)
                    .as_millis()
            })
            .unwrap_or(0);
        let sequence = context.last_control_sequence.load(Ordering::SeqCst);
        // Paths and stages originate from fixed local route/branch constants.
        // Zero means no HTTP response was received; never log peer body text.
        let (rpc_path, rpc_stage, status) = rpc.unwrap_or(("none", "none", None));
        let status = status.unwrap_or(0);
        format!(
            "federation link ended peer={} role={role} stage={stage} reason={reason} ioKind={} incarnation={} elapsedMs={elapsed_ms} lastSequence={sequence} rpcPath={rpc_path} rpcStage={rpc_stage} httpStatus={status}",
            self.snapshot.peer.host_id,
            io_kind.unwrap_or("none"),
            context.incarnation
        )
    }

    pub(crate) fn observe_attestation(&self, success: Option<u64>) {
        self.update_attestation(|observation, basis| match success {
            Some(owner)
                if (basis.transport_generation, owner)
                    >= (observation.transport_generation, observation.owner) =>
            {
                observation.owner = owner;
                observation.transport_generation = basis.transport_generation;
                observation.failures = 0;
                observation.last_success = Some(chrono::Utc::now().to_rfc3339());
                observation.last_stage = Some("install");
                observation.last_reason = Some("authenticated");
            }
            Some(_) => {}
            None => observation.failures = observation.failures.saturating_add(1),
        });
    }

    pub(crate) fn observe_attestation_failure(
        &self,
        owner: Option<u64>,
        stage: &'static str,
        reason: &'static str,
        crypto: bool,
    ) {
        self.update_attestation(|observation, basis| {
            if owner.is_some_and(|owner| {
                owner != observation.attempt_owner
                    || observation.attempt_channel.as_deref()
                        != Some(self.server_instance_id.as_str())
            }) {
                return;
            }
            observation.enrollment_generation = basis.enrollment_generation;
            observation.transport_generation = basis.transport_generation;
            observation.last_stage = Some(stage);
            observation.last_reason = Some(reason);
            if crypto {
                observation.failures = observation.failures.saturating_add(1);
            }
        });
    }

    pub(crate) fn begin_attestation(&self, owner: u64) {
        self.update_attestation(|observation, basis| {
            observation.enrollment_generation = basis.enrollment_generation;
            observation.transport_generation = basis.transport_generation;
            observation.attempt_owner = owner;
            observation.attempt_channel = Some(self.server_instance_id.clone());
            observation.last_stage = Some(if basis.local_role == FederationRole::Central {
                "startup"
            } else {
                "attest"
            });
            observation.last_reason = Some("inProgress");
        });
    }

    fn update_attestation(
        &self,
        update: impl FnOnce(&mut FederationAttestationObservation, &FederationPeerSnapshot),
    ) {
        if let Some(app) = &self.app {
            let basis = self.transport_basis();
            let mut inner = app.inner.lock().expect("state mutex poisoned");
            // A delayed old worker cannot overwrite the observation of a
            // successor enrollment/transport. Bookkeeping-only changes pass.
            if !inner
                .federation
                .peers
                .get(&basis.peer.host_id)
                .is_some_and(|peer| {
                    peer.enrollment_generation == basis.enrollment_generation
                        && peer.transport_generation == basis.transport_generation
                })
            {
                return;
            }
            let observation = inner
                .federation_attestation
                .entry(self.snapshot.peer.host_id.clone())
                .or_default();
            update(observation, &basis);
        }
    }
}

impl AppState {
    pub(crate) fn federation_observed_attestation_failure(
        &self,
        expected: &FederationPeerSnapshot,
    ) -> bool {
        let inner = self.inner.lock().expect("state mutex poisoned");
        // A local proof observation is evidence only for this authority basis.
        // Starting another owner clears its prior terminal reason immediately.
        inner
            .federation_attestation
            .get(&expected.peer.host_id)
            .is_some_and(|observation| {
                observation.enrollment_generation == expected.enrollment_generation
                    && observation.transport_generation == expected.transport_generation
                    && observation.attempt_owner > 0
                    && observation.last_stage == Some("attest")
                    && matches!(
                        observation.last_reason,
                        Some(
                            "proofRefused"
                                | "invalidFrame"
                                | "attestExpired"
                                | "linkDownBeforeTag"
                                | "linkDownBeforeProof"
                                | "tagWriteExpired"
                        )
                    )
            })
    }
}
