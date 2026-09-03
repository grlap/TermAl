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
/// Session settings can wait up to 55 seconds for acknowledged OpenCode
/// model/effort/mode updates on the owning backend. Leave transport overhead
/// beyond that application deadline so the proxy cannot report failure for a
/// change the remote has already committed.
const REMOTE_SESSION_SETTINGS_TIMEOUT: Duration = Duration::from_secs(60);
const REMOTE_STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const REMOTE_ACTION_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const REMOTE_HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(250);
const REMOTE_EVENT_RETRY_DELAY: Duration = Duration::from_secs(2);
const REMOTE_EVENT_SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(50);
const TERMINAL_REMOTE_STREAM_READ_CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_REMOTE_ERROR_BODY_CHARS: usize = 512;
const MAX_REMOTE_ERROR_BODY_BYTES: usize = 64 * 1024;
const MAX_REMOTE_ACTION_OUTPUT_CHARS: usize = 4_000;
const MAX_REMOTE_ACTION_OUTPUT_BYTES: usize = 64 * 1024;
static NEXT_REMOTE_FORWARD_PORT: AtomicU16 = AtomicU16::new(REMOTE_FORWARD_PORT_START);

/// Represents remote registry.
struct RemoteRegistry {
    client: BlockingHttpClientHandle,
    /// Authoritative settings-owned configurations. Settings publication takes
    /// the state mutex before the registry mutexes, whose order is configs,
    /// desired event bridges, then connections. Registry callers never take
    /// the state mutex while holding a registry mutex.
    configs: Arc<Mutex<HashMap<String, RemoteConfig>>>,
    /// Remote ids whose local proxy state needs an inbound event bridge. The
    /// subscription survives a disabled interval so re-enabling the remote can
    /// restart the bridge without running an idle retry worker while disabled.
    desired_event_bridges: Arc<Mutex<HashSet<String>>>,
    connections: Arc<Mutex<HashMap<String, Arc<RemoteConnection>>>>,
    /// Monotonic publication epoch used by streaming readers to keep the
    /// normal per-chunk authority check lock-free. A changed epoch triggers a
    /// full settings-map comparison before the reader accepts more bytes.
    config_generation: Arc<AtomicU64>,
    /// One-shot deterministic interleaving seam for tests that must publish a
    /// settings cycle after JSON decoding but before the caller localizes the
    /// response. Production builds contain no hook or branch.
    #[cfg(test)]
    test_after_json_decode: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>,
    /// One-shot deterministic interleaving seam for tests that replace remote
    /// authority after a bridge frame's first fence but before bounded delta
    /// hydration resolves its target. Production builds contain no hook.
    #[cfg(test)]
    test_before_remote_delta_hydration_target:
        Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>,
    /// One-shot deterministic interleaving seam for tests that delete or
    /// rebind a project after an existing-project fast path first resolves it
    /// but before that path performs its final state-locked revalidation.
    #[cfg(test)]
    test_before_existing_remote_project_revalidation:
        Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>,
    /// One-shot deterministic interleaving seam for tests that arm remote
    /// persistence debt after the apply entry retry but before an
    /// informational delta records its consumed revision.
    #[cfg(test)]
    test_before_remote_informational_delta_watermark:
        Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>,
}

/// Result of publishing a new settings-owned remote list. Connections are
/// removed from the shared cache and marked retired while registry locks are
/// held, but process teardown is deliberately deferred until after the caller
/// releases the application state mutex.
struct RemoteConfigPublication {
    changed_ids: Vec<String>,
    retired_connections: Vec<Arc<RemoteConnection>>,
    bridges_to_restart: Vec<String>,
}

/// A request-scoped route pinned to the authoritative config observed during
/// registry lookup. Cached connections are never retargeted in place.
#[derive(Clone)]
struct RemoteRequestLease {
    connection: Arc<RemoteConnection>,
    pinned: RemoteConfig,
    config_generation: u64,
    /// Same-connection state-continuity epoch captured at issuance. Event
    /// bridge cleanup advances this epoch when it clears recovery watermarks,
    /// preventing a pre-cleanup response from mutating proxy state afterward.
    state_continuity_generation: u64,
}

/// Cloneable authority fence for consumers that buffer bytes after the HTTP
/// body has already passed its producer-side route checks.
#[derive(Clone)]
struct RemoteStreamingAuthority {
    lease: RemoteRequestLease,
    configs: Arc<Mutex<HashMap<String, RemoteConfig>>>,
    config_generation: Arc<AtomicU64>,
    observed_generation: Arc<AtomicU64>,
    /// One-shot deterministic seam after the optimistic terminal dispatch
    /// check but before its authoritative locked enqueue. Production builds
    /// contain no hook or branch.
    #[cfg(test)]
    test_before_terminal_event_enqueue: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>,
}

impl RemoteStreamingAuthority {
    fn new(
        lease: RemoteRequestLease,
        configs: Arc<Mutex<HashMap<String, RemoteConfig>>>,
        config_generation: Arc<AtomicU64>,
    ) -> Self {
        let observed_generation = lease.config_generation;
        Self {
            lease,
            configs,
            config_generation,
            observed_generation: Arc::new(AtomicU64::new(observed_generation)),
            #[cfg(test)]
            test_before_terminal_event_enqueue: Arc::new(Mutex::new(None)),
        }
    }

    fn ensure_current(&self) -> Result<(), ApiError> {
        // Retirement is a monotonic cross-thread authority fence. Keep every
        // access SeqCst so request, bridge, and localization audits all reason
        // about the same ordering contract.
        if self.lease.connection.retired.load(Ordering::SeqCst) {
            // Resolve settings first so removal keeps the same 400
            // unknown-remote contract as ordinary request leases. A replaced
            // or name-only-retired route remains a retryable 409.
            let configs = self
                .configs
                .lock()
                .expect("remote registry config mutex poisoned");
            ensure_remote_routing_config(
                configs.get(&self.lease.pinned.id),
                &self.lease.pinned,
            )?;
            return Err(ApiError::conflict(
                REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST,
            ));
        }
        let published_generation = self.config_generation.load(Ordering::Acquire);
        if self.observed_generation.load(Ordering::Acquire) == published_generation {
            return Ok(());
        }

        let configs = self
            .configs
            .lock()
            .expect("remote registry config mutex poisoned");
        ensure_remote_routing_config(configs.get(&self.lease.pinned.id), &self.lease.pinned)?;
        self.lease
            .connection
            .ensure_pinned_route(&self.lease.pinned)?;
        // Publication increments the epoch while this same configs mutex is
        // held, so this stores the exact map version that was just checked.
        self.observed_generation.store(
            self.config_generation.load(Ordering::Acquire),
            Ordering::Release,
        );
        Ok(())
    }

    /// Runs a nonblocking operation while settings publication is excluded.
    /// The config lookup and immutable-connection retirement fence execute
    /// under the same configs lock that publication takes before retiring a
    /// connection, so the operation is ordered entirely before or after a
    /// route change.
    fn with_current<T>(&self, operation: impl FnOnce() -> T) -> Result<T, ApiError> {
        let configs = self
            .configs
            .lock()
            .expect("remote registry config mutex poisoned");
        ensure_remote_routing_config(configs.get(&self.lease.pinned.id), &self.lease.pinned)?;
        self.lease
            .connection
            .ensure_pinned_route(&self.lease.pinned)?;
        let result = operation();
        self.observed_generation.store(
            self.config_generation.load(Ordering::Acquire),
            Ordering::Release,
        );
        drop(configs);
        Ok(result)
    }

    #[cfg(test)]
    fn set_test_before_terminal_event_enqueue(&self, hook: impl FnOnce() + Send + 'static) {
        *self
            .test_before_terminal_event_enqueue
            .lock()
            .expect("remote terminal event enqueue hook mutex poisoned") = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn run_test_before_terminal_event_enqueue(&self) {
        let hook = self
            .test_before_terminal_event_enqueue
            .lock()
            .expect("remote terminal event enqueue hook mutex poisoned")
            .take();
        if let Some(hook) = hook {
            hook();
        }
    }

    fn prefer_current<T>(&self, result: Result<T, ApiError>) -> Result<T, ApiError> {
        match self.ensure_current() {
            Ok(()) => result,
            Err(authority_error) => Err(authority_error),
        }
    }
}

/// A long-lived response body that retains its route lease. Each read checks
/// both the immutable connection and settings-owned authority so terminal
/// output from a replaced endpoint stops before another chunk is forwarded.
struct RemoteStreamingResponse {
    response: Option<BlockingHttpResponse>,
    authority: RemoteStreamingAuthority,
}

impl RemoteStreamingResponse {
    fn ensure_current(&self) -> Result<(), ApiError> {
        self.authority.ensure_current()
    }

    fn authority(&self) -> RemoteStreamingAuthority {
        self.authority.clone()
    }

    fn decode_json<T: DeserializeOwned>(mut self) -> Result<T, ApiError> {
        self.ensure_current()?;
        let response = self
            .response
            .take()
            .expect("remote streaming response should exist until consumed");
        self.authority.prefer_current(decode_remote_json(response))
    }
}

impl std::ops::Deref for RemoteStreamingResponse {
    type Target = BlockingHttpResponse;

    fn deref(&self) -> &Self::Target {
        self.response
            .as_ref()
            .expect("remote streaming response should exist until consumed")
    }
}

impl std::io::Read for RemoteStreamingResponse {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.ensure_current().map_err(remote_authority_io_error)?;
        let read_result = self
            .response
            .as_mut()
            .expect("remote streaming response should exist until consumed")
            .read(buffer);
        // Check authority even when the body read failed. Tunnel retirement
        // commonly surfaces as an I/O error, but callers need the retryable
        // routing conflict when settings changed during that read.
        self.ensure_current().map_err(remote_authority_io_error)?;
        read_result
    }
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
            configs: Arc::new(Mutex::new(HashMap::from([(
                LOCAL_REMOTE_ID.to_owned(),
                RemoteConfig::local(),
            )]))),
            desired_event_bridges: Arc::new(Mutex::new(HashSet::new())),
            connections: Arc::new(Mutex::new(HashMap::new())),
            config_generation: Arc::new(AtomicU64::new(1)),
            #[cfg(test)]
            test_after_json_decode: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            test_before_remote_delta_hydration_target: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            test_before_existing_remote_project_revalidation: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            test_before_remote_informational_delta_watermark: Arc::new(Mutex::new(None)),
        })
    }

    #[cfg(test)]
    fn set_test_after_json_decode(&self, hook: impl FnOnce() + Send + 'static) {
        *self
            .test_after_json_decode
            .lock()
            .expect("remote JSON decode hook mutex poisoned") = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn run_test_after_json_decode(&self) {
        let hook = self
            .test_after_json_decode
            .lock()
            .expect("remote JSON decode hook mutex poisoned")
            .take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    fn set_test_before_remote_delta_hydration_target(
        &self,
        hook: impl FnOnce() + Send + 'static,
    ) {
        *self
            .test_before_remote_delta_hydration_target
            .lock()
            .expect("remote delta hydration hook mutex poisoned") = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn set_test_before_existing_remote_project_revalidation(
        &self,
        hook: impl FnOnce() + Send + 'static,
    ) {
        *self
            .test_before_existing_remote_project_revalidation
            .lock()
            .expect("existing remote project revalidation hook mutex poisoned") =
            Some(Box::new(hook));
    }

    #[cfg(test)]
    fn run_test_before_existing_remote_project_revalidation(&self) {
        let hook = self
            .test_before_existing_remote_project_revalidation
            .lock()
            .expect("existing remote project revalidation hook mutex poisoned")
            .take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    fn run_test_before_remote_delta_hydration_target(&self) {
        let hook = self
            .test_before_remote_delta_hydration_target
            .lock()
            .expect("remote delta hydration hook mutex poisoned")
            .take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    fn set_test_before_remote_informational_delta_watermark(
        &self,
        hook: impl FnOnce() + Send + 'static,
    ) {
        *self
            .test_before_remote_informational_delta_watermark
            .lock()
            .expect("remote informational delta watermark hook mutex poisoned") =
            Some(Box::new(hook));
    }

    #[cfg(test)]
    fn run_test_before_remote_informational_delta_watermark(&self) {
        let hook = self
            .test_before_remote_informational_delta_watermark
            .lock()
            .expect("remote informational delta watermark hook mutex poisoned")
            .take();
        if let Some(hook) = hook {
            hook();
        }
    }

    /// Publishes the settings-owned remote list before the state mutex is
    /// released. This is deliberately separate from connection teardown: a
    /// request in the interval before `reconcile_connections` still consults
    /// this map and therefore cannot use or install stale routing state.
    fn publish_configs(&self, remotes: &[RemoteConfig]) -> RemoteConfigPublication {
        self.publish_configs_with_event_bridge_rearms(remotes, &[])
    }

    /// Publishes settings authority and restores bridge subscriptions for
    /// remote ids that were removed and later re-added while proxy sessions
    /// still refer to them. Disabled remotes retain the subscription without
    /// starting a retry worker; a later enable edit queues the restart through
    /// the normal changed-config path.
    fn publish_configs_with_event_bridge_rearms(
        &self,
        remotes: &[RemoteConfig],
        event_bridge_rearms: &[String],
    ) -> RemoteConfigPublication {
        let next_by_id = remotes
            .iter()
            .map(|remote| (remote.id.clone(), remote.clone()))
            .collect::<HashMap<_, _>>();
        let mut configs = self
            .configs
            .lock()
            .expect("remote registry config mutex poisoned");
        // Full equality is intentional: even a display-name-only edit keeps
        // the pre-existing behavior of resetting connection continuity. The
        // request authority fence below compares routing fields because names
        // do not determine an endpoint.
        let mut changed_ids = configs
            .iter()
            .filter(|(id, remote)| next_by_id.get(*id) != Some(*remote))
            .map(|(id, _)| id.clone())
            .chain(
                next_by_id
                    .iter()
                    .filter(|(id, remote)| configs.get(*id) != Some(*remote))
                    .map(|(id, _)| id.clone()),
            )
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        changed_ids.sort();

        let mut desired_event_bridges = self
            .desired_event_bridges
            .lock()
            .expect("remote event bridge subscription mutex poisoned");
        desired_event_bridges.extend(
            event_bridge_rearms
                .iter()
                .filter(|remote_id| next_by_id.contains_key(remote_id.as_str()))
                .cloned(),
        );
        let mut connections = self
            .connections
            .lock()
            .expect("remote registry mutex poisoned");
        let mut retired_connections = Vec::new();
        let mut bridges_to_restart = Vec::new();
        for remote_id in &changed_ids {
            if next_by_id
                .get(remote_id)
                .is_some_and(|remote| remote.enabled)
                && desired_event_bridges.contains(remote_id)
            {
                bridges_to_restart.push(remote_id.clone());
            }
            if !next_by_id.contains_key(remote_id) {
                desired_event_bridges.remove(remote_id);
            }
            let Some(connection) = connections.remove(remote_id) else {
                continue;
            };
            connection.retired.store(true, Ordering::SeqCst);
            // Keep an owned Arc so removing the map entry cannot drop the last
            // process-owning reference while either registry mutex is held.
            retired_connections.push(connection);
        }
        *configs = next_by_id;
        if !changed_ids.is_empty() {
            self.config_generation.fetch_add(1, Ordering::AcqRel);
        }
        RemoteConfigPublication {
            changed_ids,
            retired_connections,
            bridges_to_restart,
        }
    }

    /// Finishes a publication after all global/application locks are released.
    /// Child termination can wait on the OS, so it must never run while the
    /// configs or connections mutex is held.
    fn finish_config_publication(&self, publication: RemoteConfigPublication) -> Vec<String> {
        for connection in publication.retired_connections {
            connection.stop_event_bridge();
        }
        publication.bridges_to_restart
    }

    /// Publishes and reconciles a complete remote list. Boot and tests use this
    /// convenience method; settings updates split the phases so only the cheap
    /// publication occurs while holding the state mutex.
    #[cfg(test)]
    fn reconcile(&self, remotes: &[RemoteConfig]) -> Vec<String> {
        let publication = self.publish_configs(remotes);
        let changed_ids = publication.changed_ids.clone();
        self.finish_config_publication(publication);
        changed_ids
    }

    fn ensure_current_config(&self, pinned: &RemoteConfig) -> Result<(), ApiError> {
        let configs = self
            .configs
            .lock()
            .expect("remote registry config mutex poisoned");
        ensure_remote_routing_config(configs.get(&pinned.id), pinned)?;
        // An A -> B -> A settings cycle is accepted here: identical routing
        // bytes identify the same endpoint, while the B publication retires the
        // old cached tunnel so an overlapping request cannot be retargeted.
        Ok(())
    }

    fn ensure_lease_current(&self, lease: &RemoteRequestLease) -> Result<(), ApiError> {
        // Resolve settings authority first so removal keeps the established
        // unknown-remote contract (400 on create flows). Replacement under the
        // same id still returns the retryable 409; an A -> B -> A cycle then
        // reaches the retired-connection check and also fails closed.
        self.ensure_current_config(&lease.pinned)?;
        lease.connection.ensure_pinned_route(&lease.pinned)
    }

    /// Prefer a retryable authority conflict over an operation result whenever
    /// settings changed while the operation was in flight. This applies to
    /// both success and failure paths so tunnel teardown cannot masquerade as
    /// a transport or response-decoding failure.
    fn prefer_current_lease<T>(
        &self,
        lease: &RemoteRequestLease,
        result: Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        match self.ensure_lease_current(lease) {
            Ok(()) => result,
            Err(authority_error) => Err(authority_error),
        }
    }

    /// Removes a cache entry whose immutable connection config disagrees
    /// with settings authority. Callers hold the configs lock and the mutable
    /// connections guard; process teardown remains deferred until both are
    /// released.
    fn retire_stale_cached_connection_locked(
        connections: &mut HashMap<String, Arc<RemoteConnection>>,
        remote_id: &str,
        current: &RemoteConfig,
    ) -> Option<Arc<RemoteConnection>> {
        let connection = connections
            .get(remote_id)
            .filter(|connection| connection.config() != *current)
            .cloned()?;
        connections.remove(remote_id);
        connection.retired.store(true, Ordering::SeqCst);
        Some(connection)
    }

    fn connection(&self, remote: &RemoteConfig) -> Result<RemoteRequestLease, ApiError> {
        // Always gate caller snapshots against settings-owned authority before
        // touching the cache. Keeping the config lock through cache insertion
        // prevents a removed remote from being reinserted by a stale caller.
        let configs = self
            .configs
            .lock()
            .expect("remote registry config mutex poisoned");
        ensure_remote_routing_config(configs.get(&remote.id), remote)?;
        let current = configs
            .get(&remote.id)
            .cloned()
            .expect("validated remote config should remain present while locked");
        let mut connections = self
            .connections
            .lock()
            .expect("remote registry mutex poisoned");
        let config_generation = self.config_generation.load(Ordering::Acquire);
        let retired_connection = Self::retire_stale_cached_connection_locked(
            &mut connections,
            &remote.id,
            &current,
        );
        let connection = connections
            .entry(remote.id.clone())
            .or_insert_with(|| {
                Arc::new(RemoteConnection::new(
                    current.clone(),
                    config_generation,
                ))
            })
            .clone();
        drop(connections);
        drop(configs);
        if let Some(connection) = retired_connection {
            if connection.event_bridge_started.load(Ordering::SeqCst) {
                eprintln!(
                    "remote registry invariant warning> retired a started bridge for `{}` after finding a stale cached route; settings publication should have removed it",
                    remote.id
                );
            }
            connection.stop_event_bridge();
        }
        Ok(RemoteRequestLease {
            state_continuity_generation: connection
                .state_continuity_generation
                .load(Ordering::SeqCst),
            connection,
            pinned: current,
            config_generation,
        })
    }

    fn request_json<T: DeserializeOwned>(
        &self,
        remote: &RemoteConfig,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
    ) -> Result<T, ApiError> {
        self.request_json_with_lease(remote, method, path, query, body)
            .map(|(response, _lease)| response)
    }

    fn request_json_with_lease<T: DeserializeOwned>(
        &self,
        remote: &RemoteConfig,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
    ) -> Result<(T, RemoteRequestLease), ApiError> {
        self.request_json_with_timeout_and_lease(
            remote,
            method,
            path,
            query,
            body,
            REMOTE_REQUEST_TIMEOUT,
        )
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
        self.request_json_with_timeout_and_lease(remote, method, path, query, body, timeout)
            .map(|(response, _lease)| response)
    }

    fn request_json_with_timeout_and_lease<T: DeserializeOwned>(
        &self,
        remote: &RemoteConfig,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
        timeout: Duration,
    ) -> Result<(T, RemoteRequestLease), ApiError> {
        let (response, lease) =
            self.request_with_timeout(remote, method, path, query, body, timeout)?;
        let response = self.prefer_current_lease(&lease, decode_remote_json(response))?;
        #[cfg(test)]
        self.run_test_after_json_decode();
        Ok((response, lease))
    }

    fn request_without_timeout(
        &self,
        remote: &RemoteConfig,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
    ) -> Result<RemoteStreamingResponse, ApiError> {
        let (response, lease) =
            self.request_with_optional_timeout(remote, method, path, query, body, None)?;
        Ok(RemoteStreamingResponse {
            response: Some(response),
            authority: RemoteStreamingAuthority::new(
                lease,
                self.configs.clone(),
                self.config_generation.clone(),
            ),
        })
    }

    fn request_with_timeout(
        &self,
        remote: &RemoteConfig,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
        timeout: Duration,
    ) -> Result<(BlockingHttpResponse, RemoteRequestLease), ApiError> {
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
    ) -> Result<(BlockingHttpResponse, RemoteRequestLease), ApiError> {
        let lease = self.connection(remote)?;
        self.request_with_optional_timeout_for_lease(lease, method, path, query, body, timeout)
    }

    fn request_with_optional_timeout_for_lease(
        &self,
        lease: RemoteRequestLease,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
        timeout: Option<Duration>,
    ) -> Result<(BlockingHttpResponse, RemoteRequestLease), ApiError> {
        self.ensure_lease_current(&lease)?;
        let available = lease
            .connection
            .ensure_available(self.client.client(), &lease.pinned);
        let base_url = self.prefer_current_lease(&lease, available)?;
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
        let response = match request.send() {
            Ok(response) => response,
            Err(err) => {
                self.ensure_lease_current(&lease)?;
                eprintln!(
                    "failed to contact remote `{}` at {}: {err}",
                    lease.pinned.name,
                    lease.pinned.host.as_deref().unwrap_or("unknown host")
                );
                return Err(ApiError::bad_gateway(remote_connection_issue_message(
                    &lease.pinned.name,
                ))
                .with_kind(ApiErrorKind::RemoteConnectionUnavailable));
            }
        };
        self.ensure_lease_current(&lease)?;
        Ok((response, lease))
    }

    /// Claims and starts the bridge for the authoritative config currently
    /// published under `remote_id`. Config lookup, cache insertion, and the
    /// started flag are one configs -> connections critical section: a later
    /// settings publication must therefore either observe the claim and queue
    /// a restart, or win first and make this claim use its newer config.
    fn start_event_bridge_by_id(&self, state: AppState, remote_id: &str) {
        match self.claim_event_bridge(remote_id) {
            Ok(Some(connection)) => {
                connection.spawn_claimed_event_bridge(self.client.client().clone(), state)
            }
            Ok(None) => {}
            Err(err) => {
                eprintln!(
                    "remote event bridge `{remote_id}` was not started: {}",
                    err.message
                );
            }
        }
    }

    /// Claims and starts the bridge only when the exact request lease still
    /// owns the settings-published connection. Remote create responses use
    /// this path before localization so an old endpoint cannot claim the
    /// replacement endpoint's bridge in the post-decode race window.
    fn start_event_bridge_for_lease(
        &self,
        state: AppState,
        lease: &RemoteRequestLease,
    ) -> Result<(), ApiError> {
        if let Some(connection) = self.claim_event_bridge_for_lease(lease)? {
            connection.spawn_claimed_event_bridge(self.client.client().clone(), state);
        }
        Ok(())
    }

    fn claim_event_bridge(&self, remote_id: &str) -> Result<Option<Arc<RemoteConnection>>, ApiError> {
        self.claim_event_bridge_with_expected_lease(remote_id, None, |_| {})
    }

    fn claim_event_bridge_for_lease(
        &self,
        lease: &RemoteRequestLease,
    ) -> Result<Option<Arc<RemoteConnection>>, ApiError> {
        self.claim_event_bridge_with_expected_lease(&lease.pinned.id, Some(lease), |_| {})
    }

    #[cfg(test)]
    fn claim_event_bridge_with_locked_claim(
        &self,
        remote_id: &str,
        on_locked_claim: impl FnOnce(&Arc<RemoteConnection>),
    ) -> Result<Option<Arc<RemoteConnection>>, ApiError> {
        self.claim_event_bridge_with_expected_lease(remote_id, None, on_locked_claim)
    }

    fn claim_event_bridge_with_expected_lease(
        &self,
        remote_id: &str,
        expected_lease: Option<&RemoteRequestLease>,
        on_locked_claim: impl FnOnce(&Arc<RemoteConnection>),
    ) -> Result<Option<Arc<RemoteConnection>>, ApiError> {
        let configs = self
            .configs
            .lock()
            .expect("remote registry config mutex poisoned");
        let current = configs
            .get(remote_id)
            .cloned()
            .ok_or_else(|| ApiError::bad_request(format!("unknown remote `{remote_id}`")))?;
        if let Some(lease) = expected_lease {
            ensure_remote_routing_config(Some(&current), &lease.pinned)?;
            lease.connection.ensure_pinned_route(&lease.pinned)?;
        }
        let mut desired_event_bridges = self
            .desired_event_bridges
            .lock()
            .expect("remote event bridge subscription mutex poisoned");
        if !current.enabled {
            desired_event_bridges.insert(remote_id.to_owned());
            return Ok(None);
        }
        let mut connections = self
            .connections
            .lock()
            .expect("remote registry mutex poisoned");
        let (connection, retired_connection) = if let Some(lease) = expected_lease {
            let connection = connections
                .get(remote_id)
                .filter(|connection| Arc::ptr_eq(connection, &lease.connection))
                .cloned()
                .ok_or_else(|| {
                    ApiError::conflict(REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST)
                })?;
            (connection, None)
        } else {
            // Publication normally removes changed entries before replacing
            // the authoritative map. Share `connection()`'s defensive
            // retirement so a stale cache entry can never be claimed if that
            // invariant breaks.
            let retired_connection = Self::retire_stale_cached_connection_locked(
                &mut connections,
                remote_id,
                &current,
            );
            let connection = connections
                .entry(remote_id.to_owned())
                .or_insert_with(|| {
                    Arc::new(RemoteConnection::new(
                        current,
                        self.config_generation.load(Ordering::Acquire),
                    ))
                })
                .clone();
            (connection, retired_connection)
        };
        desired_event_bridges.insert(remote_id.to_owned());
        connection
            .event_bridge_shutdown
            .store(false, Ordering::SeqCst);
        let already_started = connection.event_bridge_started.swap(true, Ordering::SeqCst);
        // Test seams can pause here to prove that publication cannot observe
        // the cached connection before bridge ownership has been claimed.
        on_locked_claim(&connection);
        drop(connections);
        drop(desired_event_bridges);
        drop(configs);
        if let Some(connection) = retired_connection {
            connection.stop_event_bridge();
        }
        if already_started {
            return Ok(None);
        }
        Ok(Some(connection))
    }
}


/// Represents remote connection.
struct RemoteConnection {
    /// Immutable routing identity captured when this connection is created.
    /// Settings publication retires and replaces the whole connection instead
    /// of mutating an endpoint in place.
    config: RemoteConfig,
    /// Publication generation that owns inbound bridge frames from this
    /// connection. Request leases instead capture the current generation at
    /// issuance, even when they reuse an older still-authoritative connection.
    authority_generation: u64,
    /// Monotonic route-retirement fence; all loads and stores use SeqCst.
    retired: AtomicBool,
    /// Invalidates state-localizing request leases when the event bridge loses
    /// continuity without replacing this connection. Transport-only consumers
    /// deliberately ignore this epoch.
    state_continuity_generation: AtomicU64,
    forwarded_port: u16,
    process: Mutex<Option<RemoteProcessHandle>>,
    event_bridge_started: AtomicBool,
    event_bridge_shutdown: AtomicBool,
}

impl RemoteConnection {
    /// Creates a new instance.
    fn new(remote: RemoteConfig, authority_generation: u64) -> Self {
        Self {
            config: remote,
            authority_generation,
            retired: AtomicBool::new(false),
            state_continuity_generation: AtomicU64::new(1),
            forwarded_port: allocate_remote_forward_port(),
            process: Mutex::new(None),
            event_bridge_started: AtomicBool::new(false),
            event_bridge_shutdown: AtomicBool::new(false),
        }
    }

    fn config(&self) -> RemoteConfig {
        self.config.clone()
    }

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

    fn ensure_pinned_route(&self, pinned: &RemoteConfig) -> Result<(), ApiError> {
        // The config comparison is defensive: the field is immutable, while
        // `retired` is the live publication signal.
        if self.retired.load(Ordering::SeqCst)
            || !same_remote_routing_config(&self.config, pinned)
        {
            return Err(ApiError::conflict(
                REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST,
            ));
        }
        Ok(())
    }

    fn ensure_state_continuity_generation(&self, expected: u64) -> Result<(), ApiError> {
        if self.state_continuity_generation.load(Ordering::SeqCst) != expected {
            return Err(ApiError::conflict(
                REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST,
            ));
        }
        Ok(())
    }

    fn invalidate_state_continuity(&self) {
        self.state_continuity_generation
            .fetch_add(1, Ordering::SeqCst);
    }

    /// Ensures available.
    fn ensure_available(
        &self,
        client: &BlockingHttpClient,
        pinned: &RemoteConfig,
    ) -> Result<String, ApiError> {
        self.ensure_pinned_route(pinned)?;
        validate_remote_connection_config(pinned)?;
        if pinned.transport != RemoteTransport::Ssh {
            return Err(ApiError::bad_request(format!(
                "remote `{}` does not use SSH transport",
                pinned.name
            )));
        }
        let base_url = self.base_url();

        if remote_healthcheck(client, &base_url).is_ok() {
            self.ensure_pinned_route(pinned)?;
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
                        self.ensure_pinned_route(pinned)?;
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

        self.ensure_pinned_route(pinned)?;
        let managed_attempt = self.start_process(pinned, RemoteProcessMode::ManagedServer)?;
        match wait_for_remote_health(client, &base_url, managed_attempt) {
            Ok(mut handle) => {
                if let Err(err) = self.ensure_pinned_route(pinned) {
                    let _ = handle.child.kill();
                    let _ = handle.child.wait();
                    return Err(err);
                }
                *process = Some(handle);
                Ok(base_url)
            }
            Err(managed_error) => {
                self.ensure_pinned_route(pinned)?;
                let tunnel_attempt = self.start_process(pinned, RemoteProcessMode::TunnelOnly)?;
                match wait_for_remote_health(client, &base_url, tunnel_attempt) {
                    Ok(mut handle) => {
                        if let Err(err) = self.ensure_pinned_route(pinned) {
                            let _ = handle.child.kill();
                            let _ = handle.child.wait();
                            return Err(err);
                        }
                        *process = Some(handle);
                        Ok(base_url)
                    }
                    Err(tunnel_error) => {
                        eprintln!(
                            "remote SSH connection failed for `{}`. managed start failed: {}. tunnel-only fallback failed: {}",
                            pinned.name, managed_error, tunnel_error
                        );
                        Err(ApiError::bad_gateway(remote_connection_issue_message(
                            &pinned.name,
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

    /// Starts a bridge whose ownership was claimed atomically by the registry.
    fn spawn_claimed_event_bridge(self: &Arc<Self>, client: BlockingHttpClient, state: AppState) {
        if self.retired.load(Ordering::SeqCst) {
            self.event_bridge_started.store(false, Ordering::SeqCst);
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
            let remote = connection.config();

            loop {
                if connection.event_bridge_shutdown.load(Ordering::SeqCst)
                    || connection.retired.load(Ordering::SeqCst)
                {
                    break;
                }

                // Settings validation normally reserves Local transport for
                // the built-in local authority, which does not need a bridge.
                // Keep this defensive wait path for legacy/test registry
                // entries without attempting to launch SSH for them.
                if remote.transport != RemoteTransport::Ssh {
                    if !state.clear_remote_bridge_continuity_if_current(&remote, &connection) {
                        break;
                    }
                    if connection.wait_for_bridge_retry_or_shutdown(REMOTE_EVENT_RETRY_DELAY) {
                        break;
                    }
                    continue;
                }

                let base_url = match connection.ensure_available(&client, &remote) {
                    Ok(base_url) => base_url,
                    Err(err) => {
                        eprintln!(
                            "remote event bridge `{}` failed to connect: {err:#?}",
                            remote.id
                        );
                        if !state.clear_remote_bridge_continuity_if_current(&remote, &connection) {
                            break;
                        }
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
                        if !state.clear_remote_bridge_continuity_if_current(&remote, &connection) {
                            break;
                        }
                        if connection.wait_for_bridge_retry_or_shutdown(REMOTE_EVENT_RETRY_DELAY) {
                            break;
                        }
                        continue;
                    }
                };

                if let Err(err) =
                    process_remote_event_stream(&state, &remote, &connection, response)
                {
                    eprintln!("remote event bridge `{}` disconnected: {err:#}", remote.id);
                }
                if !state.clear_remote_bridge_continuity_if_current(&remote, &connection) {
                    break;
                }
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


