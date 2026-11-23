// Runtime-only selection of an acknowledged name for the next turn. Naming
// owns validation and acknowledgement; dispatch owns the opening and commit.
// Restore keeps its existing work-binding rule and restores no selection.

#[derive(Clone)]
pub(crate) struct EngramPendingNamingSelection {
    pub(crate) store: EngramAuthorityStoreKey,
    pub(crate) connection: EngramConnectionConfig,
    pub(crate) previous: Option<EngramControlWorkBinding>,
    pub(crate) previous_root: Option<EngramNamedRootState>,
    pub(crate) binding: EngramControlWorkBinding,
    pub(crate) root: EngramNamedRootState,
    pub(crate) named_in_generation: u64,
    pub(crate) named_turn_ref: Option<String>,
    pub(crate) predecessors: Vec<EngramNamingPredecessor>,
    pub(crate) applied_in_generation: Option<u64>,
}

// A fixed acknowledgement cut, never a generation-based enrollment rule.
// The scope is retained only for live adopted reporting, not new measurement.
#[derive(Clone)]
pub(crate) struct EngramNamingPredecessor {
    pub(crate) turn_ref: String,
    pub(crate) scope: Option<EngramRuntimeClosingScope>,
    pub(crate) adoptable: bool,
    pub(crate) capture_durable: bool,
    pub(crate) terminal_no_report: Option<String>,
}

fn naming_report_has_scope(
    report: &EngramRuntimeObservationReport,
    store: &EngramAuthorityStoreKey,
    connection: &EngramConnectionConfig,
    binding: &EngramControlWorkBinding,
) -> bool {
    report.store == *store
        && report.connection == *connection
        && !report.envelopes.is_empty()
        && report
            .envelopes
            .iter()
            .all(|envelope| envelope.request.get("binding") == Some(&json!(binding)))
}

fn engram_naming_has_pending_scope(
    record: &SessionRecord,
    store: &EngramAuthorityStoreKey,
    connection: &EngramConnectionConfig,
) -> bool {
    let Some(binding) = record.engram.work_binding.as_ref() else {
        return false;
    };
    record.engram.unadmitted_closing_jobs.iter().any(|job| {
        job.terminal_failure.is_none()
            && job.scope.binding.as_ref() == Some(binding)
            && job.scope.store == *store
            && job.scope.connection == *connection
    }) || record.engram.unadmitted_reports.iter().any(|report| {
        !report.is_settled()
            && !record
                .engram
                .durable_runtime_captures
                .contains(&report.host_turn_ref)
            && naming_report_has_scope(report, store, connection, binding)
    })
}

#[derive(Clone, PartialEq, Eq)]
struct EngramNamingCustody {
    generation: u64,
    turn_ref: Option<String>,
    closed_turn_ref: Option<String>,
    binding: Option<EngramControlWorkBinding>,
}

impl EngramNamingCustody {
    fn capture(record: &SessionRecord) -> Self {
        Self {
            generation: record.active_turn_generation,
            turn_ref: record
                .engram
                .unadmitted_active_turn
                .as_ref()
                .map(|trace| trace.host_turn_ref.clone()),
            closed_turn_ref: record.engram.runtime_closed_turn_ref.clone(),
            binding: record.engram.work_binding.clone(),
        }
    }
}

fn named_selection_root_locked(
    inner: &StateInner,
    store: &EngramAuthorityStoreKey,
    binding: &EngramControlWorkBinding,
) -> EngramNamedRootState {
    engram_work_source_root_for_claim(
        &inner.engram_work_source_roots,
        store,
        &binding.work_id,
        &binding.claim_id,
    )
    .map_or(EngramNamedRootState::None, |root| {
        EngramNamedRootState::Bound {
            workspace_id: root.root.clone(),
            generation: root.generation as i64,
            named_at: root.named_at.clone(),
        }
    })
}

fn stage_engram_named_root_selection_locked(
    inner: &mut StateInner,
    index: usize,
    store: &EngramAuthorityStoreKey,
    connection: &EngramConnectionConfig,
    binding: &EngramControlWorkBinding,
    naming: &EngramNamingCustody,
    durable_row: Option<&Value>,
) -> Result<(), ApiError> {
    let root = named_selection_root_locked(inner, store, binding);
    let record = inner
        .session_mut_by_index(index)
        .expect("naming session exists");
    // An already-selected claim needs no deferred rewrite. In particular,
    // naming it again cancels an older choice without repairing any root
    // authority that may be lost before the next opening.
    if record.engram.work_binding.as_ref() == Some(binding) {
        record.engram.pending_naming_selection = None;
        return Ok(());
    }
    // A later successful explicit name supersedes an earlier pending choice,
    // including naming the running claim again to keep it selected.
    // Re-read custody at acknowledgement. A turn can start, finish, or be
    // replaced during naming's off-lock reads. Only the actual predecessor
    // can hold the next opening; a generation is never a turn identity.
    let predecessor = record
        .engram
        .unadmitted_active_turn
        .as_ref()
        .map(|trace| trace.host_turn_ref.clone())
        .or_else(|| {
            record
                .engram
                .runtime_closed_turn_ref
                .as_ref()
                .filter(|turn| {
                    naming.closed_turn_ref.as_ref() != Some(*turn)
                        || naming.turn_ref.as_ref() == Some(*turn)
                        || engram_turn_capture_pending(record, turn)
                })
                .cloned()
        });
    let mut predecessors = Vec::new();
    if let Some(previous) = record.engram.work_binding.as_ref() {
        for job in &record.engram.unadmitted_closing_jobs {
            if job.terminal_failure.is_none()
                && job.scope.binding.as_ref() == Some(previous)
                && job.scope.store == *store
                && job.scope.connection == *connection
                && naming_job_has_durable_scope(durable_row, job)?
            {
                predecessors.push(EngramNamingPredecessor {
                    turn_ref: job.trace.host_turn_ref.clone(),
                    scope: Some(job.scope.clone()),
                    // Restored jobs have no process-local opening image. They
                    // wait through their existing owner and are never marked.
                    adoptable: job.trace.opening_scope.as_ref() == Some(&job.scope),
                    capture_durable: false,
                    terminal_no_report: None,
                });
            }
        }
        for report in &record.engram.unadmitted_reports {
            if !report.is_settled()
                && naming_report_has_scope(report, store, connection, previous)
                && !predecessors
                    .iter()
                    .any(|member| member.turn_ref == report.host_turn_ref)
                && runtime_observation_row_contains(
                    durable_row,
                    "engramUnadmittedReports",
                    &serde_json::to_value(report).map_err(|error| {
                        ApiError::internal(format!("encoding naming report: {error}"))
                    })?,
                )
            {
                predecessors.push(EngramNamingPredecessor {
                    turn_ref: report.host_turn_ref.clone(),
                    scope: None,
                    adoptable: false,
                    capture_durable: record
                        .engram
                        .durable_runtime_captures
                        .contains(&report.host_turn_ref),
                    terminal_no_report: None,
                });
            }
        }
    }
    if let Some(turn) = predecessor.as_ref()
        && !predecessors.iter().any(|member| &member.turn_ref == turn)
    {
        let opening = record
            .engram
            .unadmitted_active_turn
            .as_ref()
            .filter(|trace| &trace.host_turn_ref == turn)
            .and_then(|trace| trace.opening_scope.clone())
            .or_else(|| {
                record
                    .engram
                    .unadmitted_closing_jobs
                    .iter()
                    .find(|job| {
                        &job.trace.host_turn_ref == turn
                            && job.terminal_failure.is_none()
                            && job.scope.binding == record.engram.work_binding
                            && job.scope.store == *store
                            && job.scope.connection == *connection
                            && job.trace.opening_scope.as_ref() == Some(&job.scope)
                    })
                    .and_then(|job| job.trace.opening_scope.clone())
            });
        // The exact actual naming turn is tracked separately from older
        // durable jobs, even when it closes during the acknowledgement.
        predecessors.push(EngramNamingPredecessor {
            turn_ref: turn.clone(),
            adoptable: opening.is_some(),
            scope: opening,
            capture_durable: record.engram.durable_runtime_captures.contains(turn),
            terminal_no_report: None,
        });
    }
    record.engram.pending_naming_selection = Some(EngramPendingNamingSelection {
        store: store.clone(),
        connection: connection.clone(),
        previous: record.engram.work_binding.clone(),
        previous_root: record.engram.named_root.clone(),
        binding: binding.clone(),
        root,
        named_in_generation: record.active_turn_generation,
        named_turn_ref: predecessor,
        predecessors,
        applied_in_generation: None,
    });
    settle_engram_named_predecessor_locked(inner, index, None);
    Ok(())
}

fn naming_job_has_durable_scope(
    row: Option<&Value>,
    job: &EngramRuntimeClosingJob,
) -> Result<bool, ApiError> {
    let scope = serde_json::to_value(&job.scope).map_err(|error| {
        ApiError::internal(format!("encoding naming predecessor scope: {error}"))
    })?;
    // Membership proves this exact turn's A scope, not its retry progress.
    // A queued policy/attempt update cannot erase already-durable custody.
    // Capture itself still requires the worker's full exact-job readback.
    Ok(row
        .and_then(|row| row.get("engramUnadmittedClosingJobs"))
        .and_then(Value::as_array)
        .is_some_and(|jobs| {
            jobs.iter().any(|saved| {
                saved
                    .get("trace")
                    .and_then(|trace| trace.get("host_turn_ref"))
                    .and_then(Value::as_str)
                    == Some(job.trace.host_turn_ref.as_str())
                    && saved.get("scope") == Some(&scope)
                    && saved.get("terminal_failure").is_none_or(Value::is_null)
            })
        }))
}

impl AppState {
    // Naming has acknowledged its root image, but that does not acknowledge
    // the session's closing jobs. Freeze this cut, read its row off-lock, then
    // compare custody before staging. A later save cannot enroll another job.
    fn stage_engram_named_root_selection<'a>(
        &'a self,
        mut inner: StateMutexGuard<'a, StateInner>,
        session: &str,
        store: &EngramAuthorityStoreKey,
        connection: &EngramConnectionConfig,
        binding: &EngramControlWorkBinding,
        naming: &EngramNamingCustody,
    ) -> Result<StateMutexGuard<'a, StateInner>, ApiError> {
        let index = inner.find_session_index(session).ok_or_else(|| {
            ApiError::conflict("naming session disappeared before durable predecessor readback")
        })?;
        if inner.sessions[index].engram.work_binding.as_ref() == Some(binding) {
            // Keeping the selected claim creates no predecessor set and needs
            // no observation-row read. Preserve ordinary same-claim naming.
            stage_engram_named_root_selection_locked(
                &mut inner, index, store, connection, binding, naming, None,
            )?;
            return Ok(inner);
        }
        let record = &inner.sessions[index];
        let custody = EngramNamingCustody::capture(record);
        let stamp = record.mutation_stamp;
        let workdir = record.session.workdir.clone();
        let previous_root = record.engram.named_root.clone();
        let root = named_selection_root_locked(&inner, store, binding);
        let target = Self::engram_source_root_target_locked(&inner, session)
            .map_err(ApiError::conflict)?
            .ok_or_else(|| {
                ApiError::conflict("naming session lost its work-service association")
            })?;
        drop(inner);
        let durable = runtime_observation_session_row(&self.persistence_path, session)
            .map_err(|error| ApiError::internal(format!(
                "reading durable naming predecessors failed: {}. Retry the naming request after the session store is readable", error.message)))?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(session).ok_or_else(|| {
            ApiError::conflict("naming session disappeared during durable predecessor readback")
        })?;
        let record = &inner.sessions[index];
        let current = record.mutation_stamp == stamp
            && EngramNamingCustody::capture(record) == custody
            && record.engram.named_root == previous_root
            && record.session.workdir == workdir
            && named_selection_root_locked(&inner, store, binding) == root
            && !engram_authority_work_unresolved(&inner, store, &binding.work_id)
            && Self::engram_source_root_target_locked(&inner, session)
                .map_err(ApiError::conflict)?
                .is_some_and(|now| {
                    now.connection == target.connection
                        && now.settings.same_work_service_settings(&target.settings)
                        && now.settings.authority_store_key.as_ref() == Some(store)
                        && now.connection == *connection
                        && now.runtime_snapshot == target.runtime_snapshot
                        && now.project_id == target.project_id
                        && now.project_reset_owner_generation
                            == target.project_reset_owner_generation
                        && now.external_ref == target.external_ref
                        && now.work_binding == target.work_binding
                });
        if !current {
            return Err(ApiError::conflict(
                "naming predecessor custody changed during durable session-row readback; retry naming to acknowledge the current turn and source root",
            ));
        }
        stage_engram_named_root_selection_locked(
            &mut inner,
            index,
            store,
            connection,
            binding,
            naming,
            durable.as_ref(),
        )?;
        Ok(inner)
    }
}

pub(crate) fn apply_engram_named_root_selection_locked(
    inner: &mut StateInner,
    index: usize,
    generation: u64,
    adopted: bool,
) {
    settle_engram_named_predecessor_locked(inner, index, None);
    let record = &inner.sessions[index];
    let Some(selection) = record.engram.pending_naming_selection.clone() else {
        return;
    };
    if selection.applied_in_generation.is_some()
        || generation == selection.named_in_generation
        || record.engram.unadmitted_active_turn.is_some()
        || (!adopted && engram_named_selection_capture_pending(record))
    {
        return;
    }
    let current = AppState::engram_source_root_target_locked(inner, &record.session.id)
        .ok()
        .flatten()
        .is_some_and(|target| {
            target.connection == selection.connection
                && target.settings.authority_store_key.as_ref() == Some(&selection.store)
                && target.work_binding == selection.previous
                && record.engram.named_root == selection.previous_root
        })
        && !engram_authority_work_unresolved(inner, &selection.store, &selection.binding.work_id)
        && named_selection_root_locked(inner, &selection.store, &selection.binding)
            == selection.root;
    if !current {
        inner
            .session_mut_by_index(index)
            .expect("opening session exists")
            .engram
            .pending_naming_selection = None;
        return;
    }
    if adopted {
        // The validated application and marking share this lock; a discarded
        // name must not degrade a predecessor that remains under A.
        mark_engram_adopted_naming_predecessor_locked(inner, index);
    }
    let record = inner
        .session_mut_by_index(index)
        .expect("opening session exists");
    record.engram.work_binding = Some(selection.binding);
    record.engram.named_root = Some(selection.root);
    record
        .engram
        .pending_naming_selection
        .as_mut()
        .expect("pending selection")
        .applied_in_generation = Some(generation);
}

pub(crate) fn finish_engram_named_root_selection_locked(inner: &mut StateInner, index: usize) {
    let record = &inner.sessions[index];
    if record
        .engram
        .pending_naming_selection
        .as_ref()
        .is_some_and(|selection| selection.applied_in_generation.is_some())
    {
        // Called only after the opening's existing row commit succeeds.
        // This post-commit cleanup changes no persisted or public field and
        // must not replace the committed message delta's mutation stamp.
        inner.sessions[index].engram.pending_naming_selection = None;
    }
}

// Successful capture waits for exact report readback. A terminal no-report
// outcome instead releases the hold with a diagnostic, never with credit.
pub(crate) fn engram_turn_capture_pending(record: &SessionRecord, turn: &str) -> bool {
    if record
        .engram
        .unadmitted_active_turn
        .as_ref()
        .is_some_and(|trace| trace.host_turn_ref == turn)
    {
        return true;
    }
    if let Some(job) = record
        .engram
        .unadmitted_closing_jobs
        .iter()
        .find(|job| job.trace.host_turn_ref == turn)
    {
        return job.terminal_failure.is_none();
    }
    record.engram.unadmitted_reports.iter().any(|report| {
        report.host_turn_ref == turn
            && !report.is_settled()
            && !record.engram.durable_runtime_captures.contains(turn)
    })
}

fn engram_named_member_no_report(
    record: &SessionRecord,
    member: &EngramNamingPredecessor,
) -> Option<&'static str> {
    if member.capture_durable || member.terminal_no_report.is_some() {
        return None;
    }
    if record
        .engram
        .unadmitted_active_turn
        .as_ref()
        .is_some_and(|trace| trace.host_turn_ref == member.turn_ref)
    {
        return None;
    }
    if let Some(job) = record
        .engram
        .unadmitted_closing_jobs
        .iter()
        .find(|job| job.trace.host_turn_ref == member.turn_ref)
    {
        return job
            .terminal_failure
            .as_ref()
            .map(|_| "closing capture ended without a report");
    }
    if record
        .engram
        .unadmitted_reports
        .iter()
        .any(|report| report.host_turn_ref == member.turn_ref)
    {
        return None;
    }
    Some("closing capture ended without a retained report or job")
}

pub(crate) fn settle_engram_named_predecessor_locked(
    inner: &mut StateInner,
    index: usize,
    reason: Option<&str>,
) -> bool {
    let record = &inner.sessions[index];
    let Some(selection) = record.engram.pending_naming_selection.as_ref() else {
        return false;
    };
    if selection.applied_in_generation.is_some() {
        return false;
    }
    let failures: Vec<_> = selection
        .predecessors
        .iter()
        .filter_map(|member| {
            engram_named_member_no_report(record, member).map(|detected| {
                let reason = if selection.named_turn_ref.as_ref() == Some(&member.turn_ref) {
                    reason.unwrap_or(detected)
                } else {
                    detected
                };
                (member.turn_ref.clone(), reason.to_owned())
            })
        })
        .collect();
    let mut changed = false;
    for (turn, reason) in failures {
        changed |=
            note_engram_named_predecessor_without_report_locked(inner, index, &turn, &reason);
    }
    changed
}

fn note_engram_named_predecessor_without_report_locked(
    inner: &mut StateInner,
    index: usize,
    turn: &str,
    reason: &str,
) -> bool {
    let record = &mut inner.sessions[index];
    let Some(selection) = record.engram.pending_naming_selection.as_mut() else {
        return false;
    };
    if selection.applied_in_generation.is_some() {
        return false;
    }
    let Some(member) = selection
        .predecessors
        .iter_mut()
        .find(|member| member.turn_ref == turn)
    else {
        return false;
    };
    if member.capture_durable || member.terminal_no_report.is_some() {
        return false;
    }
    member.terminal_no_report = Some(reason.to_owned());
    let diagnostic = format!(
        "Runtime-started turn {turn}: no predecessor report: {reason}. Its checks remain uncredited."
    );
    if let Some(job) = record
        .engram
        .unadmitted_closing_jobs
        .iter_mut()
        .find(|job| job.trace.host_turn_ref == turn)
    {
        job.last_error = Some(diagnostic.clone());
    }
    let message = Message::Text {
        attachments: Vec::new(),
        id: inner.next_message_id(),
        timestamp: stamp_now(),
        author: Author::System,
        text: diagnostic,
        expanded_text: None,
        source: None,
    };
    push_message_on_record(&mut inner.sessions[index], message);
    true
}

pub(crate) fn engram_named_selection_capture_pending(record: &SessionRecord) -> bool {
    record
        .engram
        .pending_naming_selection
        .as_ref()
        .is_some_and(|selection| {
            selection.applied_in_generation.is_none()
                && selection.predecessors.iter().any(|member| {
                    !member.capture_durable
                        && member.terminal_no_report.is_none()
                        && !record.engram.unadmitted_reports.iter().any(|report| {
                            report.host_turn_ref == member.turn_ref && report.is_settled()
                        })
                        && engram_named_member_no_report(record, member).is_none()
                })
        })
}

pub(crate) const ENGRAM_ADOPTED_PREDECESSOR_REASON: &str =
    "a successor adopted turn opened before the predecessor capture settled";

fn mark_engram_adopted_naming_predecessor_locked(inner: &mut StateInner, index: usize) {
    let record = &mut inner.sessions[index];
    let Some(selection) = record.engram.pending_naming_selection.as_ref() else {
        return;
    };
    if selection.applied_in_generation.is_some() {
        return;
    }
    for job in &mut record.engram.unadmitted_closing_jobs {
        let Some(member) = selection
            .predecessors
            .iter()
            .find(|member| member.turn_ref == job.trace.host_turn_ref)
        else {
            continue;
        };
        if member.adoptable
            && !member.capture_durable
            && member.terminal_no_report.is_none()
            && job.terminal_failure.is_none()
            && member.scope.as_ref() == Some(&job.scope)
            && job.trace.opening_scope.as_ref() == Some(&job.scope)
            && job.scope.binding == selection.previous
            && job.scope.store == selection.store
            && job.scope.connection == selection.connection
        {
            // This marks only live jobs present at the acknowledgement cut.
            // Restored jobs wait through their old owner without this marker.
            job.trace.source_attribution_uncertain = true;
            job.last_error = Some(ENGRAM_ADOPTED_PREDECESSOR_REASON.to_owned());
            job.scope.historical_fields.insert(
                "adopted_successor_reason".to_owned(),
                Value::String(ENGRAM_ADOPTED_PREDECESSOR_REASON.to_owned()),
            );
        }
    }
}
