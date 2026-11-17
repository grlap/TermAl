// Local refusal presentation recovery, independent notice retry and retirement.
use super::*;

#[test]
fn failed_refusal_mirror_is_repaired_after_reload_without_a_producer_send() {
    let mut fixture = RunFixture::new("unclassified-mirror-recovery");
    synchronous_persistence(&mut fixture);
    let (session, transport) = engram_session(&fixture, true);
    let run = wrapper_run(&fixture, &session, "test-unclassified-mirror-recovery");
    register(&fixture, &[&run]);
    // Force the atomic rename to fail, after the authoritative refusal saves.
    let progress_path = run.run_dir.join(ENGRAM_VERIFICATION_RECORD_STATE_FILE);
    fs::create_dir(&progress_path).unwrap();
    fixture.state.engram_verification_records_tick();
    let detail = refusal(&fixture, &run);
    assert!(engram_verification_record_progress(&run.run_dir).is_none());
    assert!(
        session_engram(&fixture, &session, |engram| engram
            .pending_source_root_line
            .clone())
        .unwrap()
        .contains(&detail)
    );
    assert!(transport.sent_operations().is_empty());
    let loaded = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    *fixture.state.inner.lock().unwrap() = loaded;
    assert_eq!(refusal(&fixture, &run), detail);
    fs::remove_dir(&progress_path).unwrap();
    let mut cursor = EngramVerificationRecordCursor::default();
    assert_eq!(
        fixture.state.engram_verification_records_tick_from(
            Duration::from_secs(1),
            &|| false,
            &mut cursor
        ),
        1
    );
    let rendered = super::super::refusals::summary(&run.run_dir);
    assert!(rendered.contains("verification: rejected:"), "{rendered}");
    assert!(rendered.contains(&detail), "{rendered}");
    assert!(
        session_engram(&fixture, &session, |engram| engram
            .pending_source_root_line
            .clone())
        .unwrap()
        .contains(&detail)
    );
    // Once repaired, the same process does not read or requeue it every tick.
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.pending_source_root_line = None;
    }
    assert_eq!(
        fixture.state.engram_verification_records_tick_from(
            Duration::from_secs(1),
            &|| false,
            &mut cursor
        ),
        0
    );
    assert!(
        session_engram(&fixture, &session, |engram| engram
            .pending_source_root_line
            .clone())
        .is_none()
    );
    // A fresh process requeues the notice even when the mirror survived.
    *fixture.state.inner.lock().unwrap() = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    fixture.state.engram_verification_records_tick();
    assert!(
        session_engram(&fixture, &session, |engram| engram
            .pending_source_root_line
            .clone())
        .unwrap()
        .contains(&detail)
    );
    assert!(transport.sent_operations().is_empty());
    assert!(
        !run.run_dir
            .join(ENGRAM_VERIFICATION_RECORD_BODY_FILE)
            .exists()
    );
}

#[test]
fn failed_synchronous_refusal_save_rolls_back_and_later_tick_recovers() {
    let mut fixture = RunFixture::new("unclassified-save-recovery");
    synchronous_persistence(&mut fixture);
    let (session, transport) = engram_session(&fixture, true);
    let run = wrapper_run(&fixture, &session, "test-unclassified-save-recovery");
    register(&fixture, &[&run]);
    let before = current(&fixture, &run);
    let notice_before = session_engram(&fixture, &session, |engram| {
        engram.pending_source_root_line.clone()
    });
    let path = fixture.state.persistence_path.clone();
    let blocked = fixture.root.join("unclassified-save-blocked");
    fs::create_dir(&blocked).unwrap();
    fixture.state.persistence_path = Arc::new(blocked);
    fixture.state.engram_verification_records_tick();
    assert_eq!(current(&fixture, &run), before);
    assert_eq!(
        session_engram(&fixture, &session, |engram| engram
            .pending_source_root_line
            .clone()),
        notice_before
    );
    assert!(!matches!(
        engram_verification_record_progress(&run.run_dir),
        Some(EngramVerificationRecordProgress::Rejected { .. })
    ));
    assert!(transport.sent_operations().is_empty());
    fixture.state.persistence_path = path;
    fixture.state.engram_verification_records_tick();
    assert!(refusal(&fixture, &run).starts_with("unclassified_stages:"));
    assert!(matches!(
        engram_verification_record_progress(&run.run_dir),
        Some(EngramVerificationRecordProgress::Rejected { .. })
    ));
    assert!(transport.sent_operations().is_empty());
}

#[test]
fn mirror_retry_obeys_initial_backoff_and_never_requeues_the_notice() {
    let mut fixture = RunFixture::new("unclassified-independent-retry");
    synchronous_persistence(&mut fixture);
    let clock = fixture.state.select_test_scripted_engram_budget_clock();
    let (session, transport) = engram_session(&fixture, true);
    let run = wrapper_run(&fixture, &session, "test-unclassified-independent-retry");
    register(&fixture, &[&run]);
    let path = run.run_dir.join(ENGRAM_VERIFICATION_RECORD_STATE_FILE);
    fs::create_dir(&path).unwrap();
    let mut cursor = EngramVerificationRecordCursor::default();
    assert_eq!(pass_with(&fixture, &mut cursor), 1);
    let original = session_engram(&fixture, &session, |engram| {
        engram.pending_source_root_line.clone()
    });
    assert_eq!(
        pass_with(&fixture, &mut cursor),
        0,
        "the first repair failure observes its post-decision backoff"
    );
    for _ in 0..3 {
        clock.advance(ENGRAM_VERIFICATION_RECORD_BACKOFF_MAX);
        assert_eq!(pass_with(&fixture, &mut cursor), 1);
        assert_eq!(
            session_engram(&fixture, &session, |engram| engram
                .pending_source_root_line
                .clone()),
            original
        );
    }
    fs::remove_dir(&path).unwrap();
    clock.advance(ENGRAM_VERIFICATION_RECORD_BACKOFF_MAX);
    assert_eq!(pass_with(&fixture, &mut cursor), 1);
    assert!(super::super::refusals::summary(&run.run_dir).contains("verification: rejected:"));
    assert_eq!(
        session_engram(&fixture, &session, |engram| engram
            .pending_source_root_line
            .clone()),
        original
    );
    assert_eq!(pass_with(&fixture, &mut cursor), 0);
    assert!(transport.sent_operations().is_empty());
}

#[test]
fn removed_run_directory_is_presented_once_after_reload_then_retired() {
    let mut fixture = RunFixture::new("unclassified-removed-directory");
    synchronous_persistence(&mut fixture);
    let clock = fixture.state.select_test_scripted_engram_budget_clock();
    let (session, transport) = engram_session(&fixture, true);
    let run = wrapper_run(&fixture, &session, "test-unclassified-removed-directory");
    register(&fixture, &[&run]);
    fixture.state.engram_verification_records_tick();
    let detail = refusal(&fixture, &run);
    assert!(run.run_dir.starts_with(fixture.runs_dir()));
    fs::remove_dir_all(&run.run_dir).unwrap();
    *fixture.state.inner.lock().unwrap() = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let mut cursor = EngramVerificationRecordCursor::default();
    assert_eq!(pass_with(&fixture, &mut cursor), 1);
    let notice = session_engram(&fixture, &session, |engram| {
        engram.pending_source_root_line.clone()
    })
    .unwrap();
    assert_eq!(notice.matches(&detail).count(), 1);
    for _ in 0..3 {
        clock.advance(ENGRAM_VERIFICATION_RECORD_BACKOFF_MAX);
        assert_eq!(pass_with(&fixture, &mut cursor), 0);
        assert_eq!(
            session_engram(&fixture, &session, |engram| engram
                .pending_source_root_line
                .clone())
            .as_deref(),
            Some(notice.as_str())
        );
    }
    assert!(
        !run.run_dir.exists(),
        "repair does not recreate retired artifacts"
    );
    assert!(transport.sent_operations().is_empty());
    assert_eq!(refusal(&fixture, &run), detail);
}

#[test]
fn deleted_requester_ends_presentation_without_redirecting_or_retrying() {
    let mut fixture = RunFixture::new("unclassified-deleted-requester");
    synchronous_persistence(&mut fixture);
    let clock = fixture.state.select_test_scripted_engram_budget_clock();
    let (session, transport) = engram_session(&fixture, true);
    let (other, _) = engram_session(&fixture, true);
    let run = wrapper_run(&fixture, &session, "test-unclassified-deleted-requester");
    register(&fixture, &[&run]);
    fixture.state.engram_verification_records_tick();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions.remove(index);
        fixture.state.persist_internal_locked(&inner).unwrap();
    }
    *fixture.state.inner.lock().unwrap() = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let before = session_engram(&fixture, &other, |engram| {
        engram.pending_source_root_line.clone()
    });
    let mut cursor = EngramVerificationRecordCursor::default();
    assert_eq!(pass_with(&fixture, &mut cursor), 1);
    for _ in 0..3 {
        clock.advance(ENGRAM_VERIFICATION_RECORD_BACKOFF_MAX);
        assert_eq!(pass_with(&fixture, &mut cursor), 0);
    }
    assert_eq!(
        session_engram(&fixture, &other, |engram| engram
            .pending_source_root_line
            .clone()),
        before
    );
    assert!(transport.sent_operations().is_empty());
}
