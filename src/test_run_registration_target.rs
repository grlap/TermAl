// Owns targeted registration-path admission, independent of history size.
// Does not discover runs, index artifacts, or grant verification credit.
// Split from test_run_registration.rs's global allowed-directory scan.
fn test_run_registration_target(common: &FsPath, run: &FsPath) -> bool {
    let Ok(common) = fs::canonicalize(common) else {
        return false;
    };
    let Some(parent) = run.parent().and_then(|parent| fs::canonicalize(parent).ok()) else {
        return false;
    };
    // Check the resolved shape before reconstructing it: canonicalizing an
    // expected path alone could follow the same redirect outside Git metadata.
    let Some(relative) = test_run_registration_relative_parent(&common, &parent) else {
        return false;
    };
    fs::canonicalize(common.join(relative)).is_ok_and(|expected| expected == parent)
}

// Pure native-component guard; it also covers non-UTF8 path representations on
// systems whose filesystems cannot create such names (for example APFS).
fn test_run_registration_relative_parent<'a>(common: &FsPath, parent: &'a FsPath) -> Option<&'a FsPath> {
    let relative = parent.strip_prefix(common).ok()?;
    use std::path::Component::Normal;
    let components: Vec<_> = relative.components().collect();
    let shape = match components.as_slice() {
        [Normal(runs)] => *runs == "review-runs",
        [Normal(worktrees), Normal(_admin), Normal(runs)] => {
            *worktrees == "worktrees" && *runs == "review-runs"
        }
        _ => false,
    };
    shape.then_some(relative)
}
