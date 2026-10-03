// Owns the operator-run check of the acceptance evaluation's initial-sighting
// preflight (src/acceptance_named_root_sighting.rs) against a real Engram
// binary and a disposable store: the host-private `named_root_sighting_read`
// sent over the production host reader, its answer parsed by the real parser,
// and the preflight's decision for each state the real producer gives here.
// Does not own the scripted checks of the same preflight
// (src/tests/acceptance_evaluation_sighting.rs) or the fixture, which is its
// parent module's (src/tests/engram_named_root_completion_live.rs). New
// module, a child of that live module so the launcher's `live` mode runs it.
//
// The states are those of Engram's own result (Engram
// src/domain/named_root_sighting_read.rs: `NamedRootAtCut` and
// `InitialSighting`). Covered here: root `none`; root `bound` with sighting
// `absent`; root `bound` with sighting `present`; and `binding_changed` for a
// cut read from before the root was bound. Not set up here: a binding that
// moves between the task's read and the sighting read, a race the scripted
// checks cover.
use super::*;

/// Stops the host reader's control process, which these reads start and the
/// fixture's cleanup does not know, before the disposable store is removed.
struct HostReaderCleanup {
    transport: Arc<BoundaryFaultTransport>,
}

impl Drop for HostReaderCleanup {
    fn drop(&mut self) {
        self.transport.shutdown_session(WORK_HOST_READER_SESSION_ID);
    }
}

/// The task as the evaluation request reads it from the real tracker: the
/// windowed and full shows, with the canonical identity from `core inspect`.
fn live_task(
    fixture: &CompletionFixture,
    session: &str,
    work_ref: &str,
) -> AcceptanceEvaluationTask {
    let windowed = fixture.work(session, &["show", work_ref, "--notes", "--gates", "--json"]);
    let full = fixture.work(session, &["show", work_ref, "--full", "--json"]);
    let mut task = parse_acceptance_evaluation_task(windowed, full)
        .unwrap_or_else(|error| panic!("the real show is a task: {}", error.message));
    let core = fixture.work(session, &["core", "inspect", work_ref, "--json"]);
    task.canonical_identity = acceptance_core_identity(&core)
        .unwrap_or_else(|error| panic!("the real core identity parses: {}", error.message));
    assert!(
        task.canonical_work_id().is_some(),
        "the real task names its work: {core:#}"
    );
    task
}

/// The local root an evaluation of the session's claimed work reads, as the
/// request selects it, if one is named.
fn live_local_root(
    fixture: &CompletionFixture,
    session: &str,
    task: &AcceptanceEvaluationTask,
) -> Option<AcceptanceEvaluationSourceRoot> {
    let inner = fixture
        .live
        .state
        .inner
        .lock()
        .expect("state mutex poisoned");
    let store = acceptance_evaluation_host_target_locked(&inner, session)
        .expect("the host reader is configured")
        .store;
    let binding = inner.sessions[inner.find_session_index(session).expect("session exists")]
        .engram
        .work_binding
        .clone()
        .expect("the session is bound to its claim");
    engram_work_source_root_for_claim(
        &inner.engram_work_source_roots,
        &store,
        task.canonical_work_id().expect("canonical work id"),
        &binding.claim_id,
    )
    .map(AcceptanceEvaluationSourceRoot::from_entry)
}

/// The production preflight for `task`, against `root` as the local side.
fn live_preflight(
    fixture: &CompletionFixture,
    session: &str,
    task: &AcceptanceEvaluationTask,
    root: Option<&AcceptanceEvaluationSourceRoot>,
) -> Result<(), ApiError> {
    let target = {
        let inner = fixture
            .live
            .state
            .inner
            .lock()
            .expect("state mutex poisoned");
        acceptance_evaluation_host_target_locked(&inner, session)
            .expect("the host reader is configured")
    };
    fixture.live.state.require_acceptance_named_root_sighting(
        session,
        &target.connection,
        &target.store,
        task,
        root,
        std::time::Instant::now() + DEADLOCK_GUARD,
        &std::time::Instant::now,
    )
}

/// The real producer's answer at `cut`, read over the same host reader and
/// parsed by the production parser, so its state can be asserted directly.
fn live_sighting(
    fixture: &CompletionFixture,
    session: &str,
    task: &AcceptanceEvaluationTask,
    cut: i64,
) -> EngramNamedRootSightingRead {
    let target = {
        let inner = fixture
            .live
            .state
            .inner
            .lock()
            .expect("state mutex poisoned");
        acceptance_evaluation_host_target_locked(&inner, session)
            .expect("the host reader is configured")
    };
    // The run the preflight reads: the canonical identity's, else the show's.
    let run_id = task
        .canonical_identity
        .as_ref()
        .and_then(|identity| identity.active_run_id.clone())
        .or_else(|| task.active_run_id.clone())
        .unwrap_or_else(|| panic!("the claimed task has an active run: {task:#?}"));
    let value = fixture
        .live
        .transport
        .request(
            &target.connection,
            &EngramControlRequest::NamedRootSightingRead {
                work_ref: task.canonical_work_id().unwrap().to_owned(),
                run_id: run_id.clone(),
                run_cut: Some(cut),
            },
            Duration::from_millis(ENGRAM_DEFAULT_CALL_TIMEOUT_MS),
        )
        .expect("the real producer answers the sighting read");
    eprintln!("real named_root_sighting_read at cut {cut}: {value:#}");
    let read = parse_engram_named_root_sighting_read(value)
        .unwrap_or_else(|why| panic!("the production parser takes the real answer: {why}"));
    assert_eq!(read.read_cut, cut);
    assert_eq!(read.run_id, run_id);
    assert_eq!(read.work_id, task.canonical_work_id().unwrap());
    read
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn live_sighting_preflight_follows_each_state_of_the_real_producer() {
    let fixture = completion_fixture();
    // Dropped before the fixture, so the reader no longer holds the store.
    let _reader = HostReaderCleanup {
        transport: fixture.live.transport.clone(),
    };
    let session = fixture.live.session_id.clone();
    let added = fixture.work(
        &session,
        &[
            "add",
            "Evaluate the named worktree only once it is sighted",
            "--accept",
            "A test passes in the named root",
            "--bind",
            "1=test",
            "--json",
        ],
    );
    let work_ref = added["work"]["short_ref"]
        .as_str()
        .or_else(|| added["short_ref"].as_str())
        .expect("work ref")
        .to_owned();
    fixture.work(&session, &["claim", &work_ref, "--json"]);
    fixture
        .live
        .state
        .ensure_engram_session_bound_off_lock(&session)
        .expect("the claim binds")
        .expect("control is enabled");

    // Neither side binds a root: the evaluation may go ahead.
    let unnamed = live_task(&fixture, &session, &work_ref);
    assert!(live_local_root(&fixture, &session, &unnamed).is_none());
    let read = live_sighting(&fixture, &session, &unnamed, unnamed.evidence_basis);
    assert_eq!(read.root, EngramSightingRoot::None, "{read:#?}");
    assert!(!read.binding_changed, "{read:#?}");
    live_preflight(&fixture, &session, &unnamed, None)
        .unwrap_or_else(|error| panic!("no root on either side admits: {}", error.message));

    // Name a worktree: Engram binds it, but nothing has sighted it yet.
    let worktree = fixture.root.join(".worktrees").join("sighted");
    run_git_test_command(
        &fixture.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "sighted",
            worktree.to_str().expect("a UTF-8 worktree path"),
        ],
    );
    let named = fixture
        .live
        .state
        .name_engram_source_root(
            &session,
            EngramSourceRootRequest {
                work: work_ref.clone(),
                path: Some(Some(worktree.to_string_lossy().into_owned())),
            },
        )
        .expect("the real named_root_bind acknowledges the worktree");
    let bound = live_task(&fixture, &session, &work_ref);
    let local = live_local_root(&fixture, &session, &bound).expect("the root is named locally");
    assert_eq!(local.generation, named.generation);
    let read = live_sighting(&fixture, &session, &bound, bound.evidence_basis);
    match &read.root {
        EngramSightingRoot::Bound {
            workspace_id,
            generation,
            sighting,
            ..
        } => {
            assert_eq!(
                *workspace_id, local.root,
                "Engram binds the root TermAl named"
            );
            assert_eq!(u64::try_from(*generation).ok(), Some(local.generation));
            assert_eq!(*sighting, EngramRootSighting::Absent, "{read:#?}");
        }
        EngramSightingRoot::None => panic!("the named root is bound in Engram: {read:#?}"),
    }
    let error = live_preflight(&fixture, &session, &bound, Some(&local))
        .expect_err("a named root with no sighting is refused before any evaluator");
    assert!(
        error.message.contains("has no sighting"),
        "{}",
        error.message
    );
    let error = live_preflight(&fixture, &session, &bound, None)
        .expect_err("a root Engram binds but TermAl does not name is refused");
    assert!(
        error.message.contains("TermAl has no named source root"),
        "{}",
        error.message
    );

    // The cut from before the naming: no root there, and the binding moved.
    let earlier = live_sighting(&fixture, &session, &bound, unnamed.evidence_basis);
    assert_eq!(earlier.root, EngramSightingRoot::None, "{earlier:#?}");
    assert!(earlier.binding_changed, "{earlier:#?}");
    let mut stale = bound.clone();
    stale.evidence_basis = unnamed.evidence_basis;
    let error = live_preflight(&fixture, &session, &stale, Some(&local))
        .expect_err("a basis whose binding moved since is refused");
    assert!(error.message.contains("moved after"), "{}", error.message);

    // A checkpointed turn of the claim sights the root: the evaluation may go.
    fixture.begin(&session, "Look at the named worktree.");
    finish_live_root(&fixture.live.state, &session);
    let sighted = live_task(&fixture, &session, &work_ref);
    let read = live_sighting(&fixture, &session, &sighted, sighted.evidence_basis);
    match &read.root {
        EngramSightingRoot::Bound { sighting, .. } => assert!(
            matches!(sighting, EngramRootSighting::Present { .. }),
            "the checkpointed turn sighted the root: {read:#?}"
        ),
        EngramSightingRoot::None => panic!("the named root is still bound: {read:#?}"),
    }
    assert!(!read.binding_changed, "{read:#?}");
    let local = live_local_root(&fixture, &session, &sighted).expect("still named");
    live_preflight(&fixture, &session, &sighted, Some(&local))
        .unwrap_or_else(|error| panic!("a sighted root admits: {}", error.message));
}
