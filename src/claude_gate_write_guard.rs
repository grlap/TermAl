// Owns the host's guard on Claude file writes into a worktree whose credited
// full gate has not settled: which permission requests it covers (Write,
// Edit, MultiEdit, NotebookEdit), the target such a request names, resolved
// against the requesting tool's directory through links and not-yet-existing
// components, the worktree that target lies in, judged as the workspace
// watcher judges a change there, the carried gate that could still be
// credited there, and the denial the runtime receives instead of an Allow.
// Does not own the permission policy that answers every other request
// (src/claude.rs), the carried checks and their fences
// (src/engram_carried_checks.rs), or Bash commands, which it does not parse.
// New module.

/// The input field naming the one file a guarded tool writes, or `None` for
/// a tool the guard does not cover.
fn claude_gate_guarded_target_field(tool: &str) -> Option<&'static str> {
    match tool {
        "Write" | "Edit" | "MultiEdit" => Some("file_path"),
        "NotebookEdit" => Some("notebook_path"),
        _ => None,
    }
}

/// The target of a guarded write, as far as the request names it.
enum ClaudeGuardedTarget {
    /// The target resolved: relative to the requesting tool's directory,
    /// through every existing link, with missing components appended.
    /// `worktree` is the key (`engram_path_key`) of the worktree a change
    /// there is routed to, as the workspace watcher routes it
    /// (`EngramHost::workspace_files_changed`): its directory's nearest
    /// enclosing Git worktree. `None` when the watcher leaves such a change
    /// out (its ignored directories, or the scratch directory at that
    /// worktree's root), so no gate there loses its credit by it.
    Resolved {
        path: PathBuf,
        worktree: Option<String>,
    },
    /// A network path, which is never resolved on the runtime's event reader
    /// (`engram_network_path`): its key as written. It is protected by any
    /// gate whose root it lies under.
    Network(PathBuf),
    /// The request names no usable path: it may lie anywhere, so it may lie
    /// in a protected worktree.
    Unresolved,
}

/// The target a `tool` request with `input` writes, resolved against `cwd`
/// on the file system, so never under the state lock.
fn claude_gate_guarded_target(field: &str, input: &Value, cwd: &str) -> ClaudeGuardedTarget {
    let Some(raw) = input
        .get(field)
        .and_then(Value::as_str)
        .filter(|raw| !raw.trim().is_empty())
    else {
        return ClaudeGuardedTarget::Unresolved;
    };
    let named = FsPath::new(raw);
    let absolute = if named.is_absolute() {
        named.to_path_buf()
    } else if cwd.trim().is_empty() {
        return ClaudeGuardedTarget::Unresolved;
    } else {
        FsPath::new(cwd).join(named)
    };
    if engram_network_path(&absolute.to_string_lossy()) {
        return ClaudeGuardedTarget::Network(absolute);
    }
    let path = engram_canonical_path(&absolute);
    let worktree = claude_gate_target_worktree(&path);
    ClaudeGuardedTarget::Resolved { path, worktree }
}

/// The key of the worktree a change at the resolved `path` is routed to, or
/// `None` when the workspace watcher leaves it out: the same ignored
/// directories (`is_ignored_workspace_file_event_path`) and the same scratch
/// directory at its own worktree's root (`EngramHost::is_worktree_scratch`), so a
/// worktree nested in another is its own.
fn claude_gate_target_worktree(path: &FsPath) -> Option<String> {
    if is_ignored_workspace_file_event_path(path) {
        return None;
    }
    let directory = path.parent().unwrap_or(path);
    let root = engram_worktree_root_path(directory);
    let name = path.file_name().map(|name| name.to_string_lossy());
    if EngramHost::is_worktree_scratch(
        &engram_exact_path_key(&root),
        &engram_exact_path_key(directory),
        name.as_deref(),
    ) {
        return None;
    }
    Some(engram_path_key(&root))
}

/// A carried full gate whose worktree a guarded write would land in.
struct ClaudeProtectedGate {
    root: PathBuf,
    started_at: String,
    run_directory: Option<PathBuf>,
    /// The session that launched it, as the denial names it.
    holder: String,
    /// Its holder's focus is not its launch claim now: the gate settles only at a
    /// checkpoint after the holder makes the launch claim its focus again,
    /// since only that claim's checkpoint settles it, so it may stay
    /// protected until it expires.
    holder_focused_elsewhere: bool,
    /// When it stops being protected by its age, if nothing settles it first.
    expires_at: Option<String>,
}

/// Whether `carried` is a launcher full gate a write in its worktree could
/// still cost a credit: not settled yet, not fenced, and not refused already
/// at its next checkpoint (as the checkpoint settles a carried check: an
/// ambiguous launch, a conflicting terminal record, no run started, a launcher
/// gone without a result) or by its age (`EngramHost::carried_check_expired`). A run
/// read as terminal stays protected until its checkpoint settles it: the
/// watcher reports late, so a change it sees then still refuses the gate.
/// Only a checkpoint on the claim that launched it settles it, so while its
/// holder is focused on another claim it stays protected until it expires;
/// the denial names that release (`claude_gate_write_denial`).
fn claude_gate_creditable(
    carried: &EngramCarriedCheck,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let unended = carried.terminal_digest.is_none();
    !carried.ends_by_report
        && carried.fence.is_none()
        && !carried.ambiguous
        && !carried.terminal_conflict
        && !(unended && carried.run_directory.is_none() && carried.run_missing)
        && !(unended && carried.launcher_gone)
        && !EngramHost::carried_check_expired(carried, now)
}

/// Whether any session carries a gate `claude_gate_creditable` protects:
/// with none, a request is never resolved on the file system.
fn claude_any_gate_creditable_locked(
    inner: &StateInner,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    inner.sessions.iter().any(|record| {
        record
            .engram
            .carried_checks
            .iter()
            .any(|carried| claude_gate_creditable(carried, now))
    })
}

/// The gate `claude_gate_creditable` protects whose worktree holds `target`;
/// for an unresolved target, any such gate. Paths are resolved off the lock;
/// this compares keys only.
fn claude_protected_gate_locked(
    inner: &StateInner,
    target: &ClaudeGuardedTarget,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<ClaudeProtectedGate> {
    inner.sessions.iter().find_map(|record| {
        record
            .engram
            .carried_checks
            .iter()
            .filter(|carried| claude_gate_creditable(carried, now))
            .find(|carried| {
                let root = engram_path_key(&carried.check.target.root);
                match target {
                    ClaudeGuardedTarget::Resolved { worktree, .. } => {
                        worktree.as_deref() == Some(root.as_str())
                    }
                    ClaudeGuardedTarget::Network(path) => {
                        engram_path_within(&engram_path_key(path), &root)
                    }
                    ClaudeGuardedTarget::Unresolved => true,
                }
            })
            .map(|carried| ClaudeProtectedGate {
                root: carried.check.target.root.clone(),
                started_at: carried.check.started_at.clone(),
                run_directory: carried.run_directory.clone(),
                holder: format!("session {} ({})", record.session.name, record.session.id),
                holder_focused_elsewhere: !record.engram.work_binding.as_ref().is_some_and(
                    |binding| {
                        binding.claim_id == carried.claim_id
                            && binding.work_id == carried.work_id
                            && binding.claim_fence == carried.claim_fence
                    },
                ),
                expires_at: EngramHost::carried_check_expires_at(carried)
                    .map(|expires| expires.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            })
    })
}

/// The denial the runtime receives for a guarded write into `gate`'s
/// worktree.
fn claude_gate_write_denial(
    request_id: &str,
    tool: &str,
    target: &ClaudeGuardedTarget,
    gate: &ClaudeProtectedGate,
) -> ClaudePermissionDecision {
    let target = match target {
        ClaudeGuardedTarget::Resolved { path, .. } | ClaudeGuardedTarget::Network(path) => {
            path.display().to_string()
        }
        ClaudeGuardedTarget::Unresolved => "a target TermAl could not resolve".to_owned(),
    };
    let run = gate
        .run_directory
        .as_ref()
        .map_or_else(String::new, |run| format!(", run {}", run.display()));
    let expiry = gate.expires_at.as_deref().map_or_else(
        || "six hours after its launch".to_owned(),
        |at| format!("at {at}"),
    );
    let holder = &gate.holder;
    let release = if gate.holder_focused_elsewhere {
        format!(
            "Its holder, {holder}, is not focused on the claim that launched it now, so the gate \
             settles only at a checkpoint after its run ends and the holder makes that claim its \
             focus again; until then it stays protected until it expires {expiry}."
        )
    } else {
        format!(
            "Write there once the gate settles, at its holder's ({holder}) next checkpoint \
             after its run ends, or once it expires {expiry}."
        )
    };
    ClaudePermissionDecision::Deny {
        request_id: request_id.to_owned(),
        message: format!(
            "TermAl denied this {tool}: {target} lies in {}, where a credited full gate \
             launched at {}{run} has not settled, and a write there now would cost that gate \
             its credit. {release}",
            gate.root.display(),
            gate.started_at,
        ),
    }
}

impl AppState {
    /// The denial for a Claude permission request `message` from
    /// `session_id`, working in `cwd`, when it is a guarded write into the
    /// worktree of a gate that has not settled; `None` lets the ordinary
    /// policy answer.
    fn claude_gate_write_guard(
        &self,
        session_id: &str,
        message: &Value,
        cwd: &str,
    ) -> Option<ClaudePermissionDecision> {
        let request = parse_claude_tool_permission_request(message)?;
        self.claude_gate_write_guard_for(
            session_id,
            &request.request_id,
            &request.tool_name,
            &request.tool_input,
            cwd,
        )
    }

    /// As `claude_gate_write_guard`, for a request already parsed: the
    /// automatic decision and a delayed approval check through the same
    /// rule.
    fn claude_gate_write_guard_for(
        &self,
        session_id: &str,
        request_id: &str,
        tool: &str,
        input: &Value,
        cwd: &str,
    ) -> Option<ClaudePermissionDecision> {
        let field = claude_gate_guarded_target_field(tool)?;
        let any = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            claude_any_gate_creditable_locked(&inner, chrono::Utc::now())
        };
        if !any {
            return None;
        }
        let target = claude_gate_guarded_target(field, input, cwd);
        let gate = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            claude_protected_gate_locked(&inner, &target, chrono::Utc::now())
        }?;
        let denial = claude_gate_write_denial(request_id, tool, &target, &gate);
        if let ClaudePermissionDecision::Deny { message, .. } = &denial {
            eprintln!("engram> session={session_id} request={request_id} {message}");
        }
        Some(denial)
    }
}
