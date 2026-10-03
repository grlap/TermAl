// Owns the tests of the state-owned Engram budget clock boundary
// (src/engram_budget.rs): every snapshot comes from the one reader, a
// fixture's first choice of clock is kept when it chooses again, a choice made
// after a snapshot fails at once, and a test boot chooses its clock before its
// own recovery runs. Does not own the families that use the boundary (the
// uncertain-begin tests in src/tests/engram_uncertain_begin.rs and the
// boot-recovery target tests) or the clock's scripted waits. New file.
use super::*;

fn snapshots(state: &AppState) -> usize {
    state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .engram_budget_clock_snapshots
        .load(std::sync::atomic::Ordering::SeqCst)
}

fn is_scripted(clock: &EngramBudgetClock) -> bool {
    matches!(clock, EngramBudgetClock::Scripted(_))
}

#[test]
fn a_fixtures_first_clock_choice_is_kept_when_it_chooses_again() {
    let scripted = test_app_state();
    let first = scripted.select_test_scripted_engram_budget_clock();
    let EngramBudgetClock::Scripted(first) = first else {
        panic!("the first choice installs a scripted clock");
    };
    let again = scripted.select_test_scripted_engram_budget_clock();
    let EngramBudgetClock::Scripted(again) = again else {
        panic!("choosing again keeps the scripted clock");
    };
    assert!(
        Arc::ptr_eq(&first, &again),
        "the same scripted clock is kept"
    );
    assert!(
        is_scripted(&scripted.declare_test_real_engram_budget_clock()),
        "a later Real declaration does not replace the chosen scripted clock"
    );

    let real = test_app_state();
    assert!(!is_scripted(&real.declare_test_real_engram_budget_clock()));
    assert!(
        !is_scripted(&real.select_test_scripted_engram_budget_clock()),
        "a Real declaration is kept when the fixture enables Engram again"
    );
    assert!(!is_scripted(&real.engram_budget_clock()));
}

#[test]
fn every_clock_snapshot_comes_from_the_one_reader() {
    let state = test_app_state();
    let clock = state.select_test_scripted_engram_budget_clock();
    let before = snapshots(&state);
    let read = state.engram_budget_clock();
    assert_eq!(
        snapshots(&state),
        before + 1,
        "the off-lock accessor reads through it"
    );
    let (EngramBudgetClock::Scripted(chosen), EngramBudgetClock::Scripted(read)) = (clock, read)
    else {
        panic!("the accessor returns the chosen clock");
    };
    assert!(Arc::ptr_eq(&chosen, &read));
}

#[test]
#[should_panic(expected = "must choose its Engram budget clock before")]
fn choosing_a_clock_after_a_snapshot_fails_at_once() {
    let state = test_app_state();
    // A target or worker already holds the default clock.
    let _held = state.engram_budget_clock();
    state.select_test_scripted_engram_budget_clock();
}

#[test]
fn a_test_boot_chooses_its_clock_before_its_own_recovery() {
    let (_temp_root, project_root, persistence_path, templates_path) =
        super::delegation_support::temp_delegation_state_paths();
    let booted = AppState::new_with_paths_engram_transport_and_clock_for_test(
        project_root.to_string_lossy().into_owned(),
        persistence_path,
        templates_path,
        ScriptedEngramControlTransport::new([]),
        EngramBudgetClock::scripted(),
    )
    .expect("state should boot");
    let inner = booted.inner.lock().expect("state mutex poisoned");
    assert!(inner.engram_budget_clock_selected);
    assert!(is_scripted(&inner.engram_budget_clock_snapshot()));
    drop(inner);
    booted.shutdown_persist_blocking();
}
