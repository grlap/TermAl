// Agent readiness cache behavior.
//
// "Agent readiness" is TermAl's per-agent availability snapshot: for each
// supported agent (Claude, Codex, Gemini, Cursor) it reports whether the
// local CLI is installed, reachable on PATH, and correctly versioned, plus
// any soft warnings (e.g. Codex on Windows asking the user to route shell
// commands through WSL). Determining readiness requires spawning `which
// claude` / `which codex` / etc. to probe the filesystem, which is
// measurably slow on Windows — so the result is cached on `AppState` and
// reused across hot-path snapshots rather than recomputed on every
// `/api/state` call. The cache is refreshed lazily via TTL expiry and
// eagerly via explicit invalidation whenever app settings change or a new
// session is created. The wire contract requires `AgentReadiness` to
// serialize `warning_detail` as camelCase `warningDetail`, omitting the
// field entirely when `None`.

use super::*;

// Pins codex_windows_shell_warning() to the current platform: Some(_) mentioning
// "WSL" on Windows, None elsewhere. Guards against accidentally surfacing the
// warning on non-Windows builds or dropping the WSL hint on Windows.
#[test]
fn codex_windows_shell_warning_matches_platform() {
    if cfg!(windows) {
        let warning = codex_windows_shell_warning()
            .expect("Windows builds should surface the Codex shell warning");
        assert!(warning.contains("WSL"));
    } else {
        assert_eq!(codex_windows_shell_warning(), None);
    }
}

// Pins codex_agent_readiness() to stay in sync with runtime CLI resolution:
// when a command_path is resolved, status is Ready and non-blocking with the
// Windows warning attached; when missing, status is Missing/blocking with
// install guidance. Guards against the two halves drifting apart.
#[test]
fn codex_agent_readiness_matches_runtime_resolution() {
    let readiness = codex_agent_readiness();

    assert!(matches!(readiness.agent, Agent::Codex));
    match readiness.command_path.as_deref() {
        Some(command_path) => {
            assert!(matches!(readiness.status, AgentReadinessStatus::Ready));
            assert!(!readiness.blocking);
            assert!(readiness.detail.contains(command_path));
            assert_eq!(readiness.warning_detail, codex_windows_shell_warning());
        }
        None => {
            assert!(matches!(readiness.status, AgentReadinessStatus::Missing));
            assert!(readiness.blocking);
            assert!(readiness.detail.contains("Install the `codex` CLI"));
            assert_eq!(readiness.warning_detail, None);
        }
    }
}

fn sentinel_agent_readiness_snapshot() -> Vec<AgentReadiness> {
    vec![
        AgentReadiness {
            agent: Agent::Codex,
            status: AgentReadinessStatus::NeedsSetup,
            blocking: true,
            detail: "sentinel codex readiness".to_owned(),
            warning_detail: Some("sentinel codex warning".to_owned()),
            command_path: Some("sentinel-codex".to_owned()),
        },
        AgentReadiness {
            agent: Agent::Cursor,
            status: AgentReadinessStatus::Missing,
            blocking: true,
            detail: "sentinel cursor readiness".to_owned(),
            warning_detail: None,
            command_path: Some("sentinel-cursor".to_owned()),
        },
        AgentReadiness {
            agent: Agent::Gemini,
            status: AgentReadinessStatus::Ready,
            blocking: false,
            detail: "sentinel gemini readiness".to_owned(),
            warning_detail: Some("sentinel gemini warning".to_owned()),
            command_path: Some("sentinel-gemini".to_owned()),
        },
    ]
}

// Pins snapshot_from_inner() to always return the cached readiness even when
// the TTL has expired, because it runs under the `inner` mutex where blocking
// filesystem probes are unsafe. Guards against someone adding a refresh call
// that would deadlock or stall every `/api/state` request.
#[test]
fn snapshot_from_inner_uses_cached_agent_readiness_when_cache_is_stale() {
    let state = test_app_state();
    let sentinel = sentinel_agent_readiness_snapshot();
    {
        let mut cache = state
            .agent_readiness_cache
            .write()
            .expect("agent readiness cache should not be poisoned");
        // Only expire the TTL — `invalidated` stays false so the test isolates
        // the TTL-stale path without conflating the two staleness signals.
        *cache = AgentReadinessCache {
            snapshot: sentinel.clone(),
            expires_at: Instant::now() - AGENT_READINESS_CACHE_TTL,
            invalidated: false,
        };
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let snapshot = state.snapshot_from_inner(&inner);
    drop(inner);

    assert_eq!(snapshot.agent_readiness, sentinel);
}

// Pins update_app_settings() to refresh the readiness cache and return a
// fresh (non-sentinel) snapshot, and to clear the `invalidated` flag once
// refreshed. Guards against app-settings responses leaking stale readiness
// to the client after preferences change.
#[test]
fn update_app_settings_refreshes_invalidated_agent_readiness_cache() {
    let state = test_app_state();
    let sentinel = sentinel_agent_readiness_snapshot();
    {
        let mut cache = state
            .agent_readiness_cache
            .write()
            .expect("agent readiness cache should not be poisoned");
        *cache = AgentReadinessCache::fresh(sentinel.clone());
    }

    let next_reasoning_effort = {
        let current = state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .preferences
            .default_codex_reasoning_effort;
        if current == CodexReasoningEffort::High {
            CodexReasoningEffort::Medium
        } else {
            CodexReasoningEffort::High
        }
    };

    let updated = state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_reasoning_effort: Some(next_reasoning_effort),
            default_claude_approval_mode: None,
            default_claude_effort: None,
            remotes: None,
        })
        .expect("app settings should update");

    assert_ne!(updated.agent_readiness, sentinel);

    let cache = state
        .agent_readiness_cache
        .read()
        .expect("agent readiness cache should not be poisoned");
    assert_eq!(cache.snapshot, updated.agent_readiness);
    assert!(!cache.invalidated);
}

// Pins snapshot() (the outer, non-locked variant) to refresh readiness on
// TTL expiry even without explicit invalidation, replacing the sentinel with
// a real collect_agent_readiness() result. Guards against the TTL becoming
// purely cosmetic while the cache is served forever.
#[test]
fn snapshot_refreshes_agent_readiness_when_ttl_expires() {
    let state = test_app_state();
    let sentinel = sentinel_agent_readiness_snapshot();
    {
        let mut cache = state
            .agent_readiness_cache
            .write()
            .expect("agent readiness cache should not be poisoned");
        *cache = AgentReadinessCache {
            snapshot: sentinel.clone(),
            expires_at: Instant::now() - AGENT_READINESS_CACHE_TTL,
            invalidated: false,
        };
    }

    let snapshot = state.snapshot();

    // `snapshot()` should have refreshed the cache, producing readiness from a
    // real `collect_agent_readiness` call rather than returning the sentinel.
    assert_ne!(snapshot.agent_readiness, sentinel);

    let cache = state
        .agent_readiness_cache
        .read()
        .expect("agent readiness cache should not be poisoned");
    assert_eq!(cache.snapshot, snapshot.agent_readiness);
    assert!(!cache.invalidated);
}

// Pins snapshot_from_inner() to return the cached readiness even when the
// cache is explicitly invalidated but still within TTL. Together with the
// TTL-stale variant above, guards the invariant that snapshot_from_inner
// never refreshes under any staleness signal.
#[test]
fn snapshot_from_inner_uses_cached_agent_readiness_when_cache_is_invalidated() {
    let state = test_app_state();
    let sentinel = sentinel_agent_readiness_snapshot();
    {
        let mut cache = state
            .agent_readiness_cache
            .write()
            .expect("agent readiness cache should not be poisoned");
        *cache = AgentReadinessCache {
            snapshot: sentinel.clone(),
            // TTL still valid, but explicitly invalidated.
            expires_at: Instant::now() + AGENT_READINESS_CACHE_TTL,
            invalidated: true,
        };
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let snapshot = state.snapshot_from_inner(&inner);
    drop(inner);

    assert_eq!(snapshot.agent_readiness, sentinel);
}

// Pins update_app_settings() so the state event it publishes shares the same
// revision and agent_readiness as the returned API response. Guards against
// the stale-SSE / duplicate-revision race where subscribers would see a
// different readiness snapshot than the HTTP caller.
#[test]
fn update_app_settings_sse_matches_api_response() {
    let state = test_app_state();
    let sentinel = sentinel_agent_readiness_snapshot();
    {
        let mut cache = state
            .agent_readiness_cache
            .write()
            .expect("agent readiness cache should not be poisoned");
        *cache = AgentReadinessCache::fresh(sentinel.clone());
    }

    let mut state_events = state.subscribe_events();
    let next_reasoning_effort = {
        let current = state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .preferences
            .default_codex_reasoning_effort;
        if current == CodexReasoningEffort::High {
            CodexReasoningEffort::Medium
        } else {
            CodexReasoningEffort::High
        }
    };
    let api_response = state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_reasoning_effort: Some(next_reasoning_effort),
            default_claude_approval_mode: None,
            default_claude_effort: None,
            remotes: None,
        })
        .expect("app settings should update");

    let published: StateResponse = serde_json::from_str(
        &state_events
            .try_recv()
            .expect("update_app_settings should publish a state snapshot"),
    )
    .expect("SSE state event should decode");

    assert_eq!(published.revision, api_response.revision);
    assert_eq!(published.agent_readiness, api_response.agent_readiness);
}

// Pins create_session() to pre-refresh the readiness cache before mutating
// state, so the next full snapshot is fresh. Also confirms the create path
// publishes a SessionCreated delta (not a full state event) and that the
// delta's revision and session_id match the API response.
#[test]
fn create_session_refreshes_agent_readiness_cache() {
    let state = test_app_state();
    let sentinel = sentinel_agent_readiness_snapshot();
    {
        let mut cache = state
            .agent_readiness_cache
            .write()
            .expect("agent readiness cache should not be poisoned");
        *cache = AgentReadinessCache::fresh(sentinel.clone());
    }

    let mut state_events = state.subscribe_events();
    let mut delta_events = state.subscribe_delta_events();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Cache Test".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should be created");

    // The create path pre-refreshes readiness before mutating state so the next
    // full snapshot is fresh even though create itself now publishes a small
    // session-created delta instead of a full state event.
    assert_ne!(state.cached_agent_readiness(), sentinel);

    assert!(state_events.try_recv().is_err());
    let published: DeltaEvent = serde_json::from_str(
        &delta_events
            .try_recv()
            .expect("create_session should publish a delta"),
    )
    .expect("SSE delta event should decode");
    match published {
        DeltaEvent::SessionCreated {
            revision,
            session_id,
            session,
        } => {
            assert_eq!(revision, created.revision);
            assert_eq!(session_id, created.session_id);
            assert_eq!(session.id, created.session_id);
        }
        _ => panic!("expected sessionCreated delta"),
    }
}

// Pins the AgentReadiness serde contract: `warning_detail` must serialize as
// camelCase `warningDetail` when set, and must be omitted entirely when None.
// Guards against a rename or serde-attribute change breaking the TypeScript
// client that consumes `/api/state`.
#[test]
fn agent_readiness_serialization_emits_warning_detail_camel_case() {
    let readiness = AgentReadiness {
        agent: Agent::Codex,
        status: AgentReadinessStatus::Ready,
        blocking: false,
        detail: "Codex CLI is available.".to_owned(),
        warning_detail: Some("Use WSL for shell commands on Windows.".to_owned()),
        command_path: Some("codex".to_owned()),
    };

    let serialized =
        serde_json::to_value(&readiness).expect("AgentReadiness should serialize to JSON");
    assert_eq!(
        serialized.pointer("/warningDetail"),
        Some(&Value::String(
            "Use WSL for shell commands on Windows.".to_owned()
        ))
    );

    let serialized_without_warning = serde_json::to_value(AgentReadiness {
        warning_detail: None,
        ..readiness
    })
    .expect("AgentReadiness without warning detail should serialize to JSON");
    assert_eq!(serialized_without_warning.get("warningDetail"), None);
}
