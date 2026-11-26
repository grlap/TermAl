// Sender-to-artifact-to-launcher-summary witnesses; all producer calls are
// scripted and use disposable fixture files, never a live store or a replay.
use super::*;

pub(super) fn summary(run_dir: &FsPath) -> String {
    let output = crate::host_command::Command::new("node")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/test-launcher.mjs"))
        .arg("summary")
        .arg(run_dir)
        .output()
        .expect("Node can read the fixture summary");
    assert!(output.status.success(), "{}", output.stderr.len());
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn typed_producer_refusal_reaches_the_retained_record_and_actual_summary() {
    let fixture = RunFixture::new("verification-refusal-summary");
    let run = record_run(
        &fixture,
        "test-refusal-summary",
        commit_request(&fixture.root),
        commit_results(),
    );
    let transport = Arc::new(RecordingTransport::default());
    let adapter = adapter(&transport);
    let own = connection(SESSION, "actor");
    read_work_session(&transport, &adapter, &own, supported());
    let wire = json!({"status":"error", "error":{
        "code":"verification_record_refused",
        "message":format!("execution_observe source_id {}", source_projection::source_for_refusal_test()),
        "details":{"field":"root", "reason":"secret-not-for-output", "ignored":"secret-not-for-output"}
    }});
    let EngramControlResponse::Error { error } = serde_json::from_value(wire).unwrap() else {
        panic!("error envelope")
    };
    transport.script(Err(EngramTransportError::remote(error.into_body())));
    let frozen = ready(&run.registration);
    let original_body_digest = engram_verification_record_body_digest(&frozen.body);
    let step = engram_verification_record_send(frozen, link(&adapter, &own));
    let EngramVerificationRecordStep::ProducerRejected {
        detail,
        body_digest,
    } = &step
    else {
        panic!("{step:?}")
    };
    assert_eq!(body_digest, &original_body_digest);
    assert_eq!(detail, "Engram refused it: verification_record_refused");
    assert!(!detail.contains("source_id"));
    assert!(!detail.contains("execution_observe"));
    assert!(!detail.contains("secret-not-for-output"));
    assert_eq!(
        engram_verification_record_progress(&run.run_dir),
        Some(EngramVerificationRecordProgress::Rejected {
            detail: detail.clone()
        })
    );
    let rendered = summary(&run.run_dir);
    assert!(rendered.contains("verification: rejected:"), "{rendered}");
    assert!(
        rendered.contains("verification_record_refused"),
        "{rendered}"
    );
    assert!(!rendered.contains("secret-not-for-output"));
    assert!(!rendered.contains("private-child.txt"));
    assert_eq!(
        transport.sent_operations(),
        ["work_session_read", "verification_record"]
    );
    // A terminal refusal must remain terminal without another producer send.
    register(&fixture, &[&run]);
    fixture
        .state
        .note_engram_verification_record_step(&run.registration, &step);
    let stored = current(&fixture, &run);
    assert!(matches!(
        &stored.verification_record,
        Some(TestRunVerificationAck::Rejected { detail: saved, producer_body_digest: Some(saved_digest) })
            if saved == detail && saved_digest == &original_body_digest
    ));
    assert!(matches!(
        engram_verification_record_prepare(&stored),
        EngramVerificationRecordPrepared::Finished
    ));
}

#[test]
fn command_limit_names_both_fields_and_counts_utf8_without_sending_or_rewriting() {
    for field in ["run", "check"] {
        let fixture = RunFixture::new(&format!("verification-command-limit-{field}"));
        let run = record_run(
            &fixture,
            "test-command-limit",
            commit_request(&fixture.root),
            commit_results(),
        );
        let transport = Arc::new(RecordingTransport::default());
        let adapter = adapter(&transport);
        let own = connection(SESSION, "actor");
        read_work_session(&transport, &adapter, &own, supported());
        let mut frozen = ready(&run.registration);
        // A long selector: byte length, not Unicode scalar count, is the contract.
        let command = format!("cargo test {}", "é".repeat(4575));
        let command_bytes = command.len();
        if field == "run" {
            frozen.body.run.command = command.clone();
        } else {
            frozen.body.checks[0].command = command.clone();
        }
        frozen.record = serde_json::to_value(&frozen.body).unwrap();
        let body_path = run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE);
        let body_before = fs::read(&body_path).unwrap();
        let step = engram_verification_record_send(frozen, link(&adapter, &own));
        let EngramVerificationRecordStep::Rejected(detail) = step else {
            panic!("{step:?}")
        };
        assert!(detail.contains("verification_command_too_long"));
        assert!(detail.contains(&format!("{command_bytes} UTF-8 bytes")));
        assert!(detail.contains("Engram limit is 8192 bytes"));
        assert!(detail.contains(if field == "run" {
            "run.command"
        } else {
            "checks[0].command"
        }));
        assert_eq!(transport.sent_operations(), ["work_session_read"]);
        assert_eq!(
            fs::read(&body_path).unwrap(),
            body_before,
            "do not rewrite the frozen record"
        );
        assert!(summary(&run.run_dir).contains(&detail));
    }
}

#[test]
fn command_at_8192_bytes_still_reaches_the_producer_unchanged() {
    let fixture = RunFixture::new("verification-command-boundary");
    let run = record_run(
        &fixture,
        "test-command-boundary",
        commit_request(&fixture.root),
        commit_results(),
    );
    let transport = Arc::new(RecordingTransport::default());
    let adapter = adapter(&transport);
    let own = connection(SESSION, "actor");
    read_work_session(&transport, &adapter, &own, supported());
    let mut frozen = ready(&run.registration);
    let command = "x".repeat(8192);
    frozen.body.run.command = command.clone();
    frozen.body.checks[0].command = command.clone();
    frozen.record = serde_json::to_value(&frozen.body).unwrap();
    transport.script(Err(EngramTransportError::remote(EngramControlErrorBody {
        code: "verification_record_refused".into(),
        message: "scripted boundary refusal".into(),
    })));
    let _ = engram_verification_record_send(frozen, link(&adapter, &own));
    assert_eq!(
        transport.sent_operations(),
        ["work_session_read", "verification_record"]
    );
    let sent = &transport.sent_records()[0];
    assert_eq!(sent["run"]["command"], command);
    assert_eq!(sent["checks"][0]["command"], command);
}

/// The fixture full gate, passed on its exact committed input with equal
/// source ids.
fn passed_results() -> Value {
    let mut results = commit_results();
    results["state"] = json!("passed");
    results["exitCode"] = json!(0);
    results["stages"][4] = stage("vitest", "passed", 0, &VITEST_ARGV, 4);
    results
}

fn binding_refusal(field: &str) -> EngramTransportError {
    let wire = json!({"status": "error", "error": {
        "code": "verification_record_refused",
        "message": "private producer message",
        "details": {"field": field, "reason": "private producer reason"},
    }});
    let EngramControlResponse::Error { error } = serde_json::from_value(wire).unwrap() else {
        panic!("error envelope");
    };
    error.into_transport_error()
}

fn ready_binding(run: &RecordRun, holder: &str, workspace: &str) -> Value {
    let binding = run.registration.work_binding.as_ref().unwrap();
    let position = json!({
        "feed": {"kind": "run_execution", "id": binding.run_id},
        "position": 12,
    });
    let generation = run.registration.named_root.as_ref().unwrap().generation + 1;
    json!({
        "project_id": store().project_id,
        "work_id": binding.work_id,
        "run_id": binding.run_id,
        "claim_id": binding.claim_id,
        "root_execution_id": binding.root_execution_id,
        "run": {"state": "active", "generation": 1},
        "claim": {
            "state": "active", "holder": holder, "fence": binding.claim_fence + 1,
            "expires_at": "2099-01-01T00:00:00Z",
        },
        "named_root": {
            "state": "bound", "workspace_id": workspace,
            "generation": generation, "named_at": STARTED,
        },
        "latest_event": {
            "kind": "bound", "event": "bound-event", "position": position,
            "workspace_id": workspace, "generation": generation, "named_at": STARTED,
        },
        "read_cut": position,
    })
}

fn persisted_registration(fixture: &RunFixture, run_id: &str) -> TestRunRegistration {
    let (state, _) = load_state_for_boot(&fixture.state.persistence_path).unwrap();
    state.unwrap().test_run_registrations[run_id].clone()
}

fn restore_registered_run(
    fixture: &RunFixture,
    run: &RecordRun,
    session: &str,
) -> Arc<RecordingTransport> {
    let saved = persisted_registration(fixture, &run.registration.run_id);
    assert_eq!(
        saved.verification_record,
        current(fixture, run).verification_record
    );
    let transport = Arc::new(RecordingTransport::default());
    reload_session(fixture, session, &transport);
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .insert(saved.run_id.clone(), saved);
    transport
}

#[test]
fn typed_binding_refusals_wait_for_the_original_holder_and_workspace_then_replay_exactly() {
    for field in [
        EngramVerificationBindingRefusal::ClaimLive,
        EngramVerificationBindingRefusal::RootBound,
    ] {
        let mut fixture = RunFixture::new(field.field());
        synchronous_persistence(&mut fixture);
        let (session, first_transport) = engram_session(&fixture, true);
        let mut run = record_run(
            &fixture,
            "test-binding-resend",
            commit_request(&fixture.root),
            passed_results(),
        );
        initiated_by(&mut run, &session);
        register(&fixture, &[&run]);
        let row = attach_launch_row(&fixture, &run);
        first_transport.script(Ok(work_supported(&session)));
        first_transport.script(Err(binding_refusal(field.field())));
        assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
        let original = first_transport.sent.lock().unwrap()[1].clone();
        assert!(matches!(
            current(&fixture, &run).verification_record,
            Some(TestRunVerificationAck::BindingRetry { wait: Some(saved), .. }) if saved == field
        ));
        let rendered = summary(&run.run_dir);
        assert!(rendered.contains(&format!("verification: pending (field: {})", field.field())));
        assert!(!rendered.contains("private producer"));

        let transport = restore_registered_run(&fixture, &run, &session);
        let workspace = &run.registration.named_root.as_ref().unwrap().workspace_id;
        let mut expired = ready_binding(&run, &session, workspace);
        expired["claim"]["expires_at"] = json!(STARTED);
        for proof in [
            ready_binding(&run, "session-replacement", workspace),
            ready_binding(&run, &session, "elsewhere"),
            expired,
        ] {
            transport.script(Ok(work_supported(&session)));
            transport.script(Ok(proof));
            assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
            assert!(transport.sent_records().is_empty());
            assert!(matches!(
                current(&fixture, &run).verification_record,
                Some(TestRunVerificationAck::BindingRetry { wait: Some(saved), .. }) if saved == field
            ));
        }
        transport.script(Ok(work_supported(&session)));
        transport.script(Ok(ready_binding(&run, &session, workspace)));
        let state_path = fixture.state.persistence_path.clone();
        let run_id = run.registration.run_id.clone();
        *transport.observe.lock().unwrap() = Some(Box::new(move |request| {
            if let EngramControlRequest::VerificationRecord { .. } = request {
                let (state, _) = load_state_for_boot(&state_path).unwrap();
                assert!(matches!(
                    state.unwrap().test_run_registrations[&run_id].verification_record,
                    Some(TestRunVerificationAck::BindingRetry { wait: None, .. })
                ));
            }
        }));
        transport.script(Err(EngramTransportError::transport(
            "reply lost after resend",
        )));
        assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
        assert_eq!(transport.sent.lock().unwrap().last(), Some(&original));
        assert!(matches!(
            current(&fixture, &run).verification_record,
            Some(TestRunVerificationAck::BindingRetry { wait: None, .. })
        ));

        // The producer can replay the earlier receipt after another authority
        // move. This attempt must reach it without requiring another root proof.
        let replay_transport = restore_registered_run(&fixture, &run, &session);
        replay_transport.script(Ok(work_supported(&session)));
        // The producer has saved the receipt under the original request.
        // A different body or key would require a new producer write.
        let existing_request = original.clone();
        *replay_transport.observe.lock().unwrap() = Some(Box::new(move |request| {
            if let EngramControlRequest::VerificationRecord { .. } = request {
                assert_eq!(serde_json::to_vec(request).unwrap(), existing_request);
            }
        }));
        replay_transport.script(Ok(receipt_for("passed")));
        assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
        assert_eq!(
            replay_transport.sent_operations(),
            ["work_session_read", "verification_record"]
        );
        assert_eq!(
            replay_transport.sent.lock().unwrap().last(),
            Some(&original)
        );
        assert!(matches!(
            current(&fixture, &run).verification_record,
            Some(TestRunVerificationAck::Settled { .. })
        ));
        let records = settled_records();
        assert_eq!(row_records(&fixture, &row), records);
        pass_with(&fixture, &mut Default::default());
        assert_eq!(replay_transport.sent_records().len(), 1);
        assert_eq!(row_records(&fixture, &row), records);
    }
}

#[test]
fn deferred_binding_never_resends_with_changed_artifacts_or_store() {
    let fixture = RunFixture::new("verification-binding-frozen-body");
    let (session, transport) = engram_session(&fixture, true);
    let mut run = record_run(
        &fixture,
        "test-binding-frozen-body",
        commit_request(&fixture.root),
        passed_results(),
    );
    initiated_by(&mut run, &session);
    register(&fixture, &[&run]);
    transport.script(Ok(work_supported(&session)));
    transport.script(Err(binding_refusal("claim_live")));
    assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
    let results = run.run_dir.join("results.json");
    let original_results = fs::read(&results).unwrap();
    let mut altered_source = passed_results();
    altered_source["sourceIdAfter"]["tree"] = json!("9".repeat(40));
    for changed in [altered_source, commit_results()] {
        fs::write(&results, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
        assert_eq!(transport.sent_records().len(), 1);
        assert!(matches!(
            current(&fixture, &run).verification_record,
            Some(TestRunVerificationAck::BindingRetry { wait: Some(_), .. })
        ));
    }
    fs::write(&results, original_results).unwrap();
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .projects
        .iter_mut()
        .find(|project| project.id == fixture.project_id)
        .unwrap()
        .engram
        .as_mut()
        .unwrap()
        .authority_store_key
        .as_mut()
        .unwrap()
        .project_id = "github.com/example/another-store".into();
    assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
    assert_eq!(
        transport.sent_operations(),
        ["work_session_read", "verification_record"]
    );

    let body = run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE);
    fs::remove_file(&body).unwrap();
    assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
    assert!(!body.exists());
    assert_eq!(transport.sent_records().len(), 1);
    assert!(matches!(
        current(&fixture, &run).verification_record,
        Some(TestRunVerificationAck::BindingRetry { wait: Some(_), .. })
    ));
}

#[test]
fn a_proved_unsent_resend_restores_the_durable_binding_wait() {
    let mut fixture = RunFixture::new("verification-binding-resend-unsent");
    synchronous_persistence(&mut fixture);
    let clock = fixture.state.select_test_scripted_engram_budget_clock();
    let (session, transport) = engram_session(&fixture, true);
    let mut run = record_run(
        &fixture,
        "test-binding-resend-unsent",
        commit_request(&fixture.root),
        passed_results(),
    );
    initiated_by(&mut run, &session);
    register(&fixture, &[&run]);
    transport.script(Ok(work_supported(&session)));
    transport.script(Err(binding_refusal("claim_live")));
    assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
    let waiting = current(&fixture, &run).verification_record;
    assert!(matches!(
        &waiting,
        Some(TestRunVerificationAck::BindingRetry {
            wait: Some(EngramVerificationBindingRefusal::ClaimLive),
            ..
        })
    ));
    let workspace = &run.registration.named_root.as_ref().unwrap().workspace_id;

    // A replacement child invalidates the capability after the root read.
    let child = Arc::downgrade(&transport);
    *transport.observe.lock().unwrap() = Some(Box::new(move |request| {
        if let EngramControlRequest::NamedRootRead { .. } = request {
            child.upgrade().unwrap().respawn();
        }
    }));
    transport.script(Ok(work_supported(&session)));
    transport.script(Ok(ready_binding(&run, &session, workspace)));
    assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
    assert_eq!(transport.sent_records().len(), 1);
    assert_eq!(
        persisted_registration(&fixture, &run.registration.run_id).verification_record,
        waiting
    );
    *transport.observe.lock().unwrap() = None;

    // The forward marker is durable when the transport receives the call.
    // Its error proves no handoff, after the send budget has expired.
    *transport.observe.lock().unwrap() = Some(Box::new(move |request| {
        if let EngramControlRequest::VerificationRecord { .. } = request {
            clock.advance(ENGRAM_VERIFICATION_RECORD_TICK_BUDGET);
        }
    }));
    transport.script(Ok(work_supported(&session)));
    transport.script(Ok(ready_binding(&run, &session, workspace)));
    transport.script(Err(EngramTransportError::deadline(
        "control worker was unavailable before handoff",
    )
    .request_never_sent()));
    assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
    assert_eq!(transport.sent_records().len(), 2);
    assert_eq!(current(&fixture, &run).verification_record, waiting);
    let restarted = restore_registered_run(&fixture, &run, &session);
    restarted.script(Ok(work_supported(&session)));
    restarted.script(Ok(ready_binding(&run, "replacement-holder", workspace)));
    assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
    assert_eq!(
        restarted.sent_operations(),
        ["work_session_read", "named_root_read"]
    );
    assert_eq!(current(&fixture, &run).verification_record, waiting);
}

#[test]
fn an_unconfirmed_replay_marker_never_dispatches_the_retry() {
    let mut fixture = RunFixture::new("verification-binding-resend-write-failed");
    synchronous_persistence(&mut fixture);
    let (session, transport) = engram_session(&fixture, true);
    let mut run = record_run(
        &fixture,
        "test-binding-resend-write-failed",
        commit_request(&fixture.root),
        passed_results(),
    );
    initiated_by(&mut run, &session);
    register(&fixture, &[&run]);
    transport.script(Ok(work_supported(&session)));
    transport.script(Err(binding_refusal("root_bound")));
    assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
    let waiting = current(&fixture, &run).verification_record;
    assert!(matches!(
        &waiting,
        Some(TestRunVerificationAck::BindingRetry {
            wait: Some(EngramVerificationBindingRefusal::RootBound),
            ..
        })
    ));

    failing_persist_worker(&mut fixture);
    let workspace = &run.registration.named_root.as_ref().unwrap().workspace_id;
    transport.script(Ok(work_supported(&session)));
    transport.script(Ok(ready_binding(&run, &session, workspace)));
    assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
    assert_eq!(transport.sent_records().len(), 1);
    assert!(matches!(
        current(&fixture, &run).verification_record,
        Some(TestRunVerificationAck::BindingRetry { wait: None, .. })
    ));
    assert_eq!(
        persisted_registration(&fixture, &run.registration.run_id).verification_record,
        waiting
    );
    transport.script(Ok(work_supported(&session)));
    assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
    assert_eq!(transport.sent_records().len(), 1);
}

#[test]
fn a_verification_registration_fence_requires_the_committed_marker_and_digest() {
    let mut fixture = RunFixture::new("verification-registration-readback");
    synchronous_persistence(&mut fixture);
    let mut run = record_run(
        &fixture,
        "test-verification-registration-readback",
        commit_request(&fixture.root),
        passed_results(),
    );
    let old_marker = TestRunVerificationAck::BindingRetry {
        wait: None,
        body_digest: "a".repeat(64),
    };
    run.registration.verification_record = Some(old_marker.clone());
    register(&fixture, &[&run]);
    {
        let inner = fixture.state.inner.lock().unwrap();
        fixture.state.persist_synchronous_locked(&inner).unwrap();
    }

    let run_id = &run.registration.run_id;
    let replay = TestRunVerificationAck::BindingRetry {
        wait: None,
        body_digest: "b".repeat(64),
    };
    let mut expected = run.registration.clone();
    expected.verification_record = Some(replay.clone());
    let (fence, waiter) = PersistFence::new(
        PersistFenceTarget::TestRunRegistrationReadback(expected.clone()),
        std::time::Instant::now() + crate::tests::phase_sync::DEADLOCK_GUARD,
    );
    let mut batch = PersistFenceBatch::default();
    batch.accept(PersistRequest::Fence(Box::new(fence)));
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .update_unindexed_fields(run_id, |row| row.verification_record = Some(replay));
    let delta = collect_persist_delta_from_shared_state(&fixture.state.inner, 0);
    assert_eq!(
        delta.metadata.test_run_registrations.get(run_id),
        Some(&expected)
    );
    let connection =
        open_sqlite_state_read_connection(fixture.state.persistence_path.as_path()).unwrap();

    // A matching collected delta cannot stand in for the committed row.
    batch.finish_write(&delta, &Ok(Vec::new()), Some(&connection));
    assert!(
        waiter
            .completion
            .poll_at(std::time::Instant::now())
            .is_none()
    );
    assert_eq!(
        persisted_registration(&fixture, run_id).verification_record,
        Some(old_marker)
    );

    {
        let inner = fixture.state.inner.lock().unwrap();
        fixture.state.persist_synchronous_locked(&inner).unwrap();
    }
    batch.finish_write(&delta, &Ok(Vec::new()), Some(&connection));
    assert_eq!(
        waiter.completion.poll_at(std::time::Instant::now()),
        Some(Ok(()))
    );
}

#[test]
fn binding_replay_rollback_preserves_scan_updates_and_recovers_unconfirmed_writes() {
    for rollback_fails in [false, true] {
        let mut fixture = RunFixture::new(if rollback_fails {
            "verification-binding-resend-rollback-failed"
        } else {
            "verification-binding-resend-scan"
        });
        synchronous_persistence(&mut fixture);
        let (session, transport) = engram_session(&fixture, true);
        let mut run = record_run(
            &fixture,
            "test-binding-resend-scan",
            commit_request(&fixture.root),
            passed_results(),
        );
        initiated_by(&mut run, &session);
        register(&fixture, &[&run]);
        let launch = attach_launch_row(&fixture, &run);
        transport.script(Ok(work_supported(&session)));
        transport.script(Err(binding_refusal("root_bound")));
        assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
        let waiting = current(&fixture, &run).verification_record;
        let original_request = transport.sent_records()[0].clone();

        let (tx, rx) = std::sync::mpsc::channel();
        fixture.state.persist_tx = tx;
        let next_fence = || {
            while let Ok(request) = rx.recv_timeout(crate::tests::phase_sync::DEADLOCK_GUARD) {
                if let PersistRequest::Fence(fence) = request {
                    return fence;
                }
            }
            panic!("expected the writer's fence");
        };
        let diagnostic = TestRunNativeCompletionRefusal::new(
            TestRunNativeCompletionPhase::ArtifactAssociation,
            TestRunNativeCompletionRefusalCode::ResultsUnavailable,
            &[],
        );
        let workspace = &run.registration.named_root.as_ref().unwrap().workspace_id;
        transport.script(Ok(work_supported(&session)));
        transport.script(Ok(ready_binding(&run, &session, workspace)));
        if rollback_fails {
            transport.script(Err(
                EngramTransportError::deadline("before handoff").request_never_sent()
            ));
        }
        std::thread::scope(|scope| {
            let attempt = scope.spawn(|| {
                fixture
                    .state
                    .engram_verification_records_tick_within(Duration::from_secs(15), &|| false)
            });
            let forward = next_fence();
            {
                let mut inner = fixture.state.inner.lock().unwrap();
                if !rollback_fails {
                    inner
                        .test_run_registrations
                        .update(&run.registration.run_id, |row| {
                            row.native_completion_refusal = Some(diagnostic.clone());
                        });
                }
                fixture.state.persist_synchronous_locked(&inner).unwrap();
            }
            let marked = persisted_registration(&fixture, &run.registration.run_id);
            assert_eq!(
                marked.native_completion_refusal.as_ref(),
                (!rollback_fails).then_some(&diagnostic)
            );
            assert!(matches!(
                marked.verification_record,
                Some(TestRunVerificationAck::BindingRetry { wait: None, .. })
            ));
            forward.finish(Ok(()));

            let rollback = next_fence();
            if rollback_fails {
                rollback.finish(Err(PersistFenceError::WriteFailed(
                    "rollback interrupted".to_owned(),
                )));
            } else {
                let inner = fixture.state.inner.lock().unwrap();
                fixture.state.persist_synchronous_locked(&inner).unwrap();
                rollback.finish(Ok(()));
            }
            assert_eq!(attempt.join().unwrap(), 1);
        });
        drop(rx);
        let saved = persisted_registration(&fixture, &run.registration.run_id);
        assert_eq!(
            saved.native_completion_refusal,
            (!rollback_fails).then_some(diagnostic)
        );
        if rollback_fails {
            assert_eq!(
                transport.sent_records(),
                [original_request.clone(), original_request.clone()]
            );
            assert!(matches!(
                saved.verification_record,
                Some(TestRunVerificationAck::BindingRetry { wait: None, .. })
            ));
        } else {
            assert_eq!(transport.sent_records(), [original_request.clone()]);
            assert_eq!(saved.verification_record, waiting);
        }

        let restarted = restore_registered_run(&fixture, &run, &session);
        restarted.script(Ok(work_supported(&session)));
        if rollback_fails {
            restarted.script(Ok(receipt_for("passed")));
            assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
            assert_eq!(
                restarted.sent_operations(),
                ["work_session_read", "verification_record"]
            );
            assert_eq!(restarted.sent_records(), [original_request]);
            assert_eq!(row_records(&fixture, &launch), settled_records());
        } else {
            restarted.script(Ok(ready_binding(&run, "replacement-holder", workspace)));
            assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
            assert_eq!(
                restarted.sent_operations(),
                ["work_session_read", "named_root_read"]
            );
        }
    }
}

#[test]
fn a_fresh_binding_refusal_on_exact_replay_rearms_the_persisted_wait() {
    let mut fixture = RunFixture::new("verification-binding-replay-refused");
    synchronous_persistence(&mut fixture);
    let (session, transport) = engram_session(&fixture, true);
    let mut run = record_run(
        &fixture,
        "test-binding-replay-refused",
        commit_request(&fixture.root),
        passed_results(),
    );
    initiated_by(&mut run, &session);
    register(&fixture, &[&run]);
    let workspace = &run.registration.named_root.as_ref().unwrap().workspace_id;
    transport.script(Ok(work_supported(&session)));
    transport.script(Err(binding_refusal("root_bound")));
    assert_eq!(pass_with(&fixture, &mut Default::default()), 1);

    transport.script(Ok(work_supported(&session)));
    transport.script(Ok(ready_binding(&run, &session, workspace)));
    transport.script(Err(EngramTransportError::transport("lost reply")));
    assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
    transport.script(Ok(work_supported(&session)));
    transport.script(Err(binding_refusal("claim_live")));
    assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
    let sent = transport.sent_records();
    assert_eq!(sent.len(), 3);
    assert!(sent.iter().all(|request| request == &sent[0]));
    assert!(matches!(
        persisted_registration(&fixture, &run.registration.run_id).verification_record,
        Some(TestRunVerificationAck::BindingRetry {
            wait: Some(EngramVerificationBindingRefusal::ClaimLive),
            ..
        })
    ));

    let restarted = restore_registered_run(&fixture, &run, &session);
    restarted.script(Ok(work_supported(&session)));
    restarted.script(Ok(ready_binding(&run, &session, "another-workspace")));
    assert_eq!(pass_with(&fixture, &mut Default::default()), 1);
    assert_eq!(
        restarted.sent_operations(),
        ["work_session_read", "named_root_read"]
    );
}

#[test]
fn a_held_route_settles_the_passed_record_once_under_the_same_key_when_it_is_available() {
    let fixture = RunFixture::new("verification-held-route");
    let run = record_run(
        &fixture,
        "test-held-route",
        commit_request(&fixture.root),
        passed_results(),
    );
    let transport = Arc::new(RecordingTransport::default());
    let adapter = adapter(&transport);
    let own = connection(SESSION, "actor");
    register(&fixture, &[&run]);
    // While the initiating session is gone, or its connection or store is not
    // the registration's, nothing is sent: the run stays pending, with no
    // receipt held, and no test is run again.
    for reason in [
        EngramVerificationRecordPending::InitiatingSessionGone,
        EngramVerificationRecordPending::NoConnection,
        EngramVerificationRecordPending::StoreChanged,
    ] {
        let step = engram_verification_record_send(ready(&run.registration), Err(reason.clone()));
        assert!(
            matches!(&step, EngramVerificationRecordStep::Pending(pending) if *pending == reason),
            "{step:?}"
        );
        assert_eq!(
            engram_verification_record_progress(&run.run_dir),
            Some(EngramVerificationRecordProgress::Pending { reason })
        );
        fixture
            .state
            .note_engram_verification_record_step(&run.registration, &step);
        assert_eq!(current(&fixture, &run).verification_record, None);
    }
    assert!(transport.sent_records().is_empty());

    // Once the route is there, the same frozen record goes once, under the
    // run's own key, and its receipt is held.
    read_work_session(&transport, &adapter, &own, supported());
    transport.script(Ok(receipt_for("passed")));
    let step = engram_verification_record_send(ready(&run.registration), link(&adapter, &own));
    assert!(
        matches!(step, EngramVerificationRecordStep::Received { .. }),
        "{step:?}"
    );
    fixture
        .state
        .note_engram_verification_record_step(&run.registration, &step);
    // A later look settles from that receipt and never sends again.
    assert!(matches!(
        engram_verification_record_prepare(&current(&fixture, &run)),
        EngramVerificationRecordPrepared::ReceiptHeld { .. }
    ));
    let sent = transport.sent_records();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["operation_key"], "termal-run:test-held-route");
}

#[test]
fn a_claim_refusal_stays_rejected_and_its_summary_names_the_cause_and_the_correction() {
    let refused = |field: &str| {
        let wire = json!({"status":"error", "error":{
            "code":"verification_record_refused",
            "message":"private-refusal-message",
            "details":{"field":field, "reason":"claim, fence or run moved since registration"}
        }});
        let EngramControlResponse::Error { error } = serde_json::from_value(wire).unwrap() else {
            panic!("error envelope")
        };
        error.into_transport_error()
    };
    // Only a plain field label is kept as the refused field.
    for (field, kept) in [
        ("claim", Some("claim")),
        ("root", Some("root")),
        ("input.kind", Some("input.kind")),
        ("Claim", None),
        ("claim; root", None),
        ("", None),
    ] {
        assert_eq!(refused(field).refused_field.as_deref(), kept, "{field:?}");
    }
    // A reply that is not a verification refusal keeps no refused field.
    let other: EngramControlResponse = serde_json::from_value(json!({"status":"error", "error":{
        "code":"invalid_request", "message":"m", "details":{"field":"claim", "reason":"r"}
    }}))
    .unwrap();
    let EngramControlResponse::Error { error } = other else {
        panic!("error envelope")
    };
    assert_eq!(error.into_transport_error().refused_field, None);
    // Only the claim field names a cause.
    for field in ["Claim", "claim; root", "root"] {
        let error = refused(field);
        let detail = engram_verification_record_refusal_detail(
            error.code.as_deref(),
            error.refused_field.as_deref(),
        );
        assert_eq!(
            detail, "Engram refused it: verification_record_refused",
            "{field}"
        );
    }

    let fixture = RunFixture::new("verification-claim-refusal");
    let run = record_run(
        &fixture,
        "test-claim-refusal",
        commit_request(&fixture.root),
        passed_results(),
    );
    let transport = Arc::new(RecordingTransport::default());
    let adapter = adapter(&transport);
    let own = connection(SESSION, "actor");
    read_work_session(&transport, &adapter, &own, supported());
    transport.script(Err(refused("claim")));
    let step = engram_verification_record_send(ready(&run.registration), link(&adapter, &own));
    let EngramVerificationRecordStep::ProducerRejected { detail, .. } = &step else {
        panic!("{step:?}")
    };
    assert!(
        detail.starts_with("Engram refused it: verification_record_refused (field: claim)"),
        "{detail}"
    );
    let cause = "the claim was recovered under a new fence before the record was sent";
    let correction = "keep the claim live until the record lands";
    // Engram refuses the same field for a lapsed claim or a pending handoff.
    let handoff = "a handoff of it was pending";
    let no_handoff = "offer no handoff of it until then";
    assert!(
        detail.contains(cause) && detail.contains(correction),
        "{detail}"
    );
    assert!(
        detail.contains(handoff) && detail.contains(no_handoff),
        "{detail}"
    );
    assert!(!detail.contains("private-refusal-message"), "{detail}");
    assert_eq!(
        engram_verification_record_progress(&run.run_dir),
        Some(EngramVerificationRecordProgress::Rejected {
            detail: detail.clone()
        })
    );
    let rendered = summary(&run.run_dir);
    assert!(
        rendered.contains(cause) && rendered.contains(correction) && rendered.contains(no_handoff),
        "{rendered}"
    );
    // Neither Engram's message nor its reason reaches the stored detail or
    // the summary: only the fixed cause and correction do.
    for private in [
        "private-refusal-message",
        "claim, fence or run moved since registration",
    ] {
        assert!(!detail.contains(private), "{detail}");
        assert!(!rendered.contains(private), "{rendered}");
    }
    // The refusal stays final: the record is never sent again.
    register(&fixture, &[&run]);
    fixture
        .state
        .note_engram_verification_record_step(&run.registration, &step);
    assert!(matches!(
        engram_verification_record_prepare(&current(&fixture, &run)),
        EngramVerificationRecordPrepared::Finished
    ));
    assert_eq!(
        transport.sent_operations(),
        ["work_session_read", "verification_record"]
    );
}
