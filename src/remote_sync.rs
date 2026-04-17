// Remote state synchronization — applies inbound remote snapshots + delta
// events from a proxied backend into the local state, performing ID
// localization so remote `project_id` / `session_id` / orchestrator ids get
// remapped to their local proxy equivalents.
//
// Covers: snapshot sync (`sync_remote_state_inner`,
// `apply_remote_state_if_newer_locked`), per-session proxy record upsert
// (`apply_remote_session_to_record`, `upsert_remote_proxy_session_record`,
// `ensure_remote_proxy_session_record`), remote→local ID localization
// (`localize_remote_session`, `remote_project_id_map`,
// `local_project_id_for_remote_project`, `local_session_id_for_remote_session`),
// orchestrator mirroring (`localize_remote_orchestrator_instance`,
// `ensure_remote_orchestrator_instance`, `sync_remote_orchestrators_inner`),
// and the event-stream fan-out (`process_remote_event_stream`,
// `dispatch_remote_event`, `resync_remote_state_snapshot`).
//
// Extracted from remote.rs into its own `include!()` fragment so remote.rs
// stays focused on SSH + HTTP transport and terminal stream forwarding.

/// Returns the originating remote revision for a delta event.
fn delta_event_revision(event: &DeltaEvent) -> u64 {
    match event {
        DeltaEvent::SessionCreated { revision, .. }
        | DeltaEvent::MessageCreated { revision, .. }
        | DeltaEvent::TextDelta { revision, .. }
        | DeltaEvent::TextReplace { revision, .. }
        | DeltaEvent::CommandUpdate { revision, .. }
        | DeltaEvent::ParallelAgentsUpdate { revision, .. }
        | DeltaEvent::OrchestratorsUpdated { revision, .. } => *revision,
    }
}

struct RemoteSyncRollback {
    next_session_number: usize,
    sessions: Vec<SessionRecord>,
    orchestrator_instances: Vec<OrchestratorInstance>,
    removed_session_ids: Vec<String>,
}

impl RemoteSyncRollback {
    fn capture(inner: &StateInner) -> Self {
        Self {
            next_session_number: inner.next_session_number,
            sessions: inner.sessions.clone(),
            orchestrator_instances: inner.orchestrator_instances.clone(),
            removed_session_ids: inner.removed_session_ids.clone(),
        }
    }

    fn restore(self, inner: &mut StateInner) {
        inner.next_session_number = self.next_session_number;
        inner.sessions = self.sessions;
        inner.orchestrator_instances = self.orchestrator_instances;
        inner.removed_session_ids = self.removed_session_ids;
    }
}

/// Folds an inbound remote `StateResponse` into local state.
///
/// This is the single under-lock entry point for snapshot-shaped
/// updates from a remote backend. It runs two kinds of sync depending
/// on `focus_remote_session_id`:
///
/// - **Broad snapshot sync** (`focus = None`): the remote's entire
///   state replaces the mirrored projection for this remote id. The
///   function first captures a rollback shape of sessions,
///   orchestrators, session numbering, and queued session-delete
///   tombstones so a mid-apply failure can restore the pre-sync view;
///   then it upserts every remote project, every
///   remote session (via `localize_remote_session` +
///   `upsert_remote_proxy_session_record`), and every remote
///   orchestrator instance, each translated from remote ids to their
///   local proxy ids. Sessions that exist locally but no longer
///   appear in the remote snapshot are tombstoned via
///   `record_removed_session` so the delta persist path deletes them.
/// - **Focused sync** (`focus = Some(remote_session_id)`): only the
///   single session is updated. No orchestrator state is touched and
///   no tombstones are issued — other local sessions for this remote
///   stay as-is even if they were dropped from this response.
///
/// **Revision gate.** This function does not check revisions itself;
/// callers should first test via
/// `StateInner::should_skip_remote_applied_revision`
/// (see [`apply_remote_state_if_newer_locked`]) so stale responses
/// from out-of-order delivery don't clobber a newer mirrored state.
/// The one exception is the focused path where the caller has
/// already chosen to force an apply (e.g. after a 404 from a
/// targeted fetch).
///
/// **ID localization.** Every `remote_*_id` on the wire is remapped
/// to the local proxy id via `remote_project_id_map` / `localize_*`
/// before it lands on any `SessionRecord`, `OrchestratorInstance`,
/// or `Project`, so downstream code never has to disambiguate "whose
/// id is this".
///
/// **Mutation stamps.** Every updated session lands through
/// `session_mut_by_index`, so the delta-persist path picks up the
/// remote-sourced changes the same way a local mutation would.
///
/// **Called from:** the SSE bridge (`process_remote_event_stream`),
/// the periodic resync path (`resync_remote_state_snapshot`), the
/// remote-proxy session-creation helpers in
/// `remote_create_proxies.rs`, and
/// `sync_remote_state_for_target` in `remote_routes.rs`.
///
/// Broad-sync with rollback:
///
/// ```mermaid
/// flowchart TD
///   Start([sync_remote_state_inner]) --> Focus{focus_remote_session_id?}
///   Focus -- Some --> FocusedSync[update one session;<br/>no orchestrators, no tombstones]
///   FocusedSync --> End([return])
///   Focus -- None --> Capture[RemoteSyncRollback::capture<br/>sessions + orchestrators +<br/>next_session_number + removed_session_ids]
///   Capture --> Projects[upsert remote projects]
///   Projects --> Sessions[upsert remote sessions<br/>via localize_remote_session]
///   Sessions --> Orchestrators[upsert remote orchestrator instances]
///   Orchestrators --> LocalizeError{localization failure?}
///   LocalizeError -- yes --> Restore[rollback.restore:<br/>restore all captured fields]
///   Restore --> End
///   LocalizeError -- no --> Retain[retain_sessions: drop local proxies<br/>not in snapshot, queue tombstones]
///   Retain --> NoteRevision[note_remote_applied_revision]
///   NoteRevision --> End
/// ```
fn sync_remote_state_inner(
    inner: &mut StateInner,
    remote_id: &str,
    remote_state: &StateResponse,
    focus_remote_session_id: Option<&str>,
) {
    // Mutation contract — this function mutates only these fields of
    // `StateInner`: `sessions` (via retain_sessions /
    // upsert_remote_proxy_session_record / session_mut_by_index),
    // `orchestrator_instances` (via localize_remote_orchestrator_instance),
    // `next_session_number` (via push_session), `removed_session_ids` (via
    // retain_sessions → record_removed_session), and `last_mutation_stamp`
    // (side-effect of session_mut_by_index / push_session). If a future
    // change adds mutation of any other `StateInner` field here (e.g.
    // `projects`, `remote_applied_revisions`), extend `RemoteSyncRollback`
    // to capture that field too or the rollback below will silently
    // restore a partial view.
    //
    // Broad-sync path captures rollback BEFORE `retain_sessions` runs so
    // the tombstones it queues can be unwound on failure.
    let pre_retain_rollback_state =
        focus_remote_session_id.is_none().then(|| RemoteSyncRollback::capture(inner));

    // Inline-build the remote→local project id map with the typed
    // keys/values so it plugs into `localize_remote_*` helpers
    // without a conversion step. Kept inline (rather than reusing
    // `remote_project_id_map`) because this hot-path sync runs under
    // the state mutex and already has `&inner.projects` in scope.
    let mut local_project_ids_by_remote_project_id: HashMap<RemoteProjectId, LocalProjectId> =
        HashMap::new();
    for project in &inner.projects {
        if project.remote_id == remote_id {
            if let Some(remote_project_id) = project.remote_project_id.as_deref() {
                local_project_ids_by_remote_project_id.insert(
                    RemoteProjectId::from(remote_project_id),
                    LocalProjectId::from(project.id.as_str()),
                );
            }
        }
    }

    if focus_remote_session_id.is_none() {
        let live_remote_session_ids = remote_state
            .sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect::<HashSet<_>>();
        inner.retain_sessions(|record| {
            if record.remote_id.as_deref() != Some(remote_id) {
                return true;
            }
            let Some(remote_session_id) = record.remote_session_id.as_deref() else {
                return true;
            };
            live_remote_session_ids.contains(remote_session_id)
        });
    }

    let remote_sessions_by_id = remote_state
        .sessions
        .iter()
        .map(|session| (session.id.as_str(), session))
        .collect::<HashMap<_, _>>();

    // Two-phase update: first scan immutably to collect `(index,
    // remote_session_id, local_project_id)` tuples, then mutate via
    // `session_mut_by_index` so each changed record gets a fresh
    // `mutation_stamp`. Iterating `&mut inner.sessions` directly would
    // skip stamping, and the new SQLite delta persist would then drop
    // these remote-proxy updates entirely.
    let updates: Vec<(usize, String, Option<LocalProjectId>)> = inner
        .sessions
        .iter()
        .enumerate()
        .filter_map(|(idx, record)| {
            if record.remote_id.as_deref() != Some(remote_id) {
                return None;
            }
            let remote_session_id = record.remote_session_id.as_deref()?;
            if focus_remote_session_id.is_some_and(|focus| focus != remote_session_id) {
                return None;
            }
            let remote_session = remote_sessions_by_id.get(remote_session_id)?;
            let local_project_id = remote_session
                .project_id
                .as_deref()
                .and_then(|remote_project_id| {
                    local_project_ids_by_remote_project_id
                        .get(remote_project_id)
                        .cloned()
                })
                .or_else(|| record.session.project_id.as_deref().map(LocalProjectId::from));
            Some((idx, remote_session_id.to_string(), local_project_id))
        })
        .collect();

    for (idx, remote_session_id, local_project_id) in updates {
        let Some(remote_session) = remote_sessions_by_id.get(remote_session_id.as_str()) else {
            continue;
        };
        let Some(record) = inner.session_mut_by_index(idx) else {
            continue;
        };
        apply_remote_session_to_record(
            record,
            local_project_id.map(LocalProjectId::into_inner),
            remote_session,
        );
    }

    let rollback_state = if focus_remote_session_id.is_some() {
        Some(RemoteSyncRollback::capture(inner))
    } else {
        pre_retain_rollback_state
    };

    if let Err(err) = sync_remote_orchestrators_inner(
        inner,
        remote_id,
        &remote_state.orchestrators,
        &local_project_ids_by_remote_project_id,
        Some(&remote_sessions_by_id),
    ) {
        if let Some(rollback_state) = rollback_state {
            rollback_state.restore(inner);
        }
        eprintln!(
            "remote state warning> failed to sync remote orchestrators for `{remote_id}` at revision {}: {err:#}",
            remote_state.revision
        );
    }
}

/// Revision-gated wrapper around [`sync_remote_state_inner`]. Returns
/// `true` if the snapshot was applied, `false` if skipped as stale.
///
/// Checks the stored `applied_remote_revision` for the remote via
/// `StateInner::should_skip_remote_applied_revision`. Remote events
/// can arrive out of order (especially after a reconnect where the
/// full snapshot races the buffered event stream), so any revision
/// not strictly greater than what was already applied for this remote
/// is dropped. Broad-snapshot callers should always use this; focused
/// per-session applies from the forced-resync path
/// (`resync_remote_state_snapshot` → 404 retry) bypass the gate and
/// call `sync_remote_state_inner` directly.
fn apply_remote_state_if_newer_locked(
    inner: &mut StateInner,
    remote_id: &str,
    remote_state: &StateResponse,
    focus_remote_session_id: Option<&str>,
) -> bool {
    if inner.should_skip_remote_applied_revision(remote_id, remote_state.revision) {
        return false;
    }
    sync_remote_state_inner(inner, remote_id, remote_state, focus_remote_session_id);
    true
}

/// Applies remote session to record.
fn apply_remote_session_to_record(
    record: &mut SessionRecord,
    local_project_id: Option<String>,
    remote_session: &Session,
) {
    let local_session_id = record.session.id.clone();
    record.session = localize_remote_session(&local_session_id, local_project_id, remote_session);
    record.external_session_id = record.session.external_session_id.clone();
    sync_codex_thread_state(record);
    record.codex_approval_policy = record
        .session
        .approval_policy
        .unwrap_or_else(default_codex_approval_policy);
    record.codex_reasoning_effort = record
        .session
        .reasoning_effort
        .unwrap_or_else(default_codex_reasoning_effort);
    record.codex_sandbox_mode = record
        .session
        .sandbox_mode
        .unwrap_or_else(default_codex_sandbox_mode);
    record.runtime = SessionRuntime::None;
    record.runtime_reset_required = false;
    clear_all_pending_requests(record);
    record.message_positions = build_message_positions(&record.session.messages);
}

/// Upserts remote proxy session record.
fn upsert_remote_proxy_session_record(
    inner: &mut StateInner,
    remote_id: &str,
    remote_session: &Session,
    local_project_id: Option<String>,
) -> String {
    if let Some(index) = inner.find_remote_session_index(remote_id, &remote_session.id) {
        apply_remote_session_to_record(
            inner
        .session_mut_by_index(index)
        .expect("session index should be valid"),
            local_project_id,
            remote_session,
        );
        return inner.sessions[index].session.id.clone();
    }

    let number = inner.next_session_number;
    inner.next_session_number += 1;
    let local_session_id = format!("session-{number}");
    let session = localize_remote_session(&local_session_id, local_project_id, remote_session);
    let mut record = SessionRecord {
        active_codex_approval_policy: None,
        active_codex_reasoning_effort: None,
        active_codex_sandbox_mode: None,
        active_turn_start_message_count: None,
        active_turn_file_changes: BTreeMap::new(),
        active_turn_file_change_grace_deadline: None,
        agent_commands: Vec::new(),
        codex_approval_policy: session
            .approval_policy
            .unwrap_or_else(default_codex_approval_policy),
        codex_reasoning_effort: session
            .reasoning_effort
            .unwrap_or_else(default_codex_reasoning_effort),
        codex_sandbox_mode: session
            .sandbox_mode
            .unwrap_or_else(default_codex_sandbox_mode),
        external_session_id: session.external_session_id.clone(),
        pending_claude_approvals: HashMap::new(),
        pending_codex_approvals: HashMap::new(),
        pending_codex_user_inputs: HashMap::new(),
        pending_codex_mcp_elicitations: HashMap::new(),
        pending_codex_app_requests: HashMap::new(),
        pending_acp_approvals: HashMap::new(),
        queued_prompts: VecDeque::new(),
        message_positions: build_message_positions(&session.messages),
        remote_id: Some(remote_id.to_owned()),
        remote_session_id: Some(remote_session.id.clone()),
        runtime: SessionRuntime::None,
        runtime_reset_required: false,
        orchestrator_auto_dispatch_blocked: false,
        runtime_stop_in_progress: false,
        deferred_stop_callbacks: Vec::new(),
        hidden: false,
        // Freshly created records start unstamped; subsequent edits
        // flow through `session_mut*` which stamps them on access.
        mutation_stamp: 0,
        session,
    };
    sync_codex_thread_state(&mut record);
    inner.push_session(record);
    local_session_id
}

/// Ensures a remote proxy session record exists locally, optionally refreshing
/// the existing record from the response payload.
fn ensure_remote_proxy_session_record(
    inner: &mut StateInner,
    remote_id: &str,
    remote_session: &Session,
    local_project_id: Option<String>,
    update_existing: bool,
) -> (String, bool) {
    if let Some(index) = inner.find_remote_session_index(remote_id, &remote_session.id) {
        let local_session_id = inner.sessions[index].session.id.clone();
        if update_existing {
            apply_remote_session_to_record(
                inner
        .session_mut_by_index(index)
        .expect("session index should be valid"),
                local_project_id,
                remote_session,
            );
            return (local_session_id, true);
        }
        return (local_session_id, false);
    }

    (
        upsert_remote_proxy_session_record(inner, remote_id, remote_session, local_project_id),
        true,
    )
}

fn localize_remote_session(
    local_session_id: &str,
    local_project_id: Option<String>,
    remote_session: &Session,
) -> Session {
    let mut session = remote_session.clone();
    session.id = local_session_id.to_owned();
    session.project_id = local_project_id;
    session
}

/// Builds the `RemoteProjectId -> LocalProjectId` map for one remote
/// host. The typed keys/values make id-kind mixups compile-detectable
/// at every caller that threads this map into localization helpers.
fn remote_project_id_map(
    inner: &StateInner,
    remote_id: &str,
) -> HashMap<RemoteProjectId, LocalProjectId> {
    let mut map = HashMap::new();
    for project in &inner.projects {
        if project.remote_id == remote_id {
            if let Some(remote_project_id) = project.remote_project_id.as_deref() {
                map.insert(
                    RemoteProjectId::from(remote_project_id),
                    LocalProjectId::from(project.id.as_str()),
                );
            }
        }
    }
    map
}

/// Returns the localized project id for a remote project. The raw
/// `&str` input comes from wire payloads (e.g.
/// `Session.project_id: Option<String>`); the map lookup itself
/// traffics in typed `RemoteProjectId` / `LocalProjectId` so a
/// downstream caller cannot accidentally pass the returned local id
/// where a remote id is expected.
fn local_project_id_for_remote_project(
    local_project_ids_by_remote_project_id: &HashMap<RemoteProjectId, LocalProjectId>,
    remote_project_id: Option<&str>,
) -> Option<LocalProjectId> {
    // `Borrow<str>` impl on `RemoteProjectId` (see `src/ids.rs`) lets
    // `HashMap::get` accept the raw `&str` directly without allocating a
    // temporary `RemoteProjectId` wrapper. Matters on the hot remote-sync
    // path where this helper runs under the state mutex for every session
    // + orchestrator instance + transition reference.
    remote_project_id.and_then(|project_id| {
        local_project_ids_by_remote_project_id
            .get(project_id)
            .cloned()
    })
}

/// Returns the local session id for a remote session, creating a proxy record
/// when a full snapshot provides the missing session payload.
///
/// The id arguments mix three identity spaces — `remote_session_id`
/// is now typed as `&RemoteSessionId` so callers have to explicitly
/// convert wire strings at the boundary (usually `RemoteSessionId::from(s)`),
/// preventing accidental reuse of a `LocalSessionId` or bare `&str`
/// for the wrong space. The returned `LocalSessionId` completes the
/// round-trip: the caller is guaranteed it has received a local-side
/// id and cannot feed it back into a remote-id slot without a
/// compile-visible `.into_inner()` / `.as_str()`.
fn local_session_id_for_remote_session(
    inner: &mut StateInner,
    remote_id: &str,
    remote_session_id: &RemoteSessionId,
    remote_sessions_by_id: Option<&HashMap<&str, &Session>>,
    local_project_ids_by_remote_project_id: &HashMap<RemoteProjectId, LocalProjectId>,
    fallback_local_project_id: Option<&str>,
) -> Option<LocalSessionId> {
    if let Some(index) = inner.find_remote_session_index(remote_id, remote_session_id.as_str()) {
        return Some(LocalSessionId::from(inner.sessions[index].session.id.clone()));
    }

    let remote_session = remote_sessions_by_id?.get(remote_session_id.as_str())?;
    let local_project_id = local_project_id_for_remote_project(
        local_project_ids_by_remote_project_id,
        remote_session.project_id.as_deref(),
    )
    .or_else(|| fallback_local_project_id.map(LocalProjectId::from));
    Some(LocalSessionId::from(upsert_remote_proxy_session_record(
        inner,
        remote_id,
        remote_session,
        local_project_id.map(LocalProjectId::into_inner),
    )))
}

/// Localizes a remote orchestrator instance. All the `local_*` ids
/// that emerge here (project, per-session, per-transition) flow as
/// typed `LocalProjectId` / `LocalSessionId` through the helpers
/// below, so `remote_orchestrator.id` / `session_id` / `project_id`
/// cannot be accidentally re-used in a local-id slot without a
/// compile error.
fn localize_remote_orchestrator_instance(
    inner: &mut StateInner,
    remote_id: &str,
    remote_orchestrator: &OrchestratorInstance,
    local_project_ids_by_remote_project_id: &HashMap<RemoteProjectId, LocalProjectId>,
    remote_sessions_by_id: Option<&HashMap<&str, &Session>>,
) -> Result<OrchestratorInstance, anyhow::Error> {
    // `delete_project` (`src/state.rs`) clears `OrchestratorInstance.project_id`
    // to `""` for any orchestrator bound to the deleted project rather than
    // removing the orchestrator outright. Treat that detached-after-delete
    // state as "no local project id" at capture time so the `.or(...)`
    // fallback below never re-materializes `""` as a valid local project id
    // and never persists it back into `template_snapshot.project_id` on the
    // next remote re-sync. This matches the `.trim().is_empty()` convention
    // already used for `remote_project_id` just below, and is why
    // `OrchestratorInstance.project_id` stays a `String` rather than an
    // `Option<String>`: the asymmetry with `Session.project_id` is contained
    // inside this one localization helper.
    let (existing_local_instance_id, existing_local_project_id) = inner
        .find_remote_orchestrator_index(remote_id, &remote_orchestrator.id)
        .and_then(|index| inner.orchestrator_instances.get(index))
        .map(|instance| {
            let project_id = Some(instance.project_id.clone())
                .filter(|id| !id.trim().is_empty())
                .map(LocalProjectId::from);
            (Some(instance.id.clone()), project_id)
        })
        .unwrap_or((None, None));
    let remote_project_id = Some(remote_orchestrator.project_id.as_str())
        .filter(|project_id| !project_id.trim().is_empty())
        .or_else(|| {
            remote_orchestrator
                .template_snapshot
                .project_id
                .as_deref()
                .filter(|project_id| !project_id.trim().is_empty())
        });
    let local_project_id = local_project_id_for_remote_project(
        local_project_ids_by_remote_project_id,
        remote_project_id,
    )
    .or(existing_local_project_id)
    .ok_or_else(|| {
        anyhow!(
            "no local project for remote project `{}`",
            remote_project_id.unwrap_or("unknown")
        )
    })?;

    let mut session_instances = Vec::with_capacity(remote_orchestrator.session_instances.len());
    for session_instance in &remote_orchestrator.session_instances {
        let remote_session_id = RemoteSessionId::from(session_instance.session_id.as_str());
        let local_session_id = local_session_id_for_remote_session(
            inner,
            remote_id,
            &remote_session_id,
            remote_sessions_by_id,
            local_project_ids_by_remote_project_id,
            Some(local_project_id.as_str()),
        )
        .ok_or_else(|| anyhow!("remote session `{}` not found", session_instance.session_id))?;
        session_instances.push(OrchestratorSessionInstance {
            session_id: local_session_id.into_inner(),
            ..session_instance.clone()
        });
    }

    let mut template_snapshot = remote_orchestrator.template_snapshot.clone();
    template_snapshot.project_id = Some(local_project_id.as_str().to_owned());

    let mut pending_transitions = Vec::with_capacity(remote_orchestrator.pending_transitions.len());
    for transition in &remote_orchestrator.pending_transitions {
        let remote_source_session_id =
            RemoteSessionId::from(transition.source_session_id.as_str());
        let source_session_id = local_session_id_for_remote_session(
            inner,
            remote_id,
            &remote_source_session_id,
            remote_sessions_by_id,
            local_project_ids_by_remote_project_id,
            Some(local_project_id.as_str()),
        )
        .ok_or_else(|| {
            anyhow!(
                "remote session `{}` not found",
                transition.source_session_id
            )
        })?;
        let remote_destination_session_id =
            RemoteSessionId::from(transition.destination_session_id.as_str());
        let destination_session_id = local_session_id_for_remote_session(
            inner,
            remote_id,
            &remote_destination_session_id,
            remote_sessions_by_id,
            local_project_ids_by_remote_project_id,
            Some(local_project_id.as_str()),
        )
        .ok_or_else(|| {
            anyhow!(
                "remote session `{}` not found",
                transition.destination_session_id
            )
        })?;
        pending_transitions.push(PendingTransition {
            source_session_id: source_session_id.into_inner(),
            destination_session_id: destination_session_id.into_inner(),
            ..transition.clone()
        });
    }

    let active_session_ids_during_stop = remote_orchestrator
        .active_session_ids_during_stop
        .as_ref()
        .map(|session_ids| {
            session_ids
                .iter()
                .map(|session_id| {
                    let remote_session_id = RemoteSessionId::from(session_id.as_str());
                    local_session_id_for_remote_session(
                        inner,
                        remote_id,
                        &remote_session_id,
                        remote_sessions_by_id,
                        local_project_ids_by_remote_project_id,
                        Some(local_project_id.as_str()),
                    )
                    .map(LocalSessionId::into_inner)
                    .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let stopped_session_ids_during_stop = remote_orchestrator
        .stopped_session_ids_during_stop
        .iter()
        .map(|session_id| {
            let remote_session_id = RemoteSessionId::from(session_id.as_str());
            local_session_id_for_remote_session(
                inner,
                remote_id,
                &remote_session_id,
                remote_sessions_by_id,
                local_project_ids_by_remote_project_id,
                Some(local_project_id.as_str()),
            )
            .map(LocalSessionId::into_inner)
            .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(OrchestratorInstance {
        id: existing_local_instance_id
            .unwrap_or_else(|| format!("orchestrator-instance-{}", Uuid::new_v4())),
        remote_id: Some(remote_id.to_owned()),
        remote_orchestrator_id: Some(remote_orchestrator.id.clone()),
        template_id: remote_orchestrator.template_id.clone(),
        project_id: local_project_id.into_inner(),
        template_snapshot,
        status: remote_orchestrator.status,
        session_instances,
        pending_transitions,
        created_at: remote_orchestrator.created_at.clone(),
        error_message: remote_orchestrator.error_message.clone(),
        completed_at: remote_orchestrator.completed_at.clone(),
        stop_in_progress: remote_orchestrator.stop_in_progress,
        active_session_ids_during_stop,
        stopped_session_ids_during_stop,
    })
}

fn ensure_remote_orchestrator_instance(
    inner: &mut StateInner,
    remote_id: &str,
    remote_orchestrator: &OrchestratorInstance,
    remote_sessions_by_id: Option<&HashMap<&str, &Session>>,
    update_existing: bool,
) -> Result<(OrchestratorInstance, bool), anyhow::Error> {
    if let Some(index) = inner.find_remote_orchestrator_index(remote_id, &remote_orchestrator.id)
    {
        if !update_existing {
            return Ok((inner.orchestrator_instances[index].clone(), false));
        }
    }

    let rollback_state = RemoteSyncRollback::capture(inner);
    let local_project_ids_by_remote_project_id = remote_project_id_map(inner, remote_id);
    let localized = match localize_remote_orchestrator_instance(
        inner,
        remote_id,
        remote_orchestrator,
        &local_project_ids_by_remote_project_id,
        remote_sessions_by_id,
    ) {
        Ok(localized) => localized,
        Err(err) => {
            rollback_state.restore(inner);
            return Err(err);
        }
    };

    if let Some(index) = inner.find_remote_orchestrator_index(remote_id, &remote_orchestrator.id) {
        inner.orchestrator_instances[index] = localized.clone();
    } else {
        inner.orchestrator_instances.push(localized.clone());
    }

    Ok((localized, true))
}

/// Syncs remote orchestrators. Threads the typed project-id map
/// through so `localize_remote_orchestrator_instance` keeps its
/// compile-time guarantees; the caller must build the map via
/// `remote_project_id_map` to inherit the typed keys/values.
fn sync_remote_orchestrators_inner(
    inner: &mut StateInner,
    remote_id: &str,
    remote_orchestrators: &[OrchestratorInstance],
    local_project_ids_by_remote_project_id: &HashMap<RemoteProjectId, LocalProjectId>,
    remote_sessions_by_id: Option<&HashMap<&str, &Session>>,
) -> Result<(), anyhow::Error> {
    let mut localized_by_remote_orchestrator_id = HashMap::new();
    let mut localized_remote_orchestrator_ids = Vec::with_capacity(remote_orchestrators.len());
    for remote_orchestrator in remote_orchestrators {
        let localized = localize_remote_orchestrator_instance(
            inner,
            remote_id,
            remote_orchestrator,
            local_project_ids_by_remote_project_id,
            remote_sessions_by_id,
        )?;
        let remote_orchestrator_id = localized
            .remote_orchestrator_id
            .clone()
            .ok_or_else(|| anyhow!("remote orchestrator id missing"))?;
        if !localized_by_remote_orchestrator_id.contains_key(&remote_orchestrator_id) {
            localized_remote_orchestrator_ids.push(remote_orchestrator_id.clone());
        }
        localized_by_remote_orchestrator_id.insert(remote_orchestrator_id, localized);
    }

    let mut next_orchestrator_instances = Vec::with_capacity(
        inner.orchestrator_instances.len() + localized_by_remote_orchestrator_id.len(),
    );
    for instance in inner.orchestrator_instances.drain(..) {
        if instance.remote_id.as_deref() != Some(remote_id) {
            next_orchestrator_instances.push(instance);
            continue;
        }
        let Some(remote_orchestrator_id) = instance.remote_orchestrator_id.as_deref() else {
            next_orchestrator_instances.push(instance);
            continue;
        };
        if let Some(localized) = localized_by_remote_orchestrator_id.remove(remote_orchestrator_id)
        {
            next_orchestrator_instances.push(localized);
        }
    }
    for remote_orchestrator_id in localized_remote_orchestrator_ids {
        if let Some(localized) = localized_by_remote_orchestrator_id.remove(&remote_orchestrator_id)
        {
            next_orchestrator_instances.push(localized);
        }
    }
    inner.orchestrator_instances = next_orchestrator_instances;

    Ok(())
}

/// Processes remote event stream.
fn process_remote_event_stream(
    state: &AppState,
    remote_id: &str,
    response: BlockingHttpResponse,
) -> Result<()> {
    let mut event_name = String::new();
    let mut data_lines = Vec::new();
    let reader = BufReader::new(response);
    for line in reader.lines() {
        let line =
            line.with_context(|| format!("failed to read SSE line for remote `{remote_id}`"))?;
        if line.is_empty() {
            dispatch_remote_event(state, remote_id, &event_name, &data_lines)?;
            event_name.clear();
            data_lines.clear();
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event_name = value.trim().to_owned();
            continue;
        }
        if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim_start().to_owned());
        }
    }
    if !data_lines.is_empty() {
        dispatch_remote_event(state, remote_id, &event_name, &data_lines)?;
    }
    Ok(())
}

/// Dispatches remote event.
fn dispatch_remote_event(
    state: &AppState,
    remote_id: &str,
    event_name: &str,
    data_lines: &[String],
) -> Result<()> {
    if data_lines.is_empty() {
        return Ok(());
    }
    let payload = data_lines.join("\n");
    match event_name {
        "state" => {
            let remote_payload: StateEventPayload = serde_json::from_str(&payload)
                .with_context(|| format!("failed to decode remote state event `{remote_id}`"))?;
            if remote_payload.sse_fallback {
                if state.should_skip_remote_sse_fallback_resync(
                    remote_id,
                    remote_payload.state.revision,
                ) {
                    return Ok(());
                }
                eprintln!(
                    "remote event warning> received fallback SSE state payload from `{remote_id}` at revision {}; forcing full state resync",
                    remote_payload.state.revision
                );
                resync_remote_state_snapshot(state, remote_id)?;
                state.note_remote_sse_fallback_resync(remote_id, remote_payload.state.revision);
                return Ok(());
            }
            state
                .apply_remote_state_snapshot(remote_id, remote_payload.state)
                .map_err(|err| anyhow!(err.message))?;
        }
        "delta" => {
            let delta: DeltaEvent = serde_json::from_str(&payload)
                .with_context(|| format!("failed to decode remote delta event `{remote_id}`"))?;
            if let Err(err) = state.apply_remote_delta_event(remote_id, delta) {
                eprintln!("remote delta apply failed for `{remote_id}`: {err:#}");
                resync_remote_state_snapshot(state, remote_id)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn resync_remote_state_snapshot(state: &AppState, remote_id: &str) -> Result<()> {
    let remote = state
        .lookup_remote_config(remote_id)
        .map_err(|err| anyhow!(err.message))?;
    let full_state: StateResponse = state
        .remote_registry
        .request_json(&remote, Method::GET, "/api/state", &[], None)
        .map_err(|err| anyhow!(err.message))?;
    state
        .apply_remote_state_snapshot(remote_id, full_state)
        .map_err(|err| anyhow!(err.message))?;
    Ok(())
}
