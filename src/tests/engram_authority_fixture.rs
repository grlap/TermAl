//! Native authority-command fixture for Stop-ordering tests. Real PowerShell
//! and shell CLI conformance remains in the Engram host-adapter fixture tests.

use super::*;

/// Re-exec the test binary through the normal Engram command/process-tree path.
/// Only this child gets the marker; the parent test environment is untouched.
pub(super) fn binary_path(root: &FsPath) -> PathBuf {
    let executable = std::env::current_exe().expect("current test executable");
    let filter = format!("{}::authority_command", module_path!());
    // module_path includes the crate name; libtest lists names relative to it.
    let filter = filter.split_once("::").unwrap().1;
    let output = root.join("native-authority-output");
    let log = root.join("native-authority-test.log");
    // Keep libtest's progress/summary out of the doctor's JSON protocol, while
    // retaining its diagnostics and exit status when a fixture assertion fails.
    // Each test owns one root; command kinds use separate output slots so an
    // advisory work-context read cannot overwrite the revocation response.
    #[cfg(windows)]
    let (path, script) = (
        root.join("native-authority.cmd"),
        format!(
            "@echo off\r\nset \"TERMAL_TEST_AUTHORITY_FIXTURE={}-%~5\"\r\ntype nul > \"{}-%~5\"\r\n\"{}\" --exact {filter} --nocapture -- %* > \"{}-%~5\"\r\nif errorlevel 1 (type \"{}-%~5\" 1>&2 & exit /b 1)\r\ntype \"{}-%~5\"\r\n",
            output.display(),
            output.display(),
            executable.display(),
            log.display(),
            log.display(),
            output.display()
        ),
    );
    #[cfg(not(windows))]
    let (path, script) = (
        root.join("native-authority.sh"),
        format!(
            "#!/bin/sh\nexport TERMAL_TEST_AUTHORITY_FIXTURE='{}'-\"$5\"\n: > '{}'-\"$5\"\nif '{}' --exact {filter} --nocapture -- \"$@\" > '{}'-\"$5\"; then\n  cat '{}'-\"$5\"\nelse\n  cat '{}'-\"$5\" >&2\n  exit 1\nfi\n",
            output.to_string_lossy().replace('\'', "'\\''"),
            output.to_string_lossy().replace('\'', "'\\''"),
            executable.to_string_lossy().replace('\'', "'\\''"),
            log.to_string_lossy().replace('\'', "'\\''"),
            output.to_string_lossy().replace('\'', "'\\''"),
            log.to_string_lossy().replace('\'', "'\\''")
        ),
    );
    fs::write(&path, script).expect("write native authority launcher");
    path
}

#[test]
fn authority_command() {
    let Some(output) = std::env::var_os("TERMAL_TEST_AUTHORITY_FIXTURE") else {
        return;
    };
    let args: Vec<String> = std::env::args()
        .skip_while(|arg| arg != "--")
        .skip(1)
        .collect();
    assert!(args.len() >= 6, "Engram fixture argv: {args:?}");
    assert_eq!(args[0], "--project-file");
    assert_eq!(args[2], "--home");
    let root = FsPath::new(&args[3]);
    assert_eq!(FsPath::new(&args[1]), root.join(".engram-project"));
    let mode = fs::read_to_string(&args[1]).expect("fixture project declaration");
    assert_eq!(mode.trim(), "fixture-ready");
    if args[4..] == ["doctor", "--json"] {
        let response = serde_json::json!({
            "healthy": true,
            "control": { "required_assurance": "turn_gated" },
            "database": root.join("fixture-engram.db"),
            "project_id": mode.trim(),
        });
        fs::write(output, serde_json::to_vec(&response).unwrap()).unwrap();
        return;
    }
    if args[4] == "work" {
        assert_eq!(args.len(), 14, "work-context argv: {args:?}");
        assert_eq!(args[5], "--actor-id");
        assert_eq!(args[7], "--session-id");
        assert_eq!(args[9], "--actor-context");
        assert_eq!(&args[11..13], ["next", "--context-generation"]);
        for (name, value) in [
            ("ENGRAM_HOME", &args[3]),
            ("ENGRAM_ACTOR_ID", &args[6]),
            ("ENGRAM_SESSION_ID", &args[8]),
            ("ENGRAM_ACTOR_CONTEXT", &args[10]),
        ] {
            assert_eq!(std::env::var(name).as_ref(), Ok(value));
        }
        fs::write(
            output,
            format!("Engram work context for {} as {}\n", args[8], args[6]),
        )
        .unwrap();
        return;
    }
    assert_eq!(args.len(), 12, "authority command argv: {args:?}");
    assert_eq!(
        &args[4..],
        [
            "authority",
            "revoke",
            "--revoked-by",
            "termal:host",
            "--reason",
            "TermAl project Engram work-authority grant removed",
            "--",
            "grant-old",
        ]
    );
    #[cfg(windows)]
    fs::write(
        root.join("engram-authority-revoke-args.json"),
        serde_json::to_vec(&args).unwrap(),
    )
    .expect("journal exact authority argv");
    #[cfg(not(windows))]
    fs::write(
        root.join("engram-authority-revoke-args.txt"),
        args.join(" "),
    )
    .expect("journal authority argv");
    fs::write(output, "fixture-revocation-hash\n").unwrap();
}
