use super::*;

struct OwnerFixtureRetention<'a>(&'a TestTempRoot);

impl Drop for OwnerFixtureRetention<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.retain_for_diagnosis();
            eprintln!("source_owner retained_fixture_parent={}", self.0.path().display());
        }
    }
}

fn owner_fixture(module: &FsPath, parent: &FsPath, id: &str) -> Value {
    let mut command = BoundedReadCommand::new("node");
    command.current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg(FsPath::new(env!("CARGO_MANIFEST_DIR")).join("scripts/test-source-owner-fixture.mjs"))
        .arg(module).arg(parent).arg(id);
    // Fixture construction is test setup, separate from each production capture's
    // eight-second budget. Reuse the existing finite Git setup/read budget.
    let output = run_bounded_read_command(&mut command,
        std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT,
        ENGRAM_SOURCE_JSON_LIMIT, true).expect("owner fixture should complete");
    assert!(output.status.success(), "fixture {id}: {}",
        String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).expect("owner fixture should return JSON")
}

fn measure_owner_source(
    id: &str, binary: &FsPath, digest: &str, root: &FsPath,
    pin: &mut Option<EngramSourceOwnerPin>,
) -> Result<EngramSourceId> {
    let started = std::time::Instant::now();
    let started_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?.as_millis();
    let deadline = started + ENGRAM_SOURCE_COMMAND_BUDGET;
    let phase = if pin.is_some() { "capture" } else { "bootstrap" };
    let mut calls = 0;
    let mut run = |program: &FsPath, cwd: &FsPath, args: &[String],
        until: std::time::Instant, limit: usize| {
        calls += 1;
        let operation_started = std::time::Instant::now();
        let operation_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?.as_millis();
        let remaining_before = until.saturating_duration_since(operation_started).as_millis();
        let output = engram_source_owner_run(program, cwd, args, until, limit);
        eprintln!("source_owner operation={}", json!({
            "case": id, "phase": phase, "operation": args[0],
            "started_unix_ms": operation_unix_ms,
            "elapsed_ms": operation_started.elapsed().as_millis(),
            "remaining_before_ms": remaining_before,
            "remaining_after_ms": until.saturating_duration_since(std::time::Instant::now()).as_millis(),
            "exit_code": output.as_ref().ok().and_then(|output| output.status.code()),
            "error": output.as_ref().err().map(|error|
                format!("{error:#}").chars().take(1024).collect::<String>()),
        }));
        output
    };
    let captured = if let Some(pin) = pin.as_ref() {
        engram_source_id_with(pin, root, deadline, &mut run)
    } else {
        engram_source_owner_observe_with(binary, root, root, deadline, &mut run)
            .and_then(|(observed_pin, observation)| {
                assert_eq!(observed_pin.sha256, digest, "preserve the explicit candidate pin");
                *pin = Some(observed_pin);
                // This is the bootstrap's actual measurement of this root, now.
                // No second schema probe or later measurement is substituted.
                observation.into_source_id(deadline)
            })
    };
    eprintln!("source_owner measurement={}", json!({
        "case": id, "phase": phase, "elapsed_ms": started.elapsed().as_millis(),
        "started_unix_ms": started_unix_ms,
        "process_calls": calls,
        "remaining_ms": deadline.saturating_duration_since(std::time::Instant::now()).as_millis(),
        "error": captured.as_ref().err().map(|error|
            format!("{error:#}").chars().take(1024).collect::<String>()),
    }));
    captured
}

#[test]
#[ignore = "requires a pinned candidate Engram binary and its exact shared fixture module"]
fn source_owner_live_shared_fixtures_preserve_complete_producer_identity() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let parent = TestTempRoot::create("so");
    let _retention = OwnerFixtureRetention(&parent);
    let module = PathBuf::from(std::env::var_os("TERMAL_TEST_SOURCE_FIXTURES")
        .expect("TERMAL_TEST_SOURCE_FIXTURES must name the exact owner fixture module"));
    assert!(module.is_absolute() && module.is_file());
    let binary = PathBuf::from(std::env::var_os("TERMAL_TEST_LIVE_ENGRAM_BINARY")
        .expect("TERMAL_TEST_LIVE_ENGRAM_BINARY must name the pinned candidate"));
    assert!(binary.is_absolute() && binary.is_file());
    let digest = std::env::var("TERMAL_TEST_LIVE_ENGRAM_SHA256")
        .expect("TERMAL_TEST_LIVE_ENGRAM_SHA256 must identify the pinned candidate");
    assert_eq!(digest.len(), 64);
    assert!(is_lowercase_hex(&digest));
    let until = || std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET;
    // Match the supplied pin before running any candidate code.
    assert_eq!(engram_source_binary_hash(&binary, until()).unwrap(), digest);

    let cases = owner_fixture(&module, &parent, "--list");
    let cases = cases.as_array().expect("owner case list");
    assert!(!cases.is_empty());
    let mut names = HashSet::new();
    let mut passed = 0;
    let mut skipped = 0;
    let mut pin = None;
    for case in cases {
        let id = case["id"].as_str().expect("owner case id");
        assert!(names.insert(id), "duplicate owner case {id}");
        if case["skipped"].as_bool().expect("explicit platform disposition") {
            eprintln!("source_owner case={id} skipped=owner-platform-rule");
            skipped += 1;
            continue;
        }
        let fixture = owner_fixture(&module, &parent, id);
        let root = FsPath::new(fixture["root"].as_str().expect("fixture root"));
        assert!(root.is_absolute() && root.starts_with(parent.as_ref()));
        let captured = measure_owner_source(id, &binary, &digest, root, &mut pin);
        if fixture["status"] == "unavailable" {
            assert!(fixture["sourceId"].is_null(), "{id}: no partial expectation");
            let error = captured.expect_err("owner unavailable must not yield an identity");
            assert!(error.to_string().contains("InputUnavailable"), "{id}: {error:#}");
            eprintln!("source_owner case={id} unavailable={error}");
        } else {
            let actual = captured.unwrap_or_else(|error| panic!("{id}: {error:#}"));
            let expected: EngramSourceId = serde_json::from_value(fixture["sourceId"].clone())
                .expect("runtime-derived owner identity");
            assert_eq!(actual, expected, "{id}: compare the whole owner value");
            let actual = serde_json::to_value(actual).unwrap();
            assert_eq!(actual, fixture["sourceId"], "{id}: preserve every wire field");
            eprintln!("source_owner case={id} source_id={actual}");
        }
        passed += 1;
    }
    assert!(passed > 0);
    eprintln!("source_owner conformance passed={passed} skipped={skipped}");
}

#[test]
#[ignore = "requires a pinned owner binary and its exact shared fixture module"]
fn source_owner_live_child_fixture_reports_observation_costs() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let parent = TestTempRoot::create("sd");
    let _retention = OwnerFixtureRetention(&parent);
    let module = PathBuf::from(std::env::var_os("TERMAL_TEST_SOURCE_FIXTURES")
        .expect("TERMAL_TEST_SOURCE_FIXTURES must name the exact owner fixture module"));
    assert!(module.is_absolute() && module.is_file());
    let binary = PathBuf::from(std::env::var_os("TERMAL_TEST_LIVE_ENGRAM_BINARY")
        .expect("TERMAL_TEST_LIVE_ENGRAM_BINARY must name the pinned candidate"));
    let digest = std::env::var("TERMAL_TEST_LIVE_ENGRAM_SHA256")
        .expect("TERMAL_TEST_LIVE_ENGRAM_SHA256 must identify the pinned candidate");
    assert!(binary.is_absolute() && binary.is_file());
    assert!(is_lowercase_sha256(&digest));
    assert_eq!(engram_source_binary_hash(&binary,
        std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET).unwrap(), digest);
    // A new instance of the owner's one case, not a reconstruction of the
    // missing original fixture or a retry of the full conformance matrix.
    let fixture = owner_fixture(&module, &parent, "child-dirty");
    let root = FsPath::new(fixture["root"].as_str().expect("owner fixture root"));
    assert!(root.is_absolute() && root.starts_with(parent.as_ref()));
    assert_eq!(fixture["status"], "not_clean");
    let expected: EngramSourceId = serde_json::from_value(fixture["sourceId"].clone()).unwrap();
    eprintln!("source_owner diagnostic_fixture_root={}", root.display());
    let started = std::time::Instant::now();
    let mut pin = None;
    let first = measure_owner_source("child-dirty-first", &binary, &digest, root, &mut pin)
        .expect("the first bounded observation must return a complete source_id");
    eprintln!("source_owner case=child-dirty-first source_id={}", serde_json::to_value(&first).unwrap());
    assert_eq!(first, expected, "the first observation preserves the owner's complete identity");
    let second = measure_owner_source("child-dirty-second", &binary, &digest, root, &mut pin)
        .expect("the subsequent bounded observation must return a complete source_id");
    eprintln!("source_owner case=child-dirty-second source_id={}", serde_json::to_value(&second).unwrap());
    eprintln!("source_owner child_pair_elapsed_ms={}", started.elapsed().as_millis());
    assert_eq!(second, expected, "the subsequent observation preserves the same owner identity");
}
