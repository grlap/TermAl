// A queued evaluation host record may land after its caller's deadline.
use super::*;

fn rejected_evaluation_key_has_no_effect(schema_one: bool) {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (state, mut writer, delegation, child) = evaluator_fixture_with_stepped_writer();
    {
        let inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&delegation).unwrap();
        assert!(
            inner.delegations[index]
                .acceptance_evaluation
                .as_ref()
                .unwrap()
                .attempt_history
                .is_none()
        );
    }
    if schema_one {
        let (parent, cwd) = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.delegations[inner.find_delegation_index(&delegation).unwrap()];
            (record.parent_session_id.clone(), record.cwd.clone())
        };
        update_evaluation_target(&state, &delegation, |target| {
            target.attempt_key = acceptance_evaluation_ordinal_key(&delegation, 1);
            target.attempt_history = Some(AcceptanceEvaluationAttemptHistory {
                schema_version: 1,
                parent_session_id: parent,
                child_session_id: child.clone(),
                cwd,
                identity: acceptance_core_identity(&evidence_selection::canonical_core_receipt())
                    .unwrap()
                    .unwrap(),
                ordinal: 1,
                requester_text_tainted: false,
                identity_refused: false,
                previous: Vec::new(),
                refusal: None,
                prepared_brief: None,
                citation_retry: None,
            });
        });
    }
    writer.write();
    let before = submission_of(&state, &delegation);
    let durable_before = durable_submission(&writer.path, &delegation);
    let mut request = two_verdicts();
    if !schema_one {
        request.attempt_key = Some("wrong-legacy-attempt".to_owned());
    }
    let submitting = state.clone();
    let worker = std::thread::spawn(move || {
        submitting.submit_acceptance_evaluation_with_runner(&child, request, runner_must_not_run)
    });
    // Drive any incorrectly admitted fence to a terminal failure, so the
    // regression fails on admission rather than leaving a blocked worker.
    let started = std::time::Instant::now();
    let mut fences = 0;
    while !worker.is_finished() {
        assert!(started.elapsed() < phase_sync::DEADLOCK_GUARD);
        match writer.rx.recv_timeout(Duration::from_millis(20)) {
            Ok(PersistRequest::Fence(fence)) => {
                fences += 1;
                fence.finish(Err(PersistFenceError::WriteFailed(
                    "unexpected legacy-key admission".to_owned(),
                )));
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => panic!("fixture writer disconnected: {error}"),
        }
    }
    for request in writer.rx.try_iter() {
        if matches!(request, PersistRequest::Fence(_)) {
            fences += 1;
        }
    }
    let error = worker
        .join()
        .unwrap()
        .expect_err("a wrong key is not admitted");
    assert_eq!(
        fences, 0,
        "reject the supplied key before ANY fence enqueue"
    );
    assert_eq!(error.status, StatusCode::CONFLICT, "{}", error.message);
    assert!(error.message.contains("attemptKey"));
    assert!(error.durability_deadline.is_none());
    assert_eq!(submission_of(&state, &delegation), before);
    assert_eq!(
        durable_submission(&writer.path, &delegation),
        durable_before
    );
}

#[test]
fn durability_pending_review_legacy_wrong_key_refuses_before_enqueue() {
    rejected_evaluation_key_has_no_effect(false);
}

#[test]
fn durability_pending_review_schema_one_missing_key_refuses_before_enqueue() {
    rejected_evaluation_key_has_no_effect(true);
}

#[tokio::test]
async fn durability_pending_review_legacy_omitted_key_keeps_stored_identity() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (state, mut writer, delegation, child) = evaluator_fixture_with_stepped_writer();
    let request = two_verdicts();
    assert!(request.attempt_key.is_none());
    let submitting = state.clone();
    let evaluator = child.clone();
    let worker = std::thread::spawn(move || {
        submitting.submit_acceptance_evaluation_with_runner(
            &evaluator,
            request,
            runner_must_not_run,
        )
    });
    writer.receive_fence();
    let pending = submission_of(&state, &delegation);
    assert!(matches!(
        pending,
        AcceptanceEvaluationSubmission::Pending { .. }
    ));
    let late = collect_persist_delta_from_shared_state(&state.inner, writer.watermark);
    assert!(matches!(
        writer.batch.pending[0]
            .completion
            .poll_at(writer.batch.pending[0].completion.deadline),
        Some(Err(PersistFenceError::Deadline))
    ));
    state.engram_budget_clock().notify();
    let error = worker
        .join()
        .unwrap()
        .expect_err("pending is not a receipt");
    let response = error.into_response();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let envelope: Value = serde_json::from_slice(&bytes).unwrap();
    persist_delta_with_fences(&mut writer.cache, &writer.path, &late, &mut writer.batch).unwrap();
    assert_eq!(durable_submission(&writer.path, &delegation), Some(pending));
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(envelope["identity"]["attemptKey"], delegation);
    assert!(
        durability_pending_for_request(
            status,
            "POST",
            &format!("/api/sessions/{child}/acceptance-evaluation"),
            &child,
            &json!({"schemaVersion": 1}),
            &envelope
        )
        .is_some()
    );
    assert!(outcome_of(&state, &delegation).is_none());
}

fn reloaded_pending_evaluator(state: &AppState) -> AppState {
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let mut restarted = test_app_state();
    restarted.persistence_path = state.persistence_path.clone();
    restarted.inner = Arc::new(StateMutex::new(loaded));
    restarted.select_test_engram_budget_clock(EngramBudgetClock::scripted());
    restarted.install_test_engram_transport(ScriptedEngramControlTransport::new([]));
    assert!(!Arc::ptr_eq(&state.inner, &restarted.inner));
    assert!(
        restarted
            .inner
            .lock()
            .unwrap()
            .acceptance_evaluation_submissions_in_flight
            .is_empty()
    );
    restarted
}

#[tokio::test]
async fn durability_pending_review_evaluation_recorded_deadline_recovers_same_receipt() {
    let _temp_scope = TestTempRootDirectoryScope::repository_local();
    let (state, mut writer, delegation, child) = evaluator_fixture_with_stepped_writer();
    let mut request = two_verdicts();
    request.attempt_key = Some(delegation.clone());
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let run = move |_: &EngramConnectionConfig, _: &[String], _: Duration| {
        let replayed = seen.fetch_add(1, Ordering::SeqCst) > 0;
        Ok(cli_output(true, &evaluate_receipt(replayed), ""))
    };
    let (error, late) = std::thread::scope(|scope| {
        let (submitting, evaluator, original, runner) =
            (state.clone(), child.clone(), request.clone(), &run);
        let worker = scope.spawn(move || {
            submitting.submit_acceptance_evaluation_with_runner(&evaluator, original, runner)
        });
        writer.receive_fence();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        writer.write();
        writer.receive_fence();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            !worker.is_finished(),
            "a tracker receipt is not a durable host ACK"
        );
        assert!(matches!(
            submission_of(&state, &delegation),
            AcceptanceEvaluationSubmission::Recorded { .. }
        ));
        let late = collect_persist_delta_from_shared_state(&state.inner, writer.watermark);
        assert!(matches!(
            writer.batch.pending[0]
                .completion
                .poll_at(writer.batch.pending[0].completion.deadline),
            Some(Err(PersistFenceError::Deadline))
        ));
        let error = worker
            .join()
            .unwrap()
            .expect_err("a recorded-row deadline is not success");
        (error, late)
    });
    assert!(
        outcome_of(&state, &delegation).is_none(),
        "the unacknowledged receipt is not published"
    );
    persist_delta_with_fences(&mut writer.cache, &writer.path, &late, &mut writer.batch).unwrap();
    writer.watermark = late.watermark;
    assert!(matches!(
        durable_submission(&writer.path, &delegation),
        Some(AcceptanceEvaluationSubmission::Recorded { .. })
    ));
    let response = error.into_response();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let pending: Value = serde_json::from_slice(&bytes).unwrap();

    // A fresh loader sees the actual committed Recorded row, not an old
    // waiter ACK. It refuses another write under that completed attempt and
    // exposes the same durable receipt through the delegation outcome.
    let restarted = reloaded_pending_evaluator(&state);
    let error = restarted
        .submit_acceptance_evaluation_with_runner(&child, request.clone(), runner_must_not_run)
        .expect_err("a recorded attempt after reload is not a new submission");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error.message.contains("active local read-only evaluator")
            || error.message == ACCEPTANCE_EVALUATION_ALREADY_RECORDED_ERROR,
        "{}",
        error.message
    );
    assert!(error.durability_deadline.is_none());
    assert_eq!(
        submission_of(&restarted, &delegation),
        durable_submission(&writer.path, &delegation).unwrap()
    );
    assert!(outcome_of(&restarted, &delegation).is_some());

    let recorded = std::thread::scope(|scope| {
        let (submitting, evaluator, runner) = (state.clone(), child.clone(), &run);
        let worker = scope.spawn(move || {
            submitting.submit_acceptance_evaluation_with_runner(&evaluator, request, runner)
        });
        writer.receive_fence();
        writer.write();
        writer.receive_fence();
        assert!(!worker.is_finished());
        writer.write();
        worker.join().unwrap().unwrap()
    });
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "one write and one explicit identical tracker replay"
    );
    assert_eq!(recorded.attempt_key, delegation);
    assert_eq!(recorded.receipt["evaluation"]["replayed"], true);
    assert!(outcome_of(&state, &delegation).is_some());
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{pending}");
    assert_eq!(pending["contract"], "termal-durability-pending");
    assert_eq!(pending["version"], 1);
    assert_eq!(pending["operation"], "acceptance_evaluation_submission");
    assert_eq!(pending["identity"]["delegationId"], delegation);
    assert_eq!(pending["identity"]["attemptKey"], recorded.attempt_key);
    assert_eq!(pending["identity"]["evaluatorSessionId"], child);
    assert!(
        durability_pending_for_request(
            status,
            "POST",
            &format!("/api/sessions/{child}/acceptance-evaluation"),
            &child,
            &json!({"attemptKey": recorded.attempt_key}),
            &pending
        )
        .is_some()
    );
    assert_eq!(pending["retry"]["mode"], "same_request");
}

#[tokio::test]
async fn durability_pending_evaluation_preserves_attempt_and_verdicts_across_late_commit() {
    let _temp_scope = TestTempRootDirectoryScope::repository_local();
    let (state, mut writer, delegation, child) = evaluator_fixture_with_stepped_writer();
    let mut request = two_verdicts();
    request.attempt_key = Some(delegation.clone());
    let first_request = request.clone();
    let worker = {
        let state = state.clone();
        let child = child.clone();
        std::thread::spawn(move || {
            state.submit_acceptance_evaluation_with_runner(
                &child,
                first_request,
                runner_must_not_run,
            )
        })
    };
    writer.receive_fence();
    assert!(
        !worker.is_finished(),
        "no tracker call before the host's pending row ACK"
    );
    let original = submission_of(&state, &delegation);
    assert!(matches!(
        original,
        AcceptanceEvaluationSubmission::Pending { .. }
    ));
    // Capture the REAL writer image before expiry; the worker is allowed to
    // restore its conservative in-memory state when its waiter expires.
    let late = collect_persist_delta_from_shared_state(&state.inner, writer.watermark);
    assert!(matches!(
        writer.batch.pending[0]
            .completion
            .poll_at(writer.batch.pending[0].completion.deadline),
        Some(Err(PersistFenceError::Deadline))
    ));
    let error = match worker.join().unwrap() {
        Err(error) => error,
        Ok(_) => panic!("an expired pending-row fence is not an evaluation receipt"),
    };
    let response = error.into_response();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let pending: Value = serde_json::from_slice(&bytes).unwrap();
    persist_delta_with_fences(&mut writer.cache, &writer.path, &late, &mut writer.batch).unwrap();
    writer.watermark = late.watermark;
    assert_eq!(
        durable_submission(&writer.path, &delegation),
        Some(original.clone())
    );
    assert!(
        outcome_of(&state, &delegation).is_none(),
        "no recorded-success receipt before ACK"
    );

    let mut changed = request.clone();
    changed.verdicts[0].rationale.push_str(" changed judgment");
    let error = state
        .submit_acceptance_evaluation_with_runner(&child, changed, runner_must_not_run)
        .expect_err("a pending attempt cannot silently accept different verdicts");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("same verdicts"));
    assert!(error.durability_deadline.is_none());
    assert_eq!(submission_of(&state, &delegation), original);
    let restarted = reloaded_pending_evaluator(&state);
    assert_eq!(submission_of(&restarted, &delegation), original);
    // Boot has no running evaluator runtime. The unchanged resend is a
    // current-authority conflict, not an invented ACK or a replacement attempt.
    let error = restarted
        .submit_acceptance_evaluation_with_runner(&child, request.clone(), runner_must_not_run)
        .expect_err("a loaded record alone does not authorize a fresh evaluator write");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.durability_deadline.is_none());
    assert_eq!(submission_of(&restarted, &delegation), original);

    let (calls, run) = scripted_runner(vec![Ok(cli_output(true, &evaluate_receipt(false), ""))]);
    let worker = {
        let state = state.clone();
        let child = child.clone();
        std::thread::spawn(move || {
            state.submit_acceptance_evaluation_with_runner(&child, request, run)
        })
    };
    writer.receive_fence();
    assert!(calls.lock().unwrap().is_empty());
    writer.write();
    writer.receive_fence();
    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "one tracker operation under the same attempt"
    );
    assert!(
        !worker.is_finished(),
        "the outcome also needs its exact host ACK"
    );
    writer.write();
    let recorded = worker.join().unwrap().unwrap();
    assert_eq!(recorded.attempt_key, delegation);
    assert!(matches!(
        durable_submission(&writer.path, &delegation),
        Some(AcceptanceEvaluationSubmission::Recorded { .. })
    ));
    assert!(outcome_of(&state, &delegation).is_some());

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{pending}");
    assert_eq!(pending["contract"], "termal-durability-pending");
    assert_eq!(pending["version"], 1);
    assert_eq!(pending["code"], "durability_pending");
    assert_eq!(pending["outcome"], "pending");
    assert_eq!(pending["operation"], "acceptance_evaluation_submission");
    assert_eq!(pending["identity"]["delegationId"], delegation);
    assert_eq!(pending["identity"]["attemptKey"], recorded.attempt_key);
    assert_eq!(pending["identity"]["evaluatorSessionId"], child);
    assert_eq!(pending["retry"]["mode"], "same_request");
}
