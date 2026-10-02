//! Prospective recovery of an actual restored local-only host selection.
//! Engram's disposable canonical store is never edited to invent history.
use super::*;

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn legacy_local_only_root_recovers_through_normal_forward_naming() {
    let fixture = completion_fixture();
    let session = &fixture.live.session_id;
    let added = fixture.work(
        session,
        &[
            "add",
            "Check a prospective legacy root",
            "--accept",
            "A test passes in the newly confirmed generation",
            "--bind",
            "1=test",
            "--json",
        ],
    );
    let work = added["work"]["short_ref"]
        .as_str()
        .or_else(|| added["short_ref"].as_str())
        .unwrap();
    fixture.work(session, &["claim", work, "--json"]);
    fixture
        .live
        .state
        .ensure_engram_session_bound_off_lock(session)
        .unwrap()
        .unwrap();
    let binding = fixture.record(session, |record| {
        record.engram.work_binding.clone().unwrap()
    });
    let before = fixture.canonical_read(session, &binding);
    assert_eq!(before.named_root, EngramNamedRootState::None);
    assert!(
        before.latest_event.is_none(),
        "the producer has no old naming event"
    );
    let (root, common_dir_key) = validate_engram_source_root(
        fixture.root.to_str().unwrap(),
        fixture.root.to_str().unwrap(),
        fixture.root.to_str().unwrap(),
    )
    .unwrap();
    let runtime = fixture.record(session, |record| record.runtime.clone());
    {
        let mut inner = fixture.live.state.inner.lock().unwrap();
        let store = inner
            .find_project(&fixture.project_id)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .authority_store_key
            .clone()
            .unwrap();
        inner.engram_work_source_roots.push(EngramWorkSourceRoot {
            store,
            work_id: binding.work_id.clone(),
            short_ref: work.to_owned(),
            claim_id: binding.claim_id.clone(),
            claim_fence: binding.claim_fence,
            root,
            common_dir_key,
            named_by_session: session.clone(),
            named_at: "2026-09-28T00:00:00Z".to_owned(),
            generation: 7,
        });
        inner.engram_source_root_generation = 7;
        // Write the old host shape to TermAl's disposable SQLite image only.
        inner.engram_work_naming_history.clear();
        assert!(inner.engram_named_root_journal.is_empty());
    }
    let delta = collect_persist_delta_from_shared_state(&fixture.live.state.inner, 0);
    persist_delta_via_cache(
        &mut SqlitePersistConnectionCache::new(),
        fixture.live.state.persistence_path.as_path(),
        &delta,
    )
    .unwrap();
    let restored = load_state(fixture.live.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(restored.engram_work_naming_history[0].epoch, 0);
    assert_eq!(restored.engram_work_naming_history[0].known_generation, 7);
    assert!(restored.engram_work_naming_history[0].frontier.is_none());
    *fixture.live.state.inner.lock().unwrap() = restored;
    // Restoration creates a default transport. Reconnect the fixture's real
    // process adapter so every boundary read observes the same connection.
    fixture
        .live
        .state
        .install_test_engram_transport(fixture.live.transport.clone());
    {
        let mut inner = fixture.live.state.inner.lock().unwrap();
        let index = inner.find_session_index(session).unwrap();
        inner.sessions[index].runtime = runtime;
    }
    let dispatch = dispatch_live_root(
        &fixture.live.state,
        session,
        "Inspect the legacy opening.",
        None,
    );
    deliver_turn_dispatch(&fixture.live.state, dispatch).unwrap();
    let CodexRuntimeCommand::Prompt { command, .. } = fixture.live.receiver.try_recv().unwrap()
    else {
        panic!("provider prompt");
    };
    assert!(
        command.prompt.contains("canonical naming history"),
        "{}",
        command.prompt
    );
    assert!(fixture.record(session, |record| {
        record.engram.active_turn_start_basis.is_none()
    }));
    assert_eq!(
        fixture.record(session, |record| record
            .engram
            .active_turn_root_capture
            .clone()),
        Some(EngramRootCapture::Unconfirmed)
    );
    fixture
        .live
        .state
        .recover_engram_authority_runs(session, Duration::from_secs(2));
    assert!(fixture.record(session, |record| {
        !record.engram.source_root_notices.is_empty()
    }));
    let named = fixture
        .live
        .state
        .name_engram_source_root(
            session,
            EngramSourceRootRequest {
                work: work.to_owned(),
                path: Some(Some(fixture.root.to_string_lossy().into_owned())),
            },
        )
        .expect("ordinary forward naming recovers without historical repair");
    assert!(named.generation > 7);
    assert_eq!(
        fixture.live.state.engram_turn_root_capture(session),
        EngramRootCapture::Unconfirmed
    );
    let bound = fixture.canonical_read(session, &binding);
    assert!(
        matches!(bound.named_root, EngramNamedRootState::Bound { generation, .. } if generation == named.generation as i64)
    );
    assert_eq!(bound.latest_event.unwrap().kind, EngramNamedRootKind::Bound);
    finish_live_root(&fixture.live.state, session);
    let old_checkpoints = fixture.live.transport.requests_for("turn_checkpoint");
    let old = old_checkpoints.last().unwrap();
    assert!(
        old["observations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|observation| observation.get("source_basis").is_none_or(Value::is_null)),
        "{old:#}"
    );
    let dispatch = dispatch_live_root(
        &fixture.live.state,
        session,
        "Check the new generation without an edit.",
        None,
    );
    deliver_turn_dispatch(&fixture.live.state, dispatch).unwrap();
    let CodexRuntimeCommand::Prompt { command, .. } = fixture.live.receiver.try_recv().unwrap()
    else {
        panic!("fresh provider prompt");
    };
    assert!(
        !command
            .prompt
            .contains("Source-root authority recovery remains incomplete"),
        "{}",
        command.prompt
    );
    assert!(
        !command
            .prompt
            .contains("This turn's source-root binding is unconfirmed"),
        "{}",
        command.prompt
    );
    assert!(
        matches!(fixture.record(session, |record| record.engram.active_turn_root_capture.clone()), Some(EngramRootCapture::Recorded { generation, state: EngramSourceRootState::Named, .. }) if generation == named.generation as i64)
    );
    assert_eq!(
        fixture.record(session, |record| record
            .engram
            .active_turn_start_basis
            .as_ref()
            .unwrap()
            .source_root_generation),
        Some(named.generation as i64)
    );
    let revision = named.source_revision.unwrap();
    let mut recorder = SessionRecorder::new(fixture.live.state.clone(), session.clone());
    let cwd = fs::canonicalize(&fixture.root)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    recorder
        .command_started_in(
            "legacy-test",
            "cargo test --offline",
            Some("cargo test --offline"),
            Some(&cwd),
        )
        .unwrap();
    fixture.wait_for_check_capture(session);
    let output = Command::new("cargo")
        .args(["test", "--offline"])
        .current_dir(&fixture.root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.contains("1 passed"), "{output}");
    recorder
        .command_completed_with_exit(
            "legacy-test",
            "cargo test --offline",
            &output,
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    fixture.wait_for_check_capture(session);
    finish_live_root(&fixture.live.state, session);
    let checkpoints = fixture.live.transport.requests_for("turn_checkpoint");
    let checked = checkpoints.last().unwrap();
    assert_eq!(
        checked["verification_evidence"].as_array().map(Vec::len),
        Some(1),
        "{checked:#}"
    );
    let basis = checked["observations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|observation| observation["source_basis"]["source_revision"] == revision)
        .unwrap()["source_basis"]
        .clone();
    assert_eq!(basis["source_root_generation"], named.generation);
    assert_eq!(
        content_revision(&fixture.root).unwrap().1,
        revision,
        "no invented edit"
    );
    let shown = fixture.work(session, &["show", work, "--notes", "--json"]);
    let mut rows = Vec::new();
    objects_with_key(&shown, "verification", &mut rows);
    assert_eq!(rows.len(), 1, "{shown:#}");
    let verification = rows[0]["locator"].as_str().unwrap();
    assert_eq!(fixture.object(verification)["source_basis"], basis);
    let shown = fixture.work(session, &["show", work, "--json"]);
    let acceptance = shown["acceptance_basis"].as_i64().unwrap().to_string();
    let evidence = shown["evidence_basis"].as_i64().unwrap().to_string();
    fixture.work(
        session,
        &[
            "evaluate",
            work,
            "--mode",
            "same-session",
            "--acceptance-basis",
            &acceptance,
            "--evidence-basis",
            &evidence,
            "--verdict",
            "1=pass:observed",
            "--rationale",
            "1=Actual check in a prospectively confirmed legacy root passed",
            "--evidence",
            &format!("1={verification}"),
            "--source-fingerprint",
            &revision,
            "--attempt",
            "legacy-forward-completion",
            "--json",
        ],
    );
    fixture.work(
        session,
        &[
            "done",
            work,
            "Prospective root check passed",
            "--source-fingerprint",
            &revision,
            "--json",
        ],
    );
    let shown = fixture.work(session, &["show", work, "--json"]);
    assert_eq!(
        shown["status"]["work"]["lifecycle"], "completed",
        "{shown:#}"
    );
}
