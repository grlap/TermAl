// Owns the end-to-end witness that a turn-gated-control OFF session whose Base
// token a host reload flagged for rebinding gets its pending verification
// record delivered once its own source-root naming binds it again: the record
// stays pending and nothing is sent until then, the same record, key and
// original registration association go out under the re-established token,
// and later ticks send no duplicate. Does not own the naming bind's fences,
// rollback or replacement rules (engram_source_root_off_rebind.rs) or the
// sender's other rules (engram_verification_record.rs, whose fixture this
// child module uses). New module.
use super::*;

const SHORT_REF: &str = "w-record";

/// A naming bind's answer as Engram gives it, listing what the connection
/// supports beside its status (the shape `old_and_new_bind_and_status_answers_both_parse`
/// pins).
fn naming_bind_reply(token: &str) -> ScriptedEngramControlResponse {
    ScriptedEngramControlResponse::Reply(Ok(json!({
        "routing_token": token,
        "status": { "phase": "ready" },
        "capabilities": ["verification_record_v1"],
    })))
}

/// A session status answer as Engram gives it, listing what the connection
/// supports. The named-roots model adds the claim's `named_root`.
fn naming_status_reply() -> ScriptedEngramControlResponse {
    ScriptedEngramControlResponse::Reply(Ok(json!({
        "phase": "ready",
        "capabilities": ["verification_record_v1"],
    })))
}

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
fn a_reloaded_off_session_naming_its_root_again_gets_the_same_pending_record_delivered_once() {
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

    let (session, _recording) = engram_session(&fixture, false, None);
    // In the host's order: each naming binds, then reads the claim's
    // named_root through the session's status; the sender then sends the
    // record. The second naming asks under a key of its own, so Engram
    // answers it with a fresh binding.
    let transport = ScriptedEngramControlTransport::new_with_work_bindings(
        [
            naming_bind_reply("naming-token"),
            naming_status_reply(),
            naming_bind_reply("rebound-token"),
            naming_status_reply(),
            ScriptedEngramControlResponse::Reply(Ok(receipt_for("failed"))),
        ],
        [Ok(Some(binding()))],
    );
    transport.enable_named_roots(&session, &binding());
    {
        let mut roots = transport.named_roots.lock().unwrap();
        let roots = roots.get_or_insert_with(TestEngramNamedRoots::default);
        roots.project_id = Some(store().project_id);
        // Status answers come from the script, in the producer's shape; the
        // model's own status answer lists no capabilities.
        roots.scripted_status = true;
    }
    transport.replace_held_claims([Ok(held_claim())]);
    fixture
        .state
        .install_control_test_transport(transport.clone());

    // Ordinary naming binds the fresh OFF session; its run is registered with
    // the association that naming gave it.
    let first = name_the_root(&fixture, &session);
    assert_eq!(of_kind(&operations(&transport), "session_bind").len(), 1);
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

    // A reload flags the restored token: the sender holds the record and
    // sends nothing.
    reload(&fixture, &session, &transport);
    assert!(session_engram(&fixture, &session, |engram| engram.rebind_required));
    let before_tick = operations(&transport).len();
    fixture.state.engram_verification_records_tick();
    assert_eq!(operations(&transport).len(), before_tick, "nothing is sent");
    assert_eq!(
        engram_verification_record_progress(&run.run_dir),
        Some(EngramVerificationRecordProgress::Pending {
            reason: EngramVerificationRecordPending::AwaitingNamingBinding,
        })
    );
    let body_path = run.run_dir.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE);
    let frozen_text = fs::read_to_string(&body_path).expect("the body is frozen");

    // The session names the same root again under the same live claim: one
    // naming bind with the same intent under a key of its own, and the root
    // keeps its generation.
    let again = name_the_root(&fixture, &session);
    assert_eq!(again.generation, first.generation);
    let binds = of_kind(&operations(&transport), "session_bind");
    assert_eq!(binds.len(), 2, "exactly one more session_bind");
    let without_key = |bind: &Value| {
        let mut bind = bind.clone();
        bind.as_object_mut().unwrap().remove("idempotency_key");
        bind
    };
    assert_eq!(without_key(&binds[1]), without_key(&binds[0]), "the same naming intent");
    assert_ne!(
        binds[1]["idempotency_key"], binds[0]["idempotency_key"],
        "under a key of its own"
    );
    assert!(!session_engram(&fixture, &session, |engram| engram.rebind_required));

    // The next tick sends the same frozen record under the re-established
    // token, with the registration's original association.
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
        "rebound-token",
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
