use super::*;

#[test]
fn a_bounded_capture_counts_live_threads_and_refuses_past_the_limit() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let live = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(AtomicUsize::new(0));
    let budget = Duration::ZERO;
    // Two captures that stall until released overrun their budget: nothing
    // is returned, and their threads stay counted while they live.
    let mut releases = Vec::new();
    for _ in 0..2 {
        let (release, stalled) = std::sync::mpsc::channel::<()>();
        releases.push(release);
        let started = started.clone();
        let revision = bounded_source_capture(
            &live,
            &Arc::new(CaptureLifetime::default()),
            2,
            budget,
            move || {
                started.fetch_add(1, Ordering::SeqCst);
                let _ = stalled.recv();
                Some("late".to_owned())
            },
        );
        assert_eq!(revision, None, "an overrun capture declares nothing");
    }
    assert_eq!(live.load(Ordering::SeqCst), 2);
    // At the limit nothing new starts. The refused capture has its own
    // counter: the stalled ones may reach theirs at any moment.
    let refused_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let refused =
        bounded_source_capture(&live, &Arc::new(CaptureLifetime::default()), 2, budget, {
            let refused_ran = refused_ran.clone();
            move || {
                refused_ran.store(true, Ordering::SeqCst);
                Some("never".to_owned())
            }
        });
    assert_eq!(refused, None);
    assert!(!refused_ran.load(Ordering::SeqCst), "no thread started");
    // Released, the threads end and are uncounted.
    drop(releases);
    wait_for_no_live_captures(&live);
    // A capture within its budget returns its revision and leaves no count.
    let taken = bounded_source_capture(
        &live,
        &Arc::new(CaptureLifetime::default()),
        2,
        TEST_PHASE_DEADLOCK_GUARD,
        || Some("captured".to_owned()),
    );
    assert_eq!(taken.as_deref(), Some("captured"));
    wait_for_no_live_captures(&live);
}

#[test]
fn a_bounded_capture_that_panics_is_uncounted() {
    let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let revision: Option<String> = bounded_source_capture(
        &live,
        &Arc::new(CaptureLifetime::default()),
        1,
        TEST_PHASE_DEADLOCK_GUARD,
        || panic!("a capture that panics"),
    );
    assert_eq!(revision, None);
    wait_for_no_live_captures(&live);
    // The one slot is free again.
    let taken = bounded_source_capture(
        &live,
        &Arc::new(CaptureLifetime::default()),
        1,
        TEST_PHASE_DEADLOCK_GUARD,
        || Some("after".to_owned()),
    );
    assert_eq!(taken.as_deref(), Some("after"));
}

/// Waits until every capture thread counted in `live` has ended; its guard
/// lowers the count as the thread unwinds, just after it answers.
fn wait_for_no_live_captures(live: &Arc<std::sync::atomic::AtomicUsize>) {
    let deadline = std::time::Instant::now() + TEST_PHASE_DEADLOCK_GUARD;
    while live.load(std::sync::atomic::Ordering::SeqCst) != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "capture threads never ended"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
