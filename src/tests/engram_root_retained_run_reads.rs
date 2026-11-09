//! The bounded background reads of retained runs at turn opening and at host
//! startup, and the console lines a settlement writes. Mounted from
//! `engram_root_retained_runs.rs` to share its retained-run fixture.
use super::*;

/// Background reads start in a test only after it opts in.
fn enable_background_reads() {
    TEST_ENGRAM_RETAINED_RUN_READS_ENABLED.with(|enabled| enabled.set(true));
}

fn started_reads() -> usize {
    TEST_ENGRAM_RETAINED_RUN_READS.with(|reads| reads.borrow().len())
}

fn join_background_reads() {
    let reads =
        TEST_ENGRAM_RETAINED_RUN_READS.with(|reads| std::mem::take(&mut *reads.borrow_mut()));
    for read in reads {
        read.join().expect("the background read finished");
    }
}

fn in_flight(fixture: &Retained) -> bool {
    !fixture
        .claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_retained_run_reads
        .in_flight
        .is_empty()
}

fn startup_read(fixture: &Retained, store: &EngramAuthorityStoreKey) -> bool {
    fixture
        .claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_retained_run_reads
        .startup_read
        .contains(store)
}

#[test]
fn without_opting_in_a_test_starts_no_background_read() {
    let fixture = retained("reads-not-enabled");
    fixture
        .claimed
        .state
        .engram_host()
        .turn_starting(&fixture.claimed.session_id);
    fixture
        .claimed
        .state
        .schedule_engram_retained_run_reads(None);
    assert_eq!(started_reads(), 0);
    assert!(!in_flight(&fixture));
}

#[test]
fn a_turn_opening_reads_its_works_retained_runs_off_the_callers_thread() {
    enable_background_reads();
    let fixture = retained("turn-opening-read");
    engram_answers(
        &fixture,
        &fixture.earlier.claim_id,
        refusal(
            "claim_not_current_for_run",
            "the claim does not belong to the run",
        ),
    );
    let reads = fixture.claimed.transport.requests().len();
    fixture
        .claimed
        .state
        .engram_host()
        .turn_starting(&fixture.claimed.session_id);
    join_background_reads();
    assert!(
        fixture.claimed.transport.requests().len() > reads,
        "the opened work's run was read"
    );
    assert_eq!(
        unresolved(&fixture),
        None,
        "the next opening of this work is no longer blocked"
    );
    assert!(!in_flight(&fixture));
    // A work with nothing retained starts no read at all.
    fixture
        .claimed
        .state
        .engram_host()
        .turn_starting(&fixture.claimed.session_id);
    assert_eq!(started_reads(), 0);
}

#[test]
fn a_publication_interrupted_after_the_last_retirement_is_finished_by_a_turn_opening() {
    enable_background_reads();
    let fixture = retained("interrupted-publication");
    engram_answers(
        &fixture,
        &fixture.earlier.claim_id,
        refusal(
            "claim_not_current_for_run",
            "the claim does not belong to the run",
        ),
    );
    maintain(&fixture);
    assert_eq!(unresolved(&fixture), None);
    {
        // The retirement was persisted but its owner's publication was not.
        let mut inner = fixture.claimed.state.inner.lock().unwrap();
        let history = history_mut(&mut inner, &fixture.store, &fixture.current.work_id);
        let owner = history.transition.as_mut().expect("the retiring owner");
        assert!(engram_same_recovery_run(&owner.binding, &fixture.earlier));
        owner.phase = EngramAuthorityPhase::Prepared;
        assert!(history.unresolved_runs.is_empty());
    }
    assert_eq!(
        unresolved(&fixture),
        Some(EngramUnconfirmedRootReason::WorkTransitionUnpublished)
    );
    fixture
        .claimed
        .state
        .engram_host()
        .turn_starting(&fixture.claimed.session_id);
    assert_eq!(
        started_reads(),
        1,
        "the interrupted publication is recovery work"
    );
    join_background_reads();
    assert_eq!(
        unresolved(&fixture),
        None,
        "publication finished without a read of the retired run"
    );
}

#[test]
fn no_background_read_starts_while_a_reclamation_pass_owns_the_authority() {
    enable_background_reads();
    let fixture = retained("reads-yield-to-reclamation");
    fixture
        .claimed
        .state
        .inner
        .lock()
        .unwrap()
        .engram_root_reclamation
        .flight = Some("a-running-reclamation-pass".to_owned());
    fixture
        .claimed
        .state
        .engram_host()
        .turn_starting(&fixture.claimed.session_id);
    fixture
        .claimed
        .state
        .schedule_engram_retained_run_reads(None);
    assert_eq!(started_reads(), 0);
    assert!(
        !startup_read(&fixture, &fixture.store),
        "the startup read is still owed"
    );
}

#[test]
fn host_startup_reads_each_store_once_and_retries_only_transient_failures_at_turn_opening() {
    enable_background_reads();
    let fixture = retained("startup-read");
    fixture
        .claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .root_read_failures
        .insert(fixture.earlier.claim_id.clone());
    fixture
        .claimed
        .state
        .schedule_engram_retained_run_reads(None);
    join_background_reads();
    assert_eq!(
        unresolved(&fixture),
        Some(EngramUnconfirmedRootReason::WorkRunsUnresolved),
        "a transient failure keeps the run retained"
    );
    assert!(startup_read(&fixture, &fixture.store));
    // The store had its startup read: later ticks do not read it again.
    let reads = fixture.claimed.transport.requests().len();
    fixture
        .claimed
        .state
        .schedule_engram_retained_run_reads(None);
    assert_eq!(started_reads(), 0);
    assert_eq!(fixture.claimed.transport.requests().len(), reads);
    // A later opening of the work retries the read.
    fixture
        .claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .root_read_failures
        .remove(&fixture.earlier.claim_id);
    engram_answers(
        &fixture,
        &fixture.earlier.claim_id,
        refusal("run_unknown", "the run is unknown in this store"),
    );
    fixture
        .claimed
        .state
        .engram_host()
        .turn_starting(&fixture.claimed.session_id);
    join_background_reads();
    assert_eq!(unresolved(&fixture), None);
}

#[test]
fn a_store_without_a_reader_does_not_make_startup_read_another_store_again() {
    enable_background_reads();
    let fixture = retained("startup-two-stores");
    fixture
        .claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .root_read_failures
        .insert(fixture.earlier.claim_id.clone());
    // A second store retains a run too, but no session can read it.
    let unreadable = EngramAuthorityStoreKey {
        database_path: fixture.claimed.root.join("another-store.db"),
        project_id: "github.com/example/another-store".to_owned(),
    };
    {
        let mut inner = fixture.claimed.state.inner.lock().unwrap();
        let mut other = history_mut(&mut inner, &fixture.store, &fixture.current.work_id).clone();
        other.store = unreadable.clone();
        other.unresolved_runs = vec![fixture.earlier.clone()];
        inner.engram_work_naming_history.push(other);
    }
    fixture
        .claimed
        .state
        .schedule_engram_retained_run_reads(None);
    join_background_reads();
    assert!(startup_read(&fixture, &fixture.store));
    assert!(
        !startup_read(&fixture, &unreadable),
        "no reader of the other store yet"
    );
    let reads = fixture.claimed.transport.requests().len();
    for _ in 0..3 {
        fixture
            .claimed
            .state
            .schedule_engram_retained_run_reads(None);
        assert_eq!(started_reads(), 0, "the readable store is not read again");
    }
    assert_eq!(fixture.claimed.transport.requests().len(), reads);
    assert!(
        !startup_read(&fixture, &unreadable),
        "still owed until a reader exists"
    );
}

#[test]
fn host_startup_waits_for_a_reader_of_the_store() {
    enable_background_reads();
    let fixture = retained("startup-without-reader");
    let project = fixture
        .claimed
        .record_mut(|record| record.session.project_id.take());
    fixture
        .claimed
        .state
        .schedule_engram_retained_run_reads(None);
    assert_eq!(started_reads(), 0, "no session can read the store yet");
    assert!(!startup_read(&fixture, &fixture.store));
    fixture
        .claimed
        .record_mut(|record| record.session.project_id = project);
    engram_answers(
        &fixture,
        &fixture.earlier.claim_id,
        refusal(
            "claim_not_current_for_run",
            "the claim does not belong to the run",
        ),
    );
    fixture
        .claimed
        .state
        .schedule_engram_retained_run_reads(None);
    join_background_reads();
    assert_eq!(unresolved(&fixture), None);
    assert!(startup_read(&fixture, &fixture.store));
}

#[test]
fn another_project_refusal_logs_a_host_binding_bug_and_store_damage_logs_one_alarm() {
    TEST_ENGRAM_RETAINED_RUN_LOG.with(|log| log.borrow_mut().clear());
    let fixture = retained("logged-other-project");
    engram_answers(
        &fixture,
        &fixture.earlier.claim_id,
        refusal("other_project", "the run belongs to another project"),
    );
    maintain(&fixture);
    let damaged = retained("logged-store-damage");
    engram_answers(
        &damaged,
        &damaged.earlier.claim_id,
        json!({"code": "storage_error",
        "message": "work projection contains invalid data: a claim crosses its canonical run"}),
    );
    maintain(&damaged);
    // The same damage read again is not reported again.
    maintain(&damaged);
    let log = TEST_ENGRAM_RETAINED_RUN_LOG.with(|log| log.borrow().clone());
    assert_eq!(log.len(), 2, "{log:#?}");
    assert!(
        log[0].starts_with("engram> host-binding bug: ")
            && log[0].contains(&fixture.earlier.run_id),
        "{}",
        log[0]
    );
    assert!(
        log[1].starts_with("engram> store integrity alarm: ")
            && log[1].contains(&damaged.earlier.run_id),
        "{}",
        log[1]
    );
}

#[test]
fn a_retained_runs_retirement_is_never_a_named_root_proof() {
    let fixture = retained("retired-earns-nothing");
    engram_answers(
        &fixture,
        &fixture.earlier.claim_id,
        refusal(
            "claim_not_current_for_run",
            "the claim does not belong to the run",
        ),
    );
    maintain(&fixture);
    let mut inner = fixture.claimed.state.inner.lock().unwrap();
    let history = history_mut(&mut inner, &fixture.store, &fixture.current.work_id);
    assert_eq!(history.retired_runs.len(), 1);
    assert!(
        history
            .proofs
            .iter()
            .all(|proof| !engram_same_recovery_run(&proof.binding, &fixture.earlier)),
        "a retired run never earns credit: it has no canonical proof"
    );
}
