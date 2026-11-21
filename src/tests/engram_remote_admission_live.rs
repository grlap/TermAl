// Operator-run conformance against the pinned real consumer in a disposable
// store. Host facts here are fixture assertions, not a real remote execution.
use super::*;
use crate::engram_remote_verification::{Association, IntendedUse, Purpose, Request};

fn source(root: &FsPath) -> EngramSourceId {
    let git = |arg: &str| {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["rev-parse", arg])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    EngramSourceId {
        git_delta: None,
        commit: git("HEAD"),
        tree: git("HEAD^{tree}"),
        changes: vec![],
        submodules: vec![],
    }
}

#[test]
#[ignore = "operator-run against an explicitly pinned disposable Engram binary"]
fn real_consumer_replays_original_receipt_before_new_association_currency_and_keeps_terminal_facts()
{
    let gate = live_carried_gate("remote-admission");
    name_live_root(&gate);
    let state = &gate.fixture.state;
    let target = {
        let inner = state.inner.lock().unwrap();
        AppState::engram_source_root_target_locked(&inner, &gate.fixture.session_id)
            .unwrap()
            .unwrap()
    };
    let store = target.settings.authority_store_key.as_ref().unwrap();
    target
        .adapter
        .read_work_session(&target.connection, store, target.settings.call_timeout())
        .unwrap();
    assert!(
        target
            .adapter
            .remote_verification_record_route_support(&target.connection)
            .is_supported()
    );
    let held = target
        .adapter
        .read_held_claims(&target.connection, target.settings.call_timeout())
        .unwrap();
    let binding = held
        .items
        .iter()
        .filter(|claim| claim.short_ref == gate.work_ref)
        .find_map(|claim| claim.control_binding.clone())
        .unwrap();
    let root = {
        let inner = state.inner.lock().unwrap();
        inner
            .engram_work_source_roots
            .iter()
            .find(|root| root.work_id == binding.work_id)
            .unwrap()
            .clone()
    };
    let source = source(&gate.worktree);
    let completion = native_test_completion::Completion {
        schema_version: 1,
        origin: native_test_completion::Origin {
            host_id: Uuid::new_v4().to_string(),
            runner_session_id: "fixture-native-runner".into(),
        },
        run_id: format!("test-{}", Uuid::new_v4()),
        mode: native_test_completion::Mode::Full,
        result: native_test_completion::NativeResult {
            verdict: native_test_completion::Verdict::Pass,
            terminal: true,
            exit: Some(0),
            reason: "clean".into(),
        },
        input: native_test_completion::Input::SourceId(native_test_completion::SourceInput {
            kind: native_test_completion::SourceInputKind::SourceId,
            source_id_before: Some(source.clone()),
            source_id_after: Some(source.clone()),
        }),
        finished_at: Some(chrono::Utc::now().to_rfc3339()),
    };
    let association = Association {
        binding: binding.clone(),
        root: EngramVerificationRecordRoot {
            workspace_id: root.root.clone(),
            generation: root.generation.try_into().unwrap(),
            state: EngramVerificationRecordRootState::Named,
        },
        intended_use: IntendedUse::CommittedInput {
            expected_commit: source.commit,
            purpose: Purpose::LandingGate,
        },
    };
    let original = Request::new(
        "fixture-original-remote".into(),
        &completion,
        association.clone(),
    )
    .unwrap();
    let send = |request: Request| {
        target
            .adapter
            .request(
                &target.connection,
                &request.into_control_request(),
                target.settings.call_timeout(),
            )
            .unwrap()
    };
    let accepted = send(original.clone());
    assert!(
        original
            .receipt(accepted.clone())
            .unwrap()
            .qualification
            .credited
    );
    let same = Request::new(
        "fixture-duplicate-delivery".into(),
        &completion,
        association.clone(),
    )
    .unwrap();
    assert_eq!(
        send(same),
        accepted,
        "same origin and destination return the original delivery receipt"
    );
    let mut conflict = completion.clone();
    conflict.result.reason = "different later native facts".into();
    let conflicted = Request::new(
        "fixture-conflicting-terminal".into(),
        &conflict,
        association.clone(),
    )
    .unwrap();
    let refused = conflicted.receipt(send(conflicted.clone())).unwrap();
    assert_eq!(
        refused.qualification.reason,
        Some(engram_remote_verification::Reason::ResultConflict)
    );
    assert!(refused.verifications.is_empty());
    // New root generation displaces the captured destination. It cannot change
    // a previously accepted receipt, but it must refuse a NEW association.
    state
        .name_engram_source_root(
            &gate.fixture.session_id,
            EngramSourceRootRequest {
                work: gate.work_ref.clone(),
                path: None,
            },
        )
        .unwrap();
    assert_eq!(send(original.clone()), accepted);
    let stale = Request::new(
        "fixture-new-stale-association".into(),
        &completion,
        association,
    )
    .unwrap();
    let refused = stale.receipt(send(stale.clone())).unwrap();
    assert_eq!(
        refused.qualification.reason,
        Some(engram_remote_verification::Reason::AssociationStale)
    );
    assert!(refused.verifications.is_empty());
    assert_eq!(refused.origin, original.receipt(accepted).unwrap().origin);
}
