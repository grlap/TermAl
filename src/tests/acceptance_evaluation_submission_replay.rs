//! Retained-write recovery through the parent HTTP route and a disposable CLI fixture.
use super::*;

#[tokio::test]
async fn replay_persistence_deadlines_preserve_the_write_and_recover_the_public_result() {
    for after_receipt in [false, true] {
        let (mut state, parent, delegation, _, original) = retained_fixture();
        state.select_test_engram_budget_clock(EngramBudgetClock::scripted());
        let mut writer = SteppedPersistWriter::attach(&mut state);
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let runner = move |_: &EngramConnectionConfig, args: &[String], _: Duration| {
            assert_eq!(args, original.args);
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(cli_output(true, &evaluate_receipt(true), ""))
        };
        let before = submission_of(&state, &delegation);
        let error = std::thread::scope(|scope| {
            let (replaying, owner, id, run) =
                (state.clone(), parent.clone(), delegation.clone(), &runner);
            let worker = scope.spawn(move || {
                replaying.replay_acceptance_evaluation_with_runner(
                    &owner,
                    ReplayAcceptanceEvaluationRequest { delegation_id: id },
                    run,
                )
            });
            writer.receive_fence();
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            if after_receipt {
                writer.write();
                writer.receive_fence();
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                let public = state.get_delegation_result(&parent, &delegation).unwrap();
                assert_eq!(
                    public.result.status,
                    DelegationStatus::Failed,
                    "receipt truth must wait for the durable ACK"
                );
            }
            assert!(matches!(
                writer.batch.pending[0]
                    .completion
                    .poll_at(writer.batch.pending[0].completion.deadline),
                Some(Err(PersistFenceError::Deadline))
            ));
            state.engram_budget_clock().notify();
            worker
                .join()
                .unwrap()
                .err()
                .expect("a writer deadline is pending")
        });
        assert_eq!(submission_of(&state, &delegation), before);
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(after_receipt));
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        let envelope: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(envelope["identity"]["delegationId"], delegation);
        assert!(
            durability_pending_for_request(
                StatusCode::SERVICE_UNAVAILABLE,
                "POST",
                &format!("/api/sessions/{parent}/acceptance-evaluation-replay"),
                &parent,
                &json!({"delegationId":delegation}),
                &envelope
            )
            .is_some()
        );
        writer.write();
        std::thread::scope(|scope| {
            let (replaying, owner, id, run) =
                (state.clone(), parent.clone(), delegation.clone(), &runner);
            let worker = scope.spawn(move || {
                replaying.replay_acceptance_evaluation_with_runner(
                    &owner,
                    ReplayAcceptanceEvaluationRequest { delegation_id: id },
                    run,
                )
            });
            writer.receive_fence();
            writer.write();
            writer.receive_fence();
            writer.write();
            assert!(matches!(
                worker.join().unwrap().unwrap(),
                AcceptanceEvaluationReplayResponse::Recorded { .. }
            ));
        });
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(after_receipt) + 1);
        let public = state.get_delegation_result(&parent, &delegation).unwrap();
        assert_eq!(public.result.status, DelegationStatus::Completed);
        assert!(public.result.findings.is_empty());
    }
}

#[test]
fn replay_preserves_a_real_transport_failure_beside_the_recorded_receipt() {
    let (state, parent, delegation, _, _) = retained_fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&delegation).unwrap();
        inner.delegations[index].status = DelegationStatus::Running;
        assert!(
            mark_delegation_failed_locked(
                &mut inner,
                index,
                "provider transport failed after the unknown submission"
            )
            .is_some()
        );
    }
    let before = serde_json::to_value(
        state
            .get_delegation_result(&parent, &delegation)
            .unwrap()
            .result,
    )
    .unwrap();
    let (_, runner) = scripted_runner(vec![Ok(cli_output(true, &evaluate_receipt(true), ""))]);
    state
        .replay_acceptance_evaluation_with_runner(
            &parent,
            ReplayAcceptanceEvaluationRequest {
                delegation_id: delegation.clone(),
            },
            runner,
        )
        .unwrap();
    let public = state.get_delegation_result(&parent, &delegation).unwrap();
    assert_eq!(public.result.status, DelegationStatus::Failed);
    assert_eq!(serde_json::to_value(public.result).unwrap(), before);
    assert!(outcome_of(&state, &delegation).is_some());
}

fn retained_fixture() -> (
    AppState,
    String,
    String,
    PathBuf,
    AcceptanceEvaluationOpenWrite,
) {
    let (state, parent, delegation, child) = evaluator_fixture();
    let root = {
        let inner = state.inner.lock().unwrap();
        PathBuf::from(
            &inner.sessions[inner.find_session_index(&parent).unwrap()]
                .session
                .workdir,
        )
    };
    let fixture = root.join(if cfg!(windows) {
        "replay.ps1"
    } else {
        "replay.sh"
    });
    if cfg!(windows) {
        fs::write(
            &fixture,
            r#"$fixtureHome = ''
for ($i = 0; $i -lt $args.Count; $i++) {
    if ($args[$i] -eq '--home') { $fixtureHome = $args[$i + 1] }
}
[IO.File]::WriteAllLines((Join-Path $fixtureHome 'replay-args.txt'), [string[]]$args)
[IO.File]::AppendAllText((Join-Path $fixtureHome 'replay-sends.txt'), "sent`n")
[Console]::WriteLine([IO.File]::ReadAllText((Join-Path $fixtureHome 'replay-receipt.json')))
"#,
        )
        .unwrap();
    } else {
        fs::write(
            &fixture,
            r#"#!/bin/sh
fixture_home=''
previous=''
for arg in "$@"; do
  if [ "$previous" = '--home' ]; then fixture_home="$arg"; fi
  previous="$arg"
done
printf '%s\n' "$@" > "$fixture_home/replay-args.txt"
printf 'sent\n' >> "$fixture_home/replay-sends.txt"
cat "$fixture_home/replay-receipt.json"
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&fixture, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    {
        let mut inner = state.inner.lock().unwrap();
        let project = inner.sessions[inner.find_session_index(&parent).unwrap()]
            .session
            .project_id
            .clone()
            .unwrap();
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram
            .as_mut()
            .unwrap()
            .binary_path = Some(fixture.to_string_lossy().into_owned());
    }
    fs::write(root.join("replay-receipt.json"), evaluate_receipt(true)).unwrap();
    let (_, run) = scripted_runner(vec![response_lost(), response_lost()]);
    state
        .submit_acceptance_evaluation_with_runner(&child, two_verdicts(), run)
        .unwrap_err();
    let AcceptanceEvaluationSubmission::Unconfirmed { original, .. } =
        submission_of(&state, &delegation)
    else {
        panic!("the fixture must begin with a retained unknown write");
    };
    super::super::delegation_support::finish_delegation_child_with_assistant_text(
        &state,
        &child,
        "## Result\n\nStatus: completed\n\nSummary:\nOriginal evaluator diagnostic remains available.",
    );
    let before = state.get_delegation_result(&parent, &delegation).unwrap();
    assert_eq!(before.result.status, DelegationStatus::Failed);
    assert_eq!(before.result.findings[0].severity, "Unavailable");
    (state, parent, delegation, root, original)
}

async fn replay(app: &Router, parent: &str, delegation: &str) -> axum::response::Response {
    request_response(
        app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{parent}/acceptance-evaluation-replay"
            ))
            .header("content-type", "application/json")
            .body(Body::from(json!({"delegationId": delegation}).to_string()))
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn parent_replays_the_ended_evaluators_original_write_and_recorded_repeats_do_nothing() {
    let (state, parent, delegation, root, original) = retained_fixture();
    state
        .inner
        .lock()
        .unwrap()
        .preferences
        .engram
        .developer_name = "renamed after submission".into();
    let app = app_router(state.clone());
    let response = replay(&app, &parent, &delegation).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "retained recovery must be available after the evaluator ends"
    );
    assert!(outcome_of(&state, &delegation).is_some_and(|(receipt, _)| receipt.replayed));
    let public = state.get_delegation_result(&parent, &delegation).unwrap();
    assert_eq!(public.result.status, DelegationStatus::Completed);
    assert!(public.result.summary.contains("1 of 2 criteria passed"));
    assert!(public.result.findings.is_empty());
    assert!(
        public
            .result
            .notes
            .iter()
            .any(|note| note.contains("Human-readable child output"))
    );
    let arguments: Vec<String> = fs::read_to_string(root.join("replay-args.txt"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert!(
        arguments
            .windows(original.args.len())
            .any(|words| words == original.args),
        "the fixture must receive every retained argument unchanged: {arguments:?}"
    );
    let before = submission_of(&state, &delegation);
    assert_eq!(
        replay(&app, &parent, &delegation).await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(submission_of(&state, &delegation), before);
    assert_eq!(
        fs::read_to_string(root.join("replay-sends.txt"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[tokio::test]
async fn replay_refuses_a_changed_payload_without_invoking_engram() {
    let (state, parent, delegation, root, _) = retained_fixture();
    update_evaluation_target(&state, &delegation, |target| {
        let AcceptanceEvaluationSubmission::Unconfirmed { original, .. } = &mut target.submission
        else {
            unreachable!()
        };
        original.args.push("altered".into());
    });
    let before = submission_of(&state, &delegation);
    assert_eq!(
        replay(&app_router(state.clone()), &parent, &delegation)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(submission_of(&state, &delegation), before);
    assert!(!root.join("replay-sends.txt").exists());
}

#[tokio::test]
async fn replay_refuses_an_overlapping_submission_without_invoking_engram() {
    let (state, parent, delegation, root, _) = retained_fixture();
    let guard = {
        let mut inner = state.inner.lock().unwrap();
        AcceptanceEvaluationSubmissionInFlight::admit_locked(&state, &mut inner, &delegation)
            .unwrap()
    };
    assert_eq!(
        replay(&app_router(state.clone()), &parent, &delegation)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert!(!root.join("replay-sends.txt").exists());
    drop(guard);
}

#[test]
fn another_timeout_keeps_the_original_write_and_attempt_key() {
    let (state, parent, delegation, _, original) = retained_fixture();
    let before = submission_of(&state, &delegation);
    let (calls, runner) = scripted_runner(vec![response_lost()]);
    let error = state
        .replay_acceptance_evaluation_with_runner(
            &parent,
            ReplayAcceptanceEvaluationRequest {
                delegation_id: delegation.clone(),
            },
            runner,
        )
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::BAD_GATEWAY);
    let calls = calls.lock().unwrap();
    assert_eq!(
        &*calls,
        &[original.args.clone()],
        "one unchanged resend, no automatic retry"
    );
    let AcceptanceEvaluationSubmission::Unconfirmed {
        payload_digest: before_digest,
        ..
    } = before
    else {
        unreachable!()
    };
    let AcceptanceEvaluationSubmission::Unconfirmed {
        payload_digest,
        original: after,
        ..
    } = submission_of(&state, &delegation)
    else {
        panic!("another timeout must leave the original write open");
    };
    assert_eq!(payload_digest, before_digest);
    assert_eq!(after, original);
    assert!(
        !state
            .inner
            .lock()
            .unwrap()
            .acceptance_evaluation_submissions_in_flight
            .contains_key(&delegation)
    );
}

#[test]
fn refused_resend_is_typed_and_does_not_erase_original_uncertainty() {
    let (state, parent, delegation, _, original) = retained_fixture();
    let (calls, runner) = scripted_runner(vec![revision_conflict()]);
    let answer = state
        .replay_acceptance_evaluation_with_runner(
            &parent,
            ReplayAcceptanceEvaluationRequest {
                delegation_id: delegation.clone(),
            },
            runner,
        )
        .unwrap();
    let AcceptanceEvaluationReplayResponse::Refused {
        code,
        original_outcome_unknown,
        attempt_key,
        ..
    } = answer
    else {
        panic!("a typed refusal is not a recorded evaluation");
    };
    assert_eq!(code.as_deref(), Some("work_revision_conflict"));
    assert!(original_outcome_unknown);
    assert!(
        original
            .args
            .windows(2)
            .any(|pair| pair[0] == "--attempt" && pair[1] == attempt_key)
    );
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(
        submission_of(&state, &delegation).open_write(),
        Some(&original)
    );
    assert!(outcome_of(&state, &delegation).is_none());
}

#[tokio::test]
async fn foreign_parent_and_child_cannot_replay_retained_verdicts() {
    for use_child in [false, true] {
        let (state, parent, delegation, root, _) = retained_fixture();
        let foreign = {
            let inner = state.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&parent).unwrap()]
                .session
                .project_id
                .clone()
                .unwrap()
        };
        let foreign = create_test_project_session(&state, Agent::Codex, &foreign, &root);
        let caller = {
            let inner = state.inner.lock().unwrap();
            if use_child {
                inner.delegations[inner.find_delegation_index(&delegation).unwrap()]
                    .child_session_id
                    .clone()
            } else {
                foreign
            }
        };
        let response = replay(&app_router(state.clone()), &caller, &delegation).await;
        assert!(response.status().is_client_error());
        assert!(!root.join("replay-sends.txt").exists());
        assert!(matches!(
            submission_of(&state, &delegation),
            AcceptanceEvaluationSubmission::Unconfirmed { .. }
        ));
    }
}

#[tokio::test]
async fn an_inflight_followup_blocks_replay_without_a_tracker_write() {
    let (state, parent, delegation, root, _) = retained_fixture();
    state
        .inner
        .lock()
        .unwrap()
        .delegation_followup_admissions
        .insert(delegation.clone(), FollowupAdmissionReservation::new(None));
    let before = submission_of(&state, &delegation);
    assert_eq!(
        replay(&app_router(state.clone()), &parent, &delegation)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(submission_of(&state, &delegation), before);
    assert!(!root.join("replay-sends.txt").exists());
}

#[tokio::test]
async fn a_running_evaluator_is_not_a_recovery_target() {
    let (state, parent, delegation, root, _) = retained_fixture();
    set_delegation_status(&state, &delegation, DelegationStatus::Running);
    assert_eq!(
        replay(&app_router(state.clone()), &parent, &delegation)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert!(!root.join("replay-sends.txt").exists());
}

#[test]
fn replay_holds_the_submission_guard_until_its_tracker_call_answers() {
    let (state, parent, delegation, _, _) = retained_fixture();
    let calls = std::cell::Cell::new(0);
    let answer = state
        .replay_acceptance_evaluation_with_runner(
            &parent,
            ReplayAcceptanceEvaluationRequest {
                delegation_id: delegation.clone(),
            },
            |_, _, _| {
                calls.set(calls.get() + 1);
                let nested = state
                    .replay_acceptance_evaluation_with_runner(
                        &parent,
                        ReplayAcceptanceEvaluationRequest {
                            delegation_id: delegation.clone(),
                        },
                        runner_must_not_run,
                    )
                    .err()
                    .unwrap();
                assert_eq!(nested.status, StatusCode::CONFLICT);
                assert!(nested.message.contains("acceptance_replay_in_progress"));
                Ok(cli_output(true, &evaluate_receipt(true), ""))
            },
        )
        .unwrap();
    assert!(matches!(
        answer,
        AcceptanceEvaluationReplayResponse::Recorded { .. }
    ));
    assert_eq!(calls.get(), 1);
}

#[test]
fn replay_keeps_exact_saved_source_bytes_until_the_receipt() {
    let (state, parent, delegation, _root, mut original) = retained_fixture();
    let mut evaluation = {
        let inner = state.inner.lock().unwrap();
        inner.delegations[inner.find_delegation_index(&delegation).unwrap()]
            .acceptance_evaluation
            .clone()
            .unwrap()
    };
    evaluation.source_fingerprint = None;
    evaluation.source_id = Some(shared_source::identity());
    let directory = engram_host_workdir(state.persistence_path.as_ref())
        .unwrap()
        .join("evaluation-sources");
    let (payload, file) = acceptance_source_candidate(&evaluation, true, &directory).unwrap();
    original.source_payload = payload;
    original
        .args
        .extend(["--source-id-file".into(), "<host-private-source-id>".into()]);
    let digest = original.digest().unwrap();
    let file = file.unwrap();
    let path = file.path().to_path_buf();
    let bytes = fs::read(&path).unwrap();
    drop(file);
    update_evaluation_target(&state, &delegation, |target| {
        target.submission = unconfirmed_with(&digest, "lost response", original.clone());
    });
    let calls = std::cell::Cell::new(0);
    let answer = state
        .replay_acceptance_evaluation_with_runner(
            &parent,
            ReplayAcceptanceEvaluationRequest {
                delegation_id: delegation.clone(),
            },
            |connection, args, _| {
                calls.set(calls.get() + 1);
                let index = args
                    .iter()
                    .position(|arg| arg == "--source-id-file")
                    .unwrap();
                assert_eq!(FsPath::new(&args[index + 1]), path);
                assert_eq!(fs::read(&args[index + 1]).unwrap(), bytes);
                assert_eq!(connection.session_id, original.args[4]);
                assert_eq!(connection.actor_id, original.args[2]);
                Ok(cli_output(true, &evaluate_receipt(true), ""))
            },
        )
        .unwrap();
    assert!(matches!(
        answer,
        AcceptanceEvaluationReplayResponse::Recorded { .. }
    ));
    assert_eq!(calls.get(), 1);
    assert!(
        !path.exists(),
        "the exact saved file is released only after a recorded receipt"
    );
}
