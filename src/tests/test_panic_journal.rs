// Owns the regression for the test binary's panic journal. When the
// launcher names a journal file, every panic in the real test binary is
// recorded there outside libtest's output capture. So the record survives
// even when a second panic, during unwinding, aborts the process and libtest's
// captured output is lost with it. Does not own the launcher's handling of
// the journal (scripts/test-launcher.mjs). New file, not split from another.
use super::*;

const CASE: &str =
    "tests::test_panic_journal::a_panic_is_recorded_even_when_a_second_panic_aborts_the_test_process";
const CHILD: &str = "TERMAL_TEST_PANIC_JOURNAL_CHILD";
/// The variable the launcher sets to name a run's journal file.
const JOURNAL: &str = "TERMAL_TEST_PANIC_JOURNAL";
const MARKER: &str = "first panic: the journal must keep this one";

/// The child is this same test binary, started with only the journal
/// variable set and with libtest's capture on. Nothing in its body installs a
/// hook, so the record proves the binary installs one itself.
#[test]
fn a_panic_is_recorded_even_when_a_second_panic_aborts_the_test_process() {
    if std::env::var(CHILD).as_deref() == Ok(CASE) {
        panic_twice();
    }
    let root = TestTempRoot::create("termal-panic-journal");
    let journal = root.path().join("rust-panics.jsonl");
    let mut command = Command::new(std::env::current_exe().unwrap());
    // The child crashes on purpose: running it in the test's own directory
    // keeps any core file or crash artifact out of the repository.
    command
        .current_dir(root.path())
        .args(["--exact", CASE, "--test-threads=1"])
        .env(CHILD, CASE)
        .env(JOURNAL, &journal);
    let (status, stderr) =
        phase_sync::CapturedStderrProcess::spawn(&mut command).wait_with_stderr("panic journal child");
    assert!(
        !status.success() && status.code() != Some(101),
        "the second panic must abort the child, not fail it normally: {status:?}\n{stderr}"
    );
    let text = fs::read_to_string(&journal).unwrap_or_else(|error| {
        panic!("the child's panic journal must exist: {error}\n{stderr}")
    });
    let lines: Vec<Value> = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect();
    let first = lines
        .iter()
        .find(|record| record["message"] == MARKER)
        .unwrap_or_else(|| panic!("the journal must hold the first panic: {text}"));
    // The thread is written as its own line after the panic line, joined to
    // it by process id and sequence.
    let thread = lines
        .iter()
        .find(|line| {
            line.get("thread").is_some() && line["pid"] == first["pid"] && line["seq"] == first["seq"]
        })
        .unwrap_or_else(|| panic!("the journal must name the first panic's thread: {text}"));
    assert_eq!(thread["thread"], CASE, "the record names the test: {thread}");
    assert!(
        first["location"]
            .as_str()
            .is_some_and(|location| location.contains("test_panic_journal.rs")),
        "the record keeps the panic's location: {first}"
    );
    assert!(
        first["backtrace"]
            .as_str()
            .is_some_and(|backtrace| !backtrace.trim().is_empty()),
        "the record keeps a backtrace: {first}"
    );
}

/// The child's body: a panic while a value whose drop also panics is alive.
/// The second panic happens during unwinding, so the process aborts.
fn panic_twice() -> ! {
    struct PanicsOnDrop;
    impl Drop for PanicsOnDrop {
        fn drop(&mut self) {
            panic!("second panic, during unwinding");
        }
    }
    let _guard = PanicsOnDrop;
    panic!("{MARKER}");
}
