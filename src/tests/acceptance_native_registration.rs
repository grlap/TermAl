// Real native registration and frozen artifact, with a scripted owner and
// receipt. These disposable protocol tests execute no consumer test suite.
use super::*;

type NativeCapture = dyn Fn(&FsPath, &FsPath, &[String], std::time::Instant, usize) -> Result<std::process::Output>
    + Send
    + Sync;

fn completed_native(
    dirty: bool,
    exit: i64,
) -> (
    NativeFixture,
    DeclaredRunReservation,
    TestRunRegistration,
    Value,
    Value,
) {
    let native = NativeFixture::new("acceptance-native-source", false);
    if dirty {
        fs::write(native.fixture.root.join("local.txt"), "local source\n").unwrap();
        native.record_change("local.txt", "??");
    }
    native.prepare();
    let mut recorder = native.recorder();
    native.start(&mut recorder);
    native.wait();
    native.artifact(if exit == 0 {
        include_str!("fixtures/declared-trx/pass.trx")
    } else {
        include_str!("fixtures/declared-trx/fail.trx")
    });
    let (start, registration, request, results) = native.finish(&mut recorder, exit);
    assert_eq!(results["input"]["kind"], "source_id");
    assert!(registration.declared_native.is_some());
    assert!(registration.declared_terminal.is_some());
    assert_eq!(
        engram_verification_record_body(&registration, &request, &results)
            .unwrap()
            .run
            .outcome,
        if exit == 0 {
            EngramVerificationRecordRunOutcome::Passed
        } else {
            EngramVerificationRecordRunOutcome::Failed
        },
        "the fixture's native artifact must provide the requested verdict"
    );
    (native, start, registration, request, results)
}

fn acknowledge(
    native: &NativeFixture,
    registration: &TestRunRegistration,
    request: &Value,
    results: &Value,
) {
    let body = engram_verification_record_body(registration, request, results).unwrap();
    assert_eq!(body.run.outcome, EngramVerificationRecordRunOutcome::Passed);
    let receipt: EngramVerificationRecordReceipt = serde_json::from_value(json!({
        "operation":"5f0c2b6e8d4a4e1f9a3b7c6d5e4f3a2b",
        "producer":"5f0c2b6e8d4a4e1f9a3b7c6d5e4f3a2b",
        "verifications":[{"stage":"focused","record_id":"a".repeat(32)}],
        "run_outcome":"passed"
    }))
    .unwrap();
    engram_verification_record_receipt_check(&body, &receipt).unwrap();
    fs::write(
        PathBuf::from(&registration.run_directory).join(ENGRAM_VERIFICATION_RECORD_BODY_FILE),
        engram_verification_record_text(&body).unwrap(),
    )
    .unwrap();
    native.fixture.state.note_engram_verification_record_ack(
        registration,
        TestRunVerificationAck::Received {
            receipt,
            body_digest: engram_verification_record_body_digest(&body),
        },
    );
}

fn source_capture(root: &FsPath, source: EngramSourceId) -> Arc<NativeCapture> {
    let entries: Vec<Value> = source
        .changes
        .iter()
        .map(|change| json!({"path":change.path,"status":change.status,"origin":change.origin}))
        .collect();
    let envelope = serde_json::to_vec(&json!({"version":1,
        "status":if entries.is_empty() {"clean"} else {"not_clean"},
        "root":engram_exact_path_key(root),
        "source":format!("git-commit:{}",source.commit),"tree":source.tree,
        "entries":entries,"refusal":null,"source_id":source}))
    .unwrap();
    Arc::new(move |_, _, args, _, _| {
        if args.first().map(String::as_str) == Some("--version") {
            return Ok(crate::tests::engram_source_owner::source_owner_reply(args));
        }
        Ok(crate::tests::engram_source_owner::source_owner_output(
            0,
            envelope.clone(),
        ))
    })
}

fn current_source(native: &NativeFixture, source: EngramSourceId) {
    current_capture(native, source_capture(&native.fixture.root, source));
}

fn current_capture(native: &NativeFixture, capture: Arc<NativeCapture>) {
    let mut inner = native.fixture.state.inner.lock().unwrap();
    // Selection and later owner capture enter through different production
    // callers. Both must receive the same metadata and complete source reply.
    inner.test_acceptance_source_owner_capture = Some(capture.clone());
    inner.test_observation_source_owner = Some(capture);
}

fn decision(
    native: &NativeFixture,
    start: &DeclaredRunReservation,
) -> Result<AcceptanceCommittedInputDecision, AcceptanceCommittedInputRefusal> {
    let (target, root) = {
        let inner = native.fixture.state.inner.lock().unwrap();
        (
            acceptance_evaluation_host_target_locked(&inner, &native.session).unwrap(),
            AcceptanceEvaluationSourceRoot::from_entry(&inner.engram_work_source_roots[0]),
        )
    };
    let work = json!({"short_ref":"native-test","lifecycle":"open","revision":1,"title":"Native acceptance",
        "outcome":"The native test verifies the named source.","acceptance":["The native test passes."]});
    let mut task = parse_acceptance_evaluation_task(
        json!({"acceptance_basis":1,"evidence_basis":1,"status":{"work":work},"notes":[],"notes_omitted":0}),
        json!({"work":work})).unwrap();
    task.work_id = Some(start.binding.work_id.clone());
    task.current_claim = Some(AcceptanceCurrentClaim {
        claim_id: start.binding.claim_id.clone(),
        claim_fence: start.binding.claim_fence,
        binding: Some(start.binding.clone()),
    });
    native.fixture.state.acceptance_committed_input_decision(
        &native.session,
        &target.connection,
        &target.store,
        &task,
        Some(&root),
        std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT,
        &std::time::Instant::now,
    )
}

#[test]
fn an_acknowledged_clean_native_run_admits_its_named_root() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (native, start, registration, request, results) = completed_native(false, 0);
    acknowledge(&native, &registration, &request, &results);
    let source = start.source.source_id.clone();
    current_source(&native, source.clone());
    assert_eq!(
        decision(&native, &start),
        Ok(AcceptanceCommittedInputDecision::Admitted(
            AcceptanceCommittedInputAdmission {
                run_id: start.run_id,
                commit: source.commit.clone(),
                native_source: Some(source)
            }
        )),
        "an acknowledged native record on clean HEAD must not be malformed launcher input"
    );
}

#[test]
fn acknowledged_dirty_native_source_refuses_with_a_commit_and_rerun_remedy() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (native, start, registration, request, results) = completed_native(true, 0);
    // Recording unchanged local source remains valid; admission is separate.
    acknowledge(&native, &registration, &request, &results);
    for clean_now in [false, true] {
        let mut source = start.source.source_id.clone();
        if clean_now {
            source.changes.clear();
        }
        current_source(&native, source);
        let refusal = decision(&native, &start).unwrap_err();
        assert_eq!(
            refusal,
            AcceptanceCommittedInputRefusal::NativeLocalChanges {
                run_id: start.run_id.clone()
            }
        );
        let error = refusal.into_api_error("native-test");
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(
            error.message.contains("local source changes")
                && error.message.contains("Commit")
                && error.message.contains("re-run"),
            "{}",
            error.message
        );
    }
}

#[test]
fn native_admission_requires_a_pass_and_the_host_acknowledgement() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for exit in [0, 1] {
        let (native, start, _, _, _) = completed_native(false, exit);
        current_source(&native, start.source.source_id.clone());
        let refusal = decision(&native, &start).unwrap_err();
        if exit == 0 {
            assert!(matches!(
                refusal,
                AcceptanceCommittedInputRefusal::RecordNotAcknowledged { .. }
            ));
        } else {
            assert!(matches!(
                refusal,
                AcceptanceCommittedInputRefusal::NoEligibleRun { .. }
            ));
        }
    }
}

#[test]
fn native_admission_rejects_rewritten_results_body_and_receipt() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for damaged in ["results", "body", "receipt"] {
        let (native, start, registration, request, mut results) = completed_native(false, 0);
        acknowledge(&native, &registration, &request, &results);
        current_source(&native, start.source.source_id.clone());
        match damaged {
            "results" => {
                results["nativeEligible"] = json!(false);
                fs::write(
                    PathBuf::from(&registration.run_directory).join("results.json"),
                    serde_json::to_vec_pretty(&results).unwrap(),
                )
                .unwrap();
            }
            "body" => fs::write(
                PathBuf::from(&registration.run_directory)
                    .join(ENGRAM_VERIFICATION_RECORD_BODY_FILE),
                "{}",
            )
            .unwrap(),
            _ => {
                let mut inner = native.fixture.state.inner.lock().unwrap();
                let mut row = inner.test_run_registrations.get_mut(&start.run_id).unwrap();
                let ack = row.verification_record.as_mut().unwrap();
                let TestRunVerificationAck::Received { body_digest, .. } = ack else {
                    panic!("receipt fixture");
                };
                *body_digest = "b".repeat(64);
            }
        }
        assert!(
            decision(&native, &start).is_err(),
            "{damaged} must not admit or go legacy"
        );
    }
}

#[test]
fn native_admission_fences_the_full_source_at_selection_recheck_and_owner_capture() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (native, start, registration, request, results) = completed_native(false, 0);
    acknowledge(&native, &registration, &request, &results);
    let source = start.source.source_id.clone();
    let mut other = source.clone();
    other.tree = if source.tree == "b".repeat(40) {
        "c".repeat(40)
    } else {
        "b".repeat(40)
    };
    current_source(&native, other.clone());
    assert!(
        matches!(
            decision(&native, &start),
            Err(AcceptanceCommittedInputRefusal::NoEligibleRun { .. })
        ),
        "the same commit does not replace a full-source comparison"
    );
    current_source(&native, source.clone());
    let state = native.fixture.state.clone();
    let capture = source_capture(&native.fixture.root, other.clone());
    TEST_ACCEPTANCE_COMMITTED_INPUT_BEFORE_RECHECK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            state
                .inner
                .lock()
                .unwrap()
                .test_acceptance_source_owner_capture = Some(capture);
        }))
    });
    assert_eq!(
        decision(&native, &start),
        Err(AcceptanceCommittedInputRefusal::Changed)
    );
    current_source(&native, source);
    let AcceptanceCommittedInputDecision::Admitted(admission) = decision(&native, &start).unwrap()
    else {
        panic!("native admission");
    };
    let place = {
        let inner = native.fixture.state.inner.lock().unwrap();
        AcceptanceEvaluationSourceRoot::from_entry(&inner.engram_work_source_roots[0]).place()
    };
    let (basis, _, _) = native
        .fixture
        .state
        .acceptance_owner_judged_source(&native.session, &place, Some(&admission))
        .unwrap();
    assert!(
        matches!(basis, Some(AcceptanceSourceBasis::Shared(ref basis))
        if Some(&basis.source_id) == admission.native_source.as_ref())
    );
    current_source(&native, other);
    assert!(
        native
            .fixture
            .state
            .acceptance_owner_judged_source(&native.session, &place, Some(&admission))
            .is_err()
    );
    current_capture(
        &native,
        Arc::new(|_, _, _, _, _| Err(anyhow!("source unavailable"))),
    );
    assert!(
        native
            .fixture
            .state
            .acceptance_owner_judged_source(&native.session, &place, Some(&admission))
            .is_err(),
        "native admission cannot become unmeasured"
    );
}

#[test]
fn native_pending_and_changed_terminal_authority_never_admit() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (native, start, registration, request, results) = completed_native(false, 0);
    acknowledge(&native, &registration, &request, &results);
    current_source(&native, start.source.source_id.clone());
    let state = native.fixture.state.clone();
    let run = start.run_id.clone();
    TEST_ACCEPTANCE_COMMITTED_INPUT_BEFORE_RECHECK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            state
                .inner
                .lock()
                .unwrap()
                .test_run_registrations
                .get_mut(&run)
                .unwrap()
                .declared_terminal = None;
        }))
    });
    assert_eq!(
        decision(&native, &start),
        Err(AcceptanceCommittedInputRefusal::Changed)
    );
    fs::write(
        PathBuf::from(&registration.run_directory).join("results.json"),
        serde_json::to_vec_pretty(&json!({"runId":start.run_id,"state":"running",
            "stages":[{"name":"focused","state":"running","started":start.registered_at}]}))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        decision(&native, &start),
        Err(AcceptanceCommittedInputRefusal::ScanIncomplete),
        "an uncompleted native run selects neither admission nor legacy"
    );
}
