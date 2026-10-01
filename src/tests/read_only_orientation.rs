// Owns the test that the host's orientation read for a read-only delegation
// child carries no context generation, while a writable session's keeps it.
// With a generation the tracker tells the reader to list memories under it,
// a call a read-only child's gate refuses. Does not own the orientation's
// delivery and compaction lifecycle (tests/engram_compaction.rs) nor the
// read-only gate itself (read_only_claude_permission_decision in claude.rs,
// tested in tests/claude_permission_boundary.rs). New file.
use super::engram_host_adapter::real_engram_control_fixture_path;
use super::*;
use std::path::Path;

fn tracker_project(state: &AppState) -> (String, PathBuf) {
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root")
        .path()
        .join("read-only-orientation");
    fs::create_dir_all(&root).expect("create project");
    fs::write(root.join(".engram-project"), "fixture-ready\n").expect("declaration");
    let project_id = create_test_project(state, &root, "Read-only orientation");
    let mut inner = state.inner.lock().expect("state mutex");
    inner
        .projects
        .iter_mut()
        .find(|project| project.id == project_id)
        .expect("project")
        .engram = Some(EngramProjectSettings {
        acceptance_evaluation: None,
        enabled: true,
        turn_gated_control: false,
        binary_path: Some(
            real_engram_control_fixture_path()
                .to_string_lossy()
                .into_owned(),
        ),
        home: Some(root.to_string_lossy().into_owned()),
        work_authority_grant: None,
        authority_store_key: None,
        deadline_ms: Some(250),
    });
    drop(inner);
    (project_id, root)
}

/// A running delegation child of `parent` under `write_policy`, configured
/// as delegation creation configures it.
fn delegation_child(
    state: &AppState,
    parent: &str,
    project_id: &str,
    root: &Path,
    write_policy: DelegationWritePolicy,
) -> String {
    let mut inner = state.inner.lock().expect("state mutex");
    let delegation_id = inner.next_delegation_id();
    let child = inner.create_session(
        Agent::Claude,
        Some("Delegated reviewer".to_owned()),
        root.to_string_lossy().into_owned(),
        Some(project_id.to_owned()),
        None,
    );
    let child_session_id = child.session.id.clone();
    let index = inner.find_session_index(&child_session_id).unwrap();
    inner.sessions[index].session.parent_delegation_id = Some(delegation_id.clone());
    configure_delegation_child_prompt_settings(
        &mut inner.sessions[index],
        DelegationMode::Reviewer,
        &write_policy,
    );
    inner.delegations.push(DelegationRecord {
        id: delegation_id,
        parent_session_id: parent.to_owned(),
        child_session_id: child_session_id.clone(),
        mode: DelegationMode::Reviewer,
        status: DelegationStatus::Running,
        title: "Delegated reviewer".to_owned(),
        prompt: "Review the change.".to_owned(),
        cwd: root.to_string_lossy().into_owned(),
        agent: Agent::Claude,
        model: None,
        write_policy,
        created_at: stamp_now(),
        started_at: Some(stamp_now()),
        completed_at: None,
        result: None,
        submitted_review_result: None,
        post_submission_transport_error: None,
        review_result_recovery_probe_attempt: None,
        review_result_recovery_error: None,
        review_result_schema_version: None,
        queued_followup_prompt_id: None,
        review_result_submission_attempt: 0,
        acceptance_evaluation: None,
        attempt: DelegationAttemptState::default(),
    });
    inner.rebuild_running_read_only_delegations();
    state.commit_locked(&mut inner).unwrap();
    child_session_id
}

/// Prepares `session_id`'s orientation through the real fixture and returns
/// the arguments the tracker was started with.
fn orientation_args(state: &AppState, root: &Path, session_id: &str) -> Vec<String> {
    let args_file = root.join(if cfg!(windows) {
        "work-context-args.json"
    } else {
        "work-context-args.txt"
    });
    let _ = fs::remove_file(&args_file);
    {
        let mut inner = state.inner.lock().expect("state mutex");
        let index = inner.find_session_index(session_id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.engram.context_nudge_pending = true;
        record.engram.pending_context_nudge = None;
    }
    assert_eq!(
        state.prepare_engram_context_nudge_off_lock(session_id),
        EngramContextNudgePreparation::Ready
    );
    let text = fs::read_to_string(&args_file).expect("the fixture records its arguments");
    if cfg!(windows) {
        serde_json::from_str(text.trim_start_matches('\u{feff}')).unwrap()
    } else {
        text.split_whitespace().map(str::to_owned).collect()
    }
}

#[test]
fn a_read_only_childs_orientation_read_carries_no_context_generation() {
    let state = test_app_state();
    let (project_id, root) = tracker_project(&state);
    let parent = create_test_project_session(&state, Agent::Claude, &project_id, &root);
    let read_only = delegation_child(
        &state,
        &parent,
        &project_id,
        &root,
        DelegationWritePolicy::ReadOnly,
    );

    let child_args = orientation_args(&state, &root, &read_only);
    assert!(
        child_args.iter().any(|arg| arg == "--peek"),
        "{child_args:?}"
    );
    assert!(
        !child_args.iter().any(|arg| arg == "--context-generation"),
        "a read-only child's orientation must carry no generation: {child_args:?}"
    );
    assert!(
        !child_args.iter().any(|arg| arg.starts_with("termal-")),
        "{child_args:?}"
    );

    // A follow-up turn to a finished read-only delegation is dispatched before
    // the delegation is marked running again; it is still read-only.
    {
        let mut inner = state.inner.lock().expect("state mutex");
        let delegation = inner
            .delegations
            .iter_mut()
            .find(|delegation| delegation.child_session_id == read_only)
            .expect("delegation");
        delegation.status = DelegationStatus::Completed;
        delegation.completed_at = Some(stamp_now());
        inner.rebuild_running_read_only_delegations();
    }
    let finished_args = orientation_args(&state, &root, &read_only);
    assert!(
        !finished_args
            .iter()
            .any(|arg| arg == "--context-generation"),
        "a finished read-only delegation's follow-up carries no generation: {finished_args:?}"
    );

    // A writable session of the same project keeps the generation.
    let parent_args = orientation_args(&state, &root, &parent);
    assert!(
        parent_args.iter().any(|arg| arg == "--peek"),
        "{parent_args:?}"
    );
    assert!(
        parent_args
            .windows(2)
            .any(|pair| pair[0] == "--context-generation" && pair[1].starts_with("termal-")),
        "a writable session's orientation keeps its generation: {parent_args:?}"
    );
}
