/*
Remote execution bridge
Browser
  -> local TermAl
     -> RemoteRegistry
        -> RemoteConnection
           -> ssh tunnel or managed remote server
              -> remote TermAl /api + /api/events
This layer keeps the browser on one local origin while proxying REST calls,
bridging SSE streams, and rewriting local/remote identifiers.
*/

use reqwest::Method;
use reqwest::blocking::{Client as BlockingHttpClient, Response as BlockingHttpResponse};
use serde::de::DeserializeOwned;
use std::io::Read as _;
use std::thread;
use std::time::Instant;

const REMOTE_SERVER_PORT: u16 = 8787;
const REMOTE_FORWARD_PORT_START: u16 = 47000;
const REMOTE_FORWARD_PORT_END: u16 = 56999;
const REMOTE_HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const REMOTE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const REMOTE_STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const REMOTE_HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(250);
const REMOTE_EVENT_RETRY_DELAY: Duration = Duration::from_secs(2);
const REMOTE_EVENT_SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(50);
const TERMINAL_REMOTE_STREAM_READ_CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_REMOTE_ERROR_BODY_CHARS: usize = 512;
const MAX_REMOTE_ERROR_BODY_BYTES: usize = 64 * 1024;

static NEXT_REMOTE_FORWARD_PORT: AtomicU16 = AtomicU16::new(REMOTE_FORWARD_PORT_START);

/// Represents remote registry.
struct RemoteRegistry {
    client: BlockingHttpClientHandle,
    connections: Arc<Mutex<HashMap<String, Arc<RemoteConnection>>>>,
}

/// Represents the blocking HTTP client handle.
struct BlockingHttpClientHandle {
    client: Option<BlockingHttpClient>,
}

impl BlockingHttpClientHandle {
    /// Creates a new instance.
    fn new(client: BlockingHttpClient) -> Self {
        Self {
            client: Some(client),
        }
    }

    /// Handles client.
    fn client(&self) -> &BlockingHttpClient {
        self.client
            .as_ref()
            .expect("remote HTTP client should exist while registry is alive")
    }
}

impl Drop for BlockingHttpClientHandle {
    /// Releases resources when the value is dropped.
    fn drop(&mut self) {
        if let Some(client) = self.client.take() {
            // reqwest::blocking tears down an internal Tokio runtime on drop.
            // Offload that work so the last AppState clone can be released from
            // async handler contexts without panicking.
            let _ = thread::spawn(move || drop(client));
        }
    }
}

impl RemoteRegistry {
    /// Creates a new instance.
    fn new() -> Result<Self> {
        let client = BlockingHttpClient::builder()
            .connect_timeout(REMOTE_HEALTH_TIMEOUT)
            .build()
            .context("failed to build remote HTTP client")?;
        Ok(Self {
            client: BlockingHttpClientHandle::new(client),
            connections: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Reconciles the set of remote connections against the latest config list.
    /// Returns the IDs of remotes whose config changed (connection was torn down
    /// and watermarks should be cleared by the caller) and the IDs of remotes
    /// that were removed entirely.
    fn reconcile(&self, remotes: &[RemoteConfig]) -> Vec<String> {
        let next_by_id = remotes
            .iter()
            .map(|remote| (remote.id.clone(), remote.clone()))
            .collect::<HashMap<_, _>>();
        let mut connections = self
            .connections
            .lock()
            .expect("remote registry mutex poisoned");
        let existing_ids = connections.keys().cloned().collect::<Vec<_>>();
        let mut changed_ids = Vec::new();
        for remote_id in existing_ids {
            let Some(connection) = connections.get(&remote_id).cloned() else {
                continue;
            };
            let Some(remote) = next_by_id.get(&remote_id).cloned() else {
                connection.stop_event_bridge();
                connections.remove(&remote_id);
                changed_ids.push(remote_id);
                continue;
            };
            if connection.update_config(remote) {
                changed_ids.push(remote_id);
            }
        }
        changed_ids
    }

    /// Handles connection.
    fn connection(&self, remote: &RemoteConfig) -> Arc<RemoteConnection> {
        let mut connections = self
            .connections
            .lock()
            .expect("remote registry mutex poisoned");
        let connection = connections
            .entry(remote.id.clone())
            .or_insert_with(|| Arc::new(RemoteConnection::new(remote.clone())))
            .clone();
        connection.update_config(remote.clone());
        connection
    }

    /// Handles request JSON.
    fn request_json<T: DeserializeOwned>(
        &self,
        remote: &RemoteConfig,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
    ) -> Result<T, ApiError> {
        self.request_json_with_timeout(remote, method, path, query, body, REMOTE_REQUEST_TIMEOUT)
    }

    /// Handles request JSON with an explicit timeout.
    fn request_json_with_timeout<T: DeserializeOwned>(
        &self,
        remote: &RemoteConfig,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
        timeout: Duration,
    ) -> Result<T, ApiError> {
        let response = self.request_with_timeout(remote, method, path, query, body, timeout)?;
        decode_remote_json(response)
    }

    /// Handles request without a response read timeout.
    fn request_without_timeout(
        &self,
        remote: &RemoteConfig,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
    ) -> Result<BlockingHttpResponse, ApiError> {
        self.request_with_optional_timeout(remote, method, path, query, body, None)
    }

    /// Handles request with an explicit timeout.
    fn request_with_timeout(
        &self,
        remote: &RemoteConfig,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
        timeout: Duration,
    ) -> Result<BlockingHttpResponse, ApiError> {
        self.request_with_optional_timeout(remote, method, path, query, body, Some(timeout))
    }

    /// Handles request with an optional response read timeout.
    fn request_with_optional_timeout(
        &self,
        remote: &RemoteConfig,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
        timeout: Option<Duration>,
    ) -> Result<BlockingHttpResponse, ApiError> {
        let connection = self.connection(remote);
        let base_url = connection.ensure_available(self.client.client())?;
        let url = format!("{base_url}{path}");
        let mut request = self.client.client().request(method, &url);
        if let Some(timeout) = timeout {
            request = request.timeout(timeout);
        }
        if !query.is_empty() {
            request = request.query(query);
        }
        if let Some(payload) = body {
            request = request.json(&payload);
        }
        let response = request.send().map_err(|err| {
            eprintln!(
                "failed to contact remote `{}` at {}: {err}",
                remote.name,
                remote.host.as_deref().unwrap_or("unknown host")
            );
            ApiError::bad_gateway(remote_connection_issue_message(&remote.name))
        })?;
        Ok(response)
    }

    /// Starts event bridge.
    fn start_event_bridge(&self, state: AppState, remote: &RemoteConfig) {
        let connection = self.connection(remote);
        connection.start_event_bridge(self.client.client().clone(), state);
    }
    /// Returns the cached inline-template capability for the active remote.
    fn cached_supports_inline_orchestrator_templates(
        &self,
        remote: &RemoteConfig,
    ) -> Option<bool> {
        let connection = self.connection(remote);
        connection.cached_supports_inline_orchestrator_templates()
    }
}


/// Represents remote connection.
struct RemoteConnection {
    config: Mutex<RemoteConfig>,
    forwarded_port: u16,
    process: Mutex<Option<RemoteProcessHandle>>,
    event_bridge_started: AtomicBool,
    event_bridge_shutdown: AtomicBool,
    supports_inline_orchestrator_templates: Mutex<Option<bool>>,
}

impl RemoteConnection {
    /// Creates a new instance.
    fn new(remote: RemoteConfig) -> Self {
        Self {
            config: Mutex::new(remote),
            forwarded_port: allocate_remote_forward_port(),
            process: Mutex::new(None),
            event_bridge_started: AtomicBool::new(false),
            event_bridge_shutdown: AtomicBool::new(false),
            supports_inline_orchestrator_templates: Mutex::new(None),
        }
    }

    /// Handles config.
    fn config(&self) -> RemoteConfig {
        self.config
            .lock()
            .expect("remote config mutex poisoned")
            .clone()
    }

    /// Updates config. Returns `true` when the config actually changed and the
    /// connection was torn down.
    fn update_config(&self, remote: RemoteConfig) -> bool {
        let mut config = self.config.lock().expect("remote config mutex poisoned");
        if *config != remote {
            *config = remote;
            drop(config);
            self.disconnect();
            true
        } else {
            false
        }
    }

    /// Handles disconnect.
    fn disconnect(&self) {
        let mut process = self.process.lock().expect("remote process mutex poisoned");
        if let Some(mut handle) = process.take() {
            let _ = handle.child.kill();
            let _ = handle.child.wait();
        }
        *self
            .supports_inline_orchestrator_templates
            .lock()
            .expect("remote capability mutex poisoned") = None;
    }

    /// Stops event bridge.
    fn stop_event_bridge(&self) {
        self.event_bridge_shutdown.store(true, Ordering::SeqCst);
        self.disconnect();
    }

    /// Handles wait for bridge retry or shutdown.
    fn wait_for_bridge_retry_or_shutdown(&self, duration: Duration) -> bool {
        let deadline = Instant::now() + duration;
        loop {
            if self.event_bridge_shutdown.load(Ordering::SeqCst) {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            thread::sleep(std::cmp::min(
                REMOTE_EVENT_SHUTDOWN_POLL_INTERVAL,
                deadline.saturating_duration_since(now),
            ));
        }
    }

    /// Handles base URL.
    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.forwarded_port)
    }

    /// Returns the cached inline-template capability.
    fn cached_supports_inline_orchestrator_templates(&self) -> Option<bool> {
        *self
            .supports_inline_orchestrator_templates
            .lock()
            .expect("remote capability mutex poisoned")
    }

    /// Caches capabilities reported by the remote health endpoint.
    fn cache_health_response(&self, payload: &HealthResponse) {
        *self
            .supports_inline_orchestrator_templates
            .lock()
            .expect("remote capability mutex poisoned") =
            Some(payload.supports_inline_orchestrator_templates);
    }

    /// Ensures available.
    fn ensure_available(&self, client: &BlockingHttpClient) -> Result<String, ApiError> {
        let remote = self.config();
        validate_remote_connection_config(&remote)?;
        if remote.transport != RemoteTransport::Ssh {
            return Err(ApiError::bad_request(format!(
                "remote `{}` does not use SSH transport",
                remote.name
            )));
        }
        let base_url = self.base_url();

        if let Ok(payload) = remote_healthcheck(client, &base_url) {
            self.cache_health_response(&payload);
            return Ok(base_url);
        }

        let mut process = self.process.lock().expect("remote process mutex poisoned");
        if let Some(handle) = process.as_mut() {
            match handle.child.try_wait() {
                Ok(Some(_)) => {
                    *process = None;
                }
                Ok(None) => {
                    if let Ok(payload) = remote_healthcheck(client, &base_url) {
                        self.cache_health_response(&payload);
                        return Ok(base_url);
                    }
                    if let Some(mut handle) = process.take() {
                        let _ = handle.child.kill();
                        let _ = handle.child.wait();
                    }
                }
                Err(_) => {
                    *process = None;
                }
            }
        }

        let managed_attempt = self.start_process(&remote, RemoteProcessMode::ManagedServer)?;
        match wait_for_remote_health(client, &base_url, managed_attempt) {
            Ok((handle, health)) => {
                self.cache_health_response(&health);
                *process = Some(handle);
                Ok(base_url)
            }
            Err(managed_error) => {
                let tunnel_attempt = self.start_process(&remote, RemoteProcessMode::TunnelOnly)?;
                match wait_for_remote_health(client, &base_url, tunnel_attempt) {
                    Ok((handle, health)) => {
                        self.cache_health_response(&health);
                        *process = Some(handle);
                        Ok(base_url)
                    }
                    Err(tunnel_error) => {
                        eprintln!(
                            "remote SSH connection failed for `{}`. managed start failed: {}. tunnel-only fallback failed: {}",
                            remote.name, managed_error, tunnel_error
                        );
                        Err(ApiError::bad_gateway(remote_connection_issue_message(
                            &remote.name,
                        )))
                    }
                }
            }
        }
    }

    /// Starts process.
    fn start_process(
        &self,
        remote: &RemoteConfig,
        mode: RemoteProcessMode,
    ) -> Result<RemoteProcessHandle, ApiError> {
        let mut command = Command::new("ssh");
        for arg in remote_ssh_command_args(remote, self.forwarded_port, mode)? {
            command.arg(arg);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let child = command.spawn().map_err(|err| {
            eprintln!(
                "failed to start SSH connection for remote `{}`: {err}",
                remote.name
            );
            ApiError::bad_gateway(local_ssh_start_issue_message(&remote.name))
        })?;
        Ok(RemoteProcessHandle { child, mode })
    }

    /// Starts event bridge.
    fn start_event_bridge(self: &Arc<Self>, client: BlockingHttpClient, state: AppState) {
        self.event_bridge_shutdown.store(false, Ordering::SeqCst);
        if self.event_bridge_started.swap(true, Ordering::SeqCst) {
            return;
        }

        let connection = Arc::clone(self);
        thread::spawn(move || {
            /// Represents event bridge reset.
            struct EventBridgeReset {
                connection: Arc<RemoteConnection>,
            }

            impl Drop for EventBridgeReset {
                /// Releases resources when the value is dropped.
                fn drop(&mut self) {
                    self.connection
                        .event_bridge_started
                        .store(false, Ordering::SeqCst);
                }
            }

            let _reset = EventBridgeReset {
                connection: Arc::clone(&connection),
            };

            loop {
                if connection.event_bridge_shutdown.load(Ordering::SeqCst) {
                    break;
                }

                let remote = connection.config();
                if !remote.enabled || remote.transport != RemoteTransport::Ssh {
                    state.clear_remote_applied_revision(&remote.id);
                    state.clear_remote_sse_fallback_resync(&remote.id);
                    if connection.wait_for_bridge_retry_or_shutdown(REMOTE_EVENT_RETRY_DELAY) {
                        break;
                    }
                    continue;
                }

                let base_url = match connection.ensure_available(&client) {
                    Ok(base_url) => base_url,
                    Err(err) => {
                        eprintln!(
                            "remote event bridge `{}` failed to connect: {err:#?}",
                            remote.id
                        );
                        state.clear_remote_applied_revision(&remote.id);
                        state.clear_remote_sse_fallback_resync(&remote.id);
                        if connection.wait_for_bridge_retry_or_shutdown(REMOTE_EVENT_RETRY_DELAY) {
                            break;
                        }
                        continue;
                    }
                };

                let response = match client.get(format!("{base_url}/api/events")).send() {
                    Ok(response) => response,
                    Err(err) => {
                        eprintln!(
                            "remote event bridge `{}` failed to connect: {err:#?}",
                            remote.id
                        );
                        state.clear_remote_applied_revision(&remote.id);
                        state.clear_remote_sse_fallback_resync(&remote.id);
                        if connection.wait_for_bridge_retry_or_shutdown(REMOTE_EVENT_RETRY_DELAY) {
                            break;
                        }
                        continue;
                    }
                };

                if let Err(err) = process_remote_event_stream(&state, &remote.id, response) {
                    eprintln!("remote event bridge `{}` disconnected: {err:#}", remote.id);
                }
                state.clear_remote_applied_revision(&remote.id);
                state.clear_remote_sse_fallback_resync(&remote.id);
                if connection.wait_for_bridge_retry_or_shutdown(REMOTE_EVENT_RETRY_DELAY) {
                    break;
                }
            }
        });
    }
}

/// Represents the remote process handle.
struct RemoteProcessHandle {
    child: Child,
    mode: RemoteProcessMode,
}

/// Enumerates remote process modes.
#[derive(Clone, Copy)]
enum RemoteProcessMode {
    ManagedServer,
    TunnelOnly,
}

/// Represents remote scope.
#[derive(Clone)]
struct RemoteScope {
    remote: RemoteConfig,
    remote_project_id: Option<String>,
    remote_session_id: Option<String>,
}

/// Represents the remote session target.
#[derive(Clone)]
struct RemoteSessionTarget {
    local_session_id: String,
    remote: RemoteConfig,
    remote_session_id: String,
}

/// Represents the remote orchestrator target.
#[derive(Clone)]
struct RemoteOrchestratorTarget {
    local_instance_id: String,
    remote: RemoteConfig,
    remote_orchestrator_id: String,
}

/// Represents remote project binding.
#[derive(Clone)]
struct RemoteProjectBinding {
    local_project_id: String,
    remote: RemoteConfig,
    remote_project_id: String,
}

impl AppState {
    /// Handles restore remote event bridges.
    fn restore_remote_event_bridges(&self) {
        let remotes = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .sessions
                .iter()
                .filter_map(|record| record.remote_id.as_deref())
                .filter_map(|remote_id| inner.find_remote(remote_id))
                .cloned()
                .collect::<Vec<_>>()
        };

        for remote in remotes {
            self.remote_registry
                .start_event_bridge(self.clone(), &remote);
        }
    }

    /// Handles remote session target.
    fn remote_session_target(
        &self,
        session_id: &str,
    ) -> Result<Option<RemoteSessionTarget>, ApiError> {
        let (remote_id, remote_session_id) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = &inner.sessions[index];
            let Some(remote_id) = record.remote_id.clone() else {
                return Ok(None);
            };
            let Some(remote_session_id) = record.remote_session_id.clone() else {
                return Ok(None);
            };
            (remote_id, remote_session_id)
        };
        let remote = self.lookup_remote_config(&remote_id)?;
        Ok(Some(RemoteSessionTarget {
            local_session_id: session_id.to_owned(),
            remote,
            remote_session_id,
        }))
    }

    /// Handles remote orchestrator target.
    fn remote_orchestrator_target(
        &self,
        instance_id: &str,
    ) -> Result<Option<RemoteOrchestratorTarget>, ApiError> {
        let (remote_id, remote_orchestrator_id) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let instance = inner
                .orchestrator_instances
                .iter()
                .find(|instance| instance.id == instance_id)
                .ok_or_else(|| ApiError::not_found("orchestrator instance not found"))?;
            let Some(remote_id) = instance.remote_id.clone() else {
                return Ok(None);
            };
            let Some(remote_orchestrator_id) = instance.remote_orchestrator_id.clone() else {
                return Ok(None);
            };
            (remote_id, remote_orchestrator_id)
        };
        let remote = self.lookup_remote_config(&remote_id)?;
        Ok(Some(RemoteOrchestratorTarget {
            local_instance_id: instance_id.to_owned(),
            remote,
            remote_orchestrator_id,
        }))
    }

    /// Peeks whether a terminal request with the given identifiers would
    /// resolve to a remote scope, using only in-memory state (no network
    /// I/O). Callers use this to decide which concurrency semaphore to
    /// acquire before invoking `remote_scope_for_request`, which can
    /// otherwise trigger `ensure_remote_project_binding`'s unbounded
    /// `POST /api/projects` call outside the 429 rate limit on a burst of
    /// first-time remote terminal requests.
    fn terminal_request_is_remote(
        &self,
        session_id: Option<&str>,
        project_id: Option<&str>,
    ) -> bool {
        let inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(session_id) = normalize_optional_identifier(session_id) {
            if let Some(index) = inner.find_session_index(session_id) {
                let record = &inner.sessions[index];
                if record.remote_id.is_some() && record.remote_session_id.is_some() {
                    return true;
                }
            }
        }

        if let Some(project_id) = normalize_optional_identifier(project_id) {
            if let Some(project) = inner.find_project(project_id) {
                if project.remote_id != LOCAL_REMOTE_ID {
                    return true;
                }
            }
        }

        false
    }

    /// Handles remote scope for request.
    fn remote_scope_for_request(
        &self,
        session_id: Option<&str>,
        project_id: Option<&str>,
    ) -> Result<Option<RemoteScope>, ApiError> {
        if let Some(session_id) = normalize_optional_identifier(session_id) {
            if let Some(target) = self.remote_session_target(session_id)? {
                return Ok(Some(RemoteScope {
                    remote: target.remote,
                    remote_project_id: None,
                    remote_session_id: Some(target.remote_session_id),
                }));
            }
        }

        if let Some(project_id) = normalize_optional_identifier(project_id) {
            if let Some(binding) = self.ensure_remote_project_binding(project_id)? {
                return Ok(Some(RemoteScope {
                    remote: binding.remote,
                    remote_project_id: Some(binding.remote_project_id),
                    remote_session_id: None,
                }));
            }
        }

        Ok(None)
    }

    /// Handles remote get JSON.
    fn remote_get_json<T: DeserializeOwned>(
        &self,
        scope: &RemoteScope,
        path: &str,
        mut query: Vec<(String, String)>,
    ) -> Result<T, ApiError> {
        apply_remote_scope_to_query(scope, &mut query);
        self.remote_registry
            .request_json(&scope.remote, Method::GET, path, &query, None)
    }

    /// Handles remote post JSON.
    fn remote_post_json<T: DeserializeOwned>(
        &self,
        scope: &RemoteScope,
        path: &str,
        body: Value,
    ) -> Result<T, ApiError> {
        self.remote_registry.request_json(
            &scope.remote,
            Method::POST,
            path,
            &[],
            Some(apply_remote_scope_to_body(scope, body)),
        )
    }

    /// Handles remote post JSON with an explicit timeout.
    fn remote_post_json_with_timeout<T: DeserializeOwned>(
        &self,
        scope: &RemoteScope,
        path: &str,
        body: Value,
        timeout: Duration,
    ) -> Result<T, ApiError> {
        self.remote_registry.request_json_with_timeout(
            &scope.remote,
            Method::POST,
            path,
            &[],
            Some(apply_remote_scope_to_body(scope, body)),
            timeout,
        )
    }

    /// Handles remote post response without a response read timeout.
    fn remote_post_response_without_timeout(
        &self,
        scope: &RemoteScope,
        path: &str,
        body: Value,
    ) -> Result<BlockingHttpResponse, ApiError> {
        self.remote_registry.request_without_timeout(
            &scope.remote,
            Method::POST,
            path,
            &[],
            Some(apply_remote_scope_to_body(scope, body)),
        )
    }

    /// Handles remote put JSON.
    fn remote_put_json<T: DeserializeOwned>(
        &self,
        scope: &RemoteScope,
        path: &str,
        body: Value,
    ) -> Result<T, ApiError> {
        self.remote_registry.request_json(
            &scope.remote,
            Method::PUT,
            path,
            &[],
            Some(apply_remote_scope_to_body(scope, body)),
        )
    }

    /// Handles remote put JSON with query scope.
    fn remote_put_json_with_query_scope<T: DeserializeOwned>(
        &self,
        scope: &RemoteScope,
        path: &str,
        mut query: Vec<(String, String)>,
        body: Value,
    ) -> Result<T, ApiError> {
        apply_remote_scope_to_query(scope, &mut query);
        self.remote_registry
            .request_json(&scope.remote, Method::PUT, path, &query, Some(body))
    }

    /// Handles lookup remote config.
    fn lookup_remote_config(&self, remote_id: &str) -> Result<RemoteConfig, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner
            .find_remote(remote_id)
            .cloned()
            .ok_or_else(|| ApiError::bad_request(format!("unknown remote `{remote_id}`")))
    }

    /// Ensures remote project binding.
    fn ensure_remote_project_binding(
        &self,
        project_id: &str,
    ) -> Result<Option<RemoteProjectBinding>, ApiError> {
        let project = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .find_project(project_id)
                .cloned()
                .ok_or_else(|| ApiError::not_found("project not found"))?
        };
        if project.remote_id == LOCAL_REMOTE_ID {
            return Ok(None);
        }

        let remote = self.lookup_remote_config(&project.remote_id)?;
        validate_remote_connection_config(&remote)?;
        if let Some(remote_project_id) = project.remote_project_id.clone() {
            return Ok(Some(RemoteProjectBinding {
                local_project_id: project.id,
                remote,
                remote_project_id,
            }));
        }

        let response: CreateProjectResponse = self.remote_registry.request_json(
            &remote,
            Method::POST,
            "/api/projects",
            &[],
            Some(json!({
                "name": project.name,
                "rootPath": project.root_path,
                "remoteId": LOCAL_REMOTE_ID,
            })),
        )?;

        let remote_project_id = response.project_id;
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .projects
                .iter()
                .position(|candidate| candidate.id == project.id)
                .ok_or_else(|| ApiError::not_found("project not found"))?;
            if inner.projects[index].remote_project_id.as_deref()
                != Some(remote_project_id.as_str())
            {
                inner.projects[index].remote_project_id = Some(remote_project_id.clone());
                self.commit_locked(&mut inner).map_err(|err| {
                    ApiError::internal(format!("failed to persist remote project binding: {err:#}"))
                })?;
            }
        }

        Ok(Some(RemoteProjectBinding {
            local_project_id: project.id,
            remote,
            remote_project_id,
        }))
    }
    /// Creates remote project proxy.
    fn create_remote_project_proxy(
        &self,
        request: CreateProjectRequest,
        remote: RemoteConfig,
        root_path: String,
    ) -> Result<CreateProjectResponse, ApiError> {
        let existing = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .projects
                .iter()
                .find(|project| project.remote_id == remote.id && project.root_path == root_path)
                .cloned()
        };
        if let Some(existing) = existing {
            if existing.remote_project_id.is_none() {
                let _ = self.ensure_remote_project_binding(&existing.id)?;
            }
            return Ok(CreateProjectResponse {
                project_id: existing.id,
                state: self.snapshot(),
            });
        }

        let remote_response: CreateProjectResponse = self.remote_registry.request_json(
            &remote,
            Method::POST,
            "/api/projects",
            &[],
            Some(json!({
                "name": request.name,
                "rootPath": root_path,
                "remoteId": LOCAL_REMOTE_ID,
            })),
        )?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let existing_len = inner.projects.len();
        let project = inner.create_project(request.name, root_path, remote.id.clone());
        let index = inner
            .projects
            .iter()
            .position(|candidate| candidate.id == project.id)
            .ok_or_else(|| ApiError::not_found("project not found"))?;
        let mut changed = inner.projects.len() != existing_len;
        if inner.projects[index].remote_project_id.as_deref()
            != Some(remote_response.project_id.as_str())
        {
            inner.projects[index].remote_project_id = Some(remote_response.project_id.clone());
            changed = true;
        }
        if changed {
            self.commit_locked(&mut inner)
                .map_err(|err| ApiError::internal(format!("failed to persist project: {err:#}")))?;
        }
        Ok(CreateProjectResponse {
            project_id: project.id,
            state: self.snapshot_from_inner(&inner),
        })
    }

    /// Creates remote session proxy.
    fn create_remote_session_proxy(
        &self,
        request: CreateSessionRequest,
        project: Project,
    ) -> Result<CreateSessionResponse, ApiError> {
        let Some(binding) = self.ensure_remote_project_binding(&project.id)? else {
            return Err(ApiError::bad_request("remote project binding is missing"));
        };
        let remote_response: CreateSessionResponse = self.remote_registry.request_json(
            &binding.remote,
            Method::POST,
            "/api/sessions",
            &[],
            Some(json!({
                "agent": request.agent,
                "name": request.name,
                "workdir": request.workdir,
                "projectId": binding.remote_project_id,
                "model": request.model,
                "approvalPolicy": request.approval_policy,
                "reasoningEffort": request.reasoning_effort,
                "sandboxMode": request.sandbox_mode,
                "cursorMode": request.cursor_mode,
                "claudeApprovalMode": request.claude_approval_mode,
                "claudeEffort": request.claude_effort,
                "geminiApprovalMode": request.gemini_approval_mode,
            })),
        )?;
        self.remote_registry
            .start_event_bridge(self.clone(), &binding.remote);
        let remote_session = remote_response
            .session
            .clone()
            .or_else(|| {
                remote_response.state.as_ref().and_then(|state| {
                    state
                        .sessions
                        .iter()
                        .find(|session| session.id == remote_response.session_id)
                        .cloned()
                })
            })
            .ok_or_else(|| ApiError::bad_gateway("remote session was not returned"))?;
        let (revision, local_session_id, local_session) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let applied_remote_revision = remote_response.state.as_ref().is_some_and(|state| {
                apply_remote_state_if_newer_locked(
                    &mut inner,
                    &binding.remote.id,
                    state,
                    Some(remote_response.session_id.as_str()),
                )
            });
            let (local_session_id, changed) = ensure_remote_proxy_session_record(
                &mut inner,
                &binding.remote.id,
                &remote_session,
                Some(binding.local_project_id),
                applied_remote_revision,
            );
            if applied_remote_revision {
                inner.note_remote_applied_revision(
                    &binding.remote.id,
                    remote_response
                        .state
                        .as_ref()
                        .map(|state| state.revision)
                        .unwrap_or(remote_response.revision),
                );
            }
            let local_record = inner
                .find_session_index(&local_session_id)
                .and_then(|index| inner.sessions.get(index))
                .cloned()
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let local_session = local_record.session.clone();
            let revision = if applied_remote_revision {
                self.bump_revision_and_persist_locked(&mut inner).map_err(|err| {
                    ApiError::internal(format!("failed to persist remote session proxy: {err:#}"))
                })?
            } else if changed {
                self.commit_session_created_locked(&mut inner, &local_record)
                    .map_err(|err| {
                        ApiError::internal(format!(
                            "failed to persist remote session proxy: {err:#}"
                        ))
                    })?
            } else {
                inner.revision
            };
            (revision, local_session_id, local_session)
        };
        self.publish_delta(&DeltaEvent::SessionCreated {
            revision,
            session_id: local_session.id.clone(),
            session: local_session.clone(),
        });

        Ok(CreateSessionResponse {
            session_id: local_session_id,
            session: Some(local_session),
            revision,
            state: None,
        })
    }

    /// Creates remote orchestrator proxy.
    fn create_remote_orchestrator_proxy(
        &self,
        template: &OrchestratorTemplate,
        project: &Project,
    ) -> Result<CreateOrchestratorInstanceResponse, ApiError> {
        let Some(binding) = self.ensure_remote_project_binding(&project.id)? else {
            return Err(ApiError::bad_request("remote project binding is missing"));
        };
        let mut remote_template = orchestrator_template_to_draft(template);
        remote_template.project_id = Some(binding.remote_project_id.clone());
        let request_body = serde_json::to_value(CreateOrchestratorInstanceRequest {
            template_id: template.id.clone(),
            project_id: Some(binding.remote_project_id.clone()),
            template: Some(remote_template),
        })
        .map_err(|err| {
            ApiError::internal(format!(
                "failed to encode remote orchestrator create request: {err}"
            ))
        })?;
        let remote_response: CreateOrchestratorInstanceResponse = match self.remote_registry.request_json(
            &binding.remote,
            Method::POST,
            "/api/orchestrators",
            &[],
            Some(request_body),
        ) {
            Ok(response) => response,
            Err(err)
                if err.status == StatusCode::NOT_FOUND
                    && !matches!(
                        self.remote_registry
                            .cached_supports_inline_orchestrator_templates(&binding.remote),
                        Some(true)
                    ) =>
            {
                return Err(ApiError::bad_gateway(format!(
                    "remote `{}` must be upgraded before it can launch local orchestrator templates",
                    binding.remote.name
                )));
            }
            Err(err) => return Err(err),
        };
        let (state, local_orchestrator) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let applied_remote_revision = apply_remote_state_if_newer_locked(
                &mut inner,
                &binding.remote.id,
                &remote_response.state,
                None,
            );
            let remote_sessions_by_id = remote_response
                .state
                .sessions
                .iter()
                .map(|session| (session.id.as_str(), session))
                .collect::<HashMap<_, _>>();
            let (local_orchestrator, changed) = match ensure_remote_orchestrator_instance(
                &mut inner,
                &binding.remote.id,
                &remote_response.orchestrator,
                Some(&remote_sessions_by_id),
                applied_remote_revision,
            ) {
                Ok(result) => result,
                Err(err) => {
                    return Err(ApiError::bad_gateway(format!(
                        "remote orchestrator could not be localized: {err}"
                    )));
                }
            };
            if applied_remote_revision {
                inner.note_remote_applied_revision(
                    &binding.remote.id,
                    remote_response.state.revision,
                );
            }
            if applied_remote_revision || changed {
                self.commit_locked(&mut inner).map_err(|err| {
                    ApiError::internal(format!(
                        "failed to persist remote orchestrator proxy: {err:#}"
                    ))
                })?;
            }
            (self.snapshot_from_inner(&inner), local_orchestrator)
        };
        self.remote_registry
            .start_event_bridge(self.clone(), &binding.remote);

        Ok(CreateOrchestratorInstanceResponse {
            orchestrator: local_orchestrator,
            state,
        })
    }

    /// Proxies remote fork Codex thread.
    fn proxy_remote_fork_codex_thread(
        &self,
        session_id: &str,
    ) -> Result<CreateSessionResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_response: CreateSessionResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/codex/thread/fork",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            None,
        )?;
        let remote_session = remote_response
            .session
            .clone()
            .or_else(|| {
                remote_response.state.as_ref().and_then(|state| {
                    state
                        .sessions
                        .iter()
                        .find(|session| session.id == remote_response.session_id)
                        .cloned()
                })
            })
            .ok_or_else(|| ApiError::bad_gateway("remote forked session was not returned"))?;
        let local_project_id = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&target.local_session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            inner.sessions[index].session.project_id.clone()
        };
        let (revision, local_session_id, local_session) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let applied_remote_revision = remote_response.state.as_ref().is_some_and(|state| {
                apply_remote_state_if_newer_locked(
                    &mut inner,
                    &target.remote.id,
                    state,
                    Some(&target.remote_session_id),
                )
            });
            let (local_session_id, changed) = ensure_remote_proxy_session_record(
                &mut inner,
                &target.remote.id,
                &remote_session,
                local_project_id,
                applied_remote_revision,
            );
            if applied_remote_revision {
                inner.note_remote_applied_revision(
                    &target.remote.id,
                    remote_response
                        .state
                        .as_ref()
                        .map(|state| state.revision)
                        .unwrap_or(remote_response.revision),
                );
            }
            let local_record = inner
                .find_session_index(&local_session_id)
                .and_then(|index| inner.sessions.get(index))
                .cloned()
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let local_session = local_record.session.clone();
            let revision = if applied_remote_revision {
                self.bump_revision_and_persist_locked(&mut inner).map_err(|err| {
                    ApiError::internal(format!(
                        "failed to persist remote forked session proxy: {err:#}"
                    ))
                })?
            } else if changed {
                self.commit_session_created_locked(&mut inner, &local_record)
                    .map_err(|err| {
                        ApiError::internal(format!(
                            "failed to persist remote forked session proxy: {err:#}"
                        ))
                    })?
            } else {
                inner.revision
            };
            (revision, local_session_id, local_session)
        };
        self.publish_delta(&DeltaEvent::SessionCreated {
            revision,
            session_id: local_session.id.clone(),
            session: local_session.clone(),
        });

        Ok(CreateSessionResponse {
            session_id: local_session_id,
            session: Some(local_session),
            revision,
            state: None,
        })
    }

    /// Proxies remote archive Codex thread.
    fn proxy_remote_archive_codex_thread(
        &self,
        session_id: &str,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/codex/thread/archive",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            None,
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    /// Proxies remote unarchive Codex thread.
    fn proxy_remote_unarchive_codex_thread(
        &self,
        session_id: &str,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/codex/thread/unarchive",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            None,
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    /// Proxies remote compact Codex thread.
    fn proxy_remote_compact_codex_thread(
        &self,
        session_id: &str,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/codex/thread/compact",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            None,
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    /// Proxies remote rollback Codex thread.
    fn proxy_remote_rollback_codex_thread(
        &self,
        session_id: &str,
        num_turns: usize,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/codex/thread/rollback",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            Some(json!({ "numTurns": num_turns })),
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    /// Proxies remote session settings.
    fn proxy_remote_session_settings(
        &self,
        session_id: &str,
        request: UpdateSessionSettingsRequest,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/settings",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            Some(json!({
                "name": request.name,
                "model": request.model,
                "approvalPolicy": request.approval_policy,
                "reasoningEffort": request.reasoning_effort,
                "sandboxMode": request.sandbox_mode,
                "cursorMode": request.cursor_mode,
                "claudeApprovalMode": request.claude_approval_mode,
                "claudeEffort": request.claude_effort,
                "geminiApprovalMode": request.gemini_approval_mode,
            })),
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    /// Proxies remote refresh session model options.
    fn proxy_remote_refresh_session_model_options(
        &self,
        session_id: &str,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/model-options/refresh",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            None,
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    /// Proxies remote turn dispatch.
    fn proxy_remote_turn_dispatch(
        &self,
        session_id: &str,
        request: SendMessageRequest,
    ) -> Result<(), ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/messages",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            Some(json!({
                "text": request.text,
                "expandedText": request.expanded_text,
                "attachments": request.attachments,
            })),
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(())
    }

    /// Proxies remote cancel queued prompt.
    fn proxy_remote_cancel_queued_prompt(
        &self,
        session_id: &str,
        prompt_id: &str,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/queued-prompts/{}/cancel",
                encode_uri_component(&target.remote_session_id),
                encode_uri_component(prompt_id)
            ),
            &[],
            None,
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    /// Proxies remote stop session.
    fn proxy_remote_stop_session(&self, session_id: &str) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/stop",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            None,
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    /// Proxies remote kill session.
    fn proxy_remote_kill_session(&self, session_id: &str) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/kill",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            None,
        )?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let applied_remote_revision = apply_remote_state_if_newer_locked(
            &mut inner,
            &target.remote.id,
            &remote_state,
            Some(&target.remote_session_id),
        );
        let removed = if let Some(index) = inner.find_session_index(&target.local_session_id) {
            inner.remove_session_at(index);
            true
        } else {
            false
        };
        if applied_remote_revision {
            inner.note_remote_applied_revision(&target.remote.id, remote_state.revision);
        }
        if applied_remote_revision || removed {
            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist remote session removal: {err:#}"))
            })?;
        }
        Ok(self.snapshot_from_inner(&inner))
    }

    /// Proxies remote update approval.
    fn proxy_remote_update_approval(
        &self,
        session_id: &str,
        message_id: &str,
        decision: ApprovalDecision,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/approvals/{}",
                encode_uri_component(&target.remote_session_id),
                encode_uri_component(message_id)
            ),
            &[],
            Some(json!({ "decision": decision })),
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    /// Proxies remote submit Codex user input.
    fn proxy_remote_submit_codex_user_input(
        &self,
        session_id: &str,
        message_id: &str,
        answers: BTreeMap<String, Vec<String>>,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/user-input/{}",
                encode_uri_component(&target.remote_session_id),
                encode_uri_component(message_id)
            ),
            &[],
            Some(json!({ "answers": answers })),
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    /// Proxies remote submit Codex MCP elicitation.
    fn proxy_remote_submit_codex_mcp_elicitation(
        &self,
        session_id: &str,
        message_id: &str,
        action: McpElicitationAction,
        content: Option<Value>,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/mcp-elicitation/{}",
                encode_uri_component(&target.remote_session_id),
                encode_uri_component(message_id)
            ),
            &[],
            Some(json!({
                "action": action,
                "content": content,
            })),
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    /// Proxies remote submit Codex app request.
    fn proxy_remote_submit_codex_app_request(
        &self,
        session_id: &str,
        message_id: &str,
        result: Value,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/codex/requests/{}",
                encode_uri_component(&target.remote_session_id),
                encode_uri_component(message_id)
            ),
            &[],
            Some(json!({ "result": result })),
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    /// Proxies remote list agent commands.
    fn proxy_remote_list_agent_commands(
        &self,
        session_id: &str,
    ) -> Result<AgentCommandsResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        self.remote_registry.request_json(
            &target.remote,
            Method::GET,
            &format!(
                "/api/sessions/{}/agent-commands",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            None,
        )
    }

    /// Proxies remote search instructions.
    fn proxy_remote_search_instructions(
        &self,
        session_id: &str,
        query: &str,
    ) -> Result<InstructionSearchResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        self.remote_registry.request_json(
            &target.remote,
            Method::GET,
            "/api/instructions/search",
            &[
                ("q".to_owned(), query.to_owned()),
                ("sessionId".to_owned(), target.remote_session_id),
            ],
            None,
        )
    }
    /// Syncs remote state for target.
    fn sync_remote_state_for_target(
        &self,
        target: &RemoteSessionTarget,
        remote_state: StateResponse,
    ) -> Result<(), ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if !apply_remote_state_if_newer_locked(
            &mut inner,
            &target.remote.id,
            &remote_state,
            Some(&target.remote_session_id),
        ) {
            return Ok(());
        }
        inner.note_remote_applied_revision(&target.remote.id, remote_state.revision);
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist remote state: {err:#}"))
        })?;
        Ok(())
    }

    /// Proxies remote pause orchestrator instance.
    fn proxy_remote_pause_orchestrator_instance(
        &self,
        target: RemoteOrchestratorTarget,
    ) -> Result<StateResponse, ApiError> {
        self.proxy_remote_orchestrator_state_action(target, "pause")
    }

    /// Proxies remote resume orchestrator instance.
    fn proxy_remote_resume_orchestrator_instance(
        &self,
        target: RemoteOrchestratorTarget,
    ) -> Result<StateResponse, ApiError> {
        self.proxy_remote_orchestrator_state_action(target, "resume")
    }

    /// Proxies remote stop orchestrator instance.
    fn proxy_remote_stop_orchestrator_instance(
        &self,
        target: RemoteOrchestratorTarget,
    ) -> Result<StateResponse, ApiError> {
        self.proxy_remote_orchestrator_state_action(target, "stop")
    }

    /// Proxies remote orchestrator lifecycle action.
    fn proxy_remote_orchestrator_state_action(
        &self,
        target: RemoteOrchestratorTarget,
        action: &str,
    ) -> Result<StateResponse, ApiError> {
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/orchestrators/{}/{}",
                encode_uri_component(&target.remote_orchestrator_id),
                action
            ),
            &[],
            None,
        )?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if apply_remote_state_if_newer_locked(&mut inner, &target.remote.id, &remote_state, None)
        {
            inner.note_remote_applied_revision(&target.remote.id, remote_state.revision);
            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!(
                    "failed to persist remote orchestrator `{}` state: {err:#}",
                    target.local_instance_id
                ))
            })?;
        }
        Ok(self.snapshot_from_inner(&inner))
    }

    /// Applies remote state snapshot.
    fn apply_remote_state_snapshot(
        &self,
        remote_id: &str,
        remote_state: StateResponse,
    ) -> Result<(), ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if !apply_remote_state_if_newer_locked(&mut inner, remote_id, &remote_state, None) {
            return Ok(());
        }
        inner.note_remote_applied_revision(remote_id, remote_state.revision);
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist remote state: {err:#}"))
        })?;
        Ok(())
    }

    /// Applies remote delta event.
    fn apply_remote_delta_event(
        &self,
        remote_id: &str,
        event: DeltaEvent,
    ) -> Result<(), anyhow::Error> {
        let remote_revision = delta_event_revision(&event);
        match event {
            DeltaEvent::SessionCreated {
                session,
                session_id,
                ..
            } => {
                if session.id != session_id {
                    return Err(anyhow!(
                        "remote created session payload id `{}` did not match event id `{session_id}`",
                        session.id
                    ));
                }
                let (local_session, revision) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                        return Ok(());
                    }
                    let local_project_ids_by_remote_project_id =
                        remote_project_id_map(&inner, remote_id);
                    let local_project_id = local_project_id_for_remote_project(
                        &local_project_ids_by_remote_project_id,
                        session.project_id.as_deref(),
                    );
                    let (local_session_id, changed) = ensure_remote_proxy_session_record(
                        &mut inner,
                        remote_id,
                        &session,
                        local_project_id,
                        true,
                    );
                    let local_record = inner
                        .find_session_index(&local_session_id)
                        .and_then(|index| inner.sessions.get(index))
                        .cloned()
                        .ok_or_else(|| {
                            anyhow!("local proxy session `{local_session_id}` not found")
                        })?;
                    let local_session = local_record.session.clone();
                    let revision = if changed {
                        self.commit_session_created_locked(&mut inner, &local_record)?
                    } else {
                        inner.revision
                    };
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (local_session, revision)
                };
                self.publish_delta(&DeltaEvent::SessionCreated {
                    revision,
                    session_id: local_session.id.clone(),
                    session: local_session,
                });
            }
            DeltaEvent::MessageCreated {
                message,
                message_id,
                message_index,
                preview,
                session_id,
                status,
                ..
            } => {
                let (local_session_id, revision) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                        return Ok(());
                    }
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let record = inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid");
                    if message_index_on_record(record, &message_id).is_none() {
                        insert_message_on_record(record, message_index, message.clone());
                    }
                    record.session.preview = preview.clone();
                    record.session.status = status;
                    let local_session_id = record.session.id.clone();
                    let revision = self.commit_persisted_delta_locked(&mut inner)?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (local_session_id, revision)
                };
                self.publish_delta(&DeltaEvent::MessageCreated {
                    revision,
                    session_id: local_session_id,
                    message_id,
                    message_index,
                    message,
                    preview,
                    status,
                });
            }
            DeltaEvent::TextDelta {
                delta,
                message_id,
                preview,
                session_id,
                ..
            } => {
                let (local_session_id, message_index, revision) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                        return Ok(());
                    }
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let record = inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid");
                    let message_index = message_index_on_record(record, &message_id)
                        .ok_or_else(|| anyhow!("remote message `{message_id}` not found"))?;
                    let Some(message) = record.session.messages.get_mut(message_index) else {
                        return Err(anyhow!(
                            "remote message index `{message_index}` is out of bounds"
                        ));
                    };
                    match message {
                        Message::Text { text, .. } => text.push_str(&delta),
                        _ => {
                            return Err(anyhow!(
                                "remote message `{message_id}` is not a text message"
                            ));
                        }
                    }
                    if let Some(next_preview) = preview.as_ref() {
                        record.session.preview = next_preview.clone();
                    }
                    let local_session_id = record.session.id.clone();
                    let revision = self.commit_delta_locked(&mut inner)?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (local_session_id, message_index, revision)
                };
                self.publish_delta(&DeltaEvent::TextDelta {
                    revision,
                    session_id: local_session_id,
                    message_id,
                    message_index,
                    delta,
                    preview,
                });
            }
            DeltaEvent::TextReplace {
                message_id,
                preview,
                session_id,
                text,
                ..
            } => {
                let (local_session_id, message_index, revision) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                        return Ok(());
                    }
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let record = inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid");
                    let message_index = message_index_on_record(record, &message_id)
                        .ok_or_else(|| anyhow!("remote message `{message_id}` not found"))?;
                    let Some(message) = record.session.messages.get_mut(message_index) else {
                        return Err(anyhow!(
                            "remote message index `{message_index}` is out of bounds"
                        ));
                    };
                    match message {
                        Message::Text {
                            text: current_text, ..
                        } => {
                            current_text.clear();
                            current_text.push_str(&text);
                        }
                        _ => {
                            return Err(anyhow!(
                                "remote message `{message_id}` is not a text message"
                            ));
                        }
                    }
                    if let Some(next_preview) = preview.as_ref() {
                        record.session.preview = next_preview.clone();
                    }
                    let local_session_id = record.session.id.clone();
                    let revision = self.commit_delta_locked(&mut inner)?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (local_session_id, message_index, revision)
                };
                self.publish_delta(&DeltaEvent::TextReplace {
                    revision,
                    session_id: local_session_id,
                    message_id,
                    message_index,
                    text,
                    preview,
                });
            }
            DeltaEvent::CommandUpdate {
                command,
                command_language,
                message_id,
                message_index,
                output,
                output_language,
                preview,
                session_id,
                status,
                ..
            } => {
                let (local_session_id, created_message, revision, session_status) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                        return Ok(());
                    }
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let record = inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid");
                    let created_message = if let Some(existing_index) =
                        message_index_on_record(record, &message_id)
                    {
                        let Some(message) = record.session.messages.get_mut(existing_index) else {
                            return Err(anyhow!(
                                "remote message index `{existing_index}` is out of bounds"
                            ));
                        };
                        match message {
                            Message::Command {
                                command: existing_command,
                                command_language: existing_command_language,
                                output: existing_output,
                                output_language: existing_output_language,
                                status: existing_status,
                                ..
                            } => {
                                *existing_command = command.clone();
                                *existing_command_language = command_language.clone();
                                *existing_output = output.clone();
                                *existing_output_language = output_language.clone();
                                *existing_status = status;
                                None
                            }
                            _ => {
                                return Err(anyhow!(
                                    "remote message `{message_id}` is not a command message"
                                ));
                            }
                        }
                    } else {
                        let message = Message::Command {
                            id: message_id.clone(),
                            timestamp: stamp_now(),
                            author: Author::Assistant,
                            command: command.clone(),
                            command_language: command_language.clone(),
                            output: output.clone(),
                            output_language: output_language.clone(),
                            status,
                        };
                        insert_message_on_record(record, message_index, message.clone());
                        Some(message)
                    };
                    record.session.preview = preview.clone();
                    let local_session_id = record.session.id.clone();
                    let session_status = record.session.status;
                    let revision = if created_message.is_some() {
                        self.commit_persisted_delta_locked(&mut inner)?
                    } else {
                        self.commit_delta_locked(&mut inner)?
                    };
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (local_session_id, created_message, revision, session_status)
                };
                if let Some(message) = created_message {
                    self.publish_delta(&DeltaEvent::MessageCreated {
                        revision,
                        session_id: local_session_id,
                        message_id,
                        message_index,
                        message,
                        preview,
                        status: session_status,
                    });
                } else {
                    self.publish_delta(&DeltaEvent::CommandUpdate {
                        revision,
                        session_id: local_session_id,
                        message_id,
                        message_index,
                        command,
                        command_language,
                        output,
                        output_language,
                        status,
                        preview,
                    });
                }
            }
            DeltaEvent::ParallelAgentsUpdate {
                agents,
                message_id,
                message_index,
                preview,
                session_id,
                ..
            } => {
                let (local_session_id, created_message, revision, session_status) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                        return Ok(());
                    }
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let record = inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid");
                    let created_message = if let Some(existing_index) =
                        message_index_on_record(record, &message_id)
                    {
                        let Some(message) = record.session.messages.get_mut(existing_index) else {
                            return Err(anyhow!(
                                "remote message index `{existing_index}` is out of bounds"
                            ));
                        };
                        match message {
                            Message::ParallelAgents {
                                agents: existing_agents,
                                ..
                            } => {
                                *existing_agents = agents.clone();
                                None
                            }
                            _ => {
                                return Err(anyhow!(
                                    "remote message `{message_id}` is not a parallel-agents message"
                                ));
                            }
                        }
                    } else {
                        let message = Message::ParallelAgents {
                            id: message_id.clone(),
                            timestamp: stamp_now(),
                            author: Author::Assistant,
                            agents: agents.clone(),
                        };
                        insert_message_on_record(record, message_index, message.clone());
                        Some(message)
                    };
                    record.session.preview = preview.clone();
                    let local_session_id = record.session.id.clone();
                    let session_status = record.session.status;
                    let revision = if created_message.is_some() {
                        self.commit_persisted_delta_locked(&mut inner)?
                    } else {
                        self.commit_delta_locked(&mut inner)?
                    };
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (local_session_id, created_message, revision, session_status)
                };
                if let Some(message) = created_message {
                    self.publish_delta(&DeltaEvent::MessageCreated {
                        revision,
                        session_id: local_session_id,
                        message_id,
                        message_index,
                        message,
                        preview,
                        status: session_status,
                    });
                } else {
                    self.publish_delta(&DeltaEvent::ParallelAgentsUpdate {
                        revision,
                        session_id: local_session_id,
                        message_id,
                        message_index,
                        agents,
                        preview,
                    });
                }
            }
            DeltaEvent::OrchestratorsUpdated {
                orchestrators,
                sessions,
                ..
            } => {
                let (revision, localized_orchestrators) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                        return Ok(());
                    }
                    let local_project_ids_by_remote_project_id =
                        remote_project_id_map(&inner, remote_id);
                    let remote_sessions_by_id = (!sessions.is_empty()).then(|| {
                        sessions
                            .iter()
                            .map(|session| (session.id.as_str(), session))
                            .collect::<HashMap<_, _>>()
                    });
                    let rollback_state = (
                        inner.next_session_number,
                        inner.sessions.clone(),
                        inner.orchestrator_instances.clone(),
                    );
                    if let Err(err) = sync_remote_orchestrators_inner(
                        &mut inner,
                        remote_id,
                        &orchestrators,
                        &local_project_ids_by_remote_project_id,
                        remote_sessions_by_id.as_ref(),
                    ) {
                        inner.next_session_number = rollback_state.0;
                        inner.sessions = rollback_state.1;
                        inner.orchestrator_instances = rollback_state.2;
                        return Err(err);
                    }
                    let revision = self.commit_persisted_delta_locked(&mut inner)?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (revision, inner.orchestrator_instances.clone())
                };
                self.publish_orchestrators_updated(revision, localized_orchestrators);
            }
        }
        Ok(())
    }
}


