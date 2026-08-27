// Owns test-only temporary directory creation and cleanup whose lifetime must
// follow the last state/runtime handle that can keep files inside it open.
// Does not own production paths, test fixture contents, or stale-directory
// sweeping from crashed test processes.
// New module; consolidates the private temp-root guards previously scattered
// across state, mailbox, board-route, and agent-command tests.

struct TestTempRoot {
    path: PathBuf,
}

impl TestTempRoot {
    fn create(prefix: &str) -> Self {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("test temp root should be created");
        Self { path }
    }

    fn own(path: PathBuf) -> Self {
        Self { path }
    }

    fn path(&self) -> &FsPath {
        &self.path
    }

    fn database_path(&self) -> PathBuf {
        self.path.join("termal.sqlite")
    }
}

impl Drop for TestTempRoot {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            eprintln!(
                "test temp root not removed: {} ({error})",
                self.path.display()
            );
        }
    }
}
