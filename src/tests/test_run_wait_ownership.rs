// Regression coverage for registered gate ownership and frozen wait inputs.
// Uses the registration/index fixtures from test_run_registration.rs; owns no
// launcher execution, notification delivery or tracker-credit policy.
use super::*;

struct GateWaitFixture {
    runs: RunFixture,
    merger: String,
    notify: String,
    directory: PathBuf,
    run_id: String,
}

impl GateWaitFixture {
    fn new(label: &str) -> Self {
        let runs = RunFixture::new(label);
        let merger = owner(&runs);
        let notify_root = runs.root.with_file_name(format!("notify-{label}"));
        fs::create_dir_all(&notify_root).unwrap();
        run_git_test_command(&notify_root, &["init", "--quiet"]);
        let notify_project = create_test_project(&runs.state, &notify_root, "Notification only");
        let notify = create_test_project_session(
            &runs.state, Agent::Claude, &notify_project, &notify_root,
        );
        let run_id = format!("test-{}", Uuid::new_v4());
        let mut request = full_request(&runs.root, "2026-10-24T22:00:00.000Z");
        request["owner"] = json!(merger);
        request["creatorPid"] = json!(std::process::id());
        request["detached"] = json!(true);
        request["notifyTo"] = json!(notify);
        request["expectedFingerprint"] = json!("a".repeat(64));
        let directory = write_run(
            &runs.runs_dir(), &run_id, request, Some(running_results(Some(std::process::id()))),
        );
        runs.state.register_test_run(&merger, registration_request(&run_id, &directory)).unwrap();
        {
            let mut inner = runs.state.inner.lock().unwrap();
            // Reproduce ambiguous filesystem discovery without changing the
            // durable registration or either session's owning project.
            let mut shadow = inner.find_project(&runs.project_id).unwrap().clone();
            shadow.id = "project-shadow".to_owned();
            inner.projects.insert(0, shadow);
            let index = inner.find_session_index(&merger).unwrap();
            inner.sessions[index].session.status = SessionStatus::Active;
        }
        runs.refresh(&[std::process::id()]);
        Self { runs, merger, notify, directory, run_id }
    }

    fn wait(&self, session: &str) -> Result<TestRunWaitResponse, ApiError> {
        self.runs.state.create_test_run_wait_with(
            session,
            serde_json::from_value(json!({ "runIds": [self.run_id], "mode": "all" })).unwrap(),
            &|| {},
        )
    }

    fn prompts(&self, session: &str) -> Vec<String> {
        let inner = self.runs.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(session).unwrap()].queued_prompts.iter()
            .map(|queued| queued.pending_prompt.text.clone()).collect()
    }

    fn pass(&self) {
        let mut results = passed_results();
        results["runId"] = json!(self.run_id);
        results["expectedFingerprint"] = json!("a".repeat(64));
        results["before"] = json!("a".repeat(64));
        results["after"] = json!("a".repeat(64));
        fs::write(self.directory.join("results.json"), results.to_string()).unwrap();
        self.runs.refresh(&[]);
        self.runs.state.refresh_test_run_waits();
    }
}

#[test]
fn registered_gate_wait_belongs_to_merger_not_overlapping_project_or_notify_target() {
    let fixture = GateWaitFixture::new("gate-wait-owner");
    let run = fixture.runs.summary(&fixture.run_id);
    assert_eq!(run.project_id.as_deref(), Some(fixture.runs.project_id.as_str()));
    assert_eq!(run.owner_session_id.as_deref(), Some(fixture.merger.as_str()));
    assert_eq!(run.notify_session_id.as_deref(), Some(fixture.notify.as_str()));
    let response = fixture.wait(&fixture.merger).unwrap();
    assert!(response.wait.runs[0].request_identity.is_some());
    assert_eq!(response.wait.runs[0].expected_fingerprint, Some("a".repeat(64)));
    assert!(!response.resume_prompt_queued);
    // Notification delivery itself is irrelevant: no mailbox wake is invoked.
    fixture.pass();
    let prompts = fixture.prompts(&fixture.merger);
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("- Verdict: PASS"));
    assert!(prompts[0].contains(&fixture.run_id));
    assert!(prompts[0].contains(&format!("Frozen input fingerprint: `{}`", "a".repeat(64))));
    assert!(fixture.prompts(&fixture.notify).is_empty());
    fixture.runs.state.refresh_test_run_waits();
    assert_eq!(fixture.prompts(&fixture.merger), prompts, "consume once per wait");
    // The owner can consume a settled gate through a second wait.
    assert!(fixture.wait(&fixture.merger).unwrap().resume_prompt_queued);
}

#[test]
fn a_notification_recipient_in_another_project_cannot_consume_the_registered_gate() {
    let fixture = GateWaitFixture::new("gate-wait-notify");
    let error = fixture.wait(&fixture.notify).err().unwrap();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("not in the waiting session's project"));
}

#[test]
fn only_exact_durable_registration_overrides_discovered_project() {
    for mode in ["pending", "different-request", "different-locator", "different-native-locator"] {
        let fixture = GateWaitFixture::new(&format!("gate-wait-{mode}"));
        {
            let mut inner = fixture.runs.state.inner.lock().unwrap();
            if mode == "pending" {
                inner.test_runs.registered.remove(&fixture.run_id);
            } else {
                let row = inner.test_run_registrations.get_mut(&fixture.run_id).unwrap();
                if mode == "different-request" {
                    row.request_identity = "b".repeat(64);
                } else if mode == "different-native-locator" {
                    row.locator_identity = Some("b".repeat(64));
                } else {
                    row.run_directory.push_str("-other");
                }
            }
        }
        fixture.runs.refresh(&[std::process::id()]);
        let error = fixture.wait(&fixture.merger).err().unwrap();
        assert_eq!(error.status, StatusCode::BAD_REQUEST, "{mode}");
    }
}

#[test]
fn a_substituted_request_or_frozen_input_cannot_settle_the_original_wait_as_passed() {
    for field in ["expectedFingerprint", "started"] {
        let fixture = GateWaitFixture::new(&format!("gate-wait-substitute-{field}"));
        let response = fixture.wait(&fixture.merger).unwrap();
        let mut request: Value = serde_json::from_slice(
            &fs::read(fixture.directory.join("request.json")).unwrap(),
        ).unwrap();
        request[field] = if field == "started" {
            json!("2026-10-24T23:00:00.000Z")
        } else {
            json!("b".repeat(64))
        };
        fs::write(fixture.directory.join("request.json"), request.to_string()).unwrap();
        fixture.pass();
        let prompts = fixture.prompts(&fixture.merger);
        assert_eq!(prompts.len(), 1, "changed identity settles as unknown, not a stuck wait");
        assert!(prompts[0].contains("UNKNOWN"));
        assert!(!prompts[0].contains("- Verdict: PASS"));
        assert!(prompts[0].contains(response.wait.runs[0].request_identity.as_ref().unwrap()));
        assert!(prompts[0].contains(&"a".repeat(64)), "keep original frozen input");
    }
}

#[test]
fn linked_worktree_wait_keeps_its_identity_when_the_main_scan_project_is_removed() {
    let runs = RunFixture::new("gate-wait-linked-continuity");
    run_git_test_command(&runs.root, &[
        "-c", "user.name=Test", "-c", "user.email=test@example.com",
        "commit", "--allow-empty", "--message", "fixture",
    ]);
    let linked = runs.root.with_file_name("gate-wait-linked-checkout");
    run_git_test_command(&runs.root, &[
        "worktree", "add", "--detach", linked.to_str().unwrap(), "HEAD",
    ]);
    let project = create_test_project(&runs.state, &linked, "Linked gate owner");
    let merger = create_test_project_session(&runs.state, Agent::Codex, &project, &linked);
    let run_id = format!("test-{}", Uuid::new_v4());
    let mut request = full_request(&runs.root, "2026-10-24T22:00:00.000Z");
    request["owner"] = json!(merger);
    request["creatorPid"] = json!(std::process::id());
    request["expectedFingerprint"] = json!("a".repeat(64));
    let directory = write_run(&runs.runs_dir(), &run_id, request,
        Some(running_results(Some(std::process::id()))));
    runs.state.register_test_run(&merger, registration_request(&run_id, &directory)).unwrap();
    runs.refresh(&[std::process::id()]);
    {
        let mut inner = runs.state.inner.lock().unwrap();
        let index = inner.find_session_index(&merger).unwrap();
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    let response = runs.state.create_test_run_wait_with(&merger,
        serde_json::from_value(json!({ "runIds": [run_id] })).unwrap(), &|| {}).unwrap();
    assert!(!response.resume_prompt_queued);
    let locator = response.wait.runs[0].run_dir.clone();

    // Only the linked project now supplies common_dirs. Its .git/commondir
    // spells the same repository with ../..; neither request nor run moved.
    runs.state.delete_project(&runs.project_id).unwrap();
    runs.refresh(&[std::process::id()]);
    assert_eq!(runs.summary(&run_id).run_dir, locator);
    assert!(runs.state.inner.lock().unwrap().test_runs.parsed.contains_key(&directory),
        "canonical discovery keeps the native run-directory cache key");
    assert_eq!(runs.summary(&run_id).project_id.as_deref(), Some(project.as_str()));
    runs.state.refresh_test_run_waits();
    assert_eq!(runs.state.inner.lock().unwrap().test_run_waits.len(), 1);

    let mut result = passed_results();
    result["runId"] = json!(run_id);
    fs::write(directory.join("results.json"), result.to_string()).unwrap();
    runs.refresh(&[]);
    runs.state.refresh_test_run_waits();
    let inner = runs.state.inner.lock().unwrap();
    assert!(inner.test_run_waits.is_empty());
    let record = &inner.sessions[inner.find_session_index(&merger).unwrap()];
    assert_eq!(record.queued_prompts.len(), 1);
    assert!(record.queued_prompts[0].pending_prompt.text.contains("- Verdict: PASS"));
}


#[test]
fn canonical_registration_and_scanned_locator_keep_the_same_owner() {
    let fixture = GateWaitFixture::new("gate-canonical-owner");
    let canonical = fs::canonicalize(&fixture.directory).unwrap();
    let inner = fixture.runs.state.inner.lock().unwrap();
    let row = &inner.test_run_registrations[&fixture.run_id];
    let disk = &inner.test_runs.find(&fixture.run_id).unwrap().disk;
    assert_eq!(row.locator_identity, test_run_locator_identity(&canonical));
    assert_eq!(row.locator_identity, disk.locator_identity);
    assert_eq!(row.run_directory, test_run_display_path(&canonical));
    assert_eq!(test_run_registration_view(&inner, disk).registration_state.as_deref(),
        Some("registered"));
    drop(inner);
    assert!(!fixture.wait(&fixture.merger).unwrap().resume_prompt_queued);
}

fn assert_copied_locator_is_rejected(label: &str, unix_alias: bool) {
    let fixture = GateWaitFixture::new(label);
    let response = fixture.wait(&fixture.merger).unwrap();
    let copy_root = if unix_alias {
        // On Unix these are different native paths but old display keys collide.
        PathBuf::from(fixture.runs.root.to_str().unwrap().replace('\\', "/"))
    } else {
        fixture.runs.root.with_file_name("gate-copied-other-root")
    };
    fs::create_dir_all(&copy_root).unwrap();
    run_git_test_command(&copy_root, &["init", "--quiet"]);
    let project = create_test_project(&fixture.runs.state, &copy_root, "Copied locator");
    {
        let mut inner = fixture.runs.state.inner.lock().unwrap();
        let index = inner.projects.iter().position(|row| row.id == project).unwrap();
        let project = inner.projects.remove(index);
        inner.projects.insert(0, project);
    }
    let request: Value = serde_json::from_slice(
        &fs::read(fixture.directory.join("request.json")).unwrap()).unwrap();
    let copied = write_run(&copy_root.join(".git/review-runs"), &fixture.run_id,
        request, Some(passed_results()));
    let copied_disk = TestRunDisk::read(&copied, None).unwrap();
    assert_eq!(copied_disk.request_identity, response.wait.runs[0].request_identity);
    assert_eq!(copied_disk.expected_fingerprint, response.wait.runs[0].expected_fingerprint);
    assert_ne!(copied_disk.locator_identity, response.wait.runs[0].locator_identity);
    if unix_alias {
        assert_eq!(test_run_path_key(&test_run_display_path(&fixture.directory)),
            test_run_path_key(&test_run_display_path(&copied)));
    }
    fs::rename(&fixture.directory,
        fixture.directory.with_file_name("removed-original-run")).unwrap();
    fixture.runs.refresh(&[]);
    {
        let inner = fixture.runs.state.inner.lock().unwrap();
        let entry = inner.test_runs.find(&fixture.run_id).unwrap();
        assert_eq!(entry.disk.locator_identity, copied_disk.locator_identity,
            "the scanner really selected the copied request");
        assert_ne!(entry.summary.registration.registration_state.as_deref(), Some("registered"));
    }
    fixture.runs.state.refresh_test_run_waits();
    let prompts = fixture.prompts(&fixture.merger);
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("UNKNOWN"));
    assert!(!prompts[0].contains("- Verdict: PASS"));
}

#[test]
fn a_copied_request_at_another_native_locator_cannot_supply_pass() {
    assert_copied_locator_is_rejected("gate-copy-locator", false);
}

#[cfg(unix)]
#[test]
fn distinct_unix_native_paths_with_the_same_display_key_cannot_supply_pass() {
    assert_copied_locator_is_rejected("gate-native\\copy", true);
}

#[test]
fn deleting_the_registered_project_restores_discovery_for_the_surviving_project() {
    let fixture = GateWaitFixture::new("gate-deleted-project");
    let project = {
        let inner = fixture.runs.state.inner.lock().unwrap();
        inner.projects[0].id.clone()
    };
    assert_ne!(project, fixture.runs.project_id);
    let session = create_test_project_session(&fixture.runs.state, Agent::Codex,
        &project, &fixture.runs.root);
    fixture.runs.state.delete_project(&fixture.runs.project_id).unwrap();
    fixture.runs.refresh(&[std::process::id()]);
    assert_eq!(fixture.runs.summary(&fixture.run_id).project_id.as_deref(), Some(project.as_str()));
    assert!(!fixture.wait(&session).unwrap().resume_prompt_queued);
}

#[cfg(windows)]
#[test]
fn canonical_unc_display_and_native_identity_do_not_share_a_lossy_key() {
    let canonical = FsPath::new(r"\\?\UNC\server\share\repo\.git\review-runs\test-run");
    let ordinary = FsPath::new(r"\\server\share\repo\.git\review-runs\test-run");
    assert_eq!(test_run_display_path(canonical), test_run_display_path(ordinary));
    assert_eq!(test_run_display_path(canonical), "//server/share/repo/.git/review-runs/test-run");
    // Synthetic canonical paths exercise encoding; no UNC service is contacted.
    let different = FsPath::new(r"\\?\UNC\server\share\other\.git\review-runs\test-run");
    assert_ne!(test_run_native_path_key(canonical), test_run_native_path_key(different));
}


#[cfg(unix)]
#[test]
fn native_locator_digest_preserves_non_utf8_directory_bytes() {
    use std::os::unix::ffi::OsStringExt;
    let first = PathBuf::from(std::ffi::OsString::from_vec(b"/repo/a\xff/test-run".to_vec()));
    let second = PathBuf::from(std::ffi::OsString::from_vec(b"/repo/a\xfe/test-run".to_vec()));
    assert_eq!(test_run_display_path(&first), test_run_display_path(&second));
    assert_ne!(test_run_native_path_key(&first), test_run_native_path_key(&second));
}


#[test]
fn a_readable_replacement_that_leaves_the_original_id_unindexed_settles_unknown() {
    for mode in ["changed-run-id", "unsupported"] {
        let fixture = GateWaitFixture::new(&format!("gate-replaced-{mode}"));
        fixture.wait(&fixture.merger).unwrap();
        let mut request: Value = serde_json::from_slice(
            &fs::read(fixture.directory.join("request.json")).unwrap()).unwrap();
        request["runId"] = if mode == "changed-run-id" {
            json!("test-replacement")
        } else {
            json!("x".repeat(TEST_RUN_TEXT_MAX_BYTES + 1))
        };
        fs::write(fixture.directory.join("request.json"), request.to_string()).unwrap();
        if mode == "unsupported" {
            assert!(TestRunDisk::read(&fixture.directory, None).is_none(),
                "readable JSON that the index cannot hold");
        }
        fixture.runs.refresh(&[std::process::id()]);
        assert!(fixture.runs.state.inner.lock().unwrap().test_runs.find(&fixture.run_id).is_none());
        fixture.runs.state.refresh_test_run_waits();
        let prompts = fixture.prompts(&fixture.merger);
        assert_eq!(prompts.len(), 1, "{mode}");
        assert!(prompts[0].contains("UNKNOWN (not indexed)"), "{mode}: {}", prompts[0]);
        assert!(!prompts[0].contains("- Verdict: PASS"));
        assert!(fixture.runs.state.inner.lock().unwrap().test_run_waits.is_empty());
    }
}

#[test]
fn a_matching_pinned_original_temporarily_absent_from_the_index_stays_pending() {
    let fixture = GateWaitFixture::new("gate-matching-unindexed");
    fixture.wait(&fixture.merger).unwrap();
    fixture.runs.state.inner.lock().unwrap().test_runs.entries.clear();
    fixture.runs.state.refresh_test_run_waits();
    assert!(fixture.prompts(&fixture.merger).is_empty());
    assert_eq!(fixture.runs.state.inner.lock().unwrap().test_run_waits.len(), 1);
}

#[test]
fn an_absent_cached_locator_is_retried_without_request_or_results_changes() {
    let fixture = GateWaitFixture::new("gate-retry-native-locator");
    {
        let mut inner = fixture.runs.state.inner.lock().unwrap();
        let cached = inner.test_runs.parsed.values_mut()
            .find(|disk| disk.run_id == fixture.run_id).unwrap();
        assert_eq!(cached.read_failures, 0);
        Arc::make_mut(cached).locator_identity = None;
    }
    fixture.runs.refresh(&[std::process::id()]);
    let inner = fixture.runs.state.inner.lock().unwrap();
    let entry = inner.test_runs.find(&fixture.run_id).unwrap();
    assert!(entry.disk.locator_identity.is_some());
    assert_eq!(entry.summary.registration.registration_state.as_deref(), Some("registered"));
    drop(inner);
    fixture.wait(&fixture.merger).unwrap();
}


#[tokio::test]
async fn durable_registration_changes_cached_project_authority_without_another_scan() {
    let runs = RunFixture::new("gate-scan-before-registration");
    let merger = owner(&runs);
    let shadow_id = "project-before-registration";
    {
        let mut inner = runs.state.inner.lock().unwrap();
        let mut shadow = inner.find_project(&runs.project_id).unwrap().clone();
        shadow.id = shadow_id.to_owned();
        inner.projects.insert(0, shadow);
    }
    let foreign = create_test_project_session(&runs.state, Agent::Codex, shadow_id, &runs.root);
    let run_id = format!("test-{}", Uuid::new_v4());
    let mut request = full_request(&runs.root, "2026-10-24T22:00:00.000Z");
    request["owner"] = json!(merger);
    request["creatorPid"] = json!(std::process::id());
    let directory = write_run(&runs.runs_dir(), &run_id, request,
        Some(running_results(Some(std::process::id()))));
    runs.refresh(&[std::process::id()]);
    assert_eq!(runs.summary(&run_id).project_id.as_deref(), Some(shadow_id));
    runs.state.register_test_run(&merger, registration_request(&run_id, &directory)).unwrap();
    // The scanner's cache is deliberately untouched; current projections and
    // wait validation must use the newly durable authority without a rescan.
    assert_eq!(runs.state.inner.lock().unwrap().test_runs.find(&run_id).unwrap()
        .summary.project_id.as_deref(), Some(shadow_id));
    assert_eq!(runs.summary(&run_id).project_id.as_deref(), Some(runs.project_id.as_str()));
    assert!(runs.state.test_run_summaries(Some(shadow_id)).is_empty());
    assert_eq!(runs.state.test_run_summaries(Some(&runs.project_id)).len(), 1);
    let wait = || serde_json::from_value(json!({ "runIds": [run_id] })).unwrap();
    assert_eq!(runs.state.create_test_run_wait_with(&foreign, wait(), &|| {}).err().unwrap()
        .status, StatusCode::BAD_REQUEST);
    runs.state.create_test_run_wait_with(&merger, wait(), &|| {}).unwrap();
    let app = app_router(runs.state.clone());
    let (status, list): (StatusCode, Value) = request_json(&app, Request::builder()
        .uri("/api/test-runs").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    let listed = list["runs"].as_array().unwrap().iter()
        .find(|run| run["runId"] == run_id).unwrap();
    assert_eq!(listed["projectId"], runs.project_id);
    assert_eq!(listed["registrationState"], "registered");
    let (status, detail): (StatusCode, Value) = request_json(&app, Request::builder()
        .uri(format!("/api/test-runs/{run_id}")).body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["source"], "index");
    assert_eq!(detail["run"]["projectId"], runs.project_id);
    assert_eq!(detail["run"]["registrationState"], "registered");
}
