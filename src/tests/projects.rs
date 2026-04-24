// projects in termal are workdir folders that group related agent sessions,
// optionally backed by a remote ssh host via a pre-registered `RemoteConfig`.
// sessions inherit their workdir from the project root, and the backend
// enforces that a session's workdir stays inside the project tree to prevent
// path-traversal escapes from the agent sandbox. remote projects proxy their
// sessions over ssh; local projects resolve against the host filesystem.
//
// the central guards are `resolve_project_scoped_requested_path` and
// `resolve_session_scoped_requested_path` (in `src/paths.rs`): both validate
// the project/session id and enforce containment before any fs operation runs.
// these tests cover the production surfaces `create_project`, `delete_project`,
// `create_session` (remote + local), `resolve_requested_path`, and the
// project-scoped path resolver — pinning the sandbox boundary end to end.

use super::*;

// pins the remote_id validation path in `create_project`: a project that
// references a remote not registered in `app_settings.remotes` must be
// rejected with BAD_REQUEST before any session plumbing is attempted.
// guards against silently accepting projects bound to phantom remotes.
#[test]
fn rejects_projects_with_unknown_remote() {
    let state = test_app_state();

    let error = match state.create_project(CreateProjectRequest {
        name: Some("Remote Project".to_owned()),
        root_path: "/tmp".to_owned(),
        remote_id: "missing-remote".to_owned(),
    }) {
        Ok(_) => panic!("project creation should reject unknown remotes"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("unknown remote"));
}

// pins the remote-session creation path: sessions created inside an ssh-backed
// project must be proxied through the `RemoteTransport::Ssh` plumbing and
// surface BAD_GATEWAY / BAD_REQUEST with a non-empty message when the remote
// is unreachable. guards against silently falling back to local execution.
#[test]
#[ignore = "requires a reachable SSH remote"]
fn creates_sessions_for_remote_projects_over_ssh() {
    let state = test_app_state();

    state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_reasoning_effort: None,
            default_claude_approval_mode: None,
            default_claude_effort: None,
            remotes: Some(vec![
                RemoteConfig::local(),
                RemoteConfig {
                    id: "ssh-lab".to_owned(),
                    name: "SSH Lab".to_owned(),
                    transport: RemoteTransport::Ssh,
                    enabled: true,
                    host: Some("example.com".to_owned()),
                    port: Some(22),
                    user: Some("alice".to_owned()),
                },
            ]),
        })
        .unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Remote Project".to_owned()),
            root_path: "/workspace/demo".to_owned(),
            remote_id: "ssh-lab".to_owned(),
        })
        .unwrap();

    let stored_project = project
        .state
        .projects
        .iter()
        .find(|entry| entry.id == project.project_id)
        .expect("created project should be present");
    assert_eq!(stored_project.remote_id, "ssh-lab");

    let error = match state.create_session(CreateSessionRequest {
        agent: Some(Agent::Codex),
        name: Some("Remote Session".to_owned()),
        workdir: None,
        project_id: Some(project.project_id),
        model: None,
        approval_policy: None,
        reasoning_effort: None,
        sandbox_mode: None,
        cursor_mode: None,
        claude_approval_mode: None,
        claude_effort: None,
        gemini_approval_mode: None,
    }) {
        Ok(_) => {
            panic!(
                "remote session creation should require a reachable SSH remote in this integration test"
            )
        }
        Err(error) => error,
    };

    assert!(matches!(
        error.status,
        StatusCode::BAD_GATEWAY | StatusCode::BAD_REQUEST
    ));
    assert!(!error.message.trim().is_empty());
}

// pins the happy-path link between `create_project` and `create_session`:
// a session created with `project_id` set and `workdir: None` must inherit
// the project's resolved root as its workdir and record its project_id.
// guards against broken project-session membership or workdir defaulting.
#[test]
fn creates_projects_and_assigns_sessions_to_them() {
    let state = test_app_state();
    let expected_root = resolve_project_root_path("/tmp").unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: None,
            root_path: "/tmp".to_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    assert_eq!(project.state.projects.len(), 1);

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Project Session".to_owned()),
            workdir: None,
            project_id: Some(project.project_id.clone()),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let session = &created.session;

    assert_eq!(
        session.project_id.as_deref(),
        Some(project.project_id.as_str())
    );
    assert_eq!(session.workdir, expected_root);
}

// pins the `delete_project` cascade: removing a project must drop it from
// `state.projects` while leaving its sessions and orchestrators visible with
// their project_id cleared (None / ""), not cascade-deleted along with it.
// guards against accidental data loss when a user deletes a project folder.
#[test]
fn deletes_projects_and_unassigns_existing_sessions() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-delete-project-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Delete Project".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Project Session".to_owned()),
            workdir: None,
            project_id: Some(project.project_id.clone()),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .unwrap()
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project.project_id.clone()),
            template: None,
        })
        .unwrap()
        .orchestrator;

    let deleted = state.delete_project(&project.project_id).unwrap();

    assert!(
        deleted
            .projects
            .iter()
            .all(|entry| entry.id != project.project_id)
    );
    let session = deleted
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("created session should remain visible");
    assert_eq!(session.project_id, None);
    let deleted_orchestrator = deleted
        .orchestrators
        .iter()
        .find(|instance| instance.id == orchestrator.id)
        .expect("created orchestrator should remain visible");
    assert_eq!(deleted_orchestrator.project_id, "");

    fs::remove_dir_all(root).unwrap();
}

// pins the session-workdir containment check in `create_session`: an explicit
// workdir outside the project root must fail with BAD_REQUEST and the
// "must stay inside project" message, never silently widening the sandbox.
// guards against path-traversal via the session-creation request.
#[test]
fn rejects_session_workdirs_outside_the_selected_project() {
    let state = test_app_state();
    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Project".to_owned()),
            root_path: "/tmp".to_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();

    let result = state.create_session(CreateSessionRequest {
        agent: Some(Agent::Codex),
        name: Some("Out of Bounds".to_owned()),
        workdir: Some("/Users".to_owned()),
        project_id: Some(project.project_id),
        model: None,
        approval_policy: None,
        reasoning_effort: None,
        sandbox_mode: None,
        cursor_mode: None,
        claude_approval_mode: None,
        claude_effort: None,
        gemini_approval_mode: None,
    });

    let error = match result {
        Ok(_) => panic!("session workdir outside project should fail"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("must stay inside project"));
}

// pins the input-validation guard in `create_project`: a whitespace-only
// `root_path` must be rejected with the exact error "project root path
// cannot be empty", not coerced to cwd or the host root.
// guards against accidentally creating projects that cover the whole disk.
#[test]
fn rejects_empty_project_roots() {
    let state = test_app_state();

    let result = state.create_project(CreateProjectRequest {
        name: None,
        root_path: "   ".to_owned(),
        remote_id: default_local_remote_id(),
    });
    let error = match result {
        Ok(_) => panic!("empty project path should fail"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, "project root path cannot be empty");
}

// pins `resolve_session_scoped_requested_path` in `ExistingFile` mode: a file
// under the session's project root must canonicalize and normalize, while a
// sibling outside the root must fail with BAD_REQUEST + "must stay inside
// project". guards against read-side path-traversal via session-scoped apis.
#[test]
fn resolves_requested_paths_inside_the_session_project_root() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-project-scope-{}", Uuid::new_v4()));
    let inside_dir = root.join("src");
    let inside_file = inside_dir.join("main.rs");
    let outside_root =
        std::env::temp_dir().join(format!("termal-project-scope-outside-{}", Uuid::new_v4()));
    let outside_file = outside_root.join("main.rs");

    fs::create_dir_all(&inside_dir).unwrap();
    fs::create_dir_all(&outside_root).unwrap();
    fs::write(&inside_file, "fn main() {}\n").unwrap();
    fs::write(&outside_file, "fn main() {}\n").unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Scoped Project".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Scoped Session".to_owned()),
            workdir: Some(inside_dir.to_string_lossy().into_owned()),
            project_id: Some(project.project_id),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let resolved = resolve_session_scoped_requested_path(
        &state,
        &created.session_id,
        &inside_file.to_string_lossy(),
        ScopedPathMode::ExistingFile,
    )
    .unwrap();
    assert_eq!(
        resolved,
        normalize_user_facing_path(&fs::canonicalize(&inside_file).unwrap())
    );

    let error = resolve_session_scoped_requested_path(
        &state,
        &created.session_id,
        &outside_file.to_string_lossy(),
        ScopedPathMode::ExistingFile,
    )
    .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("must stay inside project"));

    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(outside_root).unwrap();
}

// pins `resolve_session_scoped_requested_path` in `AllowMissingLeaf` mode: a
// yet-to-exist file inside the project root must resolve to its target path
// (for write/create), while a missing path outside the root still fails.
// guards against write-side path-traversal when creating new files.
#[test]
fn allows_new_file_paths_inside_the_session_project_root() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-project-write-scope-{}", Uuid::new_v4()));
    let inside_dir = root.join("src");
    let new_file = root.join("generated").join("output.rs");
    let outside_root =
        std::env::temp_dir().join(format!("termal-project-write-outside-{}", Uuid::new_v4()));
    let outside_file = outside_root.join("escape.rs");

    fs::create_dir_all(&inside_dir).unwrap();
    fs::create_dir_all(&outside_root).unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Writable Project".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Writable Session".to_owned()),
            workdir: Some(inside_dir.to_string_lossy().into_owned()),
            project_id: Some(project.project_id),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let resolved = resolve_session_scoped_requested_path(
        &state,
        &created.session_id,
        &new_file.to_string_lossy(),
        ScopedPathMode::AllowMissingLeaf,
    )
    .unwrap();
    assert_eq!(resolved, new_file);

    let error = resolve_session_scoped_requested_path(
        &state,
        &created.session_id,
        &outside_file.to_string_lossy(),
        ScopedPathMode::AllowMissingLeaf,
    )
    .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("must stay inside project"));

    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(outside_root).unwrap();
}

// pins `resolve_project_scoped_requested_path` when only `project_id` is
// supplied (no session): paths inside the project root resolve, paths outside
// fail with "must stay inside project". guards the project-only entry point
// used by pre-session ui flows (project browser, pre-create file picker).
#[test]
fn resolves_project_scoped_paths_without_a_session() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-project-scope-only-{}", Uuid::new_v4()));
    let inside_dir = root.join("src");
    let inside_file = inside_dir.join("main.rs");
    let outside_root = std::env::temp_dir().join(format!(
        "termal-project-scope-only-outside-{}",
        Uuid::new_v4()
    ));
    let outside_file = outside_root.join("escape.rs");

    fs::create_dir_all(&inside_dir).unwrap();
    fs::create_dir_all(&outside_root).unwrap();
    fs::write(&inside_file, "fn main() {}\n").unwrap();
    fs::write(&outside_file, "fn main() {}\n").unwrap();

    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Scope Only Project".to_owned()),
            root_path: root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .unwrap();

    let resolved = resolve_project_scoped_requested_path(
        &state,
        None,
        Some(&project.project_id),
        &inside_file.to_string_lossy(),
        ScopedPathMode::ExistingFile,
    )
    .unwrap();
    assert_eq!(
        resolved,
        normalize_user_facing_path(&fs::canonicalize(&inside_file).unwrap())
    );

    let error = resolve_project_scoped_requested_path(
        &state,
        None,
        Some(&project.project_id),
        &outside_file.to_string_lossy(),
        ScopedPathMode::ExistingFile,
    )
    .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("must stay inside project"));

    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(outside_root).unwrap();
}

// pins the required-identifier precondition in
// `resolve_project_scoped_requested_path`: calling with both session_id and
// project_id as None must fail with the exact "sessionId or projectId is
// required" error. guards against ungated path resolution via the public api.
#[test]
fn project_scoped_paths_require_a_session_or_project_identifier() {
    let state = test_app_state();
    let error = resolve_project_scoped_requested_path(
        &state,
        None,
        None,
        "/tmp",
        ScopedPathMode::ExistingPath,
    )
    .unwrap_err();

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, "sessionId or projectId is required");
}
