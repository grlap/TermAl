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
    target
}
