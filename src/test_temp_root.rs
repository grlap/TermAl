// Owns test-only temporary directory creation and cleanup whose lifetime must
// follow the last state/runtime handle that can keep files inside it open.
// Does not own production paths, test fixture contents, or stale-directory
// sweeping from crashed test processes.
// New module; consolidates the private temp-root guards previously scattered
// across state, mailbox, board-route, and agent-command tests.

struct TestTempRoot {
    path: PathBuf,
    cleanup_observers: Mutex<Vec<mpsc::Sender<std::result::Result<(), String>>>>,
}

impl TestTempRoot {
    fn create(prefix: &str) -> Self {
        assert!(!prefix.is_empty() && prefix.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'), "test root prefix must be a simple name");
        let path = test_temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("test temp root should be created");
        Self::own(path)
    }

    fn own(path: PathBuf) -> Self {
        Self {
            path,
            cleanup_observers: Mutex::new(Vec::new()),
        }
    }

    fn path(&self) -> &FsPath {
        &self.path
    }

    fn database_path(&self) -> PathBuf {
        self.path.join("termal.sqlite")
    }

    fn observe_cleanup(&self) -> mpsc::Receiver<std::result::Result<(), String>> {
        let (tx, rx) = mpsc::channel();
        self.cleanup_observers.lock().expect("cleanup observers mutex poisoned").push(tx);
        rx
    }
}

impl Drop for TestTempRoot {
    fn drop(&mut self) {
        let outcome = match fs::remove_dir_all(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "test temp root not removed: {} ({error}; kind={:?}; os={:?})",
                self.path.display(), error.kind(), error.raw_os_error()
            )),
        };
        if let Err(detail) = &outcome {
            eprintln!("{detail}");
        }
        // This receipt is a resource-release barrier: AppState keeps its root
        // last, so every state-owned file handle is gone before it is sent.
        let observers = self.cleanup_observers.get_mut().unwrap_or_else(|poisoned| {
            eprintln!("cleanup observers mutex poisoned for {}; completing removal receipt", self.path.display());
            poisoned.into_inner()
        });
        for observer in observers.drain(..) {
            let _ = observer.send(outcome.clone());
        }
    }
}

/// Test fixture owner for production continuations which retain AppState.
/// Release them through their normal cancellation path before dropping state;
/// then wait for the last resource owner, not a scheduler delay or Arc poll.
struct TestAppStateCleanup<F: FnOnce() -> std::result::Result<(), String>> {
    state: Option<AppState>,
    release: Option<F>,
    cleaned: mpsc::Receiver<std::result::Result<(), String>>,
    holder: &'static str,
    path: PathBuf,
}

impl<F: FnOnce() -> std::result::Result<(), String>> TestAppStateCleanup<F> {
    fn new(state: AppState, holder: &'static str, release: F) -> Self {
        let root = state.test_temp_root.as_ref().expect("fixture temp root");
        let cleaned = root.observe_cleanup();
        let path = root.path().to_owned();
        Self { state: Some(state), release: Some(release), cleaned, holder, path }
    }

    fn finish(self) {
        drop(self);
    }
}

impl<F: FnOnce() -> std::result::Result<(), String>> std::ops::Deref for TestAppStateCleanup<F> {
    type Target = AppState;

    fn deref(&self) -> &AppState {
        self.state.as_ref().expect("fixture state is still owned")
    }
}

impl<F: FnOnce() -> std::result::Result<(), String>> Drop for TestAppStateCleanup<F> {
    fn drop(&mut self) {
        let mut failures = Vec::new();
        if let Some(release) = self.release.take() {
            // Release callbacks should finish every cleanup step and return an
            // error. Contain an unexpected callback panic as well: Drop may be
            // running during an unrelated assertion's unwind.
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(release)) {
                Ok(Ok(())) => (),
                Ok(Err(detail)) => failures.push(detail),
                Err(payload) => {
                    let detail = payload.downcast_ref::<String>().map(String::as_str)
                        .or_else(|| payload.downcast_ref::<&str>().copied())
                        .unwrap_or("non-string panic payload");
                    failures.push(format!("fixture release panicked: {detail}"));
                }
            }
        }
        drop(self.state.take());
        let outcome = self.cleaned.recv_timeout(TEST_PHASE_DEADLOCK_GUARD)
            .map_err(|error| format!("{} retained fixture {}: {error}", self.holder, self.path.display()))
            .and_then(|outcome| outcome);
        if let Err(detail) = outcome {
            failures.push(detail);
        }
        if !failures.is_empty() {
            let detail = format!("{} fixture {}: {}", self.holder, self.path.display(), failures.join("; "));
            if std::thread::panicking() {
                eprintln!("fixture cleanup during unwind failed: {detail}");
            } else {
                panic!("{detail}");
            }
        }
    }
}
