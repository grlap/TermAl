// Real native producer -> MAC-authenticated relay -> ordinary wait consumer.
// Launcher/provider telemetry is fixture data, not a consumer test or credit.
use super::*;
use crate::tests::test_runs::test_run_registration::{
    compacted_native_completion_producer_fixture, finish_native_completion_producer,
    native_completion_producer_fixture,
};

const SCOPE: &str = "example.test/remote/wait-consumer";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_wait_reads_original_compacted_terminal_after_reload_for_a_fresh_destination() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for success in [true, false] {
        let fixture = Fixture::production_shape().await;
        let (id, retained) = compacted_native_completion_producer_fixture(
            fixture.satellite.clone(),
            "remote-compacted-origin",
            SCOPE,
            success,
        );
        assert!(
            fixture
                .central
                .inner
                .lock()
                .unwrap()
                .remote_test_run_facts
                .is_empty()
        );
        let session = destination(&fixture.central, "remote-compacted-destination", SCOPE);
        let wait = register(&fixture, &session, &id);
        let original = wire(&fixture, &session, &id, SCOPE).await.unwrap();
        assert_eq!(original.completion.as_ref(), Some(&retained.completion));
        assert_eq!(
            retained.completion.result.verdict,
            if success {
                native_test_completion::Verdict::Pass
            } else {
                native_test_completion::Verdict::Fail
            }
        );
        let survivor = fixture.satellite.inner.lock().unwrap().test_run_compacted[&id].clone();
        for mismatch in ["execution", "request", "duplicate"] {
            {
                let mut changed = survivor.clone();
                let row = changed
                    .observations
                    .iter_mut()
                    .find(|row| row.execution_id == retained.execution_id)
                    .unwrap();
                if mismatch == "execution" {
                    row.execution_id.push_str("-different");
                } else if mismatch == "request" {
                    row.artifact.as_mut().unwrap().request_identity = Some("f".repeat(64));
                } else {
                    let duplicate = row.clone();
                    changed.observations.push(duplicate);
                }
                let mut inner = fixture.satellite.inner.lock().unwrap();
                inner.test_run_compacted.insert(id.clone(), changed);
                fixture.satellite.persist_internal_locked(&inner).unwrap();
            }
            assert!(
                wire(&fixture, &session, &id, SCOPE)
                    .await
                    .unwrap()
                    .completion
                    .is_none(),
                "{mismatch}: a different or ambiguous retained execution cannot supply a terminal"
            );
        }
        {
            let mut inner = fixture.satellite.inner.lock().unwrap();
            inner.test_run_compacted.insert(id.clone(), survivor);
            fixture.satellite.persist_internal_locked(&inner).unwrap();
        }
        let restored = wire(&fixture, &session, &id, SCOPE).await.unwrap();
        assert_eq!(restored.completion, original.completion);
        apply(&fixture, &wait, restored).unwrap();
        let queued = prompts(&fixture.central, &session);
        assert_eq!(queued.len(), 1);
        assert!(queued[0].contains(if success {
            "Verdict: PASS"
        } else {
            "Verdict: FAIL"
        }));
        assert!(queued[0].contains("producer_unsupported"));
        assert_observation_only(&fixture.central, &id);
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_wait_request_and_locator_are_pinned_before_terminal_and_never_rebound() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for field in ["request", "locator"] {
        let fixture = Fixture::production_shape().await;
        let (_, id, directory) = native_completion_producer_fixture(
            fixture.satellite.clone(),
            "remote-identity-origin",
            SCOPE,
        );
        let session = destination(&fixture.central, "remote-identity-destination", SCOPE);
        let wait = register(&fixture, &session, &id);
        let pending = wire(&fixture, &session, &id, SCOPE).await.unwrap();
        let original_identity = pending.identity.clone().unwrap();
        apply(&fixture, &wait, pending).unwrap();
        finish_native_completion_producer(&fixture.satellite, &id, &directory, true);
        let mut terminal = wire(&fixture, &session, &id, SCOPE).await.unwrap();
        if field == "request" {
            terminal.identity.as_mut().unwrap().request_identity = "f".repeat(64);
        } else {
            terminal
                .identity
                .as_mut()
                .unwrap()
                .locator_identity
                .push_str("-replacement");
        }
        apply(&fixture, &wait, terminal).unwrap();
        let inner = fixture.central.inner.lock().unwrap();
        let facts = inner.remote_test_run_facts.values().next().unwrap();
        assert_eq!(facts.identity, original_identity);
        assert!(facts.identity_conflict);
        assert!(
            facts.completion.is_none(),
            "a replacement request or locator cannot lend a PASS"
        );
        drop(inner);
        let queued = prompts(&fixture.central, &session);
        assert_eq!(queued.len(), 1);
        assert!(queued[0].contains("UNKNOWN (conflicting authoritative remote terminals)"));
        assert!(queued[0].contains("result_conflict"));
        fixture.finish().await;
    }
}

#[test]
fn remote_wait_references_preserve_native_host_boundaries_and_reject_noncanonical_selectors() {
    let one = FederationHostIdentity {
        host_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_owned(),
        host_generation: 1,
    };
    let two = FederationHostIdentity {
        host_id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".to_owned(),
        host_generation: 1,
    };
    let id = "test-identical";
    assert_ne!(
        remote_test_run_facts_key(&one, id),
        remote_test_run_facts_key(&two, id)
    );
    assert_eq!(remote_test_run_reference(id).unwrap(), None);
    assert_eq!(
        remote_test_run_reference(&format!("host:{}::{id}", one.host_id)).unwrap(),
        Some((one.host_id.clone(), id.to_owned()))
    );
    for reference in [
        "host:display-name::test-identical".to_owned(),
        format!("host:{}::../test-identical", one.host_id),
        format!("host:{}::test-identical", one.host_id.to_uppercase()),
        format!("host:{}::test-identical::other", one.host_id),
    ] {
        assert!(
            remote_test_run_reference(&reference).is_err(),
            "{reference}"
        );
    }
}

fn destination(app: &AppState, label: &str, scope: &str) -> String {
    let root = app.test_temp_root.as_ref().unwrap().path().join(label);
    fs::create_dir_all(&root).unwrap();
    run_git_test_command(&root, &["init", "--quiet"]);
    let project = create_test_project(app, &root, label);
    let session = create_test_project_session(app, Agent::Codex, &project, &root);
    let mut inner = app.inner.lock().unwrap();
    inner
        .projects
        .iter_mut()
        .find(|row| row.id == project)
        .unwrap()
        .engram = Some(EngramProjectSettings {
        authority_store_key: Some(EngramAuthorityStoreKey {
            database_path: root.join("private-store.db"),
            project_id: scope.to_owned(),
        }),
        ..EngramProjectSettings::default()
    });
    let index = inner.find_session_index(&session).unwrap();
    inner.sessions[index].session.status = SessionStatus::Active; // Keep the real resume queued.
    app.persist_internal_locked(&inner).unwrap();
    session
}

fn register(fixture: &Fixture, session: &str, id: &str) -> TestRunWaitRecord {
    let host = &fixture.channel.snapshot.peer.host_id;
    let reference = format!("host:{host}::{id}");
    let response = fixture
        .central
        .create_test_run_wait_with(
            session,
            serde_json::from_value(json!({"runIds":[reference],"mode":"all"})).unwrap(),
            &|| panic!("remote reference must not rescan local paths"),
        )
        .unwrap();
    assert!(!response.resume_prompt_queued);
    response.wait
}

fn prompts(app: &AppState, session: &str) -> Vec<String> {
    let inner = app.inner.lock().unwrap();
    inner.sessions[inner.find_session_index(session).unwrap()]
        .queued_prompts
        .iter()
        .map(|row| row.pending_prompt.text.clone())
        .collect()
}

fn assert_observation_only(app: &AppState, id: &str) {
    let inner = app.inner.lock().unwrap();
    assert!(inner.test_runs.find(id).is_none());
    assert!(!inner.test_run_registrations.contains_key(id));
    assert!(inner.test_run_observations.is_empty());
    assert!(
        inner
            .remote_test_run_facts
            .values()
            .all(|facts| facts.identity.run_id == id)
    );
}

async fn wire(
    fixture: &Fixture,
    session: &str,
    id: &str,
    scope: &str,
) -> Result<RemoteTestRunReply, ApiError> {
    let handle = FederationPeerHandle {
        channel: fixture.channel.clone(),
        rpc: fixture.rpc.clone(),
    };
    let actor = fixture
        .central
        .native_mailbox_relay_descriptor(session)
        .unwrap()
        .principal;
    fixture
        .central
        .request_mailbox_relay(
            &handle,
            &fixture.channel.current_lease().unwrap(),
            MailboxRelayOperation::ObserveTestRun {
                actor,
                project_scope: scope.to_owned(),
                run_id: id.to_owned(),
            },
        )
        .await
}

fn apply(
    fixture: &Fixture,
    wait: &TestRunWaitRecord,
    reply: RemoteTestRunReply,
) -> Result<(), ApiError> {
    fixture.central.ingest_remote_test_run_reply(
        wait,
        &wait.run_ids[0],
        reply,
        &fixture.channel.current_lease().unwrap(),
    )
}

fn restore_wait_collections(app: &AppState) {
    let loaded = load_state(app.persistence_path.as_path()).unwrap().unwrap();
    let mut inner = app.inner.lock().unwrap();
    inner.test_run_waits = loaded.test_run_waits;
    inner.remote_test_run_facts = loaded.remote_test_run_facts;
    inner.test_runs = TestRunIndex::default();
}

struct PeerHandleGuard(AppState);
impl Drop for PeerHandleGuard {
    fn drop(&mut self) {
        self.0
            .federation_peer_handles
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }
}

async fn recovery_once(fixture: &Fixture, recovery: &mut RemoteTestRunWaitRecovery) {
    let host = fixture.channel.snapshot.peer.host_id.clone();
    fixture
        .central
        .federation_peer_handles
        .lock()
        .unwrap()
        .insert(
            host,
            Arc::new(FederationPeerHandle {
                channel: fixture.channel.clone(),
                rpc: fixture.rpc.clone(),
            }),
        );
    recovery.tick(&fixture.central, true).await;
    if let Some(task) = recovery.task.take() {
        tokio::time::timeout(phase_sync::DEADLOCK_GUARD, task)
            .await
            .expect("remote recovery must finish")
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_wait_native_wire_pending_reconnect_restart_then_terminal_resumes_once_without_credit()
 {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for success in [true, false] {
        let fixture = Fixture::production_shape().await;
        let _handles = PeerHandleGuard(fixture.central.clone());
        let (_, id, directory) = native_completion_producer_fixture(
            fixture.satellite.clone(),
            "remote-origin-clone",
            SCOPE,
        );
        let session = destination(&fixture.central, "remote-destination", SCOPE);
        let wait = register(&fixture, &session, &id);
        assert!(!fixture.central.inner.lock().unwrap().test_runs.scanned_once);
        let pending = wire(&fixture, &session, &id, SCOPE).await.unwrap();
        assert!(pending.identity.is_some());
        assert!(
            pending.completion.is_none(),
            "a native launch receipt supplies no terminal"
        );
        apply(&fixture, &wait, pending).unwrap();
        assert!(prompts(&fixture.central, &session).is_empty());
        assert_eq!(
            fixture.central.inner.lock().unwrap().test_run_waits.len(),
            1
        );
        restore_wait_collections(&fixture.central);
        assert!(
            !fixture.central.inner.lock().unwrap().test_run_waits[0].runs[0]
                .remote
                .as_ref()
                .unwrap()
                .confirmed
        );
        fixture.reconnect().await;
        finish_native_completion_producer(&fixture.satellite, &id, &directory, success);
        let original = wire(&fixture, &session, &id, SCOPE).await.unwrap();
        let completion = original
            .completion
            .clone()
            .expect("native retained terminal");
        assert_eq!(
            completion.result.verdict,
            if success {
                native_test_completion::Verdict::Pass
            } else {
                native_test_completion::Verdict::Fail
            }
        );
        assert_eq!(completion.result.exit, Some(if success { 0 } else { 101 }));
        let mut recovery = RemoteTestRunWaitRecovery::default();
        recovery_once(&fixture, &mut recovery).await;
        assert!(
            fixture
                .central
                .inner
                .lock()
                .unwrap()
                .test_run_waits
                .is_empty()
        );
        let queued = prompts(&fixture.central, &session);
        assert_eq!(queued.len(), 1);
        assert!(queued[0].contains(if success {
            "Verdict: PASS"
        } else {
            "Verdict: FAIL"
        }));
        assert!(queued[0].contains("producer_unsupported"));
        assert!(!queued[0].contains("scripts/test-launcher.mjs summary"));
        assert!(!queued[0].contains("private-store.db"));
        apply(&fixture, &wait, original.clone()).unwrap();
        recovery_once(&fixture, &mut recovery).await;
        assert_eq!(prompts(&fixture.central, &session), queued);
        let loaded = load_state(fixture.central.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let facts = loaded.remote_test_run_facts.values().next().unwrap();
        assert_eq!(facts.completion.as_ref(), Some(&completion));
        assert!(facts.conflict.is_none());
        assert_observation_only(&fixture.central, &id);
        recovery.shutdown().await;
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_wait_unknown_and_late_delivery_preserve_native_failure_and_project_refusal() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (_, id, directory) =
        native_completion_producer_fixture(fixture.satellite.clone(), "remote-late-origin", SCOPE);
    let session = destination(&fixture.central, "remote-late-destination", SCOPE);
    let wait = register(&fixture, &session, &id);
    let foreign = destination(
        &fixture.central,
        "remote-foreign-project",
        "example.test/another/project",
    );
    let error = wire(&fixture, &foreign, &id, "example.test/another/project")
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        fixture
            .central
            .inner
            .lock()
            .unwrap()
            .remote_test_run_facts
            .is_empty()
    );
    let mut unknown = wire(&fixture, &session, &id, SCOPE).await.unwrap();
    let retained_unknown = native_test_completion::Completion {
        schema_version: 1,
        origin: native_test_completion::Origin {
            host_id: fixture.channel.snapshot.peer.host_id.clone(),
            runner_session_id: unknown
                .identity
                .as_ref()
                .unwrap()
                .origin
                .native_session_id
                .clone(),
        },
        run_id: id.clone(),
        mode: native_test_completion::Mode::Full,
        result: native_test_completion::NativeResult {
            verdict: native_test_completion::Verdict::Unknown,
            terminal: true,
            exit: None,
            reason: "delivery unknown".to_owned(),
        },
        input: native_test_completion::Input::default(),
        finished_at: None,
    };
    unknown.completion = Some(retained_unknown);
    apply(&fixture, &wait, unknown.clone()).unwrap();
    assert!(
        fixture
            .central
            .inner
            .lock()
            .unwrap()
            .remote_test_run_facts
            .values()
            .all(|facts| facts.completion.is_none())
    );
    assert!(prompts(&fixture.central, &session).is_empty());
    finish_native_completion_producer(&fixture.satellite, &id, &directory, false);
    let failed = wire(&fixture, &session, &id, SCOPE).await.unwrap();
    apply(&fixture, &wait, failed.clone()).unwrap();
    apply(&fixture, &wait, unknown).unwrap(); // A late Unknown cannot replace FAIL.
    let facts = fixture
        .central
        .inner
        .lock()
        .unwrap()
        .remote_test_run_facts
        .values()
        .next()
        .unwrap()
        .clone();
    assert_eq!(facts.completion, failed.completion);
    assert!(facts.conflict.is_none());
    assert_eq!(prompts(&fixture.central, &session).len(), 1);
    assert_observation_only(&fixture.central, &id);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_wait_conflicting_authoritative_terminals_remain_visible_after_consumption_and_reload()
 {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (_, id, directory) = native_completion_producer_fixture(
        fixture.satellite.clone(),
        "remote-conflict-origin",
        SCOPE,
    );
    let session = destination(&fixture.central, "remote-conflict-destination", SCOPE);
    let wait = register(&fixture, &session, &id);
    finish_native_completion_producer(&fixture.satellite, &id, &directory, false);
    let original = wire(&fixture, &session, &id, SCOPE).await.unwrap();
    apply(&fixture, &wait, original.clone()).unwrap();
    let mut contradiction = original.clone();
    let terminal = contradiction.completion.as_mut().unwrap();
    terminal.result.verdict = native_test_completion::Verdict::Pass;
    terminal.result.exit = Some(0);
    terminal.result.reason = "passed".to_owned();
    // Inject a contradictory authenticated observation at the same consumer
    // boundary, after the original wait has already been consumed.
    apply(&fixture, &wait, contradiction.clone()).unwrap();
    let loaded = load_state(fixture.central.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let facts = loaded.remote_test_run_facts.values().next().unwrap();
    assert_eq!(facts.completion, original.completion);
    assert_eq!(facts.conflict, contradiction.completion);
    let next = register(&fixture, &session, &id);
    apply(&fixture, &next, original).unwrap();
    let queued = prompts(&fixture.central, &session);
    assert_eq!(queued.len(), 2);
    assert!(queued[1].contains("UNKNOWN (conflicting authoritative remote terminals)"));
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_wait_identity_authority_and_stop_fences_refuse_aliases_and_stale_delivery() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (_, id, directory) = native_completion_producer_fixture(
        fixture.satellite.clone(),
        "remote-fences-origin",
        SCOPE,
    );
    let session = destination(&fixture.central, "remote-fences-destination", SCOPE);
    let wait = register(&fixture, &session, &id);
    let local = fixture
        .central
        .create_test_run_wait_with(
            &session,
            serde_json::from_value(json!({"runIds":[id],"mode":"all"})).unwrap(),
            &|| {},
        )
        .err()
        .unwrap();
    assert_eq!(
        local.status,
        StatusCode::NOT_FOUND,
        "a bare remote id never aliases a local run"
    );
    finish_native_completion_producer(&fixture.satellite, &id, &directory, true);
    let original = wire(&fixture, &session, &id, SCOPE).await.unwrap();
    for field in ["host", "run", "runner", "scope"] {
        let mut wrong = original.clone();
        match field {
            "host" => {
                wrong.identity.as_mut().unwrap().origin.host.host_id = Uuid::new_v4().to_string()
            }
            "run" => wrong.completion.as_mut().unwrap().run_id = "test-another".to_owned(),
            "runner" => {
                wrong.completion.as_mut().unwrap().origin.runner_session_id =
                    "session-another".to_owned()
            }
            _ => {
                wrong.identity.as_mut().unwrap().origin.project_scope =
                    Some("example.test/foreign".to_owned())
            }
        }
        assert!(apply(&fixture, &wait, wrong).is_err());
    }
    let old = fixture.channel.current_lease().unwrap();
    fixture.reconnect().await;
    assert!(
        fixture
            .central
            .ingest_remote_test_run_reply(&wait, &wait.run_ids[0], original.clone(), &old)
            .is_err()
    );
    {
        let mut inner = fixture.central.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let creation = inner.sessions[index].federation_native_creation_id.clone();
        inner.sessions[index].federation_native_creation_id = Uuid::new_v4().to_string();
        drop(inner);
        assert!(apply(&fixture, &wait, original.clone()).is_err());
        let mut inner = fixture.central.inner.lock().unwrap();
        inner.sessions[index].federation_native_creation_id = creation;
        consume_test_run_waits_for_stopped_session_locked(&mut inner, &session);
        fixture.central.persist_internal_locked(&inner).unwrap();
    }
    apply(&fixture, &wait, original).unwrap();
    assert!(
        prompts(&fixture.central, &session).is_empty(),
        "Stop keeps a late origin result from waking the session"
    );
    fixture.finish().await;
}
