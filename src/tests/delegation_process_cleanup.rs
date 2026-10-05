//! Host-managed Engram launch-directory and immutable-authority witnesses.
use super::delegation_support::*;
use super::*;

#[test]
fn process_cleanup_launch_directory_preserves_immutable_engram_authority() {
    struct AuthorityEcho;
    impl EngramControlTransport for AuthorityEcho {
        fn request(
            &self,
            connection: &EngramConnectionConfig,
            _request: &EngramControlRequest,
            _timeout: Duration,
        ) -> Result<Value, EngramTransportError> {
            Ok(serde_json::to_value(connection).unwrap())
        }
        fn shutdown_session(&self, _session_id: &str) {}
    }
    let temp = TestTempRoot::create("control-authority-launch-policy");
    let connection = EngramConnectionConfig {
        binary_path: temp.path().join("fixture.exe"),
        project_file: temp.path().join(".engram-project"),
        home: temp.path().join("store"),
        project_root: temp.path().join("project"),
        actor_id: "fixture-actor".to_owned(),
        actor_context: Some("fixture-context".to_owned()),
        session_id: "fixture-session".to_owned(),
    };
    let adapter = EngramHostAdapter {
        host_workdir: temp.path().to_path_buf(),
        transport: Arc::new(AuthorityEcho),
    };
    let launch_record = adapter
        .request(
            &connection,
            &EngramControlRequest::SessionStatus {
                routing_token: "fixture-token".to_owned(),
            },
            phase_sync::DEADLOCK_GUARD,
        )
        .unwrap();
    let launched: EngramConnectionConfig = serde_json::from_value(launch_record).unwrap();
    let durable = serde_json::to_value(&connection).unwrap();
    assert_eq!(serde_json::to_value(&launched).unwrap(), durable);
    let replay: EngramConnectionConfig = serde_json::from_value(durable.clone()).unwrap();
    assert_eq!(
        launched, replay,
        "launch policy cannot change captured authority"
    );
    for field in [
        "binary_path",
        "project_file",
        "home",
        "project_root",
        "actor_id",
        "actor_context",
        "session_id",
    ] {
        let mut changed = durable.clone();
        changed[field] = Value::String("different-authority".to_owned());
        let changed: EngramConnectionConfig = serde_json::from_value(changed).unwrap();
        assert_ne!(
            launched, changed,
            "original authority field {field} must remain fenced"
        );
    }
}

fn control_cwd_case(result_status: &str, relative_wrapper: bool) {
    let temp = TestTempRoot::create("delegation-process-control-cwd");
    let worktree = temp.path().join("worktree");
    let home = temp.path().join("control-home");
    fs::create_dir(&worktree).unwrap();
    fs::create_dir(&home).unwrap();
    let project_file = worktree.join(".engram-project");
    fs::write(&project_file, "fixture\n").unwrap();
    #[cfg(windows)]
    let (script, contents) = (
        if relative_wrapper {
            worktree.join("control-cwd.ps1")
        } else {
            temp.path().join("control-cwd.ps1")
        },
        r#"
$ErrorActionPreference = 'Stop'
[Console]::Out.WriteLine('delegation-process-control-ready')
[Console]::Out.Flush()
while ($null -ne ($line = [Console]::In.ReadLine())) {
  [Console]::Out.WriteLine((@{status='ok';result=@{routing_token='fixture-token';fixture_cwd=[Environment]::CurrentDirectory;fixture_project_file=$args[1];fixture_home=$args[3];fixture_actor=$args[6];fixture_session=$args[8];fixture_env_home=$env:ENGRAM_HOME;fixture_env_session=$env:ENGRAM_SESSION_ID}} | ConvertTo-Json -Compress))
  [Console]::Out.Flush()
}
"#,
    );
    #[cfg(not(windows))]
    let (script, contents) = (
        temp.path().join("control-cwd.sh"),
        r#"#!/bin/sh
printf '%s\n' 'delegation-process-control-ready'
while IFS= read -r line; do
  printf '{"status":"ok","result":{"routing_token":"fixture-token","fixture_cwd":"%s"}}\n' "$PWD"
done
"#,
    );
    fs::write(&script, contents).unwrap();
    let state = test_app_state_with_drained_delegation_codex_runtime("process-control-finish");
    let parent = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent,
            CreateDelegationRequest {
                prompt: "Finish fixture".to_owned(),
                title: None,
                cwd: Some(worktree.to_string_lossy().into_owned()),
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Explorer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .unwrap();
    let connection = EngramConnectionConfig {
        binary_path: if relative_wrapper {
            PathBuf::from("./control-cwd.ps1")
        } else {
            script.clone()
        },
        project_file: project_file.clone(),
        home: home.clone(),
        project_root: worktree.clone(),
        actor_id: "fixture-actor".to_owned(),
        actor_context: None,
        session_id: created.delegation.child_session_id.clone(),
    };
    let transport = Arc::new(
        ProcessEngramControlTransport::with_startup_handshake_and_idle_timeout(
            temp.path().to_path_buf(),
            "delegation-process-control-ready",
            phase_sync::DEADLOCK_GUARD,
            Duration::from_secs(5 * 60),
        ),
    );
    let adapter = EngramHostAdapter {
        host_workdir: temp.path().to_path_buf(),
        transport,
    };
    // Replay acquires current launch policy without adding it to durable authority.
    let replay: EngramConnectionConfig =
        serde_json::from_value(serde_json::to_value(&connection).unwrap()).unwrap();
    assert_eq!(replay, connection);
    let reply = adapter
        .request(
            &replay,
            &EngramControlRequest::SessionBind {
                external_ref: "fixture".to_owned(),
                title: "Fixture".to_owned(),
                assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
                mediated_effects: vec![EngramEffect::Observe],
                capability_map_revision: ENGRAM_CAPABILITY_MAP_REVISION,
                work_binding: None,
                idempotency_key: "fixture-bind".to_owned(),
            },
            phase_sync::DEADLOCK_GUARD,
        )
        .unwrap();
    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        &format!("## Result\nStatus: {result_status}\n\nSummary:\nFinished fixture."),
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .unwrap();
    let actual_cwd = fs::canonicalize(reply["fixture_cwd"].as_str().unwrap()).unwrap();
    assert!(
        !actual_cwd.starts_with(fs::canonicalize(&worktree).unwrap()),
        "finished child's control sidecar retains worktree cwd: {}",
        actual_cwd.display()
    );
    assert_eq!(
        actual_cwd,
        fs::canonicalize(temp.path()).unwrap(),
        "control uses the explicit host cwd"
    );
    assert_eq!(
        reply["fixture_project_file"],
        project_file.to_string_lossy().as_ref()
    );
    assert_eq!(reply["fixture_home"], home.to_string_lossy().as_ref());
    assert_eq!(reply["fixture_actor"], connection.actor_id);
    assert_eq!(reply["fixture_session"], connection.session_id);
    assert_eq!(reply["fixture_env_home"], home.to_string_lossy().as_ref());
    assert_eq!(reply["fixture_env_session"], connection.session_id);
    if relative_wrapper {
        fs::remove_file(script).unwrap();
    }
    fs::remove_file(project_file).unwrap();
    fs::remove_dir(&worktree)
        .expect("finished child permits empty worktree removal before idle reap");
    // Transport Drop is failure teardown, not the asserted product cleanup.
}

#[test]
fn delegation_process_cleanup_completed_control_sidecar_cwd() {
    control_cwd_case("completed", false);
}

#[test]
fn delegation_process_cleanup_failed_control_sidecar_cwd() {
    control_cwd_case("failed", false);
}

#[test]
fn delegation_process_cleanup_relative_control_sidecar_wrapper_cwd() {
    control_cwd_case("completed", true);
}

#[test]
fn delegation_process_cleanup_host_one_shot_families_preserve_authority_and_cwd() {
    let temp = TestTempRoot::create("delegation-process-host-families");
    let worktree = temp.path().join("worktree");
    let home = temp.path().join("store");
    let host = temp.path().join("persistence");
    for path in [&worktree, &home, &host] {
        fs::create_dir(path).unwrap();
    }
    let marker = worktree.join(".engram-project");
    fs::write(&marker, "fixture\n").unwrap();
    let script = worktree.join("host-families.ps1");
    let journal = worktree.join("launches.jsonl");
    fs::write(&script, r#"
$ErrorActionPreference = 'Stop'
$report = @{cwd=[Environment]::CurrentDirectory;argv=@($args);home=$env:ENGRAM_HOME;actor=$env:ENGRAM_ACTOR_ID;session=$env:ENGRAM_SESSION_ID} | ConvertTo-Json -Compress
[IO.File]::AppendAllText((Join-Path $PSScriptRoot 'launches.jsonl'), ($report + [Environment]::NewLine))
[Console]::Out.WriteLine($report)
"#).unwrap();
    let binary = PathBuf::from("./host-families.ps1");
    let connection = EngramConnectionConfig {
        binary_path: binary.clone(),
        project_file: marker.clone(),
        home: home.clone(),
        project_root: worktree.clone(),
        actor_id: "fixture-actor".to_owned(),
        actor_context: None,
        session_id: "fixture-session".to_owned(),
    };
    let timeout = phase_sync::DEADLOCK_GUARD;
    assert!(
        run_engram_diagnostic_within(&binary, &marker, &home, &worktree, "doctor", timeout, &host)
            .unwrap()
            .status
            .success()
    );
    run_engram_authority_revoke_command(
        &binary,
        &marker,
        &home,
        &worktree,
        "fixture-grant",
        "fixture-reason",
        timeout,
        &host,
    )
    .unwrap();
    run_engram_json_command(
        &connection,
        &["work", "ls", "--session-id", "fixture-session"],
        timeout,
        "fixture read",
        &host,
    )
    .unwrap();
    run_work_read_command(&connection, &["ls".to_owned()], &host).unwrap();
    run_engram_context_nudge(&EngramContextNudgeTarget {
        host_workdir: host.clone(),
        command: binary.clone(),
        home: home.to_string_lossy().into_owned(),
        project_file: marker.clone(),
        project_root: worktree.clone(),
        actor_id: connection.actor_id.clone(),
        actor_context: None,
        session_id: connection.session_id.clone(),
        host_instance_id: "fixture-host".to_owned(),
        generation: 1,
        advertise_context_generation: true,
        timeout,
    })
    .unwrap();
    assert!(
        run_engram_diagnostic_args_until(
            &binary,
            &marker,
            &home,
            &worktree,
            "control-session-inspect",
            &[
                "--target-session-id=fixture-session",
                "--retained-grant-id=fixture-grant"
            ],
            std::time::Instant::now() + timeout,
            timeout,
            &host
        )
        .unwrap()
        .status
        .success()
    );
    let reports = fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        reports.len(),
        6,
        "every finite host launch family actually ran"
    );
    for report in &reports {
        assert_eq!(
            fs::canonicalize(report["cwd"].as_str().unwrap()).unwrap(),
            fs::canonicalize(&host).unwrap()
        );
        let argv = report["argv"].as_array().unwrap();
        assert!(
            argv.iter()
                .any(|value| value.as_str() == Some(marker.to_string_lossy().as_ref())),
            "explicit project file: {report}"
        );
        assert!(
            argv.iter()
                .any(|value| value.as_str() == Some(home.to_string_lossy().as_ref())),
            "explicit store home: {report}"
        );
    }
    for report in &reports[2..5] {
        assert_eq!(report["home"], home.to_string_lossy().as_ref());
        assert_eq!(report["actor"], "fixture-actor");
        assert_eq!(report["session"], "fixture-session");
    }
    for path in [journal, script, marker] {
        fs::remove_file(path).unwrap();
    }
    fs::remove_dir(&worktree).expect("one-shot helpers leave no worktree cwd owner");
}

#[test]
fn delegation_process_cleanup_lock_retries_keep_the_host_directory() {
    let temp = TestTempRoot::create("delegation-process-lock-retry-cwd");
    let worktree = temp.path().join("worktree");
    let host = temp.path().join("persistence");
    fs::create_dir(&worktree).unwrap();
    fs::create_dir(&host).unwrap();
    let script = worktree.join("lock-retry.ps1");
    let journal = worktree.join("launches.jsonl");
    fs::write(&script, r#"
$ErrorActionPreference = 'Stop'
$kind = if ($args -contains 'json-retry') { 'json' } else { 'cli' }
$report = @{cwd=[Environment]::CurrentDirectory;argv=@($args)} | ConvertTo-Json -Compress
[IO.File]::AppendAllText((Join-Path $PSScriptRoot 'launches.jsonl'), ($report + [Environment]::NewLine))
$marker = Join-Path $PSScriptRoot ($kind + '.attempted')
if (-not [IO.File]::Exists($marker)) {
    [IO.File]::WriteAllText($marker, 'attempted')
    [Console]::Error.WriteLine('database is locked')
    exit 1
}
[Console]::Out.WriteLine($report)
"#).unwrap();
    let connection = EngramConnectionConfig {
        binary_path: PathBuf::from("./lock-retry.ps1"),
        project_file: worktree.join(".engram-project"),
        home: temp.path().to_path_buf(),
        project_root: worktree.clone(),
        actor_id: "fixture-actor".to_owned(),
        actor_context: None,
        session_id: "fixture-session".to_owned(),
    };
    run_engram_json_command_with_lock_retry(
        &connection,
        &["json-retry"],
        phase_sync::DEADLOCK_GUARD,
        "fixture",
        &host,
    )
    .unwrap();
    assert!(
        run_engram_cli_command_with_lock_retry(
            &connection,
            &["cli-retry"],
            phase_sync::DEADLOCK_GUARD,
            "fixture",
            &host,
        )
        .unwrap()
        .success
    );
    let reports = fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(reports.len(), 4, "each wrapper retries exactly once");
    for report in reports {
        assert_eq!(
            fs::canonicalize(report["cwd"].as_str().unwrap()).unwrap(),
            fs::canonicalize(&host).unwrap()
        );
    }
    for path in [
        journal,
        script,
        worktree.join("json.attempted"),
        worktree.join("cli.attempted"),
    ] {
        fs::remove_file(path).unwrap();
    }
    fs::remove_dir(worktree).expect("neither retry retains the worktree directory");
}

#[test]
fn delegation_process_cleanup_missing_host_cwd_fails_before_control_spawn() {
    let temp = TestTempRoot::create("delegation-process-missing-host-cwd");
    let marker = temp.path().join(".engram-project");
    let script = temp.path().join("must-not-start.ps1");
    let witness = temp.path().join("started");
    fs::write(&marker, "fixture\n").unwrap();
    fs::write(
        &script,
        "[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'started'), 'started')",
    )
    .unwrap();
    let connection = EngramConnectionConfig {
        binary_path: script,
        project_file: marker,
        home: temp.path().to_path_buf(),
        project_root: temp.path().to_path_buf(),
        actor_id: "fixture-actor".to_owned(),
        actor_context: None,
        session_id: "fixture-session".to_owned(),
    };
    let error = EngramHostAdapter::default()
        .request(
            &connection,
            &EngramControlRequest::SessionStatus {
                routing_token: "fixture-token".to_owned(),
            },
            phase_sync::DEADLOCK_GUARD,
        )
        .unwrap_err();
    assert_eq!(error.kind, EngramTransportErrorKind::LocalState);
    assert!(error.message.contains("Engram host cwd"));
    assert!(error.process_never_started);
    assert!(
        !witness.exists(),
        "no process or worktree fallback was launched"
    );
}

fn resolution_failure_connection(temp: &TestTempRoot) -> EngramConnectionConfig {
    let project_file = temp.path().join(".engram-project");
    fs::write(&project_file, "fixture\n").unwrap();
    EngramConnectionConfig {
        binary_path: temp.path().join("missing-program.exe"),
        project_file,
        home: temp.path().to_path_buf(),
        project_root: temp.path().to_path_buf(),
        actor_id: "fixture-actor".to_owned(),
        actor_context: None,
        session_id: "fixture-session".to_owned(),
    }
}

#[test]
fn delegation_process_cleanup_resolution_control_is_never_started() {
    let temp = TestTempRoot::create("control-resolution-not-sent");
    let connection = resolution_failure_connection(&temp);
    let error = EngramHostAdapter::new(temp.path().to_path_buf())
        .request(
            &connection,
            &EngramControlRequest::SessionStatus { routing_token: "fixture-token".to_owned() },
            phase_sync::DEADLOCK_GUARD,
        )
        .unwrap_err();
    assert!(error.process_never_started, "missing control executable cannot have sent anything: {error}");
}

#[test]
fn delegation_process_cleanup_resolution_read_is_never_started() {
    let temp = TestTempRoot::create("read-resolution-not-sent");
    let connection = resolution_failure_connection(&temp);
    let error = run_engram_json_command(
        &connection, &["work", "show", "fixture", "--json"],
        phase_sync::DEADLOCK_GUARD, "fixture read", temp.path(),
    ).unwrap_err();
    assert!(error.process_never_started, "missing read executable cannot have sent anything: {error}");
}

#[test]
fn delegation_process_cleanup_resolution_write_is_never_started() {
    let temp = TestTempRoot::create("write-resolution-not-sent");
    let connection = resolution_failure_connection(&temp);
    let result = run_engram_cli_command(
        &connection, &["work", "evaluate", "fixture"],
        phase_sync::DEADLOCK_GUARD, "fixture acceptance write", temp.path(),
    );
    assert!(matches!(classify_acceptance_evaluation_run(result), AcceptanceEvaluationRunOutcome::NeverStarted(_)),
        "a missing evaluator executable must not trigger an identical resend or an unconfirmed write");
}

#[test]
fn delegation_process_cleanup_resolution_post_spawn_transport_stays_unknown() {
    let temp = TestTempRoot::create("control-resolution-started-unknown");
    let mut connection = resolution_failure_connection(&temp);
    connection.binary_path = temp.path().join("close-control.ps1");
    fs::write(&connection.binary_path,
        "[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'started'), 'started'); exit 0").unwrap();
    let error = EngramHostAdapter::new(temp.path().to_path_buf())
        .request(
            &connection,
            &EngramControlRequest::SessionStatus { routing_token: "fixture-token".to_owned() },
            phase_sync::DEADLOCK_GUARD,
        )
        .unwrap_err();
    assert!(temp.path().join("started").is_file(), "real control process reached its script before losing transport");
    assert!(!error.process_never_started, "post-spawn transport loss retains uncertainty: {error}");
    assert!(matches!(classify_acceptance_evaluation_run(Err(error)), AcceptanceEvaluationRunOutcome::Unknown(_)));
}

fn resolver_search_child(case: &str, name: &str) -> bool {
    if std::env::var("TERMAL_RESOLUTION_CASE").as_deref() == Ok(case) {
        return true;
    }
    let temp = TestTempRoot::create("engram-native-search-shadow");
    let witness = temp.path().join("shim-started");
    fs::write(temp.path().join("cmd.cmd"), format!(
        "@echo off\r\n> \"{}\" echo shim\r\nexit /b 0\r\n", witness.display()
    )).unwrap();
    let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
    let child_path = std::env::join_paths([temp.path(), system.as_path()]).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env("PATH", child_path)
        .env("TERMAL_RESOLUTION_CASE", case)
        .env("TERMAL_RESOLUTION_ROOT", temp.path())
        .current_dir(temp.path())
        .output().unwrap();
    assert!(output.status.success(), "isolated search witness failed: {}\n{}\n{}",
        output.status, String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    false
}

#[test]
fn delegation_process_cleanup_native_search_exe_precedes_batch_shadow() {
    if !resolver_search_child("shadow", "tests::delegation_process_cleanup::delegation_process_cleanup_native_search_exe_precedes_batch_shadow") {
        return;
    }
    let root = PathBuf::from(std::env::var_os("TERMAL_RESOLUTION_ROOT").unwrap());
    let resolved = resolve_engram_host_program(FsPath::new("cmd"), &root).unwrap();
    let expected = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/cmd.exe");
    assert_eq!(fs::canonicalize(resolved).unwrap(), fs::canonicalize(expected).unwrap(),
        "a bare command must preserve native Windows resolution despite an earlier PATH batch shim");
}

#[test]
fn delegation_process_cleanup_native_search_preserves_absence_shim_guard() {
    if !resolver_search_child("guard", "tests::delegation_process_cleanup::delegation_process_cleanup_native_search_preserves_absence_shim_guard") {
        return;
    }
    let root = PathBuf::from(std::env::var_os("TERMAL_RESOLUTION_ROOT").unwrap());
    let binary = FsPath::new("cmd");
    validate_engram_absence_executable(binary).unwrap();
    assert!(validate_engram_absence_executable(&root.join("cmd.cmd")).is_err(),
        "an explicit batch shim stays forbidden for absence selectors");
    let project_file = root.join(".engram-project");
    fs::write(&project_file, "fixture\n").unwrap();
    let _ = run_engram_diagnostic_args_until(
        binary, &project_file, &root, &root, "control-session-inspect",
        &["--session-id", "fixture-session"],
        std::time::Instant::now() + phase_sync::DEADLOCK_GUARD,
        phase_sync::DEADLOCK_GUARD, &root,
    );
    assert!(!root.join("shim-started").exists(),
        "a validated bare absence command must never deliver selectors to a PATH batch shim");
}

#[test]
fn delegation_process_cleanup_native_search_keeps_application_directory() {
    if !resolver_search_child("application", "tests::delegation_process_cleanup::delegation_process_cleanup_native_search_keeps_application_directory") {
        return;
    }
    let root = PathBuf::from(std::env::var_os("TERMAL_RESOLUTION_ROOT").unwrap());
    let executable = std::env::current_exe().unwrap();
    let name = FsPath::new(executable.file_name().unwrap());
    assert_eq!(fs::canonicalize(resolve_engram_host_program(name, &root).unwrap()).unwrap(),
        fs::canonicalize(executable).unwrap(), "a native executable beside TermAl is found without PATH");
}

#[test]
fn delegation_process_cleanup_resolution_missing_interpreter_is_never_started() {
    if !resolver_search_child("interpreter", "tests::delegation_process_cleanup::delegation_process_cleanup_resolution_missing_interpreter_is_never_started") {
        return;
    }
    let root = PathBuf::from(std::env::var_os("TERMAL_RESOLUTION_ROOT").unwrap());
    let script = root.join("unstarted.ps1");
    let marker = root.join(".engram-project");
    fs::write(&script, "[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'script-started'), 'started')").unwrap();
    fs::write(&marker, "fixture\n").unwrap();
    let error = engram_host_command(&script, &root, &marker, &root, &root).unwrap_err();
    assert!(error.process_never_started, "missing interpreter is positively not sent: {error}");
    assert!(matches!(classify_acceptance_evaluation_run(Err(error)), AcceptanceEvaluationRunOutcome::NeverStarted(_)));
    assert!(!root.join("script-started").exists());
}

#[test]
fn delegation_process_cleanup_invalid_host_cwd_rejected_before_spawn() {
    let temp = TestTempRoot::create("invalid-host-directory-not-sent");
    let mut connection = resolution_failure_connection(&temp);
    connection.binary_path = temp.path().join("must-not-start.ps1");
    fs::write(&connection.binary_path,
        "[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'started'), 'started')").unwrap();
    for directory in [PathBuf::from("relative-host-directory"), temp.path().join("nonexistent-host-directory")] {
        let error = run_engram_cli_command(&connection, &["work", "evaluate", "fixture"],
            phase_sync::DEADLOCK_GUARD, "invalid cwd fixture", &directory).unwrap_err();
        assert!(error.process_never_started);
        assert_eq!(error.kind, EngramTransportErrorKind::LocalState);
        assert!(!temp.path().join("started").exists(), "invalid cwd cannot fall back to a project worktree");
    }
}

fn resolved_alias_connection(temp: &TestTempRoot, extension: &str) -> EngramConnectionConfig {
    let mut connection = resolution_failure_connection(temp);
    let script = temp.path().join(format!("target.{extension}"));
    let marker = temp.path().join("alias-started");
    let contents = if extension == "cmd" {
        format!("@echo off\r\n> \"{}\" echo started\r\necho {{}}\r\nexit /b 0\r\n", marker.display())
    } else {
        "[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'alias-started'), 'started'); [Console]::Out.WriteLine('{}')".to_owned()
    };
    fs::write(&script, contents).unwrap();
    connection.binary_path = temp.path().join("engram.exe");
    std::os::windows::fs::symlink_file(&script, &connection.binary_path)
        .unwrap_or_else(|error| panic!("alias witness unavailable: cannot create owned executable symlink: {error}; this is not a passing or skipped witness"));
    assert_eq!(fs::canonicalize(&connection.binary_path).unwrap(), fs::canonicalize(script).unwrap());
    connection
}

#[test]
fn delegation_process_cleanup_resolved_alias_batch_absence_is_rejected() {
    let temp = TestTempRoot::create("resolved-alias-batch-absence");
    let connection = resolved_alias_connection(&temp, "cmd");
    validate_engram_absence_executable(&connection.binary_path).unwrap();
    let result = run_engram_diagnostic_args_until(
        &connection.binary_path, &connection.project_file, &connection.home,
        &connection.project_root, "control-session-inspect", &["--target-session-id=fixture"],
        std::time::Instant::now() + phase_sync::DEADLOCK_GUARD,
        phase_sync::DEADLOCK_GUARD, temp.path(),
    );
    assert!(!temp.path().join("alias-started").exists(),
        "raw .exe absence guard passed, but its resolved batch target must never receive selectors");
    assert!(result.is_err(), "resolved batch alias must be refused before launch");
}

#[test]
fn delegation_process_cleanup_resolved_alias_powershell_work_is_rejected() {
    let temp = TestTempRoot::create("resolved-alias-powershell-work");
    let connection = resolved_alias_connection(&temp, "ps1");
    validate_work_read_binary(&connection).unwrap();
    let result = run_work_read_command(&connection, &["ls".to_owned(), "--json".to_owned()], temp.path());
    assert!(!temp.path().join("alias-started").exists(),
        "raw native Work guard passed, but its resolved PowerShell target must never launch");
    assert!(result.is_err(), "native-spelled script alias is not a permitted PowerShell test fixture");
}

#[test]
fn delegation_process_cleanup_resolved_alias_powershell_absence_keeps_exact_selectors() {
    let temp = TestTempRoot::create("resolved-alias-powershell-absence");
    let mut connection = resolved_alias_connection(&temp, "ps1");
    let script = fs::canonicalize(&connection.binary_path).unwrap();
    connection.binary_path = temp.path().join("alias.ps1");
    std::os::windows::fs::symlink_file(&script, &connection.binary_path)
        .expect("same-kind PowerShell alias witness must be available, never skipped");
    fs::write(script, "[Console]::Out.WriteLine((ConvertTo-Json -InputObject @($args) -Compress))").unwrap();
    let selectors = ["--target-session-id=fixture trailing  ", "--retained-grant-id=quote\";& value  "];
    for binary in [&connection.binary_path, &temp.path().join("target.ps1")] {
        validate_engram_absence_executable(binary).unwrap();
        let output = run_engram_diagnostic_args_until(
            binary, &connection.project_file, &connection.home, &connection.project_root,
            "control-session-inspect", &selectors,
            std::time::Instant::now() + phase_sync::DEADLOCK_GUARD,
            phase_sync::DEADLOCK_GUARD, temp.path(),
        ).unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let actual: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
        let expected = vec!["--project-file".to_owned(), connection.project_file.to_string_lossy().into_owned(),
            "--home".to_owned(), connection.home.to_string_lossy().into_owned(),
            "control-session-inspect".to_owned(), selectors[0].to_owned(), selectors[1].to_owned(), "--json".to_owned()];
        assert_eq!(actual, expected, "resolved PS and explicit PS must preserve selectors as data");
    }
}

#[test]
fn delegation_process_cleanup_resolved_alias_native_launch_stays_native() {
    let temp = TestTempRoot::create("resolved-alias-native-control");
    let mut connection = resolution_failure_connection(&temp);
    connection.binary_path = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/cmd.exe");
    let resolved = resolve_engram_host_program(&connection.binary_path, &connection.project_root).unwrap();
    let command = engram_host_command(&connection.binary_path, &connection.project_root,
        &connection.project_file, &connection.home, temp.path()).unwrap();
    assert_eq!(FsPath::new(command.get_program()), resolved);
    assert_eq!(command.get_args().count(), 0, "native launch must not acquire an interpreter prefix");
    let work = work_read_launch_command(&connection, temp.path()).unwrap();
    assert_eq!(FsPath::new(work.get_program()), resolved);
    validate_work_read_program(&resolved, false).unwrap();
}

fn resolved_alias_write_is_never_started(extension: &str) {
    let temp = TestTempRoot::create("resolved-alias-write-not-sent");
    let connection = resolved_alias_connection(&temp, extension);
    let resolved = fs::canonicalize(&connection.binary_path).unwrap();
    let result = run_engram_cli_command(&connection, &["work", "evaluate", "fixture"],
        phase_sync::DEADLOCK_GUARD, "alias acceptance write", temp.path());
    assert!(!temp.path().join("alias-started").exists(),
        "an alias that changes native to {extension} must not execute even on an unrestricted host write");
    let control_error = EngramHostAdapter::new(temp.path().to_path_buf())
        .request(&connection, &EngramControlRequest::SessionStatus { routing_token: "fixture-token".to_owned() },
            phase_sync::DEADLOCK_GUARD).unwrap_err();
    assert!(control_error.process_never_started, "control launch uses the same definite refusal: {control_error}");
    match classify_acceptance_evaluation_run(result) {
        AcceptanceEvaluationRunOutcome::NeverStarted(message) => {
            assert!(message.contains(&connection.binary_path.display().to_string()));
            assert!(message.contains("target."));
            assert!(resolved.extension().unwrap().eq_ignore_ascii_case(extension));
        }
        _ => panic!("launch-kind refusal must be definite NeverStarted, not an uncertain or confirmed write"),
    }
}

#[test]
fn delegation_process_cleanup_resolved_alias_batch_write_is_never_started() {
    resolved_alias_write_is_never_started("cmd");
}

#[test]
fn delegation_process_cleanup_resolved_alias_powershell_write_is_never_started() {
    resolved_alias_write_is_never_started("ps1");
}
