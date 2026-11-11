// Owner-backed ordinary source identity, complete comparisons and immutable
// replay. The scripted subprocess supplies explicit producer facts; it does
// not judge the real checkout or certify that a check ran.
use super::*;
use crate::tests::engram_source_command::source_envelope;
use crate::tests::engram_source_owner::{
    source_owner_fixture_binary, source_owner_output, source_owner_reply,
};

fn identity() -> EngramSourceId {
    serde_json::from_value(json!({"commit":"a".repeat(40), "tree":"b".repeat(40),
        "submodules":[{"path":"vendor/lib", "commit":"c".repeat(40), "changes":[]}],
        "changes":[{"path":"new.rs", "status":"R ", "origin":"old.rs",
            "blob":"d".repeat(40), "mode":"100755"}]}))
    .unwrap()
}

fn owner_basis(id: EngramSourceId) -> EngramExecutionSourceBasis {
    EngramExecutionSourceBasis {
        workspace_id: "observation-workspace".into(),
        source_revision: String::new(),
        source_id: Some(id),
        source_root_generation: Some(2),
        source_root_state: Some(EngramSourceRootState::Named),
    }
}

fn capture_with(id: EngramSourceId) -> EngramRuntimeClosingCapture {
    let mut capture = observation_capture();
    capture.source = EngramRuntimeClosingSource::Measured(EngramMeasuredSighting {
        source_basis: owner_basis(id),
        observed_at: observation_at(50),
    });
    capture
}

#[test]
fn ordinary_source_wire_preserves_complete_baseline_sighting_and_quiet_identity() {
    let before = identity();
    let mut after = before.clone();
    after.changes[0].blob = Some("e".repeat(40));
    let mut capture = capture_with(after.clone());
    capture.baseline = Some(EngramRuntimeCheckpointBasis::of_sighting(
        observation_binding(),
        &owner_basis(before.clone()),
        &observation_at(5),
    ));
    let mut trace = observation_trace();
    let mut command = observation_command("check-one");
    command.check.source_basis = None;
    trace.commands.push(command);
    let requests = prepare_runtime_observations(&trace, capture, chrono::Utc::now()).unwrap();
    let change = &requests[0]["occurrence"]["source_change"];
    assert_eq!(
        change["baseline"]["source_id"],
        serde_json::to_value(&before).unwrap()
    );
    assert_eq!(
        change["sighting"]["source_basis"]["source_id"],
        serde_json::to_value(&after).unwrap()
    );
    assert!(change["baseline"].get("source_revision").is_none());
    assert!(
        change["sighting"]["source_basis"]
            .get("source_revision")
            .is_none()
    );
    assert!(
        requests[0]["occurrence"]["observed_checks"][0]
            .get("source_basis")
            .is_none()
    );
    assert_eq!(
        requests[1]["occurrence"]["source_basis"]["source_id"],
        serde_json::to_value(&after).unwrap()
    );
    let quiet = prepare_runtime_observations(
        &trace,
        {
            let mut capture = capture_with(after.clone());
            capture.baseline = Some(EngramRuntimeCheckpointBasis::of_sighting(
                observation_binding(),
                &owner_basis(after),
                &observation_at(5),
            ));
            capture
        },
        chrono::Utc::now(),
    )
    .unwrap();
    assert!(quiet[0]["occurrence"]["source_change"].is_null());
}

#[test]
fn registered_endpoints_compare_whole_objects_and_keep_the_middle_of_x_y_x() {
    let first = identity();
    for component in ["tree", "submodule", "blob", "origin", "mode"] {
        let mut middle = first.clone();
        match component {
            "tree" => middle.tree = "e".repeat(40),
            "submodule" => middle.submodules[0].commit = "e".repeat(40),
            "blob" => middle.changes[0].blob = Some("e".repeat(40)),
            "origin" => middle.changes[0].origin = Some("other.rs".into()),
            _ => middle.changes[0].mode = Some(EngramSourceIdMode::Regular),
        }
        let mut capture = capture_with(first.clone());
        capture.baseline = Some(EngramRuntimeCheckpointBasis::of_sighting(
            observation_binding(),
            &owner_basis(first.clone()),
            &observation_at(5),
        ));
        capture.registered_check_baselines = [(&first, 20), (&middle, 30), (&first, 40)]
            .into_iter()
            .map(|(id, second)| {
                EngramRuntimeCheckpointBasis::of_sighting(
                    observation_binding(),
                    &owner_basis(id.clone()),
                    &observation_at(second),
                )
            })
            .collect();
        let Some(EngramObservedSourceChange::ContentComparison { baseline, .. }) =
            capture.source_change()
        else {
            panic!("{component}: the intermediate source differs");
        };
        assert_eq!(baseline.source_id, Some(middle), "{component}");
        assert_eq!(baseline.observed_at, observation_at(30));
    }
}

#[test]
fn historical_text_requests_round_trip_unchanged_and_are_not_new_comparison_baselines() {
    let old = prepare_runtime_observations(
        &observation_trace(),
        observation_capture(),
        chrono::Utc::now(),
    )
    .unwrap();
    for wire in old {
        let bytes = serde_json::to_vec(&wire).unwrap();
        let decoded: EngramControlRequest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
    }
    let mut capture = capture_with(identity());
    capture.baseline = Some(historical_observation_endpoint());
    assert!(matches!(
        capture.source_change(),
        Some(EngramObservedSourceChange::AssumedMissingBaseline { .. })
    ));
}

#[test]
fn unavailable_owner_keeps_checks_and_outcome_without_a_source_claim() {
    let mut capture = capture_with(identity());
    capture.source = EngramRuntimeClosingSource::Unavailable {
        observed_at: observation_at(50),
    };
    let mut trace = observation_trace();
    let mut command = observation_command("check-one");
    command.check.source_basis = None;
    trace.commands.push(command);
    let requests = prepare_runtime_observations(&trace, capture, chrono::Utc::now()).unwrap();
    assert_eq!(requests.len(), 1);
    let occurrence = &requests[0]["occurrence"];
    assert!(occurrence["source_change"].is_null());
    assert_eq!(occurrence["outcome"], "succeeded");
    assert_eq!(
        occurrence["observed_checks"][0]["observed_result"],
        "passed"
    );
}

#[test]
fn oversized_workspace_with_successful_owner_still_closes_with_outcome_and_checks() {
    let _temp_scope = TestTempRootDirectoryScope::repository_local();
    let turn = EOCheckedTurn::idle("observation-long-root", true);
    turn.acknowledge_unnamed_root();
    let mut root = turn.root.join("long-worktree");
    while root.to_string_lossy().len() <= ENGRAM_OBSERVATION_MAX_SOURCE_BYTES {
        root.push("界".repeat(60));
    }
    // Git derives its administrative directory from this final name. Keep
    // that directory short while the measured worktree stays over the bound.
    root.push("wt");
    fs::create_dir_all(root.parent().unwrap()).unwrap();
    // A scripted owner supplies the contents. No checkout is needed, and it
    // would pass the long .git path to Git's fixed-size GIT_DIR startup check.
    run_git_test_command(
        &turn.root,
        &[
            "-c",
            "core.longpaths=true",
            "worktree",
            "add",
            "--quiet",
            "--detach",
            "--no-checkout",
            root.to_str().unwrap(),
        ],
    );
    let root = fs::canonicalize(root).unwrap();
    assert!(root.to_string_lossy().len() > ENGRAM_OBSERVATION_MAX_SOURCE_BYTES);
    let root_text = root.to_string_lossy();
    eprintln!(
        "observation workspace fixture> canonical_chars={} canonical_utf8_bytes={} canonical_utf16_units={}",
        root_text.chars().count(),
        root_text.len(),
        root_text.encode_utf16().count()
    );
    assert_eq!(engram_worktree_root_path(&root), root);
    let binary = source_owner_fixture_binary(turn.state.test_temp_root.as_ref().unwrap().path());
    let project = turn.record(|record| record.session.project_id.clone().unwrap());
    turn.record_mut(|record| record.session.workdir = root.to_string_lossy().into_owned());
    let owner_reply = source_owner_output(
        0,
        serde_json::to_vec(&source_envelope(&root, "clean")).unwrap(),
    );
    // The long path is accepted by the owner protocol. It is the smaller
    // observation workspace bound, not an owner refusal, that needs fallback.
    assert!(
        parse_engram_source_observation(
            &owner_reply.stdout,
            Some(0),
            &root,
            std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET
        )
        .unwrap()
        .into_source_id(std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET)
        .is_ok()
    );
    let source_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls = source_calls.clone();
    {
        let mut inner = turn.state.inner.lock().unwrap();
        inner.test_engram_source_capture = None;
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram
            .as_mut()
            .unwrap()
            .binary_path = Some(binary.to_string_lossy().into_owned());
        inner.test_observation_source_owner = Some(Arc::new(move |_, _, args, _, _| {
            if args[0] == "--version" {
                return Ok(source_owner_reply(args));
            }
            assert!(args[2].len() > ENGRAM_OBSERVATION_MAX_SOURCE_BYTES);
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(owner_reply.clone())
        }));
    }
    let mut trace = observation_trace();
    let mut command = observation_command("long-root-check");
    command.check.source_basis = None;
    trace.commands.push(command);
    let scope =
        EngramRuntimeClosingScope::capture(&turn.state.inner.lock().unwrap(), &turn.session_id)
            .unwrap();
    let mut job = EngramRuntimeClosingJob::new(trace, None, scope);
    job.policy = Some(observation_policy());
    let outcome = turn
        .state
        .capture_runtime_observation_report(&turn.session_id, &job)
        .unwrap();
    let EngramRuntimeClosingOutcome::Report(report, _) = outcome else {
        panic!("a long workspace must preserve its closing report");
    };
    assert_eq!(source_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        report.envelopes.len(),
        1,
        "an unavailable source supplies no separate sighting"
    );
    let occurrence = &report.envelopes[0].request["occurrence"];
    assert_eq!(occurrence["kind"], "unadmitted_turn");
    assert!(occurrence["source_change"].is_null());
    assert_eq!(occurrence["outcome"], "succeeded");
    assert_eq!(occurrence["observed_checks"].as_array().unwrap().len(), 1);
    assert_eq!(
        occurrence["observed_checks"][0]["host_check_id"],
        "long-root-check"
    );
    assert_eq!(
        occurrence["observed_checks"][0]["observed_result"],
        "passed"
    );
}

#[test]
fn large_observation_keeps_both_objects_but_refuses_an_oversize_identity() {
    let mut before = identity();
    before.changes[0].path = "p".repeat(600_000);
    let mut after = before.clone();
    after.changes[0].blob = Some("e".repeat(40));
    let mut capture = capture_with(after.clone());
    capture.baseline = Some(EngramRuntimeCheckpointBasis::of_sighting(
        observation_binding(),
        &owner_basis(before.clone()),
        &observation_at(5),
    ));
    let requests =
        prepare_runtime_observations(&observation_trace(), capture, chrono::Utc::now()).unwrap();
    let bytes = serde_json::to_vec(&requests[0]).unwrap();
    assert!(bytes.len() > ENGRAM_CONTROL_MAX_FRAME_BYTES);
    assert_eq!(
        requests[0]["occurrence"]["source_change"]["baseline"]["source_id"],
        serde_json::to_value(before).unwrap()
    );
    assert!(serde_json::from_slice::<EngramControlRequest>(&bytes).is_ok());
    after.changes[0].path = "p".repeat(ENGRAM_SOURCE_JSON_LIMIT);
    assert!(
        prepare_runtime_observations(
            &observation_trace(),
            capture_with(after),
            chrono::Utc::now()
        )
        .is_err()
    );
}

#[test]
fn only_observation_frames_pass_the_ordinary_transport_limit() {
    let root = TestTempRoot::create("observation-frame-bound");
    let connection = observation_connection(&root);
    assert!(
        !connection.binary_path.exists(),
        "the fixture must never select an installed owner"
    );
    let transport = ProcessEngramControlTransport::new(root.to_path_buf());
    let mut id = identity();
    id.changes[0].path = "p".repeat(600_000);
    let wire =
        prepare_runtime_observations(&observation_trace(), capture_with(id), chrono::Utc::now())
            .unwrap()
            .remove(0);
    let request: EngramControlRequest = serde_json::from_value(wire).unwrap();
    let error = transport
        .request(&connection, &request, ENGRAM_SOURCE_COMMAND_BUDGET)
        .unwrap_err();
    assert!(
        !error.message.contains("maximum control frame"),
        "the observation reached executable resolution"
    );
    assert_eq!(error.kind, EngramTransportErrorKind::Transport);
    // Windows rejects the missing binary during resolution; Unix reaches
    // spawn and marks the request unsent. Neither path sends this frame.
    assert!(
        error.process_never_started || error.request_never_sent,
        "{error:?}"
    );
    assert!(
        error.control_process.is_none(),
        "no control child was created"
    );
    let ordinary = EngramControlRequest::AcceptanceBindingRead {
        work_id: "x".repeat(ENGRAM_CONTROL_MAX_FRAME_BYTES + 1),
        expected_work_revision: 1,
        run_id: "frame-test".into(),
        after: None,
    };
    let error = transport
        .request(&connection, &ordinary, ENGRAM_SOURCE_COMMAND_BUDGET)
        .unwrap_err();
    assert!(error.request_never_sent && error.message.contains("maximum control frame"));
    let mut oversize = request;
    if let EngramControlRequest::ExecutionObserve { observation } = &mut oversize {
        observation.idempotency_key = "x".repeat(ENGRAM_OBSERVATION_MAX_REQUEST_BYTES + 1);
    }
    let error = transport
        .request(&connection, &oversize, ENGRAM_SOURCE_COMMAND_BUDGET)
        .unwrap_err();
    assert!(error.request_never_sent && error.message.contains("maximum control frame"));
    assert!(
        transport
            .processes
            .lock()
            .unwrap()
            .values()
            .all(Option::is_none)
    );
}

#[test]
fn observed_checks_cannot_expand_the_two_identity_frame_contract() {
    let mut trace = observation_trace();
    let mut command = observation_command("check-one");
    command.check.source_basis = Some(owner_basis(identity()));
    trace.commands.push(command);
    let error = prepare_runtime_observations(&trace, capture_with(identity()), chrono::Utc::now())
        .unwrap_err();
    assert!(error.message.contains("observed checks carry no source_id"));
    let summary = format!("{:?}", owner_basis(identity()));
    assert!(
        !summary.contains("new.rs")
            && !summary.contains("vendor/lib")
            && !summary.contains("old.rs")
    );
}

#[test]
fn observation_capture_consumes_the_pinned_owner_and_discards_failure_payloads() {
    let turn = EOCheckedTurn::idle("observation-owner-identity", true);
    let binary = source_owner_fixture_binary(turn.state.test_temp_root.as_ref().unwrap().path());
    let project = turn.record(|record| record.session.project_id.clone().unwrap());
    {
        let mut inner = turn.state.inner.lock().unwrap();
        inner.test_engram_source_capture = None;
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram
            .as_mut()
            .unwrap()
            .binary_path = Some(binary.to_string_lossy().into_owned());
    }
    let owner_id = identity();
    let expected = owner_id.clone();
    let inner = Arc::downgrade(&turn.state.inner);
    turn.state
        .inner
        .lock()
        .unwrap()
        .test_observation_source_owner = Some(Arc::new(move |_, _, args, until, _| {
        if current_capture_cancellation().is_some() {
            // Advance only the cancellation observation, not real time. The
            // real check worker must still permit this admitted source call
            // beyond the unrelated Git capture window.
            let after_git_window =
                std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT + Duration::from_millis(1);
            assert!(after_git_window < until);
            check_capture_cancellation_at(after_git_window)?;
            assert_eq!(capture_deadline(until), until);
        }
        assert!(
            inner
                .upgrade()
                .unwrap()
                .is_not_held_by_current_thread_for_test()
        );
        if args[0] == "--version" {
            return Ok(source_owner_reply(args));
        }
        let mut value = source_envelope(FsPath::new(&args[2]), "not_clean");
        value["source_id"] = serde_json::to_value(&owner_id).unwrap();
        Ok(source_owner_output(0, serde_json::to_vec(&value).unwrap()))
    }));
    let place = EngramBasisPlace::Workdir(turn.root.to_string_lossy().into_owned());
    let measured = turn
        .state
        .engram_observation_source_capture(&turn.session_id, &place, ENGRAM_SOURCE_COMMAND_BUDGET)
        .ok()
        .flatten()
        .expect("owner supplied a full identity");
    assert_eq!(measured.source_id, Some(expected.clone()));
    assert!(measured.source_revision.is_empty());
    let workers = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let check_endpoint = engram_spawn_basis_capture(
        place.clone(),
        &workers,
        EngramRootCapture::Unnamed,
        &turn.state.capture_lifetime,
        turn.state.clone(),
        turn.session_id.clone(),
    );
    let endpoint = check_endpoint
        .wait_until(std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET)
        .flatten()
        .expect("the internal check endpoint retains its complete identity");
    assert_eq!(endpoint.source_id, Some(expected));
    assert_eq!(workers.load(std::sync::atomic::Ordering::SeqCst), 0);
    for failure in [
        "stderr",
        "malformed",
        "wrong-root",
        "replaced-binary",
        "configuration",
    ] {
        let binary = binary.clone();
        let state = Arc::downgrade(&turn.state.inner);
        let project_for_failure = project.clone();
        turn.state
            .inner
            .lock()
            .unwrap()
            .test_observation_source_owner = Some(Arc::new(move |_, _, args, _, _| {
            if args[0] == "--version" {
                return Ok(source_owner_reply(args));
            }
            let mut value = source_envelope(FsPath::new(&args[2]), "clean");
            if failure == "wrong-root" {
                value["root"] = json!(binary.parent().unwrap());
            }
            let mut output = source_owner_output(0, serde_json::to_vec(&value).unwrap());
            match failure {
                "stderr" => output.stderr = b"private owner diagnostic".to_vec(),
                "malformed" => output.stdout = b"not-json".to_vec(),
                "replaced-binary" => fs::write(&binary, b"replacement bytes").unwrap(),
                "configuration" => {
                    state
                        .upgrade()
                        .unwrap()
                        .lock()
                        .unwrap()
                        .projects
                        .iter_mut()
                        .find(|p| p.id == project_for_failure)
                        .unwrap()
                        .engram
                        .as_mut()
                        .unwrap()
                        .binary_path = Some("other-owner".into())
                }
                _ => {}
            }
            Ok(output)
        }));
        assert!(
            matches!(
                turn.state.engram_observation_source_capture(
                    &turn.session_id,
                    &place,
                    ENGRAM_SOURCE_COMMAND_BUDGET
                ),
                Err(EngramObservationSourceRefusal::OwnerUnavailable)
            ),
            "{failure}"
        );
        let restored =
            source_owner_fixture_binary(turn.state.test_temp_root.as_ref().unwrap().path());
        turn.state
            .inner
            .lock()
            .unwrap()
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram
            .as_mut()
            .unwrap()
            .binary_path = Some(restored.to_string_lossy().into_owned());
    }
    turn.state
        .inner
        .lock()
        .unwrap()
        .test_observation_source_owner =
        Some(Arc::new(|_, _, args, _, _| Ok(source_owner_reply(args))));
    assert!(matches!(
        turn.state.engram_observation_source_capture(
            &turn.session_id,
            &place,
            ENGRAM_SOURCE_COMMAND_BUDGET
        ),
        Err(EngramObservationSourceRefusal::OwnerUnavailable)
    ));
    // No real waiting: an expired budget must not invoke the transport.
    turn.state
        .inner
        .lock()
        .unwrap()
        .test_observation_source_owner = Some(Arc::new(|_, _, _, _, _| {
        panic!("expired captures cannot start a process")
    }));
    assert!(matches!(
        turn.state
            .engram_observation_source_capture(&turn.session_id, &place, Duration::ZERO),
        Err(EngramObservationSourceRefusal::OwnerUnavailable)
    ));
    turn.state
        .inner
        .lock()
        .unwrap()
        .test_observation_source_owner = None;
}
