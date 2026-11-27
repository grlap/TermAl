// Same-path recovery may retain an acknowledged complete source identity.
// Exercises the real evaluation request/spawn boundary and its negative controls;
// no registered evidence is rewritten to the current root generation.
use super::*;

fn recovered_reader(
    binding: EngramControlWorkBinding,
) -> impl Fn(&EngramConnectionConfig, &[String], Duration) -> Result<Value, EngramTransportError> {
    let reader = independent_reader();
    move |connection, args, timeout| {
        if args.iter().any(|arg| arg == "held") {
            Ok(
                json!({"items":[{"work_id":SIGHTED_WORK_ID,"short_ref":"w-task",
                "claim_id":binding.claim_id,"claim_fence":binding.claim_fence,
                "control_binding":binding}],"omitted":0}),
            )
        } else {
            reader(connection, args, timeout)
        }
    }
}

fn recovered_request(
    state: &AppState,
    parent: &str,
    binding: EngramControlWorkBinding,
) -> Result<AcceptanceEvaluationRequestResponse, ApiError> {
    state.request_acceptance_evaluation_with_runner(
        parent,
        evaluation_request(Some(Agent::Codex)),
        recovered_reader(binding),
    )
}

fn recover(
    state: &AppState,
    parent: &str,
    registration: &TestRunRegistration,
) -> (
    EngramControlWorkBinding,
    Arc<ScriptedEngramControlTransport>,
) {
    recover_with_sighting(state, parent, registration, json!({"state":"absent"}))
}

fn recover_with_sighting(
    state: &AppState,
    parent: &str,
    registration: &TestRunRegistration,
    sighting: Value,
) -> (
    EngramControlWorkBinding,
    Arc<ScriptedEngramControlTransport>,
) {
    let mut binding = registration.work_binding.clone().unwrap();
    binding.claim_fence += 1;
    binding.work_revision += 1;
    let store = registration.authority_store.as_ref().unwrap();
    rename_same_workspace_with_binding(state, parent, store, binding.clone());
    let root = &registration.named_root.as_ref().unwrap().workspace_id;
    let mut answer = sighting_reply(root, 2, sighting);
    answer["current_binding"] = json!("fixture-renamed-event");
    answer["root"]["binding_event"] = json!("fixture-renamed-event");
    answer["root"]["binding_position"] = json!(2);
    let transport = install_sighting_answer(state, Ok(answer));
    transport.replace_held_claims([Ok(EngramHeldClaims {
        items: vec![EngramHeldClaim {
            work_id: binding.work_id.clone(),
            short_ref: "w-task".into(),
            claim_id: binding.claim_id.clone(),
            claim_fence: binding.claim_fence,
            focused: false,
            control_binding: Some(binding.clone()),
        }],
        omitted: 0,
    })]);
    (binding, transport)
}

#[test]
fn same_path_recovery_lists_and_disposes_more_than_the_admission_scan_limit() {
    let (state, parent, _, root) = named_root_parent();
    let head = commit_named_root(&root);
    let runs: Vec<_> = (0..=ACCEPTANCE_COMMITTED_INPUT_CANDIDATES_MAX)
        .map(|index| {
            let run_id = format!("test-older-same-path-{index:04}");
            let mut results = committed_results("passed", input_record(&head, "clean"));
            results["runId"] = json!(run_id);
            let registration = register_committed_run_as(&state, &parent, &root, &run_id, results);
            host_observed(&state, &registration, &head, acknowledged("settled"));
            registration
        })
        .collect();
    let (binding, transport) = recover_with_sighting(&state, &parent, &runs[0], present_sighting());
    let error = refusal(
        &state,
        recovered_request(&state, &parent, binding.clone()),
        "over-bound retired history",
    );
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.starts_with("registered_check_root_rebound"));
    for run in &runs {
        assert!(error.message.contains(&run.run_id));
    }

    let mut request = evaluation_request(Some(Agent::Codex));
    request.retired_check_dispositions = runs
        .iter()
        .map(|run| AcceptanceRetiredCheckRequest {
            run_id: run.run_id.clone(),
            reason: "Use the recovered root at the current generation.".into(),
        })
        .collect();
    let reader = recovered_reader(binding);
    let response = state
        .request_acceptance_evaluation_with_runner(&parent, request, |connection, args, timeout| {
            if args.iter().any(|arg| arg == "note") {
                return Ok(json!({"operation":"note","evidence":"a".repeat(32),
                    "work":{"short_ref":"w-task","work_id":SIGHTED_WORK_ID,
                    "revision":7,"lifecycle":"open"}}));
            }
            reader(connection, args, timeout)
        })
        .unwrap_or_else(|error| panic!("{}", error.message));
    assert!(matches!(
        response,
        AcceptanceEvaluationRequestResponse::Spawned { .. }
    ));
    assert_eq!(sighting_reads(&transport), 1);
}

#[test]
fn same_path_rebound_preserves_a_newer_source_run_on_another_commit() {
    let (state, holder, _, root) = named_root_parent();
    let worktree = worktree(&root);
    let older_head = commit_named_root(&root);
    // The full-source run sorts before this one even if registration times tie.
    let older_run_id = "test-older-commit-only";
    let mut results = committed_results("passed", input_record(&older_head, "clean"));
    results["runId"] = json!(older_run_id);
    let older = register_committed_run_as(&state, &holder, &root, older_run_id, results);
    host_observed(&state, &older, &older_head, acknowledged("settled"));

    fs::write(worktree.join("judged.txt"), "a second committed input\n").unwrap();
    let newer_head = commit_named_root(&root);
    let newer_source = EngramSourceId {
        git_delta: None,
        commit: newer_head.clone(),
        tree: run_git_test_command_output(&worktree, &["rev-parse", "HEAD^{tree}"]),
        changes: Vec::new(),
        submodules: Vec::new(),
    };
    acceptance_owner_reports(&state, &holder, &root, Vec::new(), Vec::new());
    let newer = try_register_committed_run_with_source_as(
        &state,
        &holder,
        &root,
        COMMITTED_RUN_ID,
        committed_results("passed", input_record(&newer_head, "clean")),
        Some(newer_source.clone()),
    )
    .unwrap();
    let body = source_record_compatibility::write_owner_run_body(&newer);
    let mut progress = acknowledged("settled");
    progress["body_digest"] = json!(engram_verification_record_body_digest(&body));
    state.note_engram_verification_record_ack(&newer, serde_json::from_value(progress).unwrap());

    run_git_test_command(&worktree, &["switch", "--detach", &older_head]);
    acceptance_owner_reports(&state, &holder, &root, Vec::new(), Vec::new());
    let (binding, transport) = recover(&state, &holder, &older);
    let rebound = refusal(
        &state,
        recovered_request(&state, &holder, binding.clone()),
        "only the applicable commit-only predecessor blocks the request",
    );
    assert_eq!(rebound.status, StatusCode::CONFLICT);
    assert!(
        rebound
            .message
            .starts_with("registered_check_root_rebound:")
    );
    assert!(rebound.message.contains(older_run_id));
    assert!(!rebound.message.contains(COMMITTED_RUN_ID));

    let mut request = evaluation_request(Some(Agent::Codex));
    request.retired_check_dispositions = vec![AcceptanceRetiredCheckRequest {
        run_id: COMMITTED_RUN_ID.into(),
        reason: "Attempt to exclude a check that can admit on its own commit.".into(),
    }];
    let reader = recovered_reader(binding.clone());
    let notes = std::cell::Cell::new(0);
    let denied = refusal(
        &state,
        state.request_acceptance_evaluation_with_runner(
            &holder,
            request,
            |connection, args, timeout| {
                if args.iter().any(|arg| arg == "note") {
                    notes.set(notes.get() + 1);
                    return Ok(json!({"operation":"note","evidence":"b".repeat(32),
                    "work":{"short_ref":"w-task","work_id":SIGHTED_WORK_ID,
                    "revision":7,"lifecycle":"open"}}));
                }
                reader(connection, args, timeout)
            },
        ),
        "the skipped run cannot be excluded",
    );
    assert_eq!(denied.status, StatusCode::BAD_REQUEST);
    assert!(denied.message.contains(COMMITTED_RUN_ID));
    assert_eq!(notes.get(), 0);
    assert!(
        current_registration(&state)
            .retired_check_disposition
            .is_none()
    );

    run_git_test_command(&worktree, &["switch", "--detach", &newer_head]);
    acceptance_owner_reports(&state, &holder, &root, Vec::new(), Vec::new());
    let response = recovered_request(&state, &holder, binding)
        .unwrap_or_else(|error| panic!("{}", error.message));
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response else {
        panic!("the holder remains eligible on the newer run's source");
    };
    let target = delegation.delegation.acceptance_evaluation.unwrap();
    assert_eq!(target.source_id, Some(newer_source));
    let named = target.source_root.unwrap();
    assert_eq!(named.root, root);
    assert_eq!(named.generation, 2);
    assert_eq!(sighting_reads(&transport), 0);
}

#[test]
fn same_path_rebound_stops_reading_when_an_outcome_exhausts_the_deadline() {
    let (state, parent, _, root) = named_root_parent();
    let head = commit_named_root(&root);
    let runs: Vec<_> = ["test-predecessor-older", "test-predecessor-newer"]
        .into_iter()
        .map(|run_id| {
            let mut results = committed_results("passed", input_record(&head, "clean"));
            results["runId"] = json!(run_id);
            let registration = register_committed_run_as(&state, &parent, &root, run_id, results);
            host_observed(&state, &registration, &head, acknowledged("settled"));
            registration
        })
        .collect();
    let (binding, _) = recover(&state, &parent, &runs[0]);
    let task = task_under(current_claim(binding.claim_fence, Some(binding)));
    let (target, root) = {
        let inner = state.inner.lock().unwrap();
        (
            acceptance_evaluation_host_target_locked(&inner, &parent).unwrap(),
            AcceptanceEvaluationSourceRoot::from_entry(&inner.engram_work_source_roots[0]),
        )
    };
    let before = std::time::Instant::now();
    let deadline = before + Duration::from_secs(60);
    let decide = |now: &dyn Fn() -> std::time::Instant| {
        state.acceptance_committed_input_decision(
            &parent,
            &target.connection,
            &target.store,
            &task,
            Some(&root),
            deadline,
            &now,
        )
    };
    assert!(matches!(
        decide(&|| before),
        Err(AcceptanceCommittedInputRefusal::RootRebound { checks, .. }) if checks.len() == 2
    ));
    // Each attempt gets a fresh clock and body-read count. Trigger expiry
    // at the older outcome boundary, then separately after its body read.
    for (expire_after_outcome, expected_body_reads) in [(true, 1), (false, 2)] {
        TEST_ACCEPTANCE_COMMITTED_INPUT_BODY_READS.with(|reads| reads.set(0));
        let outcome_spent_budget = std::rc::Rc::new(std::cell::Cell::new(false));
        if expire_after_outcome {
            let outcome_spent_budget = outcome_spent_budget.clone();
            TEST_ACCEPTANCE_COMMITTED_INPUT_AFTER_CENSUS_OUTCOME.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || outcome_spent_budget.set(true)));
            });
        }
        let expired = decide(&|| {
            if outcome_spent_budget.get()
                || TEST_ACCEPTANCE_COMMITTED_INPUT_BODY_READS.with(|reads| reads.get())
                    >= runs.len()
            {
                deadline
            } else {
                before
            }
        });
        assert_eq!(
            expired,
            Err(AcceptanceCommittedInputRefusal::ScanIncomplete)
        );
        assert_eq!(outcome_spent_budget.get(), expire_after_outcome);
        TEST_ACCEPTANCE_COMMITTED_INPUT_BODY_READS.with(|reads| {
            assert_eq!(reads.get(), expected_body_reads);
        });
    }

    // Arm the clock only after the start scan, then expire on the initial
    // selector's first outcome read. It must leave both bodies unopened.
    TEST_ACCEPTANCE_COMMITTED_INPUT_BODY_READS.with(|reads| reads.set(0));
    let selecting = std::rc::Rc::new(std::cell::Cell::new(false));
    let selecting_in_hook = selecting.clone();
    TEST_ACCEPTANCE_COMMITTED_INPUT_BEFORE_SELECTION.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            TEST_ACCEPTANCE_COMMITTED_INPUT_RESULTS_READS.with(|reads| reads.set(0));
            selecting_in_hook.set(true);
        }));
    });
    let expired = decide(&|| {
        if selecting.get()
            && TEST_ACCEPTANCE_COMMITTED_INPUT_RESULTS_READS.with(|reads| reads.get() > 0)
        {
            deadline
        } else {
            before
        }
    });
    assert!(selecting.get());
    assert_eq!(
        expired,
        Err(AcceptanceCommittedInputRefusal::ScanIncomplete)
    );
    TEST_ACCEPTANCE_COMMITTED_INPUT_BODY_READS.with(|reads| assert_eq!(reads.get(), 0));
}

#[test]
fn an_unheld_request_cannot_dispose_the_holders_eligible_same_path_predecessor() {
    let (state, holder, root, head, registration) =
        source_record_compatibility::owner_run("settled");
    let (binding, transport) = recover(&state, &holder, &registration);
    let (project, workdir) = {
        let mut inner = state.inner.lock().unwrap();
        // A different session must see the acknowledged naming journal.
        // The holder's recovery fixture only installs its control binding
        // and published authority proof.
        let entry = inner.engram_work_source_roots[0].clone();
        let event = inner
            .engram_work_naming_history
            .iter()
            .find(|history| history.store == entry.store && history.work_id == entry.work_id)
            .and_then(|history| {
                history.proofs.iter().find_map(|proof| {
                    proof
                        .read
                        .latest_event
                        .as_ref()
                        .filter(|event| {
                            proof.read.run_id == binding.run_id
                                && proof.read.claim_id == binding.claim_id
                                && u64::try_from(event.generation).ok() == Some(entry.generation)
                        })
                        .cloned()
                })
            })
            .expect("the recovered naming has a published canonical event");
        inner
            .engram_named_root_journal
            .retain(|journal| journal.store != entry.store || journal.claim_id != entry.claim_id);
        inner
            .engram_named_root_journal
            .push(EngramNamedRootJournal {
                store: entry.store.clone(),
                claim_id: entry.claim_id.clone(),
                read_binding: Some(binding.clone()),
                confirmed: Some((
                    EngramNamedRootEvent {
                        root: entry.clone(),
                        reporter: holder.clone(),
                        kind: EngramNamedRootKind::Bound,
                        end_reason: None,
                    },
                    EngramNamedRootReceipt {
                        event: event.event,
                        position: event.position,
                        workspace_id: entry.root,
                        generation: event.generation,
                        kind: EngramNamedRootKind::Bound,
                    },
                )),
                pending: None,
                obsolete: false,
                retirement: None,
                reconciliation: None,
            });
        let record = &inner.sessions[inner.find_session_index(&holder).unwrap()];
        (
            record.session.project_id.clone().unwrap(),
            record.session.workdir.clone(),
        )
    };
    let requester =
        create_test_project_session(&state, Agent::Codex, &project, FsPath::new(&workdir));
    let base = independent_reader();
    let notes = std::cell::Cell::new(0);
    let unheld = |connection: &EngramConnectionConfig, args: &[String], timeout| {
        if args.iter().any(|arg| arg == "held") {
            assert_eq!(connection.session_id, requester);
            return Ok(json!({"items":[],"omitted":0}));
        }
        if args.iter().any(|arg| arg == "note") {
            notes.set(notes.get() + 1);
            return Ok(json!({"operation":"note","evidence":"b".repeat(32),
                "work":{"short_ref":"w-task","work_id":SIGHTED_WORK_ID,
                "revision":7,"lifecycle":"open"}}));
        }
        base(connection, args, timeout)
    };
    let unheld_rebound = refusal(
        &state,
        state.request_acceptance_evaluation_with_runner(
            &requester,
            evaluation_request(Some(Agent::Codex)),
            &unheld,
        ),
        "unheld requester cannot select the predecessor",
    );
    assert_eq!(unheld_rebound.status, StatusCode::CONFLICT);
    assert!(
        unheld_rebound
            .message
            .starts_with("registered_check_same_path_holder_required:"),
        "expected the retained root to reach blocker selection, got {}",
        unheld_rebound.message
    );
    assert!(unheld_rebound.message.contains(COMMITTED_RUN_ID));
    assert!(!unheld_rebound.message.contains("retiredCheckDispositions"));
    let mut disposition = evaluation_request(Some(Agent::Codex));
    disposition.retired_check_dispositions = vec![AcceptanceRetiredCheckRequest {
        run_id: COMMITTED_RUN_ID.into(),
        reason: "Apply an exclusion solely because this requester cannot select the run.".into(),
    }];
    let denied = refusal(
        &state,
        state.request_acceptance_evaluation_with_runner(&requester, disposition, &unheld),
        "an unheld requester cannot install a standing exclusion",
    );
    assert_eq!(denied.status, StatusCode::CONFLICT);
    assert_eq!(notes.get(), 0);
    assert!(
        current_registration(&state)
            .retired_check_disposition
            .is_none()
    );

    let response = recovered_request(&state, &holder, binding)
        .unwrap_or_else(|error| panic!("{}", error.message));
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response else {
        panic!("the holder remains eligible for independent evaluation");
    };
    let target = delegation.delegation.acceptance_evaluation.unwrap();
    assert_eq!(target.source_id.unwrap().commit, head);
    assert_eq!(target.source_root.unwrap().root, root);
    assert_eq!(
        sighting_reads(&transport),
        0,
        "holder still uses committed-input admission"
    );
}

#[test]
fn same_path_recovery_spawns_on_the_original_complete_source_without_relabelling() {
    for progress in ["received", "settled"] {
        let (state, parent, root, head, registration) =
            source_record_compatibility::owner_run(progress);
        let (binding, transport) = recover(&state, &parent, &registration);
        let before = current_registration(&state);
        let response = recovered_request(&state, &parent, binding)
            .unwrap_or_else(|error| panic!("{progress}: {}", error.message));
        let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response else {
            panic!("the recovered claim must spawn an independent evaluator");
        };
        let target = delegation.delegation.acceptance_evaluation.unwrap();
        let named = target.source_root.unwrap();
        assert_eq!(named.root, root);
        assert_eq!(named.generation, 2);
        assert_eq!(target.source_id.unwrap().commit, head);
        assert_eq!(current_registration(&state).named_root, before.named_root);
        assert_eq!(
            current_registration(&state).work_binding,
            before.work_binding
        );
        assert_eq!(
            current_registration(&state).verification_record,
            before.verification_record
        );
        assert_eq!(state.inner.lock().unwrap().test_run_registrations.len(), 1);
        assert_eq!(
            sighting_reads(&transport),
            0,
            "the old evidence was validated, not erased"
        );
    }
}

#[test]
fn same_path_recovery_keeps_source_receipt_and_identity_refusals() {
    for case in [
        "different path",
        "dirty source",
        "changed tree",
        "another work",
        "another run",
        "another store",
        "missing receipt",
        "wrong body digest",
        "rewritten body",
        "regressed fence",
        "regressed revision",
        "Commit-only evidence with empty criterion bindings",
    ] {
        let (state, parent, root, head, registration) =
            if case == "Commit-only evidence with empty criterion bindings" {
                let (state, parent, _store, root) = named_root_parent();
                let head = commit_named_root(&root);
                let registration = register_committed_run(
                    &state,
                    &parent,
                    &root,
                    committed_results("passed", input_record(&head, "clean")),
                );
                // A historical body must be acknowledged as the original body,
                // rather than replacing an already acknowledged SourceId body.
                host_observed(&state, &registration, &head, acknowledged("settled"));
                let registration = current_registration(&state);
                assert!(matches!(
                    acceptance_committed_input_record_acknowledged(&registration, &head)
                        .expect("the original Commit body has a coherent acknowledgement")
                        .input,
                    EngramVerificationRecordInput::Commit { .. }
                ));
                (state, parent, root, head, registration)
            } else {
                source_record_compatibility::owner_run("settled")
            };
        let (mut binding, _transport) = recover(&state, &parent, &registration);
        match case {
            "different path" | "another work" | "another run" | "another store" => {
                let mut inner = state.inner.lock().unwrap();
                let mut row = inner
                    .test_run_registrations
                    .get_mut(COMMITTED_RUN_ID)
                    .unwrap();
                match case {
                    "different path" => row
                        .named_root
                        .as_mut()
                        .unwrap()
                        .workspace_id
                        .push_str("-other"),
                    "another work" => row
                        .work_binding
                        .as_mut()
                        .unwrap()
                        .work_id
                        .push_str("-other"),
                    "another run" => row.work_binding.as_mut().unwrap().run_id.push_str("-other"),
                    _ => row
                        .authority_store
                        .as_mut()
                        .unwrap()
                        .project_id
                        .push_str("-other"),
                }
            }
            "dirty source" => acceptance_owner_reports_dirty(&state, &parent, &root, "judged.txt"),
            "changed tree" => {
                let frozen = engram_verification_record_frozen(
                    FsPath::new(&registration.run_directory),
                    COMMITTED_RUN_ID,
                )
                .unwrap();
                let EngramVerificationRecordInput::SourceId { mut source_id } = frozen.body.input
                else {
                    panic!("the fixture has the complete original source");
                };
                source_id.tree = "a".repeat(40);
                let envelope = serde_json::to_vec(&json!({"version":1,"status":"clean",
                    "root":root,"source":format!("git-commit:{head}"),"tree":source_id.tree,
                    "entries":[],"refusal":null,"source_id":source_id}))
                .unwrap();
                state
                    .inner
                    .lock()
                    .unwrap()
                    .test_acceptance_source_owner_capture = Some(Arc::new(move |_, _, _, _, _| {
                    Ok(crate::tests::engram_source_owner::source_owner_output(
                        0,
                        envelope.clone(),
                    ))
                }));
            }
            "missing receipt" => {
                state
                    .inner
                    .lock()
                    .unwrap()
                    .test_run_registrations
                    .get_mut(COMMITTED_RUN_ID)
                    .unwrap()
                    .verification_record = None;
            }
            "wrong body digest" => {
                let ack: TestRunVerificationAck = serde_json::from_value(json!({"state":"settled",
                    "body_digest":"0".repeat(64),"receipt":acknowledged("settled")["receipt"]}))
                .unwrap();
                state
                    .inner
                    .lock()
                    .unwrap()
                    .test_run_registrations
                    .get_mut(COMMITTED_RUN_ID)
                    .unwrap()
                    .verification_record = Some(ack);
            }
            "rewritten body" => {
                let path = FsPath::new(&registration.run_directory)
                    .join(ENGRAM_VERIFICATION_RECORD_BODY_FILE);
                let mut body: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                body["input"]["source_id"]["tree"] = json!("b".repeat(40));
                fs::write(path, serde_json::to_vec(&body).unwrap()).unwrap();
            }
            "regressed fence" => binding.claim_fence = 1,
            "regressed revision" => {
                state
                    .inner
                    .lock()
                    .unwrap()
                    .test_run_registrations
                    .get_mut(COMMITTED_RUN_ID)
                    .unwrap()
                    .work_binding
                    .as_mut()
                    .unwrap()
                    .work_revision = binding.work_revision + 1;
            }
            "Commit-only evidence with empty criterion bindings" => {}
            _ => unreachable!(),
        }
        let error = refusal(&state, recovered_request(&state, &parent, binding), case);
        assert_eq!(
            error.status,
            StatusCode::CONFLICT,
            "{case}: {}",
            error.message
        );
        if case == "Commit-only evidence with empty criterion bindings" || case == "different path"
        {
            assert!(
                error.message.starts_with("registered_check_root_rebound"),
                "{case}: {}",
                error.message
            );
        }
    }
}

#[test]
fn same_path_recovery_does_not_skip_a_newer_nonpass_or_unknown_result() {
    for case in ["failed", "unknown"] {
        let (state, parent, root, head, registration) =
            source_record_compatibility::owner_run("settled");
        let (binding, _transport) = recover(&state, &parent, &registration);
        let newer_id = "test-newer-after-recovery";
        let mut results = committed_results("failed", input_record(&head, "clean"));
        results["runId"] = json!(newer_id);
        let newer = register_committed_run_as(&state, &parent, &root, newer_id, results);
        state
            .inner
            .lock()
            .unwrap()
            .test_run_registrations
            .get_mut(newer_id)
            .unwrap()
            .registered_at = "9999-01-01T00:00:00Z".into();
        if case == "unknown" {
            fs::remove_file(FsPath::new(&newer.run_directory).join("results.json")).unwrap();
        }
        let error = refusal(&state, recovered_request(&state, &parent, binding), case);
        let expected = if case == "failed" {
            "committed_input_no_eligible_run"
        } else {
            "committed_input_scan_incomplete"
        };
        assert!(
            error.message.starts_with(expected),
            "{case}: {}",
            error.message
        );
        assert_eq!(
            current_registration(&state).named_root,
            registration.named_root
        );
    }
}
