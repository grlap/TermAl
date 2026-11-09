// Dart/Flutter artifact reader and declared-route regressions, mounted under
// engram_declared_checks. Reuses its EOCheckedTurn teardown and artifact helpers.
use super::*;

const DART_PASS: &[u8] = include_bytes!("fixtures/declared-dart/dart-pass.jsonl");
const DART_FAIL: &[u8] = include_bytes!("fixtures/declared-dart/dart-fail.jsonl");
const FLUTTER_PASS: &[u8] = include_bytes!("fixtures/declared-dart/flutter-pass.jsonl");
const FLUTTER_FAIL: &[u8] = include_bytes!("fixtures/declared-dart/flutter-fail.jsonl");
const QUESTLY_COMMAND: &str = "flutter test --no-pub --exclude-tags supabase-local --file-reporter json:.termal-results/tests.json";

fn events(bytes: &[u8]) -> Vec<Value> {
    std::str::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn encode(events: &[Value]) -> Vec<u8> {
    let mut text = events
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    text.push('\n');
    text.into_bytes()
}

#[test]
fn real_dart_and_flutter_files_count_visible_executions_only() {
    for (bytes, outcome) in [
        (DART_PASS, "pass"),
        (DART_FAIL, "fail"),
        (
            include_bytes!("fixtures/declared-dart/dart-skipped.jsonl").as_slice(),
            "unknown",
        ),
        (
            include_bytes!("fixtures/declared-dart/dart-truncated.jsonl").as_slice(),
            "unknown",
        ),
        (FLUTTER_PASS, "pass"),
        (FLUTTER_FAIL, "fail"),
        (
            include_bytes!("fixtures/declared-dart/flutter-skipped.jsonl").as_slice(),
            "unknown",
        ),
        (
            include_bytes!("fixtures/declared-dart/flutter-truncated.jsonl").as_slice(),
            "unknown",
        ),
    ] {
        let (format, reading) = engram_read_declared_artifact(bytes);
        assert_eq!(format, EngramDeclaredArtifactFormat::DartJson);
        match outcome {
            "pass" => {
                assert_eq!(reading.verdict, EngramTrxVerdict::Pass);
                let counts = reading.counters.unwrap();
                assert_eq!(counts.get("total"), 1);
                assert_eq!(counts.get("executed"), 1);
                assert_eq!(
                    counts.get("passed"),
                    1,
                    "successful hidden load is excluded"
                );
            }
            "fail" => {
                assert!(matches!(reading.verdict, EngramTrxVerdict::Fail(_)));
                let counts = reading.counters.unwrap();
                assert_eq!(counts.get("passed"), 0);
                assert_eq!(counts.get("failed"), 1);
            }
            _ => assert!(matches!(reading.verdict, EngramTrxVerdict::Unknown(_))),
        }
    }
}

#[test]
fn real_flutter_main_suite_with_parallel_loading_and_skip_has_exact_counts() {
    let reading = engram_read_dart_json(include_bytes!(
        "fixtures/declared-dart/flutter-main-suite.jsonl"
    ));
    assert!(matches!(reading.verdict, EngramTrxVerdict::Fail(_)));
    let counters = reading.counters.unwrap();
    assert_eq!(counters.get("total"), 3);
    assert_eq!(counters.get("executed"), 2);
    assert_eq!(counters.get("passed"), 1);
    assert_eq!(counters.get("failed"), 1);
    assert_eq!(counters.get("notExecuted"), 1);
}

#[test]
fn malformed_or_incomplete_stream_never_promotes_a_passing_or_failing_prefix() {
    let original = events(DART_PASS);
    let mut cases = Vec::new();
    let mut missing_done = original.clone();
    missing_done.pop();
    cases.push(missing_done);
    let mut failing_prefix = events(DART_FAIL);
    failing_prefix.pop();
    cases.push(failing_prefix);
    let mut orphan = original.clone();
    orphan.iter_mut().find(|e| e["type"] == "testDone").unwrap()["testID"] = json!(999);
    cases.push(orphan);
    let mut duplicate = original.clone();
    duplicate.insert(1, original[0].clone());
    cases.push(duplicate);
    let mut unknown_type = original.clone();
    unknown_type[1]["type"] = json!("futureEvent");
    cases.push(unknown_type);
    let mut unknown_version = original.clone();
    unknown_version[0]["protocolVersion"] = json!("9.0.0");
    cases.push(unknown_version);
    let mut unknown_result = original.clone();
    unknown_result
        .iter_mut()
        .find(|e| e["type"] == "testDone")
        .unwrap()["result"] = json!("futureResult");
    cases.push(unknown_result);
    let mut missing_start = original.clone();
    missing_start.remove(0);
    cases.push(missing_start);
    let mut open_test = original.clone();
    open_test.retain(|e| !(e["type"] == "testDone" && e["testID"] == 3));
    cases.push(open_test);
    let mut duplicate_end = original.clone();
    duplicate_end.insert(5, original[4].clone());
    cases.push(duplicate_end);
    let mut nullable_done = original.clone();
    nullable_done.last_mut().unwrap()["success"] = Value::Null;
    cases.push(nullable_done);
    let mut after_done = original.clone();
    after_done.push(original[1].clone());
    cases.push(after_done);
    let mut missing_flag = original.clone();
    missing_flag
        .iter_mut()
        .find(|e| e["type"] == "testDone")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("hidden");
    cases.push(missing_flag);
    for (index, case) in cases.iter().enumerate() {
        assert!(
            matches!(
                engram_read_dart_json(&encode(case)).verdict,
                EngramTrxVerdict::Unknown(_)
            ),
            "case {index}"
        );
    }
    for bytes in [b"{\"type\":".as_slice(), b"\xff".as_slice()] {
        assert!(matches!(
            engram_read_dart_json(bytes).verdict,
            EngramTrxVerdict::Unknown(_)
        ));
    }
    let mut invalid_tail = encode(&original);
    invalid_tail.extend_from_slice(b"not json\n");
    assert!(matches!(
        engram_read_dart_json(&invalid_tail).verdict,
        EngramTrxVerdict::Unknown(_)
    ));
    let mut broken_failure = encode(&events(DART_FAIL));
    broken_failure.extend_from_slice(b"{\n");
    assert!(matches!(
        engram_read_dart_json(&broken_failure).verdict,
        EngramTrxVerdict::Unknown(_)
    ));
    assert!(matches!(
        engram_read_dart_json(&vec![b' '; ENGRAM_ARTIFACT_MAX_BYTES + 1]).verdict,
        EngramTrxVerdict::Unknown(_)
    ));
}

#[test]
fn complete_stream_failures_include_late_and_hidden_errors() {
    for id in [1, 3] {
        let mut stream = events(DART_PASS);
        let before_done = stream.len() - 1;
        stream.insert(
            before_done,
            json!({"type":"error","time":800,"testID":id,
            "error":"late error","stackTrace":"","isFailure":false}),
        );
        let reading = engram_read_dart_json(&encode(&stream));
        assert!(matches!(reading.verdict, EngramTrxVerdict::Fail(_)));
        let counters = reading.counters.unwrap();
        assert_eq!(counters.get("executed"), if id == 1 { 2 } else { 1 });
        assert_eq!(counters.get("passed"), if id == 3 { 0 } else { 1 });
        assert_eq!(counters.get("error"), 1);
        assert_eq!(counters.get("failed"), 0);
    }
    let mut hidden_failure = events(DART_PASS);
    hidden_failure.insert(
        hidden_failure.len() - 1,
        json!({"type":"error","time":800,"testID":1,
        "error":"late failure","stackTrace":"","isFailure":true}),
    );
    let reading = engram_read_dart_json(&encode(&hidden_failure));
    assert!(matches!(reading.verdict, EngramTrxVerdict::Fail(_)));
    let counters = reading.counters.unwrap();
    assert_eq!(
        (
            counters.get("total"),
            counters.get("executed"),
            counters.get("passed"),
            counters.get("failed"),
            counters.get("error")
        ),
        (2, 2, 1, 1, 0)
    );
    let mut run_failed = events(DART_PASS);
    run_failed.last_mut().unwrap()["success"] = json!(false);
    assert!(matches!(
        engram_read_dart_json(&encode(&run_failed)).verdict,
        EngramTrxVerdict::Fail(_)
    ));
    let mut result_error = events(DART_PASS);
    result_error
        .iter_mut()
        .find(|e| e["type"] == "testDone" && e["testID"] == 3)
        .unwrap()["result"] = json!("error");
    assert!(matches!(
        engram_read_dart_json(&encode(&result_error)).verdict,
        EngramTrxVerdict::Fail(_)
    ));
}

#[test]
fn hidden_load_success_alone_and_no_matches_are_unknown() {
    let mut stream = events(DART_PASS);
    stream.retain(|e| {
        !((e["type"] == "testStart" && e["test"]["id"] == 3)
            || (e["type"] == "testDone" && e["testID"] == 3))
    });
    let reading = engram_read_dart_json(&encode(&stream));
    assert!(matches!(reading.verdict, EngramTrxVerdict::Unknown(_)));
    assert_eq!(reading.counters.unwrap().get("passed"), 0);
    let none = br#"{"type":"start","time":0,"pid":0,"protocolVersion":"0.1.1"}
{"type":"allSuites","time":0,"count":0}
{"type":"done","time":0,"success":true}
"#;
    assert!(matches!(
        engram_read_dart_json(none).verdict,
        EngramTrxVerdict::Unknown(_)
    ));
}

#[test]
fn suite_group_print_and_debug_references_must_be_consistent() {
    let original = events(DART_PASS);
    let mut cases = Vec::new();
    let mut missing_count = original.clone();
    missing_count.retain(|e| e["type"] != "allSuites");
    cases.push(missing_count);
    let mut wrong_count = original.clone();
    wrong_count
        .iter_mut()
        .find(|e| e["type"] == "allSuites")
        .unwrap()["count"] = json!(9);
    cases.push(wrong_count);
    let mut duplicate_count = original.clone();
    duplicate_count.insert(4, original[3].clone());
    cases.push(duplicate_count);
    let mut duplicate_suite = original.clone();
    duplicate_suite.insert(2, original[1].clone());
    cases.push(duplicate_suite);
    let mut unknown_group_suite = original.clone();
    unknown_group_suite
        .iter_mut()
        .find(|e| e["type"] == "group")
        .unwrap()["group"]["suiteID"] = json!(9);
    cases.push(unknown_group_suite);
    let mut unknown_test_suite = original.clone();
    unknown_test_suite
        .iter_mut()
        .find(|e| e["type"] == "testStart")
        .unwrap()["test"]["suiteID"] = json!(9);
    cases.push(unknown_test_suite);
    let mut foreign_group = original.clone();
    foreign_group.insert(
        2,
        json!({"type":"suite","time":0,"suite":{"id":9,"platform":"vm","path":"other.dart"}}),
    );
    foreign_group
        .iter_mut()
        .find(|e| e["type"] == "group")
        .unwrap()["group"]["suiteID"] = json!(9);
    foreign_group
        .iter_mut()
        .find(|e| e["type"] == "allSuites")
        .unwrap()["count"] = json!(2);
    cases.push(foreign_group);
    for event in [
        json!({"type":"print","time":0,"testID":3,"messageType":"future","message":"text"}),
        json!({"type":"print","time":0,"testID":999,"messageType":"print","message":"text"}),
        json!({"type":"error","time":0,"testID":999,"error":"text","stackTrace":"","isFailure":true}),
        json!({"type":"debug","time":0,"suiteID":999}),
    ] {
        let mut stream = original.clone();
        stream.insert(stream.len() - 1, event);
        cases.push(stream);
    }
    for (index, stream) in cases.iter().enumerate() {
        assert!(
            matches!(
                engram_read_dart_json(&encode(stream)).verdict,
                EngramTrxVerdict::Unknown(_)
            ),
            "case {index}"
        );
    }
    let mut valid_debug = original;
    valid_debug.insert(2, json!({"type":"debug","time":0,"suiteID":0}));
    valid_debug.insert(
        valid_debug.len() - 1,
        json!({"type":"print","time":900,"testID":3,"messageType":"skip","message":"reason"}),
    );
    assert_eq!(
        engram_read_dart_json(&encode(&valid_debug)).verdict,
        EngramTrxVerdict::Pass
    );
}

#[test]
fn real_printing_dart_and_flutter_suites_pass_without_counting_output() {
    for bytes in [
        include_bytes!("fixtures/declared-dart/dart-print.jsonl").as_slice(),
        include_bytes!("fixtures/declared-dart/flutter-print.jsonl").as_slice(),
    ] {
        assert!(
            events(bytes)
                .iter()
                .any(|event| event["type"] == "print" && event["message"] == "fixture output")
        );
        let reading = engram_read_dart_json(bytes);
        assert_eq!(reading.verdict, EngramTrxVerdict::Pass);
        assert_eq!(reading.counters.as_ref().unwrap().get("executed"), 1);
        assert_eq!(reading.counters.unwrap().get("passed"), 1);
    }
}

#[test]
fn production_declared_diagnostics_are_bounded_fixed_and_do_not_expose_payload() {
    let pass = judge(
        "dart-log-pass",
        |_| {},
        |root| put(root, DART_PASS),
        EngramCommandExit::Code(0),
    );
    let fail = judge(
        "dart-log-fail",
        |_| {},
        |root| put(root, DART_FAIL),
        EngramCommandExit::Code(0),
    );
    let missing = judge(
        "dart-log-missing",
        |_| {},
        |_| {},
        EngramCommandExit::Code(0),
    );
    for (result, expected) in [
        (
            &pass,
            "declared-check format=dart-json verdict=PASS counters=total=1,executed=1,passed=1,failed=0,error=0,notExecuted=0 reason=none\n",
        ),
        (
            &fail,
            "declared-check format=dart-json verdict=FAIL counters=total=1,executed=1,passed=0,failed=1,error=0,notExecuted=0 reason=dart_run_failed\n",
        ),
        (
            &missing,
            "declared-check format=unread verdict=UNKNOWN counters=unavailable reason=artifact_missing\n",
        ),
    ] {
        let mut log = Vec::new();
        engram_write_declared_diagnostic(&mut log, result);
        assert_eq!(String::from_utf8(log).unwrap(), expected);
    }
    let mut untrusted = pass;
    untrusted.verdict = EngramTrxVerdict::Unknown("secret/path\nforged-log".repeat(1000));
    untrusted
        .counters
        .as_mut()
        .unwrap()
        .values
        .push(("secret-name".into(), u64::MAX));
    let mut log = Vec::new();
    engram_write_declared_diagnostic(&mut log, &untrusted);
    let line = String::from_utf8(log).unwrap();
    assert!(line.len() < 256);
    assert_eq!(line.lines().count(), 1);
    assert!(line.ends_with("reason=invalid_result\n"));
    assert!(!line.contains("secret"));
    struct Closed;
    impl std::io::Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    engram_write_declared_diagnostic(&mut Closed, &untrusted);
}

#[test]
fn trx_content_is_a_frozen_differential_through_the_new_dispatch() {
    for bytes in [
        PASSING_TRX,
        FAILING_TRX,
        ZERO_TRX,
        MIXED_TRX,
        b"<TestRun>".as_slice(),
        b"<testsuites/>".as_slice(),
        b"".as_slice(),
    ] {
        let (format, reading) = engram_read_declared_artifact(bytes);
        assert_eq!(format, EngramDeclaredArtifactFormat::Trx);
        assert_eq!(
            reading,
            engram_read_trx(bytes),
            "TRX verdict, reason, outcome and all counters unchanged"
        );
    }
    let mut bom = b"\xEF\xBB\xBF".to_vec();
    bom.extend_from_slice(PASSING_TRX);
    assert_eq!(engram_read_declared_artifact(&bom).1, engram_read_trx(&bom));
}

#[test]
fn dart_pass_summary_names_each_skip_once() {
    let mut stream = events(DART_PASS);
    let mut start = stream
        .iter()
        .find(|e| e["type"] == "testStart" && e["test"]["id"] == 3)
        .unwrap()
        .clone();
    start["test"]["id"] = json!(4);
    stream.insert(stream.len() - 1, start);
    stream.insert(
        stream.len() - 1,
        json!({"type":"testDone","time":900,"testID":4,
        "hidden":false,"skipped":true,"result":"success"}),
    );
    let reading = engram_read_dart_json(&encode(&stream));
    assert_eq!(reading.verdict, EngramTrxVerdict::Pass);
    let result = EngramDeclaredResult {
        format: EngramDeclaredArtifactFormat::DartJson,
        verdict: reading.verdict,
        artifact: None,
        outcome: reading.outcome,
        counters: reading.counters,
    };
    let (command, _) = engram_declared_candidate(QUESTLY_COMMAND).unwrap();
    let summary = engram_declared_summary(&command, None, EngramCommandExit::Code(0), &result);
    assert!(summary.contains("PASS: 1 passed, 1 skipped"), "{summary}");
    assert!(!summary.contains("1 not executed"), "{summary}");
}

#[test]
fn captured_protocol_and_additive_fields_preserve_the_real_capture() {
    for version in ["0.1.1"] {
        let mut stream = events(DART_PASS);
        stream[0]["protocolVersion"] = json!(version);
        for event in &mut stream {
            event["futureOptionalAttribute"] = json!({"value":1});
        }
        assert_eq!(
            engram_read_dart_json(&encode(&stream)).verdict,
            EngramTrxVerdict::Pass
        );
    }
    for version in ["0.1.0", "0.1.2", "0.2"] {
        let mut stream = events(DART_PASS);
        stream[0]["protocolVersion"] = json!(version);
        assert_eq!(
            engram_read_dart_json(&encode(&stream)).verdict,
            EngramTrxVerdict::Unknown(
                "the Dart JSON reporter start or protocol version is unsupported".into()
            )
        );
    }
    let mut bom = b"\xEF\xBB\xBF".to_vec();
    bom.extend_from_slice(FLUTTER_PASS);
    assert_eq!(
        engram_read_declared_artifact(&bom).1.verdict,
        EngramTrxVerdict::Pass
    );
}

#[test]
fn declared_flutter_reuses_the_real_artifact_fences_and_exit_precedence() {
    for (label, bytes, exit, expected) in [
        ("dart-content-pass", DART_PASS, 0, "passed"),
        ("flutter-content-pass", FLUTTER_PASS, 0, "passed"),
        ("dart-content-fail", DART_FAIL, 0, "failed"),
        ("flutter-content-fail", FLUTTER_FAIL, 0, "failed"),
        ("flutter-nonzero", FLUTTER_PASS, 7, "failed"),
    ] {
        let turn = EOCheckedTurn::start(label, true);
        // Deliberately use the parent's .trx suffix for JSON: only content selects format.
        declare(&turn, QUESTLY_COMMAND, DECLARED_ARTIFACT);
        turn.run(
            "dart-check",
            QUESTLY_COMMAND,
            EngramCommandExit::Code(exit),
            || write_artifact(&turn, bytes),
        );
        let (_, diagnostic) = declared_diagnostic(&turn);
        assert_eq!(diagnostic.format, EngramDeclaredArtifactFormat::DartJson);
        assert_eq!(diagnostic.counters.unwrap().get("executed"), 1);
        let refs = diagnostic_refs(&turn);
        assert!(
            refs.iter()
                .any(|reference| reference.starts_with("dart-json-counters:total=1,executed=1,"))
        );
        assert!(!refs.iter().any(|reference| reference.starts_with("trx-")));
        assert_eq!(observed_result(&finish_observation(&turn)), expected);
    }
    for (label, missing, expected) in [
        ("flutter-missing", true, "the artifact is missing"),
        ("flutter-unchanged", false, "unchanged"),
    ] {
        let result = judge(
            label,
            |root| {
                if !missing {
                    put(root, FLUTTER_PASS);
                }
            },
            |_| {},
            EngramCommandExit::Code(0),
        );
        assert!(unknown_reason(&result).contains(expected));
    }
    let result = judge(
        "dart-deleted",
        |_| {},
        |root| {
            put(root, FLUTTER_PASS);
            fs::remove_file(root.join(DECLARED_ARTIFACT)).unwrap();
        },
        EngramCommandExit::Code(0),
    );
    assert!(unknown_reason(&result).contains("missing"));
    let result = judge(
        "dart-nonzero-no-artifact",
        |_| {},
        |_| {},
        EngramCommandExit::Code(9),
    );
    assert!(matches!(result.verdict, EngramTrxVerdict::Fail(_)));
}

#[test]
fn questly_commands_still_need_their_exact_declaration_and_cwd() {
    let root = declared_root("flutter-exact-command");
    load(
        &root,
        &entry(QUESTLY_COMMAND, None, ".termal-results/tests.json"),
    );
    assert!(bind(&root, QUESTLY_COMMAND).is_some());
    assert!(bind(&root, "flutter test --no-pub").is_none());
    assert!(bind(&root, &format!("{QUESTLY_COMMAND} test/focused_test.dart")).is_none());
    assert!(bind(&root, &format!("{QUESTLY_COMMAND} --name selected")).is_none());
    let expanded = format!("{QUESTLY_COMMAND} --reporter expanded");
    assert!(bind(&root, &expanded).is_none());
    load(&root, &entry(&expanded, None, ".termal-results/tests.json"));
    assert!(bind(&root, &expanded).is_some());
    let dart = "dart test --file-reporter json:reports/dart.json";
    fs::create_dir_all(root.join("common_core")).unwrap();
    load(
        &root,
        &entry(dart, Some("common_core"), "common_core/reports/dart.json"),
    );
    assert!(bind(&root, dart).is_none(), "package cwd is exact");
    assert!(
        engram_declared_bind(
            &engram_declared_words(dart).unwrap(),
            &target_at(&root, &root.join("common_core"))
        )
        .0
        .is_some()
    );
}
