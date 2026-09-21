// Operator settings contracts; all reads/writes use isolated stores and injected CLI runners.
use super::work_visualizer::{fixture, install_store};
use super::*;

fn receipt() -> Value {
    json!({"schema_version":1,"policy":"915c51b89cd640f481a6e2c653bcfffd","epoch":1,
        "required_assurance":"advisory","acceptance_evaluation":{"allowed_modes":[],
        "mechanical_basis":"asserted","require_source_freshness":false}})
}

#[test]
fn acceptance_settings_auto_prefers_only_a_ready_other_vendor() {
    let mut readiness = vec![AgentReadiness {
        agent: Agent::Claude,
        status: AgentReadinessStatus::Ready,
        blocking: false,
        detail: String::new(),
        warning_detail: None,
        command_path: None,
    }];
    assert_eq!(
        auto_acceptance_evaluator_agent(Agent::Codex, &readiness),
        Agent::Claude
    );
    assert_eq!(
        auto_acceptance_evaluator_agent(Agent::Claude, &readiness),
        Agent::Claude
    );
    readiness[0].blocking = true;
    assert_eq!(
        auto_acceptance_evaluator_agent(Agent::Codex, &readiness),
        Agent::Codex
    );
    assert_eq!(
        auto_acceptance_evaluator_agent(Agent::Codex, &[]),
        Agent::Codex
    );
}

fn change(reader: String) -> UpdateAcceptancePolicyRequest {
    serde_json::from_value(
        json!({"modes":["independent_session"],"mechanicalBasis":"asserted",
        "requireSourceFreshness":false,"expectedPolicy":"915c51b89cd640f481a6e2c653bcfffd",
        "readerKey":reader,"idempotencyKey":"termal-policy-test-1"}),
    )
    .unwrap()
}

#[test]
fn acceptance_settings_policy_request_rejects_removed_justification() {
    let mut payload = json!({"modes":[],"mechanicalBasis":"asserted",
        "requireSourceFreshness":false,"expectedPolicy":"915c51b89cd640f481a6e2c653bcfffd",
        "readerKey":"reader","idempotencyKey":"termal-policy-test"});
    assert!(serde_json::from_value::<UpdateAcceptancePolicyRequest>(payload.clone()).is_ok());
    payload["reason"] = json!("obsolete justification");
    assert!(serde_json::from_value::<UpdateAcceptancePolicyRequest>(payload).is_err());
}

#[test]
fn acceptance_settings_reads_policy_without_doctor_and_discloses_unavailable() {
    let (state, project, _, root) = fixture();
    install_store(&state, &project, &root);
    let result = state
        .project_acceptance_policy_with_reader(&project, |_, args, timeout| {
            assert_eq!(args, &["control-policy", "show"]);
            assert_eq!(timeout, ACCEPTANCE_EVALUATION_POLICY_READ_TIMEOUT);
            Ok(receipt())
        })
        .unwrap();
    assert!(result.available);
    assert!(result.acceptance_evaluation.unwrap().modes.is_empty());
    let unavailable = state
        .project_acceptance_policy_with_reader(&project, |_, _, _| {
            Err(EngramTransportError::transport("old binary"))
        })
        .unwrap();
    assert!(!unavailable.available);
    assert!(unavailable.policy.is_none());
    assert!(parse_acceptance_policy(json!({"schema_version":99}), "r".into()).is_err());
}

#[test]
fn acceptance_settings_policy_read_rejects_store_rotation() {
    let (state, project, _, root) = fixture();
    install_store(&state, &project, &root);
    let error = state
        .project_acceptance_policy_with_reader(&project, |_, _, _| {
            let mut inner = state.inner.lock().unwrap();
            inner
                .projects
                .iter_mut()
                .find(|p| p.id == project)
                .unwrap()
                .engram
                .as_mut()
                .unwrap()
                .enabled = false;
            Ok(receipt())
        })
        .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
}

#[test]
fn acceptance_settings_defaults_preserve_connection_sessions_and_store() {
    let (state, project, session, root) = fixture();
    install_store(&state, &project, &root);
    let (before, session_before) = {
        let inner = state.inner.lock().unwrap();
        (
            serde_json::to_value(
                inner
                    .find_project(&project)
                    .unwrap()
                    .engram
                    .as_ref()
                    .unwrap(),
            )
            .unwrap(),
            serde_json::to_value(
                &inner.sessions[inner.find_session_index(&session).unwrap()].session,
            )
            .unwrap(),
        )
    };
    // Fixture binary cannot run doctor. Saving must be preferences-only.
    state
        .update_acceptance_defaults(
            &project,
            AcceptanceEvaluatorDefaults {
                default_mode: Some(AcceptanceEvaluationMode::IndependentSession),
                evaluator_agent: Some(Agent::Claude),
                evaluator_model: Some("test-model".into()),
            },
        )
        .unwrap();
    let inner = state.inner.lock().unwrap();
    let mut after = serde_json::to_value(
        inner
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(after["acceptanceEvaluation"]["evaluatorAgent"], "Claude");
    after
        .as_object_mut()
        .unwrap()
        .remove("acceptanceEvaluation");
    assert_eq!(after, before);
    assert_eq!(
        serde_json::to_value(&inner.sessions[inner.find_session_index(&session).unwrap()].session)
            .unwrap(),
        session_before
    );
}

#[test]
fn acceptance_settings_argv_replaces_whole_policy_with_cas_and_stable_key() {
    let mut request = change("reader".into());
    let args = request.args("operator/termal").unwrap();
    assert_eq!(
        args,
        vec![
            "control-policy",
            "set-acceptance-evaluation",
            "--modes",
            "independent-session",
            "--mechanical-basis",
            "asserted",
            "--authorized-by=operator/termal",
            "--idempotency-key",
            "termal-policy-test-1",
            "--expected-policy-hash",
            "915c51b89cd640f481a6e2c653bcfffd"
        ]
    );
    request.modes.clear();
    assert!(
        !request
            .args("operator")
            .unwrap()
            .iter()
            .any(|a| a == "--modes")
    );
    request.require_source_freshness = true;
    assert!(
        request
            .args("operator")
            .unwrap()
            .iter()
            .any(|a| a == "--require-source-freshness")
    );
    assert!(
        request
            .args("-operator")
            .unwrap()
            .contains(&"--authorized-by=-operator".to_owned())
    );
    request.idempotency_key = "\n".into();
    assert!(request.args("operator").is_err());
}

#[test]
fn acceptance_settings_options_and_required_flags_match_captured_binary_usage() {
    let help = include_str!("fixtures/engram-acceptance-policy-set-help.txt");
    let usage = help
        .lines()
        .find(|line| line.starts_with("Usage: "))
        .unwrap();
    let required_flags: Vec<_> = usage
        .split_once("[OPTIONS]")
        .unwrap()
        .1
        .split_whitespace()
        .filter(|word| word.starts_with("--"))
        .collect();
    assert_eq!(required_flags, ["--authorized-by", "--idempotency-key"]);
    assert!(!help.contains("--reason"));
    let mut request = change("reader".into());
    request.modes = vec![
        AcceptanceEvaluationMode::SameSession,
        AcceptanceEvaluationMode::SubAgent,
        AcceptanceEvaluationMode::IndependentSession,
    ];
    request.require_source_freshness = true;
    for basis in ["asserted", "observed"] {
        request.mechanical_basis = basis.into();
        let args = request.args("operator/termal").unwrap();
        for required in &required_flags {
            assert!(
                args.iter()
                    .any(|arg| arg.split('=').next() == Some(*required)),
                "missing required flag: {required}"
            );
        }
        assert!(help.contains(&format!("{} {} [OPTIONS]", args[0], args[1])));
        for arg in args.iter().filter(|arg| arg.starts_with("--")) {
            let flag = arg.split('=').next().unwrap();
            assert!(
                help.lines()
                    .any(|line| line.split_whitespace().next() == Some(flag)),
                "unadvertised flag: {flag}"
            );
        }
        let modes_index = args.iter().position(|arg| arg == "--modes").unwrap();
        let advertised_modes = help
            .lines()
            .find(|line| line.contains("Allowed evaluator modes,"))
            .unwrap();
        for mode in args[modes_index + 1].split(',') {
            assert!(
                advertised_modes
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                    .any(|word| word == mode)
            );
        }
        assert!(help.contains(&format!("- {basis}:")));
    }
}

#[test]
#[ignore = "requires TERMAL_TEST_LIVE_ENGRAM_BINARY; creates only a disposable store"]
fn real_acceptance_policy_reason_free_init_write_and_replay_disposable_store() {
    let binary = PathBuf::from(
        std::env::var_os("TERMAL_TEST_LIVE_ENGRAM_BINARY").expect("candidate binary"),
    );
    assert!(binary.is_absolute() && binary.is_file());
    let state = test_app_state();
    let temp = state.test_temp_root.as_ref().expect("isolated test root");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let marker = temp.path().join(".engram-project");
    fs::write(&marker, "termal-reason-free-disposable\n").unwrap();
    let run = |args: &[String]| {
        let output = Command::new(&binary)
            .arg("--project-file")
            .arg(&marker)
            .arg("--home")
            .arg(&home)
            .args(args)
            .current_dir(temp.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "args={args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    run(&[
        "init".into(),
        "--required-assurance".into(),
        "turn_gated".into(),
        "--authorized-by=termal-test".into(),
    ]);
    let read_policy = || {
        serde_json::from_slice::<Value>(&run(&["control-policy".into(), "show".into()]))
            .expect("policy JSON")
    };
    let before = read_policy();
    let mut request = change("disposable".into());
    request.expected_policy = before["policy"].as_str().unwrap().to_owned();
    let args = request.args("termal-test").unwrap();
    assert!(!args.iter().any(|arg| arg.starts_with("--reason")));
    let first: Value = serde_json::from_slice(&run(&args)).expect("receipt JSON");
    let replay: Value = serde_json::from_slice(&run(&args)).expect("replayed receipt JSON");
    assert_eq!(
        first, replay,
        "uncertain-response retry replays the exact receipt"
    );
    let after = read_policy();
    assert_ne!(before["policy"], after["policy"]);
    assert_eq!(
        after["acceptance_evaluation"]["allowed_modes"],
        json!(["independent_session"])
    );
}

#[test]
fn acceptance_settings_write_does_not_retry_unknown_outcomes_and_detects_cas_conflicts() {
    let (state, project, _, root) = fixture();
    install_store(&state, &project, &root);
    let reader = state
        .acceptance_policy_target(&project, None)
        .unwrap()
        .reader_key;
    let calls = std::cell::Cell::new(0);
    let error = state
        .update_acceptance_policy_with_runner(
            &project,
            change(reader.clone()),
            |_, _| {
                calls.set(calls.get() + 1);
                Err(EngramTransportError::transport("response lost"))
            },
            |_, _, _| panic!("must not read after unknown write"),
        )
        .unwrap_err();
    assert_eq!(calls.get(), 1);
    assert_eq!(error.status, StatusCode::BAD_GATEWAY);
    let error = state
        .update_acceptance_policy_with_runner(
            &project,
            change(reader),
            |_, _| {
                Ok(EngramCliOutput {
                    success: false,
                    exit: EngramCliExit::Code(1),
                    status: "exit code: 1".into(),
                    stdout: vec![],
                    stderr:
                        b"error: active control policy changed: expected old, current policy is new"
                            .to_vec(),
                })
            },
            |_, _, _| panic!("must not read after conflict"),
        )
        .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn acceptance_settings_policy_write_requires_operator_browser_intent() {
    let app = app_router(test_app_state());
    for site in [None, Some("cross-site"), Some("same-origin")] {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/projects/missing/engram/acceptance-evaluation-policy")
            .header("content-type", "application/json");
        if let Some(site) = site {
            request = request.header("sec-fetch-site", site);
        }
        let (status, _): (StatusCode, Value) =
            request_json(&app, request.body(Body::from("{}")).unwrap()).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let request = Request::builder()
        .method("POST")
        .uri("/api/projects/missing/engram/acceptance-evaluation-policy")
        .header("content-type", "application/json")
        .header("sec-fetch-site", "same-origin")
        .header("x-termal-operator-action", "acceptance-policy")
        .body(Body::from(
            serde_json::to_vec(&json!({"modes":[],"mechanicalBasis":"asserted",
            "requireSourceFreshness":false,"expectedPolicy":"915c51b89cd640f481a6e2c653bcfffd",
            "readerKey":"reader","idempotencyKey":"termal-policy-test"}))
            .unwrap(),
        ))
        .unwrap();
    let (status, _): (StatusCode, Value) = request_json(&app, request).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the intent gate admitted this request"
    );
}

#[tokio::test]
async fn acceptance_settings_private_limiter_refuses_reads_and_writes() {
    let app = app_router_with_acceptance_policy_limiter(
        test_app_state(),
        AcceptancePolicyLimiter::new(0, 0),
    );
    for method in ["GET", "POST"] {
        let suffix = if method == "GET" {
            "control-policy"
        } else {
            "acceptance-evaluation-policy"
        };
        let request = Request::builder()
            .method(method)
            .uri(format!("/api/projects/missing/engram/{suffix}"))
            .header("content-type", "application/json")
            .header("sec-fetch-site", "same-origin")
            .header("x-termal-operator-action", "acceptance-policy")
            .body(Body::from(
                json!({"modes":[],"mechanicalBasis":"asserted",
                "requireSourceFreshness":false,"expectedPolicy":"915c51b89cd640f481a6e2c653bcfffd",
                "readerKey":"reader","idempotencyKey":"termal-policy-test"})
                .to_string(),
            ))
            .unwrap();
        let (status, _): (StatusCode, Value) = request_json(&app, request).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    }
}

#[tokio::test]
async fn acceptance_settings_defaults_http_rejects_unknown_fields_types_and_missing_projects() {
    let app = app_router(test_app_state());
    for (body, expected) in [
        (json!({"unexpected":true}), StatusCode::UNPROCESSABLE_ENTITY),
        (
            json!({"evaluatorModel":42}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (json!({}), StatusCode::NOT_FOUND),
    ] {
        let request = Request::builder()
            .method("PATCH")
            .uri("/api/projects/missing/engram/acceptance-evaluation-defaults")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let (status, response): (StatusCode, Value) = request_json(&app, request).await;
        assert_eq!(status, expected, "{response}");
    }
}

#[tokio::test]
async fn acceptance_settings_read_and_write_admission_are_independent() {
    let limiter = AcceptancePolicyLimiter::new(2, 1);
    let held_reads = limiter.reads.clone().try_acquire_many_owned(2).unwrap();
    let app = app_router_with_acceptance_policy_limiter(test_app_state(), limiter.clone());
    let make_post = || {
        Request::builder()
            .method("POST")
            .uri("/api/projects/missing/engram/acceptance-evaluation-policy")
            .header("content-type", "application/json")
            .header("sec-fetch-site", "same-origin")
            .header("x-termal-operator-action", "acceptance-policy")
            .body(Body::from(
                json!({"modes":[],"mechanicalBasis":"asserted",
            "requireSourceFreshness":false,"expectedPolicy":"915c51b89cd640f481a6e2c653bcfffd",
            "readerKey":"reader","idempotencyKey":"termal-policy-test"})
                .to_string(),
            ))
            .unwrap()
    };
    let make_get = || {
        Request::builder()
            .uri("/api/projects/missing/engram/control-policy")
            .body(Body::empty())
            .unwrap()
    };
    let (read_status, _): (StatusCode, Value) = request_json(&app, make_get()).await;
    assert_eq!(read_status, StatusCode::TOO_MANY_REQUESTS);
    let (write_status, _): (StatusCode, Value) = request_json(&app, make_post()).await;
    assert_eq!(
        write_status,
        StatusCode::NOT_FOUND,
        "held display reads must not reject a write before project lookup"
    );
    drop(held_reads);
    let _held_write = limiter.writes.clone().try_acquire_owned().unwrap();
    let (write_status, _): (StatusCode, Value) = request_json(&app, make_post()).await;
    assert_eq!(write_status, StatusCode::TOO_MANY_REQUESTS);
    let (read_status, _): (StatusCode, Value) = request_json(&app, make_get()).await;
    assert_eq!(
        read_status,
        StatusCode::NOT_FOUND,
        "held writes must not consume display-read slots"
    );
}

#[test]
fn acceptance_settings_model_bound_matches_the_normalized_value() {
    let mut defaults = AcceptanceEvaluatorDefaults {
        evaluator_agent: Some(Agent::Claude),
        evaluator_model: Some(format!(" {} ", "m".repeat(128))),
        ..Default::default()
    };
    defaults.normalize().unwrap();
    assert_eq!(
        defaults.evaluator_model.as_deref(),
        Some("m".repeat(128).as_str())
    );
    defaults.evaluator_model = Some("   ".into());
    assert!(defaults.normalize().is_err());
}

#[test]
fn acceptance_settings_defaults_validate_server_side() {
    for value in [
        json!({"defaultMode":"sub_agent"}),
        json!({"evaluatorAgent":"Gemini"}),
        json!({"evaluatorModel":"gpt-model"}),
        json!({"evaluatorModel":""}),
        json!({"evaluatorModel":"m".repeat(129)}),
        json!({"evaluatorModel":"bad\nmodel"}),
    ] {
        let defaults: AcceptanceEvaluatorDefaults = serde_json::from_value(value).unwrap();
        assert_eq!(
            defaults.validate().unwrap_err().status,
            StatusCode::BAD_REQUEST
        );
    }
}

#[test]
fn acceptance_settings_connection_edit_preserves_omitted_evaluator_defaults() {
    let (state, project, _, root) = fixture();
    install_store(&state, &project, &root);
    let defaults = AcceptanceEvaluatorDefaults {
        evaluator_agent: Some(Agent::Claude),
        ..Default::default()
    };
    state
        .update_acceptance_defaults(&project, defaults.clone())
        .unwrap();
    state
        .update_project_engram_settings(
            &project,
            EngramProjectSettings {
                enabled: false,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        state
            .inner
            .lock()
            .unwrap()
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .acceptance_evaluation,
        Some(defaults)
    );
}

#[test]
fn acceptance_settings_parser_refusal_is_correctable_before_a_write() {
    let (state, project, _, root) = fixture();
    install_store(&state, &project, &root);
    let reader = state
        .acceptance_policy_target(&project, None)
        .unwrap()
        .reader_key;
    let error = state.update_acceptance_policy_with_runner(&project, change(reader), |_, _| Ok(EngramCliOutput {
        success: false, exit: EngramCliExit::Code(2), status: "exit code: 2".into(), stdout: vec![],
        stderr: b"error: unexpected argument\nUsage: engram control-policy set-acceptance-evaluation [OPTIONS]".to_vec(),
    }), |_, _, _| panic!("parser refusal cannot read a new policy")).unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("not sent"));
}

#[test]
fn acceptance_settings_defaults_refuse_unconfigured_remote_and_reset_projects() {
    let (state, project, _, root) = fixture();
    install_store(&state, &project, &root);
    {
        let mut inner = state.inner.lock().unwrap();
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .remote_id = "remote".into();
    }
    assert_eq!(
        state
            .update_acceptance_defaults(&project, Default::default())
            .err()
            .unwrap()
            .status,
        StatusCode::BAD_REQUEST
    );
    {
        let mut inner = state.inner.lock().unwrap();
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .remote_id = LOCAL_REMOTE_ID.into();
        assert!(inner.engram_project_resets.claim(&project).is_some());
    }
    assert_eq!(
        state
            .update_acceptance_defaults(&project, Default::default())
            .err()
            .unwrap()
            .status,
        StatusCode::CONFLICT
    );
    {
        let mut inner = state.inner.lock().unwrap();
        let generation = inner.engram_project_resets.owners[&project];
        assert!(inner.engram_project_resets.release(&project, generation));
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram = None;
    }
    assert_eq!(
        state
            .update_acceptance_defaults(&project, Default::default())
            .err()
            .unwrap()
            .status,
        StatusCode::CONFLICT
    );
    assert!(
        state
            .inner
            .lock()
            .unwrap()
            .find_project(&project)
            .unwrap()
            .engram
            .is_none(),
        "no implicit operator veto"
    );
}

#[test]
fn acceptance_settings_defaults_restore_connection_after_persist_failure() {
    let (mut state, project, _, root) = fixture();
    install_store(&state, &project, &root);
    let before = state
        .inner
        .lock()
        .unwrap()
        .find_project(&project)
        .unwrap()
        .engram
        .clone();
    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(root);
    let error = state
        .update_acceptance_defaults(
            &project,
            AcceptanceEvaluatorDefaults {
                evaluator_agent: Some(Agent::Claude),
                ..Default::default()
            },
        )
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        state
            .inner
            .lock()
            .unwrap()
            .find_project(&project)
            .unwrap()
            .engram,
        before
    );
}

#[test]
fn acceptance_settings_readiness_provider_includes_claude() {
    let readiness = collect_agent_readiness_with("test-dir", |agent, workdir| {
        assert_eq!(workdir, "test-dir");
        AgentReadiness {
            agent,
            status: AgentReadinessStatus::Ready,
            blocking: false,
            detail: String::new(),
            warning_detail: None,
            command_path: Some("fixture".into()),
        }
    });
    assert_eq!(
        readiness
            .iter()
            .filter(|r| r.agent == Agent::Claude)
            .count(),
        1
    );
    assert_eq!(
        auto_acceptance_evaluator_agent(Agent::Codex, &readiness),
        Agent::Claude
    );
}

#[test]
fn acceptance_settings_claude_readiness_and_session_admission_cover_both_paths() {
    for path in [None, Some(PathBuf::from("fixture-claude"))] {
        let actual = claude_agent_readiness_with(|| path.clone());
        assert_eq!(actual.blocking, path.is_none());
        assert_eq!(
            actual.status,
            if path.is_some() {
                AgentReadinessStatus::Ready
            } else {
                AgentReadinessStatus::Missing
            }
        );
        assert!(actual.detail.contains(if path.is_some() {
            "available at"
        } else {
            "Install the `claude` CLI"
        }));
        let admission = validate_agent_session_setup_with(Agent::Claude, "fixture", |agent, _| {
            assert_eq!(agent, Agent::Claude);
            claude_agent_readiness_with(|| path.clone())
        });
        assert_eq!(admission.is_ok(), path.is_some());
    }
}

#[test]
fn acceptance_settings_real_receipt_contract_rejects_only_unknown_replaceable_dimensions() {
    // Captured 2026-09-19 from the installed engram.exe control-policy show on
    // an isolated --home/--project-file store (no live project). Keep unrelated
    // top-level dimensions: the acceptance setter does not replace those.
    let real: Value =
        serde_json::from_str(include_str!("fixtures/engram-control-policy-show.json")).unwrap();
    let snapshot = parse_acceptance_policy(real.clone(), "reader".into()).unwrap();
    assert!(snapshot.available);
    assert_eq!(snapshot.epoch, Some(2));
    assert_eq!(
        snapshot.acceptance_evaluation.unwrap().modes,
        vec![
            AcceptanceEvaluationMode::SameSession,
            AcceptanceEvaluationMode::IndependentSession
        ]
    );
    assert_eq!(
        acceptance_evaluation_admitted_modes(&real).unwrap().len(),
        2
    );
    let mut without_metadata = real.clone();
    without_metadata.as_object_mut().unwrap().remove("epoch");
    without_metadata
        .as_object_mut()
        .unwrap()
        .remove("required_assurance");
    let minimal = parse_acceptance_policy(without_metadata, "reader".into()).unwrap();
    assert!(minimal.available);
    assert!(minimal.epoch.is_none());
    assert!(minimal.required_assurance.is_none());
    let mut future = real;
    future["acceptance_evaluation"]["future_dimension"] = json!(true);
    assert_eq!(
        parse_acceptance_policy(future, "reader".into())
            .unwrap_err()
            .status,
        StatusCode::BAD_GATEWAY
    );
}

#[tokio::test]
async fn acceptance_settings_http_get_serves_the_captured_policy_receipt() {
    let (state, project, _, root) = fixture();
    install_store(&state, &project, &root);
    let app = app_router(state);
    let (status, value): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .uri(format!("/api/projects/{project}/engram/control-policy"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(
        value["acceptanceEvaluation"]["modes"],
        json!(["same_session", "independent_session"])
    );
    assert!(value.get("writeApplied").is_none());
    assert!(
        !root.join("work-read-args.txt").exists(),
        "policy reads do not alter the Work argv observation on any platform"
    );
}

#[test]
fn acceptance_settings_known_write_stays_applied_when_settings_change_after_send() {
    let (state, project, _, root) = fixture();
    install_store(&state, &project, &root);
    let reader = state
        .acceptance_policy_target(&project, None)
        .unwrap()
        .reader_key;
    let result = state
        .update_acceptance_policy_with_runner(
            &project,
            change(reader),
            |_, _| {
                state
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
                    .enabled = false;
                Ok(EngramCliOutput {
                    success: true,
                    exit: EngramCliExit::Code(0),
                    status: "exit code: 0".into(),
                    stdout: vec![],
                    stderr: vec![],
                })
            },
            |_, _, _| panic!("do not read another store after settings drift"),
        )
        .unwrap();
    assert_eq!(result.write_applied, Some(true));
    assert!(!result.available);
    assert!(
        result
            .error
            .unwrap()
            .contains("applied to the selected store")
    );
}

#[test]
fn acceptance_settings_normalizes_model_before_persisting() {
    let (state, project, _, root) = fixture();
    install_store(&state, &project, &root);
    state
        .update_acceptance_defaults(
            &project,
            AcceptanceEvaluatorDefaults {
                evaluator_agent: Some(Agent::Claude),
                evaluator_model: Some("  test-model  ".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let inner = state.inner.lock().unwrap();
    assert_eq!(
        inner
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .acceptance_evaluation
            .as_ref()
            .unwrap()
            .evaluator_model
            .as_deref(),
        Some("test-model")
    );
}
