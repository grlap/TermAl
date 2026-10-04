// Test-only temporary directory for terminal shell children.
//
// What it owns: under test, the TEMP and TMP a terminal shell is given when
// its workdir lies inside a test's own temp root: a `child-temp` directory in
// that test root, the directory directly under test_temp_dir() that holds
// the workdir. Windows PowerShell writes a startup policy probe
// (__PSScriptPolicyTest_*.ps1 and .psm1) to its TEMP at every start and
// deletes it moments later, so a shell, or a PowerShell child it starts,
// terminated in between leaves the pair behind. The test launcher points the
// test process's TEMP at the shared run root, which no test's own temp root
// cleans; the test root holding this directory is removed by its test, and
// the probe with it. The directory sits in the test root rather than in the
// workdir, so a workdir that is a repository gains no untracked entry.
//
// It creates and redirects nothing for a workdir outside test_temp_dir(), or
// for test_temp_dir() itself.
//
// What it does not own: production launches (this file is cfg(test) only and
// production shells keep the inherited TEMP), Engram fixture children
// (src/engram_test_child_temp.rs), the launcher's temp guard (unchanged), and
// when a child is terminated (the callers' timeouts and job teardown).
//
// New file, not split from another; included from main.rs under cfg(test).

/// The environment a test's terminal shell gets so that its temporary files
/// stay inside its test root. Empty for a workdir outside the test temp
/// directory, so nothing is created there.
fn terminal_test_child_temp_env(workdir: &FsPath) -> Vec<(&'static str, std::ffi::OsString)> {
    let Some(temp) = terminal_test_child_temp(workdir) else {
        return Vec::new();
    };
    vec![
        ("TEMP", temp.clone().into_os_string()),
        ("TMP", temp.into_os_string()),
    ]
}

/// The `child-temp` directory in the test root that holds `workdir`, created
/// on demand, spelled from test_temp_dir() (so without a verbatim prefix).
/// None, with nothing created, unless `workdir` lies strictly inside
/// test_temp_dir().
fn terminal_test_child_temp(workdir: &FsPath) -> Option<PathBuf> {
    let base = test_temp_dir();
    let canonical_base = fs::canonicalize(&base).ok()?;
    let canonical_workdir = fs::canonicalize(workdir).ok()?;
    let first = match canonical_workdir
        .strip_prefix(&canonical_base)
        .ok()?
        .components()
        .next()?
    {
        std::path::Component::Normal(name) => name.to_owned(),
        _ => return None,
    };
    let test_root = base.join(&first);
    let canonical_test_root = canonical_base.join(&first);
    if !canonical_test_root.is_dir() {
        return None;
    }
    let temp = test_root.join("child-temp");
    fs::create_dir_all(&temp).ok()?;
    // An existing child-temp that is a symlink or junction would carry TEMP
    // somewhere else, even to another directory of the same test root such
    // as a repository workdir: accept only a real directory that is itself
    // `<test root>/child-temp`, never a link.
    if fs::symlink_metadata(&temp).ok()?.file_type().is_symlink() {
        return None;
    }
    let canonical_temp = fs::canonicalize(&temp).ok()?;
    if canonical_temp != canonical_test_root.join("child-temp") {
        return None;
    }
    Some(temp)
}
