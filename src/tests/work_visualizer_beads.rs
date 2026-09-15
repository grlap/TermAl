// New Beads Work read tests: metadata-only detection, native-binary
// resolution rules, read-only argv transport, receipt normalization and the
// detail route. Every store here is an empty fixture directory; no bd runs.
use super::*;

// The adapter resolves its binary through `TERMAL_BEADS_BINARY`; tests inject
// it through the adapter's process-wide override (never the OS environment)
// and serialize on this lock so parallel tests never observe each other.
static BEADS_ENV_LOCK: Mutex<()> = Mutex::new(());

fn beads_fixture_binary() -> PathBuf {
    FsPath::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/tests/fixtures")
        .join(if cfg!(windows) {
            "beads-reader.ps1"
        } else {
            "beads-reader.sh"
        })
}

fn beads_fixture(name: &str) -> (AppState, String, PathBuf) {
    let state = test_app_state();
    let root = state
        .persistence_path
        .parent()
        .unwrap()
        .join(format!("beads-{name}"));
    fs::create_dir_all(root.join(".beads")).unwrap();
    let project = create_test_project(&state, &root, "Beads fixture");
    (state, project, root)
}

// The override is cleared in `drop` before the lock field is released.
struct BeadsEnv {
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Drop for BeadsEnv {
    fn drop(&mut self) {
        *BEADS_BINARY_OVERRIDE
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// `Some(path)` configures the binary; `None` models an unset variable.
fn with_beads_binary(value: Option<&FsPath>) -> BeadsEnv {
    let lock = BEADS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    *BEADS_BINARY_OVERRIDE
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(value.map(|path| path.as_os_str().to_owned()));
    BeadsEnv { _lock: lock }
}

fn beads_status(response: &WorkListResponse) -> &WorkSourceStatus {
    response
        .sources
        .iter()
        .find(|s| s.source == "beads")
        .expect("beads source status")
}

#[test]
fn beads_detection_is_metadata_only_and_names_the_missing_binary() {
    // A runnable fixture is configured, so a read would leave its argv file:
    // its absence proves detection of a project without `.beads` runs no bd.
    let binary = beads_fixture_binary();
    let state = test_app_state();
    let root = state
        .persistence_path
        .parent()
        .unwrap()
        .join("beads-absent");
    fs::create_dir_all(&root).unwrap();
    let project = create_test_project(&state, &root, "No beads");
    {
        let _binary = with_beads_binary(Some(&binary));
        let response = state
            .list_project_work(&project, WorkListQuery::default())
            .unwrap();
        assert_eq!(beads_status(&response).state, "absent");
        assert!(response.beads.is_none());
        assert!(!root.join("beads-read-args.txt").exists(), "no read ran");
    }

    // With a store but no usable binary, the source is unavailable and names
    // the binary; nothing is created and no read is attempted.
    let _binary = with_beads_binary(Some(FsPath::new(if cfg!(windows) {
        "C:/definitely/missing/bd.exe"
    } else {
        "/definitely/missing/bd"
    })));
    fs::create_dir_all(root.join(".beads")).unwrap();
    let response = state
        .list_project_work(&project, WorkListQuery::default())
        .unwrap();
    let status = beads_status(&response);
    assert_eq!(status.state, "unavailable", "{}", status.message);
    assert!(status.message.contains("Beads binary unavailable"));
    assert!(response.beads.is_none());
}

#[test]
fn beads_binary_validation_rejects_shims_and_relative_paths() {
    for relative in ["bd", "bin/bd.exe"] {
        assert!(validate_beads_binary(FsPath::new(relative), false).is_err());
    }
    let dir = std::env::temp_dir().join(format!("termal-beads-shim-{}", Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    for shim in ["bd.cmd", "bd.bat", "bd.js"] {
        let path = dir.join(shim);
        fs::write(&path, "echo shim").unwrap();
        let error = validate_beads_binary(&path, true).unwrap_err();
        assert!(error.contains("native bd binary"), "{shim}: {error}");
    }
    // Production policy: interpreter scripts and shebang files are shims even
    // when a test policy would admit them.
    for script in ["bd.ps1", "bd.sh"] {
        let path = dir.join(script);
        fs::write(&path, "echo fixture").unwrap();
        assert!(validate_beads_binary(&path, true).is_ok(), "{script}");
        let error = validate_beads_binary(&path, false).unwrap_err();
        assert!(error.contains("shell or Node shim"), "{script}: {error}");
    }
    let shebang = dir.join("bd");
    fs::write(&shebang, "#!/usr/bin/env node\nrequire('./bd.js')\n").unwrap();
    assert!(validate_beads_binary(&shebang, true).is_ok());
    let error = validate_beads_binary(&shebang, false).unwrap_err();
    assert!(error.contains("script shim"), "{error}");
    // The npm launcher resolves to the native binary installed beside it.
    let launcher = dir.join("bd.cmd");
    let native_dir = dir
        .join("node_modules")
        .join("@beads")
        .join("bd")
        .join("bin");
    fs::create_dir_all(&native_dir).unwrap();
    let native = native_dir.join(if cfg!(windows) { "bd.exe" } else { "bd" });
    fs::write(&native, b"MZ native stand-in").unwrap();
    assert_eq!(
        beads_native_beside(&launcher, false).as_deref(),
        Some(native.as_path())
    );
    // A configured launcher resolves to the native binary beside it, as on
    // PATH; a configured path with nothing native beside it keeps its error.
    {
        let _binary = with_beads_binary(Some(&launcher));
        assert_eq!(resolve_beads_binary(false), Ok(native.clone()));
    }
    {
        let _binary = with_beads_binary(Some(&dir.join("alone").join("bd.js")));
        assert!(
            resolve_beads_binary(false)
                .unwrap_err()
                .contains("native bd binary")
        );
    }
    // A relative configured path is rejected before the launcher fallback
    // could resolve it against the server's cwd.
    {
        let _binary = with_beads_binary(Some(FsPath::new("bd.cmd")));
        assert!(
            resolve_beads_binary(false)
                .unwrap_err()
                .contains("must be absolute")
        );
    }
    // The PATH search with the production policy: the shim in the directory
    // is skipped and the native binary installed beside it wins.
    let path_var = std::ffi::OsString::from(&dir);
    assert_eq!(
        resolve_beads_binary_on_path(Some(path_var.as_os_str()), false),
        Ok(native.clone())
    );
    let empty = dir.join("empty");
    fs::create_dir_all(&empty).unwrap();
    let empty_path = std::ffi::OsString::from(&empty);
    assert!(
        resolve_beads_binary_on_path(Some(empty_path.as_os_str()), false)
            .unwrap_err()
            .contains("not found on PATH")
    );
    assert!(
        resolve_beads_binary_on_path(None, false)
            .unwrap_err()
            .contains("PATH is not set")
    );
    // A relative PATH entry is never searched: it would resolve against the
    // server's cwd.
    let relative = std::ffi::OsString::from(".");
    assert!(
        resolve_beads_binary_on_path(Some(relative.as_os_str()), false)
            .unwrap_err()
            .contains("not found on PATH")
    );
    fs::write(&native, "#!/bin/sh\nexec node bd.js\n").unwrap();
    assert_eq!(
        beads_native_beside(&launcher, false),
        None,
        "a shebang beside the launcher is not native"
    );
    assert!(resolve_beads_binary_on_path(Some(path_var.as_os_str()), false).is_err());
    assert!(is_beads_identifier("tm-h6uc.3"));
    for bad in ["", "-x", "tm x", "tm\u{7}x", "tm/x"] {
        assert!(!is_beads_identifier(bad), "{bad:?}");
    }
    fs::remove_dir_all(dir).ok();
}

#[test]
fn beads_list_reads_once_with_readonly_json_and_keeps_relations_separate() {
    let binary = beads_fixture_binary();
    let _binary = with_beads_binary(Some(&binary));
    let (state, project, root) = beads_fixture("list");
    let response = state
        .list_project_work(&project, WorkListQuery::default())
        .unwrap();
    assert_eq!(beads_status(&response).state, "ready");
    let page = response.beads.expect("beads page");
    assert_eq!(page.total, 3);
    assert!(!page.more && page.after.is_none());
    let by_id = |id: &str| page.items.iter().find(|i| i.id == id).unwrap();
    let root_epic = by_id("tm-root");
    assert_eq!(root_epic.source, "beads");
    assert_eq!(
        (
            root_epic.lifecycle.as_str(),
            root_epic.availability.as_str()
        ),
        ("open", "active")
    );
    assert_eq!(root_epic.assigned_to.as_deref(), Some("Termal::Codex"));
    let child = by_id("tm-root.1");
    assert_eq!(
        child.parent_id.as_deref(),
        Some("tm-root"),
        "hierarchy from the parent-child edge"
    );
    assert_eq!(child.availability, "blocked");
    assert_eq!(
        child.blocked_by,
        vec!["tm-free".to_owned()],
        "only the open blocker still blocks"
    );
    assert_eq!(
        child.prerequisites,
        vec![
            WorkPrerequisiteView {
                id: "tm-free".into(),
                satisfied: false
            },
            WorkPrerequisiteView {
                id: "tm-closed".into(),
                satisfied: true
            },
        ]
    );
    let free = by_id("tm-free");
    assert_eq!(free.availability, "ready");
    assert!(free.prerequisites.is_empty() && free.parent_id.is_none());
    // Two read-only JSON commands per snapshot: the list, whose rows carry
    // their edges inline (all edge types), and one status batch for the
    // blocker missing from the open snapshot. No per-row dependency read.
    let log = fs::read_to_string(root.join("beads-read-args-log.txt")).unwrap();
    let commands = log.lines().collect::<Vec<_>>();
    assert_eq!(
        commands,
        [
            "--readonly --json list --limit 0",
            "--readonly --json show tm-closed",
        ]
    );
    let wire = serde_json::to_value(&page).unwrap();
    assert_eq!(wire["items"][1]["prerequisites"][1]["satisfied"], true);
}

#[test]
fn beads_every_row_carries_its_edges_in_the_list_receipt() {
    let binary = beads_fixture_binary();
    let _binary = with_beads_binary(Some(&binary));
    let (state, project, root) = beads_fixture("two-dependents");
    // Two rows declare blockers: both sets of edges come from the list
    // receipt, reconciled per row, and their shared closed blocker is read
    // once.
    fs::write(root.join("beads-fixture-two-dependents"), "x").unwrap();
    let page = state
        .list_project_work(&project, WorkListQuery::default())
        .unwrap()
        .beads
        .expect("beads snapshot");
    let by_id = |id: &str| page.items.iter().find(|i| i.id == id).unwrap();
    let free = by_id("tm-free");
    assert_eq!(free.availability, "ready", "its only blocker is closed");
    assert_eq!(
        free.prerequisites,
        vec![WorkPrerequisiteView {
            id: "tm-closed".into(),
            satisfied: true
        }]
    );
    let child = by_id("tm-root.1");
    assert_eq!(child.availability, "blocked");
    assert_eq!(child.parent_id.as_deref(), Some("tm-root"));
    assert!(
        page.hint.as_deref().unwrap_or_default().is_empty(),
        "every row answered: {:?}",
        page.hint
    );
    let log = fs::read_to_string(root.join("beads-read-args-log.txt")).unwrap();
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        [
            "--readonly --json list --limit 0",
            "--readonly --json show tm-closed",
        ]
    );
    fs::remove_file(root.join("beads-fixture-two-dependents")).unwrap();
}

#[test]
fn beads_blocker_references_that_are_not_identifiers_are_disclosed_unread() {
    let binary = beads_fixture_binary();
    let _binary = with_beads_binary(Some(&binary));
    let (state, project, root) = beads_fixture("external");
    // A cross-project reference is never passed to bd: it stays unsatisfied
    // and is counted with the unread statuses, never dropped silently.
    fs::write(root.join("beads-fixture-external-blocker"), "x").unwrap();
    let page = state
        .list_project_work(&project, WorkListQuery::default())
        .unwrap()
        .beads
        .expect("beads snapshot");
    let child = page.items.iter().find(|i| i.id == "tm-root.1").unwrap();
    assert_eq!(child.availability, "blocked");
    assert!(
        child
            .prerequisites
            .iter()
            .any(|p| p.id == "external:other:tm-1" && !p.satisfied),
        "{:?}",
        child.prerequisites
    );
    let hint = page.hint.clone().unwrap_or_default();
    assert!(
        hint.contains("1 blocker status could not be read")
            && hint.contains("not a Beads identifier"),
        "{hint}"
    );
    assert_eq!(
        fs::read_to_string(root.join("beads-read-args-log.txt"))
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "--readonly --json list --limit 0",
            "--readonly --json show tm-closed",
        ],
        "only the Beads identifier is looked up"
    );
    fs::remove_file(root.join("beads-fixture-external-blocker")).unwrap();
}

#[test]
fn beads_label_beyond_the_launch_bound_is_a_source_error_not_a_failed_request() {
    let binary = beads_fixture_binary();
    let _binary = with_beads_binary(Some(&binary));
    let (state, project, root) = beads_fixture("launch-bound");
    // A label that passes query validation but would push the bd argv past
    // the process launch bound is something this source cannot serve: an
    // explicit Beads error, never a failed request that drops Engram's page.
    let response = state
        .list_project_work(
            &project,
            WorkListQuery {
                label: Some("x".repeat(16_000)),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(response.beads.is_none());
    let status = beads_status(&response);
    assert_eq!(status.state, "error");
    assert!(
        status.message.contains("launch bound"),
        "{}",
        status.message
    );
    assert!(
        !root.join("beads-read-args.txt").exists(),
        "nothing launched past the bound"
    );
}

#[test]
fn beads_filters_apply_to_the_loaded_snapshot_and_labels_are_disclosed() {
    let binary = beads_fixture_binary();
    let _binary = with_beads_binary(Some(&binary));
    let (state, project, root) = beads_fixture("filters");
    let searched = state
        .list_project_work(
            &project,
            WorkListQuery {
                search: Some("READY".into()),
                ..Default::default()
            },
        )
        .unwrap()
        .beads
        .unwrap();
    assert_eq!(
        searched
            .items
            .iter()
            .map(|i| i.id.as_str())
            .collect::<Vec<_>>(),
        ["tm-free"]
    );
    let blocked = state
        .list_project_work(
            &project,
            WorkListQuery {
                availability: Some("blocked".into()),
                ..Default::default()
            },
        )
        .unwrap()
        .beads
        .unwrap();
    assert_eq!(
        blocked
            .items
            .iter()
            .map(|i| i.id.as_str())
            .collect::<Vec<_>>(),
        ["tm-root.1"]
    );
    let labelled = state
        .list_project_work(
            &project,
            WorkListQuery {
                label: Some("decision".into()),
                ..Default::default()
            },
        )
        .unwrap()
        .beads
        .unwrap();
    // Labels are filtered by bd itself (`--label=`), never by clearing rows.
    assert_eq!(
        labelled
            .items
            .iter()
            .map(|i| i.id.as_str())
            .collect::<Vec<_>>(),
        ["tm-root"]
    );
    assert!(labelled.hint.is_none());
    let log = fs::read_to_string(root.join("beads-read-args-log.txt")).unwrap();
    assert!(
        log.lines()
            .any(|line| line == "--readonly --json list --limit 0 --label=decision"),
        "{log}"
    );
}

#[test]
fn beads_admission_exhaustion_is_a_source_error_not_a_failed_request() {
    let binary = beads_fixture_binary();
    let _binary = with_beads_binary(Some(&binary));
    let (state, project, root) = beads_fixture("busy");
    let response = state
        .list_project_work_with_admission(
            &project,
            WorkListQuery::default(),
            || Ok(()),
            || -> Result<(), ApiError> {
                Err(ApiError::from_status(
                    StatusCode::TOO_MANY_REQUESTS,
                    "Beads reads busy; retry shortly",
                ))
            },
        )
        .unwrap();
    let status = beads_status(&response);
    assert_eq!(status.state, "error");
    assert!(status.message.contains("busy"), "{}", status.message);
    assert!(response.beads.is_none());
    assert!(
        !root.join("beads-read-args.txt").exists(),
        "no read ran without a permit"
    );
    // An internal failure on the Beads path is not a source condition: the
    // request fails, as it does for Engram.
    let internal = state
        .list_project_work_with_admission(
            &project,
            WorkListQuery::default(),
            || Ok(()),
            || -> Result<(), ApiError> { Err(ApiError::internal("Beads read limiter closed")) },
        )
        .unwrap_err();
    assert_eq!(internal.status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn engram_first_page_failure_keeps_the_beads_snapshot() {
    let binary = beads_fixture_binary();
    let _binary = with_beads_binary(Some(&binary));
    let (state, project, _, root) = super::work_visualizer::fixture();
    super::work_visualizer::install_store(&state, &project, &root);
    fs::create_dir_all(root.join(".beads")).unwrap();
    // The Engram fixture answers `--search=malformed` with non-JSON output.
    let response = state
        .list_project_work(
            &project,
            WorkListQuery {
                search: Some("malformed".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let engram = response
        .sources
        .iter()
        .find(|s| s.source == "engram")
        .unwrap();
    assert_eq!(engram.state, "error");
    assert!(
        engram.message.contains("engram work ls"),
        "{}",
        engram.message
    );
    assert!(response.page.is_none() && response.reader_id.is_none());
    assert_eq!(beads_status(&response).state, "ready");
    let beads = response
        .beads
        .expect("beads snapshot survives an Engram failure");
    assert_eq!(
        beads.total, 0,
        "the search filter applies to the Beads snapshot too"
    );
    // A continuation has no other source to serve and stays a hard error.
    let reader = state
        .work_read_snapshot(&project, None)
        .unwrap()
        .1
        .unwrap()
        .reader_key;
    let error = state
        .list_project_work(
            &project,
            WorkListQuery {
                search: Some("malformed".into()),
                after: Some("cursor".into()),
                reader_id: Some(reader),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_GATEWAY);
}

#[test]
fn beads_read_failures_are_explicit_source_errors_not_empty_pages() {
    let binary = beads_fixture_binary();
    let _binary = with_beads_binary(Some(&binary));
    let (state, project, root) = beads_fixture("failures");
    for (marker, expected) in [
        ("beads-fixture-fail", "database is locked"),
        ("beads-fixture-malformed", "invalid JSON"),
    ] {
        fs::write(root.join(marker), "x").unwrap();
        let response = state
            .list_project_work(&project, WorkListQuery::default())
            .unwrap();
        let status = beads_status(&response);
        assert_eq!(status.state, "error", "{}", status.message);
        assert!(status.message.contains("bd list"), "{}", status.message);
        assert!(status.message.contains(expected), "{}", status.message);
        assert!(response.beads.is_none());
        fs::remove_file(root.join(marker)).unwrap();
    }
}

#[test]
fn beads_detail_reads_show_and_comments_as_inert_data() {
    let binary = beads_fixture_binary();
    let _binary = with_beads_binary(Some(&binary));
    let (state, project, root) = beads_fixture("detail");
    let detail = state
        .read_project_work_beads_detail(&project, "tm-root.1")
        .unwrap();
    assert_eq!(detail.item.id, "tm-root.1");
    assert_eq!(detail.item.source, "beads");
    assert_eq!(detail.parent.as_deref(), Some("tm-root"));
    assert_eq!(detail.description, "Waits for <b>inert</b>");
    assert_eq!(detail.dependencies.len(), 2);
    assert_eq!(detail.item.blocked_by, vec!["tm-free".to_owned()]);
    assert_eq!(detail.comments.len(), 1);
    assert_eq!(detail.comments[0].author.as_deref(), Some("Greg Lapinski"));
    assert_eq!(detail.comment_count, 1);
    let argv = fs::read_to_string(root.join("beads-read-args.txt")).unwrap();
    assert_eq!(
        argv.lines().collect::<Vec<_>>(),
        ["--readonly", "--json", "comments", "tm-root.1"]
    );
    let wire = serde_json::to_value(&detail).unwrap();
    assert_eq!(wire["dependencies"][1]["dependencyType"], "blocks");
    assert_eq!(wire["comments"][0]["createdAt"], "2026-09-12T10:00:00Z");

    let missing = state
        .read_project_work_beads_detail(&project, "tm-missing")
        .unwrap_err();
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert!(missing.message.contains("not found"), "{}", missing.message);
    // The fixture answers `show tm-root` with tm-root.1: a receipt for another
    // issue is a bad gateway, never another issue's drawer.
    let mismatch = state
        .read_project_work_beads_detail(&project, "tm-root")
        .unwrap_err();
    assert_eq!(mismatch.status, StatusCode::BAD_GATEWAY);
    assert!(
        mismatch.message.contains("does not match"),
        "{}",
        mismatch.message
    );
    for bad in ["-x", "tm x"] {
        assert_eq!(
            state
                .read_project_work_beads_detail(&project, bad)
                .unwrap_err()
                .status,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        state
            .read_project_work_beads_detail("missing-project", "tm-root")
            .unwrap_err()
            .status,
        StatusCode::NOT_FOUND
    );
    // A comment for another issue is a receipt for another issue: bad
    // gateway, never another issue's thread in this drawer.
    fs::write(root.join("beads-fixture-stray-comment"), "x").unwrap();
    let stray = state
        .read_project_work_beads_detail(&project, "tm-root.1")
        .unwrap_err();
    assert_eq!(stray.status, StatusCode::BAD_GATEWAY);
    assert!(
        stray.message.contains("bd comments: receipt for tm-other"),
        "{}",
        stray.message
    );
    fs::remove_file(root.join("beads-fixture-stray-comment")).unwrap();
    // The receipt guards directly: an out-of-range priority or a malformed id
    // is a bad gateway, and the comment count never understates the comments
    // actually loaded.
    let show = |priority: u64, id: &str| serde_json::json!([{"id":id,"title":"T","status":"open","priority":priority,"issue_type":"task","dependency_count":0,"comment_count":0}]);
    let comment = |issue_id: &str| serde_json::json!([{"id":"c-9","issue_id":issue_id,"author":null,"text":"t","created_at":"2026-09-12T10:00:00Z"}]);
    for (priority, id) in [(9, "tm-x"), (2, "-x")] {
        let invalid = normalize_beads_detail(show(priority, id), comment(id), id).unwrap_err();
        assert_eq!(invalid.status, StatusCode::BAD_GATEWAY);
        assert!(
            invalid.message.contains("invalid issue id or priority"),
            "{}",
            invalid.message
        );
    }
    let reconciled = normalize_beads_detail(show(2, "tm-x"), comment("tm-x"), "tm-x").unwrap();
    assert_eq!(
        (reconciled.comment_count, reconciled.comments.len()),
        (1, 1)
    );
    // The drawer applies the list's rule: readiness only from a complete
    // receipt. `show`'s dependency_count counts every relation type (bd
    // 1.2.2), so it is reconciled against all parsed records.
    let closed_blocker = serde_json::json!({"id":"tm-done","title":"D","status":"closed","priority":2,"issue_type":"task","dependency_type":"blocks"});
    let show_with = |count: u64, dependencies: Value| serde_json::json!([{"id":"tm-x","title":"T","status":"open","priority":2,"issue_type":"task","dependency_count":count,"dependencies":dependencies,"comment_count":0}]);
    let complete = normalize_beads_detail(
        show_with(1, serde_json::json!([closed_blocker])),
        comment("tm-x"),
        "tm-x",
    )
    .unwrap();
    assert_eq!(
        (
            complete.item.availability.as_str(),
            complete.dependencies_unread
        ),
        ("ready", 0)
    );
    // A sparse relation record (an external reference) is disclosed, never a
    // failed drawer; and it makes readiness unknown.
    let sparse = normalize_beads_detail(
        show_with(
            2,
            serde_json::json!([closed_blocker, {"id":"external:other:tm-1","dependency_type":"blocks"}]),
        ),
        comment("tm-x"),
        "tm-x",
    )
    .unwrap();
    assert_eq!(
        (
            sparse.dependencies.len(),
            sparse.dependencies_unread,
            sparse.item.availability.as_str()
        ),
        (1, 1, "unknown")
    );
    // Fewer records than declared is a partial receipt too.
    let partial = normalize_beads_detail(
        show_with(2, serde_json::json!([closed_blocker])),
        comment("tm-x"),
        "tm-x",
    )
    .unwrap();
    assert_eq!(
        (
            partial.item.availability.as_str(),
            partial.dependencies_unread
        ),
        ("unknown", 1),
        "a declared record the receipt does not carry is unread too"
    );
    // A receipt without the count is a changed contract, like the list's.
    assert!(
        normalize_beads_detail(
            serde_json::json!([{"id":"tm-x","title":"T","status":"open","priority":2,"issue_type":"task","comment_count":0}]),
            comment("tm-x"),
            "tm-x"
        )
        .is_err()
    );
}

#[test]
fn beads_detail_route_refuses_projects_without_a_readable_store() {
    let binary = beads_fixture_binary();
    let state = test_app_state();
    let root = state
        .persistence_path
        .parent()
        .unwrap()
        .join("beads-detail-absent");
    fs::create_dir_all(&root).unwrap();
    let project = create_test_project(&state, &root, "No beads");
    // No `.beads` directory: a conflict, nothing launched.
    {
        let _binary = with_beads_binary(Some(&binary));
        let error = state
            .read_project_work_beads_detail(&project, "tm-root.1")
            .unwrap_err();
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(
            error.message.contains("No .beads directory"),
            "{}",
            error.message
        );
    }
    // A store but no usable binary: a conflict naming the binary, nothing
    // launched.
    fs::create_dir_all(root.join(".beads")).unwrap();
    let _binary = with_beads_binary(Some(FsPath::new(if cfg!(windows) {
        "C:/definitely/missing/bd.exe"
    } else {
        "/definitely/missing/bd"
    })));
    let error = state
        .read_project_work_beads_detail(&project, "tm-root.1")
        .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error.message.contains("Beads binary unavailable"),
        "{}",
        error.message
    );
    assert!(!root.join("beads-read-args.txt").exists(), "no read ran");
}

#[test]
fn dropping_the_handler_future_sets_the_abandon_flag() {
    // The guard is what a dropped handler future leaves behind: the flag the
    // blocking worker checks. Its wiring into both handlers is one line each;
    // what the worker does with the flag is covered by the seam tests, which
    // never depend on thread timing.
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let guard = WorkReadAbandonGuard(cancelled.clone());
    assert!(!cancelled.load(std::sync::atomic::Ordering::Relaxed));
    drop(guard);
    assert!(cancelled.load(std::sync::atomic::Ordering::Relaxed));
}

#[tokio::test]
async fn beads_routes_use_their_own_admission_and_validate_ids() {
    let binary = beads_fixture_binary();
    let _binary = with_beads_binary(Some(&binary));
    let (state, project, _) = beads_fixture("route");
    let limiter = Arc::new(tokio::sync::Semaphore::new(1));
    // The Engram limiter is deliberately exhausted: a Beads read that wrongly
    // waited on it would fail with 429 instead of completing.
    let engram_limiter = Arc::new(tokio::sync::Semaphore::new(1));
    let _engram_permit = engram_limiter.clone().acquire_owned().await.unwrap();
    // The routes take their Beads budget from the router, so three fixture
    // launches never race the production deadline under suite load.
    let app = app_router(state)
        .layer(axum::Extension(WorkReadLimiter(engram_limiter.clone())))
        .layer(axum::Extension(BeadsReadLimiter(limiter.clone())))
        .layer(axum::Extension(BeadsReadOptions::for_tests()));
    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .uri(format!("/api/projects/{project}/work/beads/tm-root.1"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["item"]["shortRef"], "tm-root.1");
    assert_eq!(response["item"]["availability"], "blocked");
    assert_eq!(limiter.available_permits(), 1);
    let (status, _): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .uri(format!("/api/projects/{project}/work/beads/-bad"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .uri(format!("/api/projects/{project}/work"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["beads"]["total"], 3);
    assert_eq!(response["beads"]["items"][0]["source"], "beads");
    assert_eq!(limiter.available_permits(), 1);
    assert_eq!(engram_limiter.available_permits(), 0);
}

#[test]
fn beads_normalization_maps_statuses_and_decides_prerequisites_before_the_cap() {
    let rows = || -> Vec<BeadsListRow> {
        serde_json::from_value(serde_json::json!([
            {"id":"tm-a","title":"Blocked by status","status":"blocked","priority":1,"issue_type":"task","dependency_count":0},
            {"id":"tm-b","title":"Deferred","status":"deferred","priority":2,"issue_type":"task","dependency_count":0},
            {"id":"tm-c","title":"Waits for d","status":"open","priority":3,"issue_type":"task","dependency_count":1},
            {"id":"tm-d","title":"Beyond the cap","status":"open","priority":4,"issue_type":"task","dependency_count":0},
            {"id":"tm-e","title":"Unknown status","status":"hooked","priority":0,"issue_type":"task","dependency_count":0},
            {"id":"tm-f.2","title":"Dotted id, parent cleared","status":"open","priority":2,"issue_type":"task","dependency_count":2},
            {"id":"tm-flat","title":"Flat id, reparented under tm-e","status":"open","priority":2,"issue_type":"task","dependency_count":1},
            {"id":"tm bad","title":"Malformed id","status":"open","priority":2,"issue_type":"task","dependency_count":0},
            {"id":"tm-p9","title":"Priority out of range","status":"open","priority":9,"issue_type":"task","dependency_count":0}
        ]))
        .unwrap()
    };
    let edges = || -> Vec<BeadsDependencyEdge> {
        serde_json::from_value(serde_json::json!([
            {"issue_id":"tm-c","depends_on_id":"tm-d","type":"blocks"},
            {"issue_id":"tm-f.2","depends_on_id":"tm-closed","type":"blocks"},
            {"issue_id":"tm-f.2","depends_on_id":"tm-gate","type":"blocks"},
            {"issue_id":"tm-flat","depends_on_id":"tm-e","type":"parent-child"}
        ]))
        .unwrap()
    };
    // `show` evidence: tm-closed is closed, tm-gate is open but hidden from
    // the default list; an id bd does not know is simply absent.
    let statuses = HashMap::from([
        ("tm-closed".to_owned(), "closed".to_owned()),
        ("tm-gate".to_owned(), "open".to_owned()),
    ]);
    let none = BeadsSnapshotCoverage::default();
    let capped = normalize_beads_work_page(
        rows(),
        edges(),
        &statuses,
        &none,
        &WorkListQuery::default(),
        3,
    )
    .unwrap();
    assert_eq!(
        capped.total, 7,
        "two malformed rows are skipped, not counted"
    );
    assert_eq!(capped.items.len(), 3);
    assert!(capped.more);
    let hint = capped.hint.clone().unwrap_or_default();
    assert!(
        hint.contains("2 Beads rows with malformed") && hint.contains("4 more"),
        "{hint}"
    );
    let availability = |page: &WorkPage, id: &str| {
        page.items
            .iter()
            .find(|i| i.id == id)
            .map(|i| i.availability.clone())
    };
    assert_eq!(availability(&capped, "tm-a").as_deref(), Some("blocked"));
    assert_eq!(availability(&capped, "tm-b").as_deref(), Some("deferred"));
    // tm-d is cut by the cap but is still an open, unsatisfied blocker.
    assert_eq!(availability(&capped, "tm-c").as_deref(), Some("blocked"));
    let waits = capped.items.iter().find(|i| i.id == "tm-c").unwrap();
    assert_eq!(waits.prerequisites.len(), 1);
    assert_eq!(waits.prerequisites[0].id, "tm-d");
    assert!(!waits.prerequisites[0].satisfied);
    assert!(availability(&capped, "tm-d").is_none());

    let full = normalize_beads_work_page(
        rows(),
        edges(),
        &statuses,
        &none,
        &WorkListQuery::default(),
        100,
    )
    .unwrap();
    assert!(!full.more);
    assert!(
        full.hint
            .as_deref()
            .unwrap_or_default()
            .contains("malformed")
    );
    assert_eq!(full.total, 7);
    // Coverage gaps are disclosed and never rendered as readiness.
    let partial = BeadsSnapshotCoverage {
        dependency_rows_unread: HashSet::from(["tm-e".to_owned(), "tm-a".to_owned()]),
        blockers_unread: 3,
        rows_malformed: 1,
    };
    let disclosed = normalize_beads_work_page(
        rows(),
        edges(),
        &statuses,
        &partial,
        &WorkListQuery::default(),
        100,
    )
    .unwrap();
    let hint = disclosed.hint.clone().unwrap_or_default();
    assert!(
        hint.contains("dependencies of 2 rows were not fully read")
            && hint.contains("3 blocker statuses")
            && hint.contains("3 Beads rows with malformed"),
        "{hint}"
    );
    assert_eq!(
        availability(&disclosed, "tm-a").as_deref(),
        Some("blocked"),
        "stated statuses stay"
    );
    let unread: Vec<BeadsListRow> = serde_json::from_value(serde_json::json!([
        {"id":"tm-u","title":"Edges not read","status":"open","priority":2,"issue_type":"task","dependency_count":4}
    ]))
    .unwrap();
    let unknown = normalize_beads_work_page(
        unread,
        Vec::new(),
        &statuses,
        &BeadsSnapshotCoverage {
            dependency_rows_unread: HashSet::from(["tm-u".to_owned()]),
            blockers_unread: 0,
            rows_malformed: 0,
        },
        &WorkListQuery::default(),
        100,
    )
    .unwrap();
    assert_eq!(availability(&unknown, "tm-u").as_deref(), Some("unknown"));
    // Unknown statuses are disclosed verbatim, never guessed as ready.
    assert_eq!(availability(&full, "tm-e").as_deref(), Some("hooked"));
    // Satisfied only on positive evidence: the closed blocker is satisfied,
    // the hidden open gate is not, so the row stays blocked.
    let dotted = full.items.iter().find(|i| i.id == "tm-f.2").unwrap();
    assert_eq!(
        dotted.parent_id, None,
        "no parent-child edge means no parent, whatever the id says"
    );
    assert_eq!(dotted.availability, "blocked");
    assert_eq!(dotted.blocked_by, vec!["tm-gate".to_owned()]);
    assert!(
        dotted
            .prerequisites
            .iter()
            .any(|p| p.id == "tm-closed" && p.satisfied)
    );
    assert!(
        dotted
            .prerequisites
            .iter()
            .any(|p| p.id == "tm-gate" && !p.satisfied)
    );
    let flat = full.items.iter().find(|i| i.id == "tm-flat").unwrap();
    assert_eq!(
        flat.parent_id.as_deref(),
        Some("tm-e"),
        "parent from the parent-child edge"
    );
    assert!(
        full.items
            .iter()
            .all(|i| i.id != "tm bad" && i.id != "tm-p9")
    );

    // A row's inline records: only records naming the row count, each edge
    // once; the row is complete when they account for what it declares.
    let row: BeadsListRow = serde_json::from_value(serde_json::json!({
        "id":"tm-a","title":"A","status":"open","priority":2,"issue_type":"task","dependency_count":1,"parent":"tm-p",
        "dependencies":[
            {"issue_id":"tm-a","depends_on_id":"tm-x","type":"blocks"},
            {"issue_id":"tm-a","depends_on_id":"tm-x","type":"blocks"},
            {"issue_id":"tm-a","depends_on_id":"tm-p","type":"parent-child"},
            {"issue_id":"tm-other","depends_on_id":"tm-y","type":"blocks"}
        ]
    }))
    .unwrap();
    let (edges, malformed) = beads_row_edges(&row);
    assert!(!malformed);
    assert_eq!(
        edges
            .iter()
            .map(|e| (e.depends_on_id.as_str(), e.kind.as_str()))
            .collect::<Vec<_>>(),
        [("tm-x", "blocks"), ("tm-p", "parent-child")]
    );
    assert!(beads_row_edges_complete(&row, &edges));
    // Fewer blocks than declared, or a parent without its record, is partial.
    assert!(!beads_row_edges_complete(&row, &edges[1..]));
    assert!(!beads_row_edges_complete(&row, &edges[..1]));
    // A record that does not parse makes the row's edges unread, not the row
    // malformed.
    let malformed: BeadsListRow = serde_json::from_value(serde_json::json!({
        "id":"tm-b","title":"B","status":"open","priority":2,"issue_type":"task","dependency_count":0,
        "dependencies":[{"issue_id":"tm-b","depends_on_id":"tm-x"}]
    }))
    .unwrap();
    let (kept, flagged) = beads_row_edges(&malformed);
    assert!(flagged && kept.is_empty());
    // The records that did parse are kept beside the flag: a blocker read is
    // evidence of blocking even when another record is unreadable.
    let mixed: BeadsListRow = serde_json::from_value(serde_json::json!({
        "id":"tm-c","title":"C","status":"open","priority":2,"issue_type":"task","dependency_count":1,
        "dependencies":[{"issue_id":"tm-c","depends_on_id":"tm-x","type":"blocks"},{"issue_id":"tm-c","depends_on_id":"tm-y"}]
    }))
    .unwrap();
    let (kept, flagged) = beads_row_edges(&mixed);
    assert!(flagged);
    assert_eq!(
        kept.iter()
            .map(|e| (e.depends_on_id.as_str(), e.kind.as_str()))
            .collect::<Vec<_>>(),
        [("tm-x", "blocks")]
    );
    // A bare single-issue `show` object is one row (bd 1.2.2 prints an array
    // even for one id; both are accepted).
    let one: Vec<BeadsStatusRow> = beads_rows_from_value(
        serde_json::json!({"id":"tm-x","status":"closed","title":"T"}),
        "bd show",
    )
    .unwrap();
    assert_eq!(
        (one[0].id.as_str(), one[0].status.as_str()),
        ("tm-x", "closed")
    );

    assert!(
        beads_rows_from_value::<BeadsCommentView>(Value::Null, "bd comments")
            .unwrap()
            .is_empty()
    );
    // Unknown object shapes are invalid receipts, never empty successes.
    for shape in [
        serde_json::json!({}),
        serde_json::json!({"error": "boom"}),
        serde_json::json!({"result": []}),
    ] {
        let error = beads_rows_from_value::<BeadsListRow>(shape, "bd list")
            .map(|rows| rows.len())
            .unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        assert!(
            error.message.contains("unrecognized object shape"),
            "{}",
            error.message
        );
    }
    // dependency_count is evidence, not a default: a row without it is a
    // changed contract.
    assert!(
        serde_json::from_value::<Vec<BeadsListRow>>(serde_json::json!([
            {"id":"tm-x","title":"T","status":"open","priority":1,"issue_type":"task"}
        ]))
        .is_err()
    );
    // One mistyped row is skipped and counted; a receipt where no row parses
    // is a changed contract.
    let good = serde_json::json!({"id":"tm-ok","title":"T","status":"open","priority":1,"issue_type":"task","dependency_count":0});
    let bad = serde_json::json!({"id":"tm-bad","title":null,"status":"open","priority":900,"issue_type":"task","dependency_count":0});
    let (parsed, malformed) = parse_beads_list_rows(vec![good.clone(), bad.clone()]).unwrap();
    assert_eq!((parsed.len(), malformed), (1, 1));
    assert!(parse_beads_list_rows(vec![bad]).is_err());
    assert_eq!(parse_beads_list_rows(Vec::new()).unwrap().1, 0);
    // The availability axis, directly.
    assert_eq!(beads_availability("in_progress", true), "active");
    assert_eq!(beads_availability("open", true), "blocked");
    assert_eq!(beads_availability("", true), "blocked");
    assert_eq!(beads_availability("", false), "ready");
    assert_eq!(beads_availability("closed", true), "closed");
    let ids = (0..250).map(|i| format!("tm-{i}")).collect::<Vec<_>>();
    assert_eq!(
        beads_id_batches(&ids, 100)
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        [100, 100, 50]
    );
    assert!(beads_id_batches(&[], 100).is_empty());
}

#[test]
fn beads_blocker_status_lookup_survives_unknown_ids() {
    let binary = beads_fixture_binary();
    let _binary = with_beads_binary(Some(&binary));
    let (_state, _project, root) = beads_fixture("dangling");
    let target = BeadsReadTarget {
        binary_path: binary.clone(),
        project_root: root.clone(),
    };
    // Every call gets its own budget that fixture launches cannot exhaust:
    // scheduling under parallel suite load must never decide these outcomes.
    let budget = BeadsReadOptions::for_tests().timeout;
    let fresh = move || std::time::Instant::now() + budget;
    let live = std::sync::atomic::AtomicBool::new(false);
    // A mixed batch succeeds and omits the unknown id (bd 1.2.2): one launch,
    // the known blocker's evidence kept, the omitted id counted as unread.
    let mixed = read_beads_blocker_statuses(
        &target,
        &["tm-closed".to_owned(), "tm-missing".to_owned()],
        fresh(),
        BEADS_LAUNCH_RESERVE,
        &live,
    )
    .unwrap();
    assert_eq!(
        mixed.statuses.get("tm-closed").map(String::as_str),
        Some("closed")
    );
    assert!(!mixed.statuses.contains_key("tm-missing"));
    assert_eq!(mixed.unread, 1);
    // A batch bd rejects outright (only unknown ids) is disclosed as unread
    // evidence, never retried per id and never a failed snapshot.
    let rejected = read_beads_blocker_statuses(
        &target,
        &["tm-missing".to_owned(), "tm-gone".to_owned()],
        fresh(),
        BEADS_LAUNCH_RESERVE,
        &live,
    )
    .unwrap();
    assert!(rejected.statuses.is_empty());
    assert_eq!(rejected.unread, 2);
    let log = fs::read_to_string(root.join("beads-read-args-log.txt")).unwrap();
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        [
            "--readonly --json show tm-closed tm-missing",
            "--readonly --json show tm-missing tm-gone",
        ]
    );
    // Batches beyond the per-read bound are counted, not launched.
    let many = (0..(BEADS_STATUS_BATCH * (BEADS_MAX_BATCHES_PER_READ + 2)))
        .map(|i| format!("tm-x{i}"))
        .collect::<Vec<_>>();
    // Eight fixture launches within one budget.
    let bounded =
        read_beads_blocker_statuses(&target, &many, fresh(), BEADS_LAUNCH_RESERVE, &live).unwrap();
    assert_eq!(bounded.unread, many.len());
    let launches = fs::read_to_string(root.join("beads-read-args-log.txt"))
        .unwrap()
        .lines()
        .count();
    assert_eq!(launches, 2 + BEADS_MAX_BATCHES_PER_READ);
    // When the deadline no longer leaves room for a launch, the batch is
    // disclosed as unread instead of being launched into the deadline.
    let reserved = read_beads_blocker_statuses(
        &target,
        &["tm-closed".to_owned()],
        fresh(),
        budget * 2,
        &live,
    )
    .unwrap();
    assert_eq!(reserved.unread, 1);
    assert!(reserved.statuses.is_empty());
    assert_eq!(
        fs::read_to_string(root.join("beads-read-args-log.txt"))
            .unwrap()
            .lines()
            .count(),
        launches,
        "no launch while the reserve does not fit"
    );
    // An exhausted deadline is a failure, never "no evidence" - and nothing
    // is launched for it, so the outcome never depends on how fast the
    // fixture happens to exit.
    let exhausted = read_beads_blocker_statuses(
        &target,
        &["tm-missing".to_owned()],
        std::time::Instant::now(),
        Duration::ZERO,
        &live,
    )
    .unwrap_err();
    assert!(
        exhausted
            .message
            .contains("deadline exhausted before launch"),
        "{}",
        exhausted.message
    );
    assert_eq!(
        fs::read_to_string(root.join("beads-read-args-log.txt"))
            .unwrap()
            .lines()
            .count(),
        launches,
        "no launch past the deadline"
    );
    // So is a receipt that is not JSON: only bd's refusal is absorbed.
    fs::write(root.join("beads-fixture-malformed-show"), "x").unwrap();
    let malformed = read_beads_blocker_statuses(
        &target,
        &["tm-closed".to_owned()],
        fresh(),
        BEADS_LAUNCH_RESERVE,
        &live,
    )
    .unwrap_err();
    assert!(
        malformed.message.contains("invalid JSON"),
        "{}",
        malformed.message
    );
    fs::remove_file(root.join("beads-fixture-malformed-show")).unwrap();
    // And so is a store failure: only bd's unknown-id refusal is evidence;
    // "database is locked" must never become "unknown or deleted".
    fs::write(root.join("beads-fixture-fail-show"), "x").unwrap();
    let locked = read_beads_blocker_statuses(
        &target,
        &["tm-closed".to_owned()],
        fresh(),
        BEADS_LAUNCH_RESERVE,
        &live,
    )
    .unwrap_err();
    assert_eq!(locked.status, StatusCode::BAD_GATEWAY);
    assert!(
        locked.message.contains("database is locked"),
        "{}",
        locked.message
    );
    fs::remove_file(root.join("beads-fixture-fail-show")).unwrap();
    // The same refusal as a JSON error on stdout beside an unrelated stderr
    // notice (the shape bd 1.2.2 uses elsewhere) is classified on both
    // streams and still absorbed.
    fs::write(root.join("beads-fixture-unknown-show-json"), "x").unwrap();
    let noisy = read_beads_blocker_statuses(
        &target,
        &["tm-missing".to_owned()],
        fresh(),
        BEADS_LAUNCH_RESERVE,
        &live,
    )
    .unwrap();
    assert_eq!((noisy.unread, noisy.statuses.len()), (1, 0));
    fs::remove_file(root.join("beads-fixture-unknown-show-json")).unwrap();
    // A refusal that names an unknown id AND fails on the store is a failed
    // read: the unknown-id line must never absorb the store failure.
    fs::write(root.join("beads-fixture-mixed-refusal-show"), "x").unwrap();
    let mixed_refusal = read_beads_blocker_statuses(
        &target,
        &["tm-missing".to_owned(), "tm-closed".to_owned()],
        fresh(),
        BEADS_LAUNCH_RESERVE,
        &live,
    )
    .unwrap_err();
    assert_eq!(mixed_refusal.status, StatusCode::BAD_GATEWAY);
    assert!(
        mixed_refusal.message.contains("database is locked"),
        "{}",
        mixed_refusal.message
    );
    fs::remove_file(root.join("beads-fixture-mixed-refusal-show")).unwrap();
    // The classifier itself: every requested id named, notices ignored, any
    // other error line or an unnamed id is not a refusal of unknown ids.
    let ids = ["tm-a".to_owned(), "tm-b".to_owned()];
    assert!(beads_refusal_is_unknown_ids(
        "warning: a newer bd is available\nError fetching tm-a: no issue found matching \"tm-a\"\nError fetching tm-b: no issue found matching \"tm-b\"",
        &ids
    ));
    assert!(beads_refusal_is_unknown_ids(
        "{\"error\":\"resolving tm-a: no issue found matching \\\"tm-a\\\"\",\"schema_version\":1}",
        &ids[..1]
    ));
    assert!(!beads_refusal_is_unknown_ids(
        "Error fetching tm-a: no issue found matching \"tm-a\"",
        &ids
    ));
    assert!(!beads_refusal_is_unknown_ids(
        "Error fetching tm-a: no issue found matching \"tm-a\"\nError: database is locked",
        &ids[..1]
    ));
    assert!(!beads_refusal_is_unknown_ids(
        "Error: database is locked",
        &ids
    ));
    // A notice that reports a store problem is an error line, whatever its
    // prefix; only a benign notice is skipped.
    assert!(!beads_refusal_is_unknown_ids(
        "warning: database locked, retrying\nError fetching tm-a: no issue found matching \"tm-a\"\nError fetching tm-b: no issue found matching \"tm-b\"",
        &ids
    ));
    assert!(!beads_refusal_is_unknown_ids(
        "info: Dolt error: connection failed\nError fetching tm-a: no issue found matching \"tm-a\"",
        &ids[..1]
    ));
    assert!(!beads_refusal_is_unknown_ids("", &[]));
    // An abandoned request launches nothing more: every remaining batch is
    // disclosed as unread.
    let launches_before = fs::read_to_string(root.join("beads-read-args-log.txt"))
        .unwrap()
        .lines()
        .count();
    let abandoned = std::sync::atomic::AtomicBool::new(true);
    let skipped = read_beads_blocker_statuses(
        &target,
        &["tm-closed".to_owned()],
        fresh(),
        BEADS_LAUNCH_RESERVE,
        &abandoned,
    )
    .unwrap();
    assert_eq!((skipped.unread, skipped.statuses.len()), (1, 0));
    assert_eq!(
        fs::read_to_string(root.join("beads-read-args-log.txt"))
            .unwrap()
            .lines()
            .count(),
        launches_before,
        "no launch for an abandoned request"
    );
}

#[test]
fn beads_unknown_id_refusal_rejects_same_line_store_failures() {
    let ids = ["tm-a".to_owned()];
    for diagnostic in [
        "Error fetching tm-a: no issue found matching \"tm-a\" (database is locked)",
        "database corrupt: Error fetching tm-a: no issue found matching \"tm-a\"",
        "{\"error\":\"resolving tm-a: no issue found matching \\\"tm-a\\\" (database is locked)\",\"schema_version\":1}",
        "{\"error\":\"resolving tm-a: no issue found matching \\\"tm-a\\\"\",\"other_error\":\"database locked\"}",
        "{\"error\":\"no issues found matching the provided IDs\",\"schema_version\":1}",
    ] {
        assert!(
            !beads_refusal_is_unknown_ids(diagnostic, &ids),
            "{diagnostic}"
        );
    }
    // Failure words inside the requested identifier are not store failures.
    assert!(beads_refusal_is_unknown_ids(
        "Error fetching tm-lock: no issue found matching \"tm-lock\"",
        &["tm-lock".to_owned()],
    ));
}

#[test]
fn beads_same_line_store_failure_is_bad_gateway_for_detail_and_blockers() {
    let binary = beads_fixture_binary();
    let _binary = with_beads_binary(Some(&binary));
    let (state, project, root) = beads_fixture("same-line-refusal");
    fs::write(root.join("beads-fixture-same-line-refusal-show"), "x").unwrap();
    let detail = state
        .read_project_work_beads_detail(&project, "tm-missing")
        .unwrap_err();
    assert_eq!(detail.status, StatusCode::BAD_GATEWAY);
    assert!(detail.message.contains("database is locked"));
    let target = BeadsReadTarget {
        binary_path: binary,
        project_root: root.clone(),
    };
    let blockers = read_beads_blocker_statuses(
        &target,
        &["tm-missing".to_owned()],
        std::time::Instant::now() + BeadsReadOptions::for_tests().timeout,
        BEADS_LAUNCH_RESERVE,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap_err();
    assert_eq!(blockers.status, StatusCode::BAD_GATEWAY);
    assert!(blockers.message.contains("database is locked"));

    // Exact two-stream shape observed with bd --readonly --json show on
    // 2026-09-14: named stderr refusal and a pretty-printed aggregate JSON.
    fs::remove_file(root.join("beads-fixture-same-line-refusal-show")).unwrap();
    fs::write(root.join("beads-fixture-aggregate-refusal-show"), "x").unwrap();
    let missing = state
        .read_project_work_beads_detail(&project, "tm-missing")
        .unwrap_err();
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    let unknown = read_beads_blocker_statuses(
        &target,
        &["tm-missing".to_owned()],
        std::time::Instant::now() + BeadsReadOptions::for_tests().timeout,
        BEADS_LAUNCH_RESERVE,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(unknown.unread, 1);
    assert!(unknown.statuses.is_empty());
}

#[test]
fn beads_reads_stop_when_the_request_was_abandoned() {
    let binary = beads_fixture_binary();
    let _binary = with_beads_binary(Some(&binary));
    let (state, project, root) = beads_fixture("abandoned");
    // The flag is set before the permit is used: no bd process starts and the
    // read ends with a status nobody receives (not a source condition).
    let abandoned = std::sync::atomic::AtomicBool::new(true);
    let error = state
        .list_project_work_with_options(
            &project,
            WorkListQuery::default(),
            || Ok(()),
            || Ok(()),
            BeadsReadOptions::for_tests(),
            &abandoned,
        )
        .unwrap_err();
    assert_eq!(error.status.as_u16(), 499);
    assert!(
        !root.join("beads-read-args.txt").exists(),
        "no bd launched for an abandoned request"
    );
    // The detail read re-checks right after admission: a drawer closed while
    // the read waited for its permit launches neither command.
    let gone_during_admission = std::sync::atomic::AtomicBool::new(false);
    let error = state
        .read_project_work_beads_detail_with_options(
            &project,
            "tm-root.1",
            || {
                gone_during_admission.store(true, std::sync::atomic::Ordering::Relaxed);
                Ok::<(), ApiError>(())
            },
            BeadsReadOptions::for_tests(),
            &gone_during_admission,
        )
        .unwrap_err();
    assert_eq!(error.status.as_u16(), 499);
    assert!(
        !root.join("beads-read-args.txt").exists(),
        "no bd launched for a drawer closed during admission"
    );
    // A continuation never detects Beads: its status says the snapshot came
    // with the first page, and no binary is resolved for it.
    let project_record = state
        .inner
        .lock()
        .unwrap()
        .find_project(&project)
        .unwrap()
        .clone();
    let (status, target) = beads_source_status(
        &project_record,
        &WorkListQuery {
            after: Some("cursor".into()),
            ..Default::default()
        },
    );
    assert_eq!(status.state, "skipped");
    assert!(target.is_none());
    let (status, target) = beads_source_status(&project_record, &WorkListQuery::default());
    assert_eq!(status.state, "ready");
    assert!(target.is_some());
}

#[test]
fn beads_snapshot_degrades_to_disclosed_coverage_when_time_runs_out() {
    let binary = beads_fixture_binary();
    let _binary = with_beads_binary(Some(&binary));
    let (_state, _project, root) = beads_fixture("reserve");
    let target = BeadsReadTarget {
        binary_path: binary.clone(),
        project_root: root.clone(),
    };
    // The list itself always runs and carries every row's edges; with a
    // reserve no remaining time can satisfy, every status batch is disclosed
    // instead: the closed blocker stays unsatisfied, never a failed snapshot.
    let budget = BeadsReadOptions::for_tests().timeout;
    let page = read_beads_work_page_until(
        &target,
        &WorkListQuery::default(),
        std::time::Instant::now() + budget,
        budget * 2,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(page.total, 3);
    let child = page.items.iter().find(|i| i.id == "tm-root.1").unwrap();
    assert_eq!(
        child.parent_id.as_deref(),
        Some("tm-root"),
        "edges come from the list receipt"
    );
    assert_eq!(child.availability, "blocked");
    assert!(
        child.prerequisites.len() == 2 && child.prerequisites.iter().all(|p| !p.satisfied),
        "no status evidence: nothing is satisfied ({:?})",
        child.prerequisites
    );
    let hint = page.hint.clone().unwrap_or_default();
    assert!(
        hint.contains("1 blocker status could not be read"),
        "{hint}"
    );
    assert_eq!(
        fs::read_to_string(root.join("beads-read-args-log.txt"))
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["--readonly --json list --limit 0"]
    );
}

#[test]
fn engram_admission_failures_are_isolated_only_when_they_are_source_conditions() {
    let binary = beads_fixture_binary();
    let _binary = with_beads_binary(Some(&binary));
    let (state, project, _, root) = super::work_visualizer::fixture();
    super::work_visualizer::install_store(&state, &project, &root);
    fs::create_dir_all(root.join(".beads")).unwrap();
    // Engram admission exhausted: an explicit engram error, Beads still read.
    let busy = state
        .list_project_work_with_admission(
            &project,
            WorkListQuery::default(),
            || -> Result<(), ApiError> {
                Err(ApiError::from_status(
                    StatusCode::TOO_MANY_REQUESTS,
                    "Work reads busy; retry shortly",
                ))
            },
            || Ok(()),
        )
        .unwrap();
    let engram = busy.sources.iter().find(|s| s.source == "engram").unwrap();
    assert_eq!(engram.state, "error");
    assert!(engram.message.contains("busy"), "{}", engram.message);
    assert!(busy.page.is_none());
    assert_eq!(busy.beads.expect("beads snapshot").total, 3);
    // A closed limiter is not a source condition: the request fails.
    let internal = state
        .list_project_work_with_admission(
            &project,
            WorkListQuery::default(),
            || -> Result<(), ApiError> { Err(ApiError::internal("Work read limiter closed")) },
            || -> Result<(), ApiError> {
                panic!("Beads must not be read after an internal failure")
            },
        )
        .unwrap_err();
    assert_eq!(internal.status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn beads_rows_without_dependency_records_are_unknown_not_ready() {
    let binary = beads_fixture_binary();
    let _binary = with_beads_binary(Some(&binary));
    let (state, project, root) = beads_fixture("no-edges");
    // The list row declares two blockers and a parent but carries no records:
    // its edges are unread, its readiness a guess.
    fs::write(root.join("beads-fixture-no-edges"), "x").unwrap();
    let page = state
        .list_project_work(&project, WorkListQuery::default())
        .unwrap()
        .beads
        .expect("beads snapshot");
    let child = page.items.iter().find(|i| i.id == "tm-root.1").unwrap();
    assert_eq!(child.availability, "unknown");
    assert!(child.prerequisites.is_empty() && child.parent_id.is_none());
    assert!(
        page.hint
            .as_deref()
            .unwrap_or_default()
            .contains("dependencies of 1 row were not fully read"),
        "{:?}",
        page.hint
    );
    fs::remove_file(root.join("beads-fixture-no-edges")).unwrap();
    // A partial receipt (only the parent-child record survives) is unread the
    // same way: the parent that was read shows, readiness is not guessed.
    fs::write(root.join("beads-fixture-partial-edges"), "x").unwrap();
    let page = state
        .list_project_work(&project, WorkListQuery::default())
        .unwrap()
        .beads
        .expect("beads snapshot");
    let child = page.items.iter().find(|i| i.id == "tm-root.1").unwrap();
    assert_eq!(child.availability, "unknown");
    assert_eq!(child.parent_id.as_deref(), Some("tm-root"));
    assert!(child.prerequisites.is_empty());
    assert!(
        page.hint
            .as_deref()
            .unwrap_or_default()
            .contains("dependencies of 1 row were not fully read"),
        "{:?}",
        page.hint
    );
    fs::remove_file(root.join("beads-fixture-partial-edges")).unwrap();
    // Neither snapshot learned a blocker id, so no status batch ran.
    assert_eq!(
        fs::read_to_string(root.join("beads-read-args-log.txt"))
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "--readonly --json list --limit 0",
            "--readonly --json list --limit 0",
        ]
    );
    // One record that does not parse keeps the records that did: the row is
    // still blocked by the blocker that was read, with its parent; only its
    // readiness is off the table, and the row is disclosed.
    fs::write(root.join("beads-fixture-malformed-record"), "x").unwrap();
    let page = state
        .list_project_work(&project, WorkListQuery::default())
        .unwrap()
        .beads
        .expect("beads snapshot");
    let child = page.items.iter().find(|i| i.id == "tm-root.1").unwrap();
    assert_eq!(child.availability, "blocked");
    assert_eq!(child.parent_id.as_deref(), Some("tm-root"));
    assert_eq!(child.prerequisites.len(), 2);
    assert!(
        page.hint
            .as_deref()
            .unwrap_or_default()
            .contains("dependencies of 1 row were not fully read"),
        "{:?}",
        page.hint
    );
    fs::remove_file(root.join("beads-fixture-malformed-record")).unwrap();
}

#[test]
fn beads_reads_strip_store_selection_from_the_child_environment() {
    // Asserted on the command itself, never by mutating this process's
    // environment: an `env_remove` is recorded as an explicit `None`, so the
    // child inherits no BEADS_DIR/BEADS_DB whatever the server holds.
    let target = BeadsReadTarget {
        binary_path: beads_fixture_binary(),
        project_root: std::env::temp_dir(),
    };
    let command = beads_read_command(&target, &["list".to_owned()]);
    let removed = command
        .get_envs()
        .filter(|(_, value)| value.is_none())
        .map(|(key, _)| key.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    for variable in BEADS_STORE_ENV_VARS {
        assert!(
            removed.iter().any(|key| key == variable),
            "{variable} not removed: {removed:?}"
        );
    }
    // The read flags come first after any interpreter arguments the launcher
    // chooser adds for a script fixture.
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(&args[args.len() - 3..], ["--readonly", "--json", "list"]);
    assert_eq!(
        command.get_current_dir(),
        Some(target.project_root.as_path())
    );
}
