// Real launcher inside the pinned disposable producer fixture. Copies the
// candidate's tracked scripts at runtime; owns no launcher or credit policy.
use super::*;

#[path = "engram_registered_launcher_environment.rs"]
mod environment;
use environment::{LAUNCH, isolated_launcher};

#[path = "engram_evaluation_matrix_live.rs"]
mod evaluation_matrix;

pub(super) fn install_launcher(fixture: &CompletionFixture) {
    environment::install_launcher(fixture);
}

struct LoopbackRegistration {
    runtime: tokio::runtime::Runtime,
    server: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    url: String,
    temporary: PathBuf,
}

impl LoopbackRegistration {
    fn start(state: &AppState) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let router = app_router(state.clone());
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = runtime.spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
        });
        Self {
            runtime,
            server: Some(server),
            stop: Some(stop),
            url,
            temporary: state
                .test_temp_root
                .as_ref()
                .expect("the disposable host owns its scratch")
                .path()
                .join("launcher-tmp"),
        }
    }
}

impl Drop for LoopbackRegistration {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(server) = self.server.take() {
            if let Err(_) = self.runtime.block_on(server) {
                eprintln!("fixture registration server failed to join: <details withheld>");
            }
        }
    }
}

pub(super) fn run_check(
    fixture: &CompletionFixture,
    session: &str,
    root: &FsPath,
) -> (
    EngramVerificationRecordBody,
    EngramVerificationRecordReceipt,
) {
    run_check_in_lane(fixture, session, root, false)
}

pub(super) fn run_historical_check(
    fixture: &CompletionFixture,
    session: &str,
    root: &FsPath,
) -> (
    EngramVerificationRecordBody,
    EngramVerificationRecordReceipt,
) {
    run_check_in_lane(fixture, session, root, true)
}

fn run_check_in_lane(
    fixture: &CompletionFixture,
    session: &str,
    root: &FsPath,
    historical: bool,
) -> (
    EngramVerificationRecordBody,
    EngramVerificationRecordReceipt,
) {
    assert!(run_git_test_command_output(root, &["status", "--porcelain"]).is_empty());
    let before = run_git_test_command_output(root, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let service = LoopbackRegistration::start(&fixture.live.state);
    let key = format!("real-launcher-{}", Uuid::new_v4());
    let mut recorder = SessionRecorder::new(fixture.live.state.clone(), session.to_owned());
    recorder
        .command_started_in(&key, LAUNCH, Some(LAUNCH), Some(&root.to_string_lossy()))
        .unwrap();
    fixture.wait_for_check_capture(session);
    let output = isolated_launcher(root, session, &service)
        .output()
        .expect("actual Node launcher starts");
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "actual launcher setup/check failed: {text}"
    );
    let directory = text
        .lines()
        .find_map(|line| line.strip_prefix("RUN "))
        .map(PathBuf::from)
        .expect("actual launcher must print its original RUN locator");
    let expected_parent = EngramHost::run_artifact_directory(root).unwrap();
    assert_eq!(
        fs::canonicalize(directory.parent().unwrap()).unwrap(),
        fs::canonicalize(expected_parent).unwrap(),
        "inner run cannot write an outer launcher's review-runs"
    );
    let request: Value =
        serde_json::from_slice(&fs::read(directory.join("request.json")).unwrap()).unwrap();
    let results: Value =
        serde_json::from_slice(&fs::read(directory.join("results.json")).unwrap()).unwrap();
    assert_eq!(request["owner"], session);
    assert_eq!(request["input"], "commit");
    assert!(request.get("notifyTo").is_none());
    assert_eq!(
        test_run_path_key(request["root"].as_str().unwrap()),
        test_run_path_key(&test_run_display_path(root))
    );
    assert_eq!(
        results["state"], "passed",
        "the actual registered check passed"
    );
    assert_eq!(results["input"]["commit"], before);
    assert_eq!(results["input"]["outcome"], "clean");
    assert_eq!(results["input"]["eligible"], true);
    assert_eq!(
        results["stages"][0]["tests"]["runner"],
        if cfg!(windows) {
            "node-test"
        } else {
            "cargo-libtest"
        }
    );
    assert_eq!(results["stages"][0]["tests"]["passed"], 1);
    assert_eq!(results["stages"][0]["tests"]["failed"], 0);
    assert_eq!(results["stages"][0]["tests"]["ignored"], 0);
    recorder
        .command_completed_with_exit(
            &key,
            LAUNCH,
            &text,
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    fixture.wait_for_check_capture(session);
    let run_id = request["runId"].as_str().unwrap();
    let registration = fixture
        .live
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations[run_id]
        .clone();
    assert_eq!(registration.owning_session_id, session);
    assert_eq!(
        registration.work_binding.as_ref(),
        Some(&fixture.claim_binding(session))
    );
    assert_eq!(
        registration.authority_store.as_ref(),
        Some(&fixture.store())
    );
    let own_row = |state: &AppState| {
        state
            .inner
            .lock()
            .unwrap()
            .test_run_observations
            .values()
            .filter(|row| {
                row.summary.owner_session_id.as_deref() == Some(session)
                    && row
                        .artifact
                        .as_ref()
                        .is_some_and(|artifact| artifact.run_id == run_id)
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        own_row(&fixture.live.state).len(),
        1,
        "original provider locator owns one row"
    );
    let prepared = if historical {
        CompletionFixture::prepare_historical_record(&registration)
    } else {
        engram_verification_record_prepare(&registration)
    };
    let EngramVerificationRecordPrepared::Ready(frozen) = prepared else {
        panic!("actual committed-input run must prepare native evidence");
    };
    let (body, receipt) = fixture.record_registered_check(registration.clone(), frozen);
    assert_eq!(
        fixture
            .live
            .state
            .complete_engram_verification_record(&registration, &body, &receipt),
        EngramVerificationRecordSettlement::Settled,
        "actual native association settlement must finish"
    );
    let loaded = load_state(fixture.live.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            &loaded.test_run_registrations[run_id].verification_record,
            Some(TestRunVerificationAck::Settled { .. })
        ),
        "native receipt is read back durable"
    );
    let rows = own_row(&fixture.live.state);
    assert_eq!(rows.len(), 1);
    for verification in &receipt.verifications {
        assert!(
            rows[0]
                .evidence
                .iter()
                .any(|association| association.verification_record_id.as_ref()
                    == Some(&verification.record_id)),
            "the actual producer record reached the original launcher observation"
        );
    }
    assert_eq!(
        run_git_test_command_output(root, &["rev-parse", "HEAD"]).trim(),
        before
    );
    assert!(run_git_test_command_output(root, &["status", "--porcelain"]).is_empty());
    eprintln!(
        "PINNED_LAUNCHER_CHECK {}",
        json!({ "run_id": run_id, "input": results["input"],
        "own_rows": own_row(&fixture.live.state).len(), "records": receipt.verifications.len(),
        "settled": matches!(&loaded.test_run_registrations[run_id].verification_record,
            Some(TestRunVerificationAck::Settled { .. })) })
    );
    (body, receipt)
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn current_source_id_launcher_record_is_received_and_settled_without_evaluation() {
    let fixture = completion_fixture();
    install_launcher(&fixture);
    let session = &fixture.live.session_id;
    let added = fixture.work(
        session,
        &[
            "add",
            "Receive the full owner identity",
            "--accept",
            "The registered producer is received",
            "--bind",
            "1=test",
            "--json",
        ],
    );
    let work = added["work"]["short_ref"]
        .as_str()
        .or_else(|| added["short_ref"].as_str())
        .unwrap();
    fixture.work(session, &["claim", work, "--json"]);
    fixture
        .live
        .state
        .name_engram_source_root(
            session,
            EngramSourceRootRequest {
                work: work.to_owned(),
                path: Some(Some(fixture.root.to_string_lossy().into_owned())),
            },
        )
        .unwrap();
    fixture.begin(
        session,
        "Produce a current owner-backed verification record.",
    );
    let (body, receipt) = run_check(&fixture, session, &fixture.root);
    let EngramVerificationRecordInput::SourceId { source_id } = &body.input else {
        panic!("the fresh launcher must produce a complete owner identity");
    };
    assert!(
        matches!(&body.post_run, EngramVerificationRecordPostRun::SourceId(post_run)
        if post_run.source_id == *source_id)
    );
    let stored = fixture.object(&receipt.verifications[0].record_id);
    assert!(
        stored["source_basis"]
            == serde_json::to_value(engram_verification_record_basis(&body).unwrap()).unwrap()
    );
    finish_live_root(&fixture.live.state, session);
    fixture.wait_for_reports(session);
    assert_no_live_turn_gate(&fixture.live.transport);
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn a_real_edit_after_the_registered_launcher_check_is_still_refused() {
    let fixture = completion_fixture();
    install_launcher(&fixture);
    let session = &fixture.live.session_id;
    let added = fixture.work(
        session,
        &[
            "add",
            "Reject source edited after its registered check",
            "--accept",
            "A current test passes",
            "--bind",
            "1=test",
            "--json",
        ],
    );
    let work = added["work"]["short_ref"]
        .as_str()
        .or_else(|| added["short_ref"].as_str())
        .unwrap();
    fixture.work(session, &["claim", work, "--json"]);
    fixture
        .live
        .state
        .name_engram_source_root(
            session,
            EngramSourceRootRequest {
                work: work.to_owned(),
                path: Some(Some(fixture.root.to_string_lossy().into_owned())),
            },
        )
        .unwrap();
    fixture.begin(
        session,
        "Check current content, then make a real source edit.",
    );
    let (body, receipt) = run_historical_check(&fixture, session, &fixture.root);
    let before = content_revision(&fixture.root).unwrap().1;
    fs::write(
        fixture.root.join("README.md"),
        "real content written after the registered check\n",
    )
    .unwrap();
    assert_ne!(content_revision(&fixture.root).unwrap().1, before);
    let EngramVerificationRecordInput::Commit { commit } = &body.input else {
        panic!("registered Git input required");
    };
    assert_eq!(
        run_git_test_command_output(&fixture.root, &["rev-parse", "HEAD"]).trim(),
        commit
    );
    assert!(!run_git_test_command_output(&fixture.root, &["status", "--porcelain"]).is_empty());
    finish_live_root(&fixture.live.state, session);
    fixture.wait_for_reports(session);
    let changes = fixture
        .live
        .transport
        .requests_for("execution_observe")
        .into_iter()
        .filter_map(|request| request["occurrence"].get("source_change").cloned())
        .filter(|change| !change.is_null())
        .collect::<Vec<_>>();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["detection"], "content_comparison");
    let shown = fixture.work(session, &["show", work, "--json"]);
    let acceptance = shown["acceptance_basis"].as_i64().unwrap().to_string();
    let evidence = shown["evidence_basis"].as_i64().unwrap().to_string();
    let judged = acceptance_committed_judged_revision(commit);
    let output = fixture.command_output(
        session,
        &[
            "evaluate",
            work,
            "--mode",
            "same-session",
            "--acceptance-basis",
            &acceptance,
            "--evidence-basis",
            &evidence,
            "--verdict",
            "1=pass:observed",
            "--rationale",
            "1=The registered input was checked before the edit",
            "--evidence",
            &format!("1={}", receipt.verifications[0].record_id),
            "--source-fingerprint",
            &judged,
            "--attempt",
            "edited-after-check",
            "--json",
        ],
        true,
    );
    assert!(
        !output.status.success(),
        "an actual post-check edit cannot be evaluated as passed"
    );
    let refusal: Value = serde_json::from_slice(&output.stderr).unwrap();
    eprintln!("PINNED_LAUNCHER_EDIT_REFUSAL <details withheld>");
    assert_eq!(refusal["error"]["code"], "acceptance_evaluation_refused");
    assert_eq!(
        refusal["error"]["details"]["cause"]["mismatch"],
        "source_moved_after_check"
    );
    assert_eq!(
        refusal["error"]["details"]["cause"]["checked_revision"],
        judged
    );
    assert_eq!(
        refusal["error"]["details"]["cause"]["judged_revision"],
        judged
    );
    // Keep the real refusal above, then prove useful work in the next turn can
    // obtain and use a new native record after the accounted source change.
    run_git_test_command(&fixture.root, &["add", "--", "README.md"]);
    run_git_test_command(
        &fixture.root,
        &["commit", "--quiet", "-m", "fixture intended edit"],
    );
    fixture.begin(session, "Check the intended revision in a later work turn.");
    let (later, receipt) = run_historical_check(&fixture, session, &fixture.root);
    finish_live_root(&fixture.live.state, session);
    fixture.wait_for_reports(session);
    let EngramVerificationRecordInput::Commit { commit } = &later.input else {
        panic!("later registered Git input required");
    };
    let judged = acceptance_committed_judged_revision(commit);
    let shown = fixture.work(session, &["show", work, "--json"]);
    let acceptance = shown["acceptance_basis"].as_i64().unwrap().to_string();
    let evidence = shown["evidence_basis"].as_i64().unwrap().to_string();
    let evaluated = fixture.work(
        session,
        &[
            "evaluate",
            work,
            "--mode",
            "same-session",
            "--acceptance-basis",
            &acceptance,
            "--evidence-basis",
            &evidence,
            "--verdict",
            "1=pass:observed",
            "--rationale",
            "1=A later registered check followed the recorded source edit",
            "--evidence",
            &format!("1={}", receipt.verifications[0].record_id),
            "--source-fingerprint",
            &judged,
            "--attempt",
            "later-clean-check",
            "--json",
        ],
    );
    eprintln!(
        "PINNED_LAUNCHER_LATER_CHECK {}",
        json!({
            "evaluation": evaluated, "receipt": receipt, "judged_revision": judged,
            "previous_refusal": refusal["error"]["details"]["cause"]["mismatch"],
        })
    );
}
