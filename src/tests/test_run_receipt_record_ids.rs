// Test-only child of src/tests/test_run_registration.rs, using its evidence
// fixture. Owns the record-id forms a checkpoint receipt may carry when it
// settles a reported test association: Engram's current 32-hex ids, the
// legacy 64-hex ids, mixes of both, and the malformed, duplicate, conflicting
// or unmatched receipts that must still settle nothing. Also pins that a
// launcher's expectedFingerprint, a SHA-256, still needs all 64 digits.
// Does not own receipt delivery or recovery publication: both reach the
// shared settlement owner exercised here, and test_run_registration.rs and
// its other children cover the remaining settlement predicates.
use super::*;

fn hex(digit: &str, len: usize) -> String {
    digit.repeat(len)
}

fn receipt(
    checkpoint: Option<String>,
    verification: Vec<String>,
    cursor: i64,
    confirmed: i64,
) -> EngramCheckpointReceipt {
    let mut value = json!({
        "grant_id": "receipt-grant", "cursor": cursor, "confirmed_cursor": confirmed,
        "verification_evidence": verification,
    });
    if let Some(checkpoint) = checkpoint {
        value["checkpoint"] = json!(checkpoint);
    }
    serde_json::from_value(value).unwrap()
}

fn settle(
    fixture: &RunFixture,
    report: &EngramTurnReport,
    receipt: &EngramCheckpointReceipt,
    store: &EngramAuthorityStoreKey,
    binding: &EngramControlWorkBinding,
) {
    fixture
        .state
        .settle_test_run_verification_receipt(report, receipt, store, binding);
}

fn recorded(fixture: &RunFixture, id: &str) -> Vec<String> {
    fixture.summary(id).registration.verification_record_ids
}

fn checkpoint_of(fixture: &RunFixture, id: &str) -> Option<String> {
    let inner = fixture.state.inner.lock().unwrap();
    inner.test_run_observations.get(id).unwrap().evidence[0]
        .checkpoint_id
        .clone()
}

// Engram mints record ids as 32 hex digits, 64 for ids stored before that:
// every combination names the same exact association.
#[test]
fn current_mixed_and_legacy_record_id_lengths_settle() {
    for (label, checkpoint, verification) in [
        ("receipt-ids-32-32", hex("d", 32), hex("e", 32)),
        ("receipt-ids-32-64", hex("d", 32), hex("e", 64)),
        ("receipt-ids-64-32", hex("d", 64), hex("e", 32)),
        ("receipt-ids-64-64", hex("d", 64), hex("e", 64)),
    ] {
        let (fixture, id, store, binding, report) = evidence_fixture(label);
        let answer = receipt(Some(checkpoint.clone()), vec![verification.clone()], 3, 3);
        settle(&fixture, &report, &answer, &store, &binding);
        assert_eq!(recorded(&fixture, &id), vec![verification], "{label}");
        assert_eq!(checkpoint_of(&fixture, &id), Some(checkpoint), "{label}");
    }
}

#[test]
fn settled_32_hex_record_ids_survive_reload_and_projection() {
    let (fixture, id, store, binding, report) = evidence_fixture("receipt-ids-reload");
    let answer = receipt(Some(hex("d", 32)), vec![hex("e", 32)], 3, 3);
    settle(&fixture, &report, &answer, &store, &binding);
    assert_eq!(recorded(&fixture, &id), vec![hex("e", 32)]);

    let restored = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    *fixture.state.inner.lock().unwrap() = restored;
    assert_eq!(recorded(&fixture, &id), vec![hex("e", 32)]);
    assert_eq!(checkpoint_of(&fixture, &id), Some(hex("d", 32)));
}

// The duplicate-id refusal still decides: the same receipt with distinct ids
// settles, so length is not what refuses the repeated one.
#[test]
fn a_repeated_32_hex_record_id_stays_refused() {
    let (fixture, id, store, binding, mut report) = evidence_fixture("receipt-ids-duplicate");
    let input = report.verification_evidence[0].clone();
    report.verification_evidence.push(input);
    let repeated = receipt(Some(hex("d", 32)), vec![hex("e", 32), hex("e", 32)], 3, 3);
    settle(&fixture, &report, &repeated, &store, &binding);
    assert_eq!(recorded(&fixture, &id), Vec::<String>::new());
    assert_eq!(checkpoint_of(&fixture, &id), None);

    let distinct = receipt(Some(hex("d", 32)), vec![hex("e", 32), hex("f", 32)], 3, 3);
    settle(&fixture, &report, &distinct, &store, &binding);
    assert_eq!(recorded(&fixture, &id), vec![hex("e", 32)]);
}

#[test]
fn malformed_and_unmatched_receipts_stay_refused() {
    let cases = [
        (
            "receipt-refused-no-checkpoint",
            receipt(None, vec![hex("e", 32)], 3, 3),
        ),
        (
            "receipt-refused-checkpoint-31",
            receipt(Some(hex("d", 31)), vec![hex("e", 32)], 3, 3),
        ),
        (
            "receipt-refused-checkpoint-48",
            receipt(Some(hex("d", 48)), vec![hex("e", 32)], 3, 3),
        ),
        (
            "receipt-refused-checkpoint-upper",
            receipt(Some(hex("D", 32)), vec![hex("e", 32)], 3, 3),
        ),
        (
            "receipt-refused-checkpoint-nonhex",
            receipt(Some(hex("g", 32)), vec![hex("e", 32)], 3, 3),
        ),
        (
            "receipt-refused-verification-33",
            receipt(Some(hex("d", 32)), vec![hex("e", 33)], 3, 3),
        ),
        (
            "receipt-refused-verification-upper",
            receipt(Some(hex("d", 32)), vec![hex("E", 32)], 3, 3),
        ),
        (
            "receipt-refused-verification-nonhex",
            receipt(Some(hex("d", 32)), vec![hex("x", 32)], 3, 3),
        ),
        (
            "receipt-refused-cursor",
            receipt(Some(hex("d", 32)), vec![hex("e", 32)], 3, 2),
        ),
        (
            "receipt-refused-count",
            receipt(Some(hex("d", 32)), vec![hex("e", 32), hex("f", 32)], 3, 3),
        ),
        (
            "receipt-refused-empty",
            receipt(Some(hex("d", 32)), Vec::new(), 3, 3),
        ),
    ];
    for (label, answer) in cases {
        let (fixture, id, store, binding, report) = evidence_fixture(label);
        settle(&fixture, &report, &answer, &store, &binding);
        assert_eq!(recorded(&fixture, &id), Vec::<String>::new(), "{label}");
        assert_eq!(checkpoint_of(&fixture, &id), None, "{label}");
    }

    // Well-formed 32-hex ids still settle nothing for a producer whose
    // action no longer matches the association.
    let (fixture, id, store, binding, mut report) = evidence_fixture("receipt-refused-producer");
    report.observations[0].action_fingerprint = hex("9", 64);
    let answer = receipt(Some(hex("d", 32)), vec![hex("e", 32)], 3, 3);
    settle(&fixture, &report, &answer, &store, &binding);
    assert_eq!(recorded(&fixture, &id), Vec::<String>::new());
}

// A settled association keeps its record: a later receipt naming another id,
// in either length, does not replace it.
#[test]
fn a_settled_record_id_is_not_replaced_by_a_conflicting_receipt() {
    let (fixture, id, store, binding, report) = evidence_fixture("receipt-ids-conflict");
    let first = receipt(Some(hex("d", 64)), vec![hex("e", 64)], 3, 3);
    settle(&fixture, &report, &first, &store, &binding);
    assert_eq!(recorded(&fixture, &id), vec![hex("e", 64)]);
    for other in [hex("f", 32), hex("f", 64)] {
        let answer = receipt(Some(hex("c", 32)), vec![other.clone()], 4, 4);
        settle(&fixture, &report, &answer, &store, &binding);
        assert_eq!(recorded(&fixture, &id), vec![hex("e", 64)], "{other}");
        assert_eq!(checkpoint_of(&fixture, &id), Some(hex("d", 64)), "{other}");
    }
}

// A launcher's expectedFingerprint is a SHA-256, not an Engram record id: a
// 32-digit value is still not one.
#[test]
fn expected_fingerprint_still_needs_64_hex_digits() {
    let fixture = RunFixture::new("receipt-ids-expected-fingerprint");
    for (run_id, fingerprint, kept) in [
        ("test-fingerprint-64", hex("a", 64), true),
        ("test-fingerprint-32", hex("a", 32), false),
    ] {
        let mut request = full_request(&fixture.root, "2026-10-30T00:00:00Z");
        request["expectedFingerprint"] = json!(fingerprint);
        let directory = write_run(&fixture.runs_dir(), run_id, request, Some(passed_results()));
        let disk = TestRunDisk::read(&directory, None).unwrap();
        assert_eq!(disk.expected_fingerprint.is_some(), kept, "{run_id}");
    }
}
