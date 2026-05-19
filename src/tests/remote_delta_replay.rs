//! Remote delta replay-key and replay-cache tests.
//!
//! Split out of remote.rs so the remote integration test module can keep
//! transport, hydration, orchestrator, and replay-cache coverage in focused
//! files.

use super::remote::{remote_command_message, remote_parallel_agents_message, remote_text_message};
use super::*;

#[test]
fn remote_text_delta_exact_replay_is_skipped_for_loaded_proxy_session() {
    let state = test_app_state();
    let remote = local_replay_test_remote();
    seed_remote_proxy_session_via_apply_delta(
        &state,
        &remote,
        vec![remote_text_message("remote-message-1", "Hello")],
    );

    assert_delta_publishes_once_then_replay_skips(
        &state,
        &remote,
        || DeltaEvent::TextDelta {
            revision: 3,
            session_id: "remote-session-1".to_owned(),
            message_id: "remote-message-1".to_owned(),
            message_index: 0,
            message_count: 1,
            delta: " world".to_owned(),
            preview: Some("Hello world".to_owned()),
            session_mutation_stamp: Some(11),
        },
        |published| match published {
            DeltaEvent::TextDelta { message_id, .. } => {
                assert_eq!(message_id, "remote-message-1");
            }
            _ => panic!("expected TextDelta delta"),
        },
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(matches!(
        &record.session.messages[0],
        Message::Text { text, .. } if text == "Hello world"
    ));
    assert_eq!(record.session.preview, "Hello world");
    assert_eq!(record.session.session_mutation_stamp, Some(11));
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&3));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

pub(super) fn local_replay_test_remote() -> RemoteConfig {
    RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    }
}

// Seeds through `apply_remote_delta_event(SessionCreated)` so replay-cache
// tests exercise the same apply/note path as live remote deltas.
fn seed_remote_proxy_session_via_apply_delta(
    state: &AppState,
    remote: &RemoteConfig,
    messages: Vec<Message>,
) {
    create_test_remote_project(
        state,
        remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.preview = "seed preview".to_owned();
    full_remote_session.messages = messages;
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = full_remote_session.messages.len() as u32;
    full_remote_session.session_mutation_stamp = Some(10);

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: full_remote_session.id.clone(),
                session: full_remote_session,
            },
        )
        .expect("remote full session create delta should apply");
}

fn assert_delta_publishes_once_then_replay_skips<F>(
    state: &AppState,
    remote: &RemoteConfig,
    event: impl Fn() -> DeltaEvent,
    assert_published_delta: F,
) where
    F: FnOnce(DeltaEvent),
{
    // This helper is only for variants that publish exactly one localized
    // delta on first apply. Zero-publish variants such as `CodexUpdated` need
    // their own shape.
    let mut delta_receiver = state.subscribe_delta_events();
    state
        .apply_remote_delta_event(&remote.id, event())
        .expect("first remote delta should apply");
    let published_payload = delta_receiver
        .try_recv()
        .expect("first remote delta should publish");
    let published_delta: DeltaEvent =
        serde_json::from_str(&published_payload).expect("published delta should decode");
    assert_published_delta(published_delta);
    assert!(
        delta_receiver.try_recv().is_err(),
        "first remote delta should publish exactly one localized delta"
    );

    state
        .apply_remote_delta_event(&remote.id, event())
        .expect("exact remote delta replay should be consumed");
    assert!(
        delta_receiver.try_recv().is_err(),
        "exact same-revision replay should not publish a duplicate delta"
    );
}

#[test]
fn remote_delta_replay_cache_skips_exact_replays_for_remaining_variants() {
    {
        let state = test_app_state();
        let remote = local_replay_test_remote();
        seed_remote_proxy_session_via_apply_delta(&state, &remote, Vec::new());

        assert_delta_publishes_once_then_replay_skips(
            &state,
            &remote,
            || DeltaEvent::MessageCreated {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "remote-message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message("remote-message-1", "Created once."),
                preview: "Created once.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: Some(11),
            },
            |published| match published {
                DeltaEvent::MessageCreated { message_id, .. } => {
                    assert_eq!(message_id, "remote-message-1");
                }
                _ => panic!("expected MessageCreated delta"),
            },
        );
        let _ = fs::remove_file(state.persistence_path.as_path());
    }

    {
        let state = test_app_state();
        let remote = local_replay_test_remote();
        seed_remote_proxy_session_via_apply_delta(
            &state,
            &remote,
            vec![remote_text_message("remote-message-1", "Before update.")],
        );

        assert_delta_publishes_once_then_replay_skips(
            &state,
            &remote,
            || DeltaEvent::MessageUpdated {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "remote-message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message("remote-message-1", "After update."),
                preview: "After update.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: Some(11),
            },
            |published| match published {
                DeltaEvent::MessageUpdated { message_id, .. } => {
                    assert_eq!(message_id, "remote-message-1");
                }
                _ => panic!("expected MessageUpdated delta"),
            },
        );
        let _ = fs::remove_file(state.persistence_path.as_path());
    }

    {
        let state = test_app_state();
        let remote = local_replay_test_remote();
        seed_remote_proxy_session_via_apply_delta(
            &state,
            &remote,
            vec![remote_text_message("remote-message-1", "Before replace.")],
        );

        assert_delta_publishes_once_then_replay_skips(
            &state,
            &remote,
            || DeltaEvent::TextReplace {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "remote-message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                text: "After replace.".to_owned(),
                preview: Some("After replace.".to_owned()),
                session_mutation_stamp: Some(11),
            },
            |published| match published {
                DeltaEvent::TextReplace { message_id, .. } => {
                    assert_eq!(message_id, "remote-message-1");
                }
                _ => panic!("expected TextReplace delta"),
            },
        );
        let _ = fs::remove_file(state.persistence_path.as_path());
    }

    {
        let state = test_app_state();
        let remote = local_replay_test_remote();
        seed_remote_proxy_session_via_apply_delta(
            &state,
            &remote,
            vec![remote_command_message(
                "remote-command-1",
                "cargo check",
                "checking",
            )],
        );

        assert_delta_publishes_once_then_replay_skips(
            &state,
            &remote,
            || DeltaEvent::CommandUpdate {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "remote-command-1".to_owned(),
                message_index: 0,
                message_count: 1,
                command: "cargo check".to_owned(),
                command_language: Some("shell".to_owned()),
                output: "finished".to_owned(),
                output_language: Some("text".to_owned()),
                status: CommandStatus::Success,
                preview: "cargo check finished".to_owned(),
                session_mutation_stamp: Some(11),
            },
            |published| match published {
                DeltaEvent::CommandUpdate { message_id, .. } => {
                    assert_eq!(message_id, "remote-command-1");
                }
                _ => panic!("expected CommandUpdate delta"),
            },
        );
        let _ = fs::remove_file(state.persistence_path.as_path());
    }

    {
        let state = test_app_state();
        let remote = local_replay_test_remote();
        seed_remote_proxy_session_via_apply_delta(
            &state,
            &remote,
            vec![remote_parallel_agents_message(
                "remote-parallel-1",
                Vec::new(),
            )],
        );

        assert_delta_publishes_once_then_replay_skips(
            &state,
            &remote,
            || DeltaEvent::ParallelAgentsUpdate {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                message_id: "remote-parallel-1".to_owned(),
                message_index: 0,
                message_count: 1,
                agents: vec![ParallelAgentProgress {
                    detail: Some("working".to_owned()),
                    id: "agent-1".to_owned(),
                    source: ParallelAgentSource::Tool,
                    status: ParallelAgentStatus::Running,
                    title: "Agent one".to_owned(),
                }],
                preview: "agent working".to_owned(),
                session_mutation_stamp: Some(11),
            },
            |published| match published {
                DeltaEvent::ParallelAgentsUpdate { message_id, .. } => {
                    assert_eq!(message_id, "remote-parallel-1");
                }
                _ => panic!("expected ParallelAgentsUpdate delta"),
            },
        );
        let _ = fs::remove_file(state.persistence_path.as_path());
    }

    {
        let state = test_app_state();
        let remote = local_replay_test_remote();
        create_test_remote_project(
            &state,
            &remote,
            "/remote/repo",
            "Remote Project",
            "remote-project-1",
        );
        let remote_state = sample_remote_orchestrator_state(
            "remote-project-1",
            "/remote/repo",
            1,
            OrchestratorInstanceStatus::Running,
        );

        assert_delta_publishes_once_then_replay_skips(
            &state,
            &remote,
            || DeltaEvent::OrchestratorsUpdated {
                revision: 3,
                orchestrators: remote_state.orchestrators.clone(),
                sessions: remote_state.sessions.clone(),
            },
            |published| match published {
                DeltaEvent::OrchestratorsUpdated { orchestrators, .. } => {
                    assert_eq!(orchestrators.len(), 1);
                }
                _ => panic!("expected OrchestratorsUpdated delta"),
            },
        );
        let _ = fs::remove_file(state.persistence_path.as_path());
    }

    {
        let state = test_app_state();
        let remote = local_replay_test_remote();
        let codex_updated = || DeltaEvent::CodexUpdated {
            revision: 3,
            codex: CodexState {
                rate_limits: None,
                notices: vec![CodexNotice {
                    kind: CodexNoticeKind::RuntimeNotice,
                    level: CodexNoticeLevel::Info,
                    title: "remote notice".to_owned(),
                    detail: "detail".to_owned(),
                    timestamp: "2026-04-05 10:00:00".to_owned(),
                    code: None,
                }],
            },
        };
        let replay_key = AppState::remote_delta_replay_key(&remote.id, &codex_updated())
            .expect("codex replay key should serialize");

        state
            .apply_remote_delta_event(&remote.id, codex_updated())
            .expect("CodexUpdated delta should be consumed");
        assert!(
            state.should_skip_remote_applied_delta_replay(&Some(replay_key)),
            "CodexUpdated must seed the replay cache even though it does not publish local state"
        );

        state
            .apply_remote_delta_event(&remote.id, codex_updated())
            .expect("exact CodexUpdated replay should be consumed");
        let _ = fs::remove_file(state.persistence_path.as_path());
    }
}

#[test]
fn remote_delta_replay_key_includes_state_mutating_payload_fields() {
    let remote_id = "ssh-lab";
    let replay_key = |event: DeltaEvent| {
        AppState::remote_delta_replay_key(remote_id, &event)
            .expect("sample replay payload should serialize")
    };
    let remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    let mut session_a = remote_state.sessions[0].clone();
    let mut session_b = session_a.clone();
    session_a.session_mutation_stamp = Some(10);
    session_b.session_mutation_stamp = Some(10);
    session_b.preview = "different session preview".to_owned();
    assert_eq!(
        replay_key(DeltaEvent::SessionCreated {
            revision: 3,
            session_id: session_a.id.clone(),
            session: session_a.clone(),
        }),
        replay_key(DeltaEvent::SessionCreated {
            revision: 3,
            session_id: session_a.id.clone(),
            session: session_a.clone(),
        }),
        "identical SessionCreated inputs must produce stable keys"
    );
    let mut session_with_remote_a = session_a.clone();
    let mut session_with_remote_b = session_a.clone();
    session_with_remote_a.remote_id = Some("attacker-remote-a".to_owned());
    session_with_remote_b.remote_id = Some("attacker-remote-b".to_owned());
    assert_eq!(
        replay_key(DeltaEvent::SessionCreated {
            revision: 3,
            session_id: session_with_remote_a.id.clone(),
            session: session_with_remote_a,
        }),
        replay_key(DeltaEvent::SessionCreated {
            revision: 3,
            session_id: session_with_remote_b.id.clone(),
            session: session_with_remote_b,
        }),
        "SessionCreated replay identity must ignore inbound remote_id because localization discards it"
    );
    assert_ne!(
        replay_key(DeltaEvent::SessionCreated {
            revision: 3,
            session_id: session_a.id.clone(),
            session: session_a,
        }),
        replay_key(DeltaEvent::SessionCreated {
            revision: 3,
            session_id: session_b.id.clone(),
            session: session_b,
        }),
        "SessionCreated replay identity must include the session payload"
    );

    let message_created = |message_text: &str, preview: &str| DeltaEvent::MessageCreated {
        revision: 4,
        session_id: "remote-session-1".to_owned(),
        message_id: "remote-message-1".to_owned(),
        message_index: 0,
        message_count: 1,
        message: remote_text_message("remote-message-1", message_text),
        preview: preview.to_owned(),
        status: SessionStatus::Idle,
        session_mutation_stamp: Some(11),
    };
    assert_eq!(
        replay_key(message_created("same text", "same preview")),
        replay_key(message_created("same text", "same preview")),
        "identical MessageCreated inputs must produce stable keys"
    );
    assert_ne!(
        replay_key(message_created("first text", "same preview")),
        replay_key(message_created("second text", "same preview")),
        "MessageCreated replay identity must include the message payload"
    );

    let message_updated = |message_text: &str, preview: &str| DeltaEvent::MessageUpdated {
        revision: 4,
        session_id: "remote-session-1".to_owned(),
        message_id: "remote-message-1".to_owned(),
        message_index: 0,
        message_count: 1,
        message: remote_text_message("remote-message-1", message_text),
        preview: preview.to_owned(),
        status: SessionStatus::Idle,
        session_mutation_stamp: Some(11),
    };
    assert_eq!(
        replay_key(message_updated("same text", "same preview")),
        replay_key(message_updated("same text", "same preview")),
        "identical MessageUpdated inputs must produce stable keys"
    );
    assert_ne!(
        replay_key(message_updated("first text", "same preview")),
        replay_key(message_updated("second text", "same preview")),
        "MessageUpdated replay identity must include the message payload"
    );

    let text_delta = |delta: &str, preview: Option<&str>| DeltaEvent::TextDelta {
        revision: 5,
        session_id: "remote-session-1".to_owned(),
        message_id: "remote-message-1".to_owned(),
        message_index: 0,
        message_count: 1,
        delta: delta.to_owned(),
        preview: preview.map(str::to_owned),
        session_mutation_stamp: Some(12),
    };
    assert_eq!(
        replay_key(text_delta(" same delta", Some("same preview"))),
        replay_key(text_delta(" same delta", Some("same preview"))),
        "identical TextDelta inputs must produce stable keys"
    );
    assert_ne!(
        replay_key(text_delta(" same delta", Some("first preview"))),
        replay_key(text_delta(" same delta", Some("second preview"))),
        "TextDelta replay identity must include preview changes"
    );

    let text_replace = |text: &str, preview: Option<&str>| DeltaEvent::TextReplace {
        revision: 5,
        session_id: "remote-session-1".to_owned(),
        message_id: "remote-message-1".to_owned(),
        message_index: 0,
        message_count: 1,
        text: text.to_owned(),
        preview: preview.map(str::to_owned),
        session_mutation_stamp: Some(12),
    };
    assert_eq!(
        replay_key(text_replace("same replacement", Some("same preview"))),
        replay_key(text_replace("same replacement", Some("same preview"))),
        "identical TextReplace inputs must produce stable keys"
    );
    assert_ne!(
        replay_key(text_replace("first replacement", Some("first replacement"))),
        replay_key(text_replace(
            "second replacement",
            Some("second replacement")
        )),
        "TextReplace replay identity must include replacement text"
    );

    let command_update = |output: &str, preview: &str| DeltaEvent::CommandUpdate {
        revision: 6,
        session_id: "remote-session-1".to_owned(),
        message_id: "remote-command-1".to_owned(),
        message_index: 0,
        message_count: 1,
        command: "cargo test".to_owned(),
        command_language: Some("shell".to_owned()),
        output: output.to_owned(),
        output_language: Some("text".to_owned()),
        status: CommandStatus::Running,
        preview: preview.to_owned(),
        session_mutation_stamp: Some(13),
    };
    assert_eq!(
        replay_key(command_update("same output", "same preview")),
        replay_key(command_update("same output", "same preview")),
        "identical CommandUpdate inputs must produce stable keys"
    );
    assert_ne!(
        replay_key(command_update("first output", "first output")),
        replay_key(command_update("second output", "second output")),
        "CommandUpdate replay identity must include command output"
    );

    let parallel_agents_update = |detail: &str, preview: &str| DeltaEvent::ParallelAgentsUpdate {
        revision: 7,
        session_id: "remote-session-1".to_owned(),
        message_id: "remote-parallel-1".to_owned(),
        message_index: 0,
        message_count: 1,
        agents: vec![ParallelAgentProgress {
            detail: Some(detail.to_owned()),
            id: "agent-1".to_owned(),
            source: ParallelAgentSource::Tool,
            status: ParallelAgentStatus::Running,
            title: "Agent one".to_owned(),
        }],
        preview: preview.to_owned(),
        session_mutation_stamp: Some(14),
    };
    assert_eq!(
        replay_key(parallel_agents_update("same detail", "same preview")),
        replay_key(parallel_agents_update("same detail", "same preview")),
        "identical ParallelAgentsUpdate inputs must produce stable keys"
    );
    assert_ne!(
        replay_key(parallel_agents_update("first detail", "first detail")),
        replay_key(parallel_agents_update("second detail", "second detail")),
        "ParallelAgentsUpdate replay identity must include agent detail and preview"
    );

    let orchestrators_updated = |status: OrchestratorInstanceStatus| {
        let mut orchestrator = remote_state.orchestrators[0].clone();
        orchestrator.status = status;
        DeltaEvent::OrchestratorsUpdated {
            revision: 8,
            orchestrators: vec![orchestrator],
            sessions: Vec::new(),
        }
    };
    assert_eq!(
        replay_key(orchestrators_updated(OrchestratorInstanceStatus::Running)),
        replay_key(orchestrators_updated(OrchestratorInstanceStatus::Running)),
        "identical OrchestratorsUpdated inputs must produce stable keys"
    );
    assert_ne!(
        replay_key(orchestrators_updated(OrchestratorInstanceStatus::Running)),
        replay_key(orchestrators_updated(OrchestratorInstanceStatus::Paused)),
        "OrchestratorsUpdated replay identity must include orchestrator payloads"
    );
    let orchestrators_updated_with_session_preview = |preview: &str| {
        let mut orchestrator = remote_state.orchestrators[0].clone();
        orchestrator.status = OrchestratorInstanceStatus::Running;
        let mut session = remote_state.sessions[0].clone();
        session.preview = preview.to_owned();
        DeltaEvent::OrchestratorsUpdated {
            revision: 8,
            orchestrators: vec![orchestrator],
            sessions: vec![session],
        }
    };
    assert_ne!(
        replay_key(orchestrators_updated_with_session_preview("first preview")),
        replay_key(orchestrators_updated_with_session_preview("second preview")),
        "OrchestratorsUpdated replay identity must include session payloads"
    );
    let orchestrators_updated_with_session_remote_id = |remote_id: &str| {
        let mut orchestrator = remote_state.orchestrators[0].clone();
        orchestrator.status = OrchestratorInstanceStatus::Running;
        let mut session = remote_state.sessions[0].clone();
        session.remote_id = Some(remote_id.to_owned());
        DeltaEvent::OrchestratorsUpdated {
            revision: 8,
            orchestrators: vec![orchestrator],
            sessions: vec![session],
        }
    };
    assert_eq!(
        replay_key(orchestrators_updated_with_session_remote_id(
            "attacker-remote-a"
        )),
        replay_key(orchestrators_updated_with_session_remote_id(
            "attacker-remote-b"
        )),
        "OrchestratorsUpdated replay identity must ignore inbound session remote_id because localization discards it"
    );
    let mut inbound_session_with_remote_id = remote_state.sessions[0].clone();
    inbound_session_with_remote_id.remote_id = Some("attacker-remote-a".to_owned());
    let localized_session = localize_remote_session(
        remote_id,
        "local-session-1",
        Some("local-project-1".to_owned()),
        &inbound_session_with_remote_id,
    );
    assert!(
        localized_session.remote_id.is_none(),
        "localized OrchestratorsUpdated sessions must strip untrusted inbound remote_id before local emission"
    );

    let codex_updated = |title: &str| DeltaEvent::CodexUpdated {
        revision: 9,
        codex: CodexState {
            rate_limits: None,
            notices: vec![CodexNotice {
                kind: CodexNoticeKind::RuntimeNotice,
                level: CodexNoticeLevel::Info,
                title: title.to_owned(),
                detail: "detail".to_owned(),
                timestamp: "2026-04-05 10:00:00".to_owned(),
                code: None,
            }],
        },
    };
    assert_eq!(
        replay_key(codex_updated("same notice")),
        replay_key(codex_updated("same notice")),
        "identical CodexUpdated inputs must produce stable keys"
    );
    assert_ne!(
        replay_key(codex_updated("first notice")),
        replay_key(codex_updated("second notice")),
        "CodexUpdated replay identity must include the codex payload"
    );
}

#[test]
fn remote_delta_replay_key_isolates_individual_fingerprinted_fields() {
    let remote_id = "ssh-lab";
    let replay_key = |event: DeltaEvent| {
        AppState::remote_delta_replay_key(remote_id, &event)
            .expect("sample replay payloads should serialize")
    };
    let replay_key_for_remote = |remote_id: &str, event: DeltaEvent| {
        AppState::remote_delta_replay_key(remote_id, &event)
            .expect("sample replay payloads should serialize")
    };

    let text_delta = |delta: &str, preview: Option<&str>| DeltaEvent::TextDelta {
        revision: 5,
        session_id: "remote-session-1".to_owned(),
        message_id: "remote-message-1".to_owned(),
        message_index: 0,
        message_count: 1,
        delta: delta.to_owned(),
        preview: preview.map(str::to_owned),
        session_mutation_stamp: Some(12),
    };
    assert_eq!(
        replay_key(text_delta(" same delta", Some("preview"))),
        replay_key(text_delta(" same delta", Some("preview"))),
        "identical replay inputs must produce stable keys"
    );
    assert_ne!(
        replay_key_for_remote("ssh-lab-a", text_delta(" same delta", Some("preview"))),
        replay_key_for_remote("ssh-lab-b", text_delta(" same delta", Some("preview"))),
        "replay keys must remain scoped by remote id"
    );
    assert_ne!(
        replay_key(text_delta(" first delta", Some("preview"))),
        replay_key(text_delta(" second delta", Some("preview"))),
        "TextDelta replay identity must isolate delta text"
    );
    assert_ne!(
        replay_key(text_delta(" same delta", Some("preview"))),
        replay_key(text_delta(" same delta", None)),
        "TextDelta replay identity must distinguish present and absent previews"
    );

    let message_created = |message_text: &str, preview: &str| DeltaEvent::MessageCreated {
        revision: 4,
        session_id: "remote-session-1".to_owned(),
        message_id: "remote-message-1".to_owned(),
        message_index: 0,
        message_count: 1,
        message: remote_text_message("remote-message-1", message_text),
        preview: preview.to_owned(),
        status: SessionStatus::Idle,
        session_mutation_stamp: Some(11),
    };
    assert_ne!(
        replay_key(message_created("first text", "preview")),
        replay_key(message_created("second text", "preview")),
        "MessageCreated replay identity must isolate message payload"
    );
    assert_ne!(
        replay_key(message_created("same text", "first preview")),
        replay_key(message_created("same text", "second preview")),
        "MessageCreated replay identity must isolate preview text"
    );

    let message_updated = |message_text: &str, preview: &str| DeltaEvent::MessageUpdated {
        revision: 4,
        session_id: "remote-session-1".to_owned(),
        message_id: "remote-message-1".to_owned(),
        message_index: 0,
        message_count: 1,
        message: remote_text_message("remote-message-1", message_text),
        preview: preview.to_owned(),
        status: SessionStatus::Idle,
        session_mutation_stamp: Some(11),
    };
    assert_ne!(
        replay_key(message_updated("first text", "preview")),
        replay_key(message_updated("second text", "preview")),
        "MessageUpdated replay identity must isolate message payload"
    );
    assert_ne!(
        replay_key(message_updated("same text", "first preview")),
        replay_key(message_updated("same text", "second preview")),
        "MessageUpdated replay identity must isolate preview text"
    );

    let text_replace = |text: &str, preview: Option<&str>| DeltaEvent::TextReplace {
        revision: 5,
        session_id: "remote-session-1".to_owned(),
        message_id: "remote-message-1".to_owned(),
        message_index: 0,
        message_count: 1,
        text: text.to_owned(),
        preview: preview.map(str::to_owned),
        session_mutation_stamp: Some(12),
    };
    assert_ne!(
        replay_key(text_replace("first replacement", Some("preview"))),
        replay_key(text_replace("second replacement", Some("preview"))),
        "TextReplace replay identity must isolate replacement text"
    );
    assert_ne!(
        replay_key(text_replace("same replacement", Some("first preview"))),
        replay_key(text_replace("same replacement", Some("second preview"))),
        "TextReplace replay identity must isolate preview text"
    );

    let command_update = |command: &str,
                          command_language: Option<&str>,
                          output: &str,
                          output_language: Option<&str>,
                          status: CommandStatus,
                          preview: &str| DeltaEvent::CommandUpdate {
        revision: 6,
        session_id: "remote-session-1".to_owned(),
        message_id: "remote-command-1".to_owned(),
        message_index: 0,
        message_count: 1,
        command: command.to_owned(),
        command_language: command_language.map(str::to_owned),
        output: output.to_owned(),
        output_language: output_language.map(str::to_owned),
        status,
        preview: preview.to_owned(),
        session_mutation_stamp: Some(13),
    };
    assert_ne!(
        replay_key(command_update(
            "cargo test",
            Some("shell"),
            "output",
            Some("text"),
            CommandStatus::Running,
            "preview"
        )),
        replay_key(command_update(
            "cargo check",
            Some("shell"),
            "output",
            Some("text"),
            CommandStatus::Running,
            "preview"
        )),
        "CommandUpdate replay identity must isolate command text"
    );
    assert_ne!(
        replay_key(command_update(
            "cargo test",
            Some("shell"),
            "first output",
            Some("text"),
            CommandStatus::Running,
            "preview"
        )),
        replay_key(command_update(
            "cargo test",
            Some("shell"),
            "second output",
            Some("text"),
            CommandStatus::Running,
            "preview"
        )),
        "CommandUpdate replay identity must isolate output text"
    );
    assert_ne!(
        replay_key(command_update(
            "cargo test",
            Some("shell"),
            "output",
            Some("text"),
            CommandStatus::Running,
            "preview"
        )),
        replay_key(command_update(
            "cargo test",
            Some("powershell"),
            "output",
            Some("text"),
            CommandStatus::Running,
            "preview"
        )),
        "CommandUpdate replay identity must isolate command language"
    );
    assert_ne!(
        replay_key(command_update(
            "cargo test",
            Some("shell"),
            "output",
            Some("text"),
            CommandStatus::Running,
            "preview"
        )),
        replay_key(command_update(
            "cargo test",
            Some("shell"),
            "output",
            Some("json"),
            CommandStatus::Running,
            "preview"
        )),
        "CommandUpdate replay identity must isolate output language"
    );
    assert_ne!(
        replay_key(command_update(
            "cargo test",
            Some("shell"),
            "output",
            Some("text"),
            CommandStatus::Running,
            "preview"
        )),
        replay_key(command_update(
            "cargo test",
            Some("shell"),
            "output",
            Some("text"),
            CommandStatus::Success,
            "preview"
        )),
        "CommandUpdate replay identity must isolate command status"
    );
    assert_ne!(
        replay_key(command_update(
            "cargo test",
            Some("shell"),
            "output",
            Some("text"),
            CommandStatus::Running,
            "first preview"
        )),
        replay_key(command_update(
            "cargo test",
            Some("shell"),
            "output",
            Some("text"),
            CommandStatus::Running,
            "second preview"
        )),
        "CommandUpdate replay identity must isolate preview text"
    );

    let parallel_agents_update =
        |id: &str,
         title: &str,
         detail: Option<&str>,
         status: ParallelAgentStatus,
         preview: &str| DeltaEvent::ParallelAgentsUpdate {
            revision: 7,
            session_id: "remote-session-1".to_owned(),
            message_id: "remote-parallel-1".to_owned(),
            message_index: 0,
            message_count: 1,
            agents: vec![ParallelAgentProgress {
                detail: detail.map(str::to_owned),
                id: id.to_owned(),
                source: ParallelAgentSource::Tool,
                status,
                title: title.to_owned(),
            }],
            preview: preview.to_owned(),
            session_mutation_stamp: Some(14),
        };
    assert_ne!(
        replay_key(parallel_agents_update(
            "agent-1",
            "Agent one",
            Some("detail"),
            ParallelAgentStatus::Running,
            "preview"
        )),
        replay_key(parallel_agents_update(
            "agent-2",
            "Agent one",
            Some("detail"),
            ParallelAgentStatus::Running,
            "preview"
        )),
        "ParallelAgentsUpdate replay identity must isolate agent id"
    );
    assert_ne!(
        replay_key(parallel_agents_update(
            "agent-1",
            "Agent one",
            Some("detail"),
            ParallelAgentStatus::Running,
            "preview"
        )),
        replay_key(parallel_agents_update(
            "agent-1",
            "Agent two",
            Some("detail"),
            ParallelAgentStatus::Running,
            "preview"
        )),
        "ParallelAgentsUpdate replay identity must isolate agent title"
    );
    assert_ne!(
        replay_key(parallel_agents_update(
            "agent-1",
            "Agent one",
            Some("first detail"),
            ParallelAgentStatus::Running,
            "preview"
        )),
        replay_key(parallel_agents_update(
            "agent-1",
            "Agent one",
            Some("second detail"),
            ParallelAgentStatus::Running,
            "preview"
        )),
        "ParallelAgentsUpdate replay identity must isolate agent detail"
    );
    assert_ne!(
        replay_key(parallel_agents_update(
            "agent-1",
            "Agent one",
            Some("detail"),
            ParallelAgentStatus::Running,
            "preview"
        )),
        replay_key(parallel_agents_update(
            "agent-1",
            "Agent one",
            Some("detail"),
            ParallelAgentStatus::Completed,
            "preview"
        )),
        "ParallelAgentsUpdate replay identity must isolate agent status"
    );
    assert_ne!(
        replay_key(parallel_agents_update(
            "agent-1",
            "Agent one",
            Some("detail"),
            ParallelAgentStatus::Running,
            "first preview"
        )),
        replay_key(parallel_agents_update(
            "agent-1",
            "Agent one",
            Some("detail"),
            ParallelAgentStatus::Running,
            "second preview"
        )),
        "ParallelAgentsUpdate replay identity must isolate preview text"
    );

    let remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    let orchestrators_updated_with_session_preview = |preview: &str| {
        let mut session = remote_state.sessions[0].clone();
        session.preview = preview.to_owned();
        DeltaEvent::OrchestratorsUpdated {
            revision: 8,
            orchestrators: vec![remote_state.orchestrators[0].clone()],
            sessions: vec![session],
        }
    };
    assert_ne!(
        replay_key(orchestrators_updated_with_session_preview("first preview")),
        replay_key(orchestrators_updated_with_session_preview("second preview")),
        "OrchestratorsUpdated replay identity must isolate session payloads"
    );

    let large_output = "RAW_REPLAY_PAYLOAD_SHOULD_NOT_BE_RETAINED".repeat(64);
    let key_debug = format!(
        "{:?}",
        replay_key(command_update(
            "cargo test",
            Some("shell"),
            &large_output,
            Some("text"),
            CommandStatus::Running,
            "preview",
        ))
    );
    assert!(
        !key_debug.contains("RAW_REPLAY_PAYLOAD_SHOULD_NOT_BE_RETAINED"),
        "replay keys should retain output fingerprints, not raw output payloads"
    );
}

#[test]
fn remote_delta_replay_key_includes_revision_and_routing_fields() {
    let remote_id = "ssh-lab";
    let replay_key = |event: DeltaEvent| {
        AppState::remote_delta_replay_key(remote_id, &event)
            .expect("sample replay payloads should serialize")
    };
    let assert_changed = |left: DeltaEvent, right: DeltaEvent, reason: &str| {
        assert_ne!(replay_key(left), replay_key(right), "{reason}");
    };

    let remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    let mut session = remote_state.sessions[0].clone();
    session.message_count = 1;
    session.session_mutation_stamp = Some(10);
    let session_created = |revision: u64, session_id: &str| DeltaEvent::SessionCreated {
        revision,
        session_id: session_id.to_owned(),
        session: session.clone(),
    };
    assert_changed(
        session_created(3, "remote-session-1"),
        session_created(4, "remote-session-1"),
        "SessionCreated replay identity must include revision",
    );
    assert_changed(
        session_created(3, "remote-session-1"),
        session_created(3, "remote-session-2"),
        "SessionCreated replay identity must include session_id",
    );

    let message_created = |revision: u64,
                           session_id: &str,
                           message_id: &str,
                           message_index: usize,
                           message_count: u32,
                           session_mutation_stamp: Option<u64>| {
        DeltaEvent::MessageCreated {
            revision,
            session_id: session_id.to_owned(),
            message_id: message_id.to_owned(),
            message_index,
            message_count,
            message: remote_text_message(message_id, "same message"),
            preview: "same preview".to_owned(),
            status: SessionStatus::Idle,
            session_mutation_stamp,
        }
    };
    assert_changed(
        message_created(4, "remote-session-1", "remote-message-1", 0, 1, Some(11)),
        message_created(5, "remote-session-1", "remote-message-1", 0, 1, Some(11)),
        "MessageCreated replay identity must include revision",
    );
    assert_changed(
        message_created(4, "remote-session-1", "remote-message-1", 0, 1, Some(11)),
        message_created(4, "remote-session-2", "remote-message-1", 0, 1, Some(11)),
        "MessageCreated replay identity must include session_id",
    );
    assert_changed(
        message_created(4, "remote-session-1", "remote-message-1", 0, 1, Some(11)),
        message_created(4, "remote-session-1", "remote-message-2", 0, 1, Some(11)),
        "MessageCreated replay identity must include message_id",
    );
    assert_changed(
        message_created(4, "remote-session-1", "remote-message-1", 0, 1, Some(11)),
        message_created(4, "remote-session-1", "remote-message-1", 1, 1, Some(11)),
        "MessageCreated replay identity must include message_index",
    );
    assert_changed(
        message_created(4, "remote-session-1", "remote-message-1", 0, 1, Some(11)),
        message_created(4, "remote-session-1", "remote-message-1", 0, 2, Some(11)),
        "MessageCreated replay identity must include message_count",
    );
    assert_changed(
        message_created(4, "remote-session-1", "remote-message-1", 0, 1, Some(11)),
        message_created(4, "remote-session-1", "remote-message-1", 0, 1, Some(12)),
        "MessageCreated replay identity must include session_mutation_stamp",
    );

    let message_updated = |revision: u64,
                           session_id: &str,
                           message_id: &str,
                           message_index: usize,
                           message_count: u32,
                           stamp: Option<u64>| {
        DeltaEvent::MessageUpdated {
            revision,
            session_id: session_id.to_owned(),
            message_id: message_id.to_owned(),
            message_index,
            message_count,
            message: remote_text_message(message_id, "same message"),
            preview: "same preview".to_owned(),
            status: SessionStatus::Idle,
            session_mutation_stamp: stamp,
        }
    };
    assert_changed(
        message_updated(4, "remote-session-1", "remote-message-1", 0, 1, Some(11)),
        message_updated(5, "remote-session-1", "remote-message-1", 0, 1, Some(11)),
        "MessageUpdated replay identity must include revision",
    );
    assert_changed(
        message_updated(4, "remote-session-1", "remote-message-1", 0, 1, Some(11)),
        message_updated(4, "remote-session-2", "remote-message-1", 0, 1, Some(11)),
        "MessageUpdated replay identity must include session_id",
    );
    assert_changed(
        message_updated(4, "remote-session-1", "remote-message-1", 0, 1, Some(11)),
        message_updated(4, "remote-session-1", "remote-message-2", 0, 1, Some(11)),
        "MessageUpdated replay identity must include message_id",
    );
    assert_changed(
        message_updated(4, "remote-session-1", "remote-message-1", 0, 1, Some(11)),
        message_updated(4, "remote-session-1", "remote-message-1", 1, 1, Some(11)),
        "MessageUpdated replay identity must include message_index",
    );
    assert_changed(
        message_updated(4, "remote-session-1", "remote-message-1", 0, 1, Some(11)),
        message_updated(4, "remote-session-1", "remote-message-1", 0, 2, Some(11)),
        "MessageUpdated replay identity must include message_count",
    );
    assert_changed(
        message_updated(4, "remote-session-1", "remote-message-1", 0, 1, Some(11)),
        message_updated(4, "remote-session-1", "remote-message-1", 0, 1, Some(12)),
        "MessageUpdated replay identity must include session_mutation_stamp",
    );

    let text_delta = |revision: u64,
                      session_id: &str,
                      message_id: &str,
                      message_index: usize,
                      message_count: u32,
                      stamp: Option<u64>| {
        DeltaEvent::TextDelta {
            revision,
            session_id: session_id.to_owned(),
            message_id: message_id.to_owned(),
            message_index,
            message_count,
            delta: " same delta".to_owned(),
            preview: Some("same preview".to_owned()),
            session_mutation_stamp: stamp,
        }
    };
    assert_changed(
        text_delta(5, "remote-session-1", "remote-message-1", 0, 1, Some(12)),
        text_delta(6, "remote-session-1", "remote-message-1", 0, 1, Some(12)),
        "TextDelta replay identity must include revision",
    );
    assert_changed(
        text_delta(5, "remote-session-1", "remote-message-1", 0, 1, Some(12)),
        text_delta(5, "remote-session-2", "remote-message-1", 0, 1, Some(12)),
        "TextDelta replay identity must include session_id",
    );
    assert_changed(
        text_delta(5, "remote-session-1", "remote-message-1", 0, 1, Some(12)),
        text_delta(5, "remote-session-1", "remote-message-2", 0, 1, Some(12)),
        "TextDelta replay identity must include message_id",
    );
    assert_changed(
        text_delta(5, "remote-session-1", "remote-message-1", 0, 1, Some(12)),
        text_delta(5, "remote-session-1", "remote-message-1", 1, 1, Some(12)),
        "TextDelta replay identity must include message_index",
    );
    assert_changed(
        text_delta(5, "remote-session-1", "remote-message-1", 0, 1, Some(12)),
        text_delta(5, "remote-session-1", "remote-message-1", 0, 2, Some(12)),
        "TextDelta replay identity must include message_count",
    );
    assert_changed(
        text_delta(5, "remote-session-1", "remote-message-1", 0, 1, Some(12)),
        text_delta(5, "remote-session-1", "remote-message-1", 0, 1, Some(13)),
        "TextDelta replay identity must include session_mutation_stamp",
    );

    let text_replace = |revision: u64,
                        session_id: &str,
                        message_id: &str,
                        message_index: usize,
                        message_count: u32,
                        stamp: Option<u64>| {
        DeltaEvent::TextReplace {
            revision,
            session_id: session_id.to_owned(),
            message_id: message_id.to_owned(),
            message_index,
            message_count,
            text: "same replacement".to_owned(),
            preview: Some("same preview".to_owned()),
            session_mutation_stamp: stamp,
        }
    };
    assert_changed(
        text_replace(5, "remote-session-1", "remote-message-1", 0, 1, Some(12)),
        text_replace(6, "remote-session-1", "remote-message-1", 0, 1, Some(12)),
        "TextReplace replay identity must include revision",
    );
    assert_changed(
        text_replace(5, "remote-session-1", "remote-message-1", 0, 1, Some(12)),
        text_replace(5, "remote-session-2", "remote-message-1", 0, 1, Some(12)),
        "TextReplace replay identity must include session_id",
    );
    assert_changed(
        text_replace(5, "remote-session-1", "remote-message-1", 0, 1, Some(12)),
        text_replace(5, "remote-session-1", "remote-message-2", 0, 1, Some(12)),
        "TextReplace replay identity must include message_id",
    );
    assert_changed(
        text_replace(5, "remote-session-1", "remote-message-1", 0, 1, Some(12)),
        text_replace(5, "remote-session-1", "remote-message-1", 1, 1, Some(12)),
        "TextReplace replay identity must include message_index",
    );
    assert_changed(
        text_replace(5, "remote-session-1", "remote-message-1", 0, 1, Some(12)),
        text_replace(5, "remote-session-1", "remote-message-1", 0, 2, Some(12)),
        "TextReplace replay identity must include message_count",
    );
    assert_changed(
        text_replace(5, "remote-session-1", "remote-message-1", 0, 1, Some(12)),
        text_replace(5, "remote-session-1", "remote-message-1", 0, 1, Some(13)),
        "TextReplace replay identity must include session_mutation_stamp",
    );

    let command_update = |revision: u64,
                          session_id: &str,
                          message_id: &str,
                          message_index: usize,
                          message_count: u32,
                          stamp: Option<u64>| {
        DeltaEvent::CommandUpdate {
            revision,
            session_id: session_id.to_owned(),
            message_id: message_id.to_owned(),
            message_index,
            message_count,
            command: "cargo test".to_owned(),
            command_language: Some("shell".to_owned()),
            output: "same output".to_owned(),
            output_language: Some("text".to_owned()),
            status: CommandStatus::Running,
            preview: "same preview".to_owned(),
            session_mutation_stamp: stamp,
        }
    };
    assert_changed(
        command_update(6, "remote-session-1", "remote-command-1", 0, 1, Some(13)),
        command_update(7, "remote-session-1", "remote-command-1", 0, 1, Some(13)),
        "CommandUpdate replay identity must include revision",
    );
    assert_changed(
        command_update(6, "remote-session-1", "remote-command-1", 0, 1, Some(13)),
        command_update(6, "remote-session-2", "remote-command-1", 0, 1, Some(13)),
        "CommandUpdate replay identity must include session_id",
    );
    assert_changed(
        command_update(6, "remote-session-1", "remote-command-1", 0, 1, Some(13)),
        command_update(6, "remote-session-1", "remote-command-2", 0, 1, Some(13)),
        "CommandUpdate replay identity must include message_id",
    );
    assert_changed(
        command_update(6, "remote-session-1", "remote-command-1", 0, 1, Some(13)),
        command_update(6, "remote-session-1", "remote-command-1", 1, 1, Some(13)),
        "CommandUpdate replay identity must include message_index",
    );
    assert_changed(
        command_update(6, "remote-session-1", "remote-command-1", 0, 1, Some(13)),
        command_update(6, "remote-session-1", "remote-command-1", 0, 2, Some(13)),
        "CommandUpdate replay identity must include message_count",
    );
    assert_changed(
        command_update(6, "remote-session-1", "remote-command-1", 0, 1, Some(13)),
        command_update(6, "remote-session-1", "remote-command-1", 0, 1, Some(14)),
        "CommandUpdate replay identity must include session_mutation_stamp",
    );

    let parallel_agents_update = |revision: u64,
                                  session_id: &str,
                                  message_id: &str,
                                  message_index: usize,
                                  message_count: u32,
                                  stamp: Option<u64>| {
        DeltaEvent::ParallelAgentsUpdate {
            revision,
            session_id: session_id.to_owned(),
            message_id: message_id.to_owned(),
            message_index,
            message_count,
            agents: vec![ParallelAgentProgress {
                detail: Some("same detail".to_owned()),
                id: "agent-1".to_owned(),
                source: ParallelAgentSource::Tool,
                status: ParallelAgentStatus::Running,
                title: "Agent one".to_owned(),
            }],
            preview: "same preview".to_owned(),
            session_mutation_stamp: stamp,
        }
    };
    assert_changed(
        parallel_agents_update(7, "remote-session-1", "remote-parallel-1", 0, 1, Some(14)),
        parallel_agents_update(8, "remote-session-1", "remote-parallel-1", 0, 1, Some(14)),
        "ParallelAgentsUpdate replay identity must include revision",
    );
    assert_changed(
        parallel_agents_update(7, "remote-session-1", "remote-parallel-1", 0, 1, Some(14)),
        parallel_agents_update(7, "remote-session-2", "remote-parallel-1", 0, 1, Some(14)),
        "ParallelAgentsUpdate replay identity must include session_id",
    );
    assert_changed(
        parallel_agents_update(7, "remote-session-1", "remote-parallel-1", 0, 1, Some(14)),
        parallel_agents_update(7, "remote-session-1", "remote-parallel-2", 0, 1, Some(14)),
        "ParallelAgentsUpdate replay identity must include message_id",
    );
    assert_changed(
        parallel_agents_update(7, "remote-session-1", "remote-parallel-1", 0, 1, Some(14)),
        parallel_agents_update(7, "remote-session-1", "remote-parallel-1", 1, 1, Some(14)),
        "ParallelAgentsUpdate replay identity must include message_index",
    );
    assert_changed(
        parallel_agents_update(7, "remote-session-1", "remote-parallel-1", 0, 1, Some(14)),
        parallel_agents_update(7, "remote-session-1", "remote-parallel-1", 0, 2, Some(14)),
        "ParallelAgentsUpdate replay identity must include message_count",
    );
    assert_changed(
        parallel_agents_update(7, "remote-session-1", "remote-parallel-1", 0, 1, Some(14)),
        parallel_agents_update(7, "remote-session-1", "remote-parallel-1", 0, 1, Some(15)),
        "ParallelAgentsUpdate replay identity must include session_mutation_stamp",
    );

    let codex_updated = |revision: u64| DeltaEvent::CodexUpdated {
        revision,
        codex: CodexState {
            rate_limits: None,
            notices: vec![CodexNotice {
                kind: CodexNoticeKind::RuntimeNotice,
                level: CodexNoticeLevel::Info,
                title: "same notice".to_owned(),
                detail: "detail".to_owned(),
                timestamp: "2026-04-05 10:00:00".to_owned(),
                code: None,
            }],
        },
    };
    assert_changed(
        codex_updated(9),
        codex_updated(10),
        "CodexUpdated replay identity must include revision",
    );

    let orchestrators_updated = |revision: u64| DeltaEvent::OrchestratorsUpdated {
        revision,
        orchestrators: vec![remote_state.orchestrators[0].clone()],
        sessions: Vec::new(),
    };
    assert_changed(
        orchestrators_updated(8),
        orchestrators_updated(9),
        "OrchestratorsUpdated replay identity must include revision",
    );
}

#[test]
fn remote_delta_payload_fingerprint_returns_none_for_unserializable_payload() {
    struct UnserializablePayload;

    impl serde::Serialize for UnserializablePayload {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            Err(serde::ser::Error::custom(
                "intentional test serialization failure",
            ))
        }
    }

    assert_eq!(
        AppState::remote_delta_payload_fingerprint(&UnserializablePayload),
        None
    );
}

#[test]
fn remote_delta_replay_cache_none_key_is_explicit_noop() {
    let state = test_app_state();
    let key = None;
    let cache_len_before = state
        .remote_delta_replay_cache
        .lock()
        .expect("remote delta replay cache mutex poisoned")
        .keys
        .len();

    assert!(
        !state.should_skip_remote_applied_delta_replay(&key),
        "None replay keys should never suppress a delta"
    );
    state.note_remote_applied_delta_replay(&key);

    let cache_len_after = state
        .remote_delta_replay_cache
        .lock()
        .expect("remote delta replay cache mutex poisoned")
        .keys
        .len();
    assert_eq!(
        cache_len_after, cache_len_before,
        "None replay keys should not insert cache sentinels"
    );
}

fn test_remote_delta_replay_key(remote_id: &str, revision: u64) -> RemoteDeltaReplayKey {
    RemoteDeltaReplayKey {
        remote_id: remote_id.to_owned(),
        revision,
        payload: RemoteDeltaReplayPayload::CodexUpdated {
            codex_fingerprint: format!("codex-{remote_id}-{revision}"),
        },
    }
}

fn assert_remote_delta_replay_cache_shape(cache: &RemoteDeltaReplayCache, expected_len: usize) {
    assert_eq!(cache.keys.len(), expected_len);
    assert_eq!(cache.order.len(), expected_len);
    for key in cache.order.iter() {
        assert!(
            cache.keys.contains(key),
            "replay cache order/key indexes should stay in sync"
        );
    }
}

#[test]
fn remote_delta_replay_cache_evicts_oldest_entries_fifo() {
    let mut cache = RemoteDeltaReplayCache::default();
    let first_key = test_remote_delta_replay_key("ssh-lab", 0);
    let second_key = test_remote_delta_replay_key("ssh-lab", 1);

    cache.insert(first_key.clone());
    cache.insert(second_key.clone());
    for revision in 2..=REMOTE_DELTA_REPLAY_CACHE_LIMIT as u64 {
        cache.insert(test_remote_delta_replay_key("ssh-lab", revision));
    }

    assert_remote_delta_replay_cache_shape(&cache, REMOTE_DELTA_REPLAY_CACHE_LIMIT);
    assert!(
        !cache.contains(&first_key),
        "cache should evict the oldest entry after crossing the cap"
    );
    assert!(
        cache.contains(&second_key),
        "cache should retain the next-oldest entry until the next insert"
    );

    cache.insert(test_remote_delta_replay_key(
        "ssh-lab",
        REMOTE_DELTA_REPLAY_CACHE_LIMIT as u64 + 1,
    ));
    assert_remote_delta_replay_cache_shape(&cache, REMOTE_DELTA_REPLAY_CACHE_LIMIT);
    assert!(
        !cache.contains(&second_key),
        "subsequent inserts should continue FIFO eviction"
    );
}

#[test]
fn remote_delta_replay_cache_eviction_is_scoped_per_remote() {
    let mut cache = RemoteDeltaReplayCache::default();
    let quiet_remote_key = test_remote_delta_replay_key("ssh-lab-quiet", 10);

    cache.insert(quiet_remote_key.clone());
    for revision in 0..=REMOTE_DELTA_REPLAY_CACHE_LIMIT as u64 {
        cache.insert(test_remote_delta_replay_key("ssh-lab-noisy", revision));
    }

    assert_remote_delta_replay_cache_shape(&cache, REMOTE_DELTA_REPLAY_CACHE_LIMIT + 1);
    assert!(
        cache.contains(&quiet_remote_key),
        "a noisy remote must not evict another remote's replay key"
    );
    assert!(
        !cache.contains(&test_remote_delta_replay_key("ssh-lab-noisy", 0)),
        "same-remote FIFO eviction should still drop the noisy remote's oldest key"
    );
    assert!(
        cache.contains(&test_remote_delta_replay_key(
            "ssh-lab-noisy",
            REMOTE_DELTA_REPLAY_CACHE_LIMIT as u64,
        )),
        "same-remote FIFO eviction should retain the noisy remote's newest key"
    );
}

#[test]
fn remote_delta_replay_cache_remove_remote_preserves_other_remotes() {
    let mut cache = RemoteDeltaReplayCache::default();
    let remote_a_key = test_remote_delta_replay_key("ssh-lab-a", 10);
    let remote_a_second_key = test_remote_delta_replay_key("ssh-lab-a", 11);
    let remote_b_key = test_remote_delta_replay_key("ssh-lab-b", 10);

    cache.insert(remote_a_key.clone());
    cache.insert(remote_a_second_key.clone());
    cache.insert(remote_b_key.clone());
    assert_remote_delta_replay_cache_shape(&cache, 3);

    cache.remove_remote("ssh-lab-a");

    assert_remote_delta_replay_cache_shape(&cache, 1);
    assert!(!cache.contains(&remote_a_key));
    assert!(!cache.contains(&remote_a_second_key));
    assert!(
        cache.contains(&remote_b_key),
        "clearing one remote must not drop another remote's replay keys"
    );
}

#[test]
fn clear_remote_applied_revision_preserves_other_remote_replay_keys() {
    let state = test_app_state();
    let remote_a_key = Some(test_remote_delta_replay_key("ssh-lab-a", 10));
    let remote_a_second_key = Some(test_remote_delta_replay_key("ssh-lab-a", 11));
    let remote_b_key = Some(test_remote_delta_replay_key("ssh-lab-b", 10));

    state.note_remote_applied_delta_replay(&remote_a_key);
    state.note_remote_applied_delta_replay(&remote_a_second_key);
    state.note_remote_applied_delta_replay(&remote_b_key);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.note_remote_applied_revision("ssh-lab-a", 11);
        inner.note_remote_applied_revision("ssh-lab-b", 10);
    }
    {
        let mut in_flight = state
            .remote_delta_hydrations_in_flight
            .lock()
            .expect("remote delta hydration mutex poisoned");
        in_flight.insert(("ssh-lab-a".to_owned(), "remote-session-a".to_owned()));
        in_flight.insert(("ssh-lab-b".to_owned(), "remote-session-b".to_owned()));
    }

    state.clear_remote_applied_revision("ssh-lab-a");

    assert!(
        !state.should_skip_remote_applied_delta_replay(&remote_a_key),
        "clearing remote A should remove its replay keys"
    );
    assert!(
        !state.should_skip_remote_applied_delta_replay(&remote_a_second_key),
        "clearing remote A should remove all of its replay keys"
    );
    assert!(
        state.should_skip_remote_applied_delta_replay(&remote_b_key),
        "clearing remote A must preserve remote B's replay protection"
    );
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert_eq!(inner.remote_applied_revisions.get("ssh-lab-a"), None);
        assert_eq!(inner.remote_applied_revisions.get("ssh-lab-b"), Some(&10));
    }
    {
        let cache = state
            .remote_delta_replay_cache
            .lock()
            .expect("remote delta replay cache mutex poisoned");
        assert_remote_delta_replay_cache_shape(&cache, 1);
    }
    {
        let in_flight = state
            .remote_delta_hydrations_in_flight
            .lock()
            .expect("remote delta hydration mutex poisoned");
        assert!(
            in_flight.contains(&("ssh-lab-a".to_owned(), "remote-session-a".to_owned())),
            "clearing remote A must not remove live in-flight hydration markers; the owning \
             hydration guard retires them"
        );
        assert!(
            in_flight.contains(&("ssh-lab-b".to_owned(), "remote-session-b".to_owned())),
            "clearing remote A must preserve remote B's in-flight hydration markers"
        );
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_delta_replay_cache_clears_with_remote_revision_watermark() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut full_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    full_remote_session.messages = vec![remote_text_message("remote-message-1", "Hello")];
    full_remote_session.messages_loaded = true;
    full_remote_session.message_count = 1;
    full_remote_session.session_mutation_stamp = Some(10);

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: full_remote_session.id.clone(),
                session: full_remote_session,
            },
        )
        .expect("remote full session create delta should apply");

    let text_delta = || DeltaEvent::TextDelta {
        revision: 3,
        session_id: "remote-session-1".to_owned(),
        message_id: "remote-message-1".to_owned(),
        message_index: 0,
        message_count: 1,
        delta: " world".to_owned(),
        preview: Some("Hello world".to_owned()),
        session_mutation_stamp: Some(11),
    };

    state
        .apply_remote_delta_event(&remote.id, text_delta())
        .expect("first text delta should apply and seed replay cache");
    state.clear_remote_applied_revision(&remote.id);
    state
        .apply_remote_delta_event(&remote.id, text_delta())
        .expect("same key should be evaluated again after continuity reset");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(matches!(
        &record.session.messages[0],
        Message::Text { text, .. } if text == "Hello world world"
    ));
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&3));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}
