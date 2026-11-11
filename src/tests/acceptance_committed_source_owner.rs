// Acceptance's clean-commit policy consumes the pinned owner's verdict on
// the named root. Script only the subprocess boundary; the real metadata,
// executable custody, strict parser and authority revalidation still run.
use super::*;
use crate::tests::engram_source_owner::{source_owner_output, source_owner_reply};

fn owner_deadline() -> std::time::Instant {
    std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET
}

#[test]
fn committed_preflight_keeps_long_host_deadlines_and_issues_owner_compatible_arguments() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (state, parent, _store, named_root) = named_root_parent();
    let head = commit_named_root(&named_root);
    acceptance_owner_reports(&state, &parent, &named_root, Vec::new(), Vec::new());
    let transport = install_sighting_transport(&state, &named_root, 1, json!({"state":"absent"}));
    let registration = register_committed_run(
        &state,
        &parent,
        &named_root,
        committed_results("passed", input_record(&head, "clean")),
    );
    host_observed(&state, &registration, &head, acknowledged("received"));
    let capture = state
        .inner
        .lock()
        .unwrap()
        .test_acceptance_source_owner_capture
        .clone()
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let deadline = std::time::Instant::now() + acceptance_evaluation_request_tracker_budget();
    state
        .inner
        .lock()
        .unwrap()
        .test_acceptance_source_owner_capture =
        Some(Arc::new(move |binary, cwd, args, until, limit| {
            assert_eq!(
                until, deadline,
                "an early capture preserves the existing longer deadline"
            );
            assert!(
                !args.iter().any(|arg| arg.starts_with("--timeout-ms")),
                "the actual committed-input preflight must work with either owner's default"
            );
            count.fetch_add(1, Ordering::SeqCst);
            capture(binary, cwd, args, until, limit)
        }));
    assert_eq!(
        decide_by(&state, &parent, deadline),
        Ok(AcceptanceCommittedInputDecision::Admitted(
            AcceptanceCommittedInputAdmission {
                native_source: None,
                run_id: COMMITTED_RUN_ID.to_owned(),
                commit: head,
            }
        ))
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "initial clean observation and final revalidation, no source fallback or resend"
    );
    assert_eq!(
        sighting_reads(&transport),
        0,
        "committed admission does not fall back to sighting"
    );
}

#[test]
fn acceptance_uses_the_owner_verdict_without_a_local_git_predicate() {
    let (state, parent, _store, named_root) = named_root_parent();
    let head = commit_named_root(&named_root);
    acceptance_owner_reports(&state, &parent, &named_root, Vec::new(), Vec::new());
    // Deliberately leave the scripted owner's verdict clean. This is a
    // consumer test: independent local Git judgment would contradict it.
    fs::write(
        worktree(&named_root).join("judged.txt"),
        "local input changed\n",
    )
    .unwrap();
    assert_eq!(
        state.acceptance_judged_commit(&parent, FsPath::new(&named_root), owner_deadline()),
        Some(head)
    );
    run_git_test_command(&worktree(&named_root), &["checkout", "--", "judged.txt"]);
    // Conversely a clean local checkout cannot override the owner's dirt.
    acceptance_owner_reports_dirty(&state, &parent, &named_root, "judged.txt");
    assert_eq!(
        state.acceptance_judged_commit(&parent, FsPath::new(&named_root), owner_deadline()),
        None
    );
}

#[test]
fn acceptance_owner_failures_never_fall_back_to_a_clean_local_checkout() {
    for case in [
        "unavailable",
        "wrong root",
        "malformed",
        "stderr",
        "process failure",
        "missing identity",
        "binary replaced",
    ] {
        let (state, parent, _store, named_root) = named_root_parent();
        commit_named_root(&named_root);
        acceptance_owner_reports(&state, &parent, &named_root, Vec::new(), Vec::new());
        let original = state
            .inner
            .lock()
            .unwrap()
            .test_acceptance_source_owner_capture
            .clone()
            .unwrap();
        let wrong_root = {
            let inner = state.inner.lock().unwrap();
            engram_project_for_session_locked(&inner, &parent)
                .unwrap()
                .root_path
                .clone()
        };
        state
            .inner
            .lock()
            .unwrap()
            .test_acceptance_source_owner_capture =
            Some(Arc::new(move |binary, cwd, args, until, limit| {
                let mut reply = original(binary, cwd, args, until, limit)?;
                match case {
                    "unavailable" => return Ok(source_owner_reply(args)),
                    "malformed" => reply.stdout = b"{".to_vec(),
                    "stderr" => reply.stderr = b"source failure".to_vec(),
                    "process failure" => return Ok(source_owner_output(7, reply.stdout)),
                    "binary replaced" => fs::write(binary, b"replacement executable").unwrap(),
                    _ => {
                        let mut value: Value = serde_json::from_slice(&reply.stdout).unwrap();
                        if case == "wrong root" {
                            value["root"] = json!(engram_exact_path_key(FsPath::new(&wrong_root)));
                        } else {
                            value["source_id"] = Value::Null;
                        }
                        reply.stdout = serde_json::to_vec(&value).unwrap();
                    }
                }
                Ok(reply)
            }));
        assert_eq!(
            state.acceptance_judged_commit(&parent, FsPath::new(&named_root), owner_deadline()),
            None,
            "{case}"
        );
    }
}

#[test]
fn acceptance_owner_configuration_and_deadline_are_revalidated() {
    let (state, parent, _store, named_root) = named_root_parent();
    commit_named_root(&named_root);
    acceptance_owner_reports(&state, &parent, &named_root, Vec::new(), Vec::new());
    let original = state
        .inner
        .lock()
        .unwrap()
        .test_acceptance_source_owner_capture
        .clone()
        .unwrap();
    let changed = state.clone();
    let owner = parent.clone();
    state
        .inner
        .lock()
        .unwrap()
        .test_acceptance_source_owner_capture =
        Some(Arc::new(move |binary, cwd, args, until, limit| {
            let reply = original(binary, cwd, args, until, limit)?;
            let mut inner = changed.inner.lock().unwrap();
            let project = engram_project_for_session_locked(&inner, &owner)
                .unwrap()
                .id
                .clone();
            inner
                .projects
                .iter_mut()
                .find(|row| row.id == project)
                .unwrap()
                .engram
                .as_mut()
                .unwrap()
                .binary_path = None;
            Ok(reply)
        }));
    assert_eq!(
        state.acceptance_judged_commit(&parent, FsPath::new(&named_root), owner_deadline()),
        None
    );
    // Neither a missing configured owner nor an expired aggregate deadline
    // may cause a local clean check or an alternative executable lookup.
    assert_eq!(
        state.acceptance_judged_commit(&parent, FsPath::new(&named_root), owner_deadline()),
        None
    );
    acceptance_owner_reports(&state, &parent, &named_root, Vec::new(), Vec::new());
    state
        .inner
        .lock()
        .unwrap()
        .test_acceptance_source_owner_capture = Some(Arc::new(|_, _, _, _, _| {
        panic!("expired request must not reach capture")
    }));
    assert_eq!(
        state.acceptance_judged_commit(
            &parent,
            FsPath::new(&named_root),
            std::time::Instant::now()
        ),
        None
    );
}
