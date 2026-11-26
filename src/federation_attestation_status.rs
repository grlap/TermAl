// Owns redacted runtime attestation observations and owner-generation status.
// Does not persist keys or change membership. New projection beside federation_views.rs.

use super::*;
use std::io::Write;

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
                && terminal.transport_generation == peer.transport_generation
                && terminal.owner_generation == self.owner;
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
        self.fence_terminal_event(context, stage, reason, io_kind, None, |_| {});
    }

    /// Fences the exact incarnation after an RPC security failure.
    pub(crate) fn fence_rpc_security_failure(
        &self,
        context: &FederationSecurityContext,
        reason: &'static str,
        path: &str,
        rpc_stage: &'static str,
        status: Option<u16>,
    ) {
        self.fence_terminal_event(
            context,
            "rpc",
            reason,
            None,
            Some((path, rpc_stage, status)),
            |state| self.fence_security_locked(state, context.owner, &context.incarnation),
        );
    }

    pub(crate) fn fence_terminal_event(
        &self,
        context: &FederationSecurityContext,
        stage: &'static str,
        reason: &'static str,
        io_kind: Option<&'static str>,
        rpc: Option<(&str, &'static str, Option<u16>)>,
        fence: impl FnOnce(&mut FederationLinkState),
    ) {
        if self.record_terminal_event(context, stage, reason, io_kind, fence) {
            self.write_terminal_diagnostic(context, stage, reason, io_kind, rpc);
        }
    }

    // Cancellation can record the event without introducing console I/O.
    pub(crate) fn record_terminal_event(
        &self,
        context: &FederationSecurityContext,
        stage: &'static str,
        reason: &'static str,
        io_kind: Option<&'static str>,
        fence: impl FnOnce(&mut FederationLinkState),
    ) -> bool {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                // An exact withdrawal still has to run during recovery.
                fence(&mut poisoned.into_inner());
                return false;
            }
        };
        // The first claim, withdrawal and structured publication all use the
        // same owner as installation. Console I/O stays outside that lock.
        let first = !context.terminal_logged.swap(true, Ordering::SeqCst);
        let terminal_at = first.then(|| self.control.clock.now());
        fence(&mut state);
        let Some(terminal_at) = terminal_at else {
            return false;
        };
        self.retain_terminal_observation(
            context,
            stage,
            reason,
            io_kind,
            terminal_at,
            state.installed_owner,
        );
        true
    }

    fn retain_terminal_observation(
        &self,
        context: &FederationSecurityContext,
        stage: &'static str,
        reason: &'static str,
        io_kind: Option<&'static str>,
        terminal_at: std::time::Instant,
        installed_owner: u64,
    ) {
        let terminal = context
            .diagnostic_basis
            .get()
            .filter(|_| context.installed_at.get().is_some() && context.owner == installed_owner)
            .map(|&(enrollment_generation, transport_generation)| {
                let heartbeat = context.last_accepted_heartbeat();
                FederationTerminalObservation {
                    role: self.snapshot.local_role,
                    stage,
                    reason,
                    io_kind,
                    incarnation: context.incarnation.clone(),
                    owner_generation: context.owner,
                    enrollment_generation,
                    transport_generation,
                    last_accepted_sequence: heartbeat.map(|(sequence, _)| sequence),
                    last_accepted_age_ms_at_terminal: heartbeat.map(|(_, at)| {
                        terminal_at
                            .saturating_duration_since(at)
                            .as_millis()
                            .min(u128::from(u64::MAX)) as u64
                    }),
                }
            });
        self.update_attestation(|observation, basis| {
            if let Some(terminal) = terminal.filter(|terminal| {
                terminal.enrollment_generation == basis.enrollment_generation
                    && terminal.transport_generation == basis.transport_generation
            }) {
                if observation.last_terminal.as_ref().is_none_or(|last| {
                    (
                        last.enrollment_generation,
                        last.transport_generation,
                        last.owner_generation,
                    ) < (
                        terminal.enrollment_generation,
                        terminal.transport_generation,
                        terminal.owner_generation,
                    )
                }) {
                    observation.last_terminal = Some(terminal);
                }
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
    }

    fn write_terminal_diagnostic(
        &self,
        context: &FederationSecurityContext,
        stage: &'static str,
        reason: &'static str,
        io_kind: Option<&'static str>,
        rpc: Option<(&str, &'static str, Option<u16>)>,
    ) {
        self.retain_terminal_diagnostic(context, stage, reason, io_kind, rpc);
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
        // A closed console must not replace the caller's typed terminal error.
        let _ = writeln!(
            std::io::stderr().lock(),
            "federation link ended peer={} role={role} stage={stage} reason={reason} ioKind={} incarnation={} elapsedMs={elapsed_ms} lastSequence={sequence} rpcPath={rpc_path} rpcStage={rpc_stage} httpStatus={status}",
            self.snapshot.peer.host_id,
            io_kind.unwrap_or("none"),
            context.incarnation
        );
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
            let Ok(basis) = self.basis.read().map(|basis| basis.clone()) else {
                return;
            };
            let Ok(mut inner) = app.inner.lock() else {
                return;
            };
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
