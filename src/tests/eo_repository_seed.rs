//! Reuse immutable repository bytes; every case owns all of its writable files.
use super::*;

thread_local! {
    static USE_SEED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(super) struct EORepositorySeedScope {
    previous: bool,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl EORepositorySeedScope {
    pub(super) fn new() -> Self {
        Self {
            previous: USE_SEED.with(|slot| slot.replace(true)),
            _thread_bound: std::marker::PhantomData,
        }
    }
}

impl Drop for EORepositorySeedScope {
    fn drop(&mut self) {
        USE_SEED.with(|slot| slot.set(self.previous));
    }
}

fn initialize(root: &FsPath) {
    run_git_test_command(root, &["init", "--quiet"]);
    run_git_test_command(root, &["config", "user.email", "termal-tests@example.com"]);
    run_git_test_command(root, &["config", "user.name", "TermAl tests"]);
    fs::write(root.join("README.md"), "checked\n").unwrap();
    run_git_test_command(root, &["add", "README.md"]);
    run_git_test_command(root, &["commit", "--quiet", "-m", "checked"]);
}

fn snapshot_files(root: &FsPath, relative: &FsPath, files: &mut Vec<(PathBuf, Vec<u8>)>) {
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path).unwrap();
    assert!(
        !metadata.file_type().is_symlink(),
        "seed contains a link: {}",
        path.display()
    );
    if metadata.is_dir() {
        for entry in fs::read_dir(&path).unwrap() {
            snapshot_files(root, &relative.join(entry.unwrap().file_name()), files);
        }
    } else {
        assert!(
            metadata.is_file(),
            "seed contains a special file: {}",
            path.display()
        );
        assert_ne!(
            path.extension().and_then(|value| value.to_str()),
            Some("lock")
        );
        files.push((relative.to_owned(), fs::read(path).unwrap()));
    }
}

pub(super) fn prepare_eo_repository(root: &FsPath) {
    if !USE_SEED.with(std::cell::Cell::get) {
        initialize(root);
        return;
    }
    static SEED: std::sync::OnceLock<Vec<(PathBuf, Vec<u8>)>> = std::sync::OnceLock::new();
    let files = SEED.get_or_init(|| {
        let _placement = TestTempRootDirectoryScope::repository_local();
        let seed = TestTempRoot::create("eo-repository-seed");
        initialize(seed.path());
        let mut files = Vec::new();
        // Omit hooks, logs, worktree registrations and all other metadata.
        // The seed's filesystem can disappear once these immutable bytes exist.
        assert!(!seed.join(".git/objects/info/alternates").exists());
        let config = git_command()
            .arg("-C")
            .arg(seed.path())
            .args([
                "config",
                "--local",
                "--get-regexp",
                "core[.]worktree|include",
            ])
            .output()
            .unwrap();
        assert_eq!(
            config.status.code(),
            Some(1),
            "seed config must have no worktree override or include: {:?}",
            config
        );
        for relative in [
            "README.md",
            ".git/HEAD",
            ".git/config",
            ".git/index",
            ".git/objects",
            ".git/refs",
        ] {
            snapshot_files(seed.path(), FsPath::new(relative), &mut files);
        }
        files
    });
    for (relative, bytes) in files {
        let destination = root.join(relative);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        // Fresh regular files, never hardlinks or shared writable Git metadata.
        fs::write(destination, bytes).unwrap();
    }
}
