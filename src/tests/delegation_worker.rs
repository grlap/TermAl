//! Worker admission, committed-base workspaces and compatible metadata.
//! Does not own result handoff, integration, or read-only permission internals.
//! New suite alongside `delegations.rs`, reusing `delegation_support` fixtures.

use super::delegation_support::test_app_state_with_delegation_codex_runtime;
use super::*;

pub(super) struct WorkerFixture {
    pub(super) state: AppState,
    pub(super) input_rx: mpsc::Receiver<CodexRuntimeCommand>,
    pub(super) parent: String,
    pub(super) source: PathBuf,
    pub(super) worktree: PathBuf,
    base: String,
    tip: String,
    _root: TestTempRoot,
}

impl WorkerFixture {
    pub(super) fn new() -> Self {
        let _placement = TestTempRootDirectoryScope::repository_local();
        let root = TestTempRoot::create("delegation-worker");
        let source = root.path().join("source");
        let worktree = root.path().join("child");
        fs::create_dir_all(&source).expect("source directory");
        run_git_test_command(&source, &["init"]);
        run_git_test_command(&source, &["config", "user.name", "Worker fixture"]);
        run_git_test_command(&source, &["config", "user.email", "worker@example.invalid"]);
        run_git_test_command(&source, &["config", "core.autocrlf", "false"]);
        run_git_test_command(&source, &["config", "commit.gpgsign", "false"]);
        fs::write(source.join("README.md"), "base\n").expect("base content");
        run_git_test_command(&source, &["add", "README.md"]);
        run_git_test_command(&source, &["commit", "-m", "base"]);
        let base = run_git_test_command_output(&source, &["rev-parse", "HEAD"]);
        run_git_test_command(&source, &["branch", "worker-base"]);
        fs::write(source.join("README.md"), "tip\n").expect("tip content");
        run_git_test_command(&source, &["commit", "-am", "tip"]);
        let tip = run_git_test_command_output(&source, &["rev-parse", "HEAD"]);

        let (mut state, input_rx) =
            test_app_state_with_delegation_codex_runtime("worker-fixture-runtime");
        state.default_workdir = root.path().to_string_lossy().into_owned();
        let (parent, saved) = {
            let mut inner = state.inner.lock().expect("state lock");
            let record = inner.create_session(
                Agent::Codex,
                Some("Worker parent".to_owned()),
                source.to_string_lossy().into_owned(),
                None,
                None,
            );
            let parent = record.session.id.clone();
            (parent, state.commit_locked(&mut inner))
        };
        saved.expect("parent persisted");
        Self {
            state,
            input_rx,
            parent,
            source,
            worktree,
            base,
            tip,
            _root: root,
        }
    }

    pub(super) fn isolated_policy(&self, base: Option<&str>) -> Value {
        let mut policy = json!({
            "kind": "isolatedWorktree",
            "ownedPaths": [],
            "worktreePath": self.worktree.to_string_lossy(),
        });
        if let Some(base) = base {
            policy["base"] = json!(base);
        }
        policy
    }

    pub(super) async fn spawn(&self, mode: &str, policy: Value) -> (StatusCode, Value) {
        let response = app_router(self.state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{}/delegations", self.parent))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "prompt": "Implement the requested change in this workspace.",
                            "title": "Worker route fixture",
                            "mode": mode,
                            "writePolicy": policy,
                        }))
                        .expect("request JSON"),
                    ))
                    .expect("request"),
            )
            .await
            .expect("spawn route response");
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        let value = serde_json::from_slice(&body)
            .unwrap_or_else(|_| json!({"error": String::from_utf8_lossy(&body)}));
        (status, value)
    }

    fn assert_worker_prompt(&self, child: &str) {
        let command = recv_within_guard(&self.input_rx, "worker prompt delivery")
            .expect("worker prompt delivered");
        match command {
            CodexRuntimeCommand::Prompt {
                session_id,
                command,
            } => {
                assert_eq!(session_id, child);
                assert_eq!(command.approval_policy, CodexApprovalPolicy::AutoApprove);
                assert_eq!(command.sandbox_mode, CodexSandboxMode::WorkspaceWrite);
                assert_eq!(
                    fs::canonicalize(&command.cwd).expect("child cwd"),
                    fs::canonicalize(&self.worktree).expect("worktree"),
                );
                assert!(command.prompt.contains("Mode: Worker"));
                assert!(
                    !command
                        .prompt
                        .contains("TERMAL_STRUCTURED_REVIEW_RESULT_V1")
                );
            }
            _ => panic!("expected a worker prompt"),
        }
        assert!(matches!(
            self.input_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    fn counts(&self) -> (usize, usize) {
        let inner = self.state.inner.lock().expect("state lock");
        (inner.sessions.len(), inner.delegations.len())
    }
}

impl Drop for WorkerFixture {
    fn drop(&mut self) {
        // The source repository and every linked worktree are fixture-owned.
        // Cleanup must not panic while a RED assertion is already unwinding.
        if self.worktree.exists() {
            let _ = git_command()
                .arg("-C")
                .arg(&self.source)
                .args(["worktree", "remove", "--force"])
                .arg(&self.worktree)
                .output();
        }
    }
}

#[tokio::test]
async fn delegation_worker_route_starts_once_at_default_ref_and_commit_bases() {
    for case in ["default", "branch", "commit"] {
        let fixture = WorkerFixture::new();
        let before = fixture.counts();
        let (base, expected) = match case {
            "default" => (None, fixture.tip.as_str()),
            "branch" => (Some("worker-base"), fixture.base.as_str()),
            _ => (Some(fixture.base.as_str()), fixture.base.as_str()),
        };
        let (status, body) = fixture.spawn("worker", fixture.isolated_policy(base)).await;
        assert_eq!(status, StatusCode::CREATED, "{case}: {body}");
        assert_eq!(body["delegation"]["mode"], "worker");
        assert_eq!(body["delegation"]["baseRevision"], expected);
        assert_eq!(fixture.counts(), (before.0 + 1, before.1 + 1));
        assert_eq!(
            run_git_test_command_output(&fixture.worktree, &["rev-parse", "HEAD"]),
            expected
        );
        assert!(
            run_git_test_command_output(&fixture.worktree, &["status", "--porcelain"]).is_empty()
        );
        fixture.assert_worker_prompt(body["childSession"]["id"].as_str().expect("child id"));

        // This is a filesystem/profile witness, not a live provider tool call.
        let writable = fixture
            .state
            .ensure_read_only_delegation_allows_session_write_action(
                body["childSession"]["id"].as_str(),
                "worker workspace write",
            );
        assert!(writable.is_ok(), "a worker must not inherit a read-only guard");
        fs::write(fixture.worktree.join("worker-output.txt"), "worker edit\n")
            .expect("dedicated workspace is writable");
        assert!(!fixture.source.join("worker-output.txt").exists());
        let compact = compact_mcp_spawn_result(body);
        assert_eq!(compact["delegation"]["baseRevision"], expected);
    }
}

#[tokio::test]
async fn delegation_worker_route_does_not_copy_or_require_parent_dirty_state() {
    let fixture = WorkerFixture::new();
    fs::write(fixture.source.join("README.md"), "staged\n").expect("staged edit");
    run_git_test_command(&fixture.source, &["add", "README.md"]);
    fs::write(fixture.source.join("README.md"), "unstaged\n").expect("unstaged edit");
    fs::write(fixture.source.join("parent-only.txt"), "untracked\n").expect("untracked edit");
    let status_before = run_git_test_command_output(&fixture.source, &["status", "--porcelain"]);
    let index_before = run_git_test_command_output(&fixture.source, &["show", ":README.md"]);
    let file_before = fs::read(fixture.source.join("README.md")).expect("parent content");
    let untracked_before =
        fs::read(fixture.source.join("parent-only.txt")).expect("untracked content");
    let (status, body) = fixture.spawn("worker", fixture.isolated_policy(None)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["delegation"]["baseRevision"], fixture.tip);
    assert_eq!(
        fs::read(fixture.worktree.join("README.md")).expect("child content"),
        b"tip\n".to_vec()
    );
    assert!(!fixture.worktree.join("parent-only.txt").exists());
    assert!(run_git_test_command_output(&fixture.worktree, &["status", "--porcelain"]).is_empty());
    assert_eq!(
        run_git_test_command_output(&fixture.source, &["status", "--porcelain"]),
        status_before
    );
    assert_eq!(
        run_git_test_command_output(&fixture.source, &["show", ":README.md"]),
        index_before
    );
    assert_eq!(
        fs::read(fixture.source.join("README.md")).expect("parent content"),
        file_before
    );
    assert_eq!(
        fs::read(fixture.source.join("parent-only.txt")).expect("untracked content"),
        untracked_before
    );
    fixture.assert_worker_prompt(body["childSession"]["id"].as_str().expect("child id"));
}

#[tokio::test]
async fn delegation_worker_route_refuses_invalid_option_like_and_ambiguous_bases() {
    let fixture = WorkerFixture::new();
    run_git_test_command(
        &fixture.source,
        &["branch", "ambiguous-base", &fixture.base],
    );
    run_git_test_command(&fixture.source, &["tag", "ambiguous-base", &fixture.tip]);
    run_git_test_command(
        &fixture.source,
        &["config", "core.warnAmbiguousRefs", "false"],
    );
    let before = fixture.counts();
    for base in [
        "missing-worker-base",
        "--help",
        "--all",
        "ambiguous-base",
        "HEAD:README.md",
        "HEAD^{tree}",
        "",
    ] {
        let (status, body) = fixture
            .spawn("worker", fixture.isolated_policy(Some(base)))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "base {base}: {body}");
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|error| error.contains("base")),
            "{body}"
        );
        assert_eq!(fixture.counts(), before);
        assert!(!fixture.worktree.exists());
        assert!(matches!(
            fixture.input_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }
}

#[test]
fn delegation_worker_creation_keeps_the_commit_captured_before_parent_head_moves() {
    let fixture = WorkerFixture::new();
    // Fixture construction captured `base`, then moved the parent's HEAD to `tip`.
    assert_ne!(fixture.base, fixture.tip);
    let captured = resolve_worker_base_revision(&fixture.source, "worker-base")
        .expect("resolve the immutable basis once");
    assert_eq!(captured, fixture.base);
    run_git_test_command(
        &fixture.source,
        &["branch", "--force", "worker-base", &fixture.tip],
    );
    create_detached_git_worktree(&fixture.source, &fixture.worktree, &captured)
        .expect("create from the captured worker basis");
    assert_eq!(
        run_git_test_command_output(&fixture.worktree, &["rev-parse", "HEAD"]),
        fixture.base,
        "creation must not resolve the parent's moving HEAD again",
    );
}

#[tokio::test]
async fn delegation_worker_route_requires_isolated_policy_and_keeps_shared_deferred() {
    let fixture = WorkerFixture::new();
    let before = fixture.counts();
    let (status, body) = fixture.spawn("worker", json!({"kind": "readOnly"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let error = body["error"].as_str().expect("combination error");
    assert!(
        error.contains("worker") && error.contains("isolatedWorktree"),
        "{error}"
    );
    let (status, body) = fixture
        .spawn(
            "worker",
            json!({
                "kind": "sharedWorktree", "ownedPaths": ["README.md"],
            }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| error.contains("sharedWorktree")),
        "{body}"
    );
    assert_eq!(fixture.counts(), before);
    assert!(!fixture.worktree.exists());
}

#[test]
fn delegation_worker_failed_create_preserves_existing_worktree_owner() {
    let fixture = WorkerFixture::new();
    create_detached_git_worktree(&fixture.source, &fixture.worktree, &fixture.tip)
        .expect("another request already owns this worktree");
    fs::write(fixture.worktree.join("owner-output.txt"), "keep owner output\n")
        .expect("existing owner's output");
    // Simulate losing the race after the earlier target-availability check.
    let result = prepare_committed_worker_worktree(
        &fixture.source,
        &fixture.worktree,
        FsPath::new(""),
        "worker-base",
    );
    assert!(result.is_err(), "a second create must refuse the owned target");
    assert_eq!(
        fs::read_to_string(fixture.worktree.join("owner-output.txt")).expect("owner retained"),
        "keep owner output\n",
    );
    assert!(fixture.worktree.join(".git").is_file());
    assert_eq!(
        run_git_test_command_output(&fixture.worktree, &["rev-parse", "HEAD"]),
        fixture.tip,
    );
}

#[test]
fn delegation_worker_missing_cwd_in_base_cleans_up_its_created_worktree() {
    let fixture = WorkerFixture::new();
    let result = prepare_committed_worker_worktree(
        &fixture.source,
        &fixture.worktree,
        FsPath::new("missing-directory"),
        "worker-base",
    );
    let error = result.err().expect("cwd is absent from the selected commit");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("cwd"));
    assert!(!fixture.worktree.exists());
    assert_eq!(
        run_git_test_command_output(&fixture.source, &["rev-parse", "HEAD"]),
        fixture.tip,
    );
    assert_eq!(
        fs::read_to_string(fixture.source.join("README.md")).expect("source retained"),
        "tip\n",
    );
}

#[tokio::test]
async fn delegation_worker_addition_does_not_change_default_reviewer_read_only_guards() {
    let fixture = WorkerFixture::new();
    let (status, body) = fixture.spawn("reviewer", json!({"kind": "readOnly"})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["delegation"]["writePolicy"]["kind"], "readOnly");
    let child = body["childSession"]["id"].as_str().expect("reviewer child");
    let denied = fixture
        .state
        .ensure_read_only_delegation_allows_session_write_action(
            Some(child),
            "worker regression write",
        );
    assert!(
        denied.is_err(),
        "a reviewer must not acquire worker write authority"
    );
    match recv_within_guard(&fixture.input_rx, "reviewer prompt").expect("reviewer prompt") {
        CodexRuntimeCommand::Prompt { command, .. } => {
            assert_eq!(command.approval_policy, CodexApprovalPolicy::Never);
            assert_eq!(command.sandbox_mode, CodexSandboxMode::ReadOnly);
        }
        _ => panic!("expected reviewer prompt"),
    }
}

#[test]
fn delegation_worker_profiles_are_edit_capable() {
    let fixture = WorkerFixture::new();
    let isolated: DelegationWritePolicy =
        serde_json::from_value(fixture.isolated_policy(None)).expect("existing isolated policy");
    for agent in [
        Agent::Codex,
        Agent::Claude,
        Agent::Cursor,
        Agent::Gemini,
        Agent::Kimi,
        Agent::OpenCode,
    ] {
        validate_delegation_agent_admission(agent, DelegationMode::Worker, &isolated)
            .expect("worker admission is harness-neutral");
        let session = {
            let mut inner = fixture.state.inner.lock().expect("state lock");
            let mut record = inner.create_session(
                agent,
                None,
                fixture.source.to_string_lossy().into_owned(),
                None,
                None,
            );
            configure_delegation_child_prompt_settings(
                &mut record,
                DelegationMode::Worker,
                &isolated,
            );
            record.session.clone()
        };
        match agent {
            Agent::Codex => {
                assert_eq!(
                    session.approval_policy,
                    Some(CodexApprovalPolicy::AutoApprove)
                );
                assert_eq!(session.sandbox_mode, Some(CodexSandboxMode::WorkspaceWrite));
            }
            Agent::Claude => assert_eq!(
                session.claude_approval_mode,
                Some(ClaudeApprovalMode::AutoApprove)
            ),
            Agent::Cursor => assert_eq!(session.cursor_mode, Some(CursorMode::Agent)),
            Agent::Gemini => {
                assert_eq!(session.gemini_approval_mode, Some(GeminiApprovalMode::Yolo))
            }
            Agent::Kimi => {
                assert_eq!(session.kimi_mode, Some(KimiMode::Default));
                assert_eq!(
                    session.kimi_approval_mode,
                    Some(KimiApprovalMode::AutoApprove)
                );
            }
            Agent::OpenCode => assert_eq!(
                session.opencode_approval_mode,
                Some(OpenCodeApprovalMode::AutoApprove)
            ),
        }
    }
}

#[tokio::test]
async fn delegation_worker_base_metadata_persists_and_legacy_records_still_load() {
    let fixture = WorkerFixture::new();
    let (status, body) = fixture.spawn("reviewer", json!({"kind": "readOnly"})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let record = {
        let inner = fixture.state.inner.lock().expect("state lock");
        inner.delegations[0].clone()
    };
    let mut legacy = serde_json::to_value(record).expect("record JSON");
    legacy
        .as_object_mut()
        .expect("record object")
        .remove("baseRevision");
    legacy["status"] = json!("completed");
    for (value, expected) in [
        (legacy.clone(), None),
        (
            {
                legacy["baseRevision"] = json!(fixture.base);
                legacy
            },
            Some(fixture.base.as_str()),
        ),
    ] {
        let decoded: DelegationRecord = serde_json::from_value(value).expect("compatible record");
        let saved = {
            let mut inner = fixture.state.inner.lock().expect("state lock");
            inner.delegations[0] = decoded;
            persist_state(&fixture.state.persistence_path, &inner)
        };
        saved.expect("record saved to SQLite");
        let loaded = load_state(&fixture.state.persistence_path)
            .expect("load store")
            .expect("saved store");
        let summary = serde_json::to_value(delegation_summary_from_record(&loaded.delegations[0]))
            .expect("loaded summary");
        match expected {
            None => assert!(
                summary.get("baseRevision").is_none(),
                "legacy display shape: {summary}"
            ),
            Some(base) => assert_eq!(summary["baseRevision"], base),
        }
    }
}
