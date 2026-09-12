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
        let expected_generation = if after_compaction {
            "termal-2"
        } else {
            "termal-1"
        };
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
