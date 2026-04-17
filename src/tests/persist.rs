//! `PersistedState` load/serialize round-trip tests — legacy path
//! normalization, required-field validation (projects, cursor_mode,
//! Claude settings, Gemini approval mode, Codex prompt fields, queued
//! prompt source), and app-state bootstrap normalization.
//!
//! Extracted from `tests.rs` so each domain lives in its own sibling
//! module under `tests/`. Covers the `persisted_state_*` tests and
//! the `persisted_state_load_error_after_mutation` shared helper.
//!
//! Review-document persistence tests (`persist_review_document_*`,
//! `resolve_review_document_path_*`) remain in `mod.rs` for a
//! follow-up because they interleave with HTTP review route tests.

use super::*;


fn persisted_state_load_error_after_mutation<F>(inner: StateInner, mutate: F) -> String
where
    F: FnOnce(&mut Value),
{
    let path =
        std::env::temp_dir().join(format!("termal-state-load-error-{}.json", Uuid::new_v4()));
    persist_state(&path, &inner).expect("persisted state should be written");

    let mut encoded: Value = serde_json::from_slice(&fs::read(&path).unwrap())
        .expect("persisted state should deserialize");
    mutate(&mut encoded);
    fs::write(&path, serde_json::to_vec(&encoded).unwrap()).expect("persisted state should update");

    let err = match load_state(&path) {
        Ok(_) => panic!("mutated persisted state should fail to load"),
        Err(err) => err,
    };
    let _ = fs::remove_file(path);
    format!("{err:#}")
}

// Tests that persisted state normalizes legacy local verbatim paths.
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
    let path = std::env::temp_dir().join(format!(
        "termal-legacy-verbatim-state-{}.json",
        Uuid::new_v4()
    ));

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

    let mut encoded: Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).expect("state should deserialize");
    encoded["projects"][0]["rootPath"] = Value::String(legacy_root.clone());
    encoded["sessions"][0]["session"]["workdir"] = Value::String(legacy_root);
    fs::write(&path, serde_json::to_vec(&encoded).unwrap()).expect("persisted state should update");

    let loaded = load_state(&path)
        .expect("persisted state should load")
        .expect("persisted state should exist");
    assert_eq!(loaded.projects[0].root_path, normalized_root);
    assert_eq!(loaded.sessions[0].session.workdir, normalized_root);

    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(project_root);
}

// Tests that persisted state normalizes legacy workspace layout paths.
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
    let path = std::env::temp_dir().join(format!(
        "termal-layout-verbatim-state-{}.json",
        Uuid::new_v4()
    ));

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
    persist_state(&path, &inner).expect("persisted state should be written");

    let loaded = load_state(&path)
        .expect("persisted state should load")
        .expect("persisted state should exist");
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

    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(project_root);
}

// Tests that app state bootstrap normalizes legacy local verbatim working directory.
#[test]
fn app_state_bootstrap_normalizes_legacy_local_verbatim_workdir() {
    let project_root =
        std::env::temp_dir().join(format!("termal-bootstrap-verbatim-path-{}", Uuid::new_v4()));
    let state_root =
        std::env::temp_dir().join(format!("termal-bootstrap-state-root-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&state_root).expect("state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let legacy_root = format!(r"\\?\{normalized_root}");
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let state = AppState::new_with_paths(
        legacy_root,
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");

    assert_eq!(state.default_workdir, normalized_root);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.projects.len(), 1);
    assert_eq!(inner.projects[0].root_path, normalized_root);
    let bootstrapped_sessions = inner
        .sessions
        .iter()
        .filter(|record| matches!(record.session.name.as_str(), "Codex Live" | "Claude Live"))
        .collect::<Vec<_>>();
    assert_eq!(bootstrapped_sessions.len(), 2);
    assert!(
        bootstrapped_sessions
            .iter()
            .all(|record| record.session.workdir == normalized_root)
    );
    drop(inner);
    drop(state);

    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
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
    let path = std::env::temp_dir().join(format!(
        "termal-significant-path-space-state-{}.json",
        Uuid::new_v4()
    ));

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
    let _ = fs::remove_dir_all(project_root);
}

// Tests that persisted state requires projects.
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

// Tests that persisted state requires next project number.
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

// Tests that persisted state requires project remote ID.
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

// Tests that persisted state requires valid remotes.
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

// Tests that persisted state requires cursor mode.
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

// Tests that persisted state requires Claude settings.
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

// Tests that persisted state requires Gemini approval mode.
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

// Tests that persisted state requires Codex prompt fields.
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

// Tests that persisted state requires Codex thread state for live threads.
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

// Tests that persisted state requires queued prompt source.
#[test]
fn persisted_state_requires_queued_prompt_source() {
    let path = std::env::temp_dir().join(format!(
        "termal-queued-prompt-source-required-{}",
        Uuid::new_v4()
    ));
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
    persist_state(&path, &inner).unwrap();

    let mut encoded: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
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
    fs::write(&path, serde_json::to_vec(&encoded).unwrap()).unwrap();

    let err = match load_state(&path) {
        Ok(_) => panic!("persisted state without queued prompt source should fail"),
        Err(err) => err,
    };
    let err_text = format!("{err:#}");
    assert!(
        err_text.contains("missing field `source`"),
        "unexpected load_state error: {err_text}"
    );

    let _ = fs::remove_file(path);
}
