//! Read-only provider-enqueue observations, never runtime ACK or receipt authority.
use super::*;
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;

fn wire(state: &AppState, session: &str) -> Value {
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    serde_json::to_value(AppState::wire_session_from_record(&state.server_instance_id, record)).unwrap()
}

fn change_record<T>(state: &AppState, session: &str, change: impl FnOnce(&mut SessionRecord) -> T) -> T {
    let mut inner = state.inner.lock().unwrap();
    change(inner.session_mut(session).unwrap())
}

#[test]
fn removed_admission_budget_wire_fields_are_not_restored_as_permission_or_display() {
    let turn = OrdinaryRoot::new("removed-budget-wire-view");
    turn.enqueue("ordinary queued content", QueuedPromptSource::User);
    let original = json!({"admittedAt":"2026-10-20T10:00:00Z","deadline":"2026-10-20T10:04:00Z",
        "attempts":3,"renewedAt":"2026-10-20T10:05:00Z","unknown":[null,"unchanged"]});
    turn.edit(|record| {
        record.engram.historical_control.insert("engramBeginPreparationLineage".into(), original.clone());
    });
    let mut encoded = turn.saved();
    encoded["session"]["promptAdmissionBudgets"] = json!({"old-head":original});
    let loaded = serde_json::from_value::<PersistedSessionRecord>(encoded).unwrap().into_record().unwrap();
    assert_eq!(loaded.engram.historical_control["engramBeginPreparationLineage"], original);
    let view = serde_json::to_value(AppState::wire_session_from_record(&turn.state.server_instance_id, &loaded)).unwrap();
    assert!(view["promptAdmissionBudgets"].is_null());
    assert!(loaded.prompt_deliveries.is_empty());
    turn.assert_no_prompt();
    turn.assert_no_control();
}

fn actual_enqueue(source: QueuedPromptSource, receiver_closed: bool) {
    let mut turn = OrdinaryRoot::new("actual-handoff-observation");
    let id = turn.enqueue("ordinary observed prompt", source);
    let started = turn.state.start_next_queued_turn_off_lock(&turn.session, false, false).unwrap().unwrap();
    assert!(wire(&turn.state, &turn.session)["providerHandoffs"].as_array().is_none_or(Vec::is_empty),
        "preparation is not an enqueue");
    let (_, empty) = mpsc::channel();
    let receiver = std::mem::replace(&mut turn.receiver, empty);
    let mut receiver = Some(receiver);
    if receiver_closed { drop(receiver.take()); }
    let outcome = handoff_prepared_turn_dispatch(&turn.state, started.dispatch);
    if receiver_closed {
        assert!(outcome.is_err(), "the actual receiver was closed");
    } else {
        assert!(matches!(outcome.unwrap(), HandoffPreparedTurnDispatchOutcome::Delivered));
        assert!(matches!(receiver.as_ref().unwrap().try_recv().unwrap(), CodexRuntimeCommand::Prompt { .. }));
        assert!(receiver.as_ref().unwrap().try_recv().is_err());
    }
    let view = wire(&turn.state, &turn.session);
    let rows = view["providerHandoffs"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["sessionId"], turn.session);
    assert_eq!(row["projectId"], turn.project);
    assert_eq!(row["promptId"], id);
    assert_eq!(row["source"], match source {
        QueuedPromptSource::User => "user", QueuedPromptSource::Human => "human",
        QueuedPromptSource::Mailbox => "mailbox", QueuedPromptSource::Orchestrator => "orchestrator",
    });
    assert_eq!(row["outcome"], if receiver_closed { "enqueueFailed" } else { "enqueued" });
    chrono::DateTime::parse_from_rfc3339(row["at"].as_str().unwrap()).unwrap();
    turn.record(|record| assert!(record.prompt_deliveries.is_empty()));
    assert!(view["promptDeliveries"].as_array().is_none_or(Vec::is_empty));
    turn.assert_no_control();
}

#[test]
fn prompt_observation_actual_user_and_mailbox_enqueues_have_project_attribution() {
    for source in [QueuedPromptSource::User, QueuedPromptSource::Mailbox] {
        actual_enqueue(source, false);
    }
}

#[test]
fn prompt_observation_failed_user_and_mailbox_enqueues_are_distinct_outcomes() {
    for source in [QueuedPromptSource::User, QueuedPromptSource::Mailbox] {
        actual_enqueue(source, true);
    }
}

#[test]
fn prompt_observation_orchestrator_enqueues_keep_the_queued_source() {
    for receiver_closed in [false, true] {
        actual_enqueue(QueuedPromptSource::Orchestrator, receiver_closed);
    }
}

#[test]
fn prompt_observation_direct_idle_dispatch_uses_the_user_source() {
    let turn = OrdinaryRoot::new("direct-user-handoff-observation");
    let dispatch = turn.prepare("Direct ordinary prompt.");
    assert!(matches!(handoff_prepared_turn_dispatch(&turn.state, dispatch).unwrap(),
        HandoffPreparedTurnDispatchOutcome::Delivered));
    turn.prompt();
    turn.assert_no_prompt();
    let view = wire(&turn.state, &turn.session);
    let rows = view["providerHandoffs"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["source"], "user");
    assert_eq!(rows[0]["sessionId"], turn.session);
    assert_eq!(rows[0]["projectId"], turn.project);
    assert_eq!(rows[0]["outcome"], "enqueued");
    turn.record(|record| assert!(record.prompt_deliveries.is_empty()));
    turn.assert_no_control();
}

fn seed_observation(state: &AppState, session: &str, ordinal: u64, at: &str) {
    change_record(state, session, |record| {
        record.provider_handoff_observations.prepared = Some(ProviderHandoffPrompt {
            prompt_id: format!("observed-{ordinal}"),
            source: QueuedPromptSource::User,
            turn_generation: ordinal,
        });
        observe_provider_handoff(record, ordinal, true);
        record
            .provider_handoff_observations
            .rows
            .back_mut()
            .unwrap()
            .at = at.into();
    });
}

#[test]
fn prompt_observation_history_is_bounded_and_never_restored_as_receipt_authority() {
    let turn = OrdinaryRoot::new("bounded-handoff-observations");
    let state = &turn.state;
    let session = &turn.session;
    for ordinal in 0..=PROVIDER_HANDOFF_OBSERVATION_LIMIT {
        seed_observation(&state, &session, ordinal as u64, "2026-10-20T10:00:00Z");
    }
    let view = wire(&state, &session);
    assert_eq!(view["providerHandoffs"].as_array().unwrap().len(), 256);
    assert_eq!(view["providerHandoffs"][0]["promptId"], "observed-1");
    assert_eq!(view["providerHandoffHistory"]["dropped"], 1);
    assert_eq!(view["providerHandoffHistory"]["truncated"], true);
    assert_eq!(view["providerHandoffHistory"]["retentionLimit"], 256);
    chrono::DateTime::parse_from_rfc3339(
        view["providerHandoffHistory"]["observedSince"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    turn.record(|record| {
        assert!(record.prompt_deliveries.is_empty());
        let persisted = PersistedSessionRecord::from_record(record);
        let encoded = serde_json::to_value(&persisted).unwrap();
        assert!(encoded["session"]["providerHandoffs"].is_null());
        assert!(encoded["session"]["providerHandoffHistory"].is_null());
        let loaded = persisted.into_record().unwrap();
        assert!(loaded.provider_handoff_observations.rows.is_empty());
        assert!(loaded.prompt_deliveries.is_empty());
        assert_eq!(loaded.provider_handoff_observations.dropped, 0);
    });
}

fn observed_session_fixture() -> (OrdinaryRoot, Session) {
    let turn = OrdinaryRoot::new("bounded-handoff-observations");
    let state = &turn.state;
    let session = &turn.session;
    turn.enqueue("retained observed prompt", QueuedPromptSource::User);
    seed_observation(&state, &session, 1, "2026-10-20T10:00:00Z");
    let projected = turn.record(|record| {
        AppState::wire_session_from_record(&state.server_instance_id, record)
    });
    assert!(!projected.provider_handoffs.is_empty());
    assert!(projected.provider_handoff_history.is_some());
    (turn, projected)
}

#[test]
fn prompt_observation_serialization_and_restore_drop_seeded_wire_fields() {
    let (turn, projected) = observed_session_fixture();
    let state = &turn.state;
    let session = &turn.session;
    let persisted = change_record(&state, &session, |record| {
        record.session.provider_handoffs = projected.provider_handoffs.clone();
        record.session.provider_handoff_history = projected.provider_handoff_history.clone();
        PersistedSessionRecord::from_record(record)
    });
    assert!(!persisted.session.provider_handoffs.is_empty());
    let mut encoded = serde_json::to_value(&persisted).unwrap();
    for field in [
        "promptAdmissionBudgets",
        "providerHandoffs",
        "providerHandoffHistory",
    ] {
        assert!(encoded["session"][field].is_null(), "serialized {field}");
    }
    // Inject saved wire data independently of serialization to cover loading.
    let wire_fields = serde_json::to_value(&projected).unwrap();
    for field in [
        "promptAdmissionBudgets",
        "providerHandoffs",
        "providerHandoffHistory",
    ] {
        encoded["session"][field] = wire_fields[field].clone();
    }
    encoded["session"]["promptAdmissionBudgets"] = json!({"old-head":{"originalDeadline":"historical-only"}});
    let loaded = serde_json::from_value::<PersistedSessionRecord>(encoded)
        .unwrap()
        .into_record()
        .unwrap();
    assert!(loaded.session.provider_handoffs.is_empty());
    assert!(loaded.session.provider_handoff_history.is_none());
    assert!(loaded.provider_handoff_observations.rows.is_empty());
    assert!(loaded.prompt_deliveries.is_empty());
}

#[tokio::test]
async fn prompt_observation_proxy_hydration_and_summary_never_publish_foreign_diagnostics() {
    let (turn, mut upstream) = observed_session_fixture();
    let state = &turn.state;
    let session = &turn.session;
    let remote = RemoteConfig {
        id: "observation-upstream".into(),
        name: "Observation upstream".into(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let proxy =
        crate::tests::remote::seed_remote_proxy_session_via_state_inner_upsert(&state, &remote);
    upstream.id = "remote-session-1".into();
    upstream.project_id = Some("foreign-project-record".into());
    upstream.session_mutation_stamp = Some(10);
    upstream.messages = vec![crate::tests::remote::remote_text_message(
        "upstream-message",
        "Cached upstream transcript survives metadata refresh.",
    )]
    .into();
    upstream.messages_loaded = true;
    upstream.message_count = 1;
    for row in &mut upstream.provider_handoffs {
        row.session_id = upstream.id.clone();
        row.project_id = upstream.project_id.clone();
    }
    change_record(&state, &proxy, |record| {
        assert!(record.is_remote_proxy());
        let project = record.session.project_id.clone();
        apply_remote_session_to_record(record, &remote.id, project, &upstream);
        assert!(record.session.messages_loaded);
        assert!(record.session.provider_handoffs.is_empty());
        assert!(record.session.provider_handoff_history.is_none());
    });
    for after_summary in [false, true] {
        if after_summary {
            let summary = test_state_session_summary_from_session(&upstream);
            change_record(&state, &proxy, |record| {
                let project = record.session.project_id.clone();
                apply_remote_session_summary_to_record(record, &remote.id, project, &summary);
                assert!(
                    record.session.messages_loaded,
                    "same-body summary keeps the cache valid"
                );
            });
        }
        let (status, response) =
            session_read(&state, &proxy, "?tail=20&handoffFrom=2026-10-20T09:59:00Z").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["session"]["id"], proxy);
        assert_eq!(
            response["session"]["messages"][0]["text"],
            "Cached upstream transcript survives metadata refresh."
        );
        for field in [
            "promptAdmissionBudgets",
            "providerHandoffs",
            "providerHandoffHistory",
        ] {
            assert!(
                response["session"][field].is_null(),
                "proxy returned {field}"
            );
        }
    }
    // Even a stale embedded wire cache cannot bypass the local projection.
    change_record(&state, &proxy, |record| {
        record.session.provider_handoffs = upstream.provider_handoffs.clone();
        record.session.provider_handoff_history = upstream.provider_handoff_history.clone();
    });
    let (status, response) = session_read(&state, &proxy, "?tail=20").await;
    assert_eq!(status, StatusCode::OK);
    assert!(response["session"]["providerHandoffs"].is_null());
    assert!(response["session"]["promptAdmissionBudgets"].is_null());
    assert!(response["session"]["providerHandoffHistory"].is_null());
    assert_eq!(
        wire(&state, &session)["providerHandoffs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

async fn session_read(state: &AppState, session: &str, query: &str) -> (StatusCode, Value) {
    let response = app_router(state.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/sessions/{session}{query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn prompt_observation_session_route_filters_half_open_time_window() {
    let turn = OrdinaryRoot::new("bounded-handoff-observations");
    let state = &turn.state;
    let session = &turn.session;
    for (ordinal, at) in [
        (1, "2026-10-20T09:59:59Z"),
        (2, "2026-10-20T10:00:00Z"),
        (3, "2026-10-20T10:01:00Z"),
    ] {
        seed_observation(&state, &session, ordinal, at);
    }
    let (status, all) = session_read(&state, &session, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        all["session"]["providerHandoffs"].as_array().unwrap().len(),
        3
    );
    let (status, filtered) = session_read(
        &state,
        &session,
        "?handoffFrom=2026-10-20T10:00:00Z&handoffTo=2026-10-20T10:01:00Z",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rows = filtered["session"]["providerHandoffs"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["promptId"], "observed-2");
    assert_eq!(
        filtered["session"]["providerHandoffHistory"],
        all["session"]["providerHandoffHistory"]
    );
    assert_eq!(
        wire(&state, &session)["providerHandoffs"]
            .as_array()
            .unwrap()
            .len(),
        3,
        "filtering a read never removes recorded observations"
    );
}

#[tokio::test]
async fn prompt_observation_session_route_refuses_bad_or_inverted_time_bounds() {
    let turn = OrdinaryRoot::new("bounded-handoff-observations");
    let state = &turn.state;
    let session = &turn.session;
    for query in [
        "?handoffFrom=not-a-time",
        "?handoffTo=not-a-time",
        "?handoffFrom=2026-10-20T10:01:00Z&handoffTo=2026-10-20T10:00:00Z",
    ] {
        let (status, _) = session_read(&state, &session, query).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
