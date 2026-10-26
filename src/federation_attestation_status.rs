// Owns redacted runtime attestation observations and owner-generation status.
// Does not persist keys or change membership. New projection beside federation_views.rs.

#[derive(Default)]
struct FederationAttestationObservation {
    failures: u64,
    owner: u64,
    transport_generation: u64,
    last_success: Option<String>,
    last_stage: Option<&'static str>,
    last_reason: Option<&'static str>,
    enrollment_generation: u64,
    attempt_owner: u64,
    attempt_channel: Option<String>,
}

impl FederationAttestationObservation {
    fn project(&self) -> Value {
        json!({"state": if self.failures >= 3 { "failing" } else if self.failures > 0 { "retrying" }
            else if self.owner > 0 { "authenticated" } else { "unverified" },
            "consecutiveFailures":self.failures, "ownerGeneration":self.owner, "lastSuccess":self.last_success,
            "lastStage":self.last_stage, "lastReason":self.last_reason})
    }
}

impl FederationChannel {
    fn observe_terminal(
        &self,
        context: &FederationSecurityContext,
        stage: &'static str,
        reason: &'static str,
    ) {
        self.observe_terminal_detail(context, stage, reason, None);
    }

    fn observe_terminal_detail(
        &self,
        context: &FederationSecurityContext,
        stage: &'static str,
        reason: &'static str,
        io_kind: Option<&'static str>,
    ) {
        self.observe_terminal_event(context, stage, reason, io_kind, None);
    }

    fn observe_rpc_terminal(
        &self,
        context: &FederationSecurityContext,
        reason: &'static str,
        path: &str,
        rpc_stage: &'static str,
        status: Option<u16>,
    ) {
        self.observe_terminal_event(context, "rpc", reason, None, Some((path, rpc_stage, status)));
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
        self.update_attestation(|observation, _| {
            // Record before the owning teardown. A delayed old control task
            // cannot replace its successor, nor can cleanup hide the first cause.
            if observation.attempt_owner != context.owner
                || observation.attempt_channel.as_deref()
                    != Some(self.server_instance_id.as_str())
                || matches!(observation.last_stage, Some("control" | "monitor" | "rpc"))
            {
                return;
            }
            observation.last_stage = Some(stage);
            observation.last_reason = Some(reason);
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
                self.control.clock.now().saturating_duration_since(*installed).as_millis()
            })
            .unwrap_or(0);
        let sequence = context.last_control_sequence.load(Ordering::SeqCst);
        // Paths and stages originate from fixed local route/branch constants.
        // Zero means no HTTP response was received; never log peer body text.
        let (rpc_path, rpc_stage, status) = rpc.unwrap_or(("none", "none", None));
        let status = status.unwrap_or(0);
        eprintln!(
            "federation link ended peer={} role={role} stage={stage} reason={reason} ioKind={} incarnation={} elapsedMs={elapsed_ms} lastSequence={sequence} rpcPath={rpc_path} rpcStage={rpc_stage} httpStatus={status}",
            self.snapshot.peer.host_id, io_kind.unwrap_or("none"), context.incarnation
        );
    }

    fn observe_attestation(&self, success: Option<u64>) {
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

    fn observe_attestation_failure(
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

    fn begin_attestation(&self, owner: u64) {
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
    fn federation_observed_attestation_failure(&self, expected: &FederationPeerSnapshot) -> bool {
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
