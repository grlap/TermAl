// Owns worker-only admission, committed-base workspace preparation and startup profiles.
// Does not own result handoff, integration, read-only policy or path containment.
// Split from the delegation creation/settings path in delegations.rs.

fn validate_worker_write_policy(
    mode: DelegationMode,
    policy: &DelegationWritePolicy,
) -> Result<(), ApiError> {
    if mode == DelegationMode::Worker
        && !matches!(policy, DelegationWritePolicy::IsolatedWorktree { .. })
    {
        return Err(ApiError::bad_request(
            "worker mode requires writePolicy `isolatedWorktree`; other writable modes are not supported",
        ));
    }
    if mode != DelegationMode::Worker
        && matches!(
            policy,
            DelegationWritePolicy::IsolatedWorktree { base: Some(_), .. }
        )
    {
        return Err(ApiError::bad_request(
            "writePolicy base is supported only for worker mode with isolatedWorktree",
        ));
    }
    Ok(())
}

fn resolve_worker_base_revision(repo_root: &FsPath, base: &str) -> Result<String, ApiError> {
    let base = base.trim();
    if base.is_empty()
        || base.starts_with('-')
        || base.len() > 1024
        || base.chars().any(char::is_control)
    {
        return Err(ApiError::bad_request(
            "worker base must name one commit or ref, not an option or empty value",
        ));
    }
    // Peel once to a commit, using argv rather than a shell. Force ambiguity
    // warnings even when the user's Git config disables them. A successful
    // rev-parse can otherwise choose one of a branch/tag name collision.
    let output = git_command()
        .args(["-c", "core.warnAmbiguousRefs=true", "-C"])
        .arg(repo_root)
        .args(["rev-parse", "--verify", "--end-of-options"])
        .arg(format!("{base}^{{commit}}"))
        .output()
        .map_err(|err| ApiError::internal(format!("failed to resolve worker base: {err}")))?;
    let resolved = String::from_utf8_lossy(&output.stdout);
    let mut lines = resolved.split_whitespace();
    let oid = lines.next().unwrap_or_default();
    if !output.status.success()
        || !output.stderr.is_empty()
        || lines.next().is_some()
        || !matches!(oid.len(), 40 | 64)
        || !oid.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ApiError::bad_request(
            "worker base must resolve to exactly one unambiguous commit",
        ));
    }
    Ok(oid.to_owned())
}

fn prepare_committed_worker_worktree(
    source_repo_root: &FsPath,
    worktree_root: &FsPath,
    relative_cwd: &FsPath,
    base: &str,
) -> Result<PreparedIsolatedWorktree, ApiError> {
    let base_revision = resolve_worker_base_revision(source_repo_root, base)?;
    // Never collect or copy the parent's index, working tree or untracked files.
    // Both creation and recorded metadata use this exact OID, never HEAD again.
    create_detached_git_worktree(source_repo_root, worktree_root, &base_revision)?;
    // Only this successful create owns cleanup. A failed add may have lost a
    // race to another request using the same explicit target path.
    let mut cleanup = IsolatedWorktreeCleanupGuard::new(source_repo_root, worktree_root);
    let child_cwd = normalize_user_facing_path(&worktree_root.join(relative_cwd));
    if !child_cwd.is_dir() {
        return Err(ApiError::bad_request(
            "delegation cwd is not a directory in the resolved worker base",
        ));
    }
    let prepared = PreparedIsolatedWorktree {
        child_cwd: child_cwd.to_string_lossy().into_owned(),
        source_repo_root: source_repo_root.to_string_lossy().into_owned(),
        worktree_root: worktree_root.to_string_lossy().into_owned(),
        base_revision: Some(base_revision),
    };
    cleanup.disarm();
    Ok(prepared)
}

// Worker mode deliberately overrides app approval defaults for coding.
// Existing lifecycle/read-only responders are not worker operation/path guards;
// worktree separation is not containment. Codex alone has the sandbox below.
fn configure_worker_prompt_settings(child: &mut SessionRecord) {
    match child.session.agent {
        Agent::Codex => {
            // Host AutoApprove keeps native Codex on-request and answers routine
            // approvals; it does not restrict a worker's permitted operations.
            child.codex_approval_policy = CodexApprovalPolicy::AutoApprove;
            child.codex_sandbox_mode = CodexSandboxMode::WorkspaceWrite;
            child.session.approval_policy = Some(CodexApprovalPolicy::AutoApprove);
            child.session.sandbox_mode = Some(CodexSandboxMode::WorkspaceWrite);
        }
        Agent::Claude => {
            child.session.claude_approval_mode = Some(ClaudeApprovalMode::AutoApprove);
        }
        Agent::Cursor => {
            // Agent uses the existing automatic ACP permission responder.
            // It provides no protective worker operation/path guard or containment.
            child.session.cursor_mode = Some(CursorMode::Agent);
        }
        Agent::Gemini => {
            // Native Yolo is worker-only: permission requests bypass the host.
            // No host-side worker permission guard or containment is claimed.
            child.session.gemini_approval_mode = Some(GeminiApprovalMode::Yolo);
        }
        Agent::Kimi => {
            // Default keeps permission requests visible to the host responder.
            child.session.kimi_mode = Some(KimiMode::Default);
            child.session.kimi_approval_mode = Some(KimiApprovalMode::AutoApprove);
        }
        Agent::OpenCode => {
            child.session.opencode_approval_mode = Some(OpenCodeApprovalMode::AutoApprove);
        }
    }
}
