//! Engram compaction cache lifecycle through real nudge preparation and delivery
//! acknowledgement. General control-plane tests remain in engram_host_adapter.
use super::engram_host_adapter::real_engram_control_fixture_path;
use super::*;
use std::path::Path;

fn fixture(gated: bool) -> (AppState, String, PathBuf) {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root")
        .path()
        .join("compaction-cache");
    fs::create_dir_all(&root).expect("create project");
    fs::write(
        root.join(".engram-project"),
        if gated {
            "fixture-work-next-gated\n"
        } else {
            "fixture-ready\n"
        },
    )
    .expect("declaration");
    let project_id = create_test_project(&state, &root, "Compaction cache");
    {
        let mut inner = state.inner.lock().expect("state mutex");
        let project = inner
            .projects
            .iter_mut()
            .find(|p| p.id == project_id)
            .expect("project");
        project.engram = Some(EngramProjectSettings {
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
    }
    let id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    (state, id, root)
}

fn reads(root: &Path) -> usize {
    fs::read_to_string(root.join("work-context-reads"))
        .expect("read log")
        .lines()
        .count()
}

fn delivered(state: &AppState, id: &str) {
    {
        let mut inner = state.inner.lock().expect("state mutex");
        let index = inner.find_session_index(id).expect("session");
        let cache = &mut inner.sessions[index].engram;
        assert!(cache.pending_context_nudge.is_some());
        cache.context_nudge_delivery_generation = Some(cache.context_nudge_generation);
        cache.context_nudge_delivery_turn_generation = Some(42);
    }
    state.acknowledge_engram_context_nudge_delivery(id, 41);
    {
        let inner = state.inner.lock().expect("state mutex");
        assert!(
            inner.sessions[inner.find_session_index(id).expect("session")]
                .engram
                .pending_context_nudge
                .is_some(),
            "stale delivery cannot consume a page"
        );
    }
    state.acknowledge_engram_context_nudge_delivery(id, 42);
}

#[test]
fn compaction_keeps_undelivered_page_until_ack_then_refreshes() {
    let (state, id, root) = fixture(false);
    assert_eq!(
        state.prepare_engram_context_nudge_off_lock(&id),
        EngramContextNudgePreparation::Ready
    );
    state.mark_engram_context_nudge_pending(&id);
    assert_eq!(
        state.prepare_engram_context_nudge_off_lock(&id),
        EngramContextNudgePreparation::Ready
    );
    assert_eq!(
        reads(&root),
        1,
        "compaction must not acknowledge an undelivered page with another advancing read"
    );
    delivered(&state, &id);
    assert_eq!(
        state.prepare_engram_context_nudge_off_lock(&id),
        EngramContextNudgePreparation::Ready
    );
    assert_eq!(reads(&root), 2, "refresh must survive until after delivery");
}

#[test]
fn compaction_during_read_keeps_result_and_defers_next_read_until_ack() {
    let (state, id, root) = fixture(true);
    let mut gate = phase_sync::WorkContextGate::new(&root);
    let worker_state = state.clone();
    let worker_id = id.clone();
    let worker =
        std::thread::spawn(move || worker_state.prepare_engram_context_nudge_off_lock(&worker_id));
    gate.wait();
    state.mark_engram_context_nudge_pending(&id);
    gate.release();
    assert_eq!(
        worker.join().expect("reader"),
        EngramContextNudgePreparation::Ready
    );
    assert_eq!(
        reads(&root),
        1,
        "compaction must retain the in-flight page rather than advancing again"
    );
    assert_eq!(
        state.prepare_engram_context_nudge_off_lock(&id),
        EngramContextNudgePreparation::Ready
    );
    assert_eq!(reads(&root), 1);
    delivered(&state, &id);
    assert_eq!(
        state.prepare_engram_context_nudge_off_lock(&id),
        EngramContextNudgePreparation::Ready
    );
    assert_eq!(reads(&root), 2);
}

#[test]
fn compaction_dedup_is_bounded_and_eviction_never_discards_a_page() {
    let mut cache = EngramSessionState::default();
    cache.pending_context_nudge = Some("undelivered".to_owned());
    cache.context_nudge_generation = 7;
    for i in 0..80 {
        assert!(cache.mark_context_refresh_needed(Some(&format!("item-{i}"))));
    }
    assert_eq!(cache.signalled_compaction_item_ids.len(), 64);
    assert!(!cache.mark_context_refresh_needed(Some("item-79")));
    assert!(
        cache.mark_context_refresh_needed(Some("item-0")),
        "evicted ids may signal again"
    );
    assert!(cache.mark_context_refresh_needed(None));
    assert!(cache.mark_context_refresh_needed(Some(&"x".repeat(257))));
    assert_eq!(cache.signalled_compaction_item_ids.len(), 64);
    assert!(
        cache
            .signalled_compaction_item_ids
            .iter()
            .all(|id| id.len() <= 256)
    );
    assert_eq!(cache.pending_context_nudge.as_deref(), Some("undelivered"));
    assert_eq!(cache.context_nudge_generation, 7);
}

#[test]
fn manual_compaction_preserves_undelivered_context() {
    let (state, id, root) = fixture(false);
    state
        .set_external_session_id(&id, "manual-compact-thread".to_owned())
        .unwrap();
    assert_eq!(
        state.prepare_engram_context_nudge_off_lock(&id),
        EngramContextNudgePreparation::Ready
    );
    let (runtime, input_rx, _process) = test_shared_codex_runtime("manual-compaction-cache");
    *state.shared_codex_runtime.lock().unwrap() = Some(runtime);
    let responder = std::thread::spawn(move || {
        let command = recv_within_guard(&input_rx, "manual compaction RPC").unwrap();
        let CodexRuntimeCommand::JsonRpcRequest {
            method,
            params,
            response_tx,
            ..
        } = command
        else {
            panic!("expected JSON-RPC");
        };
        assert_eq!(method, "thread/compact/start");
        assert_eq!(params["threadId"], "manual-compact-thread");
        response_tx.send(Ok(json!({}))).unwrap();
    });
    state.compact_codex_thread(&id).unwrap();
    responder.join().unwrap();
    {
        let inner = state.inner.lock().unwrap();
        let cache = &inner.sessions[inner.find_session_index(&id).unwrap()].engram;
        assert!(
            cache.pending_context_nudge.is_some(),
            "manual compaction must preserve the page"
        );
    }
    assert_eq!(
        state.prepare_engram_context_nudge_off_lock(&id),
        EngramContextNudgePreparation::Ready
    );
    assert_eq!(reads(&root), 1);
    delivered(&state, &id);
    assert_eq!(
        state.prepare_engram_context_nudge_off_lock(&id),
        EngramContextNudgePreparation::Ready
    );
    assert_eq!(reads(&root), 2);
}

#[test]
fn settings_rollback_preserves_compaction_refresh_after_delivery() {
    let (state, id, root) = fixture(false);
    assert_eq!(
        state.prepare_engram_context_nudge_off_lock(&id),
        EngramContextNudgePreparation::Ready
    );
    state.mark_engram_context_nudge_pending(&id);
    let generation = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        let generation = inner.sessions[index].engram.context_nudge_generation;
        let previous = mark_engram_mcp_runtime_resets_locked(&mut inner, std::slice::from_ref(&id));
        restore_engram_mcp_runtime_resets_locked(&mut inner, previous);
        generation
    };
    delivered(&state, &id);
    assert_eq!(
        state.prepare_engram_context_nudge_off_lock(&id),
        EngramContextNudgePreparation::Ready
    );
    assert_eq!(reads(&root), 2, "rollback must retain the deferred read");
    let inner = state.inner.lock().unwrap();
    let cache = &inner.sessions[inner.find_session_index(&id).unwrap()].engram;
    assert_eq!(
        cache.context_nudge_generation,
        generation + 1,
        "refresh must not replay the old generation"
    );
}

#[test]
fn compaction_deferred_page_is_ready_for_prompt_admission() {
    let (state, id, _) = fixture(false);
    assert_eq!(
        state.prepare_engram_context_nudge_off_lock(&id),
        EngramContextNudgePreparation::Ready
    );
    state.mark_engram_context_nudge_pending(&id);
    {
        let inner = state.inner.lock().unwrap();
        let cache = &inner.sessions[inner.find_session_index(&id).unwrap()].engram;
        assert!(
            !cache.context_needs_preparation(),
            "both admission paths must admit the pending page without retrying"
        );
    }
    delivered(&state, &id);
    {
        let inner = state.inner.lock().unwrap();
        let cache = &inner.sessions[inner.find_session_index(&id).unwrap()].engram;
        assert!(
            cache.context_needs_preparation(),
            "delivery exposes the deferred refresh for the next prompt"
        );
    }
    assert_eq!(
        state.prepare_engram_context_nudge_off_lock(&id),
        EngramContextNudgePreparation::Ready
    );
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&id).unwrap();
    let cache = &mut inner.sessions[index].engram;
    assert!(!cache.context_needs_preparation());
    cache.invalidate_context_nudge();
    assert!(
        cache.context_needs_preparation(),
        "settings invalidation must still block admission for a fresh read"
    );
}
