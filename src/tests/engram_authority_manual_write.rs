// Owns the tests of `AppState::engram_authority_manual_write`, the durable write
// of a test state with no writer thread, against a commit landing between its
// capture and its write. Does not own what the connected writer's fence
// acknowledges (src/tests/engram_authority_fence_phases.rs) or the naming flow
// these tests drive (src/tests/engram_source_root_naming.rs). New module.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

const STALE: &str = "stale metadata snapshot: durable predecessor changed";

fn durable_generation(path: &FsPath) -> u64 {
    metadata_unit_generation(&open_sqlite_state_connection_unconfigured(path).unwrap()).unwrap()
}

fn set_durable_generation(path: &FsPath, generation: u64) {
    open_sqlite_state_connection_unconfigured(path)
        .unwrap()
        .execute(
            "UPDATE meta SET value = ?1 WHERE key = 'metadata_units_generation'",
            [generation.to_string()],
        )
        .unwrap();
}

#[test]
fn a_stale_capture_stays_refused_unless_the_store_already_holds_the_content() {
    let claimed = EOCheckedTurn::idle("manual-write-stale", true);
    let path = claimed.state.persistence_path.clone();
    let original = durable_generation(path.as_path());
    // Only the durable generation moves between capture and write, so the
    // store cannot have taken this capture's content from another commit.
    let moved = path.clone();
    claimed
        .state
        .inner
        .lock()
        .unwrap()
        .test_engram_authority_ack_boundary = Some(Arc::new(move |phase| {
        if phase == "after_delta_capture" {
            set_durable_generation(moved.as_path(), original + 1);
        }
    }));

    let refused = claimed
        .state
        .engram_authority_manual_write(|_| Ok(false))
        .unwrap_err();
    assert_eq!(format!("{refused:#}"), STALE);

    let read_backs = Mutex::new(0);
    assert!(
        claimed
            .state
            .engram_authority_manual_write(|_| {
                *read_backs.lock().unwrap() += 1;
                Ok(true)
            })
            .unwrap()
    );
    assert_eq!(
        *read_backs.lock().unwrap(),
        1,
        "the store is read back once"
    );

    claimed
        .state
        .inner
        .lock()
        .unwrap()
        .test_engram_authority_ack_boundary = None;
    set_durable_generation(path.as_path(), original);
}

#[test]
fn a_write_failure_other_than_a_stale_capture_is_never_read_back() {
    let claimed = EOCheckedTurn::idle("manual-write-other-failure", true);
    // The failure comes after the commit, with the store open and readable, so
    // only the refusal's type keeps this write from being read back.
    METADATA_COMMIT_INTEGRITY_FAULT.with(|fault| fault.set(true));
    let read_backs = Mutex::new(0);
    let error = claimed
        .state
        .engram_authority_manual_write(|_| {
            *read_backs.lock().unwrap() += 1;
            Ok(true)
        })
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("injected metadata post-commit integrity failure"),
        "{error:#}"
    );
    assert_eq!(*read_backs.lock().unwrap(), 0);
}

/// Another manual writer waits out this one's whole window, so it cannot
/// commit an older capture inside it.
#[test]
fn manual_writes_of_one_store_take_turns_across_capture_and_write() {
    let claimed = EOCheckedTurn::idle("manual-write-turns", true);
    let path = claimed.state.persistence_path.clone();
    let observed = Arc::new(Mutex::new(None));
    let recorded = observed.clone();
    claimed
        .state
        .inner
        .lock()
        .unwrap()
        .test_engram_authority_ack_boundary = Some(Arc::new(move |phase| {
        if phase == "after_delta_capture" {
            let turn = engram_authority_manual_write_turn(path.as_path());
            *recorded.lock().unwrap() = Some(turn.try_lock().is_err());
        }
    }));
    let path = claimed.state.persistence_path.clone();
    let turn = engram_authority_manual_write_turn(path.as_path());
    assert!(
        claimed
            .state
            .engram_authority_manual_write(|_| Ok(turn.try_lock().is_err()))
            .unwrap(),
        "the turn lasts through the write to its read-back",
    );
    assert_eq!(
        *observed.lock().unwrap(),
        Some(true),
        "the turn is held at the capture"
    );
    assert!(turn.try_lock().is_ok(), "the turn ends with the write");
}

/// The order that failed a full gate, with the worker's closing capture made
/// before any change of the clear: the real observation worker measures, waits
/// at its install until the clear has changed the state and captured its image,
/// and commits between that capture and its write. The install's image is taken
/// under the state lock at its commit, so it carries the clear's change: the
/// write is refused as stale, the store holds the content, and the clear is
/// confirmed.
#[test]
fn a_closing_install_inside_the_manual_write_leaves_the_clear_clean() {
    let label = "manual-write-closing-install";
    let guard = TEST_PHASE_DEADLOCK_GUARD;
    let claimed = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&claimed.root);
    request_naming(&claimed, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    claimed.begin("The original ordinary turn.");
    let store = naming_store(&claimed);
    let work = claimed
        .record_mut(|record| record.engram.work_binding.clone().unwrap())
        .work_id;
    assert!(
        engram_work_source_root_for_work(
            &claimed.state.inner.lock().unwrap().engram_work_source_roots,
            &store,
            &work,
        )
        .is_some(),
        "the root is named before the clear",
    );

    let (at_install, at_install_rx) = mpsc::channel::<()>();
    let (release, release_rx) = mpsc::channel::<()>();
    let (installed, installed_rx) = mpsc::channel::<()>();
    let at_install = Mutex::new(at_install);
    let release_rx = Mutex::new(release_rx);
    let installed = Mutex::new(installed);
    let held = AtomicBool::new(false);
    *claimed
        .state
        .runtime_observation_lifetime
        .install_hook
        .lock()
        .unwrap() = Some(Arc::new(move |phase| match phase {
        "capture" if !held.swap(true, Ordering::SeqCst) => {
            at_install.lock().unwrap().send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(guard)
                .expect("the writer released the closing install");
        }
        "closing_installed" => {
            let _ = installed.lock().unwrap().send(());
        }
        _ => {}
    }));

    let generations = Arc::new(Mutex::new(None::<(u64, u64)>));
    let recorded = generations.clone();
    let path = claimed.state.persistence_path.clone();
    let fence_inner = claimed.state.inner.clone();
    let fence_session = claimed.session_id.clone();
    let fired = AtomicBool::new(false);
    let refused = Arc::new(AtomicBool::new(false));
    let saw_refusal = refused.clone();
    let release = Mutex::new(release);
    let installed_rx = Mutex::new(installed_rx);
    let boundary: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |phase| {
        if phase == "after_stale_capture_refused" {
            saw_refusal.store(true, Ordering::SeqCst);
            return;
        }
        // The closing install is fenced while a naming owns the bind, so a
        // write held inside that fence would wait for it forever. Only the
        // publication after the bind can release it, once this capture holds
        // the clear's change.
        let publishing_the_clear = {
            let inner = fence_inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&fence_session).unwrap()]
                .engram
                .source_root_bind_owner
                .is_none()
                && engram_work_source_root_for_work(&inner.engram_work_source_roots, &store, &work)
                    .is_none()
        };
        if phase != "after_delta_capture"
            || !publishing_the_clear
            || fired.swap(true, Ordering::SeqCst)
        {
            return;
        }
        let before = durable_generation(path.as_path());
        release.lock().unwrap().send(()).unwrap();
        installed_rx
            .lock()
            .unwrap()
            .recv_timeout(guard)
            .expect("the closing install committed");
        *recorded.lock().unwrap() = Some((before, durable_generation(path.as_path())));
    });

    let state = claimed.state.clone();
    let session = claimed.session_id.clone();
    let runtime = claimed._runtime_rx.clone();
    TEST_ENGRAM_AFTER_SOURCE_ROOT_HELD_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let token = {
                let inner = state.inner.lock().unwrap();
                inner.sessions[inner.find_session_index(&session).unwrap()]
                    .runtime
                    .runtime_token()
                    .unwrap()
            };
            state
                .finish_turn_ok_if_runtime_matches(&session, &token)
                .unwrap();
            let dispatch = match state
                .dispatch_turn(
                    &session,
                    SendMessageRequest {
                        text: "The successor ordinary turn.".into(),
                        expanded_text: None,
                        attachments: Vec::new(),
                        source_session_id: None,
                        source_mailbox: None,
                    },
                )
                .unwrap()
            {
                DispatchTurnResult::Dispatched(dispatch)
                | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
                DispatchTurnResult::Queued => panic!("the real successor must dispatch"),
            };
            deliver_turn_dispatch(&state, dispatch).unwrap();
            assert!(matches!(
                receive(&runtime.lock().unwrap(), "successor provider handoff"),
                CodexRuntimeCommand::Prompt { .. }
            ));
            // The worker's off-lock capture is done before the clear changes
            // anything: it now waits at the install.
            at_install_rx
                .recv_timeout(guard)
                .expect("the closing capture reached its install");
            state
                .inner
                .lock()
                .unwrap()
                .test_engram_authority_ack_boundary = Some(boundary);
        }));
    });

    let cleared = request_naming(&claimed, label, None, vec![naming_claim(label)]).unwrap();
    assert!(cleared.sealed.is_none());
    let (before, after) = generations
        .lock()
        .unwrap()
        .expect("the writer released the install");
    assert!(
        after > before,
        "the closing install moved the durable generation: {before} -> {after}"
    );
    assert!(
        refused.load(Ordering::SeqCst),
        "the write really was refused as stale"
    );
    *claimed
        .state
        .runtime_observation_lifetime
        .install_hook
        .lock()
        .unwrap() = None;
    // The boundary holds the state it is stored in; dropping it ends that cycle.
    claimed
        .state
        .inner
        .lock()
        .unwrap()
        .test_engram_authority_ack_boundary = None;
    claimed.end();
}
