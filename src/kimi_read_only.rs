// Kimi read-only delegation children: the host's permission gate.
//
// Owns: what the ACP reader observes about a Kimi child's tool calls (the
// first title and the streamed argument JSON per `toolCallId`), the decision
// for each `session/request_permission` of a read-only Kimi child, and the
// after-the-fact check of the `rawInput` Kimi reports for an approved call.
//
// Does not own: runtime fencing and sending the answer (acp.rs,
// `handle_acp_message`), the read-only bash grammar (claude.rs,
// `claude_bash_command_is_read_only`, reused unchanged), control-plane
// authority (delegations.rs), or Kimi's Default-mode ACK (kimi.rs).
//
// New module, not split from another file. Contract and evidence:
// docs/features/kimi-cli-integration.md, "Read-only delegation children".
//
// Evidence that shapes it (Kimi Code 2.0.2 ACP captures): a permission request
// carries only the tool's exact title and a summary truncated to about 50
// characters. Kimi reports `rawInput` only after the request is answered, so
// the one pre-decision source for the arguments is the complete argument JSON
// Kimi streams as the call's content text before the request. A subagent's
// request has no stream in the parent session and is rejected.

/// Largest number of tool calls whose observations the reader keeps at once.
/// A call is forgotten when it finishes, so this bounds calls in flight; with
/// the per-call text cap it bounds the memory a streaming model can hold.
const KIMI_READ_ONLY_OBSERVED_CALLS_MAX: usize = 32;

/// Largest streamed argument text kept for one call. A longer stream marks the
/// call oversized, and its request is rejected.
const KIMI_READ_ONLY_STREAMED_ARGS_MAX_BYTES: usize = 256 * 1024;

/// What Kimi's summary text says before a Bash command.
const KIMI_BASH_SUMMARY_PREFIX: &str = "Requesting approval to Running: ";

/// What Kimi's summary text says before an MCP tool name.
const KIMI_MCP_SUMMARY_PREFIX: &str = "Requesting approval to Approve ";

/// The only keys a Bash call may carry for the gate to judge it.
const KIMI_BASH_ALLOWED_KEYS: &[&str] = &[
    "command",
    "description",
    "timeout",
    "cwd",
    "run_in_background",
    "disable_timeout",
];

/// What the reader saw of one tool call before and after its permission
/// request.
#[derive(Clone, Debug, Default)]
struct KimiToolCallObservation {
    /// The title of the call's first `tool_call`: the exact tool name.
    title: Option<String>,
    /// The last content text streamed before `rawInput` was reported: the
    /// cumulative argument JSON.
    streamed_args: Option<String>,
    /// The stream outgrew `KIMI_READ_ONLY_STREAMED_ARGS_MAX_BYTES`.
    oversized: bool,
    /// The arguments the gate approved, checked against `rawInput` later.
    approved_args: Option<Value>,
}

impl KimiToolCallObservation {
    fn stream(&mut self, text: &str) {
        if text.len() > KIMI_READ_ONLY_STREAMED_ARGS_MAX_BYTES {
            self.oversized = true;
            self.streamed_args = None;
        } else if !self.oversized {
            self.streamed_args = Some(text.to_owned());
        }
    }
}

/// The text of an update's content when, and only when, it is exactly the
/// shape Kimi streams arguments in: `[{"type":"content","content":
/// {"type":"text","text":…}}]`. Any other shape counts as no text.
fn kimi_streamed_text(update: &Value) -> Option<&str> {
    let [item] = update.get("content")?.as_array()?.as_slice() else {
        return None;
    };
    if item.get("type").and_then(Value::as_str) != Some("content") {
        return None;
    }
    let content = item.get("content")?;
    if content.get("type").and_then(Value::as_str) != Some("text") {
        return None;
    }
    content.get("text").and_then(Value::as_str)
}

/// Parses `text` as one JSON object, refusing a repeated key at any depth.
/// `serde_json::Value` silently keeps the last of two equal keys, and Kimi's
/// own parser might keep another, so a duplicate is never resolved.
fn kimi_parse_arguments(text: &str) -> Option<Value> {
    struct Strict(Value);
    impl<'de> serde::Deserialize<'de> for Strict {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            deserializer.deserialize_any(StrictVisitor).map(Strict)
        }
    }
    struct StrictVisitor;
    impl<'de> serde::de::Visitor<'de> for StrictVisitor {
        type Value = Value;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("JSON without repeated object keys")
        }
        fn visit_bool<E>(self, value: bool) -> Result<Value, E> {
            Ok(Value::Bool(value))
        }
        fn visit_i64<E>(self, value: i64) -> Result<Value, E> {
            Ok(Value::from(value))
        }
        fn visit_u64<E>(self, value: u64) -> Result<Value, E> {
            Ok(Value::from(value))
        }
        fn visit_f64<E>(self, value: f64) -> Result<Value, E> {
            Ok(Value::from(value))
        }
        fn visit_str<E>(self, value: &str) -> Result<Value, E> {
            Ok(Value::String(value.to_owned()))
        }
        fn visit_string<E>(self, value: String) -> Result<Value, E> {
            Ok(Value::String(value))
        }
        fn visit_unit<E>(self) -> Result<Value, E> {
            Ok(Value::Null)
        }
        fn visit_none<E>(self) -> Result<Value, E> {
            Ok(Value::Null)
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
            let mut items = Vec::new();
            while let Some(Strict(item)) = seq.next_element()? {
                items.push(item);
            }
            Ok(Value::Array(items))
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
            let mut object = serde_json::Map::new();
            while let Some(key) = map.next_key::<String>()? {
                if object.contains_key(&key) {
                    return Err(serde::de::Error::custom(format!("repeated key `{key}`")));
                }
                let Strict(value) = map.next_value()?;
                object.insert(key, value);
            }
            Ok(Value::Object(object))
        }
    }
    serde_json::from_str::<Strict>(text)
        .ok()
        .map(|Strict(value)| value)
        .filter(Value::is_object)
}

/// Per-runtime observations of Kimi tool calls, bounded.
#[derive(Default)]
struct KimiReadOnlyObservations {
    calls: HashMap<String, KimiToolCallObservation>,
    order: VecDeque<String>,
}

/// A `rawInput` that differs from what the gate approved.
#[derive(Debug, PartialEq)]
struct KimiApprovedInputMismatch {
    title: String,
}

impl KimiReadOnlyObservations {
    fn entry(&mut self, tool_call_id: &str) -> &mut KimiToolCallObservation {
        if !self.calls.contains_key(tool_call_id) {
            if self.order.len() >= KIMI_READ_ONLY_OBSERVED_CALLS_MAX {
                // Make room with the oldest call that holds no approval: an
                // approved call still owes its rawInput check. Only when every
                // tracked call is approved does the oldest one go.
                let evict = self
                    .order
                    .iter()
                    .position(|id| {
                        self.calls
                            .get(id)
                            .is_none_or(|call| call.approved_args.is_none())
                    })
                    .unwrap_or(0);
                if let Some(oldest) = self.order.remove(evict) {
                    self.calls.remove(&oldest);
                }
            }
            self.order.push_back(tool_call_id.to_owned());
        }
        self.calls.entry(tool_call_id.to_owned()).or_default()
    }

    fn forget(&mut self, tool_call_id: &str) {
        if self.calls.remove(tool_call_id).is_some() {
            self.order.retain(|id| id != tool_call_id);
        }
    }

    fn get(&self, tool_call_id: &str) -> Option<&KimiToolCallObservation> {
        self.calls.get(tool_call_id)
    }

    /// Records a `tool_call` or `tool_call_update`. Returns a mismatch when
    /// Kimi reports a `rawInput` for an approved call that differs from what
    /// was approved.
    fn observe(&mut self, update: &Value) -> Option<KimiApprovedInputMismatch> {
        let kind = update.get("sessionUpdate").and_then(Value::as_str)?;
        let tool_call_id = update.get("toolCallId").and_then(Value::as_str)?;
        let content_text = kimi_streamed_text(update);
        match kind {
            "tool_call" => {
                let entry = self.entry(tool_call_id);
                if entry.title.is_none() {
                    entry.title = update
                        .get("title")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
                if let Some(text) = content_text.filter(|text| !text.is_empty()) {
                    entry.stream(text);
                }
                None
            }
            "tool_call_update" => {
                let status = update.get("status").and_then(Value::as_str);
                let terminal = matches!(status, Some("completed" | "failed" | "error"));
                let mismatch = match update.get("rawInput") {
                    // The report after the answer: judged against the approval,
                    // never taken as streamed arguments.
                    Some(raw_input) => self.calls.get(tool_call_id).and_then(|entry| {
                        entry.approved_args.as_ref().and_then(|approved| {
                            (approved != raw_input).then(|| KimiApprovedInputMismatch {
                                title: entry.title.clone().unwrap_or_default(),
                            })
                        })
                    }),
                    None => {
                        if !terminal {
                            if let Some(text) = content_text {
                                self.entry(tool_call_id).stream(text);
                            }
                        }
                        None
                    }
                };
                // A finished call is forgotten, whether or not its last update
                // also carried rawInput.
                if terminal {
                    self.forget(tool_call_id);
                }
                mismatch
            }
            _ => None,
        }
    }

    fn record_approval(&mut self, tool_call_id: &str, args: Value) {
        self.entry(tool_call_id).approved_args = Some(args);
    }
}

/// The host's answer to one permission request of a read-only Kimi child.
#[derive(Debug, PartialEq)]
enum KimiReadOnlyDecision {
    /// Approve once; `args` is what was judged and must match `rawInput`.
    Allow { args: Value },
    /// Leave plan mode without accepting the plan.
    PlanRejectAndExit,
    /// Refuse, with a short reason naming the tool only.
    Reject { reason: String },
}

fn kimi_read_only_reject(title: &str, why: &str) -> KimiReadOnlyDecision {
    KimiReadOnlyDecision::Reject {
        reason: format!("TermAl denied `{title}` for this read-only delegation: {why}"),
    }
}

/// Decides one `session/request_permission` of a read-only Kimi child.
/// `control_plane_allowed` answers whether the child currently holds a
/// TermAl control-plane capability (delegation authority, checked under the
/// caller's lock).
fn kimi_read_only_permission_decision(
    params: &Value,
    observation: Option<&KimiToolCallObservation>,
    workdir: &str,
    control_plane_allowed: &dyn Fn(DelegationControlPlaneCapability) -> bool,
) -> KimiReadOnlyDecision {
    let title = params
        .pointer("/toolCall/title")
        .and_then(Value::as_str)
        .unwrap_or("");
    if title.is_empty() {
        return kimi_read_only_reject("tool", "the request names no tool");
    }
    let summary = params
        .pointer("/toolCall/content/0/content/text")
        .and_then(Value::as_str)
        .unwrap_or("");

    // Leaving plan mode is a refusal of the plan, never an acceptance.
    if title == "ExitPlanMode" {
        let offers_exit = params
            .get("options")
            .and_then(Value::as_array)
            .is_some_and(|options| {
                options.iter().any(|option| {
                    option.get("optionId").and_then(Value::as_str) == Some("plan_reject_and_exit")
                })
            });
        return if offers_exit {
            KimiReadOnlyDecision::PlanRejectAndExit
        } else {
            kimi_read_only_reject(title, "plan approval is not a read-only action")
        };
    }

    let capability = match title {
        TERMAL_SUBMIT_REVIEW_RESULT_QUALIFIED_TOOL_NAME => {
            Some(DelegationControlPlaneCapability::SubmitReviewResult)
        }
        TERMAL_REVIEW_FREEZE_QUALIFIED_TOOL_NAME => {
            Some(DelegationControlPlaneCapability::ReviewFreeze)
        }
        _ => None,
    };
    if title != "Bash" && capability.is_none() {
        return kimi_read_only_reject(title, "only read-only commands and the TermAl result tools are allowed");
    }

    // The arguments: the complete JSON object streamed for this very call.
    let Some(observation) = observation else {
        return kimi_read_only_reject(title, "no tool call was observed for this request");
    };
    if observation.title.as_deref() != Some(title) {
        return kimi_read_only_reject(title, "the request does not match the observed tool call");
    }
    if observation.oversized {
        return kimi_read_only_reject(title, "its arguments are larger than the gate reads");
    }
    let Some(args) = observation
        .streamed_args
        .as_deref()
        .and_then(kimi_parse_arguments)
    else {
        return kimi_read_only_reject(
            title,
            "its complete arguments were not observed as one object without repeated keys",
        );
    };

    if let Some(capability) = capability {
        if summary != format!("{KIMI_MCP_SUMMARY_PREFIX}{title}") {
            return kimi_read_only_reject(title, "the request summary does not match the tool");
        }
        let shape_ok = match capability {
            DelegationControlPlaneCapability::SubmitReviewResult => {
                args.get("schemaVersion").and_then(Value::as_u64)
                    == Some(u64::from(DELEGATION_REVIEW_RESULT_SCHEMA_VERSION))
            }
            DelegationControlPlaneCapability::ReviewFreeze => {
                serde_json::from_value::<ReviewFreezeRequest>(args.clone())
                    .is_ok_and(|request| request.validate().is_ok())
            }
            DelegationControlPlaneCapability::SubmitAcceptanceEvaluation => false,
        };
        if !shape_ok {
            return kimi_read_only_reject(title, "its arguments are not a valid request");
        }
        if !control_plane_allowed(capability) {
            return kimi_read_only_reject(title, "this delegation does not hold that capability");
        }
        return KimiReadOnlyDecision::Allow { args };
    }

    // Bash.
    let object = args.as_object().expect("filtered to objects");
    if object
        .keys()
        .any(|key| !KIMI_BASH_ALLOWED_KEYS.contains(&key.as_str()))
    {
        return kimi_read_only_reject(title, "the command carries an option the gate does not judge");
    }
    for flag in ["run_in_background", "disable_timeout"] {
        match object.get(flag) {
            None | Some(Value::Bool(false)) => {}
            Some(_) => {
                return kimi_read_only_reject(title, "background and untimed commands are not allowed");
            }
        }
    }
    if let Some(cwd) = object.get("cwd") {
        let same = cwd
            .as_str()
            .is_some_and(|cwd| kimi_path_key(cwd) == kimi_path_key(workdir));
        if !same {
            return kimi_read_only_reject(title, "commands run only in the delegation's working directory");
        }
    }
    let Some(command) = object.get("command").and_then(Value::as_str) else {
        return kimi_read_only_reject(title, "the command is missing");
    };
    if !kimi_bash_summary_matches(summary, command) {
        return kimi_read_only_reject(title, "the request summary does not match the command");
    }
    if !claude_bash_command_is_read_only(command, workdir) {
        return kimi_read_only_reject(title, "the command is not read-only");
    }
    KimiReadOnlyDecision::Allow { args }
}

/// Whether Kimi's summary ("Requesting approval to Running: <prefix>…") shows
/// the start of `command`. Whitespace runs are compared collapsed, since the
/// summary is display text.
fn kimi_bash_summary_matches(summary: &str, command: &str) -> bool {
    let Some(shown) = summary.strip_prefix(KIMI_BASH_SUMMARY_PREFIX) else {
        return false;
    };
    let collapse = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let command = collapse(command);
    match shown.strip_suffix('\u{2026}') {
        Some(prefix) => {
            let prefix = collapse(prefix);
            !prefix.is_empty() && command.starts_with(&prefix)
        }
        None => collapse(shown) == command,
    }
}

/// A path as a comparison key, with no trailing separator. On Windows the
/// verbatim prefix is dropped, `\` and `/` are the same separator and case is
/// ignored. Elsewhere a backslash is an ordinary file-name character, so
/// `/work\repo` stays a different directory from `/work/repo`.
fn kimi_path_key(path: &str) -> String {
    if cfg!(windows) {
        let path = path.strip_prefix(r"\\?\").unwrap_or(path).replace('\\', "/");
        path.trim_end_matches('/').to_lowercase()
    } else {
        path.trim_end_matches('/').to_owned()
    }
}

/// The option to select for a decision, from the request's offered options.
/// `allow_always` is never chosen.
fn kimi_read_only_option_id(options: &[Value], decision: &KimiReadOnlyDecision) -> Option<String> {
    match decision {
        KimiReadOnlyDecision::Allow { .. } => find_acp_permission_option(options, &["allow_once"]),
        KimiReadOnlyDecision::PlanRejectAndExit => options
            .iter()
            .find(|option| {
                option.get("optionId").and_then(Value::as_str) == Some("plan_reject_and_exit")
            })
            .and_then(|option| option.get("optionId").and_then(Value::as_str))
            .map(str::to_owned),
        KimiReadOnlyDecision::Reject { .. } => {
            find_acp_permission_option(options, &["reject_once", "reject_always"])
        }
    }
}

/// Whether a Kimi mode keeps the host's permission gate in force: Default asks
/// for every write and command, and Plan is stricter. Auto and yolo do not ask.
fn kimi_mode_keeps_permission_gate(mode: &str) -> bool {
    matches!(mode, "default" | "plan")
}

/// Opens or closes the window in which the read-only mode guard acts: from
/// Kimi's Default-mode ACK for a prompt until that prompt settles.
fn set_kimi_mode_gate_armed(runtime_state: &Arc<Mutex<AcpRuntimeState>>, armed: bool) {
    runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned")
        .kimi_mode_gate_armed = armed;
}

fn kimi_mode_gate_armed(runtime_state: &Arc<Mutex<AcpRuntimeState>>) -> bool {
    runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned")
        .kimi_mode_gate_armed
}

/// The mode a Kimi session update reports, if it reports one.
fn kimi_reported_mode(update: &Value) -> Option<String> {
    match update.get("sessionUpdate").and_then(Value::as_str)? {
        "current_mode_update" => update
            .get("currentModeId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        kind if is_acp_config_update_kind(kind, AcpAgent::Kimi) => {
            current_acp_config_option_value(update, "mode")
        }
        _ => None,
    }
}

impl AppState {
    /// Answers a permission request of a read-only Kimi delegation child and
    /// returns `true`. Returns `false`, answering nothing, for any other Kimi
    /// session, whose requests stay manual.
    ///
    /// Fenced like OpenCode's automatic replies: the decision and the send
    /// happen under the state lock that also guards Stop and runtime
    /// replacement, and a stale or stopping runtime gets `cancelled`. Nothing
    /// is held: the answer is sent before this returns.
    fn answer_kimi_read_only_permission(
        &self,
        message: &Value,
        session_id: &str,
        runtime_token: &RuntimeToken,
        input_tx: &Sender<AcpRuntimeCommand>,
        observations: &mut KimiReadOnlyObservations,
    ) -> Result<bool> {
        let params = message.get("params").unwrap_or(&Value::Null);
        let tool_call_id = params
            .pointer("/toolCall/toolCallId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let options = params
            .get("options")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let approved = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if read_only_session_delegation_block_locked(&inner, Some(session_id)).is_none() {
                return Ok(false);
            }
            let record = inner
                .find_session_index(session_id)
                .map(|index| &inner.sessions[index]);
            let current = record.is_some_and(|record| {
                record.runtime.matches_runtime_token(runtime_token)
                    && !record.runtime_stop_in_progress
                    && !matches!(
                        record.session.status,
                        SessionStatus::Stopping | SessionStatus::Idle
                    )
            });
            let (outcome, approved) = if current {
                let workdir = record
                    .map(|record| record.session.workdir.clone())
                    .unwrap_or_default();
                let decision = kimi_read_only_permission_decision(
                    params,
                    tool_call_id.as_deref().and_then(|id| observations.get(id)),
                    &workdir,
                    &|capability| {
                        delegation_control_plane_capability_allowed_locked(
                            &inner, session_id, capability,
                        )
                    },
                );
                if let KimiReadOnlyDecision::Reject { reason } = &decision {
                    eprintln!("[termal] Kimi session {session_id}: {reason}");
                }
                let (selected, approved) =
                    match (kimi_read_only_option_id(&options, &decision), decision) {
                        (Some(option_id), KimiReadOnlyDecision::Allow { args }) => {
                            (Some(option_id), Some(args))
                        }
                        // An approval with no allow-once option is refused.
                        (None, KimiReadOnlyDecision::Allow { .. }) => (
                            find_acp_permission_option(&options, &["reject_once", "reject_always"]),
                            None,
                        ),
                        (selected, _) => (selected, None),
                    };
                let outcome = match selected {
                    Some(option_id) => json!({"outcome": "selected", "optionId": option_id}),
                    None => json!({"outcome": "cancelled"}),
                };
                (outcome, approved)
            } else {
                (json!({"outcome": "cancelled"}), None)
            };
            input_tx
                .send(AcpRuntimeCommand::JsonRpcMessage(
                    json_rpc_result_response_message(
                        message.get("id").cloned().unwrap_or(Value::Null),
                        json!({"outcome": outcome}),
                    ),
                ))
                .map_err(|err| anyhow!("failed delivering Kimi permission response: {err}"))?;
            approved
        };
        if let (Some(args), Some(tool_call_id)) = (approved, tool_call_id) {
            observations.record_approval(&tool_call_id, args);
        }
        Ok(true)
    }

    /// Stops the turn of a read-only Kimi child whose permission gate no
    /// longer holds: Kimi left a mode that asks before acting, or ran an
    /// approved call with other arguments. Any other session is left alone.
    fn stop_kimi_read_only_violation(
        &self,
        session_id: &str,
        runtime_token: &RuntimeToken,
        input_tx: &Sender<AcpRuntimeCommand>,
        detail: &str,
    ) -> Result<()> {
        let read_only = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            read_only_session_delegation_block_locked(&inner, Some(session_id)).is_some()
        };
        if !read_only {
            return Ok(());
        }
        eprintln!("[termal] Kimi session {session_id}: {detail}");
        let _ = input_tx.send(AcpRuntimeCommand::Cancel);
        self.fail_turn_if_runtime_matches(session_id, runtime_token, detail)
    }
}
