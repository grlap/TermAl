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

    /// Handles reconcile.
    fn reconcile(&self, remotes: &[RemoteConfig]) {
        let next_by_id = remotes
            .iter()
            .map(|remote| (remote.id.clone(), remote.clone()))
            .collect::<HashMap<_, _>>();
        let mut connections = self
            .connections
            .lock()
            .expect("remote registry mutex poisoned");
        let existing_ids = connections.keys().cloned().collect::<Vec<_>>();
        for remote_id in existing_ids {
            let Some(connection) = connections.get(&remote_id).cloned() else {
                continue;
            };
            let Some(remote) = next_by_id.get(&remote_id).cloned() else {
                connection.stop_event_bridge();
                connections.remove(&remote_id);
                continue;
            };
            connection.update_config(remote);
        }
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
        let connection = self.connection(remote);
        let base_url = connection.ensure_available(self.client.client())?;
        let url = format!("{base_url}{path}");
        let mut request = self
            .client
            .client()
            .request(method, &url)
            .timeout(REMOTE_REQUEST_TIMEOUT);
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
        decode_remote_json(response)
    }

    /// Starts event bridge.
    fn start_event_bridge(&self, state: AppState, remote: &RemoteConfig) {
        let connection = self.connection(remote);
        connection.start_event_bridge(self.client.client().clone(), state);
    }
}

/// Represents remote connection.
struct RemoteConnection {
    config: Mutex<RemoteConfig>,
    forwarded_port: u16,
    process: Mutex<Option<RemoteProcessHandle>>,
    event_bridge_started: AtomicBool,
    event_bridge_shutdown: AtomicBool,
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
        }
    }

    /// Handles config.
    fn config(&self) -> RemoteConfig {
        self.config
            .lock()
            .expect("remote config mutex poisoned")
            .clone()
    }

    /// Updates config.
    fn update_config(&self, remote: RemoteConfig) {
        let mut config = self.config.lock().expect("remote config mutex poisoned");
        if *config != remote {
            *config = remote;
            drop(config);
            self.disconnect();
        }
    }

    /// Handles disconnect.
    fn disconnect(&self) {
        let mut process = self.process.lock().expect("remote process mutex poisoned");
        if let Some(mut handle) = process.take() {
            let _ = handle.child.kill();
            let _ = handle.child.wait();
        }
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

        if remote_healthcheck(client, &base_url).is_ok() {
            return Ok(base_url);
        }

        let mut process = self.process.lock().expect("remote process mutex poisoned");
        if let Some(handle) = process.as_mut() {
            match handle.child.try_wait() {
                Ok(Some(_)) => {
                    *process = None;
                }
                Ok(None) => {
                    if remote_healthcheck(client, &base_url).is_ok() {
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
            Ok(handle) => {
                *process = Some(handle);
                Ok(base_url)
            }
            Err(managed_error) => {
                let tunnel_attempt = self.start_process(&remote, RemoteProcessMode::TunnelOnly)?;
                match wait_for_remote_health(client, &base_url, tunnel_attempt) {
                    Ok(handle) => {
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
                    state.clear_remote_sse_fallback_resync(&remote.id);
                    if connection.wait_for_bridge_retry_or_shutdown(REMOTE_EVENT_RETRY_DELAY) {
                        break;
                    }
                    continue;
                }

                let base_url = match connection.ensure_available(&client) {
                    Ok(base_url) => base_url,
                    Err(_) => {
                        state.clear_remote_sse_fallback_resync(&remote.id);
                        if connection.wait_for_bridge_retry_or_shutdown(REMOTE_EVENT_RETRY_DELAY) {
                            break;
                        }
                        continue;
                    }
                };

                let response = match client.get(format!("{base_url}/api/events")).send() {
                    Ok(response) => response,
                    Err(_) => {
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
            .state
            .sessions
            .iter()
            .find(|session| session.id == remote_response.session_id)
            .cloned()
            .ok_or_else(|| {
                ApiError::bad_gateway("remote session was not returned by remote state")
            })?;
        let local_session_id = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let local_session_id = upsert_remote_proxy_session_record(
                &mut inner,
                &binding.remote.id,
                &remote_session,
                Some(binding.local_project_id),
            );
            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist remote session proxy: {err:#}"))
            })?;
            local_session_id
        };

        Ok(CreateSessionResponse {
            session_id: local_session_id,
            state: self.snapshot(),
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
            .state
            .sessions
            .iter()
            .find(|session| session.id == remote_response.session_id)
            .cloned()
            .ok_or_else(|| ApiError::bad_gateway("remote forked session was not returned"))?;
        let local_project_id = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&target.local_session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            inner.sessions[index].session.project_id.clone()
        };
        let local_session_id = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            sync_remote_state_inner(
                &mut inner,
                &target.remote.id,
                &remote_response.state,
                Some(&target.remote_session_id),
            );
            let local_session_id = upsert_remote_proxy_session_record(
                &mut inner,
                &target.remote.id,
                &remote_session,
                local_project_id,
            );
            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!(
                    "failed to persist remote forked session proxy: {err:#}"
                ))
            })?;
            local_session_id
        };

        Ok(CreateSessionResponse {
            session_id: local_session_id,
            state: self.snapshot(),
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
        sync_remote_state_inner(
            &mut inner,
            &target.remote.id,
            &remote_state,
            Some(&target.remote_session_id),
        );
        if let Some(index) = inner.find_session_index(&target.local_session_id) {
            inner.sessions.remove(index);
        }
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist remote session removal: {err:#}"))
        })?;
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
        sync_remote_state_inner(
            &mut inner,
            &target.remote.id,
            &remote_state,
            Some(&target.remote_session_id),
        );
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
        sync_remote_state_inner(&mut inner, &target.remote.id, &remote_state, None);
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!(
                "failed to persist remote orchestrator `{}` state: {err:#}",
                target.local_instance_id
            ))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    /// Applies remote state snapshot.
    fn apply_remote_state_snapshot(
        &self,
        remote_id: &str,
        remote_state: StateResponse,
    ) -> Result<(), ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        sync_remote_state_inner(&mut inner, remote_id, &remote_state, None);
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
        match event {
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
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let record = &mut inner.sessions[index];
                    if message_index_on_record(record, &message_id).is_none() {
                        insert_message_on_record(record, message_index, message.clone());
                    }
                    record.session.preview = preview.clone();
                    record.session.status = status;
                    let local_session_id = record.session.id.clone();
                    let revision = self.commit_persisted_delta_locked(&mut inner)?;
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
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let record = &mut inner.sessions[index];
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
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let record = &mut inner.sessions[index];
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
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let record = &mut inner.sessions[index];
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
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let record = &mut inner.sessions[index];
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
                    (revision, inner.orchestrator_instances.clone())
                };
                self.publish_orchestrators_updated(revision, localized_orchestrators);
            }
        }
        Ok(())
    }
}
/// Syncs remote state inner.
fn sync_remote_state_inner(
    inner: &mut StateInner,
    remote_id: &str,
    remote_state: &StateResponse,
    focus_remote_session_id: Option<&str>,
) {
    let pre_orchestrator_rollback_state = focus_remote_session_id.is_none().then(|| {
        (
            inner.next_session_number,
            inner.sessions.clone(),
            inner.orchestrator_instances.clone(),
        )
    });

    let mut local_project_ids_by_remote_project_id = HashMap::new();
    for project in &inner.projects {
        if project.remote_id == remote_id {
            if let Some(remote_project_id) = project.remote_project_id.as_deref() {
                local_project_ids_by_remote_project_id
                    .insert(remote_project_id.to_owned(), project.id.clone());
            }
        }
    }

    if focus_remote_session_id.is_none() {
        let live_remote_session_ids = remote_state
            .sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect::<HashSet<_>>();
        inner.sessions.retain(|record| {
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

    for record in &mut inner.sessions {
        if record.remote_id.as_deref() != Some(remote_id) {
            continue;
        }
        let Some(remote_session_id) = record.remote_session_id.as_deref() else {
            continue;
        };
        if focus_remote_session_id.is_some_and(|focus| focus != remote_session_id) {
            continue;
        }
        let Some(remote_session) = remote_state
            .sessions
            .iter()
            .find(|session| session.id == remote_session_id)
        else {
            continue;
        };
        let local_project_id = remote_session
            .project_id
            .as_deref()
            .and_then(|remote_project_id| {
                local_project_ids_by_remote_project_id
                    .get(remote_project_id)
                    .cloned()
            })
            .or_else(|| record.session.project_id.clone());
        apply_remote_session_to_record(record, local_project_id, remote_session);
    }

    let rollback_state = if focus_remote_session_id.is_some() {
        Some((
            inner.next_session_number,
            inner.sessions.clone(),
            inner.orchestrator_instances.clone(),
        ))
    } else {
        pre_orchestrator_rollback_state
    };

    if let Err(err) = sync_remote_orchestrators_inner(
        inner,
        remote_id,
        &remote_state.orchestrators,
        &local_project_ids_by_remote_project_id,
        Some(&remote_sessions_by_id),
    ) {
        if let Some((next_session_number, sessions, orchestrator_instances)) = rollback_state {
            inner.next_session_number = next_session_number;
            inner.sessions = sessions;
            inner.orchestrator_instances = orchestrator_instances;
        }
        eprintln!(
            "remote state warning> failed to sync remote orchestrators for `{remote_id}` at revision {}: {err:#}",
            remote_state.revision
        );
    }
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
            &mut inner.sessions[index],
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
        session,
    };
    sync_codex_thread_state(&mut record);
    inner.sessions.push(record);
    local_session_id
}

/// Handles localize remote session.
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

/// Returns remote project id mapping.
fn remote_project_id_map(inner: &StateInner, remote_id: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for project in &inner.projects {
        if project.remote_id == remote_id {
            if let Some(remote_project_id) = project.remote_project_id.as_deref() {
                map.insert(remote_project_id.to_owned(), project.id.clone());
            }
        }
    }
    map
}

/// Returns the localized project id for a remote project.
fn local_project_id_for_remote_project(
    local_project_ids_by_remote_project_id: &HashMap<String, String>,
    remote_project_id: Option<&str>,
) -> Option<String> {
    remote_project_id.and_then(|project_id| {
        local_project_ids_by_remote_project_id
            .get(project_id)
            .cloned()
    })
}

/// Returns the local session id for a remote session, creating a proxy record
/// when a full snapshot provides the missing session payload.
fn local_session_id_for_remote_session(
    inner: &mut StateInner,
    remote_id: &str,
    remote_session_id: &str,
    remote_sessions_by_id: Option<&HashMap<&str, &Session>>,
    local_project_ids_by_remote_project_id: &HashMap<String, String>,
    fallback_local_project_id: Option<&str>,
) -> Option<String> {
    if let Some(index) = inner.find_remote_session_index(remote_id, remote_session_id) {
        return Some(inner.sessions[index].session.id.clone());
    }

    let remote_session = remote_sessions_by_id?.get(remote_session_id)?;
    let local_project_id = local_project_id_for_remote_project(
        local_project_ids_by_remote_project_id,
        remote_session.project_id.as_deref(),
    )
    .or_else(|| fallback_local_project_id.map(str::to_owned));
    Some(upsert_remote_proxy_session_record(
        inner,
        remote_id,
        remote_session,
        local_project_id,
    ))
}

/// Localizes a remote orchestrator instance.
fn localize_remote_orchestrator_instance(
    inner: &mut StateInner,
    remote_id: &str,
    remote_orchestrator: &OrchestratorInstance,
    local_project_ids_by_remote_project_id: &HashMap<String, String>,
    remote_sessions_by_id: Option<&HashMap<&str, &Session>>,
) -> Result<OrchestratorInstance, anyhow::Error> {
    let (existing_local_instance_id, existing_local_project_id) = inner
        .find_remote_orchestrator_index(remote_id, &remote_orchestrator.id)
        .and_then(|index| inner.orchestrator_instances.get(index))
        .map(|instance| (Some(instance.id.clone()), Some(instance.project_id.clone())))
        .unwrap_or((None, None));
    let local_project_id = local_project_id_for_remote_project(
        local_project_ids_by_remote_project_id,
        Some(remote_orchestrator.project_id.as_str())
            .filter(|project_id| !project_id.trim().is_empty()),
    )
    .or_else(|| {
        local_project_id_for_remote_project(
            local_project_ids_by_remote_project_id,
            remote_orchestrator.template_snapshot.project_id.as_deref(),
        )
    })
    .or(existing_local_project_id);

    let mut session_instances = Vec::with_capacity(remote_orchestrator.session_instances.len());
    for session_instance in &remote_orchestrator.session_instances {
        let local_session_id = local_session_id_for_remote_session(
            inner,
            remote_id,
            &session_instance.session_id,
            remote_sessions_by_id,
            local_project_ids_by_remote_project_id,
            local_project_id.as_deref(),
        )
        .ok_or_else(|| anyhow!("remote session `{}` not found", session_instance.session_id))?;
        session_instances.push(OrchestratorSessionInstance {
            session_id: local_session_id,
            ..session_instance.clone()
        });
    }

    let mut template_snapshot = remote_orchestrator.template_snapshot.clone();
    template_snapshot.project_id = local_project_id.clone();

    let mut pending_transitions = Vec::with_capacity(remote_orchestrator.pending_transitions.len());
    for transition in &remote_orchestrator.pending_transitions {
        let source_session_id = local_session_id_for_remote_session(
            inner,
            remote_id,
            &transition.source_session_id,
            remote_sessions_by_id,
            local_project_ids_by_remote_project_id,
            local_project_id.as_deref(),
        )
        .ok_or_else(|| {
            anyhow!(
                "remote session `{}` not found",
                transition.source_session_id
            )
        })?;
        let destination_session_id = local_session_id_for_remote_session(
            inner,
            remote_id,
            &transition.destination_session_id,
            remote_sessions_by_id,
            local_project_ids_by_remote_project_id,
            local_project_id.as_deref(),
        )
        .ok_or_else(|| {
            anyhow!(
                "remote session `{}` not found",
                transition.destination_session_id
            )
        })?;
        pending_transitions.push(PendingTransition {
            source_session_id,
            destination_session_id,
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
                    local_session_id_for_remote_session(
                        inner,
                        remote_id,
                        session_id,
                        remote_sessions_by_id,
                        local_project_ids_by_remote_project_id,
                        local_project_id.as_deref(),
                    )
                    .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let stopped_session_ids_during_stop = remote_orchestrator
        .stopped_session_ids_during_stop
        .iter()
        .map(|session_id| {
            local_session_id_for_remote_session(
                inner,
                remote_id,
                session_id,
                remote_sessions_by_id,
                local_project_ids_by_remote_project_id,
                local_project_id.as_deref(),
            )
            .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(OrchestratorInstance {
        id: existing_local_instance_id
            .unwrap_or_else(|| format!("orchestrator-instance-{}", Uuid::new_v4())),
        remote_id: Some(remote_id.to_owned()),
        remote_orchestrator_id: Some(remote_orchestrator.id.clone()),
        template_id: remote_orchestrator.template_id.clone(),
        project_id: local_project_id.unwrap_or_default(),
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

/// Syncs remote orchestrators.
fn sync_remote_orchestrators_inner(
    inner: &mut StateInner,
    remote_id: &str,
    remote_orchestrators: &[OrchestratorInstance],
    local_project_ids_by_remote_project_id: &HashMap<String, String>,
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

/// Applies remote scope to query.
fn apply_remote_scope_to_query(scope: &RemoteScope, query: &mut Vec<(String, String)>) {
    if let Some(remote_session_id) = scope.remote_session_id.as_deref() {
        query.push(("sessionId".to_owned(), remote_session_id.to_owned()));
    } else if let Some(remote_project_id) = scope.remote_project_id.as_deref() {
        query.push(("projectId".to_owned(), remote_project_id.to_owned()));
    }
}

/// Applies remote scope to body.
fn apply_remote_scope_to_body(scope: &RemoteScope, body: Value) -> Value {
    let mut object = match body {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    if let Some(remote_session_id) = scope.remote_session_id.as_deref() {
        object.insert(
            "sessionId".to_owned(),
            Value::String(remote_session_id.to_owned()),
        );
    } else if let Some(remote_project_id) = scope.remote_project_id.as_deref() {
        object.insert(
            "projectId".to_owned(),
            Value::String(remote_project_id.to_owned()),
        );
    }
    Value::Object(object)
}

/// Allocates remote forward port.
fn allocate_remote_forward_port() -> u16 {
    loop {
        let current = NEXT_REMOTE_FORWARD_PORT.fetch_add(1, Ordering::SeqCst);
        if current <= REMOTE_FORWARD_PORT_END {
            return current;
        }
        NEXT_REMOTE_FORWARD_PORT.store(REMOTE_FORWARD_PORT_START, Ordering::SeqCst);
    }
}

/// Validates remote connection config.
fn validate_remote_connection_config(remote: &RemoteConfig) -> Result<(), ApiError> {
    if !remote.enabled {
        return Err(ApiError::bad_request(format!(
            "remote `{}` is disabled",
            remote.name
        )));
    }
    validate_remote_id_value(remote.id.trim())?;
    match remote.transport {
        RemoteTransport::Local => Ok(()),
        RemoteTransport::Ssh => {
            normalized_remote_ssh_host(remote)?;
            normalized_remote_ssh_user(remote)?;
            Ok(())
        }
    }
}

/// Handles remote SSH target.
fn remote_ssh_target(remote: &RemoteConfig) -> Result<String, ApiError> {
    let host = normalized_remote_ssh_host(remote)?;
    let user = normalized_remote_ssh_user(remote)?;
    Ok(match user {
        Some(user) => format!("{user}@{host}"),
        None => host,
    })
}

/// Handles remote SSH command args.
fn remote_ssh_command_args(
    remote: &RemoteConfig,
    forwarded_port: u16,
    mode: RemoteProcessMode,
) -> Result<Vec<String>, ApiError> {
    let target = remote_ssh_target(remote)?;
    let mut args = vec![
        "-T".to_owned(),
        "-o".to_owned(),
        "BatchMode=yes".to_owned(),
        "-o".to_owned(),
        "ExitOnForwardFailure=yes".to_owned(),
        "-o".to_owned(),
        "ServerAliveInterval=15".to_owned(),
        "-o".to_owned(),
        "ServerAliveCountMax=3".to_owned(),
        "-p".to_owned(),
        remote.port.unwrap_or(DEFAULT_SSH_REMOTE_PORT).to_string(),
        "-L".to_owned(),
        format!("{forwarded_port}:127.0.0.1:{REMOTE_SERVER_PORT}"),
    ];
    if matches!(mode, RemoteProcessMode::TunnelOnly) {
        args.push("-N".to_owned());
    }
    args.push("--".to_owned());
    args.push(target);
    if matches!(mode, RemoteProcessMode::ManagedServer) {
        args.push("termal".to_owned());
        args.push("server".to_owned());
    }
    Ok(args)
}

/// Returns the normalized remote SSH host.
fn normalized_remote_ssh_host(remote: &RemoteConfig) -> Result<String, ApiError> {
    let host = remote
        .host
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ApiError::bad_request(format!("remote `{}` is missing an SSH host", remote.name))
        })?;
    validate_remote_ssh_host_value(host, &remote.name)?;
    Ok(host.to_owned())
}

/// Returns the normalized remote SSH user.
fn normalized_remote_ssh_user(remote: &RemoteConfig) -> Result<Option<String>, ApiError> {
    let Some(user) = remote
        .user
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    validate_remote_ssh_user_value(user, &remote.name)?;
    Ok(Some(user.to_owned()))
}

/// Validates remote ID value.
fn validate_remote_id_value(id: &str) -> Result<(), ApiError> {
    if id.is_empty() {
        return Err(ApiError::bad_request("remote id cannot be empty"));
    }
    if !id
        .bytes()
        .all(|byte| matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.'))
    {
        return Err(ApiError::bad_request(format!(
            "remote id `{id}` contains unsupported characters",
        )));
    }
    Ok(())
}

/// Validates remote SSH host value.
fn validate_remote_ssh_host_value(host: &str, remote_name: &str) -> Result<(), ApiError> {
    if host.starts_with('-') {
        return Err(ApiError::bad_request(format!(
            "remote `{remote_name}` has an invalid SSH host",
        )));
    }
    if !host.bytes().any(|byte| byte.is_ascii_alphanumeric())
        || !host.bytes().all(|byte| {
            matches!(
                byte,
                b'a'..=b'z'
                    | b'A'..=b'Z'
                    | b'0'..=b'9'
                    | b'.'
                    | b'-'
                    | b'_'
                    | b':'
                    | b'['
                    | b']'
                    | b'%'
            )
        })
    {
        return Err(ApiError::bad_request(format!(
            "remote `{remote_name}` has an invalid SSH host",
        )));
    }
    Ok(())
}

/// Validates remote SSH user value.
fn validate_remote_ssh_user_value(user: &str, remote_name: &str) -> Result<(), ApiError> {
    if user.contains('@')
        || !user.bytes().all(
            |byte| matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'-' | b'_'),
        )
    {
        return Err(ApiError::bad_request(format!(
            "remote `{remote_name}` has an invalid SSH user",
        )));
    }
    Ok(())
}

/// Handles wait for remote health.
fn wait_for_remote_health(
    client: &BlockingHttpClient,
    base_url: &str,
    mut handle: RemoteProcessHandle,
) -> std::result::Result<RemoteProcessHandle, String> {
    let started_at = Instant::now();
    loop {
        if remote_healthcheck(client, base_url).is_ok() {
            return Ok(handle);
        }
        match handle.child.try_wait() {
            Ok(Some(status)) => {
                return Err(format!(
                    "{} exited with {}{}",
                    handle.mode.label(),
                    status,
                    read_process_stderr_suffix(&mut handle.child)
                ));
            }
            Ok(None) => {}
            Err(err) => {
                return Err(format!("failed to inspect SSH process: {err}"));
            }
        }
        if started_at.elapsed() >= REMOTE_STARTUP_TIMEOUT {
            let _ = handle.child.kill();
            let _ = handle.child.wait();
            return Err(format!("{} timed out", handle.mode.label()));
        }
        thread::sleep(REMOTE_HEALTH_POLL_INTERVAL);
    }
}

/// Handles remote connection issue message.
fn remote_connection_issue_message(remote_name: &str) -> String {
    format!(
        "Could not connect to remote \"{remote_name}\" over SSH. Check the host, network, and SSH settings, then try again."
    )
}

/// Handles local SSH start issue message.
fn local_ssh_start_issue_message(remote_name: &str) -> String {
    format!(
        "Could not start the local SSH client for remote \"{remote_name}\". Verify OpenSSH is installed and available on PATH, then try again."
    )
}

/// Reads process stderr suffix.
fn read_process_stderr_suffix(child: &mut Child) -> String {
    let mut detail = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut detail);
    }
    let trimmed = detail.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!(": {trimmed}")
    }
}

/// Handles remote health check.
fn remote_healthcheck(client: &BlockingHttpClient, base_url: &str) -> Result<()> {
    let response = client
        .get(format!("{base_url}/api/health"))
        .timeout(REMOTE_HEALTH_TIMEOUT)
        .send()
        .with_context(|| format!("failed to contact {base_url}/api/health"))?;
    let payload: HealthResponse =
        decode_remote_json(response).map_err(|err| anyhow!(err.message))?;
    if payload.ok {
        Ok(())
    } else {
        Err(anyhow!("remote health endpoint returned ok=false"))
    }
}

/// Decodes remote JSON.
fn decode_remote_json<T: DeserializeOwned>(response: BlockingHttpResponse) -> Result<T, ApiError> {
    let status = response.status();
    let raw = response.text().map_err(|err| {
        ApiError::bad_gateway(format!("failed to read remote response body: {err}"))
    })?;
    if !status.is_success() {
        if let Ok(error) = serde_json::from_str::<ErrorResponse>(&raw) {
            return Err(ApiError::from_status(status, error.error));
        }
        let message = if raw.trim().is_empty() {
            format!("remote request failed with status {}", status.as_u16())
        } else {
            raw
        };
        return Err(ApiError::from_status(status, message));
    }
    serde_json::from_str(&raw)
        .map_err(|err| ApiError::bad_gateway(format!("failed to decode remote response: {err}")))
}

/// Encodes uri component.
fn encode_uri_component(value: &str) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::new();
    for byte in value.bytes() {
        if matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{:02X}", byte);
        }
    }
    encoded
}

impl RemoteProcessMode {
    /// Handles label.
    fn label(self) -> &'static str {
        match self {
            Self::ManagedServer => "managed SSH session",
            Self::TunnelOnly => "SSH tunnel",
        }
    }
}
