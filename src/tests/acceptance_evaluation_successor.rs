//! Requester changes must not redirect evaluation away from the task's tree.
use super::*;

pub(super) fn successor_fixture() -> (AppState, String, String, EngramWorkSourceRoot) {
    let (state, predecessor, _store, root) = named_root_parent();
    let (project, workdir, entry) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&predecessor).unwrap()];
        (
            record.session.project_id.clone().unwrap(),
            record.session.workdir.clone(),
            inner.engram_work_source_roots[0].clone(),
        )
    };
    let requester =
        create_test_project_session(&state, Agent::Codex, &project, FsPath::new(&workdir));
    assert_ne!(requester, predecessor);
    assert_ne!(
        fs::canonicalize(&workdir).unwrap(),
        fs::canonicalize(&root).unwrap()
    );
    // Match a completed naming publication, not only the original fixture's
    // naming-session cache. A different holder needs the acknowledged event.
    {
        let mut inner = state.inner.lock().unwrap();
        inner
            .engram_named_root_journal
            .push(EngramNamedRootJournal {
                store: entry.store.clone(),
                claim_id: entry.claim_id.clone(),
                read_binding: Some(named_parent_binding()),
                confirmed: Some((
                    EngramNamedRootEvent {
                        root: entry.clone(),
                        reporter: predecessor,
                        kind: EngramNamedRootKind::Bound,
                        end_reason: None,
                    },
                    EngramNamedRootReceipt {
                        event: "fixture-named-event".into(),
                        position: EngramNamedRootReadPosition {
                            feed: EngramNamedRootReadFeed {
                                kind: "run_execution".into(),
                                id: "run".into(),
                            },
                            position: 1,
                        },
                        workspace_id: root.clone(),
                        generation: 1,
                        kind: EngramNamedRootKind::Bound,
                    },
                )),
                pending: None,
                obsolete: false,
                retirement: None,
                reconciliation: None,
            });
    }
    run_git_test_command(FsPath::new(&root), &["add", "--", "judged.txt"]);
    run_git_test_command(
        FsPath::new(&root),
        &["commit", "--quiet", "-m", "Distinct judged tree"],
    );
    assert_ne!(
        run_git_test_command_output(FsPath::new(&root), &["rev-parse", "HEAD"]),
        run_git_test_command_output(FsPath::new(&workdir), &["rev-parse", "HEAD"])
    );
    let mut expected = super::super::shared_source::identity();
    expected.commit = run_git_test_command_output(FsPath::new(&root), &["rev-parse", "HEAD"])
        .trim()
        .into();
    expected.tree = run_git_test_command_output(FsPath::new(&root), &["rev-parse", "HEAD^{tree}"])
        .trim()
        .into();
    let canonical = fs::canonicalize(&root).unwrap();
    super::super::shared_source::install_owner(&state, &project, move |path| {
        if fs::canonicalize(path).ok().as_ref() != Some(&canonical) {
            return None;
        }
        Some(expected.clone())
    });
    (state, requester, root, entry)
}

fn assert_evaluator_tree(
    state: &AppState,
    response: AcceptanceEvaluationRequestResponse,
    root: &str,
    original: &EngramWorkSourceRoot,
) {
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response else {
        panic!("independent evaluator expected");
    };
    assert_eq!(
        delegation.delegation.cwd,
        resolve_session_workdir(root).unwrap()
    );
    let inner = state.inner.lock().unwrap();
    let target = inner.delegations[inner
        .find_delegation_index(&delegation.delegation.id)
        .unwrap()]
    .acceptance_evaluation
    .as_ref()
    .unwrap();
    let selected = target.source_root.as_ref().unwrap();
    assert_eq!(selected.root, root);
    assert_eq!(selected.generation, original.generation);
    assert!(target.source_id.is_some());
    assert_eq!(
        inner.engram_work_source_roots.as_slice(),
        &[original.clone()],
        "evaluation must not name or rewrite the retained root"
    );
}

#[test]
fn coordinator_without_claim_evaluates_the_retained_bound_tree() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (state, requester, root, original) = successor_fixture();
    install_sighting_transport(&state, &root, 1, present_sighting());
    let base = independent_reader();
    let response = state
        .request_acceptance_evaluation_with_runner(
            &requester,
            evaluation_request(Some(Agent::Codex)),
            |connection, args, timeout| {
                if args.iter().any(|arg| arg == "held") {
                    assert_eq!(connection.session_id, requester);
                    Ok(json!({"items":[], "omitted":0}))
                } else {
                    base(connection, args, timeout)
                }
            },
        )
        .expect("independent policy allows a coordinator to evaluate the bound task tree");
    assert_evaluator_tree(&state, response, &root, &original);
}

#[test]
fn handoff_and_recovery_holders_keep_the_bound_tree_without_renaming() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    // Engram keeps the claim id and binding for both accepted handoff and
    // another holder's recovery. Model their advanced fence in the held reply.
    for fence in [2, 3] {
        let (state, requester, root, original) = successor_fixture();
        install_sighting_transport(&state, &root, 1, present_sighting());
        let base = independent_reader();
        let response = state
            .request_acceptance_evaluation_with_runner(
                &requester,
                evaluation_request(Some(Agent::Codex)),
                |connection, args, timeout| {
                    if args.iter().any(|arg| arg == "held") {
                        assert_eq!(connection.session_id, requester);
                        Ok(
                            json!({"items":[{"work_id":SIGHTED_WORK_ID,"short_ref":"w-task",
                        "claim_id":original.claim_id,"claim_fence":fence}],"omitted":0}),
                        )
                    } else {
                        base(connection, args, timeout)
                    }
                },
            )
            .expect("a successor holding the same claim needs no new name");
        assert_evaluator_tree(&state, response, &root, &original);
    }
}

#[test]
fn coordinator_cannot_bypass_canonical_root_fences() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for case in ["unbound", "generation", "path", "moved"] {
        let (state, requester, root, _original) = successor_fixture();
        let mut answer = sighting_reply(&root, 1, present_sighting());
        match case {
            "unbound" => answer["root"] = json!({"state":"none"}),
            "generation" => answer["root"]["generation"] = json!(2),
            "path" => answer["root"]["workspace_id"] = json!("C:/elsewhere/unrelated"),
            "moved" => answer["binding_changed"] = json!(true),
            _ => unreachable!(),
        }
        install_sighting_answer(&state, Ok(answer));
        let base = independent_reader();
        let response = state.request_acceptance_evaluation_with_runner(
            &requester,
            evaluation_request(Some(Agent::Codex)),
            |connection, args, timeout| {
                if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items":[],"omitted":0}))
                } else {
                    base(connection, args, timeout)
                }
            },
        );
        let error = refusal(&state, response, case);
        assert_eq!(error.status, StatusCode::CONFLICT, "{case}: {error:?}");
    }
}

#[test]
fn coordinator_refuses_a_missing_root_or_unavailable_owner_capture() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for case in ["missing-root", "unavailable-owner"] {
        let (state, requester, root, original) = successor_fixture();
        install_sighting_transport(&state, &root, 1, present_sighting());
        if case == "missing-root" {
            fs::rename(&root, format!("{root}-moved")).unwrap();
        } else {
            let project = {
                let inner = state.inner.lock().unwrap();
                inner.sessions[inner.find_session_index(&requester).unwrap()]
                    .session
                    .project_id
                    .clone()
                    .unwrap()
            };
            super::super::shared_source::install_owner(&state, &project, |_| None);
        }
        let base = independent_reader();
        let response = state.request_acceptance_evaluation_with_runner(
            &requester,
            evaluation_request(Some(Agent::Codex)),
            |connection, args, timeout| {
                if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items":[],"omitted":0}))
                } else {
                    base(connection, args, timeout)
                }
            },
        );
        let error = refusal(&state, response, case);
        assert_eq!(error.status, StatusCode::CONFLICT, "{case}: {error:?}");
        assert!(
            error.message.contains("could not be captured"),
            "{case}: {error:?}"
        );
        assert_eq!(
            state
                .inner
                .lock()
                .unwrap()
                .engram_work_source_roots
                .as_slice(),
            &[original]
        );
    }
}

#[test]
fn another_holder_cannot_use_a_name_without_its_acknowledged_journal() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for held in [true, false] {
        let (state, requester, root, _original) = successor_fixture();
        state
            .inner
            .lock()
            .unwrap()
            .engram_named_root_journal
            .clear();
        install_sighting_transport(&state, &root, 1, present_sighting());
        let base = independent_reader();
        let response = state.request_acceptance_evaluation_with_runner(
            &requester,
            evaluation_request(Some(Agent::Codex)),
            |connection, args, timeout| {
                if !held && args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items":[],"omitted":0}))
                } else {
                    base(connection, args, timeout)
                }
            },
        );
        let error = refusal(&state, response, "missing naming receipt");
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(
            error.message.contains("binding is unconfirmed"),
            "held={held}: {error:?}"
        );
    }
}

#[test]
fn unheld_same_session_refuses_unavailable_capture_or_missing_journal() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for case in ["capture", "journal"] {
        let (state, requester, root, _original) = successor_fixture();
        if case == "journal" {
            state
                .inner
                .lock()
                .unwrap()
                .engram_named_root_journal
                .clear();
        } else {
            let project = {
                let inner = state.inner.lock().unwrap();
                inner.sessions[inner.find_session_index(&requester).unwrap()]
                    .session
                    .project_id
                    .clone()
                    .unwrap()
            };
            super::super::shared_source::install_owner(&state, &project, |_| None);
        }
        install_sighting_transport(&state, &root, 1, present_sighting());
        let base = reader_admitting("same_session");
        let response = state.request_acceptance_evaluation_with_runner(
            &requester,
            evaluation_request(None),
            |connection, args, timeout| {
                if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items":[],"omitted":0}))
                } else {
                    base(connection, args, timeout)
                }
            },
        );
        let error = refusal(&state, response, case);
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(
            error.message.contains(if case == "capture" {
                "could not be captured"
            } else {
                "binding is unconfirmed"
            }),
            "{case}: {error:?}"
        );
    }
}
