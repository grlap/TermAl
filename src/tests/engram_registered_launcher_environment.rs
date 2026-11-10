// Platform-specific real check and isolated environment for the disposable
// launcher fixture. Windows exercises Node; non-Windows retains Cargo coverage.
use super::*;

const NODE_TEST: &str = "launcher-check.test.mjs";
pub(super) const LAUNCH: &str = if cfg!(windows) {
    "node scripts/test-launcher.mjs focused --input commit -- node --test launcher-check.test.mjs"
} else {
    "node scripts/test-launcher.mjs focused --input commit -- cargo test --offline"
};

pub(super) fn install_launcher(fixture: &CompletionFixture) {
    let candidate = FsPath::new(env!("CARGO_MANIFEST_DIR"));
    // Copy the tracked tree so relative imports and child entry points use the
    // candidate's actual launcher, without maintaining a dependency list here.
    let paths = run_git_test_command_output(candidate, &["ls-files", "-z", "--", "scripts"]);
    assert!(
        paths
            .split('\0')
            .any(|path| path == "scripts/test-launcher.mjs"),
        "candidate setup: tracked production launcher is missing"
    );
    for relative in paths.split('\0').filter(|path| !path.is_empty()) {
        let source = candidate.join(relative);
        let destination = fixture.root.join(relative);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(&source, &destination).unwrap_or_else(|error| {
            panic!(
                "candidate launcher setup: cannot copy {}: {error}",
                source.display()
            )
        });
        assert_eq!(
            fs::read(&source).unwrap(),
            fs::read(&destination).unwrap(),
            "candidate launcher setup: copied bytes differ for {relative}"
        );
    }
    run_git_test_command(&fixture.root, &["add", "--", "scripts"]);
    if cfg!(windows) {
        // Real Node builtin test output is parsed by the maintained launcher.
        // It reads tracked source, and does not manufacture result artifacts.
        fs::write(
            fixture.root.join(NODE_TEST),
            r#"import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
test("checked content", () => {
  assert.match(readFileSync(new URL("./README.md", import.meta.url), "utf8"), /\S/);
});
"#,
        )
        .unwrap();
        run_git_test_command(&fixture.root, &["add", "--", NODE_TEST]);
    }
    run_git_test_command(
        &fixture.root,
        &["commit", "--quiet", "-m", "fixture production launcher"],
    );
    assert!(run_git_test_command_output(&fixture.root, &["status", "--porcelain"]).is_empty());
}

pub(super) fn isolated_launcher(
    root: &FsPath,
    session: &str,
    service: &LoopbackRegistration,
) -> Command {
    isolated_launcher_for(
        root,
        session,
        service,
        &if cfg!(windows) {
            ["node", "--test", NODE_TEST]
        } else {
            ["cargo", "test", "--offline"]
        },
    )
}

pub(super) fn isolated_launcher_for(
    root: &FsPath,
    session: &str,
    service: &LoopbackRegistration,
    native_command: &[&str],
) -> Command {
    isolated_launcher_in_mode(root, session, service, "focused", native_command)
}

pub(super) fn isolated_full_launcher(
    root: &FsPath,
    session: &str,
    service: &LoopbackRegistration,
) -> Command {
    isolated_launcher_in_mode(root, session, service, "full", &[])
}

fn isolated_launcher_in_mode(
    root: &FsPath,
    session: &str,
    service: &LoopbackRegistration,
    mode: &str,
    native_command: &[&str],
) -> Command {
    let mut command = Command::new(if cfg!(windows) { "node.exe" } else { "node" });
    // Construct a child environment, including only this fixture's identity.
    // Outer outputs, notifications, wrappers and failure injection stay out.
    command.env_clear();
    for key in [
        "PATH",
        "HOME",
        "USERPROFILE",
        "HOMEDRIVE",
        "HOMEPATH",
        "SystemRoot",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    if !cfg!(windows) {
        for key in ["CARGO_HOME", "RUSTUP_HOME"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .env("CARGO_TARGET_DIR", root.join("target"))
            .env("CARGO_BUILD_JOBS", "1");
    }
    // The owner may stage EOL configuration, which must be outside the
    // inspected checkout. The disposable host owns this sibling scratch,
    // including when the launcher measures a linked worktree.
    let temporary = &service.temporary;
    fs::create_dir_all(temporary).unwrap();
    assert!(
        !fs::canonicalize(temporary)
            .unwrap()
            .starts_with(fs::canonicalize(root).unwrap()),
        "owner scratch stays outside the measured checkout"
    );
    command
        .env("TMPDIR", temporary)
        .env("TMP", temporary)
        .env("TEMP", temporary)
        .env("TERMAL_SESSION_ID", session)
        .env("TERMAL_BASE_URL", &service.url)
        .current_dir(root)
        .args(["scripts/test-launcher.mjs", mode, "--input", "commit"]);
    if mode == "focused" {
        command.arg("--").args(native_command);
    }
    let retained = command
        .get_envs()
        .filter_map(|(key, value)| {
            value.map(|value| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(retained["TERMAL_SESSION_ID"], session);
    assert_eq!(retained["TERMAL_BASE_URL"], service.url);
    for key in retained.keys() {
        assert!(
            !key.starts_with("TERMAL_")
                || matches!(key.as_str(), "TERMAL_SESSION_ID" | "TERMAL_BASE_URL")
        );
        assert!(
            !key.starts_with("RUST_TEST") && !key.starts_with("TEST_"),
            "outer environment leaked: {key}"
        );
    }
    if cfg!(windows) {
        assert!(
            !retained
                .keys()
                .any(|key| key.starts_with("CARGO") || key.starts_with("RUSTUP"))
        );
    } else {
        assert_eq!(
            retained["CARGO_TARGET_DIR"],
            root.join("target").to_string_lossy()
        );
    }
    command
}
