// Adapter conformance to Engram cc1aa93a, domain/remote_verification.rs and
// storage/work/remote_verification/tests/host_wire.rs. Scripted receipts below
// exercise the consumer, not a live Engram admission or verification credit.
// Native facts come through the real registration/provider settlement owner.
use super::*;
use crate::engram_remote_verification::{Association, IntendedUse, Purpose, Reason, Request};

fn source() -> EngramSourceId {
    let oid = sha256_hex(Uuid::new_v4().as_bytes());
    EngramSourceId {
        commit: oid[..40].to_owned(),
        tree: oid[..40].to_owned(),
        submodules: Vec::new(),
        changes: Vec::new(),
    }
}

fn association(source: &EngramSourceId, purpose: Purpose) -> Association {
    Association {
        binding: EngramControlWorkBinding {
            root_execution_id: Uuid::new_v4().to_string(),
            work_id: Uuid::new_v4().to_string(),
            run_id: Uuid::new_v4().to_string(),
            work_revision: 2,
            claim_id: Uuid::new_v4().to_string(),
            claim_fence: 3,
        },
        root: EngramVerificationRecordRoot {
            workspace_id: "fixture-destination-root".to_owned(),
            generation: 7,
            state: EngramVerificationRecordRootState::Named,
        },
        intended_use: IntendedUse::CommittedInput {
            expected_commit: source.commit.clone(),
            purpose,
        },
    }
}

fn completion() -> native_test_completion::Completion {
    native_test_completion::Completion {
        schema_version: 1,
        origin: native_test_completion::Origin {
            host_id: Uuid::new_v4().to_string(),
            runner_session_id: "session-runner".to_owned(),
        },
        run_id: format!("test-{}", Uuid::new_v4()),
        mode: native_test_completion::Mode::Focused,
        result: native_test_completion::NativeResult {
            verdict: native_test_completion::Verdict::Pass,
            terminal: true,
            exit: None,
            reason: "original native reason".to_owned(),
        },
        input: native_test_completion::Input::SourceId(native_test_completion::SourceInput {
            kind: native_test_completion::SourceInputKind::SourceId,
            source_id_before: Some(source()),
            source_id_after: None,
        }),
        finished_at: Some("2026-11-01T12:00:00Z".to_owned()),
    }
}

fn request(completion: &native_test_completion::Completion) -> Request {
    Request::new(
        "retained-operation-key".to_owned(),
        completion,
        association(&source(), Purpose::Verification),
    )
    .unwrap()
}

fn record_id() -> String {
    Uuid::new_v4().simple().to_string()
}

fn credited(stage: &str) -> Value {
    json!({"operation": record_id(), "origin": record_id(),
        "verifications": [{"stage": stage, "record_id": record_id()}],
        "qualification": {"credited": true}})
}

#[test]
fn native_settlement_crosses_the_bridge_without_changing_facts_or_minting_local_credit() {
    for success in [true, false] {
        let (fixture, _, id, directory) = native_completion::native_run("remote-bridge", true);
        observed_terminal::finish_artifact(&directory, &id, success);
        let path = directory.join("results.json");
        let mut results: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let source = source();
        results["sourceIdBefore"] = json!(source);
        results["sourceIdAfter"] = json!(source);
        fs::write(&path, results.to_string()).unwrap();
        fixture.refresh(&[]);
        let original = fixture.state.inner.lock().unwrap().test_run_registrations[&id]
            .native_completion
            .clone()
            .expect("real native settlement");
        assert_eq!(
            original.completion.input,
            native_test_completion::Input::SourceId(native_test_completion::SourceInput {
                kind: native_test_completion::SourceInputKind::SourceId,
                source_id_before: Some(source.clone()),
                source_id_after: Some(source.clone()),
            })
        );
        let before = serde_json::to_value(&original).unwrap();
        for purpose in [Purpose::Verification, Purpose::LandingGate] {
            let destination = association(&source, purpose);
            let built = Request::new(
                "one-retained-intent".to_owned(),
                &original.completion,
                destination.clone(),
            )
            .unwrap();
            let control = built.into_control_request();
            let wire: Value =
                serde_json::from_slice(&encode_engram_control_request(&control).unwrap()).unwrap();
            assert_eq!(control.operation(), "remote_verification_record");
            assert_eq!(wire["completion"], before["completion"]);
            assert_eq!(
                wire["association"],
                serde_json::to_value(destination).unwrap()
            );
            assert!(wire.get("registration").is_none());
            assert!(wire.get("routing_token").is_none());
        }
        observed_terminal::assert_no_credit(&fixture);
        let loaded = load_state(fixture.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(&loaded.test_run_registrations[&id].native_completion).unwrap(),
            before
        );
    }
}

#[test]
fn new_requests_refuse_legacy_without_rewriting_the_retained_completion() {
    let mut completion = completion();
    completion.input = native_test_completion::Input::Legacy(native_test_completion::LegacyInput {
        commit: Some(source().commit),
        before_fingerprint: Some(sha256_hex(b"before")),
        ..Default::default()
    });
    let before = serde_json::to_value(&completion).unwrap();
    assert!(
        Request::new(
            "new-key".to_owned(),
            &completion,
            association(&source(), Purpose::Verification)
        )
        .is_err()
    );
    assert_eq!(serde_json::to_value(&completion).unwrap(), before);
    let stored: native_test_completion::Completion =
        serde_json::from_value(before.clone()).unwrap();
    assert_eq!(serde_json::to_value(stored).unwrap(), before);
    let mut wire =
        serde_json::to_value(request(&self::completion()).into_control_request()).unwrap();
    wire["completion"] = before;
    assert!(serde_json::from_value::<EngramControlRequest>(wire).is_err());
}

#[test]
fn missing_changed_and_provisional_facts_reach_the_single_admission_owner_unchanged() {
    use native_test_completion::{Mode, Verdict};
    let mut completion = completion();
    for (verdict, terminal, exit) in [
        (Verdict::Pass, true, Some(17)),
        (Verdict::Fail, true, Some(101)),
        (Verdict::Interrupted, false, None),
        (Verdict::Unknown, true, None),
        (Verdict::Running, false, None),
    ] {
        completion.result = native_test_completion::NativeResult {
            verdict,
            terminal,
            exit,
            reason: "unchanged reason from origin".to_owned(),
        };
        completion.finished_at = None;
        for mode in [Mode::Focused, Mode::Full] {
            completion.mode = mode;
            for endpoints in [(None, None), (Some(source()), Some(source()))] {
                completion.input =
                    native_test_completion::Input::SourceId(native_test_completion::SourceInput {
                        kind: native_test_completion::SourceInputKind::SourceId,
                        source_id_before: endpoints.0,
                        source_id_after: endpoints.1,
                    });
                let wire =
                    serde_json::to_value(request(&completion).into_control_request()).unwrap();
                assert_eq!(
                    wire["completion"],
                    serde_json::to_value(&completion).unwrap()
                );
                assert!(
                    wire["completion"]["input"]
                        .get("source_id_before")
                        .is_some()
                );
                assert!(wire["completion"]["input"].get("source_id_after").is_some());
                assert!(wire["completion"].get("finished_at").is_none());
            }
        }
    }
    let wire = serde_json::to_value(request(&completion).into_control_request()).unwrap();
    for bad in ["missing", "mixed", "malformed"] {
        let mut malformed = wire.clone();
        match bad {
            "missing" => {
                malformed["completion"]["input"]
                    .as_object_mut()
                    .unwrap()
                    .remove("source_id_after");
            }
            "mixed" => malformed["completion"]["input"]["commit"] = json!(source().commit),
            _ => malformed["completion"]["input"]["source_id_before"] = json!({"commit":"partial"}),
        }
        assert!(
            serde_json::from_value::<EngramControlRequest>(malformed).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn qualification_receipts_preserve_engram_reasons_and_record_ids() {
    let request = request(&completion());
    let value = credited("focused");
    assert_eq!(
        serde_json::to_value(request.receipt(value.clone()).unwrap()).unwrap(),
        value
    );
    for (word, expected) in [
        ("producer_unsupported", Reason::ProducerUnsupported),
        ("association_stale", Reason::AssociationStale),
        ("result_conflict", Reason::ResultConflict),
        ("not_full_mode", Reason::NotFullMode),
        (
            "untracked_debris_pending_disposition",
            Reason::UntrackedDebrisPendingDisposition,
        ),
        ("result_not_pass", Reason::ResultNotPass),
        ("commit_mismatch", Reason::CommitMismatch),
        ("tree_mismatch", Reason::TreeMismatch),
        (
            "source_id_missing_or_changed",
            Reason::SourceIdMissingOrChanged,
        ),
    ] {
        let value = json!({"operation": record_id(), "origin": null, "verifications": [],
            "qualification": {"credited": false, "reason": word, "detail": "original producer detail"}});
        let receipt = request.receipt(value.clone()).unwrap();
        assert_eq!(receipt.qualification.reason, Some(expected));
        assert_eq!(serde_json::to_value(receipt).unwrap(), value);
    }
    for bad in [
        "stage", "count", "origin", "decision", "reason", "record", "extra",
    ] {
        let mut value = credited("focused");
        match bad {
            "stage" => value["verifications"][0]["stage"] = json!("full"),
            "count" => value["verifications"] = json!([]),
            "origin" => {
                value.as_object_mut().unwrap().remove("origin");
            }
            "decision" => value["qualification"]["credited"] = json!(false),
            "reason" => value["qualification"]["reason"] = json!("invalid_request"),
            "record" => value["verifications"][0]["record_id"] = json!("not-a-record"),
            _ => value["qualification"]["extra"] = json!(true),
        }
        assert!(request.receipt(value).is_err(), "{bad}");
    }
}

#[test]
fn authorized_test_ref_uses_the_same_source_completion_and_ordinary_frame_bound() {
    let mut completion = completion();
    completion.mode = native_test_completion::Mode::Full;
    let mut association = association(&source(), Purpose::LandingGate);
    association.intended_use = IntendedUse::AuthorizedTestRef {
        ref_name: "refs/heads/test/example".to_owned(),
        expected_tree: source().tree,
        purpose: Purpose::LandingGate,
    };
    let request = Request::new("test-ref-delivery".to_owned(), &completion, association).unwrap();
    request.receipt(credited("full")).unwrap();
    assert!(request.receipt(credited("focused")).is_err());
    let mut wire = serde_json::to_value(request.into_control_request()).unwrap();
    assert_eq!(wire["association"]["use"]["kind"], "authorized_test_ref");
    assert_eq!(
        wire["completion"],
        serde_json::to_value(&completion).unwrap()
    );
    // The remote operation keeps the ordinary 256 KiB INCLUDING newline,
    // rather than inheriting native verification_record's larger allowance.
    wire["operation_key"] = json!("");
    let overhead = serde_json::to_vec(&wire).unwrap().len();
    wire["operation_key"] = json!("k".repeat(ENGRAM_CONTROL_MAX_FRAME_BYTES - overhead - 1));
    let control: EngramControlRequest = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(
        encode_engram_control_request(&control).unwrap().len() + 1,
        ENGRAM_CONTROL_MAX_FRAME_BYTES
    );
    wire["operation_key"] = json!("k".repeat(ENGRAM_CONTROL_MAX_FRAME_BYTES - overhead));
    let oversized: EngramControlRequest = serde_json::from_value(wire).unwrap();
    assert!(encode_engram_control_request(&oversized).is_err());
}

#[test]
fn remote_capability_needs_its_own_current_work_session_connection_and_child() {
    use EngramVerificationRecordSupport as Support;
    let connection = EngramConnectionConfig {
        binary_path: "engram".into(),
        project_file: ".engram-project".into(),
        home: "fixture-home".into(),
        project_root: "fixture-project".into(),
        actor_id: "fixture-actor".to_owned(),
        actor_context: None,
        session_id: "session-destination".to_owned(),
    };
    let capabilities = EngramConnectionCapabilities::default();
    for (names, expected) in [
        (
            json!(["host_work_v1", "verification_record_v1"]),
            Support::EntryMissing,
        ),
        (
            json!(["host_work_v1", "remote_verification_record_v1"]),
            Support::Supported,
        ),
        (
            json!(["host_work_v1", "remote_verification_record_v1", 1]),
            Support::NotRead,
        ),
    ] {
        capabilities.note_reply(&connection, Some(7), &EngramControlRequest::WorkSessionRead {},
            &Ok(json!({"work_session":true,"session_id":connection.session_id,"capabilities":names})));
        assert_eq!(
            capabilities.work_session_remote_verification_record(&connection, Some(7)),
            expected
        );
        assert_eq!(
            capabilities.work_session_remote_verification_record(&connection, Some(8)),
            Support::NotRead
        );
        let mut changed = connection.clone();
        changed.home = "different-store-home".into();
        assert_eq!(
            capabilities.work_session_remote_verification_record(&changed, Some(7)),
            Support::NotRead
        );
    }
}
