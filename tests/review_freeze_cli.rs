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
fn compiled_freeze_mode_resolves_relative_manifest_in_linked_worktree() {
    use std::{io::Write, process::Stdio};

    let fixture = temp::Fixture::new();
    let main = fixture.0.join("main");
    let linked = fixture.0.join("linked");
    fs::create_dir(&main).unwrap();
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let git = |args: &[&str], input: Option<&[u8]>| {
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
        command
            .current_dir(&main)
            .env("GIT_CONFIG_GLOBAL", null)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &fixture.0)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if input.is_some() {
            command.stdin(Stdio::piped());
        }
        let mut child = command.spawn().unwrap();
        if let Some(bytes) = input {
            child.stdin.take().unwrap().write_all(bytes).unwrap();
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    git(
        &[
            "-c",
            &format!("init.templateDir={null}"),
            "init",
            "--object-format=sha1",
        ],
        None,
    );
    assert_eq!(
        git(&["hash-object", "-t", "tree", "-w", "--stdin"], Some(b"")),
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    );
    // Fixed empty commit and independently framed schema-1 golden: neither is
    // captured with the checker or a repository helper at test runtime.
    let commit = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor Freeze Tester <freeze@example.test> 0 +0000\ncommitter Freeze Tester <freeze@example.test> 0 +0000\n\nempty fixture\n";
    let head = git(
        &["hash-object", "-t", "commit", "-w", "--stdin"],
        Some(commit),
    );
    assert_eq!(head, "2a5874a2844dd641b7a9f9005b936d5d9df4acca");
    git(&["update-ref", "HEAD", &head], None);
    git(
        &["worktree", "add", "--detach", &linked.to_string_lossy()],
        None,
    );
    assert!(linked.join(".git").is_file());
    let root = fs::canonicalize(&linked).unwrap();
    let own_git_dir = git(
        &[
            "-C",
            &linked.to_string_lossy(),
            "rev-parse",
            "--absolute-git-dir",
        ],
        None,
    );
    let manifest = std::path::Path::new(&own_git_dir).join("engram-review-freeze.json");
    let fingerprint = "932b1020a412524292fd9f8a1868ab36cb84fc581025aa4d335b2330292edd8c";
    fs::write(
        &manifest,
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 1, "root": root, "fingerprint": fingerprint
        }))
        .unwrap(),
    )
    .unwrap();
    for path in [
        manifest.to_string_lossy().into_owned(),
        ".git/engram-review-freeze.json".to_owned(),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_termal"))
            .args([
                "review-freeze-check",
                &root.to_string_lossy(),
                &path,
                fingerprint,
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{path}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, format!("{fingerprint}\n").as_bytes());
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
