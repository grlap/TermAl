// Owns same-connection gating-off containment and bounded checkpoint diagnostics.
// Does not replace bindings, infer never-sent Begin or grant dispatch authority.

fn engram_containment_error_code<'a>(code: Option<&'a str>, fallback: &'a str) -> &'a str {
    code.filter(|code| {
        !code.is_empty()
            && code.len() <= 64
            && code
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    })
    .unwrap_or(fallback)
}

/// Where a project-reset checkpoint failure leaves the request, read from the
/// causal origin attached where it failed, never from the error kind: "not
/// sent" when it was never built or the transport proved it unsent, "not
/// applied" for a certified no-change answer, "refused" for an explicit
/// refusal, and otherwise "unresolved" (dispatched without a valid reply).
fn engram_project_checkpoint_outcome(error: &EngramTransportError) -> &'static str {
    match error.causal_failure.as_ref().map(|cause| &cause.remote_application) {
        Some(EngramRemoteApplication::NotStarted) => "not_sent",
        Some(EngramRemoteApplication::NotApplied) => "not_applied",
        Some(EngramRemoteApplication::Refused) => "refused",
        Some(EngramRemoteApplication::Unknown) | None => "unresolved",
    }
}

fn engram_project_checkpoint_failure(
    project_id: &str,
    session_id: &str,
    error: &EngramTransportError,
) -> String {
    let code = engram_containment_error_code(error.code.as_deref(), "checkpoint_failed");
    let outcome = engram_project_checkpoint_outcome(error);
    // Log only the operation identity and typed code, not a routing token,
    // frozen request, or transport message that could contain private output.
    eprintln!(
        "engram> project={project_id} session={session_id} operation=turn_checkpoint outcome={outcome} code={code}"
    );
    let summary = match error.kind {
        EngramTransportErrorKind::LocalState => "Engram checkpoint local ownership was unavailable",
        EngramTransportErrorKind::Remote => "Engram refused the project-reset checkpoint",
        EngramTransportErrorKind::Deadline => "Engram checkpoint deadline exceeded",
        EngramTransportErrorKind::Transport => "Engram checkpoint transport unavailable",
        EngramTransportErrorKind::Protocol => "Engram checkpoint response was invalid",
        EngramTransportErrorKind::Backoff => "Engram checkpoint transport is in backoff",
    };
    let disclosure = match outcome {
        "not_sent" => "not sent",
        "unresolved" => "outcome unknown; the original checkpoint remains unresolved; recovery must retain its identical request",
        "not_applied" => "not applied; recovery must retain the original checkpoint request",
        _ => "refused",
    };
    format!("session {session_id}: operation=turn_checkpoint code={code}: {summary} ({disclosure})")
}

struct EngramContainmentAttempt {
    obligation_id: String,
    local_session_id: String,
    owner_generation: u64,
    target: EngramBindingTarget,
    checkpoint_owned: bool,
    read_only_terminal: bool,
    runtime_running: bool,
    stop_owner: Option<EngramContainmentStopOwner>,
}

struct EngramContainmentResetGuard {
    state: AppState,
    project_id: String,
    generation: u64,
    session_ids: Vec<String>,
}

impl Drop for EngramContainmentResetGuard {
    fn drop(&mut self) {
        self.state.release_engram_project_reset_fence(
            &self.project_id,
            self.generation,
            &self.session_ids,
        );
    }
}

fn rollback_engram_containment_precommit(
    inner: &mut StateInner,
    project: &Project,
    generation: u64,
    ledger: EngramSessionObligationLedger,
    sessions: Vec<(String, EngramSessionState)>,
) {
    if let Some(current) = inner
        .projects
        .iter_mut()
        .find(|current| current.id == project.id)
    {
        current.engram = project.engram.clone();
    }
    inner.engram_session_obligations = ledger;
    for (id, previous) in sessions {
        if let Some(index) = inner.find_session_index(&id) {
            inner
                .session_mut_by_index(index)
                .expect("captured session")
                .engram = previous;
        }
    }
    inner.engram_project_resets.release(&project.id, generation);
}

impl AppState {
    // This path is restricted to the same-connection true -> false transition.
    // No fresh binding, readiness probe or inferred grant retirement belongs here.
    fn apply_engram_project_containment(
        &self,
        project_snapshot: &Project,
        settings: EngramProjectSettings,
        authority: EngramContainmentAuthority,
        expected_host: Option<&EngramHostSettings>,
    ) -> Result<StateResponse, ApiError> {
        let project_id = &project_snapshot.id;
        let (generation, session_ids, attempts, initial_image) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let project = inner
                .find_project(project_id)
                .ok_or_else(|| ApiError::not_found("project not found"))?;
            if !project_matches_engram_reset_snapshot(project, project_snapshot)
                || expected_host.is_some_and(|expected| inner.preferences.engram != *expected)
            {
                return Err(ApiError::conflict(
                    "project changed while containment was being prepared",
                ));
            }
            if inner.engram_project_resets.contains(project_id) {
                return Err(inner.engram_project_reset_conflict(project_id));
            }
            let session_ids = project_engram_session_ids_locked(&inner, project_id);
            let mut ledger = inner.engram_session_obligations.clone();
            let mut prepared = Vec::new();
            for session_id in &session_ids {
                if let Some(entry) = prepare_engram_containment_entry_locked(
                    &inner,
                    session_id,
                    project_id,
                    &authority.route,
                    authority.confirmed,
                )? {
                    ledger = ledger.with_entry(entry.obligation.clone()).map_err(|_| {
                        ApiError::conflict("Engram containment recovery cannot be safely retained")
                    })?;
                    prepared.push(entry);
                }
            }
            // Capacity/shape checks precede all remote effects and the setting.
            let generation = inner
                .claim_engram_project_reset(project_id, true)
                .ok_or_else(|| ApiError::conflict("Engram project reset ownership changed"))?;
            let previous_ledger = inner.engram_session_obligations.clone();
            let previous_sessions = session_ids
                .iter()
                .filter_map(|id| {
                    inner
                        .find_session_index(id)
                        .map(|index| (id.clone(), inner.sessions[index].engram.clone()))
                })
                .collect::<Vec<_>>();
            let mut attempts = Vec::new();
            let consumed_retry = prepared
                .iter()
                .any(|entry| entry.consume_admission_retry || entry.consume_bind_retry);
            for EngramContainmentPrepared {
                obligation,
                mut target,
                can_checkpoint,
                read_only_terminal,
                runtime_running,
                consume_admission_retry,
                consume_bind_retry,
            } in prepared
            {
                let index = inner
                    .find_session_index(&obligation.local_session_id)
                    .expect("captured session");
                let record = inner.session_mut_by_index(index).expect("captured session");
                if consume_admission_retry {
                    // The original identity/history is retained by the frozen
                    // obligation in the same snapshot. A selected old tick
                    // loses its generation; absence is not remote settlement.
                    clear_engram_admission_retry(record);
                }
                if consume_bind_retry {
                    // The strict plan admits only this sole retry kind; no
                    // abort/admission owner shares its ACK fields. A delayed
                    // waiter can complete only its old fence, not re-arm it.
                    clear_engram_bind_retry(record);
                }
                record.engram.dispatch_generation = obligation.owner_generation;
                record.engram.project_reset_in_progress = true;
                target.project_reset_owner_generation = Some(generation);
                let checkpoint_owned =
                    can_checkpoint && record.engram.begin_checkpoint(Some(generation));
                let stop_owner = runtime_running.then(|| EngramContainmentStopOwner {
                    obligation_id: obligation.id.clone(),
                    project_id: project_id.clone(),
                    reset_generation: generation,
                    runtime_token: record.runtime.runtime_token(),
                    active_turn_generation: record.active_turn_generation,
                });
                attempts.push(EngramContainmentAttempt {
                    obligation_id: obligation.id,
                    local_session_id: obligation.local_session_id,
                    owner_generation: obligation.owner_generation,
                    target,
                    checkpoint_owned,
                    read_only_terminal,
                    runtime_running,
                    stop_owner,
                });
            }
            inner.engram_session_obligations = ledger;
            inner
                .projects
                .iter_mut()
                .find(|project| project.id == *project_id)
                .expect("captured project")
                .engram = Some(settings.clone());
            let image =
                match EngramProjectContainmentFence::capture(&inner, project_id, &session_ids) {
                    Ok(image) => image,
                    Err(_) => {
                        rollback_engram_containment_precommit(
                            &mut inner,
                            project_snapshot,
                            generation,
                            previous_ledger,
                            previous_sessions,
                        );
                        return Err(ApiError::internal(
                            "failed capturing containment durability proof",
                        ));
                    }
                };
            if let Err(error) = self.commit_locked(&mut inner) {
                if consumed_retry {
                    // A failed write has no positive acknowledgement. Keep
                    // the exclusion and consumed schedule rather than revive
                    // an automatic owner whose persistence outcome is unknown.
                    inner
                        .projects
                        .iter_mut()
                        .find(|p| p.id == *project_id)
                        .expect("captured project")
                        .engram = project_snapshot.engram.clone();
                    inner.engram_project_resets.release(project_id, generation);
                    for id in &session_ids {
                        if let Some(index) = inner.find_session_index(id) {
                            let record =
                                inner.session_mut_by_index(index).expect("captured session");
                            record.engram.project_reset_in_progress = false;
                            record.engram.clear_checkpoint_if_owned_by(Some(generation));
                        }
                    }
                    return Err(ApiError::internal(format!(
                        "Engram containment persistence is unconfirmed; affected sessions remain excluded: {error:#}"
                    )));
                }
                rollback_engram_containment_precommit(
                    &mut inner,
                    project_snapshot,
                    generation,
                    previous_ledger,
                    previous_sessions,
                );
                return Err(ApiError::internal(format!(
                    "failed persisting Engram containment: {error:#}"
                )));
            }
            (generation, session_ids, attempts, image)
        };
        let _reset_guard = EngramContainmentResetGuard {
            state: self.clone(),
            project_id: project_id.clone(),
            generation,
            session_ids: session_ids.clone(),
        };
        let reset_timer = self.engram_project_reset_timer(project_id, generation);
        reset_timer.committed();
        // Before any RPC/interrupt, settings, exclusions and original identity
        // must have committed in the same primary-state transaction.
        let acknowledgement = reset_timer.phase("containment acknowledgement", None);
        if let Err(error) = self.confirm_engram_containment_until(initial_image, reset_timer.claim.deadline) {
            self.release_engram_project_reset_fence(project_id, generation, &session_ids);
            return Err(error);
        }
        drop(acknowledgement);
        // Register the whole interruption batch before budget/cap deferral.
        // This is live scheduling only; the acknowledged original obligations
        // above retain crash custody and remain excluded until settled.
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let due_at = reset_timer.claim.clock.wall_now()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let mut changed = false;
            for attempt in attempts.iter().filter(|attempt| attempt.stop_owner.is_some()) {
                let owner = attempt.stop_owner.as_ref().expect("filtered Stop owner");
                let Some(index) = inner.find_session_index(&attempt.local_session_id)
                else { continue };
                if inner.sessions[index].runtime_stop_in_progress
                    || inner.sessions[index].session.status == SessionStatus::Stopping
                {
                    // The existing Stop keeps interruption custody. It is
                    // already requested, rather than waiting for a retry slot.
                    continue;
                }
                if owner.matches_runtime(&inner.sessions[index]) {
                    if inner.sessions[index].engram_containment_stop.as_ref()
                        .is_none_or(|pending| !pending.owner.matches_runtime(&inner.sessions[index]))
                    {
                        inner.sessions[index].engram_containment_stop = Some(EngramDeferredStop {
                            owner: owner.clone(), claim: None, due_at: due_at.clone(),
                            attempts: 0, in_flight: false,
                        });
                    }
                    if let Some(entry) = inner.engram_session_obligations.entries.iter_mut()
                        .find(|entry| entry.id == owner.obligation_id)
                    {
                        entry.reason_code = "provider_stop_deferred".to_owned();
                        changed = true;
                    }
                } else if let Some(entry) = inner.engram_session_obligations.entries.iter_mut()
                    .find(|entry| entry.id == owner.obligation_id)
                {
                    entry.reason_code = "provider_stop_owner_changed".to_owned();
                    changed = true;
                }
            }
            if changed {
                self.commit_locked(&mut inner).map_err(|_| ApiError::internal(
                    "containment is applied; original provider interruption remains held for recovery",
                ))?;
            }
        }
        let settlement_deadline = reset_timer.claim.deadline;
        for (ordinal, attempt) in attempts.iter().enumerate() {
            let _phase = reset_timer.phase_work("containment settlement", None, ordinal, attempts.len());
            if attempt.runtime_running {
                // The same capped scheduler owns deferred and in-flight Stop.
                // Registration is neither interruption nor settlement proof.
                continue;
            }
            if !attempt.checkpoint_owned && !attempt.read_only_terminal {
                continue;
            }
            let started = reset_timer.claim.clock.now();
            let mut target = attempt.target.clone();
            let remaining = settlement_deadline.saturating_duration_since(started);
            target.settings.deadline_ms =
                Some(duration_millis(target.settings.call_timeout().min(remaining)).max(1));
            let receipt_reservation = if attempt.checkpoint_owned && !remaining.is_zero() {
                match self.reserve_off_checkpoint_receipt(attempt, generation) {
                    Ok(reservation) => Some(reservation),
                    Err(code) => {
                        self.note_off_checkpoint_accounting_pending(attempt, code);
                        continue;
                    }
                }
            } else {
                None
            };
            let mut result = if remaining.is_zero() {
                // Exhausted before any send: the checkpoint was never dispatched.
                let mut error = EngramTransportError::deadline(
                    "project containment settlement budget exhausted",
                );
                error.causal_failure = Some(EngramCausalFailure::local(
                    &error,
                    "turn_checkpoint",
                    "TermAl host project containment",
                ));
                Err(error)
            } else if attempt.checkpoint_owned {
                target.project_reset_checkpoint_receipt_off_lock()
            } else {
                Err(EngramTransportError::local_state(
                    "exact grant state remains unresolved",
                ))
            };
            if let (Some(reservation), Ok(response)) = (&receipt_reservation, &result) {
                if self
                    .retain_off_checkpoint_response(&reservation.key, response)
                    .is_ok()
                {
                    if self
                        .finish_off_checkpoint_receipt(&reservation.key, settlement_deadline, true)
                        .is_err()
                    {
                        self.note_off_checkpoint_accounting_pending(
                            attempt,
                            "checkpoint_receipt_accounting_pending",
                        );
                    }
                    continue;
                }
                // Unsupported adapter output cannot be treated as a retained
                // success; the write-ahead duty remains conservative.
                result = Err(EngramTransportError::protocol(
                    "unsupported checkpoint response custody",
                ));
            }
            let evidence = match &result {
                Ok(receipt) => Some(json!({"operation": "turn_checkpoint", "response": receipt})),
                Err(_) if reset_timer.claim.clock.now() < settlement_deadline => {
                    target.settings.deadline_ms = Some(
                        duration_millis(
                            target.settings.call_timeout().min(
                                settlement_deadline
                                    .saturating_duration_since(reset_timer.claim.clock.now()),
                            ),
                        )
                        .max(1),
                    );
                    target.terminal_project_grant_evidence_off_lock()
                }
                Err(_) => None,
            };
            if let Err(error) = &result
                && attempt.checkpoint_owned
            {
                engram_project_checkpoint_failure(project_id, &attempt.local_session_id, error);
                self.record_engram_project_reset_checkpoint_failure(
                    &attempt.target,
                    error,
                    reset_timer.claim.clock.elapsed_since(started),
                );
            }
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let current = inner.find_session_index(&attempt.local_session_id);
            let matches = inner
                .engram_project_resets
                .is_owned_by(project_id, generation)
                && inner
                    .find_project(project_id)
                    .and_then(|project| project.engram.as_ref())
                    == Some(&settings)
                && inner.find_project(project_id).is_some_and(|project| {
                    project.root_path == project_snapshot.root_path
                        && project.remote_id == project_snapshot.remote_id
                })
                && expected_host.is_none_or(|expected| inner.preferences.engram == *expected)
                && current.is_some_and(|index| {
                    let record = &inner.sessions[index];
                    record.engram.dispatch_generation == attempt.owner_generation
                        && record.engram.routing_token == attempt.target.routing_token
                        && record.engram.active_grant_id == attempt.target.active_grant_id
                        && record.engram.uncertain_grant_id == attempt.target.uncertain_grant_id
                });
            let mut confirmed_ledger = inner.engram_session_obligations.clone();
            let confirmation_supported = evidence.as_ref().is_some_and(|evidence| {
                let Some(entry) = confirmed_ledger
                    .entries
                    .iter_mut()
                    .find(|entry| entry.id == attempt.obligation_id)
                else {
                    return false;
                };
                entry.phase = EngramSessionObligationPhase::Confirmed;
                entry.settlement_evidence = evidence.clone();
                confirmed_ledger.validate().is_ok()
            });
            if confirmation_supported && matches {
                let record = inner
                    .session_mut_by_index(current.expect("matched owner"))
                    .expect("matched session");
                record.engram.active_grant_id = None;
                record.engram.uncertain_grant_id = None;
                record.engram.clear_checkpoint_if_owned_by(Some(generation));
                inner.engram_session_obligations = confirmed_ledger;
            } else if let Some(entry) = inner
                .engram_session_obligations
                .entries
                .iter_mut()
                .find(|entry| entry.id == attempt.obligation_id)
            {
                entry.reason_code = if !matches {
                    "containment_owner_changed".to_owned()
                } else {
                    engram_containment_error_code(
                        result
                            .as_ref()
                            .err()
                            .and_then(|error| error.code.as_deref()),
                        if result.is_ok() {
                            "settlement_evidence_unmatched"
                        } else if attempt.read_only_terminal {
                            "grant_state_unresolved"
                        } else {
                            "checkpoint_failed"
                        },
                    )
                    .to_owned()
                };
            }
            inner.engram_session_obligations.validate().map_err(|_| {
                ApiError::internal(
                    "containment settlement image is invalid; affected sessions remain excluded",
                )
            })?;
            if self.commit_locked(&mut inner).is_err() {
                drop(inner);
                self.release_engram_project_reset_fence(project_id, generation, &session_ids);
                return Err(ApiError::internal(
                    "containment is applied but settlement persistence is unconfirmed; affected sessions remain excluded",
                ));
            }
        }
        let final_image = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            EngramProjectContainmentFence::capture(&inner, project_id, &session_ids).map_err(
                |_| ApiError::conflict("containment ownership changed during settlement"),
            )?
        };
        let _acknowledgement = reset_timer.phase("containment acknowledgement", None);
        if let Err(error) = self.confirm_engram_containment_until(final_image, settlement_deadline) {
            self.release_engram_project_reset_fence(project_id, generation, &session_ids);
            return Err(error);
        }
        // Keep confirmed history. Only this acknowledged, matching containment
        // transition may release its exclusion; other kinds keep their handler.
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let confirmed = inner
                .engram_session_obligations
                .entries
                .iter()
                .filter(|entry| {
                    attempts
                        .iter()
                        .any(|attempt| attempt.obligation_id == entry.id)
                        && engram_containment_confirmation_materialized(&inner, entry)
                })
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>();
            inner.engram_confirmed_containment_ids.extend(confirmed);
            self.commit_locked(&mut inner).map_err(|_| {
                ApiError::internal(
                    "containment is durable but its updated recovery view could not be published",
                )
            })?;
        }
        self.release_engram_project_reset_fence(project_id, generation, &session_ids);
        reset_timer.released();
        Ok(self.snapshot())
    }
}
