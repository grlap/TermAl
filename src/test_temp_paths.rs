// Owns the test-only product temporary-directory boundary for direct Cargo and
// wrapper launches. Does not own production paths or retry failed removals.
// New module, extracted from the raw OS-temp lookup in TestTempRoot.

fn resolve_test_temp_directory(user_temp: &FsPath, run_root: Option<&FsPath>) -> io::Result<PathBuf> {
    if !user_temp.is_absolute()
        || user_temp.components().any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "user temp must be absolute without parent traversal"));
    }
    // Same lexical contract as scripts/test-temp-root.mjs: normalize dot and
    // separator spelling, but do not canonicalize OS-temp filesystem aliases.
    // Product/run components are independently checked for links at creation.
    let user_temp = user_temp.components().collect::<PathBuf>();
    let product = user_temp.join("termal").join("tests");
    match run_root {
        None => Ok(product),
        Some(run) if run.is_absolute()
            && run.parent() == Some(product.as_path())
            && run.file_name().is_some_and(|name| name.to_string_lossy().starts_with("run-"))
            && !run.components().any(|part| matches!(part, std::path::Component::ParentDir)) => Ok(run.components().collect()),
        Some(_) => Err(io::Error::new(io::ErrorKind::InvalidInput, "test run root must be a direct run-* child of the product test directory")),
    }
}

fn ensure_plain_test_directory(path: &FsPath) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("test directory is not a plain directory: {}", path.display())));
    }
    Ok(())
}

fn test_temp_dir() -> PathBuf {
    static FIRST_USE_SWEEP: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    let user_temp = std::env::var_os("TERMAL_TEST_USER_TEMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let run_root = std::env::var_os("TERMAL_TEST_RUN_ROOT").map(PathBuf::from);
    let target = resolve_test_temp_directory(&user_temp, run_root.as_deref())
        .expect("valid product test temporary directory");
    // Validate one component at a time, before creating anything below it.
    for path in [user_temp.join("termal"), user_temp.join("termal").join("tests"), target.clone()] {
        ensure_plain_test_directory(&path)
            .unwrap_or_else(|error| panic!("test temporary directory {}: {error}", path.display()));
    }
    let sweep_outcome = FIRST_USE_SWEEP.get_or_init(|| {
        // The wrapper has already swept the parent before creating its run.
        // Direct Cargo/IDE launches sweep only unmarked fixture roots. Marked
        // runs are retained here; the Node launcher owns their PID checks.
        if run_root.is_none() {
            let removed = sweep_unmarked_test_fixtures(&target, std::time::SystemTime::now())
                .map_err(|error| format!("test startup sweep {}: {error}; os={:?}", target.display(), error.raw_os_error()))?;
            for path in removed {
                eprintln!("removed stale test fixture: {}", path.display());
            }
        }
        Ok(())
    });
    // Publish failures before panicking: every caller must fail with the original
    // diagnostic, without poisoning initialization or retrying a failed sweep.
    if let Err(diagnostic) = sweep_outcome {
        panic!("{diagnostic}");
    }
    target
}

fn stale_unmarked_test_fixture(path: &FsPath, now: std::time::SystemTime) -> io::Result<Option<fs::FileType>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if (!metadata.is_dir() && !metadata.is_file()) || metadata.file_type().is_symlink() {
        return Ok(None);
    }
    if metadata.is_dir() {
        match fs::symlink_metadata(path.join(".termal-test-run")) {
            Ok(_) => return Ok(None), // Any marker, even malformed, excludes the fixture.
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
    }
    Ok(now.duration_since(metadata.modified()?).is_ok_and(|age| age > Duration::from_secs(48 * 60 * 60)).then_some(metadata.file_type()))
}

fn sweep_unmarked_test_fixtures(root: &FsPath, now: std::time::SystemTime) -> io::Result<Vec<PathBuf>> {
    let plain_root = || -> io::Result<()> {
        let metadata = fs::symlink_metadata(root)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("sweep root is not a plain directory: {}", root.display())));
        }
        Ok(())
    };
    plain_root()?;
    let mut removed = Vec::new();
    for entry in fs::read_dir(root)? {
        if removed.len() == 64 {
            break;
        }
        let path = entry?.path();
        if stale_unmarked_test_fixture(&path, now)?.is_none() {
            continue;
        }
        // Direct children only, with parent, type, marker and age revalidated
        // immediately before the single removal attempt. Never walk a run to
        // select individual fixtures, and never traverse a directory link.
        plain_root()?;
        let Some(kind) = stale_unmarked_test_fixture(&path, now)? else {
            continue;
        };
        let removal = if kind.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        match removal {
            Ok(()) => removed.push(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(io::Error::new(error.kind(), format!("stale fixture {}: {error}; os={:?}", path.display(), error.raw_os_error()))),
        }
    }
    Ok(removed)
}

/// One cleanup attempt, with the path and original OS error on failure. During
/// assertion unwind report the cleanup failure without causing a double panic.
#[track_caller]
fn remove_test_directory(path: impl AsRef<FsPath>) {
    let path = path.as_ref();
    if let Err(error) = fs::remove_dir_all(path) {
        if error.kind() == io::ErrorKind::NotFound {
            return;
        }
        let detail = format!("test directory not removed: {} ({error}; kind={:?}; os={:?})",
            path.display(), error.kind(), error.raw_os_error());
        if std::thread::panicking() {
            eprintln!("{detail}");
        } else {
            panic!("{detail}");
        }
    }
}
