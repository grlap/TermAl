//! Boot-recovery target selection for delegation children (tm-cr0i, tm-8b7y):
//! a child of a delegation that already ended and holds no mirrored grant is
//! skipped and keeps its stale token for the ordinary rebind path of a later
//! follow-up, while roots, running children and finished children with a
//! mirrored grant keep the existing control-state recovery.
//!
//! Owns the finished-child selection test, the settlement of records written
//! before begins were recorded, the persistence round trip of an uncertain
//! grant across boots, and the recording transport they share with
//! `engram_uncertain_begin.rs`. Does not own the worker-concurrency, budget
//! or grant-checkpoint recovery tests, which stay in
//! `src/tests/engram_host_adapter.rs`, nor the mirroring of an in-flight
//! begin (`engram_uncertain_begin.rs`). New module beside that file, created
//! instead of growing it.

use super::super::delegation_support::{
    install_delegation_codex_runtime, temp_delegation_state_paths,
};
use super::*;

/// Answers every control request generically and records which session asked
/// for which operation, so target selection can be asserted without ordering
/// assumptions across concurrent recovery workers. Per session it can report
/// a grant open until a checkpoint receipt names it, and answer checkpoints
/// with a receipt for another grant.
pub(super) struct RecordingBootRecoveryTransport {
    requests: Mutex<Vec<(String, String)>>,
    failing_sessions: Mutex<HashSet<String>>,
    open_grants: Mutex<HashMap<String, String>>,
    checkpoint_receipts: Mutex<HashMap<String, String>>,
}

impl RecordingBootRecoveryTransport {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            failing_sessions: Mutex::new(HashSet::new()),
            open_grants: Mutex::new(HashMap::new()),
            checkpoint_receipts: Mutex::new(HashMap::new()),
        })
    }

    /// Status for `session_id` reports `grant_id` open until a checkpoint
    /// whose receipt names it lands, modelling a grant Engram still holds.
    pub(super) fn report_open_grant(&self, session_id: &str, grant_id: &str) {
        self.open_grants
            .lock()
            .expect("recording transport mutex poisoned")
            .insert(session_id.to_owned(), grant_id.to_owned());
    }

    /// Every checkpoint for `session_id` is answered with a receipt naming
    /// `grant_id` instead of the grant it asked for.
    pub(super) fn answer_checkpoints_with(&self, session_id: &str, grant_id: &str) {
        self.checkpoint_receipts
            .lock()
            .expect("recording transport mutex poisoned")
            .insert(session_id.to_owned(), grant_id.to_owned());
    }

    /// Every request for `session_id` fails in transport until it is
    /// released again, modelling one target that a boot cannot settle.
    pub(super) fn fail_session(&self, session_id: &str, failing: bool) {
        let mut failing_sessions = self
            .failing_sessions
            .lock()
            .expect("recording transport mutex poisoned");
        if failing {
            failing_sessions.insert(session_id.to_owned());
        } else {
            failing_sessions.remove(session_id);
        }
    }

    pub(super) fn operations_for(&self, session_id: &str) -> Vec<String> {
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
        if self
            .failing_sessions
            .lock()
            .expect("recording transport mutex poisoned")
            .contains(&connection.session_id)
        {
            return Err(EngramTransportError::transport(
                "fixture recovery transport failure",
            ));
        }
        match operation.as_str() {
            "session_status" => {
                let open_grant_id = self
                    .open_grants
                    .lock()
                    .expect("recording transport mutex poisoned")
                    .get(&connection.session_id)
                    .cloned();
                Ok(json!({ "phase": "ready", "open_grant_id": open_grant_id }))
            }
            "turn_checkpoint" => {
                let requested = request["grant_id"].as_str().unwrap_or_default().to_owned();
                let receipt_grant_id = self
                    .checkpoint_receipts
                    .lock()
                    .expect("recording transport mutex poisoned")
                    .get(&connection.session_id)
                    .cloned()
                    .unwrap_or_else(|| requested.clone());
                let mut open_grants = self
                    .open_grants
                    .lock()
                    .expect("recording transport mutex poisoned");
                if receipt_grant_id == requested
                    && open_grants.get(&connection.session_id) == Some(&requested)
                {
                    open_grants.remove(&connection.session_id);
                }
                Ok(json!({
                    "decision": "checkpointed",
                    "receipt": {
                        "grant_id": receipt_grant_id,
                        "cursor": 1,
                        "confirmed_cursor": 1
                    }
                }))
            }
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
fn boot_recovery_skips_finished_delegation_children_without_a_mirrored_grant() {
    let (_temp_root, project_root, persistence_path, templates_path) =
        temp_delegation_state_paths();
    let root = project_root.join("engram-finished-child-boot-recovery-project");
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
    install_delegation_codex_runtime(&state, "engram-finished-child-boot-recovery");
    let project_id = create_test_project(&state, &root, "Engram finished-child boot recovery");
    // A parent runs only a bounded number of delegations at once, so the
    // children spread over as many parents as the bound requires. Every
    // parent is an ordinary root with a stale token.
    const CHILDREN: usize = 7;
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
    // The children with a begun grant model turns whose end-of-turn
    // checkpoint failed, one completed and one canceled; the one with an
    // uncertain grant models a cancellation that raced an in-flight begin,
    // with the process ending before the compensating checkpoint settled it.
    let [
        completed,
        completed_with_grant,
        failed,
        canceled,
        canceled_with_grant,
        canceled_with_uncertain_grant,
        running,
    ] = <[String; CHILDREN]>::try_from(children).expect("seven delegation children");
    let terminal_statuses = [
        (&completed, DelegationStatus::Completed),
        (&completed_with_grant, DelegationStatus::Completed),
        (&failed, DelegationStatus::Failed),
        (&canceled, DelegationStatus::Canceled),
        (&canceled_with_grant, DelegationStatus::Canceled),
        (&canceled_with_uncertain_grant, DelegationStatus::Canceled),
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
            .expect("completed child with a mirrored grant should exist");
        inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .engram
            .active_grant_id = Some("begun-before-completion".to_owned());
        let index = inner
            .find_session_index(&canceled_with_grant)
            .expect("child with a mirrored grant should exist");
        inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .engram
            .active_grant_id = Some("begun-before-cancellation".to_owned());
        let index = inner
            .find_session_index(&canceled_with_uncertain_grant)
            .expect("child with an uncertain grant should exist");
        inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .engram
            .uncertain_grant_id = Some("begin-in-flight-at-cancellation".to_owned());
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
    let skipped = [&completed, &failed, &canceled];
    let kept = parents
        .iter()
        .chain([
            &completed_with_grant,
            &canceled_with_grant,
            &canceled_with_uncertain_grant,
            &running,
        ])
        .collect::<Vec<_>>();
    assert_eq!(
        planned,
        sorted(kept.iter().map(|id| (*id).clone())),
        "roots, running children and finished children with a mirrored or uncertain grant stay boot targets"
    );
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        for session_id in skipped {
            let record = &inner.sessions[inner
                .find_session_index(session_id)
                .expect("skipped child should exist")];
            assert_eq!(
                record.engram.routing_token.as_deref(),
                Some(format!("stale-{session_id}").as_str()),
                "{session_id}: a skipped finished child keeps its stale token for a later follow-up"
            );
            assert!(record.engram.rebind_required, "{session_id}");
            assert!(
                !record.engram_boot_recovery_pending,
                "{session_id}: a skipped finished child is not readiness-fenced"
            );
        }
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

    for session_id in skipped {
        assert_eq!(
            transport.operations_for(session_id),
            Vec::<String>::new(),
            "{session_id}: a skipped finished child costs no control request"
        );
    }
    for session_id in &kept {
        assert_eq!(
            transport.operations_for(session_id),
            ["session_status", "session_bind"],
            "{session_id}: a boot target runs status plus fresh bind"
        );
    }
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        for (session_id, kind) in [
            (&completed_with_grant, "completed"),
            (&canceled_with_grant, "canceled"),
        ] {
            let record = &inner.sessions[inner
                .find_session_index(session_id)
                .expect("child with a mirrored grant should exist")];
            assert_eq!(
                record.engram.active_grant_id, None,
                "a clean control-plane status clears the mirrored grant of a {kind} child"
            );
            assert_eq!(
                record.engram.routing_token.as_deref(),
                Some(format!("recovered-{session_id}").as_str())
            );
        }
        let record = &inner.sessions[inner
            .find_session_index(&canceled_with_uncertain_grant)
            .expect("child with an uncertain grant should exist")];
        assert_eq!(
            record.engram.uncertain_grant_id, None,
            "a clean control-plane status settles the uncertain grant of a canceled child"
        );
        assert_eq!(
            record.engram.routing_token.as_deref(),
            Some(format!("recovered-{canceled_with_uncertain_grant}").as_str())
        );
    }
    // Drain the background persist worker so the reload reads the recovered
    // tokens and the cleared grant, not a snapshot from before them.
    state.shutdown_persist_blocking();
    drop(state);

    // The rule applies afresh on every boot from persisted state: the next
    // boot's own recovery (which the test constructor runs inline before
    // returning) still skips the three finished children, now also the two
    // whose grants the first boot settled, keeps all their tokens, and
    // recovers only the roots and the running child.
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
        (&failed, format!("stale-{failed}")),
        (&canceled, format!("stale-{canceled}")),
        (
            &canceled_with_grant,
            format!("recovered-{canceled_with_grant}"),
        ),
        (
            &canceled_with_uncertain_grant,
            format!("recovered-{canceled_with_uncertain_grant}"),
        ),
    ];
    let kept_after_reload = parents.iter().chain([&running]).collect::<Vec<_>>();
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
            assert_eq!(record.engram.uncertain_grant_id, None, "{session_id}");
            assert!(
                record.engram.begins_recorded,
                "{session_id}: a record created since begins were recorded reloads settled"
            );
            assert!(!record.engram_boot_recovery_pending, "{session_id}");
        }
    }
    for (session_id, _) in &skipped_after_reload {
        assert_eq!(
            second_boot.operations_for(session_id),
            Vec::<String>::new(),
            "{session_id}: the second boot leaves a skipped finished child alone"
        );
    }
    for session_id in &kept_after_reload {
        assert_eq!(
            second_boot.operations_for(session_id),
            ["session_status", "session_bind"],
            "{session_id}: the second boot recovers the roots and the running child"
        );
    }
    reloaded.shutdown_persist_blocking();
}

#[test]
fn a_record_written_before_begins_were_recorded_keeps_recovering_until_a_bind_settles_it() {
    // A record loaded from a store written before begins were recorded may
    // hide one, so it keeps its eager recovery, after the live sessions,
    // until one accepted bind settles it. Each bounded boot settles as many
    // such records as it can; the settled ones are skipped from then on.
    let (_temp_root, project_root, persistence_path, templates_path) =
        temp_delegation_state_paths();
    let root = project_root.join("engram-boot-settlement-project");
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
    install_delegation_codex_runtime(&state, "engram-boot-settlement");
    let project_id = create_test_project(&state, &root, "Engram boot settlement");
    let parent = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let children = (0..3)
        .map(|index| {
            state
                .create_read_only_delegation(
                    &parent,
                    CreateDelegationRequest {
                        prompt: format!("Create settlement target {index}."),
                        title: Some(format!("Engram settlement target {index}")),
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
    // Failed and canceled are the statuses the older selection never skipped:
    // exactly the records whose hidden begin the settlement pass exists to
    // find. A completed child was skipped whatever its record said, and an
    // unrecorded one stays skipped rather than bringing that cost back.
    let [completed, failed, canceled] =
        <[String; 3]>::try_from(children).expect("three delegation children");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .expect("project should exist")
            .engram = Some(EngramProjectSettings {
            acceptance_evaluation: None,
            enabled: true,
            turn_gated_control: true,
            binary_path: Some(root.join("engram-fixture").to_string_lossy().into_owned()),
            home: Some(home.to_string_lossy().into_owned()),
            work_authority_grant: None,
            authority_store_key: None,
            deadline_ms: Some(250),
        });
        for (child, status) in [
            (&completed, DelegationStatus::Completed),
            (&failed, DelegationStatus::Failed),
            (&canceled, DelegationStatus::Canceled),
        ] {
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
        for index in 0..inner.sessions.len() {
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            record.engram.routing_token = Some(format!("stale-{}", record.session.id));
            record.engram.rebind_required = true;
            // Records created in this process are born settled; a store
            // written before begins were recorded loads its children unsettled.
            if record.session.id != parent {
                record.engram.begins_recorded = false;
            }
        }
        state
            .commit_locked(&mut inner)
            .expect("settlement setup should persist");
    }
    let transport = RecordingBootRecoveryTransport::new();
    state.install_control_test_transport(transport.clone());

    // Boot 1: the canceled child cannot be settled and stays unrecorded; the
    // failed child settles; the completed child is not planned at all.
    // Unsettled finished children are planned last.
    transport.fail_session(&canceled, true);
    let plan = state
        .prepare_engram_sessions_for_boot_recovery()
        .expect("first boot recovery plan should be prepared");
    let planned = plan
        .targets
        .iter()
        .map(|target| target.connection.session_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        planned[0], parent,
        "live sessions are planned before unsettled finished children"
    );
    assert_eq!(
        sorted(planned),
        sorted([parent.clone(), failed.clone(), canceled.clone()]),
        "unsettled failed and canceled children are still recovered; an unrecorded completed child is skipped as it always was"
    );
    state.recover_prepared_engram_sessions_after_boot(plan);
    assert_eq!(
        transport.operations_for(&completed),
        Vec::<String>::new(),
        "an unrecorded completed child costs no control request"
    );
    assert_eq!(
        transport.operations_for(&failed),
        ["session_status", "session_bind"],
        "the settled finished child recovered as before"
    );
    assert_eq!(
        transport.operations_for(&canceled),
        ["session_status"],
        "the failing finished child was attempted and failed"
    );
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner
            .find_session_index(&failed)
            .expect("failed child should exist")];
        assert!(
            record.engram.begins_recorded,
            "an accepted bind marks the record settled"
        );
        let record = &inner.sessions[inner
            .find_session_index(&canceled)
            .expect("canceled child should exist")];
        assert!(
            !record.engram.begins_recorded,
            "a record recovery could not settle stays unsettled"
        );
    }
    state.shutdown_persist_blocking();
    drop(state);

    // Boot 2: the settled child is skipped, the unsettled one is recovered
    // again and now settles; the completed child stays skipped; the root is
    // recovered as always.
    let second_boot = RecordingBootRecoveryTransport::new();
    let reloaded = AppState::new_with_paths_and_engram_transport_for_test(
        project_root.to_string_lossy().into_owned(),
        persistence_path.clone(),
        templates_path.clone(),
        second_boot.clone(),
    )
    .expect("state should boot again");
    assert_eq!(
        second_boot.operations_for(&failed),
        Vec::<String>::new(),
        "the settled finished child is skipped"
    );
    assert_eq!(
        second_boot.operations_for(&completed),
        Vec::<String>::new(),
        "an unrecorded completed child is skipped on every boot"
    );
    assert_eq!(
        second_boot.operations_for(&canceled),
        ["session_status", "session_bind"],
        "the unsettled finished child is recovered again"
    );
    assert_eq!(
        second_boot.operations_for(&parent),
        ["session_status", "session_bind"],
        "the root is still recovered"
    );
    {
        let inner = reloaded.inner.lock().expect("state mutex poisoned");
        for session_id in [&failed, &canceled] {
            let record = &inner.sessions[inner
                .find_session_index(session_id)
                .expect("child should reload")];
            assert!(
                record.engram.begins_recorded,
                "{session_id}: settled durably"
            );
        }
        let record = &inner.sessions[inner
            .find_session_index(&completed)
            .expect("completed child should reload")];
        assert!(
            !record.engram.begins_recorded,
            "a completed child owes no settlement, so its record is never marked"
        );
        assert_eq!(
            record.engram.routing_token.as_deref(),
            Some(format!("stale-{completed}").as_str()),
            "its stale token waits for the ordinary rebind path of a later follow-up"
        );
    }

    // Boot 3 from the same state: nothing finished is left to settle.
    let plan = reloaded
        .prepare_engram_sessions_for_boot_recovery()
        .expect("third boot recovery plan should be prepared");
    assert_eq!(
        plan.targets
            .iter()
            .map(|target| target.connection.session_id.clone())
            .collect::<Vec<_>>(),
        [parent.clone()],
        "only the root remains a boot target once every finished child is settled"
    );
    reloaded.shutdown_persist_blocking();
}

#[test]
fn a_persisted_uncertain_grant_survives_a_reload_and_keeps_the_finished_child_a_boot_target() {
    // The record is the only durable evidence of an uncertain begin. It must
    // come back from the store as written, make the finished child a target
    // of the next boot's recovery, stay when that boot cannot settle it, and
    // be settled by the boot that can.
    let (_temp_root, project_root, persistence_path, templates_path) =
        temp_delegation_state_paths();
    let root = project_root.join("engram-uncertain-reload-project");
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
    install_delegation_codex_runtime(&state, "engram-uncertain-reload");
    let project_id = create_test_project(&state, &root, "Engram uncertain reload");
    let parent = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let child = state
        .create_read_only_delegation(
            &parent,
            CreateDelegationRequest {
                prompt: "Create the reload target.".to_owned(),
                title: Some("Engram reload target".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("Engram-off delegation should start")
        .delegation
        .child_session_id;
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .expect("project should exist")
            .engram = Some(EngramProjectSettings {
            acceptance_evaluation: None,
            enabled: true,
            turn_gated_control: true,
            binary_path: Some(root.join("engram-fixture").to_string_lossy().into_owned()),
            home: Some(home.to_string_lossy().into_owned()),
            work_authority_grant: None,
            authority_store_key: None,
            deadline_ms: Some(250),
        });
        let index = inner
            .delegations
            .iter()
            .position(|delegation| delegation.child_session_id == child)
            .expect("delegation row should exist");
        inner.delegations[index].status = DelegationStatus::Canceled;
        inner
            .mark_delegation_mutated(index)
            .expect("delegation index should be valid");
        for index in 0..inner.sessions.len() {
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            record.engram.routing_token = Some(format!("stale-{}", record.session.id));
            record.engram.rebind_required = true;
        }
        let index = inner
            .find_session_index(&child)
            .expect("child should exist");
        inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .engram
            .uncertain_grant_id = Some("uncertain-across-reload".to_owned());
        state
            .commit_locked(&mut inner)
            .expect("the uncertain grant should persist");
    }
    state.shutdown_persist_blocking();
    drop(state);

    // Boot 2 cannot settle the child: the grant reloads as written, makes
    // the canceled child a recovery target, and stays when recovery fails.
    let second_boot = RecordingBootRecoveryTransport::new();
    second_boot.fail_session(&child, true);
    let reloaded = AppState::new_with_paths_and_engram_transport_for_test(
        project_root.to_string_lossy().into_owned(),
        persistence_path.clone(),
        templates_path.clone(),
        second_boot.clone(),
    )
    .expect("state should boot again");
    assert_eq!(
        second_boot.operations_for(&child),
        ["session_status"],
        "the reloaded uncertain grant makes the canceled child a boot target"
    );
    {
        let inner = reloaded.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner
            .find_session_index(&child)
            .expect("child should reload")];
        assert_eq!(
            record.engram.uncertain_grant_id.as_deref(),
            Some("uncertain-across-reload"),
            "the grant reloads as written and survives a boot that cannot settle it"
        );
        assert_eq!(
            record.engram.routing_token.as_deref(),
            Some(format!("stale-{child}").as_str())
        );
        assert!(record.engram.rebind_required);
    }
    reloaded.shutdown_persist_blocking();
    drop(reloaded);

    // Boot 3 recovers it again, and a clean status settles it.
    let third_boot = RecordingBootRecoveryTransport::new();
    let settled = AppState::new_with_paths_and_engram_transport_for_test(
        project_root.to_string_lossy().into_owned(),
        persistence_path.clone(),
        templates_path.clone(),
        third_boot.clone(),
    )
    .expect("state should boot a third time");
    assert_eq!(
        third_boot.operations_for(&child),
        ["session_status", "session_bind"],
        "the next boot recovers the kept grant and settles it"
    );
    {
        let inner = settled.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner
            .find_session_index(&child)
            .expect("child should reload again")];
        assert_eq!(record.engram.uncertain_grant_id, None);
        assert_eq!(
            record.engram.routing_token.as_deref(),
            Some(format!("recovered-{child}").as_str())
        );
    }
    settled.shutdown_persist_blocking();
}
