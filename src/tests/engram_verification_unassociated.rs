// Owns the witnesses that a held verification receipt no launch observation
// can carry any longer becomes final as unassociated
// (`engram_verification_record_unassociable_locked`,
// src/engram_verification_record.rs). Each one is driven through the
// registered committed-run fixture of its parent module
// (engram_registered_launcher_closures.rs) and the production completion: a
// launch row still open keeps the receipt held, and the outcome follows the
// rows, never a timer. It does not own the run wait's prompt text
// (src/tests/test_run_waits/verification_records.rs) or the settling paths
// that carry the run's artifact, which the parent module covers. New module.
use super::*;

const FULL_COMMAND: &str = "node scripts/test-launcher.mjs full --input commit";

/// The run's terminal results and Engram's answer to its record, completed by
/// the production settlement, as `RegisteredLauncherTurn::receive_until_with`
/// does, but with no assumption about where the receipt ends.
fn complete(
    fixture: &RegisteredLauncherTurn,
) -> (EngramVerificationRecordReceipt, EngramVerificationRecordSettlement) {
    let registration =
        fixture.turn.state.inner.lock().unwrap().test_run_registrations[&fixture.run_id].clone();
    let source = fixture.source_id.as_ref().expect("committed fixture");
    let head = source["commit"].as_str().unwrap();
    let at = test_run_timestamp();
    fs::write(
        fixture.directory.join("results.json"),
        serde_json::to_vec(&json!({
            "runId": fixture.run_id, "state": "passed", "started": fixture.started, "ended": at,
            "exitCode": 0, "sourceIdBefore": source, "sourceIdAfter": source,
            "stages": [{"name": "focused", "state": "passed", "code": 0, "started": at, "ended": at}],
            "input": {"kind": "commit", "commit": head, "cleanBefore": true,
                "after": {"commit": head, "clean": true, "paths": []}, "outcome": "clean",
                "eligible": true}
        }))
        .unwrap(),
    )
    .unwrap();
    let EngramVerificationRecordPrepared::Ready(frozen) =
        engram_verification_record_prepare(&registration)
    else {
        panic!("clean committed fixture is ready");
    };
    let receipt = EngramVerificationRecordReceipt {
        operation: "0123456789abcdef0123456789abcdef".into(),
        producer: "0123456789abcdef0123456789abcdef".into(),
        verifications: vec![EngramVerificationRecordVerification {
            stage: "focused".into(),
            record_id: "fedcba9876543210fedcba9876543210".into(),
        }],
        run_outcome: frozen.body.run.outcome,
    };
    let settlement = fixture.turn.state.complete_engram_verification_record_until(
        &registration,
        &frozen.body,
        &receipt,
        std::time::Instant::now() + DEADLOCK_GUARD,
    );
    fixture.wait_for_received_notices();
    (receipt, settlement)
}

/// The producer's next look at the run's held receipt: prepared from the
/// registration as it stands, then completed from the receipt alone.
fn look_again(fixture: &RegisteredLauncherTurn) -> EngramVerificationRecordSettlement {
    let registration = registration(fixture);
    let EngramVerificationRecordPrepared::ReceiptHeld { frozen, receipt } =
        engram_verification_record_prepare(&registration)
    else {
        panic!("the held receipt is looked at again");
    };
    fixture
        .turn
        .state
        .complete_engram_verification_record(&registration, &frozen.body, &receipt)
}

fn registration(fixture: &RegisteredLauncherTurn) -> TestRunRegistration {
    fixture.turn.state.inner.lock().unwrap().test_run_registrations[&fixture.run_id].clone()
}

fn held_unassociated(fixture: &RegisteredLauncherTurn) -> bool {
    fixture
        .turn
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_unassociated_receipts
        .contains_key(&fixture.run_id)
}

/// What the run's final receipt is: its reason, once it is unassociated, after
/// checking that it is the same receipt and digest the host held and that the
/// host never looks at it again.
fn assert_final_unassociated(
    fixture: &RegisteredLauncherTurn,
    receipt: &EngramVerificationRecordReceipt,
    held_digest: &str,
) -> String {
    let registration = registration(fixture);
    let Some(TestRunVerificationAck::Unassociated {
        receipt: kept,
        body_digest,
        reason,
    }) = registration.verification_record.clone()
    else {
        panic!("final as unassociated: {:?}", registration.verification_record);
    };
    assert_eq!(&kept, receipt);
    assert_eq!(body_digest, held_digest, "the receipt stays bound to its body");
    assert!(!held_unassociated(fixture), "no longer held for a later look");
    assert!(matches!(
        engram_verification_record_prepare(&registration),
        EngramVerificationRecordPrepared::Finished
    ));
    // The exact run read shows the final state and its reason, with the
    // records Engram received.
    let view = test_run_verification_read(&fixture.turn.state.inner.lock().unwrap(), &fixture.run_id)
        .unwrap()
        .verification_record
        .expect("the host holds an answer for the run");
    assert_eq!(view.state, "unassociated");
    assert_eq!(view.detail.as_deref(), Some(reason.as_str()));
    assert_eq!(view.record_ids, vec!["fedcba9876543210fedcba9876543210".to_owned()]);
    // The launcher summary's copy says the same.
    assert!(matches!(
        engram_verification_record_progress(&fixture.directory),
        Some(EngramVerificationRecordProgress::Unassociated { reason: copied, .. }) if copied == reason
    ));
    // No host check credit is told for it.
    assert!(ReceivedCheckNotice::read(
        &registration,
        std::time::Instant::now() + RECEIVED_NOTICE_CAPTURE_BUDGET
    )
    .is_none());
    reason
}

fn held_digest(fixture: &RegisteredLauncherTurn) -> String {
    let Some(TestRunVerificationAck::Received { body_digest, .. }) =
        registration(fixture).verification_record
    else {
        panic!("the receipt is held, not settled");
    };
    body_digest
}

#[test]
fn a_held_receipt_stays_held_while_its_launch_runs_and_is_final_once_it_ends_without_the_run() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::committed();
    // The launch is still running: its row may yet carry the run's locator.
    let (receipt, settlement) = complete(&fixture);
    assert_eq!(settlement, EngramVerificationRecordSettlement::NoLaunchRow);
    let digest = held_digest(&fixture);
    assert!(held_unassociated(&fixture));
    assert_eq!(look_again(&fixture), EngramVerificationRecordSettlement::NoLaunchRow);
    // The launch ends, and its reply named no run directory: no row of the
    // session in the run's worktree can carry the records any longer.
    fixture
        .recorder
        .command_completed_with_exit(
            KEY,
            &fixture.command,
            "launcher output without its run locator",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    let EngramVerificationRecordSettlement::Unassociated { reason } = look_again(&fixture) else {
        panic!("final once no launch observation can carry the records");
    };
    assert!(reason.contains("none is still open"), "{reason}");
    assert_eq!(assert_final_unassociated(&fixture, &receipt, &digest), reason);
}

#[test]
fn a_claude_background_launch_keeps_its_receipt_held_until_the_task_ends_then_makes_it_final() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    // A session launches the full gate in the background: the launch's only
    // result says it is running and names no run directory.
    let turn = RegisteredLauncherTurn::committed_turn("unassociated-background");
    let token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    let generation = turn.record(|record| record.active_turn_generation);
    let cwd = turn.record(|record| record.engram.observed_source_root().unwrap().root.clone());
    let mut recorder = turn.recorder();
    recorder
        .command_started_in(KEY, FULL_COMMAND, Some(FULL_COMMAND), Some(&cwd))
        .unwrap();
    let mut fixture = RegisteredLauncherTurn::after_start_with_input(turn, recorder, true);
    fixture
        .recorder
        .command_completed_with_exit(
            KEY,
            FULL_COMMAND,
            "Command running in background with ID: b-unassociated.",
            CommandStatus::Running,
            EngramCommandExit::NotFinished,
        )
        .unwrap();
    fixture.turn.record_mut(|record| {
        record.claude_outstanding.register(ClaudeOutstandingEntry {
            token: token.clone(),
            key: KEY.to_owned(),
            origin: ClaudeWorkOrigin::Attempt {
                turn_generation: generation,
            },
            background: true,
            nested: false,
            self_gate: true,
            locations: Vec::new(),
            registered_at: 0,
        });
    });
    let launch_only = fixture.turn.state.inner.lock().unwrap().test_run_observations.values().any(
        |row| row.tool_identity == sha256_hex(KEY.as_bytes()) && row.launch_only && row.artifact.is_none(),
    );
    assert!(launch_only, "the background launch acknowledged only the launch");
    // The run finished, but Claude has not said its background task ended.
    let (receipt, settlement) = complete(&fixture);
    assert_eq!(settlement, EngramVerificationRecordSettlement::NoLaunchRow);
    let digest = held_digest(&fixture);
    // Claude's terminal notice for the task, read and settled as the frame
    // reader does: its row still names no run directory.
    let ended = claude_frame_work(
        &json!({"type": "system", "subtype": "task_notification", "task_id": "b-unassociated",
            "tool_use_id": KEY, "status": "completed"}),
        false,
    );
    fixture
        .turn
        .state
        .settle_claude_frame(&fixture.turn.session_id, &token, &ended);
    let EngramVerificationRecordSettlement::Unassociated { reason } = look_again(&fixture) else {
        panic!("final once the background task ended");
    };
    assert!(reason.contains("background task"), "{reason}");
    assert_eq!(assert_final_unassociated(&fixture, &receipt, &digest), reason);
}

#[test]
fn a_launch_row_of_an_earlier_runtime_can_carry_nothing_so_the_receipt_is_final() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = RegisteredLauncherTurn::committed();
    // The session's runtime was replaced while its launch was still open: no
    // frame of the earlier runtime is applied any more.
    let (handle, _commands) = test_claude_runtime_handle("unassociated-successor");
    fixture
        .turn
        .record_mut(|record| record.runtime = SessionRuntime::Claude(handle));
    let (receipt, settlement) = complete(&fixture);
    let EngramVerificationRecordSettlement::Unassociated { reason } = settlement else {
        panic!("final: no row of the current runtime can carry the records");
    };
    assert!(reason.contains("earlier runtime"), "{reason}");
    let digest = registration(&fixture)
        .verification_record
        .and_then(|ack| ack.body_digest().map(str::to_owned))
        .unwrap();
    assert_final_unassociated(&fixture, &receipt, &digest);
}

#[test]
fn a_launch_that_names_the_run_still_settles() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::committed();
    fixture.background_reply();
    fixture.finish();
    let (_, settlement) = complete(&fixture);
    assert_eq!(settlement, EngramVerificationRecordSettlement::Settled);
    assert!(matches!(
        registration(&fixture).verification_record,
        Some(TestRunVerificationAck::Settled { .. })
    ));
}

#[test]
fn an_unassociated_receipt_stays_final_across_a_restart() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::committed();
    let (receipt, _) = complete(&fixture);
    let digest = held_digest(&fixture);
    fixture
        .recorder
        .command_completed_with_exit(
            KEY,
            &fixture.command,
            "launcher output without its run locator",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    let EngramVerificationRecordSettlement::Unassociated { reason } = look_again(&fixture) else {
        panic!("final once its launch ended without the run");
    };
    // The registration as persisted reads back to the same final state, and a
    // restarted producer's look finds nothing to do.
    let persisted = serde_json::to_string(&registration(&fixture)).unwrap();
    let restored: TestRunRegistration = serde_json::from_str(&persisted).unwrap();
    assert_eq!(
        restored.verification_record,
        Some(TestRunVerificationAck::Unassociated {
            receipt,
            body_digest: digest,
            reason,
        })
    );
    assert!(matches!(
        engram_verification_record_prepare(&restored),
        EngramVerificationRecordPrepared::Finished
    ));
}
