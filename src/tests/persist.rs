//! `PersistedState` load/serialize round-trip tests.
//!
//! `PersistedState` is the on-disk schema for TermAl's entire backend
//! state: preferences, projects, remotes, sessions, orchestrators, and
//! workspace layouts. Runtime persistence uses the SQLite-backed store in
//! `src/persist.rs`, while schema-shape tests deserialize in memory when they
//! need to corrupt individual fields deliberately.
//!
//! Schema validation on load is deliberately strict: any missing
//! required field produces an error rather than a silent default, so a
//! corrupted or partially-migrated file cannot resurrect sessions with
//! quietly-broken state. Path normalization additionally folds Windows
//! `\\?\` extended-length prefixes and legacy backslash forms to a
//! canonical form, so a file saved on one machine reloads cleanly on
//! another.
//!
//! The `persisted_state_load_error_after_mutation` helper takes a mutation
//! closure, deserializes the mutated value through the persisted-state schema,
//! and returns the error string. Each required-field test stays focused on a
//! single missing-field assertion without reviving the removed JSON file path.

use super::*;

fn persisted_state_load_error_after_mutation<F>(inner: StateInner, mutate: F) -> String
where
    F: FnOnce(&mut Value),
{
    let mut encoded = persisted_state_value(&inner);
    mutate(&mut encoded);

    let err = match state_inner_from_persisted_value(encoded) {
        Ok(_) => panic!("mutated persisted state should fail to load"),
        Err(err) => err,
    };
    format!("{err:#}")
}

fn persisted_state_value(inner: &StateInner) -> Value {
    serde_json::to_value(PersistedState::from_inner(inner))
        .expect("persisted state should serialize")
}

fn state_inner_from_persisted_value(encoded: Value) -> Result<StateInner> {
    let persisted: PersistedState =
        serde_json::from_value(encoded).context("failed to deserialize persisted state")?;
    persisted
        .into_inner()
        .context("failed to validate state from in-memory persisted state")
}

// Pins that legacy Windows `\\?\` verbatim prefixes on a project
// `rootPath` and a session `workdir` are stripped back to their
// canonical form on load. Guards against stale files from older
// TermAl builds resurrecting duplicate or mismatched projects.
#[cfg(windows)]
#[test]
fn persisted_state_normalizes_legacy_local_verbatim_paths() {
    let project_root =
        std::env::temp_dir().join(format!("termal-legacy-verbatim-path-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let legacy_root = format!(r"\\?\{normalized_root}");
    let mut inner = StateInner::new();
    let project = inner.create_project(None, normalized_root.clone(), default_local_remote_id());
    inner.create_session(
        Agent::Claude,
        Some("Claude".to_owned()),
        normalized_root.clone(),
        Some(project.id),
        None,
    );
    let mut encoded = persisted_state_value(&inner);
    encoded["projects"][0]["rootPath"] = Value::String(legacy_root.clone());
    encoded["sessions"][0]["session"]["workdir"] = Value::String(legacy_root);

    let loaded = state_inner_from_persisted_value(encoded).expect("persisted state should load");
    assert_eq!(loaded.projects[0].root_path, normalized_root);
    assert_eq!(loaded.sessions[0].session.workdir, normalized_root);

    let _ = fs::remove_dir_all(project_root);
}

// Pins that legacy `\\?\` verbatim prefixes inside workspace layout
// tabs (filesystem `rootPath`, git/debug `workdir`, source `path`,
// diff `filePath`, pane `sourcePath`) are all normalized on load.
// Guards against tab paths drifting out of sync with canonical roots.
#[cfg(windows)]
#[test]
fn persisted_state_normalizes_legacy_workspace_layout_paths() {
    let project_root =
        std::env::temp_dir().join(format!("termal-layout-verbatim-path-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let legacy_root = format!(r"\\?\{normalized_root}");
    let normalized_file = format!(r"{normalized_root}\src\main.rs");
    let legacy_file = format!(r"\\?\{normalized_file}");
    let mut inner = StateInner::new();
    inner.workspace_layouts.insert(
        "workspace-1".to_owned(),
        WorkspaceLayoutDocument {
            id: "workspace-1".to_owned(),
            revision: 1,
            updated_at: "2026-04-01 12:00:00".to_owned(),
            control_panel_side: WorkspaceControlPanelSide::Left,
            theme_id: None,
            style_id: None,
            font_size_px: None,
            editor_font_size_px: None,
            density_percent: None,
            workspace: json!({
                "root": {
                    "type": "pane",
                    "paneId": "pane-a"
                },
                "panes": [{
                    "id": "pane-a",
                    "tabs": [
                        {
                            "id": "tab-files",
                            "kind": "filesystem",
                            "rootPath": legacy_root,
                            "originSessionId": serde_json::Value::Null
                        },
                        {
                            "id": "tab-git",
                            "kind": "gitStatus",
                            "workdir": format!(r"\\?\{normalized_root}"),
                            "originSessionId": serde_json::Value::Null
                        },
                        {
                            "id": "tab-debug",
                            "kind": "instructionDebugger",
                            "workdir": format!(r"\\?\{normalized_root}"),
                            "originSessionId": serde_json::Value::Null
                        },
                        {
                            "id": "tab-source",
                            "kind": "source",
                            "path": legacy_file,
                            "originSessionId": serde_json::Value::Null
                        },
                        {
                            "id": "tab-diff",
                            "kind": "diffPreview",
                            "changeType": "edit",
                            "diff": "-before\n+after",
                            "diffMessageId": "message-1",
                            "filePath": format!(r"\\?\{normalized_file}"),
                            "originSessionId": serde_json::Value::Null,
                            "summary": "Updated file"
                        }
                    ],
                    "activeTabId": "tab-files",
                    "activeSessionId": serde_json::Value::Null,
                    "viewMode": "filesystem",
                    "lastSessionViewMode": "session",
                    "sourcePath": format!(r"\\?\{normalized_file}")
                }],
                "activePaneId": "pane-a"
            }),
        },
    );
    let loaded = state_inner_from_persisted_value(persisted_state_value(&inner))
        .expect("persisted state should load");
    let layout = loaded
        .workspace_layouts
        .get("workspace-1")
        .expect("workspace layout should load");
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/sourcePath")
            .and_then(Value::as_str),
        Some(normalized_file.as_str())
    );
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/tabs/0/rootPath")
            .and_then(Value::as_str),
        Some(normalized_root.as_str())
    );
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/tabs/1/workdir")
            .and_then(Value::as_str),
        Some(normalized_root.as_str())
    );
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/tabs/2/workdir")
            .and_then(Value::as_str),
        Some(normalized_root.as_str())
    );
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/tabs/3/path")
            .and_then(Value::as_str),
        Some(normalized_file.as_str())
    );
    assert_eq!(
        layout
            .workspace
            .pointer("/panes/0/tabs/4/filePath")
            .and_then(Value::as_str),
        Some(normalized_file.as_str())
    );

    let _ = fs::remove_dir_all(project_root);
}

#[cfg(windows)]
#[test]
fn app_state_new_with_paths_normalizes_verbatim_bootstrap_workdirs() {
    let _env_lock = TEST_HOME_ENV_MUTEX
        .lock()
        .expect("test home env mutex poisoned");
    let project_root =
        std::env::temp_dir().join(format!("termal-bootstrap-verbatim-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let verbatim_root = format!(r"\\?\{normalized_root}");
    let state_root = std::env::temp_dir().join(format!(
        "termal-bootstrap-verbatim-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let _home = ScopedEnvVar::set_path(TEST_HOME_ENV_KEY, &state_root);
    let persistence_path = state_root.join("termal.sqlite");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let state =
        AppState::new_with_paths(verbatim_root, persistence_path, orchestrator_templates_path)
            .expect("app state should bootstrap from verbatim default workdir");

    assert_eq!(state.default_workdir, normalized_root);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.projects.len(), 1);
    assert_eq!(inner.projects[0].root_path, normalized_root);
    for agent in [Agent::Codex, Agent::Claude] {
        let session = inner
            .sessions
            .iter()
            .find(|record| record.session.agent == agent)
            .expect("bootstrapped live session should exist");
        assert_eq!(session.session.workdir, normalized_root);
        assert_eq!(
            session.session.project_id.as_deref(),
            Some(inner.projects[0].id.as_str())
        );
    }
    drop(inner);
    state.shutdown_persist_blocking();

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(state_root);
}

#[cfg(windows)]
fn create_windows_file_symlink_or_skip(target: &FsPath, link: &FsPath) -> bool {
    match std::os::windows::fs::symlink_file(target, link) {
        Ok(()) => true,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("skipping Windows symlink assertion without symlink privilege: {err}");
            false
        }
        Err(err) => panic!("Windows file symlink should be created: {err}"),
    }
}

#[cfg(windows)]
fn create_windows_dir_reparse_point_or_skip(target: &FsPath, link: &FsPath) -> bool {
    match std::os::windows::fs::symlink_dir(target, link) {
        Ok(()) => true,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!(
                "skipping Windows directory reparse-point assertion without symlink privilege: {err}"
            );
            false
        }
        Err(err) => panic!("Windows directory reparse point should be created: {err}"),
    }
}

#[cfg(windows)]
fn assert_windows_state_redirection_rejected(error: anyhow::Error) {
    assert!(
        format!("{error:#}").contains("refusing to follow redirected state path"),
        "{error:#}"
    );
}

#[cfg(windows)]
#[test]
fn windows_sqlite_state_redirection_rejects_main_database_link() {
    let state_root = std::env::temp_dir().join(format!(
        "termal-windows-main-redirection-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let db = state_root.join("termal.sqlite");
    let main_target = state_root.join("main-target.sqlite");

    fs::write(&main_target, b"target").expect("main target should write");
    if create_windows_file_symlink_or_skip(&main_target, &db) {
        let main_error = reject_existing_sqlite_state_path_redirection(&db)
            .expect_err("main sqlite symlink should be rejected");
        assert_windows_state_redirection_rejected(main_error);
    }

    let _ = fs::remove_dir_all(state_root);
}

#[cfg(windows)]
#[test]
fn windows_sqlite_state_redirection_rejects_sidecar_link_independently() {
    let state_root = std::env::temp_dir().join(format!(
        "termal-windows-sidecar-redirection-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let db = state_root.join("termal.sqlite");
    let wal_target = state_root.join("wal-target");

    fs::write(&wal_target, b"wal").expect("wal target should write");
    let wal_link = sqlite_sidecar_path(&db, "-wal");
    if create_windows_file_symlink_or_skip(&wal_target, &wal_link) {
        let wal_error = reject_existing_sqlite_state_path_redirection(&db)
            .expect_err("sqlite sidecar symlink should be rejected");
        assert_windows_state_redirection_rejected(wal_error);
    }

    let _ = fs::remove_dir_all(state_root);
}

#[cfg(windows)]
#[test]
fn windows_sqlite_state_redirection_rejects_termal_directory_reparse_point_independently() {
    let state_root =
        std::env::temp_dir().join(format!("termal-windows-dir-redirection-{}", Uuid::new_v4()));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let redirected_target = state_root.join("redirected-termal-target");
    let termal_dir = state_root.join(".termal");

    fs::create_dir_all(&redirected_target).expect("redirected target should exist");
    if create_windows_dir_reparse_point_or_skip(&redirected_target, &termal_dir) {
        let directory_error =
            reject_existing_sqlite_state_path_redirection(&termal_dir.join("termal.sqlite"))
                .expect_err(".termal directory reparse point should be rejected");
        assert_windows_state_redirection_rejected(directory_error);
    }

    let _ = fs::remove_dir_all(state_root);
}

// Tests that persisted state preserves significant local path spaces.
#[cfg(not(windows))]
#[test]
fn persisted_state_preserves_significant_local_path_spaces() {
    let project_root =
        std::env::temp_dir().join(format!("termal-significant-path-space-{} ", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let state_root = std::env::temp_dir().join(format!(
        "termal-significant-path-space-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let path = state_root.join("termal.sqlite");

    assert!(normalized_root.ends_with(' '));

    let mut inner = StateInner::new();
    let project = inner.create_project(None, normalized_root.clone(), default_local_remote_id());
    inner.create_session(
        Agent::Claude,
        Some("Claude".to_owned()),
        normalized_root.clone(),
        Some(project.id),
        None,
    );
    persist_state(&path, &inner).expect("persisted state should be written");

    let loaded = load_state(&path)
        .expect("persisted state should load")
        .expect("persisted state should exist");
    assert_eq!(loaded.projects[0].root_path, normalized_root);
    assert_eq!(loaded.sessions[0].session.workdir, normalized_root);

    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

// Pins that stripping the top-level `projects` field causes
// `load_state` to fail with `missing field `projects``. Guards
// against sessions reloading against an empty project list and
// silently losing their project associations.
#[test]
fn persisted_state_requires_projects() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Codex,
        Some("Migrated".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        encoded
            .as_object_mut()
            .expect("persisted state should be an object")
            .remove("projects");
    });

    assert!(
        err_text.contains("missing field `projects`"),
        "unexpected load_state error: {err_text}"
    );
}

// Pins that stripping `nextProjectNumber` causes `load_state` to
// fail with `missing field `nextProjectNumber``. Guards against the
// project-number counter resetting on reload and colliding with
// existing project names.
#[test]
fn persisted_state_requires_next_project_number() {
    let inner = StateInner::new();

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        encoded
            .as_object_mut()
            .expect("persisted state should be an object")
            .remove("nextProjectNumber");
    });

    assert!(
        err_text.contains("missing field `nextProjectNumber`"),
        "unexpected load_state error: {err_text}"
    );
}

// Pins that stripping `remoteId` from a project entry causes
// `load_state` to fail with `missing field `remoteId``. Guards
// against projects reloading without a remote binding and being
// silently re-homed onto the default local remote.
#[test]
fn persisted_state_requires_project_remote_id() {
    let mut inner = StateInner::new();
    inner.create_project(None, "/tmp".to_owned(), default_local_remote_id());

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        encoded["projects"]
            .as_array_mut()
            .expect("persisted projects should be an array")[0]
            .as_object_mut()
            .expect("persisted project should be an object")
            .remove("remoteId");
    });

    assert!(
        err_text.contains("missing field `remoteId`"),
        "unexpected load_state error: {err_text}"
    );
}

// Pins that injecting two remotes sharing the same `id` fails load
// with a `duplicate remote id` validation error. Guards against the
// remote registry accepting ambiguous ids that would let sessions
// silently resolve to the wrong transport.
#[test]
fn persisted_state_requires_valid_remotes() {
    let inner = StateInner::new();

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        encoded["preferences"]["remotes"] = json!([
            {
                "id": "local",
                "name": "Local",
                "transport": "local",
                "enabled": true
            },
            {
                "id": "ssh-1",
                "name": "pop-os",
                "transport": "ssh",
                "enabled": true,
                "host": "pop-os.local",
                "port": 22,
                "user": "greg"
            },
            {
                "id": "ssh-1",
                "name": "backup",
                "transport": "ssh",
                "enabled": true,
                "host": "backup.local",
                "port": 22,
                "user": "greg"
            }
        ]);
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("duplicate remote id `ssh-1`"),
        "unexpected load_state error: {err_text}"
    );
}

// Pins that stripping `cursorMode` from a Cursor session fails load
// with a `missing cursorMode` validation error. Guards against
// Cursor sessions reloading with an ambiguous tool mode and
// executing under the wrong approval posture.
#[test]
fn persisted_state_requires_cursor_mode() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Cursor,
        Some("Cursor".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let session = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.remove("cursorMode");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing cursorMode"),
        "unexpected load_state error: {err_text}"
    );
}

// Pins that stripping `claudeApprovalMode` and `claudeEffort` from
// a Claude session fails load with `missing claudeApprovalMode`.
// Guards against Claude sessions losing their approval posture and
// silently reloading with default permissiveness.
#[test]
fn persisted_state_requires_claude_settings() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Claude,
        Some("Claude".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let session = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.remove("claudeApprovalMode");
        session.remove("claudeEffort");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing claudeApprovalMode"),
        "unexpected load_state error: {err_text}"
    );
}

// Pins that stripping `geminiApprovalMode` from a Gemini session
// fails load with `missing geminiApprovalMode`. Guards against
// Gemini sessions reloading with an unspecified approval mode and
// running with a different tool posture than the user configured.
#[test]
fn persisted_state_requires_gemini_approval_mode() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Gemini,
        Some("Gemini".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let session = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.remove("geminiApprovalMode");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing geminiApprovalMode"),
        "unexpected load_state error: {err_text}"
    );
}

// Pins that stripping `approvalPolicy`, `reasoningEffort`, and
// `sandboxMode` from a Codex session fails load with `missing
// approvalPolicy`. Guards against Codex sessions reloading without
// the prompt-control triplet that gates each new turn.
#[test]
fn persisted_state_requires_codex_prompt_fields() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Codex,
        Some("Codex".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let session = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.remove("approvalPolicy");
        session.remove("reasoningEffort");
        session.remove("sandboxMode");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing approvalPolicy"),
        "unexpected load_state error: {err_text}"
    );
}

// Pins that a Codex session carrying an `externalSessionId` (a live
// thread) fails load when `codexThreadState` is stripped, with
// `missing codexThreadState`. Guards against a live thread coming
// back attached but with no resume state for the orchestrator.
#[test]
fn persisted_state_requires_codex_thread_state_for_live_threads() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Codex,
        Some("Codex".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );

    let err_text = persisted_state_load_error_after_mutation(inner, |encoded| {
        let entry = encoded["sessions"]
            .as_array_mut()
            .expect("persisted sessions should be an array")[0]
            .as_object_mut()
            .expect("persisted session record should be an object");
        entry.insert(
            "externalSessionId".to_owned(),
            Value::String("thread-live".to_owned()),
        );
        let session = entry["session"]
            .as_object_mut()
            .expect("persisted session should be an object");
        session.insert(
            "externalSessionId".to_owned(),
            Value::String("thread-live".to_owned()),
        );
        session.remove("codexThreadState");
    });

    assert!(
        err_text.contains("failed to validate state from")
            && err_text.contains("missing codexThreadState"),
        "unexpected load_state error: {err_text}"
    );
}

// Pins that stripping `source` from a persisted queued prompt fails
// load with `missing field `source``. Guards against queued prompts
// reloading with an unknown origin (user vs orchestrator) and being
// routed or billed against the wrong caller on resume.
#[test]
fn persisted_state_requires_queued_prompt_source() {
    let mut inner = StateInner::new();
    let record = inner.create_session(
        Agent::Codex,
        Some("Queued".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let session_id = record.session.id.clone();
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    queue_prompt_on_record(
        &mut inner.sessions[index],
        PendingPrompt {
            attachments: Vec::new(),
            id: "queued-prompt-1".to_owned(),
            timestamp: stamp_now(),
            text: "queued prompt".to_owned(),
            expanded_text: None,
        },
        Vec::new(),
    );
    let mut encoded = persisted_state_value(&inner);
    let sessions = encoded["sessions"]
        .as_array_mut()
        .expect("persisted sessions should be an array");
    let queued_prompts = sessions[0]["queuedPrompts"]
        .as_array_mut()
        .expect("persisted queued prompts should be an array");
    queued_prompts[0]
        .as_object_mut()
        .expect("queued prompt should be an object")
        .remove("source");

    let err = match state_inner_from_persisted_value(encoded) {
        Ok(_) => panic!("persisted state without queued prompt source should fail"),
        Err(err) => err,
    };
    let err_text = format!("{err:#}");
    assert!(
        err_text.contains("missing field `source`"),
        "unexpected load_state error: {err_text}"
    );
}

#[test]
fn persisted_state_omits_runtime_session_mutation_stamp_on_save() {
    let mut inner = StateInner::new();
    let record = inner.create_session(
        Agent::Claude,
        Some("Stamped".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let session_id = record.session.id.clone();
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    inner.sessions[index].mutation_stamp = 99;
    inner.sessions[index].session.session_mutation_stamp = Some(99);

    let encoded = persisted_state_value(&inner);
    {
        let persisted_session = encoded["sessions"][0]["session"]
            .as_object()
            .expect("persisted session should be an object");
        assert!(
            !persisted_session.contains_key("sessionMutationStamp"),
            "runtime mutation stamps must not be serialized into persisted sessions"
        );
    }
}

#[test]
fn persisted_state_clears_runtime_session_mutation_stamp_on_load() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Claude,
        Some("Stamped".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let mut encoded = persisted_state_value(&inner);
    encoded["sessions"][0]["session"]
        .as_object_mut()
        .expect("persisted session should be an object")
        .insert("sessionMutationStamp".to_owned(), Value::from(99));

    let loaded = state_inner_from_persisted_value(encoded).expect("persisted state should load");
    assert_eq!(loaded.sessions[0].session.session_mutation_stamp, None);
}

#[test]
fn persisted_state_round_trips_conversation_markers() {
    let mut inner = StateInner::new();
    let record = inner.create_session(
        Agent::Codex,
        Some("Marked".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let session_id = record.session.id.clone();
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    inner.sessions[index]
        .session
        .markers
        .push(ConversationMarker {
            id: "marker-1".to_owned(),
            session_id: session_id.clone(),
            kind: ConversationMarkerKind::Decision,
            name: "Use the overview rail".to_owned(),
            body: Some("User accepted the overview-map direction.".to_owned()),
            color: "#3b82f6".to_owned(),
            message_id: "message-1".to_owned(),
            message_index_hint: 0,
            end_message_id: Some("message-3".to_owned()),
            end_message_index_hint: Some(2),
            created_at: "2026-05-01 10:00:00".to_owned(),
            updated_at: "2026-05-01 10:05:00".to_owned(),
            created_by: ConversationMarkerAuthor::User,
        });

    let encoded = persisted_state_value(&inner);
    assert_eq!(
        encoded["sessions"][0]["session"]["markers"][0]["name"],
        Value::String("Use the overview rail".to_owned())
    );

    let loaded = state_inner_from_persisted_value(encoded).expect("persisted state should load");
    let markers = &loaded.sessions[0].session.markers;
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].id, "marker-1");
    assert_eq!(markers[0].session_id, session_id);
    assert_eq!(markers[0].kind, ConversationMarkerKind::Decision);
    assert_eq!(markers[0].created_by, ConversationMarkerAuthor::User);
    assert_eq!(markers[0].end_message_id.as_deref(), Some("message-3"));
}

#[test]
fn persisted_state_defaults_missing_conversation_markers() {
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Claude,
        Some("No Markers".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let mut encoded = persisted_state_value(&inner);
    encoded["sessions"][0]["session"]
        .as_object_mut()
        .expect("persisted session should be an object")
        .remove("markers");

    let loaded = state_inner_from_persisted_value(encoded).expect("persisted state should load");
    assert!(loaded.sessions[0].session.markers.is_empty());
}

#[test]
fn persisted_state_maps_unknown_conversation_marker_kind_to_custom() {
    let mut inner = StateInner::new();
    let record = inner.create_session(
        Agent::Codex,
        Some("Marked".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let session_id = record.session.id.clone();
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    inner.sessions[index]
        .session
        .markers
        .push(ConversationMarker {
            id: "marker-legacy".to_owned(),
            session_id,
            kind: ConversationMarkerKind::Custom,
            name: "Legacy marker".to_owned(),
            body: None,
            color: "#94a3b8".to_owned(),
            message_id: "message-1".to_owned(),
            message_index_hint: 0,
            end_message_id: None,
            end_message_index_hint: None,
            created_at: "2026-05-01 10:00:00".to_owned(),
            updated_at: "2026-05-01 10:00:00".to_owned(),
            created_by: ConversationMarkerAuthor::System,
        });
    let mut encoded = persisted_state_value(&inner);
    encoded["sessions"][0]["session"]["markers"][0]["kind"] =
        Value::String("obsoleteKind".to_owned());

    let loaded = state_inner_from_persisted_value(encoded).expect("persisted state should load");
    assert_eq!(
        loaded.sessions[0].session.markers[0].kind,
        ConversationMarkerKind::Custom
    );
}

// Builds an `AppState` like `test_app_state` but with a LIVE
// persist channel receiver so the caller can observe
// `PersistRequest` signals. The default `test_app_state` drops
// the receiver on construction so every `persist_tx.send(...)`
// returns `Err(Disconnected)` and tests automatically take the
// synchronous SQLite fallback path, which hides whether a code path
// correctly routes async.
fn test_app_state_with_live_persist_channel() -> (AppState, mpsc::Receiver<PersistRequest>) {
    let (persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();
    let state_root = std::env::temp_dir().join(format!("termal-test-state-{}", Uuid::new_v4()));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let persistence_path = state_root.join("termal.sqlite");
    let state = AppState {
        server_instance_id: Uuid::new_v4().to_string(),
        default_workdir: "/tmp".to_owned(),
        local_http_base_url: Arc::new(Mutex::new(None)),
        persistence_path: Arc::new(persistence_path),
        orchestrator_templates_path: Arc::new(
            std::env::temp_dir().join(format!("termal-orchestrators-test-{}.json", Uuid::new_v4())),
        ),
        orchestrator_templates_lock: Arc::new(Mutex::new(())),
        review_documents_lock: Arc::new(Mutex::new(())),
        state_events: broadcast::channel(16).0,
        delta_events: broadcast::channel(16).0,
        file_events: broadcast::channel(16).0,
        file_events_revision: Arc::new(AtomicU64::new(0)),
        persist_tx,
        // Live persist channel for the test; the worker thread is not
        // spawned by this constructor, so there's no JoinHandle to track.
        persist_thread_handle: Arc::new(Mutex::new(None)),
        persist_worker_alive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        shutdown_signal_tx: Arc::new(tokio::sync::watch::channel(false).0),
        state_broadcast_mailbox: None,
        telegram_relay_runtime: Arc::new(Mutex::new(TelegramRelayRuntime::default())),
        shared_codex_runtime: Arc::new(Mutex::new(None)),
        test_acp_runtime_overrides: Arc::new(Mutex::new(Vec::new())),
        test_agent_setup_failures: Arc::new(Mutex::new(Vec::new())),
        agent_readiness_cache: Arc::new(RwLock::new(fresh_agent_readiness_cache("/tmp"))),
        agent_readiness_refresh_lock: Arc::new(Mutex::new(())),
        remote_registry: test_remote_registry(),
        remote_sse_fallback_resynced_revision: Arc::new(Mutex::new(HashMap::new())),
        remote_delta_replay_cache: Arc::new(Mutex::new(RemoteDeltaReplayCache::default())),
        remote_delta_hydrations_in_flight: Arc::new(Mutex::new(HashSet::new())),
        remote_lifecycle_actions_in_flight: Arc::new(Mutex::new(HashSet::new())),
        terminal_local_command_semaphore: Arc::new(tokio::sync::Semaphore::new(
            TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT,
        )),
        terminal_remote_command_semaphore: Arc::new(tokio::sync::Semaphore::new(
            TERMINAL_REMOTE_COMMAND_CONCURRENCY_LIMIT,
        )),
        stopping_orchestrator_ids: Arc::new(Mutex::new(HashSet::new())),
        stopping_orchestrator_session_ids: Arc::new(Mutex::new(HashMap::new())),
        inner: Arc::new(Mutex::new(StateInner::new())),
    };
    (state, persist_rx)
}

#[test]
fn persist_worker_retry_state_doubles_and_resets_backoff() {
    let mut retry_state = PersistWorkerRetryState::default();

    let failure: Result<()> = Err(anyhow!("injected persist failure"));
    retry_state.record_result(&failure);
    assert!(retry_state.retry_after_failure);
    assert_eq!(
        retry_state.retry_delay,
        PERSIST_RETRY_SEED_DELAY * 2,
        "first failure should arm the first retry wait"
    );

    retry_state.record_result(&Ok(()));
    assert!(!retry_state.retry_after_failure);
    assert_eq!(
        retry_state.retry_delay, PERSIST_RETRY_SEED_DELAY,
        "successful retry should reset the next failure to the baseline backoff"
    );
}

#[test]
fn persist_worker_shutdown_exits_only_after_successful_final_tick() {
    let mut retry_state = PersistWorkerRetryState::default();

    let failure: Result<()> = Err(anyhow!("injected shutdown persist failure"));
    retry_state.record_result(&failure);
    assert!(
        !retry_state.should_exit_after_tick(true),
        "shutdown should keep retrying after a failed final persist tick"
    );
    assert!(
        !retry_state.should_exit_after_tick(false),
        "non-shutdown ticks should never exit the worker"
    );

    retry_state.record_result(&Ok(()));
    assert!(
        retry_state.should_exit_after_tick(true),
        "shutdown should exit once durability is confirmed"
    );
}

#[test]
fn persist_worker_retry_wait_times_out_without_new_delta() {
    let (_persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();
    let retry_state = PersistWorkerRetryState {
        retry_after_failure: true,
        retry_delay: Duration::from_millis(1),
    };

    assert_eq!(
        retry_state.wait_for_next_tick(&persist_rx),
        PersistWorkerWaitOutcome::Process,
        "timeout while the channel is still connected should trigger a retry tick"
    );
}

#[test]
fn persist_worker_retry_wait_accepts_new_delta_during_backoff() {
    let (persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();
    let retry_state = PersistWorkerRetryState {
        retry_after_failure: true,
        retry_delay: Duration::from_secs(30),
    };
    persist_tx
        .send(PersistRequest::Delta)
        .expect("test persist signal should send");

    assert_eq!(
        retry_state.wait_for_next_tick(&persist_rx),
        PersistWorkerWaitOutcome::Process,
        "new persist signals during backoff should wake the worker immediately"
    );
}

#[test]
fn persist_worker_retry_wait_observes_shutdown_during_backoff() {
    let (persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();
    drop(persist_tx);
    let retry_state = PersistWorkerRetryState {
        retry_after_failure: true,
        retry_delay: Duration::from_secs(30),
    };

    assert_eq!(
        retry_state.wait_for_next_tick(&persist_rx),
        PersistWorkerWaitOutcome::Exit,
        "disconnected retry wait should stop the worker instead of spinning"
    );
}

#[test]
fn persist_worker_wait_observes_explicit_shutdown_signal() {
    let (persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();
    persist_tx
        .send(PersistRequest::Shutdown)
        .expect("explicit shutdown signal should send");
    let retry_state = PersistWorkerRetryState::default();

    // The wait must distinguish a graceful shutdown signal from a
    // disconnected channel: shutdown still wants one final drain pass
    // (so the in-flight commit reaches SQLite) before the loop exits,
    // while disconnect aborts immediately. See `app_boot.rs`'s persist
    // loop for the corresponding `should_exit_after_tick` handling.
    assert_eq!(
        retry_state.wait_for_next_tick(&persist_rx),
        PersistWorkerWaitOutcome::Shutdown,
        "explicit shutdown signal must be reported as Shutdown, not Exit",
    );
}

#[test]
fn persist_worker_wait_observes_shutdown_during_retry_backoff() {
    let (persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();
    persist_tx
        .send(PersistRequest::Shutdown)
        .expect("explicit shutdown signal should send");
    let retry_state = PersistWorkerRetryState {
        retry_after_failure: true,
        retry_delay: Duration::from_secs(30),
    };

    assert_eq!(
        retry_state.wait_for_next_tick(&persist_rx),
        PersistWorkerWaitOutcome::Shutdown,
        "shutdown during a retry backoff should still drain one final tick before exit",
    );
}

#[test]
fn shutdown_persist_blocking_is_idempotent_when_no_worker_handle() {
    // Test-only constructors don't spawn the persist worker thread, so
    // `persist_thread_handle` stays `None`. Calling
    // `shutdown_persist_blocking` must be a safe no-op the first time
    // (no thread to join) and on every subsequent call (the handle is
    // already taken / still `None`). The production caller in main.rs
    // is one-shot, but `AppState` is `Clone`-able and the handle is
    // shared — making the operation idempotent prevents a future
    // shutdown ordering bug from panicking.
    let (state, persist_rx) = test_app_state_with_live_persist_channel();
    state.shutdown_persist_blocking();
    assert!(
        state
            .persist_thread_handle
            .lock()
            .expect("persist handle mutex poisoned")
            .is_none(),
        "no-worker shutdown should leave the join handle absent",
    );
    assert!(
        !state.persist_worker_alive.load(Ordering::Acquire),
        "no-worker shutdown should publish the stopped worker state",
    );
    assert!(
        matches!(persist_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "no-worker shutdown should not enqueue a shutdown request",
    );
    state.shutdown_persist_blocking();
    assert!(
        state
            .persist_thread_handle
            .lock()
            .expect("persist handle mutex poisoned")
            .is_none(),
        "second no-worker shutdown should remain idempotent",
    );
    assert!(
        !state.persist_worker_alive.load(Ordering::Acquire),
        "second no-worker shutdown should keep the worker stopped",
    );
    assert!(
        matches!(persist_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "second no-worker shutdown should not enqueue a shutdown request",
    );
}

#[test]
fn concurrent_shutdown_waits_for_join_owner_before_publishing_stopped() {
    // Regression for the bug ledger entry "Concurrent shutdown callers can
    // flip `persist_worker_alive` before the join owner finishes". The first
    // caller takes the worker handle and blocks in `join()`. A concurrent
    // caller must block behind that full transition instead of seeing `None`
    // and publishing `alive == false` while the worker thread is still alive.
    let (persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();
    let (shutdown_seen_tx, shutdown_seen_rx) = mpsc::channel::<()>();
    let (release_worker_tx, release_worker_rx) = mpsc::channel::<()>();

    let worker = std::thread::Builder::new()
        .name("test-concurrent-persist-shutdown".to_owned())
        .spawn(move || {
            while let Ok(req) = persist_rx.recv() {
                if matches!(req, PersistRequest::Shutdown) {
                    let _ = shutdown_seen_tx.send(());
                    release_worker_rx
                        .recv()
                        .expect("test should release the blocked worker");
                    break;
                }
            }
        })
        .expect("test persist worker should spawn");

    let (state, _stale_rx) = test_app_state_with_live_persist_channel();
    let state = AppState {
        persist_tx: persist_tx.clone(),
        persist_thread_handle: Arc::new(Mutex::new(Some(worker))),
        ..state
    };

    let first = state.clone();
    let first_joiner = std::thread::spawn(move || {
        first.shutdown_persist_blocking();
    });

    shutdown_seen_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("first shutdown caller should signal the worker");
    assert!(
        state.persist_worker_alive.load(Ordering::Acquire),
        "alive must stay true while the join owner is still waiting",
    );

    let second = state.clone();
    let (second_done_tx, second_done_rx) = mpsc::channel::<()>();
    let second_joiner = std::thread::spawn(move || {
        second.shutdown_persist_blocking();
        let _ = second_done_tx.send(());
    });

    assert!(
        second_done_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "concurrent shutdown caller should block until the join owner finishes",
    );
    assert!(
        state.persist_worker_alive.load(Ordering::Acquire),
        "blocked concurrent shutdown must not publish stopped early",
    );

    release_worker_tx
        .send(())
        .expect("test should release worker join");
    first_joiner
        .join()
        .expect("first shutdown caller should not panic");
    second_joiner
        .join()
        .expect("second shutdown caller should not panic");
    second_done_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("second shutdown caller should return after join owner finishes");
    assert!(
        !state.persist_worker_alive.load(Ordering::Acquire),
        "alive should flip only after the worker has exited",
    );
}

#[test]
fn shutdown_persist_blocking_persists_delta_committed_while_joining_worker() {
    // Regression for the bug ledger entry "Post-shutdown persistence writes
    // still leave a post-collection-pre-join window". A delta-only mutation
    // can land while shutdown is waiting for the worker thread to exit. The
    // worker may already be past its final collection, so shutdown itself must
    // perform a final synchronized full-state persist after `join()`.
    let (persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();
    let (shutdown_seen_tx, shutdown_seen_rx) = mpsc::channel::<()>();
    let (release_worker_tx, release_worker_rx) = mpsc::channel::<()>();

    let worker = std::thread::Builder::new()
        .name("test-joining-persist-worker".to_owned())
        .spawn(move || {
            while let Ok(req) = persist_rx.recv() {
                if matches!(req, PersistRequest::Shutdown) {
                    let _ = shutdown_seen_tx.send(());
                    release_worker_rx
                        .recv()
                        .expect("test should release the blocked worker");
                    break;
                }
            }
        })
        .expect("test persist worker should spawn");

    let (state, _stale_rx) = test_app_state_with_live_persist_channel();
    let persistence_path = Arc::clone(&state.persistence_path);
    let state = AppState {
        persist_tx: persist_tx.clone(),
        persist_thread_handle: Arc::new(Mutex::new(Some(worker))),
        ..state
    };

    let session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let project = inner.create_project(
            Some("Join Persist Project".to_owned()),
            "/tmp".to_owned(),
            default_local_remote_id(),
        );
        inner
            .create_session(
                Agent::Claude,
                Some("Join Persist Session".to_owned()),
                "/tmp".to_owned(),
                Some(project.id),
                None,
            )
            .session
            .id
    };

    let shutdown_state = state.clone();
    let shutdown_joiner = std::thread::spawn(move || {
        shutdown_state.shutdown_persist_blocking();
    });

    shutdown_seen_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("shutdown should reach the worker before the late mutation");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let session_index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        inner
            .session_mut_by_index(session_index)
            .expect("test session should be mutable")
            .session
            .preview = "delta while shutdown is joining".to_owned();
        state
            .commit_delta_locked(&mut inner)
            .expect("late delta commit should succeed");
    }

    release_worker_tx
        .send(())
        .expect("test should release worker join");
    shutdown_joiner
        .join()
        .expect("shutdown caller should not panic");

    let persisted = load_state(&persistence_path)
        .expect("shutdown should persist final state")
        .expect("persisted state should exist");
    let persisted_session = persisted
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("late-mutated session should be persisted");
    assert_eq!(
        persisted_session.session.preview, "delta while shutdown is joining",
        "shutdown's final synchronized persist must include delta-only mutations that land while \
         the worker is joining",
    );

    let _ = fs::remove_file(&*persistence_path);
}

#[tokio::test]
async fn shutdown_signal_wakes_a_subscriber_registered_before_the_signal() {
    // Standard ordering: subscribe first, trigger second. The subscriber's
    // first `borrow_and_update()` reads the initial `false`, then it awaits
    // `changed()` which fires when production calls `trigger_shutdown_signal`.
    // This is the load-bearing invariant for graceful shutdown — without
    // it `with_graceful_shutdown` blocks forever on long-lived SSE streams.
    let state = test_app_state();
    let mut shutdown_rx = state.subscribe_shutdown_signal();
    let waiter = tokio::spawn(async move {
        // Mirror the production helper in `api_sse.rs::wait_for_shutdown_signal`:
        // returns immediately on the sticky-true case, otherwise loops until
        // the value flips.
        if *shutdown_rx.borrow_and_update() {
            return;
        }
        while shutdown_rx.changed().await.is_ok() {
            if *shutdown_rx.borrow_and_update() {
                return;
            }
        }
    });

    // Yield once so the spawned task has a chance to enter `changed().await`.
    tokio::task::yield_now().await;
    state.trigger_shutdown_signal();

    tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
        .await
        .expect("shutdown waiter must complete within the timeout")
        .expect("shutdown waiter task should not panic");
}

#[tokio::test]
async fn shutdown_signal_wakes_a_subscriber_registered_after_the_signal() {
    // The race that motivated switching from `Notify` to `watch`: an
    // `/api/events` request that begins handler setup AFTER Ctrl+C has
    // already fired must still observe the shutdown signal — otherwise
    // its loop runs forever and graceful shutdown blocks.
    //
    // Critically, this test uses NO in-test re-notify: it triggers
    // shutdown first, subscribes second, and the waiter is expected to
    // exit purely on the sticky `true` value the subscriber sees during
    // its initial `borrow_and_update()` pre-check. A `Notify`-based
    // implementation would HANG here because `notify_waiters` only wakes
    // currently-waiting tasks, and the `tokio::time::timeout` below
    // would fire and fail the test. See bugs.md "One-shot SSE shutdown
    // notification can be missed before waiter registration".
    let state = test_app_state();
    state.trigger_shutdown_signal();

    let mut shutdown_rx = state.subscribe_shutdown_signal();
    let waiter = tokio::spawn(async move {
        if *shutdown_rx.borrow_and_update() {
            return;
        }
        while shutdown_rx.changed().await.is_ok() {
            if *shutdown_rx.borrow_and_update() {
                return;
            }
        }
    });

    tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
        .await
        .expect(
            "shutdown waiter must complete within the timeout — the watch \
             channel's sticky semantics require the late subscriber to see \
             the prior trigger immediately",
        )
        .expect("shutdown waiter task should not panic");
}

#[tokio::test]
async fn shutdown_signal_is_idempotent_and_durable() {
    // Repeated `trigger_shutdown_signal()` calls are safe (no-op after the
    // first). Subscribers registered at any time after the first trigger
    // see the sticky `true` value. This is what lets the production
    // graceful-shutdown future call `trigger_shutdown_signal()` exactly
    // once without coordinating with the unknown number of `/api/events`
    // streams that may be concurrently subscribing.
    let state = test_app_state();
    state.trigger_shutdown_signal();
    state.trigger_shutdown_signal();
    state.trigger_shutdown_signal();

    for _ in 0..3 {
        let mut rx = state.subscribe_shutdown_signal();
        assert!(
            *rx.borrow_and_update(),
            "every late subscriber must see the sticky shutdown value",
        );
    }
}

#[test]
fn shutdown_persist_blocking_drains_and_joins_a_real_worker() {
    // Spawn a worker thread that mirrors the production `app_boot.rs`
    // loop semantics. Sending a Delta enqueues work; sending Shutdown
    // signals the loop to perform one final drain pass and exit.
    // `shutdown_persist_blocking` must wait until the thread actually
    // exits, so a subsequent `handle.join()` would not race with an
    // in-flight SQLite commit. This is the contract that closes the
    // bugs.md "Server restart without browser refresh can lose the
    // last streamed message" durability window.
    let (persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();
    let drained_ticks = Arc::new(AtomicU64::new(0));
    let drained_ticks_for_thread = Arc::clone(&drained_ticks);

    let worker = std::thread::Builder::new()
        .name("test-persist-shutdown-loop".to_owned())
        .spawn(move || {
            let mut retry_state = PersistWorkerRetryState::default();
            loop {
                let outcome = retry_state.wait_for_next_tick(&persist_rx);
                if matches!(outcome, PersistWorkerWaitOutcome::Exit) {
                    break;
                }
                let mut should_exit_after_tick =
                    matches!(outcome, PersistWorkerWaitOutcome::Shutdown);
                while let Ok(req) = persist_rx.try_recv() {
                    if matches!(req, PersistRequest::Shutdown) {
                        should_exit_after_tick = true;
                    }
                }
                drained_ticks_for_thread.fetch_add(1, Ordering::SeqCst);
                retry_state.record_result(&Ok(()));
                if should_exit_after_tick {
                    break;
                }
            }
        })
        .expect("test persist worker should spawn");

    let (state, _stale_rx) = test_app_state_with_live_persist_channel();
    let state = AppState {
        persist_tx: persist_tx.clone(),
        persist_thread_handle: Arc::new(Mutex::new(Some(worker))),
        ..state
    };

    persist_tx
        .send(PersistRequest::Delta)
        .expect("delta enqueue should succeed");
    state.shutdown_persist_blocking();

    // The worker exited cleanly: the handle was taken (so a second
    // shutdown is a no-op) and the thread processed at least one tick
    // (the queued Delta + the Shutdown drain pass). On a hard kill
    // before this fix, the queued Delta would never have been
    // processed.
    assert!(
        drained_ticks.load(Ordering::SeqCst) >= 1,
        "worker should have drained at least the queued Delta before exit",
    );
    state.shutdown_persist_blocking();
}

#[test]
fn commit_delta_locked_after_shutdown_falls_back_to_synchronous_persist() {
    // Regression for the bug ledger entry "Persist shutdown drain can run
    // before background mutation sources are quiesced". The HTTP server's
    // graceful-shutdown phase only waits for in-flight HTTP handlers, but
    // background agent runtime threads, remote SSE bridges, and the
    // orchestrator transition resumer can still hold `AppState` clones
    // and call `commit_delta_locked` AFTER `shutdown_persist_blocking`
    // has drained and exited the worker. `commit_delta_locked` doesn't
    // send its own `PersistRequest::Delta`; under normal operation the
    // worker drains the bumped mutation_stamps on a subsequent persist
    // signal, but post-shutdown there is no worker. Without this
    // synchronous fallback, those final mutations are kept only in
    // memory and lost when the process exits.
    let unique_suffix = Uuid::new_v4();
    let project_root =
        std::env::temp_dir().join(format!("termal-post-shutdown-commit-root-{unique_suffix}"));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let state_root =
        std::env::temp_dir().join(format!("termal-post-shutdown-commit-state-{unique_suffix}"));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let persistence_path = state_root.join("termal.sqlite");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let durable_session_id;
    {
        let state = AppState::new_with_paths(
            project_root.to_string_lossy().into_owned(),
            persistence_path.clone(),
            orchestrator_templates_path.clone(),
        )
        .expect("initial state should boot");

        let project_id =
            create_test_project(&state, &project_root, "Post-Shutdown Commit Regression");
        durable_session_id =
            create_test_project_session(&state, Agent::Claude, &project_id, &project_root);

        // Run the production graceful-shutdown drain. After this returns
        // the worker is gone; subsequent commits cannot wake it.
        state.shutdown_persist_blocking();

        // Simulate a background mutation source (agent runtime / remote
        // bridge / orchestrator resumer) committing AFTER the persist
        // worker has exited. Bump the session's mutation_stamp via the
        // standard mutator path so the commit looks production-shaped,
        // then route through `commit_delta_locked` — exactly the pattern
        // a Claude/Codex stdio thread uses for streaming text chunks.
        // Without the post-shutdown synchronous fallback, the bumped
        // stamp would be queued for a worker that never returns, and
        // the in-memory mutation would be lost on the next reload.
        let post_shutdown_revision = {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let session_index = inner
                .find_session_index(&durable_session_id)
                .expect("session committed pre-shutdown should exist");
            // `session_mut_by_index` stamps the session's mutation_stamp
            // and is what real runtime threads would use.
            inner
                .session_mut_by_index(session_index)
                .expect("session_mut_by_index should return the existing session")
                .session
                .preview = "post-shutdown delta".to_owned();
            state
                .commit_delta_locked(&mut inner)
                .expect("commit_delta_locked must succeed even after persist shutdown")
        };
        assert!(
            post_shutdown_revision >= 1,
            "commit_delta_locked must continue to bump the revision after shutdown",
        );
    }

    // Reload from the same path and verify the post-shutdown delta is
    // durable. The synchronous fallback writes the full state, so the
    // session's preview should be the post-shutdown value.
    let restarted = AppState::new_with_paths(
        project_root.to_string_lossy().into_owned(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("restarted state should boot from the persisted file");

    let reloaded_inner = restarted.inner.lock().expect("state mutex poisoned");
    let session_index = reloaded_inner
        .find_session_index(&durable_session_id)
        .expect("session must reload");
    let preview = reloaded_inner.sessions[session_index]
        .session
        .preview
        .clone();
    drop(reloaded_inner);
    restarted.shutdown_persist_blocking();

    assert_eq!(
        preview, "post-shutdown delta",
        "the mutation that landed after `shutdown_persist_blocking` must reach disk via the \
         synchronous fallback path; otherwise the bug ledger entry \"Persist shutdown drain \
         can run before background mutation sources are quiesced\" remains open",
    );

    let _ = fs::remove_dir_all(&state_root);
    let _ = fs::remove_dir_all(&project_root);
}

#[test]
fn graceful_shutdown_drain_persists_final_mutation_across_reload() {
    // End-to-end durability regression for the bug ledger entry "Server
    // restart without browser refresh can lose the last streamed message"
    // and the follow-up gap "Graceful-shutdown durability regression does
    // not reload persisted state". This test exercises the REAL
    // production-shaped path: `AppState::new_with_paths` spawns the actual
    // background persist thread, `commit_locked` triggers a real
    // `PersistRequest::Delta` signal, `shutdown_persist_blocking` runs the
    // production drain-and-join, and a fresh `AppState::new_with_paths`
    // against the same persistence path verifies the mutation survived.
    //
    // Without this test, the prior unit-level coverage of `wait_for_next_tick`
    // and the fake-loop integration test could pass even if the real worker
    // failed to write the final delta or a restarted `AppState` silently
    // dropped the last record.
    let unique_suffix = Uuid::new_v4();
    let project_root =
        std::env::temp_dir().join(format!("termal-graceful-shutdown-root-{unique_suffix}"));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let state_root =
        std::env::temp_dir().join(format!("termal-graceful-shutdown-state-{unique_suffix}"));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let persistence_path = state_root.join("termal.sqlite");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let durable_session_ids;
    {
        let state = AppState::new_with_paths(
            project_root.to_string_lossy().into_owned(),
            persistence_path.clone(),
            orchestrator_templates_path.clone(),
        )
        .expect("initial state should boot");

        // Commit a burst through the production path. Each `commit_locked`
        // bumps the revision and signals `PersistRequest::Delta` to the
        // background worker, which now uses the same SQLite delta path in
        // tests and production. A burst keeps this test sensitive to the
        // shutdown drain ordering instead of relying on a single mutation
        // that the worker might happen to flush before shutdown begins.
        let project = create_test_project(&state, &project_root, "Graceful Shutdown Durability");
        durable_session_ids = (0..50)
            .map(|_| create_test_project_session(&state, Agent::Claude, &project, &project_root))
            .collect::<Vec<_>>();

        // The commit's `Delta` signal may or may not have been processed
        // by the worker before this point — that's the durability window
        // the graceful drain closes. `shutdown_persist_blocking` sends
        // `PersistRequest::Shutdown` and joins; the worker's loop drains
        // every queued Delta + the Shutdown signal, runs one final tick
        // that captures the whole burst, and only then exits. After
        // the join returns, SQLite on disk MUST contain the sessions.
        state.shutdown_persist_blocking();
    }

    // Reload from the same path. The reborn `AppState` must observe the
    // sessions that the prior process committed just before shutdown.
    let restarted = AppState::new_with_paths(
        project_root.to_string_lossy().into_owned(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("restarted state should boot from the persisted file");

    let reloaded_inner = restarted.inner.lock().expect("state mutex poisoned");
    let missing_session_ids = durable_session_ids
        .iter()
        .filter(|session_id| reloaded_inner.find_session_index(session_id).is_none())
        .collect::<Vec<_>>();
    assert!(
        missing_session_ids.is_empty(),
        "every session committed just before graceful shutdown must be reloadable from the \
         persistence file — without `shutdown_persist_blocking`'s final drain, the \
         `PersistRequest::Delta` queued by `commit_locked` would be lost when the worker \
         exited and the next process boot would never see the full burst; missing: {missing_session_ids:?}",
    );
    drop(reloaded_inner);
    restarted.shutdown_persist_blocking();

    // Best-effort cleanup; tests that fail mid-flight intentionally leave
    // the temp files in place for postmortem inspection.
    let _ = fs::remove_dir_all(&state_root);
    let _ = fs::remove_dir_all(&project_root);
}

#[test]
fn persist_delta_restore_requeues_only_drained_explicit_tombstones() {
    let mut inner = StateInner::new();
    let removed_id = inner
        .create_session(
            Agent::Claude,
            Some("removed".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let hidden_id = inner
        .create_session(
            Agent::Claude,
            Some("hidden".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let hidden_index = inner
        .find_session_index(&hidden_id)
        .expect("hidden session should exist");
    inner
        .session_mut_by_index(hidden_index)
        .expect("hidden session should be mutable")
        .hidden = true;
    let removed_index = inner
        .find_session_index(&removed_id)
        .expect("removed session should exist");
    inner.remove_session_at(removed_index);

    let delta = inner.collect_persist_delta(0);

    assert_eq!(delta.drained_explicit_tombstones, vec![removed_id.clone()]);
    assert_eq!(
        delta
            .removed_session_ids
            .iter()
            .filter(|id| id.as_str() == removed_id.as_str())
            .count(),
        1
    );
    assert_eq!(
        delta
            .removed_session_ids
            .iter()
            .filter(|id| id.as_str() == hidden_id.as_str())
            .count(),
        1
    );
    assert!(inner.removed_session_ids.is_empty());

    inner.restore_drained_explicit_tombstones(&delta.drained_explicit_tombstones);
    inner.restore_drained_explicit_tombstones(&delta.drained_explicit_tombstones);
    assert_eq!(inner.removed_session_ids, vec![removed_id.clone()]);

    let retry_delta = inner.collect_persist_delta(0);

    assert_eq!(
        retry_delta.drained_explicit_tombstones,
        vec![removed_id.clone()]
    );
    assert_eq!(
        retry_delta
            .removed_session_ids
            .iter()
            .filter(|id| id.as_str() == hidden_id.as_str())
            .count(),
        1,
        "hidden-session deletes should be regenerated, not restored as explicit tombstones"
    );
}

fn sqlite_row_json(path: &FsPath, table: &str, id: &str) -> Option<String> {
    let connection = rusqlite::Connection::open(path).expect("sqlite state should open");
    let sql = format!("SELECT value_json FROM {table} WHERE id = ?1");
    match connection.query_row(&sql, rusqlite::params![id], |row| row.get(0)) {
        Ok(value) => Some(value),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(err) => panic!("sqlite row query should succeed: {err}"),
    }
}

fn sqlite_table_ids(path: &FsPath, table: &str) -> Vec<String> {
    let connection = rusqlite::Connection::open(path).expect("sqlite state should open");
    let sql = format!("SELECT id FROM {table} ORDER BY id");
    let mut statement = connection
        .prepare(&sql)
        .expect("sqlite id query should prepare");
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("sqlite id query should run");
    rows.map(|row| row.expect("sqlite id row should read"))
        .collect()
}

fn make_persist_test_delegation(
    id: &str,
    parent_session_id: &str,
    child_session_id: &str,
) -> DelegationRecord {
    DelegationRecord {
        id: id.to_owned(),
        parent_session_id: parent_session_id.to_owned(),
        child_session_id: child_session_id.to_owned(),
        mode: DelegationMode::Reviewer,
        status: DelegationStatus::Running,
        title: "Persisted Delegation".to_owned(),
        prompt: "/review-local".to_owned(),
        cwd: "/tmp".to_owned(),
        agent: Agent::Codex,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: Some(stamp_now()),
        completed_at: None,
        result: None,
    }
}

#[test]
fn sqlite_persist_connection_cache_reuses_matching_connection_until_invalidated() {
    let state_root =
        std::env::temp_dir().join(format!("termal-sqlite-cache-reuse-{}", Uuid::new_v4()));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let path = state_root.join("termal.sqlite");
    let mut cache = SqlitePersistConnectionCache::new();

    {
        let connection = cache
            .connection_for(&path)
            .expect("cached sqlite connection should open");
        connection
            .execute("CREATE TEMP TABLE cache_probe(value TEXT)", [])
            .expect("connection-local temp table should be created");
        connection
            .execute("INSERT INTO cache_probe(value) VALUES('reused')", [])
            .expect("connection-local temp row should be inserted");
    }
    {
        let connection = cache
            .connection_for(&path)
            .expect("matching path should reuse cached sqlite connection");
        let value: String = connection
            .query_row("SELECT value FROM cache_probe", [], |row| row.get(0))
            .expect("connection-local temp table should survive cache reuse");
        assert_eq!(value, "reused");
    }

    cache.invalidate();

    {
        let connection = cache
            .connection_for(&path)
            .expect("invalidated cache should reopen sqlite connection");
        let error = connection
            .query_row("SELECT value FROM cache_probe", [], |row| {
                row.get::<_, String>(0)
            })
            .expect_err("fresh connection should not see the prior temp table");
        assert!(
            error.to_string().contains("no such table"),
            "unexpected cache invalidation error: {error}"
        );
    }

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn sqlite_delta_upserts_only_changed_session_rows_and_removes_hidden_or_deleted_rows() {
    let state_root = std::env::temp_dir().join(format!("termal-sqlite-delta-{}", Uuid::new_v4()));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let path = state_root.join("termal.sqlite");
    let mut inner = StateInner::new();
    let changed_id = inner
        .create_session(
            Agent::Claude,
            Some("Changed".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let unchanged_id = inner
        .create_session(
            Agent::Claude,
            Some("Unchanged".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let hidden_id = inner
        .create_session(
            Agent::Claude,
            Some("Hidden".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let deleted_id = inner
        .create_session(
            Agent::Claude,
            Some("Deleted".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    persist_state(&path, &inner).expect("initial sqlite state should persist");
    let unchanged_before =
        sqlite_row_json(&path, "sessions", &unchanged_id).expect("unchanged row should exist");
    let watermark = inner.last_mutation_stamp;

    let changed_index = inner
        .find_session_index(&changed_id)
        .expect("changed session should exist");
    inner
        .session_mut_by_index(changed_index)
        .expect("changed session should be mutable")
        .session
        .preview = "Targeted changed preview".to_owned();
    let hidden_index = inner
        .find_session_index(&hidden_id)
        .expect("hidden session should exist");
    inner
        .session_mut_by_index(hidden_index)
        .expect("hidden session should be mutable")
        .hidden = true;
    let deleted_index = inner
        .find_session_index(&deleted_id)
        .expect("deleted session should exist");
    inner.remove_session_at(deleted_index);

    let delta = inner.collect_persist_delta(watermark);
    assert_eq!(delta.changed_sessions.len(), 1);
    assert_eq!(delta.removed_session_ids.len(), 2);
    let mut cache = SqlitePersistConnectionCache::new();
    persist_delta_via_cache(&mut cache, &path, &delta).expect("delta should persist");

    assert_eq!(
        sqlite_table_ids(&path, "sessions"),
        vec![changed_id.clone(), unchanged_id.clone()]
    );
    let changed_row =
        sqlite_row_json(&path, "sessions", &changed_id).expect("changed row should remain");
    let changed_value: Value =
        serde_json::from_str(&changed_row).expect("changed row should decode as json");
    assert_eq!(
        changed_value["session"]["preview"],
        Value::String("Targeted changed preview".to_owned())
    );
    assert_eq!(
        sqlite_row_json(&path, "sessions", &unchanged_id),
        Some(unchanged_before),
        "unchanged session row should not be rewritten by a targeted delta"
    );
    assert!(sqlite_row_json(&path, "sessions", &hidden_id).is_none());
    assert!(sqlite_row_json(&path, "sessions", &deleted_id).is_none());

    let loaded = load_state(&path)
        .expect("sqlite state should load")
        .expect("sqlite state should exist");
    assert!(loaded.find_session_index(&changed_id).is_some());
    assert!(loaded.find_session_index(&unchanged_id).is_some());
    assert!(loaded.find_session_index(&hidden_id).is_none());
    assert!(loaded.find_session_index(&deleted_id).is_none());

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn sqlite_delta_upserts_changed_delegation_rows_and_removes_deleted_rows() {
    let state_root =
        std::env::temp_dir().join(format!("termal-sqlite-delegation-delta-{}", Uuid::new_v4()));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let path = state_root.join("termal.sqlite");
    let mut inner = StateInner::new();
    let parent_id = inner
        .create_session(
            Agent::Codex,
            Some("Parent".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let child_id = inner
        .create_session(
            Agent::Codex,
            Some("Child".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let changed_id = "delegation-delta-changed";
    let unchanged_id = "delegation-delta-unchanged";
    let deleted_id = "delegation-delta-deleted";
    inner.delegations.push(make_persist_test_delegation(
        changed_id, &parent_id, &child_id,
    ));
    inner.delegations.push(make_persist_test_delegation(
        unchanged_id,
        &parent_id,
        &child_id,
    ));
    inner.delegations.push(make_persist_test_delegation(
        deleted_id, &parent_id, &child_id,
    ));
    persist_state(&path, &inner).expect("initial sqlite state should persist");
    let unchanged_before =
        sqlite_row_json(&path, "delegations", unchanged_id).expect("unchanged row should exist");
    let watermark = inner.last_mutation_stamp;

    let changed_index = inner
        .find_delegation_index(changed_id)
        .expect("changed delegation should exist");
    inner.delegations[changed_index].title = "Changed Delegation Row".to_owned();
    inner.mark_delegation_mutated(changed_index);
    let deleted_index = inner
        .find_delegation_index(deleted_id)
        .expect("deleted delegation should exist");
    inner.remove_delegation_at(deleted_index);

    let delta = inner.collect_persist_delta(watermark);
    assert_eq!(
        delta
            .changed_delegations
            .as_ref()
            .expect("changed delegation should be persisted")
            .iter()
            .map(|delegation| delegation.id.as_str())
            .collect::<Vec<_>>(),
        vec![changed_id]
    );
    assert_eq!(delta.removed_delegation_ids, vec![deleted_id.to_owned()]);
    let mut cache = SqlitePersistConnectionCache::new();
    persist_delta_via_cache(&mut cache, &path, &delta).expect("delegation delta should persist");

    assert_eq!(
        sqlite_table_ids(&path, "delegations"),
        vec![changed_id.to_owned(), unchanged_id.to_owned()]
    );
    let changed_row =
        sqlite_row_json(&path, "delegations", changed_id).expect("changed row should remain");
    let changed_value: Value =
        serde_json::from_str(&changed_row).expect("changed row should decode as json");
    assert_eq!(
        changed_value["title"],
        Value::String("Changed Delegation Row".to_owned())
    );
    assert_eq!(
        sqlite_row_json(&path, "delegations", unchanged_id),
        Some(unchanged_before),
        "unchanged delegation row should not be rewritten by a targeted delta"
    );
    assert!(sqlite_row_json(&path, "delegations", deleted_id).is_none());

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn sqlite_delta_metadata_only_update_does_not_rewrite_session_rows() {
    let state_root =
        std::env::temp_dir().join(format!("termal-sqlite-metadata-only-{}", Uuid::new_v4()));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let path = state_root.join("termal.sqlite");
    let mut inner = StateInner::new();
    let session_id = inner
        .create_session(
            Agent::Claude,
            Some("Session".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    persist_state(&path, &inner).expect("initial sqlite state should persist");
    let session_before =
        sqlite_row_json(&path, "sessions", &session_id).expect("session row should exist");
    let watermark = inner.last_mutation_stamp;

    let project = inner.create_project(
        Some("Metadata Project".to_owned()),
        "/tmp/metadata-project".to_owned(),
        default_local_remote_id(),
    );
    let delta = inner.collect_persist_delta(watermark);
    assert!(delta.changed_sessions.is_empty());
    assert!(delta.removed_session_ids.is_empty());

    let mut cache = SqlitePersistConnectionCache::new();
    persist_delta_via_cache(&mut cache, &path, &delta).expect("metadata-only delta should persist");

    assert_eq!(
        sqlite_row_json(&path, "sessions", &session_id),
        Some(session_before),
        "metadata-only persist should leave session rows untouched"
    );
    let metadata = sqlite_metadata_state_value(&path);
    assert!(
        metadata["projects"]
            .as_array()
            .expect("projects should be encoded")
            .iter()
            .any(|value| value["id"] == Value::String(project.id.clone())),
        "metadata row should contain the newly-created project"
    );

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn sqlite_startup_loads_sessions_and_delegations_from_split_tables() {
    let state_root =
        std::env::temp_dir().join(format!("termal-sqlite-split-load-{}", Uuid::new_v4()));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let path = state_root.join("termal.sqlite");
    let mut inner = StateInner::new();
    let parent_id = inner
        .create_session(
            Agent::Codex,
            Some("Parent".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let child_id = inner
        .create_session(
            Agent::Codex,
            Some("Child".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let delegation = make_persist_test_delegation("delegation-split", &parent_id, &child_id);
    inner.delegations.push(delegation.clone());
    persist_state(&path, &inner).expect("split sqlite state should persist");

    let metadata = sqlite_metadata_state_value(&path);
    assert_eq!(metadata["sessions"], Value::Array(Vec::new()));
    assert!(
        metadata.get("delegations").is_none(),
        "delegations should be stored in the dedicated table, not embedded metadata"
    );

    let loaded = load_state(&path)
        .expect("sqlite state should load")
        .expect("sqlite state should exist");
    assert!(loaded.find_session_index(&parent_id).is_some());
    assert!(loaded.find_session_index(&child_id).is_some());
    assert_eq!(loaded.delegations.len(), 1);
    let loaded_delegation = &loaded.delegations[0];
    assert_eq!(loaded_delegation.id, delegation.id);
    assert_eq!(
        loaded_delegation.parent_session_id,
        delegation.parent_session_id
    );
    assert_eq!(
        loaded_delegation.child_session_id,
        delegation.child_session_id
    );
    assert_eq!(loaded_delegation.mode, delegation.mode);
    assert_eq!(loaded_delegation.title, delegation.title);
    assert_eq!(loaded_delegation.prompt, delegation.prompt);
    assert_eq!(loaded_delegation.cwd, delegation.cwd);
    assert_eq!(loaded_delegation.agent, delegation.agent);
    assert_eq!(loaded_delegation.write_policy, delegation.write_policy);
    assert_eq!(loaded_delegation.created_at, delegation.created_at);
    assert_eq!(loaded_delegation.started_at, delegation.started_at);
    assert_eq!(
        loaded_delegation.status,
        DelegationStatus::Failed,
        "startup load should finalize in-flight delegations without result packets"
    );
    assert_eq!(
        loaded_delegation
            .result
            .as_ref()
            .map(|result| result.summary.as_str()),
        Some("child finished without a result packet")
    );

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn sqlite_legacy_embedded_delegations_are_requeued_for_table_migration() {
    let state_root = std::env::temp_dir().join(format!(
        "termal-sqlite-legacy-delegations-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let path = state_root.join("termal.sqlite");
    let mut inner = StateInner::new();
    let parent_id = inner
        .create_session(
            Agent::Codex,
            Some("Parent".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let child_id = inner
        .create_session(
            Agent::Codex,
            Some("Child".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let delegation = make_persist_test_delegation("delegation-legacy", &parent_id, &child_id);
    inner.delegations.push(delegation.clone());
    let legacy_embedded_state = PersistedState::from_inner(&inner);
    persist_state_parts_to_sqlite(&path, &legacy_embedded_state, &[], true, &[], true)
        .expect("legacy sqlite metadata should persist with an empty delegation table");

    let mut loaded = load_state(&path)
        .expect("legacy sqlite state should load")
        .expect("legacy sqlite state should exist");
    assert!(
        loaded
            .delegations
            .iter()
            .any(|record| record.id == delegation.id),
        "embedded legacy delegations should survive when the dedicated table is empty"
    );

    let delta = loaded.collect_persist_delta(0);
    let changed_delegations = delta
        .changed_delegations
        .as_ref()
        .expect("legacy embedded delegations should be requeued for table migration");
    assert!(
        changed_delegations
            .iter()
            .any(|record| record.id == delegation.id)
    );
    let mut cache = SqlitePersistConnectionCache::new();
    persist_delta_via_cache(&mut cache, &path, &delta)
        .expect("legacy delegation migration delta should persist");
    assert_eq!(
        sqlite_table_ids(&path, "delegations"),
        vec![delegation.id.clone()]
    );

    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn sqlite_load_reports_malformed_metadata_session_and_delegation_rows() {
    let state_root =
        std::env::temp_dir().join(format!("termal-sqlite-malformed-{}", Uuid::new_v4()));
    fs::create_dir_all(&state_root).expect("state root should exist");
    let session_row_path = state_root.join("session-row.sqlite");
    let delegation_row_path = state_root.join("delegation-row.sqlite");
    let metadata_path = state_root.join("metadata.sqlite");

    let mut inner = StateInner::new();
    let parent_id = inner
        .create_session(
            Agent::Claude,
            Some("Parent".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let child_id = inner
        .create_session(
            Agent::Claude,
            Some("Child".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let session_id = inner
        .create_session(
            Agent::Claude,
            Some("Session".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id;
    let delegation = make_persist_test_delegation("delegation-malformed", &parent_id, &child_id);
    inner.delegations.push(delegation.clone());
    persist_state(&session_row_path, &inner).expect("session-row state should persist");
    persist_state(&delegation_row_path, &inner).expect("delegation-row state should persist");
    persist_state(&metadata_path, &inner).expect("metadata state should persist");

    {
        let connection =
            rusqlite::Connection::open(&session_row_path).expect("sqlite state should open");
        connection
            .execute(
                "UPDATE sessions SET value_json = '{ not json' WHERE id = ?1",
                rusqlite::params![session_id],
            )
            .expect("session row should be corrupted");
    }
    let session_error = match load_state(&session_row_path) {
        Ok(_) => panic!("malformed session row should fail startup load"),
        Err(error) => error,
    };
    assert!(
        format!("{session_error:#}").contains("failed to parse persisted session row"),
        "{session_error:#}"
    );

    {
        let connection =
            rusqlite::Connection::open(&delegation_row_path).expect("sqlite state should open");
        connection
            .execute(
                "UPDATE delegations SET value_json = '{ not json' WHERE id = ?1",
                rusqlite::params![delegation.id],
            )
            .expect("delegation row should be corrupted");
    }
    let delegation_error = match load_state(&delegation_row_path) {
        Ok(_) => panic!("malformed delegation row should fail startup load"),
        Err(error) => error,
    };
    assert!(
        format!("{delegation_error:#}").contains("failed to parse persisted delegation row"),
        "{delegation_error:#}"
    );

    {
        let connection =
            rusqlite::Connection::open(&metadata_path).expect("sqlite state should open");
        connection
            .execute(
                "UPDATE app_state SET value_json = '{ not json' WHERE key = 'metadataState'",
                [],
            )
            .expect("metadata row should be corrupted");
    }
    let metadata_error = match load_state(&metadata_path) {
        Ok(_) => panic!("malformed app_state row should fail startup load"),
        Err(error) => error,
    };
    assert!(
        format!("{metadata_error:#}").contains("failed to parse persisted state"),
        "{metadata_error:#}"
    );

    let _ = fs::remove_dir_all(state_root);
}

// Regression guard: `commit_session_created_locked` must route
// the persist work through the background channel rather than
// calling `persist_created_session` synchronously under the state
// mutex. Previously the mutex was held across a full SQLite
// transaction (connection open, schema-ensure, metadata + session
// upsert, commit with fsync) — every concurrent request that
// called `self.inner.lock()` blocked behind that I/O for 10-100 ms
// on slow disks.
//
// The sibling `persist_internal_locked` path has used the
// background channel since it was introduced; this test pins that
// the session-creation path shares the same contract. The
// crash-before-persist window is acceptable because a freshly-
// created `SessionRecord` has no user content (empty
// `messages: []`, no agent output) — see the commit message for
// the trade-off analysis.
#[test]
fn commit_session_created_locked_signals_background_persist_instead_of_blocking() {
    let (state, persist_rx) = test_app_state_with_live_persist_channel();
    let persistence_path = state.persistence_path.as_ref().to_path_buf();
    // Session record built under the same lock the caller would
    // hold, matching the real call site in `session_crud.rs`.
    let (revision, record_id) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Claude,
            Some("persist-channel-signal test".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        );
        // `create_session` already called `push_session` internally
        // (see `state_inner.rs`), so the record is in `inner.sessions`
        // at a known id — re-stamp it via `session_mut_by_index` to
        // mirror the real caller in `session_crud.rs`, which
        // overwrites the pushed slot with agent-specific field
        // defaults and then re-stamps so `collect_persist_delta`
        // picks up the rewrite on the next persist tick.
        let record_id = record.session.id.clone();
        let index = inner
            .find_session_index(&record_id)
            .expect("create_session should have pushed the record");
        let _ = inner.session_mut_by_index(index);
        let revision = state
            .commit_session_created_locked(&mut inner, &record)
            .expect("commit_session_created_locked should succeed");
        (revision, record_id)
    };

    // Primary assertion: the background channel received a `Delta`
    // wake. Reverting the fix (restoring the synchronous
    // `persist_created_session` call on every invocation) makes
    // the channel `try_recv` return `Err(Empty)` and this
    // assertion fails.
    let received = persist_rx
        .try_recv()
        .expect("commit_session_created_locked should have sent PersistRequest::Delta");
    // `PersistRequest` is a single-variant enum today; `matches!`
    // with an exhaustive pattern makes the assertion structural
    // (a future variant addition forces the reviewer to update
    // the test, which is the desired signal).
    assert!(matches!(received, PersistRequest::Delta));

    // Negative assertion: no synchronous persist happened. Under
    // the `#[cfg(test)]` build, the fallback path writes via
    // `persist_state_from_persisted` to the JSON `persistence_path`.
    // A synchronous fallback would create that file; the async
    // path never touches it.
    assert!(
        !persistence_path.exists(),
        "persistence path should not exist — fallback persist ran unexpectedly"
    );

    // Sanity: the revision did advance (the pre-persist increment
    // is unchanged by the fix).
    assert_eq!(revision, 1);
    // Fixture sanity — the record id should follow the
    // `session-<n>` shape `StateInner::create_session` mints.
    // Phrased as a `starts_with` so a future change to
    // `StateInner::new()`'s `next_session_number` seed (or a move
    // to UUID-shaped ids) doesn't false-fail this assertion.
    assert!(
        record_id.starts_with("session-"),
        "record id should follow `session-<n>` shape, got: {record_id}"
    );

    // Defensive cleanup: the negative assertion above already
    // proves the fallback persist did not run, but in a regression
    // where it did, clean up so the rogue file doesn't linger in
    // the shared temp dir. `remove_file` on a non-existent path
    // returns an error we ignore.
    let _ = fs::remove_file(&persistence_path);
}
