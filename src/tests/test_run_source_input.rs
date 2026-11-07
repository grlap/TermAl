use super::*;

fn dirty_source() -> Value {
    let mut source = launcher_source_id_fixture();
    source["changes"] = json!([{"path":"local.txt","status":"??","origin":null,
        "blob":"c".repeat(40),"mode":"100644"}]);
    source["submodules"] = json!([{"path":"child/nested","commit":"d".repeat(40),
        "changes":[{"path":"child.txt","status":" M","origin":null,
            "blob":"e".repeat(40),"mode":"100644"}]}]);
    source
}

fn records(source: &Value) -> (Value, Value) {
    (json!({"sourceId":source}), json!({"sourceIdBefore":source,"sourceIdAfter":source}))
}

#[test]
fn full_owner_values_accept_equal_dirty_inputs_and_refuse_every_changed_component() {
    for source in [launcher_source_id_fixture(), dirty_source()] {
        let (request, results) = records(&source);
        assert!(test_run_source_input_matches(&request, &results));
    }
    let source = dirty_source();
    let (request, results) = records(&source);
    let mutations: [fn(&mut Value); 8] = [
        |value| value["commit"] = json!("f".repeat(40)),
        |value| value["tree"] = json!("f".repeat(40)),
        |value| value["changes"][0]["blob"] = json!("f".repeat(40)),
        |value| value["changes"][0]["mode"] = json!("100755"),
        |value| value["changes"].as_array_mut().unwrap().push(json!({
            "path":"new.txt","status":"??","origin":null,"blob":"f".repeat(40),"mode":"100644"})),
        |value| value["submodules"][0]["commit"] = json!("f".repeat(40)),
        |value| value["submodules"][0]["changes"][0]["blob"] = json!("f".repeat(40)),
        |value| value["submodules"][0]["changes"][0]["mode"] = json!("120000"),
    ];
    for mutate in mutations {
        let mut changed = results.clone();
        mutate(&mut changed["sourceIdAfter"]);
        assert!(!test_run_source_input_matches(&request, &changed));
    }
}

#[test]
fn partial_unavailable_or_oversized_source_is_not_equality_and_digests_cannot_replace_it() {
    let mut missing_nullable = dirty_source();
    missing_nullable["changes"][0].as_object_mut().unwrap().remove("origin");
    let mut unknown = launcher_source_id_fixture();
    unknown["extra"] = json!(true);
    let mut oversized = dirty_source();
    oversized["changes"][0]["path"] = json!("x".repeat(ENGRAM_SOURCE_JSON_LIMIT));
    for source in [Value::Null, json!({}), missing_nullable, unknown, oversized] {
        let (request, results) = records(&source);
        assert!(!test_run_source_input_matches(&request, &results));
        assert_eq!(TestRunWaitEvidence::from_results(&results).source_comparison, "unknown");
    }
    assert!(!test_run_source_input_matches(&json!({"expectedFingerprint":"a".repeat(64)}),
        &json!({"expectedFingerprint":"a".repeat(64),"before":"a".repeat(64),"after":"a".repeat(64)})));
}

#[test]
fn remote_source_input_requires_nullable_endpoints_and_preserves_legacy_shape() {
    let source = dirty_source();
    for input in [
        json!({"kind":"source_id","source_id_before":source,"source_id_after":source}),
        json!({"kind":"source_id","source_id_before":null,"source_id_after":null}),
    ] {
        let parsed: native_test_completion::Input = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), input);
    }
    for input in [
        json!({"kind":"source_id","source_id_before":source}),
        json!({"kind":"source_id","source_id_before":source,"source_id_after":{},"before_fingerprint":"a".repeat(64)}),
        json!({"kind":"unknown","source_id_before":source,"source_id_after":source}),
        json!({"kind":"source_id","source_id_before":source,"source_id_after":source,"source_id":source}),
    ] {
        assert!(serde_json::from_value::<native_test_completion::Input>(input).is_err());
    }
    let historical = json!({"commit":"a".repeat(40),"tree":"b".repeat(40),
        "before_fingerprint":"c".repeat(64),"after_fingerprint":"c".repeat(64)});
    let parsed: native_test_completion::Input = serde_json::from_value(historical.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), historical);
}

#[test]
fn remote_completion_bounds_each_source_endpoint_without_rejecting_null() {
    let source = dirty_source();
    let mut oversized = source.clone();
    oversized["changes"][0]["path"] = json!("x".repeat(ENGRAM_SOURCE_JSON_LIMIT));
    let completion = |before: Value, after: Value| -> native_test_completion::Completion {
        serde_json::from_value(json!({"schema_version":1,
            "origin":{"host_id":"11111111-1111-4111-8111-111111111111","runner_session_id":"session-runner"},
            "run_id":"test-source-endpoints","mode":"focused",
            "result":{"verdict":"PASS","terminal":true,"exit":0,"reason":"original terminal"},
            "input":{"kind":"source_id","source_id_before":before,"source_id_after":after},
            "finished_at":"2026-11-07T00:00:00Z"})).unwrap()
    };
    assert!(completion(source.clone(), source.clone()).validate().is_ok());
    assert!(completion(Value::Null, Value::Null).validate().is_ok());
    for (before, after) in [
        (oversized.clone(), source.clone()),
        (source, oversized.clone()),
        (oversized.clone(), oversized),
    ] {
        assert!(completion(before, after).validate().is_err(), "neither endpoint can exceed the owner bound");
    }
}

#[test]
fn remote_sender_keeps_actual_nullable_source_endpoints_without_a_third_copy() {
    let source = dirty_source();
    for after in [source.clone(), Value::Null] {
        let results = json!({"sourceIdBefore":source,"sourceIdAfter":after,
            "before":"f".repeat(64),"after":"f".repeat(64)});
        let sent = serde_json::to_value(test_run_native_input(&results)).unwrap();
        assert_eq!(sent, json!({"kind":"source_id","source_id_before":source,"source_id_after":after}));
    }
}

#[test]
fn details_and_waits_report_current_actual_objects_without_using_a_parent_fence() {
    let fixture = RunFixture::new("source-input-detail");
    let source = dirty_source();
    let (mut request, mut results) = records(&source);
    request["root"] = json!(fixture.root);
    results["stages"] = json!([]);
    let directory = write_run(&fixture.runs_dir(), "test-source-detail", request.clone(), Some(results.clone()));
    let detail = test_run_detail(&directory).unwrap();
    assert_eq!(detail.source_comparison.as_deref(), Some("matched"));
    assert_eq!(serde_json::to_value(detail.source_id.unwrap()).unwrap(), source);
    request["sourceId"]["tree"] = json!("f".repeat(40));
    fs::write(directory.join("request.json"), serde_json::to_vec(&request).unwrap()).unwrap();
    assert_eq!(test_run_detail(&directory).unwrap().source_comparison.as_deref(), Some("mismatched"));
    results["sourceIdAfter"]["tree"] = json!("f".repeat(40));
    assert_eq!(TestRunWaitEvidence::from_results(&results).source_comparison, "mismatched");
}

#[test]
fn compact_source_records_remain_readable_at_the_host_byte_limit() {
    let fixture = RunFixture::new("source-input-byte-limit");
    let source = dirty_source();
    let (mut request, mut results) = records(&source);
    request["root"] = json!(fixture.root);
    results["stages"] = json!([]);
    results["padding"] = json!("é😀");
    let overhead = serde_json::to_vec(&results).unwrap().len() + 1;
    results["padding"] = json!(format!("é😀{}", "x".repeat(TEST_RUN_JSON_MAX_BYTES - overhead)));
    let mut bytes = serde_json::to_vec(&results).unwrap();
    bytes.push(b'\n');
    assert_eq!(bytes.len(), TEST_RUN_JSON_MAX_BYTES);
    let directory = write_run(&fixture.runs_dir(), "test-source-byte-limit", request, Some(results));
    fs::write(directory.join("results.json"), &bytes).unwrap();
    let detail = test_run_detail(&directory).unwrap();
    assert_eq!(detail.source_comparison.as_deref(), Some("matched"));
    bytes.push(b' ');
    fs::write(directory.join("results.json"), bytes).unwrap();
    assert!(test_run_detail(&directory).is_err(), "the producer cap includes its newline");
}

#[test]
fn newly_untracked_input_changes_remain_readable_without_the_historical_debris_exception() {
    let commit = "a".repeat(40);
    let results = json!({"input":{"kind":"commit","commit":commit,"cleanBefore":true,
        "after":{"commit":commit,"clean":false,"paths":[{"status":"??","path":"new.txt"}]},
        "outcome":"input_changed","eligible":false}});
    let input = TestRunCommittedInput::read(&results).unwrap();
    assert_eq!(input.outcome, Some(TestRunCommittedInputOutcome::InputChanged));
    assert_eq!(input.eligible, Some(false));
}
