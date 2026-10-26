// The predicates that decide whether a session has an Engram binding target,
// and the narrow reader view built from them. Both binding-target builders
// (engram_host_adapter.rs) call these, so a target and the view can never
// disagree about which sessions are eligible. The view answers the boolean
// eligibility and current-owner questions named-root reclamation asks under
// the global lock, without building a full binding target or the root read
// fence each target captures (which clones every root, journal and project
// store).
//
// Owns the predicate sequence (child or root shape, local session, project
// and delegation chain, project reset access, local remote, disabled reason,
// control enabled, binary path and home present) and the reader view. Does
// not own building the transport target (the builders), what reclamation
// does with an eligible reader (engram_named_root_reclamation.rs), or any
// comparison of a reader's connection, which still needs a full target. New
// module, split from the builders' shared prefix in engram_host_adapter.rs.

/// What a session's binding target is built from, once every predicate that
/// decides whether it has one has passed.
struct EngramSessionAuthority<'a> {
    /// The session id the target's connection carries.
    connection_session_id: &'a str,
    record: &'a SessionRecord,
    delegation: Option<&'a DelegationRecord>,
    project: &'a Project,
    settings: &'a EngramProjectSettings,
    binary_path: &'a str,
    home: &'a str,
}

/// A child-shaped session's authority (it never falls back to a parent
/// shape). `Err` keeps the builders' errors: a missing child session or a
/// missing binary path or home on an enabled project.
fn engram_child_session_authority_locked<'a>(
    inner: &'a StateInner,
    session_id: &'a str,
    require_runtime_enabled: bool,
    project_reset_owner_generation: Option<u64>,
) -> std::result::Result<Option<EngramSessionAuthority<'a>>, String> {
    let Some(delegation) = inner
        .delegations
        .iter()
        .find(|delegation| delegation.child_session_id == session_id)
    else {
        return Ok(None);
    };
    let record = inner
        .find_session_index(session_id)
        .and_then(|index| inner.sessions.get(index))
        .ok_or_else(|| format!("delegation child session `{session_id}` is missing"))?;
    if !record.is_local_session() {
        return Ok(None);
    }
    // Engram is configured per project, so a projectless delegation is an
    // ordinary disabled path rather than an adapter failure.
    let Some(project) = engram_project_for_session_locked(inner, session_id) else {
        return Ok(None);
    };
    engram_session_authority_in_project(
        inner,
        session_id,
        record,
        Some(delegation),
        project,
        require_runtime_enabled,
        project_reset_owner_generation,
    )
}

/// A root (parent-shaped) session's authority.
fn engram_parent_session_authority_locked<'a>(
    inner: &'a StateInner,
    session_id: &'a str,
    require_runtime_enabled: bool,
    project_reset_owner_generation: Option<u64>,
) -> std::result::Result<Option<EngramSessionAuthority<'a>>, String> {
    let record = inner
        .find_session_index(session_id)
        .and_then(|index| inner.sessions.get(index))
        .ok_or_else(|| format!("session `{session_id}` is missing"))?;
    if !record.is_local_session() {
        return Ok(None);
    }
    let Some(project_id) = record.session.project_id.as_deref() else {
        return Ok(None);
    };
    let project = inner
        .find_project(project_id)
        .ok_or_else(|| format!("project `{project_id}` is missing"))?;
    engram_session_authority_in_project(
        inner,
        session_id,
        record,
        None,
        project,
        require_runtime_enabled,
        project_reset_owner_generation,
    )
}

fn engram_session_authority_in_project<'a>(
    inner: &'a StateInner,
    connection_session_id: &'a str,
    record: &'a SessionRecord,
    delegation: Option<&'a DelegationRecord>,
    project: &'a Project,
    require_runtime_enabled: bool,
    project_reset_owner_generation: Option<u64>,
) -> std::result::Result<Option<EngramSessionAuthority<'a>>, String> {
    let Some(settings) = project.engram.as_ref() else {
        return Ok(None);
    };
    let reset_access_allowed = match project_reset_owner_generation {
        Some(owner_generation) => inner
            .engram_project_resets
            .is_owned_by(&project.id, owner_generation),
        None => !inner.engram_project_resets.contains(&project.id),
    };
    if !reset_access_allowed
        || project.remote_id != LOCAL_REMOTE_ID
        || (require_runtime_enabled && record.engram.disabled_reason.is_some())
        || (require_runtime_enabled && !settings.is_control_enabled())
    {
        return Ok(None);
    }
    let binary_path = settings
        .binary_path
        .as_deref()
        .ok_or_else(|| "enabled Engram project is missing binaryPath".to_owned())?;
    let home = settings
        .home
        .as_deref()
        .ok_or_else(|| "enabled Engram project is missing home".to_owned())?;
    Ok(Some(EngramSessionAuthority {
        connection_session_id,
        record,
        delegation,
        project,
        settings,
        binary_path,
        home,
    }))
}

/// The session's authority, by the same shape choice the target dispatcher
/// makes: a child-shaped session never falls back to a parent shape.
fn engram_session_authority_locked<'a>(
    inner: &'a StateInner,
    session_id: &'a str,
    require_runtime_enabled: bool,
    project_reset_owner_generation: Option<u64>,
) -> std::result::Result<Option<EngramSessionAuthority<'a>>, String> {
    if AppState::engram_session_has_child_binding_shape_locked(inner, session_id) {
        engram_child_session_authority_locked(
            inner,
            session_id,
            require_runtime_enabled,
            project_reset_owner_generation,
        )
    } else {
        engram_parent_session_authority_locked(
            inner,
            session_id,
            require_runtime_enabled,
            project_reset_owner_generation,
        )
    }
}

// Naming and its canonical readers use Base MCP without authorizing a turn.
// Keep the session/store safety checks even though turn gating is optional.
fn engram_source_root_base_is_enabled_locked(inner: &StateInner, session_id: &str) -> bool {
    let Some(record) = inner
        .find_session_index(session_id)
        .and_then(|index| inner.sessions.get(index))
    else {
        return false;
    };
    let Some(project) = engram_project_for_session_locked(inner, session_id) else {
        return false;
    };
    record.is_local_session()
        && record.engram.disabled_reason.is_none()
        && !inner.engram_project_resets.contains(&project.id)
        && project.remote_id == LOCAL_REMOTE_ID
        && project.engram.as_ref().is_some_and(EngramProjectSettings::is_base_enabled)
}

impl AppState {
    fn engram_source_root_target_locked(
        inner: &StateInner,
        session_id: &str,
    ) -> std::result::Result<Option<EngramBindingTarget>, String> {
        if !engram_source_root_base_is_enabled_locked(inner, session_id) {
            return Ok(None);
        }
        // `false` alone also skips Base enablement and disabled_reason. The
        // explicit guard above preserves both; the builder preserves the
        // root/child shape, project, reset and connection validation.
        Self::engram_binding_target_for_session_shape_locked(inner, session_id, false)
    }
}

/// The narrow view of the Base-service naming target, or None when it would
/// build none or fail. Nothing is cloned and no turn grant is created.
struct EngramSessionReaderView<'a> {
    /// The session id the target's connection would carry, as the core
    /// computes it (a reader's identity for the namer preference).
    connection_session_id: &'a str,
    routing_token: Option<&'a str>,
    store: Option<&'a EngramAuthorityStoreKey>,
}

fn engram_session_reader_view_locked<'a>(
    inner: &'a StateInner,
    session_id: &'a str,
) -> Option<EngramSessionReaderView<'a>> {
    if !engram_source_root_base_is_enabled_locked(inner, session_id) {
        return None;
    }
    let authority = engram_session_authority_locked(inner, session_id, false, None)
        .ok()
        .flatten()?;
    Some(EngramSessionReaderView {
        connection_session_id: authority.connection_session_id,
        routing_token: authority.record.engram.routing_token.as_deref(),
        store: authority.settings.authority_store_key.as_ref(),
    })
}
