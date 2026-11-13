// Current producer records must pass the same TermAl-side acknowledgement
// and notice boundaries as retained Commit records, without changing credit.
use super::*;

pub(super) fn owner_run(progress: &str) -> (AppState, String, String, String, TestRunRegistration) {
    let (state, parent, _store, named_root) = named_root_parent();
    let head = commit_named_root(&named_root);
    let root = worktree(&named_root);
    let source: EngramSourceId = serde_json::from_value(json!({
        "commit":head,
        "tree":run_git_test_command_output(&root, &["rev-parse", "HEAD^{tree}"]).trim(),
        "submodules":[], "changes":[]
    }))
    .unwrap();
    let registration = try_register_committed_run_with_source_as(
        &state,
        &parent,
        &named_root,
        COMMITTED_RUN_ID,
        committed_results("passed", input_record(&head, "clean")),
        Some(source),
    )
    .unwrap();
    let directory = FsPath::new(&registration.run_directory);
    // Model a zero-duration check after its actual registration. The shared
    // admission fixture's historical timestamps predate this receipt.
    let results_path = directory.join("results.json");
    let mut results: Value = serde_json::from_slice(&fs::read(&results_path).unwrap()).unwrap();
    results["ended"] = json!(registration.registered_at);
    results["stages"][0]["started"] = json!(registration.registered_at);
    results["stages"][0]["ended"] = json!(registration.registered_at);
    assert!(engram_verification_record_producer_refusal(&registration, &results).is_none());
    fs::write(results_path, serde_json::to_vec(&results).unwrap()).unwrap();
    let read = |name: &str| -> Value {
        serde_json::from_slice(&fs::read(directory.join(name)).unwrap()).unwrap()
    };
    // Use the NEW production writer, without the historical test helper.
    let body = engram_verification_record_body(
        &registration,
        &read("request.json"),
        &read("results.json"),
    )
    .unwrap();
    assert!(matches!(
        &body.input,
        EngramVerificationRecordInput::SourceId { .. }
    ));
    fs::write(
        directory.join(ENGRAM_VERIFICATION_RECORD_BODY_FILE),
        engram_verification_record_text(&body).unwrap(),
    )
    .unwrap();
    let mut observed = acknowledged(progress);
    observed["body_digest"] = json!(engram_verification_record_body_digest(&body));
    let ack: TestRunVerificationAck = serde_json::from_value(observed).unwrap();
    state.note_engram_verification_record_ack(&registration, ack);
    let current = current_registration(&state);
    (state, parent, named_root, head, current)
}

#[test]
fn new_source_record_passes_the_termal_acceptance_acknowledgement_boundary() {
    for progress in ["received", "settled"] {
        let (state, parent, _root, head, registration) = owner_run(progress);
        assert!(
            acceptance_committed_input_record_acknowledged(&registration, &head).is_ok(),
            "{progress}"
        );
        assert_eq!(
            decide(&state, &parent),
            Ok(AcceptanceCommittedInputDecision::Admitted(
                AcceptanceCommittedInputAdmission {
                    native_source: None,
                    run_id: COMMITTED_RUN_ID.into(),
                    commit: head
                }
            ))
        );
    }
}

#[test]
fn historical_commit_record_keeps_its_acceptance_acknowledgement() {
    let (state, parent, _store, named_root) = named_root_parent();
    let head = commit_named_root(&named_root);
    let registration = register_committed_run(
        &state,
        &parent,
        &named_root,
        committed_results("passed", input_record(&head, "clean")),
    );
    host_observed(&state, &registration, &head, acknowledged("settled"));
    let current = current_registration(&state);
    assert!(acceptance_committed_input_record_acknowledged(&current, &head).is_ok());
    assert_eq!(
        decide(&state, &parent),
        Ok(AcceptanceCommittedInputDecision::Admitted(
            AcceptanceCommittedInputAdmission {
                native_source: None,
                run_id: COMMITTED_RUN_ID.into(),
                commit: head
            }
        ))
    );
}

#[test]
fn new_source_record_keeps_its_received_check_notice() {
    let (_state, _parent, root, head, registration) = owner_run("settled");
    assert!(
        acceptance_committed_input_record_acknowledged(&registration, &head).is_ok(),
        "the settled fixture body and receipt still answer the run"
    );
    let disk = TestRunDisk::read(FsPath::new(&registration.run_directory), None).unwrap();
    assert!(
        test_run_registered_disk_matches(&registration, &disk),
        "the registered disk identity matches"
    );
    assert!(
        registered_closing_clean_input(&registration, &disk),
        "the actual clean-input gates admit the fixture"
    );
    let current =
        committed_source_status(&ReviewFreezeGit::new(FsPath::new(&root)).unwrap()).unwrap();
    assert_eq!(
        current.0, head,
        "the real notice source is still at its recorded commit"
    );
    assert!(
        current.1.is_empty(),
        "the real notice source is still clean: {:?}",
        current.1
    );
    assert!(
        ReceivedCheckNotice::read(
            &registration,
            std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET
        )
        .is_some(),
        "the SourceId record crosses the same settled-receipt notice boundary"
    );
}

#[test]
fn held_source_receipt_cannot_ignore_a_changed_post_run_classification() {
    let (_state, _parent, _root, head, registration) = owner_run("received");
    let path = FsPath::new(&registration.run_directory).join("results.json");
    let mut results: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    results["input"] = input_record(&head, "input_changed");
    assert!(
        TestRunCommittedInput::read(&results).is_some(),
        "the legacy classification is internally coherent"
    );
    fs::write(path, serde_json::to_vec(&results).unwrap()).unwrap();
    // A held receipt is rebuilt before the fresh-run refusal selector.
    // SourceId equality must not erase its original clean-run requirement.
    assert!(matches!(
        engram_verification_record_prepare(&registration),
        EngramVerificationRecordPrepared::Pending(EngramVerificationRecordPending::BodyMismatch)
    ));
}
