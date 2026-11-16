// Owns the test-only product temporary-directory boundary for direct Cargo and
// wrapper launches, and the rule that it lies inside the repository (this
// checkout or its main checkout). Does not own production paths, the
// process-wide TEMP, TMP and TMPDIR, or retry failed removals.
// New module, extracted from the raw OS-temp lookup in TestTempRoot.

fn resolve_test_temp_directory(
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

fn ensure_plain_test_directory(path: &FsPath) -> io::Result<()> {
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

/// Lexical form of a path: `.` dropped and `..` taken back. Nothing is read
/// from the file system, so links are not followed.
fn lexical_test_path(path: &FsPath) -> PathBuf {
    let mut normal = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normal.pop();
            }
            other => normal.push(other),
        }
    }
    normal
}

/// Whether `path` is `root` or lies below it, compared component by component
/// (without case on Windows, where paths ignore it).
fn test_path_within(path: &FsPath, root: &FsPath) -> bool {
    let mut path = path.components();
    root.components().all(|root_part| {
        path.next().is_some_and(|part| {
            if cfg!(windows) {
                part.as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&root_part.as_os_str().to_string_lossy())
            } else {
                part == root_part
            }
        })
    })
}

/// The folders test data may live in: the checkout this crate was built from
/// and, when that is a linked Git worktree, its main checkout. The main
/// checkout is read from the worktree's `.git` file and the common directory
/// it names; Git itself is not run.
fn test_repository_roots(manifest: &FsPath) -> Vec<PathBuf> {
    let manifest = lexical_test_path(manifest);
    let main = fs::read_to_string(manifest.join(".git"))
        .ok()
        .and_then(|text| {
            let gitdir = text.lines().find_map(|line| line.strip_prefix("gitdir:"))?;
            let gitdir = manifest.join(gitdir.trim());
            let common = fs::read_to_string(gitdir.join("commondir")).ok()?;
            lexical_test_path(&gitdir.join(common.trim()))
                .parent()
                .map(FsPath::to_path_buf)
        });
    std::iter::once(manifest).chain(main).collect()
}

/// Whether `path` lies inside one of this crate's repository roots: an
/// absolute path, no `..` step, and below the worktree or its main checkout.
fn test_path_inside_repository(path: &FsPath) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        && test_repository_roots(FsPath::new(env!("CARGO_MANIFEST_DIR")))
            .iter()
            .any(|root| test_path_within(&lexical_test_path(path), root))
}

/// Tests write only inside the repository. A user temp anywhere else (the
/// user's home, the platform temp directory, another tree) is refused, with
/// the path and the rule.
fn check_test_user_temp(user_temp: &FsPath) -> Result<(), String> {
    if test_path_inside_repository(user_temp) {
        return Ok(());
    }
    let roots = test_repository_roots(FsPath::new(env!("CARGO_MANIFEST_DIR")))
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "test user temp {} is outside the repository: tests write only inside \
         this checkout or its main checkout ({roots}); set TERMAL_TEST_USER_TEMP \
         to a directory there, such as <repository>/target/tmp/termal",
        user_temp.display()
    ))
}

/// The user temp every test temporary directory lives under: the launcher's
/// TERMAL_TEST_USER_TEMP when set, else this repository's target/tmp/termal,
/// created on first use. The platform temp directory is never consulted, and
/// the process-wide TEMP, TMP and TMPDIR are left as they are.
fn test_user_temp() -> PathBuf {
    let explicit = std::env::var_os("TERMAL_TEST_USER_TEMP").map(PathBuf::from);
    let user_temp = explicit.clone().unwrap_or_else(|| {
        FsPath::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("tmp")
            .join("termal")
    });
    // Checked before anything is created under it.
    if let Err(rule) = check_test_user_temp(&user_temp) {
        panic!("{rule}");
    }
    if explicit.is_none() {
        fs::create_dir_all(&user_temp)
            .unwrap_or_else(|error| panic!("test user temp {}: {error}", user_temp.display()));
    }
    user_temp
}

fn test_temp_dir() -> PathBuf {
    static FIRST_USE_SWEEP: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    let user_temp = test_user_temp();
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

fn sweep_unmarked_test_fixtures(
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
fn remove_test_directory(path: impl AsRef<FsPath>) {
    report_test_directory_failures(test_directory_removal_failure(path.as_ref()).into_iter());
}

/// One cleanup attempt for each directory. Every directory is attempted even
/// after an earlier one failed; all failures are then reported together, as
/// `remove_test_directory` reports one.
#[track_caller]
fn remove_test_directories<P: AsRef<FsPath>>(paths: impl IntoIterator<Item = P>) {
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
