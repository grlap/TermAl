// Owns the test-only product temporary-directory boundary for direct Cargo and
// wrapper launches. Does not own production paths or retry failed removals.
// New module, extracted from the raw OS-temp lookup in TestTempRoot.

use super::*;

pub(crate) fn resolve_test_temp_directory(
    user_temp: &FsPath,
    run_root: Option<&FsPath>,
) -> io::Result<PathBuf> {
    if !user_temp.is_absolute()
        || user_temp
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "user temp must be absolute without parent traversal",
        ));
    }
    // Same lexical contract as scripts/test-temp-root.mjs: normalize dot and
    // separator spelling, but do not canonicalize OS-temp filesystem aliases.
    // Product/run components are independently checked for links at creation.
    let user_temp = user_temp.components().collect::<PathBuf>();
    let product = user_temp.join("termal").join("tests");
    match run_root {
        None => Ok(product),
        Some(run)
            if run.is_absolute()
                && run.parent() == Some(product.as_path())
                && run
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("run-"))
                && !run
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir)) =>
        {
            Ok(run.components().collect())
        }
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "test run root must be a direct run-* child of the product test directory",
        )),
    }
}

pub(crate) fn ensure_plain_test_directory(path: &FsPath) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "test directory is not a plain directory: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

pub(crate) fn test_temp_dir() -> PathBuf {
    static FIRST_USE_SWEEP: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    let user_temp = std::env::var_os("TERMAL_TEST_USER_TEMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let run_root = std::env::var_os("TERMAL_TEST_RUN_ROOT").map(PathBuf::from);
    let target = resolve_test_temp_directory(&user_temp, run_root.as_deref())
        .expect("valid product test temporary directory");
    // Validate one component at a time, before creating anything below it.
    for path in [
        user_temp.join("termal"),
        user_temp.join("termal").join("tests"),
        target.clone(),
    ] {
        ensure_plain_test_directory(&path)
            .unwrap_or_else(|error| panic!("test temporary directory {}: {error}", path.display()));
    }
    let sweep_outcome = FIRST_USE_SWEEP.get_or_init(|| {
        // The wrapper has already swept the parent before creating its run.
        // Direct Cargo/IDE launches sweep only unmarked fixture roots. Marked
        // runs are retained here; the Node launcher owns their PID checks.
        if run_root.is_none() {
            let removed = sweep_unmarked_test_fixtures(&target, std::time::SystemTime::now())
                .map_err(|error| {
                    format!(
                        "test startup sweep {}: {error}; os={:?}",
                        target.display(),
                        error.raw_os_error()
                    )
                })?;
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

/// Longest directory, in UTF-8 bytes (at least its character count), that may
/// hold Git-fixture roots on Windows. The deepest fixture Git directory this
/// suite creates lies 133 characters below such a directory. Git refuses an
/// explicit GIT_DIR longer than 220 (PATH_MAX - 40) even with core.longpaths,
/// and 67 keeps 20 characters in reserve.
pub(crate) const WINDOWS_FIXTURE_DIRECTORY_BUDGET: usize = 67;
/// Remedy for a default directory that is too deep (the user temp chooses it).
pub(crate) const DEFAULT_FIXTURE_DIRECTORY_REMEDY: &str =
    "set TERMAL_TEST_USER_TEMP to a shorter absolute path";
/// Remedy for a repository-local directory that is too deep.
pub(crate) const REPOSITORY_FIXTURE_DIRECTORY_REMEDY: &str = "use a shallower checkout";

/// Refuses a fixture directory too deep for this suite's Git fixtures under
/// Windows' path limits, naming the remedy for how the directory was chosen.
pub(crate) fn fixture_directory_within_budget(
    directory: &FsPath,
    remedy: &str,
) -> Result<(), String> {
    let length = directory.as_os_str().len();
    if length <= WINDOWS_FIXTURE_DIRECTORY_BUDGET {
        return Ok(());
    }
    Err(format!(
        "test fixture directory {} is {length} bytes long, more than the \
         {WINDOWS_FIXTURE_DIRECTORY_BUDGET} that keeps this suite's Git fixtures within \
         Git's Windows path limits; {remedy}",
        directory.display()
    ))
}

/// The budget check for a directory as Git will see it: fixture roots are
/// canonicalized, so an existing directory is measured in that spelling.
#[cfg(windows)]
pub(crate) fn windows_fixture_directory_check(
    directory: &FsPath,
    remedy: &str,
) -> Result<(), String> {
    let spelled = fs::canonicalize(directory)
        .map(|canonical| {
            // This fixture helper is included directly by integration tests.
            // Measure the ordinary Win32 spelling that Git receives.
            let path = canonical.to_string_lossy();
            if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
                PathBuf::from(format!(r"\\{rest}"))
            } else if let Some(rest) = path.strip_prefix(r"\\?\") {
                PathBuf::from(rest)
            } else {
                canonical
            }
        })
        .unwrap_or_else(|_| directory.to_owned());
    fixture_directory_within_budget(&spelled, remedy)
}

/// The default directory's budget result, computed once per process so that
/// every refused fixture root fails with the same message.
#[cfg(windows)]
pub(crate) fn default_fixture_directory_budget() -> &'static Result<(), String> {
    static RESULT: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    RESULT.get_or_init(|| {
        windows_fixture_directory_check(&test_temp_dir(), DEFAULT_FIXTURE_DIRECTORY_REMEDY)
    })
}

fn stale_unmarked_test_fixture(
    path: &FsPath,
    now: std::time::SystemTime,
) -> io::Result<Option<fs::FileType>> {
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
    Ok(now
        .duration_since(metadata.modified()?)
        .is_ok_and(|age| age > Duration::from_secs(48 * 60 * 60))
        .then_some(metadata.file_type()))
}

pub(crate) fn sweep_unmarked_test_fixtures(
    root: &FsPath,
    now: std::time::SystemTime,
) -> io::Result<Vec<PathBuf>> {
    let plain_root = || -> io::Result<()> {
        let metadata = fs::symlink_metadata(root)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("sweep root is not a plain directory: {}", root.display()),
            ));
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
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "stale fixture {}: {error}; os={:?}",
                        path.display(),
                        error.raw_os_error()
                    ),
                ));
            }
        }
    }
    Ok(removed)
}

/// One cleanup attempt, with the path and original OS error on failure. During
/// assertion unwind report the cleanup failure without causing a double panic.
#[track_caller]
pub(crate) fn remove_test_directory(path: impl AsRef<FsPath>) {
    report_test_directory_failures(test_directory_removal_failure(path.as_ref()).into_iter());
}

/// One cleanup attempt for each directory. Every directory is attempted even
/// after an earlier one failed; all failures are then reported together, as
/// `remove_test_directory` reports one.
#[track_caller]
pub(crate) fn remove_test_directories<P: AsRef<FsPath>>(paths: impl IntoIterator<Item = P>) {
    let failures = paths
        .into_iter()
        .filter_map(|path| test_directory_removal_failure(path.as_ref()))
        .collect::<Vec<_>>();
    report_test_directory_failures(failures.into_iter());
}

/// The path and original OS error of a failed removal; an absent directory
/// counts as removed.
fn test_directory_removal_failure(path: &FsPath) -> Option<String> {
    match fs::remove_dir_all(path) {
        Ok(()) => None,
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => Some(format!(
            "{} ({error}; kind={:?}; os={:?})",
            path.display(),
            error.kind(),
            error.raw_os_error()
        )),
    }
}

#[track_caller]
fn report_test_directory_failures(mut failures: impl Iterator<Item = String>) {
    let Some(first) = failures.next() else {
        return;
    };
    let detail = failures.fold(
        format!("test directory not removed: {first}"),
        |detail, failure| format!("{detail}; {failure}"),
    );
    if std::thread::panicking() {
        eprintln!("{detail}");
    } else {
        panic!("{detail}");
    }
}
