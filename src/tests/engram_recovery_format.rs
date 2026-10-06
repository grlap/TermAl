//! Dormant format contracts: no admission, provider delivery or loader is activated.
use super::*;

fn original_store_key() -> EngramAuthorityStoreKey {
    EngramAuthorityStoreKey {
        project_id: "original-store-project".to_owned(),
        database_path: PathBuf::from("C:/github/Personal/TermAl/.tmp/original-store.sqlite"),
    }
}

fn expected_settings_wire() -> serde_json::Value {
    json!({
        "enabled": true,
        "turnGatedControl": true,
        "acceptanceEvaluation": {
            "defaultMode": "independent_session",
            "evaluatorAgent": "Claude",
            "evaluatorModel": "fixture-evaluator"
        },
        "binaryPath": "C:/github/Personal/TermAl/.tmp/original-binary.exe",
        "home": "C:/github/Personal/TermAl/.tmp/original-settings-home",
        "deadlineMs": 1234,
        "authorityStoreKey": {
            "projectId": "original-store-project",
            "databasePath": "C:/github/Personal/TermAl/.tmp/original-store.sqlite"
        }
    })
}

fn authority(session_id: &str) -> EngramRecoveryAuthority {
    EngramRecoveryAuthority {
        project_id: "local-project".to_owned(),
        connection: EngramConnectionConfig {
            binary_path: PathBuf::from("C:/github/Personal/TermAl/.tmp/engram.exe"),
            project_file: PathBuf::from("C:/github/Personal/TermAl/.engram-project"),
            home: PathBuf::from("C:/github/Personal/TermAl/.tmp/store-home"),
            project_root: PathBuf::from("C:/github/Personal/TermAl"),
            actor_id: "original-actor".to_owned(),
            actor_context: Some("original-context".to_owned()),
            session_id: session_id.to_owned(),
        },
        settings: EngramProjectSettings {
            enabled: true,
            turn_gated_control: true,
            acceptance_evaluation: Some(AcceptanceEvaluatorDefaults {
                default_mode: Some(AcceptanceEvaluationMode::IndependentSession),
                evaluator_agent: Some(Agent::Claude),
                evaluator_model: Some("fixture-evaluator".to_owned()),
            }),
            binary_path: Some("C:/github/Personal/TermAl/.tmp/original-binary.exe".to_owned()),
            home: Some("C:/github/Personal/TermAl/.tmp/original-settings-home".to_owned()),
            // The retired in-process grant is intentionally not persisted.
            work_authority_grant: None,
            authority_store_key: Some(original_store_key()),
            deadline_ms: Some(1234),
        },
        work_binding: Some(EngramControlWorkBinding {
            root_execution_id: "root".to_owned(),
            work_id: "work".to_owned(),
            run_id: "run".to_owned(),
            work_revision: 19,
            claim_id: "claim".to_owned(),
            claim_fence: 27,
        }),
        origin: EngramRecoveryOrigin::ModernBegin,
    }
}

fn envelope(session_id: &str) -> EngramRecoveryEnvelope<EngramBeginReplay> {
    EngramRecoveryEnvelope {
        schema_version: 1,
        envelope_id: "original-operation".to_owned(),
        revision: 41,
        authority: authority(session_id),
        payload: serde_json::from_value(json!({
            "routingToken": "original-routing",
            "originalHead": head(
                "original-head",
                "entire first line\nsecond line\nżółw 🐢"
            ),
            "promptId": "original-head",
            "fingerprint": engram_turn_intent_fingerprint(
                "entire first line\nsecond line\nżółw 🐢",
                None,
                &[],
                None,
                QueuedPromptSource::User,
            ),
            "dispatchGeneration": 97,
            "grantId": "original-grant",
            "deliveryTokens": ["first-delivery", "second-delivery"],
            "idempotencyKey": "original-begin-key",
            "grantBasis": {"sourceRevision":"content-v1:original", "effects":["observe"]}
        }))
        .unwrap(),
        phase: EngramRecoveryPhase::Prepared,
        settlement: Some(json!({"decision":"unknown", "receipt":{"sequence":7}})),
        attempts: 3,
        held_since: "2026-10-01T01:02:03Z".to_owned(),
        due_at: "2026-10-01T01:02:04Z".to_owned(),
    }
}

fn head(id: &str, text: &str) -> QueuedPromptRecord {
    serde_json::from_value(json!({
        "source":"user", "attachments":[], "promoted_message_index":7,
        "promotion_disposition_known":true, "engram_waiting":true,
        "pending_prompt":{"id":id,"timestamp":"2026-10-01T01:02:03Z","text":text}
    }))
    .unwrap()
}

#[test]
fn engram_recovery_dormant_contracts_do_not_change_the_live_serializer_or_loader() {
    let root = TestTempRoot::create("dormant-recovery-baseline");
    let mut inner = StateInner::new();
    let id = inner
        .create_session(
            Agent::Codex,
            None,
            root.path().to_string_lossy().into_owned(),
            None,
            None,
        )
        .session
        .id;
    let index = inner.find_session_index(&id).unwrap();
    let record = &mut inner.sessions[index];
    record
        .queued_prompts
        .push_back(head("ordinary-head", "ordinary text"));
    record.queued_prompts[0].engram_waiting = false;
    let persisted = PersistedSessionRecord::from_record(record);
    let row = serde_json::to_value(&persisted).unwrap();
    for key in [
        "engramBeginRecovery",
        "engramBeginRecoveryAudit",
        "engramBeginRecoveryFormat",
        "engramBeginOperationId",
        "engramBeginCancelCapture",
        "engramBeginCancelAudit",
    ] {
        assert!(
            row.get(key).is_none(),
            "dormant contracts must not add a persisted field: {key}"
        );
    }
    let restored = persisted.into_record().unwrap();
    assert!(!restored.orchestrator_auto_dispatch_blocked);
    assert!(!restored.session.queue_paused);
    assert_eq!(
        restored.queued_prompts[0].pending_prompt.id,
        "ordinary-head"
    );
    assert_eq!(
        restored.queued_prompts[0].pending_prompt.text,
        "ordinary text"
    );
    assert!(restored.engram.uncertain_grant_id.is_none());
    let mut future_row = row;
    future_row["engramBeginRecovery"] = json!({"malformed":"future-only"});
    future_row["engramBeginCancelCapture"] = json!({"malformed":"future-only"});
    let ignored_future: PersistedSessionRecord = serde_json::from_value(future_row).unwrap();
    let restored = ignored_future.into_record().unwrap();
    assert!(!restored.orchestrator_auto_dispatch_blocked);
    assert!(!restored.session.queue_paused);
    assert_eq!(
        restored.queued_prompts[0].pending_prompt.id,
        "ordinary-head"
    );
}

#[test]
fn engram_recovery_roundtrip_preserves_exact_authority_payload_and_phase() {
    let original = envelope("original-session");
    for phase in [
        EngramRecoveryPhase::Prepared,
        EngramRecoveryPhase::Settled,
        EngramRecoveryPhase::Held,
        EngramRecoveryPhase::OperatorCancelled,
        EngramRecoveryPhase::Delivered,
        EngramRecoveryPhase::Closed,
    ] {
        let mut expected = original.clone();
        expected.phase = phase;
        let encoded = serde_json::to_vec(&expected).unwrap();
        let encoded_value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(
            encoded_value["authority"]["settings"],
            expected_settings_wire()
        );
        let restored: EngramRecoveryEnvelope<EngramBeginReplay> =
            serde_json::from_slice(&encoded).unwrap();
        assert_eq!(restored, expected);
        assert_eq!(restored.authority.settings, expected.authority.settings);
        assert_eq!(
            restored.authority.settings.authority_store_key,
            Some(original_store_key())
        );
        let attempt = EngramRecoveryAttempt {
            envelope_id: restored.envelope_id.clone(),
            revision: restored.revision,
            attempt_id: "attempt".to_owned(),
            authority: restored.authority.clone(),
        };
        assert_eq!(attempt.authority, original.authority);
        assert_eq!(attempt.revision, 41);
    }
}

#[test]
fn engram_recovery_causal_wire_reconstruction_preserves_original_routing_and_all_begin_fields() {
    let original = envelope("original-session");
    let stored = serde_json::to_vec(&original).unwrap();
    let stored_value: serde_json::Value = serde_json::from_slice(&stored).unwrap();
    assert_eq!(
        stored_value["authority"]["settings"],
        expected_settings_wire()
    );
    // Routing and work authority may have changed since preparation. They are
    // deliberately unavailable to reconstruction, which consumes stored bytes.
    let mut current = original.clone();
    current.authority.connection.session_id = "successor-session".to_owned();
    current.authority.work_binding.as_mut().unwrap().claim_id = "successor-claim".to_owned();
    let restored: EngramRecoveryEnvelope<EngramBeginReplay> =
        serde_json::from_slice(&stored).unwrap();
    let payload = serde_json::to_value(&restored.payload).unwrap();
    let request: EngramControlRequest = serde_json::from_value(json!({
        "operation": "turn_begin",
        "routing_token": payload["routingToken"],
        "grant_id": payload["grantId"],
        "delivery_tokens": payload["deliveryTokens"],
        "idempotency_key": payload["idempotencyKey"]
    }))
    .expect("the typed stored payload must reconstruct the actual Begin wire request");
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        json!({
            "operation": "turn_begin", "routing_token": "original-routing",
            "grant_id": "original-grant", "delivery_tokens": ["first-delivery", "second-delivery"],
            "idempotency_key": "original-begin-key"
        })
    );
    assert_eq!(restored, original);
    assert_eq!(restored.authority.settings, original.authority.settings);
    assert_eq!(
        restored.authority.settings.authority_store_key,
        Some(original_store_key())
    );
    assert_ne!(restored.authority, current.authority);
    assert_eq!(payload["dispatchGeneration"], 97);
    assert_eq!(
        payload["originalHead"]["pending_prompt"]["text"],
        "entire first line\nsecond line\nżółw 🐢"
    );
    assert_eq!(
        payload["grantBasis"],
        json!({"sourceRevision":"content-v1:original", "effects":["observe"]})
    );
}

#[test]
fn engram_recovery_non_default_settings_detect_each_missing_wire_field() {
    let original = envelope("original-session");
    let expected_settings = original.authority.settings.clone();
    let encoded = serde_json::to_value(&original).unwrap();
    let complete = encoded["authority"]["settings"].clone();
    assert_eq!(complete, expected_settings_wire());
    // This retired in-process grant is deliberately excluded from persistence.
    assert!(expected_settings.work_authority_grant.is_none());
    assert!(complete.get("workAuthorityGrant").is_none());
    let restored: EngramProjectSettings = serde_json::from_value(complete.clone()).unwrap();
    assert_eq!(restored, expected_settings);
    for (parent, field) in [
        ("", "enabled"),
        ("", "turnGatedControl"),
        ("", "acceptanceEvaluation"),
        ("", "binaryPath"),
        ("", "home"),
        ("", "authorityStoreKey"),
        ("", "deadlineMs"),
        ("/acceptanceEvaluation", "defaultMode"),
        ("/acceptanceEvaluation", "evaluatorAgent"),
        ("/acceptanceEvaluation", "evaluatorModel"),
        ("/authorityStoreKey", "projectId"),
        ("/authorityStoreKey", "databasePath"),
    ] {
        let mut damaged = complete.clone();
        assert!(
            damaged
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(field)
                .is_some()
        );
        // Defaults may admit a lost optional field, but must expose its loss.
        // A missing required store-key component may instead reject decoding.
        if let Ok(restored) = serde_json::from_value::<EngramProjectSettings>(damaged) {
            assert_ne!(restored, expected_settings, "lost {parent}/{field}");
        }
    }
}

#[test]
fn engram_recovery_local_fault_clone_and_handoff_preserve_distinct_kinds_and_details() {
    let detail = "same ACK/reply detail\nżółw 🐢";
    let faults = [
        EngramBeginLocalFault::PrepareAck(detail.to_owned()),
        EngramBeginLocalFault::ResultAck(detail.to_owned()),
        EngramBeginLocalFault::InvalidReply(detail.to_owned()),
        EngramBeginLocalFault::StaleOwner,
    ];
    for (index, fault) in faults.iter().enumerate() {
        for other in &faults[index + 1..] {
            assert_ne!(std::mem::discriminant(fault), std::mem::discriminant(other));
        }
    }
    let assert_preserved = |actual: &EngramBeginLocalFault, expected: &EngramBeginLocalFault| {
        assert_eq!(
            std::mem::discriminant(actual),
            std::mem::discriminant(expected)
        );
        match actual {
            EngramBeginLocalFault::PrepareAck(actual_detail)
            | EngramBeginLocalFault::ResultAck(actual_detail)
            | EngramBeginLocalFault::InvalidReply(actual_detail) => {
                assert_eq!(actual_detail, detail);
            }
            EngramBeginLocalFault::StaleOwner => {}
        }
    };
    for fault in &faults {
        assert_preserved(&fault.clone(), fault);
    }
    // These are local fault values, not a persisted wire format. Round-trip
    // their ownership through a worker without inventing serialization support.
    let (request_tx, request_rx) = mpsc::channel::<EngramBeginLocalFault>();
    let (reply_tx, reply_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        for fault in request_rx {
            if reply_tx.send(fault.clone()).is_err() {
                break;
            }
        }
    });
    for fault in &faults {
        request_tx.send(fault.clone()).unwrap();
    }
    drop(request_tx);
    let replies: Vec<_> = faults
        .iter()
        .map(|_| phase_sync::receive_before_cleanup(&reply_rx, "local fault round trip"))
        .collect();
    drop(reply_rx);
    worker.join().unwrap();
    for (reply, expected) in replies.into_iter().zip(&faults) {
        assert_preserved(&reply.unwrap(), expected);
    }
}

#[test]
fn engram_recovery_ack_uses_the_worker_off_lock_and_preserves_failure_results() {
    for result in [
        Ok(()),
        Err(PersistFenceError::WriteFailed(
            "actual-write-failure".to_owned(),
        )),
        Err(PersistFenceError::Shutdown),
        Err(PersistFenceError::WorkerStopped),
    ] {
        let mut state = test_app_state();
        let (tx, rx) = mpsc::channel();
        state.persist_tx = tx;
        let state = Arc::new(state);
        let caller = state.clone();
        let worker = std::thread::spawn(move || {
            caller.acknowledge_engram_recovery(
                "original-session",
                json!({"exact":"image"}),
                std::time::Instant::now() + phase_sync::DEADLOCK_GUARD,
            )
        });
        let request = phase_sync::receive(&rx, "recovery ACK worker request");
        let PersistRequest::Fence(fence) = request else {
            panic!("ACK must enqueue a fence");
        };
        let (probe_tx, probe_rx) = mpsc::channel();
        let probe_state = state.clone();
        let probe = std::thread::spawn(move || {
            let acquired = probe_state.inner.lock().is_ok();
            let _ = probe_tx.send(acquired);
        });
        let off_lock =
            phase_sync::receive_before_cleanup(&probe_rx, "Inner acquired during ACK wait");
        let target_matches = matches!(&fence.target, PersistFenceTarget::EngramRecovery {session_id, content}
            if session_id == "original-session" && content == &json!({"exact":"image"}));
        // Release the ACK before joining/asserting, including a failed probe.
        fence.finish(result.clone());
        let actual = worker.join().unwrap();
        probe.join().unwrap();
        assert_eq!(off_lock, Ok(true), "ACK must not hold Inner while waiting");
        assert!(target_matches);
        assert_eq!(actual, result);
    }
}

#[test]
fn engram_recovery_ack_deadline_or_disconnected_worker_never_uses_manual_persistence() {
    let state = test_app_state();
    let before = state.persistence_path.exists();
    assert_eq!(
        state.acknowledge_engram_recovery("absent", json!({}), std::time::Instant::now()),
        Err(PersistFenceError::Deadline)
    );
    assert_eq!(
        state.acknowledge_engram_recovery(
            "absent",
            json!({}),
            std::time::Instant::now() + phase_sync::DEADLOCK_GUARD
        ),
        Err(PersistFenceError::WorkerStopped)
    );
    assert_eq!(state.persistence_path.exists(), before);
}
