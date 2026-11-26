//! Compiled command dispatch for the retired schema-1 review checker.
use std::process::Command;

#[test]
fn old_review_check_command_is_rejected_without_a_fingerprint() {
    let output = Command::new(env!("CARGO_BIN_EXE_termal"))
        .args([
            "review-freeze-check",
            "root",
            "manifest.json",
            &"a".repeat(64),
        ])
        .output()
        .expect("run compiled TermAl");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        stderr.contains("unknown argument") && stderr.contains("review-freeze-check"),
        "{stderr}"
    );
}
