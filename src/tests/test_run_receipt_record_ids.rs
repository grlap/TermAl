// Native producer receipt ids are opaque 32/64-hex records. No checkpoint
// converts observations into verification credit. Reuses the sender fixture
// and its actual registration, own-row settlement and persistence path.
use super::*;

fn record_identifier(len: usize) -> String {
    let first = Uuid::new_v4().simple().to_string();
    match len {
        32 => first,
        64 => format!("{first}{}", Uuid::new_v4().simple()),
        _ => panic!("only supported record-id lengths belong in this fixture"),
    }
}

fn native_fixture(label: &str) -> (RunFixture, RecordRun, String, Arc<RecordingTransport>, String) {
    let fixture = RunFixture::new(label);
    let (session, transport) = engram_session(&fixture, true);
    let mut run = record_run(&fixture, &format!("test-{label}"),
        commit_request(&fixture.root), commit_results());
    initiated_by(&mut run, &session);
    register(&fixture, &[&run]);
    let execution_id = attach_launch_row(&fixture, &run);
    (fixture, run, session, transport, execution_id)
}

fn native_receipt(lengths: [usize; 5]) -> (Value, Vec<String>) {
    let ids = lengths.into_iter().map(record_identifier).collect::<Vec<_>>();
    let mut receipt = receipt_for("failed");
    for (entry, id) in receipt["verifications"].as_array_mut().unwrap().iter_mut().zip(&ids) {
        entry["record_id"] = json!(id);
    }
    (receipt, ids)
}

fn deliver_receipt(fixture: &RunFixture, transport: &RecordingTransport, session: &str, receipt: Value) {
    transport.script(Ok(work_supported(session)));
    transport.script(Ok(receipt));
    fixture.state.engram_verification_records_tick();
}

fn expected_records(ids: &[String]) -> Vec<Option<String>> {
    // The first two stages are builds. Only the three test stages attach
    // records to this launch row; every stage remains in the native receipt.
    ids[2..].iter().cloned().map(Some).collect()
}

#[test]
fn current_mixed_and_legacy_record_id_lengths_settle() {
    for (label, lengths) in [
        ("receipt-ids-current", [32; 5]),
        ("receipt-ids-stored", [64; 5]),
        ("receipt-ids-mixed", [32, 64, 32, 64, 32]),
    ] {
        let (fixture, run, session, transport, execution_id) = native_fixture(label);
        let (receipt, ids) = native_receipt(lengths);
        deliver_receipt(&fixture, &transport, &session, receipt);
        assert_eq!(row_records(&fixture, &execution_id), expected_records(&ids), "{label}");
        assert!(matches!(engram_verification_record_prepare(&current(&fixture, &run)),
            EngramVerificationRecordPrepared::Finished));
        assert_eq!(transport.sent_operations(), ["work_session_read", "verification_record"]);
    }
}

#[test]
fn settled_32_hex_record_ids_survive_reload_and_projection() {
    let (fixture, run, session, transport, execution_id) = native_fixture("receipt-ids-reload");
    let (receipt, ids) = native_receipt([32; 5]);
    deliver_receipt(&fixture, &transport, &session, receipt);
    assert_eq!(row_records(&fixture, &execution_id), expected_records(&ids));
    fixture.state.shutdown_persist_blocking();
    let restored = load_state(fixture.state.persistence_path.as_path()).unwrap().unwrap();
    *fixture.state.inner.lock().unwrap() = restored;
    assert_eq!(row_records(&fixture, &execution_id), expected_records(&ids));
    assert!(matches!(current(&fixture, &run).verification_record,
        Some(TestRunVerificationAck::Settled { .. })));
    let inner = fixture.state.inner.lock().unwrap();
    let row = inner.test_run_observations.get(&execution_id).unwrap();
    let projected = test_run_observation_summary(&*inner, row);
    assert_eq!(projected.registration.verification_record_ids, ids[2..].to_vec(),
        "the shared summary projection preserves the actual opaque ids");
    assert!(row.evidence.iter().all(|entry| entry.checkpoint_id.is_none()),
        "native registration creates no checkpoint association");
}

#[test]
fn a_repeated_32_hex_record_id_stays_refused() {
    for repeated in [true, false] {
        let label = if repeated { "receipt-ids-duplicate" } else { "receipt-ids-distinct" };
        let (fixture, _run, session, transport, execution_id) = native_fixture(label);
        let (mut receipt, ids) = native_receipt([32; 5]);
        if repeated {
            receipt["verifications"][1]["record_id"] = json!(ids[0]);
        }
        deliver_receipt(&fixture, &transport, &session, receipt);
        assert_eq!(row_records(&fixture, &execution_id), if repeated {
            Vec::new()
        } else { expected_records(&ids) }, "{label}");
    }
}

#[test]
fn malformed_and_unmatched_receipts_stay_refused() {
    for cause in ["short", "long", "upper", "nonhex", "empty", "count", "stage", "producer"] {
        let (fixture, _run, session, transport, execution_id) =
            native_fixture(&format!("receipt-refused-{cause}"));
        let (mut receipt, ids) = native_receipt([32; 5]);
        match cause {
            "short" => receipt["verifications"][0]["record_id"] = json!(&ids[0][..31]),
            "long" => receipt["verifications"][0]["record_id"] = json!(format!("{}0", ids[0])),
            "upper" => receipt["verifications"][0]["record_id"] = json!(format!("A{}", &ids[0][1..])),
            "nonhex" => receipt["verifications"][0]["record_id"] = json!(format!("g{}", &ids[0][1..])),
            "empty" => receipt["verifications"] = json!([]),
            "count" => { receipt["verifications"].as_array_mut().unwrap().pop(); },
            "stage" => receipt["verifications"][0]["stage"] = json!("another-stage"),
            "producer" => receipt["operation"] = json!(record_identifier(32)),
            _ => unreachable!(),
        }
        deliver_receipt(&fixture, &transport, &session, receipt);
        assert!(row_records(&fixture, &execution_id).is_empty(), "{cause}");
    }

    let (fixture, run, session, transport, execution_id) = native_fixture("receipt-unmatched-association");
    let frozen = ready(&run.registration);
    let producers = engram_verification_record_producers(&run.registration.run_id, &frozen.body);
    let producer = &producers[0].1;
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner.test_run_observations.update(&execution_id, |row| {
            row.evidence.push(TestRunEvidenceAssociation {
                store: run.registration.authority_store.clone().unwrap(),
                binding: run.registration.work_binding.clone().unwrap(),
                source_basis: producer.source_basis.clone().unwrap(),
                producer_observation_id: producer.observation_id.clone(),
                action_fingerprint: sha256_hex(b"a different observed command"),
                reported_outcome: producer.outcome,
                verification_record_id: None,
                checkpoint_id: None,
                confirmed_cursor: None,
                lifecycle_read: None,
            });
        }).expect("the original launch row exists");
    }
    let (receipt, ids) = native_receipt([32; 5]);
    deliver_receipt(&fixture, &transport, &session, receipt);
    assert_eq!(row_records(&fixture, &execution_id),
        vec![None, Some(ids[3].clone()), Some(ids[4].clone())],
        "the different command stays unsettled while independent matching stages settle");
}

#[test]
fn a_settled_record_id_is_not_replaced_by_a_conflicting_receipt() {
    let (fixture, run, session, transport, execution_id) = native_fixture("receipt-ids-conflict");
    let frozen = ready(&run.registration);
    let (first, ids) = native_receipt([64; 5]);
    deliver_receipt(&fixture, &transport, &session, first);
    assert_eq!(row_records(&fixture, &execution_id), expected_records(&ids));
    let sent = transport.sent().len();
    for lengths in [[32; 5], [64; 5]] {
        let (other, _) = native_receipt(lengths);
        let answer: EngramVerificationRecordReceipt = serde_json::from_value(other.clone()).unwrap();
        engram_verification_record_receipt_check(&frozen.body, &answer).unwrap();
        fixture.state.settle_engram_verification_record(&run.registration, &frozen.body, &answer);
        assert_eq!(row_records(&fixture, &execution_id), expected_records(&ids),
            "even a validated conflicting receipt cannot overwrite original records");
        transport.script(Ok(other));
        fixture.state.engram_verification_records_tick();
        assert_eq!(transport.sent().len(), sent, "a settled run is never reissued");
        assert_eq!(row_records(&fixture, &execution_id), expected_records(&ids));
    }
}

#[test]
fn expected_fingerprint_still_needs_64_hex_digits() {
    let fixture = RunFixture::new("receipt-ids-expected-fingerprint");
    for (run_id, fingerprint, kept) in [
        ("test-fingerprint-64", sha256_hex(b"fingerprint content"), true),
        ("test-fingerprint-32", record_identifier(32), false),
    ] {
        let mut request = commit_request(&fixture.root);
        request["expectedFingerprint"] = json!(fingerprint);
        let directory = write_run(&fixture.runs_dir(), run_id, request, Some(commit_results()));
        let disk = TestRunDisk::read(&directory, None).unwrap();
        assert_eq!(disk.expected_fingerprint.is_some(), kept, "{run_id}");
    }
}
