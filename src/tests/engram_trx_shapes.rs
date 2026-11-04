//! Real producer captures: NUnit/VSTest, MSTest/VSTest and MSTest/MTP.
//! Owns the finite report-shape regression matrix, not command recognition
//! or artifact freshness. The fixture README records commands and sanitizing.
use super::*;

fn assert_capture(
    bytes: &[u8],
    expected_outcome: &str,
    expected_counts: (u64, u64, u64, u64),
    expected_verdict: &str,
) {
    let reading = engram_read_trx(bytes);
    assert_eq!(reading.outcome.as_deref(), Some(expected_outcome));
    let counters = reading.counters.expect("captured report has counters");
    assert_eq!(
        (
            counters.get("total"),
            counters.get("executed"),
            counters.get("passed"),
            counters.get("failed"),
        ),
        expected_counts,
    );
    let actual = match &reading.verdict {
        EngramTrxVerdict::Pass => "PASS",
        EngramTrxVerdict::Fail(_) => "FAIL",
        EngramTrxVerdict::Unknown(_) => "UNKNOWN",
    };
    assert_eq!(actual, expected_verdict, "{:?}", reading.verdict);
    if expected_verdict == "UNKNOWN" {
        assert_eq!(
            reading.verdict,
            EngramTrxVerdict::Unknown("no tests executed".to_owned()),
        );
    }
}

#[test]
fn captured_nunit_vstest_pass_fail_and_zero() {
    assert_capture(
        include_bytes!("fixtures/declared-trx/runner-shapes/nunit/pass.trx"),
        "Completed",
        (1, 1, 1, 0),
        "PASS",
    );
    assert_capture(
        include_bytes!("fixtures/declared-trx/runner-shapes/nunit/fail.trx"),
        "Failed",
        (1, 1, 0, 1),
        "FAIL",
    );
    assert_capture(
        include_bytes!("fixtures/declared-trx/runner-shapes/nunit/zero.trx"),
        "Completed",
        (0, 0, 0, 0),
        "UNKNOWN",
    );
}

#[test]
fn captured_mstest_vstest_pass_fail_and_zero() {
    assert_capture(
        include_bytes!("fixtures/declared-trx/runner-shapes/mstest/pass.trx"),
        "Completed",
        (1, 1, 1, 0),
        "PASS",
    );
    assert_capture(
        include_bytes!("fixtures/declared-trx/runner-shapes/mstest/fail.trx"),
        "Failed",
        (1, 1, 0, 1),
        "FAIL",
    );
    assert_capture(
        include_bytes!("fixtures/declared-trx/runner-shapes/mstest/zero.trx"),
        "Completed",
        (0, 0, 0, 0),
        "UNKNOWN",
    );
}

#[test]
fn captured_mstest_mtp_pass_fail_and_zero() {
    assert_capture(
        include_bytes!("fixtures/declared-trx/runner-shapes/mtp/pass.trx"),
        "Completed",
        (1, 1, 1, 0),
        "PASS",
    );
    assert_capture(
        include_bytes!("fixtures/declared-trx/runner-shapes/mtp/fail.trx"),
        "Failed",
        (1, 1, 0, 1),
        "FAIL",
    );
    // This producer treats an empty run as failed (native exit 8), unlike
    // VSTest's Completed/exit-0 empty run. Never override that failure.
    assert_capture(
        include_bytes!("fixtures/declared-trx/runner-shapes/mtp/zero.trx"),
        "Failed",
        (0, 0, 0, 0),
        "FAIL",
    );
}
