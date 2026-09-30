// Tests of the declared source fingerprint of an acceptance evaluation: the
// request takes the content revision of the evaluator's worktree, a
// same-session caller is handed it, the record keeps it across a reload, and
// the first submission declares it only while the worktree still holds it. Does
// not own the rest of the acceptance-evaluation tests, the fixtures and helpers
// of which it uses from its parent module. New tests, placed in this child
// module of src/tests/acceptance_evaluation.rs so that file stays within the
// size the architecture lens allows for a test file.
use super::*;

#[test]
fn acceptance_requested_claim_controls_the_root_even_when_another_claim_is_bound() {
    for case in ["shared", "distinct", "unnamed"] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        make_workdir_a_worktree(&root);
        fs::write(
            root.join(".gitignore"),
            ".engram-project\nprojects/\nwork-read-args.txt\n.worktrees/\n",
        )
        .unwrap();
        run_git_test_command(&root, &["add", ".gitignore"]);
        run_git_test_command(&root, &["commit", "--quiet", "-m", "ignore worktrees"]);
        let first = root.join(".worktrees/first");
        run_git_test_command(
            &root,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "first",
                first.to_str().unwrap(),
            ],
        );
        let requested = if case == "distinct" {
            let second = root.join(".worktrees/second");
            run_git_test_command(
                &root,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "-b",
                    "second",
                    second.to_str().unwrap(),
                ],
            );
            second
        } else {
            first.clone()
        };
        fs::write(first.join("judged.txt"), "first named tree\n").unwrap();
        if case == "distinct" {
            fs::write(requested.join("judged.txt"), "requested named tree\n").unwrap();
        }
        let store = established_store(&state, &project).unwrap();
        let named = |work: &str, path: &FsPath, generation| {
            let (named_root, common_dir_key) = validate_engram_source_root(
                &path.to_string_lossy(),
                &root.to_string_lossy(),
                &root.to_string_lossy(),
            )
            .unwrap();
            EngramWorkSourceRoot {
                store: store.clone(),
                work_id: format!("work-{work}"),
                short_ref: format!("w-{work}"),
                claim_id: format!("claim-{work}"),
                claim_fence: 1,
                root: named_root,
                common_dir_key,
                named_by_session: parent.clone(),
                named_at: "2026-09-30T00:00:00Z".to_owned(),
                generation,
            }
        };
        {
            let mut inner = state.inner.lock().unwrap();
            inner.engram_work_source_roots = vec![named("other", &first, 1)];
            if case != "unnamed" {
                inner
                    .engram_work_source_roots
                    .push(named("task", &requested, 2));
            }
            let index = inner.find_session_index(&parent).unwrap();
            inner.sessions[index].engram.work_binding = Some(EngramControlWorkBinding {
                root_execution_id: "root-other".to_owned(),
                work_id: "work-other".to_owned(),
                run_id: "run-other".to_owned(),
                work_revision: 1,
                claim_id: "claim-other".to_owned(),
                claim_fence: 1,
            });
        }
        super::delegation_support::install_delegation_codex_runtime(
            &state,
            "requested-claim-runtime",
        );
        let (expected_actor, expected_context) = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            let record = &inner.sessions[inner.find_session_index(&parent).unwrap()];
            (
                format!("{}/codex", inner.preferences.engram.developer_name),
                engram_actor_context(&record.session),
            )
        };
        let mut show = show_receipt(None);
        show["status"]["work"]["work_id"] = json!("work-task");
        let base = fixture_reader(
            Arc::default(),
            show,
            Ok(policy_receipt(Some(&["independent_session"]))),
        );
        let held_reads = std::cell::Cell::new(0);
        let response = state.request_acceptance_evaluation_with_runner(&parent, evaluation_request(Some(Agent::Codex)), |connection, args, timeout| {
            if args.iter().any(|arg| arg == "held") {
                held_reads.set(held_reads.get() + 1);
                assert_eq!(connection.session_id, parent, "held claims are read as the requesting session");
                assert_ne!(connection.session_id, WORK_HOST_READER_SESSION_ID);
                assert_eq!(connection.actor_id, expected_actor);
                assert_eq!(connection.actor_context, expected_context);
                Ok(json!({"items": [
                    {"work_id":"work-other", "short_ref":"w-other", "claim_id":"claim-other", "claim_fence":1, "focused":true},
                    {"work_id":"work-task", "short_ref":"w-task", "claim_id":"claim-task", "claim_fence":1, "focused":false}
                ], "omitted":0}))
            } else { base(connection, args, timeout) }
        }).unwrap();
        let AcceptanceEvaluationRequestResponse::Spawned {
            delegation, notice, ..
        } = response
        else {
            panic!("evaluator expected")
        };
        let record = delegation.delegation;
        let target = record.acceptance_evaluation.unwrap();
        let expected_dir = if case == "unnamed" { &root } else { &requested };
        let expected = content_revision(expected_dir).unwrap().1;
        assert_eq!(
            record.cwd,
            normalize_user_facing_path(&fs::canonicalize(expected_dir).unwrap()),
            "{case}"
        );
        assert_eq!(target.source_fingerprint, Some(expected), "{case}");
        if case == "unnamed" {
            assert!(target.source_root.is_none());
            assert_eq!(
                target.source_claim,
                Some(AcceptanceEvaluationSourceClaim {
                    work_id: "work-task".to_owned(),
                    claim_id: "claim-task".to_owned()
                })
            );
            assert!(
                notice
                    .as_deref()
                    .is_some_and(|notice| notice.contains("w-task")
                        && notice.contains("no source root named")),
                "{notice:?}"
            );
        } else {
            let source = target.source_root.unwrap();
            assert_eq!(source.work_id, "work-task");
            assert_eq!(source.claim_id, "claim-task");
            assert_eq!(source.generation, 2);
            assert!(target.source_claim.is_none());
        }
        assert_eq!(held_reads.get(), 1);
    }
}

/// The parent's workdir as a committed Git worktree, so a content revision
/// can be taken on it. The fixture's tracker home is this same directory, and
/// the scripted tracker writes its store and its argument log there while the
/// request runs; they are ignored, as a real store outside the repository
/// never shows in its revision.
fn make_workdir_a_worktree(root: &FsPath) {
    init_git_document_test_repo(root);
    fs::write(
        root.join(".gitignore"),
        ".engram-project\nprojects/\nwork-read-args.txt\n",
    )
    .unwrap();
    fs::write(root.join("judged.txt"), "the judged content\n").unwrap();
    run_git_test_command(root, &["add", ".gitignore", "judged.txt"]);
    run_git_test_command(root, &["commit", "--quiet", "-m", "judged"]);
}

#[test]
fn acceptance_request_declares_the_content_revision_of_the_evaluators_worktree() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    make_workdir_a_worktree(&root);
    super::delegation_support::install_delegation_codex_runtime(
        &state,
        "acceptance-evaluation-runtime",
    );
    let response = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(Some(Agent::Codex)),
            fixture_reader(
                Arc::default(),
                show_receipt(None),
                Ok(policy_receipt(Some(&["independent_session"]))),
            ),
        )
        .unwrap();
    let expected = content_revision(&root).unwrap().1;
    assert!(
        serde_json::to_value(&response)
            .unwrap()
            .get("notice")
            .is_none(),
        "a measured revision needs no notice"
    );
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response else {
        panic!("an independent evaluation spawns an evaluator");
    };
    let record = delegation.delegation;
    // The evaluator reads the worktree the revision was taken on.
    assert_eq!(record.cwd, root.to_string_lossy());
    assert_eq!(
        record
            .acceptance_evaluation
            .expect("an evaluator carries its target")
            .source_fingerprint,
        Some(expected)
    );
}

#[test]
fn acceptance_same_session_request_hands_the_session_its_content_revision() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    make_workdir_a_worktree(&root);
    let response = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(None),
            fixture_reader(
                Arc::default(),
                show_receipt(None),
                Ok(policy_receipt(Some(&["same_session"]))),
            ),
        )
        .unwrap();
    let expected = content_revision(&root).unwrap().1;
    let wire = serde_json::to_value(&response).unwrap();
    assert_eq!(wire["sourceFingerprint"], expected.as_str());
    assert!(wire.get("notice").is_none());
    assert!(
        wire["brief"]
            .as_str()
            .unwrap()
            .contains(&format!("source_fingerprint {expected}")),
        "{}",
        wire["brief"]
    );
}

#[test]
fn acceptance_target_keeps_its_source_fingerprint_across_a_reload() {
    let target = DelegationAcceptanceEvaluation {
        source_fingerprint: Some(format!("content-v1:{}", "b".repeat(64))),
        ..evaluation_target("delegation-1", 2)
    };
    let persisted = serde_json::to_value(&target).unwrap();
    assert_eq!(
        persisted["sourceFingerprint"],
        target.source_fingerprint.as_deref().unwrap()
    );
    let reloaded: DelegationAcceptanceEvaluation = serde_json::from_value(persisted).unwrap();
    assert_eq!(reloaded, target);
    // A record persisted before the field existed loads without it.
    let legacy = serde_json::to_value(evaluation_target("delegation-1", 2)).unwrap();
    assert!(legacy.get("sourceFingerprint").is_none());
    let loaded: DelegationAcceptanceEvaluation = serde_json::from_value(legacy).unwrap();
    assert_eq!(loaded.source_fingerprint, None);
}

/// Submits for an evaluator of the fixture's workdir whose target declares
/// `fingerprint`, after `before_submit` ran; the arguments of its one tracker
/// call, or the refusal.
fn submit_declaring(
    fingerprint: impl FnOnce(&FsPath) -> String,
    make_worktree: bool,
    before_submit: impl FnOnce(&FsPath),
) -> std::result::Result<Vec<String>, ApiError> {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    if make_worktree {
        make_workdir_a_worktree(&root);
    }
    let workdir = root.to_string_lossy().into_owned();
    let (delegation, child) =
        install_evaluator_delegation(&state, &parent, Some(&project), &workdir, 2);
    let declared = fingerprint(&root);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&delegation).unwrap();
        inner.delegations[index]
            .acceptance_evaluation
            .as_mut()
            .unwrap()
            .source_fingerprint = Some(declared);
    }
    before_submit(&root);
    let calls: RecordedEngramCalls = Arc::default();
    let seen = calls.clone();
    state.submit_acceptance_evaluation_with_runner(
        &child,
        two_verdicts(),
        move |connection, args, _| {
            seen.lock()
                .unwrap()
                .push((connection.clone(), args.to_vec()));
            Ok(cli_output(true, &evaluate_receipt(false), ""))
        },
    )?;
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    Ok(calls[0].1.clone())
}

#[test]
fn acceptance_submit_declares_the_requested_revision_while_the_worktree_still_holds_it() {
    let mut expected = String::new();
    let args = submit_declaring(
        |root| {
            expected = content_revision(root).unwrap().1;
            expected.clone()
        },
        true,
        |_| {},
    )
    .unwrap();
    let at = args
        .iter()
        .position(|arg| arg == "--source-fingerprint")
        .expect("the confirmed fingerprint is declared");
    assert_eq!(args[at + 1], expected);
    assert_eq!(
        args[at + 2],
        "--model",
        "after the verdicts, before the model"
    );
    assert!(
        !args.iter().any(|arg| arg.starts_with("--source-workspace")),
        "no workspace is declared, so a peer worktree matches too"
    );
}

#[test]
fn acceptance_submit_refuses_when_the_worktree_moved_on_while_it_was_evaluated() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    make_workdir_a_worktree(&root);
    let workdir = root.to_string_lossy().into_owned();
    let (delegation, child) =
        install_evaluator_delegation(&state, &parent, Some(&project), &workdir, 2);
    let requested = content_revision(&root).unwrap().1;
    update_evaluation_target(&state, &delegation, |target| {
        target.source_fingerprint = Some(requested.clone());
    });
    fs::write(root.join("judged.txt"), "changed under the evaluator\n").unwrap();
    let (calls, run) = scripted_runner(Vec::new());
    let error = state
        .submit_acceptance_evaluation_with_runner(&child, two_verdicts(), run)
        .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error.message.contains("changed while it was evaluated"),
        "{}",
        error.message
    );
    assert!(error.message.contains(&requested), "{}", error.message);
    // Final for the evaluator, in the refusal and in the tool it reads.
    assert!(
        error.message.contains("do not submit again"),
        "{}",
        error.message
    );
    assert!(
        !error.message.contains("nothing was sent"),
        "{}",
        error.message
    );
    assert!(
        TERMAL_SUBMIT_ACCEPTANCE_EVALUATION_TOOL_DESCRIPTION
            .contains("worktree changed while it was evaluated, do not submit again")
    );
    // Refused before anything runs or is written.
    assert!(calls.lock().unwrap().is_empty());
    let inner = state.inner.lock().unwrap();
    let index = inner.find_delegation_index(&delegation).unwrap();
    assert!(matches!(
        inner.delegations[index]
            .acceptance_evaluation
            .as_ref()
            .unwrap()
            .submission,
        AcceptanceEvaluationSubmission::None
    ));
}

#[test]
fn acceptance_submit_declares_nothing_when_the_revision_cannot_be_taken_again() {
    // The fixture's workdir is no Git worktree, so the second capture fails.
    let args =
        submit_declaring(|_| format!("content-v1:{}", "a".repeat(64)), false, |_| {}).unwrap();
    assert!(
        !args.iter().any(|arg| arg == "--source-fingerprint"),
        "{args:?}"
    );
}

#[test]
fn acceptance_submit_replays_an_open_write_unchanged_after_the_worktree_moved() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    make_workdir_a_worktree(&root);
    let workdir = root.to_string_lossy().into_owned();
    let (delegation, child) =
        install_evaluator_delegation(&state, &parent, Some(&project), &workdir, 2);
    let requested = content_revision(&root).unwrap().1;
    update_evaluation_target(&state, &delegation, |target| {
        target.source_fingerprint = Some(requested.clone());
    });
    // Both sends lose their response: the write stays open, its outcome
    // unknown.
    let (first_calls, run) = scripted_runner(vec![response_lost(), response_lost()]);
    let _ = state.submit_acceptance_evaluation_with_runner(&child, two_verdicts(), run);
    let sent = first_calls.lock().unwrap()[0].clone();
    assert!(sent.contains(&requested), "{sent:?}");

    // The worktree moves on. Only a receipt resolves the open write, so the
    // same verdicts resend its stored arguments rather than being refused.
    fs::write(root.join("judged.txt"), "changed after the send\n").unwrap();
    let (calls, run) = scripted_runner(vec![Ok(cli_output(true, &evaluate_receipt(true), ""))]);
    state
        .submit_acceptance_evaluation_with_runner(&child, two_verdicts(), run)
        .expect("an open write is replayed, not refused");
    assert_eq!(
        calls.lock().unwrap()[0],
        sent,
        "the stored arguments, unchanged"
    );
}

#[test]
fn acceptance_request_on_a_named_source_root_runs_and_measures_the_evaluator_there() {
    // The parent is bound to the claim that named a worktree as the work's
    // source root, so the evaluator runs there, its prompt names it, its
    // fingerprint is taken there and the target records the root.
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    make_workdir_a_worktree(&root);
    fs::write(
        root.join(".gitignore"),
        ".engram-project\nprojects/\nwork-read-args.txt\n.worktrees/\n",
    )
    .unwrap();
    run_git_test_command(&root, &["add", ".gitignore"]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "ignore worktrees"]);
    let worktree = root.join(".worktrees").join("wt");
    run_git_test_command(
        &root,
        &["worktree", "add", "--quiet", "-b", "wt", worktree.to_str().unwrap()],
    );
    fs::write(worktree.join("judged.txt"), "the work's own tree\n").unwrap();
    let store = established_store(&state, &project).expect("the store is established");
    let (named_root, common_dir_key) = validate_engram_source_root(
        &worktree.to_string_lossy(),
        &root.to_string_lossy(),
        &root.to_string_lossy(),
    )
    .expect("the worktree may be named");
    {
        let mut inner = state.inner.lock().unwrap();
        inner.engram_work_source_roots = vec![EngramWorkSourceRoot {
            store,
            work_id: "work-task".to_owned(),
            short_ref: "w-task".to_owned(),
            claim_id: "claim-task".to_owned(),
            claim_fence: 1,
            root: named_root.clone(),
            common_dir_key,
            named_by_session: parent.clone(),
            named_at: "2026-09-27T00:00:00.000Z".to_owned(),
            generation: 1,
        }];
        let index = inner.find_session_index(&parent).unwrap();
        inner.sessions[index].engram.work_binding = Some(EngramControlWorkBinding {
            root_execution_id: "root".to_owned(),
            work_id: "work-task".to_owned(),
            run_id: "run".to_owned(),
            work_revision: 1,
            claim_id: "claim-task".to_owned(),
            claim_fence: 1,
        });
    }
    super::delegation_support::install_delegation_codex_runtime(
        &state,
        "acceptance-evaluation-runtime",
    );
    let response = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(Some(Agent::Codex)),
            |connection, args, timeout| {
                if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items":[{"work_id":"work-task","short_ref":"w-task","claim_id":"claim-task","claim_fence":1}],"omitted":0}))
                } else {
                    fixture_reader(Arc::default(), show_receipt(None), Ok(policy_receipt(Some(&["independent_session"]))))(connection, args, timeout)
                }
            },
        )
        .unwrap();
    let AcceptanceEvaluationRequestResponse::Spawned {
        delegation, notice, ..
    } = response
    else {
        panic!("an independent evaluation spawns an evaluator");
    };
    let display_root = engram_source_root_display(&named_root);
    assert!(
        notice
            .as_deref()
            .is_some_and(|notice| notice.contains("named source root")),
        "{notice:?}"
    );
    let record = delegation.delegation;
    assert_eq!(record.cwd, display_root, "the evaluator runs in the named root");
    assert!(
        record.prompt.contains(&display_root),
        "the evaluator's brief names the root it reads"
    );
    let target = record
        .acceptance_evaluation
        .expect("an evaluator carries its target");
    assert_eq!(
        target.source_fingerprint,
        Some(content_revision(&worktree).unwrap().1),
        "the fingerprint is the named root's, not the parent's workdir's"
    );
    assert_ne!(
        target.source_fingerprint,
        Some(content_revision(&root).unwrap().1)
    );
    let source_root = target.source_root.expect("the target records the root");
    assert_eq!(source_root.root, named_root);
    assert_eq!(source_root.generation, 1);
}

#[test]
fn acceptance_first_write_is_refused_when_the_root_is_renamed_during_the_submit_capture() {
    // The submit-time capture runs off the lock; a rename landing there must
    // still stop the first write, which is admitted under the lock.
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let workdir = root.to_string_lossy().into_owned();
    let (delegation, child) =
        install_evaluator_delegation(&state, &parent, Some(&project), &workdir, 2);
    let store = established_store(&state, &project).expect("the fixture's store");
    let named = EngramWorkSourceRoot {
        store: store.clone(),
        work_id: "work-raced".to_owned(),
        short_ref: "w-raced".to_owned(),
        claim_id: "claim-raced".to_owned(),
        claim_fence: 1,
        root: root.join("wt").to_string_lossy().into_owned(),
        common_dir_key: "unused".to_owned(),
        named_by_session: parent.clone(),
        named_at: "2026-09-27T00:00:00.000Z".to_owned(),
        generation: 1,
    };
    state.inner.lock().unwrap().engram_work_source_roots = vec![named.clone()];
    update_evaluation_target(&state, &delegation, |target| {
        target.source_root = Some(AcceptanceEvaluationSourceRoot::from_entry(&named));
    });
    let request = two_verdicts();
    let (authority, _target, _model, _in_flight) = state
        .acceptance_evaluation_submit_context(&child, &request)
        .expect("the evaluator may submit");
    state
        .acceptance_evaluation_declared_target(&child, &authority.target)
        .expect("the root is still named when the capture starts");

    // Renamed while the capture ran.
    state.inner.lock().unwrap().engram_work_source_roots[0].generation = 2;
    let error = match state.begin_acceptance_evaluation_submission(
        &authority,
        AcceptanceEvaluationOpenWrite {
            verdicts_digest: acceptance_evaluation_payload_digest(
                &acceptance_evaluation_verdict_args(&request),
            ),
            args: vec!["work".to_owned(), "evaluate".to_owned()],
        },
    ) {
        Ok(_) => panic!("a write for a root the work no longer names is admitted"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("source root changed"), "{}", error.message);
    let inner = state.inner.lock().unwrap();
    let index = inner.find_delegation_index(&delegation).unwrap();
    assert!(
        matches!(
            inner.delegations[index]
                .acceptance_evaluation
                .as_ref()
                .unwrap()
                .submission,
            AcceptanceEvaluationSubmission::None
        ),
        "nothing was written"
    );
}

#[test]
fn acceptance_evaluator_creation_refuses_a_root_that_changed_during_the_request_capture() {
    // The request looks the root up, takes the fingerprint off the lock, then
    // creates the evaluator under its own lock: a rename, a clear or a first
    // name in between refuses the request there, in either direction.
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let store = established_store(&state, &project).expect("the store is established");
    let named = |generation| EngramWorkSourceRoot {
        store: store.clone(),
        work_id: "work-requested".to_owned(),
        short_ref: "w-requested".to_owned(),
        claim_id: "claim-requested".to_owned(),
        claim_fence: 1,
        root: root.join("wt").to_string_lossy().into_owned(),
        common_dir_key: "unused".to_owned(),
        named_by_session: parent.clone(),
        named_at: "2026-09-27T00:00:00.000Z".to_owned(),
        generation,
    };
    let seed = |source_root: Option<AcceptanceEvaluationSourceRoot>| AcceptanceEvaluationTargetSeed {
        work_ref: "w-requested".to_owned(),
        mode: AcceptanceEvaluationMode::IndependentSession,
        acceptance_basis: 1,
        evidence_basis: 1,
        criteria_count: 2,
        store: store.clone(),
        source_fingerprint: None,
        source_claim: source_root.is_none().then(|| AcceptanceEvaluationSourceClaim {work_id:"work-requested".to_owned(), claim_id:"claim-requested".to_owned()}),
        source_root,
        work_id: Some("work-requested".to_owned()),
    };
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&parent).unwrap();
    inner.sessions[index].engram.work_binding = Some(EngramControlWorkBinding {
        root_execution_id: "root".to_owned(),
        work_id: "work-requested".to_owned(),
        run_id: "run".to_owned(),
        work_revision: 1,
        claim_id: "claim-requested".to_owned(),
        claim_fence: 1,
    });
    let read_on_first = Some(AcceptanceEvaluationSourceRoot::from_entry(&named(1)));

    for (now, seeded, case) in [
        (vec![named(2)], read_on_first.clone(), "renamed"),
        (Vec::new(), read_on_first.clone(), "cleared"),
        (vec![named(1)], None, "named after the lookup"),
    ] {
        inner.engram_work_source_roots = now;
        let error = acceptance_evaluation_spawn_admission_locked(&inner, &parent, &seed(seeded))
            .expect_err(case);
        assert_eq!(error.status, StatusCode::CONFLICT, "{case}");
        assert!(
            error.message.contains("changed while this evaluation was being requested"),
            "{case}: {}",
            error.message
        );
    }
    inner.engram_work_source_roots = vec![named(1)];
    acceptance_evaluation_spawn_admission_locked(&inner, &parent, &seed(read_on_first))
        .expect("an unchanged root is admitted");
    inner.engram_work_source_roots = Vec::new();
    acceptance_evaluation_spawn_admission_locked(&inner, &parent, &seed(None))
        .expect("a workdir evaluation with no root named is admitted");

    // Requested with no root under the parent's claim; that claim gets its
    // first root while the parent binds another claim, or none: the claim
    // recorded at the request decides, as it does at the first submission.
    let on_claim = AcceptanceEvaluationTargetSeed {
        source_claim: Some(AcceptanceEvaluationSourceClaim {
            work_id: "work-requested".to_owned(),
            claim_id: "claim-requested".to_owned(),
        }),
        ..seed(None)
    };
    acceptance_evaluation_spawn_admission_locked(&inner, &parent, &on_claim)
        .expect("no root named for the recorded claim yet");
    inner.engram_work_source_roots = vec![named(1)];
    for (binding, case) in [(Some("claim-other"), "rebound"), (None, "unbound")] {
        inner.sessions[index].engram.work_binding = binding.map(|claim_id| EngramControlWorkBinding {
            root_execution_id: "root".to_owned(),
            work_id: "work-requested".to_owned(),
            run_id: "run".to_owned(),
            work_revision: 1,
            claim_id: claim_id.to_owned(),
            claim_fence: 1,
        });
        let error = acceptance_evaluation_spawn_admission_locked(&inner, &parent, &on_claim)
            .expect_err(case);
        assert!(
            error.message.contains("changed while this evaluation was being requested"),
            "{case}: {}",
            error.message
        );
    }
}

#[test]
fn acceptance_submission_is_refused_when_the_work_gets_its_first_root_while_evaluated() {
    // Requested with no root named, so evaluated in the workdir; the parent's
    // claim is then given a root, which moves the work's measurements away
    // from the tree the evaluator judged. Both submit checks refuse it.
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    let workdir = root.to_string_lossy().into_owned();
    let (delegation, child) =
        install_evaluator_delegation(&state, &parent, Some(&project), &workdir, 2);
    let store = established_store(&state, &project).expect("the fixture's store");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&parent).unwrap();
        inner.sessions[index].engram.work_binding = Some(EngramControlWorkBinding {
            root_execution_id: "root".to_owned(),
            work_id: "work-task".to_owned(),
            run_id: "run".to_owned(),
            work_revision: 1,
            claim_id: "claim-task".to_owned(),
            claim_fence: 1,
        });
    }
    // As the request records it: no root, the claim the parent was bound to.
    update_evaluation_target(&state, &delegation, |target| {
        target.source_claim = Some(AcceptanceEvaluationSourceClaim {
            work_id: "work-task".to_owned(),
            claim_id: "claim-task".to_owned(),
        });
    });
    let request = two_verdicts();
    let (authority, _target, _model, _in_flight) = state
        .acceptance_evaluation_submit_context(&child, &request)
        .expect("the evaluator may submit");
    assert_eq!(authority.target.source_root, None, "requested with no root named");
    state
        .acceptance_evaluation_declared_target(&child, &authority.target)
        .expect("with no root named yet the workdir still measures the work");

    // The first name lands while the evaluator's verdicts are on their way.
    state.inner.lock().unwrap().engram_work_source_roots = vec![EngramWorkSourceRoot {
        store,
        work_id: "work-task".to_owned(),
        short_ref: "w-task".to_owned(),
        claim_id: "claim-task".to_owned(),
        claim_fence: 1,
        root: root.join("wt").to_string_lossy().into_owned(),
        common_dir_key: "unused".to_owned(),
        named_by_session: parent.clone(),
        named_at: "2026-09-27T00:00:00.000Z".to_owned(),
        generation: 1,
    }];
    // And the parent moves on to another claim: the claim the evaluation was
    // requested under still decides, not the parent's binding now.
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&parent).unwrap();
        inner.sessions[index]
            .engram
            .work_binding
            .as_mut()
            .expect("bound")
            .claim_id = "claim-other".to_owned();
    }
    let before_capture = state
        .acceptance_evaluation_declared_target(&child, &authority.target)
        .expect_err("the check before the capture refuses a first name");
    assert!(
        before_capture.message.contains("has since been given a named source root"),
        "{}",
        before_capture.message
    );
    let at_first_write = match state.begin_acceptance_evaluation_submission(
        &authority,
        AcceptanceEvaluationOpenWrite {
            verdicts_digest: acceptance_evaluation_payload_digest(
                &acceptance_evaluation_verdict_args(&request),
            ),
            args: vec!["work".to_owned(), "evaluate".to_owned()],
        },
    ) {
        Ok(_) => panic!("a first write for a work now measured elsewhere is admitted"),
        Err(error) => error,
    };
    assert_eq!(at_first_write.status, StatusCode::CONFLICT);
    let inner = state.inner.lock().unwrap();
    let index = inner.find_delegation_index(&delegation).unwrap();
    assert!(matches!(
        inner.delegations[index]
            .acceptance_evaluation
            .as_ref()
            .unwrap()
            .submission,
        AcceptanceEvaluationSubmission::None
    ));
}


#[test]
fn acceptance_requested_claim_refuses_an_omitted_or_malformed_held_receipt() {
    for (receipt, status) in [
        (json!({"items": [], "omitted": 1}), StatusCode::CONFLICT),
        (
            json!({"items": [{"work_id": "work-task", "short_ref": "w-task", "claim_id": ""}]}),
            StatusCode::BAD_GATEWAY,
        ),
        (json!({"unexpected": []}), StatusCode::BAD_GATEWAY),
    ] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        let base = fixture_reader(
            Arc::default(),
            show_receipt(None),
            Ok(policy_receipt(Some(&["independent_session"]))),
        );
        let error = state
            .request_acceptance_evaluation_with_runner(
                &parent,
                evaluation_request(Some(Agent::Codex)),
                |connection, args, timeout| {
                    if args.iter().any(|arg| arg == "held") {
                        Ok(receipt.clone())
                    } else {
                        base(connection, args, timeout)
                    }
                },
            )
            .err()
            .expect("an unresolved requested claim never silently chooses the workdir");
        assert_eq!(error.status, status, "{}", error.message);
        assert!(error.message.contains("held"), "{}", error.message);
        assert!(
            state
                .inner
                .lock()
                .expect("state mutex poisoned")
                .delegations
                .is_empty(),
            "no evaluator is created on the wrong source basis"
        );
    }
}

#[test]
fn acceptance_requested_claim_refuses_missing_or_inconsistent_canonical_identity() {
    for (case, row) in [
        (
            "missing work id",
            json!({"short_ref": "w-task", "claim_id": "claim-task"}),
        ),
        (
            "empty work id",
            json!({"work_id": "", "short_ref": "w-task", "claim_id": "claim-task"}),
        ),
        (
            "wrong work id",
            json!({"work_id": "work-other", "short_ref": "w-task", "claim_id": "claim-task"}),
        ),
        (
            "wrong short ref",
            json!({"work_id": "work-task", "short_ref": "w-other", "claim_id": "claim-task"}),
        ),
    ] {
        let (state, project, parent, root) = fixture();
        install_store(&state, &project, &root);
        make_workdir_a_worktree(&root);
        super::delegation_support::install_delegation_codex_runtime(
            &state,
            "malformed-claim-runtime",
        );
        let mut show = show_receipt(None);
        show["status"]["work"]["work_id"] = json!("work-task");
        let base = fixture_reader(
            Arc::default(),
            show,
            Ok(policy_receipt(Some(&["independent_session"]))),
        );
        let response = state.request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(Some(Agent::Codex)),
            |connection, args, timeout| {
                if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items": [row], "omitted": 0}))
                } else {
                    base(connection, args, timeout)
                }
            },
        );
        let error = response.err().expect(case);
        assert_eq!(
            error.status,
            StatusCode::BAD_GATEWAY,
            "{case}: {}",
            error.message
        );
        assert!(error.message.contains("held"), "{case}: {}", error.message);
        assert!(
            state
                .inner
                .lock()
                .expect("state mutex poisoned")
                .delegations
                .is_empty(),
            "{case}: no evaluator is created with an unresolved source claim"
        );
    }
}
