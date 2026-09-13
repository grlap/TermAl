//! Exercises the actual Cargo-built executable, not the unit-test harness or
//! repository JavaScript. Only a private empty Git fixture is read by the CLI.
use std::{fs, process::Command};

#[allow(dead_code)]
mod temp {
    use std::{
        fs, io,
        path::{Path as FsPath, PathBuf},
        time::Duration,
    };
    include!("../src/test_temp_paths.rs");
    pub(super) struct Fixture(pub PathBuf);
    impl Fixture {
        pub(super) fn new() -> Self {
            let path = test_temp_dir().join(format!("freeze-cli-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            remove_test_directory(&self.0);
        }
    }
}

#[test]
fn compiled_freeze_mode_reports_exact_stdout_and_fails_closed() {
    let fixture = temp::Fixture::new();
    let root = fs::canonicalize(&fixture.0).unwrap();
    let mut init_command = Command::new("git");
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    for (name, _) in std::env::vars_os() {
        if name
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            init_command.env_remove(name);
        }
    }
    let init = init_command
        .env("GIT_CONFIG_GLOBAL", null)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            &format!("init.templateDir={null}"),
            "init",
            "--object-format=sha1",
        ])
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    // Independent schema-1 empty/unborn golden, not captured with the checker.
    let fingerprint = "3d168792d0dc7091e70b8c56fb55423b9bd7ebbf5b44a83d851662c6e26a2c53";
    fs::write(
        root.join(".git/freeze.json"),
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 1, "root": root, "fingerprint": fingerprint
        }))
        .unwrap(),
    )
    .unwrap();
    let xdg = root.join(".git/host-xdg");
    fs::create_dir_all(xdg.join("git")).unwrap();
    fs::write(xdg.join("git/ignore"), "*.txt\n").unwrap();
    fs::write(xdg.join("git/attributes"), "*.txt -diff\n").unwrap();
    let run = |expected: &str| {
        Command::new(env!("CARGO_BIN_EXE_termal"))
            .env("XDG_CONFIG_HOME", &xdg)
            .arg("review-freeze-check")
            .arg(&root)
            .arg(".git/freeze.json")
            .arg(expected)
            .output()
            .unwrap()
    };
    let success = run(fingerprint);
    assert!(
        success.status.success(),
        "{}",
        String::from_utf8_lossy(&success.stderr)
    );
    assert_eq!(success.stdout, format!("{fingerprint}\n").as_bytes());
    #[cfg(windows)]
    assert!(String::from_utf8_lossy(&success.stderr).contains("unverified on Windows"));
    let mismatch = run(&"a".repeat(64));
    assert!(!mismatch.status.success());
    assert!(mismatch.stdout.is_empty());
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("independent fingerprint mismatch"));
    fs::write(root.join("drift.txt"), "new input").unwrap();
    let drift = run(fingerprint);
    // The real CLI must see this untracked file despite default XDG ignores.
    assert!(!drift.status.success());
    assert!(drift.stdout.is_empty());
    assert!(String::from_utf8_lossy(&drift.stderr).contains("drifted"));
    let arity = Command::new(env!("CARGO_BIN_EXE_termal"))
        .arg("review-freeze-check")
        .output()
        .unwrap();
    assert!(!arity.status.success());
    assert!(arity.stdout.is_empty());
    assert!(String::from_utf8_lossy(&arity.stderr).contains("usage:"));
}
