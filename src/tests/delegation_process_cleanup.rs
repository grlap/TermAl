//! Owned runtime cleanup and host-managed Engram launch witnesses.
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

// Disposable provider hierarchy launched through the dedicated runtime job owner.
use std::io::{BufRead, Write};
use std::net::{TcpListener, TcpStream};
const FIXTURE_ROLE: &str = "TERMAL_TEST_DELEGATION_PROCESS_ROLE";
const FIXTURE_ENDPOINT: &str = "TERMAL_TEST_DELEGATION_PROCESS_ENDPOINT";

fn runtime_fixture_identity(role: &str) -> Value {
    use windows_sys::Win32::{Foundation::FILETIME, System::Threading::{GetCurrentProcess, GetProcessTimes}};
    #[link(name = "kernel32")]
    unsafe extern "system" { fn GetCommandLineW() -> *const u16; }
    let mut created: FILETIME = unsafe { std::mem::zeroed() };
    let mut exited: FILETIME = unsafe { std::mem::zeroed() };
    let mut kernel: FILETIME = unsafe { std::mem::zeroed() };
    let mut user: FILETIME = unsafe { std::mem::zeroed() };
    assert_ne!(unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) }, 0);
    let ticks = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
    let millis = ((ticks - 116_444_736_000_000_000) / 10_000) as i64;
    let command_line = unsafe {
        let pointer = GetCommandLineW();
        assert!(!pointer.is_null());
        let mut length = 0;
        while *pointer.add(length) != 0 { length += 1; }
        String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length))
    };
    json!({"role":role, "pid":std::process::id(),
        "path":std::env::current_exe().unwrap(), "command_line":command_line,
        "started_at":chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis).unwrap().to_rfc3339(),
        "creation_filetime_100ns":ticks,
        "cwd":fs::canonicalize(std::env::current_dir().unwrap()).unwrap()})
}

struct ProcessPeer {
    socket: TcpStream,
    description: Value,
    #[cfg(windows)]
    process: std::os::windows::io::OwnedHandle,
}

impl ProcessPeer {
    fn accept(listener: &TcpListener) -> Self {
        let guard = phase_sync::PollGuard::new();
        let socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    guard.wait("runtime fixture readiness")
                }
                Err(error) => panic!("fixture accept: {error}"),
            }
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(phase_sync::DEADLOCK_GUARD))
            .unwrap();
        let mut description = String::new();
        std::io::BufReader::new(socket.try_clone().unwrap())
            .read_line(&mut description)
            .unwrap();
        let description: Value = serde_json::from_str(&description).unwrap();
        #[cfg(windows)]
        let process = {
            use std::os::windows::io::FromRawHandle;
            use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
            // The ready fixture is parked on our still-open socket. Capture its
            // process identity before cancellation, so PID reuse cannot pass.
            let handle = unsafe {
                OpenProcess(
                    PROCESS_SYNCHRONIZE,
                    0,
                    description["pid"].as_u64().unwrap() as u32,
                )
            };
            assert!(
                !handle.is_null(),
                "open fixture process: {}",
                std::io::Error::last_os_error()
            );
            unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle) }
        };
        Self {
            socket,
            description,
            #[cfg(windows)]
            process,
        }
    }

    fn assert_exited(&self) {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::{
                Foundation::WAIT_OBJECT_0, System::Threading::WaitForSingleObject,
            };
            let result = unsafe {
                WaitForSingleObject(
                    self.process.as_raw_handle(),
                    phase_sync::DEADLOCK_GUARD.as_millis() as u32,
                )
            };
            assert_eq!(
                result, WAIT_OBJECT_0,
                "owned fixture survived cleanup: {}", self.description
            );
        }
        #[cfg(not(windows))]
        {
            // Only this fixture process owns the peer. A closed connection is the
            // portable fallback; Windows uses the captured process handle above.
            let result = (&self.socket).read(&mut [0]);
            assert!(
                matches!(result, Ok(0)),
                "fixture process {} ({}) survived cleanup: {result:?}",
                self.description["pid"],
                self.description["role"]
            );
        }
    }
}

impl Drop for ProcessPeer {
    fn drop(&mut self) {
        // Failure teardown only: EOF releases surviving fixtures naturally.
        // Keep these sockets open until after the production-cleanup assertions.
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
    }
}

struct RuntimeTree {
    process: Arc<SharedChild>,
    process_tree: Arc<RuntimeProcessTree>,
    #[cfg(windows)]
    output_pipes: Option<(std::process::ChildStdout, std::process::ChildStderr)>,
    input: Option<std::process::ChildStdin>,
    peers: Vec<ProcessPeer>,
}

impl RuntimeTree {
    fn spawn(cwd: &FsPath) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "tests::delegation_process_cleanup::runtime_tree_fixture",
                "--nocapture",
            ])
            .env(FIXTURE_ROLE, "runtime")
            .env(FIXTURE_ENDPOINT, listener.local_addr().unwrap().to_string())
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (process, input, process_tree, output_pipes) = {
            let mut child = spawn_dedicated_runtime(&mut command, "Claude").unwrap();
            child.launch_guard.resume(&child.process, &child.tree).unwrap();
            (child.process, child.stdin, child.tree, Some((child.stdout, child.stderr)))
        };
        let mut owner = Self {
            process,
            process_tree,
            #[cfg(windows)]
            output_pipes,
            input: Some(input),
            peers: Vec::new(),
        };
        for _ in 0..3 {
            owner.peers.push(ProcessPeer::accept(&listener));
        }
        for role in ["runtime", "delegation-mcp", "engram-mcp"] {
            assert_eq!(
                owner
                    .peers
                    .iter()
                    .filter(|peer| peer.description["role"] == role)
                    .count(),
                1
            );
        }
        for peer in &owner.peers {
            assert_eq!(
                PathBuf::from(peer.description["cwd"].as_str().unwrap()),
                fs::canonicalize(cwd).unwrap()
            );
        }
        owner
    }

    fn assert_reaped(&self) {
        phase_sync::process_exit(&self.process, "public cancellation reaped runtime root");
        for peer in &self.peers {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::System::Threading::WaitForSingleObject;
            let wait_result = unsafe { WaitForSingleObject(peer.process.as_raw_handle(), 0) };
            eprintln!("owned process at cleanup assertion: {}", json!({"identity": &peer.description, "wait_result": wait_result}));
        }
        for peer in &self.peers {
            peer.assert_exited();
        }
    }
}

impl Drop for RuntimeTree {
    fn drop(&mut self) {
        // The hierarchy exits naturally once its peer sockets and stdin close.
        // This is after the asserted production cleanup, including on unwind.
        for peer in &self.peers {
            let _ = peer.socket.shutdown(std::net::Shutdown::Both);
        }
        self.input.take();
        let _ = self.process.wait();
        #[cfg(windows)]
        for peer in &self.peers {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::System::Threading::WaitForSingleObject;
            // All peers were released first, including children awaited by
            // their parents. Reap before TestTempRoot removes the fixture cwd.
            unsafe {
                WaitForSingleObject(
                    peer.process.as_raw_handle(),
                    phase_sync::DEADLOCK_GUARD.as_millis() as u32,
                );
            }
        }
        self.peers.clear();
        #[cfg(windows)]
        self.output_pipes.take();
    }
}

#[test]
fn runtime_tree_fixture() {
    let Ok(role) = std::env::var(FIXTURE_ROLE) else {
        return;
    };
    let endpoint = std::env::var(FIXTURE_ENDPOINT).unwrap();
    let mut child = if role == "engram-mcp" {
        None
    } else {
        let next = if role == "runtime" {
            "delegation-mcp"
        } else {
            "engram-mcp"
        };
        Some(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::delegation_process_cleanup::runtime_tree_fixture",
                    "--nocapture",
                ])
                .env(FIXTURE_ROLE, next)
                .env(FIXTURE_ENDPOINT, &endpoint)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        )
    };
    let mut socket = TcpStream::connect(endpoint).unwrap();
    writeln!(
        socket,
        "{}",
        runtime_fixture_identity(&role)
    )
    .unwrap();
    if role == "runtime" {
        let _ = std::io::stdin().read(&mut [0]);
    } else {
        let _ = socket.read(&mut [0]);
    }
    if let Some(child) = child.as_mut() {
        child.wait().unwrap();
    }
}

async fn cancel_tree_case(mid_turn: bool) {
    runtime_tree_case(mid_turn, None).await;
}

async fn runtime_tree_case(mid_turn: bool, terminal_result: Option<&str>) {
    let temp = TestTempRoot::create("delegation-process-cancel");
    let worktree = temp.path().join("worktree");
    fs::create_dir(&worktree).unwrap();
    let owner = RuntimeTree::spawn(&worktree);
    let (state, input_rx) = test_app_state_with_delegation_codex_runtime("process-cancel");
    let parent = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent,
            CreateDelegationRequest {
                prompt: "Fixture turn".to_owned(),
                title: None,
                cwd: Some(worktree.to_string_lossy().into_owned()),
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Explorer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .unwrap();
    phase_sync::receive(&input_rx, "child prompt dispatch");
    let (fixture_input, _fixture_receiver) = mpsc::channel();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner
            .find_session_index(&created.delegation.child_session_id)
            .unwrap();
        let child = &mut inner.sessions[index];
        // Dedicated provider hierarchy: the fixture has no model/network and
        // no actual tool command. The mid-turn control retains active turn
        // accounting, while the before-command case has produced no output.
        child.runtime = SessionRuntime::Claude(ClaudeRuntimeHandle {
            runtime_id: "dedicated-process-fixture".to_owned(),
            input_tx: fixture_input,
            process: owner.process.clone(),
            process_tree: Some(owner.process_tree.clone()),
        });
        child.session.status = SessionStatus::Active;
        if mid_turn {
            child.active_turn_start_message_count = Some(0);
        }
    }
    if let Some(result) = terminal_result {
        finish_delegation_child_with_assistant_text(
            &state,
            &created.delegation.child_session_id,
            &format!("## Result\nStatus: {result}\n\nSummary:\nFinished fixture."),
        );
        state
            .refresh_delegation_for_child_session(&created.delegation.child_session_id)
            .unwrap();
        let inner = state.inner.lock().unwrap();
        let record = inner
            .delegations
            .iter()
            .find(|record| record.id == created.delegation.id)
            .unwrap();
        assert_eq!(
            record.status,
            if result == "completed" {
                DelegationStatus::Completed
            } else {
                DelegationStatus::Failed
            }
        );
        drop(inner);
        owner.assert_reaped();
        fs::remove_dir(&worktree).expect("terminal child permits empty worktree removal");
        return;
    }
    let app = app_router(state.clone());
    let (status, response): (StatusCode, DelegationStatusResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{parent}/delegations/{}/cancel",
                created.delegation.id
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response.delegation.status, DelegationStatus::Canceled);
    owner.assert_reaped();
    fs::remove_dir(&worktree).expect("no canceled-child process pins the empty worktree");
}

#[tokio::test]
async fn delegation_process_cleanup_cancel_before_first_command() {
    cancel_tree_case(false).await;
}

#[tokio::test]
async fn delegation_process_cleanup_cancel_mid_turn() {
    cancel_tree_case(true).await;
}


#[tokio::test]
async fn delegation_process_cleanup_completed_runtime_tree() {
    runtime_tree_case(true, Some("completed")).await;
}

#[tokio::test]
async fn delegation_process_cleanup_failed_runtime_tree() {
    runtime_tree_case(true, Some("failed")).await;
}

#[cfg(windows)]
#[test]
fn delegation_process_cleanup_root_exit_retains_tree_obligation() {
    let temp = TestTempRoot::create("delegation-root-exit");
    let owner = RuntimeTree::spawn(temp.path());
    // Negative control: the old root-only cleanup leaves both owned descendants.
    kill_child_process(&owner.process, "Claude").unwrap();
    phase_sync::process_exit(&owner.process, "root-only termination");
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "root-exit-fixture".to_owned(),
        input_tx,
        process: owner.process.clone(),
        process_tree: Some(owner.process_tree.clone()),
    };
    assert!(
        !KillableRuntime::Claude(runtime.clone())
            .process_has_exited()
            .unwrap(),
        "exited root cannot discard the job's surviving-descendant cleanup owner"
    );
    runtime.kill().unwrap();
    owner.assert_reaped();
    assert!(
        KillableRuntime::Claude(runtime)
            .process_has_exited()
            .unwrap()
    );
}

#[cfg(windows)]
struct LaunchFailureScope;

#[cfg(windows)]
impl LaunchFailureScope {
    fn new() -> Self {
        TEST_RUNTIME_LAUNCH_OWNERS.with(|owners| {
            assert!(owners.borrow().is_none());
            *owners.borrow_mut() = Some(Arc::new(Mutex::new(Vec::new())));
        });
        Self
    }
}

#[cfg(windows)]
impl Drop for LaunchFailureScope {
    fn drop(&mut self) {
        TEST_RUNTIME_LAUNCH_HOOK.with(|hook| hook.borrow_mut().take());
        let registry = failed_runtime_launches();
        await_launch_cleanup_attempts(&registry);
        let entries = registry.lock().unwrap().clone();
        for entry in entries {
            let owner = entry.owner.lock().unwrap();
            if let Some(FailedRuntimeLaunch::Attached(owner)) = owner.as_ref()
                && let Some(tree) = &owner.tree
            {
                tree.termination_failures
                    .store(0, std::sync::atomic::Ordering::SeqCst);
            }
        }
        // Unwind cleanup uses the retained production owner after assertions.
        let _ = retry_failed_runtime_launch_cleanup();
        await_launch_cleanup_attempts(&registry);
        assert!(registry.lock().unwrap().is_empty(), "unwind cleanup retains no unresolved fixture owner");
        TEST_RUNTIME_LAUNCH_OWNERS.with(|owners| owners.borrow_mut().take());
    }
}

#[cfg(windows)]
fn await_launch_cleanup_attempts(registry: &FailedRuntimeLaunchRegistry) {
    let guard = phase_sync::PollGuard::new();
    loop {
        let entries = registry.lock().unwrap().clone();
        if !entries.iter().any(|entry| matches!(*entry.phase.lock().unwrap(), FailedLaunchPhase::InProgress(_))) { return; }
        guard.wait("bounded retained launch cleanup attempt");
    }
}

fn launch_fixture_command(cwd: &FsPath) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "tests::delegation_process_cleanup::runtime_tree_fixture",
        ])
        .env_remove(FIXTURE_ROLE)
        .env_remove(FIXTURE_ENDPOINT)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

#[cfg(windows)]
fn assert_launch_handle_exited(handle: &std::os::windows::io::OwnedHandle) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{Foundation::WAIT_OBJECT_0, System::Threading::WaitForSingleObject};
    assert_eq!(
        unsafe {
            WaitForSingleObject(
                handle.as_raw_handle(),
                phase_sync::DEADLOCK_GUARD.as_millis() as u32,
            )
        },
        WAIT_OBJECT_0,
        "failed launch left its exact suspended root alive"
    );
}

#[cfg(windows)]
fn launch_failure_case(phase: RuntimeLaunchPhase) {
    let temp = TestTempRoot::create("delegation-launch-failure");
    let _scope = LaunchFailureScope::new();
    let observed = Arc::new(Mutex::new(None));
    let hook_observed = observed.clone();
    TEST_RUNTIME_LAUNCH_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |current, root| {
            if current == phase {
                *hook_observed.lock().unwrap() = Some(root.try_clone().unwrap());
                bail!("forced launch failure at {phase:?}");
            }
            Ok(())
        }));
    });
    let result = (|| -> Result<()> {
        let mut child =
            spawn_dedicated_runtime(&mut launch_fixture_command(temp.path()), "Claude")?;
        child.launch_guard.resume(&child.process, &child.tree)
    })();
    assert!(format!("{:#}", result.unwrap_err()).contains("forced launch failure"));
    assert_launch_handle_exited(
        observed
            .lock()
            .unwrap()
            .as_ref()
            .expect("failure seam reached"),
    );
    await_launch_cleanup_attempts(&failed_runtime_launches());
    assert!(failed_runtime_launches().lock().unwrap().is_empty());
}

#[cfg(windows)]
#[test]
fn delegation_process_cleanup_launch_containment_failure() {
    launch_failure_case(RuntimeLaunchPhase::BeforeContainment);
}

#[cfg(windows)]
#[test]
fn delegation_process_cleanup_launch_sharing_failure() {
    launch_failure_case(RuntimeLaunchPhase::BeforeShare);
}

#[cfg(windows)]
#[test]
fn delegation_process_cleanup_launch_resume_failure() {
    launch_failure_case(RuntimeLaunchPhase::BeforeResume);
}

#[cfg(windows)]
#[test]
fn delegation_process_cleanup_launch_missing_pipe() {
    let temp = TestTempRoot::create("delegation-launch-pipe");
    let _scope = LaunchFailureScope::new();
    let observed = Arc::new(Mutex::new(None));
    let hook_observed = observed.clone();
    TEST_RUNTIME_LAUNCH_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |_, root| {
            *hook_observed.lock().unwrap() = Some(root.try_clone().unwrap());
            Ok(())
        }));
    });
    let mut command = launch_fixture_command(temp.path());
    command.stdout(Stdio::null());
    let error = spawn_dedicated_runtime(&mut command, "Claude")
        .err()
        .expect("missing pipe refused");
    assert!(error.to_string().contains("runtime pipes unavailable"));
    assert_launch_handle_exited(observed.lock().unwrap().as_ref().unwrap());
    await_launch_cleanup_attempts(&failed_runtime_launches());
    assert!(failed_runtime_launches().lock().unwrap().is_empty());
}

#[cfg(windows)]
#[test]
fn delegation_process_cleanup_launch_reader_startup_unwind() {
    let temp = TestTempRoot::create("delegation-launch-reader");
    let _scope = LaunchFailureScope::new();
    let observed = Arc::new(Mutex::new(None));
    let captured = observed.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let child =
            spawn_dedicated_runtime(&mut launch_fixture_command(temp.path()), "Claude").unwrap();
        *captured.lock().unwrap() = Some(
            child
                .launch_guard
                .root
                .as_ref()
                .unwrap()
                .try_clone()
                .unwrap(),
        );
        // Simulate std::thread::spawn's panic while installing a provider
        // reader. Providers retain the armed guard until all workers exist.
        panic!("forced reader startup failure");
    }));
    assert!(result.is_err());
    assert_launch_handle_exited(observed.lock().unwrap().as_ref().unwrap());
    await_launch_cleanup_attempts(&failed_runtime_launches());
    assert!(failed_runtime_launches().lock().unwrap().is_empty());
}

#[cfg(windows)]
#[test]
fn delegation_process_cleanup_launch_failure_retains_owner_before_retry() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{Foundation::WAIT_TIMEOUT, System::Threading::WaitForSingleObject};
    let temp = TestTempRoot::create("delegation-launch-retention");
    let _scope = LaunchFailureScope::new();
    let child =
        spawn_dedicated_runtime(&mut launch_fixture_command(temp.path()), "Claude").unwrap();
    let observed = child
        .launch_guard
        .root
        .as_ref()
        .unwrap()
        .try_clone()
        .unwrap();
    let tree = Arc::downgrade(&child.tree);
    child
        .tree
        .termination_failures
        .store(2, std::sync::atomic::Ordering::SeqCst);
    drop(child);
    await_launch_cleanup_attempts(&failed_runtime_launches());
    assert_eq!(
        unsafe { WaitForSingleObject(observed.as_raw_handle(), 0) },
        WAIT_TIMEOUT
    );
    assert!(
        tree.upgrade().is_some(),
        "registry retains the exact job lease after caller drop"
    );
    assert_eq!(failed_runtime_launches().lock().unwrap().len(), 1);
    // The next launch is refused before spawning any replacement. Its one
    // cleanup attempt fails too, and the same suspended owner stays reachable.
    let error = spawn_dedicated_runtime(&mut launch_fixture_command(temp.path()), "Claude")
        .err()
        .unwrap();
    assert!(format!("{error:#}").contains("prior runtime launch cleanup remains pending"));
    await_launch_cleanup_attempts(&failed_runtime_launches());
    assert_eq!(
        unsafe { WaitForSingleObject(observed.as_raw_handle(), 0) },
        WAIT_TIMEOUT
    );
    assert_eq!(failed_runtime_launches().lock().unwrap().len(), 1);
    assert!(retry_failed_runtime_launch_cleanup().is_err(), "scheduled cleanup remains visible to admission");
    await_launch_cleanup_attempts(&failed_runtime_launches());
    let replacement = spawn_dedicated_runtime(&mut launch_fixture_command(temp.path()), "Claude").unwrap();
    assert_launch_handle_exited(&observed);
    await_launch_cleanup_attempts(&failed_runtime_launches());
    assert!(failed_runtime_launches().lock().unwrap().is_empty());
    assert!(
        tree.upgrade().is_none(),
        "successful cleanup releases the retained lease"
    );
    drop(replacement);
}

#[cfg(windows)]
#[test]
fn delegation_process_cleanup_tree_failure_after_root_exit_is_retryable() {
    let temp = TestTempRoot::create("delegation-tree-retry");
    let owner = RuntimeTree::spawn(temp.path());
    kill_child_process(&owner.process, "Claude").unwrap();
    phase_sync::process_exit(&owner.process, "root-only negative control");
    owner
        .process_tree
        .termination_failures
        .store(1, std::sync::atomic::Ordering::SeqCst);
    assert!(
        owner
            .process_tree
            .terminate(&owner.process, "Claude")
            .is_err()
    );
    assert!(!owner.process_tree.is_confirmed());
    assert!(
        !owner.process_tree.has_exited().unwrap(),
        "root exit did not confirm descendant cleanup"
    );
    owner
        .process_tree
        .terminate(&owner.process, "Claude")
        .unwrap();
    owner.assert_reaped();
    assert!(owner.process_tree.is_confirmed());
}
#[cfg(windows)]
#[test]
fn delegation_process_cleanup_concurrent_terminators_share_tree_lease() {
    let temp = TestTempRoot::create("delegation-tree-race");
    let owner = RuntimeTree::spawn(temp.path());
    let start = Arc::new(std::sync::Barrier::new(3));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let start = start.clone();
            let tree = owner.process_tree.clone();
            let process = owner.process.clone();
            std::thread::spawn(move || {
                start.wait();
                tree.terminate(&process, "Claude")
            })
        })
        .collect();
    start.wait();
    for worker in workers {
        worker.join().unwrap().unwrap();
    }
    owner.assert_reaped();
    assert!(owner.process_tree.is_confirmed());
}

// A real stdin protocol producer for the production Claude and ACP workers.
// Descendants inherit stdout, so root exit cannot be confused with pipe EOF.
const WORKER_TREE_SCRIPT: &str = r#"
param([string]$Role, [string]$Endpoint)
$ErrorActionPreference = 'Stop'
if ($Role -ne 'engram-mcp') {
    $nextRole = if ($Role -eq 'runtime') { 'delegation-mcp' } else { 'engram-mcp' }
    $launch = New-Object System.Diagnostics.ProcessStartInfo
    $launch.FileName = (Get-Process -Id $PID).Path
    $launch.Arguments = '-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "' + $PSCommandPath + '" -Role ' + $nextRole + ' -Endpoint ' + $Endpoint
    $launch.UseShellExecute = $false
    $launch.RedirectStandardInput = $true
    $launch.CreateNoWindow = $true
    $descendant = [System.Diagnostics.Process]::Start($launch)
}
$request = $null
if ($Role -eq 'runtime') { $request = [Console]::ReadLine() | ConvertFrom-Json }
$parts = $Endpoint.Split(':')
$client = New-Object System.Net.Sockets.TcpClient($parts[0], [int]$parts[1])
$stream = $client.GetStream()
$writer = New-Object System.IO.StreamWriter($stream)
$writer.AutoFlush = $true
$method = if ($request.method) { $request.method } else { $request.request.subtype }
$identity = @{role=$Role;pid=$PID;cwd=(Get-Location).ProviderPath;path=(Get-Process -Id $PID).Path;started_at=(Get-Process -Id $PID).StartTime.ToUniversalTime().ToString('o');method=$method}
$writer.WriteLine(($identity | ConvertTo-Json -Compress))
$action = $stream.ReadByte()
if ($Role -eq 'runtime' -and $action -eq 2) {
    [Console]::WriteLine((@{jsonrpc='2.0';id=$request.id;error=@{code=-32000;message='owned producer initialization failure'}} | ConvertTo-Json -Compress))
    [void]$stream.ReadByte()
}
if ($action -eq 3) {
    [Console]::WriteLine('{"type":"user","message":{"role":"user","content":"<task-notification><task-id>owned-delayed-task</task-id><status>completed</status></task-notification>"},"isReplay":true}')
    [Console]::WriteLine('{"type":"assistant","message":{"content":[{"type":"text","text":"owned delayed reader output"}]}}')
    [void]$stream.ReadByte()
    [Console]::WriteLine('{"type":"result","subtype":"success","is_error":false}')
    [void]$stream.ReadByte()
}
$client.Close()
"#;

pub(super) struct ProductionWorkerTree {
    process: Arc<SharedChild>,
    pub(super) tree: Arc<RuntimeProcessTree>,
    peers: Vec<ProcessPeer>,
    waiter: mpsc::Receiver<()>,
    writer: Option<mpsc::Receiver<()>>,
    writer_finished: std::cell::Cell<bool>,
    pub(super) pending: Option<AcpPendingRequestMap>,
}

impl ProductionWorkerTree {
    pub(super) fn spawn(state: &AppState, session_id: &str, cwd: &FsPath, acp: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let script = cwd.join("worker.ps1");
        fs::write(&script, WORKER_TREE_SCRIPT).unwrap();
        let mut command = Command::new(PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/WindowsPowerShell/v1.0/powershell.exe"));
        command.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&script).args(["-Role", "runtime", "-Endpoint"])
            .arg(listener.local_addr().unwrap().to_string()).current_dir(cwd);
        TEST_DEDICATED_COMMAND.with(|slot| *slot.borrow_mut() = Some(command));
        let (waiter_tx, waiter) = mpsc::channel();
        TEST_DEDICATED_WAITER.with(|slot| *slot.borrow_mut() = Some(waiter_tx));
        let (writer_tx, writer_rx) = mpsc::channel();
        if acp { TEST_DEDICATED_WRITER.with(|slot| *slot.borrow_mut() = Some(writer_tx)); }
        let (pending_tx, pending_rx) = mpsc::channel();
        if acp { TEST_DEDICATED_ACP_PENDING.with(|slot| *slot.borrow_mut() = Some(pending_tx)); }
        // The root never answers initialize; each test ends it explicitly, so
        // no wall-clock handshake deadline can kill the tree first.
        if acp { TEST_DEDICATED_ACP_INITIALIZE_UNTIMED.with(|untimed| untimed.set(true)); }
        let mut inner = state.inner.lock().unwrap();
        let runtime = if acp {
            SessionRuntime::Acp(spawn_acp_runtime(state.clone(), session_id.to_owned(),
                cwd.to_string_lossy().into_owned(), AcpAgent::Kimi, None, None).unwrap())
        } else {
            SessionRuntime::Claude(spawn_claude_runtime(state.clone(), session_id.to_owned(),
                cwd.to_string_lossy().into_owned(), Agent::Claude.default_model().to_owned(),
                ClaudeApprovalMode::Ask, ClaudeEffortLevel::Default, None,
                "{\"mcpServers\":{}}".to_owned(), None, false, None).unwrap())
        };
        let (process, tree) = match &runtime {
            SessionRuntime::Claude(handle) => (handle.process.clone(), handle.process_tree.clone().unwrap()),
            SessionRuntime::Acp(handle) => (handle.process.clone(), handle.process_tree.clone().unwrap()),
            _ => unreachable!(),
        };
        let index = inner.find_session_index(session_id).unwrap();
        inner.sessions[index].runtime = runtime;
        inner.sessions[index].register_dedicated_runtime();
        inner.sessions[index].session.status = SessionStatus::Active;
        state.commit_locked(&mut inner).unwrap();
        drop(inner);
        let mut owner = Self { process, tree, peers: Vec::new(), waiter,
            writer: if acp { Some(writer_rx) } else { None },
            writer_finished: std::cell::Cell::new(false),
            pending: if acp { Some(phase_sync::receive(&pending_rx, "production ACP pending map")) } else { None } };
        for _ in 0..3 { owner.peers.push(ProcessPeer::accept(&listener)); }
        let root = owner.peers.iter().find(|peer| peer.description["role"] == "runtime").unwrap();
        assert_eq!(root.description["method"], "initialize", "actual provider writer request");
        for peer in &owner.peers {
            assert_eq!(fs::canonicalize(peer.description["cwd"].as_str().unwrap()).unwrap(), fs::canonicalize(cwd).unwrap());
        }
        owner
    }

    pub(super) fn exit_root(&self) {
        self.release_root();
        self.await_exit();
    }

    pub(super) fn release_root(&self) {
        let root = self.peers.iter().find(|peer| peer.description["role"] == "runtime").unwrap();
        (&root.socket).write_all(&[1]).unwrap();
    }

    pub(super) fn await_exit(&self) {
        phase_sync::receive(&self.waiter, "actual production waiter completed");
        phase_sync::process_exit(&self.process, "protocol producer root exited naturally");
        self.wait_for_writer();
    }

    pub(super) fn wait_for_writer(&self) {
        if let Some(writer) = &self.writer && !self.writer_finished.replace(true) {
            phase_sync::receive(writer, "ACP writer applies its normal initialization-error callback");
        }
    }

    pub(super) fn fail_initialize(&self) {
        let root = self.peers.iter().find(|peer| peer.description["role"] == "runtime").unwrap();
        (&root.socket).write_all(&[2]).unwrap();
        self.wait_for_writer();
    }

    /// Applies exactly what the writer's expiring initialize deadline does
    /// (`wait_for_acp_json_rpc_response`): the pending sender goes away, so
    /// the writer fails initialize at once and runs its failure cleanup.
    pub(super) fn expire_initialize(&self) {
        let pending = self.pending.as_ref().expect("an ACP worker tree");
        let expired = std::mem::take(&mut *pending.lock().unwrap());
        assert_eq!(expired.len(), 1, "only the provider's initialize is pending");
        drop(expired);
        self.wait_for_writer();
    }

    pub(super) fn assert_peers_exited(&self) {
        for peer in &self.peers { peer.assert_exited(); }
    }
}

impl Drop for ProductionWorkerTree {
    fn drop(&mut self) {
        for peer in &self.peers { let _ = peer.socket.shutdown(std::net::Shutdown::Both); }
        for peer in &self.peers { peer.assert_exited(); }
    }
}

fn production_waiter_failure_case(acp: bool) {
    let temp = TestTempRoot::create("production-waiter-tree-failure");
    let mut state = test_app_state();
    state.agent_runtime_spawning_enabled = true;
    let session_id = test_session_id(&state, if acp { Agent::Kimi } else { Agent::Claude });
    let owner = ProductionWorkerTree::spawn(&state, &session_id, temp.path(), acp);
    if let Some(pending) = &owner.pending { assert_eq!(pending.lock().unwrap().len(), 1); }
    owner.tree.termination_failures.store(1, std::sync::atomic::Ordering::SeqCst);
    owner.exit_root();
    assert!(!owner.tree.has_exited().unwrap(), "descendants still own the stdout pipe at failure reporting");
    let inner = state.inner.lock().unwrap();
    let record = inner.sessions[inner.find_session_index(&session_id).unwrap()].clone();
    drop(inner);
    assert_eq!(record.session.status, SessionStatus::Error,
        "the actual waiter must publish cleanup failure without waiting for descendant pipe EOF");
    assert!(record.orchestrator_auto_dispatch_blocked, "cleanup uncertainty reserves automatic dispatch");
    assert!(record.session.preview.contains("process tree cleanup failed"));
    match &record.runtime {
        SessionRuntime::Claude(handle) => assert!(Arc::ptr_eq(handle.process_tree.as_ref().unwrap(), &owner.tree)),
        SessionRuntime::Acp(handle) => assert!(Arc::ptr_eq(handle.process_tree.as_ref().unwrap(), &owner.tree)),
        _ => panic!("failed waiter must retain its exact tree for retry"),
    }
    if let Some(pending) = &owner.pending { assert!(pending.lock().unwrap().is_empty(), "ACP pending calls fail before EOF"); }
    state.request_stop_session(&session_id).expect("public Stop can retry the retained cleanup owner");
    let guard = phase_sync::PollGuard::new();
    while !owner.tree.has_exited().unwrap() { guard.wait("public Stop cleanup retry"); }
    for peer in &owner.peers { peer.assert_exited(); }
    let guard = phase_sync::PollGuard::new();
    loop {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session_id).unwrap()];
        if matches!(record.runtime, SessionRuntime::None) { break; }
        drop(inner);
        guard.wait("Stop commits exact owner disposal");
    }
}

#[test]
fn delegation_process_cleanup_production_claude_waiter_failure() { production_waiter_failure_case(false); }

#[test]
fn delegation_process_cleanup_production_acp_waiter_failure() { production_waiter_failure_case(true); }

fn production_waiter_success_case(acp: bool) {
    let temp = TestTempRoot::create("production-waiter-tree-success");
    let mut state = test_app_state();
    state.agent_runtime_spawning_enabled = true;
    let session_id = test_session_id(&state, if acp { Agent::Kimi } else { Agent::Claude });
    let owner = ProductionWorkerTree::spawn(&state, &session_id, temp.path(), acp);
    owner.exit_root();
    assert!(owner.tree.has_exited().unwrap());
    for peer in &owner.peers { peer.assert_exited(); }
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session_id).unwrap()];
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.orchestrator_auto_dispatch_blocked, "confirmed natural-exit cleanup releases its transient hold");
    assert!(!record.session.preview.contains("cleanup remains in progress"));
}

#[test]
fn delegation_process_cleanup_production_claude_natural_exit() { production_waiter_success_case(false); }
#[test]
fn delegation_process_cleanup_production_acp_natural_exit() { production_waiter_success_case(true); }

fn production_waiter_stop_race_case(acp: bool, fail_stop: bool) {
    let temp = TestTempRoot::create("production-waiter-stop-race");
    let mut state = test_app_state();
    state.agent_runtime_spawning_enabled = true;
    let session_id = test_session_id(&state, if acp { Agent::Kimi } else { Agent::Claude });
    let owner = ProductionWorkerTree::spawn(&state, &session_id, temp.path(), acp);
    owner.tree.termination_failures.store(if fail_stop { 2 } else { 1 }, std::sync::atomic::Ordering::SeqCst);
    let gate = install_test_stop_fence_gate(&state, &session_id);
    state.request_stop_session(&session_id).unwrap();
    gate.wait_until_claimed();
    owner.exit_root();
    let record = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session_id).unwrap()].clone()
    };
    assert_eq!(record.session.status, SessionStatus::Stopping, "waiter cannot overwrite the Stop owner's projection");
    assert!(record.runtime_stop_in_progress);
    assert!(!record.deferred_stop_callbacks.is_empty(), "cleanup reporting retains its Stop-fenced callback duty");
    gate.release();
    let guard = phase_sync::PollGuard::new();
    loop {
        let record = {
            let inner = state.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&session_id).unwrap()].clone()
        };
        if !record.runtime_stop_in_progress {
            if fail_stop {
                assert!(record.runtime.dedicated_cleanup_failure().is_some());
                assert_eq!(record.session.status, SessionStatus::Error);
                assert!(record.orchestrator_auto_dispatch_blocked);
                state.request_stop_session(&session_id).unwrap();
            } else { assert!(matches!(record.runtime, SessionRuntime::None)); }
            break;
        }
        guard.wait("claimed Stop settles its worker");
    }
    let guard = phase_sync::PollGuard::new();
    while !owner.tree.has_exited().unwrap() { guard.wait("retry cleans the same retained tree"); }
    for peer in &owner.peers { peer.assert_exited(); }
}

#[test]
fn delegation_process_cleanup_production_claude_stop_race() { production_waiter_stop_race_case(false, false); }
#[test]
fn delegation_process_cleanup_production_acp_stop_race() { production_waiter_stop_race_case(true, false); }
#[test]
fn delegation_process_cleanup_production_claude_failed_stop_retry() { production_waiter_stop_race_case(false, true); }
#[test]
fn delegation_process_cleanup_production_acp_failed_stop_retry() { production_waiter_stop_race_case(true, true); }

#[test]
fn delegation_process_cleanup_production_acp_writer_precedes_waiter() {
    let temp = TestTempRoot::create("production-writer-before-waiter");
    let mut state = test_app_state();
    state.agent_runtime_spawning_enabled = true;
    let session_id = test_session_id(&state, Agent::Kimi);
    let owner = ProductionWorkerTree::spawn(&state, &session_id, temp.path(), true);
    assert!(owner.tree.is_live());
    owner.tree.termination_failures.store(1, std::sync::atomic::Ordering::SeqCst);
    owner.fail_initialize();
    assert!(owner.process.try_wait().unwrap().is_none(), "provider root is still alive when its real writer exits");
    let record = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session_id).unwrap()].clone()
    };
    assert_eq!(record.session.status, SessionStatus::Error);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(record.runtime.dedicated_cleanup_failure().is_some());
    let SessionRuntime::Acp(handle) = &record.runtime else { panic!("writer must retain its live lease"); };
    assert!(Arc::ptr_eq(handle.process_tree.as_ref().unwrap(), &owner.tree));
    owner.exit_root();
    assert!(owner.tree.is_confirmed());
    assert!(owner.tree.has_exited().unwrap());
}

fn production_stale_owner_case(acp: bool) {
    let temp = TestTempRoot::create("production-stale-worker");
    let replacement_temp = TestTempRoot::create("production-replacement-worker");
    let mut state = test_app_state();
    state.agent_runtime_spawning_enabled = true;
    let session_id = test_session_id(&state, if acp { Agent::Kimi } else { Agent::Claude });
    let (claimed_tx, claimed_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    TEST_DEDICATED_EXIT_GATE.with(|slot| *slot.borrow_mut() = Some(TestStopFenceGate { claimed_tx, release_rx }));
    let owner = ProductionWorkerTree::spawn(&state, &session_id, temp.path(), acp);
    owner.tree.termination_failures.store(1, std::sync::atomic::Ordering::SeqCst);
    let root = owner.peers.iter().find(|peer| peer.description["role"] == "runtime").unwrap();
    (&root.socket).write_all(&[1]).unwrap();
    phase_sync::process_exit(&owner.process, "root exits before its waiter can claim cleanup");
    phase_sync::receive(&claimed_rx, "actual waiter paused before first cleanup claim");
    assert!(owner.tree.is_live());
    assert!(state.inner.inner.try_lock().is_ok());
    // Deliberately bypass public predecessor admission to test stale callback
    // isolation. The public route refuses this unconfirmed predecessor.
    let replacement = ProductionWorkerTree::spawn(&state, &session_id, replacement_temp.path(), acp);
    let before = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session_id).unwrap()].clone()
    };
    assert_eq!(before.retained_dedicated_owners.len(), 2, "Live registration predates the first worker claim");
    assert!(before.dedicated_predecessor_pending());
    release_tx.send(()).unwrap();
    owner.await_exit();
    assert!(owner.tree.cleanup_failure().is_some());
    assert!(!owner.tree.has_exited().unwrap());
    let after_failure = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session_id).unwrap()].clone()
    };
    assert_eq!(after_failure.runtime.runtime_token(), before.runtime.runtime_token());
    assert_eq!(after_failure.session.status, before.session.status);
    assert_eq!(after_failure.engram.active_grant_id, before.engram.active_grant_id);
    assert_eq!(after_failure.active_turn_generation, before.active_turn_generation);
    assert_eq!(after_failure.retained_dedicated_owners.len(), 2);
    state.retry_retained_dedicated_cleanup(&session_id).unwrap();
    assert!(owner.tree.is_confirmed());
    for peer in &owner.peers { peer.assert_exited(); }
    let after_retry = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session_id).unwrap()].clone()
    };
    assert_eq!(after_retry.runtime.runtime_token(), before.runtime.runtime_token());
    assert_eq!(after_retry.session.status, before.session.status);
    assert_eq!(after_retry.engram.active_grant_id, before.engram.active_grant_id);
    assert_eq!(after_retry.retained_dedicated_owners.len(), 1);
    assert!(!after_retry.dedicated_predecessor_pending());
    replacement.exit_root();
    assert!(replacement.tree.is_confirmed());
}

#[test]
fn delegation_process_cleanup_production_claude_stale_waiter_owner() { production_stale_owner_case(false); }
#[test]
fn delegation_process_cleanup_production_acp_stale_waiter_owner() { production_stale_owner_case(true); }

#[test]
fn delegation_process_cleanup_round2_in_progress_callback_releases_hold() {
    let temp = TestTempRoot::create("production-in-progress-cleanup");
    let mut state = test_app_state();
    state.agent_runtime_spawning_enabled = true;
    let session_id = test_session_id(&state, Agent::Claude);
    let owner = ProductionWorkerTree::spawn(&state, &session_id, temp.path(), false);
    let token = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session_id).unwrap()].runtime.runtime_token().unwrap()
    };
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let tree = owner.tree.clone();
    let attempt = std::thread::spawn(move || tree.terminate_with("Claude", |_| {
        entered_tx.send(()).unwrap();
        phase_sync::receive(&release_rx, "off-lock cleanup release");
        Ok(())
    }));
    phase_sync::receive(&entered_rx, "claimed attempt enters OS cleanup outside locks");
    assert!(owner.tree.cleanup.try_lock().is_ok(), "lease state cannot be locked across OS work");
    assert!(state.inner.inner.try_lock().is_ok(), "unrelated state remains usable during cleanup");
    let (joined_tx, joined_rx) = mpsc::channel();
    let callback_state = state.clone();
    let callback_id = session_id.clone();
    let callback_token = token.clone();
    let callback = std::thread::spawn(move || {
        TEST_DEDICATED_CLEANUP_JOIN.with(|slot| *slot.borrow_mut() = Some(joined_tx));
        callback_state.handle_runtime_exit_if_matches(&callback_id, &callback_token, Some("cleanup notification"))
    });
    phase_sync::receive(&joined_rx, "callback joins the pinned in-progress attempt");
    let record = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session_id).unwrap()].clone()
    };
    assert_eq!(record.runtime.runtime_token().as_ref(), Some(&token));
    assert_eq!(record.session.status, SessionStatus::Active, "joining cleanup is not a provider failure");
    assert!(!record.orchestrator_auto_dispatch_blocked);
    release_tx.send(()).unwrap();
    attempt.join().unwrap().unwrap();
    callback.join().unwrap().unwrap();
    owner.await_exit();
    assert!(owner.tree.is_confirmed());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session_id).unwrap()];
    assert!(!record.orchestrator_auto_dispatch_blocked,
        "confirmed production waiter cleanup must release the transient cleanup hold");
    assert!(!record.session.preview.contains("cleanup remains in progress"),
        "an intermediate cleanup projection cannot survive confirmation");
}

fn production_revocation_failure_case(acp: bool, confirm_before_finalization: bool) {
    let temp = TestTempRoot::create("production-revocation-uncertain-tree");
    let mut state = test_app_state();
    let state_cleaned = state.test_temp_root.as_ref().unwrap().observe_cleanup();
    state.agent_runtime_spawning_enabled = true;
    let session_id = test_session_id(&state, if acp { Agent::Kimi } else { Agent::Claude });
    let owner = ProductionWorkerTree::spawn(&state, &session_id, temp.path(), acp);
    let batch = {
        let mut inner = state.inner.lock().unwrap();
        claim_engram_mcp_runtime_revocations_locked(&mut inner, std::slice::from_ref(&session_id))
    };
    owner.tree.termination_failures.store(3, std::sync::atomic::Ordering::SeqCst);
    owner.exit_root();
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session_id).unwrap()];
        assert!(record.runtime_stop_in_progress);
        assert!(record.deferred_stop_callbacks.iter().any(|callback|
            matches!(callback, DeferredStopCallback::RuntimeExited { .. })));
        assert!(record.runtime.dedicated_cleanup_failure().is_some());
    }
    let shutdown = state.shutdown_revoked_engram_mcp_runtimes(batch, "owned revocation witness", None);
    assert!(shutdown.shutdowns[0].retain_runtime_for_retry);
    assert!(shutdown.shutdowns[0].shutdown_error.is_some());
    assert!(!owner.tree.has_exited().unwrap(), "both descendants still own pipes before finalization");
    if confirm_before_finalization {
        owner.tree.terminate(&owner.process, if acp { "Kimi" } else { "Claude" }).unwrap();
        assert!(owner.tree.is_confirmed());
    }
    let outcome = state.finalize_revoked_engram_mcp_runtimes(shutdown);
    let record = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session_id).unwrap()].clone()
    };
    // Finish the owned hierarchy before assertions so a RED cannot strand it.
    if confirm_before_finalization {
        owner.tree.terminate(&owner.process, if acp { "Kimi" } else { "Claude" }).unwrap();
    } else {
        state.stop_session_with_options(&session_id, StopSessionOptions::default()).unwrap();
    }
    for peer in &owner.peers { peer.assert_exited(); }
    if confirm_before_finalization {
        assert!(outcome.failures.is_empty(), "the exact job-zero proof permits buffered revocation success");
        assert!(matches!(record.runtime, SessionRuntime::None));
        assert!(!record.runtime_reset_required && !record.engram_mcp_runtime_quarantined);
        assert_eq!(record.session.status, SessionStatus::Idle);
    } else {
        assert!(!outcome.failures.is_empty(), "buffered root exit cannot suppress unconfirmed job cleanup");
        assert!(record.runtime.runtime_token().is_some(), "revocation keeps its current exact retry owner");
        assert!(record.runtime_reset_required && record.engram_mcp_runtime_quarantined);
        assert_eq!(record.session.status, SessionStatus::Error);
        assert!(record.orchestrator_auto_dispatch_blocked);
    }
    // The assertion snapshot also keeps the writer channel open after Stop.
    drop(record);
    {
        let inner = state.inner.lock().unwrap();
        assert!(matches!(inner.sessions[inner.find_session_index(&session_id).unwrap()].runtime,
            SessionRuntime::None));
    }
    drop(owner);
    drop(state);
    state_cleaned.recv_timeout(phase_sync::DEADLOCK_GUARD)
        .expect("all production worker state owners must release the fixture root")
        .expect("production worker fixture root must be removed");
}

#[test]
fn delegation_process_cleanup_round2_claude_revocation_requires_job_zero() { production_revocation_failure_case(false, false); }
#[test]
fn delegation_process_cleanup_round2_acp_revocation_requires_job_zero() { production_revocation_failure_case(true, false); }

#[test]
fn delegation_process_cleanup_round2_claude_revocation_accepts_exact_confirmed_job() { production_revocation_failure_case(false, true); }
#[test]
fn delegation_process_cleanup_round2_acp_revocation_accepts_exact_confirmed_job() { production_revocation_failure_case(true, true); }

fn production_post_revocation_exit_case(acp: bool, fail_waiter_retry: bool) {
    let temp = TestTempRoot::create("production-post-revocation-exit");
    let mut state = test_app_state();
    let state_cleaned = state.test_temp_root.as_ref().unwrap().observe_cleanup();
    state.agent_runtime_spawning_enabled = true;
    let session_id = test_session_id(&state, if acp { Agent::Kimi } else { Agent::Claude });
    let owner = ProductionWorkerTree::spawn(&state, &session_id, temp.path(), acp);
    let batch = {
        let mut inner = state.inner.lock().unwrap();
        claim_engram_mcp_runtime_revocations_locked(&mut inner, std::slice::from_ref(&session_id))
    };
    // Both revocation attempts fail while the root and its pipe-owning
    // descendants are live. Only the negative case also fails the later waiter.
    owner.tree.termination_failures.store(if fail_waiter_retry { 3 } else { 2 },
        std::sync::atomic::Ordering::SeqCst);
    let shutdown = state.shutdown_revoked_engram_mcp_runtimes(batch, "post-finalization exit witness", None);
    assert!(shutdown.shutdowns[0].retain_runtime_for_retry);
    let outcome = state.finalize_revoked_engram_mcp_runtimes(shutdown);
    assert!(!outcome.failures.is_empty());
    let token = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session_id).unwrap()];
        assert_eq!(record.session.status, SessionStatus::Error);
        assert!(record.engram_mcp_runtime_quarantined && record.runtime_reset_required);
        assert!(!record.runtime_stop_in_progress && record.deferred_stop_callbacks.is_empty());
        record.runtime.runtime_token().unwrap()
    };
    assert!(!owner.tree.has_exited().unwrap());
    // The actual provider waiter now runs AFTER the retained revocation
    // finalization; the ACP writer also applies its ordinary failure callback.
    owner.exit_root();
    let record = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session_id).unwrap()].clone()
    };
    let tree_was_confirmed = owner.tree.is_confirmed();
    let stop_result = if fail_waiter_retry {
        Some(state.request_stop_session(&session_id))
    } else { None };
    // RED cleanup is explicit and separate from the evidence above. If the
    // public Stop was refused, confirm this fixture's exact lease and replay
    // its root callback so the retained writer/state cycle cannot outlive it.
    if stop_result.as_ref().is_some_and(|result| result.is_err()) {
        owner.tree.terminate(&owner.process, if acp { "Kimi" } else { "Claude" }).unwrap();
        state.handle_runtime_root_exit_if_matches(&session_id, &token, None).unwrap();
    }
    let guard = phase_sync::PollGuard::new();
    loop {
        let inner = state.inner.lock().unwrap();
        let cleared = matches!(inner.sessions[inner.find_session_index(&session_id).unwrap()].runtime,
            SessionRuntime::None);
        drop(inner);
        if cleared { break; }
        guard.wait("post-revocation public Stop commits cleanup");
    }
    for peer in &owner.peers { peer.assert_exited(); }
    // Snapshot and Stop outcome precede all RED fallback cleanup.
    let observed_status = record.session.status.clone();
    let observed_preview = record.session.preview.clone();
    let observed_token = record.runtime.runtime_token();
    let observed_hold = record.orchestrator_auto_dispatch_blocked;
    let observed_quarantine = record.engram_mcp_runtime_quarantined;
    let observed_reset = record.runtime_reset_required;
    let observed_exact_owner = record.runtime.dedicated_tree_owner()
        .is_some_and(|(tree, _, _)| Arc::ptr_eq(&tree, &owner.tree));
    drop(record);
    drop(owner);
    drop(state);
    state_cleaned.recv_timeout(phase_sync::DEADLOCK_GUARD)
        .expect("post-revocation workers must release their state owners")
        .expect("post-revocation worker fixture root must be removed");
    if fail_waiter_retry {
        let stop_error = stop_result.as_ref().and_then(|result| result.as_ref().err())
            .map(|error| error.message.as_str());
        eprintln!("post-revocation witness acp={acp}: status={observed_status:?}, preview={observed_preview:?}, Stop error={stop_error:?}");
        assert!(!tree_was_confirmed);
        assert_eq!(observed_token.as_ref(), Some(&token));
        assert!(observed_exact_owner && observed_hold && observed_quarantine && observed_reset);
        assert_eq!(observed_status, SessionStatus::Error,
            "failed cleanup after revocation must stay visibly retryable");
        assert!(observed_preview.contains("process tree cleanup failed"),
            "the failed waiter retry must expose its cleanup detail");
        assert!(stop_result.unwrap().is_ok(), "public Stop must admit the current uncertain owner");
    } else {
        assert!(tree_was_confirmed, "successful waiter proves exact job-zero cleanup");
        assert!(observed_token.is_none());
        assert_eq!(observed_status, SessionStatus::Idle,
            "confirmed delayed revocation cleanup may settle to Idle");
        assert!(observed_hold, "revocation retains its explicit automatic-resume hold");
        // Confirmation releases the exact runtime owner, while the retained
        // reset/hold disposition still governs rebinding the revoked descriptor.
        assert!(observed_quarantine && observed_reset);
    }
}

#[test]
fn delegation_process_cleanup_round3_post_revocation_claude_failure() {
    production_post_revocation_exit_case(false, true);
}
#[test]
fn delegation_process_cleanup_round3_post_revocation_acp_failure() {
    production_post_revocation_exit_case(true, true);
}
#[test]
fn delegation_process_cleanup_round3_post_revocation_claude_confirmed() {
    production_post_revocation_exit_case(false, false);
}
#[test]
fn delegation_process_cleanup_round3_post_revocation_acp_confirmed() {
    production_post_revocation_exit_case(true, false);
}

// The real reader consumes frames written by a still-live, pipe-owning
// descendant, after the root waiter's exact cleanup attempt has failed.
fn production_late_reader_case(fail_before_adoption: Option<bool>) {
    let temp = TestTempRoot::create("production-late-reader");
    let mut state = test_app_state();
    let state_cleaned = state.test_temp_root.as_ref().unwrap().observe_cleanup();
    state.agent_runtime_spawning_enabled = true;
    let session_id = test_session_id(&state, Agent::Claude);
    let (frame_tx, frame_rx) = mpsc::channel();
    TEST_DEDICATED_READER_FRAMES.with(|slot| *slot.borrow_mut() = Some(frame_tx));
    let owner = ProductionWorkerTree::spawn(&state, &session_id, temp.path(), false);
    let snapshot = || {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session_id).unwrap()].clone()
    };
    let token = snapshot().runtime.runtime_token().unwrap();
    let generation = snapshot().active_turn_generation;
    state.finish_turn_ok_if_runtime_matches(&session_id, &token).unwrap();
    let fail_cleanup = || {
        owner.tree.termination_failures.store(1, std::sync::atomic::Ordering::SeqCst);
        owner.exit_root();
        let record = snapshot();
        assert_eq!(record.session.status, SessionStatus::Error);
        assert!(record.session.preview.contains("process tree cleanup failed"));
        assert!(!owner.tree.is_confirmed() && !owner.tree.has_exited().unwrap());
        record.session.preview.clone()
    };
    let mut cleanup_preview = if fail_before_adoption == Some(true) {
        Some(fail_cleanup())
    } else { None };
    let producer = owner.peers.iter()
        .find(|peer| peer.description["role"] == "engram-mcp").unwrap();
    (&producer.socket).write_all(&[3]).unwrap();
    phase_sync::receive(&frame_rx, "actual reader applies delayed task notice");
    phase_sync::receive(&frame_rx, "actual reader records delayed assistant output");
    let after_output = snapshot();
    if fail_before_adoption == Some(false) { cleanup_preview = Some(fail_cleanup()); }
    (&producer.socket).write_all(&[4]).unwrap();
    phase_sync::receive(&frame_rx, "actual reader finishes the delayed success result");
    let after_result = snapshot();
    let uncertain_at_result = !owner.tree.is_confirmed();
    let stop_result = fail_before_adoption.map(|_| state.request_stop_session(&session_id));
    // All observations above precede fallback cleanup. An original refusal
    // must still release this exact fixture and its worker/state owners.
    if stop_result.as_ref().is_none_or(|result| result.is_err()) {
        owner.tree.terminate(&owner.process, "Claude").unwrap();
        if fail_before_adoption.is_some() {
            state.handle_runtime_root_exit_if_matches(&session_id, &token, None).unwrap();
        } else { owner.await_exit(); }
    }
    let guard = phase_sync::PollGuard::new();
    while snapshot().runtime.runtime_token().is_some() {
        guard.wait("late reader cleanup commits exact owner disposal");
    }
    for peer in &owner.peers { peer.assert_exited(); }
    let exact_owner = after_result.runtime.dedicated_tree_owner()
        .is_some_and(|(tree, _, _)| Arc::ptr_eq(&tree, &owner.tree));
    let statuses = (after_output.session.status, after_result.session.status);
    let previews = (after_output.session.preview.clone(), after_result.session.preview.clone());
    let held = after_result.orchestrator_auto_dispatch_blocked;
    let retained_token = after_result.runtime.runtime_token();
    let adopted = after_output.unmediated_claude_turn.as_ref()
        .and_then(|turn| turn.adopted_generation);
    let finished = after_result.unmediated_claude_turn.is_none();
    let notice = after_result.session.messages.iter().any(|message|
        matches!(message, Message::Text { text, .. } if text.contains("TermAl did not")));
    let output = after_result.session.messages.iter().any(|message|
        matches!(message, Message::Text { text, .. } if text.contains("owned delayed reader output")));
    drop(after_output);
    drop(after_result);
    drop(owner);
    drop(state);
    state_cleaned.recv_timeout(phase_sync::DEADLOCK_GUARD)
        .expect("late reader workers release their state owners")
        .expect("late reader fixture state root removed");
    assert!(notice && output && finished, "actual delayed frames retain transcript and ownership observation");
    if let Some(before_adoption) = fail_before_adoption {
        eprintln!("late reader before_adoption={before_adoption}: status={statuses:?}, previews={previews:?}, Stop={:?}",
            stop_result.as_ref().and_then(|result| result.as_ref().err()).map(|error| &error.message));
        assert!(uncertain_at_result && exact_owner && held);
        assert_eq!(retained_token.as_ref(), Some(&token));
        assert_eq!(statuses.1, SessionStatus::Error, "success cannot hide unresolved exact cleanup");
        assert_eq!(previews.1, cleanup_preview.clone().unwrap());
        if before_adoption {
            assert_eq!(statuses.0, SessionStatus::Error, "late adoption cannot hide unresolved exact cleanup");
            assert_eq!(previews.0, cleanup_preview.unwrap());
            assert!(adopted.is_none());
        } else { assert_eq!(adopted, Some(generation.wrapping_add(1).max(1))); }
        assert!(stop_result.unwrap().is_ok(), "public Stop keeps its uncertain-owner retry route");
    } else {
        assert_eq!(statuses, (SessionStatus::Active, SessionStatus::Idle));
        assert_eq!(adopted, Some(generation.wrapping_add(1).max(1)));
        assert_eq!(retained_token.as_ref(), Some(&token));
        assert!(!held && exact_owner, "healthy reader remains live and dispatchable");
    }
}

#[test]
fn delegation_process_cleanup_round4_late_reader_uncertain_adoption() {
    production_late_reader_case(Some(true));
}
#[test]
fn delegation_process_cleanup_round4_late_reader_uncertain_success() {
    production_late_reader_case(Some(false));
}
#[test]
fn delegation_process_cleanup_round4_late_reader_healthy_adoption() {
    production_late_reader_case(None);
}

fn production_in_progress_reader_case(failed_retry: bool) {
    let temp = TestTempRoot::create("production-in-progress-reader");
    let mut state = test_app_state();
    let state_cleaned = state.test_temp_root.as_ref().unwrap().observe_cleanup();
    state.agent_runtime_spawning_enabled = true;
    let session_id = test_session_id(&state, Agent::Claude);
    let (frame_tx, frame_rx) = mpsc::channel();
    TEST_DEDICATED_READER_FRAMES.with(|slot| *slot.borrow_mut() = Some(frame_tx.clone()));
    let owner = ProductionWorkerTree::spawn(&state, &session_id, temp.path(), false);
    let snapshot = || {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session_id).unwrap()].clone()
    };
    let token = snapshot().runtime.runtime_token().unwrap();
    state.finish_turn_ok_if_runtime_matches(&session_id, &token).unwrap();
    if failed_retry {
        owner.tree.termination_failures.store(1, std::sync::atomic::Ordering::SeqCst);
        owner.exit_root();
        assert_eq!(snapshot().session.status, SessionStatus::Error);
        assert!(!owner.tree.has_exited().unwrap());
    }
    let producer = owner.peers.iter().find(|peer| peer.description["role"] == "engram-mcp").unwrap();
    if !failed_retry {
        (&producer.socket).write_all(&[3]).unwrap();
        phase_sync::receive(&frame_rx, "healthy actual reader applies task notice");
        phase_sync::receive(&frame_rx, "healthy actual reader applies assistant output");
        assert_eq!(snapshot().session.status, SessionStatus::Active);
    }
    let (claimed_tx, claimed_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *owner.tree.cleanup_gate.lock().unwrap() = Some(TestStopFenceGate { claimed_tx, release_rx });
    // This receipt and the post-frame receipt share a channel. Either the
    // original dropped finish or a corrected off-lock join releases the test
    // barrier; the test never requires a blocked reader to finish first.
    *owner.tree.cleanup_join_receipt.lock().unwrap() = Some(frame_tx);
    let retry = if failed_retry {
        owner.tree.termination_failures.store(1, std::sync::atomic::Ordering::SeqCst);
        let tree = owner.tree.clone();
        let process = owner.process.clone();
        Some(std::thread::spawn(move || tree.terminate(&process, "Claude")))
    } else {
        owner.release_root();
        None
    };
    phase_sync::receive(&claimed_rx, "actual cleanup attempt is gated InProgress");
    assert!(owner.tree.is_in_progress());
    assert!(state.inner.inner.try_lock().is_ok(), "cleanup and result waits stay off Inner");
    let after_output = if failed_retry {
        (&producer.socket).write_all(&[3]).unwrap();
        phase_sync::receive(&frame_rx, "retry reader observes task notice");
        phase_sync::receive(&frame_rx, "retry reader observes assistant output");
        Some(snapshot())
    } else { None };
    (&producer.socket).write_all(&[4]).unwrap();
    phase_sync::receive(&frame_rx, "success is either applied or joins the exact cleanup attempt");
    release_tx.send(()).unwrap();
    let retry_failed = retry.map(|worker| worker.join().unwrap().is_err());
    let stop_result = if failed_retry {
        Some(state.request_stop_session(&session_id))
    } else {
        owner.await_exit();
        None
    };
    let guard = phase_sync::PollGuard::new();
    while snapshot().runtime.runtime_token().is_some() {
        guard.wait("InProgress reader fixture disposes its exact runtime");
    }
    let final_record = snapshot();
    for peer in &owner.peers { peer.assert_exited(); }
    let final_status = final_record.session.status;
    let final_preview = final_record.session.preview.clone();
    let spurious_failure = final_record.session.messages.iter().any(|message|
        matches!(message, Message::Text { text, .. } if text.contains("exited before the active turn completed")));
    let retry_observation = after_output.as_ref().map(|record| (
        record.session.status, record.session.preview.clone(),
        record.unmediated_claude_turn.as_ref().and_then(|turn| turn.adopted_generation),
        record.runtime.matches_runtime_token(&token), record.orchestrator_auto_dispatch_blocked));
    drop(after_output);
    drop(final_record);
    drop(owner);
    drop(state);
    state_cleaned.recv_timeout(phase_sync::DEADLOCK_GUARD)
        .expect("InProgress reader workers release state owners")
        .expect("InProgress reader fixture state root removed");
    eprintln!("InProgress reader failed_retry={failed_retry}: final={final_status:?}, preview={final_preview:?}, spurious_failure={spurious_failure}");
    if failed_retry {
        let (status, preview, adopted, exact_token, held) = retry_observation.unwrap();
        assert_eq!(status, SessionStatus::Error, "a failed-owner retry cannot admit late adoption");
        assert!(preview.contains("process tree cleanup failed"));
        assert!(adopted.is_none() && exact_token && held);
        assert_eq!(retry_failed, Some(true));
        assert!(stop_result.unwrap().is_ok(), "public Stop retries failed exact ownership");
    } else {
        assert_eq!(final_status, SessionStatus::Idle, "a success result inside normal cleanup must finish normally");
        assert!(!spurious_failure, "confirmed cleanup cannot replace a completed result with failure");
    }
}

#[test]
fn delegation_process_cleanup_round5_in_progress_success() {
    production_in_progress_reader_case(false);
}
#[test]
fn delegation_process_cleanup_round5_in_progress_failed_retry_refuses_adoption() {
    production_in_progress_reader_case(true);
}
#[test]
fn delegation_process_cleanup_round5_in_progress_live_control() {
    production_late_reader_case(None);
}

fn dedicated_reset_off_lock_case(change_context: bool) {
    let temp = TestTempRoot::create("production-reset-off-lock");
    let mut state = test_app_state();
    state.agent_runtime_spawning_enabled = true;
    let session_id = test_session_id(&state, Agent::Claude);
    let owner = ProductionWorkerTree::spawn(&state, &session_id, temp.path(), false);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session_id).unwrap();
        let record = &mut inner.sessions[index];
        record.session.status = SessionStatus::Idle;
        record.runtime_reset_required = true;
        record.set_auto_dispatch_blocked(true);
        record.queued_prompts.push_back(QueuedPromptRecord {
            engram_waiting: false, promoted_message_index: None, promotion_disposition_known: true,
            engram_bind: None, engram_evaluate: None, engram_interrupted: false,
            source: QueuedPromptSource::User, attachments: Vec::new(),
            pending_prompt: PendingPrompt { engram_interrupted: false, is_engram_retained: false,
                attachments: Vec::new(), id: "reset-queued-head".to_owned(), timestamp: stamp_now(),
                text: "preserve exact head".to_owned(), expanded_text: None, source: None },
        });
    }
    let (claimed_tx, claimed_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *owner.tree.cleanup_gate.lock().unwrap() = Some(TestStopFenceGate { claimed_tx, release_rx });
    let reset_state = state.clone();
    let reset_id = session_id.clone();
    let reset = std::thread::spawn(move || reset_state.prepare_dedicated_runtime_reset_off_lock(&reset_id, false));
    phase_sync::receive(&claimed_rx, "reset owns its fence and reaches off-lock OS attempt");
    assert!(owner.tree.cleanup.try_lock().is_ok());
    let mut inner = state.inner.inner.try_lock().expect("reset cannot wait under the global state mutex");
    let index = inner.find_session_index(&session_id).unwrap();
    let record = &mut inner.sessions[index];
    assert!(record.runtime_stop_in_progress);
    assert!(record.runtime_reset_required);
    assert_eq!(record.queued_prompts.front().unwrap().pending_prompt.id, "reset-queued-head");
    if change_context { record.session.model = "replacement configuration".to_owned(); }
    drop(inner);
    release_tx.send(()).unwrap();
    let result = reset.join().unwrap();
    if change_context { assert!(result.is_err()); } else { result.unwrap(); }
    owner.await_exit();
    let record = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session_id).unwrap()].clone()
    };
    assert_eq!(record.queued_prompts.len(), 1, "cleanup does not consume an admission or queue head");
    assert_eq!(record.queued_prompts.front().unwrap().pending_prompt.id, "reset-queued-head");
    assert!(!record.runtime_stop_in_progress);
    assert!(owner.tree.is_confirmed());
    if change_context {
        assert_eq!(record.session.model, "replacement configuration");
        assert!(record.runtime_reset_required, "stale cleanup cannot clear a new reset request");
        assert!(record.orchestrator_auto_dispatch_blocked);
        state.prepare_dedicated_runtime_reset_off_lock(&session_id, false).unwrap();
    } else {
        assert!(matches!(record.runtime, SessionRuntime::None));
        assert!(!record.runtime_reset_required);
    }
}

#[test]
fn delegation_process_cleanup_reset_wait_is_off_lock() { dedicated_reset_off_lock_case(false); }
#[test]
fn delegation_process_cleanup_reset_stale_context_keeps_queue_and_reset() { dedicated_reset_off_lock_case(true); }

#[test]
fn delegation_process_cleanup_raw_launch_entry_stays_visible_off_lock() {
    use std::os::windows::io::AsHandle;
    let temp = TestTempRoot::create("raw-launch-visible-owner");
    let _scope = LaunchFailureScope::new();
    let state = test_app_state();
    let mut command = Command::new(PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe"));
    command.args(["-NoProfile", "-NonInteractive", "-Command", "[Console]::In.ReadLine() | Out-Null"])
        .current_dir(temp.path()).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null())
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    let mut child = command.spawn().unwrap();
    let handle = child.as_handle().try_clone_to_owned().unwrap();
    assert!(child.try_wait().unwrap().is_none());
    let registry = failed_runtime_launches();
    let entry = Arc::new(FailedRuntimeLaunchEntry {
        owner: Mutex::new(Some(FailedRuntimeLaunch::Raw(child))),
        phase: Mutex::new(FailedLaunchPhase::Ready),
    });
    registry.lock().unwrap().push(entry.clone());
    let held_owner = entry.owner.lock().unwrap();
    schedule_runtime_launch_cleanup(&registry, &entry);
    assert!(matches!(*entry.phase.lock().unwrap(), FailedLaunchPhase::InProgress(_)));
    assert_eq!(registry.lock().unwrap().len(), 1, "in-flight ownership cannot masquerade as an empty registry");
    assert!(state.inner.inner.try_lock().is_ok());
    assert!(retry_failed_runtime_launch_cleanup().is_err(), "admission refuses while the exact owner is in flight");
    assert_eq!(registry.lock().unwrap().len(), 1);
    drop(held_owner);
    await_launch_cleanup_attempts(&registry);
    assert!(registry.lock().unwrap().is_empty());
    assert_launch_handle_exited(&handle);
}
