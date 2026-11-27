// Owns: the host's global-allocator wrapper that waits out a transient
// allocation failure instead of letting the process abort. When the inner
// allocator returns null (for example while another process has exhausted
// system commit), `RetryOnNull` asks for one stderr notice, then retries with
// a growing pause until `ALLOC_RETRY_BOUND` has passed, and returns null only
// after a final attempt at the bound; the standard library then reports the
// failure and aborts exactly as before.
//
// Accepted trade-off: the waiting thread may hold locks (StateInner among
// them) for up to the bound, so the host can stall that long. A stall that
// recovers keeps every session's live state; an abort loses it. Fallible
// reservations (try_reserve and the std reads built on it) wait the same
// way before reporting their error, since an allocator cannot tell them
// apart. The notice bypasses the host log's lock and timestamp, so it can
// interleave with a log line written at the same moment.
//
// Nothing on the waiting path allocates or performs I/O: it reads the clock,
// sleeps, and hands the notice to a dedicated writer thread through atomics.
// That thread, started before the runtime, formats the notice into a stack
// buffer and writes it through stderr's raw handle, so a stalled stderr
// delays only the writer, never the allocating thread or its bound.
//
// Does not own: choosing the inner allocator (main.rs installs it) or
// bounding any particular large allocation.
// New module; not split out of an existing file.
use std::alloc::{GlobalAlloc, Layout};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering, fence};
use std::time::{Duration, Instant};

/// How long a failed allocation keeps retrying before it gives up. Greg's
/// rule for limits that abort an operation asks for at least 60 seconds.
pub(crate) const ALLOC_RETRY_BOUND: Duration = Duration::from_secs(60);
const ALLOC_RETRY_FIRST_PAUSE: Duration = Duration::from_millis(10);
const ALLOC_RETRY_MAX_PAUSE: Duration = Duration::from_millis(500);

/// Room for the longest notice line `format_wait_notice` writes (about 200
/// bytes with three 20-digit numbers).
pub(crate) const WAIT_NOTICE_CAPACITY: usize = 256;

/// The time, sleeping and notice request a retry uses. The host uses the
/// system's; tests inject their own.
pub(crate) trait RetryClock {
    fn now(&self) -> Instant;
    fn sleep(&self, pause: Duration);
    /// Asks for one notice that a request of `size` bytes is waiting. Must
    /// neither allocate nor block.
    fn notice(&self, size: usize);
}

pub(crate) struct SystemRetryClock;

impl RetryClock for SystemRetryClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, pause: Duration) {
        std::thread::sleep(pause);
    }

    fn notice(&self, size: usize) {
        request_wait_notice(size);
    }
}

/// A global allocator that retries a failed allocation for a bounded time.
pub(crate) struct RetryOnNull<A, C = SystemRetryClock> {
    inner: A,
    clock: C,
    /// `ALLOC_RETRY_BOUND`, except in tests that shorten it.
    bound: Duration,
}

impl<A> RetryOnNull<A, SystemRetryClock> {
    pub(crate) const fn new(inner: A) -> Self {
        Self {
            inner,
            clock: SystemRetryClock,
            bound: ALLOC_RETRY_BOUND,
        }
    }
}

impl<A, C> RetryOnNull<A, C> {
    #[cfg(test)]
    pub(crate) const fn with_clock(inner: A, clock: C) -> Self {
        Self {
            inner,
            clock,
            bound: ALLOC_RETRY_BOUND,
        }
    }

    /// A shorter bound, for tests only; the host always waits the full bound.
    #[cfg(test)]
    pub(crate) const fn with_bound(mut self, bound: Duration) -> Self {
        self.bound = bound;
        self
    }
}

unsafe impl<A: GlobalAlloc, C: RetryClock> GlobalAlloc for RetryOnNull<A, C> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        #[cfg(test)]
        probe::count();
        retry_on_null(&self.clock, self.bound, layout.size(), || unsafe {
            self.inner.alloc(layout)
        })
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        #[cfg(test)]
        probe::count();
        retry_on_null(&self.clock, self.bound, layout.size(), || unsafe {
            self.inner.alloc_zeroed(layout)
        })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { self.inner.dealloc(ptr, layout) }
    }

    // A failed realloc leaves the original block valid, so retrying it is
    // safe.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        #[cfg(test)]
        probe::count();
        retry_on_null(&self.clock, self.bound, new_size, || unsafe {
            self.inner.realloc(ptr, layout, new_size)
        })
    }
}

fn retry_on_null<C: RetryClock>(
    clock: &C,
    bound: Duration,
    size: usize,
    mut attempt: impl FnMut() -> *mut u8,
) -> *mut u8 {
    let first = attempt();
    if !first.is_null() {
        return first;
    }
    let started = clock.now();
    clock.notice(size);
    let mut pause = ALLOC_RETRY_FIRST_PAUSE;
    loop {
        let remaining = bound.saturating_sub(clock.now().saturating_duration_since(started));
        clock.sleep(pause.min(remaining));
        let ptr = attempt();
        if !ptr.is_null() {
            return ptr;
        }
        if clock.now().saturating_duration_since(started) >= bound {
            return ptr;
        }
        pause = (pause * 2).min(ALLOC_RETRY_MAX_PAUSE);
    }
}

/// Requests not yet written, as one word, so a drain takes a coherent
/// snapshot: the count in the top 16 bits and the largest requested size in
/// the low 48 bits, both saturating.
static WAIT_NOTICE_BATCH: AtomicU64 = AtomicU64::new(0);
const BATCH_SIZE_BITS: u32 = 48;
const BATCH_SIZE_MASK: u64 = (1 << BATCH_SIZE_BITS) - 1;
const BATCH_COUNT_MAX: u64 = u16::MAX as u64;
static WAIT_NOTICE_WRITER: OnceLock<std::thread::Thread> = OnceLock::new();
#[cfg(test)]
pub(crate) static WAIT_NOTICE_LINES_WRITTEN: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
pub(crate) static WAIT_NOTICE_DRAINS: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
pub(crate) static WAIT_NOTICE_WRITER_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

/// Records one waiting request and wakes the writer, if it runs. Atomics and
/// an unpark only: no allocation, no lock, no I/O.
fn request_wait_notice(size: usize) {
    let size = (size as u64).min(BATCH_SIZE_MASK);
    WAIT_NOTICE_BATCH.update(Ordering::AcqRel, Ordering::Acquire, |batch| {
        let count = ((batch >> BATCH_SIZE_BITS) + 1).min(BATCH_COUNT_MAX);
        let largest = (batch & BATCH_SIZE_MASK).max(size);
        (count << BATCH_SIZE_BITS) | largest
    });
    // Pairs with the writer's fence after it registers: either this request
    // sees the writer and wakes it, or the writer's next drain sees this
    // request (no store-buffering outcome where both miss).
    fence(Ordering::SeqCst);
    if let Some(writer) = WAIT_NOTICE_WRITER.get() {
        writer.unpark();
    }
}

/// Starts the thread that writes waiting notices. Called once at process
/// start, while allocation still succeeds. If it cannot start, that is said
/// once here, best effort; failed allocations are still retried, without a
/// notice.
pub(crate) fn start_wait_notice_writer() {
    let spawned = std::thread::Builder::new()
        .name("termal-alloc-notice".to_owned())
        .spawn(wait_notice_writer);
    if let Err(err) = spawned {
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr(),
            "termal: the allocation-notice writer could not start ({err}); a failed \
             allocation is still retried, without a notice"
        );
    }
}

/// The writer registers itself, then drains before every park: a request
/// made before registration is written on the first pass, and one made after
/// it leaves an unpark token, so no request waits for an unrelated later one.
fn wait_notice_writer() {
    let _ = WAIT_NOTICE_WRITER.set(std::thread::current());
    // Pairs with the fence in `request_wait_notice`.
    fence(Ordering::SeqCst);
    loop {
        #[cfg(not(test))]
        drain_wait_notices(write_stderr_without_allocating);
        #[cfg(test)]
        {
            let mut lines = 0;
            let allocations = probe::allocations_during(|| {
                lines = drain_wait_notices(write_stderr_without_allocating);
            });
            WAIT_NOTICE_WRITER_ALLOCATIONS.fetch_add(allocations, Ordering::SeqCst);
            WAIT_NOTICE_LINES_WRITTEN.fetch_add(lines, Ordering::SeqCst);
            WAIT_NOTICE_DRAINS.fetch_add(1, Ordering::SeqCst);
        }
        std::thread::park();
    }
}

#[cfg(test)]
pub(crate) fn wait_notice_writer_registered() -> bool {
    WAIT_NOTICE_WRITER.get().is_some()
}

/// Writes one line for each batch of requests recorded since the last
/// drain, through `sink`, formatting each into a stack buffer. Returns how
/// many lines `sink` reports as written.
pub(crate) fn drain_wait_notices(mut sink: impl FnMut(&[u8]) -> bool) -> usize {
    let mut lines = 0;
    loop {
        let batch = WAIT_NOTICE_BATCH.swap(0, Ordering::AcqRel);
        if batch == 0 {
            return lines;
        }
        let waiting = (batch >> BATCH_SIZE_BITS) as usize;
        let largest = (batch & BATCH_SIZE_MASK) as usize;
        let mut line = [0u8; WAIT_NOTICE_CAPACITY];
        let len = format_wait_notice(largest, waiting, &mut line);
        if sink(&line[..len]) {
            lines += 1;
        }
    }
}

/// Formats the waiting notice into `out` without allocating; returns its
/// length. `size` is the largest request in the batch.
pub(crate) fn format_wait_notice(
    size: usize,
    waiting: usize,
    out: &mut [u8; WAIT_NOTICE_CAPACITY],
) -> usize {
    let mut len = 0;
    let mut put = |part: &[u8]| {
        out[len..len + part.len()].copy_from_slice(part);
        len += part.len();
    };
    let mut digits = [0u8; 20];
    put(b"termal: memory allocation of ");
    put(decimal(size, &mut digits));
    if size as u64 >= BATCH_SIZE_MASK {
        // The size saturated when it was recorded.
        put(b"+");
    }
    put(b" bytes failed");
    if waiting > 1 {
        put(b" (");
        put(decimal(waiting, &mut digits));
        put(b" requests; the largest shown)");
    }
    put(b"; waiting up to ");
    put(decimal(ALLOC_RETRY_BOUND.as_secs() as usize, &mut digits));
    put(b" s for memory before giving up\n");
    len
}

fn decimal(mut value: usize, digits: &mut [u8; 20]) -> &[u8] {
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    &digits[start..]
}

/// Writes `line` to stderr through its raw handle: no lock, no buffer and no
/// allocation. Returns whether the whole line was written; a failure is
/// otherwise ignored.
///
/// On Windows the write carries its own OVERLAPPED and event, so it
/// completes on a synchronous handle and on an overlapped one (a pipe a
/// parent opened that way) alike. std's file writer aborts the process when
/// an overlapped write is still pending after its wait. Both offset words
/// set to all ones append to a redirected file; pipes and consoles ignore
/// them.
#[cfg(windows)]
fn write_stderr_without_allocating(line: &[u8]) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_IO_PENDING, GetLastError, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::WriteFile;
    use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Threading::CreateEventW;

    let handle = std::io::stderr().as_raw_handle() as HANDLE;
    if handle.is_null() {
        return false;
    }
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if event.is_null() {
        return false;
    }
    let mut rest = line;
    let mut complete = true;
    while !rest.is_empty() {
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.Anonymous.Anonymous.Offset = u32::MAX;
        overlapped.Anonymous.Anonymous.OffsetHigh = u32::MAX;
        overlapped.hEvent = event;
        let chunk = rest.len().min(u32::MAX as usize) as u32;
        let mut written = 0u32;
        // The kernel may update both through the pointers given to WriteFile
        // until the write completes, so each is one raw pointer, made once and
        // used for every call, with no reference taken while it is in flight.
        let overlapped_ptr = &raw mut overlapped;
        let written_ptr = &raw mut written;
        let done = unsafe { WriteFile(handle, rest.as_ptr(), chunk, written_ptr, overlapped_ptr) };
        let done = done != 0
            || (unsafe { GetLastError() } == ERROR_IO_PENDING
                && unsafe { GetOverlappedResult(handle, overlapped_ptr, written_ptr, 1) } != 0);
        // Complete (or failed) from here on: reading `written` is safe.
        let written = unsafe { written_ptr.read() };
        if !done || written == 0 {
            complete = false;
            break;
        }
        rest = &rest[(written as usize).min(rest.len())..];
    }
    unsafe { CloseHandle(event) };
    complete
}

#[cfg(unix)]
fn write_stderr_without_allocating(line: &[u8]) -> bool {
    use std::io::Write;
    use std::mem::ManuallyDrop;
    use std::os::unix::io::{AsRawFd, FromRawFd};
    // The descriptor stays owned by the process; ManuallyDrop never closes it.
    let file =
        ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(std::io::stderr().as_raw_fd()) });
    (&*file).write_all(line).is_ok()
}

/// Counts heap requests made through this allocator on the current thread,
/// so tests can show that a path allocates nothing. Test builds only.
#[cfg(test)]
pub(crate) mod probe {
    use std::cell::Cell;

    thread_local! {
        static COUNT: Cell<Option<usize>> = const { Cell::new(None) };
    }

    pub(super) fn count() {
        let _ = COUNT.try_with(|count| {
            if let Some(seen) = count.get() {
                count.set(Some(seen + 1));
            }
        });
    }

    /// Runs `operation` and returns how many heap requests the current
    /// thread made through the global allocator meanwhile.
    pub(crate) fn allocations_during(operation: impl FnOnce()) -> usize {
        COUNT.with(|count| count.set(Some(0)));
        operation();
        COUNT.with(|count| count.take().unwrap_or(0))
    }
}

#[cfg(test)]
pub(crate) fn take_pending_wait_notices_for_test() -> usize {
    (WAIT_NOTICE_BATCH.swap(0, Ordering::AcqRel) >> BATCH_SIZE_BITS) as usize
}
