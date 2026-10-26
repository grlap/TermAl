//! Read-only prompt admission and provider-enqueue observations. Uses the
//! existing root fixture, saved budget owner and real provider channel; no
//! live provider, Engram store or admission-policy substitute.
use super::*;

fn wire(state: &AppState, session: &str) -> Value {
    inspect(state, session, |record| {
        serde_json::to_value(AppState::wire_session_from_record(
            &state.server_instance_id,
            record,
        ))
        .unwrap()
    })
}

fn change_record<T>(
    state: &AppState,
    session: &str,
    change: impl FnOnce(&mut SessionRecord) -> T,
) -> T {
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(session).unwrap();
    change(inner.session_mut_by_index(index).unwrap())
}

#[test]
fn prompt_observation_automatic_attempts_keep_the_original_public_deadline() {
    let (state, session, receiver, _) = root_fixture([]);
    queue_test_engram_prompt(
        &state,
        &session,
        "retained prompt",
        QueuedPromptSource::User,
        None,
    );
    let admitted = chrono::DateTime::parse_from_rfc3339("2026-10-20T10:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    change_record(&state, &session, |record| {
        let owner = EngramQueuedAdmissionOwner::capture(record).unwrap();
        assert!(accept_engram_admission_budget(record, &owner, admitted));
        for seconds in [1, 20, 60] {
            assert!(count_engram_begin_preparation_attempt(record));
            assert!(!accept_engram_admission_budget(
                record,
                &owner,
                admitted + chrono::Duration::seconds(seconds),
            ));
        }
    });
    let view = wire(&state, &session);
    let budget = &view["promptAdmissionBudgets"][view["pendingPrompts"][0]["id"].as_str().unwrap()];
    assert_eq!(budget["originalAdmittedAt"], "2026-10-20T10:00:00.000Z");
    assert_eq!(budget["originalDeadline"], "2026-10-20T10:04:00.000Z");
    assert_eq!(budget["currentDeadline"], budget["originalDeadline"]);
    assert_eq!(budget["attempts"], 3);
    assert!(budget["renewedAt"].is_null());
    assert!(
        receiver.try_recv().is_err(),
        "reading a budget never dispatches a prompt"
    );
}

#[test]
fn prompt_observation_explicit_window_renewal_keeps_original_times() {
    let (state, session, _, _) = root_fixture([]);
    queue_test_engram_prompt(
        &state,
        &session,
        "retained resume",
        QueuedPromptSource::Mailbox,
        None,
    );
    let admitted = chrono::DateTime::parse_from_rfc3339("2026-10-20T10:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    change_record(&state, &session, |record| {
        let owner = EngramQueuedAdmissionOwner::capture(record).unwrap();
        assert!(accept_engram_admission_budget(record, &owner, admitted));
        assert!(renew_engram_admission_budget_for_resume(
            record,
            admitted + chrono::Duration::minutes(5),
        ));
        assert!(count_engram_begin_preparation_attempt(record));
        // Repeating Resume while the new window is running cannot renew it.
        assert!(!renew_engram_admission_budget_for_resume(
            record,
            admitted + chrono::Duration::minutes(6),
        ));
    });
    let view = wire(&state, &session);
    let budget = &view["promptAdmissionBudgets"][view["pendingPrompts"][0]["id"].as_str().unwrap()];
    assert_eq!(budget["originalAdmittedAt"], "2026-10-20T10:00:00.000Z");
    assert_eq!(budget["originalDeadline"], "2026-10-20T10:04:00.000Z");
    assert_eq!(budget["currentAdmittedAt"], "2026-10-20T10:05:00.000Z");
    assert_eq!(budget["currentDeadline"], "2026-10-20T10:09:00.000Z");
    assert_eq!(budget["renewedAt"], "2026-10-20T10:05:00.000Z");
    assert_eq!(budget["attempts"], 1);
}

fn actual_enqueue(source: QueuedPromptSource, receiver_closed: bool) {
    let responses = if source == QueuedPromptSource::Orchestrator {
        let mut replies = vec![
            bind_reply("observation-token"),
            grant_reply("observation-grant"),
            begin_reply("observation-grant"),
        ];
        if receiver_closed {
            replies.push(checkpoint_reply("observation-grant"));
        }
        replies
    } else {
        vec![ScriptedEngramControlResponse::Reply(Err(
            EngramTransportError::spawn_failed("optional control unavailable"),
        ))]
    };
    let (state, session, receiver, transport) = root_fixture(responses);
    queue_test_engram_prompt(&state, &session, "ordinary observed prompt", source, None);
    let (prompt_id, project_id) = inspect(&state, &session, |record| {
        assert!(
            record.prompt_deliveries.is_empty(),
            "ordinary head has no receipt"
        );
        (
            record
                .queued_prompts
                .front()
                .unwrap()
                .pending_prompt
                .id
                .clone(),
            record.session.project_id.clone().unwrap(),
        )
    });
    let started = state
        .start_next_queued_turn_off_lock(&session, false, false)
        .unwrap()
        .unwrap();
    assert_eq!(
        state.prepare_engram_turn_delivery_off_lock(
            &session,
            started.dispatch.engram_dispatch_generation().unwrap(),
        ),
        EngramTurnDeliveryPreparation::Ready,
    );
    if source == QueuedPromptSource::Orchestrator {
        assert_eq!(
            operations(&transport),
            ["session_bind", "turn_evaluate", "turn_begin"]
        );
    }
    assert!(
        wire(&state, &session)["providerHandoffs"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "preparation is not a provider enqueue"
    );
    let mut receiver = Some(receiver);
    if receiver_closed {
        drop(receiver.take());
    }
    let result = handoff_prepared_turn_dispatch(&state, started.dispatch);
    if receiver_closed {
        assert!(result.is_err(), "the actual provider receiver is closed");
    } else {
        assert!(matches!(
            result.unwrap(),
            HandoffPreparedTurnDispatchOutcome::Delivered
        ));
        assert!(matches!(
            receiver.as_ref().unwrap().try_recv().unwrap(),
            CodexRuntimeCommand::Prompt { .. }
        ));
        assert!(
            receiver.as_ref().unwrap().try_recv().is_err(),
            "exactly one command"
        );
    }
    let view = wire(&state, &session);
    let handoffs = view["providerHandoffs"]
        .as_array()
        .expect("provider observations are readable");
    assert_eq!(handoffs.len(), 1);
    let handoff = &handoffs[0];
    assert_eq!(handoff["sessionId"], session);
    assert_eq!(handoff["projectId"], project_id);
    assert_eq!(handoff["promptId"], prompt_id);
    assert_eq!(
        handoff["source"],
        match source {
            QueuedPromptSource::User => "user",
            QueuedPromptSource::Mailbox => "mailbox",
            QueuedPromptSource::Orchestrator => "orchestrator",
        }
    );
    assert_eq!(
        handoff["outcome"],
        if receiver_closed {
            "enqueueFailed"
        } else {
            "enqueued"
        }
    );
    chrono::DateTime::parse_from_rfc3339(handoff["at"].as_str().unwrap()).unwrap();
    assert!(
        inspect(&state, &session, |record| record
            .prompt_deliveries
            .is_empty()),
        "observing an ordinary handoff must never create receipt authority"
    );
    assert!(
        view["promptDeliveries"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
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
    let (state, session, receiver, _) = root_fixture([ScriptedEngramControlResponse::Reply(Err(
        EngramTransportError::spawn_failed("workflow control unavailable"),
    ))]);
    queue_test_engram_prompt(
        &state,
        &session,
        "withheld orchestrator prompt",
        QueuedPromptSource::Orchestrator,
        None,
    );
    let started = state
        .start_next_queued_turn_off_lock(&session, false, false)
        .unwrap()
        .unwrap();
    assert!(matches!(
        state.prepare_engram_turn_delivery_off_lock(
            &session,
            started.dispatch.engram_dispatch_generation().unwrap(),
        ),
        EngramTurnDeliveryPreparation::RetainedBindRetry(_)
    ));
    assert!(receiver.try_recv().is_err());
    assert!(
        wire(&state, &session)["providerHandoffs"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
    for receiver_closed in [false, true] {
        actual_enqueue(QueuedPromptSource::Orchestrator, receiver_closed);
    }
}

#[test]
fn prompt_observation_direct_idle_dispatch_uses_the_user_source() {
    let (state, session, receiver, _) = root_fixture([ScriptedEngramControlResponse::Reply(Err(
        EngramTransportError::spawn_failed("optional control unavailable"),
    ))]);
    assert!(inspect(&state, &session, |record| record
        .queued_prompts
        .is_empty()));
    let dispatch = root_dispatch(&state, &session, false);
    assert_eq!(
        state.prepare_engram_turn_delivery_off_lock(
            &session,
            dispatch.engram_dispatch_generation().unwrap(),
        ),
        EngramTurnDeliveryPreparation::Ready,
    );
    assert!(matches!(
        handoff_prepared_turn_dispatch(&state, dispatch).unwrap(),
        HandoffPreparedTurnDispatchOutcome::Delivered
    ));
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert!(receiver.try_recv().is_err());
    let view = wire(&state, &session);
    let rows = view["providerHandoffs"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["source"], "user");
    assert_eq!(rows[0]["sessionId"], session);
    assert_eq!(rows[0]["projectId"], view["projectId"]);
    assert_eq!(rows[0]["outcome"], "enqueued");
    assert!(inspect(&state, &session, |record| record
        .prompt_deliveries
        .is_empty()));
}

#[test]
fn prompt_observation_legacy_window_does_not_fabricate_original_history() {
    let (state, session, _, _) = root_fixture([]);
    queue_test_engram_prompt(
        &state,
        &session,
        "legacy retained prompt",
        QueuedPromptSource::User,
        None,
    );
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-20T10:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    change_record(&state, &session, |record| {
        let owner = EngramQueuedAdmissionOwner::capture(record).unwrap();
        assert!(accept_engram_admission_budget(record, &owner, now));
        let mut old =
            serde_json::to_value(record.engram.begin_preparation_lineage.as_ref().unwrap())
                .unwrap();
        old.as_object_mut().unwrap().remove("originalWindow");
        old.as_object_mut().unwrap().remove("renewedAt");
        record.engram.begin_preparation_lineage = Some(serde_json::from_value(old).unwrap());
        assert!(renew_engram_admission_budget_for_resume(
            record,
            now + chrono::Duration::minutes(5)
        ));
    });
    let view = wire(&state, &session);
    let budget = &view["promptAdmissionBudgets"][view["pendingPrompts"][0]["id"].as_str().unwrap()];
    assert!(budget["originalAdmittedAt"].is_null());
    assert!(budget["originalDeadline"].is_null());
    assert_eq!(budget["currentDeadline"], "2026-10-20T10:09:00.000Z");
    assert_eq!(budget["renewedAt"], "2026-10-20T10:05:00.000Z");
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
    let (state, session, _, _) = root_fixture([]);
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
    inspect(&state, &session, |record| {
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

fn observed_session_fixture() -> (AppState, String, Session) {
    let (state, session, _, _) = root_fixture([]);
    queue_test_engram_prompt(
        &state,
        &session,
        "retained observed prompt",
        QueuedPromptSource::User,
        None,
    );
    change_record(&state, &session, |record| {
        let owner = EngramQueuedAdmissionOwner::capture(record).unwrap();
        assert!(accept_engram_admission_budget(
            record,
            &owner,
            chrono::Utc::now()
        ));
    });
    seed_observation(&state, &session, 1, "2026-10-20T10:00:00Z");
    let projected = inspect(&state, &session, |record| {
        AppState::wire_session_from_record(&state.server_instance_id, record)
    });
    assert!(!projected.prompt_admission_budgets.is_empty());
    assert!(!projected.provider_handoffs.is_empty());
    assert!(projected.provider_handoff_history.is_some());
    (state, session, projected)
}

#[test]
fn prompt_observation_serialization_and_restore_drop_seeded_wire_fields() {
    let (state, session, projected) = observed_session_fixture();
    let persisted = change_record(&state, &session, |record| {
        record.session.prompt_admission_budgets = projected.prompt_admission_budgets.clone();
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
    let loaded = serde_json::from_value::<PersistedSessionRecord>(encoded)
        .unwrap()
        .into_record()
        .unwrap();
    assert!(loaded.session.prompt_admission_budgets.is_empty());
    assert!(loaded.session.provider_handoffs.is_empty());
    assert!(loaded.session.provider_handoff_history.is_none());
    assert!(loaded.provider_handoff_observations.rows.is_empty());
    assert!(loaded.prompt_deliveries.is_empty());
}

#[tokio::test]
async fn prompt_observation_proxy_hydration_and_summary_never_publish_foreign_diagnostics() {
    let (state, session, mut upstream) = observed_session_fixture();
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
        assert!(record.session.prompt_admission_budgets.is_empty());
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
        record.session.prompt_admission_budgets = upstream.prompt_admission_budgets.clone();
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
    let (state, session, _, _) = root_fixture([]);
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
    let (state, session, _, _) = root_fixture([]);
    for query in [
        "?handoffFrom=not-a-time",
        "?handoffTo=not-a-time",
        "?handoffFrom=2026-10-20T10:01:00Z&handoffTo=2026-10-20T10:00:00Z",
    ] {
        let (status, _) = session_read(&state, &session, query).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
