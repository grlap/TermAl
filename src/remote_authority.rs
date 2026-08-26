// Remote routing-authority fences shared by registry, proxy, and apply paths.
// Extracted from remote.rs, remote_routes.rs, and remote_create_proxies.rs so
// every endpoint replacement check uses the same routing identity contract.
// This module deliberately does not own transport, connection caching, or
// remote state/delta application.

const REMOTE_CONNECTION_CHANGED_DURING_CREATE: &str =
    "remote connection changed during remote creation";
const REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST: &str =
    "remote connection changed before request dispatch";
const REMOTE_PROJECT_BINDING_CHANGED_DURING_CREATE: &str =
    "project remote binding changed during remote creation";

#[derive(Debug)]
struct RemoteAuthorityApplyError(ApiError);

impl std::fmt::Display for RemoteAuthorityApplyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0.message)
    }
}

impl std::error::Error for RemoteAuthorityApplyError {}

fn remote_authority_apply_error(error: &anyhow::Error) -> Option<ApiError> {
    error
        .downcast_ref::<RemoteAuthorityApplyError>()
        .map(|error| ApiError::from_status(error.0.status, error.0.message.clone()))
}

fn same_remote_routing_config(left: &RemoteConfig, right: &RemoteConfig) -> bool {
    let RemoteConfig {
        id: left_id,
        name: _,
        transport: left_transport,
        enabled: left_enabled,
        host: left_host,
        port: left_port,
        user: left_user,
    } = left;
    let RemoteConfig {
        id: right_id,
        name: _,
        transport: right_transport,
        enabled: right_enabled,
        host: right_host,
        port: right_port,
        user: right_user,
    } = right;

    left_id == right_id
        && left_transport == right_transport
        && left_enabled == right_enabled
        && left_host == right_host
        && left_port == right_port
        && left_user == right_user
}

fn ensure_remote_routing_config(
    current: Option<&RemoteConfig>,
    expected: &RemoteConfig,
) -> Result<(), ApiError> {
    let current = current
        .ok_or_else(|| ApiError::bad_request(format!("unknown remote `{}`", expected.id)))?;
    if !same_remote_routing_config(current, expected) {
        return Err(ApiError::conflict(
            REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST,
        ));
    }
    Ok(())
}

/// Create/fork requests have already crossed a remote mutation boundary when
/// their lease loses authority. Preserve removal's 400 compatibility error,
/// but report replacement/retirement with the create-specific retry contract.
fn remote_create_authority_error(error: ApiError) -> ApiError {
    if error.status == StatusCode::CONFLICT
        && error.message == REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST
    {
        ApiError::conflict(REMOTE_CONNECTION_CHANGED_DURING_CREATE)
    } else {
        error
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemoteProjectBindingResolution {
    Current,
    ProjectMissing,
    ProjectChanged,
    RemoteMissing,
    RemoteChanged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemoteAuthorityResolution {
    Current,
    RemoteMissing,
    RemoteChanged,
}

fn resolve_remote_authority_locked(
    inner: &StateInner,
    remote: &RemoteConfig,
) -> RemoteAuthorityResolution {
    let Some(current) = inner.find_remote(&remote.id) else {
        return RemoteAuthorityResolution::RemoteMissing;
    };
    if !same_remote_routing_config(current, remote) {
        return RemoteAuthorityResolution::RemoteChanged;
    }
    RemoteAuthorityResolution::Current
}

/// Revalidates a remote binding using an already-held state guard.
///
/// Remote create requests release the state mutex while network I/O is in
/// flight. Callers must run this check before applying the returned payload so
/// a concurrent project mutation cannot revive stale state and a response from
/// an old endpoint cannot be attributed to a newly configured server that
/// retained the same remote id.
fn resolve_remote_project_binding_locked(
    inner: &StateInner,
    binding: &RemoteProjectBinding,
) -> RemoteProjectBindingResolution {
    let Some(remote) = inner.find_remote(&binding.remote.id) else {
        return RemoteProjectBindingResolution::RemoteMissing;
    };
    if !same_remote_routing_config(remote, &binding.remote) {
        return RemoteProjectBindingResolution::RemoteChanged;
    }
    let Some(project) = inner.find_project(&binding.local_project_id) else {
        return RemoteProjectBindingResolution::ProjectMissing;
    };
    if project.remote_id != binding.remote.id
        || project.remote_project_id.as_deref() != Some(binding.remote_project_id.as_str())
    {
        return RemoteProjectBindingResolution::ProjectChanged;
    }
    RemoteProjectBindingResolution::Current
}

impl AppState {
    /// When a decoded response is also malformed, routing authority wins the
    /// error-classification race. Otherwise endpoint replacement or removal
    /// can be hidden behind an unrelated 502 payload-validation error.
    fn prefer_current_remote_response_error(
        &self,
        lease: &RemoteRequestLease,
        response_error: ApiError,
    ) -> ApiError {
        match self.remote_registry.ensure_lease_current(lease) {
            Ok(()) => response_error,
            Err(authority_error) => authority_error,
        }
    }

    fn prefer_current_remote_create_response_error(
        &self,
        lease: &RemoteRequestLease,
        response_error: ApiError,
    ) -> ApiError {
        remote_create_authority_error(
            self.prefer_current_remote_response_error(lease, response_error),
        )
    }

    /// Revalidates request-scoped routing authority while the application
    /// state mutex is held. Settings publication takes this same mutex before
    /// updating the registry, so a successful check linearizes the following
    /// local read or mutation against endpoint replacement.
    fn ensure_remote_route_current_locked(
        &self,
        inner: &StateInner,
        expected: &RemoteConfig,
    ) -> Result<(), ApiError> {
        ensure_remote_routing_config(inner.find_remote(&expected.id), expected)
    }

    /// Revalidates the exact request connection while holding application
    /// state. The immutable routing bytes alone are insufficient because an
    /// A -> B -> A settings cycle restores the same bytes after retiring the
    /// original connection. Keeping this lease fence adjacent to the final
    /// mutation prevents a pre-cycle response from crossing that cycle.
    fn ensure_remote_request_current_locked(
        &self,
        inner: &StateInner,
        lease: &RemoteRequestLease,
    ) -> Result<(), ApiError> {
        self.ensure_remote_apply_authority_locked(
            inner,
            &lease.pinned,
            Some(&lease.connection),
        )?;
        lease
            .connection
            .ensure_state_continuity_generation(lease.state_continuity_generation)
    }

    fn ensure_remote_create_request_current_locked(
        &self,
        inner: &StateInner,
        lease: &RemoteRequestLease,
    ) -> Result<(), ApiError> {
        self.ensure_remote_request_current_locked(inner, lease)
            .map_err(remote_create_authority_error)
    }

    fn ensure_remote_apply_authority_locked(
        &self,
        inner: &StateInner,
        expected_remote: &RemoteConfig,
        expected_connection: Option<&RemoteConnection>,
    ) -> Result<(), ApiError> {
        // The settings-owned config under `inner` is authoritative and must be
        // checked first so removal preserves the established 400
        // `unknown remote` contract. A current route with different bytes is a
        // replacement conflict. Only after those cases are distinguished does
        // `retired` detect same-bytes retirement such as A -> B -> A or a
        // display-name-only publication.
        self.ensure_remote_route_current_locked(inner, expected_remote)?;
        if expected_connection
            .is_some_and(|connection| connection.retired.load(Ordering::SeqCst))
        {
            return Err(ApiError::conflict(
                REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST,
            ));
        }
        Ok(())
    }
}
