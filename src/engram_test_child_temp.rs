// Test-only temporary directory for Engram fixture children.
//
// What it owns: under test, the TEMP, TMP and TMPDIR an Engram fixture child
// is given, a directory inside its Engram home. Windows PowerShell writes a
// startup policy probe (__PSScriptPolicyTest_*.ps1 and .psm1) to its TEMP and
// deletes it moments later, so a child terminated in that window leaves the
// files behind. The test launcher points the test process's TEMP at the
// shared run root, which no test's own temp root can clean. A home inside a
// test temp root is removed with that root, and the probe with it. The child
// is also told the test's user temp (TERMAL_TEST_USER_TEMP), which the
// fixtures use to find the test temp root once TEMP no longer points at it.
//
// It creates and redirects nothing for a home outside the test temp root,
// mirroring the fixtures' own refusal to write to such a home.
//
// What it does not own: production launches (this file is cfg(test) only and
// production children keep the inherited TEMP), the launcher's temp guard
// (unchanged), and when a child is terminated (the callers' deadlines).
//
// New file, not split from another; included from main.rs under cfg(test).

/// The user temp the test process uses, as test_temp_dir() derives it: the
/// launcher's TERMAL_TEST_USER_TEMP, else the platform temp directory.
fn engram_test_user_temp() -> std::ffi::OsString {
    std::env::var_os("TERMAL_TEST_USER_TEMP")
        .unwrap_or_else(|| std::env::temp_dir().into_os_string())
}

/// The environment a test's Engram fixture child gets so that its temporary
/// files stay inside its Engram home. Empty for a home that does not exist or
/// lies outside the test temp root, so nothing is created there.
fn engram_test_child_temp_env(home: &FsPath) -> Vec<(&'static str, std::ffi::OsString)> {
    let Some(temp) = engram_test_child_temp(home) else {
        return Vec::new();
    };
    vec![
        ("TEMP", temp.clone().into_os_string()),
        ("TMP", temp.clone().into_os_string()),
        ("TMPDIR", temp.into_os_string()),
        ("TERMAL_TEST_USER_TEMP", engram_test_user_temp()),
    ]
}

/// The directory inside an Engram home that a test's fixture child uses as
/// its TEMP, created on demand. None, with nothing created, unless the home
/// exists inside <user temp>/termal/tests, the root the fixtures require.
fn engram_test_child_temp(home: &FsPath) -> Option<PathBuf> {
    let test_root = PathBuf::from(engram_test_user_temp())
        .join("termal")
        .join("tests");
    let home = fs::canonicalize(home).ok()?;
    let test_root = fs::canonicalize(test_root).ok()?;
    if !home.is_dir() || !home.starts_with(&test_root) || home == test_root {
        return None;
    }
    let temp = home.join("child-temp");
    fs::create_dir_all(&temp).ok()?;
    // An existing child-temp that is a symlink or junction would carry TEMP
    // somewhere else: accept it only if it resolves to a real directory
    // directly inside the canonical home.
    let temp = fs::canonicalize(&temp).ok()?;
    if temp.parent() != Some(home.as_path()) {
        return None;
    }
    Some(temp)
}
