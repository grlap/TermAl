// Owns focused-launcher feedback distinguishing non-test commands from
// unsupported test forms, including runtime-wrapper display. Does not own
// recognition or credit; reuses engram_focused_node_checks.rs's test context.
use super::*;

fn envelopes(launcher: &str) -> Vec<String> {
    vec![
        launcher.to_owned(),
        format!("pwsh -Command '{launcher}'"),
        format!("bash -lc '{launcher}'"),
        format!("cmd /c {launcher}"),
        format!("pushd \"{}\" && {launcher}", env!("CARGO_MANIFEST_DIR")),
        format!(
            "\"C:\\Program Files\\WindowsApps\\Microsoft.PowerShell_7.6.6.0_x64__8wekyb3d8bbwe\\pwsh.exe\" -Command '{launcher}'"
        ),
    ]
}

#[test]
fn focused_launcher_non_test_notices_distinguish_lint_prebuild_and_script_from_tests() {
    for inner in [
        "cargo clippy --all-targets --all-features -- -D warnings",
        "cargo test --bin termal --no-run",
        "node scripts/check-doc-links.mjs",
        "git diff --no-index --exit-code AGENTS.md CLAUDE.md",
    ] {
        let launcher = format!("node scripts/test-launcher.mjs focused -- {inner}");
        let expected = engram_unsupported_focused_line(&launcher).unwrap();
        assert!(expected.contains(inner), "{expected}");
        assert!(expected.contains(&launcher), "{expected}");
        assert!(
            expected.contains("not a recognized test runner"),
            "{expected}"
        );
        assert!(expected.contains("no Test record"), "{expected}");
        assert!(!expected.contains("Unsupported"), "{expected}");
        assert!(
            !expected.contains("supported test runner form"),
            "{expected}"
        );
        assert!(
            !expected.contains("no record"),
            "typed non-Test records remain possible"
        );
        for command in envelopes(&launcher) {
            assert!(engram_check_command(&command).is_none(), "{command}");
            assert_eq!(
                engram_unsupported_focused_line(&command),
                Some(expected.clone()),
                "{command}"
            );
            assert!(
                engram_check_command(&command).is_none(),
                "feedback never grants credit"
            );
        }
    }
}

#[test]
fn focused_launcher_unsupported_test_form_gets_distinct_unwrapped_guidance() {
    // Native Node runs tests, but this reporter is outside the admitted
    // foreground grammar. It must not be labelled as a non-test script.
    let launcher = "node scripts/test-launcher.mjs focused -- node --test --test-reporter=dot fixture.test.mjs";
    let expected = engram_unsupported_focused_line(launcher).unwrap();
    assert!(expected.contains("Unsupported"), "{expected}");
    assert!(
        expected.contains("supported test runner form"),
        "{expected}"
    );
    assert!(expected.contains("no Test record"), "{expected}");
    assert!(
        !expected.contains("not a recognized test runner"),
        "{expected}"
    );
    for command in envelopes(launcher) {
        assert!(engram_check_command(&command).is_none(), "{command}");
        assert_eq!(
            engram_unsupported_focused_line(&command),
            Some(expected.clone()),
            "{command}"
        );
        assert!(
            engram_check_command(&command).is_none(),
            "feedback never grants credit"
        );
    }
}

#[test]
fn focused_launcher_notices_preserve_quoted_inner_arguments() {
    let launcher = "node scripts/test-launcher.mjs focused -- node \"scripts/check doc links.mjs\"";
    let notice = engram_unsupported_focused_line(launcher).unwrap();
    let escaped = format!("{launcher:?}");
    assert!(notice.contains(&escaped), "{notice}");
    for command in envelopes(launcher) {
        assert_eq!(
            engram_unsupported_focused_line(&command),
            Some(notice.clone()),
            "{command}"
        );
    }
}
