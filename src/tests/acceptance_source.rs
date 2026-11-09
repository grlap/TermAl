use super::*;

pub(super) fn identity() -> EngramSourceId {
    serde_json::from_value(json!({
        "commit": "a".repeat(40), "tree": "b".repeat(40), "changes": [{
            "path": "private-path.txt", "status": " M", "origin": null,
            "blob": "c".repeat(40), "mode": "100644"
        }], "submodules": [{ "path": "child", "commit": "d".repeat(40), "changes": [] }]
    }))
    .unwrap()
}

pub(super) fn install_owner(
    state: &AppState,
    project: &str,
    source: impl Fn(&FsPath) -> Option<EngramSourceId> + Send + Sync + 'static,
) {
    use crate::tests::engram_source_owner::{
        source_owner_fixture_binary, source_owner_output, source_owner_reply,
    };
    let binary = source_owner_fixture_binary(state.persistence_path.parent().unwrap());
    let mut inner = state.inner.lock().unwrap();
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
        let root = FsPath::new(&args[2]);
        let value = match source(root) {
            Some(source) => {
                let mut envelope =
                    crate::tests::engram_source_command::source_envelope(root, "not_clean");
                envelope["source"] = json!(format!("git-commit:{}", source.commit));
                envelope["tree"] = json!(source.tree);
                envelope["source_id"] = serde_json::to_value(source).unwrap();
                envelope
            }
            None => crate::tests::engram_source_command::source_envelope(root, "unavailable"),
        };
        Ok(source_owner_output(
            if value["status"] == "unavailable" {
                1
            } else {
                0
            },
            serde_json::to_vec(&value).unwrap(),
        ))
    }));
}

fn target() -> DelegationAcceptanceEvaluation {
    serde_json::from_value(json!({
        "workRef":"w-fixture", "mode":"independent_session", "acceptanceBasis":1,
        "evidenceBasis":2, "criteriaCount":1, "attemptKey":"fixture-attempt"
    }))
    .unwrap()
}

fn connection() -> EngramConnectionConfig {
    EngramConnectionConfig {
        binary_path: PathBuf::from("engram"),
        project_file: PathBuf::from("/repo/.engram-project"),
        home: PathBuf::from("/fixture-home"),
        project_root: PathBuf::from("/repo"),
        actor_id: "fixture/codex".into(),
        actor_context: None,
        session_id: "fixture-child".into(),
    }
}

fn write(source: EngramSourceId, path: &str) -> AcceptanceEvaluationOpenWrite {
    AcceptanceEvaluationOpenWrite {
        verdicts_digest: "fixture-verdicts".into(),
        args: vec![
            "work".into(),
            "evaluate".into(),
            "w-fixture".into(),
            "--source-id-file".into(),
            path.into(),
            "--json".into(),
        ],
        source_payload: Some(AcceptanceSourcePayload {
            source_id: source,
            source_file: None,
        }),
    }
}

fn shared_submit_fixture() -> (AppState, SteppedPersistWriter, String, String) {
    use crate::tests::engram_source_owner::{
        source_owner_fixture_binary, source_owner_output, source_owner_reply,
    };
    let (mut state, parent, delegation, child) = evaluator_fixture();
    let (root, project) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&parent).unwrap()];
        (
            PathBuf::from(&record.session.workdir),
            record.session.project_id.clone().unwrap(),
        )
    };
    init_git_document_test_repo(&root);
    let binary = source_owner_fixture_binary(state.persistence_path.parent().unwrap());
    {
        let mut inner = state.inner.lock().unwrap();
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram
            .as_mut()
            .unwrap()
            .binary_path = Some(binary.to_string_lossy().into_owned());
        inner.test_observation_source_owner = Some(Arc::new(|_, _, args, _, _| {
            if args[0] == "--version" {
                return Ok(source_owner_reply(args));
            }
            let mut envelope = crate::tests::engram_source_command::source_envelope(
                FsPath::new(&args[2]),
                "not_clean",
            );
            envelope["source_id"] = serde_json::to_value(identity()).unwrap();
            Ok(source_owner_output(
                0,
                serde_json::to_vec(&envelope).unwrap(),
            ))
        }));
    }
    update_evaluation_target(&state, &delegation, |target| {
        target.source_id = Some(identity())
    });
    let target = {
        let inner = state.inner.lock().unwrap();
        inner.delegations[inner.find_delegation_index(&delegation).unwrap()]
            .acceptance_evaluation
            .clone()
            .unwrap()
    };
    assert!(
        state
            .acceptance_evaluation_declared_target(&child, &target)
            .unwrap()
            .source_id
            .is_some(),
        "the submit fixture must exercise shared recapture rather than undeclared fallback"
    );
    let writer = SteppedPersistWriter::attach(&mut state);
    (state, writer, delegation, child)
}

fn read_as_source_consumer(path: &FsPath) -> Vec<u8> {
    use std::io::Read;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    let mut file = options
        .open(path)
        .expect("the consumer can open the immutable source");
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    bytes
}

#[cfg(windows)]
#[test]
fn fresh_source_admits_the_windows_consumer_but_denies_writing_and_deletion() {
    let root = TestTempRoot::create("acceptance-source-reader");
    let payload = AcceptanceSourcePayload {
        source_id: identity(),
        source_file: None,
    };
    let guard = payload.publish(root.path()).unwrap();
    assert_eq!(
        read_as_source_consumer(guard.path()),
        acceptance_source_bytes(&identity()).unwrap()
    );
    assert!(
        fs::OpenOptions::new()
            .write(true)
            .open(guard.path())
            .is_err()
    );
    assert!(fs::remove_file(guard.path()).is_err());
    guard
        .verify(&acceptance_source_bytes(&identity()).unwrap())
        .unwrap();
    let path = guard.path().to_owned();
    guard.terminal();
    drop(guard);
    assert!(!path.exists());
}

#[test]
fn shared_submit_withdrawal_cleans_only_after_durable_ack_and_replays_after_failed_ack() {
    for locked in [false, true] {
        for fail_withdrawal in [false, true] {
            let (state, mut writer, delegation, child) = shared_submit_fixture();
            let path_seen = Arc::new(Mutex::new(None::<PathBuf>));
            std::thread::scope(|scope| {
                let (submitting, evaluator, seen) =
                    (state.clone(), child.clone(), path_seen.clone());
                let submit = scope.spawn(move || {
                    submitting.submit_acceptance_evaluation_with_runner(
                        &evaluator,
                        two_verdicts(),
                        |_, args, _| {
                            let index = args
                                .iter()
                                .position(|arg| arg == "--source-id-file")
                                .unwrap();
                            let path = PathBuf::from(&args[index + 1]);
                            assert!(
                                read_as_source_consumer(&path)
                                    == acceptance_source_bytes(&identity()).unwrap()
                            );
                            *seen.lock().unwrap() = Some(path);
                            if locked {
                                locked_store()
                            } else {
                                never_started()
                            }
                        },
                    )
                });
                writer.receive_fence();
                writer.write();
                writer.receive_fence();
                let path = path_seen.lock().unwrap().clone().unwrap();
                assert!(
                    path.exists(),
                    "a queued withdrawal cannot authorize cleanup"
                );
                assert!(!submit.is_finished());
                assert!(matches!(
                    durable_submission(&writer.path, &delegation),
                    Some(AcceptanceEvaluationSubmission::Pending { .. })
                ));
                if fail_withdrawal {
                    writer.fail_write();
                } else {
                    writer.write();
                }
                let error = submit.join().unwrap().unwrap_err();
                if fail_withdrawal {
                    assert!(error.message.contains("withdrawal is unconfirmed"));
                    assert!(
                        error.message.contains(if locked {
                            "database is locked"
                        } else {
                            "program not found"
                        }),
                        "the original send failure must survive withdrawal failure"
                    );
                    assert!(
                        path.exists(),
                        "restart still needs the durable Pending locator"
                    );
                } else {
                    assert_eq!(error.status, StatusCode::BAD_GATEWAY);
                    assert!(!path.exists());
                    assert_eq!(
                        durable_submission(&writer.path, &delegation),
                        Some(AcceptanceEvaluationSubmission::None)
                    );
                }
            });
            if !fail_withdrawal {
                continue;
            }
            let path = path_seen.lock().unwrap().clone().unwrap();
            // All submitting leases are gone. Restore the target from SQLite,
            // as boot does, then exercise the real identical replay path.
            let restored = load_state(&writer.path).unwrap().unwrap();
            let target = restored
                .delegations
                .iter()
                .find(|record| record.id == delegation)
                .unwrap()
                .acceptance_evaluation
                .clone()
                .unwrap();
            assert!(matches!(
                target.submission,
                AcceptanceEvaluationSubmission::Pending { .. }
            ));
            update_evaluation_target(&state, &delegation, |current| *current = target);
            std::thread::scope(|scope| {
                let (submitting, evaluator, original_path) =
                    (state.clone(), child.clone(), path.clone());
                let replay = scope.spawn(move || {
                    submitting.submit_acceptance_evaluation_with_runner(
                        &evaluator,
                        two_verdicts(),
                        |_, args, _| {
                            let index = args
                                .iter()
                                .position(|arg| arg == "--source-id-file")
                                .unwrap();
                            assert_eq!(FsPath::new(&args[index + 1]), original_path);
                            assert!(
                                fs::read(&original_path).unwrap()
                                    == acceptance_source_bytes(&identity()).unwrap()
                            );
                            Ok(cli_output(true, &evaluate_receipt(true), ""))
                        },
                    )
                });
                writer.receive_fence();
                writer.write();
                writer.receive_fence();
                assert!(path.exists());
                writer.write();
                replay.join().unwrap().unwrap();
            });
            assert!(
                !path.exists(),
                "durably recorded replay releases its original file"
            );
        }
    }
}

#[test]
fn shared_submit_begin_failure_preserves_the_durable_pending_file() {
    let (state, mut writer, delegation, child) = shared_submit_fixture();
    std::thread::scope(|scope| {
        let (submitting, evaluator) = (state.clone(), child.clone());
        let submit = scope.spawn(move || {
            submitting.submit_acceptance_evaluation_with_runner(
                &evaluator,
                two_verdicts(),
                runner_must_not_run,
            )
        });
        writer.receive_fence();
        // SQLite can contain Pending even when its acknowledgement is lost.
        persist_state(&writer.path, &state.inner.lock().unwrap()).unwrap();
        let delta = collect_persist_delta_from_shared_state(&writer.inner, writer.watermark);
        assert!(
            persist_delta_with_fences_using(&delta, &mut writer.batch, || {
                Err(anyhow!("distinct admission acknowledgement failure"))
            })
            .is_err()
        );
        writer.receive_fence();
        writer.fail_write();
        let error = submit.join().unwrap().unwrap_err();
        assert!(error.message.contains("withdrawal is unconfirmed"));
        assert!(
            error
                .message
                .contains("distinct admission acknowledgement failure"),
            "preserve the first admission reason as well as the withdrawal reason"
        );
    });
    let Some(AcceptanceEvaluationSubmission::Pending {
        payload_digest,
        original,
        ..
    }) = durable_submission(&writer.path, &delegation)
    else {
        panic!("durable Pending must survive");
    };
    // The failed begin released every live lease. Restart replay must reopen
    // the exact persisted locator, rather than publish a replacement.
    let directory = engram_host_workdir(state.persistence_path.as_ref())
        .unwrap()
        .join("evaluation-sources");
    let file = acceptance_source_replay_file(&original, &payload_digest, None, &directory)
        .unwrap()
        .unwrap();
    assert!(file.path().exists());
    file.terminal();
}

#[test]
fn source_alternatives_refuse_missing_null_mixed_and_malformed_identity() {
    let source = identity();
    let shared = json!({"source_id":source});
    assert!(serde_json::from_value::<AcceptanceSourceBasis>(shared.clone()).is_ok());
    assert!(
        serde_json::from_value::<AcceptanceSourceBasis>(
            json!({"fingerprint":"content-v1:historical"})
        )
        .is_ok()
    );
    for invalid in [
        json!({}),
        json!({"fingerprint":null}),
        json!({"source_id":null}),
        json!({"source_id":source, "fingerprint":"historical"}),
        json!({"source_id":source, "extra":true}),
        json!({"source_id":{"commit":"a".repeat(40)}}),
    ] {
        assert!(serde_json::from_value::<AcceptanceSourceBasis>(invalid).is_err());
    }
}

#[tokio::test]
async fn shared_withdrawal_deadline_keeps_the_typed_response_original_file_and_replay() {
    let (state, mut writer, delegation, child) = shared_submit_fixture();
    let error = std::thread::scope(|scope| {
        let (submitting, evaluator) = (state.clone(), child.clone());
        let submit = scope.spawn(move || {
            submitting.submit_acceptance_evaluation_with_runner(
                &evaluator,
                two_verdicts(),
                |_, _, _| never_started(),
            )
        });
        writer.receive_fence();
        writer.write();
        writer.receive_fence();
        assert!(matches!(
            writer.batch.pending[0]
                .completion
                .poll_at(writer.batch.pending[0].completion.deadline),
            Some(Err(PersistFenceError::Deadline))
        ));
        submit.join().unwrap().unwrap_err()
    });
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(error.durability_deadline.is_some());
    let pending = durable_submission(&writer.path, &delegation).unwrap();
    assert_eq!(submission_of(&state, &delegation), pending);
    let AcceptanceEvaluationSubmission::Pending { original, .. } = pending else {
        panic!("the durable original must remain open after withdrawal times out");
    };
    let path = original
        .source_payload
        .as_ref()
        .unwrap()
        .source_file
        .as_ref()
        .unwrap()
        .clone();
    assert!(path.exists());
    let response = error.into_response();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let envelope: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        durability_pending_for_request(
            status,
            "POST",
            &format!("/api/sessions/{child}/acceptance-evaluation"),
            &child,
            &json!({"schemaVersion":1}),
            &envelope
        )
        .is_some()
    );
    std::thread::scope(|scope| {
        let (submitting, evaluator, original_path) = (state.clone(), child.clone(), path.clone());
        let replay = scope.spawn(move || {
            submitting.submit_acceptance_evaluation_with_runner(
                &evaluator,
                two_verdicts(),
                |_, args, _| {
                    let index = args
                        .iter()
                        .position(|arg| arg == "--source-id-file")
                        .unwrap();
                    assert_eq!(FsPath::new(&args[index + 1]), original_path);
                    assert!(
                        fs::read(&original_path).unwrap()
                            == acceptance_source_bytes(&identity()).unwrap()
                    );
                    Ok(cli_output(true, &evaluate_receipt(true), ""))
                },
            )
        });
        writer.receive_fence();
        writer.write();
        writer.receive_fence();
        writer.write();
        replay.join().unwrap().unwrap();
    });
    assert!(
        !path.exists(),
        "only the acknowledged replay receipt authorizes removal"
    );
}

#[test]
fn complete_dirty_identity_matches_independently_of_workspace_and_never_legacy() {
    let a: AcceptanceSourceBasis =
        serde_json::from_value(json!({"workspace_id":"first", "source_id":identity()})).unwrap();
    let b: AcceptanceSourceBasis =
        serde_json::from_value(json!({"workspace_id":"second", "source_id":identity()})).unwrap();
    assert!(a.same_source(&b));
    let mut changed = identity();
    changed.submodules[0].commit = "e".repeat(40);
    let changed: AcceptanceSourceBasis =
        serde_json::from_value(json!({"source_id":changed})).unwrap();
    assert!(!a.same_source(&changed));
    let legacy: AcceptanceSourceBasis =
        serde_json::from_value(json!({"fingerprint":format!("git-commit:{}", identity().commit)}))
            .unwrap();
    assert!(!a.same_source(&legacy));
}

#[test]
fn private_verification_identity_is_complete_and_never_reconstructed_from_a_summary() {
    let valid = json!({"workspace_id":"fixture-workspace", "source_id":identity(),
        "source_root_generation":3});
    let parsed: AcceptanceVerificationSourceBasis = serde_json::from_value(valid.clone()).unwrap();
    let expected = AcceptanceSourceBasis::Shared(AcceptanceSharedSourceBasis {
        workspace_id: None,
        source_id: identity(),
    });
    assert!(parsed.identity.same_source(&expected));
    assert_eq!(parsed.workspace_id, "fixture-workspace");
    assert_eq!(parsed.source_root_generation, Some(3));
    for key in [
        "source_revision",
        "git_tree",
        "source_id_summary",
        "unknown",
    ] {
        let mut mixed = valid.clone();
        mixed[key] = if key == "git_tree" {
            Value::Null
        } else {
            json!("not an alternative")
        };
        assert!(serde_json::from_value::<AcceptanceVerificationSourceBasis>(mixed).is_err());
    }
    let legacy: AcceptanceVerificationSourceBasis = serde_json::from_value(json!({
        "workspace_id":"fixture-workspace", "source_revision":"content-v1:historical", "git_tree":"a".repeat(40)
    })).unwrap();
    assert!(!legacy.identity.same_source(&expected));
    let mut changed = identity();
    changed.submodules[0].commit = "e".repeat(40);
    let changed: AcceptanceVerificationSourceBasis = serde_json::from_value(json!({
        "workspace_id":"fixture-workspace", "source_id":changed
    }))
    .unwrap();
    assert!(!parsed.identity.same_source(&changed.identity));
}

#[test]
fn evaluator_markers_compare_the_complete_private_identity_and_never_display_labels() {
    let mut task = parse_acceptance_evaluation_task(show_receipt(None), full_receipt()).unwrap();
    let basis = AcceptanceSourceBasis::Shared(AcceptanceSharedSourceBasis {
        workspace_id: None,
        source_id: identity(),
    });
    task.judged_source_basis = Some(basis.clone());
    task.judged_revision = Some("same display label".into());
    let mut evidence = AcceptanceEvaluationEvidence {
        locator: "aaaaaaaa1111".into(),
        kind: "verification".into(),
        by: None,
        created_at: None,
        summary: Some("recorded fixture test".into()),
        body_bytes: None,
        non_holder: false,
        cut_by_tracker: false,
        verification: Some(AcceptanceEvidenceVerification {
            source_basis: Some(basis),
            applicability_tree: None,
            check_kind: "test".into(),
            result: "passed".into(),
            source_revision: Some("same display label".into()),
            source_workspace: None,
        }),
    };
    assert!(acceptance_brief_off_source_marker(&evidence, task.judged_source()).is_empty());
    evidence.verification.as_mut().unwrap().applicability_tree = Some(identity().tree);
    assert!(acceptance_brief_off_source_marker(&evidence, task.judged_source()).is_empty());
    evidence.verification.as_mut().unwrap().applicability_tree = Some("f".repeat(40));
    assert!(
        acceptance_brief_off_source_marker(&evidence, task.judged_source())
            .contains("Git tree restriction")
    );
    evidence.verification.as_mut().unwrap().applicability_tree = None;
    if let Some(AcceptanceSourceBasis::Shared(shared)) = evidence
        .verification
        .as_mut()
        .unwrap()
        .source_basis
        .as_mut()
    {
        shared.source_id.changes[0].blob = Some("e".repeat(40));
    }
    let mismatch = acceptance_brief_off_source_marker(&evidence, task.judged_source());
    assert!(
        mismatch.contains("context only"),
        "same commit and display label cannot hide a changed dirty blob"
    );
    assert!(!mismatch.contains("private-path.txt"));
    evidence.verification.as_mut().unwrap().source_basis = None;
    assert!(
        acceptance_brief_off_source_marker(&evidence, task.judged_source())
            .contains("context only"),
        "a window summary cannot provide a complete comparison identity"
    );
}

#[test]
fn target_reload_keeps_the_full_private_object_and_historical_arguments() {
    let mut shared = target();
    shared.source_id = Some(identity());
    shared.submission = AcceptanceEvaluationSubmission::Pending {
        payload_digest: "fixture-digest".into(),
        started_at: "fixture-time".into(),
        original: write(identity(), "private-file"),
    };
    let stored = serde_json::to_value(&shared).unwrap();
    let reloaded: DelegationAcceptanceEvaluation = serde_json::from_value(stored.clone()).unwrap();
    assert_eq!(reloaded, shared);
    let public = serde_json::to_value(reloaded.client_view()).unwrap();
    assert!(public.get("sourceId").is_none());
    assert!(public["submission"].get("sourcePayload").is_none());
    assert!(public["submission"].get("args").is_none());
    assert!(!format!("{:?}", shared.source_id).contains("private-path.txt"));
    let mut mixed = stored.clone();
    mixed["sourceFingerprint"] = json!("content-v1:historical");
    assert!(serde_json::from_value::<DelegationAcceptanceEvaluation>(mixed).is_err());
    let mut null = stored;
    null["sourceId"] = Value::Null;
    assert!(serde_json::from_value::<DelegationAcceptanceEvaluation>(null).is_err());
    let mut old = target();
    old.source_fingerprint = Some("content-v1:historical".into());
    let old_bytes = serde_json::to_vec(&old).unwrap();
    let restored: DelegationAcceptanceEvaluation = serde_json::from_slice(&old_bytes).unwrap();
    assert_eq!(serde_json::to_vec(&restored).unwrap(), old_bytes);
    let original = AcceptanceEvaluationOpenWrite {
        verdicts_digest: String::new(),
        args: vec![
            "--source-fingerprint".into(),
            "content-v1:historical".into(),
        ],
        source_payload: None,
    };
    assert_eq!(
        original.digest().unwrap(),
        acceptance_evaluation_payload_digest(&original.args)
    );
}

#[test]
fn payload_identity_excludes_file_location_and_preserves_every_source_field() {
    let a = write(identity(), "first-file");
    let b = write(identity(), "other-file");
    assert_eq!(a.digest().unwrap(), b.digest().unwrap());
    let mut changed = identity();
    changed.changes[0].blob = Some("e".repeat(40));
    assert_ne!(
        a.digest().unwrap(),
        write(changed, "first-file").digest().unwrap()
    );
    let mut ordinary = b.clone();
    ordinary.args.push("--model".into());
    ordinary.args.push("vendor/model".into());
    assert_ne!(a.digest().unwrap(), ordinary.digest().unwrap());
    let mut mixed = a;
    mixed.args.push("--source-fingerprint".into());
    mixed.args.push("legacy".into());
    assert!(mixed.digest().is_err());
}

#[test]
fn source_file_stays_until_terminal_and_last_consumer() {
    let root = TestTempRoot::create("acceptance-source-custody");
    let payload = AcceptanceSourcePayload {
        source_id: identity(),
        source_file: None,
    };
    let file = payload.publish(&root).unwrap();
    let path = file.path().to_path_buf();
    let consumer = file.consumer();
    let args = write(identity(), "template")
        .source_file_args(&file)
        .unwrap();
    assert_eq!(args[4], path.to_str().unwrap());
    assert_eq!(
        fs::read(&path).unwrap(),
        acceptance_source_bytes(&payload.source_id).unwrap()
    );
    file.terminal();
    drop(file);
    assert!(path.exists());
    drop(consumer);
    assert!(!path.exists());
}

#[test]
fn uncertain_file_is_retained_and_changed_bytes_are_refused_before_submission() {
    let root = TestTempRoot::create("acceptance-source-uncertain");
    let mut payload = AcceptanceSourcePayload {
        source_id: identity(),
        source_file: None,
    };
    let file = payload.publish(&root).unwrap();
    let path = file.path().to_path_buf();
    payload.source_file = Some(path.clone());
    drop(file);
    assert!(
        path.exists(),
        "unknown outcomes retain their immutable locator"
    );
    drop(payload.reopen(&root).unwrap());
    // Recovery uses the saved object and locator after the old process has
    // released its read-only custody; it must not accept replaced bytes.
    fs::write(&path, b"!").unwrap();
    let error = payload
        .reopen(&root)
        .err()
        .expect("corrupted replay is refused");
    assert!(error.message.contains("changed before submission"));
    assert!(
        path.exists(),
        "a refused replay does not rewrite or remove its input"
    );
    fs::remove_file(path).unwrap();
}

#[test]
fn large_source_uses_a_small_file_argument_and_oversize_identity_is_refused() {
    let root = TestTempRoot::create("acceptance-source-capacity");
    let mut source = identity();
    source.changes[0].path = "p".repeat(800_000);
    let original = write(source.clone(), "template");
    let file = original
        .source_payload
        .as_ref()
        .unwrap()
        .publish(&root)
        .unwrap();
    let args = original.source_file_args(&file).unwrap();
    assert!(args.iter().map(String::len).sum::<usize>() < 4096);
    assert!(fs::metadata(file.path()).unwrap().len() > ENGRAM_CONTROL_MAX_FRAME_BYTES as u64);
    file.terminal();
    source.changes[0].path = "p".repeat(ACCEPTANCE_SOURCE_ID_MAX_BYTES);
    assert!(acceptance_source_bytes(&source).is_err());
}

#[test]
fn private_evidence_response_capacity_does_not_expand_unrelated_reads() {
    let request = EngramControlRequest::AcceptanceVerificationRead {
        work_id: "fixture".into(),
        expected_work_revision: 1,
        run_id: "run".into(),
        run_cut: 2,
        criterion: 1,
        after: None,
    };
    let private = engram_control_response_frame_limit(&request);
    assert!(private > ENGRAM_CONTROL_MAX_FRAME_BYTES);
    let reply =
        serde_json::to_vec(&json!({"status":"ok", "result":{"rows":["x".repeat(300_000)]}}))
            .unwrap();
    assert!(
        exchange_engram_control_frame_with_limit(
            &mut Vec::new(),
            &mut std::io::Cursor::new(&reply),
            b"{}",
            private
        )
        .is_ok()
    );
    assert!(
        exchange_engram_control_frame(&mut Vec::new(), &mut std::io::Cursor::new(&reply), b"{}")
            .is_err()
    );
    let mut cursor = std::io::Cursor::new(vec![b'x'; private + 100]);
    assert!(
        exchange_engram_control_frame_with_limit(&mut Vec::new(), &mut cursor, b"{}", private)
            .is_err()
    );
    assert_eq!(cursor.position(), (private + 1) as u64);
    assert_eq!(
        engram_control_response_frame_limit(&EngramControlRequest::NamedRootSightingRead {
            work_ref: "fixture".into(),
            run_id: "run".into(),
            run_cut: Some(2)
        }),
        ENGRAM_CONTROL_MAX_FRAME_BYTES
    );
}

#[test]
fn sighting_summary_is_presence_only_and_refuses_mixed_or_complete_identity() {
    let shared = json!({"state":"present", "record":"fixture", "position":3,
        "source_id_summary":{"commit":"a".repeat(40),"tree":"b".repeat(40),"changes":1,"submodules":1}});
    let parsed: EngramRootSighting = serde_json::from_value(shared.clone()).unwrap();
    assert!(matches!(
        parsed,
        EngramRootSighting::PresentShared { position: 3, .. }
    ));
    assert_eq!(serde_json::to_value(parsed).unwrap(), shared);
    let mut mixed = shared.clone();
    mixed["revision"] = json!("legacy");
    assert!(serde_json::from_value::<EngramRootSighting>(mixed).is_err());
    let mut full = shared;
    full["source_id"] = json!(identity());
    assert!(serde_json::from_value::<EngramRootSighting>(full).is_err());
}

#[test]
fn shared_sighting_whole_answer_preserves_the_cut_fence_and_closed_shape() {
    let local = AcceptanceEvaluationSourceRoot {
        root: "fixture-root".into(),
        common_dir_key: "fixture-common".into(),
        work_id: "fixture-work".into(),
        claim_id: "fixture-claim".into(),
        generation: 1,
    };
    let query = AcceptanceSightingQuery {
        work_ref: "fixture-work",
        project_id: "fixture-project",
        work_id: "fixture-work",
        run_id: "fixture-run",
        cut: 42,
        root: Some(&local),
    };
    let value = json!({
        "schema_version":1, "project_id":"fixture-project", "work_id":"fixture-work",
        "run_id":"fixture-run", "read_cut":42, "head_cut":42,
        "current_binding":"fixture-binding", "binding_changed":false,
        "root":{"state":"bound", "workspace_id":"fixture-root", "generation":1,
            "binding_event":"fixture-binding", "binding_position":1,
            "sighting":{"state":"present", "record":"fixture-sighting", "position":42,
                "source_id_summary":{"commit":"a".repeat(40), "tree":"b".repeat(40), "changes":1, "submodules":1}}}
    });
    let parsed = parse_engram_named_root_sighting_read(value.clone()).unwrap();
    assert!(parsed.incoherence(&query).is_none());
    acceptance_sighting_decision(&parsed, &query, None).unwrap();
    let mut beyond_cut = value.clone();
    beyond_cut["root"]["sighting"]["position"] = json!(43);
    assert!(
        parse_engram_named_root_sighting_read(beyond_cut)
            .unwrap()
            .incoherence(&query)
            .is_some()
    );
    for extra in ["revision", "source_id", "unknown"] {
        let mut invalid = value.clone();
        invalid["root"]["sighting"][extra] = json!("not part of the sighting contract");
        assert!(parse_engram_named_root_sighting_read(invalid).is_err());
    }
}

#[test]
fn verification_request_guard_counts_the_actual_newline_and_keeps_other_limits() {
    let mut request = EngramControlRequest::VerificationRecord {
        operation_key: "fixture".into(),
        record: json!({"padding":""}),
    };
    let base = serde_json::to_vec(&request).unwrap().len();
    if let EngramControlRequest::VerificationRecord { record, .. } = &mut request {
        record["padding"] = json!("x".repeat(ACCEPTANCE_VERIFICATION_FRAME_MAX_BYTES - base - 1));
    }
    let encoded = encode_engram_control_request(&request).unwrap();
    let mut wire = Vec::new();
    exchange_engram_control_frame(
        &mut wire,
        &mut std::io::Cursor::new(b"{\"status\":\"ok\",\"result\":{}}\n"),
        &encoded,
    )
    .unwrap();
    assert_eq!(wire.len(), ACCEPTANCE_VERIFICATION_FRAME_MAX_BYTES);
    assert_eq!(wire.last(), Some(&b'\n'));
    if let EngramControlRequest::VerificationRecord { record, .. } = &mut request {
        record["padding"] = json!("x".repeat(ACCEPTANCE_VERIFICATION_FRAME_MAX_BYTES - base));
    }
    let refused = encode_engram_control_request(&request).unwrap_err();
    assert!(refused.request_never_sent);
    let unrelated = EngramControlRequest::NamedRootRead {
        run_id: "x".repeat(300_000),
        claim_id: "fixture".into(),
    };
    assert!(
        encode_engram_control_request(&unrelated)
            .unwrap_err()
            .request_never_sent
    );
}

#[test]
fn restarted_attempt_reopens_exact_immutable_bytes_and_rejects_another_location() {
    let root = TestTempRoot::create("acceptance-source-restart");
    let mut payload = AcceptanceSourcePayload {
        source_id: identity(),
        source_file: None,
    };
    let file = payload.publish(&root).unwrap();
    payload.source_file = Some(file.path().to_path_buf());
    drop(file);
    let saved = serde_json::to_vec(&payload).unwrap();
    let recovered: AcceptanceSourcePayload = serde_json::from_slice(&saved).unwrap();
    let first = recovered.reopen(&root).unwrap();
    let second = recovered.reopen(&root).unwrap();
    first
        .verify(&acceptance_source_bytes(&identity()).unwrap())
        .unwrap();
    first.terminal();
    drop(first);
    assert!(second.path().exists());
    let retained = second.path().to_path_buf();
    second.terminal();
    drop(second);
    assert!(!retained.exists());
    let other = TestTempRoot::create("acceptance-source-other-storage");
    assert!(recovered.reopen(&other).is_err());
}

#[test]
fn shared_command_uses_a_file_template_and_same_session_preparation_is_explicitly_unsupported() {
    let mut shared = target();
    shared.source_id = Some(identity());
    let request = two_verdicts();
    let shared_args =
        acceptance_evaluation_cli_args(&connection(), &shared, &request, None).unwrap();
    assert!(shared_args.iter().any(|arg| arg == "--source-id-file"));
    assert!(!shared_args.iter().any(|arg| arg == "--source-fingerprint"));
    assert!(
        !shared_args
            .iter()
            .any(|arg| arg.contains("private-path.txt"))
    );
    shared.mode = AcceptanceEvaluationMode::SameSession;
    let error = acceptance_evaluation_cli_args(&connection(), &shared, &request, None).unwrap_err();
    assert!(error.message.contains("persisted host attempt"));
    shared.source_id = None;
    shared.source_fingerprint = Some("content-v1:historical".into());
    assert!(acceptance_evaluation_cli_args(&connection(), &shared, &request, None).is_ok());
}

#[test]
fn unknown_submission_replays_saved_bytes_after_reload_and_cleans_only_after_receipt() {
    let root = TestTempRoot::create("acceptance-source-submit-replay");
    let mut shared = target();
    shared.source_id = Some(identity());
    let (payload, file) = acceptance_source_candidate(&shared, true, &root).unwrap();
    let mut original = write(identity(), "template");
    original.source_payload = payload;
    let digest = original.digest().unwrap();
    let file = acceptance_source_replay_file(&original, &digest, file, &root)
        .unwrap()
        .unwrap();
    let saved = serde_json::to_vec(&original).unwrap();
    let path = file.path().to_path_buf();
    let seen = std::cell::Cell::new(0);
    let run = |_: &EngramConnectionConfig, args: &[String], _: Duration| {
        seen.set(seen.get() + 1);
        let index = args
            .iter()
            .position(|arg| arg == "--source-id-file")
            .unwrap();
        let actual: EngramSourceId =
            serde_json::from_slice(&fs::read(&args[index + 1]).unwrap()).unwrap();
        assert_eq!(actual, identity());
        response_lost()
    };
    assert!(matches!(
        run_acceptance_source_payload(&connection(), &original, Some(&file), &run),
        AcceptanceEvaluationRunOutcome::Unknown(_)
    ));
    assert_eq!(seen.get(), 1);
    drop(file);
    assert!(path.exists());
    let restored: AcceptanceEvaluationOpenWrite = serde_json::from_slice(&saved).unwrap();
    let file = acceptance_source_replay_file(&restored, &digest, None, &root)
        .unwrap()
        .unwrap();
    assert_eq!(file.path(), path);
    let result =
        run_acceptance_source_payload(&connection(), &restored, Some(&file), &|_, args, _| {
            let index = args
                .iter()
                .position(|arg| arg == "--source-id-file")
                .unwrap();
            assert_eq!(
                fs::read(&args[index + 1]).unwrap(),
                acceptance_source_bytes(&identity()).unwrap()
            );
            Ok(cli_output(true, &evaluate_receipt(true), ""))
        });
    assert!(matches!(result, AcceptanceEvaluationRunOutcome::Receipt(_)));
    file.terminal();
    drop(file);
    assert!(!path.exists());
}

#[test]
fn payload_digest_mismatch_and_missing_custody_never_invoke_the_runner() {
    let root = TestTempRoot::create("acceptance-source-digest-refusal");
    let mut shared = target();
    shared.source_id = Some(identity());
    let (payload, file) = acceptance_source_candidate(&shared, true, &root).unwrap();
    let mut original = write(identity(), "template");
    original.source_payload = payload;
    let file = file.unwrap();
    assert!(
        acceptance_source_replay_file(
            &original,
            "not-the-payload-digest",
            Some(file.consumer()),
            &root
        )
        .is_err()
    );
    let calls = std::cell::Cell::new(0);
    let result = run_acceptance_source_payload(&connection(), &original, None, &|_, _, _| {
        calls.set(calls.get() + 1);
        Ok(cli_output(true, &evaluate_receipt(true), ""))
    });
    assert!(matches!(
        result,
        AcceptanceEvaluationRunOutcome::NeverStarted(_)
    ));
    assert_eq!(calls.get(), 0);
    file.terminal();
}
