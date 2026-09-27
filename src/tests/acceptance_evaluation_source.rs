// Tests of the declared source fingerprint of an acceptance evaluation
// (tm-5gi4): the request takes the content revision of the evaluator's
// worktree, a same-session caller is handed it, the record keeps it across a
// reload, and the first submission declares it only while the worktree still
// holds it. Does not own the rest of the acceptance-evaluation tests, the
// fixtures and helpers of which it uses from its parent module. New tests,
// written for tm-5gi4 and placed in this child module of
// src/tests/acceptance_evaluation.rs so that file stays within the size the
// architecture lens allows for a test file.
use super::*;

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
