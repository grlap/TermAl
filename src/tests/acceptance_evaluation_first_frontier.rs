// Owns the end-to-end regression for an evaluation requested on a named
// source root whose naming the work's canonical history had not yet learned:
// the evaluator's own turn admission then reads the run and learns that very
// binding's Bound event, moving the history once. The evaluation's first
// write must still be admitted. Before the fix it was refused as if the work
// no longer named the evaluated root. Written only against interfaces the
// pre-fix tree has, so this file alone is the evaluation side's test-only
// fail-first witness.
// Does not own the paused-evaluation cases and the real invalidations
// (acceptance_evaluation_unchanged_binding.rs) or the request's other checks
// (acceptance_evaluation_source.rs).
// New module.
use super::*;

/// A canonical read of the fixture run, as Engram answers it: `bound` says
/// whether it reports the root's naming (its Bound event at position 1).
fn named_root_read(
    store: &EngramAuthorityStoreKey,
    binding: &EngramControlWorkBinding,
    named_root: &str,
    bound: bool,
) -> EngramNamedRootReadResponse {
    let feed = json!({"kind":"run_execution","id":binding.run_id});
    let (named, latest, cut) = if bound {
        (
            json!({"state":"bound","workspace_id":named_root,"generation":1,"named_at":"2026-09-27T00:00:00Z"}),
            json!({"event":"fixture-named-event","position":{"feed":feed,"position":1},
                "kind":"bound","workspace_id":named_root,"generation":1,"named_at":"2026-09-27T00:00:00Z"}),
            1,
        )
    } else {
        (json!({"state":"none"}), Value::Null, 0)
    };
    serde_json::from_value(json!({
        "project_id":store.project_id,"work_id":binding.work_id,"run_id":binding.run_id,
        "claim_id":binding.claim_id,"root_execution_id":binding.root_execution_id,
        "run":{"state":"open","generation":2},
        "named_root":named,
        "latest_event":latest,
        "read_cut":{"feed":feed,"position":cut}
    }))
    .unwrap()
}

/// One canonical read learned and published, as a turn admission does it.
fn learn_read(
    state: &AppState,
    store: &EngramAuthorityStoreKey,
    binding: &EngramControlWorkBinding,
    read: &EngramNamedRootReadResponse,
) {
    let owner = state
        .prepare_engram_authority(store, binding, Duration::from_secs(2))
        .unwrap();
    AppState::learn_engram_authority_locked(&mut state.inner.lock().unwrap(), store, &owner, read)
        .unwrap();
    state
        .publish_engram_authority(store, &owner, Duration::from_secs(2))
        .unwrap();
}

fn history_revision(state: &AppState) -> u64 {
    let inner = state.inner.lock().unwrap();
    assert_eq!(inner.engram_work_naming_history.len(), 1, "one fixture work");
    inner.engram_work_naming_history[0].revision
}

#[test]
fn an_evaluation_whose_admission_learns_the_named_binding_is_still_admitted() {
    let (state, project, parent, root) = fixture();
    install_store(&state, &project, &root);
    super::source::make_workdir_a_worktree(&root);
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
    let binding = EngramControlWorkBinding {
        root_execution_id: "root".to_owned(),
        work_id: "01a0b6c5-4b4f-7b41-9e32-edc597077acf".to_owned(),
        run_id: "run".to_owned(),
        work_revision: 1,
        claim_id: "claim-task".to_owned(),
        claim_fence: 1,
    };
    {
        // The claim named the worktree: TermAl acknowledged the naming, and
        // the session is bound to the claim.
        let mut inner = state.inner.lock().unwrap();
        *inner.engram_work_source_roots = vec![EngramWorkSourceRoot {
            store: store.clone(),
            work_id: binding.work_id.clone(),
            short_ref: "w-task".to_owned(),
            claim_id: binding.claim_id.clone(),
            claim_fence: 1,
            root: named_root.clone(),
            common_dir_key,
            named_by_session: parent.clone(),
            named_at: "2026-09-27T00:00:00.000Z".to_owned(),
            generation: 1,
        }];
        let index = inner.find_session_index(&parent).unwrap();
        inner.sessions[index].engram.named_root = Some(EngramNamedRootState::Bound {
            workspace_id: named_root.clone(),
            generation: 1,
            named_at: "2026-09-27T00:00:00.000Z".to_owned(),
        });
        inner.sessions[index].engram.work_binding = Some(binding.clone());
    }
    // The last canonical read of the run before the request predates the
    // naming's Bound event: the history has not learned it yet.
    learn_read(&state, &store, &binding, &named_root_read(&store, &binding, &named_root, false));
    super::delegation_support::install_delegation_codex_runtime(
        &state,
        "acceptance-evaluation-runtime",
    );
    super::sighting::install_sighting_transport(
        &state,
        &named_root,
        1,
        super::sighting::present_sighting(),
    );
    let response = state
        .request_acceptance_evaluation_with_runner(
            &parent,
            evaluation_request(Some(Agent::Codex)),
            |connection, args, timeout| {
                if args.iter().any(|arg| arg == "held") {
                    Ok(json!({"items":[{"work_id":"01a0b6c5-4b4f-7b41-9e32-edc597077acf","short_ref":"w-task","claim_id":"claim-task","claim_fence":1}],"omitted":0}))
                } else {
                    fixture_reader(Arc::default(), show_receipt(None), Ok(policy_receipt(Some(&["independent_session"]))))(connection, args, timeout)
                }
            },
        )
        .expect("the evaluation is requested on the named root");
    let AcceptanceEvaluationRequestResponse::Spawned { delegation, .. } = response else {
        panic!("an independent evaluation spawns an evaluator");
    };
    let child = delegation.delegation.child_session_id.clone();
    // The evaluator echoes the attempt key its host-authored brief names.
    let attempt_key = delegation
        .delegation
        .acceptance_evaluation
        .as_ref()
        .expect("an evaluator carries its target")
        .attempt_key
        .clone();
    let requested_at = history_revision(&state);

    // The evaluator's own turn admission reads the run and learns the naming
    // the evaluation was requested on: the same binding's Bound event.
    learn_read(&state, &store, &binding, &named_root_read(&store, &binding, &named_root, true));
    assert_eq!(
        history_revision(&state),
        requested_at + 1,
        "learning the binding's Bound event moved the history once"
    );
    assert_eq!(
        state.inner.lock().unwrap().engram_work_source_roots[0].root,
        named_root,
        "the root is still named, under the same claim and generation"
    );

    let mut request = two_verdicts();
    request.attempt_key = Some(attempt_key);
    let (authority, _target, _model, _in_flight) = state
        .acceptance_evaluation_submit_context(&child, &request)
        .expect("the evaluator may submit");
    state
        .begin_acceptance_evaluation_submission(
            &authority,
            AcceptanceEvaluationOpenWrite {
                verdicts_digest: acceptance_evaluation_payload_digest(
                    &acceptance_evaluation_verdict_args(&request),
                ),
                args: vec!["work".to_owned(), "evaluate".to_owned()],
            },
        )
        .unwrap_or_else(|error| {
            panic!(
                "learning the binding an evaluation was requested on must not refuse its first \
                 write ({}): {}",
                error.status, error.message
            )
        });
}
