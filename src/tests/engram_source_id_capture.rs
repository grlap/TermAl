use super::*;
use super::engram_source_command::source_envelope;
use super::engram_source_owner::{source_owner_fixture_binary, source_owner_output, source_owner_reply};

fn pinned_fixture(root: &FsPath) -> (PathBuf, EngramSourceOwnerPin) {
    let binary = source_owner_fixture_binary(root);
    let pin = engram_source_owner_metadata_with(&binary, root, root,
        std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET,
        |_, _, args, _, _| Ok(source_owner_reply(args))).unwrap();
    (binary, pin)
}

#[test]
fn source_owner_first_observation_supplies_pin_and_identity_without_a_second_source_read() {
    let root = TestTempRoot::create("source-id-first");
    let binary = source_owner_fixture_binary(&root);
    let first = source_envelope(&root, "clean");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut calls = Vec::new();
    let (pin, observation) = engram_source_owner_observe_with(&binary, &root, &root, deadline,
        |_, _, args, until, _| {
            assert_eq!(until, deadline, "bootstrap retains a shorter outer deadline");
            calls.push(args[0].clone());
            Ok(if args[0] == "--version" { source_owner_reply(args) }
                else { source_owner_output(0, serde_json::to_vec(&first).unwrap()) })
        }).unwrap();
    let expected: EngramSourceId = serde_json::from_value(first["source_id"].clone()).unwrap();
    assert_eq!(observation.into_source_id(deadline).unwrap(), expected);
    assert_eq!(calls, ["--version", "source"], "the schema and identity use the same response");
    assert_eq!(pin.source_version, first["version"].as_u64().unwrap() as u32);

    let later = source_envelope(&root, "not_clean");
    let later_expected: EngramSourceId = serde_json::from_value(later["source_id"].clone()).unwrap();
    assert_ne!(later_expected, expected);
    let later_deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut later_calls = 0;
    let actual = engram_source_id_with(&pin, &root, later_deadline, |_, _, args, until, _| {
        later_calls += 1;
        assert_eq!(args[0], "source");
        assert_eq!(until, later_deadline);
        Ok(source_owner_output(0, serde_json::to_vec(&later).unwrap()))
    }).unwrap();
    assert_eq!(later_calls, 1);
    assert_eq!(actual, later_expected, "a later capture cannot reuse the first measurement");
}

#[test]
fn source_owner_first_unavailable_observation_proves_schema_without_inventing_an_identity() {
    let root = TestTempRoot::create("source-id-first-unavailable");
    let binary = source_owner_fixture_binary(&root);
    let deadline = std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET;
    let (pin, observation) = engram_source_owner_observe_with(&binary, &root, &root, deadline,
        |_, _, args, _, _| Ok(source_owner_reply(args))).unwrap();
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
        let actual = engram_source_id_with(&pin, &root, deadline,
            |program, cwd, args, until, limit| {
                calls += 1;
                assert_eq!(program, fs::canonicalize(&binary).unwrap());
                assert_eq!(cwd, fs::canonicalize(&root).unwrap());
                assert_eq!(until, deadline, "pin checks, process and parsing share the shorter deadline");
                assert_eq!(limit, ENGRAM_SOURCE_JSON_LIMIT + 1);
                assert_eq!(args[0], "source");
                assert_eq!(args[1], "--root");
                assert_eq!(args[2], normalize_user_facing_path(&fs::canonicalize(&root).unwrap()).to_str().unwrap());
                assert_eq!(args[3], "--json");
                assert_eq!(args[4], "--timeout-ms");
                assert!((1..=2000).contains(&args[5].parse::<u64>().unwrap()));
                Ok(source_owner_output(0, serde_json::to_vec(&value).unwrap()))
            }).unwrap();
        assert_eq!(calls, 1, "capture does not resolve another program or run another schema probe");
        assert_eq!(actual, expected);
    }
}

#[test]
fn source_owner_capture_refuses_unknown_incomplete_or_failed_measurements_without_fallback() {
    let root = TestTempRoot::create("source-id-errors");
    let (_, pin) = pinned_fixture(&root);
    for case in ["unavailable", "stderr", "exit", "json", "utf8", "limit", "root",
        "null-object", "unknown-field", "timeout"] {
        let error = engram_source_id_with(&pin, &root,
            std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET,
            |_, _, _, _, _| {
                if case == "timeout" {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "fixture process deadline").into());
                }
                let mut value = source_envelope(&root, if case == "unavailable" { "unavailable" } else { "clean" });
                if case == "root" { value["root"] = json!(root.join("another-root")); }
                if case == "null-object" { value["source_id"] = Value::Null; }
                if case == "unknown-field" { value["source_id"]["extra"] = json!(true); }
                let mut output = source_owner_output(if case == "unavailable" { 1 } else { 0 },
                    serde_json::to_vec(&value).unwrap());
                match case {
                    "stderr" => output.stderr = b"diagnostic".to_vec(),
                    "exit" => output.status = source_owner_output(128, vec![]).status,
                    "json" => output.stdout = b"not JSON".to_vec(),
                    "utf8" => output.stdout = vec![0xff],
                    "limit" => output.stdout = vec![b' '; ENGRAM_SOURCE_JSON_LIMIT + 2],
                    _ => {}
                }
                Ok(output)
            }).unwrap_err();
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
    assert!(engram_source_id_with(&wrong, &root, until(),
        |_, _, _, _, _| panic!("an untrusted executable must not run")).is_err());
    wrong = pin.clone();
    wrong.binary_path = root.join("missing.exe").to_str().unwrap().into();
    assert!(engram_source_id_with(&wrong, &root, until(),
        |_, _, _, _, _| panic!("a missing executable must not run")).is_err());
    assert!(engram_source_id_with(&pin, &root, std::time::Instant::now(),
        |_, _, _, _, _| panic!("an expired capture must not run")).is_err());
    let error = engram_source_id_with(&pin, &root, until(), |_, _, _, _, _| {
        let mut bytes = fs::read(&binary).unwrap();
        bytes.push(b'x');
        fs::write(&binary, bytes).unwrap();
        Ok(source_owner_output(0, serde_json::to_vec(&source_envelope(&root, "clean")).unwrap()))
    }).unwrap_err();
    assert!(error.to_string().contains("pin"), "{error:#}");
}
