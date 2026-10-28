// The test binary's panic journal.
//
// Owns: recording every panic of the test binary to the file named by the
// TERMAL_TEST_PANIC_JOURNAL environment variable, which the test launcher sets
// for each run (scripts/test-launcher.mjs). libtest keeps a test's panic
// output in memory and prints it only when the test finishes. When a second
// panic during unwinding aborts the process, that output dies with the
// process, and the failing test's name with it. A record written straight to
// a file survives the abort.
// Does not own: the launcher's reading of the journal, or any test's
// assertions. The hook chains to the one before it, so libtest's own report
// is unchanged.
//
// Why a load-time initializer: the test binary's main is libtest's, which
// offers no test-wide setup, and a hook installed by a fixture would miss
// exactly the tests that don't use it. So, as the ctor crate does, but
// without the dependency, a function pointer placed in the platform's
// initializer section runs once when the binary loads, before main:
// - Windows: `.CRT$XCU`;
// - ELF: `.init_array`;
// - Apple: `__DATA,__mod_init_func`.
// On any other target the pointer is placed in no such section, never runs,
// and nothing is recorded. The function only reads the variable and installs
// the hook, and does no other work before main.
//
// Each panic is written as two lines, both carrying the process id and a
// sequence number:
// 1. The panic line: time, message, location and backtrace. Nothing used to
//    build it can panic, and it is on disk before any thread data is read.
// 2. The thread line: the thread's name. libtest names a test's thread after
//    the test.
// Reading the name is the one step that can still panic: std::thread::current
// panics if the thread's own data is already destroyed, and a panic inside a
// panic hook aborts at once. Writing the panic line first means even that case
// keeps the panic. Readers join the two lines by process id and sequence.
//
// New file, not split from another; included from main.rs under cfg(test).

/// The variable naming a run's journal file. Without it nothing is installed
/// and nothing is written.
const TEST_PANIC_JOURNAL_ENV: &str = "TERMAL_TEST_PANIC_JOURNAL";

#[used]
#[cfg_attr(windows, unsafe(link_section = ".CRT$XCU"))]
#[cfg_attr(
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly",
        target_os = "illumos",
        target_os = "solaris"
    ),
    unsafe(link_section = ".init_array")
)]
#[cfg_attr(target_vendor = "apple", unsafe(link_section = "__DATA,__mod_init_func"))]
static TEST_PANIC_JOURNAL_AT_LOAD: extern "C" fn() = install_test_panic_journal_at_load;

/// Runs once at load, before main. It reads the variable and, when it is
/// set, chains a recording hook in front of the hook in force.
extern "C" fn install_test_panic_journal_at_load() {
    let Some(path) = std::env::var_os(TEST_PANIC_JOURNAL_ENV) else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        record_test_panic(&path, info);
        previous(info);
    }));
}

/// Writes this panic's two lines (see the header). Every failure to write is
/// ignored, since a panic inside a panic hook aborts at once.
fn record_test_panic(path: &std::path::Path, info: &std::panic::PanicHookInfo<'_>) {
    static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id();
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<non-string panic payload>".to_owned());
    let at_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok());
    let panic_line = serde_json::json!({
        "pid": pid,
        "seq": sequence,
        "at_unix_ms": at_unix_ms,
        "message": message,
        "location": info
            .location()
            .map(|location| format!("{}:{}:{}", location.file(), location.line(), location.column())),
        "backtrace": std::backtrace::Backtrace::force_capture().to_string(),
    });
    // One writer at a time, so concurrent panics never interleave a line.
    let _writing = WRITING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    append_test_panic_line(path, &panic_line);
    let thread = std::thread::current();
    let thread_line = serde_json::json!({
        "pid": pid,
        "seq": sequence,
        "thread": thread.name().unwrap_or("<unnamed>"),
    });
    append_test_panic_line(path, &thread_line);
}

fn append_test_panic_line(path: &std::path::Path, value: &serde_json::Value) {
    let mut line = value.to_string();
    line.push('\n');
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = std::io::Write::write_all(&mut file, line.as_bytes());
        let _ = std::io::Write::flush(&mut file);
    }
}
