// Owns the tests of the closing capture's test-build diagnostic line
// (`unadmitted_capture_diagnostic`, src/engram_unadmitted_closing.rs). The
// line is printed only for an attempt worth reading about: one whose source
// measurement failed, or one that took longer than its capture budget. It
// keeps the attempt, elapsed and budget fields that diagnosed a capture
// starved by slow I/O. Does not own the capture itself. New file, not split
// from another.
use super::*;

const BUDGET: Duration = Duration::from_millis(250);

#[test]
fn a_capture_within_its_budget_prints_no_diagnostic() {
    assert_eq!(
        unadmitted_capture_diagnostic("session-1", 0, BUDGET, Duration::from_millis(40), true),
        None
    );
    assert_eq!(
        unadmitted_capture_diagnostic("session-1", 3, BUDGET, BUDGET, true),
        None,
        "an attempt that used exactly its budget is within it"
    );
}

#[test]
fn an_over_budget_capture_prints_its_attempt_elapsed_and_budget() {
    let line = unadmitted_capture_diagnostic(
        "session-1",
        2,
        BUDGET,
        Duration::from_micros(1_756_975),
        true,
    )
    .expect("an attempt over its budget prints a diagnostic");
    assert_eq!(
        line,
        "UNADMITTED_CAPTURE_DIAGNOSTIC session=session-1 attempt=2 budget_us=250000 elapsed_us=1756975 measured=true"
    );
}

#[test]
fn a_failed_measurement_prints_a_diagnostic_even_within_its_budget() {
    let line = unadmitted_capture_diagnostic(
        "session-1",
        1,
        BUDGET,
        Duration::from_millis(10),
        false,
    )
    .expect("a failed measurement prints a diagnostic");
    assert_eq!(
        line,
        "UNADMITTED_CAPTURE_DIAGNOSTIC session=session-1 attempt=1 budget_us=250000 elapsed_us=10000 measured=false"
    );
}
