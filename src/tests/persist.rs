//! `PersistedState` load/serialize round-trip tests.
//!
//! `PersistedState` is the on-disk schema for TermAl's entire backend
//! state: preferences, projects, remotes, sessions, orchestrators, and
//! workspace layouts. It is persisted as a JSON file (legacy format)
//! and/or via the SQLite-backed store in `src/persist.rs`, which owns
//! both encoding paths and the `load_state` entry point exercised here.
//!
//! Schema validation on load is deliberately strict: any missing
//! required field produces an error rather than a silent default, so a
//! corrupted or partially-migrated file cannot resurrect sessions with
//! quietly-broken state. Path normalization additionally folds Windows
//! `\\?\` extended-length prefixes and legacy backslash forms to a
//! canonical form, so a file saved on one machine reloads cleanly on
//! another.
//!
//! The `persisted_state_load_error_after_mutation` helper takes a
//! mutation closure, writes the mutated state to disk, reloads it, and
//! returns the error string — each required-field test stays focused on
//! a single missing-field assertion.

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

// Pins that `AppState::new_with_paths` normalizes a legacy `\\?\`
// verbatim workdir passed in at bootstrap, so the default project and
// the bootstrapped Codex/Claude live sessions all share the canonical
// root. Guards against bootstrap paths diverging from persisted ones.
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

#[test]
fn persisted_state_omits_runtime_session_mutation_stamp_on_save() {
    let path =
        std::env::temp_dir().join(format!("termal-runtime-mutation-stamp-{}", Uuid::new_v4()));
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

    persist_state(&path, &inner).expect("persisted state should be written");

    let encoded: Value = serde_json::from_slice(&fs::read(&path).unwrap())
        .expect("persisted state should deserialize");
    {
        let persisted_session = encoded["sessions"][0]["session"]
            .as_object()
            .expect("persisted session should be an object");
        assert!(
            !persisted_session.contains_key("sessionMutationStamp"),
            "runtime mutation stamps must not be serialized into persisted sessions"
        );
    }

    let _ = fs::remove_file(path);
}

#[test]
fn persisted_state_clears_runtime_session_mutation_stamp_on_load() {
    let path = std::env::temp_dir().join(format!(
        "termal-runtime-mutation-stamp-load-{}",
        Uuid::new_v4()
    ));
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Claude,
        Some("Stamped".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    persist_state(&path, &inner).expect("persisted state should be written");

    let mut encoded: Value = serde_json::from_slice(&fs::read(&path).unwrap())
        .expect("persisted state should deserialize");
    encoded["sessions"][0]["session"]
        .as_object_mut()
        .expect("persisted session should be an object")
        .insert("sessionMutationStamp".to_owned(), Value::from(99));
    fs::write(&path, serde_json::to_vec(&encoded).unwrap()).expect("persisted state should update");

    let loaded = load_state(&path)
        .expect("persisted state should load")
        .expect("persisted state should exist");
    assert_eq!(loaded.sessions[0].session.session_mutation_stamp, None);

    let _ = fs::remove_file(path);
}

#[test]
fn persisted_state_round_trips_conversation_markers() {
    let path = std::env::temp_dir().join(format!("termal-conversation-markers-{}", Uuid::new_v4()));
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

    persist_state(&path, &inner).expect("persisted state should be written");

    let encoded: Value = serde_json::from_slice(&fs::read(&path).unwrap())
        .expect("persisted state should deserialize");
    assert_eq!(
        encoded["sessions"][0]["session"]["markers"][0]["name"],
        Value::String("Use the overview rail".to_owned())
    );

    let loaded = load_state(&path)
        .expect("persisted state should load")
        .expect("persisted state should exist");
    let markers = &loaded.sessions[0].session.markers;
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].id, "marker-1");
    assert_eq!(markers[0].session_id, session_id);
    assert_eq!(markers[0].kind, ConversationMarkerKind::Decision);
    assert_eq!(markers[0].created_by, ConversationMarkerAuthor::User);
    assert_eq!(markers[0].end_message_id.as_deref(), Some("message-3"));

    let _ = fs::remove_file(path);
}

#[test]
fn persisted_state_defaults_missing_conversation_markers() {
    let path = std::env::temp_dir().join(format!(
        "termal-conversation-markers-missing-{}",
        Uuid::new_v4()
    ));
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Claude,
        Some("No Markers".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    persist_state(&path, &inner).expect("persisted state should be written");

    let mut encoded: Value = serde_json::from_slice(&fs::read(&path).unwrap())
        .expect("persisted state should deserialize");
    encoded["sessions"][0]["session"]
        .as_object_mut()
        .expect("persisted session should be an object")
        .remove("markers");
    fs::write(&path, serde_json::to_vec(&encoded).unwrap()).expect("persisted state should update");

    let loaded = load_state(&path)
        .expect("persisted state should load")
        .expect("persisted state should exist");
    assert!(loaded.sessions[0].session.markers.is_empty());

    let _ = fs::remove_file(path);
}

#[test]
fn persisted_state_maps_unknown_conversation_marker_kind_to_custom() {
    let path = std::env::temp_dir().join(format!(
        "termal-conversation-marker-kind-{}",
        Uuid::new_v4()
    ));
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
    persist_state(&path, &inner).expect("persisted state should be written");

    let mut encoded: Value = serde_json::from_slice(&fs::read(&path).unwrap())
        .expect("persisted state should deserialize");
    encoded["sessions"][0]["session"]["markers"][0]["kind"] =
        Value::String("obsoleteKind".to_owned());
    fs::write(&path, serde_json::to_vec(&encoded).unwrap()).expect("persisted state should update");

    let loaded = load_state(&path)
        .expect("persisted state should load")
        .expect("persisted state should exist");
    assert_eq!(
        loaded.sessions[0].session.markers[0].kind,
        ConversationMarkerKind::Custom
    );

    let _ = fs::remove_file(path);
}

#[test]
fn persisted_state_rejects_oversized_legacy_json_before_reading() {
    let path =
        std::env::temp_dir().join(format!("termal-oversized-legacy-state-{}", Uuid::new_v4()));
    fs::write(&path, b"{}").expect("oversized state fixture should be written");

    let err = match read_json_persisted_state_with_limit(&path, 1) {
        Ok(_) => panic!("oversized persisted state should fail to load"),
        Err(err) => err,
    };
    let err_text = format!("{err:#}");
    assert!(
        err_text.contains("is too large"),
        "unexpected load_state error: {err_text}"
    );
    assert!(
        err_text.contains("max 1 bytes"),
        "error should include the byte cap: {err_text}"
    );
    assert!(
        err_text.contains(LEGACY_JSON_STATE_MAX_BYTES_ENV),
        "error should name the import-size override: {err_text}"
    );

    let _ = fs::remove_file(path);
}

#[test]
fn max_legacy_json_state_bytes_pin() {
    assert_eq!(MAX_LEGACY_JSON_STATE_BYTES, 100 * 1024 * 1024);
}

#[test]
fn legacy_json_state_max_bytes_override_parses_positive_byte_count() {
    assert_eq!(
        parse_legacy_json_state_max_bytes_override("209715200").expect("override should parse"),
        200 * 1024 * 1024
    );
}

#[test]
fn legacy_json_state_max_bytes_override_rejects_zero() {
    let err = parse_legacy_json_state_max_bytes_override("0")
        .expect_err("zero-byte override should fail");
    let err_text = format!("{err:#}");
    assert!(
        err_text.contains("must be greater than 0"),
        "unexpected override error: {err_text}"
    );
}

#[test]
fn legacy_json_state_max_bytes_override_rejects_invalid_inputs() {
    for raw in ["abc", "100MB", "", "   ", "-1"] {
        let err = parse_legacy_json_state_max_bytes_override(raw)
            .expect_err("invalid override should fail");
        let err_text = format!("{err:#}");
        assert!(
            err_text.contains("must be a positive integer byte count"),
            "unexpected override error for {raw:?}: {err_text}"
        );
    }
}

// Builds an `AppState` like `test_app_state` but with a LIVE
// persist channel receiver so the caller can observe
// `PersistRequest` signals. The default `test_app_state` drops
// the receiver on construction so every `persist_tx.send(...)`
// returns `Err(Disconnected)` and tests automatically take the
// synchronous fallback path — which is good for JSON round-trip
// tests but hides whether a code path correctly routes async.
fn test_app_state_with_live_persist_channel() -> (AppState, mpsc::Receiver<PersistRequest>) {
    let (persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();
    let persistence_path =
        std::env::temp_dir().join(format!("termal-test-{}.json", Uuid::new_v4()));
    let state = AppState {
        server_instance_id: Uuid::new_v4().to_string(),
        default_workdir: "/tmp".to_owned(),
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
        state_broadcast_tx: mpsc::channel().0,
        shared_codex_runtime: Arc::new(Mutex::new(None)),
        test_acp_runtime_overrides: Arc::new(Mutex::new(Vec::new())),
        test_agent_setup_failures: Arc::new(Mutex::new(Vec::new())),
        agent_readiness_cache: Arc::new(RwLock::new(fresh_agent_readiness_cache("/tmp"))),
        agent_readiness_refresh_lock: Arc::new(Mutex::new(())),
        remote_registry: test_remote_registry(),
        remote_sse_fallback_resynced_revision: Arc::new(Mutex::new(HashMap::new())),
        remote_delta_replay_cache: Arc::new(Mutex::new(RemoteDeltaReplayCache::default())),
        remote_delta_hydrations_in_flight: Arc::new(Mutex::new(HashSet::new())),
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
    let persistence_path = state_root.join("sessions.json");
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
    let persistence_path = state_root.join("sessions.json");
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
        // background worker, which the test-build worker drains and writes
        // via `persist_state_from_persisted` (the JSON fallback that runs
        // under `#[cfg(test)]`). A burst keeps this test sensitive to the
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
        // the join returns, the JSON file on disk MUST contain the
        // sessions.
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
