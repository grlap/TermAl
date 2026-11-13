// Real registration, provider-frame correlation and native artifact settlement;
// fixture telemetry is not an executed consumer-repository test or credit.
use super::observed_terminal::{LAUNCH, assert_no_credit, finish_artifact, launcher, start};
use super::*;

pub(super) fn native_run(label: &str, observed: bool) -> (RunFixture, String, String, PathBuf) {
    let fixture = RunFixture::new(label);
    let session = running_owner(&fixture, "native-completion-runtime");
    native_origin::bind_native_host(&fixture);
    if observed {
        start(&fixture, &session, "native-tool", LAUNCH);
    }
    let (id, directory) = launcher(&fixture, &session);
    if observed {
        fixture.state.note_engram_command_finished_with_capture(
            &session,
            &EngramObservationProvenance::Ambient,
            "native-tool",
            LAUNCH,
            &format!("RUN {}", test_run_display_path(&directory)),
            Some(EngramCommandExit::NotFinished),
            false,
        );
    }
    (fixture, session, id, directory)
}

#[test]
fn native_completion_retains_registered_observed_pass_and_failure_without_credit() {
    for success in [true, false] {
        let (fixture, session, id, directory) = native_run("native-completion-terminal", true);
        assert!(
            fixture.state.inner.lock().unwrap().test_run_registrations[&id]
                .native_completion
                .is_none()
        );
        finish_artifact(&directory, &id, success);
        fixture.refresh(&[]);
        let registration = fixture.state.inner.lock().unwrap().test_run_registrations[&id].clone();
        let retained = registration
            .native_completion
            .as_ref()
            .expect("original native settlement retained");
        assert_eq!(retained.completion.origin.runner_session_id, session);
        assert_eq!(
            retained.completion.origin.host_id,
            registration.native_origin.as_ref().unwrap().host.host_id
        );
        assert_eq!(retained.completion.run_id, id);
        assert_eq!(
            retained.completion.result.verdict,
            if success {
                native_test_completion::Verdict::Pass
            } else {
                native_test_completion::Verdict::Fail
            }
        );
        assert_eq!(
            retained.completion.result.exit,
            Some(if success { 0 } else { 101 })
        );
        assert!(retained.completion.authoritative_terminal());
        assert!(retained.completion.finished_at.is_some());
        retained.completion.validate().unwrap();
        assert_no_credit(&fixture);
        let loaded = load_state(fixture.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        assert_eq!(
            loaded.test_run_registrations[&id].native_completion,
            registration.native_completion
        );
        // Retention outlives the artifact; a later scan cannot rewrite it.
        fs::remove_file(directory.join("results.json")).unwrap();
        fixture.refresh(&[]);
        assert_eq!(
            fixture.state.inner.lock().unwrap().test_run_registrations[&id].native_completion,
            registration.native_completion
        );
    }
}

#[test]
fn native_completion_does_not_promote_raw_results_or_a_launch_acknowledgement() {
    let (fixture, _, id, directory) = native_run("native-completion-raw-only", false);
    finish_artifact(&directory, &id, true);
    fixture.refresh(&[]);
    assert!(
        fixture.state.inner.lock().unwrap().test_run_registrations[&id]
            .native_completion
            .is_none()
    );

    let (fixture, _, id, _) = native_run("native-completion-launch-only", true);
    fixture.refresh(&[]);
    assert!(
        fixture.state.inner.lock().unwrap().test_run_registrations[&id]
            .native_completion
            .is_none()
    );
}

#[test]
fn native_completion_unknown_publication_does_not_block_later_original_terminal() {
    let (fixture, _, id, directory) = native_run("native-completion-late", true);
    fs::write(
        directory.join("results.json"),
        json!({"runId": id, "state":"unknown"}).to_string(),
    )
    .unwrap();
    fixture.refresh(&[]);
    assert!(
        fixture.state.inner.lock().unwrap().test_run_registrations[&id]
            .native_completion
            .is_none()
    );
    finish_artifact(&directory, &id, false);
    fixture.refresh(&[]);
    assert_eq!(
        fixture.state.inner.lock().unwrap().test_run_registrations[&id]
            .native_completion
            .as_ref()
            .unwrap()
            .completion
            .result
            .verdict,
        native_test_completion::Verdict::Fail
    );
}

#[test]
fn native_completion_refuses_ambiguous_original_provider_executions() {
    let (fixture, session, id, directory) = native_run("native-completion-ambiguous", true);
    start(&fixture, &session, "another-native-tool", LAUNCH);
    fixture.state.note_engram_command_finished_with_capture(
        &session,
        &EngramObservationProvenance::Ambient,
        "another-native-tool",
        LAUNCH,
        &format!("RUN {}", test_run_display_path(&directory)),
        Some(EngramCommandExit::NotFinished),
        false,
    );
    finish_artifact(&directory, &id, true);
    fixture.refresh(&[]);
    assert!(
        fixture.state.inner.lock().unwrap().test_run_registrations[&id]
            .native_completion
            .is_none()
    );
}

#[test]
fn native_completion_scan_during_registration_replay_keeps_the_durable_ack() {
    let (fixture, session, id, directory) = native_run("native-completion-replay-race", true);
    let before = fixture.state.inner.lock().unwrap().test_run_registrations[&id].clone();
    assert!(before.native_completion.is_none());
    finish_artifact(&directory, &id, true);
    let scan = fixture.state.clone();
    TEST_TEST_RUN_REGISTRATION_RECHECKED.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            scan.refresh_test_runs_with(&|_, _| false, &|_, _| {});
        }));
    });
    let ack = fixture
        .state
        .register_test_run(&session, registration_request(&id, &directory))
        .unwrap();
    assert!(ack.registered);
    assert!(TEST_TEST_RUN_REGISTRATION_RECHECKED.with(|slot| slot.borrow().is_none()));
    let after = fixture.state.inner.lock().unwrap().test_run_registrations[&id].clone();
    assert!(after.native_completion.is_some());
    assert_ne!(
        after, before,
        "full equality still includes retained evidence"
    );
    assert!(after.matches_registration_snapshot(&before));
    assert!(
        !before.matches_registration_snapshot(&after),
        "an existing completion cannot disappear"
    );
    let mut conflicting = after.clone();
    conflicting
        .native_completion
        .as_mut()
        .unwrap()
        .execution_id
        .push_str("-different");
    assert!(!conflicting.matches_registration_snapshot(&after));
    let mut replaced = after.clone();
    replaced.request_identity.push_str("-different");
    assert!(!replaced.matches_registration_snapshot(&before));
    replaced = after.clone();
    replaced
        .native_origin
        .as_mut()
        .unwrap()
        .native_creation_id
        .push_str("-different");
    assert!(!replaced.matches_registration_snapshot(&before));
    replaced = after.clone();
    replaced.monitoring_interrupted_at = Some(test_run_timestamp());
    assert!(!replaced.matches_registration_snapshot(&before));

    let fence = PersistFenceTarget::TestRunRegistration(before);
    let mut delta = collect_persist_delta_from_shared_state(&fixture.state.inner, 0);
    assert!(
        delta
            .metadata
            .metadata_collection_is_omitted("testRunRegistrations"),
        "the synchronous scan already saved this unchanged collection"
    );
    assert!(
        !fence.is_in_delta(&delta),
        "an omitted collection is not an in-delta proof"
    );
    delta.metadata = PersistedState::metadata_from_inner(&fixture.state.inner.lock().unwrap());
    assert!(
        fence.is_in_delta(&delta),
        "a persisted completion append retains the original registration ACK"
    );
    let connection = rusqlite::Connection::open(fixture.state.persistence_path.as_path()).unwrap();
    assert!(fence.is_already_durable(&connection).unwrap());
    let stored = read_metadata_for_fence(&connection, &["testRunRegistrations"])
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.test_run_registrations[&id], after,
        "the exact full registration and completion were stored before boot normalization"
    );
    let loaded = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        loaded.test_run_registrations[&id].native_completion, after.native_completion,
        "boot preserves the original full native completion"
    );
    assert!(
        loaded.test_run_registrations[&id]
            .monitoring_interrupted_at
            .is_some(),
        "boot separately records the interruption of monitoring this launch-only provider"
    );
    assert_no_credit(&fixture);
}
