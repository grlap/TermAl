// Engram's host-private JSON-lines protocol and its process transport.
//
// Owns: protocol serialization, per-session `engram control` children,
// deadline-bounded request/response exchange, and Phase 0 adapter outcomes.
// Does not own: TermAl's dispatch/termination policy, project settings routes,
// transcript mutation, or delegation lifecycle. Those call this seam only
// after releasing the global state mutex.

const ENGRAM_CONTROL_SCHEMA_VERSION: u16 = 1;
const ENGRAM_CAPABILITY_MAP_REVISION: i64 = 1;
const ENGRAM_DEFAULT_CALL_TIMEOUT_MS: u64 = 250;
/// Engram control calls are bounded to ten seconds. Lifecycle arbitration gets
/// one additional second for the owning callback to publish its terminal state.
const ENGRAM_CONTROL_SETTLE_TIMEOUT: Duration = Duration::from_secs(11);
const ENGRAM_DISPATCH_BUDGET_MS: u64 = 600;
// Engram's current store-open path may wait up to five seconds on SQLite's
// writer lock. Bound each command in the two-command focus read above that
// healthy contention window; a timeout or lock error remains an error, never
// `None`.
#[cfg(not(test))]
const ENGRAM_WORK_BINDING_COMMAND_TIMEOUT: Duration = Duration::from_secs(6);
// The Rust suite spawns many fixture processes concurrently. Preserve the
// production lock-contention deadline while giving test subprocess scheduling
// enough headroom under full-suite load.
#[cfg(test)]
const ENGRAM_WORK_BINDING_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const ENGRAM_WORK_BINDING_LOCK_RETRY_DELAY: Duration = Duration::from_millis(250);
const ENGRAM_BOOT_RECOVERY_CONCURRENCY: usize = 8;
const ENGRAM_CONTROL_MAX_FRAME_BYTES: usize = 256 * 1_024;
const ENGRAM_CIRCUIT_BREAKER_FAILURES: u8 = 3;
const ENGRAM_CONTROL_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const ENGRAM_GLOBAL_DISABLE_ENV: &str = "TERMAL_ENGRAM_DISABLED";
const ENGRAM_PHASE_ZERO_ASSURANCE: &str = "advisory";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramAuthorityStoreKey {
    database_path: PathBuf,
    project_id: String,
}

#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramRetiredWorkAuthorityGrant {
    home: String,
    /// Raw project root retained even when `.engram-project` cannot be read.
    /// This lets a later settings repair route the pending revocation without
    /// weakening the grant-hash ingress block.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    project_root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    store_key: Option<EngramAuthorityStoreKey>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    project_id: String,
    grant_hash: String,
    retired_at: String,
    reason: String,
    #[serde(default)]
    revoke_confirmed: bool,
}

impl std::fmt::Debug for EngramRetiredWorkAuthorityGrant {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EngramRetiredWorkAuthorityGrant")
            .field("home", &self.home)
            .field("project_root", &self.project_root)
            .field("store_key", &self.store_key)
            .field("project_id", &self.project_id)
            .field("grant_hash", &"[REDACTED]")
            .field("retired_at", &self.retired_at)
            .field("reason", &self.reason)
            .field("revoke_confirmed", &self.revoke_confirmed)
            .finish()
    }
}

#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramProjectSettings {
    #[serde(default)]
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    binary_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    home: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    work_authority_grant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deadline_ms: Option<u64>,
}

impl std::fmt::Debug for EngramProjectSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EngramProjectSettings")
            .field("enabled", &self.enabled)
            .field("binary_path", &self.binary_path)
            .field("home", &self.home)
            .field(
                "work_authority_grant",
                &self
                    .work_authority_grant
                    .as_ref()
                    .map(|_| "[REDACTED]"),
            )
            .field("deadline_ms", &self.deadline_ms)
            .finish()
    }
}

impl Default for EngramProjectSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            binary_path: None,
            home: None,
            work_authority_grant: None,
            deadline_ms: None,
        }
    }
}

impl EngramProjectSettings {
    fn is_runtime_enabled(&self) -> bool {
        self.enabled && !engram_globally_disabled()
    }

    fn call_timeout(&self) -> Duration {
        Duration::from_millis(
            self.deadline_ms
                .unwrap_or(ENGRAM_DEFAULT_CALL_TIMEOUT_MS)
                .max(1),
        )
    }
}

fn engram_globally_disabled() -> bool {
    std::env::var(ENGRAM_GLOBAL_DISABLE_ENV)
        .ok()
        .is_some_and(|value| engram_disable_env_value_is_truthy(&value))
}

fn engram_disable_env_value_is_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn validate_engram_project_enablement(
    project: &Project,
    settings: &EngramProjectSettings,
) -> std::result::Result<(), ApiError> {
    let project_root = FsPath::new(&project.root_path);
    let project_file = project_root.join(".engram-project");
    let metadata = fs::metadata(&project_file).map_err(|err| {
        ApiError::bad_request(format!(
            "cannot enable Engram: `{}` is missing or unreadable: {err}",
            project_file.display()
        ))
    })?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(ApiError::bad_request(format!(
            "cannot enable Engram: `{}` must be a non-empty file",
            project_file.display()
        )));
    }
    let binary_path = settings
        .binary_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| ApiError::bad_request("cannot enable Engram without binaryPath"))?;
    if !binary_path.is_absolute() || !binary_path.is_file() {
        return Err(ApiError::bad_request(
            "cannot enable Engram: binaryPath must be an existing absolute file",
        ));
    }
    let home = settings
        .home
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| ApiError::bad_request("cannot enable Engram without home"))?;
    if !home.is_absolute() || !home.is_dir() {
        return Err(ApiError::bad_request(
            "cannot enable Engram: home must be an existing absolute directory",
        ));
    }
    run_engram_doctor(&binary_path, &project_file, &home, project_root)
}

fn run_engram_doctor(
    binary_path: &FsPath,
    project_file: &FsPath,
    home: &FsPath,
    project_root: &FsPath,
) -> std::result::Result<(), ApiError> {
    let mut command = engram_command(binary_path);
    let mut child = command
        .arg("--project-file")
        .arg(project_file)
        .arg("--home")
        .arg(home)
        .arg("doctor")
        .current_dir(project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| ApiError::bad_request(format!("Engram doctor failed to start: {err}")))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child.wait_with_output().map_err(|err| {
                    ApiError::bad_request(format!("failed collecting Engram doctor output: {err}"))
                })?;
                if status.success() {
                    return validate_engram_doctor_assurance(&output.stdout, &output.stderr);
                }
                let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                return Err(ApiError::bad_request(if detail.is_empty() {
                    format!("Engram doctor exited with {status}")
                } else {
                    format!("Engram doctor failed: {detail}")
                }));
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ApiError::bad_request(
                    "Engram doctor exceeded the 5 second enablement deadline",
                ));
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ApiError::bad_request(format!(
                    "failed waiting for Engram doctor: {err}"
                )));
            }
        }
    }
}

fn validate_engram_doctor_assurance(
    stdout: &[u8],
    stderr: &[u8],
) -> std::result::Result<(), ApiError> {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    let required = stdout
        .split_whitespace()
        .chain(stderr.split_whitespace())
        .find_map(|field| {
            let (name, value) = field.split_once('=')?;
            name.trim_matches(|character: char| !character.is_ascii_alphanumeric())
                .eq_ignore_ascii_case("required")
                .then(|| {
                    value
                        .trim_matches(|character: char| {
                            !character.is_ascii_alphanumeric() && character != '_'
                        })
                        .to_owned()
                })
        })
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ApiError::bad_request(
                "cannot enable Engram: doctor succeeded but did not report `required=` control assurance; TermAl Phase 0 requires `required=Advisory`",
            )
        })?;
    let normalized = required
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    match normalized.as_str() {
        "advisory" => Ok(()),
        "turngated" | "actiongated" => Err(ApiError::bad_request(format!(
            "cannot enable Engram: control_assurance_insufficient: doctor requires `{required}`, but TermAl Phase 0 provides only `{ENGRAM_PHASE_ZERO_ASSURANCE}`; configure the Engram store with `required=Advisory` or disable the integration"
        ))),
        _ => Err(ApiError::bad_request(format!(
            "cannot enable Engram: doctor reported unsupported control assurance `required={required}`; TermAl Phase 0 recognizes Advisory, TurnGated, and ActionGated"
        ))),
    }
}

fn engram_command(binary_path: &FsPath) -> Command {
    let extension = binary_path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();

    #[cfg(windows)]
    if extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat") {
        let mut command = Command::new("cmd.exe");
        command.args(["/D", "/S", "/C"]);
        command.arg(binary_path);
        return command;
    }

    #[cfg(windows)]
    if extension.eq_ignore_ascii_case("ps1") {
        let mut command = Command::new("powershell.exe");
        command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"]);
        command.arg(binary_path);
        return command;
    }

    #[cfg(not(windows))]
    if extension.eq_ignore_ascii_case("sh") {
        let mut command = Command::new("sh");
        command.arg(binary_path);
        return command;
    }

    Command::new(binary_path)
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramEffect {
    Observe,
    Communicate,
    MutateLocal,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramNextIntent {
    Continue,
    Wait,
    Exit,
}

impl EngramNextIntent {
    fn as_idempotency_component(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Wait => "wait",
            Self::Exit => "exit",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum EngramControlRequest {
    SessionBind {
        external_ref: String,
        title: String,
        assurance: String,
        mediated_effects: Vec<EngramEffect>,
        capability_map_revision: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        work_binding: Option<EngramControlWorkBinding>,
        idempotency_key: String,
    },
    SessionStatus {
        routing_token: String,
    },
    TurnEvaluate {
        routing_token: String,
        idempotency_key: String,
        intent_fingerprint: String,
        purpose: String,
        requested_effects: Vec<EngramEffect>,
        resource_intents: Vec<Value>,
    },
    TurnBegin {
        routing_token: String,
        grant_id: String,
        delivery_tokens: Vec<String>,
        idempotency_key: String,
    },
    TurnCheckpoint {
        routing_token: String,
        grant_id: String,
        next_intent: EngramNextIntent,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        observations: Vec<EngramExecutionObservationInput>,
        idempotency_key: String,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramControlWorkBinding {
    root_execution_id: String,
    work_id: String,
    run_id: String,
    work_revision: i64,
    claim_id: String,
    claim_fence: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramExecutionObservationInput {
    observation_id: String,
    action_fingerprint: String,
    effect: EngramEffect,
    outcome: EngramExecutionOutcome,
    source_changed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_basis: Option<EngramExecutionSourceBasis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed_at: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramExecutionOutcome {
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramExecutionSourceBasis {
    workspace_id: String,
    source_revision: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum EngramControlResponse {
    Ok { result: Value },
    Error { error: EngramControlErrorBody },
}

#[derive(Clone, Debug, Deserialize)]
struct EngramControlErrorBody {
    code: String,
    message: String,
}

#[derive(Clone, Debug, Deserialize)]
struct EngramSessionBindingResponse {
    routing_token: String,
    status: EngramSessionStatusResponse,
}

#[derive(Clone, Debug, Deserialize)]
struct EngramSessionStatusResponse {
    phase: String,
    #[serde(default)]
    open_grant_id: Option<String>,
    #[serde(default, rename = "confirmed_cursor")]
    _confirmed_cursor: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
enum EngramTurnDecisionResponse {
    Grant { grant: Box<EngramIssuedTurnGrant> },
    Refuse { directive: EngramDirectiveResponse },
    Defer { deferral: EngramDeferralResponse },
}

#[derive(Clone, Debug, Deserialize)]
struct EngramIssuedTurnGrant {
    grant_id: String,
    #[serde(default)]
    delivery: Option<EngramControlDeliveryResponse>,
}

#[derive(Clone, Debug, Deserialize)]
struct EngramControlDeliveryResponse {
    page: EngramDeliveryPageResponse,
}

#[derive(Clone, Debug, Deserialize)]
struct EngramDeliveryPageResponse {
    from_cursor: i64,
    to_cursor: i64,
    head_cursor: i64,
    delivery_token: String,
}

#[derive(Clone, Debug, Deserialize)]
struct EngramDirectiveResponse {
    directive_id: String,
    code: String,
    target: String,
    satisfaction: String,
}

#[derive(Clone, Debug, Deserialize)]
struct EngramDeferralResponse {
    code: String,
    #[serde(default)]
    retry_after_ms: Option<u64>,
    wake_condition: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
enum EngramTurnBeginResponse {
    Begin { receipt: EngramTurnBeginReceipt },
    Refuse { code: String },
}

#[derive(Clone, Debug, Deserialize)]
struct EngramTurnBeginReceipt {
    grant_id: String,
    #[serde(default, rename = "tentative_cursor")]
    _tentative_cursor: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
enum EngramTurnCheckpointResponse {
    Checkpointed { receipt: EngramCheckpointReceipt },
    Refuse { code: String },
}

#[derive(Clone, Debug, Deserialize)]
struct EngramCheckpointReceipt {
    grant_id: String,
    #[serde(rename = "cursor")]
    _cursor: i64,
    #[serde(rename = "confirmed_cursor")]
    _confirmed_cursor: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum EngramTransportErrorKind {
    Deadline,
    Transport,
    Protocol,
    Remote,
    Backoff,
}

#[derive(Clone, Debug)]
struct EngramTransportError {
    kind: EngramTransportErrorKind,
    code: Option<String>,
    message: String,
}

impl EngramTransportError {
    fn deadline(message: impl Into<String>) -> Self {
        Self {
            kind: EngramTransportErrorKind::Deadline,
            code: Some("deadline_exceeded".to_owned()),
            message: message.into(),
        }
    }

    fn transport(message: impl Into<String>) -> Self {
        Self {
            kind: EngramTransportErrorKind::Transport,
            code: Some("control_unavailable".to_owned()),
            message: message.into(),
        }
    }

    fn protocol(message: impl Into<String>) -> Self {
        Self {
            kind: EngramTransportErrorKind::Protocol,
            code: Some("unknown_control_schema".to_owned()),
            message: message.into(),
        }
    }

    fn remote(error: EngramControlErrorBody) -> Self {
        Self {
            kind: EngramTransportErrorKind::Remote,
            code: Some(error.code),
            message: error.message,
        }
    }

    fn backoff(message: impl Into<String>) -> Self {
        Self {
            kind: EngramTransportErrorKind::Backoff,
            code: Some("control_unavailable".to_owned()),
            message: message.into(),
        }
    }

    fn counts_for_circuit_breaker(&self) -> bool {
        matches!(
            self.kind,
            EngramTransportErrorKind::Deadline | EngramTransportErrorKind::Transport
        )
    }

    fn disables_session(&self) -> bool {
        self.kind == EngramTransportErrorKind::Protocol
            || (self.kind == EngramTransportErrorKind::Remote
                && matches!(
                    self.code.as_deref(),
                    Some(
                        "control_unavailable"
                            | "store_corrupt"
                            | "unknown_control_schema"
                            | "control_policy_missing"
                            | "work_claim_mismatch"
                    )
                ))
    }

    fn keeps_control_process_alive(&self) -> bool {
        self.kind == EngramTransportErrorKind::Remote
    }
}

impl std::fmt::Display for EngramTransportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramConnectionConfig {
    binary_path: PathBuf,
    project_file: PathBuf,
    home: PathBuf,
    project_root: PathBuf,
    actor_id: String,
    session_id: String,
}

trait EngramControlTransport: Send + Sync {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError>;

    fn read_work_binding(
        &self,
        _connection: &EngramConnectionConfig,
        _timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        Ok(None)
    }

    fn shutdown_session(&self, session_id: &str);
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct RecordedEngramControlRequest {
    connection: EngramConnectionConfig,
    request: Value,
}

#[cfg(test)]
thread_local! {
    static TEST_ENGRAM_BOOT_TRANSPORT: std::cell::RefCell<Option<Arc<dyn EngramControlTransport>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
#[derive(Clone, Debug)]
enum ScriptedEngramControlResponse {
    Reply(std::result::Result<Value, EngramTransportError>),
}

#[cfg(test)]
#[derive(Default)]
struct ScriptedEngramControlTransport {
    requests: Mutex<Vec<RecordedEngramControlRequest>>,
    responses: Mutex<VecDeque<ScriptedEngramControlResponse>>,
    work_bindings: Mutex<
        VecDeque<
            std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError>,
        >,
    >,
    shutdowns: Mutex<Vec<String>>,
}

#[cfg(test)]
impl ScriptedEngramControlTransport {
    fn new(responses: impl IntoIterator<Item = ScriptedEngramControlResponse>) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(responses.into_iter().collect()),
            work_bindings: Mutex::new(VecDeque::new()),
            shutdowns: Mutex::new(Vec::new()),
        })
    }

    fn new_with_work_bindings(
        responses: impl IntoIterator<Item = ScriptedEngramControlResponse>,
        work_bindings: impl IntoIterator<
            Item = std::result::Result<
                Option<EngramControlWorkBinding>,
                EngramTransportError,
            >,
        >,
    ) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(responses.into_iter().collect()),
            work_bindings: Mutex::new(work_bindings.into_iter().collect()),
            shutdowns: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<RecordedEngramControlRequest> {
        self.requests
            .lock()
            .expect("scripted Engram requests mutex poisoned")
            .clone()
    }

    fn shutdowns(&self) -> Vec<String> {
        self.shutdowns
            .lock()
            .expect("scripted Engram shutdowns mutex poisoned")
            .clone()
    }
}

#[cfg(test)]
impl EngramControlTransport for ScriptedEngramControlTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        _timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        self.requests
            .lock()
            .expect("scripted Engram requests mutex poisoned")
            .push(RecordedEngramControlRequest {
                connection: connection.clone(),
                request: serde_json::to_value(request).expect("Engram request should serialize"),
            });
        let response = self
            .responses
            .lock()
            .expect("scripted Engram responses mutex poisoned")
            .pop_front()
            .unwrap_or_else(|| {
                ScriptedEngramControlResponse::Reply(Err(EngramTransportError::protocol(
                    "scripted Engram transport has no response for request",
                )))
            });
        let ScriptedEngramControlResponse::Reply(reply) = response;
        reply
    }

    fn read_work_binding(
        &self,
        _connection: &EngramConnectionConfig,
        _timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.work_bindings
            .lock()
            .expect("scripted Engram work bindings mutex poisoned")
            .pop_front()
            .unwrap_or(Ok(None))
    }

    fn shutdown_session(&self, session_id: &str) {
        self.shutdowns
            .lock()
            .expect("scripted Engram shutdowns mutex poisoned")
            .push(session_id.to_owned());
    }
}

/// Stateful test double for the parts of Engram's control-session contract
/// whose ordering cannot be represented faithfully by a response queue.
///
/// In particular, an evaluated grant is only *issued* until `turn_begin`
/// succeeds. Issued grants cannot be checkpointed, but a fresh `session_bind`
/// expires them. Begun grants have the opposite rule: they must be checkpointed
/// before another bind can replace the session token. The idempotency tables are
/// intentionally retained across binds, matching Engram's durable operation
/// history rather than the lifetime of one `engram control` child process.
#[cfg(test)]
#[derive(Default)]
struct StatefulEngramControlTransport {
    state: Mutex<StatefulEngramControlState>,
    first_begin_refusal: Option<String>,
}

#[cfg(test)]
#[derive(Default)]
struct StatefulEngramControlState {
    sessions: HashMap<String, StatefulEngramControlSession>,
    seen_binds:
        HashMap<(String, String), (String, std::result::Result<Value, EngramTransportError>)>,
    requests: Vec<RecordedEngramControlRequest>,
    shutdowns: Vec<String>,
    next_routing_token: u64,
    next_grant_id: u64,
}

#[cfg(test)]
#[derive(Default)]
struct StatefulEngramControlSession {
    routing_token: Option<String>,
    issued_grant_id: Option<String>,
    begun_grant_id: Option<String>,
    first_begin_refusal_used: bool,
    known_grant_ids: HashSet<String>,
    seen_evaluates: HashMap<String, (String, std::result::Result<Value, EngramTransportError>)>,
    seen_begins: HashMap<String, (String, std::result::Result<Value, EngramTransportError>)>,
    seen_checkpoints: HashMap<String, (String, std::result::Result<Value, EngramTransportError>)>,
}

#[cfg(test)]
impl StatefulEngramControlTransport {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn with_first_begin_refusal(code: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(StatefulEngramControlState::default()),
            first_begin_refusal: Some(code.into()),
        })
    }

    fn requests(&self) -> Vec<RecordedEngramControlRequest> {
        self.state
            .lock()
            .expect("stateful Engram transport mutex poisoned")
            .requests
            .clone()
    }

    fn shutdowns(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("stateful Engram transport mutex poisoned")
            .shutdowns
            .clone()
    }

    fn grant_state(&self, session_id: &str) -> (Option<String>, Option<String>) {
        self.state
            .lock()
            .expect("stateful Engram transport mutex poisoned")
            .sessions
            .get(session_id)
            .map(|session| {
                (
                    session.issued_grant_id.clone(),
                    session.begun_grant_id.clone(),
                )
            })
            .unwrap_or_default()
    }

    fn mark_begun_grant_issued_unbegun(&self, session_id: &str) {
        let mut state = self
            .state
            .lock()
            .expect("stateful Engram transport mutex poisoned");
        let session = state
            .sessions
            .get_mut(session_id)
            .expect("stateful Engram session should exist");
        let grant_id = session
            .begun_grant_id
            .take()
            .expect("stateful Engram session should have a begun grant");
        session.issued_grant_id = Some(grant_id);
    }

    fn remote_error(code: &str, message: impl Into<String>) -> EngramTransportError {
        EngramTransportError::remote(EngramControlErrorBody {
            code: code.to_owned(),
            message: message.into(),
        })
    }

    fn validate_routing_token(
        session: Option<&StatefulEngramControlSession>,
        routing_token: &str,
    ) -> std::result::Result<(), EngramTransportError> {
        if session.and_then(|session| session.routing_token.as_deref()) == Some(routing_token) {
            Ok(())
        } else {
            Err(Self::remote_error(
                "control_session_token_mismatch",
                "routing token does not belong to this control session",
            ))
        }
    }

    fn idempotency_conflict(operation: &str) -> EngramTransportError {
        let code = match operation {
            "session_bind" => "control_session_bind_conflict",
            "turn_evaluate" => "turn_idempotency_conflict",
            _ => "control_operation_idempotency_conflict",
        };
        Self::remote_error(
            code,
            format!("{operation} idempotency key was reused for a different intent"),
        )
    }

    fn request_intent_without_auth_and_idempotency(request: &EngramControlRequest) -> String {
        let mut value = serde_json::to_value(request).expect("Engram request should serialize");
        let object = value
            .as_object_mut()
            .expect("Engram request should serialize as an object");
        object.remove("routing_token");
        object.remove("idempotency_key");
        serde_json::to_string(&value).expect("Engram request intent should serialize")
    }
}

#[cfg(test)]
fn stateful_engram_begin_refusal_expires_grant(code: &str) -> bool {
    // This is the fake's independent model of Engram's external contract.
    // Do not call the adapter policy helper here: tests must detect drift in
    // either implementation instead of allowing both sides to move together.
    matches!(
        code,
        "grant_expired"
            | "policy_epoch_changed"
            | "task_admission_epoch_changed"
            | "delta_required"
            | "stale_fence"
    )
}

#[cfg(test)]
impl EngramControlTransport for StatefulEngramControlTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        _timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        let mut state = self
            .state
            .lock()
            .expect("stateful Engram transport mutex poisoned");
        state.requests.push(RecordedEngramControlRequest {
            connection: connection.clone(),
            request: serde_json::to_value(request).expect("Engram request should serialize"),
        });
        let session_id = connection.session_id.clone();

        match request {
            EngramControlRequest::SessionBind {
                idempotency_key, ..
            } => {
                let intent = Self::request_intent_without_auth_and_idempotency(request);
                let receipt_key = (session_id.clone(), idempotency_key.clone());
                if let Some((seen_intent, response)) = state.seen_binds.get(&receipt_key) {
                    if seen_intent != &intent {
                        return Err(Self::idempotency_conflict("session_bind"));
                    }
                    return response.clone();
                }
                if state
                    .sessions
                    .get(&session_id)
                    .and_then(|session| session.begun_grant_id.as_ref())
                    .is_some()
                {
                    return Err(Self::remote_error(
                        "invalid_control_session",
                        "a begun grant must be checkpointed before session_bind",
                    ));
                }
                state.next_routing_token = state.next_routing_token.saturating_add(1);
                let routing_token =
                    format!("stateful-token-{}-{}", session_id, state.next_routing_token);
                let session = state.sessions.entry(session_id).or_default();
                session.routing_token = Some(routing_token.clone());
                session.issued_grant_id = None;
                let response = Ok(json!({
                    "routing_token": routing_token,
                    "status": {
                        "phase": "sync_required",
                        "confirmed_cursor": 0
                    }
                }));
                state
                    .seen_binds
                    .insert(receipt_key, (intent, response.clone()));
                response
            }
            EngramControlRequest::SessionStatus { routing_token } => {
                let session = state.sessions.get(&session_id);
                Self::validate_routing_token(session, routing_token)?;
                let session = session.expect("validated Engram session should exist");
                let phase = if session.issued_grant_id.is_some() || session.begun_grant_id.is_some()
                {
                    "turn_open"
                } else {
                    "ready"
                };
                Ok(json!({
                    "phase": phase,
                    "open_grant_id": session
                        .begun_grant_id
                        .as_ref()
                        .or(session.issued_grant_id.as_ref()),
                    "confirmed_cursor": 0
                }))
            }
            EngramControlRequest::TurnEvaluate {
                routing_token,
                idempotency_key,
                intent_fingerprint,
                ..
            } => {
                {
                    let session = state.sessions.get(&session_id);
                    Self::validate_routing_token(session, routing_token)?;
                    let session = session.expect("validated Engram session should exist");
                    if let Some((seen_fingerprint, response)) =
                        session.seen_evaluates.get(idempotency_key)
                    {
                        if seen_fingerprint != intent_fingerprint {
                            return Err(Self::idempotency_conflict("turn_evaluate"));
                        }
                        return response.clone();
                    }
                }
                if state.sessions.get(&session_id).is_some_and(|session| {
                    session.issued_grant_id.is_some() || session.begun_grant_id.is_some()
                }) {
                    let response = Ok(json!({
                        "decision": "refuse",
                        "directive": {
                            "directive_id": format!("turn-already-open-{idempotency_key}"),
                            "code": "turn_already_open",
                            "target": "host",
                            "satisfaction": "close the open turn before evaluating another"
                        }
                    }));
                    let session = state
                        .sessions
                        .get_mut(&session_id)
                        .expect("validated Engram session should remain present");
                    session.seen_evaluates.insert(
                        idempotency_key.clone(),
                        (intent_fingerprint.clone(), response.clone()),
                    );
                    return response;
                }

                state.next_grant_id = state.next_grant_id.saturating_add(1);
                let grant_id = format!("stateful-grant-{}-{}", session_id, state.next_grant_id);
                let response = Ok(json!({
                    "decision": "grant",
                    "grant": { "grant_id": grant_id }
                }));
                let session = state
                    .sessions
                    .get_mut(&session_id)
                    .expect("validated Engram session should remain present");
                session.known_grant_ids.insert(grant_id.clone());
                session.issued_grant_id = Some(grant_id);
                session.seen_evaluates.insert(
                    idempotency_key.clone(),
                    (intent_fingerprint.clone(), response.clone()),
                );
                response
            }
            EngramControlRequest::TurnBegin {
                routing_token,
                grant_id,
                delivery_tokens,
                idempotency_key,
                ..
            } => {
                let refusal_code = self.first_begin_refusal.clone();
                let session = state.sessions.get_mut(&session_id);
                Self::validate_routing_token(session.as_deref(), routing_token)?;
                let session = session.expect("validated Engram session should exist");
                let intent = serde_json::to_string(&(grant_id, delivery_tokens))
                    .expect("Engram begin intent should serialize");
                if let Some((seen_intent, response)) = session.seen_begins.get(idempotency_key) {
                    if seen_intent != &intent {
                        return Err(Self::idempotency_conflict("turn_begin"));
                    }
                    return response.clone();
                }
                if !session.known_grant_ids.contains(grant_id) {
                    return Err(Self::remote_error(
                        "turn_grant_not_found",
                        "turn_begin grant does not exist",
                    ));
                }
                if session.issued_grant_id.as_deref() != Some(grant_id.as_str()) {
                    let response = Ok(json!({
                        "decision": "refuse",
                        "code": "grant_scope_mismatch"
                    }));
                    session.seen_begins.insert(
                        idempotency_key.clone(),
                        (intent, response.clone()),
                    );
                    return response;
                }

                let response = if let Some(code) = refusal_code
                    && !session.first_begin_refusal_used
                {
                    session.first_begin_refusal_used = true;
                    if stateful_engram_begin_refusal_expires_grant(&code) {
                        session.issued_grant_id = None;
                    }
                    Ok(json!({ "decision": "refuse", "code": code }))
                } else {
                    session.issued_grant_id = None;
                    session.begun_grant_id = Some(grant_id.clone());
                    Ok(json!({
                        "decision": "begin",
                        "receipt": {
                            "grant_id": grant_id,
                            "tentative_cursor": 0
                        }
                    }))
                };
                session.seen_begins.insert(
                    idempotency_key.clone(),
                    (intent, response.clone()),
                );
                response
            }
            EngramControlRequest::TurnCheckpoint {
                routing_token,
                grant_id,
                idempotency_key,
                ..
            } => {
                let session = state.sessions.get_mut(&session_id);
                Self::validate_routing_token(session.as_deref(), routing_token)?;
                let session = session.expect("validated Engram session should exist");
                let intent = Self::request_intent_without_auth_and_idempotency(request);
                if let Some((seen_intent, response)) =
                    session.seen_checkpoints.get(idempotency_key)
                {
                    if seen_intent != &intent {
                        return Err(Self::idempotency_conflict("turn_checkpoint"));
                    }
                    return response.clone();
                }
                if !session.known_grant_ids.contains(grant_id) {
                    return Err(Self::remote_error(
                        "turn_grant_not_found",
                        "turn_checkpoint grant does not exist",
                    ));
                }
                if session.begun_grant_id.as_deref() != Some(grant_id.as_str()) {
                    // Engram distinguishes checkpointing a grant that was
                    // issued but never begun from checkpointing a grant that
                    // is not the session's current authority at all.
                    let code = if session.issued_grant_id.as_deref() == Some(grant_id.as_str()) {
                        "grant_not_begun"
                    } else {
                        "grant_scope_mismatch"
                    };
                    let response = Ok(json!({
                        "decision": "refuse",
                        "code": code
                    }));
                    session.seen_checkpoints.insert(
                        idempotency_key.clone(),
                        (intent, response.clone()),
                    );
                    return response;
                }
                session.begun_grant_id = None;
                let response = Ok(json!({
                    "decision": "checkpointed",
                    "receipt": {
                        "grant_id": grant_id,
                        "cursor": 0,
                        "confirmed_cursor": 0
                    }
                }));
                session.seen_checkpoints.insert(
                    idempotency_key.clone(),
                    (intent, response.clone()),
                );
                response
            }
        }
    }

    fn shutdown_session(&self, session_id: &str) {
        self.state
            .lock()
            .expect("stateful Engram transport mutex poisoned")
            .shutdowns
            .push(session_id.to_owned());
    }
}

struct EngramProcessRequest {
    request: Vec<u8>,
    reply: mpsc::Sender<std::result::Result<Value, EngramTransportError>>,
}

struct EngramProcessTree {
    inner: TerminalProcessTree,
    terminated: Mutex<bool>,
}

impl EngramProcessTree {
    fn attach(process: &Arc<SharedChild>) -> Result<Self> {
        Ok(Self {
            inner: TerminalProcessTree::attach(process)?,
            terminated: Mutex::new(false),
        })
    }

    fn terminate(&self, process: &Arc<SharedChild>) -> Result<()> {
        let mut terminated = self
            .terminated
            .lock()
            .expect("Engram process-tree mutex poisoned");
        if *terminated {
            return Ok(());
        }
        *terminated = true;
        self.inner.kill_before_reap(process, "Engram control")
    }

    fn resume_after_attach(&self, process: &Arc<SharedChild>) -> Result<()> {
        self.inner.resume_after_attach(process)
    }
}

struct EngramControlProcess {
    config: EngramConnectionConfig,
    process: Arc<SharedChild>,
    process_tree: Arc<EngramProcessTree>,
    requests: mpsc::Sender<EngramProcessRequest>,
    worker_finished: Arc<AtomicBool>,
}

impl EngramControlProcess {
    fn terminate(&self) {
        let _ = self.process_tree.terminate(&self.process);
        let _ = self.process.wait();
    }
}

impl Drop for EngramControlProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[derive(Clone, Copy)]
struct EngramControlStartupHandshake {
    expected_line: &'static str,
    timeout: Duration,
}

struct ProcessEngramControlTransport {
    processes: Mutex<HashMap<String, Arc<EngramControlProcess>>>,
    startup_handshake: Option<EngramControlStartupHandshake>,
    idle_timeout: Duration,
}

impl Default for ProcessEngramControlTransport {
    fn default() -> Self {
        Self {
            processes: Mutex::new(HashMap::new()),
            startup_handshake: None,
            idle_timeout: ENGRAM_CONTROL_IDLE_TIMEOUT,
        }
    }
}

impl ProcessEngramControlTransport {
    #[cfg(test)]
    fn with_startup_handshake(expected_line: &'static str, timeout: Duration) -> Self {
        Self {
            processes: Mutex::new(HashMap::new()),
            startup_handshake: Some(EngramControlStartupHandshake {
                expected_line,
                timeout,
            }),
            idle_timeout: ENGRAM_CONTROL_IDLE_TIMEOUT,
        }
    }

    #[cfg(test)]
    fn with_startup_handshake_and_idle_timeout(
        expected_line: &'static str,
        startup_timeout: Duration,
        idle_timeout: Duration,
    ) -> Self {
        Self {
            processes: Mutex::new(HashMap::new()),
            startup_handshake: Some(EngramControlStartupHandshake {
                expected_line,
                timeout: startup_timeout,
            }),
            idle_timeout,
        }
    }

    fn process_for(
        &self,
        connection: &EngramConnectionConfig,
    ) -> std::result::Result<Arc<EngramControlProcess>, EngramTransportError> {
        let stale = {
            let mut processes = self
                .processes
                .lock()
                .expect("Engram process registry mutex poisoned");
            if let Some(existing) = processes.get(&connection.session_id).cloned() {
                if existing.config == *connection
                    && !existing.worker_finished.load(Ordering::Acquire)
                {
                    return Ok(existing);
                }
                processes.remove(&connection.session_id)
            } else {
                None
            }
        };
        if let Some(stale) = stale {
            stale.terminate();
        }

        let process = Arc::new(spawn_engram_control_process(
            connection,
            self.startup_handshake,
            self.idle_timeout,
        )?);
        let (selected, displaced) = {
            let mut processes = self
                .processes
                .lock()
                .expect("Engram process registry mutex poisoned");
            if let Some(existing) = processes.get(&connection.session_id).cloned()
                && existing.config == *connection
                && !existing.worker_finished.load(Ordering::Acquire)
            {
                (existing, Some(process))
            } else {
                let displaced = processes.insert(connection.session_id.clone(), process.clone());
                (process, displaced)
            }
        };
        if let Some(displaced) = displaced {
            displaced.terminate();
        }
        Ok(selected)
    }

    fn discard_process(&self, session_id: &str, expected: &Arc<EngramControlProcess>) {
        let removed = {
            let mut processes = self
                .processes
                .lock()
                .expect("Engram process registry mutex poisoned");
            let should_remove = processes
                .get(session_id)
                .is_some_and(|current| Arc::ptr_eq(current, expected));
            should_remove
                .then(|| processes.remove(session_id))
                .flatten()
        };
        if let Some(process) = removed {
            process.terminate();
        }
    }
}

fn read_engram_work_binding_from_cli(
    connection: &EngramConnectionConfig,
    timeout: Duration,
) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
    let next = run_engram_json_command_with_lock_retry(
        connection,
        &["work", "--actor-id", &connection.actor_id, "--session-id", &connection.session_id, "next", "--sections", "focus"],
        timeout,
    )?;
    let Some(work_id) = next
        .get("focus")
        .and_then(|focus| focus.get("status"))
        .and_then(|status| status.get("work"))
        .and_then(|work| work.get("work_id"))
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    let focus = run_engram_json_command_with_lock_retry(
        connection,
        &["work", "--actor-id", &connection.actor_id, "--session-id", &connection.session_id, "focus", work_id],
        timeout,
    )?;
    focus
        .get("control_binding")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| {
            EngramTransportError::protocol(format!(
                "invalid Engram work control_binding: {error}"
            ))
        })
}

fn run_engram_json_command_with_lock_retry(
    connection: &EngramConnectionConfig,
    args: &[&str],
    timeout: Duration,
) -> std::result::Result<Value, EngramTransportError> {
    let first = run_engram_json_command(connection, args, timeout);
    match first {
        Err(error)
            if error.kind == EngramTransportErrorKind::Transport
                && error
                    .message
                    .to_ascii_lowercase()
                    .contains("database is locked") =>
        {
            std::thread::sleep(ENGRAM_WORK_BINDING_LOCK_RETRY_DELAY);
            run_engram_json_command(connection, args, timeout)
        }
        result => result,
    }
}

/// Irreversibly revokes a project work-authority grant through Engram's CLI.
/// Engram re-resolves the grant on every mutation, so this is the fail-closed
/// boundary for MCP children that outlive an unconfirmed agent interruption.
/// The command follows the one-retry SQLite lock policy used by the work-
/// binding reader; process teardown remains independent and always continues.
fn revoke_engram_project_work_authority(
    target: &EngramAuthorityRevocationTarget,
    reason: &str,
) -> std::result::Result<(), EngramTransportError> {
    let binary_path = PathBuf::from(&target.binary_path);
    let home = PathBuf::from(&target.home);
    let project_root = PathBuf::from(&target.project_root);
    let project_file = project_root.join(".engram-project");

    let run = || {
        run_engram_authority_revoke_command(
            &binary_path,
            &project_file,
            &home,
            &project_root,
            &target.work_authority_grant,
            reason,
            ENGRAM_WORK_BINDING_COMMAND_TIMEOUT,
        )
    };
    let first = run();
    match first {
        Err(error)
            if error.kind == EngramTransportErrorKind::Transport
                && error
                    .message
                    .to_ascii_lowercase()
                    .contains("database is locked") =>
        {
            std::thread::sleep(ENGRAM_WORK_BINDING_LOCK_RETRY_DELAY);
            run()
        }
        result => result,
    }
}

fn run_engram_authority_revoke_command(
    binary_path: &FsPath,
    project_file: &FsPath,
    home: &FsPath,
    project_root: &FsPath,
    grant: &str,
    reason: &str,
    timeout: Duration,
) -> std::result::Result<(), EngramTransportError> {
    let mut command = engram_command(binary_path);
    configure_terminal_process_tree(&mut command);
    let mut child = command
        .arg("--project-file")
        .arg(project_file)
        .arg("--home")
        .arg(home)
        .args([
            "authority",
            "revoke",
            "--revoked-by",
            "termal:host",
            "--reason",
            reason,
            "--",
            grant,
        ])
        .current_dir(project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            EngramTransportError::transport(format!(
                "failed spawning Engram authority revocation: {error}"
            ))
        })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        EngramTransportError::transport("Engram authority revocation stdout is unavailable")
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        EngramTransportError::transport("Engram authority revocation stderr is unavailable")
    })?;
    let process = Arc::new(SharedChild::new(child).map_err(|error| {
        EngramTransportError::transport(format!(
            "failed sharing Engram authority revocation process: {error}"
        ))
    })?);
    let process_tree = EngramProcessTree::attach(&process).map_err(|error| {
        let _ = kill_child_process(&process, "Engram authority revocation");
        let _ = process.wait();
        EngramTransportError::transport(format!(
            "failed preparing Engram authority revocation process tree: {error:#}"
        ))
    })?;
    process_tree.resume_after_attach(&process).map_err(|error| {
        let _ = process_tree.terminate(&process);
        let _ = process.wait();
        EngramTransportError::transport(format!(
            "failed resuming Engram authority revocation: {error:#}"
        ))
    })?;
    let stdout_reader = std::thread::spawn(move || read_engram_cli_output(stdout));
    let stderr_reader = std::thread::spawn(move || read_engram_cli_output(stderr));
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match process.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) => {
                let _ = process_tree.terminate(&process);
                let _ = process.wait();
                break Err(EngramTransportError::deadline(format!(
                    "Engram authority revocation exceeded {} ms",
                    timeout.as_millis()
                )));
            }
            Err(error) => {
                let _ = process_tree.terminate(&process);
                let _ = process.wait();
                break Err(EngramTransportError::transport(format!(
                    "failed waiting for Engram authority revocation: {error}"
                )));
            }
        }
    };
    let stdout = join_engram_cli_output(stdout_reader, "stdout")?;
    let stderr = join_engram_cli_output(stderr_reader, "stderr")?;
    let status = status?;
    if status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&stderr).trim().to_owned();
    let fallback = String::from_utf8_lossy(&stdout).trim().to_owned();
    Err(EngramTransportError::transport(if !detail.is_empty() {
        format!("Engram authority revocation failed: {detail}")
    } else if !fallback.is_empty() {
        format!("Engram authority revocation failed: {fallback}")
    } else {
        format!("Engram authority revocation exited with {status}")
    }))
}

fn run_engram_json_command(
    connection: &EngramConnectionConfig,
    args: &[&str],
    timeout: Duration,
) -> std::result::Result<Value, EngramTransportError> {
    let mut command = engram_command(&connection.binary_path);
    configure_terminal_process_tree(&mut command);
    let mut child = command
        .arg("--project-file")
        .arg(&connection.project_file)
        .arg("--home")
        .arg(&connection.home)
        .args(args)
        .current_dir(&connection.project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            EngramTransportError::transport(format!(
                "failed spawning Engram work-binding reader: {error}"
            ))
        })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        EngramTransportError::transport("Engram work-binding reader stdout is unavailable")
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        EngramTransportError::transport("Engram work-binding reader stderr is unavailable")
    })?;
    let process = Arc::new(SharedChild::new(child).map_err(|error| {
        EngramTransportError::transport(format!(
            "failed sharing Engram work-binding reader: {error}"
        ))
    })?);
    let process_tree = EngramProcessTree::attach(&process).map_err(|error| {
        let _ = kill_child_process(&process, "Engram work-binding reader");
        let _ = process.wait();
        EngramTransportError::transport(format!(
            "failed preparing Engram work-binding reader process tree: {error:#}"
        ))
    })?;
    process_tree.resume_after_attach(&process).map_err(|error| {
        let _ = process_tree.terminate(&process);
        let _ = process.wait();
        EngramTransportError::transport(format!(
            "failed resuming Engram work-binding reader: {error:#}"
        ))
    })?;
    let stdout_reader = std::thread::spawn(move || read_engram_cli_output(stdout));
    let stderr_reader = std::thread::spawn(move || read_engram_cli_output(stderr));
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match process.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) => {
                let _ = process_tree.terminate(&process);
                let _ = process.wait();
                break Err(EngramTransportError::deadline(format!(
                    "Engram work-binding read exceeded {} ms",
                    timeout.as_millis()
                )));
            }
            Err(error) => {
                let _ = process_tree.terminate(&process);
                let _ = process.wait();
                break Err(EngramTransportError::transport(format!(
                    "failed waiting for Engram work-binding reader: {error}"
                )));
            }
        }
    };
    let stdout = join_engram_cli_output(stdout_reader, "stdout")?;
    let stderr = join_engram_cli_output(stderr_reader, "stderr")?;
    let status = status?;
    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr).trim().to_owned();
        return Err(EngramTransportError::transport(if detail.is_empty() {
            format!("Engram work-binding reader exited with {status}")
        } else {
            format!("Engram work-binding reader failed: {detail}")
        }));
    }
    serde_json::from_slice(&stdout).map_err(|error| {
        EngramTransportError::protocol(format!(
            "invalid Engram work-binding response: {error}"
        ))
    })
}

fn read_engram_cli_output(
    reader: impl std::io::Read,
) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    std::io::Read::take(reader, (ENGRAM_CONTROL_MAX_FRAME_BYTES + 1) as u64)
        .read_to_end(&mut output)?;
    if output.len() > ENGRAM_CONTROL_MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Engram command output exceeds the maximum control frame",
        ));
    }
    Ok(output)
}

fn join_engram_cli_output(
    reader: std::thread::JoinHandle<std::io::Result<Vec<u8>>>,
    stream: &str,
) -> std::result::Result<Vec<u8>, EngramTransportError> {
    reader
        .join()
        .map_err(|_| {
            EngramTransportError::transport(format!(
                "Engram work-binding {stream} reader panicked"
            ))
        })?
        .map_err(|error| {
            EngramTransportError::transport(format!(
                "failed reading Engram work-binding {stream}: {error}"
            ))
        })
}

impl EngramControlTransport for ProcessEngramControlTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        let encoded = serde_json::to_vec(request).map_err(|err| {
            EngramTransportError::protocol(format!("failed encoding request: {err}"))
        })?;
        if encoded.len() > ENGRAM_CONTROL_MAX_FRAME_BYTES {
            return Err(EngramTransportError::protocol(
                "Engram request exceeds the maximum control frame",
            ));
        }

        let process = self.process_for(connection)?;
        let (reply_tx, reply_rx) = mpsc::channel();
        process
            .requests
            .send(EngramProcessRequest {
                request: encoded,
                reply: reply_tx,
            })
            .map_err(|err| {
                self.discard_process(&connection.session_id, &process);
                EngramTransportError::transport(format!(
                    "Engram control worker is unavailable: {err}"
                ))
            })?;

        match reply_rx.recv_timeout(timeout) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => {
                // A valid Engram error envelope is an application response on
                // a healthy JSON-lines connection. Only I/O/protocol failures
                // terminate the worker and require process replacement.
                if !error.keeps_control_process_alive() {
                    self.discard_process(&connection.session_id, &process);
                }
                Err(error)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.discard_process(&connection.session_id, &process);
                Err(EngramTransportError::deadline(format!(
                    "Engram control call exceeded {} ms",
                    timeout.as_millis()
                )))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.discard_process(&connection.session_id, &process);
                Err(EngramTransportError::transport(
                    "Engram control worker exited before replying",
                ))
            }
        }
    }

    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        read_engram_work_binding_from_cli(connection, timeout)
    }

    fn shutdown_session(&self, session_id: &str) {
        let process = self
            .processes
            .lock()
            .expect("Engram process registry mutex poisoned")
            .remove(session_id);
        if let Some(process) = process {
            process.terminate();
        }
    }
}

fn spawn_engram_control_process(
    connection: &EngramConnectionConfig,
    startup_handshake: Option<EngramControlStartupHandshake>,
    idle_timeout: Duration,
) -> std::result::Result<EngramControlProcess, EngramTransportError> {
    let mut command = engram_command(&connection.binary_path);
    configure_terminal_process_tree(&mut command);
    let mut child = command
        .arg("--project-file")
        .arg(&connection.project_file)
        .arg("--home")
        .arg(&connection.home)
        .arg("control")
        .arg("--actor-id")
        .arg(&connection.actor_id)
        .arg("--session-id")
        .arg(&connection.session_id)
        .current_dir(&connection.project_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|err| {
            EngramTransportError::transport(format!("failed spawning Engram control: {err}"))
        })?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| EngramTransportError::transport("Engram control stdin is unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| EngramTransportError::transport("Engram control stdout is unavailable"))?;
    let process = Arc::new(SharedChild::new(child).map_err(|err| {
        EngramTransportError::transport(format!("failed sharing Engram control child: {err}"))
    })?);
    let process_tree = Arc::new(EngramProcessTree::attach(&process).map_err(|err| {
        let _ = kill_child_process(&process, "Engram control");
        let _ = process.wait();
        EngramTransportError::transport(format!(
            "failed preparing Engram control process tree: {err:#}"
        ))
    })?);
    let worker_process = process.clone();
    let worker_process_tree = process_tree.clone();
    let worker_finished = Arc::new(AtomicBool::new(false));
    let worker_finished_signal = worker_finished.clone();
    let (request_tx, request_rx) = mpsc::channel::<EngramProcessRequest>();
    let (startup_tx, startup_rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name(format!("engram-control-{}", connection.session_id))
        .spawn(move || {
            let mut stdout = BufReader::new(stdout);
            if let Some(handshake) = startup_handshake {
                let startup_result = read_engram_control_startup_handshake(&mut stdout, handshake);
                let startup_failed = startup_result.is_err();
                let _ = startup_tx.send(startup_result);
                if startup_failed {
                    worker_finished_signal.store(true, Ordering::Release);
                    let _ = worker_process_tree.terminate(&worker_process);
                    let _ = worker_process.wait();
                    return;
                }
            }
            loop {
                match request_rx.recv_timeout(idle_timeout) {
                    Ok(request) => {
                        let result = exchange_engram_control_frame(
                            &mut stdin,
                            &mut stdout,
                            &request.request,
                        );
                        // Engram's host service continues after request-level
                        // `{status:"error"}` replies. Treat only malformed or
                        // broken exchanges as terminal for this sidecar.
                        let is_terminal = result
                            .as_ref()
                            .is_err_and(|error| !error.keeps_control_process_alive());
                        if is_terminal {
                            worker_finished_signal.store(true, Ordering::Release);
                        }
                        let _ = request.reply.send(result);
                        if is_terminal {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {
                        worker_finished_signal.store(true, Ordering::Release);
                        break;
                    }
                }
            }
            let _ = worker_process_tree.terminate(&worker_process);
            let _ = worker_process.wait();
        })
        .map_err(|err| {
            let _ = process_tree.terminate(&process);
            let _ = process.wait();
            EngramTransportError::transport(format!("failed spawning Engram control worker: {err}"))
        })?;

    if let Err(err) = process_tree.resume_after_attach(&process) {
        let _ = process_tree.terminate(&process);
        let _ = process.wait();
        return Err(EngramTransportError::transport(format!(
            "failed resuming Engram control process: {err:#}"
        )));
    }

    if let Some(handshake) = startup_handshake {
        let startup_result = match startup_rx.recv_timeout(handshake.timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(EngramTransportError::transport(format!(
                "Engram control startup handshake exceeded {} ms",
                handshake.timeout.as_millis()
            ))),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(EngramTransportError::transport(
                "Engram control worker exited before the startup handshake",
            )),
        };
        if let Err(error) = startup_result {
            let _ = process_tree.terminate(&process);
            let _ = process.wait();
            return Err(error);
        }
    }

    Ok(EngramControlProcess {
        config: connection.clone(),
        process,
        process_tree,
        requests: request_tx,
        worker_finished,
    })
}

fn read_engram_control_startup_handshake(
    stdout: &mut impl BufRead,
    handshake: EngramControlStartupHandshake,
) -> std::result::Result<(), EngramTransportError> {
    let mut line = String::new();
    let read = stdout.read_line(&mut line).map_err(|err| {
        EngramTransportError::transport(format!(
            "failed reading Engram control startup handshake: {err}"
        ))
    })?;
    if read == 0 {
        return Err(EngramTransportError::transport(
            "Engram control reached EOF before the startup handshake",
        ));
    }
    if line.trim_end_matches(['\r', '\n']) != handshake.expected_line {
        return Err(EngramTransportError::protocol(
            "Engram control returned an unexpected startup handshake",
        ));
    }
    Ok(())
}

fn exchange_engram_control_frame(
    stdin: &mut impl Write,
    stdout: &mut impl BufRead,
    request: &[u8],
) -> std::result::Result<Value, EngramTransportError> {
    stdin
        .write_all(request)
        .and_then(|()| stdin.write_all(b"\n"))
        .and_then(|()| stdin.flush())
        .map_err(|err| {
            EngramTransportError::transport(format!("failed writing Engram request: {err}"))
        })?;

    let mut response = Vec::new();
    let read = stdout.read_until(b'\n', &mut response).map_err(|err| {
        EngramTransportError::transport(format!("failed reading Engram response: {err}"))
    })?;
    if read == 0 {
        return Err(EngramTransportError::transport(
            "Engram control reached EOF before replying",
        ));
    }
    if response.len() > ENGRAM_CONTROL_MAX_FRAME_BYTES {
        return Err(EngramTransportError::protocol(
            "Engram response exceeds the maximum control frame",
        ));
    }
    let envelope: EngramControlResponse = serde_json::from_slice(&response)
        .map_err(|err| EngramTransportError::protocol(format!("invalid Engram response: {err}")))?;
    match envelope {
        EngramControlResponse::Ok { result } => Ok(result),
        EngramControlResponse::Error { error } => Err(EngramTransportError::remote(error)),
    }
}

#[derive(Clone)]
struct EngramHostAdapter {
    transport: Arc<dyn EngramControlTransport>,
}

impl Default for EngramHostAdapter {
    fn default() -> Self {
        Self {
            transport: Arc::new(ProcessEngramControlTransport::default()),
        }
    }
}

impl EngramHostAdapter {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        self.transport.request(connection, request, timeout)
    }

    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.transport.read_work_binding(connection, timeout)
    }

    fn shutdown_session(&self, session_id: &str) {
        self.transport.shutdown_session(session_id);
    }
}

#[cfg(test)]
impl AppState {
    fn install_test_engram_transport(&self, transport: Arc<dyn EngramControlTransport>) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        inner.engram_host_adapter = Arc::new(EngramHostAdapter { transport });
    }

    fn new_with_paths_and_engram_transport_for_test(
        default_workdir: String,
        persistence_path: PathBuf,
        orchestrator_templates_path: PathBuf,
        transport: Arc<dyn EngramControlTransport>,
    ) -> Result<Self> {
        TEST_ENGRAM_BOOT_TRANSPORT.with(|slot| {
            let previous = slot.replace(Some(transport));
            let result = Self::new_with_paths(
                default_workdir,
                persistence_path,
                orchestrator_templates_path,
            );
            slot.replace(previous);
            result
        })
    }
}

#[derive(Clone, Debug, Default)]
struct EngramSessionState {
    routing_token: Option<String>,
    active_grant_id: Option<String>,
    dispatch_generation: u64,
    consecutive_transport_failures: u8,
    circuit_open: bool,
    next_bind_retry_at: Option<std::time::Instant>,
    checkpoint_in_progress: bool,
    /// Generation of the project-reset fence that owns the checkpoint. Ordinary
    /// turn lifecycle checkpoints use `None`, so a reset release cannot clear a
    /// concurrent checkpoint that it did not start.
    checkpoint_owner_generation: Option<u64>,
    bind_in_progress: bool,
    pending_dispatch: Option<EngramPendingDispatch>,
    rebind_required: bool,
    /// Ephemeral settings-transition fence. New turns stay on the Phase 0
    /// shadow path while the old connection drains and checkpoints.
    project_reset_in_progress: bool,
    disabled_reason: Option<String>,
}

impl EngramSessionState {
    fn begin_checkpoint(&mut self, owner_generation: Option<u64>) -> bool {
        if self.checkpoint_in_progress {
            return false;
        }
        self.checkpoint_in_progress = true;
        self.checkpoint_owner_generation = owner_generation;
        true
    }

    fn clear_checkpoint_if_owned_by(&mut self, owner_generation: Option<u64>) -> bool {
        if !self.checkpoint_in_progress
            || self.checkpoint_owner_generation != owner_generation
        {
            return false;
        }
        self.checkpoint_in_progress = false;
        self.checkpoint_owner_generation = None;
        true
    }
}

#[derive(Clone, Debug)]
struct EngramPendingDispatch {
    dispatch_generation: u64,
    intent_fingerprint: String,
    evaluated: EngramDispatchEvaluation,
    evaluate_latency_ms: u64,
    started_at: std::time::Instant,
    awaiting_runtime_stop_resolution: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EngramTurnDeliveryPreparation {
    Ready,
    Superseded,
    Rejected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EngramDispatchRecordFinish {
    Ready,
    Superseded,
    Rejected,
    DeferredByRuntimeStop,
}

#[derive(Clone, Debug)]
enum EngramDispatchEvaluation {
    Grant {
        grant_id: String,
        delivery_tokens: Vec<String>,
        delivered_range: Option<EngramDeliveredRange>,
    },
    Refuse {
        directive: EngramControlDirectiveCard,
    },
    Defer {
        code: String,
        retry_after_ms: Option<u64>,
        wake_condition: String,
    },
    Degraded {
        code: String,
        detail: String,
    },
}

/// Drops an evaluated dispatch that never reached `turn_begin` ownership.
/// Engram keeps an evaluated grant open, and that grant cannot be checkpointed;
/// the next dispatch must therefore rebind so Engram expires it before issuing
/// another grant. Advancing the generation also prevents the abandoned
/// evaluate idempotency key from being reused.
fn abandon_engram_pending_dispatch(
    record: &mut SessionRecord,
    pending: Option<EngramPendingDispatch>,
) -> bool {
    let Some(pending) = pending else {
        return false;
    };
    record.engram.dispatch_generation = record.engram.dispatch_generation.saturating_add(1);
    if matches!(pending.evaluated, EngramDispatchEvaluation::Grant { .. }) {
        record.engram.rebind_required = true;
    }
    true
}

fn take_and_abandon_engram_pending_dispatch(record: &mut SessionRecord) -> bool {
    let pending = record.engram.pending_dispatch.take();
    abandon_engram_pending_dispatch(record, pending)
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramDeliveredRange {
    from: i64,
    to: i64,
    head: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramControlDirectiveCard {
    directive_id: String,
    kind: String,
    audience: String,
    satisfaction: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramControlStage {
    Dispatch,
    Checkpoint,
    Restart,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramControlCardDecision {
    Grant,
    Defer,
    Refuse,
    Degraded,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramControlCardDispatch {
    SentOnGrant,
    SentWithoutGrant,
    Queued,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramControlFailMode {
    Enforced,
    Shadow,
    Degraded,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramControlLatencyCard {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evaluate: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    begin: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checkpoint: Option<u64>,
    total: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramControlCard {
    schema_version: u16,
    stage: EngramControlStage,
    assurance: String,
    decision: EngramControlCardDecision,
    dispatch: EngramControlCardDispatch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refusal_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    defer_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    grant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    directives: Vec<EngramControlDirectiveCard>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delivered_range: Option<EngramDeliveredRange>,
    latency_ms: EngramControlLatencyCard,
    fail_mode: EngramControlFailMode,
    #[serde(default)]
    repair_armed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_intent: Option<EngramNextIntent>,
}

fn parse_engram_result<T: for<'de> Deserialize<'de>>(
    value: Value,
) -> std::result::Result<T, EngramTransportError> {
    serde_json::from_value(value).map_err(|err| {
        EngramTransportError::protocol(format!("invalid Engram result schema: {err}"))
    })
}

fn engram_actor_id(agent: Agent) -> String {
    match agent {
        Agent::Claude => "termal:claude".to_owned(),
        Agent::Codex => "termal:codex".to_owned(),
        Agent::Cursor => "termal:acp:cursor".to_owned(),
        Agent::Gemini => "termal:acp:gemini".to_owned(),
        Agent::OpenCode => "termal:acp:opencode".to_owned(),
    }
}

fn engram_effects_for_write_policy(write_policy: &DelegationWritePolicy) -> Vec<EngramEffect> {
    let mut effects = vec![EngramEffect::Observe, EngramEffect::Communicate];
    if matches!(
        write_policy,
        DelegationWritePolicy::SharedWorktree { .. }
            | DelegationWritePolicy::IsolatedWorktree { .. }
    ) {
        effects.push(EngramEffect::MutateLocal);
    }
    effects
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("{digest:x}")
}

#[derive(Clone)]
struct EngramBindingTarget {
    adapter: Arc<EngramHostAdapter>,
    connection: EngramConnectionConfig,
    settings: EngramProjectSettings,
    project_id: String,
    project_reset_owner_generation: Option<u64>,
    external_ref: String,
    title: String,
    effects: Vec<EngramEffect>,
    routing_token: Option<String>,
    active_grant_id: Option<String>,
    rebind_required: bool,
    circuit_open: bool,
    next_bind_retry_at: Option<std::time::Instant>,
}

impl EngramBindingTarget {
    fn checkpoint_for_project_reset_off_lock(
        &self,
    ) -> std::result::Result<(), EngramTransportError> {
        let routing_token = self.routing_token.as_ref().ok_or_else(|| {
            EngramTransportError::transport(
                "Engram project reset cannot checkpoint without a routing token",
            )
        })?;
        let grant_id = self.active_grant_id.as_ref().ok_or_else(|| {
            EngramTransportError::transport(
                "Engram project reset cannot checkpoint without an active grant",
            )
        })?;
        let response = self.adapter.request(
            &self.connection,
            &EngramControlRequest::TurnCheckpoint {
                routing_token: routing_token.clone(),
                grant_id: grant_id.clone(),
                next_intent: EngramNextIntent::Exit,
                observations: Vec::new(),
                idempotency_key: engram_checkpoint_idempotency_key(
                    format!(
                        "termal-project-reset-checkpoint:{}:{}",
                        self.connection.session_id, grant_id
                    ),
                    &[],
                ),
            },
            self.settings.call_timeout(),
        )?;
        match parse_engram_result::<EngramTurnCheckpointResponse>(response)? {
            EngramTurnCheckpointResponse::Checkpointed { receipt }
                if receipt.grant_id == *grant_id =>
            {
                Ok(())
            }
            EngramTurnCheckpointResponse::Checkpointed { .. } => {
                Err(EngramTransportError::protocol(
                    "Engram project-reset checkpoint receipt grant id does not match",
                ))
            }
            EngramTurnCheckpointResponse::Refuse { code } => {
                Err(EngramTransportError::remote(EngramControlErrorBody {
                    code,
                    message: "Engram refused the project-reset checkpoint".to_owned(),
                }))
            }
        }
    }
}

#[derive(Clone, Debug)]
struct EngramTurnIntentSnapshot {
    session_id: String,
    dispatch_generation: u64,
    intent_fingerprint: String,
}

fn engram_project_for_session_locked<'a>(
    inner: &'a StateInner,
    session_id: &str,
) -> Option<&'a Project> {
    let mut current_session_id = session_id.to_owned();
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(current_session_id.clone()) {
            return None;
        }
        let record = inner
            .find_session_index(&current_session_id)
            .and_then(|index| inner.sessions.get(index))?;
        if let Some(project_id) = record.session.project_id.as_deref() {
            return inner.find_project(project_id);
        }
        let delegation = inner
            .delegations
            .iter()
            .find(|delegation| delegation.child_session_id == current_session_id)?;
        current_session_id = delegation.parent_session_id.clone();
    }
}

impl AppState {
    fn engram_session_has_child_binding_shape_locked(inner: &StateInner, session_id: &str) -> bool {
        inner
            .delegations
            .iter()
            .any(|delegation| delegation.child_session_id == session_id)
            || inner
                .find_session_index(session_id)
                .and_then(|index| inner.sessions.get(index))
                .is_some_and(|record| record.session.parent_delegation_id.is_some())
    }

    fn engram_binding_target_for_session_shape_locked(
        inner: &StateInner,
        session_id: &str,
        require_child_runtime_enabled: bool,
    ) -> std::result::Result<Option<EngramBindingTarget>, String> {
        Self::engram_binding_target_for_session_shape_with_reset_access_locked(
            inner,
            session_id,
            require_child_runtime_enabled,
            None,
        )
    }

    /// Builds the post-reconfigure binding target while the caller still owns
    /// the project reset fence. The caller must validate that exact fence
    /// generation before using this escape hatch; ordinary dispatch/bind paths
    /// continue to observe the reset as unavailable.
    fn engram_binding_target_for_session_shape_during_project_reset_locked(
        inner: &StateInner,
        session_id: &str,
        require_child_runtime_enabled: bool,
        owner_generation: u64,
    ) -> std::result::Result<Option<EngramBindingTarget>, String> {
        Self::engram_binding_target_for_session_shape_with_reset_access_locked(
            inner,
            session_id,
            require_child_runtime_enabled,
            Some(owner_generation),
        )
    }

    fn engram_binding_target_for_session_shape_with_reset_access_locked(
        inner: &StateInner,
        session_id: &str,
        require_child_runtime_enabled: bool,
        project_reset_owner_generation: Option<u64>,
    ) -> std::result::Result<Option<EngramBindingTarget>, String> {
        if Self::engram_session_has_child_binding_shape_locked(inner, session_id) {
            // A delegation child has a narrower authority shape than its
            // parent. Its durable marker remains authoritative if the
            // delegation row is missing, so child-target failure must not
            // fall back to a parent-shaped project session.
            Self::engram_binding_target_for_child_with_reset_access_locked(
                inner,
                session_id,
                require_child_runtime_enabled,
                project_reset_owner_generation,
            )
        } else {
            Self::engram_binding_target_for_parent_with_reset_access_locked(
                inner,
                session_id,
                project_reset_owner_generation,
            )
        }
    }

    fn engram_child_is_enabled_locked(inner: &StateInner, session_id: &str) -> bool {
        let Some(_delegation) = inner
            .delegations
            .iter()
            .find(|delegation| delegation.child_session_id == session_id)
        else {
            return false;
        };
        let unavailable = inner
            .find_session_index(session_id)
            .and_then(|index| inner.sessions.get(index))
            .is_some_and(|record| {
                record.engram.project_reset_in_progress || record.engram.disabled_reason.is_some()
            });
        if unavailable {
            return false;
        }
        let Some(project) = engram_project_for_session_locked(inner, session_id) else {
            return false;
        };
        !inner.engram_project_resets.contains(&project.id)
            && (project.remote_id == LOCAL_REMOTE_ID)
            && project
                .engram
                .as_ref()
                .is_some_and(EngramProjectSettings::is_runtime_enabled)
    }

    fn engram_child_requires_dispatch_card_locked(inner: &StateInner, session_id: &str) -> bool {
        if Self::engram_child_is_enabled_locked(inner, session_id) {
            return true;
        }
        let disabled = inner
            .find_session_index(session_id)
            .and_then(|index| inner.sessions.get(index))
            .is_some_and(|record| {
                !record.engram.project_reset_in_progress && record.engram.disabled_reason.is_some()
            });
        disabled
            && engram_project_for_session_locked(inner, session_id)
                .filter(|project| {
                    project.remote_id == LOCAL_REMOTE_ID
                        && !inner.engram_project_resets.contains(&project.id)
                })
                .and_then(|project| project.engram.as_ref())
                .is_some_and(EngramProjectSettings::is_runtime_enabled)
    }

    fn rebind_engram_session_after_runtime_loss(&self, session_id: &str) {
        let target = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                .ok()
                .flatten()
        };
        let Some(target) = target else {
            return;
        };
        if let Err(error) = self.bind_engram_target_off_lock(target) {
            self.record_engram_transport_failure(session_id, &error);
            eprintln!("engram> session={session_id} runtime-loss rebind degraded: {error}");
        }
    }

    fn recover_engram_sessions_after_boot(&self) {
        let targets = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let mut targets = HashMap::<String, EngramBindingTarget>::new();
            for delegation in &inner.delegations {
                if let Ok(Some(target)) = Self::engram_binding_target_for_parent_locked(
                    &inner,
                    &delegation.parent_session_id,
                ) {
                    targets
                        .entry(target.connection.session_id.clone())
                        .or_insert(target);
                }
                if let Ok(Some(target)) = Self::engram_binding_target_for_child_locked(
                    &inner,
                    &delegation.child_session_id,
                    true,
                ) {
                    targets
                        .entry(target.connection.session_id.clone())
                        .or_insert(target);
                }
            }
            targets.into_values().collect::<Vec<_>>()
        };
        for batch in targets.chunks(ENGRAM_BOOT_RECOVERY_CONCURRENCY) {
            std::thread::scope(|scope| {
                let mut handles = Vec::with_capacity(batch.len());
                for target in batch.iter().cloned() {
                    let session_id = target.connection.session_id.clone();
                    let thread_name = format!("engram-recover-{session_id}");
                    match std::thread::Builder::new().name(thread_name).spawn_scoped(
                        scope,
                        move || {
                            let started_at = std::time::Instant::now();
                            let result = self.bind_engram_target_off_lock(target);
                            (started_at.elapsed(), result)
                        },
                    ) {
                        Ok(handle) => handles.push((session_id, handle)),
                        Err(error) => self.finish_engram_restart_recovery(
                            &session_id,
                            Duration::ZERO,
                            Err(EngramTransportError::transport(format!(
                                "failed spawning bounded restart recovery worker: {error}"
                            ))),
                        ),
                    }
                }
                for (session_id, handle) in handles {
                    match handle.join() {
                        Ok((elapsed, result)) => {
                            self.finish_engram_restart_recovery(&session_id, elapsed, result);
                        }
                        Err(_) => self.finish_engram_restart_recovery(
                            &session_id,
                            Duration::ZERO,
                            Err(EngramTransportError::transport(
                                "restart recovery worker panicked",
                            )),
                        ),
                    }
                }
            });
        }
    }

    fn finish_engram_restart_recovery(
        &self,
        session_id: &str,
        elapsed: Duration,
        result: std::result::Result<String, EngramTransportError>,
    ) {
        match result {
            Ok(_) => {
                self.record_engram_transport_success(session_id);
                self.append_engram_restart_card(
                    session_id,
                    EngramControlCardDecision::Grant,
                    None,
                    EngramControlFailMode::Shadow,
                    elapsed,
                );
            }
            Err(error) => {
                self.record_engram_transport_failure(session_id, &error);
                self.append_engram_restart_card(
                    session_id,
                    EngramControlCardDecision::Degraded,
                    error
                        .code
                        .clone()
                        .or_else(|| Some("rebind_failed".to_owned())),
                    EngramControlFailMode::Degraded,
                    elapsed,
                );
                eprintln!("engram> session={session_id} restart recovery degraded: {error}");
            }
        }
    }

    fn append_engram_restart_card(
        &self,
        session_id: &str,
        decision: EngramControlCardDecision,
        refusal_code: Option<String>,
        fail_mode: EngramControlFailMode,
        elapsed: Duration,
    ) {
        let (revision, creates) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            let message_id = inner.next_message_id();
            let card = EngramControlCard {
                schema_version: ENGRAM_CONTROL_SCHEMA_VERSION,
                stage: EngramControlStage::Restart,
                assurance: "advisory".to_owned(),
                decision,
                dispatch: if inner.sessions[index].engram.active_grant_id.is_some() {
                    EngramControlCardDispatch::SentOnGrant
                } else {
                    EngramControlCardDispatch::Queued
                },
                refusal_code,
                defer_code: None,
                grant_id: inner.sessions[index].engram.active_grant_id.clone(),
                directives: Vec::new(),
                delivered_range: None,
                latency_ms: EngramControlLatencyCard {
                    evaluate: None,
                    begin: None,
                    checkpoint: None,
                    total: duration_millis(elapsed),
                },
                fail_mode,
                repair_armed: false,
                next_intent: Some(EngramNextIntent::Wait),
            };
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            let message_index = push_message_on_record(
                record,
                Message::EngramControl {
                    id: message_id,
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    card,
                },
            );
            let creates = message_created_delta_parts_for_indices(record, vec![message_index]);
            let revision = match self.commit_persisted_delta_locked(&mut inner) {
                Ok(revision) => revision,
                Err(error) => {
                    eprintln!(
                        "engram> session={session_id} failed persisting restart card: {error:#}"
                    );
                    return;
                }
            };
            (revision, creates)
        };
        self.publish_message_created_delta_parts(revision, creates);
    }

    fn checkpoint_engram_turn_off_lock(
        &self,
        session_id: &str,
        runtime_token: Option<&RuntimeToken>,
        active_turn_generation: Option<u64>,
        next_intent: EngramNextIntent,
        project_reset_owner_generation: Option<u64>,
    ) {
        let snapshot = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            if runtime_token
                .is_some_and(|token| !inner.sessions[index].runtime.matches_runtime_token(token))
            {
                return;
            }
            if active_turn_generation.is_some_and(|generation| {
                inner.sessions[index].active_turn_generation != generation
            }) {
                return;
            }
            if runtime_token.is_some() && inner.sessions[index].runtime_stop_in_progress {
                return;
            }
            let Some(grant_id) = inner.sessions[index].engram.active_grant_id.clone() else {
                return;
            };
            // A global/project kill switch prevents new evaluate/bind calls, but
            // it must not strand a grant that was already begun. Resolve the
            // captured old connection even while runtime enablement is off so a
            // terminal transition can checkpoint it exactly once.
            let target = Self::engram_binding_target_for_child_with_reset_access_locked(
                &inner,
                session_id,
                false,
                project_reset_owner_generation,
            )
            .ok()
            .flatten();
            if target
                .as_ref()
                .is_some_and(|target| !target.settings.enabled)
            {
                // A project disable already made its one bounded checkpoint
                // attempt. If that attempt failed, retain the authority for
                // same-store recovery instead of retrying it from ordinary
                // shadow-runtime lifecycle transitions. Global disable still
                // reaches this path because the persisted project setting is
                // enabled in that case.
                return;
            }
            if !inner
                .session_mut_by_index(index)
                .expect("session index should be valid")
                .engram
                .begin_checkpoint(project_reset_owner_generation)
            {
                return;
            }
            (grant_id, target)
        };
        let (grant_id, target) = snapshot;
        let started_at = std::time::Instant::now();
        let outcome = match target {
            Some(target) => match target.routing_token.as_ref() {
                Some(routing_token) => target
                    .adapter
                    .request(
                        &target.connection,
                        &EngramControlRequest::TurnCheckpoint {
                            routing_token: routing_token.clone(),
                            grant_id: grant_id.clone(),
                            next_intent,
                            observations: Vec::new(),
                            idempotency_key: engram_checkpoint_idempotency_key(
                                format!(
                                    "termal-checkpoint:{}:{}:{}",
                                    session_id,
                                    grant_id,
                                    next_intent.as_idempotency_component()
                                ),
                                &[],
                            ),
                        },
                        target.settings.call_timeout(),
                    )
                    .and_then(parse_engram_result::<EngramTurnCheckpointResponse>)
                    .and_then(|response| match response {
                        EngramTurnCheckpointResponse::Checkpointed { receipt }
                            if receipt.grant_id == grant_id =>
                        {
                            Ok(())
                        }
                        EngramTurnCheckpointResponse::Checkpointed { .. } => {
                            Err(EngramTransportError::protocol(
                                "Engram checkpoint receipt grant id does not match",
                            ))
                        }
                        EngramTurnCheckpointResponse::Refuse { code } => {
                            Err(EngramTransportError::remote(EngramControlErrorBody {
                                code,
                                message: "Engram refused the turn checkpoint".to_owned(),
                            }))
                        }
                    }),
                None => Err(EngramTransportError::transport(
                    "Engram checkpoint has no routing token",
                )),
            },
            None => Err(EngramTransportError::transport(
                "Engram checkpoint target is unavailable",
            )),
        };
        match &outcome {
            Ok(()) => self.record_engram_transport_success(session_id),
            Err(error) => self.record_engram_transport_failure(session_id, error),
        }
        let (decision, refusal_code, fail_mode) = match outcome {
            Ok(()) => (
                EngramControlCardDecision::Grant,
                None,
                EngramControlFailMode::Shadow,
            ),
            Err(error) => (
                EngramControlCardDecision::Degraded,
                Some(error.code.unwrap_or_else(|| "checkpoint_failed".to_owned())),
                EngramControlFailMode::Degraded,
            ),
        };
        let card = EngramControlCard {
            schema_version: ENGRAM_CONTROL_SCHEMA_VERSION,
            stage: EngramControlStage::Checkpoint,
            assurance: "advisory".to_owned(),
            decision,
            // A checkpoint exists iff this turn was begun from a grant; the
            // checkpoint result does not rewrite how the prompt was sent.
            dispatch: EngramControlCardDispatch::SentOnGrant,
            refusal_code,
            defer_code: None,
            grant_id: Some(grant_id.clone()),
            directives: Vec::new(),
            delivered_range: None,
            latency_ms: EngramControlLatencyCard {
                evaluate: None,
                begin: None,
                checkpoint: Some(duration_millis(started_at.elapsed())),
                total: duration_millis(started_at.elapsed()),
            },
            fail_mode,
            repair_armed: false,
            next_intent: Some(next_intent),
        };
        self.finish_engram_checkpoint_record(
            session_id,
            &grant_id,
            decision,
            card,
            project_reset_owner_generation,
        );
    }

    /// A destructive session removal must not tear down the sidecar while a
    /// checkpoint started by another terminal callback is still in flight.
    /// Control calls are deadline-bounded to at most ten seconds, so this
    /// off-lock poll is finite and only exercised by that rare race.
    fn wait_for_engram_checkpoint_completion(&self, session_id: &str) {
        let deadline = std::time::Instant::now() + ENGRAM_CONTROL_SETTLE_TIMEOUT;
        loop {
            let in_progress = {
                let inner = self.inner.lock().expect("state mutex poisoned");
                inner
                    .find_session_index(session_id)
                    .and_then(|index| inner.sessions.get(index))
                    .is_some_and(|record| record.engram.checkpoint_in_progress)
            };
            if !in_progress {
                return;
            }
            if std::time::Instant::now() >= deadline {
                eprintln!(
                    "engram> session={session_id} checkpoint wait exceeded the control deadline"
                );
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn finish_engram_checkpoint_record(
        &self,
        session_id: &str,
        grant_id: &str,
        decision: EngramControlCardDecision,
        card: EngramControlCard,
        project_reset_owner_generation: Option<u64>,
    ) {
        let exited = matches!(card.next_intent, Some(EngramNextIntent::Exit));
        let (revision, creates) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            if inner.sessions[index].engram.active_grant_id.as_deref() != Some(grant_id) {
                inner.sessions[index]
                    .engram
                    .clear_checkpoint_if_owned_by(project_reset_owner_generation);
                return;
            }
            let message_id = inner.next_message_id();
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            record
                .engram
                .clear_checkpoint_if_owned_by(project_reset_owner_generation);
            if decision == EngramControlCardDecision::Grant {
                record.engram.active_grant_id = None;
                if exited {
                    record.engram.rebind_required = true;
                }
            }
            let message_index = push_message_on_record(
                record,
                Message::EngramControl {
                    id: message_id,
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    card,
                },
            );
            let creates = message_created_delta_parts_for_indices(record, vec![message_index]);
            let revision = match self.commit_persisted_delta_locked(&mut inner) {
                Ok(revision) => revision,
                Err(error) => {
                    eprintln!(
                        "engram> session={session_id} failed persisting checkpoint card: {error:#}"
                    );
                    return;
                }
            };
            (revision, creates)
        };
        self.publish_message_created_delta_parts(revision, creates);
    }

    fn checkpoint_successful_engram_turn_off_lock(
        &self,
        session_id: &str,
        runtime_token: &RuntimeToken,
        active_turn_generation: Option<u64>,
    ) {
        let next_intent = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            if inner.sessions[index].queued_prompts.is_empty() {
                EngramNextIntent::Wait
            } else {
                EngramNextIntent::Continue
            }
        };
        self.checkpoint_engram_turn_off_lock(
            session_id,
            Some(runtime_token),
            active_turn_generation,
            next_intent,
            None,
        );
    }

    fn prepare_engram_turn_delivery_off_lock(
        &self,
        session_id: &str,
        dispatch_generation: u64,
    ) -> EngramTurnDeliveryPreparation {
        let snapshot = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return EngramTurnDeliveryPreparation::Superseded;
            };
            let record = &inner.sessions[index];
            let Some(pending) = record.engram.pending_dispatch.clone() else {
                return EngramTurnDeliveryPreparation::Superseded;
            };
            if pending.dispatch_generation != dispatch_generation {
                return EngramTurnDeliveryPreparation::Superseded;
            };
            let target = Self::engram_binding_target_for_child_locked(&inner, session_id, true)
                .ok()
                .flatten();
            (pending, target)
        };
        let (pending, mut binding_target) = snapshot;
        let mut evaluation = pending.evaluated.clone();
        let mut evaluate_latency_ms = pending.evaluate_latency_ms;
        let mut begin_latency_ms = None;
        let mut retry_used = false;
        let mut dispatch_budget_started_at = pending.started_at;
        let mut active_grant_id = None;
        // Engram records an issued grant before `turn_begin`. If TermAl can no
        // longer deliver this dispatch before begin succeeds, that grant
        // cannot be checkpointed: the next attempt must rebind so Engram can
        // expire the issued-but-unbegun grant.
        let mut issued_unbegun_grant_id = None;

        let (decision, refusal_code, directives, delivered_range, fail_mode) = loop {
            match evaluation {
                EngramDispatchEvaluation::Grant {
                    grant_id,
                    delivery_tokens,
                    delivered_range,
                } => {
                    issued_unbegun_grant_id = Some(grant_id.clone());
                    let Some(target) = binding_target.as_ref() else {
                        break (
                            EngramControlCardDecision::Degraded,
                            Some("binding_unavailable".to_owned()),
                            Vec::new(),
                            delivered_range,
                            EngramControlFailMode::Degraded,
                        );
                    };
                    let Some(routing_token) = target.routing_token.as_ref() else {
                        break (
                            EngramControlCardDecision::Degraded,
                            Some("routing_token_unavailable".to_owned()),
                            Vec::new(),
                            delivered_range,
                            EngramControlFailMode::Degraded,
                        );
                    };
                    let Some(timeout) = engram_remaining_dispatch_timeout(
                        dispatch_budget_started_at,
                        target.settings.call_timeout(),
                    ) else {
                        break (
                            EngramControlCardDecision::Degraded,
                            Some("dispatch_budget_exhausted".to_owned()),
                            Vec::new(),
                            delivered_range,
                            EngramControlFailMode::Degraded,
                        );
                    };
                    let begin_started = std::time::Instant::now();
                    let begin = target
                        .adapter
                        .request(
                            &target.connection,
                            &EngramControlRequest::TurnBegin {
                                routing_token: routing_token.clone(),
                                grant_id: grant_id.clone(),
                                delivery_tokens,
                                idempotency_key: format!(
                                    "termal-begin:{}:{}:{}",
                                    session_id, pending.dispatch_generation, grant_id
                                ),
                            },
                            timeout,
                        )
                        .and_then(parse_engram_result::<EngramTurnBeginResponse>);
                    begin_latency_ms = Some(duration_millis(begin_started.elapsed()));
                    match begin {
                        Ok(EngramTurnBeginResponse::Begin { receipt }) => {
                            self.record_engram_transport_success(session_id);
                            if receipt.grant_id != grant_id {
                                break (
                                    EngramControlCardDecision::Degraded,
                                    Some("begin_grant_mismatch".to_owned()),
                                    Vec::new(),
                                    delivered_range,
                                    EngramControlFailMode::Degraded,
                                );
                            }
                            active_grant_id = Some(grant_id.clone());
                            issued_unbegun_grant_id = None;
                            break (
                                EngramControlCardDecision::Grant,
                                None,
                                Vec::new(),
                                delivered_range,
                                EngramControlFailMode::Shadow,
                            );
                        }
                        Ok(EngramTurnBeginResponse::Refuse { code })
                            if !retry_used && engram_begin_refusal_allows_reevaluation(&code) =>
                        {
                            retry_used = true;
                            // Engram has definitively rejected this issued
                            // grant. Only a subsequent re-evaluate grant needs
                            // orphan recovery if begin cannot complete.
                            issued_unbegun_grant_id = None;
                            let reevaluate_target = if code == "stale_fence" {
                                self.mark_engram_rebind_required(session_id, Some(target));
                                match self.ensure_engram_child_bound_off_lock(session_id) {
                                    Ok(Some(refreshed)) => refreshed,
                                    Ok(None) => {
                                        break (
                                            EngramControlCardDecision::Degraded,
                                            Some("binding_unavailable".to_owned()),
                                            Vec::new(),
                                            delivered_range,
                                            EngramControlFailMode::Degraded,
                                        );
                                    }
                                    Err(error) => {
                                        self.record_engram_transport_failure(session_id, &error);
                                        let code =
                                            self.engram_failure_card_code(session_id, &error);
                                        break (
                                            EngramControlCardDecision::Degraded,
                                            Some(code),
                                            Vec::new(),
                                            delivered_range,
                                            EngramControlFailMode::Degraded,
                                        );
                                    }
                                }
                            } else {
                                target.clone()
                            };
                            let reevaluate_routing_token = reevaluate_target
                                .routing_token
                                .clone()
                                .expect("re-evaluation target should carry a routing token");
                            if code == "stale_fence" {
                                binding_target = Some(reevaluate_target.clone());
                                // The stale recovery bind may perform a
                                // separately bounded work-focus process read.
                                // Start the one allowed re-evaluate/begin hot
                                // budget only after the refreshed bind exists.
                                dispatch_budget_started_at = std::time::Instant::now();
                            }
                            let Some(timeout) = engram_remaining_dispatch_timeout(
                                dispatch_budget_started_at,
                                reevaluate_target.settings.call_timeout(),
                            ) else {
                                break (
                                    EngramControlCardDecision::Degraded,
                                    Some("dispatch_budget_exhausted".to_owned()),
                                    Vec::new(),
                                    delivered_range,
                                    EngramControlFailMode::Degraded,
                                );
                            };
                            let reevaluate_started = std::time::Instant::now();
                            let reevaluated = reevaluate_target
                                .adapter
                                .request(
                                    &reevaluate_target.connection,
                                    &EngramControlRequest::TurnEvaluate {
                                        routing_token: reevaluate_routing_token,
                                        idempotency_key: format!(
                                            "termal-reevaluate:{}:{}:{}",
                                            session_id,
                                            pending.dispatch_generation,
                                            pending.intent_fingerprint
                                        ),
                                        intent_fingerprint: pending.intent_fingerprint.clone(),
                                        purpose: "ordinary".to_owned(),
                                        requested_effects: reevaluate_target.effects.clone(),
                                        resource_intents: Vec::new(),
                                    },
                                    timeout,
                                )
                                .and_then(parse_engram_result::<EngramTurnDecisionResponse>);
                            evaluate_latency_ms = evaluate_latency_ms
                                .saturating_add(duration_millis(reevaluate_started.elapsed()));
                            evaluation = match reevaluated {
                                Ok(EngramTurnDecisionResponse::Grant { grant }) => {
                                    let delivered_range = grant.delivery.as_ref().map(|delivery| {
                                        EngramDeliveredRange {
                                            from: delivery.page.from_cursor,
                                            to: delivery.page.to_cursor,
                                            head: delivery.page.head_cursor,
                                        }
                                    });
                                    let delivery_tokens = grant
                                        .delivery
                                        .iter()
                                        .map(|delivery| delivery.page.delivery_token.clone())
                                        .collect();
                                    self.record_engram_transport_success(session_id);
                                    EngramDispatchEvaluation::Grant {
                                        grant_id: grant.grant_id,
                                        delivery_tokens,
                                        delivered_range,
                                    }
                                }
                                Ok(EngramTurnDecisionResponse::Refuse { directive }) => {
                                    self.record_engram_transport_success(session_id);
                                    if engram_evaluation_refusal_requires_rebind(&directive.code) {
                                        self.mark_engram_rebind_required(
                                            session_id,
                                            Some(&reevaluate_target),
                                        );
                                    }
                                    EngramDispatchEvaluation::Refuse {
                                        directive: EngramControlDirectiveCard {
                                            directive_id: directive.directive_id,
                                            kind: directive.code,
                                            audience: directive.target,
                                            satisfaction: directive.satisfaction,
                                        },
                                    }
                                }
                                Ok(EngramTurnDecisionResponse::Defer { deferral }) => {
                                    self.record_engram_transport_success(session_id);
                                    EngramDispatchEvaluation::Defer {
                                        code: deferral.code,
                                        retry_after_ms: deferral.retry_after_ms,
                                        wake_condition: deferral.wake_condition,
                                    }
                                }
                                Err(error) => {
                                    self.record_engram_transport_failure(session_id, &error);
                                    let code = self.engram_failure_card_code(session_id, &error);
                                    EngramDispatchEvaluation::Degraded {
                                        code,
                                        detail: error.message,
                                    }
                                }
                            };
                        }
                        Ok(EngramTurnBeginResponse::Refuse { code }) => {
                            self.record_engram_transport_success(session_id);
                            if engram_begin_refusal_allows_reevaluation(&code) {
                                // A second moved-basis refusal is not retried,
                                // but Engram has still expired that grant.
                                issued_unbegun_grant_id = None;
                            }
                            // Every other refusal leaves the grant issued in
                            // Engram. Keep the orphan marker so the next
                            // dispatch expires it through a fresh bind.
                            break (
                                EngramControlCardDecision::Refuse,
                                Some(code),
                                Vec::new(),
                                delivered_range,
                                EngramControlFailMode::Shadow,
                            );
                        }
                        Err(error) => {
                            self.record_engram_transport_failure(session_id, &error);
                            let code = self.engram_failure_card_code(session_id, &error);
                            break (
                                EngramControlCardDecision::Degraded,
                                Some(code),
                                Vec::new(),
                                delivered_range,
                                EngramControlFailMode::Degraded,
                            );
                        }
                    }
                }
                EngramDispatchEvaluation::Refuse { directive } => {
                    let code = directive.kind.clone();
                    break (
                        EngramControlCardDecision::Refuse,
                        Some(code),
                        vec![directive],
                        None,
                        EngramControlFailMode::Shadow,
                    );
                }
                EngramDispatchEvaluation::Defer {
                    code,
                    retry_after_ms,
                    wake_condition,
                } => {
                    let _ = (retry_after_ms, wake_condition);
                    break (
                        EngramControlCardDecision::Defer,
                        Some(code),
                        Vec::new(),
                        None,
                        EngramControlFailMode::Shadow,
                    );
                }
                EngramDispatchEvaluation::Degraded { code, detail } => {
                    let _ = detail;
                    break (
                        EngramControlCardDecision::Degraded,
                        Some(code),
                        Vec::new(),
                        None,
                        EngramControlFailMode::Degraded,
                    );
                }
            }
        };

        let repair_armed = issued_unbegun_grant_id.is_some()
            || (decision == EngramControlCardDecision::Refuse
                && refusal_code
                    .as_deref()
                    .is_some_and(engram_evaluation_refusal_requires_rebind));
        let dispatch = if active_grant_id.is_some() {
            EngramControlCardDispatch::SentOnGrant
        } else {
            EngramControlCardDispatch::SentWithoutGrant
        };
        let (refusal_code, defer_code) = if decision == EngramControlCardDecision::Defer {
            (None, refusal_code)
        } else {
            (refusal_code, None)
        };
        let card = EngramControlCard {
            schema_version: ENGRAM_CONTROL_SCHEMA_VERSION,
            stage: EngramControlStage::Dispatch,
            assurance: "advisory".to_owned(),
            decision,
            dispatch,
            refusal_code,
            defer_code,
            grant_id: active_grant_id.clone(),
            directives,
            delivered_range,
            latency_ms: EngramControlLatencyCard {
                evaluate: Some(evaluate_latency_ms),
                begin: begin_latency_ms,
                checkpoint: None,
                total: duration_millis(pending.started_at.elapsed()),
            },
            fail_mode,
            repair_armed,
            next_intent: None,
        };
        let preparation = loop {
            match self.finish_engram_dispatch_record(
                session_id,
                pending.dispatch_generation,
                active_grant_id.clone(),
                card.clone(),
            ) {
                EngramDispatchRecordFinish::Ready => {
                    break EngramTurnDeliveryPreparation::Ready;
                }
                EngramDispatchRecordFinish::Superseded => {
                    break EngramTurnDeliveryPreparation::Superseded;
                }
                EngramDispatchRecordFinish::Rejected => {
                    break EngramTurnDeliveryPreparation::Rejected;
                }
                EngramDispatchRecordFinish::DeferredByRuntimeStop => {
                    if !self.wait_for_engram_runtime_stop_resolution(
                        session_id,
                        pending.dispatch_generation,
                    ) {
                        break EngramTurnDeliveryPreparation::Rejected;
                    }
                }
            }
        };
        if issued_unbegun_grant_id.is_some() {
            // This includes a dispatch invalidated between evaluate and begin,
            // plus a dispatch whose shared evaluate/begin budget expired before
            // begin. Engram rejects checkpointing such a grant; force the next
            // attempt through status + fresh bind instead.
            self.mark_engram_rebind_required(session_id, binding_target.as_ref());
        }
        if preparation != EngramTurnDeliveryPreparation::Ready
            && let (Some(grant_id), Some(target)) = (active_grant_id, binding_target)
            && let Some(routing_token) = target.routing_token.as_ref()
        {
            // The begin happened against this captured connection. Even if the
            // project was disabled or reconfigured while it was in flight, the
            // begun grant belongs to the old Engram store and must be closed
            // there before that sidecar is reaped.
            let _ = target.adapter.request(
                &target.connection,
                &EngramControlRequest::TurnCheckpoint {
                    routing_token: routing_token.clone(),
                    grant_id: grant_id.clone(),
                    next_intent: EngramNextIntent::Exit,
                    observations: Vec::new(),
                    idempotency_key: engram_checkpoint_idempotency_key(
                        format!("termal-stale-begin-checkpoint:{session_id}:{grant_id}"),
                        &[],
                    ),
                },
                target.settings.call_timeout(),
            );
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(index) = inner.find_session_index(session_id) {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                if record.engram.routing_token.as_deref() == Some(routing_token.as_str()) {
                    record.engram.rebind_required = true;
                }
            }
        }
        if preparation == EngramTurnDeliveryPreparation::Superseded {
            // A project reset advances the generation before waiting for the
            // off-lock evaluate/begin to quiesce. Once the stale operation has
            // finished (and any begun grant above has been closed), release
            // only its matching pending marker. A newer dispatch must remain
            // untouched.
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(index) = inner.find_session_index(session_id) {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                if record
                    .engram
                    .pending_dispatch
                    .as_ref()
                    .is_some_and(|current| {
                        current.dispatch_generation == pending.dispatch_generation
                    })
                {
                    record.engram.pending_dispatch = None;
                }
            }
        }
        preparation
    }

    fn finish_engram_dispatch_record(
        &self,
        session_id: &str,
        dispatch_generation: u64,
        active_grant_id: Option<String>,
        card: EngramControlCard,
    ) -> EngramDispatchRecordFinish {
        let (revision, creates) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return EngramDispatchRecordFinish::Superseded;
            };
            let record = &inner.sessions[index];
            let pending_dispatch_is_current = record
                .engram
                .pending_dispatch
                .as_ref()
                .is_some_and(|pending| pending.dispatch_generation == dispatch_generation);
            if record.engram.dispatch_generation == dispatch_generation
                && pending_dispatch_is_current
                && record.session.status == SessionStatus::Active
                && record.runtime_stop_in_progress
            {
                // Stop has borrowed the runtime but has not yet established
                // whether that borrow will commit. A successful Stop will
                // clear this pending dispatch; a failed non-best-effort Stop
                // restores the same Active runtime and must let this begun
                // turn reach it. Do not collapse both outcomes into a silent
                // supersede while the answer is still unknown.
                inner.sessions[index]
                    .engram
                    .pending_dispatch
                    .as_mut()
                    .expect("current Engram dispatch should remain pending")
                    .awaiting_runtime_stop_resolution = true;
                return EngramDispatchRecordFinish::DeferredByRuntimeStop;
            }
            let dispatch_is_current = record.engram.dispatch_generation == dispatch_generation
                && pending_dispatch_is_current
                && record.session.status == SessionStatus::Active;
            if !dispatch_is_current {
                return if record.engram.project_reset_in_progress
                    && pending_dispatch_is_current
                    && record.session.status == SessionStatus::Active
                {
                    EngramDispatchRecordFinish::Rejected
                } else {
                    EngramDispatchRecordFinish::Superseded
                };
            }
            let message_id = inner.next_message_id();
            let message = Message::EngramControl {
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                card,
            };
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            record.engram.pending_dispatch = None;
            if let Some(grant_id) = active_grant_id {
                record.engram.active_grant_id = Some(grant_id);
            }
            let message_index = push_message_on_record(record, message);
            let creates = message_created_delta_parts_for_indices(record, vec![message_index]);
            let revision = match self.commit_persisted_delta_locked(&mut inner) {
                Ok(revision) => revision,
                Err(error) => {
                    // Persistence failures are ambiguous: SQLite may already
                    // have committed before a later hardening step failed.
                    // Keep the mutated in-memory record authoritative and
                    // continue Phase 0 delivery fail-open. Rejecting here would
                    // leave a durable/in-memory dispatch card and open grant for
                    // a prompt that was never sent to the runtime.
                    eprintln!(
                        "engram> session={session_id} failed persisting dispatch card; \
                         publishing in-memory state and continuing delivery: {error:#}"
                    );
                    inner.revision
                }
            };
            (revision, creates)
        };
        self.publish_message_created_delta_parts(revision, creates);
        EngramDispatchRecordFinish::Ready
    }

    /// Rejects only the reset-invalidated dispatch that still owns its pending
    /// marker. The marker check and runtime/session teardown share one lock so
    /// a Stop followed by a shadow-path successor cannot be cleared or failed
    /// in the gap after off-lock `turn_begin` completion.
    fn reject_engram_turn_delivery_if_current(
        &self,
        session_id: &str,
        dispatch_generation: u64,
        error_message: &str,
    ) -> Result<bool> {
        let cleaned = error_message.trim();
        let (revision, creates) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return Ok(false);
            };
            let rejection_is_current = {
                let record = &inner.sessions[index];
                record
                    .engram
                    .pending_dispatch
                    .as_ref()
                    .is_some_and(|pending| pending.dispatch_generation == dispatch_generation)
                    && record.session.status == SessionStatus::Active
                    && !record.runtime_stop_in_progress
            };
            if !rejection_is_current {
                return Ok(false);
            }

            let message_id = (!cleaned.is_empty()).then(|| inner.next_message_id());
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            record.engram.pending_dispatch = None;
            record.clear_runtime();
            record.clear_runtime_reset();
            record.orchestrator_auto_dispatch_blocked = false;
            record.clear_runtime_stop();
            record.deferred_stop_callbacks.clear();
            clear_active_turn_file_change_tracking(record);
            clear_all_pending_requests(record);
            record.session.status = SessionStatus::Error;
            record.session.preview = make_preview(cleaned);
            let creates = if let Some(message_id) = message_id {
                let message = Message::Text {
                    attachments: Vec::new(),
                    id: message_id,
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: format!("Turn failed: {cleaned}"),
                    expanded_text: None,
                    source: None,
                };
                let message_index = push_message_on_record(record, message);
                message_created_delta_parts_for_indices(record, vec![message_index])
            } else {
                Vec::new()
            };
            let revision = self.commit_locked(&mut inner)?;
            (revision, creates)
        };
        self.publish_message_created_delta_parts(revision, creates);
        Ok(true)
    }

    /// Waits for an in-flight Stop to either commit or roll back before a
    /// begun Engram turn decides whether runtime delivery is still owned.
    /// Dedicated runtime stops settle well within this guard (shared Codex's
    /// longer failure path is best-effort); the bound prevents a broken stop
    /// implementation from stranding the delivery worker forever.
    fn wait_for_engram_runtime_stop_resolution(
        &self,
        session_id: &str,
        dispatch_generation: u64,
    ) -> bool {
        let deadline = std::time::Instant::now() + ENGRAM_CONTROL_SETTLE_TIMEOUT;
        loop {
            let still_waiting = {
                let inner = self.inner.lock().expect("state mutex poisoned");
                let Some(index) = inner.find_session_index(session_id) else {
                    return true;
                };
                let record = &inner.sessions[index];
                record.runtime_stop_in_progress
                    && record.engram.dispatch_generation == dispatch_generation
                    && record
                        .engram
                        .pending_dispatch
                        .as_ref()
                        .is_some_and(|pending| pending.dispatch_generation == dispatch_generation)
            };
            if !still_waiting {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                eprintln!(
                    "engram> session={session_id} runtime Stop arbitration exceeded the stop deadline"
                );
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn engram_binding_target_for_child_locked(
        inner: &StateInner,
        session_id: &str,
        require_runtime_enabled: bool,
    ) -> std::result::Result<Option<EngramBindingTarget>, String> {
        Self::engram_binding_target_for_child_with_reset_access_locked(
            inner,
            session_id,
            require_runtime_enabled,
            None,
        )
    }

    fn engram_binding_target_for_child_with_reset_access_locked(
        inner: &StateInner,
        session_id: &str,
        require_runtime_enabled: bool,
        project_reset_owner_generation: Option<u64>,
    ) -> std::result::Result<Option<EngramBindingTarget>, String> {
        let Some(delegation) = inner
            .delegations
            .iter()
            .find(|delegation| delegation.child_session_id == session_id)
        else {
            return Ok(None);
        };
        let child = inner
            .find_session_index(session_id)
            .and_then(|index| inner.sessions.get(index))
            .ok_or_else(|| format!("delegation child session `{session_id}` is missing"))?;
        if !child.is_local_session() {
            return Ok(None);
        }
        let Some(project) = engram_project_for_session_locked(inner, session_id) else {
            // Engram is configured per project, so a projectless delegation
            // is an ordinary disabled path rather than an adapter failure.
            return Ok(None);
        };
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
            || (require_runtime_enabled && child.engram.disabled_reason.is_some())
            || (require_runtime_enabled && !settings.is_runtime_enabled())
        {
            return Ok(None);
        }
        let binary_path = settings
            .binary_path
            .as_deref()
            .map(PathBuf::from)
            .ok_or_else(|| "enabled Engram project is missing binaryPath".to_owned())?;
        let home = settings
            .home
            .as_deref()
            .map(PathBuf::from)
            .ok_or_else(|| "enabled Engram project is missing home".to_owned())?;
        let root = PathBuf::from(&project.root_path);
        Ok(Some(EngramBindingTarget {
            adapter: inner.engram_host_adapter.clone(),
            connection: EngramConnectionConfig {
                binary_path,
                project_file: root.join(".engram-project"),
                home,
                project_root: root,
                actor_id: engram_actor_id(child.session.agent),
                session_id: session_id.to_owned(),
            },
            settings: settings.clone(),
            project_id: project.id.clone(),
            project_reset_owner_generation,
            external_ref: format!("termal:delegation:{}", delegation.id),
            title: delegation.title.clone(),
            effects: engram_effects_for_write_policy(&delegation.write_policy),
            routing_token: child.engram.routing_token.clone(),
            active_grant_id: child.engram.active_grant_id.clone(),
            rebind_required: child.engram.rebind_required,
            circuit_open: child.engram.circuit_open,
            next_bind_retry_at: child.engram.next_bind_retry_at,
        }))
    }

    fn engram_binding_target_for_parent_locked(
        inner: &StateInner,
        parent_session_id: &str,
    ) -> std::result::Result<Option<EngramBindingTarget>, String> {
        Self::engram_binding_target_for_parent_with_reset_access_locked(
            inner,
            parent_session_id,
            None,
        )
    }

    fn engram_binding_target_for_parent_with_reset_access_locked(
        inner: &StateInner,
        parent_session_id: &str,
        project_reset_owner_generation: Option<u64>,
    ) -> std::result::Result<Option<EngramBindingTarget>, String> {
        let parent = inner
            .find_session_index(parent_session_id)
            .and_then(|index| inner.sessions.get(index))
            .ok_or_else(|| format!("delegation parent session `{parent_session_id}` is missing"))?;
        if !parent.is_local_session() {
            return Ok(None);
        }
        let Some(project_id) = parent.session.project_id.as_deref() else {
            return Ok(None);
        };
        let project = inner
            .find_project(project_id)
            .ok_or_else(|| format!("delegation project `{project_id}` is missing"))?;
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
            || parent.engram.disabled_reason.is_some()
            || !settings.is_runtime_enabled()
        {
            return Ok(None);
        }
        let binary_path = settings
            .binary_path
            .as_deref()
            .map(PathBuf::from)
            .ok_or_else(|| "enabled Engram project is missing binaryPath".to_owned())?;
        let home = settings
            .home
            .as_deref()
            .map(PathBuf::from)
            .ok_or_else(|| "enabled Engram project is missing home".to_owned())?;
        let root = PathBuf::from(&project.root_path);
        Ok(Some(EngramBindingTarget {
            adapter: inner.engram_host_adapter.clone(),
            connection: EngramConnectionConfig {
                binary_path,
                project_file: root.join(".engram-project"),
                home,
                project_root: root,
                actor_id: engram_actor_id(parent.session.agent),
                session_id: parent_session_id.to_owned(),
            },
            settings: settings.clone(),
            project_id: project.id.clone(),
            project_reset_owner_generation,
            external_ref: format!("termal:session:{parent_session_id}"),
            title: parent.session.name.clone(),
            effects: vec![EngramEffect::Observe, EngramEffect::Communicate],
            routing_token: parent.engram.routing_token.clone(),
            active_grant_id: parent.engram.active_grant_id.clone(),
            rebind_required: parent.engram.rebind_required,
            circuit_open: parent.engram.circuit_open,
            next_bind_retry_at: parent.engram.next_bind_retry_at,
        }))
    }

    fn bind_engram_delegation_best_effort(&self, delegation: &DelegationRecord) {
        let targets = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            [
                Self::engram_binding_target_for_session_shape_locked(
                    &inner,
                    &delegation.parent_session_id,
                    true,
                ),
                Self::engram_binding_target_for_child_locked(
                    &inner,
                    &delegation.child_session_id,
                    true,
                ),
            ]
        };
        for result in targets {
            match result {
                Ok(Some(target)) => {
                    if target.routing_token.is_some()
                        && !target.rebind_required
                        && !target.circuit_open
                    {
                        continue;
                    }
                    let target_session_id = target.connection.session_id.clone();
                    if let Err(error) = self.bind_engram_target_off_lock(target) {
                        self.record_engram_transport_failure(&target_session_id, &error);
                        eprintln!(
                            "engram> project={} session={} bind degraded: {}",
                            delegation.id, delegation.child_session_id, error
                        );
                    }
                }
                Ok(None) => {}
                Err(error) => eprintln!(
                    "engram> delegation={} binding snapshot degraded: {error}",
                    delegation.id
                ),
            }
        }
    }

    fn bind_engram_target_off_lock(
        &self,
        target: EngramBindingTarget,
    ) -> std::result::Result<String, EngramTransportError> {
        let session_id = target.connection.session_id.clone();
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let current_project = engram_project_for_session_locked(&inner, &session_id);
            let target_is_current = current_project.is_some_and(|project| {
                project.id == target.project_id
                    && project.remote_id == LOCAL_REMOTE_ID
                    && project.engram.as_ref() == Some(&target.settings)
                    && match target.project_reset_owner_generation {
                        Some(owner_generation) => inner
                            .engram_project_resets
                            .is_owned_by(&project.id, owner_generation),
                        None => !inner.engram_project_resets.contains(&project.id),
                    }
            });
            if !target_is_current {
                return Err(EngramTransportError::backoff(
                    "Engram binding snapshot was superseded by a project reset",
                ));
            }
            let Some(index) = inner.find_session_index(&session_id) else {
                return Err(EngramTransportError::transport(
                    "session disappeared before Engram binding",
                ));
            };
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            if record.engram.bind_in_progress {
                return Err(EngramTransportError::backoff(
                    "Engram binding is already in progress",
                ));
            }
            record.engram.bind_in_progress = true;
        }

        let result = self.bind_engram_target_uncoordinated_off_lock(target);
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(index) = inner.find_session_index(&session_id) {
            inner
                .session_mut_by_index(index)
                .expect("session index should be valid")
                .engram
                .bind_in_progress = false;
        }
        result
    }

    fn bind_engram_target_uncoordinated_off_lock(
        &self,
        mut target: EngramBindingTarget,
    ) -> std::result::Result<String, EngramTransportError> {
        let runtime_enabled = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .find_project(&target.project_id)
                .filter(|project| {
                    project.remote_id == LOCAL_REMOTE_ID
                        && project.engram.as_ref() == Some(&target.settings)
                        && match target.project_reset_owner_generation {
                            Some(owner_generation) => inner
                                .engram_project_resets
                                .is_owned_by(&project.id, owner_generation),
                            None => !inner.engram_project_resets.contains(&project.id),
                        }
                })
                .and_then(|project| project.engram.as_ref())
                .is_some_and(EngramProjectSettings::is_runtime_enabled)
        };
        if !runtime_enabled {
            return Err(EngramTransportError::backoff(
                "Engram is disabled for this project",
            ));
        }
        if let Some(retry_at) = target.next_bind_retry_at
            && let Some(remaining) = retry_at.checked_duration_since(std::time::Instant::now())
        {
            return Err(EngramTransportError::backoff(format!(
                "Engram bind retry is delayed for {} ms",
                remaining.as_millis()
            )));
        }
        let recovery_started_at = std::time::Instant::now();
        let was_rebind = target.rebind_required || target.circuit_open;
        if target.rebind_required || target.circuit_open {
            if let Some(routing_token) = target.routing_token.clone() {
                let timeout =
                    engram_remaining_dispatch_timeout(
                        recovery_started_at,
                        target.settings.call_timeout(),
                    )
                        .ok_or_else(|| {
                        EngramTransportError::deadline("Engram rebind budget exhausted")
                    })?;
                let status = target.adapter.request(
                    &target.connection,
                    &EngramControlRequest::SessionStatus {
                        routing_token: routing_token.clone(),
                    },
                    timeout,
                );
                let status = match status {
                    Ok(status) => Some(parse_engram_result::<EngramSessionStatusResponse>(status)?),
                    Err(error) if engram_status_error_requires_fresh_bind(&error) => {
                        self.clear_engram_stale_binding_record(
                            &target.connection.session_id,
                            &routing_token,
                            true,
                        )?;
                        target.routing_token = None;
                        target.active_grant_id = None;
                        None
                    }
                    Err(error) => return Err(error),
                };
                if let Some(status) = status {
                    // Engram is authoritative about whether a grant is open.
                    // A stale local grant must not be checkpointed after the
                    // control plane reports a clean session.
                    if status.open_grant_id.is_none() && target.active_grant_id.is_some() {
                        self.clear_engram_stale_binding_record(
                            &target.connection.session_id,
                            &routing_token,
                            false,
                        )?;
                        target.active_grant_id = None;
                    }
                    if let Some(grant_id) = status.open_grant_id.as_deref() {
                        let timeout = engram_remaining_dispatch_timeout(
                            recovery_started_at,
                            target.settings.call_timeout(),
                        )
                        .ok_or_else(|| {
                            EngramTransportError::deadline("Engram rebind budget exhausted")
                        })?;
                        let checkpoint = target.adapter.request(
                            &target.connection,
                            &EngramControlRequest::TurnCheckpoint {
                                routing_token: routing_token.clone(),
                                grant_id: grant_id.to_owned(),
                                next_intent: EngramNextIntent::Wait,
                                observations: Vec::new(),
                                idempotency_key: engram_checkpoint_idempotency_key(
                                    format!(
                                        "termal-restart-checkpoint:{}:{}",
                                        target.connection.session_id, grant_id
                                    ),
                                    &[],
                                ),
                            },
                            timeout,
                        );
                        let checkpoint = match checkpoint {
                            Err(error) if engram_grant_was_issued_but_not_begun(&error) => None,
                            Err(error) => return Err(error),
                            Ok(checkpoint) => {
                                let checkpoint: EngramTurnCheckpointResponse =
                                    parse_engram_result(checkpoint)?;
                                match checkpoint {
                                    EngramTurnCheckpointResponse::Checkpointed { .. } => Some(()),
                                    EngramTurnCheckpointResponse::Refuse { code }
                                        if engram_grant_code_was_issued_but_not_begun(&code) =>
                                    {
                                        None
                                    }
                                    EngramTurnCheckpointResponse::Refuse { .. } => {
                                        return Err(EngramTransportError::remote(
                                            EngramControlErrorBody {
                                                code: "restart_checkpoint_refused".to_owned(),
                                                message: format!(
                                                    "Engram refused restart checkpoint for grant `{grant_id}`"
                                                ),
                                            },
                                        ));
                                    }
                                }
                            }
                        };
                        if checkpoint.is_none() {
                            // Engram reports an issued grant as open, but only a
                            // begun grant can be checkpointed. A fresh bind is
                            // the authoritative operation that expires the
                            // unbegun grant.
                            self.clear_engram_stale_binding_record(
                                &target.connection.session_id,
                                &routing_token,
                                true,
                            )?;
                            target.routing_token = None;
                            target.active_grant_id = None;
                        } else {
                            let mut inner = self.inner.lock().expect("state mutex poisoned");
                            if let Some(index) =
                                inner.find_session_index(&target.connection.session_id)
                            {
                                let record = inner
                                    .session_mut_by_index(index)
                                    .expect("session index should be valid");
                                if record.engram.active_grant_id.as_deref() == Some(grant_id) {
                                    record.engram.active_grant_id = None;
                                    self.commit_persisted_delta_locked(&mut inner).map_err(
                                        |error| {
                                            EngramTransportError::transport(format!(
                                                "failed persisting recovered Engram checkpoint: {error:#}"
                                            ))
                                        },
                                    )?;
                                }
                            }
                        }
                    }
                }
            }
        } else if let Some(routing_token) = target.routing_token.as_deref() {
            return Ok(routing_token.to_owned());
        }

        let mut stale_retry_used = false;
        let binding = loop {
            let work_binding = target
                .adapter
                .read_work_binding(&target.connection, ENGRAM_WORK_BINDING_COMMAND_TIMEOUT)?;
            // Work-focus CLI reads launch separate Engram processes and have
            // no contractual sub-600ms latency bound. Keep that bounded read
            // outside the hot JSON-lines bind budget. A stale-fence retry
            // repeats this shape exactly once.
            // The hot JSON-lines budget deliberately starts fresh after the
            // bounded CLI reader completes. A stale-fence retry performs a
            // fresh reader and receives the same fresh bind budget.
            let timeout = target.settings.call_timeout();
            let result = target.adapter.request(
                &target.connection,
                &EngramControlRequest::SessionBind {
                    external_ref: target.external_ref.clone(),
                    title: target.title.clone(),
                    assurance: "advisory".to_owned(),
                    mediated_effects: target.effects.clone(),
                    capability_map_revision: ENGRAM_CAPABILITY_MAP_REVISION,
                    work_binding,
                    idempotency_key: format!(
                        "termal-bind:{}:{}",
                        target.connection.session_id,
                        Uuid::new_v4()
                    ),
                },
                timeout,
            );
            match result {
                Err(error) if !stale_retry_used && engram_error_is_stale_fence(&error) => {
                    stale_retry_used = true;
                }
                Err(error) => return Err(error),
                Ok(result) => {
                    break parse_engram_result::<EngramSessionBindingResponse>(result)?;
                }
            }
        };
        if was_rebind && binding.status.phase != "sync_required" {
            return Err(EngramTransportError::protocol(format!(
                "Engram rebind returned phase `{}` instead of `sync_required`",
                binding.status.phase
            )));
        }
        let routing_token = binding.routing_token;
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let binding_still_enabled = inner
                .find_project(&target.project_id)
                .filter(|project| project.remote_id == LOCAL_REMOTE_ID)
                .filter(|project| match target.project_reset_owner_generation {
                    Some(owner_generation) => inner
                        .engram_project_resets
                        .is_owned_by(&project.id, owner_generation),
                    None => !inner.engram_project_resets.contains(&project.id),
                })
                .and_then(|project| project.engram.as_ref())
                .is_some_and(EngramProjectSettings::is_runtime_enabled);
            if !binding_still_enabled {
                drop(inner);
                target
                    .adapter
                    .shutdown_session(&target.connection.session_id);
                return Err(EngramTransportError::backoff(
                    "Engram was disabled while the session bind was in flight",
                ));
            }
            let Some(index) = inner.find_session_index(&target.connection.session_id) else {
                drop(inner);
                target
                    .adapter
                    .shutdown_session(&target.connection.session_id);
                return Err(EngramTransportError::transport(
                    "session disappeared while Engram binding was in flight",
                ));
            };
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            record.engram.routing_token = Some(routing_token.clone());
            record.engram.active_grant_id = None;
            record.engram.clear_checkpoint_if_owned_by(None);
            record.engram.consecutive_transport_failures = 0;
            record.engram.circuit_open = false;
            record.engram.next_bind_retry_at = None;
            record.engram.rebind_required = false;
            record.engram.disabled_reason = None;
            if let Err(error) = self.commit_persisted_delta_locked(&mut inner) {
                return Err(EngramTransportError::transport(format!(
                    "failed persisting Engram binding: {error:#}"
                )));
            }
        }
        eprintln!(
            "engram> project={} session={} bound phase={}",
            target.project_id, target.connection.session_id, binding.status.phase
        );
        Ok(routing_token)
    }

    fn clear_engram_stale_binding_record(
        &self,
        session_id: &str,
        expected_routing_token: &str,
        clear_routing_token: bool,
    ) -> std::result::Result<(), EngramTransportError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return Ok(());
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if record.engram.routing_token.as_deref() != Some(expected_routing_token) {
            return Ok(());
        }
        if clear_routing_token {
            record.engram.routing_token = None;
        }
        record.engram.active_grant_id = None;
        self.commit_persisted_delta_locked(&mut inner)
            .map_err(|error| {
                EngramTransportError::transport(format!(
                    "failed persisting stale Engram binding cleanup: {error:#}"
                ))
            })?;
        Ok(())
    }

    fn shutdown_engram_session_process_if_bound(&self, session_id: &str) {
        let adapter = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .find_session_index(session_id)
                .and_then(|index| inner.sessions.get(index))
                .filter(|record| {
                    record.engram.routing_token.is_some()
                        || record.engram.active_grant_id.is_some()
                        || record.engram.bind_in_progress
                        || record.engram.pending_dispatch.is_some()
                })
                .map(|_| inner.engram_host_adapter.clone())
        };
        if let Some(adapter) = adapter {
            adapter.shutdown_session(session_id);
        }
    }

    fn ensure_engram_child_bound_off_lock(
        &self,
        session_id: &str,
    ) -> std::result::Result<Option<EngramBindingTarget>, EngramTransportError> {
        let target = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            Self::engram_binding_target_for_child_locked(&inner, session_id, true)
                .map_err(EngramTransportError::transport)?
        };
        let Some(mut target) = target else {
            return Ok(None);
        };
        if target.routing_token.is_none() || target.rebind_required || target.circuit_open {
            if let Some(retry_at) = target.next_bind_retry_at
                && let Some(remaining) = retry_at.checked_duration_since(std::time::Instant::now())
            {
                return Err(EngramTransportError::backoff(format!(
                    "Engram bind retry is backed off for {} ms",
                    remaining.as_millis()
                )));
            }
            let routing_token = self.bind_engram_target_off_lock(target.clone())?;
            target.routing_token = Some(routing_token);
            target.rebind_required = false;
            target.circuit_open = false;
            target.next_bind_retry_at = None;
        }
        Ok(Some(target))
    }

    fn evaluate_engram_turn_off_lock(
        &self,
        intent: &EngramTurnIntentSnapshot,
    ) -> Option<EngramPendingDispatch> {
        let mut started_at = std::time::Instant::now();
        let disabled_reason = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .find_session_index(&intent.session_id)
                .and_then(|index| inner.sessions.get(index))
                .and_then(|record| record.engram.disabled_reason.clone())
        };
        if let Some(code) = disabled_reason {
            return Some(EngramPendingDispatch {
                dispatch_generation: intent.dispatch_generation,
                intent_fingerprint: intent.intent_fingerprint.clone(),
                evaluated: EngramDispatchEvaluation::Degraded {
                    detail: format!(
                        "Engram control is disabled for this session after fatal error `{code}`"
                    ),
                    code,
                },
                evaluate_latency_ms: duration_millis(started_at.elapsed()),
                started_at,
                awaiting_runtime_stop_resolution: false,
            });
        }
        let mut target = match self.ensure_engram_child_bound_off_lock(&intent.session_id) {
            Ok(Some(target)) => target,
            Ok(None) => return None,
            Err(error) => {
                self.record_engram_transport_failure(&intent.session_id, &error);
                let code = self.engram_failure_card_code(&intent.session_id, &error);
                return Some(EngramPendingDispatch {
                    dispatch_generation: intent.dispatch_generation,
                    intent_fingerprint: intent.intent_fingerprint.clone(),
                    evaluated: EngramDispatchEvaluation::Degraded {
                        code,
                        detail: error.message,
                    },
                    evaluate_latency_ms: duration_millis(started_at.elapsed()),
                    started_at,
                    awaiting_runtime_stop_resolution: false,
                });
            }
        };
        // Initial binding may include a separately bounded work-focus CLI
        // read. Start the hot evaluate/begin budget after the routing token is
        // established instead of charging process startup against 600 ms.
        started_at = std::time::Instant::now();
        let mut stale_retry_used = false;
        let evaluated = loop {
            let routing_token = target
                .routing_token
                .clone()
                .expect("ensured Engram target should carry a routing token");
            let request = EngramControlRequest::TurnEvaluate {
                routing_token,
                idempotency_key: format!(
                    "termal-{}evaluate:{}:{}:{}",
                    if stale_retry_used { "stale-re" } else { "" },
                    intent.session_id,
                    intent.dispatch_generation,
                    intent.intent_fingerprint
                ),
                intent_fingerprint: intent.intent_fingerprint.clone(),
                purpose: "ordinary".to_owned(),
                requested_effects: target.effects.clone(),
                resource_intents: Vec::new(),
            };
            let Some(timeout) =
                engram_remaining_dispatch_timeout(started_at, target.settings.call_timeout())
            else {
                break EngramDispatchEvaluation::Degraded {
                    code: "dispatch_budget_exhausted".to_owned(),
                    detail:
                        "Engram evaluate/begin dispatch budget was exhausted before evaluate"
                            .to_owned(),
                };
            };
            match target
                .adapter
                .request(&target.connection, &request, timeout)
                .and_then(parse_engram_result::<EngramTurnDecisionResponse>)
            {
            Ok(EngramTurnDecisionResponse::Grant { grant }) => {
                let delivered_range =
                    grant
                        .delivery
                        .as_ref()
                        .map(|delivery| EngramDeliveredRange {
                            from: delivery.page.from_cursor,
                            to: delivery.page.to_cursor,
                            head: delivery.page.head_cursor,
                        });
                let delivery_tokens = grant
                    .delivery
                    .iter()
                    .map(|delivery| delivery.page.delivery_token.clone())
                    .collect();
                self.record_engram_transport_success(&intent.session_id);
                break EngramDispatchEvaluation::Grant {
                    grant_id: grant.grant_id,
                    delivery_tokens,
                    delivered_range,
                };
            }
            Ok(EngramTurnDecisionResponse::Refuse { directive })
                if directive.code == "stale_fence" && !stale_retry_used =>
            {
                self.record_engram_transport_success(&intent.session_id);
                self.mark_engram_rebind_required(&intent.session_id, Some(&target));
                match self.ensure_engram_child_bound_off_lock(&intent.session_id) {
                    Ok(Some(refreshed)) => {
                        target = refreshed;
                        // stale-fence recovery performs its bounded work-focus
                        // reread outside the hot evaluate/begin budget.
                        started_at = std::time::Instant::now();
                        stale_retry_used = true;
                    }
                    Ok(None) => {
                        break EngramDispatchEvaluation::Degraded {
                            code: "binding_unavailable".to_owned(),
                            detail: "Engram binding disappeared during stale-fence recovery"
                                .to_owned(),
                        };
                    }
                    Err(error) => {
                        self.record_engram_transport_failure(&intent.session_id, &error);
                        let code = self.engram_failure_card_code(&intent.session_id, &error);
                        break EngramDispatchEvaluation::Degraded {
                            code,
                            detail: error.message,
                        };
                    }
                }
            }
            Ok(EngramTurnDecisionResponse::Refuse { directive }) => {
                self.record_engram_transport_success(&intent.session_id);
                if engram_evaluation_refusal_requires_rebind(&directive.code) {
                    self.mark_engram_rebind_required(&intent.session_id, Some(&target));
                }
                break EngramDispatchEvaluation::Refuse {
                    directive: EngramControlDirectiveCard {
                        directive_id: directive.directive_id,
                        kind: directive.code,
                        audience: directive.target,
                        satisfaction: directive.satisfaction,
                    },
                };
            }
            Ok(EngramTurnDecisionResponse::Defer { deferral }) => {
                self.record_engram_transport_success(&intent.session_id);
                break EngramDispatchEvaluation::Defer {
                    code: deferral.code,
                    retry_after_ms: deferral.retry_after_ms,
                    wake_condition: deferral.wake_condition,
                };
            }
            Err(error) => {
                self.record_engram_transport_failure(&intent.session_id, &error);
                let code = self.engram_failure_card_code(&intent.session_id, &error);
                break EngramDispatchEvaluation::Degraded {
                    code,
                    detail: error.message,
                };
            }
            }
        };
        Some(EngramPendingDispatch {
            dispatch_generation: intent.dispatch_generation,
            intent_fingerprint: intent.intent_fingerprint.clone(),
            evaluated,
            evaluate_latency_ms: duration_millis(started_at.elapsed()),
            started_at,
            awaiting_runtime_stop_resolution: false,
        })
    }

    fn record_engram_transport_success(&self, session_id: &str) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(index) = inner.find_session_index(session_id) {
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            record.engram.consecutive_transport_failures = 0;
            record.engram.next_bind_retry_at = None;
        }
    }

    fn mark_engram_rebind_required(&self, session_id: &str, target: Option<&EngramBindingTarget>) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if target
            .and_then(|target| target.routing_token.as_deref())
            .is_none_or(|routing_token| {
                record.engram.routing_token.as_deref() == Some(routing_token)
            })
        {
            record.engram.rebind_required = true;
        }
    }

    fn record_engram_transport_failure(&self, session_id: &str, error: &EngramTransportError) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(index) = inner.find_session_index(session_id) {
            let runtime_enabled = engram_project_for_session_locked(&inner, session_id)
                .filter(|project| project.remote_id == LOCAL_REMOTE_ID)
                .filter(|project| !inner.engram_project_resets.contains(&project.id))
                .and_then(|project| project.engram.as_ref())
                .is_some_and(EngramProjectSettings::is_runtime_enabled);
            if !runtime_enabled {
                return;
            }
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            if error.disables_session() {
                record.engram.disabled_reason = Some(
                    error
                        .code
                        .clone()
                        .unwrap_or_else(|| "control_disabled".to_owned()),
                );
                record.engram.next_bind_retry_at = None;
                record.engram.rebind_required = true;
                return;
            }
            if error.counts_for_circuit_breaker() {
                record.engram.consecutive_transport_failures = record
                    .engram
                    .consecutive_transport_failures
                    .saturating_add(1);
                record.engram.circuit_open =
                    record.engram.consecutive_transport_failures >= ENGRAM_CIRCUIT_BREAKER_FAILURES;
                record.engram.next_bind_retry_at = Some(
                    std::time::Instant::now()
                        + engram_bind_retry_delay(record.engram.consecutive_transport_failures),
                );
            }
            record.engram.rebind_required = true;
        }
    }

    fn engram_failure_card_code(&self, session_id: &str, error: &EngramTransportError) -> String {
        let circuit_open = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .find_session_index(session_id)
                .and_then(|index| inner.sessions.get(index))
                .is_some_and(|record| record.engram.circuit_open)
        };
        if circuit_open {
            "control_circuit_open".to_owned()
        } else {
            error
                .code
                .clone()
                .unwrap_or_else(|| "control_unavailable".to_owned())
        }
    }
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn engram_checkpoint_idempotency_key(
    base: String,
    observations: &[EngramExecutionObservationInput],
) -> String {
    if observations.is_empty() {
        return base;
    }
    let encoded = serde_json::to_vec(observations)
        .expect("Engram execution observation serialization should be infallible");
    format!("{base}:observations:{}", sha256_hex(&encoded))
}

fn engram_bind_retry_delay(failure_count: u8) -> Duration {
    Duration::from_secs(match failure_count {
        0 | 1 => 1,
        2 => 5,
        3 => 30,
        _ => 60,
    })
}

fn engram_remaining_dispatch_timeout(
    started_at: std::time::Instant,
    call_timeout: Duration,
) -> Option<Duration> {
    let budget = Duration::from_millis(ENGRAM_DISPATCH_BUDGET_MS);
    let remaining = budget.checked_sub(started_at.elapsed())?;
    (!remaining.is_zero()).then(|| call_timeout.min(remaining))
}

fn engram_begin_refusal_allows_reevaluation(code: &str) -> bool {
    matches!(
        code,
        "grant_expired"
            | "policy_epoch_changed"
            | "task_admission_epoch_changed"
            | "delta_required"
            | "stale_fence"
    )
}

fn engram_evaluation_refusal_requires_rebind(code: &str) -> bool {
    code == "turn_already_open"
}

fn engram_error_is_stale_fence(error: &EngramTransportError) -> bool {
    error.kind == EngramTransportErrorKind::Remote && error.code.as_deref() == Some("stale_fence")
}

fn engram_status_error_requires_fresh_bind(error: &EngramTransportError) -> bool {
    error.kind == EngramTransportErrorKind::Remote
        && matches!(
            error.code.as_deref(),
            Some(
                "control_session_token_mismatch"
                    | "control_session_not_bound"
                    | "control_connection_superseded"
                    | "invalid_routing_token"
                    | "unknown_routing_token"
            )
        )
}

fn engram_grant_was_issued_but_not_begun(error: &EngramTransportError) -> bool {
    error.kind == EngramTransportErrorKind::Remote
        && error
            .code
            .as_deref()
            .is_some_and(engram_grant_code_was_issued_but_not_begun)
}

fn engram_grant_code_was_issued_but_not_begun(code: &str) -> bool {
    code == "grant_not_begun"
}

fn engram_turn_intent_fingerprint(
    text: &str,
    expanded_text: Option<&str>,
    attachments: &[PromptImageAttachment],
    source: Option<&MessageSource>,
    queued_source: QueuedPromptSource,
) -> String {
    if let Some(mailbox) = source.and_then(|source| source.mailbox.as_ref()) {
        return sha256_hex(
            format!("mailbox\0{}\0{}", mailbox.mailbox_id, mailbox.sequence).as_bytes(),
        );
    }
    let source_kind = match queued_source {
        QueuedPromptSource::User => "user",
        QueuedPromptSource::Mailbox => "mailbox",
        QueuedPromptSource::Orchestrator => "orchestrator",
    };
    let attachment_digests = attachments
        .iter()
        .map(|attachment| sha256_hex(attachment.data.as_bytes()))
        .collect::<Vec<_>>()
        .join("\0");
    sha256_hex(
        format!(
            "text\0{text}\0expanded\0{}\0attachments\0{attachment_digests}\0source\0{source_kind}",
            expanded_text.unwrap_or_default()
        )
        .as_bytes(),
    )
}
