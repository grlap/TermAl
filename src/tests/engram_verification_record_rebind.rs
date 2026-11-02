// Reload and unavailable ordinary-child recovery preserve the frozen native
// record, operation key and registration association. Naming can recover its
// own work association without replacing a historical control token; neither
// naming nor verification may create a control bind. Later ticks do not resend.
use super::*;

const SHORT_REF: &str = "w-record";

fn held_claim() -> EngramHeldClaims {
    let binding = binding();
    EngramHeldClaims {
        items: vec![EngramHeldClaim {
            work_id: binding.work_id.clone(),
            short_ref: SHORT_REF.to_owned(),
            claim_id: binding.claim_id.clone(),
            claim_fence: binding.claim_fence,
            focused: true,
            control_binding: Some(binding),
        }],
        omitted: 0,
    }
}

fn name_the_root(fixture: &RunFixture, session: &str) -> EngramSourceRootResponse {
    fixture
        .state
        .name_engram_source_root(
            session,
            EngramSourceRootRequest {
                work: SHORT_REF.to_owned(),
                path: Some(Some(fixture.root.to_string_lossy().into_owned())),
            },
        )
        .expect("the session names its root under its live claim")
}

/// A host reload: the session's record goes through the persisted form and
/// back, and the adapter's process memory starts afresh.
fn reload(fixture: &RunFixture, session: &str, transport: &Arc<ScriptedEngramControlTransport>) {
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(session).expect("the session");
        let saved =
            serde_json::to_value(PersistedSessionRecord::from_record(&inner.sessions[index]))
                .expect("the record serializes");
        let loaded: PersistedSessionRecord =
            serde_json::from_value(saved).expect("the record loads");
        inner.sessions[index] = loaded.into_record().expect("the record reloads");
    }
    fixture
        .state
        .install_control_test_transport(transport.clone());
}

fn operations(transport: &ScriptedEngramControlTransport) -> Vec<Value> {
    transport
        .requests()
        .into_iter()
        .map(|request| request.request)
        .collect()
}

fn of_kind(requests: &[Value], operation: &str) -> Vec<Value> {
    requests
        .iter()
        .filter(|request| request["operation"] == operation)
        .cloned()
        .collect()
}

#[test]
fn ordinary_naming_after_reload_preserves_historical_token_and_the_same_frozen_record() {
    let fixture = RunFixture::new("verification-record-naming-rebind");
    for (key, value) in [
        ("user.email", "termal-tests@example.com"),
        ("user.name", "TermAl tests"),
    ] {
        run_git_test_command(&fixture.root, &["config", key, value]);
    }
    fs::write(fixture.root.join("README.md"), "named\n").expect("a file to commit");
    run_git_test_command(&fixture.root, &["add", "README.md"]);
    run_git_test_command(&fixture.root, &["commit", "--quiet", "-m", "named"]);

    let (session, _recording) = engram_session(&fixture, false);
    // The exact ordinary response is explicit, including the original store
    // and session identity. Reload loses the process-local capability cache.
    let transport = ScriptedEngramControlTransport::new([
        ScriptedEngramControlResponse::Reply(Ok(receipt_for("failed"))),
    ]);
    transport.set_current_work_binding(Some(binding()));
    transport.enable_named_roots(&session, &binding());
    {
        let mut roots = transport.named_roots.lock().unwrap();
        let roots = roots.get_or_insert_with(TestEngramNamedRoots::default);
        roots.project_id = Some(store().project_id);
        roots.work_session_read_reply = Some(Ok(work_supported(&session)));
    }
    transport.replace_held_claims([Ok(held_claim())]);
    fixture
        .state
        .install_control_test_transport(transport.clone());

    // Ordinary naming establishes the work association, without a control bind.
    let first = name_the_root(&fixture, &session);
    assert!(of_kind(&operations(&transport), "session_bind").is_empty());
    let mut run = record_run(
        &fixture,
        "test-record-naming-rebind",
        commit_request(&fixture.root),
        commit_results(),
    );
    initiated_by(&mut run, &session);
    run.registration.named_root = Some(TestRunNamedRoot {
        workspace_id: test_run_display_path(&fixture.root),
        generation: first.generation,
    });
    register(&fixture, &[&run]);
    attach_launch_row(&fixture, &run);
    let original = current(&fixture, &run);

    // A historical token remains passive data across reload.
    with_session_engram(&fixture, &session, |engram| {
        engram.historical_control.insert("engramRoutingToken".to_owned(), json!("historical-token"));
    });
    reload(&fixture, &session, &transport);
    transport.named_roots.lock().unwrap().as_mut().unwrap().work_session_read_reply =
        Some(Err(EngramTransportError::transport("ordinary child unavailable after reload")));
    let before_tick = operations(&transport).len();
    fixture.state.engram_verification_records_tick();
    assert_eq!(operations(&transport).len(), before_tick + 1, "only ordinary read attempted");
    assert!(matches!(
        engram_verification_record_progress(&run.run_dir),
        Some(EngramVerificationRecordProgress::Pending {
            reason: EngramVerificationRecordPending::TransportFailed { .. },
        })
    ));
    let body_path = run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE);
    let frozen_text = fs::read_to_string(&body_path).expect("the body is frozen");

    // The same held claim/root keeps its generation and does not alter the
    // original passive history.
    transport.named_roots.lock().unwrap().as_mut().unwrap().work_session_read_reply =
        Some(Ok(work_supported(&session)));
    let again = name_the_root(&fixture, &session);
    assert_eq!(again.generation, first.generation);
    let binds = of_kind(&operations(&transport), "session_bind");
    assert!(binds.is_empty(), "ordinary naming never replaces control identity");
    assert_eq!(session_engram(&fixture, &session, |engram|
        engram.historical_control.get("engramRoutingToken").cloned()), Some(json!("historical-token")));

    // The next tick uses the ordinary child and the original registration.
    let before_tick = operations(&transport).len();
    fixture.state.engram_verification_records_tick();
    let sent = operations(&transport)[before_tick..].to_vec();
    let records = of_kind(&sent, "verification_record");
    assert_eq!(records.len(), 1, "one record goes out: {sent:?}");
    assert!(
        of_kind(&sent, "session_bind").is_empty(),
        "the sender binds nothing"
    );
    let expected = serde_json::to_value(engram_verification_record_request(
        &ready(&original),
    ))
    .expect("the request serializes");
    assert_eq!(records[0], expected, "the same frozen body and key");
    assert_eq!(
        records[0]["operation_key"],
        "termal-run:test-record-naming-rebind"
    );
    assert_eq!(
        records[0]["registration"]["binding"],
        serde_json::to_value(binding()).unwrap()
    );
    assert_eq!(
        fs::read_to_string(&body_path).unwrap(),
        frozen_text,
        "never rebuilt"
    );
    assert_eq!(current(&fixture, &run).named_root, original.named_root);
    assert!(matches!(
        engram_verification_record_prepare(&current(&fixture, &run)),
        EngramVerificationRecordPrepared::Finished
    ));

    // Later ticks create no duplicate.
    let settled = operations(&transport).len();
    fixture.state.engram_verification_records_tick();
    fixture.state.engram_verification_records_tick();
    assert_eq!(operations(&transport).len(), settled, "no duplicate record");
}
