//! Boot-recovery target selection for delegation children (tm-cr0i): a child
//! of a completed delegation that holds no begun grant is skipped and keeps
//! its stale token for the ordinary rebind path of a later follow-up, while
//! roots, running, failed and canceled children, and completed children with a
//! begun grant keep the existing control-state recovery.
//!
//! Owns the completed-child selection test and its recording transport. Does
//! not own the worker-concurrency, budget or grant-checkpoint recovery tests,
//! which stay in `src/tests/engram_host_adapter.rs`. New module beside that
//! file, created instead of growing it.

use super::super::delegation_support::{
    install_delegation_codex_runtime, temp_delegation_state_paths,
};
use super::*;

/// Answers every control request generically and records which session asked
/// for which operation, so target selection can be asserted without ordering
/// assumptions across concurrent recovery workers.
struct RecordingBootRecoveryTransport {
    requests: Mutex<Vec<(String, String)>>,
}

impl RecordingBootRecoveryTransport {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
        })
    }

    fn operations_for(&self, session_id: &str) -> Vec<String> {
        self.requests
            .lock()
            .expect("recording transport mutex poisoned")
            .iter()
            .filter(|(requester, _)| requester == session_id)
            .map(|(_, operation)| operation.clone())
            .collect()
    }
}

impl EngramControlTransport for RecordingBootRecoveryTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        _timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        let request =
            serde_json::to_value(request).expect("boot recovery request should serialize");
        let operation = request["operation"].as_str().unwrap_or_default().to_owned();
        self.requests
            .lock()
            .expect("recording transport mutex poisoned")
            .push((connection.session_id.clone(), operation.clone()));
        match operation.as_str() {
            "session_status" => Ok(json!({ "phase": "ready" })),
            "session_bind" => Ok(json!({
                "routing_token": format!("recovered-{}", connection.session_id),
                "status": { "phase": "sync_required" }
            })),
            other => Err(EngramTransportError::protocol(format!(
                "unexpected boot recovery operation: {other}"
            ))),
        }
    }

    fn shutdown_session(&self, _session_id: &str) {}
}

fn sorted(ids: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut ids = ids.into_iter().collect::<Vec<_>>();
    ids.sort();
    ids
}

#[test]
fn boot_recovery_skips_completed_delegation_children_without_a_begun_grant() {
    let (_temp_root, project_root, persistence_path, templates_path) =
        temp_delegation_state_paths();
    let root = project_root.join("engram-completed-child-boot-recovery-project");
    fs::create_dir_all(&root).expect("project root should exist");
    fs::write(root.join(".engram-project"), "fixture-ready")
        .expect("Engram project marker should exist");
    let home = root.join("engram-home");
    fs::create_dir_all(&home).expect("Engram home should exist");
    let state = AppState::new_with_paths(
        project_root.to_string_lossy().into_owned(),
        persistence_path.clone(),
        templates_path.clone(),
    )
    .expect("state should boot");
    install_delegation_codex_runtime(&state, "engram-completed-child-boot-recovery");
    let project_id = create_test_project(&state, &root, "Engram completed-child boot recovery");
    // A parent runs only a bounded number of delegations at once, so the
    // children spread over as many parents as the bound requires. Every
    // parent is an ordinary root with a stale token.
    const CHILDREN: usize = 5;
    let parents = (0..CHILDREN.div_ceil(MAX_RUNNING_DELEGATIONS_PER_PARENT))
        .map(|_| create_test_project_session(&state, Agent::Codex, &project_id, &root))
        .collect::<Vec<_>>();
    let children = (0..CHILDREN)
        .map(|index| {
            state
                .create_read_only_delegation(
                    &parents[index / MAX_RUNNING_DELEGATIONS_PER_PARENT],
                    CreateDelegationRequest {
                        prompt: format!("Create recovery target {index}."),
                        title: Some(format!("Engram recovery target {index}")),
                        cwd: None,
                        agent: Some(Agent::Codex),
                        model: None,
                        mode: Some(DelegationMode::Reviewer),
                        write_policy: Some(DelegationWritePolicy::ReadOnly),
                    },
                )
                .expect("Engram-off delegation should start")
                .delegation
                .child_session_id
        })
        .collect::<Vec<_>>();
    let [completed, failed, canceled, completed_with_grant, running] =
        <[String; CHILDREN]>::try_from(children).expect("five delegation children");
    let terminal_statuses = [
        (&completed, DelegationStatus::Completed),
        (&failed, DelegationStatus::Failed),
        (&canceled, DelegationStatus::Canceled),
        (&completed_with_grant, DelegationStatus::Completed),
    ];
    let settings = EngramProjectSettings {
        acceptance_evaluation: None,
        enabled: true,
        turn_gated_control: true,
        binary_path: Some(root.join("engram-fixture").to_string_lossy().into_owned()),
        home: Some(home.to_string_lossy().into_owned()),
        work_authority_grant: None,
        authority_store_key: None,
        deadline_ms: Some(250),
    };
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .expect("project should exist")
            .engram = Some(settings);
        // Stamp every edit so the delta persister writes it: the second boot
        // below must reload these terminal statuses, not the Running rows.
        for (child, status) in terminal_statuses {
            let index = inner
                .delegations
                .iter()
                .position(|delegation| &delegation.child_session_id == child)
                .expect("delegation row should exist");
            inner.delegations[index].status = status;
            inner
                .mark_delegation_mutated(index)
                .expect("delegation index should be valid");
        }
        // Every session was bound before the restart, as a persisted token
        // re-marks it on reload.
        for index in 0..inner.sessions.len() {
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            record.engram.routing_token = Some(format!("stale-{}", record.session.id));
            record.engram.rebind_required = true;
        }
        let index = inner
            .find_session_index(&completed_with_grant)
            .expect("child with a begun grant should exist");
        inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .engram
            .active_grant_id = Some("begun-before-restart".to_owned());
        state
            .commit_locked(&mut inner)
            .expect("recovery setup should persist");
    }
    let transport = RecordingBootRecoveryTransport::new();
    state.install_control_test_transport(transport.clone());

    let plan = state
        .prepare_engram_sessions_for_boot_recovery()
        .expect("boot recovery plan should be prepared");
    let planned = sorted(
        plan.targets
            .iter()
            .map(|target| target.connection.session_id.clone()),
    );
    let kept = parents
        .iter()
        .chain([&failed, &canceled, &completed_with_grant, &running])
        .collect::<Vec<_>>();
    assert_eq!(
        planned,
        sorted(kept.iter().map(|id| (*id).clone())),
        "roots, running, failed and canceled children and a completed child with a begun grant stay boot targets"
    );
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner
            .find_session_index(&completed)
            .expect("skipped child should exist")];
        assert_eq!(
            record.engram.routing_token.as_deref(),
            Some(format!("stale-{completed}").as_str()),
            "a skipped completed child keeps its stale token for a later follow-up"
        );
        assert!(record.engram.rebind_required, "{completed}");
        assert!(
            !record.engram_boot_recovery_pending,
            "{completed}: a skipped completed child is not readiness-fenced"
        );
        for session_id in &kept {
            let record = &inner.sessions[inner
                .find_session_index(session_id)
                .expect("kept target should exist")];
            assert_eq!(
                record.engram.routing_token.as_deref(),
                Some(format!("stale-{session_id}").as_str()),
                "{session_id}: a boot target keeps its token until recovery rebinds"
            );
            assert!(record.engram.rebind_required, "{session_id}");
            assert!(record.engram_boot_recovery_pending, "{session_id}");
        }
    }

    state.recover_prepared_engram_sessions_after_boot(plan);

    assert_eq!(
        transport.operations_for(&completed),
        Vec::<String>::new(),
        "{completed}: a skipped completed child costs no control request"
    );
    for session_id in &kept {
        assert_eq!(
            transport.operations_for(session_id),
            ["session_status", "session_bind"],
            "{session_id}: a boot target runs status plus fresh bind"
        );
    }
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner
            .find_session_index(&completed_with_grant)
            .expect("child with a begun grant should exist")];
        assert_eq!(
            record.engram.active_grant_id, None,
            "a clean control-plane status clears the stale grant of a completed child"
        );
        assert_eq!(
            record.engram.routing_token.as_deref(),
            Some(format!("recovered-{completed_with_grant}").as_str())
        );
    }
    // Drain the background persist worker so the reload reads the recovered
    // tokens and the cleared grant, not a snapshot from before them.
    state.shutdown_persist_blocking();
    drop(state);

    // The rule applies afresh on every boot from persisted state: the next
    // boot's own recovery (which the test constructor runs inline before
    // returning) still skips the completed child, now also the one whose
    // grant the first boot cleared, keeps both their tokens, and recovers the
    // roots and the running, failed and canceled children again.
    let second_boot = RecordingBootRecoveryTransport::new();
    let reloaded = AppState::new_with_paths_and_engram_transport_for_test(
        project_root.to_string_lossy().into_owned(),
        persistence_path.clone(),
        templates_path.clone(),
        second_boot.clone(),
    )
    .expect("state should boot again");
    let skipped_after_reload = [
        (&completed, format!("stale-{completed}")),
        (
            &completed_with_grant,
            format!("recovered-{completed_with_grant}"),
        ),
    ];
    let kept_after_reload = parents
        .iter()
        .chain([&failed, &canceled, &running])
        .collect::<Vec<_>>();
    {
        let inner = reloaded.inner.lock().expect("state mutex poisoned");
        for (child, status) in terminal_statuses {
            let delegation = inner
                .delegations
                .iter()
                .find(|delegation| &delegation.child_session_id == child)
                .expect("delegation row should reload");
            assert_eq!(
                delegation.status, status,
                "{child}: the terminal status must have been persisted"
            );
        }
        for (session_id, token) in &skipped_after_reload {
            let record = &inner.sessions[inner
                .find_session_index(session_id)
                .expect("skipped child should reload")];
            assert_eq!(
                record.engram.routing_token.as_deref(),
                Some(token.as_str()),
                "{session_id}: the skipped child keeps the token it had"
            );
            assert!(record.engram.rebind_required, "{session_id}");
            assert_eq!(record.engram.active_grant_id, None, "{session_id}");
            assert!(!record.engram_boot_recovery_pending, "{session_id}");
        }
    }
    for (session_id, _) in &skipped_after_reload {
        assert_eq!(
            second_boot.operations_for(session_id),
            Vec::<String>::new(),
            "{session_id}: the second boot leaves a skipped completed child alone"
        );
    }
    for session_id in &kept_after_reload {
        assert_eq!(
            second_boot.operations_for(session_id),
            ["session_status", "session_bind"],
            "{session_id}: the second boot recovers the roots and the unsettled children"
        );
    }
    reloaded.shutdown_persist_blocking();
}
