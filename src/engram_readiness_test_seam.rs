// Test-only seam for Engram readiness, for tests whose subject is not
// readiness timing.
//
// What it owns:
// - Staged readiness. For a registered Engram home, `run_engram_readiness`
//   takes the receipt the control fixture would print for the `fixture-ready`
//   declaration, built here in-process, instead of launching the fixture. The
//   production path still re-reads the declaration, parses and validates the
//   receipt, and derives the store identity from it.
// - Expired launches. For a registered home, every real diagnostic process
//   launched through `run_engram_diagnostic_args_until` is treated as having
//   outlasted its budget the moment it resumes, whether or not the child has
//   already exited (on Unix it is not held suspended), and takes the same
//   cleanup and error as a real expiry. Held together with staged
//   readiness, it proves that a test no longer races a real process launch
//   against the product's wall clock: a launch would fail the test at once.
//   Held alone, it reproduces, without depending on load, the failure a
//   stalled machine causes in such a test.
//
// What it does not own: the readiness budget (ENGRAM_READINESS_TIMEOUT is the
// product's and unchanged), any receipt other than the `fixture-ready` one,
// and other Engram processes (authority revoke and control calls keep their
// own launches and budgets).
//
// New file, not split from another; included from main.rs under cfg(test).

static TEST_STAGED_ENGRAM_READINESS_HOMES: LazyLock<Mutex<HashSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

static TEST_EXPIRED_ENGRAM_DIAGNOSTIC_HOMES: LazyLock<Mutex<HashSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Registration of Engram homes with the readiness seam, removed on drop.
#[must_use = "the seam registration ends when this guard is dropped"]
struct TestEngramReadinessSeam {
    homes: Vec<PathBuf>,
    staged: bool,
}

impl Drop for TestEngramReadinessSeam {
    fn drop(&mut self) {
        let mut expired = TEST_EXPIRED_ENGRAM_DIAGNOSTIC_HOMES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for home in &self.homes {
            expired.remove(home);
        }
        drop(expired);
        if self.staged {
            let mut staged = TEST_STAGED_ENGRAM_READINESS_HOMES
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for home in &self.homes {
                staged.remove(home);
            }
        }
    }
}

/// The key a home is registered and looked up by. Homes exist when a test
/// registers them and when readiness runs, so canonical paths make the
/// lookup independent of how the settings spelled the home.
fn test_engram_readiness_seam_key(home: &FsPath) -> PathBuf {
    fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf())
}

fn register_test_engram_readiness_seam(homes: &[&FsPath], staged: bool) -> TestEngramReadinessSeam {
    let homes: Vec<PathBuf> = homes
        .iter()
        .map(|home| test_engram_readiness_seam_key(home))
        .collect();
    // Check every home before inserting any, and report a double registration
    // only after the guard is released: a panic under the guard would poison
    // the registry for every other test in the binary and leave the homes
    // inserted so far registered with no guard to remove them.
    let already = {
        let mut expired = TEST_EXPIRED_ENGRAM_DIAGNOSTIC_HOMES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let already = homes.iter().find(|home| expired.contains(*home)).cloned();
        if already.is_none() {
            expired.extend(homes.iter().cloned());
        }
        already
    };
    if let Some(home) = already {
        panic!(
            "Engram home {} is already registered with the readiness seam",
            home.display()
        );
    }
    if staged {
        TEST_STAGED_ENGRAM_READINESS_HOMES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend(homes.iter().cloned());
    }
    TestEngramReadinessSeam { homes, staged }
}

/// Every real diagnostic launch for these homes outlasts its budget at once.
fn expire_test_engram_diagnostic_launches(homes: &[&FsPath]) -> TestEngramReadinessSeam {
    register_test_engram_readiness_seam(homes, false)
}

/// Readiness for these homes is answered without a process, and any real
/// diagnostic launch for them would outlast its budget at once.
fn stage_test_engram_readiness_without_launch(homes: &[&FsPath]) -> TestEngramReadinessSeam {
    register_test_engram_readiness_seam(homes, true)
}

/// Whether a diagnostic launch for `home` has outlasted its budget the moment
/// its process resumes. The caller then takes its expiry branch whether or not
/// the child has already exited.
fn test_engram_diagnostic_launch_expired(home: &FsPath) -> bool {
    TEST_EXPIRED_ENGRAM_DIAGNOSTIC_HOMES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(&test_engram_readiness_seam_key(home))
}

/// After an expired launch is terminated, wait for its direct child to exit,
/// so a fixture started in a test temp root holds nothing there (its working
/// directory included) when the test removes the root. The real expiry branch
/// reaps in the background instead; this wait is test-only and bounded, a
/// guard against a hang rather than a timing assertion.
fn wait_for_test_engram_expired_child(process: &SharedChild) {
    let guard = std::time::Instant::now() + Duration::from_secs(30);
    while matches!(process.try_wait(), Ok(None)) && std::time::Instant::now() < guard {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The staged readiness output for a registered home, or None to launch the
/// real process.
fn staged_test_engram_readiness_output(
    marker: &FsPath,
    home: &FsPath,
) -> Option<std::result::Result<std::process::Output, ApiError>> {
    let staged = TEST_STAGED_ENGRAM_READINESS_HOMES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(&test_engram_readiness_seam_key(home));
    staged.then(|| staged_test_engram_readiness_receipt(marker, home))
}

/// The receipt src/tests/fixtures/engram-control-fixture.ps1 (and .sh) prints
/// for `readiness` under the `fixture-ready` declaration, including the store
/// file it creates under the home.
fn staged_test_engram_readiness_receipt(
    marker: &FsPath,
    home: &FsPath,
) -> std::result::Result<std::process::Output, ApiError> {
    let project_id = read_engram_diagnostic_declaration(marker)?
        .trim()
        .to_owned();
    if project_id != "fixture-ready" {
        return Err(ApiError::bad_request(format!(
            "staged readiness answers only the fixture-ready declaration, not {project_id}"
        )));
    }
    let database = work_database_path(home, &project_id);
    let store_error =
        |error: io::Error| ApiError::bad_request(format!("staged readiness store: {error}"));
    fs::create_dir_all(database.parent().expect("store path has a parent")).map_err(store_error)?;
    if !database.exists() {
        fs::write(&database, "fixture database").map_err(store_error)?;
    }
    let database = normalize_user_facing_path(&fs::canonicalize(&database).map_err(store_error)?);
    let receipt = serde_json::json!({
        "schema_version": 1,
        "scope": "readiness",
        "ready": true,
        "full_audit": "not_run",
        "mutation_enabled": false,
        "work_schema_version": 1,
        "host_path_policy": { "stored": "fixture", "resolved": "fixture", "status": "matched" },
        "control": { "required_assurance": "turn_gated" },
        "database": database.to_string_lossy(),
        "project_id": project_id,
    });
    Ok(std::process::Output {
        status: test_engram_readiness_success_status(),
        stdout: serde_json::to_vec(&receipt).expect("staged readiness receipt serializes"),
        stderr: Vec::new(),
    })
}

#[cfg(windows)]
fn test_engram_readiness_success_status() -> std::process::ExitStatus {
    std::os::windows::process::ExitStatusExt::from_raw(0)
}

#[cfg(unix)]
fn test_engram_readiness_success_status() -> std::process::ExitStatus {
    std::os::unix::process::ExitStatusExt::from_raw(0)
}
