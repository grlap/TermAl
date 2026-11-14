// Real selected S2b request -> original native producer -> authenticated READ.
use super::*;
use crate::tests::test_runs::test_run_registration::causal_completion_fixture::{
    compact, finish, producer,
};

struct Handles(Vec<AppState>);
impl Drop for Handles {
    fn drop(&mut self) {
        for state in &self.0 {
            state.federation_peer_handles.lock().unwrap().clear();
        }
    }
}

async fn connect(fixture: &Fixture) -> (DomainServer, Handles) {
    let channel = fixture.channel.clone();
    let checked = channel.clone();
    let server = DomainServer::start(federation_router(
        channel,
        Arc::new(move || checked.current_authority().map(|_| ())),
    ))
    .await;
    let reverse = FederationRpcClient::new(server.port, fixture.satellite_channel.clone(), {
        let checked = fixture.satellite_channel.clone();
        Arc::new(move || checked.current_authority().map(|_| ()))
    })
    .unwrap();
    for (app, channel, rpc) in [
        (&fixture.central, &fixture.channel, fixture.rpc.clone()),
        (&fixture.satellite, &fixture.satellite_channel, reverse),
    ] {
        app.federation_peer_handles.lock().unwrap().insert(
            channel.snapshot.peer.host_id.clone(),
            Arc::new(FederationPeerHandle {
                channel: channel.clone(),
                rpc,
            }),
        );
    }
    (
        server,
        Handles(vec![fixture.central.clone(), fixture.satellite.clone()]),
    )
}

async fn setup(
    fixture: &Fixture,
    key: &str,
) -> (String, String, PathBuf, CausalTestCompletionSelector) {
    let requester = fixture
        .central
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap()
        .principal;
    let (_, run, directory) = producer(fixture.satellite.clone(), key, |runner| async move {
        let request = SendMailboxMessageRequest {
            target_session_id: format!(
                "host:{}::{runner}",
                fixture.satellite_channel.snapshot.local.host_id
            ),
            message: "run the original selected request".into(),
            idempotency_key: key.into(),
            topic: None,
            state_stamp: None,
            class: None,
        };
        let input = crate::tests::mailboxes::test_request::typed_request(MailboxTestMode::Full);
        let mut capture = fixture
            .central
            .prepare_mailbox_test_request(&fixture.native, key, Some(&input))
            .unwrap()
            .unwrap();
        // Private retention fixture data, not a live Engram claim or credit.
        capture.sender_scope = Some(MailboxTestSenderScope {
            project_id: "fixture-project".into(),
            store: EngramAuthorityStoreKey {
                database_path: fixture
                    .central
                    .persistence_path
                    .with_extension("secret-engram-fixture"),
                project_id: "fixture-store".into(),
            },
        });
        let receipt = fixture
            .central
            .send_native_mailbox_relay_with_test_custody(&fixture.native, &request, Some(&capture))
            .await
            .unwrap()
            .unwrap();
        let page = fixture
            .satellite
            .read_native_mailbox_relay(
                &runner,
                MailboxRelayLocator::decode(&receipt.mailbox_id, false)
                    .unwrap()
                    .unwrap(),
                ReadMailboxRequest {
                    read_key: None,
                    issue_receipt: false,
                    after_sequence: None,
                    limit: 50,
                },
            )
            .await
            .unwrap();
        page.messages
            .iter()
            .find(|message| message.id == receipt.message_id)
            .expect("recipient page contains the original typed request")
            .id
            .clone()
    })
    .await;
    let registration = fixture
        .satellite
        .inner
        .lock()
        .unwrap()
        .test_run_registrations[&run]
        .clone();
    let causal = test_run_causal_request(&registration).unwrap();
    assert_eq!(causal.payload.requester, requester);
    let selector = CausalTestCompletionSelector {
        canonical_message_id: causal.canonical_message_id.clone(),
        payload: causal.payload.clone(),
    };
    (registration.owning_session_id, run, directory, selector)
}

async fn wire(
    fixture: &Fixture,
    selector: CausalTestCompletionSelector,
) -> Result<CausalTestCompletionReply, ApiError> {
    let handle = FederationPeerHandle {
        channel: fixture.channel.clone(),
        rpc: fixture.rpc.clone(),
    };
    fixture
        .central
        .request_mailbox_relay(
            &handle,
            &fixture.channel.current_lease().unwrap(),
            MailboxRelayOperation::ReadCausalTestCompletion { selector },
        )
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn causal_completion_reads_original_request_pending_then_truthful_terminals_without_local_runs()
 {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for verdict in [
        native_test_completion::Verdict::Pass,
        native_test_completion::Verdict::Fail,
        native_test_completion::Verdict::Interrupted,
    ] {
        let fixture = Fixture::production_shape().await;
        let (mut server, handles) = connect(&fixture).await;
        let (_, run, directory, selector) = setup(&fixture, "original-causal-terminal").await;
        assert!(
            wire(&fixture, selector.clone())
                .await
                .unwrap()
                .completion
                .is_none()
        );
        let mut cursor = CausalTestCompletionCursor {
            intents: true,
            ..Default::default()
        };
        assert!(
            fixture
                .central
                .read_causal_test_completion_batch(&mut cursor)
                .await
                .unwrap()
                .is_empty()
        );
        finish(
            &fixture.satellite,
            &run,
            &directory,
            verdict,
            Value::Null,
            Value::Null,
        );
        let original = wire(&fixture, selector.clone())
            .await
            .unwrap()
            .completion
            .unwrap();
        assert_eq!(original.result.verdict, verdict);
        assert!(matches!(
            original.input,
            native_test_completion::Input::SourceId(_)
        ));
        assert_eq!(
            serde_json::to_value(&original).unwrap()["input"],
            json!({"kind":"source_id","source_id_before":null,"source_id_after":null})
        );
        // A pending read must not consume the original. Reuse the same
        // cursor across both sources after native completion becomes available.
        let mut recovered = Vec::new();
        for _ in 0..2 {
            recovered.extend(
                fixture
                    .central
                    .read_causal_test_completion_batch(&mut cursor)
                    .await
                    .unwrap(),
            );
        }
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].completion, original);
        assert_eq!(
            recovered[0].selector.canonical_message_id,
            selector.canonical_message_id
        );
        // Replaced launcher telemetry cannot rewrite an already retained
        // authoritative native terminal or turn a later read into a new run.
        finish(
            &fixture.satellite,
            &run,
            &directory,
            if verdict == native_test_completion::Verdict::Pass {
                native_test_completion::Verdict::Fail
            } else {
                native_test_completion::Verdict::Pass
            },
            Value::Null,
            Value::Null,
        );
        assert_eq!(
            wire(&fixture, selector.clone()).await.unwrap().completion,
            Some(original.clone())
        );
        // A fresh process cursor needs no retained delivered bit. Repeated
        // reads return the same original, without a local run or observer row.
        for _ in 0..2 {
            let mut cursor = CausalTestCompletionCursor {
                intents: true,
                ..Default::default()
            };
            let deliveries = fixture
                .central
                .read_causal_test_completion_batch(&mut cursor)
                .await
                .unwrap();
            assert_eq!(deliveries.len(), 1);
            assert_eq!(deliveries[0].completion, original);
            assert_eq!(
                deliveries[0].selector.canonical_message_id,
                selector.canonical_message_id
            );
            assert!(
                deliveries[0]
                    .scope
                    .as_ref()
                    .unwrap()
                    .store
                    .database_path
                    .ends_with(
                        fixture
                            .central
                            .persistence_path
                            .with_extension("secret-engram-fixture")
                    )
            );
        }
        let inner = fixture.central.inner.lock().unwrap();
        assert!(inner.test_run_registrations.is_empty());
        assert!(inner.remote_test_run_facts.is_empty());
        drop(inner);
        drop(handles);
        server.finish().await;
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn causal_completion_preserves_compacted_original_after_reload_and_refuses_conflicting_custody()
 {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) = connect(&fixture).await;
    let (_, run, directory, selector) = setup(&fixture, "causal-compaction").await;
    let source = launcher_source_id_fixture();
    let mut changed = source.clone();
    changed["tree"] = json!("f".repeat(40));
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Fail,
        source.clone(),
        changed.clone(),
    );
    let original = wire(&fixture, selector.clone())
        .await
        .unwrap()
        .completion
        .unwrap();
    assert_eq!(
        serde_json::to_value(&original).unwrap()["input"],
        json!({"kind":"source_id","source_id_before":source,"source_id_after":changed})
    );
    compact(&fixture.satellite, &run);
    let loaded = load_state(fixture.satellite.persistence_path.as_path())
        .unwrap()
        .unwrap();
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        inner.test_run_registrations = loaded.test_run_registrations;
        inner.test_run_compacted = loaded.test_run_compacted;
        inner.test_run_observations = loaded.test_run_observations;
        inner.test_runs = TestRunIndex::default();
    }
    assert_eq!(
        wire(&fixture, selector.clone()).await.unwrap().completion,
        Some(original.clone())
    );
    let retained = fixture.satellite.inner.lock().unwrap().test_run_compacted[&run].clone();
    // A corrupted or ambiguous ORIGINAL execution does not fall back to a
    // nearby run, an observer fact, or a copy of raw launcher results.
    for fault in ["missing-execution", "duplicate-execution", "wrong-request"] {
        let mut altered = retained.clone();
        let execution = altered
            .registration
            .as_ref()
            .unwrap()
            .native_completion
            .as_ref()
            .unwrap()
            .execution_id
            .clone();
        let position = altered
            .observations
            .iter()
            .position(|row| row.execution_id == execution)
            .unwrap();
        if fault == "missing-execution" {
            altered.observations.remove(position);
        } else if fault == "duplicate-execution" {
            altered
                .observations
                .push(altered.observations[position].clone());
        } else {
            altered.observations[position]
                .artifact
                .as_mut()
                .unwrap()
                .request_identity = Some("f".repeat(64));
        }
        {
            let mut inner = fixture.satellite.inner.lock().unwrap();
            inner.test_run_compacted.insert(run.clone(), altered);
            fixture.satellite.persist_internal_locked(&inner).unwrap();
        }
        assert!(
            wire(&fixture, selector.clone())
                .await
                .unwrap()
                .completion
                .is_none(),
            "{fault}"
        );
    }
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        inner.test_run_compacted.insert(run.clone(), retained);
        fixture.satellite.persist_internal_locked(&inner).unwrap();
    }
    let reply = wire(&fixture, selector).await.unwrap();
    assert_eq!(reply.completion, Some(original));
    assert!(
        !serde_json::to_string(&reply)
            .unwrap()
            .contains("secret-engram-fixture")
    );
    drop(handles);
    server.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn causal_completion_requires_exact_request_runner_authority_and_durable_registration() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) = connect(&fixture).await;
    let (_, run, directory, selector) = setup(&fixture, "causal-fences").await;
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Pass,
        Value::Null,
        Value::Null,
    );
    let original = wire(&fixture, selector.clone())
        .await
        .unwrap()
        .completion
        .unwrap();
    let mut unknown = selector.clone();
    unknown.canonical_message_id = format!("mailbox-message-{}", Uuid::new_v4());
    assert!(wire(&fixture, unknown).await.unwrap().completion.is_none());
    for fault in [
        "requester-host",
        "runner-store",
        "runner-creation",
        "payload",
    ] {
        let mut altered = selector.clone();
        match fault {
            "requester-host" => altered.payload.requester.host.host_id = Uuid::new_v4().to_string(),
            "runner-store" => {
                altered.payload.recipient.as_mut().unwrap().store_id = Uuid::new_v4().to_string()
            }
            "runner-creation" => {
                altered
                    .payload
                    .recipient
                    .as_mut()
                    .unwrap()
                    .native_creation_id = Uuid::new_v4().to_string()
            }
            _ => altered.payload.mode = MailboxTestMode::Focused,
        }
        assert!(wire(&fixture, altered).await.is_err(), "{fault}");
    }
    let retained = fixture
        .satellite
        .inner
        .lock()
        .unwrap()
        .test_run_registrations[&run]
        .clone();
    // A newly visible in-memory result is not its SQLite read-back proof.
    fixture
        .satellite
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .update(&run, |row| {
            row.native_completion
                .as_mut()
                .unwrap()
                .completion
                .result
                .reason = "not durable".into();
        });
    assert!(
        wire(&fixture, selector.clone())
            .await
            .unwrap()
            .completion
            .is_none()
    );
    fixture
        .satellite
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .insert(run.clone(), retained.clone());
    assert_eq!(
        wire(&fixture, selector.clone()).await.unwrap().completion,
        Some(original)
    );
    let mut duplicate = retained;
    duplicate.run_id = "test-other-causal-run".into();
    fixture
        .satellite
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .insert(duplicate.run_id.clone(), duplicate);
    assert!(
        wire(&fixture, selector).await.is_err(),
        "two indexed original registrations must not be chosen arbitrarily"
    );
    drop(handles);
    server.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn causal_completion_does_not_promote_legacy_native_input_to_a_new_source_delivery() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) = connect(&fixture).await;
    let (_, run, directory, selector) = setup(&fixture, "causal-legacy-refusal").await;
    crate::tests::test_runs::test_run_registration::finish_native_completion_producer(
        &fixture.satellite,
        &run,
        &directory,
        true,
    );
    let retained = fixture
        .satellite
        .inner
        .lock()
        .unwrap()
        .test_run_registrations[&run]
        .clone();
    assert!(matches!(
        retained
            .native_completion
            .as_ref()
            .unwrap()
            .completion
            .input,
        native_test_completion::Input::Legacy(_)
    ));
    assert!(wire(&fixture, selector).await.is_err());
    assert_eq!(
        fixture
            .satellite
            .inner
            .lock()
            .unwrap()
            .test_run_registrations[&run],
        retained,
        "historical facts remain unchanged rather than acquiring inferred SourceIds"
    );
    drop(handles);
    server.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn causal_batch_pages_past_ordinary_history_and_recovers_disconnected_reads_without_new_intents()
 {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) = connect(&fixture).await;
    let (_, run, directory, _) = setup(&fixture, "causal-bounded-batch").await;
    finish(
        &fixture.satellite,
        &run,
        &directory,
        native_test_completion::Verdict::Pass,
        Value::Null,
        Value::Null,
    );
    {
        let connection = fixture.central.mailbox_store.connection().unwrap();
        // Ordinary historical intents are valid copies with no typed request.
        for number in 0..80 {
            connection.execute("INSERT INTO mailbox_relay_intents
                (sender_key,owner_session_id,idempotency_key,record_version,request_digest,requested_target,conversation_json,peer_json,receipt_json,created_at)
                SELECT sender_key,owner_session_id,?1,record_version,request_digest,requested_target,conversation_json,peer_json,NULL,created_at
                FROM mailbox_relay_intents WHERE idempotency_key='causal-bounded-batch'",
                [format!("ordinary-{number}")]).unwrap();
        }
        // Rowid is only the traversal position, never an authority field.
        connection.execute("UPDATE mailbox_relay_intents SET rowid=1000 WHERE idempotency_key='causal-bounded-batch'", []).unwrap();
    }
    let before: u64 = fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .query_row("SELECT count(*) FROM mailbox_relay_intents", [], |row| {
            row.get(0)
        })
        .unwrap();
    let mut cursor = CausalTestCompletionCursor {
        intents: true,
        ..Default::default()
    };
    assert!(
        fixture
            .central
            .read_causal_test_completion_batch(&mut cursor)
            .await
            .unwrap()
            .is_empty(),
        "ordinary history is paged before typed filtering, not scanned without a bound"
    );
    fixture
        .central
        .federation_peer_handles
        .lock()
        .unwrap()
        .clear();
    for _ in 0..6 {
        assert!(
            fixture
                .central
                .read_causal_test_completion_batch(&mut cursor)
                .await
                .unwrap()
                .is_empty()
        );
    }
    fixture
        .central
        .federation_peer_handles
        .lock()
        .unwrap()
        .insert(
            fixture.channel.snapshot.peer.host_id.clone(),
            Arc::new(FederationPeerHandle {
                channel: fixture.channel.clone(),
                rpc: fixture.rpc.clone(),
            }),
        );
    let mut recovered = Vec::new();
    for _ in 0..8 {
        recovered.extend(
            fixture
                .central
                .read_causal_test_completion_batch(&mut cursor)
                .await
                .unwrap(),
        );
    }
    assert!(
        !recovered.is_empty(),
        "a missed/disconnected read is recovered by a bounded later pass"
    );
    assert!(
        recovered
            .iter()
            .all(|delivery| delivery.completion.run_id == run)
    );
    let after: u64 = fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .query_row("SELECT count(*) FROM mailbox_relay_intents", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        before, after,
        "reads create no replacement send intent or delivery ledger"
    );
    drop(handles);
    server.finish().await;
    fixture.finish().await;
}
