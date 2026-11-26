// Read-only Git transport coverage moved from the retired schema-1 checker.
use super::*;

// Independent fixture/control Git, not the production command builder.
// Override only in the individual control that deliberately tests a setting.
fn git_command() -> Command {
    let mut command = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            command.env_remove(name);
        }
    }
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    command
        .env("GIT_CONFIG_GLOBAL", null)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .args([
            "-c",
            &format!("core.excludesFile={null}"),
            "-c",
            &format!("core.attributesFile={null}"),
            "-c",
            &format!("core.hooksPath={null}"),
            "-c",
            "core.fsmonitor=false",
            "-c",
            "commit.gpgsign=false",
        ]);
    command
}

fn run_git_test_command_output(root: &FsPath, args: &[&str]) -> String {
    let output = git_command().current_dir(root).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn run_git_test_command(root: &FsPath, args: &[&str]) {
    run_git_test_command_output(root, args);
}

fn init_git_document_test_repo(root: &FsPath) {
    run_git_test_command(root, &["init", "--object-format=sha1"]);
    for (key, value) in [
        ("core.autocrlf", "false"),
        ("user.email", "termal@example.com"),
        ("user.name", "TermAl"),
    ] {
        run_git_test_command(root, &["config", key, value]);
    }
}

fn git_read_fixture() -> TestTempRoot {
    let root = TestTempRoot::create("pinned-git-read");
    init_git_document_test_repo(&root);
    fs::write(root.join("tracked.txt"), b"base\n").unwrap();
    run_git_test_command(&root, &["add", "tracked.txt"]);
    run_git_test_command(&root, &["commit", "-m", "base"]);
    fs::write(root.join("tracked.txt"), b"staged\n").unwrap();
    run_git_test_command(&root, &["add", "tracked.txt"]);
    fs::write(root.join("tracked.txt"), b"working\n").unwrap();
    fs::write(root.join("untracked.txt"), b"extra\0bytes\n").unwrap();
    root
}

#[test]
fn pinned_git_ignores_xdg_files_but_keeps_repository_rules() {
    let root = git_read_fixture();
    let xdg = root.join(".git/host-xdg");
    fs::create_dir_all(xdg.join("git")).unwrap();
    fs::write(xdg.join("git/ignore"), "*.bin\n").unwrap();
    fs::write(xdg.join("git/attributes"), "*.txt -diff\n").unwrap();
    fs::write(root.join("evidence.bin"), b"evidence").unwrap();
    let git = PinnedGitRead::new(&root).unwrap();
    for args in [
        vec!["ls-files", "--others", "--exclude-standard", "-z"],
        vec![
            "diff",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
        ],
    ] {
        let clean = git.run(&args, false).unwrap();
        let protected = git
            .command(&args)
            .env("XDG_CONFIG_HOME", &xdg)
            .output()
            .unwrap();
        assert!(protected.status.success());
        assert_eq!(protected.stdout, clean);
        // Independent negative control: enable only the deliberately hostile
        // files, leaving all unrelated developer Git configuration excluded.
        let control = git_command()
            .current_dir(&root)
            .env("XDG_CONFIG_HOME", &xdg)
            .args([
                "-c",
                &format!("core.excludesFile={}", xdg.join("git/ignore").display()),
                "-c",
                &format!(
                    "core.attributesFile={}",
                    xdg.join("git/attributes").display()
                ),
            ])
            .args(&args)
            .output()
            .unwrap();
        assert!(control.status.success());
        assert_ne!(control.stdout, clean, "control must affect {args:?}");
    }
    fs::write(root.join(".gitignore"), "evidence.bin\n").unwrap();
    fs::write(root.join(".gitattributes"), "*.txt -diff\n").unwrap();
    let paths = git
        .run(&["ls-files", "--others", "--exclude-standard"], false)
        .unwrap();
    assert!(!String::from_utf8(paths).unwrap().contains("evidence.bin"));
    let diff = git
        .run(&["diff", "--no-ext-diff", "--no-textconv"], false)
        .unwrap();
    assert!(String::from_utf8(diff).unwrap().contains("Binary files"));
}

#[test]
fn pinned_git_uses_repository_normalization() {
    let root = git_read_fixture();
    // A process-local config simulates a parent's global autocrlf setting;
    // no real user/system configuration or process-global environment changes.
    run_git_test_command(&root, &["config", "--unset", "core.autocrlf"]);
    fs::write(root.join("tracked.txt"), "working\r\n").unwrap();
    let args = [
        "diff",
        "--binary",
        "--full-index",
        "--no-ext-diff",
        "--no-textconv",
    ];
    let parent = git_command()
        .current_dir(&root)
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.autocrlf")
        .env("GIT_CONFIG_VALUE_0", "true")
        .args(args)
        .output()
        .unwrap();
    assert!(parent.status.success());
    let checker = PinnedGitRead::new(&root).unwrap();
    assert_ne!(
        checker.run(&args, false).unwrap(),
        parent.stdout,
        "a CRLF worktree really differs when the parent's normalization is omitted"
    );
    // Explicit repository normalization is retained by both implementations;
    // supporting it does not re-enable global hooks, filters or transports.
    run_git_test_command(&root, &["config", "core.autocrlf", "true"]);
    assert_eq!(checker.run(&args, false).unwrap(), parent.stdout);
}

#[test]
fn pinned_git_root_and_object_reads_do_not_execute_submodule_filters() {
    let root = git_read_fixture();
    let sub = root.join("sub");
    fs::create_dir(&sub).unwrap();
    init_git_document_test_repo(&sub);
    fs::write(sub.join(".gitattributes"), "tracked.txt filter=review\n").unwrap();
    fs::write(sub.join("tracked.txt"), "base\n").unwrap();
    run_git_test_command(&sub, &["add", "."]);
    run_git_test_command(&sub, &["commit", "-m", "submodule base"]);
    fs::write(
        root.join(".gitmodules"),
        "[submodule \"sub\"]\n\tpath = sub\n\turl = ./sub\n",
    )
    .unwrap();
    run_git_test_command(&root, &["add", "sub", ".gitmodules"]);
    run_git_test_command(&root, &["commit", "-m", "submodule"]);
    run_git_test_command(
        &sub,
        &[
            "config",
            "filter.review.clean",
            "printf invoked > ../.git/submodule-filter-ran; cat",
        ],
    );
    // Same size as the indexed content: status must inspect/convert content,
    // rather than deciding it changed solely from the stat size.
    fs::write(sub.join("tracked.txt"), "work\n").unwrap();
    let marker = root.join(".git/submodule-filter-ran");
    let git = PinnedGitRead::new(&root).unwrap();
    git.run(&["cat-file", "-e", "HEAD:tracked.txt"], false)
        .unwrap();
    assert!(
        !marker.exists(),
        "ordinary reads must not execute submodule filters"
    );

    // Independent control: a worktree diff does inspect the submodule.
    git.run(
        &[
            "diff",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
        ],
        false,
    )
    .unwrap();
    assert!(
        marker.exists(),
        "vulnerable-path control must execute the filter"
    );
}

#[test]
fn pinned_git_missing_promisor_object_never_invokes_transport() {
    let (root, oid, object) = missing_promisor_fixture();
    let git = PinnedGitRead::new(&root).unwrap();
    let error = git.run(&["cat-file", "-p", &oid], false).unwrap_err();
    assert!(error.to_string().contains("Git verification"), "{error:#}");
    assert!(!root.join(".git/transport-ran").exists());
    assert!(!object.exists(), "missing object must not be fetched");
    // Negative control: the same fixture really attempts the configured local
    // transport when both guards are removed. The sentinel exits before SSH.
    let output = git_command()
        .current_dir(&root)
        .env_remove("GIT_NO_LAZY_FETCH")
        .env_remove("GIT_ALLOW_PROTOCOL")
        .args(["cat-file", "-p", &oid])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        root.join(".git/transport-ran").exists(),
        "transport control did not execute"
    );
    assert!(!object.exists());
}

fn missing_promisor_fixture() -> (TestTempRoot, String, PathBuf) {
    let root = git_read_fixture();
    let oid = run_git_test_command_output(&root, &["rev-parse", "HEAD:tracked.txt"]);
    let object = root.join(".git/objects").join(&oid[..2]).join(&oid[2..]);
    assert!(object.is_file());
    fs::remove_file(&object).unwrap();
    for args in [
        [
            "config",
            "remote.origin.url",
            "ssh://invalid.example/review",
        ],
        ["config", "remote.origin.promisor", "true"],
        ["config", "extensions.partialClone", "origin"],
        [
            "config",
            "core.sshCommand",
            "printf invoked > .git/transport-ran; exit 1",
        ],
    ] {
        run_git_test_command(&root, &args);
    }
    (root, oid, object)
}

#[test]
fn read_only_git_permission_and_execution_do_not_fetch_missing_promisor_objects() {
    let (root, oid, object) = missing_promisor_fixture();
    let state = test_app_state();
    let parent = test_session_id(&state, Agent::Codex);
    let (_, child) = super::delegation_support::install_required_review_delegation(&state, &parent);
    let (runtime, _commands) = test_codex_runtime_handle("promisor-config");
    let token = {
        let mut inner = state.inner.lock().unwrap();
        let project = inner.create_project(
            Some("guard overlay".to_owned()),
            root.to_string_lossy().into_owned(),
            LOCAL_REMOTE_ID.to_owned(),
        );
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == project.id)
            .unwrap()
            .environment = ProjectEnvironment(BTreeMap::from([
            ("GIT_NO_LAZY_FETCH".to_owned(), "0".to_owned()),
            ("GIT_ALLOW_PROTOCOL".to_owned(), "ssh".to_owned()),
        ]));
        let index = inner.find_session_index(&child).unwrap();
        inner.sessions[index].session.project_id = Some(project.id);
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
        inner.sessions[index].runtime.runtime_token().unwrap()
    };
    let command_text = format!("git cat-file -p {oid}");
    let decision = classify_claude_control_request(
        &json!({"type":"control_request", "request_id":"promisor",
            "request":{"subtype":"can_use_tool", "tool_name":"Bash",
                "input":{"command":command_text}}}),
        &mut ClaudeTurnState::default(),
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        true,
        root.to_str().unwrap(),
        false,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(
        decision,
        ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow { .. })
    ));

    // A writable parent's command remains unaffected, and proves the fixture
    // actually reaches the local transport sentinel before attempting SSH.
    let mut control = git_command();
    apply_read_only_git_environment(false, &mut control);
    assert!(
        !control
            .current_dir(&root)
            .args(["cat-file", "-p", &oid])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(root.join(".git/transport-ran").exists());
    fs::remove_file(root.join(".git/transport-ran")).unwrap();

    for codex in [false, true] {
        let mut command = git_command();
        command
            .env("GIT_NO_LAZY_FETCH", "0")
            .env("GIT_ALLOW_PROTOCOL", "ssh");
        if codex {
            let (config, _) = state
                .termal_delegation_mcp_codex_config_for_shared_runtime(
                    &child,
                    &token,
                    &root,
                    CodexApprovalPolicy::Never,
                )
                .unwrap();
            assert_eq!(
                config["shell_environment_policy"]["set"]["GIT_NO_LAZY_FETCH"],
                "1"
            );
            assert_eq!(
                config["shell_environment_policy"]["set"]["GIT_ALLOW_PROTOCOL"],
                ""
            );
            for (name, value) in config["shell_environment_policy"]["set"]
                .as_object()
                .unwrap()
            {
                command.env(name, value.as_str().unwrap());
            }
        } else {
            apply_read_only_git_environment(true, &mut command);
        }
        let mut local = git_command();
        for (name, value) in command.get_envs() {
            match value {
                Some(value) => {
                    local.env(name, value);
                }
                None => {
                    local.env_remove(name);
                }
            }
        }
        let local_output = local
            .current_dir(&root)
            .args(["cat-file", "-t", "HEAD"])
            .output()
            .unwrap();
        assert!(
            local_output.status.success(),
            "local object inspection failed"
        );
        assert_eq!(
            String::from_utf8_lossy(&local_output.stdout).trim(),
            "commit"
        );
        assert!(
            !command
                .current_dir(&root)
                .args(["cat-file", "-p", &oid])
                .output()
                .unwrap()
                .status
                .success()
        );
        assert!(
            !root.join(".git/transport-ran").exists(),
            "transport ran: codex={codex}"
        );
        assert!(!object.exists(), "missing object was fetched");
    }
}

#[test]
fn pinned_git_failures_retain_operation_timing_budget_and_cause() {
    for cause in ["bounded read deadline exceeded", "process wait failed"] {
        let error = pinned_git_failure(
            anyhow!(cause),
            &["diff", "--binary", "--no-ext-diff"],
            Duration::from_millis(125),
            Duration::from_secs(7),
        );
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(
                "Git verification operation \"diff\" with arguments \
                 [\"diff\", \"--binary\", \"--no-ext-diff\"] failed after 125ms"
            ),
            "{rendered}"
        );
        assert!(
            rendered
                .contains("remaining shared budget at call start: 7s; total shared budget: 40s"),
            "{rendered}"
        );
        assert_eq!(error.chain().last().unwrap().to_string(), cause);
    }
}

#[test]
fn pinned_git_diagnostics_are_bounded_and_safe_with_explicit_long_path_remedy() {
    let diagnostic = pinned_git_diagnostic(&[b'\n', b'\r', 0, 27, 255]);
    assert!(!diagnostic.chars().any(char::is_control), "{diagnostic:?}");
    let large = pinned_git_diagnostic(&vec![b'x'; 8192]);
    assert!(large.len() < 600);
    for cause in [
        "Filename too long",
        "file name too long",
        "path too long",
        "The filename or extension is too long",
    ] {
        let diagnostic = pinned_git_diagnostic(cause.as_bytes());
        assert!(diagnostic.contains("long-path read failed"), "{diagnostic}");
        assert!(
            diagnostic.contains("shorten the worktree or child path"),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains("no source revision was taken"),
            "{diagnostic}"
        );
    }
    let beyond_display_bound = format!("{}: Filename too long", "x".repeat(8192));
    assert!(
        pinned_git_diagnostic(beyond_display_bound.as_bytes()).contains("long-path read failed")
    );
}

#[test]
fn pinned_git_refuses_configured_filters_before_reading_source() {
    let root = git_read_fixture();
    run_git_test_command(
        &root,
        &["config", "filter.review.process", "touch executed"],
    );
    fs::write(root.join(".gitattributes"), "*.txt filter=review\n").unwrap();
    let error = PinnedGitRead::new(&root)
        .err()
        .expect("configured filters must be refused");
    assert!(matches!(
        error.downcast_ref::<SourceBasisRefusal>(),
        Some(SourceBasisRefusal::ConfiguredGitFilters)
    ));
    assert!(!root.join("executed").exists());
}

#[test]
fn pinned_git_requires_a_worktree_root() {
    let root = git_read_fixture();
    let subdir = root.join("ordinary");
    fs::create_dir(&subdir).unwrap();
    let error = PinnedGitRead::new(&subdir).err().unwrap();
    assert!(error.to_string().contains("worktree root"));
}
