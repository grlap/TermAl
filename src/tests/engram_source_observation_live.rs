//! Real producer accounting, exact report replay and native-record freshness.
use super::*;

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn source_observation_live_gitlink_reports_outcome_and_declared_check_only() {
    // Real host construction/delivery and real producer accounting; command
    // telemetry and its failing TRX are fixtures, not a native test-credit run.
    let fixture = completion_fixture();
    let session = &fixture.live.session_id;
    let added = fixture.work(
        session,
        &[
            "add",
            "Observe checks without submodule source attribution",
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
        .name_engram_source_root(
            session,
            EngramSourceRootRequest {
                work: work.to_owned(),
                path: Some(Some(fixture.root.to_string_lossy().into_owned())),
            },
        )
        .unwrap();
    fs::write(fixture.root.join("termal-tests.toml"),
        "[[test]]\ncommand = \"dotnet test\"\ncwd = \".\"\nartifact = \".termal-results/check.trx\"\n").unwrap();
    let ignored = fs::read_to_string(fixture.root.join(".gitignore")).unwrap_or_default();
    fs::write(
        fixture.root.join(".gitignore"),
        format!("{ignored}\n.termal-results/\n"),
    )
    .unwrap();
    let head = run_git_test_command_output(&fixture.root, &["rev-parse", "HEAD"]);
    fixture.source_id_now(session, &fixture.root);
    run_git_test_command(
        &fixture.root,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000",
            head.trim(),
            "child",
        ],
    );
    let source_deadline = std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET;
    let source_owner = fixture
        .live
        .state
        .source_owner_metadata_until(session, source_deadline)
        .expect("the live fixture pins its configured source owner");
    assert!(
        engram_source_id_until(&source_owner, &fixture.root, source_deadline).is_err(),
        "the owner cannot measure the uninitialized child"
    );
    fixture.begin(
        session,
        "Observe the declared result while submodule measurement is unavailable.",
    );
    let mut recorder = SessionRecorder::new(fixture.live.state.clone(), session.to_owned());
    recorder
        .command_started_in(
            "gitlink-declared-check",
            "dotnet test",
            Some("dotnet test"),
            Some(&fixture.root.to_string_lossy()),
        )
        .unwrap();
    fs::create_dir_all(fixture.root.join(".termal-results")).unwrap();
    fs::write(
        fixture.root.join(".termal-results/check.trx"),
        include_bytes!("fixtures/declared-trx/fail.trx"),
    )
    .unwrap();
    recorder
        .command_completed_with_exit(
            "gitlink-declared-check",
            "dotnet test",
            "process exited zero",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    finish_live_root(&fixture.live.state, session);
    fixture.wait_for_reports(session);
    let requests = fixture.live.transport.requests_for("execution_observe");
    assert_eq!(
        requests.len(),
        1,
        "the unmeasured close emits no source sighting"
    );
    let occurrence = &requests[0]["occurrence"];
    assert_eq!(occurrence["kind"], "unadmitted_turn");
    assert!(occurrence["source_change"].is_null());
    assert_eq!(occurrence["observed_checks"].as_array().unwrap().len(), 1);
    assert_eq!(
        occurrence["observed_checks"][0]["observed_result"],
        "failed"
    );
    assert!(occurrence["observed_checks"][0]["source_basis"].is_null());
    let receipt = fixture.record(session, |record| {
        assert!(record.engram.runtime_checkpoint.is_none());
        let envelope = &record.engram.unadmitted_reports.last().unwrap().envelopes[0];
        let EngramRuntimeObservationDelivery::Recorded { receipt } = &envelope.delivery else {
            panic!("the real Engram producer must record the source-free report");
        };
        receipt.clone()
    });
    assert_eq!(receipt["accounting"]["kind"], "no_source_change");
    assert_eq!(receipt["observed_checks"][0]["credit"], "uncredited");
    let mut recorded_occurrence = occurrence.clone();
    recorded_occurrence["observed_checks"] = json!([{
        "check": occurrence["observed_checks"][0], "credit": "uncredited",
    }]);
    assert!(
        fixture.object(receipt["observation"].as_str().unwrap())["occurrence"]
            == recorded_occurrence,
        "the exact occurrence remains stored"
    );
    assert!(
        fixture
            .live
            .transport
            .requests_for("verification_record")
            .is_empty()
    );
    assert_no_live_turn_gate(&fixture.live.transport);
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn source_observation_live_delete_recovers_original_committed_reply() {
    let fixture = completion_fixture();
    let session = &fixture.live.session_id;
    let added = fixture.work(
        session,
        &["add", "Recover tracking before deletion", "--json"],
    );
    let work = added["work"]["short_ref"]
        .as_str()
        .or_else(|| added["short_ref"].as_str())
        .unwrap();
    fixture.work(session, &["claim", work, "--json"]);
    fixture
        .live
        .state
        .name_engram_source_root(
            session,
            EngramSourceRootRequest {
                work: work.to_owned(),
                path: Some(Some(fixture.root.to_string_lossy().into_owned())),
            },
        )
        .unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *fixture.live.transport.reply_pause.lock().unwrap() =
        Some(("execution_observe".to_owned(), entered_tx, release_rx));
    fixture
        .live
        .transport
        .faults
        .lock()
        .unwrap()
        .push_back(BoundaryFault::DropAfterReply {
            operation: "execution_observe",
        });
    fixture.begin(
        session,
        "Keep the original delivered prompt and its observation.",
    );
    finish_live_root(&fixture.live.state, session);
    entered_rx
        .recv_timeout(DEADLOCK_GUARD)
        .expect("the real producer committed before the boundary pause");
    let retained = fixture.record(session, |record| {
        assert_eq!(record.engram.unadmitted_reports.len(), 1);
        serde_json::to_value(&record.engram.unadmitted_reports[0]).unwrap()
    });
    let saved: Value =
        serde_json::from_str(&persisted_session_json(&fixture.live.state, session)).unwrap();
    assert!(
        saved["engramUnadmittedReports"][0] == retained,
        "real committed session row owns the original report"
    );
    let loaded = load_state(fixture.live.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let loaded_record = &loaded.sessions[loaded.find_session_index(session).unwrap()];
    assert!(
        serde_json::to_value(&loaded_record.engram.unadmitted_reports[0]).unwrap() == retained,
        "the exact report survives reload"
    );
    let error = fixture
        .live
        .state
        .kill_session(session)
        .err()
        .expect("Delete keeps the unresolved original report");
    assert!(error.message.contains("Session kept"), "<details withheld>");
    fixture.record(session, |record| {
        assert!(record.engram.observation_delete_requested && record.orchestrator_auto_dispatch_blocked);
        assert!(record.session.messages.iter().any(|message| matches!(message,
            Message::Text { text, .. } if text == "Keep the original delivered prompt and its observation.")));
    });
    release_tx.send(()).unwrap();
    fixture.wait_for_reports(session);
    let observations = fixture.live.transport.requests_for("execution_observe");
    let original = observations.first().unwrap();
    let replay = observations
        .iter()
        .filter(|request| request["idempotency_key"] == original["idempotency_key"])
        .collect::<Vec<_>>();
    assert_eq!(
        replay.len(),
        2,
        "one discarded committed reply and one exact recovery"
    );
    assert!(
        replay[0] == replay[1],
        "source timestamps, claim, policy, interval and key replay byte-for-byte"
    );
    assert!(
        fixture.live.receiver.try_recv().is_err(),
        "report retry cannot redeliver the provider prompt"
    );
    assert_no_live_turn_gate(&fixture.live.transport);
    fixture.live.state.kill_session(session).unwrap();
    assert!(
        fixture
            .live
            .state
            .inner
            .lock()
            .unwrap()
            .find_session_index(session)
            .is_none()
    );
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn between_turn_observation_makes_the_earlier_check_stale() {
    for named in [true, false] {
        let fixture = completion_fixture();
        let session = &fixture.live.session_id;
        let added = fixture.work(
            session,
            &[
                "add",
                "Check source observation freshness",
                "--accept",
                "A test passes at the current source",
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
        if !named {
            unnamed_source_interval(&fixture, session, work);
            continue;
        }
        fixture
            .live
            .state
            .name_engram_source_root(
                session,
                EngramSourceRootRequest {
                    work: work.to_owned(),
                    path: Some(Some(fixture.root.to_string_lossy().into_owned())),
                },
            )
            .unwrap();
        fixture.begin(session, "Check the first revision.");
        let source_now = || {
            let deadline = std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET;
            let owner = fixture
                .live
                .state
                .source_owner_metadata_until(session, deadline)
                .unwrap();
            engram_source_id_until(&owner, &fixture.root, deadline).unwrap()
        };
        let before = source_now();
        let (registration, frozen) =
            fixture.prepare_registered_check(session, &fixture.root, "cargo test --offline", true);
        let (body, receipt) = fixture.record_registered_check(registration, frozen);
        let verification = &receipt.verifications[0].record_id;
        let original_check = fixture.object(verification);
        assert!(
            original_check["source_basis"]
                == serde_json::to_value(engram_verification_record_basis(&body).unwrap()).unwrap(),
            "the original check retains its complete prepared basis"
        );
        finish_live_root(&fixture.live.state, session);
        fixture.wait_for_reports(session);
        let prior_requests = fixture
            .live
            .transport
            .requests_for("execution_observe")
            .len();
        fs::write(
            fixture.root.join("README.md"),
            "changed between ordinary turns\n",
        )
        .unwrap();
        let changed = source_now();
        assert!(changed != before, "the full identity records the real edit");
        fixture.begin(
            session,
            "Inspect quietly without running a check or writing source.",
        );
        finish_live_root(&fixture.live.state, session);
        fixture.wait_for_reports(session);
        let requests = fixture.live.transport.requests_for("execution_observe");
        let current = &requests[prior_requests..];
        let change = current
            .iter()
            .find_map(|request| {
                request["occurrence"]
                    .get("source_change")
                    .filter(|change| change["detection"] == "content_comparison")
            })
            .expect("the real closing capture accounts for the interval");
        assert!(
            change["baseline"]["source_id"] == serde_json::to_value(&before).unwrap(),
            "the baseline retains the complete original identity"
        );
        assert!(
            change["sighting"]["source_basis"]["source_id"]
                == serde_json::to_value(&changed).unwrap(),
            "the sighting retains the complete changed identity"
        );
        assert!(change["baseline"].get("source_revision").is_none());
        assert!(
            change["sighting"]["source_basis"]
                .get("source_revision")
                .is_none()
        );
        let actual_receipt = fixture.record(session, |record| {
            record
                .engram
                .unadmitted_reports
                .iter()
                .flat_map(|report| &report.envelopes)
                .find_map(|envelope| {
                    if envelope.request["occurrence"]["source_change"] != *change {
                        return None;
                    }
                    match &envelope.delivery {
                        EngramRuntimeObservationDelivery::Recorded { receipt } => {
                            Some(receipt.clone())
                        }
                        _ => None,
                    }
                })
                .expect("the actual producer acknowledged this exact source change")
        });
        let stored_observation = fixture.object(actual_receipt["observation"].as_str().unwrap());
        assert!(
            stored_observation["occurrence"]["source_change"] == *change,
            "the exact source change was durably retained"
        );
        assert_eq!(stored_observation["causality"]["kind"], "unknown");
        for request in current {
            assert_eq!(request["causality"]["kind"], "unknown");
            assert_eq!(request["policy_basis"]["mode"], "account_if_eligible");
            assert!(request.get("routing_token").is_none());
            assert!(
                request["occurrence"]["observed_checks"]
                    .as_array()
                    .is_none_or(Vec::is_empty)
            );
        }
        assert!(
            fixture.object(verification) == original_check,
            "new source never rewrites old producer evidence"
        );
        assert!(
            source_now() == changed,
            "later reads never change the recorded source"
        );
        let shown = fixture.work(session, &["show", work, "--json"]);
        let acceptance = shown["acceptance_basis"].as_i64().unwrap().to_string();
        let evidence = shown["evidence_basis"].as_i64().unwrap().to_string();
        let citation = format!("1={verification}");
        let declared = AcceptanceSourcePayload {
            source_id: changed.clone(),
            source_file: None,
        }
        .publish(&fixture.home.join("evaluation-sources"))
        .unwrap();
        let refused = refused_evaluation(
            &fixture,
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
                "1=Try the earlier native check at changed source",
                "--evidence",
                &citation,
                "--source-id-file",
                declared.path().to_str().unwrap(),
                "--attempt",
                "old-check",
                "--json",
            ],
        );
        assert_eq!(
            refused["error"]["code"], "work_completion_refused",
            "<refused withheld>"
        );
        assert_eq!(
            refused["error"]["details"]["reason"],
            "no registered check at the evaluated cut tested the declared commit; run a passing check on the intended current commit"
        );
        assert!(
            matches!(&body.input, EngramVerificationRecordInput::SourceId { source_id }
            if source_id == &before),
            "the native check retains its entire original identity"
        );
        assert!(
            fixture.object(verification) == original_check,
            "the refused evaluation leaves the original check unchanged"
        );
        assert_no_live_turn_gate(&fixture.live.transport);
    }
}

fn unnamed_source_interval(fixture: &CompletionFixture, session: &str, work: &str) {
    let binding = fixture.claim_binding(session);
    // A canonical claim read alone does not select the host's work association.
    // Bind the real held claim without naming a root or seeding a checkpoint.
    let association = EngramRootIntentAssociation::validate(
        &fixture.store(),
        &binding.work_id,
        &binding.claim_id,
        Some(binding.claim_fence),
        Some(&binding),
    )
    .unwrap();
    let target = fixture.target(session);
    let deadline = fixture.live.state.engram_budget_clock().now() + target.settings.call_timeout();
    let target = fixture
        .live
        .state
        .bind_fresh_engram_source_root_until(target, &association, deadline)
        .unwrap();
    assert!(fixture.record(session, |record| {
        record.engram.work_binding.as_ref() == Some(&binding)
            && record.engram.runtime_checkpoint.is_none()
    }));
    let canonical = fixture.canonical_read(session, &binding);
    assert_eq!(canonical.named_root, EngramNamedRootState::None);
    let fence = EngramRootReadFence::capture(&fixture.live.state.inner.lock().unwrap());
    fixture
        .live
        .state
        .reconcile_engram_named_root_with_route_classified_until(
            session,
            Some(&binding),
            Some(canonical.named_root),
            &fence,
            Some(&target.connection),
            fixture.live.state.engram_budget_clock().now() + target.settings.call_timeout(),
        )
        .unwrap_or_else(|_| panic!("the real unnamed-root authority reconciles"));
    assert_eq!(
        fixture.live.state.engram_root_capture(session),
        EngramRootCapture::Unnamed,
        "the claimed workdir is canonically unnamed before provider handoff"
    );
    fixture.begin(
        session,
        "Observe a genuine unnamed check without native registration credit.",
    );
    let source_now = || {
        let deadline = std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET;
        let owner = fixture
            .live
            .state
            .source_owner_metadata_until(session, deadline)
            .unwrap();
        engram_source_id_until(&owner, &fixture.root, deadline).unwrap()
    };
    let before = source_now();
    let mut recorder = SessionRecorder::new(fixture.live.state.clone(), session.to_owned());
    recorder
        .command_started_in(
            "unnamed-check",
            "cargo test --offline",
            Some("cargo test --offline"),
            Some(&fixture.root.to_string_lossy()),
        )
        .unwrap();
    let output = Command::new("cargo")
        .args(["test", "--offline"])
        .current_dir(&fixture.root)
        .env("CARGO_TARGET_DIR", fixture.root.join("target"))
        .output()
        .unwrap();
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success() && text.contains("1 passed"),
        "{text}"
    );
    recorder
        .command_completed_with_exit(
            "unnamed-check",
            "cargo test --offline",
            &text,
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    finish_live_root(&fixture.live.state, session);
    fixture.wait_for_reports(session);
    assert!(
        source_now() == before,
        "the unnamed check leaves its full source identity unchanged"
    );
    assert!(
        fixture.record(session, |record| {
            record
                .engram
                .runtime_checkpoint
                .as_ref()
                .is_some_and(|checkpoint| {
                    checkpoint.binding == binding
                        && checkpoint.endpoint.source_id.as_ref() == Some(&before)
                        && checkpoint.source_root_generation.is_none()
                        && checkpoint.source_root_state.is_none()
                })
        }),
        "the acknowledged unnamed sighting establishes the original full baseline"
    );
    fs::write(
        fixture.root.join("README.md"),
        "new unnamed source between turns\n",
    )
    .unwrap();
    let changed = source_now();
    assert!(
        changed != before,
        "the full unnamed identity records the real edit"
    );
    fixture.begin(session, "Read the changed unnamed source quietly.");
    finish_live_root(&fixture.live.state, session);
    fixture.wait_for_reports(session);
    let requests = fixture.live.transport.requests_for("execution_observe");
    assert!(
        requests.iter().any(|request| {
            let change = &request["occurrence"]["source_change"];
            change["detection"] == "content_comparison"
                && change["baseline"]["source_id"] == serde_json::to_value(&before).unwrap()
                && change["sighting"]["source_basis"]["source_id"]
                    == serde_json::to_value(&changed).unwrap()
        }),
        "the real producer receives the distinct unnamed source interval"
    );
    let shown = fixture.work(session, &["show", work, "--notes", "--json"]);
    let mut checks = Vec::new();
    objects_with_key(&shown, "verification", &mut checks);
    assert!(
        checks.is_empty(),
        "ordinary unnamed facts cannot invent a native verification"
    );
    assert_no_live_turn_gate(&fixture.live.transport);
}

fn refused_evaluation(fixture: &CompletionFixture, session: &str, args: &[&str]) -> Value {
    let (actor, context) = {
        let inner = fixture.live.state.inner.lock().unwrap();
        engram_runtime_actor_identity(
            &inner.preferences.engram.developer_name,
            &inner.sessions[inner.find_session_index(session).unwrap()],
        )
    };
    let mut command = Command::new(live_engram_binary());
    command
        .arg("--project-file")
        .arg(fixture.root.join(".engram-project"))
        .arg("--home")
        .arg(&fixture.home)
        .args(["work", "--actor-id", &actor, "--session-id", session]);
    if let Some(context) = context.as_deref() {
        command.args(["--actor-context", context]);
    }
    let output = command.args(args).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "a stale citation must be refused: stdout_bytes={} stderr_bytes={}",
        output.stdout.len(),
        output.stderr.len()
    );
    serde_json::from_slice(&output.stderr).unwrap_or_else(|_| {
        panic!(
            "typed evaluation refusal: <details withheld>: {}",
            output.stderr.len()
        )
    })
}
