// Owns the external-command adapter and actual local platform-probe checks.
// Does not run Engram sync or install tools. New O1 tests use fixed stub commands
// and bounded child ownership, with every cwd inside the named worktree.
use super::*;

// Fixed-command success checks need their own inheritance mutex. The parent
// suite also creates thousands of unrelated children; that queue must not
// decide whether this fixture's real two-second probe succeeds.
fn external_probe_subprocess(name: &str) -> bool {
    const CHILD_SELECTOR: &str = "TERMAL_TEST_EXTERNAL_PROBE_CHILD";
    let test = format!("{}::{name}", module_path!().split_once("::").unwrap().1);
    if let Some(selected) = std::env::var_os(CHILD_SELECTOR) {
        assert_eq!(
            selected.to_str(),
            Some(test.as_str()),
            "unexpected probe child selector"
        );
        return false;
    }
    let mut child = BoundedReadCommand::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            &test,
            "--nocapture",
            "--test-threads=1",
            "--color=never",
            "--format=pretty",
        ])
        .env(CHILD_SELECTOR, &test)
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    // This is the existing fixture deadlock guard around the test process,
    // not an extension or a reset of any probe's two-second allowance.
    let output = run_bounded_read_command(
        &mut child,
        std::time::Instant::now() + TEST_PHASE_DEADLOCK_GUARD,
        64 * 1024,
        true,
    )
    .unwrap_or_else(|error| panic!("{test} child failed: {error:#}"));
    let stdout = String::from_utf8(output.stdout).expect("probe child stdout is UTF-8");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{test}: stdout={stdout}; stderr={stderr}"
    );
    assert_eq!(
        stdout
            .lines()
            .filter(|line| *line == "running 1 test")
            .count(),
        1,
        "{test}: stdout={stdout}; stderr={stderr}",
    );
    let expected = format!("test {test} ... ok");
    let executed: Vec<_> = stdout
        .lines()
        .filter(|line| line.starts_with("test ") && line.contains(" ... "))
        .collect();
    assert_eq!(
        executed,
        vec![expected.as_str()],
        "{test}: stdout={stdout}; stderr={stderr}"
    );
    let summaries: Vec<_> = stdout
        .lines()
        .filter(|line| line.starts_with("test result:"))
        .collect();
    assert_eq!(
        summaries.len(),
        1,
        "{test}: stdout={stdout}; stderr={stderr}"
    );
    assert!(
        summaries[0].starts_with("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;"),
        "{test}: stdout={stdout}; stderr={stderr}",
    );
    true
}

fn command(script: &str, json_summary: bool) -> FederationEngramProbeCommand {
    #[cfg(windows)]
    let (binary, args) = (
        PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32")
            .join("cmd.exe"),
        vec![
            "/d".to_owned(),
            "/s".to_owned(),
            "/c".to_owned(),
            script.to_owned(),
        ],
    );
    #[cfg(not(windows))]
    let (binary, args) = (
        PathBuf::from("/bin/sh"),
        vec!["-c".to_owned(), script.to_owned()],
    );
    FederationEngramProbeCommand {
        binary,
        args,
        project_root: std::env::current_dir().unwrap(),
        json_summary,
    }
}

#[test]
fn federation_onboarding_external_stub_exit_status_is_separate_from_termal_readiness() {
    if external_probe_subprocess(
        "federation_onboarding_external_stub_exit_status_is_separate_from_termal_readiness",
    ) {
        return;
    }
    let result = run_federation_engram_probe(&command("exit 7", false));
    assert_eq!(
        result.status,
        FederationEngramProbeStatus::Fail,
        "{result:?}"
    );
    assert_eq!(result.exit_code, Some(7));
    let result = run_federation_engram_probe(&command("exit 0", false));
    assert_eq!(
        result.status,
        FederationEngramProbeStatus::Pass,
        "{result:?}"
    );
    assert_eq!(result.exit_code, Some(0));
}

#[test]
fn federation_onboarding_external_typed_summary_and_unavailable_are_distinct() {
    if external_probe_subprocess(
        "federation_onboarding_external_typed_summary_and_unavailable_are_distinct",
    ) {
        return;
    }
    let _scope = TestTempRootDirectoryScope::repository_local();
    let directory = TestTempRoot::create("onboarding-external");
    let summary_path = directory.path().join("summary.json");
    fs::write(
        &summary_path,
        br#"{"passed":true,"checks":["binary","git"]}"#,
    )
    .unwrap();
    let mut config = command("exit 0", true);
    #[cfg(windows)]
    {
        config.args = vec![
            "/d".to_owned(),
            "/c".to_owned(),
            "type".to_owned(),
            summary_path.to_str().unwrap().to_owned(),
        ];
    }
    #[cfg(not(windows))]
    {
        config.binary = PathBuf::from("/bin/cat");
        config.args = vec![summary_path.to_str().unwrap().to_owned()];
    }
    config.project_root = directory.path().to_path_buf();
    let result = run_federation_engram_probe(&config);
    assert_eq!(
        result.status,
        FederationEngramProbeStatus::Pass,
        "{result:?}"
    );
    assert_eq!(result.summary.unwrap().checks, vec!["binary", "git"]);
    let result = run_federation_engram_probe(&command("exit 0", true));
    assert_eq!(
        result.status,
        FederationEngramProbeStatus::Fail,
        "{result:?}"
    );
    let mut missing = command("exit 0", false);
    missing.binary = std::env::current_dir()
        .unwrap()
        .join(".tmp")
        .join("absent-engram-verifier");
    let result = run_federation_engram_probe(&missing);
    assert_eq!(
        result.status,
        FederationEngramProbeStatus::Unavailable,
        "{result:?}"
    );
    for field in [
        "stage=spawn",
        "kind=NotFound",
        "elapsed_ms=",
        "lock_wait_ms=",
    ] {
        assert!(result.reason.contains(field), "{result:?}");
    }
    let error =
        federation_probe_command(missing.binary.to_str().unwrap(), &[], &missing.project_root)
            .unwrap_err();
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    for field in [
        "stage=spawn",
        "kind=NotFound",
        "elapsed_ms=",
        "lock_wait_ms=",
    ] {
        assert!(error.message.contains(field), "{error:?}");
    }
}

#[test]
fn federation_onboarding_local_probe_resolves_paths_and_reaps_fixed_child() {
    if external_probe_subprocess(
        "federation_onboarding_local_probe_resolves_paths_and_reaps_fixed_child",
    ) {
        return;
    }
    let probe = federation_local_platform_probe(&std::env::current_dir().unwrap())
        .unwrap_or_else(|error| panic!("platform probe failed: {error:?}"));
    assert!(probe.paths_resolved && probe.child_spawned_and_reaped);
    assert_eq!(probe.platform, std::env::consts::OS);
    assert_eq!(probe.arch, std::env::consts::ARCH);
}

#[test]
fn federation_onboarding_platform_probe_keeps_optional_toolchain_observations() {
    let directory = fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    // A compiler is optional on an installed host. Exercise both fallback
    // causes without depending on PATH or a live rustup proxy's response time.
    for observed in [Some("rustc fixture-version"), Some(""), None] {
        let calls = std::cell::Cell::new(0);
        let probe = federation_platform_probe_with(&directory, |program, args, cwd| {
            assert_eq!(cwd, directory.as_path());
            let call = calls.get();
            calls.set(call + 1);
            match call {
                0 => {
                    #[cfg(windows)]
                    {
                        assert_eq!(program, "cmd.exe");
                        assert_eq!(args, &["/d", "/s", "/c", "echo TERMAL_PROBE_WAKE"]);
                    }
                    #[cfg(not(windows))]
                    {
                        assert_eq!(program, "/bin/sh");
                        assert_eq!(args, &["-c", "printf TERMAL_PROBE_WAKE"]);
                    }
                    Ok("TERMAL_PROBE_WAKE".to_owned())
                }
                1 => {
                    assert_eq!(program, "rustc");
                    assert_eq!(args, &["--version"]);
                    observed.map(str::to_owned).ok_or_else(|| {
                        ApiError::from_status(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "fixture probe unavailable",
                        )
                    })
                }
                _ => panic!("unexpected additional platform probe"),
            }
        })
        .unwrap();
        assert_eq!(calls.get(), 2);
        assert!(probe.paths_resolved && probe.child_spawned_and_reaped);
        assert_eq!(
            probe.toolchain_identity,
            observed
                .filter(|text| !text.is_empty())
                .unwrap_or("unavailable")
        );
    }
}
