// Test-only observation seams for the candidate's history-load fixture.
// Does not replace parsing, persistence, admission, or scan serialization.
// Split from the measured registration, disk-reader and scanner call sites.

use super::*;
pub(crate) type TestHistoryParsed = Box<dyn FnMut(&FsPath, &str)>;
pub(crate) type TestHistoryExcerpt = Box<dyn FnMut(&FsPath, &TestRunCardFailureRead)>;
pub(crate) type TestHistoryCommitted = Box<dyn FnMut(&PersistDelta)>;

thread_local! {
    pub(crate) static TEST_HISTORY_SCAN_READY: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    pub(crate) static TEST_HISTORY_PARSED: std::cell::RefCell<Option<TestHistoryParsed>> = const { std::cell::RefCell::new(None) };
    pub(crate) static TEST_HISTORY_EXCERPT: std::cell::RefCell<Option<TestHistoryExcerpt>> = const { std::cell::RefCell::new(None) };
    pub(crate) static TEST_HISTORY_COMMITTED: std::cell::RefCell<Option<TestHistoryCommitted>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn test_history_received_hooks() -> &'static Mutex<HashMap<String, mpsc::Sender<()>>> {
    static HOOKS: std::sync::OnceLock<Mutex<HashMap<String, mpsc::Sender<()>>>> =
        std::sync::OnceLock::new();
    HOOKS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn test_history_registration_received(run_id: &str) {
    if let Some(release) = test_history_received_hooks().lock().unwrap().remove(run_id) {
        let _ = release.send(());
    }
}
