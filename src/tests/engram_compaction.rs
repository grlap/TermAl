//! Engram compaction cache lifecycle through real nudge preparation and delivery
//! acknowledgement. General control-plane tests remain in engram_host_adapter.
use super::delegation_support::test_app_state_with_delegation_codex_runtime;
use super::engram_host_adapter::real_engram_control_fixture_path;
use super::*;
use std::path::Path;

fn fixture(gated: bool) -> (AppState, String, PathBuf) {
    fixture_with_state(test_app_state(), gated)
}

fn fixture_with_state(state: AppState, gated: bool) -> (AppState, String, PathBuf) {
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
            acceptance_evaluation: None,
            enabled: true,
            execution_observability: false,
            binary_path: Some(
                real_engram_control_fixture_path()
                    .to_string_lossy()
                    .into_owned(),
            ),
            home: Some(root.to_string_lossy().into_owned()),
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
            "stale runtime acknowledgement cannot consume cached orientation"
        );
    }
    state.acknowledge_engram_context_nudge_delivery(id, 42);
}

fn delivery_cursor(root: &Path) -> usize {
    match fs::read_to_string(root.join("work-delivery-cursor")) {
        Ok(value) => value.trim().parse().unwrap(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => panic!("read delivery cursor: {error}"),
    }
}

#[test]
fn startup_and_compaction_nudges_peek_without_consuming_truncated_or_unsent_context() {
    for after_compaction in [false, true] {
        let (state, id, root) = fixture(false);
        fs::write(root.join(".engram-project"), "fixture-work-next-delivery\n").unwrap();
        if after_compaction {
            assert_eq!(
                state.prepare_engram_context_nudge_off_lock(&id),
                EngramContextNudgePreparation::Ready
            );
            delivered(&state, &id);
            state.mark_engram_context_nudge_pending(&id);
        }
        assert_eq!(
            state.prepare_engram_context_nudge_off_lock(&id),
            EngramContextNudgePreparation::Ready
        );
        #[cfg(windows)]
        let args: Vec<String> = serde_json::from_str(
            &fs::read_to_string(root.join("work-context-args.json"))
                .unwrap()
                .trim_start_matches('\u{feff}'),
        )
        .unwrap();
        #[cfg(not(windows))]
        let args: Vec<String> = fs::read_to_string(root.join("work-context-args.txt"))
            .unwrap()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        assert_eq!(
            args.iter().filter(|arg| *arg == "--peek").count(),
            1,
            "nudge argv must contain one non-advancing --peek"
        );
        assert_eq!(
            args.iter()
                .filter(|arg| *arg == "--context-generation")
                .count(),
            1,
            "peek must include the generation for its read-only changed signal"
        );
        let expected_generation = format!(
            "termal-{}-{}",
            state.server_instance_id,
            if after_compaction { 2 } else { 1 }
        );
        assert!(
            args.windows(2).any(|pair| {
                pair[0] == "--context-generation" && pair[1] == expected_generation
            }),
            "nudge must advertise the current generation: {args:?}"
        );
        assert_eq!(
            delivery_cursor(&root),
            0,
            "orientation must not consume ordinary delivery"
        );
        {
            let inner = state.inner.lock().unwrap();
            let context = inner.sessions[inner.find_session_index(&id).unwrap()]
                .engram
                .pending_context_nudge
                .as_ref()
                .unwrap();
            assert_eq!(context.len(), ENGRAM_CONTEXT_NUDGE_MAX_BYTES);
            assert!(
                !context.contains("complete-page-tail"),
                "fixture must actually exercise host truncation"
            );
        }
        // Do not deliver this cached, truncated snapshot. Compaction + prepare
        // retains it without turning it into an Engram delivery receipt.
        let reads_before = reads(&root);
        state.mark_engram_context_nudge_pending(&id);
        assert_eq!(
            state.prepare_engram_context_nudge_off_lock(&id),
            EngramContextNudgePreparation::Ready
        );
        assert_eq!(reads(&root), reads_before);
        assert_eq!(delivery_cursor(&root), 0);
        // Positive control: the same fixture's ordinary next DOES advance,
        // and still returns the complete first page after host truncation.
        for page in [0, 1] {
            let output = engram_command(&real_engram_control_fixture_path())
                .args(["--project-file"])
                .arg(root.join(".engram-project"))
                .arg("--home")
                .arg(&root)
                .args([
                    "work",
                    "--actor-id",
                    "fixture-agent",
                    "--session-id",
                    &id,
                    "--actor-context",
                    "fixture-context",
                    "next",
                ])
                .env(ENGRAM_HOME_ENV, &root)
                .env(ENGRAM_ACTOR_ID_ENV, "fixture-agent")
                .env(ENGRAM_SESSION_ID_ENV, &id)
                .env(ENGRAM_ACTOR_CONTEXT_ENV, "fixture-context")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let text = String::from_utf8(output.stdout).unwrap();
            assert!(text.starts_with(&format!("delivery-page-{page}")));
            assert!(text.contains("complete-page-tail"));
            assert_eq!(delivery_cursor(&root), page + 1);
        }
        delivered(&state, &id);
        assert_eq!(
            delivery_cursor(&root),
            2,
            "host cache acknowledgement is not an Engram acknowledgement"
        );
    }
}

#[test]
fn compaction_keeps_unsent_orientation_until_runtime_ack_then_refreshes() {
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
        "compaction must reuse cached orientation until runtime acceptance"
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
        "compaction must retain the in-flight orientation rather than refetching"
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
    install_single_wire_codex_fixture(&state, Some(runtime));
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
            !cache.context_needs_refresh(),
            "the pending page is available without another read"
        );
    }
    delivered(&state, &id);
    {
        let inner = state.inner.lock().unwrap();
        let cache = &inner.sessions[inner.find_session_index(&id).unwrap()].engram;
        assert!(
            cache.context_needs_refresh(),
            "delivery exposes the deferred advisory refresh"
        );
    }
    assert_eq!(
        state.prepare_engram_context_nudge_off_lock(&id),
        EngramContextNudgePreparation::Ready
    );
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&id).unwrap();
    let cache = &mut inner.sessions[index].engram;
    assert!(!cache.context_needs_refresh());
    cache.invalidate_context_nudge();
    assert!(
        cache.context_needs_refresh(),
        "settings invalidation must still request a fresh advisory read"
    );
}

fn context_argv(root: &Path) -> Vec<String> {
    #[cfg(windows)]
    {
        serde_json::from_str(
            fs::read_to_string(root.join("work-context-args.json"))
                .expect("orientation argv receipt")
                .trim_start_matches('\u{feff}'),
        )
        .expect("orientation argv JSON")
    }
    #[cfg(not(windows))]
    {
        fs::read_to_string(root.join("work-context-args.txt"))
            .expect("orientation argv receipt")
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    }
}

fn context_token(root: &Path) -> String {
    let args = context_argv(root);
    let tokens = args
        .windows(2)
        .filter(|pair| pair[0] == "--context-generation")
        .map(|pair| pair[1].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        tokens.len(),
        1,
        "one context-generation argv value: {args:?}"
    );
    tokens[0].clone()
}

#[test]
fn context_generation_restored_session_never_reuses_a_previous_host_token() {
    let (state, id, root) = fixture(false);
    fs::write(root.join(".engram-project"), "fixture-work-next-delivery\n").unwrap();
    assert_eq!(
        state.prepare_engram_context_nudge_off_lock(&id),
        EngramContextNudgePreparation::Ready
    );
    let startup_token = context_token(&root);
    delivered(&state, &id);
    state.mark_engram_context_nudge_pending(&id);
    assert_eq!(
        state.prepare_engram_context_nudge_off_lock(&id),
        EngramContextNudgePreparation::Ready
    );
    let advanced_token = context_token(&root);
    assert_ne!(
        advanced_token, startup_token,
        "compaction advances the token"
    );
    {
        let inner = state.inner.lock().expect("state mutex");
        let record = &inner.sessions[inner.find_session_index(&id).expect("session")];
        assert_eq!(record.engram.context_nudge_generation, 2);
        persist_state(state.persistence_path.as_ref(), &inner).expect("persist advanced session");
    }
    let loaded = load_state(state.persistence_path.as_ref())
        .expect("load persisted session")
        .expect("persisted state exists");
    let restored = test_app_state();
    assert_ne!(restored.server_instance_id, state.server_instance_id);
    *restored.inner.lock().expect("state mutex") = loaded;
    // A production restart installs its runtime-private launch policy after
    // loading StateInner. This fixture bypasses boot, so mirror that step.
    let transport = restored
        .inner
        .lock()
        .expect("state mutex")
        .engram_host_adapter
        .transport
        .clone();
    restored.install_test_engram_transport(transport);
    assert_eq!(
        restored.prepare_engram_context_nudge_off_lock(&id),
        EngramContextNudgePreparation::Ready
    );
    let restored_token = context_token(&root);
    assert!(
        restored_token != startup_token && restored_token != advanced_token,
        "a restarted host must not reuse any earlier token: startup={startup_token}, advanced={advanced_token}, restored={restored_token}"
    );
    assert_eq!(
        restored_token,
        format!("termal-{}-1", restored.server_instance_id)
    );
    let reads_after_restart = reads(&root);
    assert_eq!(
        restored.prepare_engram_context_nudge_off_lock(&id),
        EngramContextNudgePreparation::Ready
    );
    assert_eq!(
        reads(&root),
        reads_after_restart,
        "same context reuses its cached read"
    );
    assert_eq!(context_token(&root), restored_token);
}

#[test]
fn context_generation_orientation_token_is_plain_with_host_uuid_and_full_counter() {
    for generation in [1, u64::MAX] {
        let (state, id, root) = fixture(false);
        fs::write(root.join(".engram-project"), "fixture-work-next-delivery\n").unwrap();
        {
            let mut inner = state.inner.lock().expect("state mutex");
            let index = inner.find_session_index(&id).expect("session");
            inner.sessions[index].engram.context_nudge_generation = generation;
        }
        assert_eq!(
            state.prepare_engram_context_nudge_off_lock(&id),
            EngramContextNudgePreparation::Ready
        );
        let token = context_token(&root);
        assert_eq!(
            token,
            format!("termal-{}-{generation}", state.server_instance_id)
        );
        assert!((1..=256).contains(&token.len()));
        assert!(!token.starts_with('-'));
        assert!(
            token
                .bytes()
                .all(|byte| { byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') }),
            "Engram plain-token contract: {token}"
        );
    }
}

struct ContextReadRelease(Option<mpsc::Sender<()>>);

impl Drop for ContextReadRelease {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() { let _ = release.send(()); }
    }
}

struct ContextReadRegistration(PathBuf);

impl Drop for ContextReadRegistration {
    fn drop(&mut self) {
        engram_test_context_reads().lock().unwrap().remove(&self.0);
    }
}

#[test]
fn context_nudge_read_does_not_hold_direct_provider_delivery() {
    parked_context_send(false);
}

#[test]
fn context_nudge_read_does_not_hold_queued_provider_delivery() {
    parked_context_send(true);
}

fn parked_context_send(queued: bool) {
    for after_compaction in [false, true] {
        parked_context_send_case(queued, after_compaction, "current");
    }
}

#[test]
fn context_nudge_failed_or_superseded_background_read_keeps_the_nudge_pending() {
    for queued in [false, true] {
        for case in ["failed", "superseded", "identity"] {
            parked_context_send_case(queued, false, case);
        }
    }
}

#[test]
fn context_nudge_compaction_during_background_read_survives_cached_delivery() {
    for queued in [false, true] {
        parked_context_send_case(queued, false, "compacted");
    }
}

#[test]
fn context_nudge_rejected_observer_spawn_releases_its_refresh_reservation() {
    let (state, id, _) = fixture(false);
    state.capture_lifetime.stop();
    state.schedule_engram_context_nudge(&id);
    let inner = state.inner.lock().unwrap();
    let cache = &inner.sessions[inner.find_session_index(&id).unwrap()].engram;
    assert!(cache.context_nudge_pending);
    assert!(!cache.context_nudge_in_progress);
    assert!(cache.context_nudge_in_progress_generation.is_none());
}

#[test]
fn context_nudge_completed_claim_cannot_release_a_same_generation_retry() {
    let (state, id, root) = fixture(false);
    let _registration = ContextReadRegistration(root.clone());
    let claim = EngramContextNudgeReadClaim {
        state: state.clone(), session_id: id.clone(), generation: 1,
        armed: std::sync::atomic::AtomicBool::new(true),
    };
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        let cache = &mut inner.sessions[index].engram;
        cache.context_nudge_generation = 1;
        cache.context_nudge_in_progress = true;
        cache.context_nudge_in_progress_generation = Some(1);
        cache.context_refresh_needed = false;
    }
    engram_test_context_reads().lock().unwrap().insert(root.clone(),
        Box::new(|| Err("first read failed".into())));
    assert_eq!(state.prepare_engram_context_nudge_claiming_off_lock(&id, None, Some(&claim)),
        EngramContextNudgePreparation::Failed);
    // Keep the old claim alive after its helper released the reservation.
    // A retry now owns the SAME generation before that old claim drops.
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mut release = ContextReadRelease(Some(release_tx));
    engram_test_context_reads().lock().unwrap().insert(root.clone(), Box::new(move || {
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        Ok("retry context".into())
    }));
    state.schedule_engram_context_nudge(&id);
    recv_within_guard(&entered_rx, "same-generation retry enters transport").unwrap();
    drop(claim);
    {
        let inner = state.inner.lock().unwrap();
        let cache = &inner.sessions[inner.find_session_index(&id).unwrap()].engram;
        assert_eq!(cache.context_nudge_generation, 1);
        assert!(cache.context_nudge_in_progress, "old cleanup cannot release the live retry");
        assert_eq!(cache.context_nudge_in_progress_generation, Some(1));
    }
    release.0.take().unwrap().send(()).unwrap();
    wait_for_context_observers(&state);
    let inner = state.inner.lock().unwrap();
    let cache = &inner.sessions[inner.find_session_index(&id).unwrap()].engram;
    assert_eq!(cache.pending_context_nudge.as_deref(), Some("retry context"));
    assert!(!cache.context_nudge_in_progress);
}

#[test]
fn context_nudge_dispatch_refreshes_acp_declaration_before_runtime_creation() {
    for queued in [false, true] {
        let (state, id, root) = fixture(false);
        // No background worker can populate the declaration cache for us.
        state.capture_lifetime.stop();
        {
            let mut inner = state.inner.lock().unwrap();
            inner.engram_declared_project_ids.clear();
            inner.engram_declaration_checked_project_ids.clear();
            let index = inner.find_session_index(&id).unwrap();
            inner.sessions[index].session.agent = Agent::Cursor;
            inner.sessions[index].engram.context_nudge_pending = false;
        }
        for (phase, declared) in [true, false, true].into_iter().enumerate() {
            if declared { fs::write(root.join(".engram-project"), "fixture-ready\n").unwrap(); }
            else { fs::remove_file(root.join(".engram-project")).unwrap(); }
            let runtime_id = format!("declaration-{queued}-{phase}");
            let (runtime, input) = test_acp_runtime_handle(AcpAgent::Cursor, &runtime_id);
            state.install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
            let dispatch = dispatch_context_prompt(&state, &id, queued);
            {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
                assert_eq!(record.engram_mcp_installed.is_some(), declared,
                    "cold spawn and declaration changes must use the current caller-owned descriptor");
                assert!(matches!(&record.runtime, SessionRuntime::Acp(handle)
                    if handle.runtime_id == runtime_id), "a marker change replaces the old runtime");
            }
            deliver_turn_dispatch(&state, dispatch).unwrap();
            assert!(matches!(recv_within_guard(&input, "ACP receives prompt").unwrap(),
                AcpRuntimeCommand::Prompt(_)));
            let token = {
                let inner = state.inner.lock().unwrap();
                inner.sessions[inner.find_session_index(&id).unwrap()].runtime.runtime_token().unwrap()
            };
            state.finish_turn_ok_if_runtime_matches(&id, &token).unwrap();
        }
    }
}

#[test]
fn context_nudge_read_claim_unwinding_over_a_poisoned_state_mutex_does_not_panic_again() {
    let (state, id, _) = fixture(false);
    let poisoner = state.clone();
    assert!(std::thread::spawn(move || {
        let _inner = poisoner.inner.lock().unwrap();
        panic!("injected panic holding the state mutex");
    }).join().is_err());
    assert!(state.inner.inner.is_poisoned());
    let claim = EngramContextNudgeReadClaim {
        state: state.clone(), session_id: id, generation: 1,
        armed: std::sync::atomic::AtomicBool::new(true),
    };
    // A second panic from the claim's Drop would abort this whole test process.
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _claim = claim;
        panic!("injected worker panic");
    }));
    assert!(unwound.is_err());
}

#[test]
fn context_nudge_superseded_entry_and_panicking_read_release_only_their_reservations() {
    for panic_read in [false, true] {
        let (state, id, root) = fixture(false);
        let _registration = ContextReadRegistration(root.clone());
        let claim = EngramContextNudgeReadClaim {
            state: state.clone(), session_id: id.clone(), generation: 1,
            armed: std::sync::atomic::AtomicBool::new(true),
        };
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&id).unwrap();
            let cache = &mut inner.sessions[index].engram;
            cache.context_nudge_generation = 1;
            cache.context_nudge_in_progress = true;
            cache.context_nudge_in_progress_generation = Some(1);
            cache.context_refresh_needed = false;
            if !panic_read {
                cache.invalidate_context_nudge();
                // Model a successor which reserved before the old worker entered.
                cache.context_nudge_in_progress = true;
                cache.context_nudge_in_progress_generation = Some(cache.context_nudge_generation);
            }
        }
        engram_test_context_reads().lock().unwrap().insert(root,
            Box::new(|| panic!("injected transport panic")));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            claim.state.prepare_engram_context_nudge_claiming_off_lock(&claim.session_id, None, Some(&claim))
        }));
        if panic_read { assert!(outcome.is_err()); }
        else { assert_eq!(outcome.unwrap(), EngramContextNudgePreparation::Failed); }
        let inner = state.inner.lock().unwrap();
        let cache = &inner.sessions[inner.find_session_index(&id).unwrap()].engram;
        assert!(cache.context_nudge_pending);
        assert_eq!(cache.context_nudge_in_progress, !panic_read);
        assert_eq!(cache.context_nudge_in_progress_generation,
            if panic_read { None } else { Some(cache.context_nudge_generation) });
        assert!(cache.pending_context_nudge.is_none());
    }
}

#[test]
fn context_nudge_background_read_does_not_answer_an_explicit_compact_hook_ticket() {
    for queued in [false, true] { parked_context_send_case(queued, false, "hook"); }
}

fn dispatch_context_prompt(state: &AppState, id: &str, queued: bool) -> TurnDispatch {
    if queued {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(id).unwrap();
        queue_prompt_on_record(&mut inner.sessions[index], PendingPrompt {
            engram_interrupted: false, is_engram_retained: false, attachments: Vec::new(),
            id: Uuid::new_v4().to_string(), timestamp: stamp_now(), text: "Advisory context prompt.".into(),
            expanded_text: None, source: None,
        }, Vec::new());
        drop(inner);
        state.dispatch_next_queued_turn(id, false).unwrap().expect("queue stages")
    } else {
        match state.dispatch_turn(id, SendMessageRequest {
            text: "Advisory context prompt.".into(), expanded_text: None, attachments: Vec::new(),
            source_session_id: None, source_mailbox: None,
        }).unwrap() {
            DispatchTurnResult::Dispatched(dispatch) | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
            DispatchTurnResult::Queued => panic!("idle prompt stages"),
        }
    }
}

fn wait_for_context_observers(state: &AppState) {
    let captures = &state.capture_lifetime;
    let mut workers = captures.state.lock().unwrap();
    while workers.workers.iter().any(|worker| {
        worker.thread.thread().name() == Some("termal-context-nudge")
            && !worker.done.load(std::sync::atomic::Ordering::Acquire)
    }) {
        workers = captures.changed.wait(workers).unwrap();
    }
}

fn parked_context_send_case(queued: bool, after_compaction: bool, case: &str) {
    let (state, runtime_rx) = test_app_state_with_delegation_codex_runtime("parked-context-send");
    let (state, id, root) = fixture_with_state(state, false);
    if after_compaction {
        assert_eq!(state.prepare_engram_context_nudge_off_lock(&id), EngramContextNudgePreparation::Ready);
        delivered(&state, &id);
        state.mark_engram_context_nudge_pending(&id);
    }
    let text = format!("Deliver {queued}/{after_compaction} while context is unavailable.");
    if queued {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        queue_prompt_on_record(&mut inner.sessions[index], PendingPrompt {
            engram_interrupted: false, is_engram_retained: false, attachments: Vec::new(),
            id: "parked-context-head".into(), timestamp: stamp_now(), text: text.clone(),
            expanded_text: None, source: None,
        }, Vec::new());
    }
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mut release = ContextReadRelease(Some(release_tx));
    let send_thread = Arc::new(Mutex::new(None));
    let reading_thread = send_thread.clone();
    let failed = case == "failed";
    engram_test_context_reads().lock().unwrap().insert(root.clone(), Box::new(move || {
        let on_send_thread = Some(std::thread::current().id()) == *reading_thread.lock().unwrap();
        entered_tx.send(on_send_thread).unwrap();
        // The unchanged synchronous path reports its thread directly,
        // without a timing assertion or leaving the sender blocked on RED.
        if !on_send_thread { release_rx.recv().unwrap(); }
        if failed { Err("fixture context unavailable".into()) }
        else { Ok("fixture context from the parked read".into()) }
    }));
    let _registration = ContextReadRegistration(root.clone());
    let worker_state = state.clone();
    let worker_id = id.clone();
    let worker_text = text.clone();
    let worker = std::thread::spawn(move || {
        *send_thread.lock().unwrap() = Some(std::thread::current().id());
        let dispatch = if queued {
            worker_state.dispatch_next_queued_turn(&worker_id, false).unwrap().expect("queued prompt stages")
        } else {
            match worker_state.dispatch_turn(&worker_id, SendMessageRequest {
                text: worker_text, expanded_text: None, attachments: Vec::new(),
                source_session_id: None, source_mailbox: None,
            }).unwrap() {
                DispatchTurnResult::Dispatched(dispatch) | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
                DispatchTurnResult::Queued => panic!("idle prompt must stage"),
            }
        };
        deliver_turn_dispatch(&worker_state, dispatch).expect("provider enqueue succeeds");
    });
    let on_send_thread = entered_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
    if on_send_thread {
        worker.join().unwrap();
        panic!("context read ran synchronously on the {} thread", if queued { "queue-drain" } else { "direct-send" });
    }
    let prompt = recv_within_guard(&runtime_rx, "provider receives prompt before parked context is released").unwrap();
    assert!(matches!(prompt, CodexRuntimeCommand::Prompt { command, .. } if command.prompt.contains(&text)));
    worker.join().unwrap();
    {
        let inner = state.inner.lock().unwrap();
        let cache = &inner.sessions[inner.find_session_index(&id).unwrap()].engram;
        assert!(cache.pending_context_nudge.is_none());
        assert!(cache.context_nudge_pending || cache.context_nudge_in_progress,
            "the undelivered nudge is retained while its read is parked");
    }
    if case == "superseded" {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        inner.sessions[index].engram.invalidate_context_nudge();
    } else if case == "identity" {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        let generation = inner.sessions[index].engram.context_nudge_generation;
        assert!(inner.sessions[index].engram_mcp_installed.is_none());
        inner.sessions[index].session.model = "changed-during-background-read".into();
        assert_eq!(inner.sessions[index].engram.context_nudge_generation, generation,
            "identity-only staleness must exercise the unchanged-generation fence");
    } else if case == "hook" {
        let ticket = state.begin_engram_compact_hook_read(&id).expect("compact ticket");
        let token = {
            let inner = state.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&id).unwrap()].runtime.runtime_token().unwrap()
        };
        assert!(state.prepare_engram_compact_hook_context_off_lock(&id, &token, &ticket).is_none(),
            "an explicit hook cannot claim or reuse the background reader's page");
    } else if case == "compacted" {
        state.mark_engram_context_nudge_pending(&id);
    }
    release.0.take().unwrap().send(()).unwrap();
    wait_for_context_observers(&state);
    let expected = if matches!(case, "current" | "compacted" | "hook") { "fixture context from the parked read" }
        else { "fresh context after the failed or superseded read" };
    if matches!(case, "failed" | "superseded" | "identity") {
        {
            let inner = state.inner.lock().unwrap();
            let cache = &inner.sessions[inner.find_session_index(&id).unwrap()].engram;
            assert!(cache.pending_context_nudge.is_none(), "a failed or stale page is not cached");
            assert!(cache.context_nudge_pending, "the nudge remains pending for retry");
            assert!(!cache.context_nudge_in_progress, "the failed reservation is released");
        }
        engram_test_context_reads().lock().unwrap().insert(root.clone(),
            Box::new(move || Ok(expected.into())));
        state.schedule_engram_context_nudge(&id);
        wait_for_context_observers(&state);
    }
    {
        let inner = state.inner.lock().unwrap();
        let cache = &inner.sessions[inner.find_session_index(&id).unwrap()].engram;
        assert_eq!(cache.pending_context_nudge.as_deref(), Some(expected));
    }
    let token = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&id).unwrap()].runtime.runtime_token().unwrap()
    };
    state.finish_turn_ok_if_runtime_matches(&id, &token).unwrap();
    let dispatch = match state.dispatch_turn(&id, SendMessageRequest {
        text: "Receive the retained advisory context.".into(), expanded_text: None,
        attachments: Vec::new(), source_session_id: None, source_mailbox: None,
    }).unwrap() {
        DispatchTurnResult::Dispatched(dispatch) => dispatch,
        _ => panic!("the next idle prompt stages with cached context"),
    };
    deliver_turn_dispatch(&state, dispatch).expect("cached context reaches provider");
    assert!(matches!(recv_within_guard(&runtime_rx, "cached context delivery").unwrap(),
        CodexRuntimeCommand::Prompt { command, .. }
        if command.prompt.contains(expected) && command.prompt.contains("Receive the retained advisory context.")));
    let inner = state.inner.lock().unwrap();
    let cache = &inner.sessions[inner.find_session_index(&id).unwrap()].engram;
    assert!(cache.pending_context_nudge.is_none(), "only runtime acceptance consumes the page");
    assert_eq!(cache.context_nudge_pending, matches!(case, "compacted" | "hook"),
        "a compaction requested during the read survives delivery of that page");
    assert_eq!(delivery_cursor(&root), 0, "local runtime acknowledgement never advances Engram delivery");
}
