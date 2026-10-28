// Owns the regression for a scripted Engram budget-clock watchdog that fires
// while a persistence fence is still queued. Unwinding drops the fence, the
// fence notifies the clock (`PersistFence::finish`), and that notification
// must not become a second panic that aborts the whole test process and hides
// every later result. Does not own the scripted waits themselves
// (src/engram_budget.rs) or the fence contracts (src/tests/persist_fence.rs).
// New file, not split from another.
use super::*;

const CASE: &str = "tests::engram_budget_watchdog_unwind::a_watchdog_expiry_with_a_queued_fence_fails_the_test_without_aborting";
const CHILD: &str = "TERMAL_TEST_WATCHDOG_UNWIND_CHILD";

/// The watchdog's panic is the child's whole outcome, so the child runs in its
/// own process: an abort there cannot take this test run down with it.
#[test]
fn a_watchdog_expiry_with_a_queued_fence_fails_the_test_without_aborting() {
    if std::env::var(CHILD).as_deref() == Ok(CASE) {
        watchdog_expires_with_a_queued_fence();
    }
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", CASE, "--test-threads=1", "--nocapture"])
        .env(CHILD, CASE);
    let (status, stderr) = phase_sync::CapturedStderrProcess::spawn(&mut command)
        .wait_with_stderr("scripted watchdog unwind child");
    assert!(
        stderr.contains("scripted Engram driver watchdog expired"),
        "the child must fail on its driver's watchdog: {stderr}"
    );
    assert!(
        !stderr.contains("budget event mutex poisoned"),
        "the queued fence's notify must not panic a second time while unwinding: {stderr}"
    );
    assert_eq!(
        status.code(),
        Some(101),
        "the watchdog must be an ordinary test failure, not a process abort: {status:?}\n{stderr}"
    );
}

/// The child's body. A fence waits on a scripted clock that never advances,
/// with the fence still queued in this frame, as when a fixture hangs. Its
/// driver's watchdog fails the test, and unwinding then drops the fence.
fn watchdog_expires_with_a_queued_fence() -> ! {
    let clock = EngramBudgetClock::scripted_with_watchdog(Duration::from_millis(100));
    let (_queued, waiter) = PersistFence::new_with_clock(
        PersistFenceTarget::TestRunCardsEpoch("watchdog-unwind".to_owned()),
        clock.now() + Duration::from_secs(1),
        clock.clone(),
    );
    let _ = waiter.wait();
    unreachable!("a scripted clock that never advances must expire its driver's watchdog");
}
