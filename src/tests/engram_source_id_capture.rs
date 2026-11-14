use super::engram_source_command::source_envelope;
use super::engram_source_owner::{
    source_owner_fixture_binary, source_owner_output, source_owner_reply,
};
use super::*;

fn pinned_fixture(root: &FsPath) -> (PathBuf, EngramSourceOwnerPin) {
    let binary = source_owner_fixture_binary(root);
    let pin = engram_source_owner_metadata_with(
        &binary,
        root,
        root,
        std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET,
        |_, _, args, _, _| Ok(source_owner_reply(args)),
    )
    .unwrap();
    (binary, pin)
}

const SOURCE_SLOW_WARNING: &str = "warning: source observation is still running after 10 seconds; attempt=41/0; elapsed_ms=10000\n";
const GIT_SLOW_WARNING: &str =
    "warning: local Git query is still running after 10 seconds; attempt=41/7; elapsed_ms=11023\n";
const SLOW_WARNING_STDERR_BOUND: usize = 256 * 1024;

fn warning_stderr_at_bound() -> String {
    let lines = SOURCE_SLOW_WARNING.repeat(SLOW_WARNING_STDERR_BOUND / SOURCE_SLOW_WARNING.len());
    lines.replacen(
        "attempt=41/",
        &format!(
            "attempt={}41/",
            "0".repeat(SLOW_WARNING_STDERR_BOUND - lines.len())
        ),
        1,
    )
}

#[test]
fn source_owner_capture_accepts_only_bounded_slow_warnings_without_changing_identity() {
    let root = TestTempRoot::create("source-owner-slow-warning");
    let (_, pin) = pinned_fixture(&root);
    let at_bound = warning_stderr_at_bound();
    assert_eq!(at_bound.len(), SLOW_WARNING_STDERR_BOUND);
    for status in ["clean", "not_clean"] {
        for warning in [
            SOURCE_SLOW_WARNING.to_owned(),
            format!("{SOURCE_SLOW_WARNING}{GIT_SLOW_WARNING}"),
            GIT_SLOW_WARNING.replace('\n', "\r\n"),
            SOURCE_SLOW_WARNING.repeat(878),
            SOURCE_SLOW_WARNING.replace("41/0; elapsed_ms=10000", "0/000; elapsed_ms=0"),
            at_bound.clone(),
        ] {
            let value = source_envelope(&root, status);
            let expected: EngramSourceId =
                serde_json::from_value(value["source_id"].clone()).unwrap();
            let mut calls = 0;
            let actual = engram_source_id_with(
                &pin,
                &root,
                std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET,
                |_, _, _, _, _| {
                    calls += 1;
                    let mut output = source_owner_output(0, serde_json::to_vec(&value).unwrap());
                    output.stderr = warning.as_bytes().to_vec();
                    Ok(output)
                },
            )
            .unwrap();
            assert_eq!(actual, expected);
            assert_eq!(calls, 1, "a warning must not retry the source operation");
        }
    }
}

#[test]
fn source_owner_capture_refuses_other_malformed_and_oversized_warning_stderr() {
    let root = TestTempRoot::create("source-owner-bad-warning");
    let (_, pin) = pinned_fixture(&root);
    let invalid = [
        b"private diagnostic\n".to_vec(),
        format!("{SOURCE_SLOW_WARNING}private diagnostic\n").into_bytes(),
        format!("private diagnostic\n{SOURCE_SLOW_WARNING}").into_bytes(),
        format!("{SOURCE_SLOW_WARNING}\n").into_bytes(),
        SOURCE_SLOW_WARNING
            .replace("warning:", "warning")
            .into_bytes(),
        SOURCE_SLOW_WARNING
            .replace("source observation", "untrusted operation")
            .into_bytes(),
        SOURCE_SLOW_WARNING
            .replace("10 seconds", "9 seconds")
            .into_bytes(),
        SOURCE_SLOW_WARNING.replace("41/0", "41/-1").into_bytes(),
        SOURCE_SLOW_WARNING
            .replace("elapsed_ms=10000", "elapsed_ms=1e4")
            .into_bytes(),
        SOURCE_SLOW_WARNING.trim_end().as_bytes().to_vec(),
        format!("\u{feff}{SOURCE_SLOW_WARNING}").into_bytes(),
        SOURCE_SLOW_WARNING.replace("41/0", "41/٠").into_bytes(),
        SOURCE_SLOW_WARNING.replace("41/0", "41/0\0").into_bytes(),
        SOURCE_SLOW_WARNING
            .replace('\n', "\u{1b}[0m\n")
            .into_bytes(),
        vec![0xff],
        warning_stderr_at_bound()
            .replacen("attempt=", "attempt=0", 1)
            .into_bytes(),
    ];
    for stderr in invalid {
        let result = engram_source_id_with(
            &pin,
            &root,
            std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET,
            |_, _, _, _, _| {
                let mut output = source_owner_output(
                    0,
                    serde_json::to_vec(&source_envelope(&root, "clean")).unwrap(),
                );
                output.stderr = stderr.clone();
                Ok(output)
            },
        );
        assert!(result.is_err(), "stderr must fail closed: {stderr:?}");
    }
}

#[test]
fn source_owner_schema_source_probe_accepts_slow_warning_but_version_probe_stays_strict() {
    let root = TestTempRoot::create("source-owner-probe-warning");
    let binary = source_owner_fixture_binary(&root);
    let value = source_envelope(&root, "clean");
    let expected: EngramSourceId = serde_json::from_value(value["source_id"].clone()).unwrap();
    for warn_version in [true, false] {
        let result = engram_source_owner_observe_with(
            &binary,
            &root,
            &root,
            std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET,
            |_, _, args, _, _| {
                let version = args == ["--version"];
                let mut output = if version {
                    source_owner_reply(args)
                } else {
                    source_owner_output(0, serde_json::to_vec(&value).unwrap())
                };
                if version == warn_version {
                    output.stderr = SOURCE_SLOW_WARNING.as_bytes().to_vec();
                }
                Ok(output)
            },
        );
        if warn_version {
            assert!(result.is_err(), "version stderr remains a refusal");
        } else {
            let (pin, observed) = result.unwrap();
            assert_eq!(pin.source_version, 1);
            assert_eq!(
                observed
                    .into_source_id(std::time::Instant::now() + Duration::from_secs(1))
                    .unwrap(),
                expected,
            );
        }
    }
}

#[test]
fn source_owner_warning_cannot_validate_an_inconsistent_source_envelope() {
    let root = TestTempRoot::create("source-owner-warning-envelope");
    let (_, pin) = pinned_fixture(&root);
    for case in ["json", "version", "identity", "root", "exit"] {
        let error = engram_source_id_with(
            &pin,
            &root,
            std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET,
            |_, _, _, _, _| {
                let mut value = source_envelope(&root, "clean");
                if case == "version" {
                    value["version"] = json!(2);
                }
                if case == "identity" {
                    value["source_id"]["tree"] = json!("f".repeat(40));
                }
                if case == "root" {
                    value["root"] = json!(root.join("missing-source-root"));
                }
                let mut output = source_owner_output(
                    if case == "exit" { 1 } else { 0 },
                    serde_json::to_vec(&value).unwrap(),
                );
                if case == "json" {
                    output.stdout = b"not JSON".to_vec();
                }
                output.stderr = SOURCE_SLOW_WARNING.as_bytes().to_vec();
                Ok(output)
            },
        )
        .unwrap_err();
        assert!(!error.to_string().is_empty(), "{case}");
    }
}

#[test]
fn source_owner_slow_warning_is_preserved_on_the_diagnostic_stream() {
    const CHILD: &str = "TERMAL_TEST_SOURCE_WARNING_DIAGNOSTIC";
    if std::env::var_os(CHILD).is_some() {
        let root = TestTempRoot::create("source-owner-warning-diagnostic");
        let (_, pin) = pinned_fixture(&root);
        let value = source_envelope(&root, "clean");
        let expected: EngramSourceId = serde_json::from_value(value["source_id"].clone()).unwrap();
        let actual = engram_source_id_with(
            &pin,
            &root,
            std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET,
            |_, _, _, _, _| {
                let mut output = source_owner_output(0, serde_json::to_vec(&value).unwrap());
                output.stderr = SOURCE_SLOW_WARNING.as_bytes().to_vec();
                Ok(output)
            },
        )
        .unwrap();
        assert_eq!(actual, expected);
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tests::engram_source_id_capture::source_owner_slow_warning_is_preserved_on_the_diagnostic_stream",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains(SOURCE_SLOW_WARNING.trim_end()),
        "warning was dropped: {stderr}"
    );
}

#[test]
fn source_owner_first_observation_supplies_pin_and_identity_without_a_second_source_read() {
    let root = TestTempRoot::create("source-id-first");
    let binary = source_owner_fixture_binary(&root);
    let first = source_envelope(&root, "clean");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut calls = Vec::new();
    let mut operation_deadline = None;
    let (pin, observation) = engram_source_owner_observe_with(
        &binary,
        &root,
        &root,
        deadline,
        |_, _, args, until, _| {
            assert!(
                until > deadline,
                "an admitted source operation keeps its minimum window"
            );
            if let Some(first_deadline) = operation_deadline {
                assert!(
                    until >= first_deadline,
                    "the source retains the probe's existing longer window"
                );
            } else {
                operation_deadline = Some(until);
            }
            calls.push(args[0].clone());
            Ok(if args[0] == "--version" {
                source_owner_reply(args)
            } else {
                source_owner_output(0, serde_json::to_vec(&first).unwrap())
            })
        },
    )
    .unwrap();
    let expected: EngramSourceId = serde_json::from_value(first["source_id"].clone()).unwrap();
    assert_eq!(observation.into_source_id(deadline).unwrap(), expected);
    assert_eq!(
        calls,
        ["--version", "source"],
        "the schema and identity use the same response"
    );
    assert_eq!(
        pin.source_version,
        first["version"].as_u64().unwrap() as u32
    );

    let later = source_envelope(&root, "not_clean");
    let later_expected: EngramSourceId =
        serde_json::from_value(later["source_id"].clone()).unwrap();
    assert_ne!(later_expected, expected);
    let later_deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut later_calls = 0;
    let actual = engram_source_id_with(&pin, &root, later_deadline, |_, _, args, until, _| {
        later_calls += 1;
        assert_eq!(args[0], "source");
        assert!(
            until > later_deadline,
            "a later admitted capture gets its operation window"
        );
        Ok(source_owner_output(0, serde_json::to_vec(&later).unwrap()))
    })
    .unwrap();
    assert_eq!(later_calls, 1);
    assert_eq!(
        actual, later_expected,
        "a later capture cannot reuse the first measurement"
    );
}

#[test]
fn source_owner_first_unavailable_observation_proves_schema_without_inventing_an_identity() {
    let root = TestTempRoot::create("source-id-first-unavailable");
    let binary = source_owner_fixture_binary(&root);
    let deadline = std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET;
    let (pin, observation) =
        engram_source_owner_observe_with(&binary, &root, &root, deadline, |_, _, args, _, _| {
            Ok(source_owner_reply(args))
        })
        .unwrap();
    assert_eq!(pin.source_version, 1);
    let error = observation.into_source_id(deadline).unwrap_err();
    assert!(error.to_string().contains("InputUnavailable"), "{error:#}");
}

#[test]
fn source_owner_capture_returns_complete_clean_and_dirty_objects_under_the_same_pin() {
    let root = TestTempRoot::create("source-id-capture");
    let (binary, pin) = pinned_fixture(&root);
    for status in ["clean", "not_clean"] {
        let value = source_envelope(&root, status);
        let expected: EngramSourceId = serde_json::from_value(value["source_id"].clone()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut calls = 0;
        let actual =
            engram_source_id_with(&pin, &root, deadline, |program, cwd, args, until, limit| {
                calls += 1;
                assert_eq!(program, fs::canonicalize(&binary).unwrap());
                assert_eq!(cwd, fs::canonicalize(&root).unwrap());
                assert!(
                    until > deadline,
                    "a short caller window cannot abort an admitted source call"
                );
                assert_eq!(limit, ENGRAM_SOURCE_JSON_LIMIT + 1);
                assert_eq!(args[0], "source");
                assert_eq!(args[1], "--root");
                assert_eq!(
                    args[2],
                    normalize_user_facing_path(&fs::canonicalize(&root).unwrap())
                        .to_str()
                        .unwrap()
                );
                assert_eq!(args[3], "--json");
                assert_eq!(args.len(), 4, "the owner's default is not overridden");
                Ok(source_owner_output(0, serde_json::to_vec(&value).unwrap()))
            })
            .unwrap();
        assert_eq!(
            calls, 1,
            "capture does not resolve another program or run another schema probe"
        );
        assert_eq!(actual, expected);
    }
}

#[test]
fn source_owner_capture_refuses_unknown_incomplete_or_failed_measurements_without_fallback() {
    let root = TestTempRoot::create("source-id-errors");
    let (_, pin) = pinned_fixture(&root);
    for case in [
        "unavailable",
        "stderr",
        "exit",
        "json",
        "utf8",
        "limit",
        "root",
        "null-object",
        "unknown-field",
        "timeout",
    ] {
        let error = engram_source_id_with(
            &pin,
            &root,
            std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET,
            |_, _, _, _, _| {
                if case == "timeout" {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "fixture process deadline",
                    )
                    .into());
                }
                let mut value = source_envelope(
                    &root,
                    if case == "unavailable" {
                        "unavailable"
                    } else {
                        "clean"
                    },
                );
                if case == "root" {
                    value["root"] = json!(root.join("another-root"));
                }
                if case == "null-object" {
                    value["source_id"] = Value::Null;
                }
                if case == "unknown-field" {
                    value["source_id"]["extra"] = json!(true);
                }
                let mut output = source_owner_output(
                    if case == "unavailable" { 1 } else { 0 },
                    serde_json::to_vec(&value).unwrap(),
                );
                match case {
                    "stderr" => output.stderr = b"diagnostic".to_vec(),
                    "exit" => output.status = source_owner_output(128, vec![]).status,
                    "json" => output.stdout = b"not JSON".to_vec(),
                    "utf8" => output.stdout = vec![0xff],
                    "limit" => output.stdout = vec![b' '; ENGRAM_SOURCE_JSON_LIMIT + 2],
                    _ => {}
                }
                Ok(output)
            },
        )
        .unwrap_err();
        assert!(!error.to_string().is_empty(), "{case}");
    }
}

#[test]
fn source_owner_capture_refuses_missing_or_changed_executable_before_and_after_measurement() {
    let root = TestTempRoot::create("source-id-pin");
    let (binary, pin) = pinned_fixture(&root);
    let until = || std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET;
    let mut wrong = pin.clone();
    wrong.sha256 = "0".repeat(64);
    assert!(
        engram_source_id_with(&wrong, &root, until(), |_, _, _, _, _| panic!(
            "an untrusted executable must not run"
        ))
        .is_err()
    );
    wrong = pin.clone();
    wrong.binary_path = root.join("missing.exe").to_str().unwrap().into();
    assert!(
        engram_source_id_with(&wrong, &root, until(), |_, _, _, _, _| panic!(
            "a missing executable must not run"
        ))
        .is_err()
    );
    assert!(
        engram_source_id_with(
            &pin,
            &root,
            std::time::Instant::now(),
            |_, _, _, _, _| panic!("an expired capture must not run")
        )
        .is_err()
    );
    let error = engram_source_id_with(&pin, &root, until(), |_, _, _, _, _| {
        let mut bytes = fs::read(&binary).unwrap();
        bytes.push(b'x');
        fs::write(&binary, bytes).unwrap();
        Ok(source_owner_output(
            0,
            serde_json::to_vec(&source_envelope(&root, "clean")).unwrap(),
        ))
    })
    .unwrap_err();
    assert!(error.to_string().contains("pin"), "{error:#}");
}
