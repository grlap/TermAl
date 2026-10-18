// Owns the external-command adapter and actual local platform-probe checks.
// Does not run Engram sync or install tools. New O1 tests use fixed stub commands
// and bounded child ownership, with every cwd inside the named worktree.
use super::*;

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
    let result = run_federation_engram_probe(&command("exit 7", false));
    assert_eq!(result.status, FederationEngramProbeStatus::Fail);
    assert_eq!(result.exit_code, Some(7));
    let result = run_federation_engram_probe(&command("exit 0", false));
    assert_eq!(result.status, FederationEngramProbeStatus::Pass);
    assert_eq!(result.exit_code, Some(0));
}

#[test]
fn federation_onboarding_external_typed_summary_and_unavailable_are_distinct() {
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
    assert_eq!(result.status, FederationEngramProbeStatus::Pass);
    assert_eq!(result.summary.unwrap().checks, vec!["binary", "git"]);
    let result = run_federation_engram_probe(&command("exit 0", true));
    assert_eq!(result.status, FederationEngramProbeStatus::Fail);
    let mut missing = command("exit 0", false);
    missing.binary = std::env::current_dir()
        .unwrap()
        .join(".tmp")
        .join("absent-engram-verifier");
    assert_eq!(
        run_federation_engram_probe(&missing).status,
        FederationEngramProbeStatus::Unavailable
    );
}

#[test]
fn federation_onboarding_local_probe_resolves_paths_and_reaps_fixed_child() {
    let probe = federation_local_platform_probe(&std::env::current_dir().unwrap()).unwrap();
    assert!(probe.paths_resolved && probe.child_spawned_and_reaped);
    assert_eq!(probe.platform, std::env::consts::OS);
    assert_eq!(probe.arch, std::env::consts::ARCH);
    assert!(probe.toolchain_identity.starts_with("rustc "));
}
