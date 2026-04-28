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
                .with_kind(ApiErrorKind::RemoteConnectionUnavailable)
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
                        ))
                        .with_kind(ApiErrorKind::RemoteConnectionUnavailable))
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
        let child = command
            .spawn()
            .map_err(|err| local_ssh_start_error(&remote.name, err))?;
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

fn local_ssh_start_error(remote_name: &str, err: std::io::Error) -> ApiError {
    eprintln!("failed to start SSH connection for remote `{remote_name}`: {err}");
    ApiError::bad_gateway(local_ssh_start_issue_message(remote_name))
        .with_kind(ApiErrorKind::RemoteConnectionUnavailable)
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



