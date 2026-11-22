// Owns the native approval-policy selection and the backstop for explicit
// permission requests arriving from an already-running Codex turn. Codex owns
// sandbox execution; this module never parses shell text or expands its cap.
// Permission paths are display data only: proving an additional write grant
// by following them could itself escape the sandbox. Restricted modes refuse
// those grants without filesystem I/O; Codex owns ordinary sandboxed writes.

use super::*;

pub(crate) fn codex_native_approval_policy(
    policy: CodexApprovalPolicy,
    sandbox: CodexSandboxMode,
) -> Value {
    if policy == CodexApprovalPolicy::AutoApprove && sandbox != CodexSandboxMode::DangerFullAccess {
        // Reject native sandbox escalation without suppressing MCP tool asks
        // and genuine elicitations, which keep their existing host owner.
        json!({"granular": {
            "sandbox_approval": false,
            "rules": true,
            "mcp_elicitations": true,
            "request_permissions": true,
            "skill_approval": true
        }})
    } else {
        json!(policy.as_cli_value())
    }
}

#[derive(PartialEq, Eq)]
pub(crate) struct CodexApprovalSandbox {
    pub(crate) mode: Option<CodexSandboxMode>,
    pub(crate) approval_policy: Option<CodexApprovalPolicy>,
    pub(crate) workdir: String,
    pub(crate) generation: u64,
    pub(crate) runtime: Option<RuntimeToken>,
}

impl CodexApprovalSandbox {
    pub(crate) fn capture(record: &SessionRecord) -> Self {
        Self {
            mode: record.active_codex_sandbox_mode,
            approval_policy: record.active_codex_approval_policy,
            workdir: record.session.workdir.clone(),
            generation: record.active_turn_generation,
            runtime: record.runtime.runtime_token(),
        }
    }

    fn native_sandbox_capped(&self) -> bool {
        self.approval_policy == Some(CodexApprovalPolicy::AutoApprove)
            && matches!(
                self.mode,
                Some(CodexSandboxMode::ReadOnly | CodexSandboxMode::WorkspaceWrite)
            )
    }

    pub(crate) fn refusal(&self, method: &str, params: &Value) -> Option<String> {
        if self.mode == Some(CodexSandboxMode::DangerFullAccess) {
            return None;
        }
        if self.mode.is_none() {
            return Some("permissions without a confirmed active sandbox".to_owned());
        }
        if params.get("environmentId").is_some_and(|id| !id.is_null()) {
            return Some("access in an unconfirmed execution environment".to_owned());
        }
        match method {
            "item/permissions/requestApproval" => self.profile_refusal(&params["permissions"]),
            "item/commandExecution/requestApproval" => {
                if params
                    .get("networkApprovalContext")
                    .is_some_and(|v| !v.is_null())
                {
                    return Some("network access".to_owned());
                }
                match params.get("additionalPermissions").filter(|v| !v.is_null()) {
                    Some(profile) => self.profile_refusal(profile),
                    // This turn's AutoApprove policy disabled native sandbox
                    // escalation. A plain rule/skill ask adds no permissions.
                    None if self.native_sandbox_capped() => None,
                    None => Some("command access whose sandbox extent is not specified".to_owned()),
                }
            }
            "item/fileChange/requestApproval" => {
                match params.get("grantRoot").filter(|v| !v.is_null()) {
                    Some(Value::String(path)) => Some(Self::write_refusal(path)),
                    Some(_) => Some("file writes with an invalid grant root".to_owned()),
                    None if self.native_sandbox_capped() => None,
                    None => Some("file writes whose sandbox extent is not specified".to_owned()),
                }
            }
            _ => None,
        }
    }

    fn profile_refusal(&self, profile: &Value) -> Option<String> {
        let Some(fields) = profile.as_object() else {
            return Some("an unspecified permission profile".to_owned());
        };
        if fields
            .keys()
            .any(|key| !matches!(key.as_str(), "network" | "fileSystem"))
        {
            return Some("unsupported additional permissions".to_owned());
        }
        if let Some(network) = fields.get("network").filter(|v| !v.is_null()) {
            let Some(fields) = network.as_object() else {
                return Some("invalid network permissions".to_owned());
            };
            if fields.keys().any(|key| key != "enabled") {
                return Some("unsupported network permissions".to_owned());
            }
            match fields.get("enabled") {
                Some(Value::Bool(true)) => return Some("network access".to_owned()),
                None | Some(Value::Null | Value::Bool(false)) => {}
                _ => return Some("invalid network permissions".to_owned()),
            }
        }
        let Some(files) = fields.get("fileSystem").filter(|v| !v.is_null()) else {
            return None;
        };
        let Some(fields) = files.as_object() else {
            return Some("invalid filesystem permissions".to_owned());
        };
        if fields.keys().any(|key| {
            !matches!(
                key.as_str(),
                "read" | "write" | "entries" | "globScanMaxDepth"
            )
        }) {
            return Some("unsupported filesystem permissions".to_owned());
        }
        for key in ["read", "write"] {
            if let Some(paths) = fields.get(key).filter(|v| !v.is_null()) {
                let Some(paths) = paths.as_array().filter(|paths| paths.len() <= 256) else {
                    return Some("invalid or oversized filesystem permissions".to_owned());
                };
                for path in paths {
                    let Some(path) = path.as_str().filter(|path| FsPath::new(path).is_absolute())
                    else {
                        return Some("filesystem access without an absolute path".to_owned());
                    };
                    if key == "write" {
                        return Some(Self::write_refusal(path));
                    }
                }
            }
        }
        if let Some(entries) = fields.get("entries").filter(|v| !v.is_null()) {
            let Some(entries) = entries.as_array().filter(|entries| entries.len() <= 256) else {
                return Some("invalid or oversized filesystem permissions".to_owned());
            };
            for entry in entries {
                let Some(fields) = entry.as_object() else {
                    return Some("invalid filesystem entry".to_owned());
                };
                if fields
                    .keys()
                    .any(|key| !matches!(key.as_str(), "path" | "access"))
                {
                    return Some("unsupported filesystem entry".to_owned());
                }
                let path = &entry["path"];
                let Some(path_fields) = path.as_object() else {
                    return Some("invalid filesystem path".to_owned());
                };
                if path["type"] != "path"
                    || path_fields
                        .keys()
                        .any(|key| !matches!(key.as_str(), "type" | "path"))
                {
                    return Some(
                        "filesystem patterns or special roots outside a confirmed path".to_owned(),
                    );
                }
                let Some(path) = path["path"]
                    .as_str()
                    .filter(|path| FsPath::new(path).is_absolute())
                else {
                    return Some("filesystem access without an absolute path".to_owned());
                };
                match entry["access"].as_str() {
                    Some("read" | "deny") => {}
                    Some("write") => {
                        return Some(Self::write_refusal(path));
                    }
                    _ => return Some("unsupported filesystem access".to_owned()),
                }
            }
        }
        if let Some(depth) = fields.get("globScanMaxDepth").filter(|v| !v.is_null()) {
            if depth.as_u64().is_none_or(|n| n == 0) {
                return Some("invalid filesystem scan depth".to_owned());
            }
        }
        None
    }

    fn write_refusal(path: &str) -> String {
        // A lexical child may be a link, junction or automount. Even a
        // preliminary no-follow check cannot protect a later pathname lookup
        // from replacement. Do not resolve or probe model-supplied paths.
        let mut chars = path.chars().filter(|ch| !ch.is_control());
        let mut preview: String = chars.by_ref().take(400).collect();
        if chars.next().is_some() {
            preview.push('…');
        }
        format!(
            "additional file-write access to \"{preview}\" whose filesystem extent cannot be established"
        )
    }
}

pub(crate) fn note_codex_sandbox_refusal_locked(
    state: &AppState,
    inner: &mut StateInner,
    index: usize,
    requested: &str,
) -> Result<()> {
    let mode = inner.sessions[index]
        .active_codex_sandbox_mode
        .map(CodexSandboxMode::as_cli_value)
        .unwrap_or("unconfirmed");
    let text = format!(
        "Codex's active {mode} sandbox refused requested {requested}. Change the Sandbox setting for a later turn if that access is needed, or use an operation within the active sandbox."
    );
    let message = Message::Text {
        attachments: Vec::new(),
        id: inner.next_message_id(),
        timestamp: stamp_now(),
        author: Author::System,
        text: text.clone(),
        expanded_text: None,
        source: None,
    };
    let record = inner
        .session_mut_by_index(index)
        .expect("visible Codex session");
    if record.runtime_projection_allowed() {
        record.session.preview = make_preview(&text);
    }
    let message_index = push_message_on_record(record, message);
    let creates = message_created_delta_parts_for_indices(record, vec![message_index]);
    let revision = state.commit_persisted_delta_locked(inner)?;
    state.publish_message_created_delta_parts(inner, revision, creates);
    Ok(())
}
