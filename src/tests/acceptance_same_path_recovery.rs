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
    let mut binding = registration.work_binding.clone().unwrap();
    binding.claim_fence += 1;
    binding.work_revision += 1;
    let store = registration.authority_store.as_ref().unwrap();
    rename_same_workspace_with_binding(state, parent, store, binding.clone());
    let root = &registration.named_root.as_ref().unwrap().workspace_id;
    let transport = install_sighting_transport(state, root, 2, json!({"state":"absent"}));
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
