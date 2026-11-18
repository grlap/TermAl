// Provider-boundary regressions for proven Codex starts and safe Git
// metadata placement, using the existing disposable run fixtures.
use super::super::selected_provider::codex_provider_dispatch_for_turn;
use super::*;

#[test]
fn a_provisional_native_turn_cannot_associate_before_proof_or_lend_its_start() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for prove_same_turn in [true, false] {
        let fixture = RunFixture::new("turn-proof");
        let [worktree] = linked_stream_worktrees(&fixture, ["linked"]);
        let session = running_owner(&fixture, "turn-proof-runtime");
        let origin = stream_turn(&fixture, &session, 1);
        let dispatch =
            codex_provider_dispatch_for_turn(fixture.state.clone(), session.clone(), origin, None);
        dispatch(json!({"method":"turn/started","params":{"threadId":PROVIDER_THREAD}}));
        let reported_turn = "reported-before-rpc";
        dispatch(stream_start(reported_turn, &worktree));
        let original = stream_observation(&fixture, 1);
        let expected = register_stream_run(&fixture, &session, &worktree);
        let locator = format!("RUN {}\n", expected.run_directory);
        dispatch(stream_delta(reported_turn, &locator));
        assert_eq!(
            stream_observation(&fixture, 1),
            original,
            "an item-derived turn is not proof of native ownership"
        );

        let proven_turn = if prove_same_turn {
            reported_turn
        } else {
            PROVIDER_TURN
        };
        dispatch(json!({"method":"turn/started","params":{
            "threadId":PROVIDER_THREAD,"turn":{"id":proven_turn}
        }}));
        // Output alone cannot retroactively make an earlier start eligible.
        dispatch(stream_delta(proven_turn, &locator));
        assert_eq!(stream_observation(&fixture, 1), original);
        dispatch(stream_start(proven_turn, &worktree));
        dispatch(stream_delta(proven_turn, &locator));
        let mut expected_row = original;
        if prove_same_turn {
            expected_row.artifact = Some(expected);
        }
        assert_eq!(
            stream_observation(&fixture, 1),
            expected_row,
            "only proof matching the recorded start can supply its artifact"
        );
    }
}

#[test]
fn a_later_addressed_start_cannot_change_the_cwd_of_an_unaddressed_start() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for change_cwd in [false, true] {
        let fixture = RunFixture::new("vacant-stream");
        let [worktree, other] = linked_stream_worktrees(&fixture, ["linked-a", "linked-b"]);
        let session = running_owner(&fixture, "vacant-stream-runtime");
        let origin = stream_turn(&fixture, &session, 1);
        let dispatch = codex_provider_dispatch(
            fixture.state.clone(),
            session.clone(),
            origin,
            PROVIDER_TURN,
        );
        let mut first = stream_start(PROVIDER_TURN, &worktree);
        first["params"].as_object_mut().unwrap().remove("turnId");
        dispatch(first);
        let original = stream_observation(&fixture, 1);
        let expected = register_stream_run(&fixture, &session, &worktree);
        let locator = format!("RUN {}\n", expected.run_directory);
        dispatch(stream_delta(PROVIDER_TURN, &locator));
        assert_eq!(stream_observation(&fixture, 1), original);

        let cwd = if change_cwd { &other } else { &worktree };
        dispatch(stream_start(PROVIDER_TURN, cwd));
        dispatch(stream_delta(PROVIDER_TURN, &locator));
        let mut expected_row = original;
        if !change_cwd {
            expected_row.artifact = Some(expected);
        }
        assert_eq!(
            stream_observation(&fixture, 1),
            expected_row,
            "a vacant stream entry must agree with the observed original cwd"
        );
    }
}

#[test]
fn a_proven_start_records_its_own_execution_when_an_older_start_is_open() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = RunFixture::new("open-prior");
    let [worktree] = linked_stream_worktrees(&fixture, ["linked"]);
    let session = running_owner(&fixture, "open-prior-runtime");
    let origin = stream_turn(&fixture, &session, 1);
    let dispatch = codex_provider_dispatch(
        fixture.state.clone(),
        session.clone(),
        origin,
        PROVIDER_TURN,
    );
    dispatch(stream_start(PROVIDER_TURN, &worktree));
    let prior = stream_observation(&fixture, 1);
    assert!(prior.summary.ended_at.is_none());

    let next_turn = "next-proven-turn";
    let origin = stream_turn(&fixture, &session, 2);
    let dispatch =
        codex_provider_dispatch(fixture.state.clone(), session.clone(), origin, next_turn);
    let mut ambient = stream_start(next_turn, &worktree);
    ambient["params"].as_object_mut().unwrap().remove("turnId");
    dispatch(ambient);
    assert_eq!(
        fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_run_observations
            .len(),
        1,
        "a frame without a native turn cannot disambiguate the old start"
    );
    dispatch(stream_start(next_turn, &worktree));
    let current = stream_observation(&fixture, 2);
    assert_ne!(current.execution_id, prior.execution_id);
    assert_eq!(current.command_fingerprint, prior.command_fingerprint);
    assert_eq!(current.tool_identity, prior.tool_identity);
    let expected = register_stream_run(&fixture, &session, &worktree);
    dispatch(stream_delta(
        next_turn,
        &format!("RUN {}\n", expected.run_directory),
    ));
    assert_eq!(stream_observation(&fixture, 1), prior);
    let mut expected_row = current;
    expected_row.artifact = Some(expected);
    assert_eq!(stream_observation(&fixture, 2), expected_row);
}

#[test]
fn automatic_placement_checks_containment_before_reading_bounded_git_metadata() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = RunFixture::new("git-placement");
    let [linked] = linked_stream_worktrees(&fixture, ["linked"]);
    let session = running_owner(&fixture, "git-placement-runtime");
    let outside = fixture.root.with_file_name("outside");
    fs::create_dir(&outside).unwrap();
    let foreign_admin = fixture
        .root
        .with_file_name("foreign")
        .join(".git/worktrees/link");
    // There is intentionally no commondir there. Placement must refuse
    // repository membership before untrusted metadata can decide the result.
    fs::create_dir_all(&foreign_admin).unwrap();
    fs::write(
        outside.join(".git"),
        format!("gitdir: {}\n", foreign_admin.display()),
    )
    .unwrap();

    let mut generation = 0;
    let mut observe = |cwd: &FsPath, expected| {
        generation += 1;
        let provenance = stream_turn(&fixture, &session, generation);
        let cwd = test_run_display_path(cwd);
        fixture.state.engram_host().observe(
            &session,
            &provenance,
            EngramRecorderObservation::CommandStarted {
                key: STREAM_ITEM,
                ran: Some(STREAM_LAUNCHER),
                cwd: Some(&cwd),
            },
        );
        let inner = fixture.state.inner.lock().unwrap();
        let row = inner
            .test_run_observations
            .values()
            .find(|row| row.turn_generation == generation)
            .expect("provider start remains visible");
        assert_eq!(row.placement_refusal, expected);
        assert!(row.artifact.is_none() && row.evidence.is_empty() && row.work_binding.is_none());
        if expected.is_none() {
            assert_eq!(
                test_run_path_key(&row.summary.worktree),
                test_run_path_key(&cwd)
            );
        } else {
            assert!(row.summary.worktree.is_empty());
        }
    };
    observe(&linked, None);
    observe(&outside, Some(TestRunPlacementRefusal::ForeignRepository));

    let marker = linked.join(".git");
    let original = fs::read_to_string(&marker).unwrap();
    let admin = linked.join(original.trim().strip_prefix("gitdir:").unwrap().trim());
    let common = admin.join("commondir");
    let original_common = fs::read_to_string(&common).unwrap();
    // A valid target with excess padding was previously accepted and read
    // without a size bound. No network path or external I/O is required.
    fs::write(
        &common,
        format!("{}{}", original_common.trim_end(), " ".repeat(8193)),
    )
    .unwrap();
    observe(
        &linked,
        Some(TestRunPlacementRefusal::RepositoryUnavailable),
    );
    fs::remove_file(&common).unwrap();
    fs::create_dir(&common).unwrap();
    observe(
        &linked,
        Some(TestRunPlacementRefusal::RepositoryUnavailable),
    );
}
