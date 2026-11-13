// A selected original mailbox request is part of immutable launch identity.
// Registration owns custody; editing launcher artifacts cannot create it.
use super::*;

fn causal_fixture(label: &str, mode: MailboxTestMode) -> (RunFixture, String, String) {
    let mut fixture = RunFixture::new(label);
    fixture.state.mailbox_store = Arc::new(
        MailboxStore::open(&resolve_coordination_persistence_path(
            fixture.state.persistence_path.as_path(),
        ))
        .unwrap(),
    );
    let sender = owner(&fixture);
    let runner = owner(&fixture);
    native_origin::bind_native_host(&fixture);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        for session in [&sender, &runner] {
            let index = inner.find_session_index(session).unwrap();
            inner.sessions[index].session.status = SessionStatus::Active;
        }
    }
    let request = crate::tests::mailboxes::test_request::typed_request(mode);
    let capture = fixture
        .state
        .prepare_mailbox_test_request(&sender, "causal-send", Some(&request))
        .unwrap()
        .unwrap();
    let receipt = fixture
        .state
        .append_native_mailbox_message_with_test_custody(
            &sender,
            SendMailboxMessageRequest {
                target_session_id: runner.clone(),
                message: "test this committed input".into(),
                idempotency_key: "causal-send".into(),
                topic: None,
                state_stamp: None,
                class: None,
            },
            NativeMailboxNamespace::Local,
            Some(capture),
        )
        .unwrap();
    (fixture, runner, receipt.message_id)
}

fn selected_run(
    fixture: &RunFixture,
    session: &str,
    run: &str,
    message: &str,
    mode: MailboxTestMode,
) -> PathBuf {
    let directory = committed_run(fixture, session, run);
    let path = directory.join("request.json");
    let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    request["full"] = json!(mode == MailboxTestMode::Full);
    request["requestMessageId"] = json!(message);
    fs::write(path, request.to_string()).unwrap();
    directory
}

#[test]
fn typed_capture_uses_each_actual_held_claim_and_named_root_without_a_focus_fallback() {
    let mut fixture = RunFixture::new("causal-held-capture");
    fixture.state.mailbox_store = Arc::new(
        MailboxStore::open(&resolve_coordination_persistence_path(
            fixture.state.persistence_path.as_path(),
        ))
        .unwrap(),
    );
    let sender = owner(&fixture);
    let runner = owner(&fixture);
    native_origin::bind_native_host(&fixture);
    let binding = EngramControlWorkBinding {
        root_execution_id: Uuid::new_v4().to_string(),
        work_id: Uuid::new_v4().to_string(),
        run_id: Uuid::new_v4().to_string(),
        work_revision: 5,
        claim_id: Uuid::new_v4().to_string(),
        claim_fence: 2,
    };
    let store = EngramAuthorityStoreKey {
        database_path: fixture.root.join("private-current-store.db"),
        project_id: "canonical-project".into(),
    };
    let root = named_root::name_root(
        &fixture,
        &store,
        &binding,
        &sender,
        &fs::canonicalize(&fixture.root).unwrap(),
        7,
    );
    let transport = ScriptedEngramControlTransport::new([]);
    transport.replace_held_claims([Ok(EngramHeldClaims {
        items: vec![EngramHeldClaim {
            work_id: binding.work_id.clone(),
            short_ref: format!("w-{}", binding.work_id.rsplit('-').next().unwrap()),
            claim_id: binding.claim_id.clone(),
            claim_fence: binding.claim_fence,
            focused: false,
            control_binding: Some(binding.clone()),
        }],
        omitted: 1,
    })]);
    fixture
        .state
        .install_test_engram_transport(transport.clone());
    let mut request = crate::tests::mailboxes::test_request::typed_request(MailboxTestMode::Full);
    request.targets[0].work_ref = binding.work_id.clone();
    let mut unheld = request.targets[0].clone();
    unheld.work_ref = Uuid::new_v4().to_string();
    request.targets.push(unheld);
    let capture = fixture
        .state
        .prepare_mailbox_test_request(&sender, "held-capture", Some(&request))
        .unwrap()
        .unwrap();
    assert_eq!(transport.held_claim_reads(), 1);
    let first = capture.payload.targets[0].association.as_ref().unwrap();
    assert_eq!(first.binding, binding);
    assert_eq!(first.root.workspace_id, root.root);
    assert_eq!(first.root.generation, 7);
    assert!(capture.payload.targets[0].uncredited_reason.is_none());
    assert!(capture.payload.targets[1].association.is_none());
    assert!(
        capture.payload.targets[1]
            .uncredited_reason
            .as_ref()
            .unwrap()
            .starts_with("association_stale")
    );
    assert_eq!(capture.sender_scope.as_ref().unwrap().store, store);
    assert!(!format!("{:?}", capture.sender_scope).contains("private-current-store.db"));
    let target = fixture
        .state
        .native_mailbox_relay_descriptor(&runner)
        .unwrap()
        .principal;
    let bound = capture.for_recipient(&target).unwrap();
    let input = MailboxAppendInput {
        sender_session_id: sender.clone(),
        sender_name: "Sender".into(),
        target_session_id: runner,
        target_name: "Runner".into(),
        body: "captured once".into(),
        idempotency_key: "held-capture".into(),
        topic: None,
        state_stamp: None,
    };
    fixture
        .state
        .mailbox_store
        .append_with_test_request_custody(
            &input,
            Some(&bound.payload),
            bound.sender_scope.as_ref(),
            || Ok(()),
            |_, _| Ok(()),
        )
        .unwrap();
    transport.replace_held_claims([Err(EngramTransportError::transport(
        "service later unavailable",
    ))]);
    let replay = fixture
        .state
        .prepare_mailbox_test_request(&sender, "held-capture", Some(&request))
        .unwrap()
        .unwrap();
    assert_eq!(replay.payload, bound.payload);
    assert_eq!(replay.sender_scope, bound.sender_scope);
    assert_eq!(
        transport.held_claim_reads(),
        1,
        "retry does not capture from a later focus or service state"
    );
}

#[tokio::test]
async fn native_full_and_focused_custody_replay_reload_and_echo_the_same_original() {
    for mode in [MailboxTestMode::Full, MailboxTestMode::Focused] {
        let (fixture, runner, message) = causal_fixture("causal-echo", mode);
        let id = "test-causal-echo";
        let directory = selected_run(&fixture, &runner, id, &message, mode);
        let app = app_router(fixture.state.clone());
        let post = |selector: &str| {
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{runner}/test-runs"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"runId":id, "runDirectory":test_run_display_path(&directory),
                "requestMessageId":selector})
                    .to_string(),
                ))
                .unwrap()
        };
        let (bad, _): (StatusCode, Value) = request_json(
            &app,
            post("mailbox-message-99999999-9999-4999-8999-999999999999"),
        )
        .await;
        assert!(bad.is_client_error());
        for _ in 0..2 {
            let (status, receipt): (StatusCode, Value) = request_json(&app, post(&message)).await;
            assert!(status.is_success(), "{status}: {receipt}");
            assert_eq!(receipt["registered"], true);
        }
        let original = fixture.state.inner.lock().unwrap().test_run_registrations[id].clone();
        let causal = test_run_causal_request(&original).unwrap();
        assert_eq!(causal.request_message_id, message);
        assert_eq!(causal.canonical_message_id, message);
        assert_eq!(causal.runner.native_session_id, runner);
        assert_eq!(causal.payload.mode, mode);
        assert!(
            original.verification_record.is_none(),
            "registration does not create verification credit"
        );
        let loaded = load_state(fixture.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        assert_eq!(
            test_run_causal_request(&loaded.test_run_registrations[id]),
            Some(causal)
        );
        for indexed in [false, true] {
            if indexed {
                fixture.refresh(&[std::process::id()]);
            }
            for suffix in ["", "?view=registration"] {
                let (status, response): (StatusCode, Value) = request_json(
                    &app,
                    Request::builder()
                        .uri(format!("/api/test-runs/{id}{suffix}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await;
                assert_eq!(status, StatusCode::OK, "{response}");
                assert_eq!(response["run"]["requestMessageId"], message);
            }
        }
        let second = selected_run(&fixture, &runner, "test-causal-replacement", &message, mode);
        assert!(
            fixture
                .state
                .register_test_run(
                    &runner,
                    registration_request("test-causal-replacement", &second)
                )
                .is_err()
        );
        assert_eq!(
            fixture
                .state
                .inner
                .lock()
                .unwrap()
                .test_run_registrations
                .len(),
            1
        );
    }
}

#[test]
fn selected_request_rejects_wrong_mode_plain_input_and_replaced_recipient() {
    let (fixture, runner, message) = causal_fixture("causal-refusals", MailboxTestMode::Focused);
    let sender = fixture
        .state
        .mailbox_store
        .read_message(&runner, &message)
        .unwrap()
        .sender_session_id;
    let wrong_owner = selected_run(
        &fixture,
        &sender,
        "test-causal-sender-is-not-runner",
        &message,
        MailboxTestMode::Focused,
    );
    assert!(
        fixture
            .state
            .register_test_run(
                &sender,
                registration_request("test-causal-sender-is-not-runner", &wrong_owner)
            )
            .is_err(),
        "knowing the original key and message never makes its sender the native runner"
    );
    let id = "test-causal-refused";
    let directory = selected_run(&fixture, &runner, id, &message, MailboxTestMode::Full);
    assert!(
        fixture
            .state
            .register_test_run(&runner, registration_request(id, &directory))
            .is_err()
    );
    let path = directory.join("request.json");
    let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    request["full"] = json!(false);
    request["input"] = json!("plain");
    fs::write(&path, request.to_string()).unwrap();
    assert!(
        fixture
            .state
            .register_test_run(&runner, registration_request(id, &directory))
            .is_err()
    );
    request["input"] = json!("commit");
    fs::write(path, request.to_string()).unwrap();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&runner).unwrap();
        inner.sessions[index].federation_native_creation_id = Uuid::new_v4().to_string();
    }
    assert!(
        fixture
            .state
            .register_test_run(&runner, registration_request(id, &directory))
            .is_err()
    );
    assert!(
        !fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_run_registrations
            .contains_key(id)
    );
}

#[test]
fn concurrent_registrations_cannot_consume_the_same_causal_request_twice() {
    let (fixture, runner, message) = causal_fixture("causal-concurrent", MailboxTestMode::Full);
    let inputs = ["test-causal-concurrent-a", "test-causal-concurrent-b"]
        .into_iter()
        .map(|id| {
            (
                id,
                selected_run(&fixture, &runner, id, &message, MailboxTestMode::Full),
            )
        })
        .collect::<Vec<_>>();
    let responses = phase_sync::run_concurrently(inputs.into_iter().map(|(id, directory)| {
        let state = fixture.state.clone();
        let runner = runner.clone();
        move || state.register_test_run(&runner, registration_request(id, &directory))
    }));
    assert_eq!(responses.iter().filter(|result| result.is_ok()).count(), 1);
    let refusal = responses
        .into_iter()
        .find_map(|result| result.err())
        .unwrap();
    assert_eq!(refusal.status, StatusCode::CONFLICT);
    assert!(refusal.message.contains("already consumed"), "{refusal:?}");
    let inner = fixture.state.inner.lock().unwrap();
    assert_eq!(inner.test_run_registrations.len(), 1);
    assert!(
        inner
            .test_run_registrations
            .values()
            .all(|row| test_run_causal_request(row).is_some())
    );
}

#[test]
fn compacted_registration_keeps_single_use_custody_after_primary_reload() {
    let (fixture, runner, message) = causal_fixture("causal-compacted", MailboxTestMode::Full);
    let id = "test-causal-compacted";
    let directory = selected_run(&fixture, &runner, id, &message, MailboxTestMode::Full);
    fixture
        .state
        .register_test_run(&runner, registration_request(id, &directory))
        .unwrap();
    fixture.refresh(&[std::process::id()]);
    let summary = fixture.summary(id);
    {
        // Install the real registration in the existing survivor collection.
        // This tests its persistence/admission boundary, not compaction policy.
        let mut inner = fixture.state.inner.lock().unwrap();
        let registration = inner.test_run_registrations.remove(id).unwrap();
        inner.test_run_compacted.insert(
            id.into(),
            TestRunCompacted {
                compacted: true,
                request_identity: Some(registration.request_identity.clone()),
                summary,
                registration: Some(registration),
                observations: Vec::new(),
                request_stamp: None,
                results_stamp: None,
                reason: "fixture retained original registration".into(),
                artifact_conflict: false,
                artifact_unavailable: false,
            },
        );
        fixture.state.persist_internal_locked(&inner).unwrap();
    }
    let loaded = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(
        test_run_causal_request(loaded.test_run_compacted[id].registration.as_ref().unwrap())
            .is_some()
    );
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner.test_run_registrations = loaded.test_run_registrations;
        inner.test_run_compacted = loaded.test_run_compacted;
    }
    let replacement = selected_run(
        &fixture,
        &runner,
        "test-after-compaction",
        &message,
        MailboxTestMode::Full,
    );
    assert!(
        fixture
            .state
            .register_test_run(
                &runner,
                registration_request("test-after-compaction", &replacement)
            )
            .is_err()
    );
}

#[test]
fn remote_observer_projection_excludes_causal_custody_and_refuses_injected_identity() {
    let (fixture, runner, message) = causal_fixture("causal-observer", MailboxTestMode::Full);
    let id = "test-causal-observer";
    let directory = selected_run(&fixture, &runner, id, &message, MailboxTestMode::Full);
    fixture
        .state
        .register_test_run(&runner, registration_request(id, &directory))
        .unwrap();
    let registration = fixture.state.inner.lock().unwrap().test_run_registrations[id].clone();
    let origin = registration.native_origin.unwrap();
    assert!(origin.causal_request.is_some());
    let identity = RemoteTestRunIdentity {
        origin: origin.clone(),
        run_id: id.into(),
        request_identity: registration.request_identity,
        locator_identity: registration.locator_identity.unwrap(),
        started_at: registration.started_at,
    };
    let mut without_custody = identity.clone();
    without_custody.origin.causal_request = None;
    assert_eq!(
        identity, without_custody,
        "private registration custody is outside observer equality"
    );
    let mut another_scope = without_custody;
    another_scope.origin.project_scope = Some("another-project".into());
    assert_ne!(
        identity, another_scope,
        "ordinary origin identity fields remain part of equality"
    );
    let mut wire = serde_json::to_value(&identity).unwrap();
    assert!(wire["origin"].get("causalRequest").is_none());
    assert!(
        serde_json::from_value::<RemoteTestRunIdentity>(wire.clone())
            .unwrap()
            .origin
            .causal_request
            .is_none()
    );
    wire["origin"]["causalRequest"] = serde_json::to_value(origin.causal_request.unwrap()).unwrap();
    assert!(serde_json::from_value::<RemoteTestRunIdentity>(wire).is_err());
}

#[test]
fn pending_or_changed_selector_never_acknowledges_a_different_causal_request() {
    for case in ["deadline", "selector_changed"] {
        let (mut fixture, runner, message) =
            causal_fixture("causal-pending", MailboxTestMode::Full);
        let id = "test-causal-pending";
        let directory = selected_run(&fixture, &runner, id, &message, MailboxTestMode::Full);
        let original_request = fs::read(directory.join("request.json")).unwrap();
        let (tx, rx) = mpsc::channel();
        fixture.state.persist_tx = tx;
        let (ready_tx, ready_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        let worker = {
            let state = fixture.state.clone();
            let runner = runner.clone();
            let directory = directory.clone();
            std::thread::spawn(move || {
                TEST_TEST_RUN_REGISTRATION_FENCE_SENT.with(|slot| {
                    *slot.borrow_mut() = Some(Box::new(move || ready_tx.send(()).unwrap()));
                });
                let result = state.register_test_run(&runner, registration_request(id, &directory));
                reply_tx.send(result).unwrap();
            })
        };
        let mut batch = PersistFenceBatch::default();
        let written = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ready_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
            assert!(matches!(
                rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap(),
                PersistRequest::Delta
            ));
            let PersistRequest::Fence(fence) = rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap()
            else {
                panic!("registration must queue its original content fence");
            };
            let expected = match &fence.target {
                PersistFenceTarget::TestRunRegistration(expected) => expected.clone(),
                _ => panic!("registration fence target"),
            };
            assert_eq!(
                test_run_causal_request(&expected)
                    .unwrap()
                    .request_message_id,
                message
            );
            assert!(
                matches!(reply_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "enqueue is not a durable ACK"
            );
            if case == "deadline" {
                assert!(matches!(
                    fence.completion.poll_at(fence.completion.deadline),
                    Some(Err(PersistFenceError::Deadline))
                ));
            } else {
                select_message(
                    &directory,
                    json!("mailbox-message-aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"),
                );
            }
            batch.accept(PersistRequest::Fence(fence));
            let delta = collect_persist_delta_from_shared_state(&fixture.state.inner, 0);
            let mut cache = SqlitePersistConnectionCache::new();
            persist_delta_with_fences(
                &mut cache,
                fixture.state.persistence_path.as_path(),
                &delta,
                &mut batch,
            )
            .unwrap();
        }));
        batch.drain(&rx, true);
        batch.fail(PersistFenceError::Shutdown);
        worker.join().unwrap();
        if let Err(panic) = written {
            std::panic::resume_unwind(panic);
        }
        let error = match reply_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap() {
            Err(error) => error,
            Ok(_) => panic!("unsettled or changed request must not receive an ACK"),
        };
        if case == "deadline" {
            assert!(error.durability_deadline.is_some(), "{error:?}");
        } else {
            assert_eq!(error.status, StatusCode::CONFLICT);
            assert!(
                error
                    .message
                    .contains("request changed before acknowledgement"),
                "{error:?}"
            );
        }
        assert!(
            !fixture
                .state
                .inner
                .lock()
                .unwrap()
                .test_runs
                .registered
                .contains(id)
        );
        let loaded = load_state(fixture.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let original_causal = test_run_causal_request(&loaded.test_run_registrations[id])
            .unwrap()
            .clone();
        assert_eq!(
            original_causal.request_message_id, message,
            "late write retains its original custody"
        );
        fs::write(directory.join("request.json"), original_request).unwrap();
        // The failed response creates no replacement run. A later explicit
        // retry uses the existing synchronous owner and the same original row.
        let (closed, receiver) = mpsc::channel();
        drop(receiver);
        fixture.state.persist_tx = closed;
        fixture
            .state
            .register_test_run(&runner, registration_request(id, &directory))
            .unwrap();
        let inner = fixture.state.inner.lock().unwrap();
        assert_eq!(inner.test_run_registrations.len(), 1);
        assert_eq!(
            test_run_causal_request(&inner.test_run_registrations[id]),
            Some(&original_causal)
        );
    }
}

fn committed_run(fixture: &RunFixture, session: &str, id: &str) -> PathBuf {
    let directory = registered_run(fixture, session, id);
    let path = directory.join("request.json");
    let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    request["input"] = json!("commit");
    fs::write(path, request.to_string()).unwrap();
    directory
}

fn select_message(directory: &FsPath, message: Value) {
    let path = directory.join("request.json");
    let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    request["requestMessageId"] = message;
    fs::write(path, request.to_string()).unwrap();
}

#[test]
fn selecting_a_different_original_message_changes_launch_identity() {
    let request = json!({
        "runId": "test-selected-identity", "root": "/repository", "full": false,
        "owner": "runner", "started": "2026-11-12T01:00:00Z",
        "stages": [{"name": "focused"}],
    });
    let original = test_run_registration_identity(&request).unwrap();
    let mut selected = request.clone();
    selected["requestMessageId"] = json!("mailbox-message-11111111-1111-4111-8111-111111111111");
    let first = test_run_registration_identity(&selected).unwrap();
    assert_ne!(
        first, original,
        "an ordinary launch and a selected request must not share an identity"
    );
    selected["requestMessageId"] = json!("mailbox-message-22222222-2222-4222-8222-222222222222");
    assert_ne!(
        test_run_registration_identity(&selected).unwrap(),
        first,
        "changing the causal message must invalidate the original acknowledgement"
    );
    selected.as_object_mut().unwrap().remove("requestMessageId");
    assert_eq!(test_run_registration_identity(&selected).unwrap(), original);
}

#[test]
fn host_and_launcher_selector_grammar_accept_the_same_wire_forms() {
    let canonical = "mailbox-message-11111111-1111-4111-8111-111111111111";
    for selector in [
        canonical.to_owned(),
        "mailbox-message-00000000-0000-0000-0000-000000000000".into(),
        "relay-message-QWJjMTIz_-".into(),
        format!("relay-message-{}", "a".repeat(1024)),
    ] {
        assert!(mailbox_test_message_selector_valid(&selector));
        let body = json!({"runId":"test-selector-grammar", "runDirectory":"/repository/run", "requestMessageId":selector});
        assert!(serde_json::from_value::<RegisterTestRunApiRequest>(body).is_ok());
    }
    for selector in [
        Value::Null,
        json!(""),
        json!(format!(" {canonical}")),
        json!(format!("{canonical}\n")),
        json!("mailbox-message-AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA"),
        json!("mailbox-message-short"),
        json!("relay-message-"),
        json!(format!("relay-message-{}", "a".repeat(1025))),
    ] {
        let body = json!({"runId":"test-selector-grammar", "runDirectory":"/repository/run", "requestMessageId":selector});
        assert!(serde_json::from_value::<RegisterTestRunApiRequest>(body).is_err());
    }
}

#[test]
fn an_unretained_selected_message_never_publishes_registration() {
    for full in [true, false] {
        let fixture = RunFixture::new("causal-missing-original");
        let session = owner(&fixture);
        let ordinary_id = "test-ordinary-control";
        let ordinary = committed_run(&fixture, &session, ordinary_id);
        fixture
            .state
            .register_test_run(&session, registration_request(ordinary_id, &ordinary))
            .unwrap();
        let selected_id = "test-selected-missing";
        let selected = committed_run(&fixture, &session, selected_id);
        let path = selected.join("request.json");
        let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        request["full"] = json!(full);
        request["requestMessageId"] = json!("mailbox-message-33333333-3333-4333-8333-333333333333");
        fs::write(path, request.to_string()).unwrap();
        let outcome = fixture
            .state
            .register_test_run(&session, registration_request(selected_id, &selected));
        assert!(
            outcome.is_err(),
            "a caller-writable selector without an original mailbox record acquired durable custody"
        );
        let inner = fixture.state.inner.lock().unwrap();
        assert!(inner.test_run_registrations.contains_key(ordinary_id));
        assert!(!inner.test_run_registrations.contains_key(selected_id));
        drop(inner);
        let loaded = load_state(fixture.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        assert!(!loaded.test_run_registrations.contains_key(selected_id));
    }
}

#[test]
fn a_registered_ordinary_run_cannot_adopt_a_later_request_selector() {
    let fixture = RunFixture::new("causal-no-retro-adoption");
    let session = owner(&fixture);
    let id = "test-ordinary-no-retro-adoption";
    let directory = committed_run(&fixture, &session, id);
    fixture
        .state
        .register_test_run(&session, registration_request(id, &directory))
        .unwrap();
    let original = fixture.state.inner.lock().unwrap().test_run_registrations[id].clone();
    select_message(
        &directory,
        json!("mailbox-message-44444444-4444-4444-8444-444444444444"),
    );
    let replay = fixture
        .state
        .register_test_run(&session, registration_request(id, &directory));
    assert!(
        replay.is_err(),
        "an earlier ordinary registration was retroactively adopted by a selected request"
    );
    assert_eq!(
        fixture.state.inner.lock().unwrap().test_run_registrations[id],
        original
    );
}
