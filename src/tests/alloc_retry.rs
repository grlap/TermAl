// A failed allocation waits for memory for a bounded time instead of letting
// the host abort: the wrapper retries the inner allocator with a growing pause,
// asks for one notice when it starts waiting, and returns null only after a
// final attempt at the bound. The clock and the inner allocator are injected,
// so no test sleeps or exhausts real memory. The production notice path is
// driven directly, under a per-thread allocation probe.
use crate::alloc_retry::{
    ALLOC_RETRY_BOUND, RetryClock, RetryOnNull, SystemRetryClock, WAIT_NOTICE_CAPACITY,
    WAIT_NOTICE_DRAINS, WAIT_NOTICE_LINES_WRITTEN, WAIT_NOTICE_WRITER_ALLOCATIONS,
    drain_wait_notices, probe, start_wait_notice_writer, take_pending_wait_notices_for_test,
    wait_notice_writer_registered,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Time advances only when the retry sleeps. `events` records, in order, each
/// inner-allocator attempt, notice request and pause; `notices` the size each
/// notice reports.
struct FakeClock {
    start: Instant,
    elapsed: Cell<Duration>,
    sleeps: RefCell<Vec<Duration>>,
    notices: RefCell<Vec<usize>>,
    events: RefCell<Vec<&'static str>>,
}

impl FakeClock {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            elapsed: Cell::new(Duration::ZERO),
            sleeps: RefCell::new(Vec::new()),
            notices: RefCell::new(Vec::new()),
            events: RefCell::new(Vec::new()),
        }
    }
}

impl RetryClock for &FakeClock {
    fn now(&self) -> Instant {
        self.start + self.elapsed.get()
    }

    fn sleep(&self, pause: Duration) {
        self.events.borrow_mut().push("sleep");
        self.sleeps.borrow_mut().push(pause);
        self.elapsed.set(self.elapsed.get() + pause);
    }

    fn notice(&self, size: usize) {
        self.events.borrow_mut().push("notice");
        self.notices.borrow_mut().push(size);
    }
}

/// What a flaky inner allocator does: fail its first `failures` requests (or
/// every request), then serve from the system allocator. Records the time of
/// every attempt.
struct FlakyState<'a> {
    failures: Cell<usize>,
    always_fail: bool,
    clock: &'a FakeClock,
    attempts: RefCell<Vec<Duration>>,
    /// The inner method each attempt went through.
    methods: RefCell<Vec<&'static str>>,
}

impl<'a> FlakyState<'a> {
    fn failing(failures: usize, clock: &'a FakeClock) -> Self {
        Self {
            failures: Cell::new(failures),
            always_fail: false,
            clock,
            attempts: RefCell::new(Vec::new()),
            methods: RefCell::new(Vec::new()),
        }
    }

    fn exhausted(clock: &'a FakeClock) -> Self {
        Self {
            always_fail: true,
            ..Self::failing(0, clock)
        }
    }

    fn should_fail(&self, method: &'static str) -> bool {
        self.methods.borrow_mut().push(method);
        self.clock.events.borrow_mut().push("attempt");
        self.attempts.borrow_mut().push(self.clock.elapsed.get());
        if self.always_fail {
            return true;
        }
        let left = self.failures.get();
        if left > 0 {
            self.failures.set(left - 1);
            return true;
        }
        false
    }
}

/// The inner allocator handed to the wrapper; the test keeps its state.
struct Flaky<'s, 'a>(&'s FlakyState<'a>);

unsafe impl GlobalAlloc for Flaky<'_, '_> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if self.0.should_fail("alloc") {
            return std::ptr::null_mut();
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if self.0.should_fail("alloc_zeroed") {
            return std::ptr::null_mut();
        }
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if self.0.should_fail("realloc") {
            return std::ptr::null_mut();
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

fn layout(size: usize) -> Layout {
    Layout::from_size_align(size, 8).expect("a valid layout")
}

#[test]
fn an_allocation_that_succeeds_first_time_never_waits() {
    let clock = FakeClock::new();
    let inner = FlakyState::failing(0, &clock);
    let allocator = RetryOnNull::with_clock(Flaky(&inner), &clock);
    let ptr = unsafe { allocator.alloc(layout(64)) };
    assert!(!ptr.is_null());
    unsafe { allocator.dealloc(ptr, layout(64)) };
    assert_eq!(inner.attempts.borrow().len(), 1);
    assert!(clock.sleeps.borrow().is_empty());
    assert!(clock.notices.borrow().is_empty());
}

#[test]
fn a_transient_failure_is_waited_out_with_one_notice() {
    let clock = FakeClock::new();
    let inner = FlakyState::failing(5, &clock);
    let allocator = RetryOnNull::with_clock(Flaky(&inner), &clock);
    let ptr = unsafe { allocator.alloc(layout(2_719_744)) };
    assert!(
        !ptr.is_null(),
        "the allocation succeeds once memory returns"
    );
    unsafe { allocator.dealloc(ptr, layout(2_719_744)) };

    assert_eq!(
        inner.attempts.borrow().len(),
        6,
        "five failures, then success"
    );
    assert_eq!(
        clock.sleeps.borrow().len(),
        5,
        "one pause before each retry"
    );
    assert!(clock.elapsed.get() < ALLOC_RETRY_BOUND);
    assert_eq!(
        *clock.notices.borrow(),
        [2_719_744],
        "one notice for the whole wait, reporting the failed request's size"
    );
}

/// Waits for `condition`, failing the test after a generous guard.
fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting: {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn the_production_notice_path_allocates_nothing_and_reaches_stderr_from_the_writer() {
    // The only test that touches the process-wide notice state or starts the
    // writer. On the failing thread a notice is only a request: atomics, and a
    // wake once a writer runs. No writer runs yet, so nothing is written.
    let requested = probe::allocations_during(|| {
        SystemRetryClock.notice(4096);
        SystemRetryClock.notice(2_719_744);
    });
    assert_eq!(requested, 0, "requesting a notice must not allocate");

    // The writer's drain formats each line on its stack: no allocation.
    let mut lines = 0;
    let mut last = [0u8; WAIT_NOTICE_CAPACITY];
    let mut last_len = 0;
    let drained = probe::allocations_during(|| {
        drain_wait_notices(|line| {
            lines += 1;
            last[..line.len()].copy_from_slice(line);
            last_len = line.len();
            true
        });
    });
    assert_eq!(
        drained, 0,
        "formatting and draining notices must not allocate"
    );
    assert_eq!(lines, 1, "requests waiting together share one line");
    assert_eq!(
        take_pending_wait_notices_for_test(),
        0,
        "the drain took both"
    );
    let line = &last[..last_len];
    assert_eq!(line.iter().filter(|byte| **byte == b'\n').count(), 1);
    assert_eq!(line.last(), Some(&b'\n'), "the notice is one whole line");
    let reports = |value: u64| {
        let digits = value.to_string();
        line.windows(digits.len())
            .any(|window| window == digits.as_bytes())
    };
    assert!(
        reports(2_719_744),
        "the notice reports the largest waiting request"
    );
    assert!(
        reports(ALLOC_RETRY_BOUND.as_secs()),
        "the notice reports the actual bound"
    );

    // The probe itself sees an ordinary allocation, so a zero above is real.
    let seen = probe::allocations_during(|| drop(std::hint::black_box(vec![0u8; 64])));
    assert!(seen >= 1, "the probe counts this thread's allocations");

    // Start the production writer as main() does and let it finish its first
    // drain, so the request below is delivered only if it wakes the parked
    // writer. The request still allocates nothing on this thread, and the
    // writer writes it through the real stderr sink (one notice line on this
    // test process's stderr is expected; only a successful write counts)
    // without allocating on its own thread.
    start_wait_notice_writer();
    wait_until("the writer registers", wait_notice_writer_registered);
    wait_until("the writer's first drain", || {
        WAIT_NOTICE_DRAINS.load(Ordering::SeqCst) >= 1
    });
    let written = WAIT_NOTICE_LINES_WRITTEN.load(Ordering::SeqCst);
    let woke = probe::allocations_during(|| SystemRetryClock.notice(4096));
    assert_eq!(
        woke, 0,
        "requesting a notice and waking the writer must not allocate"
    );
    wait_until("the writer writes the notice", || {
        WAIT_NOTICE_LINES_WRITTEN.load(Ordering::SeqCst) > written
    });
    assert_eq!(
        WAIT_NOTICE_WRITER_ALLOCATIONS.load(Ordering::SeqCst),
        0,
        "the writer formats and writes notices without allocating"
    );
}

#[test]
fn a_test_only_shorter_bound_gives_up_at_that_bound() {
    // The host always waits ALLOC_RETRY_BOUND; tests may shorten it.
    let shortened = Duration::from_secs(2);
    assert!(shortened < ALLOC_RETRY_BOUND);
    let clock = FakeClock::new();
    let inner = FlakyState::exhausted(&clock);
    let allocator = RetryOnNull::with_clock(Flaky(&inner), &clock).with_bound(shortened);
    let ptr = unsafe { allocator.alloc(layout(4096)) };
    assert!(
        ptr.is_null(),
        "null is returned once the shortened bound has passed"
    );
    assert_eq!(
        clock.elapsed.get(),
        shortened,
        "it waits the shortened bound, and no longer"
    );
    assert_eq!(
        inner.attempts.borrow().last().copied(),
        Some(shortened),
        "a final attempt is made at the shortened bound"
    );
    assert_eq!(clock.notices.borrow().len(), 1);
}

#[test]
fn each_failed_request_gives_one_notice_before_any_further_inner_call() {
    let clock = FakeClock::new();
    let inner = FlakyState::failing(2, &clock);
    let allocator = RetryOnNull::with_clock(Flaky(&inner), &clock);
    let first = unsafe { allocator.alloc(layout(1024)) };
    assert!(!first.is_null());
    assert_eq!(
        *clock.events.borrow(),
        ["attempt", "notice", "sleep", "attempt", "sleep", "attempt"],
        "the notice follows the first failure, and the inner allocator is not \
         called again until after the first pause"
    );

    inner.failures.set(1);
    let second = unsafe { allocator.alloc(layout(1024)) };
    assert!(!second.is_null());
    assert_eq!(
        clock.events.borrow()[6..],
        ["attempt", "notice", "sleep", "attempt"],
        "a later failed request gives its own single notice"
    );
    assert_eq!(clock.notices.borrow().len(), 2);
    unsafe {
        allocator.dealloc(first, layout(1024));
        allocator.dealloc(second, layout(1024));
    }
}

#[test]
fn an_exhausted_allocator_gives_up_only_at_the_bound() {
    assert!(ALLOC_RETRY_BOUND >= Duration::from_secs(60));
    let clock = FakeClock::new();
    let inner = FlakyState::exhausted(&clock);
    let allocator = RetryOnNull::with_clock(Flaky(&inner), &clock);
    let ptr = unsafe { allocator.alloc(layout(4096)) };
    assert!(ptr.is_null(), "null is returned once the bound has passed");

    assert_eq!(
        clock.elapsed.get(),
        ALLOC_RETRY_BOUND,
        "it waits the whole bound, and no longer"
    );
    let attempts = inner.attempts.borrow();
    assert_eq!(
        attempts.last().copied(),
        Some(ALLOC_RETRY_BOUND),
        "a final attempt is made at the bound"
    );
    assert!(
        attempts.windows(2).all(|pair| pair[0] < pair[1]),
        "every retry follows a pause"
    );
    assert!(
        clock
            .sleeps
            .borrow()
            .iter()
            .all(|pause| *pause <= Duration::from_secs(1)),
        "pauses stay short, so returning memory is noticed promptly"
    );
    assert_eq!(clock.notices.borrow().len(), 1);
}

#[test]
fn a_failed_realloc_is_retried_and_keeps_the_block() {
    let clock = FakeClock::new();
    let inner = FlakyState::failing(0, &clock);
    let allocator = RetryOnNull::with_clock(Flaky(&inner), &clock);
    let old = layout(16);
    let ptr = unsafe { allocator.alloc(old) };
    assert!(!ptr.is_null());
    unsafe { std::ptr::copy_nonoverlapping(b"live state bytes".as_ptr(), ptr, 16) };

    inner.failures.set(3);
    let grown = unsafe { allocator.realloc(ptr, old, 4096) };
    assert!(!grown.is_null(), "the realloc succeeds once memory returns");
    let kept = unsafe { std::slice::from_raw_parts(grown, 16) };
    assert_eq!(kept, b"live state bytes", "the contents survive the wait");
    unsafe { allocator.dealloc(grown, layout(4096)) };
    assert_eq!(clock.sleeps.borrow().len(), 3);
    assert_eq!(clock.notices.borrow().len(), 1);
    assert_eq!(
        inner.methods.borrow()[1..],
        ["realloc"; 4],
        "every retry of the realloc is a realloc of the same block"
    );
}

#[test]
fn a_failed_zeroed_allocation_is_retried_and_zeroed() {
    let clock = FakeClock::new();
    let inner = FlakyState::failing(2, &clock);
    let allocator = RetryOnNull::with_clock(Flaky(&inner), &clock);
    let ptr = unsafe { allocator.alloc_zeroed(layout(256)) };
    assert!(!ptr.is_null());
    let bytes = unsafe { std::slice::from_raw_parts(ptr, 256) };
    assert!(bytes.iter().all(|byte| *byte == 0));
    unsafe { allocator.dealloc(ptr, layout(256)) };
    assert_eq!(clock.sleeps.borrow().len(), 2);
    assert_eq!(
        *inner.methods.borrow(),
        ["alloc_zeroed"; 3],
        "every retry asks the inner allocator for zeroed memory"
    );
}
