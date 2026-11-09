// Full owner identities through record construction, receipt association,
// persistence and bounded presentation. Historical replay keeps its bytes.
use super::*;

fn source() -> Value {
    json!({"commit":COMMIT,"tree":"b".repeat(40),"changes":[],
        "submodules":[{"path":"private-child.txt","commit":"d".repeat(40),"changes":[]}]})
}

fn dirty_source() -> Value {
    let mut value = source();
    value["changes"] = json!([{
        "path":"private-input.txt","status":"??","origin":null,
        "blob":"c".repeat(40),"mode":"100644"
    }]);
    value["submodules"][0]["changes"] = json!([{
        "path":"private-child.txt","status":" M","origin":null,
        "blob":"e".repeat(40),"mode":"100644"
    }]);
    value
}

pub(super) fn source_for_refusal_test() -> String {
    serde_json::to_string(&dirty_source()).unwrap()
}

fn source_artifacts(root: &FsPath) -> (Value, Value) {
    let mut request = commit_request(root);
    request["sourceId"] = source();
    let mut results = commit_results();
    results["sourceIdBefore"] = source();
    results["sourceIdAfter"] = source();
    (request, results)
}

#[test]
fn frozen_record_debug_keeps_owner_payload_out_of_preparation_diagnostics() {
    let fixture = RunFixture::new("source-record-debug");
    let (request, results) = source_artifacts(&fixture.root);
    let run = record_run(&fixture, "test-source-debug", request.clone(), results.clone());
    let body = engram_verification_record_body(&run.registration, &request, &results).unwrap();
    let frozen = EngramVerificationRecordFrozen {
        run_dir: PathBuf::from(&run.registration.run_directory),
        run_id: run.registration.run_id.clone(),
        record: serde_json::to_value(&body).unwrap(),
        body,
    };
    for diagnostic in [
        format!("{frozen:?}"),
        format!("{:?}", EngramVerificationRecordPrepared::Ready(frozen.clone())),
    ] {
        assert!(diagnostic.contains(&run.registration.run_id));
        assert!(diagnostic.len() < 256, "preparation diagnostic stays bounded");
        for hidden in [COMMIT, "private-child.txt", "source_id", "submodules", "record:", "body:"] {
            assert!(!diagnostic.contains(hidden), "preparation diagnostic omits the record payload");
        }
    }
}

#[test]
fn new_launcher_records_and_check_bases_keep_every_owner_component() {
    let fixture = RunFixture::new("source-record-complete");
    let (request, results) = source_artifacts(&fixture.root);
    let run = record_run(&fixture, "test-source-complete", request.clone(), results.clone());
    let body = engram_verification_record_body(&run.registration, &request, &results).unwrap();
    let wire = serde_json::to_value(&body).unwrap();
    assert!(wire["input"] == json!({"kind":"source_id","source_id":source()}), "complete input identity is preserved");
    assert!(wire["post_run"] == json!({"source_id":source()}), "complete post-run identity is preserved");
    let producers = engram_verification_record_producers(&run.registration.run_id, &body);
    assert_eq!(producers.len(), 3);
    for (_, producer) in producers {
        let basis = producer.source_basis.unwrap();
        assert!(serde_json::to_value(basis.source_id.unwrap()).unwrap() == source(), "complete producer identity is preserved");
        assert!(basis.source_revision.is_empty());
        assert_eq!(basis.source_root_generation, Some(3));
    }
    let text = engram_verification_record_text(&body).unwrap();
    assert_eq!(engram_verification_record_parse(&text).unwrap().1, body);
}

#[test]
fn new_source_records_preserve_large_identities_beyond_the_legacy_frame_limit() {
    let fixture = RunFixture::new("source-record-frame-refusal");
    let mut identity = source();
    identity["submodules"] = json!((0..2_000).map(|index| json!({
        "path":format!("private-child-{index:05}"), "commit":"d".repeat(40), "changes":[]
    })).collect::<Vec<_>>());
    assert!(test_run_source_id(Some(&identity)).is_some(), "the complete owner identity is legal");
    let (mut request, mut results) = source_artifacts(&fixture.root);
    request["sourceId"] = identity.clone();
    results["sourceIdBefore"] = identity.clone();
    results["sourceIdAfter"] = identity;
    let run = record_run(&fixture, "test-source-frame-refusal", request.clone(), results.clone());
    let body = engram_verification_record_body(&run.registration, &request, &results).unwrap();
    let frame = EngramControlRequest::VerificationRecord {
        operation_key: engram_verification_record_key(&run.registration.run_id),
        record: serde_json::to_value(&body).unwrap(),
    };
    let encoded = encode_engram_control_request(&frame).unwrap();
    assert!(encoded.len() > ENGRAM_CONTROL_MAX_FRAME_BYTES,
        "this fixture must exceed the historical frame capacity");
    assert!(encoded.len() + 1 <= ACCEPTANCE_VERIFICATION_FRAME_MAX_BYTES);
    let restored = engram_verification_record_parse(&engram_verification_record_text(&body).unwrap()).unwrap().1;
    assert!(serde_json::to_value(&restored).unwrap()["input"]["source_id"] == request["sourceId"],
        "the large complete identity survives the private record round trip");
    let retained: Value = serde_json::from_slice(&fs::read(run.run_dir.join("results.json")).unwrap()).unwrap();
    assert!(retained["sourceIdAfter"] == results["sourceIdAfter"], "the full endpoint remains unchanged on disk");
}

#[test]
fn new_source_records_refuse_partial_or_changed_identity_without_commit_fallback() {
    let fixture = RunFixture::new("source-record-refusals");
    let (request, results) = source_artifacts(&fixture.root);
    let run = record_run(&fixture, "test-source-refusals", request.clone(), results.clone());
    for field in ["sourceIdBefore", "sourceIdAfter"] {
        for replacement in [Value::Null, json!({"commit":COMMIT}), json!("content-v1:abc")] {
            let mut changed = results.clone();
            changed[field] = replacement;
            assert!(engram_verification_record_body(&run.registration, &request, &changed).is_err());
        }
        let mut missing = results.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(engram_verification_record_body(&run.registration, &request, &missing).is_err());
    }
    let mutations: [fn(&mut Value); 6] = [
        |value| value["commit"] = json!("f".repeat(40)),
        |value| value["tree"] = json!("f".repeat(40)),
        |value| value["changes"] = dirty_source()["changes"].clone(),
        |value| value["submodules"][0]["path"] = json!("other-child"),
        |value| value["submodules"][0]["commit"] = json!("f".repeat(40)),
        |value| value["submodules"][0]["changes"] = dirty_source()["submodules"][0]["changes"].clone(),
    ];
    for mutate in mutations {
        let mut changed = results.clone();
        mutate(&mut changed["sourceIdAfter"]);
        assert!(engram_verification_record_body(&run.registration, &request, &changed).is_err());
    }
    let mut changed_request = request.clone();
    changed_request.as_object_mut().unwrap().remove("sourceId");
    assert!(engram_verification_record_body(&run.registration, &changed_request, &results).is_err());
    let mut old_request = request;
    old_request.as_object_mut().unwrap().remove("sourceId");
    let mut old_results = results;
    old_results.as_object_mut().unwrap().remove("sourceIdBefore");
    old_results.as_object_mut().unwrap().remove("sourceIdAfter");
    assert!(engram_verification_record_body(&run.registration, &old_request, &old_results).is_err(),
        "absent owner fields do not select a legacy write format");
}

#[test]
fn clean_commit_projection_refuses_equal_dirty_root_or_child_owner_values() {
    let fixture = RunFixture::new("source-record-clean-coherence");
    let legacy = EngramVerificationRecordInput::Commit { commit: COMMIT.into() };
    for child in [false, true] {
        let mut dirty = source();
        if child {
            dirty["submodules"][0]["changes"] = dirty_source()["submodules"][0]["changes"].clone();
        } else {
            dirty["changes"] = dirty_source()["changes"].clone();
        }
        let (mut request, mut results) = source_artifacts(&fixture.root);
        request["sourceId"] = dirty.clone();
        results["sourceIdBefore"] = dirty.clone();
        results["sourceIdAfter"] = dirty;
        let run = record_run(&fixture, &format!("test-source-dirty-{child}"), request.clone(), results.clone());
        assert!(engram_verification_record_body(&run.registration, &request, &results).is_err(),
            "equal endpoints cannot make the committed input clean: child={child}");
        assert!(engram_verification_record_build_body(&run.registration, &request, &results, Some(&legacy)).is_err(),
            "historical replay cannot ignore contradictory new fields: child={child}");
    }
}

#[test]
fn historical_frozen_commit_records_replay_their_original_bytes() {
    let fixture = RunFixture::new("source-record-historical");
    let (request, results) = source_artifacts(&fixture.root);
    let run = record_run(&fixture, "test-source-historical", request.clone(), results);
    // Reconstruct the body written by the earlier producer from the same
    // registration and its original commit-format artifacts.
    let old_input = EngramVerificationRecordInput::Commit { commit: COMMIT.into() };
    let old = engram_verification_record_build_body(&run.registration,
        &commit_request(&fixture.root), &commit_results(), Some(&old_input)).unwrap();
    let text = engram_verification_record_text(&old).unwrap();
    let path = run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE);
    fs::write(&path, &text).unwrap();
    let frozen = ready(&run.registration);
    assert_eq!(frozen.body, old);
    assert!(engram_verification_record_is_host_body(&run.registration, &frozen.body));
    assert!(fs::read_to_string(&path).unwrap() == text, "the retained body bytes are unchanged");
    let basis = engram_verification_record_basis(&frozen.body).unwrap();
    assert!(basis.source_id.is_none());
    assert_eq!(basis.source_revision, format!("git-commit:{COMMIT}"));
    let mut historical = basis;
    historical.source_revision = "content-v1:retained-historical-value".into();
    let stored = serde_json::to_value(&historical).unwrap();
    let restored: EngramExecutionSourceBasis = serde_json::from_value(stored.clone()).unwrap();
    assert!(serde_json::to_value(&restored).unwrap() == stored, "the complete historical basis is unchanged");
    assert_eq!(test_run_source_summary(&restored), historical.source_revision);

    let mut changed = serde_json::from_slice::<Value>(&fs::read(run.run_dir.join("results.json")).unwrap()).unwrap();
    changed["sourceIdAfter"]["submodules"][0]["commit"] = json!("f".repeat(40));
    fs::write(run.run_dir.join("results.json"), serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(matches!(engram_verification_record_prepare(&run.registration),
        EngramVerificationRecordPrepared::Pending(EngramVerificationRecordPending::BodyNotBuilt { .. })),
        "new identity fields cannot be ignored by the historical reader");
    assert!(fs::read_to_string(&path).unwrap() == text, "refusal does not rewrite the retained body");
}

#[test]
fn unavailable_owner_at_either_boundary_preserves_the_original_no_credit_result() {
    let fixture = RunFixture::new("source-record-owner-unavailable");
    let historical = EngramVerificationRecordInput::Commit { commit: COMMIT.into() };
    for boundary in ["before", "after"] {
        let (request, mut results) = source_artifacts(&fixture.root);
        results["sourceIdAfter"] = Value::Null;
        results["input"]["eligible"] = json!(false);
        results["input"].as_object_mut().unwrap().remove("after");
        results["input"].as_object_mut().unwrap().remove("outcome");
        if boundary == "before" {
            results["sourceIdBefore"] = Value::Null;
            results["input"]["commit"] = Value::Null;
            results["input"]["cleanBefore"] = json!(false);
        }
        let run = record_run(&fixture, &format!("test-owner-unavailable-{boundary}"), request.clone(), results.clone());
        // The launcher never supplies post-run commit classification when
        // its owner is unavailable. Both the original retained Commit path
        // and the current writer refuse before an identity can earn credit.
        let original = engram_verification_record_build_body(&run.registration, &request, &results, Some(&historical));
        let current = engram_verification_record_body(&run.registration, &request, &results);
        assert!(original.is_err(), "{boundary}");
        assert_eq!(current.unwrap_err(), original.unwrap_err(), "{boundary}");
        assert!(engram_verification_record_frozen(&run.run_dir, &run.registration.run_id).is_none());
    }
}

#[test]
fn full_source_receipt_associations_survive_reload_but_do_not_enter_run_summaries() {
    let fixture = RunFixture::new("source-record-settle");
    let (session, transport) = engram_session(&fixture, true);
    let (request, results) = source_artifacts(&fixture.root);
    let mut run = record_run(&fixture, "test-source-settle", request, results);
    initiated_by(&mut run, &session);
    register(&fixture, &[&run]);
    let execution = attach_launch_row(&fixture, &run);
    transport.script(Ok(work_supported(&session)));
    transport.script(Ok(receipt_for("failed")));
    fixture.state.engram_verification_records_tick();
    assert_eq!(row_records(&fixture, &execution), (2..5).map(|index| Some(record_id(index))).collect::<Vec<_>>());
    fixture.state.shutdown_persist_blocking();
    let restored = load_state(fixture.state.persistence_path.as_path()).unwrap().unwrap();
    *fixture.state.inner.lock().unwrap() = restored;
    let inner = fixture.state.inner.lock().unwrap();
    let row = inner.test_run_observations.get(&execution).unwrap();
    assert_eq!(row.evidence.len(), 3);
    assert!(serde_json::to_string(&row.evidence).unwrap().contains("private-child.txt"));
    for evidence in &row.evidence {
        assert!(serde_json::to_value(evidence.source_basis.source_id.as_ref().unwrap()).unwrap() == source(),
            "complete associated identity survives reload");
        assert!(evidence.source_basis.source_revision.is_empty());
    }
    let summary = test_run_observation_summary(&*inner, row);
    assert_eq!(summary.registration.source_fingerprints.len(), 1);
    let label = &summary.registration.source_fingerprints[0];
    assert!(label.starts_with("source_id: commit 0123456789ab, tree bbbbbbbbbbbb"));
    assert!(label.len() < 128);
    let presented = serde_json::to_string(&summary).unwrap();
    assert!(!presented.contains("private-child.txt"));
    assert!(!presented.contains(&"d".repeat(40)));
}

#[test]
fn bounded_source_summary_distinguishes_dirty_children_without_disclosing_paths() {
    let clean = EngramExecutionSourceBasis {
        workspace_id: "fixture-workspace".into(), source_revision: String::new(),
        source_id: Some(serde_json::from_value(source()).unwrap()), source_root_generation: None,
        source_root_state: None,
    };
    let mut dirty = clean.clone();
    dirty.source_id.as_mut().unwrap().submodules[0].changes =
        serde_json::from_value(dirty_source()["submodules"][0]["changes"].clone()).unwrap();
    let clean_label = test_run_source_summary(&clean);
    let dirty_label = test_run_source_summary(&dirty);
    assert_ne!(clean_label, dirty_label, "a dirty child must be visible in the bounded summary");
    assert!(dirty_label.len() < 128);
    assert!(!dirty_label.contains("private-child.txt"));
    assert!(!dirty_label.contains(&"d".repeat(40)) && !dirty_label.contains(&"e".repeat(40)));
}

#[test]
fn receipt_settlement_compares_complete_source_not_its_presentation_label() {
    let fixture = RunFixture::new("source-record-equality");
    let (request, results) = source_artifacts(&fixture.root);
    let run = record_run(&fixture, "test-source-equality", request.clone(), results.clone());
    let body = engram_verification_record_body(&run.registration, &request, &results).unwrap();
    let producers = engram_verification_record_producers(&run.registration.run_id, &body);
    // Native SourceId runs may legitimately retain dirt. Exercise the shared
    // settlement boundary with that full basis, without declaring a dirty
    // observation to be a clean committed-input launcher run.
    let mut native_producer = producers[0].1.clone();
    native_producer.source_basis.as_mut().unwrap().source_id = Some(serde_json::from_value(dirty_source()).unwrap());
    let producer = &native_producer;
    let basis = producer.source_basis.as_ref().unwrap();
    let execution = attach_launch_row(&fixture, &run);
    let record = record_id(2);
    let mut inner = fixture.state.inner.lock().unwrap();
    let mutations: [fn(&mut EngramSourceId); 3] = [
        |source| source.changes[0].blob = Some("f".repeat(40)),
        |source| source.submodules[0].commit = "f".repeat(40),
        |source| source.submodules[0].changes[0].mode = Some(EngramSourceIdMode::Executable),
    ];
    for mutate in mutations {
        let mut changed = basis.clone();
        mutate(changed.source_id.as_mut().unwrap());
        assert_eq!(test_run_source_summary(&changed), test_run_source_summary(basis));
        inner.test_run_observations.update(&execution, |row| {
            row.evidence = vec![TestRunEvidenceAssociation {
                store: store(), binding: binding(), source_basis: changed,
                producer_observation_id: producer.observation_id.clone(),
                action_fingerprint: producer.action_fingerprint.clone(), reported_outcome: producer.outcome,
                verification_record_id: None, checkpoint_id: None, confirmed_cursor: None, lifecycle_read: None,
            }];
        }).unwrap();
        let settlements = HashMap::from([(producer.observation_id.as_str(), vec![(producer, &record)])]);
        assert!(AppState::settle_test_run_producer_records_locked(&mut inner, settlements, &store(), &binding()).is_empty());
        assert!(inner.test_run_observations.get(&execution).unwrap().evidence[0].verification_record_id.is_none());
    }
    inner.test_run_observations.update(&execution, |row| row.evidence[0].source_basis = basis.clone()).unwrap();
    let settlements = HashMap::from([(producer.observation_id.as_str(), vec![(producer, &record)])]);
    assert_eq!(AppState::settle_test_run_producer_records_locked(&mut inner, settlements, &store(), &binding()).len(), 1);
    assert_eq!(inner.test_run_observations.get(&execution).unwrap().evidence[0].verification_record_id.as_ref(), Some(&record));
}
